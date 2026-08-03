//! provider 家族识别 + 原生服务端搜索的「这轮开不开」判据（纯逻辑·可单测）。
//! 各家请求体注入在后续 task 加（apply_* / kimi_*）。

use crate::goal::NetworkPolicy;
use serde_json::{json, Value};

/// provider「家族」——决定有没有原生服务端搜索 + 怎么开。按 provider_id 子串识别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFamily {
    Glm,
    Qwen,
    Kimi,
    Generic,
}

impl ProviderFamily {
    /// 这家 provider 的服务端是否有原生搜索（真·静态能力·与网/开关无关）。
    pub fn has_native_search(self) -> bool {
        matches!(
            self,
            ProviderFamily::Glm | ProviderFamily::Qwen | ProviderFamily::Kimi
        )
    }
}

/// 按 provider_id（小写·子串包含·带别名）识别家族。未知一律 Generic（安全降级到内置 web_search）。
pub fn provider_family(provider_id: &str) -> ProviderFamily {
    let id = provider_id.to_ascii_lowercase();
    let has = |needle: &str| id.contains(needle);
    if has("glm") || has("zhipu") {
        ProviderFamily::Glm
    } else if has("qwen") || has("dashscope") || has("qwq") {
        ProviderFamily::Qwen
    } else if has("kimi") || has("moonshot") {
        ProviderFamily::Kimi
    } else {
        ProviderFamily::Generic
    }
}

/// 「这轮原生搜索到底开不开」+ 为什么（利于日志/测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSearchState {
    DisabledByNoCapability,
    DisabledByNetwork,
    DisabledByUser,
    Active,
}

/// 唯一判据·两处共用（provider 拼请求 / orchestrator 防双份）·输入须同源（run 的 network + run 的开关）。
pub fn native_search_state(
    has_native: bool,
    network: NetworkPolicy,
    user_enabled: bool,
) -> NativeSearchState {
    if !has_native {
        return NativeSearchState::DisabledByNoCapability;
    }
    if network == NetworkPolicy::Off {
        return NativeSearchState::DisabledByNetwork;
    }
    if !user_enabled {
        return NativeSearchState::DisabledByUser;
    }
    NativeSearchState::Active
}

/// GLM：tools 追加 web_search 服务端搜索项（不覆盖已有 tools）。
/// 国内 bigmodel.cn 形状：https://docs.bigmodel.cn/cn/guide/tools/web-search
/// 国际 z.ai 形状：https://docs.z.ai/api-reference/llm/chat-completion
pub fn apply_glm(body: &mut Value, base_url: &str) {
    let item = if base_url.contains("z.ai") {
        json!({
            "type": "web_search",
            "web_search": { "enable": true, "search_engine": "search_pro_jina", "search_result": true }
        })
    } else {
        json!({
            "type": "web_search",
            "web_search": { "enable": "True", "search_engine": "search_pro", "search_result": "True" }
        })
    };
    match body.get_mut("tools").and_then(|t| t.as_array_mut()) {
        Some(arr) => arr.push(item),
        None => body["tools"] = json!([item]),
    }
}

/// Qwen：body 加 enable_search + search_options（OpenAI 兼容模式）。
/// https://www.alibabacloud.com/help/en/model-studio/web-search
pub fn apply_qwen(body: &mut Value) {
    body["enable_search"] = json!(true);
    body["search_options"] = json!({ "search_strategy": "agent" });
}

/// Kimi $web_search builtin_function 工具项。
pub fn kimi_tool_def() -> Value {
    json!({ "type": "builtin_function", "function": { "name": "$web_search" } })
}

/// 关 Kimi 思考模式（与 $web_search 互斥）。
/// https://platform.moonshot.ai/docs/guide/use-web-search
pub fn disable_thinking(body: &mut Value) {
    body["thinking"] = json!({ "type": "disabled" });
}

/// 是否 Kimi 内部回声触发的搜索调用。
pub fn is_kimi_web_search(name: &str) -> bool {
    name == "$web_search"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::NetworkPolicy;
    use serde_json::json;

    #[test]
    fn family_recognizes_aliases_and_substrings() {
        assert_eq!(provider_family("glm"), ProviderFamily::Glm);
        assert_eq!(provider_family("zhipu"), ProviderFamily::Glm);
        assert_eq!(provider_family("glm-prod"), ProviderFamily::Glm); // 子串
        assert_eq!(provider_family("qwen"), ProviderFamily::Qwen);
        assert_eq!(provider_family("dashscope"), ProviderFamily::Qwen);
        assert_eq!(provider_family("qwq-32b"), ProviderFamily::Qwen);
        assert_eq!(provider_family("kimi"), ProviderFamily::Kimi);
        assert_eq!(provider_family("moonshot-v1"), ProviderFamily::Kimi);
        assert_eq!(provider_family("deepseek"), ProviderFamily::Generic);
        assert_eq!(provider_family("openai"), ProviderFamily::Generic);
        assert_eq!(provider_family("grok"), ProviderFamily::Generic);
        assert_eq!(provider_family("totally-unknown"), ProviderFamily::Generic);
    }

    #[test]
    fn has_native_search_only_for_three() {
        assert!(ProviderFamily::Glm.has_native_search());
        assert!(ProviderFamily::Qwen.has_native_search());
        assert!(ProviderFamily::Kimi.has_native_search());
        assert!(!ProviderFamily::Generic.has_native_search());
    }

    #[test]
    fn native_state_truth_table() {
        use NativeSearchState::*;
        // 无能力 → 永远 DisabledByNoCapability（无论网/开关）
        assert_eq!(
            native_search_state(false, NetworkPolicy::On, true),
            DisabledByNoCapability
        );
        assert_eq!(
            native_search_state(false, NetworkPolicy::Off, false),
            DisabledByNoCapability
        );
        // 有能力：网关 > 用户关 > Active（优先级）
        assert_eq!(
            native_search_state(true, NetworkPolicy::Off, true),
            DisabledByNetwork
        );
        // network off 优先于 user off（钉死优先级·防先判 user 的错误实现）
        assert_eq!(
            native_search_state(true, NetworkPolicy::Off, false),
            DisabledByNetwork
        );
        assert_eq!(
            native_search_state(true, NetworkPolicy::On, false),
            DisabledByUser
        );
        assert_eq!(native_search_state(true, NetworkPolicy::On, true), Active);
    }

    #[test]
    fn apply_glm_appends_bigmodel_web_search_tool_item() {
        let mut body = json!({ "model": "glm-4-plus", "tools": [{"type":"function","function":{"name":"shell_exec"}}] });
        apply_glm(&mut body, "https://open.bigmodel.cn/api/paas/v4");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2); // 普通工具仍在
        assert!(tools.iter().any(|t| {
            t["type"] == "web_search"
                && t["web_search"]["enable"] == json!("True")
                && t["web_search"]["search_engine"] == "search_pro"
                && t["web_search"]["search_result"] == json!("True")
        }));
    }

    #[test]
    fn apply_glm_uses_zai_boolean_shape_and_jina_engine() {
        let mut body = json!({ "model": "glm-4.5" });
        apply_glm(&mut body, "https://api.z.ai/api/paas/v4");
        let web_search = &body["tools"][0]["web_search"];
        assert_eq!(web_search["enable"], json!(true));
        assert_eq!(web_search["search_engine"], json!("search_pro_jina"));
        assert_eq!(web_search["search_result"], json!(true));
    }

    #[test]
    fn apply_glm_unknown_base_url_defaults_to_bigmodel_shape() {
        let mut body = json!({ "model": "glm-4-plus" });
        apply_glm(&mut body, "https://example.test/v1");
        let tools = body["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["type"] == "web_search"));
        let web_search = &tools[0]["web_search"];
        assert_eq!(web_search["enable"], json!("True"));
        assert_eq!(web_search["search_engine"], json!("search_pro"));
        assert_eq!(web_search["search_result"], json!("True"));
    }

    #[test]
    fn apply_qwen_sets_enable_search_and_options() {
        let mut body = json!({ "model": "qwen-plus" });
        apply_qwen(&mut body);
        assert_eq!(body["enable_search"], true);
        assert_eq!(body["search_options"]["search_strategy"], "agent");
    }

    #[test]
    fn disable_thinking_uses_kimi_thinking_object() {
        let mut body = json!({ "model": "kimi-k2.5" });
        disable_thinking(&mut body);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("enable_thinking").is_none());
    }
}
