#![cfg(test)]

use super::*;

fn autofeed_test_conn(team: bool, session_id: &str) -> Connection {
    let conn = crate::test_support::mem_db();
    db::seed_builtin_agents(&conn).unwrap();
    db::create_session(&conn, session_id, "autofeed", "local-default", "local").unwrap();
    if team {
        db::set_session_agent_config(
            &conn,
            session_id,
            Some("claude".to_string()),
            vec!["codex".to_string()],
        )
        .unwrap();
    }
    conn
}

fn append_autofeed_message(conn: &Connection, session_id: &str, text: &str) -> i64 {
    db::append_message(
        conn,
        session_id,
        "assistant",
        &[db::Block::Text {
            text: text.to_string(),
        }],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    let message_id = conn.last_insert_rowid();
    if text.starts_with("[Worker report]") {
        let assignment_id = text
            .lines()
            .find_map(|line| line.strip_prefix("assignment_id: "));
        conn.execute(
            "INSERT INTO member_report_delivery
                    (session_id, message_id, assignment_id, delivered_at)
                 VALUES (?1, ?2, ?3, NULL)",
            rusqlite::params![session_id, message_id, assignment_id],
        )
        .unwrap();
    }
    message_id
}

#[test]
fn autofeed_solo_session_returns_none() {
    let session_id = "s-autofeed-solo";
    let conn = autofeed_test_conn(false, session_id);
    append_autofeed_message(&conn, session_id, "[Worker report]\nsolo report");

    assert_eq!(autofeed_decision(&conn, session_id).unwrap(), None);
}

#[test]
fn autofeed_team_unconsumed_report_returns_report_id() {
    let session_id = "s-autofeed-unconsumed";
    let conn = autofeed_test_conn(true, session_id);
    append_autofeed_message(&conn, session_id, "lead dispatched work");
    let report_id = append_autofeed_message(&conn, session_id, "[Worker report]\ndone");

    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(report_id)
    );
}

#[test]
fn autofeed_globalstop_silences_until_new_user_message_and_clears_record() {
    let session_id = "s-autofeed-globalstop-silence";
    let conn = autofeed_test_conn(true, session_id);
    append_autofeed_message(&conn, session_id, "lead dispatched work");
    let report_id = append_autofeed_message(&conn, session_id, "[Worker report]\nstopped");
    AUTOFEED_GLOBAL_STOP
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(session_id.to_string(), report_id);

    assert_eq!(autofeed_decision(&conn, session_id).unwrap(), None);

    db::append_message(
        &conn,
        session_id,
        "user",
        &[db::Block::Text {
            text: "继续".to_string(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    assert!(conn.last_insert_rowid() > report_id);
    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(report_id)
    );
    assert!(!AUTOFEED_GLOBAL_STOP
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(session_id));
}

#[test]
fn autofeed_globalstop_new_assistant_message_does_not_clear_silence() {
    let session_id = "s-autofeed-globalstop-assistant";
    let conn = autofeed_test_conn(true, session_id);
    let report_id = append_autofeed_message(&conn, session_id, "[Worker report]\nstopped");
    record_autofeed_global_stop(session_id, report_id);
    let assistant_id = append_autofeed_message(&conn, session_id, "late assistant update");
    assert!(assistant_id > report_id);

    assert_eq!(autofeed_decision(&conn, session_id).unwrap(), None);
    assert_eq!(
        AUTOFEED_GLOBAL_STOP
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session_id)
            .copied(),
        Some(report_id)
    );
    clear_autofeed_global_stop(session_id);
}

#[test]
fn autofeed_recheck_blocks_start_when_globalstop_arrives_after_decision() {
    let session_id = "s-autofeed-recheck-race";
    let conn = autofeed_test_conn(true, session_id);
    let report_id = append_autofeed_message(&conn, session_id, "[Worker report]\ndone");
    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(report_id)
    );

    record_autofeed_global_stop(session_id, report_id);
    let starts = std::cell::Cell::new(0);
    if autofeed_recheck_before_start(&conn, session_id).unwrap() {
        starts.set(starts.get() + 1);
    }
    assert_eq!(starts.get(), 0);
    clear_autofeed_global_stop(session_id);
}

#[test]
fn globalstop_clear_session_stop_state_clears_member_stop_and_autofeed_silence() {
    let session_id = "s-globalstop-user-revive";
    let team_running = member_runner::TeamRunning::default();
    team_running.mark_session_stopped(session_id);
    record_autofeed_global_stop(session_id, 7);

    clear_session_stop_state(&team_running, session_id);
    assert!(!team_running.is_session_stopped(session_id));
    assert!(!AUTOFEED_GLOBAL_STOP
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(session_id));
}

#[test]
fn globalstop_resume_and_autofeed_message_none_gate_returns_err_and_releases_reserved_slot() {
    let session_id = "s-globalstop-none-post-reserve";
    let conn = crate::test_support::mem_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    team_running.mark_session_stopped(session_id);
    record_autofeed_global_stop(session_id, 17);

    // try_resume_pending 只委托 start_lead_session(message=None)，
    // 因而落到这个占槽后门；不需分别复制命令层测试。
    let error = match reserve_lead_start_after_globalstop(
        &conn,
        &running,
        &team_running,
        session_id,
        Locale::Zh,
        false,
    ) {
        Ok(_) => panic!("停止标记已设时 message=None 必须返回可辨识错误"),
        Err(error) => error,
    };

    assert!(
        error.contains("run.globallyStopped"),
        "错误必须带稳定 AL_ERR code，供前端 catch 清理乐观 run：{error}"
    );
    assert!(team_running.is_session_stopped(session_id));
    assert!(AUTOFEED_GLOBAL_STOP
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(session_id));
    assert!(
        try_reserve(&running, session_id).is_ok(),
        "返回 Err 前必须由 ReservationGuard 释放刚占到的槽"
    );
    running.0.lock().unwrap().remove(session_id);
    team_running.clear_session_stopped(session_id);
    clear_autofeed_global_stop(session_id);
}

#[test]
fn globalstop_user_start_clears_only_after_reservation_succeeds() {
    let session_id = "s-globalstop-user-post-reserve";
    let conn = crate::test_support::mem_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    team_running.mark_session_stopped(session_id);
    record_autofeed_global_stop(session_id, 11);
    try_reserve(&running, session_id).unwrap();

    let error = match reserve_lead_start_after_globalstop(
        &conn,
        &running,
        &team_running,
        session_id,
        Locale::Zh,
        true,
    ) {
        Ok(_) => panic!("已占槽时用户启动必须返回 busy"),
        Err(error) => error,
    };
    assert!(error.starts_with("SESSION_ALREADY_RUNNING:"));
    assert!(team_running.is_session_stopped(session_id));
    assert!(AUTOFEED_GLOBAL_STOP
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(session_id));

    running.0.lock().unwrap().remove(session_id);
    let guard = reserve_lead_start_after_globalstop(
        &conn,
        &running,
        &team_running,
        session_id,
        Locale::Zh,
        true,
    )
    .unwrap()
    .expect("占槽成功的用户主动启动应继续");
    assert!(!team_running.is_session_stopped(session_id));
    assert!(!AUTOFEED_GLOBAL_STOP
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(session_id));
    drop(guard);
}

#[test]
fn globalstop_poisoned_db_and_running_still_kill_all_members_and_report_errors() {
    let session_id = "s-globalstop-poisoned-stop";
    let db = Arc::new(test_db());
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    team_running.register(
        &member_runner::MemberKey::new(session_id, "run-a", "assignment-a"),
        101,
    );
    team_running.register(
        &member_runner::MemberKey::new(session_id, "run-b", "assignment-b"),
        102,
    );

    let poisoned_db = db.clone();
    let _ = std::thread::spawn(move || {
        let _guard = poisoned_db.0.lock().unwrap();
        panic!("poison db for globalstop contract");
    })
    .join();
    let poisoned_running = running.clone();
    let _ = std::thread::spawn(move || {
        let _guard = poisoned_running.0.lock().unwrap();
        panic!("poison running for globalstop contract");
    })
    .join();

    let killed = std::cell::Cell::new(0);
    let error = stop_session_with(
        &db,
        &running,
        &team_running,
        session_id,
        |_| killed.set(killed.get() + 1),
        |_| {},
    )
    .unwrap_err();

    assert_eq!(killed.get(), 2, "lead 锁失败也必须继续遍历并杀完所有成员");
    assert!(error.contains("global stop watermark failed:"));
    assert!(error.contains("initial lead stop failed:"));
    assert!(error.contains("final lead stop failed:"));
    assert!(error.contains("poisoned"), "锁失败详情不得被吞掉: {error}");
}

#[test]
fn autofeed_pending_report_survives_later_lead_message() {
    let session_id = "s-autofeed-pending-after-lead";
    let conn = autofeed_test_conn(true, session_id);
    let report_id = append_autofeed_message(
        &conn,
        session_id,
        "[Worker report]\nassignment_id: assignment-pending\nstatus: done",
    );
    append_autofeed_message(&conn, session_id, "lead consumed report");

    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(report_id)
    );
}

#[test]
fn autofeed_wait_delivered_report_is_not_fed_again() {
    let session_id = "s-autofeed-wait-delivered";
    let conn = autofeed_test_conn(true, session_id);
    append_autofeed_message(
        &conn,
        session_id,
        "[Worker report]\nassignment_id: assignment-wait\nstatus: done",
    );

    assert_eq!(
        ack_autofeed_result_delivery(&conn, session_id, "assignment-wait"),
        Ok(true)
    );
    assert_eq!(autofeed_decision(&conn, session_id).unwrap(), None);
}

#[test]
fn autofeed_timeout_unmarked_report_is_fed() {
    let session_id = "s-autofeed-timeout-unmarked";
    let conn = autofeed_test_conn(true, session_id);
    let report_id = append_autofeed_message(
        &conn,
        session_id,
        "[Worker report]\nassignment_id: assignment-timeout\nstatus: done",
    );
    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(report_id)
    );
}

#[test]
fn autofeed_ledger_older_pending_report_survives_newer_ack() {
    let session_id = "s-autofeed-older-pending";
    let conn = autofeed_test_conn(true, session_id);
    let older_report_id = append_autofeed_message(
        &conn,
        session_id,
        "[Worker report]\nassignment_id: assignment-old\nstatus: done",
    );
    append_autofeed_message(
        &conn,
        session_id,
        "[Worker report]\nassignment_id: assignment-new\nstatus: done",
    );

    assert_eq!(
        ack_autofeed_result_delivery(&conn, session_id, "assignment-new"),
        Ok(true)
    );
    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(older_report_id)
    );
}

#[test]
fn autofeed_all_acked_reports_return_none() {
    let session_id = "s-autofeed-all-acked";
    let conn = autofeed_test_conn(true, session_id);
    for assignment_id in ["assignment-a", "assignment-b"] {
        append_autofeed_message(
            &conn,
            session_id,
            &format!("[Worker report]\nassignment_id: {assignment_id}\nstatus: done"),
        );
        assert_eq!(
            ack_autofeed_result_delivery(&conn, session_id, assignment_id),
            Ok(true)
        );
    }

    assert_eq!(autofeed_decision(&conn, session_id).unwrap(), None);
}

#[test]
fn autofeed_decision_is_idempotent_until_assignment_ack() {
    let session_id = "s-autofeed-idempotent-pending";
    let conn = autofeed_test_conn(true, session_id);
    let report_id = append_autofeed_message(
        &conn,
        session_id,
        "[Worker report]\nassignment_id: assignment-idempotent\nstatus: done",
    );

    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(report_id)
    );
    assert_eq!(
        autofeed_decision(&conn, session_id).unwrap(),
        Some(report_id)
    );
    assert_eq!(
        ack_autofeed_result_delivery(&conn, session_id, "assignment-idempotent"),
        Ok(true)
    );
    assert_eq!(autofeed_decision(&conn, session_id).unwrap(), None);
}

#[test]
fn autofeed_decision_returns_db_error() {
    let session_id = "s-autofeed-db-error";
    let conn = autofeed_test_conn(true, session_id);
    conn.execute("DROP TABLE member_report_delivery", [])
        .unwrap();

    let error = autofeed_decision(&conn, session_id).unwrap_err();
    assert!(error.to_string().contains("member_report_delivery"));
}

#[test]
fn persist_lead_start_message_p1_2_none_never_appends() {
    // P1-②（opus 对抗审）核心钉子：resume 路径（message=None）绝不落第二条用户消息——
    // 变异测试：把 persist_lead_start_message 里的 `let Some(text) = message else { .. }`
    // 守卫删掉/永远落库，这条测试立刻变红。
    let db = test_db();
    let conn = db.0.lock().unwrap();
    persist_lead_start_message(
        &conn,
        "s-resume",
        "lead-1",
        "Lead",
        None,
        &display_reduce::user_send_key("run-resume-test"),
    )
    .expect("None message must be a no-op, not an error");
    let msgs = db::get_messages(&conn, "s-resume").unwrap();
    assert_eq!(msgs.len(), 0, "message=None 不应产生任何落库消息: {msgs:?}");
}

#[test]
fn persist_lead_start_message_some_appends_exactly_one_user_message() {
    // 真实首轮/带话续写路径（message=Some）行为不变：落恰好一条 user 消息。
    let db = test_db();
    let conn = db.0.lock().unwrap();
    persist_lead_start_message(
        &conn,
        "s-first",
        "lead-1",
        "Lead",
        Some("做点什么"),
        &display_reduce::user_send_key("run-first-test"),
    )
    .expect("Some message should persist");
    let msgs = db::get_messages(&conn, "s-first").unwrap();
    assert_eq!(msgs.len(), 1, "message=Some 应恰好落一条消息: {msgs:?}");
    assert_eq!(msgs[0].role, "user");
    match &msgs[0].content[0] {
        db::Block::Text { text } => assert_eq!(text, "做点什么"),
        other => panic!("期望 Text·得到 {other:?}"),
    }
    // P0-c 返工（测试硬度钉②-a）：本机路 dedup_key 字面断言——不满足于「有落库」，直接钉
    // `user_send:{run_id}` 这把键工厂的字面输出（比对硬编码字符串，不再调用
    // `display_reduce::user_send_key` 复算期望值——那样「键工厂改常量」类变异会同时改动
    // 期望值和实际值，测试永远不红），防「键工厂改常量」类变异。
    let dedup_key_1: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE id = ?1",
            [msgs[0].id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        dedup_key_1, "user_send:run-first-test",
        "本机 send 落库行 dedup_key 必须字面等于 user_send:{{run_id}}"
    );

    // 再送一次不同 run_id：两次不同 send 必须各自派生出不同的键，否则同键会被
    // `INSERT OR IGNORE` 悄悄吞掉第二条——这类回归比"落库行数不对"更隐蔽。
    persist_lead_start_message(
        &conn,
        "s-first",
        "lead-1",
        "Lead",
        Some("再做点别的"),
        &display_reduce::user_send_key("run-second-test"),
    )
    .expect("second send with a different run_id should also persist");
    let msgs = db::get_messages(&conn, "s-first").unwrap();
    assert_eq!(
        msgs.len(),
        2,
        "两次不同 run_id 的 send 应各自落一条: {msgs:?}"
    );
    let dedup_key_2: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE id = ?1",
            [msgs[1].id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        dedup_key_2, "user_send:run-second-test",
        "第二次 send 的 dedup_key 必须字面等于 user_send:{{run_id}}"
    );
    assert_ne!(
        dedup_key_1, dedup_key_2,
        "两次不同 run_id 的 send 必须派生出两把不同的键"
    );
}
