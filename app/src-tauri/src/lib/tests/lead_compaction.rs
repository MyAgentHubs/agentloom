#![cfg(test)]

use super::*;

#[test]
fn parse_goal_title_arg_trims_empty_absent() {
    assert_eq!(
        parse_goal_title_arg(&serde_json::json!({"goal_title": "  目标条变绿 "})),
        Some("目标条变绿".to_string())
    );
    assert_eq!(
        parse_goal_title_arg(&serde_json::json!({"goal_title": "   "})),
        None
    );
    assert_eq!(parse_goal_title_arg(&serde_json::json!({})), None);
}

/// 新项 A（2026-07-09）：dispatch_worker 工具 description 在注册处动态拼上花名册——
/// lead 不必派错一次（agent_hint 不匹配）才看见谁在池子里。这里直接核 lib.rs 注册处
/// 实际会用的 lead_tools::dispatch_worker_description 输出（同一份函数·非另造断言）。
#[test]
fn dispatch_worker_registration_description_lists_enabled_members() {
    let pool = vec![
        lead_tools::PoolMember {
            agent_id: "glm-1".into(),
            name: "GLM".into(),
            provider: "zhipu".into(),
            participant_id: "participant-glm-1".into(),
        },
        lead_tools::PoolMember {
            agent_id: "codex-1".into(),
            name: "Codex".into(),
            provider: "codex".into(),
            participant_id: "participant-codex-1".into(),
        },
    ];
    let desc = lead_tools::dispatch_worker_description(&pool);
    assert!(desc.contains("GLM") && desc.contains("glm-1"));
    assert!(desc.contains("Codex") && desc.contains("codex-1"));
}

#[test]
fn dispatch_worker_registration_description_empty_pool_is_honest() {
    let desc = lead_tools::dispatch_worker_description(&[]);
    assert!(desc.contains("No workers are currently enabled"));
}

fn lead_compact_wiring_transcript_nonce(prompt: &str) -> &str {
    prompt
        .lines()
        .find_map(|line| {
            line.strip_prefix("===== AGENTLOOM-COMPACT-SUMMARY ")
                .and_then(|rest| rest.split_once(" through="))
                .map(|(nonce, _)| nonce)
        })
        .expect("compact summary marker nonce")
}

#[test]
fn lead_compact_wiring_harness_reads_state_marks_history_and_refreshes_nonce() {
    let session_id = "s-lead-compact-wiring-harness";
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, session_id, "lead compact", "local-default", "local").unwrap();
    db::append_message(
        &conn,
        session_id,
        "user",
        &[db::Block::Text {
            text: "covered old message".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let through_message_id = conn.last_insert_rowid();
    db::append_message(
        &conn,
        session_id,
        "assistant",
        &[db::Block::Text {
            text: "fresh lead message".into(),
        }],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    db::upsert_compact_state(
        &conn,
        session_id,
        "rolled lead summary",
        through_message_id,
        Some("run-compact"),
    )
    .unwrap();

    let first = build_lead_context_prompt_for_session(
        &conn,
        session_id,
        &[],
        Locale::Zh,
        LeadEngine::Harness,
        &[],
    )
    .unwrap()
    .prompt;
    let second = build_lead_context_prompt_for_session(
        &conn,
        session_id,
        &[],
        Locale::Zh,
        LeadEngine::Harness,
        &[],
    )
    .unwrap()
    .prompt;

    let first_nonce = lead_compact_wiring_transcript_nonce(&first);
    let second_nonce = lead_compact_wiring_transcript_nonce(&second);
    assert_eq!(first_nonce.len(), 32);
    assert!(first_nonce
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    assert_ne!(
        first_nonce, second_nonce,
        "each assembly gets a fresh nonce"
    );
    assert!(first.contains("rolled lead summary"));
    assert!(first.contains(&format!("===== AGENTLOOM-MSG {first_nonce} ")));
    assert!(first.contains(&format!("===== AGENTLOOM-HISTORY-END {first_nonce} =====")));
    assert!(!first.contains("covered old message"));
    assert!(first.contains("fresh lead message"));
    let data_nonce = first
        .lines()
        .find_map(|line| {
            line.strip_prefix("===== AGENTLOOM-DATA ")
                .and_then(|rest| rest.strip_suffix(" ====="))
        })
        .expect("DATA fence nonce");
    assert_ne!(data_nonce, first_nonce);
}

#[test]
fn lead_compact_wiring_non_harness_keeps_legacy_unmarked_history() {
    let session_id = "s-lead-compact-wiring-native";
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, session_id, "lead native", "local-default", "local").unwrap();
    db::append_message(
        &conn,
        session_id,
        "user",
        &[db::Block::Text {
            text: "legacy unmarked message".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    db::upsert_compact_state(
        &conn,
        session_id,
        "must remain invisible",
        conn.last_insert_rowid(),
        Some("run-native"),
    )
    .unwrap();

    let prompt = build_lead_context_prompt_for_session(
        &conn,
        session_id,
        &[],
        Locale::Zh,
        LeadEngine::NativeClaude,
        &[],
    )
    .unwrap()
    .prompt;

    assert!(prompt.contains("legacy unmarked message"));
    assert!(!prompt.contains("AGENTLOOM-MSG"));
    assert!(!prompt.contains("AGENTLOOM-COMPACT-SUMMARY"));
    assert!(!prompt.contains("AGENTLOOM-HISTORY-END"));
    assert!(!prompt.contains("must remain invisible"));
}

#[test]
fn lead_compact_wiring_context_compacted_event_persists_renderable_chip_block() {
    let session_id = "s-lead-compact-wiring-chip";
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, session_id, "lead chip", "local-default", "local").unwrap();
    let mut reducer = display_reduce::DisplayReducer::new("run-lead-compact-chip");
    reducer.feed(&agent_event::AgentEvent::ContextCompacted {
        summary: "summary is persisted separately".into(),
        through_message_id: 7,
    });
    let reduced = reducer
        .finish(&base_run_outcome("run-lead-compact-chip"))
        .expect("lead event must cross the shared display reducer finalizer");
    db::append_message_dedup_and_publish(
        &conn,
        session_id,
        "assistant",
        &reduced.blocks,
        Some("agent-team"),
        Some("lead-harness"),
        Some("Harness Lead"),
        &reduced.dedup_key,
    )
    .unwrap();

    let messages = db::get_messages(&conn, session_id).unwrap();
    assert!(messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, db::Block::ContextCompacted {}))
    }));
}
