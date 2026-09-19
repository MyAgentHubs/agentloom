#![cfg(test)]

use super::*;

#[test]
fn first_event_watchdog_timeout_decision_covers_timeout_first_line_and_stop() {
    assert_eq!(FIRST_EVENT_TIMEOUT_SECS, 60);
    assert!(first_event_watchdog_should_trigger(
        FirstEventWatchdogState::Armed
    ));
    assert!(!first_event_watchdog_should_trigger(
        FirstEventWatchdogState::FirstLineSeen
    ));
    assert!(!first_event_watchdog_should_trigger(
        FirstEventWatchdogState::StopRequested
    ));
}

fn first_event_watchdog_test_signal() -> FirstEventWatchdogSignal {
    FirstEventWatchdogSignal {
        state: Arc::new((Mutex::new(FirstEventWatchdogState::Armed), Condvar::new())),
        timeout_stderr: Arc::new(Mutex::new(None)),
    }
}

#[test]
fn first_event_watchdog_stdout_eof_before_first_line_cancels_watchdog() {
    let signal = first_event_watchdog_test_signal();

    assert!(!signal.stdout_closed());
    let state = *signal.state.0.lock().unwrap();
    assert_eq!(state, FirstEventWatchdogState::StdoutClosed);
    assert!(!first_event_watchdog_should_trigger(state));
}

#[test]
fn first_event_watchdog_stdout_eof_after_first_line_remains_cancelled() {
    let signal = first_event_watchdog_test_signal();

    signal.first_line_seen();
    assert!(signal.stdout_closed());
    let state = *signal.state.0.lock().unwrap();
    assert_eq!(state, FirstEventWatchdogState::StdoutClosed);
    assert!(!first_event_watchdog_should_trigger(state));
}

#[test]
fn first_event_watchdog_message_is_bilingual_and_has_diagnostics() {
    let zh = first_event_watchdog_error_message(
        Locale::Zh,
        "run.spawnFailed",
        "claude",
        "/opt/agentloom/bin/claude",
        "warning one\nwarning two",
    );
    let en = first_event_watchdog_error_message(
        Locale::En,
        "run.spawnFailed",
        "codex",
        "codex",
        "warning three",
    );

    for (message, expected) in [
        (
            zh.as_str(),
            [
                "claude",
                "/opt/agentloom/bin/claude",
                "warning one",
                "warning two",
            ],
        ),
        (
            en.as_str(),
            ["codex", "codex", "warning three", "warning three"],
        ),
    ] {
        assert!(message.starts_with("AL_ERR:run.spawnFailed:"), "{message}");
        for fragment in expected {
            assert!(
                message.contains(fragment),
                "missing {fragment:?} in {message}"
            );
        }
    }
    assert!(zh.contains("60 秒"), "{zh}");
    assert!(en.contains("60 seconds"), "{en}");
}

#[test]
fn first_event_watchdog_stderr_summary_keeps_at_most_three_lines() {
    let tail = Arc::new(Mutex::new(b"one\ntwo\nthree\nfour\n".to_vec()));

    assert_eq!(stderr_tail_last_lines(&tail), "two\nthree\nfour");
}

#[test]
fn first_event_watchdog_timeout_claim_kills_and_reports_at_function_boundary() {
    let running = Running::default();
    running
        .0
        .lock()
        .unwrap()
        .insert("watchdog-run".into(), RunSlot::Running(4242));
    let actions = std::cell::RefCell::new(Vec::new());

    assert!(claim_first_event_watchdog_timeout(
        &running,
        "watchdog-run",
        4242,
        |pid| {
            assert_eq!(pid, 4242);
            assert!(
                running.0.try_lock().is_err(),
                "kill must run under slot lock"
            );
            actions.borrow_mut().push("kill");
        },
        || {
            assert!(
                running.0.try_lock().is_err(),
                "timeout report must run under slot lock"
            );
            actions.borrow_mut().push("report");
        },
    )
    .unwrap());
    assert_eq!(*actions.borrow(), ["kill", "report"]);
    assert!(matches!(
        running.0.lock().unwrap().get("watchdog-run"),
        Some(RunSlot::Finalizing {
            stop_requested: false
        })
    ));
}

#[test]
fn first_event_watchdog_timeout_does_not_fire_after_stop_request() {
    let running = Running::default();
    running.0.lock().unwrap().insert(
        "watchdog-stopped".into(),
        RunSlot::Finalizing {
            stop_requested: true,
        },
    );
    let killed = std::cell::Cell::new(false);
    let reported = std::cell::Cell::new(false);

    assert!(!claim_first_event_watchdog_timeout(
        &running,
        "watchdog-stopped",
        4343,
        |_| killed.set(true),
        || reported.set(true),
    )
    .unwrap());
    assert!(!killed.get());
    assert!(!reported.get());
}

#[test]
fn first_event_watchdog_timeout_rejects_wrong_pid_finalizing_and_missing_slots() {
    let running = Running::default();
    running
        .0
        .lock()
        .unwrap()
        .insert("watchdog-replaced".into(), RunSlot::Running(4545));
    let killed = std::cell::Cell::new(false);
    let reported = std::cell::Cell::new(false);

    assert!(!claim_first_event_watchdog_timeout(
        &running,
        "watchdog-replaced",
        4646,
        |_| killed.set(true),
        || reported.set(true),
    )
    .unwrap());
    assert!(!killed.get());
    assert!(!reported.get());
    assert!(matches!(
        running.0.lock().unwrap().get("watchdog-replaced"),
        Some(RunSlot::Running(4545))
    ));
    running.0.lock().unwrap().insert(
        "watchdog-finalizing".into(),
        RunSlot::Finalizing {
            stop_requested: false,
        },
    );
    assert!(!claim_first_event_watchdog_timeout(
        &running,
        "watchdog-finalizing",
        4747,
        |_| killed.set(true),
        || reported.set(true),
    )
    .unwrap());
    assert!(!claim_first_event_watchdog_timeout(
        &running,
        "watchdog-missing",
        4848,
        |_| killed.set(true),
        || reported.set(true),
    )
    .unwrap());
    assert!(!killed.get());
    assert!(!reported.get());
    assert!(matches!(
        running.0.lock().unwrap().get("watchdog-finalizing"),
        Some(RunSlot::Finalizing {
            stop_requested: false
        })
    ));
    assert!(!running.0.lock().unwrap().contains_key("watchdog-missing"));
}

#[test]
fn first_event_watchdog_failed_claim_never_retries_after_same_pid_aba() {
    let running = Running::default();
    running.0.lock().unwrap().insert(
        "watchdog-aba".into(),
        RunSlot::Finalizing {
            stop_requested: false,
        },
    );
    let killed = std::cell::Cell::new(0);

    assert!(!claim_first_event_watchdog_timeout(
        &running,
        "watchdog-aba",
        4949,
        |_| killed.set(killed.get() + 1),
        || {},
    )
    .unwrap());
    running
        .0
        .lock()
        .unwrap()
        .insert("watchdog-aba".into(), RunSlot::Running(4949));

    assert_eq!(
        killed.get(),
        0,
        "a failed claim must not install a later kill"
    );
}

struct FakeFirstEventChild {
    polls: std::collections::VecDeque<Result<Option<i32>, &'static str>>,
    waits: usize,
}

#[test]
fn first_event_owner_wait_exits_before_deadline_without_kill() {
    let started = Instant::now();
    let clock = std::cell::Cell::new(started);
    let killed = std::cell::Cell::new(0);
    let mut child = FakeFirstEventChild {
        polls: [Ok(None), Ok(Some(7))].into(),
        waits: 0,
    };

    let result = wait_for_first_event_owner(
        &mut child,
        5050,
        started + std::time::Duration::from_secs(1),
        |child| child.polls.pop_front().unwrap(),
        |child| {
            child.waits += 1;
            Ok(9)
        },
        |_| killed.set(killed.get() + 1),
        || clock.get(),
        |duration| clock.set(clock.get() + duration),
    );

    assert_eq!(result, FirstEventOwnerWait::Exited(7));
    assert_eq!(killed.get(), 0);
    assert_eq!(child.waits, 0);
}

#[test]
fn first_event_owner_wait_kills_once_at_deadline_then_reaps() {
    let started = Instant::now();
    let clock = std::cell::Cell::new(started);
    let killed = std::cell::Cell::new(0);
    let mut child = FakeFirstEventChild {
        polls: [Ok(None), Ok(None)].into(),
        waits: 0,
    };

    let result = wait_for_first_event_owner(
        &mut child,
        5151,
        started + FIRST_EVENT_WAIT_POLL_INTERVAL,
        |child| child.polls.pop_front().unwrap(),
        |child| {
            child.waits += 1;
            Ok(11)
        },
        |pid| {
            assert_eq!(pid, 5151);
            killed.set(killed.get() + 1);
        },
        || clock.get(),
        |duration| clock.set(clock.get() + duration),
    );

    assert_eq!(result, FirstEventOwnerWait::TimedOut(Some(11)));
    assert_eq!(killed.get(), 1);
    assert_eq!(child.waits, 1);
}

#[test]
fn first_event_owner_wait_error_does_not_kill_or_reap() {
    let started = Instant::now();
    let killed = std::cell::Cell::new(0);
    let mut child = FakeFirstEventChild {
        polls: [Err("try_wait failed")].into(),
        waits: 0,
    };

    let result = wait_for_first_event_owner(
        &mut child,
        5252,
        started,
        |child| child.polls.pop_front().unwrap(),
        |child| {
            child.waits += 1;
            Ok(13)
        },
        |_| killed.set(killed.get() + 1),
        || started,
        |_| panic!("try_wait errors must not sleep"),
    );

    assert_eq!(result, FirstEventOwnerWait::WaitError);
    assert_eq!(killed.get(), 0);
    assert_eq!(child.waits, 0);
}
