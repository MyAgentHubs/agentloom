//! T1 图片附件：把有 images 的消息拼成 OpenAI 多模态 content 数组形态
//! `[{"type":"text",...},{"type":"image_url",...}...]`（先文本后图）。
//! 拆出单独文件（避免 `openai_compatible.rs` 继续超出文件大小门禁的基线历史额度）。
//! 没 images 的消息保持 serde 派生出的原样字符串形态，调用方一字节不变。

use serde_json::{json, Value};

use crate::provider::ChatMessage;

/// 测试夹具，不是生产默认值：真实配置来自 CLI/env（见 `crate::config::provider_config_with_model`），
/// 生产代码没有任何构造点用 `..Default::default()`。存在的唯一理由是省得每加一个字段就要补遍
/// 全部既有测试字面量——新字段一律先进这里，调用点按需 `..Default::default()`。
/// `#[doc(hidden)]`：别让它看起来像一个可用的生产默认值。
impl Default for super::OpenAiCompatibleConfig {
    #[doc(hidden)]
    fn default() -> Self {
        Self {
            provider_id: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            model: String::new(),
            timeout_secs: 30,
            temperature: None,
            sampling: super::SamplingParams::default(),
            network: crate::goal::NetworkPolicy::On,
            native_search_enabled: true,
            fallback_model: None,
            context_tokens: None,
            output_tokens: None,
            supports_images_override: None,
        }
    }
}

/// 按 model 名种子表 + provider_id 家族猜默认值，除非配置显式覆盖了 supports_images
/// （显式覆盖永远压过种子表——见 `crate::image::resolve_default_supports_images`）。
pub(crate) fn resolve_supports_images(config: &super::OpenAiCompatibleConfig) -> bool {
    config.supports_images_override.unwrap_or_else(|| {
        crate::image::resolve_default_supports_images(&config.provider_id, &config.model)
    })
}

pub(super) fn apply_image_content(body: &mut Value, wire_messages: &[ChatMessage]) {
    for (i, message) in wire_messages.iter().enumerate() {
        if message.images.is_empty() {
            continue;
        }
        // `serde_json::to_value(&wire_messages)` 已经把 canonical `images` 字段（含本机
        // 绝对路径 `source_path`）序列化进了这个 message 对象——不管下面是否真的重拼了
        // content，这个内部字段的兄弟键都必须删掉，否则它会随每次请求原样发给外部
        // provider（协议上也没有这个字段，严格网关会 400）。
        if let Some(obj) = body["messages"][i].as_object_mut() {
            obj.remove("images");
        }
        // 图片目前只该挂在 user 消息上；任何别的角色带图都不当多模态处理——OpenAI
        // tool message 的 content 协议上必须是字符串，硬拼数组会被严格端拒绝。
        if message.role != "user" {
            crate::image::debug_log(&format!(
                "ignoring {} non-empty images on non-user role `{}` (images only apply to user messages)",
                message.images.len(),
                message.role
            ));
            continue;
        }
        let mut parts: Vec<Value> = Vec::with_capacity(1 + message.images.len());
        if let Some(text) = &message.content {
            parts.push(json!({"type": "text", "text": text}));
        }
        for image in &message.images {
            // 出线前最后一道保险丝：data_base64 为空（重读失败/未来任何漏管的代码
            // 路径）绝不发这张图出去——空 data URI 真 API 会 400，而且会把这个坏
            // 块永久钉进历史。
            if image.data_base64.is_empty() {
                crate::image::debug_log(&format!(
                    "dropping image with empty data_base64 before wire (media_type={})",
                    image.media_type
                ));
                continue;
            }
            parts.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!(
                        "data:{};base64,{}",
                        image.media_type, image.data_base64
                    )
                }
            }));
        }
        body["messages"][i]["content"] = Value::Array(parts);
    }
}
