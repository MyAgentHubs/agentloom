#![cfg(test)]

use super::*;

/// Bug B 回归钉子（验收 b）：lead 见到 NeedsDecision 终态事件（myagent 退出码 4 的正常
/// 收工）时，落库收尾卡必须是 needs_decision，不能被误判成 error/fallback。
#[test]
fn lead_needs_decision_event_reduces_to_needs_decision_status() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(
        &c,
        "s-lead-needs-decision",
        "lead needs decision",
        "local-default",
        "local",
    )
    .unwrap();

    let decision = lead_terminal_decision(false, false, false, true, false, false);
    assert_eq!(
        decision,
        LeadTerminal::EmitRunCloseout,
        "见过 NeedsDecision 不得合成假 error，也不得当 metadata-bearing Completed"
    );

    let mut reducer = display_reduce::DisplayReducer::new("run-lead-needs-decision");
    reducer.feed(&agent_event::AgentEvent::NeedsDecision {
        run_id: "run-lead-needs-decision".into(),
        reason: "scope_change".into(),
        changes: vec![agent_event::ScopeChange {
            proposal_id: "p1".into(),
            kind: "add_file".into(),
            detail_text: "want to touch an extra file".into(),
            detail_summary: None,
        }],
    });

    // decision == EmitRunCloseout（非 EmitError）→ 事件循环不产合成 error。
    let saw_error = false;
    let outcome = display_reduce::RunOutcome {
        run_id: "run-lead-needs-decision".into(),
        exit_success: false,
        interrupted: false,
        saw_error,
        saw_blocked: false,
        saw_needs_decision: true,
        finish_called: Some(true),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text: None,
    };
    let reduced = reducer
        .finish(&outcome)
        .expect("needs_decision 终态必须照落库（display_reduce.rs Finding C 白名单里）");
    persist_normal_finalizer(
        &c,
        "s-lead-needs-decision",
        "myagent",
        None,
        Some(&reduced),
        None,
    );

    let messages = db::get_messages(&c, "s-lead-needs-decision").unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.iter().any(|block| matches!(
        block,
        Block::RunTerminal { run_id, status, .. }
            if run_id == "run-lead-needs-decision" && status == "needs_decision"
    )));
    assert!(
        !messages[0]
            .content
            .iter()
            .any(|block| matches!(block, Block::RunTerminal { status, .. } if status == "error")),
        "needs_decision 收工不得同时/改判成 error 卡"
    );
}

/// Bug B 回归钉子（验收 c）：lead 见到 Blocked 终态事件（myagent 退出码 3 的正常收工）
/// 时，落库收尾卡必须是 blocked，且带 Blocked 事件的原话（last_blocked），不能被误判成
/// error/fallback。
#[test]
fn lead_blocked_event_reduces_to_blocked_status() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(
        &c,
        "s-lead-blocked",
        "lead blocked",
        "local-default",
        "local",
    )
    .unwrap();

    let decision = lead_terminal_decision(false, false, true, false, false, false);
    assert_eq!(
        decision,
        LeadTerminal::EmitRunCloseout,
        "见过 Blocked 不得合成假 error，也不得当 metadata-bearing Completed"
    );

    let mut reducer = display_reduce::DisplayReducer::new("run-lead-blocked");
    reducer.feed(&agent_event::AgentEvent::Blocked {
        message: "waiting on human input for credentials".into(),
        reason: None,
    });

    let saw_error = false;
    let outcome = display_reduce::RunOutcome {
        run_id: "run-lead-blocked".into(),
        exit_success: false,
        interrupted: false,
        saw_error,
        saw_blocked: true,
        saw_needs_decision: false,
        finish_called: Some(true),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text: None,
    };
    let reduced = reducer
        .finish(&outcome)
        .expect("blocked 终态必须照落库（display_reduce.rs Finding C 白名单里）");
    persist_normal_finalizer(&c, "s-lead-blocked", "myagent", None, Some(&reduced), None);

    let messages = db::get_messages(&c, "s-lead-blocked").unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.iter().any(|block| matches!(
        block,
        Block::RunTerminal {
            run_id,
            status,
            message: Some(message),
        } if run_id == "run-lead-blocked"
            && status == "blocked"
            && message == "waiting on human input for credentials"
    )));
}

#[test]
fn transport_terminal_barrier_observes_removed_slot_and_emits_terminal_last() {
    let root = tempfile::tempdir().unwrap();
    let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    running.0.lock().unwrap().insert(
        "s-transport-release".into(),
        RunSlot::Finalizing {
            stop_requested: false,
        },
    );
    transport
        .register_run(
            "run-transport-release",
            "s-transport-release",
            None,
            member_runner::TextGranularity::Token,
        )
        .unwrap();
    transport.push(
        "run-transport-release",
        agent_event::AgentEvent::TextDelta {
            text: "streamed".into(),
        },
    );

    let payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = payloads.clone();
    let observed_running = running.clone();
    transport.install_emitter_for_test(move |payload| {
        assert!(
            !observed_running
                .0
                .lock()
                .unwrap()
                .contains_key("s-transport-release"),
            "transport emitter must observe the slot already removed"
        );
        recorded.lock().unwrap().push(payload);
    });
    let terminal = agent_event::AgentEvent::RunCloseout {
        run_id: "run-transport-release".into(),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: Some(false),
    };

    assert!(emit_terminal_after_releasing_run_slot(
        &running,
        &team_running,
        "s-transport-release",
        "run-transport-release",
        vec![terminal.clone()],
        &transport,
        None,
    ));

    let payloads = payloads.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    let events = &payloads[0].batches[0].events;
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].event,
        agent_event::AgentEvent::TextDelta { .. }
    ));
    assert_eq!(events.last().unwrap().event, terminal);
}

/// M1-T1（remote control M0 §4c）+ M1 修复轮 P1-1（2026-08-11）：释放咽喉——槽释放后传入
/// 的 db 必须落一条 idle 行。P1-1 修复后写口改走 `refresh_session_runtime`
/// （`db::upsert_session_runtime_status`）——run_id 列不再被清空覆盖成 NULL，而是原样保留
/// 表中现值（重算口本身拿不到 run_id，见该函数文档）：这是行为变化，旧版本这里断言
/// run_id 必须清空，现在改断言 run_id 保留。
#[test]
fn emit_terminal_after_releasing_run_slot_writes_session_runtime_idle() {
    let conn = crate::test_support::mem_db();
    db::set_session_runtime(
        &conn,
        "s-runtime-release",
        "running",
        Some("run-runtime-release"),
    )
    .unwrap();

    let root = tempfile::tempdir().unwrap();
    let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    running.0.lock().unwrap().insert(
        "s-runtime-release".into(),
        RunSlot::Finalizing {
            stop_requested: false,
        },
    );
    transport
        .register_run(
            "run-runtime-release",
            "s-runtime-release",
            None,
            member_runner::TextGranularity::Token,
        )
        .unwrap();
    transport.install_emitter_for_test(|_| {});
    let terminal = agent_event::AgentEvent::RunCloseout {
        run_id: "run-runtime-release".into(),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: Some(false),
    };
    let db = crate::db::Db(crate::perf_probe::TimedMutex::new(conn));

    assert!(emit_terminal_after_releasing_run_slot(
        &running,
        &team_running,
        "s-runtime-release",
        "run-runtime-release",
        vec![terminal],
        &transport,
        Some(&db),
    ));

    let conn = db.0.lock().unwrap();
    let row = db::get_session_runtime(&conn, "s-runtime-release")
        .unwrap()
        .expect("release 后仍应留一条运行态行（idle）");
    assert_eq!(row.status, "idle");
    assert_eq!(
        row.run_id.as_deref(),
        Some("run-runtime-release"),
        "refresh 写口不覆盖 run_id——重算口本身拿不到新值，不该清空旧值"
    );
}

/// 同款释放咽喉的 `runtime_db: None` 分支——测试调用点不关心运行态表时不能报错/不能
/// 影响既有终态事件行为（回归钉：签名加参数后旧调用点仍必须原样能过）。
#[test]
fn emit_terminal_after_releasing_run_slot_tolerates_none_runtime_db() {
    let root = tempfile::tempdir().unwrap();
    let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    running.0.lock().unwrap().insert(
        "s-runtime-release-none".into(),
        RunSlot::Finalizing {
            stop_requested: false,
        },
    );
    transport
        .register_run(
            "run-runtime-release-none",
            "s-runtime-release-none",
            None,
            member_runner::TextGranularity::Token,
        )
        .unwrap();
    transport.install_emitter_for_test(|_| {});
    let terminal = agent_event::AgentEvent::RunCloseout {
        run_id: "run-runtime-release-none".into(),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: Some(false),
    };

    assert!(emit_terminal_after_releasing_run_slot(
        &running,
        &team_running,
        "s-runtime-release-none",
        "run-runtime-release-none",
        vec![terminal],
        &transport,
        None,
    ));
    assert!(!running
        .0
        .lock()
        .unwrap()
        .contains_key("s-runtime-release-none"));
}

#[test]
fn repeated_request_stop_missing_slot_stays_on_legacy_channel_without_transport_lane() {
    let root = tempfile::tempdir().unwrap();
    let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    let running = Running::default();
    transport.install_emitter_for_test(|_| {
        panic!("missing-slot stop fallback must not enter the batch channel")
    });
    let mut legacy_events = Vec::new();

    for _ in 0..2 {
        request_stop(
            &running,
            "s-stop-fallback",
            |pid| panic!("missing slot must not kill pid {pid}"),
            |event| {
                assert!(
                    running.0.try_lock().is_err(),
                    "stop fallback must hold the slot lock through synchronous legacy emit"
                );
                legacy_events.push(event.clone());
            },
        )
        .unwrap();
    }

    assert_eq!(legacy_events.len(), 2);
    assert!(legacy_events.iter().all(|event| matches!(
        event,
        agent_event::AgentEvent::RunCloseout { run_id, .. } if run_id.is_empty()
    )));
    assert_eq!(transport.high_watermarks(""), None);
}

#[test]
fn single_run_streaming_source_has_no_legacy_agent_event_emit() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let single = source
        .split("fn spawn_and_stream(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]").next())
        .expect("spawn_and_stream source slice");

    assert!(single.contains("transport.push(&run_id, event)"));
    assert!(
        !single.contains("emit_agent_event("),
        "solo streaming and terminal events must be exclusive to EventTransport"
    );
}
