#![cfg(test)]

use super::*;

struct FakeFinalizerOwner {
    polls: std::collections::VecDeque<Result<bool, &'static str>>,
    waits: usize,
}

#[test]
fn finalizer_owner_wait_finishes_before_deadline_without_kill() {
    let started = Instant::now();
    let clock = std::cell::Cell::new(started);
    let killed = std::cell::Cell::new(0);
    let mut owner = FakeFinalizerOwner {
        polls: [Ok(false), Ok(true)].into(),
        waits: 0,
    };

    let result = finalizer_owner_wait(
        &mut owner,
        started + std::time::Duration::from_secs(1),
        |owner| owner.polls.pop_front().unwrap(),
        |owner| {
            owner.waits += 1;
            Ok::<_, &'static str>(17)
        },
        || killed.set(killed.get() + 1),
        || clock.get(),
        |duration| clock.set(clock.get() + duration),
        |outcome| outcome,
    );

    assert_eq!(result, FinalizerOwnerWait::Finished(17));
    assert_eq!(killed.get(), 0);
    assert_eq!(owner.waits, 1);
}

#[test]
fn finalizer_owner_wait_timeout_kills_once_and_continues_closeout() {
    let started = Instant::now();
    let clock = std::cell::Cell::new(started);
    let killed = std::cell::Cell::new(0);
    let persisted = std::cell::Cell::new(0);
    let terminal_emitted = std::cell::Cell::new(0);
    let mut owner = FakeFinalizerOwner {
        polls: [Ok(false), Ok(false)].into(),
        waits: 0,
    };

    let result = finalizer_owner_wait(
        &mut owner,
        started + FINALIZER_OWNER_WAIT_POLL_INTERVAL,
        |owner| owner.polls.pop_front().unwrap(),
        |owner| {
            owner.waits += 1;
            Ok::<_, &'static str>(19)
        },
        || killed.set(killed.get() + 1),
        || clock.get(),
        |duration| clock.set(clock.get() + duration),
        |outcome| {
            if outcome == FinalizerOwnerWait::TimedOut {
                persisted.set(persisted.get() + 1);
                terminal_emitted.set(terminal_emitted.get() + 1);
            }
            outcome
        },
    );

    assert_eq!(result, FinalizerOwnerWait::TimedOut);
    assert_eq!(killed.get(), 1);
    assert_eq!(owner.waits, 0, "timeout must not enter an unbounded wait");
    assert_eq!(persisted.get(), 1, "timeout must continue to persistence");
    assert_eq!(
        terminal_emitted.get(),
        1,
        "timeout must continue to the terminal release"
    );
}

#[test]
fn finalizer_owner_wait_poll_error_does_not_kill_or_wait() {
    let started = Instant::now();
    let killed = std::cell::Cell::new(0);
    let mut owner = FakeFinalizerOwner {
        polls: [Err("poll failed")].into(),
        waits: 0,
    };

    let result = finalizer_owner_wait(
        &mut owner,
        started,
        |owner| owner.polls.pop_front().unwrap(),
        |owner| {
            owner.waits += 1;
            Ok::<_, &'static str>(23)
        },
        || killed.set(killed.get() + 1),
        || started,
        |_| panic!("poll errors must not sleep"),
        |outcome| outcome,
    );

    assert_eq!(result, FinalizerOwnerWait::WaitError);
    assert_eq!(killed.get(), 0);
    assert_eq!(owner.waits, 0);
}

#[test]
fn finalizer_owner_wait_timeout_keeps_completed_run_successful() {
    assert!(finalizer_exit_success_after_owner_wait(
        None::<&i32>,
        true,
        true,
        |_| false,
    ));
    assert!(!finalizer_exit_success_after_owner_wait(
        None::<&i32>,
        true,
        false,
        |_| true,
    ));
}

#[test]
fn finalizer_owner_wait_timeout_persists_then_emits_through_closeout_seam() {
    use crate::test_support::mem_db;

    let c = mem_db();
    db::create_session(
        &c,
        "s-owner-wait-timeout",
        "owner wait timeout",
        "local-default",
        "local",
    )
    .unwrap();
    let reduced = display_reduce::ReducedMessage {
        dedup_key: "run-owner-wait-timeout".into(),
        blocks: vec![Block::Text {
            text: "durable assistant reply".into(),
        }],
    };

    let root = tempfile::tempdir().unwrap();
    let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    let emitted = Arc::new(AtomicBool::new(false));
    let emitted_t = emitted.clone();
    transport.install_emitter_for_test(move |_| {
        emitted_t.store(true, Ordering::SeqCst);
    });
    transport
        .register_run(
            "run-owner-wait-timeout",
            "s-owner-wait-timeout",
            None,
            member_runner::TextGranularity::Line,
        )
        .unwrap();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    running.0.lock().unwrap().insert(
        "s-owner-wait-timeout".into(),
        RunSlot::Finalizing {
            stop_requested: false,
        },
    );
    let terminal = agent_event::AgentEvent::RunCloseout {
        run_id: "run-owner-wait-timeout".into(),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: Some(false),
    };

    let (_, _, continuation) =
        prepare_finalizer_closeout(FinalizerOwnerWait::<i32>::TimedOut, true, None, |_| false);
    assert!(matches!(
        &continuation,
        FinalizerCloseoutContinuation::CleanupTimedOut { .. }
    ));
    continuation.persist_then_emit(
        || {
            persist_normal_finalizer(
                &c,
                "s-owner-wait-timeout",
                "claude",
                Some("Claude"),
                Some(&reduced),
                None,
            );
        },
        || {
            assert_eq!(
                db::get_messages(&c, "s-owner-wait-timeout").unwrap().len(),
                1,
                "the terminal barrier must run after durable persistence"
            );
            assert!(emit_terminal_after_releasing_run_slot(
                &running,
                &team_running,
                "s-owner-wait-timeout",
                "run-owner-wait-timeout",
                vec![terminal],
                &transport,
                None,
            ));
        },
    );

    assert_eq!(
        db::get_messages(&c, "s-owner-wait-timeout").unwrap().len(),
        1
    );
    assert!(!running
        .0
        .lock()
        .unwrap()
        .contains_key("s-owner-wait-timeout"));
    assert!(emitted.load(Ordering::SeqCst));
}

#[test]
fn finalizer_owner_wait_normal_persists_then_emits_through_closeout_seam() {
    use crate::test_support::mem_db;

    let c = mem_db();
    db::create_session(
        &c,
        "s-owner-wait-normal",
        "owner wait normal",
        "local-default",
        "local",
    )
    .unwrap();
    let reduced = display_reduce::ReducedMessage {
        dedup_key: "run-owner-wait-normal".into(),
        blocks: vec![Block::Text {
            text: "durable normal reply".into(),
        }],
    };
    let emitted = std::cell::Cell::new(false);

    let (_, _, continuation) =
        prepare_finalizer_closeout(FinalizerOwnerWait::Finished(0), true, None, |_| true);
    assert!(matches!(
        &continuation,
        FinalizerCloseoutContinuation::Normal { .. }
    ));
    continuation.persist_then_emit(
        || {
            persist_normal_finalizer(
                &c,
                "s-owner-wait-normal",
                "claude",
                Some("Claude"),
                Some(&reduced),
                None,
            );
        },
        || {
            assert_eq!(
                db::get_messages(&c, "s-owner-wait-normal").unwrap().len(),
                1,
                "normal closeout must persist before emitting its terminal event"
            );
            emitted.set(true);
        },
    );

    assert!(emitted.get());
    assert_eq!(
        db::get_messages(&c, "s-owner-wait-normal").unwrap().len(),
        1
    );
}

#[test]
fn finalizer_owner_wait_auth_retry_cleanup_is_bounded_once_for_handoff_failures() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let helper = source
        .split("fn wait_for_child_cleanup_bounded(")
        .nth(1)
        .and_then(|tail| {
            tail.split("\nfn transition_stdout_closed_to_finalizing(")
                .next()
        })
        .expect("auth retry cleanup helper source slice");
    assert!(helper.contains("finalizer_owner_wait("));
    assert!(helper.contains("Instant::now() + FINALIZER_OWNER_WAIT_TIMEOUT"));

    let solo = source
        .split("fn spawn_and_stream(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]").next())
        .expect("spawn_and_stream source slice");
    assert_eq!(
        solo.matches("wait_for_child_cleanup_bounded(&mut retry_child, retry_pid)")
            .count(),
        1,
        "failed auth retry handoffs must share one bounded cleanup call site"
    );
    assert!(!solo.contains("let _ = retry_child.wait();"));
}

#[test]
fn finalizer_owner_wait_repairs_poisoned_running_slot_before_closeout() {
    let running = Running::default();
    let running_t = running.clone();
    std::thread::spawn(move || {
        let mut slots = running_t.0.lock().unwrap();
        slots.insert(
            "s-owner-wait-poisoned".into(),
            RunSlot::Finalizing {
                stop_requested: true,
            },
        );
        panic!("poison Running after installing the recoverable slot state");
    })
    .join()
    .expect_err("test thread must poison Running");
    assert!(running.0.is_poisoned());

    assert!(transition_stdout_closed_to_finalizing(
        &running,
        "s-owner-wait-poisoned"
    ));
    assert!(running.0.is_poisoned());
    // Only the test clears poison so the pre-existing slot assertion can keep using unwrap;
    // production must preserve the global signal after repairing this one session.
    running.0.clear_poison();
    assert!(matches!(
        running.0.lock().unwrap().get("s-owner-wait-poisoned"),
        Some(RunSlot::Finalizing {
            stop_requested: true
        })
    ));

    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let solo = source
        .split("fn spawn_and_stream(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]").next())
        .expect("spawn_and_stream source slice");
    let transition = solo
        .find("transition_stdout_closed_to_finalizing(&running_t, &session_id)")
        .expect("stdout close must repair the Running slot");
    let bounded_wait = solo[transition..]
        .find("finalizer_owner_wait(")
        .expect("slot repair must continue into bounded owner wait");
    assert!(bounded_wait > 0);
}

#[test]
fn finalizer_owner_wait_auth_handoff_poison_persists_and_flushes_terminal() {
    use crate::test_support::mem_db;

    let c = mem_db();
    db::create_session(
        &c,
        "s-owner-wait-auth-poison",
        "auth poison",
        "local-default",
        "local",
    )
    .unwrap();
    let reduced = display_reduce::ReducedMessage {
        dedup_key: "run-owner-wait-auth-poison".into(),
        blocks: vec![Block::Text {
            text: "durable auth handoff failure".into(),
        }],
    };

    let root = tempfile::tempdir().unwrap();
    let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    transport
        .register_run(
            "run-owner-wait-auth-poison",
            "s-owner-wait-auth-poison",
            None,
            member_runner::TextGranularity::Line,
        )
        .unwrap();
    let emitted = Arc::new(AtomicBool::new(false));
    let emitted_t = emitted.clone();
    transport.install_emitter_for_test(move |_| {
        emitted_t.store(true, Ordering::SeqCst);
    });

    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let running_t = running.clone();
    std::thread::spawn(move || {
        let mut slots = running_t.0.lock().unwrap();
        slots.insert(
            "s-owner-wait-auth-poison".into(),
            RunSlot::Finalizing {
                stop_requested: false,
            },
        );
        panic!("poison Running before auth retry handoff");
    })
    .join()
    .expect_err("test thread must poison Running");

    assert!(
        transition_auth_retry_handoff(&running, "s-owner-wait-auth-poison", 6161).is_err(),
        "a poisoned auth retry handoff must enter the Err closeout path"
    );
    let terminal = agent_event::AgentEvent::RunCloseout {
        run_id: "run-owner-wait-auth-poison".into(),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: Some(false),
    };

    FinalizerCloseoutContinuation::Normal {
        exit_success: false,
    }
    .persist_then_emit(
        || {
            persist_normal_finalizer(
                &c,
                "s-owner-wait-auth-poison",
                "claude",
                Some("Claude"),
                Some(&reduced),
                None,
            );
        },
        || {
            assert_eq!(
                db::get_messages(&c, "s-owner-wait-auth-poison")
                    .unwrap()
                    .len(),
                1,
                "auth handoff Err must persist before terminal flush"
            );
            assert!(emit_terminal_after_releasing_run_slot(
                &running,
                &team_running,
                "s-owner-wait-auth-poison",
                "run-owner-wait-auth-poison",
                vec![terminal],
                &transport,
                None,
            ));
        },
    );

    assert_eq!(
        db::get_messages(&c, "s-owner-wait-auth-poison")
            .unwrap()
            .len(),
        1
    );
    assert!(emitted.load(Ordering::SeqCst));
    assert!(running.0.is_poisoned());
    assert!(!running
        .0
        .lock()
        .unwrap_err()
        .into_inner()
        .contains_key("s-owner-wait-auth-poison"));
}

#[test]
fn finalizer_owner_wait_completed_event_is_wired_into_closeout_success() {
    let completed = agent_event::AgentEvent::Completed {
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        final_text: Some("done".into()),
        result: None,
        run_id: Some("run-owner-wait-completed".into()),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: Some(false),
    };

    let (exit_status, _, continuation) = prepare_finalizer_closeout(
        FinalizerOwnerWait::<i32>::TimedOut,
        true,
        Some(&completed),
        |_| false,
    );

    assert!(exit_status.is_none());
    assert!(matches!(
        &continuation,
        FinalizerCloseoutContinuation::CleanupTimedOut { .. }
    ));
    assert!(
        continuation.exit_success(),
        "a production-shaped pending Completed must remain authoritative on cleanup timeout"
    );
}

#[test]
fn finalizer_owner_wait_production_passes_pending_completed_to_closeout() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let solo = source
        .split("fn spawn_and_stream(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]").next())
        .expect("spawn_and_stream source slice");
    let closeout_call = solo
        .split("prepare_finalizer_closeout(")
        .nth(1)
        .expect("production closeout preparation call");
    let closeout_call = &closeout_call[..closeout_call.len().min(400)];

    assert!(
        closeout_call.contains("pending_completed.as_ref()"),
        "production owner-wait closeout must derive success from the parsed Completed event"
    );
}

#[test]
fn finalizer_owner_wait_stderr_timeout_keeps_full_bounded_tail() {
    let tail = Arc::new(Mutex::new(b"one\ntwo\nthree\nfour\nfive\n".to_vec()));

    assert_eq!(
        finalizer_stderr_tail_after_owner_wait(FinalizerOwnerWait::TimedOut, &tail),
        "one\ntwo\nthree\nfour\nfive"
    );

    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let solo = source
        .split("fn spawn_and_stream(")
        .nth(1)
        .and_then(|rest| rest.split("\n#[tauri::command]").next())
        .expect("spawn_and_stream source slice");
    assert!(solo.contains("finalizer_stderr_tail_after_owner_wait(outcome, &stderr_live_tail)"));
}
