#![cfg(test)]

use super::*;

#[test]
fn parse_remote_input_accepts_valid_input_send() {
    assert_eq!(
        parse_remote_input("input.send", r#"{"text":"hello"}"#),
        Ok("hello".to_string())
    );
}

#[test]
fn parse_remote_input_rejects_malformed_payload() {
    assert_eq!(
        parse_remote_input("input.send", "{malformed"),
        Err("REMOTE_INBOX_PAYLOAD_MALFORMED".to_string())
    );
}

#[test]
fn parse_remote_input_rejects_unknown_kind() {
    let error = parse_remote_input("control.bogus", "{}").unwrap_err();
    assert!(
        error.starts_with("UNKNOWN_REMOTE_INBOX_KIND:"),
        "未知 kind 必须保留稳定分类前缀"
    );
    assert_eq!(error, "UNKNOWN_REMOTE_INBOX_KIND:control.bogus");
}

#[test]
fn parse_remote_input_rejects_input_answer_defensively() {
    assert_eq!(
        parse_remote_input("input.answer", r#"{"text":"answer"}"#),
        Err("REMOTE_INBOX_KIND_NOT_SUPPORTED_YET".to_string())
    );
}

#[test]
fn answer_lead_question_outcome_serializes_stable_snake_case_fields() {
    let json = serde_json::to_string(&AnswerLeadQuestionOutcome {
        resumed: true,
        lead_agent_id: Some("claude".into()),
        resume_error: None,
    })
    .unwrap();
    assert_eq!(
        json,
        r#"{"resumed":true,"lead_agent_id":"claude","resume_error":null}"#
    );
}

#[test]
fn resolve_agent_team_uses_lead() {
    use crate::test_support::mem_db;
    let c = mem_db();
    let profile = db::AgentProfile {
        id: "claude".to_string(),
        name: "Claude".to_string(),
        access: "native".to_string(),
        provider: "claude".to_string(),
        primary_model: None,
        endpoint: None,
        auth_mode: None,
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "auto".to_string(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: None,
        cap_computer_use: None,
        cap_lead: Some("native_cli".to_string()),
        has_key: true,
        is_builtin: true,
        enabled: true,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    };
    db::upsert_agent(&c, &profile).unwrap();
    db::create_session(&c, "s-team", "x", "local-default", "local").unwrap();
    db::set_session_agent_config(&c, "s-team", Some("claude".to_string()), vec![]).unwrap();
    let resolved = resolve_session_run_agent(&c, "s-team").unwrap();
    assert_eq!(resolved.id, "claude");
    assert_eq!(resolved.provider, "claude");
    assert_eq!(resolved.access, "native");
}

#[test]
fn resolve_agent_solo_uses_last_run_commit() {
    use crate::test_support::mem_db;
    let c = mem_db();
    let profile = db::AgentProfile {
        id: "claude".to_string(),
        name: "Claude".to_string(),
        access: "native".to_string(),
        provider: "claude".to_string(),
        primary_model: None,
        endpoint: None,
        auth_mode: None,
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "auto".to_string(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: None,
        cap_computer_use: None,
        cap_lead: Some("native_cli".to_string()),
        has_key: true,
        is_builtin: true,
        enabled: true,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    };
    db::upsert_agent(&c, &profile).unwrap();
    db::create_session(&c, "s-solo", "x", "local-default", "local").unwrap();
    // Solo: NO session_agent_configs row (lead_agent_id stays None)
    // Insert a run_commits row with engine="claude"
    db::insert_run_pending(&c, "s-solo", "run-1", "claude", "abc123").unwrap();
    let resolved = resolve_session_run_agent(&c, "s-solo").unwrap();
    assert_eq!(
        resolved.id, "claude",
        "Solo session should resolve agent from last_run_commit.engine"
    );
    assert_eq!(resolved.provider, "claude");
}

#[test]
fn resolve_agent_solo_no_runs_errs() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(&c, "s-solo-norun", "x", "local-default", "local").unwrap();
    // Solo: NO session_agent_configs row, NO run_commits rows
    let err = resolve_session_run_agent(&c, "s-solo-norun").unwrap_err();
    assert_eq!(err, "AL_ERR:agent.sessionRunUnknown");
}

#[test]
fn resolve_agent_solo_fallback_to_message_agent_id() {
    use crate::test_support::mem_db;
    let c = mem_db();
    let profile = db::AgentProfile {
        id: "glm-international".to_string(),
        name: "GLM International".to_string(),
        access: "borrow".to_string(),
        provider: "deepseek".to_string(),
        primary_model: None,
        endpoint: None,
        auth_mode: None,
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "auto".to_string(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: None,
        cap_computer_use: None,
        cap_lead: None,
        has_key: true,
        is_builtin: false,
        enabled: true,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    };
    db::upsert_agent(&c, &profile).unwrap();
    db::create_session(&c, "s-solo-chat", "x", "local-default", "local").unwrap();
    db::append_message(
        &c,
        "s-solo-chat",
        "assistant",
        &[],
        Some("glm-international"),
        Some("glm-international"),
        Some("GLM International"),
    )
    .unwrap();
    let resolved = resolve_session_run_agent(&c, "s-solo-chat").unwrap();
    assert_eq!(resolved.id, "glm-international");
    assert_eq!(resolved.provider, "deepseek");
}
