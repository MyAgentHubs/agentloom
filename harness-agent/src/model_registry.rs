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
        // All DeepSeek V4 variants (v4-flash / v4-pro, including dated snapshot builds such as -0731/-0813)
        // advertise a 1,048,576 (1M) window on the official API. The old 65,536 value was a V3-era remnant,
        // undersizing it by 16x: three real-device runs were incorrectly hard-stopped by the engine with
        // context_budget_exhausted at 52815 tokens. Window size verified against:
        // api-docs.deepseek.com/news/news260424/ ("1M context is now the default across all official DeepSeek services"),
        // huggingface.co/deepseek-ai/DeepSeek-V4-Pro model card ("context length of one million tokens"), and
        // openrouter.ai/api/v1/models live aggregation (both v4-pro/v4-flash report context_length=1,048,576).
        // max_output=65,536 is a deliberate default: the official API permits roughly 384K~393,216 tokens
        // (OpenRouter top_provider.max_completion_tokens, corresponding to the official Think Max reasoning tier),
        // but the registry need not default to that maximum. 64K is already 8x the old value, supports complete
        // reasoning-model output, and leaves about 93% of the 1,048,576 window for history.
        // Use a per-provider override for the full Think Max output allowance.
        return Some(model_spec_full(1_048_576, 65_536, true));
    }

    match provider_family(provider_id) {
        ProviderFamily::Kimi => Some(kimi_spec(&model)),
        ProviderFamily::Glm => Some(glm_spec(&model)),
        ProviderFamily::Qwen => Some(model_spec(131_072, false)),
        ProviderFamily::Generic => None,
    }
}

/// GLM windows vary greatly by version; one size would overstate older models' capacity or halve newer models' windows:
///   5.2 onward starts at a 1M window / 128K (131,072) output (docs.z.ai/guides/llm/glm-5.2);
///   4.6 / 5 / 5-turbo mainline models have a 200K window / 128K (131,072) output
///   (docs.z.ai/guides/llm/{glm-4.6,glm-5,glm-5-turbo}; the old 128_000 output value was a stale registry entry);
///   older models such as 4-plus retain their original 128K window / 8,192 output because newer output limits
///   are unverified; raising output to 131,072 would make max_output > context_window and budget() return 0.
/// Unknown models fall back to the middle 200K tier, not 1M, to avoid API rejections from overstating
/// the capacity of a model that may actually belong to an older generation.
fn glm_spec(model: &str) -> ModelSpec {
    if model.contains("5.2") {
        model_spec_full(1_000_000, 131_072, false)
    } else if model.contains("4-plus") {
        model_spec_full(128_000, 8_192, false)
    } else {
        model_spec_full(200_000, 131_072, false)
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
    model_spec_full(context_window, 8_192, supports_reasoning)
}

fn model_spec_full(context_window: u32, max_output: u32, supports_reasoning: bool) -> ModelSpec {
    ModelSpec {
        context_window,
        max_output,
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
    fn lookup_deepseek_matches_official_1m_window() {
        // Sources: api-docs.deepseek.com/news/news260424/, the huggingface.co/deepseek-ai/DeepSeek-V4-Pro
        // model card, and openrouter.ai live aggregation.
        let s = lookup("deepseek", "deepseek-v4-flash").expect("deepseek in table");
        assert_eq!(s.context_window, 1_048_576);
        assert_eq!(s.max_output, 65_536);
        assert!(s.supports_reasoning);
    }

    #[test]
    fn lookup_deepseek_v4_pro_window_regression_guard() {
        // 钉子测试：防回归——deepseek 窗口不得再被写小回 v3 时代的 65,536。
        let s = lookup("deepseek", "deepseek-v4-pro").expect("deepseek in table");
        assert!(
            s.context_window >= 1_000_000,
            "deepseek-v4-pro context_window regressed below 1M: {}",
            s.context_window
        );
    }

    #[test]
    fn lookup_glm_mainline_200k_window_131072_output() {
        // Sources: docs.z.ai/guides/llm/{glm-4.6,glm-5,glm-5-turbo}.
        for model in ["glm-4.6", "glm-5", "glm-5-turbo"] {
            let s = lookup("glm", model).unwrap_or_else(|| panic!("{model} in table"));
            assert_eq!(s.context_window, 200_000, "{model}");
            assert_eq!(s.max_output, 131_072, "{model}");
        }
    }

    #[test]
    fn lookup_glm_5_2_gets_1m_window() {
        // Source: docs.z.ai/guides/llm/glm-5.2.
        let s = lookup("glm", "glm-5.2").expect("glm in table");
        assert_eq!(s.context_window, 1_000_000);
        assert_eq!(s.max_output, 131_072);
    }

    #[test]
    fn lookup_glm_4_plus_legacy_window_unchanged() {
        // 旧款窗口维持 128K，不随主线一起涨到 200K（避免对旧款声称过量窗口被 API 拒）。
        let s = lookup("glm", "glm-4-plus").expect("glm in table");
        assert_eq!(s.context_window, 128_000);
        assert_eq!(s.max_output, 8_192);
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
