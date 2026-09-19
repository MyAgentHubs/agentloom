#![cfg(test)]

use super::*;

fn pending_answer_entry(command_id: &str, payload: &str) -> db::RemoteInboxEntry {
    db::RemoteInboxEntry {
        id: 1,
        session_id: "s-answer-recovery".to_string(),
        command_id: command_id.to_string(),
        kind: "input.answer".to_string(),
        payload: payload.to_string(),
        created_at: 1,
    }
}

#[test]
fn parse_remote_answer_payload_accepts_decision_id_and_option() {
    assert_eq!(
        parse_remote_answer_payload(r#"{"decision_id":"d-1","option":"yes"}"#),
        Ok(("d-1".to_string(), "yes".to_string()))
    );
}

#[test]
fn parse_remote_answer_payload_rejects_missing_required_field() {
    assert_eq!(
        parse_remote_answer_payload(r#"{"decision_id":"d-1"}"#),
        Err("REMOTE_INBOX_PAYLOAD_MALFORMED".to_string())
    );
}

#[test]
fn parse_remote_answer_payload_rejects_non_string_required_fields() {
    for payload in [
        r#"{"decision_id":1,"option":"yes"}"#,
        r#"{"decision_id":"d-1","option":false}"#,
    ] {
        assert_eq!(
            parse_remote_answer_payload(payload),
            Err("REMOTE_INBOX_PAYLOAD_MALFORMED".to_string())
        );
    }
}

#[test]
fn recover_pending_remote_answers_spawns_valid_answer_without_marking_failed() {
    let entries = vec![pending_answer_entry(
        "cmd-answer-ok",
        r#"{"decision_id":"d-ok","option":"approve"}"#,
    )];
    let mut spawned = Vec::new();
    let mut failures = Vec::new();

    recover_pending_remote_answers_loop(
        entries,
        |entry, decision_id, option| {
            spawned.push((entry.command_id.clone(), decision_id, option));
            true
        },
        |command_id, error| failures.push((command_id.to_string(), error.to_string())),
    );

    assert_eq!(
        spawned,
        vec![(
            "cmd-answer-ok".to_string(),
            "d-ok".to_string(),
            "approve".to_string()
        )]
    );
    assert!(failures.is_empty(), "spawn 成功不得写失败终态");
}

#[test]
fn recover_pending_remote_answers_marks_failed_when_spawn_returns_false() {
    let entries = vec![pending_answer_entry(
        "cmd-answer-spawn-failed",
        r#"{"decision_id":"d-fail","option":"reject"}"#,
    )];
    let mut spawn_calls = 0;
    let mut failures = Vec::new();

    recover_pending_remote_answers_loop(
        entries,
        |_entry, decision_id, option| {
            spawn_calls += 1;
            assert_eq!(decision_id, "d-fail");
            assert_eq!(option, "reject");
            false
        },
        |command_id, error| failures.push((command_id.to_string(), error.to_string())),
    );

    assert_eq!(spawn_calls, 1);
    assert_eq!(
        failures,
        vec![(
            "cmd-answer-spawn-failed".to_string(),
            REMOTE_ANSWER_SPAWN_FAILED.to_string()
        )]
    );
}

#[test]
fn recover_pending_remote_answers_marks_malformed_without_spawning() {
    let entries = vec![pending_answer_entry(
        "cmd-answer-malformed",
        r#"{"decision_id":"d-missing-option"}"#,
    )];
    let mut spawn_calls = 0;
    let mut failures = Vec::new();

    recover_pending_remote_answers_loop(
        entries,
        |_entry, _decision_id, _option| {
            spawn_calls += 1;
            true
        },
        |command_id, error| failures.push((command_id.to_string(), error.to_string())),
    );

    assert_eq!(spawn_calls, 0, "畸形 payload 不得进入答案处理线程");
    assert_eq!(
        failures,
        vec![(
            "cmd-answer-malformed".to_string(),
            "REMOTE_INBOX_PAYLOAD_MALFORMED".to_string()
        )]
    );
}

#[test]
fn pending_remote_answer_restart_recovery_reaches_terminal_state() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    db::enqueue_remote_input(
        &conn,
        "s-answer-restart",
        "cmd-answer-restart",
        "input.answer",
        r#"{"decision_id":"d-restart","option":"continue"}"#,
    )
    .unwrap();
    let entries = db::pending_remote_answers(&conn, "s-answer-restart").unwrap();
    assert_eq!(entries.len(), 1, "重启前应有一条 pending answer");

    recover_pending_remote_answers_loop(
        entries,
        |entry, decision_id, option| {
            assert_eq!(decision_id, "d-restart");
            assert_eq!(option, "continue");
            db::mark_remote_input_delivered_by_command_id(&conn, &entry.command_id).unwrap();
            true
        },
        |_command_id, _error| panic!("成功恢复不得 mark_failed"),
    );

    assert!(
        db::pending_remote_answers(&conn, "s-answer-restart")
            .unwrap()
            .is_empty(),
        "启动恢复处理完后不能继续永久 pending"
    );
    assert_eq!(
        db::remote_inbox_terminal_state_by_command_id(&conn, "cmd-answer-restart").unwrap(),
        Some(db::RemoteInboxTerminalState::Delivered)
    );
}

#[test]
fn pending_remote_answer_and_input_send_queries_are_bidirectionally_isolated() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    for session_id in ["s-answer-only", "s-send-only", "s-mixed"] {
        conn.execute(
            "INSERT INTO sessions (id, title, created_at, namespace_id) VALUES (?1, ?1, 0, NULL)",
            [session_id],
        )
        .unwrap();
    }
    db::enqueue_remote_input(
        &conn,
        "s-answer-only",
        "cmd-answer-only",
        "input.answer",
        r#"{"decision_id":"d-1","option":"yes"}"#,
    )
    .unwrap();
    db::enqueue_remote_input(
        &conn,
        "s-send-only",
        "cmd-send-only-e2e",
        "input.send",
        r#"{"text":"send"}"#,
    )
    .unwrap();
    db::enqueue_remote_input(
        &conn,
        "s-mixed",
        "cmd-mixed-send",
        "input.send",
        r#"{"text":"mixed"}"#,
    )
    .unwrap();
    db::enqueue_remote_input(
        &conn,
        "s-mixed",
        "cmd-mixed-answer",
        "input.answer",
        r#"{"decision_id":"d-2","option":"no"}"#,
    )
    .unwrap();

    assert_eq!(
        db::sessions_with_pending_remote_answer(&conn).unwrap(),
        vec!["s-answer-only".to_string(), "s-mixed".to_string()]
    );
    assert_eq!(
        db::pending_remote_answers(&conn, "s-mixed")
            .unwrap()
            .into_iter()
            .map(|entry| entry.command_id)
            .collect::<Vec<_>>(),
        vec!["cmd-mixed-answer".to_string()],
        "answer 查询不得混入同 session 的 input.send"
    );
    assert_eq!(
        db::next_pending_remote_input(&conn, "s-mixed")
            .unwrap()
            .map(|entry| entry.command_id),
        Some("cmd-mixed-send".to_string()),
        "send FIFO 不得混入同 session 的 input.answer"
    );
    assert_eq!(
        db::next_pending_remote_input(&conn, "s-answer-only").unwrap(),
        None,
        "只有 answer 的会话不能被 send FIFO 取出"
    );
}

#[test]
fn startup_pending_remote_answer_rescan_calls_recovery_without_fifo_guard() {
    // 结构护栏：启动后台线程必须并列消费 answer 会话，并保持在线 answer 同款的独立线程语义。
    // 变异自证：删掉 pending_remote_answer_sessions 循环，这条测试会变红。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let loop_body = production
        .split("for session_id in pending_remote_answer_sessions {")
        .nth(1)
        .expect("setup 启动线程必须逐会话恢复 pending input.answer")
        .split("\n                }")
        .next()
        .unwrap();
    assert!(
        loop_body.contains("startup_recover_pending_remote_answers(&drain_app, &session_id)"),
        "answer 启动重扫必须调用独立恢复薄壳"
    );
    assert!(
        !loop_body.contains("try_begin_draining("),
        "answer 独立处理路径不得套 input.send FIFO 排空互斥"
    );
}

#[test]
fn remote_gateway_input_answer_handler_spawns_before_processing() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let handler = production
        .split("fn remote_gateway_input_answer_handler(")
        .nth(1)
        .unwrap()
        .split("\nfn remote_gateway_control_stop_handler(")
        .next()
        .unwrap();
    let spawn_idx = handler
        .find("spawn_remote_answer_processing(")
        .expect("input.answer 新行必须启动独立答案线程");
    let process_idx = handler
        .find("process_remote_answer(")
        .expect("独立答案线程必须调用 process_remote_answer");

    assert_eq!(
        handler.matches("process_remote_answer(").count(),
        1,
        "handler 只能在独立线程闭包中调用一次 process_remote_answer"
    );
    assert!(
        spawn_idx < process_idx && handler[spawn_idx..process_idx].contains("move ||"),
        "process_remote_answer 必须位于 spawn_remote_answer_processing 的 move 闭包内"
    );
    assert!(
        handler.contains("if !spawn_remote_answer_processing(")
            && handler.contains("mark_remote_answer_spawn_failed("),
        "独立答案线程 spawn 失败必须同步写 failed 终态"
    );
}

#[test]
fn remote_answer_terminal_error_marks_failed_without_marking_delivered() {
    let mut delivered = false;
    let mut failure = None;

    remote_answer_terminal(
        || Err("NO_PENDING_QUESTION".to_string()),
        || delivered = true,
        |error| failure = Some(error.to_owned()),
    );

    assert!(!delivered);
    assert_eq!(failure.as_deref(), Some("NO_PENDING_QUESTION"));
}

#[test]
fn remote_answer_terminal_success_marks_delivered_without_marking_failed() {
    let mut delivered = false;
    let mut failure = None;

    remote_answer_terminal(
        || Ok(AnswerLeadQuestionOutcome::quietly_not_resumed()),
        || delivered = true,
        |error| failure = Some(error.to_owned()),
    );

    assert!(delivered);
    assert_eq!(failure, None);
}
