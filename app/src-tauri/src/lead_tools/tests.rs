#![cfg(test)]

use super::*;

mod dispatch;
mod interaction;

#[test]
fn prompt_user_emits_decision_card_resolved_only_inside_changed_branch() {
    // AppHandle 无法在普通 #[test] 中构造；结构性钉死薄壳契约：CAS 调用在前，且 resolved
    // emit 必须实际嵌套在 `if changed` 花括号内。把 emit 挪到该分支前/后都会使本测试变红。
    let source = include_str!("../lead_tools.rs");
    let production = source.split("\n#[cfg(test)]\nmod tests;").next().unwrap();
    let prompt_user = production
        .split("fn prompt_user(")
        .nth(1)
        .expect("必须找到 prompt_user")
        .split("\nfn unbounded_prompt_never_pending(")
        .next()
        .unwrap();
    let answered = prompt_user
        .split("crate::WaitOutcome::Answered(opt) => {")
        .nth(1)
        .expect("必须找到 Answered 分支")
        .split("crate::WaitOutcome::TimedOut =>")
        .next()
        .unwrap();

    assert_eq!(
        answered.matches("\"decision-card-resolved\"").count(),
        1,
        "Answered 分支应且仅应有一次翻卡 emit"
    );
    let update_idx = answered
        .find("crate::db::update_decision_card_status_message_id(")
        .expect("Answered 分支必须执行决策卡 CAS");
    let if_idx = answered
        .find("if changed {")
        .expect("Answered 分支必须以 changed 门控 emit");
    assert!(update_idx < if_idx, "必须先取得 CAS 结果，再判断是否 emit");
    let changed_source = answered
        .split("let (changed, republish) = {")
        .nth(1)
        .expect("必须捕获 CAS 是否成功（含 msgfix1 T5 缺口④重发用的 message_id）")
        .split("if changed {")
        .next()
        .unwrap();
    assert!(
        changed_source.contains(".unwrap_or(None)")
            && changed_source.contains("cas_message_id.is_some()"),
        "只有 CAS 真的命中改写（message_id 非空）才能把 changed 置为 true"
    );
    assert!(
        changed_source.contains("Err(_) => (false, None)"),
        "DB 锁失败必须折叠为 changed=false 且不产出重发目标，不得 emit/重发"
    );

    let open_idx = if_idx
        + answered[if_idx..]
            .find('{')
            .expect("if changed 必须有分支体");
    let mut depth = 0usize;
    let mut close_idx = None;
    for (offset, ch) in answered[open_idx..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close_idx = Some(open_idx + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close_idx = close_idx.expect("if changed 花括号必须闭合");
    let emit_idx = answered
        .find("\"decision-card-resolved\"")
        .expect("必须存在翻卡 emit");
    assert!(
        emit_idx > open_idx && emit_idx < close_idx,
        "decision-card-resolved emit 必须嵌套在 CAS changed=true 分支内"
    );
    let answer_return_idx = answered
        .find("Ok(PromptOutcome::Answered(opt, decision_id.clone()))")
        .expect("无论是否 emit 都必须返回 Answered");
    assert!(
        close_idx < answer_return_idx,
        "门控 emit 完成后仍应维持原有 Answered 返回"
    );
}

fn pool_member(agent_id: &str) -> PoolMember {
    PoolMember {
        agent_id: agent_id.to_string(),
        name: format!("Agent {agent_id}"),
        provider: "codex".to_string(),
        participant_id: format!("participant-{agent_id}"),
    }
}

fn fake_result() -> MemberResult {
    MemberResult {
        schema_version: 1,
        assignment_id: "dispatch-agent-1".to_string(),
        participant_id: "participant-agent-1".to_string(),
        status: "done".to_string(),
        failure_reason: None,
        changed_files: vec![
            ChangedFile {
                path: "a.txt".to_string(),
                insertions: 1,
                deletions: 0,
            },
            ChangedFile {
                path: "b.txt".to_string(),
                insertions: 2,
                deletions: 1,
            },
        ],
        anchor: ResultAnchor {
            base_sha: "abc".to_string(),
            head_sha: None,
            diff_ref: None,
            generated_from: "test".to_string(),
        },
        command_evidence: vec![],
        risk_inputs: RiskInputs {
            files_changed: 0,
            cmd_danger: "low".to_string(),
            reversibility: "reversible".to_string(),
        },
        decisions: vec![],
        risks: vec![],
        final_text_ref: Some("DONE".to_string()),
        artifact_refs: vec![],
        result_source: "raw".to_string(),
        requires_long_task: None,
        exit_code: None,
        stderr_tail: None,
        failure_kind: None,
    }
}

/// 测试用 `begin_dispatch_intent` 闭包：包一个全新的 `TeamRunning`，恒能占到 intent
/// （测试不关心真实会话状态，只关心 `dispatch_worker_inner` 拿到 guard 后的行为）。
fn always_ok_intent() -> Arc<dyn Fn() -> Result<DispatchIntentGuard, String> + Send + Sync> {
    let team_running = crate::member_runner::TeamRunning::default();
    Arc::new(move || team_running.begin_dispatch_intent("test-session"))
}

fn noop_worker_settled() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

fn noop_result_delivered() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|_| {})
}

/// 测试用空幂等账本——每个测试各自新建，互不干扰。
fn empty_ledger() -> Arc<Mutex<HashMap<String, DispatchLedgerEntry>>> {
    Arc::new(Mutex::new(HashMap::new()))
}

#[test]
fn dispatch_worker_description_lists_all_enabled_members() {
    let pool = vec![
        PoolMember {
            agent_id: "glm-1".into(),
            name: "GLM".into(),
            provider: "zhipu".into(),
            participant_id: "participant-glm-1".into(),
        },
        PoolMember {
            agent_id: "codex-1".into(),
            name: "Codex".into(),
            provider: "codex".into(),
            participant_id: "participant-codex-1".into(),
        },
    ];
    let desc = dispatch_worker_description(&pool);
    assert!(
        desc.contains("GLM"),
        "should mention member name GLM: {desc}"
    );
    assert!(
        desc.contains("glm-1"),
        "should mention member id glm-1: {desc}"
    );
    assert!(
        desc.contains("Codex"),
        "should mention member name Codex: {desc}"
    );
    assert!(
        desc.contains("codex-1"),
        "should mention member id codex-1: {desc}"
    );
    assert!(
        !desc.contains("No workers are currently enabled"),
        "non-empty pool should not show the empty-pool warning: {desc}"
    );
}

#[test]
fn dispatch_worker_description_empty_pool_warns_honestly() {
    let desc = dispatch_worker_description(&[]);
    assert!(
        desc.contains("No workers are currently enabled"),
        "empty pool description should say no worker enabled: {desc}"
    );
}

#[test]
fn member_roster_prompt_section_empty_pool_states_empty_explicitly() {
    let section = member_roster_prompt_section(&[], crate::Locale::Zh);
    assert!(
        section.contains("可派 worker 花名册"),
        "空池仍要渲染节标签: {section}"
    );
    assert!(
        section.contains("没有启用任何 worker"),
        "空池要明说没有启用任何 worker（防续聊残留旧花名册）: {section}"
    );
    assert!(
        !section.contains("agent-1") && !section.contains("agent-2"),
        "空池不应含任何具体成员 id/名字条目: {section}"
    );
}

#[test]
fn member_roster_prompt_section_lists_members() {
    let pool = vec![pool_member("agent-1"), pool_member("agent-2")];
    let section = member_roster_prompt_section(&pool, crate::Locale::Zh);
    assert!(section.contains("花名册"));
    assert!(section.contains("agent-1"));
    assert!(section.contains("agent-2"));
}

#[test]
fn member_roster_prompt_section_uses_english_wrapper_and_keeps_member_format() {
    let empty = member_roster_prompt_section(&[], crate::Locale::En);
    assert!(empty.starts_with("Available worker roster: (empty — no workers enabled;"));
    assert!(!empty.contains("可派 worker 花名册"), "{empty}");

    let pool = vec![pool_member("agent-1")];
    let section = member_roster_prompt_section(&pool, crate::Locale::En);
    assert!(
        section.starts_with("Available worker roster: "),
        "{section}"
    );
    assert!(
        section.contains("Agent agent-1（codex·agent-1）"),
        "member formatting should remain shared across locales: {section}"
    );
    assert!(!section.contains("可派 worker 花名册"), "{section}");
}

// ---- 派单幂等键 P1：改动二·① agent_hint 报错候选表用裸 agent_id ----

#[test]
fn dispatch_worker_hint_error_gives_plain_pastable_agent_ids_not_display_format() {
    let ctx = LeadCtx {
        on_result_delivered: noop_result_delivered(),
        on_worker_settled: noop_worker_settled(),
        run_worker: Arc::new(|_| panic!("run_worker should not be called")),
        is_session_running: Arc::new(|| false),
        begin_dispatch_intent: always_ok_intent(),
        member_pool: vec![pool_member("agent-1"), pool_member("agent-2")],
        done: Arc::new(AtomicBool::new(false)),
        terminated: Arc::new(AtomicBool::new(false)),
        dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
        lead_run_id: "run1".to_string(),
        dispatch_ledger: empty_ledger(),
    };

    let err = dispatch_worker(
        &ctx,
        DispatchArgs {
            task: "do work".to_string(),
            agent_hint: Some("nonexistent".to_string()),
            goal_title: None,
        },
    )
    .unwrap_err();
    assert!(err.contains("agent-1"), "err: {err}");
    assert!(err.contains("agent-2"), "err: {err}");
    assert!(
        !err.contains('（'),
        "报错候选表不该用全角展示格式，模型照抄整串会再次不匹配: {err}"
    );
}

// ---- 派单幂等键 P1：改动二·② pool_hint_matches 宽松匹配 ----

#[test]
fn pool_hint_matches_rescues_display_format_copy_paste() {
    let pool = vec![
        PoolMember {
            agent_id: "glm-1".into(),
            name: "GLM".into(),
            provider: "zhipu".into(),
            participant_id: "participant-glm-1".into(),
        },
        PoolMember {
            agent_id: "codex-1".into(),
            name: "Codex".into(),
            provider: "codex".into(),
            participant_id: "participant-codex-1".into(),
        },
    ];
    // 模型照抄了 format_pool_member 的展示格式（全角括号 + 全角间隔号）整串回填。
    let matches = pool_hint_matches(&pool, "GLM（zhipu·glm-1）");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].agent_id, "glm-1");
}

#[test]
fn pool_hint_matches_supports_half_width_wrapping_and_bare_provider_id_form() {
    let pool = vec![pool_member("agent-1")];
    assert_eq!(
        pool_hint_matches(&pool, "Agent agent-1(codex·agent-1)").len(),
        1
    );
    assert_eq!(pool_hint_matches(&pool, "codex·agent-1").len(), 1);
}

#[test]
fn pool_hint_matches_falls_back_to_unique_agent_id_prefix() {
    let pool = vec![pool_member("agent-1")];
    let matches = pool_hint_matches(&pool, "agent");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].agent_id, "agent-1");
}

#[test]
fn pool_hint_matches_ambiguous_prefix_returns_multiple_not_a_guess() {
    let pool = vec![pool_member("agent-1"), pool_member("agent-2")];
    let matches = pool_hint_matches(&pool, "agent");
    assert_eq!(
        matches.len(),
        2,
        "多个前缀命中留给上层报 ambiguous，不该在这里替模型瞎猜"
    );
}

// ---- 派单幂等键 P1：改动二·③ agent_hint 非字符串不静默丢 ----

#[test]
fn parse_agent_hint_arg_accepts_absent_null_and_string() {
    assert_eq!(parse_agent_hint_arg(&serde_json::json!({})).unwrap(), None);
    assert_eq!(
        parse_agent_hint_arg(&serde_json::json!({"agent_hint": null})).unwrap(),
        None
    );
    assert_eq!(
        parse_agent_hint_arg(&serde_json::json!({"agent_hint": "glm-1"})).unwrap(),
        Some("glm-1".to_string())
    );
}

#[test]
fn parse_agent_hint_arg_rejects_non_string_with_honest_type_name() {
    let err = parse_agent_hint_arg(&serde_json::json!({"agent_hint": ["glm-1"]})).unwrap_err();
    assert!(err.contains("array"), "err: {err}");

    let err =
        parse_agent_hint_arg(&serde_json::json!({"agent_hint": {"id": "glm-1"}})).unwrap_err();
    assert!(err.contains("object"), "err: {err}");

    let err = parse_agent_hint_arg(&serde_json::json!({"agent_hint": 42})).unwrap_err();
    assert!(err.contains("number"), "err: {err}");

    let err = parse_agent_hint_arg(&serde_json::json!({"agent_hint": true})).unwrap_err();
    assert!(err.contains("boolean"), "err: {err}");
}

// ---- 派单幂等键 P1：改动二·④ dispatch_worker_input_schema required/enum 形状 ----

#[test]
fn dispatch_worker_input_schema_optional_agent_hint_for_pool_of_one() {
    let schema = dispatch_worker_input_schema(&[pool_member("agent-1")]);
    let required = schema["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "task"));
    assert!(
        !required.iter().any(|v| v == "agent_hint"),
        "pool==1 时 agent_hint 仍应可选: {schema}"
    );
    assert!(schema["properties"]["agent_hint"].get("enum").is_none());
}

#[test]
fn dispatch_worker_input_schema_empty_pool_keeps_agent_hint_optional() {
    let schema = dispatch_worker_input_schema(&[]);
    let required = schema["required"].as_array().unwrap();
    assert!(!required.iter().any(|v| v == "agent_hint"));
    assert!(schema["properties"]["agent_hint"].get("enum").is_none());
}

#[test]
fn dispatch_worker_input_schema_requires_and_enumerates_agent_hint_for_pool_of_many() {
    let pool = vec![pool_member("agent-1"), pool_member("agent-2")];
    let schema = dispatch_worker_input_schema(&pool);
    let required = schema["required"].as_array().unwrap();
    assert!(
        required.iter().any(|v| v == "agent_hint"),
        "pool>1 时 agent_hint 必填: {schema}"
    );
    let enum_values: Vec<&str> = schema["properties"]["agent_hint"]["enum"]
        .as_array()
        .expect("pool>1 时 agent_hint 应带 enum")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(enum_values, vec!["agent-1", "agent-2"]);
}
