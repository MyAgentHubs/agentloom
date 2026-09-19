#![cfg(test)]

use super::*;
use std::io::Write;

fn valid_draft_json() -> &'static str {
    r#"{"goal":"加登录页","subtasks":[{"id":"s1","desc":"加表单","scope_files":["a.rs"],"acceptance":[{"claim":"测试过","verifier":"cargo test"}],"needed_caps":[]}],"assignments":[{"subtask_id":"s1","agent_id":"claude-1"}]}"#
}

#[test]
fn lead_draft_sys_prompt_forbids_aggregator_meta_subtask() {
    let p = LEAD_DRAFT_SYS_PROMPT;
    assert!(!p.contains("Language:"), "语言指令不得写入 sys prompt 常量");
    // 锚「依赖其他队员/子任务产出的汇聚元任务」语义·非「汇总」关键词
    assert!(
        p.contains("other subtasks") || p.contains("other workers") || p.contains("worker outputs")
    );
    assert!(p.contains("closeout stage") || p.contains("system")); // 汇总归确定性收尾步
                                                                   // 豁免：用户本就要的报告/总结交付不拦
    assert!(p.contains("report") || p.contains("deliverable"));
}

#[test]
fn lead_draft_sys_prompt_constrains_goal_to_one_sentence_smart() {
    let p = LEAD_DRAFT_SYS_PROMPT;
    // goal 要求一句话 SMART·别照抄原话/罗列路径背景
    assert!(p.contains("one-sentence") || p.contains("one sentence"));
    assert!(p.contains("Do not copy"));
    assert!(
        p.contains("~30 characters for Chinese") && p.contains("~10 words for English"),
        "goal length guidance must state equivalent Chinese and English constraints"
    );
    // 验收 claim 用用户能观察到的结果大白话
    assert!(p.contains("users can observe"));
    // 写硬：goal 不收执行细节（条件分支/路径/格式），那些进 subtasks/acceptance
    assert!(p.contains("Do not put") && p.contains("in goal"));
}

/// 写类 verifier 的 draft JSON（tier1 fixture·T2 起 valid_draft_json 判 tier0 不再适用落库断言）
fn write_like_draft_json() -> &'static str {
    r#"{"goal":"改格式","subtasks":[{"id":"s1","desc":"格式化","scope_files":["a.rs"],"acceptance":[{"claim":"格式过","verifier":"rustfmt a.rs"}],"needed_caps":[]}],"assignments":[{"subtask_id":"s1","agent_id":"claude-1"}]}"#
}

/// 造一个吐「claude result 信封」的假 driver：result 字段 = 给定 final_text。
/// 用 `cat <tempfile>`·避免 /bin/sh printf 的嵌套 JSON 转义地狱。
fn fake_driver_emitting(
    final_text: &str,
) -> (
    tempfile::NamedTempFile,
    impl Fn() -> Result<std::process::Child, String>,
) {
    let line = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": final_text,
        "total_cost_usd": 0.01,
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })
    .to_string();
    let mut tf = tempfile::NamedTempFile::new().unwrap();
    writeln!(tf, "{line}").unwrap();
    let path = tf.path().to_path_buf();
    let spawn = move || -> Result<std::process::Child, String> {
        std::process::Command::new("cat")
            .arg(&path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())
    };
    (tf, spawn)
}

fn fake_codex_driver_emitting_text_delta(
    text: &str,
) -> (
    tempfile::NamedTempFile,
    impl Fn() -> Result<std::process::Child, String>,
) {
    fake_codex_driver_emitting_text_deltas(vec![text.to_string()])
}

fn fake_codex_driver_emitting_text_deltas(
    texts: Vec<String>,
) -> (
    tempfile::NamedTempFile,
    impl Fn() -> Result<std::process::Child, String>,
) {
    let completed = serde_json::json!({
        "type": "turn.completed",
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })
    .to_string();
    let mut tf = tempfile::NamedTempFile::new().unwrap();
    for (idx, text) in texts.into_iter().enumerate() {
        let agent_message = serde_json::json!({
            "type": "item.completed",
            "item": {
                "id": format!("item-{idx}"),
                "type": "agent_message",
                "text": text
            }
        })
        .to_string();
        writeln!(tf, "{agent_message}").unwrap();
    }
    writeln!(tf, "{completed}").unwrap();
    let path = tf.path().to_path_buf();
    let spawn = move || -> Result<std::process::Child, String> {
        std::process::Command::new("cat")
            .arg(&path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())
    };
    (tf, spawn)
}

fn agent_profile(
    id: &str,
    enabled: bool,
    cap_reasoning: Option<&str>,
    sort_order: i64,
) -> crate::db::AgentProfile {
    crate::db::AgentProfile {
        id: id.into(),
        name: format!("Agent {id}"),
        access: "native".into(),
        provider: "claude".into(),
        primary_model: Some("claude-opus".into()),
        endpoint: None,
        auth_mode: None,
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "high".into(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: cap_reasoning.map(|s| s.to_string()),
        cap_computer_use: None,
        cap_lead: None,
        has_key: true,
        is_builtin: false,
        enabled,
        sort_order,
        created_at: 0,
        updated_at: 0,
    }
}

fn mem_db_with_agent() -> crate::db::Db {
    let conn = crate::test_support::mem_db();
    crate::db::upsert_agent(&conn, &agent_profile("claude-1", true, Some("native"), 0)).unwrap();
    crate::db::Db(crate::perf_probe::TimedMutex::new(conn))
}

fn draft_with(scope_total: usize, verifier: Option<&str>) -> DriverDraftOutput {
    let files: Vec<String> = (0..scope_total).map(|i| format!("f{i}.rs")).collect();
    DriverDraftOutput {
        goal: "g".into(),
        subtasks: vec![DraftSubtask {
            id: "s1".into(),
            desc: "d".into(),
            scope_files: files,
            acceptance: vec![DraftCriterion {
                claim: "c".into(),
                verifier: verifier.map(|s| s.to_string()),
            }],
            needed_caps: vec![],
        }],
        tier: None,
        assignments: vec![],
    }
}

// 风险档（disagreement 固定低·测 risk 维度·existing_files 直传旧 total 同值·验档位表不变）
#[test]
fn estimate_risk_low_med_high_by_signals() {
    assert_eq!(
        estimate_tier(&draft_with(2, Some("cargo test")), 0.0, 2).risk_level,
        "low"
    );
    assert_eq!(
        estimate_tier(&draft_with(1, Some("git apply patch")), 0.0, 1).risk_level,
        "med"
    );
    assert_eq!(
        estimate_tier(&draft_with(5, Some("cargo test")), 0.0, 5).risk_level,
        "med"
    );
    assert_eq!(
        estimate_tier(&draft_with(11, Some("cargo test")), 0.0, 11).risk_level,
        "high"
    );
}

// 决策表三档（disagreement 入参驱动·覆盖 Tier0/1/2）
#[test]
fn estimate_tier0_low_risk_low_disagreement() {
    // 低风险 + 低分歧（B4 真采样场景）→ tier0
    assert_eq!(
        estimate_tier(&draft_with(2, Some("cargo test")), 0.0, 2).tier,
        "tier0"
    );
}

#[test]
fn estimate_tier1_middle_band() {
    // 低风险 + B1 占位分歧 0.3（不<0.20·不≥0.50）→ tier1（B1 期间 low 风险也至少 Tier1）
    assert_eq!(
        estimate_tier(&draft_with(2, Some("cargo test")), 0.3, 2).tier,
        "tier1"
    );
    // 中风险 + 低分歧 → tier1
    assert_eq!(
        estimate_tier(&draft_with(5, Some("cargo test")), 0.0, 5).tier,
        "tier1"
    );
}

#[test]
fn estimate_tier2_high_risk_or_high_disagreement() {
    // 高风险（>10 文件）→ tier2
    assert_eq!(
        estimate_tier(&draft_with(11, Some("cargo test")), 0.0, 11).tier,
        "tier2"
    );
    // 高分歧（≥0.50）→ tier2（即便低风险·B4 场景）
    assert_eq!(
        estimate_tier(&draft_with(1, Some("cargo test")), 0.6, 1).tier,
        "tier2"
    );
}

#[test]
fn read_only_no_existing_scope_unlocks_tier0_research_draft() {
    // existing=0 + 只读 verifier → 放行
    assert!(draft_is_read_only_no_existing_scope(
        &draft_with(1, Some("cargo test")),
        0
    ));
    // verifier 为 None（无命令）+ existing=0 → 放行
    assert!(draft_is_read_only_no_existing_scope(
        &draft_with(0, None),
        0
    ));
}

#[test]
fn write_like_or_existing_file_keeps_placeholder() {
    // 写类 verifier → 不放行（哪怕 existing=0）
    assert!(!draft_is_read_only_no_existing_scope(
        &draft_with(1, Some("rustfmt a.rs")),
        0
    ));
    // 触达已存在文件（existing>0）→ 不放行
    assert!(!draft_is_read_only_no_existing_scope(
        &draft_with(1, Some("cargo test")),
        1
    ));
}

#[test]
fn four_new_output_files_read_only_lands_tier0() {
    // GUI 验收场景：4 个子任务各 1 个不存在的产出 md + 只读 verifier → exists=0 → 放行 → tier0
    let dir = tempfile::tempdir().unwrap();
    let draft = DriverDraftOutput {
        goal: "g".into(),
        subtasks: (0..4)
            .map(|i| DraftSubtask {
                id: format!("s{i}"),
                desc: "d".into(),
                scope_files: vec![format!("research/f{i}.md")],
                acceptance: vec![DraftCriterion {
                    claim: "c".into(),
                    verifier: Some("test $(grep -c x f) -ge 3".into()),
                }],
                needed_caps: vec![],
            })
            .collect(),
        tier: None,
        assignments: vec![],
    };
    let existing = count_existing_scope_files(&draft, dir.path());
    assert_eq!(existing, 0);
    assert!(draft_is_read_only_no_existing_scope(&draft, existing));
    assert_eq!(estimate_tier(&draft, 0.0, existing).tier, "tier0");
}

#[test]
fn touching_existing_file_blocks_tier0_unlock() {
    // 重构已有文件（哪怕只读 verifier）→ 不放行 → 至少 tier1
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "x").unwrap();
    let draft = draft_with(1, Some("cargo test")); // scope_files = ["f0.rs"]——不命中
                                                   // 用真实存在的文件名造 draft
    let mut d = draft;
    d.subtasks[0].scope_files = vec!["a.rs".into()];
    let existing = count_existing_scope_files(&d, dir.path());
    assert_eq!(existing, 1);
    assert!(!draft_is_read_only_no_existing_scope(&d, existing));
    assert_eq!(
        estimate_tier(&d, tier_const::B1_PLACEHOLDER_DISAGREEMENT, existing).tier,
        "tier1"
    );
}

#[test]
fn absolute_scope_paths_are_ignored() {
    // Path::join 遇绝对路径替换 base——绝对路径一律不算（repo 外不进风险口径）
    let dir = tempfile::tempdir().unwrap();
    let mut d = draft_with(1, Some("cargo test"));
    d.subtasks[0].scope_files = vec!["/etc/hosts".into(), "/nonexistent/x".into()];
    assert_eq!(count_existing_scope_files(&d, dir.path()), 0);
}

#[test]
fn duplicate_scope_paths_counted_once() {
    // 同一路径横跨多个 subtask 只数一次（NIT-2 覆盖去重）
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "x").unwrap();
    let mut d = draft_with(1, Some("cargo test"));
    d.subtasks[0].scope_files = vec!["a.rs".into(), "a.rs".into()];
    assert_eq!(count_existing_scope_files(&d, dir.path()), 1);
}

#[test]
fn b1_placeholder_disagreement_is_point_three() {
    assert_eq!(tier_const::B1_PLACEHOLDER_DISAGREEMENT, 0.3);
}

fn sample_assignee() -> Assignee {
    Assignee {
        agent_id: "claude-1".into(),
        provider: "claude".into(),
        model: "claude-opus".into(),
    }
}

#[test]
fn build_assignments_json_shapes_per_unit() {
    let draft = draft_with(1, Some("cargo test"));
    let picks = vec![("s1".to_string(), Some(sample_assignee()))];
    let json = build_assignments_json(&draft, &picks);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["subtask_id"], "s1");
    assert_eq!(arr[0]["subtask"], "d"); // opus P1-2：子任务描述文本（B3 TaskPack.subtask 来源）
    assert_eq!(arr[0]["assignee"]["agent_id"], "claude-1");
    assert_eq!(arr[0]["assignee"]["provider"], "claude"); // opus P1-3：provider 快照
    assert_eq!(arr[0]["assignee"]["model"], "claude-opus");
    assert_eq!(arr[0]["acceptance"][0]["claim"], "c");
}

#[test]
fn build_assignments_json_unassigned_is_null_assignee() {
    let draft = draft_with(1, Some("cargo test"));
    let picks = vec![("s1".to_string(), None)];
    let json = build_assignments_json(&draft, &picks);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v[0]["assignee"].is_null());
}

#[test]
fn persist_draft_contract_writes_draft_and_task_acceptance() {
    let conn = crate::test_support::mem_db();
    let draft = draft_with(1, Some("cargo test"));
    let picks = vec![("s1".to_string(), Some(sample_assignee()))];
    let aj = build_assignments_json(&draft, &picks);
    persist_draft_contract(&conn, "s1", "r1", "lead", &draft, &aj).unwrap();

    let gc = crate::db::get_goal_contract_by_run(&conn, "s1", "r1")
        .unwrap()
        .unwrap();
    assert_eq!(gc.status, "draft");
    assert!(!gc.assignments_json.is_empty() && gc.assignments_json != "[]");

    let crits = crate::db::list_acceptance_by_run(&conn, "s1", "r1").unwrap();
    assert_eq!(crits.len(), 1);
    assert_eq!(crits[0].scope, "task");
    assert_eq!(crits[0].status, "pending"); // B7：draft 不冒充已验证
    assert_eq!(crits[0].task_id, "s1");
}

#[test]
fn pick_returns_first_eligible_by_sort_order() {
    let agents = vec![
        agent_profile("a1", true, None, 0),
        agent_profile("a2", true, None, 1),
    ];
    assert_eq!(pick_agent_for_subtask(&agents, &[], None, 0).unwrap(), "a1");
}

#[test]
fn pick_respects_hint_when_in_eligible_set() {
    let agents = vec![
        agent_profile("a1", true, None, 0),
        agent_profile("a2", true, None, 1),
    ];
    assert_eq!(
        pick_agent_for_subtask(&agents, &[], Some("a2"), 0).unwrap(),
        "a2"
    );
}

#[test]
fn pick_rejects_disabled_hint_and_falls_back() {
    // hint a2 被禁用 → 越权挡 → 降级到首个 enabled（a1）
    let agents = vec![
        agent_profile("a1", true, None, 0),
        agent_profile("a2", false, None, 1),
    ];
    assert_eq!(
        pick_agent_for_subtask(&agents, &[], Some("a2"), 0).unwrap(),
        "a1"
    );
}

#[test]
fn pick_filters_by_capability_tag() {
    // 需要 reasoning·只有 a2 有该标签
    let agents = vec![
        agent_profile("a1", true, None, 0),
        agent_profile("a2", true, Some("native"), 1),
    ];
    assert_eq!(
        pick_agent_for_subtask(&agents, &["reasoning".to_string()], None, 0).unwrap(),
        "a2"
    );
}

#[test]
fn pick_errors_when_no_eligible_agent() {
    let agents = vec![agent_profile("a1", false, None, 0)];
    let e = pick_agent_for_subtask(&agents, &[], None, 0).unwrap_err();
    assert!(matches!(e, PickError::NoEligibleAgent { .. }));
}

#[test]
fn draft_prompt_lists_enabled_agent_pool() {
    let agents = vec![
        agent_profile("claude-1", true, Some("native"), 0),
        agent_profile("kimi-1", true, Some("native"), 1),
    ];
    let p = build_draft_prompt("查中美欧", None, &agents, crate::Locale::Zh);
    assert!(p.contains("可派的 agent 池"));
    assert!(p.contains("id: claude-1"));
    assert!(p.contains("id: kimi-1"));
    assert!(p.contains("语言要求"));
    assert!(
        p.ends_with("能力标签保持原样。"),
        "中文语言指令应位于 prompt 末尾: {p}"
    );
}

#[test]
fn draft_prompt_appends_english_language_directive() {
    let p = build_draft_prompt(
        "Research China, the US, and Europe",
        None,
        &[],
        crate::Locale::En,
    );

    assert!(p.contains("用户需求："), "{p}");
    assert!(p.contains("Language: write the plan's"), "{p}");
    assert!(
        p.ends_with("capability tags as-is."),
        "English language directive should end the prompt: {p}"
    );
}

#[test]
fn pick_fallback_round_robins_across_eligible() {
    // hint 全失配 → 按 subtask 序轮转·不全落第一个
    let agents = vec![
        agent_profile("a1", true, Some("native"), 0),
        agent_profile("a2", true, Some("native"), 1),
        agent_profile("a3", true, Some("native"), 2),
    ];
    let p0 = pick_agent_for_subtask(&agents, &[], None, 0).unwrap();
    let p1 = pick_agent_for_subtask(&agents, &[], None, 1).unwrap();
    let p2 = pick_agent_for_subtask(&agents, &[], None, 2).unwrap();
    let p3 = pick_agent_for_subtask(&agents, &[], None, 3).unwrap();
    assert_eq!(p0, "a1");
    assert_eq!(p1, "a2");
    assert_eq!(p2, "a3");
    assert_eq!(p3, "a1");
}

#[test]
fn parse_valid_draft_ok() {
    let d = parse_driver_draft(valid_draft_json()).expect("应解析成功");
    assert_eq!(d.goal, "加登录页");
    assert_eq!(d.subtasks.len(), 1);
    assert_eq!(d.subtasks[0].id, "s1");
    assert_eq!(
        d.subtasks[0].acceptance[0].verifier.as_deref(),
        Some("cargo test")
    );
    assert_eq!(d.assignments[0].subtask_id, "s1");
}

#[test]
fn parse_strips_markdown_fence() {
    let fenced = format!("```json\n{}\n```", valid_draft_json());
    let d = parse_driver_draft(&fenced).expect("围栏应被剥掉后解析成功");
    assert_eq!(d.goal, "加登录页");
}

#[test]
fn parse_not_json_errors() {
    let e = parse_driver_draft("这不是 JSON 只是闲聊").unwrap_err();
    assert!(matches!(e, DraftParseError::NotJson(_)));
}

#[test]
fn parse_schema_mismatch_errors() {
    let e = parse_driver_draft(r#"{"goal":"x"}"#).unwrap_err();
    assert!(matches!(e, DraftParseError::SchemaMismatch(_)));
}

#[test]
fn parse_empty_goal_is_semantic_invalid() {
    let e =
        parse_driver_draft(r#"{"goal":"","subtasks":[{"id":"s1","desc":"d"}],"assignments":[]}"#)
            .unwrap_err();
    assert!(matches!(e, DraftParseError::SemanticInvalid(_)));
}

#[test]
fn parse_empty_subtasks_is_semantic_invalid() {
    let e = parse_driver_draft(r#"{"goal":"g","subtasks":[],"assignments":[]}"#).unwrap_err();
    assert!(matches!(e, DraftParseError::SemanticInvalid(_)));
}

#[test]
fn parse_empty_subtask_desc_is_semantic_invalid() {
    // codex P1-1：强化围栏·空 desc（driver 半成功）须挡
    let e =
        parse_driver_draft(r#"{"goal":"g","subtasks":[{"id":"s1","desc":""}],"assignments":[]}"#)
            .unwrap_err();
    assert!(matches!(e, DraftParseError::SemanticInvalid(_)));
}

#[test]
fn parse_empty_criterion_claim_is_semantic_invalid() {
    // codex P1-1：acceptance 项 claim 不能空
    let e = parse_driver_draft(
        r#"{"goal":"g","subtasks":[{"id":"s1","desc":"d","acceptance":[{"claim":""}]}],"assignments":[]}"#,
    )
    .unwrap_err();
    assert!(matches!(e, DraftParseError::SemanticInvalid(_)));
}

#[test]
fn parse_assignment_refs_unknown_subtask_is_semantic_invalid() {
    let e = parse_driver_draft(
        r#"{"goal":"g","subtasks":[{"id":"s1","desc":"d"}],"assignments":[{"subtask_id":"NOPE"}]}"#,
    )
    .unwrap_err();
    assert!(matches!(e, DraftParseError::SemanticInvalid(_)));
}

#[test]
fn parse_duplicate_assignment_for_same_subtask_is_semantic_invalid() {
    // codex P1-1：同一 subtask 被派两次（重复 assignment）须挡
    let e = parse_driver_draft(
        r#"{"goal":"g","subtasks":[{"id":"s1","desc":"d"}],"assignments":[{"subtask_id":"s1"},{"subtask_id":"s1"}]}"#,
    )
    .unwrap_err();
    assert!(matches!(e, DraftParseError::SemanticInvalid(_)));
}

#[test]
fn lead_invoke_draft_parses_valid_final_text() {
    let (_tf, spawn) = fake_driver_emitting(valid_draft_json());
    let d =
        lead_invoke_draft(3, crate::agent_event::parse_claude_line, spawn).expect("应拿到 draft");
    assert_eq!(d.goal, "加登录页");
}

#[test]
fn lead_invoke_draft_parses_codex_text_delta_when_final_text_is_absent() {
    let (_tf, spawn) = fake_codex_driver_emitting_text_delta(valid_draft_json());
    let d = lead_invoke_draft(3, crate::agent_event::parse_codex_line, spawn)
        .expect("Codex 正文在 TextDelta，也应拿到 draft");
    assert_eq!(d.goal, "加登录页");
}

#[test]
fn lead_invoke_draft_uses_last_json_codex_text_delta() {
    let (_tf, spawn) = fake_codex_driver_emitting_text_deltas(vec![
        "先解释一下，不是 JSON".into(),
        valid_draft_json().into(),
    ]);
    let d = lead_invoke_draft(3, crate::agent_event::parse_codex_line, spawn)
        .expect("Codex 多条 agent_message 时应取最后一个 JSON draft");
    assert_eq!(d.goal, "加登录页");
}

#[test]
fn text_delta_fallback_ignores_json_scalar_chunks() {
    let fallback = fallback_text_delta_text(&[
        "{\"goal\":".into(),
        "\"加登录页\"".into(),
        ",\"subtasks\":[{\"id\":\"s1\",\"desc\":\"加表单\",\"scope_files\":[\"a.rs\"],\"acceptance\":[{\"claim\":\"测试过\",\"verifier\":\"cargo test\"}],\"needed_caps\":[]}],\"assignments\":[{\"subtask_id\":\"s1\",\"agent_id\":\"claude-1\"}]}".into(),
    ])
    .expect("chunked fallback should still join");
    assert_eq!(fallback, valid_draft_json());
}

#[test]
fn lead_invoke_draft_retries_then_exhausts_on_garbage() {
    let (_tf, spawn) = fake_driver_emitting("不是 JSON 的闲聊");
    let err = lead_invoke_draft(3, crate::agent_event::parse_claude_line, spawn).unwrap_err();
    match err {
        DraftFailure::ParseExhausted { attempts, .. } => assert_eq!(attempts, 3),
        other => panic!("应为 ParseExhausted·实得 {other:?}"),
    }
}

#[test]
fn draft_failure_serializes_camel_case_fields() {
    // 前端 types/gate.ts 读 lastError（camelCase）·rename_all 在 enum 上只改变体名不改字段（GUI 曾显 undefined）
    let f = DraftFailure::ParseExhausted {
        attempts: 3,
        last_error: "x".into(),
    };
    let j = serde_json::to_value(&f).unwrap();
    assert_eq!(j["kind"], "parseExhausted");
    assert_eq!(j["attempts"], 3);
    assert_eq!(j["lastError"], "x");
}

#[test]
fn lead_invoke_draft_surfaces_stderr_tail_on_no_final_text() {
    // GUI 失败可诊断：driver 没吐 final_text 时·stderr 尾部要进 last_error（曾全被丢弃没法断案）
    let spawn = || -> Result<std::process::Child, String> {
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg("echo 'node: command not found' >&2");
        c.stdout(std::process::Stdio::piped());
        c.stderr(std::process::Stdio::piped());
        c.spawn().map_err(|e| e.to_string())
    };
    let err = lead_invoke_draft(2, crate::agent_event::parse_claude_line, spawn).unwrap_err();
    match err {
        DraftFailure::ParseExhausted { last_error, .. } => {
            let params: serde_json::Value = serde_json::from_str(
                last_error
                    .strip_prefix("AL_ERR:lead.draftNoFinalTextStderr:")
                    .unwrap_or_else(|| panic!("unexpected last_error: {last_error}")),
            )
            .unwrap();
            assert_eq!(params["tail"], "node: command not found");
        }
        other => panic!("应为 ParseExhausted·实得 {other:?}"),
    }
}

#[test]
fn lead_invoke_draft_codes_no_final_text_without_stderr() {
    let spawn = || -> Result<std::process::Child, String> {
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg("true");
        c.stdout(std::process::Stdio::piped());
        c.stderr(std::process::Stdio::piped());
        c.spawn().map_err(|e| e.to_string())
    };
    let err = lead_invoke_draft(1, crate::agent_event::parse_claude_line, spawn).unwrap_err();
    assert!(matches!(
        err,
        DraftFailure::ParseExhausted { last_error, .. }
            if last_error == "AL_ERR:lead.draftNoFinalText"
    ));
}

#[test]
fn lead_invoke_draft_invoke_failed_when_spawn_errors() {
    let spawn = || -> Result<std::process::Child, String> { Err("起不来".into()) };
    let err = lead_invoke_draft(3, crate::agent_event::parse_claude_line, spawn).unwrap_err();
    assert!(matches!(err, DraftFailure::InvokeFailed { .. }));
}

#[test]
fn run_propose_team_plan_drafts_and_persists() {
    // 写类 verifier → 非 tier0 → 维持现状落 draft 行（验「非 tier0 照旧落库」）
    let db = mem_db_with_agent();
    let dir = tempfile::tempdir().unwrap();
    let (_tf, spawn) = fake_driver_emitting(write_like_draft_json());
    let outcome = run_propose_team_plan(
        &db,
        "s1",
        "claude-1",
        3,
        crate::agent_event::parse_claude_line,
        spawn,
        dir.path(),
        None,
    )
    .unwrap();
    let result = match outcome {
        ProposeOutcome::Drafted(r) => r,
        other => panic!("应为 Drafted·实得 {other:?}"),
    };
    assert_eq!(result.status, "draft");
    assert_eq!(result.subtask_count, 1);
    assert_eq!(result.unassigned_count, 0);
    // 回传面够 B2 渲：contract_id + assignments_json 带 assignee/subtask
    assert!(result.assignments_json.contains("claude-1"));
    let aj: serde_json::Value = serde_json::from_str(&result.assignments_json).unwrap();
    assert_eq!(aj[0]["assignee"]["agent_id"], "claude-1");
    assert_eq!(aj[0]["subtask"], "格式化");
    // 落库可读
    let conn = db.0.lock().unwrap();
    let gc = crate::db::get_goal_contract_by_run(&conn, "s1", &result.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(gc.status, "draft");
    assert_eq!(gc.id, result.contract_id);
}

#[test]
fn run_propose_team_plan_research_draft_lands_tier0() {
    // valid_draft_json = verifier "cargo test" + scope ["a.rs"]（"a.rs" 在 tempdir 不存在 → existing=0·只读）→ 放行喂 0.0 → tier0
    let db = mem_db_with_agent();
    let dir = tempfile::tempdir().unwrap();
    let (_tf, spawn) = fake_driver_emitting(valid_draft_json());
    let outcome = run_propose_team_plan(
        &db,
        "s1",
        "claude-1",
        3,
        crate::agent_event::parse_claude_line,
        spawn,
        dir.path(),
        None,
    )
    .unwrap();
    let r = match outcome {
        ProposeOutcome::Drafted(r) => r,
        other => panic!("应为 Drafted·实得 {other:?}"),
    };
    assert_eq!(r.tier, "tier0");
}

#[test]
fn run_propose_tier0_skips_draft_contract_persist() {
    // 拍板③（spec §4）：Tier0 不落 goal_contracts——propose 阶段跳过 persist_draft_contract
    let db = mem_db_with_agent();
    let dir = tempfile::tempdir().unwrap();
    let (_tf, spawn) = fake_driver_emitting(valid_draft_json()); // tier0 fixture
    let outcome = run_propose_team_plan(
        &db,
        "s1",
        "claude-1",
        3,
        crate::agent_event::parse_claude_line,
        spawn,
        dir.path(),
        None,
    )
    .unwrap();
    let r = match outcome {
        ProposeOutcome::Drafted(r) => r,
        other => panic!("应为 Drafted·实得 {other:?}"),
    };
    assert_eq!(r.tier, "tier0");
    // 回传面仍完整（前端要靠 assignments_json 组装派单）
    assert!(!r.assignments_json.is_empty());
    assert_eq!(r.unassigned_count, 0);
    let conn = db.0.lock().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM goal_contracts WHERE session_id='s1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "tier0 不落 goal_contracts");
    let m: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM acceptance_criteria WHERE session_id='s1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        m, 0,
        "tier0 不落 acceptance（随 start_team_run 的 criteria 入参走·T3）"
    );
}

#[test]
fn run_propose_team_plan_draft_failed_on_garbage_leaves_db_clean() {
    let db = mem_db_with_agent();
    let dir = tempfile::tempdir().unwrap();
    let (_tf, spawn) = fake_driver_emitting("闲聊不是 JSON");
    let outcome = run_propose_team_plan(
        &db,
        "s1",
        "claude-1",
        2,
        crate::agent_event::parse_claude_line,
        spawn,
        dir.path(),
        None,
    )
    .unwrap();
    assert!(matches!(outcome, ProposeOutcome::DraftFailed { .. }));
    // 拟失败不落库（codex NIT-2）
    let conn = db.0.lock().unwrap();
    // run_id 没生成·按 session 查 goal_contracts 应为空
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM goal_contracts WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn filter_by_roster_none_keeps_all() {
    let agents = vec![
        agent_profile("a1", true, None, 0),
        agent_profile("a2", true, None, 1),
    ];
    let got = filter_agents_by_roster(&agents, None);
    assert_eq!(got.len(), 2);
}

#[test]
fn filter_by_roster_empty_keeps_all() {
    // Some([]) 视为「未收窄」（前端没传/全勾）→ 不约束
    let agents = vec![agent_profile("a1", true, None, 0)];
    let empty: Vec<String> = vec![];
    let got = filter_agents_by_roster(&agents, Some(&empty));
    assert_eq!(got.len(), 1);
}

#[test]
fn filter_by_roster_strict_empty_keeps_empty() {
    let agents = vec![agent_profile("a1", true, None, 0)];
    let empty: Vec<String> = vec![];

    let got = filter_agents_by_roster_strict(&agents, Some(&empty));

    assert!(got.is_empty());
}

#[test]
fn filter_by_roster_keeps_only_listed() {
    let agents = vec![
        agent_profile("a1", true, None, 0),
        agent_profile("a2", true, None, 1),
        agent_profile("a3", true, None, 2),
    ];
    let roster = vec!["a1".to_string(), "a3".to_string()];
    let got = filter_agents_by_roster(&agents, Some(&roster));
    let ids: Vec<&str> = got.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["a1", "a3"]);
}

#[test]
fn pick_via_roster_filtered_slice_never_returns_excluded() {
    // 关键回归：兜底轮转不再把活派给被收窄掉的 agent（假闭环修复）
    let agents = vec![
        agent_profile("a1", true, None, 0),
        agent_profile("a2", true, None, 1),
    ];
    let roster = vec!["a1".to_string()];
    let pool = filter_agents_by_roster(&agents, Some(&roster));
    // 即便 hint 指 a2、subtask_index 轮到 a2·收窄后池里只有 a1
    for idx in 0..4 {
        assert_eq!(
            pick_agent_for_subtask(&pool, &[], Some("a2"), idx).unwrap(),
            "a1"
        );
    }
}

#[test]
fn run_propose_team_plan_strict_empty_roster_does_not_pick_all_agents() {
    let db = mem_db_with_agent();
    let dir = tempfile::tempdir().unwrap();
    let empty: Vec<String> = Vec::new();
    let (_tf, spawn) = fake_driver_emitting(valid_draft_json());

    let outcome = run_propose_team_plan_with_roster_mode(
        &db,
        "s1",
        "claude-1",
        3,
        crate::agent_event::parse_claude_line,
        spawn,
        dir.path(),
        Some(&empty),
        true,
    )
    .unwrap();
    let r = match outcome {
        ProposeOutcome::Drafted(r) => r,
        other => panic!("应为 Drafted·实得 {other:?}"),
    };
    let assignments: serde_json::Value = serde_json::from_str(&r.assignments_json).unwrap();

    assert_eq!(r.unassigned_count, 1);
    assert!(assignments[0]["assignee"].is_null());
}
