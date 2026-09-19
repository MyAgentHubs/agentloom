//! t12-img 第四轮返工（P2-A）：`default_context_tokens` 从「provider_id 子串一刀切、短路在
//! `model_registry::lookup` 之前」改回「`model_registry::lookup` 优先，查不到再按 model 名
//! 落种子表」。这里钉住差分语料里最关键的几条回归——registry 本来答对的不许被种子表盖掉、
//! 「DeepSeek 借壳 Claude Code」那条 1M 窗口不许被腰斩、明显没登记过的本地/代理端点不许被
//! 瞎填高。挂在 `config::tests` 下（`use super::*;` 复用 `default_context_tokens`）。
use super::*;

#[test]
fn registry_answer_not_overridden_by_provider_seed_table() {
    // kimi 家族走 model_registry 精细登记（moonshot-v1-8k = 8192），provider_id 里带
    // "openai" 子串不该短路掉这个答案（P2-A 曾把它盖成 128000，15.6x 过报）。
    assert_eq!(
        default_context_tokens("openai-kimi-proxy", "moonshot-v1-8k"),
        Some(8_192)
    );
}

#[test]
fn deepseek_via_claude_code_shim_keeps_1m_window() {
    // CLAUDE.md：「DeepSeek『借壳』经 Claude Code 接入」——provider_id 带 "claude" 子串
    // 不该把 model_registry 已确认的 1,048,576 窗口腰斩成 200_000（P2-A 曾误伤这条）。
    assert_eq!(
        default_context_tokens("claude-code-deepseek", "deepseek-v4-pro"),
        Some(1_048_576)
    );
}

#[test]
fn openai_legacy_gpt4_uses_8k_not_widened_seed() {
    // 老款 gpt-4（非 4o/4.1）真实窗口 8K；不该被 provider 子串一刀切成 128K。
    assert_eq!(default_context_tokens("openai", "gpt-4"), Some(8_192));
}

#[test]
fn unregistered_local_gateway_model_stays_none() {
    // 本地/代理网关（LM Studio 等）明显没在任何登记表里，必须留 None 走通用保守默认，
    // 不许被 provider_id 里带 "gpt" 子串瞎填高成 128K。
    assert_eq!(
        default_context_tokens("lmstudio-gpt", "phi-3-mini-4k"),
        None
    );
}
