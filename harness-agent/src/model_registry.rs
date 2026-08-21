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
        // DeepSeek V4 系（v4-flash / v4-pro，含 -0731/-0813 等日期快照）官方 API 均为
        // 1,048,576(1M) 窗口；旧值 65,536 是 V3 时代残留，写小 16 倍——真机 3 个 run
        // 在 52815 token 就被引擎误判 context_budget_exhausted 硬停。
        // 窗口来源：api-docs.deepseek.com/news/news260424/（"1M context is now the
        // default across all official DeepSeek services"）+ huggingface.co/deepseek-ai/
        // DeepSeek-V4-Pro 模型卡（"context length of one million tokens"）+
        // openrouter.ai/api/v1/models 实时聚合（v4-pro/v4-flash 均报 context_length=
        // 1,048,576），2026-08-21 核。
        // max_output=65,536：官方 API 实际允许上限约 384K~393,216（openrouter
        // top_provider.max_completion_tokens，对应官方 Think Max 推理档），但登记表
        // 默认值不必顶格——64K 已是旧值的 8 倍、足够思考模型完整输出，且在 1,048,576
        // 窗口下仍留 ~93% 预算给历史；要跑 Think Max 满血输出可走 per-provider 覆盖。
        return Some(model_spec_full(1_048_576, 65_536, true));
    }

    match provider_family(provider_id) {
        ProviderFamily::Kimi => Some(kimi_spec(&model)),
        ProviderFamily::Glm => Some(glm_spec(&model)),
        ProviderFamily::Qwen => Some(model_spec(131_072, false)),
        ProviderFamily::Generic => None,
    }
}

/// GLM 家族窗口随版本大幅漂移，不能一刀切（否则老款被声称过量窗口·新款被腰斩）：
///   5.2 起步 1M 窗口 / 128K(131,072) 输出（docs.z.ai/guides/llm/glm-5.2，2026-08-21 核）；
///   4.6 / 5 / 5-turbo 现役主线 200K 窗口 / 128K(131,072) 输出
///   （docs.z.ai/guides/llm/{glm-4.6,glm-5,glm-5-turbo}，2026-08-21 核，原值 128_000 是
///   旧登记值残留）；
///   4-plus 等旧款维持原 128K 窗口 / 8,192 输出（未核实其新输出上限，保守不动——顶格
///   到 131,072 会让 max_output > context_window，把 budget() 算成 0）。
/// 未识别型号落中间档 200K，不给 1M——防止对实际仍是旧款的型号声称过量窗口被 API 拒。
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
        // 来源：api-docs.deepseek.com/news/news260424/ + huggingface.co/deepseek-ai/
        // DeepSeek-V4-Pro 模型卡 + openrouter.ai 实时聚合，2026-08-21 核。
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
        // 来源：docs.z.ai/guides/llm/{glm-4.6,glm-5,glm-5-turbo}，2026-08-21 核。
        for model in ["glm-4.6", "glm-5", "glm-5-turbo"] {
            let s = lookup("glm", model).unwrap_or_else(|| panic!("{model} in table"));
            assert_eq!(s.context_window, 200_000, "{model}");
            assert_eq!(s.max_output, 131_072, "{model}");
        }
    }

    #[test]
    fn lookup_glm_5_2_gets_1m_window() {
        // 来源：docs.z.ai/guides/llm/glm-5.2，2026-08-21 核。
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
