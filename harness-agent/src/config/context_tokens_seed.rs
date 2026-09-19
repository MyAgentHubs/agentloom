//! `default_context_tokens` 的 model-name 兜底种子表——拆出独立文件（避免
//! `config.rs` 继续超出 `tests/file_size_ratchet.rs` 的白名单上限：棘轮只许降不许
//! 升，不改白名单）。`model_registry` 查不到时才落这里（典型：provider 是
//! `ProviderFamily::Generic`，如 "zai"/"openai"/"anthropic"/自定义网关）。
//!
//! 按 model 名而非 provider 判——同一 provider_id 下不同 model 窗口可以差 16 倍
//! （kimi 就是反例），provider 子串一刀切会把 registry 答对的盖掉、也会把无关
//! provider 错判成某家族（t12-img 第四轮返工 P2-A）。每档数字附官方文档出处 + 核对
//! 日期；查不到出处或存疑的一律不进这张表、留 `None` 走
//! `context_budget::DEFAULT_CONTEXT_TOKENS` 通用保守默认。
pub(super) fn seed_by_model(model: &str) -> Option<u32> {
    let m = model.to_ascii_lowercase();
    // gpt-4o / gpt-4.1（含 -mini/-nano 等变体）/ o1 / o3 推理家族：128K。
    // 来源：platform.openai.com/docs/models，2026-09-18 核（按模型名前缀匹配，不含更新
    // 换代型号的场景不在此表覆盖范围内，届时以 model_registry 精细登记为准）。
    if m.contains("gpt-4o") || m.contains("gpt-4.1") || m.starts_with("o1") || m.starts_with("o3") {
        return Some(128_000);
    }
    // 老款 gpt-4（非 4o/4.1 的基线版本）：8K。
    // 来源：platform.openai.com/docs/models（legacy gpt-4 系列标称 8K 上下文），2026-09-18 核。
    if m.contains("gpt-4") {
        return Some(8_192);
    }
    // gpt-3.5-turbo：16K（现役主线版本）。
    // 来源：platform.openai.com/docs/models，2026-09-18 核。
    if m.contains("gpt-3.5") {
        return Some(16_384);
    }
    // claude-*：200K（Opus/Sonnet/Haiku 现役各代标称至少 200K）。
    // 来源：docs.anthropic.com/en/docs/about-claude/models，2026-09-18 核。
    if m.contains("claude") {
        return Some(200_000);
    }
    // glm-4.6 / glm-5 系——仅当 provider 不属于 `ProviderFamily::Glm`（registry 查不到）
    // 时才会落到这里，典型是 "zai"（默认模型即 "glm-4.6"）。200K 对齐
    // `model_registry::glm_spec` 主线档；注意 glm-5.2 真实窗口是 1M，但那只在 provider_id
    // 能被识别成 Glm 家族时才由 registry 精确给出，这里是粗粒度兜底、已知偏保守。
    // 来源：docs.z.ai/guides/llm/{glm-4.6,glm-5,glm-5-turbo}，2026-08-21 核（沿用
    // model_registry.rs 已核对日期）。
    if m.contains("glm-4.6") || m.contains("glm-5") {
        return Some(200_000);
    }
    None
}
