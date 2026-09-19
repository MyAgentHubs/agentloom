#![cfg(test)]

use super::*;

#[test]
fn dispatch_card_block_round_trips_through_db() {
    use crate::agent_event::{MemberResult, ResultAnchor, RiskInputs};
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();

    let member = MemberSnapshot {
        participant_id: "worker-1".into(),
        assignment_id: "a-dispatch-1".into(),
        task_id: "t-1".into(),
        name: "codex-worker".into(),
        started_at: Some(1785500450000),
        status: "done".into(),
        sub: "实现块①.5".into(),
        steps_total: 3,
        steps_done: 3,
        cost_usd: Some(0.05),
        input_tokens: 500,
        output_tokens: 100,
        failed: false,
        blocks: vec![Block::Text {
            text: "干完了".into(),
        }],
        result: Some(MemberResult {
            schema_version: 1,
            assignment_id: "a-dispatch-1".into(),
            participant_id: "worker-1".into(),
            status: "done".into(),
            failure_reason: None,
            changed_files: vec![],
            anchor: ResultAnchor {
                base_sha: "abc".into(),
                head_sha: None,
                diff_ref: None,
                generated_from: "test".into(),
            },
            command_evidence: vec![],
            risk_inputs: RiskInputs {
                files_changed: 1,
                cmd_danger: "none".into(),
                reversibility: "reversible".into(),
            },
            decisions: vec![],
            risks: vec![],
            final_text_ref: None,
            artifact_refs: vec![],
            result_source: "raw".into(),
            requires_long_task: None,
            exit_code: None,
            stderr_tail: None,
            failure_kind: None,
        }),
    };

    let blocks = vec![
        Block::Text {
            text: "头部文本".into(),
        },
        Block::DispatchCard {
            run_id: "wrun-1".into(),
            member: member.clone(),
        },
    ];
    append_message(
        &c,
        "s1",
        "assistant",
        &blocks,
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    let got = get_messages(&c, "s1").unwrap();
    // 断言①：消息没被 unwrap_or_default 清空
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].content.len(), 2, "content 应有 2 个块·没被清空");
    // 断言②：第 2 个块是 DispatchCard·字段对
    match &got[0].content[1] {
        Block::DispatchCard { run_id, member: m } => {
            assert_eq!(run_id, "wrun-1");
            assert_eq!(m.assignment_id, "a-dispatch-1");
            assert_eq!(m.started_at, Some(1785500450000));
            assert!(!m.blocks.is_empty(), "member.blocks 应非空");
            assert!(m.result.is_some(), "member.result 应 Some");
        }
        other => panic!("期望 DispatchCard·得到 {:?}", other),
    }
}

#[test]
fn update_dispatch_card_terminal_updates_running_card_and_preserves_snapshot_fields() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let original = running_dispatch_card("assignment-1");
    append_message(
        &c,
        "s1",
        "assistant",
        &[
            Block::Text {
                text: "lead".into(),
            },
            original.clone(),
        ],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    let report = "[Worker report]\nagent: Codex Worker\nassignment_id: assignment-1\nstatus: done\nfinal_text:\nfinished";

    assert!(update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", report,).unwrap());

    let messages = get_messages(&c, "s1").unwrap();
    let Block::DispatchCard { run_id, member } = &messages[0].content[1] else {
        panic!("expected dispatch card");
    };
    let Block::DispatchCard {
        run_id: original_run_id,
        member: original_member,
    } = original
    else {
        unreachable!();
    };
    assert_eq!(run_id, &original_run_id);
    assert_eq!(member.status, "done");
    assert!(!member.failed);
    assert_eq!(
        member.blocks,
        vec![Block::Text {
            text: report.into()
        }]
    );
    assert_eq!(member.started_at, original_member.started_at);
    assert_eq!(member.sub, original_member.sub);
    assert_eq!(member.name, original_member.name);
    assert_eq!(member.steps_total, original_member.steps_total);
    assert_eq!(member.steps_done, original_member.steps_done);
    assert_eq!(member.cost_usd, original_member.cost_usd);
    assert_eq!(member.input_tokens, original_member.input_tokens);
    assert_eq!(member.output_tokens, original_member.output_tokens);
    assert_eq!(member.result, original_member.result);
}

#[test]
fn update_dispatch_card_terminal_preserves_unknown_json_fields() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let original_content = serde_json::json!([
        {
            "type": "dispatch_card",
            "run_id": "worker-run-assignment-1",
            "future_field": 1,
            "future_null": null,
            "member": {
                "participant_id": "worker-1",
                "assignment_id": "assignment-1",
                "task_id": "task-1",
                "name": "Codex Worker",
                "started_at": 1_785_500_450_123_i64,
                "status": "running",
                "sub": "实现终态收敛",
                "steps_total": 3,
                "steps_done": 1,
                "cost_usd": 0.25,
                "input_tokens": 17,
                "output_tokens": 29,
                "failed": false,
                "blocks": [],
                "result": null,
                "member_extra": "x",
                "member_null": null
            }
        },
        {
            "type": "text",
            "text": "untouched sibling",
            "text_extra": { "future": true }
        }
    ]);
    c.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, 'assistant', ?2, 1)",
            ("s1", original_content.to_string()),
        )
        .unwrap();
    let report = "terminal report";
    let serialized_report_block = serde_json::to_value(Block::Text {
        text: report.into(),
    })
    .unwrap();
    assert_eq!(
        serialized_report_block,
        serde_json::json!({ "type": "text", "text": report })
    );

    assert!(update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", report).unwrap());

    let raw_content: String = c
        .query_row(
            "SELECT content FROM messages WHERE session_id = 's1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let actual: serde_json::Value = serde_json::from_str(&raw_content).unwrap();
    let mut expected = original_content;
    expected[0]["member"]["status"] = serde_json::json!("done");
    expected[0]["member"]["failed"] = serde_json::json!(false);
    expected[0]["member"]["blocks"] = serde_json::json!([serialized_report_block]);
    assert_eq!(actual, expected);
    assert_eq!(actual[0]["future_field"], 1);
    assert_eq!(actual[0]["member"]["member_extra"], "x");
    assert_eq!(actual[1]["text_extra"]["future"], true);
}

#[test]
fn update_dispatch_card_terminal_is_idempotent_after_terminal_state() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[running_dispatch_card("assignment-1")],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();

    assert!(update_dispatch_card_terminal(&c, "s1", "assignment-1", "failed", "first").unwrap());
    let after_first = get_messages(&c, "s1").unwrap();
    assert!(!update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", "second").unwrap());
    assert_eq!(get_messages(&c, "s1").unwrap(), after_first);
}

#[test]
fn update_dispatch_card_terminal_returns_false_without_matching_card() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "assignment-1 appears only in prose".into(),
        }],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    let before = get_messages(&c, "s1").unwrap();

    assert!(!update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", "report").unwrap());
    assert_eq!(get_messages(&c, "s1").unwrap(), before);
}

#[test]
fn update_dispatch_card_terminal_only_changes_message_with_matching_assignment() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[running_dispatch_card("assignment-other")],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[running_dispatch_card("assignment-target")],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();

    assert!(update_dispatch_card_terminal(
        &c,
        "s1",
        "assignment-target",
        "stopped",
        "target report",
    )
    .unwrap());

    let messages = get_messages(&c, "s1").unwrap();
    let Block::DispatchCard { member: other, .. } = &messages[0].content[0] else {
        panic!("expected other dispatch card");
    };
    let Block::DispatchCard { member: target, .. } = &messages[1].content[0] else {
        panic!("expected target dispatch card");
    };
    assert_eq!(other.status, "running");
    assert!(other.blocks.is_empty());
    assert_eq!(target.status, "stopped");
    assert!(target.failed);
    assert_eq!(
        target.blocks,
        vec![Block::Text {
            text: "target report".into()
        }]
    );
}

// msgfix1 T2（M0 §10.7）：messages.revision 是内容版本唯一真相源。

#[test]
fn messages_revision_migration_backfills_existing_rows_to_one() {
    let c = mem();
    // 模拟旧库：先重建一个没有 revision 列（也没有 dedup_key 列）的旧版 messages 表，
    // 插入一行存量数据，再跑 init_schema 走 ALTER TABLE 迁移路径补列。
    c.execute_batch(
        "DROP TABLE messages;
             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 content TEXT NOT NULL CHECK (json_valid(content)),
                 engine TEXT,
                 agent_id TEXT,
                 agent_name_snapshot TEXT,
                 created_at INTEGER NOT NULL
             );",
    )
    .unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) \
             VALUES ('legacy-session', 'user', '[]', 1)",
        [],
    )
    .unwrap();

    init_schema(&c).unwrap();

    let revision: i64 = c
        .query_row(
            "SELECT revision FROM messages WHERE session_id = 'legacy-session'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revision, 1, "旧库存量行迁移后 revision 应自然回填为 1");

    // 迁移是幂等的：再跑一次 init_schema 不应报错、不应改变已有值。
    init_schema(&c).unwrap();
    let revision_again: i64 = c
        .query_row(
            "SELECT revision FROM messages WHERE session_id = 'legacy-session'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revision_again, 1);
}

#[test]
fn append_message_new_row_starts_at_revision_one() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text { text: "hi".into() }],
        None,
        None,
        None,
    )
    .unwrap();
    let messages = get_messages(&c, "s1").unwrap();
    assert_eq!(messages[0].revision, 1);
}

#[test]
fn update_dispatch_card_terminal_bumps_revision_on_change_and_not_on_noop() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[running_dispatch_card("assignment-1")],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(get_messages(&c, "s1").unwrap()[0].revision, 1);

    assert!(update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", "first").unwrap());
    assert_eq!(
        get_messages(&c, "s1").unwrap()[0].revision,
        2,
        "首次原地更新原子 +1"
    );

    // 幂等：卡已终态，再次调用不命中 running 分支 → 不改内容 → revision 不动。
    assert!(!update_dispatch_card_terminal(&c, "s1", "assignment-1", "failed", "second").unwrap());
    assert_eq!(
        get_messages(&c, "s1").unwrap()[0].revision,
        2,
        "幂等 no-op 更新不应额外递增 revision"
    );
}

#[test]
fn member_snapshot_started_at_is_backward_compatible() {
    let old_json = serde_json::json!({
        "participant_id": "worker-1",
        "assignment_id": "a1",
        "task_id": "t1",
        "name": "worker",
        "status": "done",
        "sub": "旧数据",
        "steps_total": 1,
        "steps_done": 1,
        "cost_usd": null,
        "input_tokens": 0,
        "output_tokens": 0,
        "failed": false,
        "blocks": [],
        "result": null
    });

    let member: MemberSnapshot = serde_json::from_value(old_json).unwrap();
    assert_eq!(member.started_at, None);

    let serialized = serde_json::to_value(member).unwrap();
    assert!(
        serialized.get("started_at").is_none(),
        "None started_at 不应序列化出键: {serialized}"
    );
}
