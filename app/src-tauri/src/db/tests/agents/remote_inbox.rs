#![cfg(test)]

use super::*;

// T-4b（remote control M0 §3/§4b）：remote_inbox 表 helper 单测。

fn insert_remote_inbox_session(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO sessions (id, title, created_at, namespace_id) VALUES (?1, ?1, 0, NULL)",
        [id],
    )
    .unwrap();
}

#[test]
fn init_schema_upgrades_legacy_remote_inbox_idempotently() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE remote_inbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                command_id TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                delivered_at INTEGER
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE INDEX idx_remote_inbox_pending \
             ON remote_inbox(session_id, id) \
             WHERE delivered_at IS NULL",
        [],
    )
    .unwrap();

    init_schema(&conn).unwrap();

    let mut stmt = conn.prepare("PRAGMA table_info(remote_inbox)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for column in ["attempts", "failed_at", "last_error"] {
        assert!(
            cols.iter().any(|existing| existing == column),
            "旧库 remote_inbox 应补上 {column} 列：实际 {cols:?}"
        );
    }

    let index_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_remote_inbox_pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        index_sql.contains("failed_at"),
        "旧 partial index 应重建并排除 failed 行：实际 {index_sql}"
    );

    insert_remote_inbox_session(&conn, "s-inbox-legacy");
    assert!(enqueue_remote_input(
        &conn,
        "s-inbox-legacy",
        "cmd-legacy",
        "input.send",
        r#"{"text":"legacy"}"#,
    )
    .unwrap());
    let entry = next_pending_remote_input(&conn, "s-inbox-legacy")
        .unwrap()
        .expect("升级后的旧库应能读取 pending 消息");
    mark_remote_input_failed(&conn, entry.id, "LEGACY_DELIVERY_FAILED").unwrap();
    assert_eq!(
        next_pending_remote_input(&conn, "s-inbox-legacy").unwrap(),
        None,
        "标记失败后的消息应被 pending 查询排除"
    );
    let (attempts, failed_at, last_error): (i64, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT attempts, failed_at, last_error FROM remote_inbox WHERE id = ?1",
            [entry.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(attempts, 0);
    assert!(failed_at.is_some());
    assert_eq!(last_error.as_deref(), Some("LEGACY_DELIVERY_FAILED"));

    init_schema(&conn).unwrap();
}

#[test]
fn enqueue_remote_input_is_idempotent_by_command_id() {
    let conn = mem();
    let first = enqueue_remote_input(
        &conn,
        "s-inbox-1",
        "cmd-1",
        "input.send",
        "{\"text\":\"a\"}",
    )
    .unwrap();
    let second = enqueue_remote_input(
        &conn,
        "s-inbox-1",
        "cmd-1",
        "input.send",
        "{\"text\":\"dup\"}",
    )
    .unwrap();
    assert!(first, "首次插入必须成功");
    assert!(!second, "同 command_id 二次插入必须被幂等吞掉");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM remote_inbox WHERE command_id = 'cmd-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "重复 command_id 不该产生第二行");
}

#[test]
fn remote_inbox_terminal_state_lookup_covers_pending_delivered_failed_and_missing() {
    let conn = mem();
    enqueue_remote_input(&conn, "s-inbox-state", "cmd-state", "input.send", "{}").unwrap();
    assert_eq!(
        remote_inbox_terminal_state_by_command_id(&conn, "cmd-state").unwrap(),
        Some(RemoteInboxTerminalState::Pending)
    );

    let delivered = next_pending_remote_input(&conn, "s-inbox-state")
        .unwrap()
        .unwrap();
    assert_eq!(delivered.command_id, "cmd-state");
    mark_remote_input_delivered(&conn, delivered.id).unwrap();
    assert_eq!(
        remote_inbox_terminal_state_by_command_id(&conn, "cmd-state").unwrap(),
        Some(RemoteInboxTerminalState::Delivered)
    );

    enqueue_remote_input(&conn, "s-inbox-state", "cmd-failed", "input.send", "{}").unwrap();
    let failed = next_pending_remote_input(&conn, "s-inbox-state")
        .unwrap()
        .unwrap();
    assert_eq!(failed.command_id, "cmd-failed");
    mark_remote_input_failed(&conn, failed.id, "TEST_FAILED").unwrap();

    assert_eq!(
        remote_inbox_terminal_state_by_command_id(&conn, "cmd-failed").unwrap(),
        Some(RemoteInboxTerminalState::Failed)
    );
    assert_eq!(
        remote_inbox_terminal_state_by_command_id(&conn, "cmd-missing").unwrap(),
        None
    );
}

#[test]
fn control_command_ledger_is_idempotent_and_terminal_from_insert() {
    let conn = mem();
    insert_remote_inbox_session(&conn, "s-control-ledger");

    assert!(
        record_control_command_seen(&conn, "s-control-ledger", "cmd-control-ledger-1", "{}",)
            .unwrap(),
        "首次 control command_id 应写入账本"
    );
    assert!(
        !record_control_command_seen(&conn, "s-control-ledger", "cmd-control-ledger-1", "{}",)
            .unwrap(),
        "重复 control command_id 应被 UNIQUE 幂等拒绝"
    );

    let (kind, payload, delivered_at): (String, String, Option<i64>) = conn
        .query_row(
            "SELECT kind, payload, delivered_at FROM remote_inbox \
                 WHERE command_id = 'cmd-control-ledger-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(kind, "control");
    assert_eq!(payload, "{}");
    assert!(delivered_at.is_some(), "账本行插入时必须已经是终态");
    assert_eq!(
        next_pending_remote_input(&conn, "s-control-ledger").unwrap(),
        None,
        "终态 control 账本行不得进入单会话 pending 查询"
    );
    assert!(
        !sessions_with_pending_remote_input(&conn)
            .unwrap()
            .contains(&"s-control-ledger".to_owned()),
        "终态 control 账本行不得进入启动重扫查询"
    );
}

#[test]
fn next_pending_remote_input_returns_fifo_order() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-inbox-2",
        "cmd-a",
        "input.send",
        "{\"text\":\"a\"}",
    )
    .unwrap();
    enqueue_remote_input(
        &conn,
        "s-inbox-2",
        "cmd-b",
        "input.send",
        "{\"text\":\"b\"}",
    )
    .unwrap();

    let first = next_pending_remote_input(&conn, "s-inbox-2")
        .unwrap()
        .unwrap();
    assert_eq!(first.command_id, "cmd-a", "FIFO：先插的先出");

    mark_remote_input_delivered(&conn, first.id).unwrap();

    let second = next_pending_remote_input(&conn, "s-inbox-2")
        .unwrap()
        .unwrap();
    assert_eq!(second.command_id, "cmd-b");
}

#[test]
fn next_pending_remote_input_excludes_input_answer_without_hiding_input_send() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-inbox-kind-isolation",
        "cmd-answer-pending",
        "input.answer",
        r#"{"decision_id":"d-1","option":"yes"}"#,
    )
    .unwrap();

    assert_eq!(
        next_pending_remote_input(&conn, "s-inbox-kind-isolation").unwrap(),
        None,
        "pending input.answer 只能由独立答案线程处理，绝不能进入 FIFO"
    );

    enqueue_remote_input(
        &conn,
        "s-inbox-kind-isolation",
        "cmd-send-pending",
        "input.send",
        r#"{"text":"hello"}"#,
    )
    .unwrap();
    let entry = next_pending_remote_input(&conn, "s-inbox-kind-isolation")
        .unwrap()
        .expect("同 session 的 input.send 仍应进入 FIFO");
    assert_eq!(entry.command_id, "cmd-send-pending");
    assert_eq!(entry.kind, "input.send");
}

#[test]
fn sessions_with_pending_remote_answer_reports_answer_but_not_send_only_session() {
    let conn = mem();
    insert_remote_inbox_session(&conn, "s-answer-pending");
    insert_remote_inbox_session(&conn, "s-send-only");
    enqueue_remote_input(
        &conn,
        "s-answer-pending",
        "cmd-answer-pending",
        "input.answer",
        r#"{"decision_id":"d-1","option":"yes"}"#,
    )
    .unwrap();
    enqueue_remote_input(
        &conn,
        "s-send-only",
        "cmd-send-only",
        "input.send",
        r#"{"text":"hello"}"#,
    )
    .unwrap();

    assert_eq!(
        sessions_with_pending_remote_answer(&conn).unwrap(),
        vec!["s-answer-pending".to_string()],
        "answer 启动重扫只报告有 pending input.answer 的会话"
    );
}

#[test]
fn sessions_with_pending_remote_answer_excludes_delivered_and_failed_answers() {
    let conn = mem();
    for session_id in ["s-answer-live", "s-answer-delivered", "s-answer-failed"] {
        insert_remote_inbox_session(&conn, session_id);
    }
    for (session_id, command_id) in [
        ("s-answer-live", "cmd-answer-live"),
        ("s-answer-delivered", "cmd-answer-delivered-terminal"),
        ("s-answer-failed", "cmd-answer-failed-terminal"),
    ] {
        enqueue_remote_input(
            &conn,
            session_id,
            command_id,
            "input.answer",
            r#"{"decision_id":"d-1","option":"yes"}"#,
        )
        .unwrap();
    }
    mark_remote_input_delivered_by_command_id(&conn, "cmd-answer-delivered-terminal").unwrap();
    mark_remote_input_failed_by_command_id(&conn, "cmd-answer-failed-terminal", "TEST_FAILED")
        .unwrap();

    assert_eq!(
        sessions_with_pending_remote_answer(&conn).unwrap(),
        vec!["s-answer-live".to_string()],
        "delivered/failed answer 已是终态，不得进入启动重扫"
    );
}

#[test]
fn sessions_with_pending_remote_answer_excludes_soft_deleted_sessions() {
    let conn = mem();
    insert_remote_inbox_session(&conn, "s-answer-deleted");
    enqueue_remote_input(
        &conn,
        "s-answer-deleted",
        "cmd-answer-deleted",
        "input.answer",
        r#"{"decision_id":"d-1","option":"yes"}"#,
    )
    .unwrap();
    conn.execute(
        "UPDATE sessions SET deleted_at = 1 WHERE id = 's-answer-deleted'",
        [],
    )
    .unwrap();

    assert!(
        sessions_with_pending_remote_answer(&conn)
            .unwrap()
            .is_empty(),
        "软删会话即使还有 pending answer 也不得参与启动恢复"
    );
}

#[test]
fn pending_remote_answers_returns_all_pending_answers_in_id_order_without_send() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-answer-list",
        "cmd-answer-first",
        "input.answer",
        r#"{"decision_id":"d-1","option":"yes"}"#,
    )
    .unwrap();
    enqueue_remote_input(
        &conn,
        "s-answer-list",
        "cmd-send-middle",
        "input.send",
        r#"{"text":"hello"}"#,
    )
    .unwrap();
    enqueue_remote_input(
        &conn,
        "s-answer-list",
        "cmd-answer-second",
        "input.answer",
        r#"{"decision_id":"d-2","option":"no"}"#,
    )
    .unwrap();

    let entries = pending_remote_answers(&conn, "s-answer-list").unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.command_id.as_str())
            .collect::<Vec<_>>(),
        vec!["cmd-answer-first", "cmd-answer-second"]
    );
    assert!(entries[0].id < entries[1].id, "答案必须按 id 升序返回");
    assert!(entries.iter().all(|entry| entry.kind == "input.answer"));
}

#[test]
fn pending_remote_answers_does_not_return_other_session_or_terminal_rows() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-answer-target",
        "cmd-answer-target",
        "input.answer",
        r#"{"decision_id":"d-target","option":"yes"}"#,
    )
    .unwrap();
    enqueue_remote_input(
        &conn,
        "s-answer-other",
        "cmd-answer-other",
        "input.answer",
        r#"{"decision_id":"d-other","option":"no"}"#,
    )
    .unwrap();
    enqueue_remote_input(
        &conn,
        "s-answer-target",
        "cmd-answer-terminal",
        "input.answer",
        r#"{"decision_id":"d-terminal","option":"yes"}"#,
    )
    .unwrap();
    mark_remote_input_delivered_by_command_id(&conn, "cmd-answer-terminal").unwrap();

    let entries = pending_remote_answers(&conn, "s-answer-target").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command_id, "cmd-answer-target");
    assert_eq!(entries[0].session_id, "s-answer-target");
}

#[test]
fn mark_remote_input_delivered_removes_it_from_pending_queue() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-inbox-3",
        "cmd-only",
        "input.send",
        "{\"text\":\"a\"}",
    )
    .unwrap();
    let entry = next_pending_remote_input(&conn, "s-inbox-3")
        .unwrap()
        .unwrap();
    mark_remote_input_delivered(&conn, entry.id).unwrap();

    assert_eq!(
        next_pending_remote_input(&conn, "s-inbox-3").unwrap(),
        None,
        "标记投递后不该再被 next_pending 取到"
    );
}

#[test]
fn mark_remote_input_delivered_by_command_id_sets_delivered_terminal_state() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-answer-delivered",
        "cmd-answer-delivered",
        "input.answer",
        "{}",
    )
    .unwrap();

    mark_remote_input_delivered_by_command_id(&conn, "cmd-answer-delivered").unwrap();

    assert_eq!(
        remote_inbox_terminal_state_by_command_id(&conn, "cmd-answer-delivered").unwrap(),
        Some(RemoteInboxTerminalState::Delivered)
    );
}

#[test]
fn mark_remote_input_failed_by_command_id_sets_failed_terminal_state_and_error() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-answer-failed",
        "cmd-answer-failed",
        "input.answer",
        "{}",
    )
    .unwrap();

    mark_remote_input_failed_by_command_id(&conn, "cmd-answer-failed", "NO_PENDING_QUESTION")
        .unwrap();

    assert_eq!(
        remote_inbox_terminal_state_by_command_id(&conn, "cmd-answer-failed").unwrap(),
        Some(RemoteInboxTerminalState::Failed)
    );
    let last_error: Option<String> = conn
        .query_row(
            "SELECT last_error FROM remote_inbox WHERE command_id = 'cmd-answer-failed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_error.as_deref(), Some("NO_PENDING_QUESTION"));
}

#[test]
fn sessions_with_pending_remote_input_only_reports_undelivered_sessions() {
    let conn = mem();
    insert_remote_inbox_session(&conn, "s-inbox-pending");
    insert_remote_inbox_session(&conn, "s-inbox-delivered");
    enqueue_remote_input(&conn, "s-inbox-pending", "cmd-p1", "input.send", "{}").unwrap();
    enqueue_remote_input(&conn, "s-inbox-delivered", "cmd-d1", "input.send", "{}").unwrap();
    let delivered = next_pending_remote_input(&conn, "s-inbox-delivered")
        .unwrap()
        .unwrap();
    mark_remote_input_delivered(&conn, delivered.id).unwrap();

    let sessions = sessions_with_pending_remote_input(&conn).unwrap();
    assert!(sessions.contains(&"s-inbox-pending".to_string()));
    assert!(
        !sessions.contains(&"s-inbox-delivered".to_string()),
        "已全部投递的会话不该出现在重扫列表里"
    );
}

#[test]
fn remote_input_delivery_failure_retries_twice_then_becomes_terminal() {
    let conn = mem();
    insert_remote_inbox_session(&conn, "s-inbox-retry");
    enqueue_remote_input(
        &conn,
        "s-inbox-retry",
        "cmd-retry",
        "input.send",
        r#"{"text":"retry"}"#,
    )
    .unwrap();
    let entry = next_pending_remote_input(&conn, "s-inbox-retry")
        .unwrap()
        .unwrap();

    for expected_attempts in 1..=3 {
        let attempts = record_remote_input_failure(&conn, entry.id, "AGENT_NOT_FOUND")
            .expect("记录投递失败次数");
        assert_eq!(attempts, expected_attempts);
        if attempts < 3 {
            assert!(
                next_pending_remote_input(&conn, "s-inbox-retry")
                    .unwrap()
                    .is_some(),
                "前两次失败后仍应保留 pending"
            );
            assert!(
                sessions_with_pending_remote_input(&conn)
                    .unwrap()
                    .contains(&"s-inbox-retry".to_string()),
                "前两次失败后启动重扫仍应报告该会话"
            );
        } else {
            mark_remote_input_failed(&conn, entry.id, "AGENT_NOT_FOUND").expect("第三次失败标终态");
        }
    }

    assert_eq!(
        next_pending_remote_input(&conn, "s-inbox-retry").unwrap(),
        None,
        "第三次失败终态后不得再返回"
    );
    assert!(
        !sessions_with_pending_remote_input(&conn)
            .unwrap()
            .contains(&"s-inbox-retry".to_string()),
        "第三次失败终态后启动重扫不得再报告该会话"
    );
    let (attempts, failed_at, last_error): (i64, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT attempts, failed_at, last_error FROM remote_inbox WHERE id = ?1",
            [entry.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(attempts, 3);
    assert!(failed_at.is_some());
    assert_eq!(last_error.as_deref(), Some("AGENT_NOT_FOUND"));
}

#[test]
fn mark_remote_input_failed_is_terminal_without_incrementing_attempts() {
    let conn = mem();
    enqueue_remote_input(
        &conn,
        "s-inbox-parse",
        "cmd-parse",
        "input.send",
        "{malformed",
    )
    .unwrap();
    let entry = next_pending_remote_input(&conn, "s-inbox-parse")
        .unwrap()
        .unwrap();
    mark_remote_input_failed(&conn, entry.id, "REMOTE_INBOX_PAYLOAD_MALFORMED").unwrap();

    assert_eq!(
        next_pending_remote_input(&conn, "s-inbox-parse").unwrap(),
        None,
        "parse 失败应直接终态"
    );
    let (attempts, failed_at, last_error): (i64, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT attempts, failed_at, last_error FROM remote_inbox WHERE id = ?1",
            [entry.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(attempts, 0, "parse 失败不消耗真实投递重试次数");
    assert!(failed_at.is_some());
    assert_eq!(
        last_error.as_deref(),
        Some("REMOTE_INBOX_PAYLOAD_MALFORMED")
    );
}

#[test]
fn sessions_with_pending_remote_input_excludes_soft_deleted_sessions() {
    let conn = mem();
    insert_remote_inbox_session(&conn, "s-inbox-deleted");
    enqueue_remote_input(
        &conn,
        "s-inbox-deleted",
        "cmd-deleted",
        "input.send",
        r#"{"text":"queued"}"#,
    )
    .unwrap();
    conn.execute(
        "UPDATE sessions SET deleted_at = 1 WHERE id = 's-inbox-deleted'",
        [],
    )
    .unwrap();

    assert!(
        !sessions_with_pending_remote_input(&conn)
            .unwrap()
            .contains(&"s-inbox-deleted".to_string()),
        "软删会话即使还有 pending 行也不得参与启动重扫"
    );
}
