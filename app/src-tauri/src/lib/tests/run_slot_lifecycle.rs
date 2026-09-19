#![cfg(test)]

use super::*;

#[test]
fn new_run_reservation_rejects_running_team_pending_row() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    db::insert_team_run_pending(&conn, "s-team-pending", "r1", "goal", "lead", "[]").unwrap();

    let err = reserve_new_session_run(
        &conn,
        &Running::default(),
        &member_runner::TeamRunning::default(),
        "s-team-pending",
        Locale::En,
    )
    .unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:run.teamMembersActive:{"detail":"Team members are still executing assignments from the previous run"}"#
    );
}

#[test]
fn mutating_slot_blocks_new_run_reservation() {
    let running = Running(std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::HashMap::new(),
    )));
    // 占住 mutating slot
    let guard = reserve_mutation(&running, "s1", "undo").expect("首次占位应成功");
    // 同 session 再 try_reserve（新 run）应被挡
    assert!(
        try_reserve(&running, "s1").is_err(),
        "Mutating 占位期间 try_reserve 应返 busy"
    );
    // 释放后可再占
    drop(guard);
    assert!(try_reserve(&running, "s1").is_ok(), "释放后应可 reserve");
}

#[test]
fn reservation_guard_drop_releases_launching_slot() {
    let running = Running::default();
    try_reserve(&running, "s-drop").unwrap();
    {
        let _guard = ReservationGuard::new(running.clone(), "s-drop".to_string());
    }
    assert!(running.0.lock().unwrap().get("s-drop").is_none());
}

#[test]
fn reservation_guard_disarm_keeps_launching_slot() {
    let running = Running::default();
    try_reserve(&running, "s-disarm").unwrap();
    {
        let mut guard = ReservationGuard::new(running.clone(), "s-disarm".to_string());
        guard.disarm();
    }
    assert!(matches!(
        running.0.lock().unwrap().get("s-disarm"),
        Some(RunSlot::Launching {
            stop_requested: false
        })
    ));
}

#[test]
fn reserve_team_run_slot_rejects_when_occupied() {
    let running = Running::default();
    reserve_team_run_slot(&running, "s-team").unwrap();
    let err = reserve_team_run_slot(&running, "s-team").unwrap_err();
    assert_eq!(err, "SESSION_ALREADY_RUNNING:s-team");
    assert!(matches!(
        running.0.lock().unwrap().get("s-team"),
        Some(RunSlot::TeamRun)
    ));
}

#[test]
fn release_team_run_slot_is_noop_on_foreign_slot() {
    // release_team_run_slot 只在槽仍是本次占的 TeamRun 标记时才删——目前没有任何非
    // team-run 路径会调用本函数（`run_single_worker` 那条 lead 自己 dispatch_worker
    // 同步派单单个队员的路径压根不调它），这里纯是防御性验证：万一将来某个调用点误传了
    // 不属于自己的 session_id（比如槽属于 lead 自身的 Running(pid)），必须原样保留、
    // 不能被误删。
    let running = Running::default();
    try_reserve(&running, "s-lead-owned").unwrap();
    release_team_run_slot(&running, "s-lead-owned");
    assert!(matches!(
        running.0.lock().unwrap().get("s-lead-owned"),
        Some(RunSlot::Launching {
            stop_requested: false
        })
    ));
}

#[test]
fn team_run_slot_guard_drop_releases_slot() {
    // 钉子③（异常路径）：start_team_run 在真正 spawn 队员之前的准备阶段（写
    // team_run_pending / goal 事件 / EventTransport 注册……）提前 `?` 失败退出，槽必须靠
    // guard 的 Drop 兜底释放，不能永久残留把该会话的 delete/archive 卡死。
    let running = Running::default();
    reserve_team_run_slot(&running, "s-team-abort").unwrap();
    {
        let _guard = TeamRunSlotGuard::new(running.clone(), "s-team-abort".to_string());
        // armed 状态下 drop（模拟提前失败退出，从未走到 spawn 循环/disarm）
    }
    assert!(running.0.lock().unwrap().get("s-team-abort").is_none());
}

#[test]
fn team_run_slot_guard_disarm_keeps_slot() {
    let running = Running::default();
    reserve_team_run_slot(&running, "s-team-handoff").unwrap();
    {
        let mut guard = TeamRunSlotGuard::new(running.clone(), "s-team-handoff".to_string());
        guard.disarm();
    }
    assert!(matches!(
        running.0.lock().unwrap().get("s-team-handoff"),
        Some(RunSlot::TeamRun)
    ));
}

#[test]
fn request_stop_marks_launching_without_kill() {
    let running = Running::default();
    try_reserve(&running, "s-stop").unwrap();
    let kill_count = std::cell::Cell::new(0);

    request_stop(
        &running,
        "s-stop",
        |_| kill_count.set(kill_count.get() + 1),
        |_| {},
    )
    .unwrap();
    assert_eq!(kill_count.get(), 0, "Launching 态 stop 不应 kill");
    assert!(matches!(
        running.0.lock().unwrap().get("s-stop"),
        Some(RunSlot::Launching {
            stop_requested: true
        })
    ));
}

#[test]
fn request_stop_missing_slot_emits_terminal_release_idempotently() {
    let running = Running::default();
    let mut emitted = Vec::new();

    request_stop(
        &running,
        "s-missing",
        |pid| panic!("missing slot must not kill pid {pid}"),
        |event| {
            assert!(
                running.0.try_lock().is_err(),
                "None 检查与兜底 closeout emit 必须处于同一临界区"
            );
            emitted.push(event.clone());
        },
    )
    .unwrap();
    request_stop(
        &running,
        "s-missing",
        |pid| panic!("missing slot must not kill pid {pid}"),
        |event| {
            assert!(
                running.0.try_lock().is_err(),
                "重复 stop 的兜底 emit 也必须持有 slot 锁"
            );
            emitted.push(event.clone());
        },
    )
    .unwrap();

    assert_eq!(emitted.len(), 2, "重复 stop 应可重复安全补发终态");
    assert!(emitted.iter().all(|event| matches!(
        event,
        agent_event::AgentEvent::RunCloseout {
            run_id,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: Some(true),
        } if run_id.is_empty()
    )));
}

#[test]
fn request_stop_lead_launching_fast_path_emits_closeout() {
    let root = tempfile::tempdir().unwrap();
    let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    transport
        .register_run(
            "lead-fast-run",
            "s-lead-fast-stop",
            None,
            member_runner::TextGranularity::Line,
        )
        .unwrap();
    let payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = payloads.clone();
    transport.install_emitter_for_test(move |payload| recorded.lock().unwrap().push(payload));
    let running = Running::default();
    try_reserve(&running, "s-lead-fast-stop").unwrap();
    request_stop(&running, "s-lead-fast-stop", |_| {}, |_| {}).unwrap();
    let killed_pid = std::cell::Cell::new(None);
    let terminated = AtomicBool::new(false);
    let team_running = member_runner::TeamRunning::default();

    let proceed = transition_lead_spawn_handoff(
        &running,
        &team_running,
        None,
        &terminated,
        "s-lead-fast-stop",
        4242,
        "lead-fast-run",
        |pid| killed_pid.set(Some(pid)),
        |event| {
            assert!(
                running.0.lock().unwrap().get("s-lead-fast-stop").is_none(),
                "closeout 前必须先释放 Launching slot"
            );
            transport
                .flush_barrier("lead-fast-run", vec![event.clone()])
                .unwrap();
        },
    )
    .unwrap();

    assert!(!proceed);
    assert!(terminated.load(Ordering::SeqCst));
    assert_eq!(killed_pid.get(), Some(4242));
    let payloads = payloads.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    assert!(matches!(
        payloads[0].batches[0].events.as_slice(),
        [event_transport::SequencedEvent {
            event: agent_event::AgentEvent::RunCloseout {
            run_id,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: Some(true),
            },
            ..
        }] if run_id == "lead-fast-run"
    ));
}

#[test]
fn launching_stop_handoff_transitions_to_finalizing_and_continues_finalizer() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    try_reserve(&running, "s-fast-stop").unwrap();
    request_stop(&running, "s-fast-stop", |_| {}, |_| {}).unwrap();

    let action =
        transition_spawn_handoff(&running, &team_running, None, "s-fast-stop", 4242).unwrap();

    assert_eq!(action, SpawnHandoffAction::StopAndFinalize);
    assert!(matches!(
        running.0.lock().unwrap().get("s-fast-stop"),
        Some(RunSlot::Finalizing {
            stop_requested: true
        })
    ));
    assert!(
        try_reserve(&running, "s-fast-stop").is_err(),
        "fast Stop must stay busy until the shared finalizer releases the slot"
    );
}

#[test]
fn auth_retry_lead_handoff_keeps_slot_and_honors_stop() {
    let running = Running::default();
    running.0.lock().unwrap().insert(
        "s-auth-retry".to_string(),
        RunSlot::Finalizing {
            stop_requested: false,
        },
    );

    assert!(transition_auth_retry_handoff(&running, "s-auth-retry", 5151).unwrap());
    assert!(matches!(
        running.0.lock().unwrap().get("s-auth-retry"),
        Some(RunSlot::Running(5151))
    ));

    running.0.lock().unwrap().insert(
        "s-auth-retry".to_string(),
        RunSlot::Finalizing {
            stop_requested: true,
        },
    );
    assert!(!transition_auth_retry_handoff(&running, "s-auth-retry", 5252).unwrap());
    assert!(matches!(
        running.0.lock().unwrap().get("s-auth-retry"),
        Some(RunSlot::Finalizing {
            stop_requested: true
        })
    ));
}

#[test]
fn auth_retry_handoff_classification_covers_all_results_and_stop_states() {
    let cases = [
        (
            "success_without_stop",
            Ok(true),
            false,
            AuthRetryHandoff::Continue,
        ),
        (
            "success_with_stop",
            Ok(true),
            true,
            AuthRetryHandoff::Continue,
        ),
        (
            "lost_slot_without_stop",
            Ok(false),
            false,
            AuthRetryHandoff::Failed {
                detail: "auth retry lost the run slot".to_string(),
            },
        ),
        (
            "lost_slot_with_stop",
            Ok(false),
            true,
            AuthRetryHandoff::Interrupted,
        ),
        (
            "poisoned_without_stop",
            Err("poisoned".to_string()),
            false,
            AuthRetryHandoff::Failed {
                detail: "auth retry handoff failed: poisoned".to_string(),
            },
        ),
        (
            "poisoned_with_stop",
            Err("poisoned".to_string()),
            true,
            AuthRetryHandoff::Interrupted,
        ),
    ];

    for (label, handoff, stop_requested, expected) in cases {
        assert_eq!(
            classify_auth_retry_handoff(handoff, stop_requested),
            expected,
            "{label}"
        );
    }
}

#[test]
fn auth_retry_resolve_continue_skips_cleanup_and_stop_read() {
    let calls = std::cell::RefCell::new(Vec::new());

    let result = resolve_auth_retry_handoff(
        Ok(true),
        || calls.borrow_mut().push("cleanup"),
        || {
            calls.borrow_mut().push("read_stop");
            true
        },
    );

    assert_eq!(result, AuthRetryHandoff::Continue);
    assert_eq!(*calls.borrow(), Vec::<&str>::new());
}

#[test]
fn auth_retry_resolve_lost_slot_with_stop_cleans_up_before_reading_stop() {
    let calls = std::cell::RefCell::new(Vec::new());

    let result = resolve_auth_retry_handoff(
        Ok(false),
        || calls.borrow_mut().push("cleanup"),
        || {
            calls.borrow_mut().push("read_stop");
            true
        },
    );

    assert_eq!(*calls.borrow(), vec!["cleanup", "read_stop"]);
    assert_eq!(result, AuthRetryHandoff::Interrupted);
}

#[test]
fn auth_retry_resolve_lost_slot_without_stop_cleans_up_before_reading_stop() {
    let calls = std::cell::RefCell::new(Vec::new());

    let result = resolve_auth_retry_handoff(
        Ok(false),
        || calls.borrow_mut().push("cleanup"),
        || {
            calls.borrow_mut().push("read_stop");
            false
        },
    );

    assert_eq!(*calls.borrow(), vec!["cleanup", "read_stop"]);
    assert_eq!(
        result,
        AuthRetryHandoff::Failed {
            detail: "auth retry lost the run slot".to_string(),
        }
    );
}

#[test]
fn auth_retry_resolve_error_with_stop_cleans_up_before_reading_stop() {
    let calls = std::cell::RefCell::new(Vec::new());

    let result = resolve_auth_retry_handoff(
        Err("poisoned".to_string()),
        || calls.borrow_mut().push("cleanup"),
        || {
            calls.borrow_mut().push("read_stop");
            true
        },
    );

    assert_eq!(*calls.borrow(), vec!["cleanup", "read_stop"]);
    assert_eq!(result, AuthRetryHandoff::Interrupted);
}

#[test]
fn auth_retry_resolve_error_without_stop_cleans_up_before_reading_stop() {
    let calls = std::cell::RefCell::new(Vec::new());

    let result = resolve_auth_retry_handoff(
        Err("poisoned".to_string()),
        || calls.borrow_mut().push("cleanup"),
        || {
            calls.borrow_mut().push("read_stop");
            false
        },
    );

    assert_eq!(*calls.borrow(), vec!["cleanup", "read_stop"]);
    assert_eq!(
        result,
        AuthRetryHandoff::Failed {
            detail: "auth retry handoff failed: poisoned".to_string(),
        }
    );
}

#[test]
fn abort_handoff_kills_before_unlock_and_preserves_foreign_slot() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    running.0.lock().unwrap().insert(
        "s-abort-foreign".to_string(),
        RunSlot::Mutating { op: "undo" },
    );
    let kill_count = std::cell::Cell::new(0);

    let action = transition_spawn_handoff_with_abort_kill(
        &running,
        &team_running,
        None,
        "s-abort-foreign",
        4242,
        |pid| {
            assert_eq!(pid, 4242);
            assert!(
                running.0.try_lock().is_err(),
                "abort kill must happen while reservation is still mutually excluded"
            );
            kill_count.set(kill_count.get() + 1);
        },
    )
    .unwrap();

    assert_eq!(action, SpawnHandoffAction::Abort);
    assert_eq!(kill_count.get(), 1, "abort path must kill exactly once");
    assert!(matches!(
        running.0.lock().unwrap().get("s-abort-foreign"),
        Some(RunSlot::Mutating { op: "undo" })
    ));
    assert!(
        try_reserve(&running, "s-abort-foreign").is_err(),
        "abort must not delete a slot owned by another operation"
    );
}

#[test]
fn abort_handoff_without_slot_kills_before_reservation_can_resume() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let kill_count = std::cell::Cell::new(0);

    let action = transition_spawn_handoff_with_abort_kill(
        &running,
        &team_running,
        None,
        "s-abort-missing",
        4343,
        |pid| {
            assert_eq!(pid, 4343);
            assert!(
                running.0.try_lock().is_err(),
                "same-session reservation must remain excluded until kill is issued"
            );
            kill_count.set(kill_count.get() + 1);
        },
    )
    .unwrap();

    assert_eq!(action, SpawnHandoffAction::Abort);
    assert_eq!(kill_count.get(), 1, "abort path must kill exactly once");
    assert!(
        try_reserve(&running, "s-abort-missing").is_ok(),
        "a missing slot may become reservable only after abort kill returns"
    );
}

#[test]
fn spawn_abort_cleanup_disarms_guard_before_waiting() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-abort-wait-reserve";
    try_reserve(&running, session_id).unwrap();
    let mut old_guard = ReservationGuard::new(running.clone(), session_id.to_string());
    let order = Arc::new(Mutex::new(Vec::new()));
    let disarm_order = order.clone();
    old_guard.test_on_disarm = Some(Box::new(move || {
        disarm_order.lock().unwrap().push("disarm");
    }));

    // 模拟 handoff 前 slot 异常丢失，走 missing-slot Abort。
    running.0.lock().unwrap().remove(session_id);
    let action = transition_spawn_handoff_with_abort_kill(
        &running,
        &team_running,
        None,
        session_id,
        4444,
        |_| {},
    )
    .unwrap();
    assert_eq!(action, SpawnHandoffAction::Abort);

    let wait_order = order.clone();
    wait_for_aborted_child(&mut old_guard, || {
        wait_order.lock().unwrap().push("wait");
        assert!(
            try_reserve(&running, session_id).is_ok(),
            "a new request may reserve while the killed child is being reaped"
        );
    });
    assert_eq!(
        order.lock().unwrap().as_slice(),
        ["disarm", "wait"],
        "the old guard must be disarmed before child cleanup starts"
    );

    drop(old_guard);
    assert!(matches!(
        running.0.lock().unwrap().get(session_id),
        Some(RunSlot::Launching {
            stop_requested: false
        })
    ));
}

#[test]
fn spawn_abort_register_failure_obeys_slot_ownership() {
    let cases = [
        ("running", RunSlot::Running(4545), true),
        (
            "finalizing",
            RunSlot::Finalizing {
                stop_requested: true,
            },
            true,
        ),
        ("foreign", RunSlot::Running(9999), false),
    ];

    for (label, slot, removes_slot) in cases {
        let running = Running::default();
        let team_running = member_runner::TeamRunning::default();
        let session_id = format!("s-register-failed-{label}");
        running.0.lock().unwrap().insert(session_id.clone(), slot);
        let mut guard = ReservationGuard::new(running.clone(), session_id.clone());
        let kills = std::cell::Cell::new(0);
        let waits = std::cell::Cell::new(0);

        abort_spawn_after_register_failure(
            &running,
            &team_running,
            None,
            &session_id,
            4545,
            &mut guard,
            |pid| {
                assert_eq!(pid, 4545);
                kills.set(kills.get() + 1);
            },
            || waits.set(waits.get() + 1),
        )
        .unwrap();

        assert_eq!(kills.get(), 1, "{label}: kill must run exactly once");
        assert_eq!(waits.get(), 1, "{label}: wait must run exactly once");
        let slots = running.0.lock().unwrap();
        assert_eq!(
            slots.contains_key(&session_id),
            !removes_slot,
            "{label}: slot ownership must control removal"
        );
        if !removes_slot {
            assert!(matches!(
                slots.get(&session_id),
                Some(RunSlot::Running(9999))
            ));
        }
    }
}

#[test]
fn spawn_abort_register_failure_removes_only_the_target_session() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-register-failed-target";
    let unrelated_session_id = "s-register-failed-unrelated";
    {
        let mut slots = running.0.lock().unwrap();
        slots.insert(session_id.to_string(), RunSlot::Running(4545));
        slots.insert(unrelated_session_id.to_string(), RunSlot::Running(7878));
    }
    let mut guard = ReservationGuard::new(running.clone(), session_id.to_string());

    abort_spawn_after_register_failure(
        &running,
        &team_running,
        None,
        session_id,
        4545,
        &mut guard,
        |_| {},
        || {},
    )
    .unwrap();

    let slots = running.0.lock().unwrap();
    assert!(
        !slots.contains_key(session_id),
        "the owned target slot must be removed"
    );
    assert!(matches!(
        slots.get(unrelated_session_id),
        Some(RunSlot::Running(7878))
    ));
}

#[test]
fn spawn_abort_register_failure_kills_then_disarms_and_waits_after_unlock() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-register-failed";
    try_reserve(&running, session_id).unwrap();
    let mut guard = ReservationGuard::new(running.clone(), session_id.to_string());
    let action = transition_spawn_handoff(&running, &team_running, None, session_id, 4545).unwrap();
    assert_eq!(action, SpawnHandoffAction::Stream);

    let order = Arc::new(Mutex::new(Vec::new()));
    let disarm_order = order.clone();
    guard.test_on_disarm = Some(Box::new(move || {
        disarm_order.lock().unwrap().push("disarm");
    }));
    let kill_order = order.clone();
    let wait_order = order.clone();
    abort_spawn_after_register_failure(
        &running,
        &team_running,
        None,
        session_id,
        4545,
        &mut guard,
        |pid| {
            assert_eq!(pid, 4545);
            assert!(
                running.0.try_lock().is_err(),
                "kill must run under slot lock"
            );
            kill_order.lock().unwrap().push("kill");
        },
        || {
            assert!(
                running.0.try_lock().is_ok(),
                "child cleanup must run after releasing the slot lock"
            );
            wait_order.lock().unwrap().push("wait");
        },
    )
    .unwrap();

    assert_eq!(
        order.lock().unwrap().as_slice(),
        ["kill", "disarm", "wait"],
        "cleanup ordering must exclude a live child before reopening the slot"
    );
    assert!(
        !running.0.lock().unwrap().contains_key(session_id),
        "register failure must release its Running slot"
    );
    request_stop(
        &running,
        session_id,
        |pid| panic!("released slot must not kill recycled pid {pid}"),
        |_| {},
    )
    .unwrap();
}

#[test]
fn spawn_abort_register_failure_poison_still_kills_and_waits() {
    let running = Running::default();
    let running_to_poison = running.clone();
    std::thread::spawn(move || {
        let _slots = running_to_poison.0.lock().unwrap();
        panic!("poison Running for register-failure cleanup");
    })
    .join()
    .expect_err("test thread must poison Running");
    assert!(running.0.is_poisoned());

    let team_running = member_runner::TeamRunning::default();
    let mut guard = ReservationGuard::new(running.clone(), "s-register-poison".into());
    let order = Arc::new(Mutex::new(Vec::new()));
    let kill_order = order.clone();
    let wait_order = order.clone();
    let result = abort_spawn_after_register_failure(
        &running,
        &team_running,
        None,
        "s-register-poison",
        4646,
        &mut guard,
        |pid| {
            assert_eq!(pid, 4646);
            kill_order.lock().unwrap().push("kill");
        },
        || wait_order.lock().unwrap().push("wait"),
    );

    assert!(
        result.is_err(),
        "the poisoned lock error must reach the caller"
    );
    assert_eq!(
        order.lock().unwrap().as_slice(),
        ["kill", "wait"],
        "poisoned cleanup must still kill and wait without panicking"
    );
}

#[test]
fn spawn_abort_production_cleanup_uses_bounded_helper_in_both_paths() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let solo = source
        .split("fn spawn_and_stream(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]").next())
        .expect("spawn_and_stream source slice");
    let abort_cleanup = solo
        .split("if handoff == SpawnHandoffAction::Abort {")
        .nth(1)
        .and_then(|tail| tail.split("\n    if let Err(error)").next())
        .expect("spawn abort cleanup source slice");
    assert!(
        abort_cleanup.contains(concat!(
            "wait_for_aborted_child(guard, || {\n",
            "            wait_for_child_cleanup_bounded(&mut child, pid);\n",
            "        });"
        )),
        "spawn abort cleanup must call the shared bounded helper"
    );
    assert!(
        !abort_cleanup.contains("let _ = child.wait();"),
        "spawn abort cleanup must not restore an unbounded wait"
    );

    let register_failure_cleanup = solo
        .split("if let Err(error) = event_transport().register_run")
        .nth(1)
        .and_then(|tail| tail.split("\n    guard.disarm();").next())
        .expect("register-failure cleanup source slice");
    assert!(
        register_failure_cleanup.contains(concat!(
            "|| {\n",
            "                wait_for_child_cleanup_bounded(&mut child, pid);\n",
            "            },"
        )),
        "register-failure cleanup must call the shared bounded helper"
    );
    assert!(
        !register_failure_cleanup.contains("let _ = child.wait();"),
        "register-failure cleanup must not restore an unbounded wait"
    );
}
