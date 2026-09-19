//! T19 出线自愈：`supports_images` 判定表（种子表/家族猜）总会有漏——新型号、走自定义
//! 代理的端点。这里加一道运行时保险丝：真实发生 400 且响应体带着「图片内容不被接受」
//! 的已知特征、且本次请求确实带了图片块时，把这个 provider 实例的 supports_images
//! 运行时覆盖为 false、剥图重发一次；不满足条件（含「已经重试过」）原样透传。
//! 拆出单独文件（同 `image_wire.rs`）：避免 `openai_compatible.rs` 继续逼近文件大小
//! 门禁的基线历史额度。
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::provider::{ChatMessage, ProviderCapabilities, ProviderClient};

impl super::OpenAiCompatibleProvider {
    /// 发请求；若这轮原生搜索 Active 且 provider 返 4xx，则 warning + 不带原生重发一次。
    /// 返回最终要 collect 的 Response。Kimi 每轮 post 也走这个（T8）。挪进本文件（同
    /// `image_wire.rs`）：避免 `openai_compatible.rs` 继续逼近文件大小门禁棘轮上限。
    ///
    /// T19b P3-2：图片保险丝优先于搜索降级。原生搜索 Active 的 GLM 这类家族真实撞到的
    /// 400 常常是厂商拒图（`is_image_rejected_body`），跟搜索毫无关系——若不加甄别，会
    /// 先把它误判成「搜索导致的 4xx」发一条与搜索无关的假 `native_search_degraded`
    /// warning、再带着图片不带搜索重发一次（还是撞拒图 400），最后才轮到图片保险丝剥图
    /// 重发，一共 3 次请求 + 1 条误导性 warning。这里先甄别：带图的 400 若命中拒图特征，
    /// 直接走 `strip_images_and_retry`（剥图 + 翻运行时覆盖 + 重发），不产生搜索降级
    /// warning，一共 2 次请求。
    pub(crate) async fn post_native_or_degrade(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        events: &mut crate::events::EventRecorder,
    ) -> crate::error::Result<reqwest::Response> {
        use crate::provider::native_search::{
            native_search_state, provider_family, NativeSearchState,
        };
        let family = provider_family(&self.config.provider_id);
        let active = native_search_state(
            family.has_native_search(),
            self.config.network,
            self.config.native_search_enabled,
        ) == NativeSearchState::Active;
        let response = self
            .post(&self.build_body(messages, tools, true)?, events)
            .await?;
        if active && response.status().is_client_error() {
            let status = response.status();
            if status == StatusCode::BAD_REQUEST && messages_have_images(messages) {
                let body_text = response.text().await.unwrap_or_default();
                if is_image_rejected_body(&body_text) {
                    // `strip_images_and_retry` 调回 `post_native_or_degrade`，与这里互为
                    // 递归——async fn 直接递归需要显式装箱间接化，否则 future 大小无限。
                    return Box::pin(
                        self.strip_images_and_retry(messages, tools, events, &body_text),
                    )
                    .await;
                }
            }
            events.emit(
                "provider.warning",
                json!({
                    "warning": "native_search_degraded",
                    "status": status.as_u16(),
                }),
            )?;
            return self
                .post(&self.build_body(messages, tools, false)?, events)
                .await;
        }
        Ok(response)
    }

    /// `capabilities()` 用的短名 accessor（省 `openai_compatible.rs` 里那行的宽度——它已经
    /// 逼近文件大小门禁的基线历史额度，拆细节到这个子模块）。
    pub(crate) fn runtime_images_disabled(&self) -> bool {
        self.images_disabled_at_runtime
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// `next_turn` 的唯一发请求入口（T19）：运行时已经因为上一轮 400 拒图而降级过，这轮
    /// 直接把图剥掉再发——不再让 provider 二次撞线。这里剥图**不 emit** `attachment.dropped`
    /// （T19b P2-2）：同一张图已经在第一次被拒时记过一条事件了，覆盖生效后每轮都重复剥
    /// 同一批图（canonical messages 从没被真正改过），若照样每轮都 emit 会把一张图记成
    /// N 条 dropped 事件——说明行仍每轮追加在这份临时 wire 副本上（wire 用完即弃）。
    /// 还没降级过，走正常发送 + 事后判 400 特征（见 `maybe_retry_after_image_rejection`）。
    pub(crate) async fn post_with_image_guard(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        events: &mut crate::events::EventRecorder,
    ) -> crate::error::Result<reqwest::Response> {
        if self.runtime_images_disabled() && messages_have_images(messages) {
            let stripped = self.strip_images(messages, events, "provider_rejected", None, false)?;
            return self.post_native_or_degrade(&stripped, tools, events).await;
        }
        let response = self.post_native_or_degrade(messages, tools, events).await?;
        self.maybe_retry_after_image_rejection(response, messages, tools, events)
            .await
    }

    /// 触发条件：状态码 400 + 本次消息里确实带了图片块。不满足时原样把 response 交还
    /// 调用方（这种情况下 body 还没被读，调用方仍可正常走 `collect` 读状态/流）。
    ///
    /// 满足条件但响应体不符合「图片不被接受」特征：body 已经被这里读掉了，不能把半读的
    /// `Response` 交回去给 `collect` 二次读 body（会读到空串、吞掉真实错误信息）——直接
    /// 在这里按原样报错。
    async fn maybe_retry_after_image_rejection(
        &self,
        response: reqwest::Response,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        events: &mut crate::events::EventRecorder,
    ) -> crate::error::Result<reqwest::Response> {
        if response.status() != reqwest::StatusCode::BAD_REQUEST || !messages_have_images(messages)
        {
            return Ok(response);
        }
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        if !is_image_rejected_body(&body_text) {
            let message = if body_text.trim().is_empty() {
                format!("provider returned HTTP {status}")
            } else {
                format!("provider returned HTTP {status}: {body_text}")
            };
            return Err(crate::error::HarnessError::Provider(message));
        }
        self.strip_images_and_retry(messages, tools, events, &body_text)
            .await
    }

    /// 已确认响应体符合「图片被拒」特征（`is_image_rejected_body` 判过 true）：翻运行时
    /// 覆盖、剥图重发一次。同时被 `maybe_retry_after_image_rejection`（无原生搜索/首次
    /// 撞线）和 `post_native_or_degrade`（T19b P3-2：原生搜索 Active 时图片保险丝优先，
    /// 见该函数注释）两条路径复用，避免各自重复一份「翻覆盖 + 剥图 + 重发」。
    pub(crate) async fn strip_images_and_retry(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        events: &mut crate::events::EventRecorder,
        body_text: &str,
    ) -> crate::error::Result<reqwest::Response> {
        self.images_disabled_at_runtime
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let provider_error = truncate_provider_error(body_text);
        let retry_messages = self.strip_images(
            messages,
            events,
            "provider_rejected",
            Some(&provider_error),
            true,
        )?;
        self.post_native_or_degrade(&retry_messages, tools, events)
            .await
    }

    /// 剥掉 `messages` 里的所有图片块（返回一份改过的拷贝，不动调用方原件）+ 追加降级
    /// 说明 + （`emit_event` 为真时）记 `attachment.dropped`（reason/厂商原文见入参）。
    /// 复用 `crate::image::degrade_images_with_reason`——与配置/种子表判定不支持图片时的
    /// 语义完全一致，只是 reason/事件开关不同。
    fn strip_images(
        &self,
        messages: &[ChatMessage],
        events: &mut crate::events::EventRecorder,
        reason: &str,
        provider_error: Option<&str>,
        emit_event: bool,
    ) -> crate::error::Result<Vec<ChatMessage>> {
        let mut copy = messages.to_vec();
        let caps = ProviderCapabilities {
            supports_images: false,
            ..self.capabilities()
        };
        crate::image::degrade_images_with_reason(
            &mut copy,
            &caps,
            events,
            reason,
            provider_error,
            emit_event,
        )?;
        Ok(copy)
    }
}

/// T19b P2-1：`attachment.dropped` payload 里的 `provider_error` 截断上限（按字符数，不按
/// 字节数——避免在多字节字符中间切断产生非法 UTF-8）。
const PROVIDER_ERROR_MAX_CHARS: usize = 300;

/// 厂商拒图响应体截断成事件 payload / 降级说明行可用的摘要。
fn truncate_provider_error(body_text: &str) -> String {
    if body_text.chars().count() <= PROVIDER_ERROR_MAX_CHARS {
        return body_text.to_string();
    }
    let mut truncated: String = body_text.chars().take(PROVIDER_ERROR_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}

/// 本次请求里是否真的带了图片块——不带图的 400 是别的错误，重试只会把普通 400 变成
/// 无意义的重试循环。`pub(crate)`：也被 `openai_compatible.rs` 的
/// `post_native_or_degrade`（T19b P3-2）复用。
pub(crate) fn messages_have_images(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|m| !m.images.is_empty())
}

/// 厂商 400 响应体是否符合「图片内容不被接受（能力否定）」的已知特征（任一即可）：
/// - `error.code` 为字符串 `"1210"`（智谱 GLM 这类错误码）；
/// - message 同时含 `content.type` 与 `allowed values`（大小写不敏感）；
/// - message 同时含 `image` 与 `not support`/`unsupported`/`does not support`/
///   `not enabled`（大小写不敏感）。
/// T19b P2-1 收窄：去掉了此前宽泛的裸 `invalid` 子串（`Invalid image_url`/`image exceeds
/// 20 MB`/`Invalid base64 image`/`too many images` 这类真实格式·尺寸·数量错误也含
/// `image`+`invalid`，会被误判成「厂商不支持图片」而把真实错误吞掉）；也去掉了 JSON 解析
/// 失败时对整段原文做子串扫描的兜底（HTML 错误页这类非结构化响应体不再可能被误判为拒图
/// 特征）——JSON 解析失败或没有 `error.message` 字段一律判不匹配、原样报错。`pub(crate)`：
/// 也被 `openai_compatible.rs` 的 `post_native_or_degrade`（T19b P3-2）复用。
pub(crate) fn is_image_rejected_body(body_text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body_text) else {
        return false;
    };
    if value["error"]["code"].as_str() == Some("1210") {
        return true;
    }
    let Some(message) = value["error"]["message"].as_str() else {
        return false;
    };
    let lower = message.to_ascii_lowercase();
    (lower.contains("content.type") && lower.contains("allowed values"))
        || (lower.contains("image")
            && (lower.contains("not support")
                || lower.contains("unsupported")
                || lower.contains("does not support")
                || lower.contains("not enabled")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_glm_1210_error_code() {
        let body = r#"{"error":{"code":"1210","message":"messages.content.type is invalid, allowed values: ['text']"}}"#;
        assert!(is_image_rejected_body(body));
    }

    #[test]
    fn recognizes_content_type_allowed_values_signature() {
        let body = r#"{"error":{"code":"9999","message":"messages.content.type is invalid, allowed values: ['text']"}}"#;
        assert!(is_image_rejected_body(body));
    }

    #[test]
    fn recognizes_image_not_supported_signature() {
        let body = r#"{"error":{"message":"image input is not supported by this model"}}"#;
        assert!(is_image_rejected_body(body));
    }

    #[test]
    fn plain_bad_request_is_not_recognized() {
        assert!(!is_image_rejected_body(
            r#"{"error":{"message":"invalid api key"}}"#
        ));
        assert!(!is_image_rejected_body("bad request"));
    }

    /// T19b P2-1 反例集：真实格式/尺寸/数量/网关错误，都含 `image` 或 `invalid` 子串，
    /// 但都不是厂商「不支持图片输入」——不能被剥图重试吞掉，必须原样报错。
    #[test]
    fn real_image_format_and_size_errors_are_not_recognized_as_capability_rejection() {
        assert!(!is_image_rejected_body(
            r#"{"error":{"message":"Invalid image_url: expected a valid URL or data URI"}}"#
        ));
        assert!(!is_image_rejected_body(
            r#"{"error":{"message":"Invalid request: image exceeds the maximum size of 20 MB"}}"#
        ));
        assert!(!is_image_rejected_body(
            r#"{"error":{"message":"Invalid base64 image data"}}"#
        ));
        assert!(!is_image_rejected_body(
            r#"{"error":{"message":"Invalid request: too many images in one message (max 10)"}}"#
        ));
        assert!(!is_image_rejected_body(
            "<html><body>Invalid request. <a href='/image'>image</a></body></html>"
        ));
    }

    #[test]
    fn does_not_support_and_not_enabled_signatures_are_recognized() {
        assert!(is_image_rejected_body(
            r#"{"error":{"message":"this endpoint does not support image inputs"}}"#
        ));
        assert!(is_image_rejected_body(
            r#"{"error":{"message":"image understanding is not enabled for this model"}}"#
        ));
    }

    #[test]
    fn messages_have_images_detects_any_message_with_images() {
        let img = crate::image::ImageBlock {
            media_type: "image/png".into(),
            data_base64: "QUJD".into(),
            source_path: None,
            bytes: 3,
            sha256: None,
        };
        assert!(messages_have_images(&[ChatMessage::user_with_images(
            "hi",
            vec![img]
        )]));
        assert!(!messages_have_images(&[ChatMessage::user("hi")]));
    }
}
