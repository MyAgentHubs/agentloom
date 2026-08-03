use crate::provider::native_search::{provider_family, ProviderFamily};

/// 这个模型偏好哪种改文件方式（ModelAdapter 行为档案的第一个真字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFormat {
    /// 引导走定点编辑 + 大文件整写硬拦截生效（默认·所有模型出厂值）。
    Targeted,
    /// 放宽：大文件整写拦截不生效（留给确有大输出预算的模型/档位）。
    WholeFileOk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub context_window: u32,
    pub max_output: u32,
    pub supports_reasoning: bool,
    pub supports_reasoning_deltas: bool,
    pub supports_streaming: bool,
    pub supports_function_calling: bool,
    pub edit_format: EditFormat,
}

/// 按 provider_id + model 查表。deepseek 钉死；别家按 ProviderFamily + model 名子串。
/// 查不到返回 None，让调用方走各自保守默认。
pub fn lookup(provider_id: &str, model: &str) -> Option<ModelSpec> {
    let provider = provider_id.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();

    if provider.contains("deepseek") || model.contains("deepseek") {
        return Some(model_spec(65_536, true));
    }

    match provider_family(provider_id) {
        ProviderFamily::Kimi => Some(kimi_spec(&model)),
        ProviderFamily::Glm => Some(model_spec(128_000, false)),
        ProviderFamily::Qwen => Some(model_spec(131_072, false)),
        ProviderFamily::Generic => None,
    }
}

fn kimi_spec(model: &str) -> ModelSpec {
    let reasoning = contains_any(model, &["k2.5", "k2.6", "k2-thinking"]);

    let context_window = if model.contains("k2-thinking") {
        131_072
    } else if contains_any(model, &["k2-0905", "turbo", "k2.5", "k2.6"]) {
        262_144
    } else if contains_any(model, &["128k", "latest", "k2-0711"]) {
        131_072
    } else if model.contains("32k") {
        32_768
    } else if model.contains("8k") {
        8_192
    } else {
        131_072
    };

    model_spec(context_window, reasoning)
}

fn model_spec(context_window: u32, supports_reasoning: bool) -> ModelSpec {
    ModelSpec {
        context_window,
        max_output: 8_192,
        supports_reasoning,
        supports_reasoning_deltas: supports_reasoning,
        supports_streaming: true,
        supports_function_calling: true,
        edit_format: EditFormat::Targeted,
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::lookup;

    #[test]
    fn lookup_deepseek_matches_current_65536() {
        let s = lookup("deepseek", "deepseek-v4-flash").expect("deepseek in table");
        assert_eq!(s.context_window, 65_536); // 桶二·钉死现有值·别用 LiteLLM 的 1M
        assert_eq!(s.max_output, 8_192);
        assert!(s.supports_reasoning);
    }

    #[test]
    fn lookup_kimi_128k_real_window_sane_output() {
        let s = lookup("kimi", "moonshot-v1-128k").expect("kimi in table");
        assert_eq!(s.context_window, 131_072);
        assert_eq!(s.max_output, 8_192); // 预留·非 LiteLLM 的满窗口(否则预算归零)
        assert!(!s.supports_reasoning);
    }

    #[test]
    fn lookup_sets_targeted_edit_format_by_default() {
        use super::EditFormat;
        assert_eq!(
            lookup("kimi", "moonshot-v1-128k").unwrap().edit_format,
            EditFormat::Targeted
        );
        assert_eq!(
            lookup("deepseek", "deepseek-v4-flash").unwrap().edit_format,
            EditFormat::Targeted
        );
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("whatever", "mystery-model").is_none());
    }
}
