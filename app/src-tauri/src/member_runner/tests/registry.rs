#![cfg(test)]

use super::*;

#[test]
fn globalstop_running_member_keys_only_returns_unreaped_members_for_session() {
    let tr = TeamRunning::default();
    let target_a = MemberKey::new("s-globalstop-target", "run-a", "assignment-a");
    let target_b = MemberKey::new("s-globalstop-target", "run-b", "assignment-b");
    let reaped = MemberKey::new("s-globalstop-target", "run-old", "assignment-old");
    let other_session = MemberKey::new("s-globalstop-other", "run-c", "assignment-c");

    tr.register(&target_a, 101);
    tr.register(&target_b, 102);
    tr.register(&reaped, 103);
    tr.register(&other_session, 104);
    assert!(!tr.finish_member(&reaped));

    let mut keys = tr.running_member_keys_for_session("s-globalstop-target");
    keys.sort_by(|left, right| {
        (&left.run_id, &left.assignment_id).cmp(&(&right.run_id, &right.assignment_id))
    });
    assert_eq!(keys, vec![target_a, target_b]);
}

#[test]
fn globalstop_new_member_self_stops_until_session_is_cleared() {
    let tr = TeamRunning::default();
    let stopped_key = MemberKey::new("s-globalstop-birth", "run-stopped", "assignment-a");
    tr.mark_session_stopped("s-globalstop-birth");
    tr.register(&stopped_key, 201);

    let killed = std::cell::Cell::new(0);
    assert!(request_stop_new_member_if_session_stopped(
        &tr,
        &stopped_key,
        |pid| killed.set(pid)
    ));
    assert_eq!(killed.get(), 201);
    assert!(tr.finish_member(&stopped_key));

    crate::clear_session_stop_state(&tr, "s-globalstop-birth");
    let revived_key = MemberKey::new("s-globalstop-birth", "run-revived", "assignment-b");
    tr.register(&revived_key, 202);
    assert!(!request_stop_new_member_if_session_stopped(
        &tr,
        &revived_key,
        |_| killed.set(999)
    ));
    assert_eq!(killed.get(), 201);
    assert!(!tr.finish_member(&revived_key));
}

#[test]
fn globalstop_poisoned_registry_preserves_stop_gate_and_member_enumeration() {
    let tr = TeamRunning::default();
    let session_id = "s-globalstop-poisoned-registry";
    let key = MemberKey::new(session_id, "run-poisoned", "assignment-a");
    tr.mark_session_stopped(session_id);
    tr.register(&key, 301);

    let registry = tr.0.clone();
    let _ = std::thread::spawn(move || {
        let _guard = registry.lock().unwrap();
        panic!("poison TeamRunning registry for globalstop contract");
    })
    .join();

    assert!(
        tr.is_session_stopped(session_id),
        "毒锁下停止门仍必须按 stopped_sessions 的记录值关门"
    );
    assert_eq!(
        tr.running_member_keys_for_session(session_id),
        vec![key.clone()],
        "毒锁下全局停止仍必须枚举并杀到已注册成员"
    );
    let killed = std::cell::Cell::new(0);
    assert!(tr.request_stop_member(&key, |pid| killed.set(pid)));
    assert_eq!(killed.get(), 301, "毒锁下必须照常 kill 已枚举成员");

    tr.clear_session_stopped(session_id);
    assert!(!tr.is_session_stopped(session_id));
    tr.mark_session_stopped("s-globalstop-poisoned-mark");
    assert!(tr.is_session_stopped("s-globalstop-poisoned-mark"));
}

#[test]
fn dispatch_intent_wraps_success_and_failure_without_leaking() {
    let tr = TeamRunning::default();
    let registered = MemberKey::new("s-intent", "r1", "a1");
    tr.with_dispatch_intent("s-intent", || {
        assert!(tr.is_session_running("s-intent").unwrap());
        tr.register(&registered, 42);
        assert!(!tr.finish_member(&registered));
        Ok(())
    })
    .unwrap();
    assert!(!tr.is_session_running("s-intent").unwrap());

    let error = tr
        .with_dispatch_intent("s-intent", || {
            assert!(tr.is_session_running("s-intent").unwrap());
            Err::<(), _>("preflight failed".to_string())
        })
        .unwrap_err();
    assert_eq!(error, "preflight failed");
    assert!(!tr.is_session_running("s-intent").unwrap());
}

#[test]
fn session_idle_reservation_executes_under_team_registry_lock() {
    let tr = TeamRunning::default();
    let called = std::cell::Cell::new(false);
    assert!(tr
        .reserve_if_session_idle("s-atomic", || {
            assert!(tr.0.try_lock().is_err());
            called.set(true);
            Ok(())
        })
        .unwrap());
    assert!(called.get());

    let _intent = tr.begin_dispatch_intent("s-atomic").unwrap();
    assert!(!tr
        .reserve_if_session_idle("s-atomic", || panic!("busy session must not reserve"))
        .unwrap());
}

#[test]
fn dispatch_intent_guard_drop_cleans_unwind_path() {
    let tr = TeamRunning::default();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tr.with_dispatch_intent("s-panic", || -> Result<(), String> {
            assert!(tr.is_session_running("s-panic").unwrap());
            panic!("handler panic");
        })
    }));
    assert!(unwind.is_err());
    assert!(!tr.is_session_running("s-panic").unwrap());
}

#[test]
fn dispatch_intent_guard_drop_clears_recovered_mutex_poison() {
    let tr = TeamRunning::default();
    let intent = tr.begin_dispatch_intent("s-poison").unwrap();
    let registry = tr.0.clone();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _registry = registry.lock().unwrap();
        panic!("poison team registry");
    }));
    assert!(unwind.is_err());
    assert!(tr.0.is_poisoned());

    drop(intent);

    assert!(!tr.0.is_poisoned());
    assert!(!tr.is_session_running("s-poison").unwrap());
}

#[test]
fn request_stop_member_kills_under_lock_and_finish_reports_stopped() {
    let tr = TeamRunning::default();
    tr.register(&key(), 4321);
    let killed = std::cell::Cell::new(None);
    assert!(tr.request_stop_member(&key(), |pid| {
        assert_eq!(pid, 4321);
        assert!(
            tr.0.try_lock().is_err(),
            "member kill must run under registry lock"
        );
        killed.set(Some(pid));
    }));
    assert_eq!(killed.get(), Some(4321));
    // child.wait() 后 finish_member：返回 true（被请求停过）+ 摘除 slot
    assert!(tr.finish_member(&key()));
    // 摘除后再 stop → 不杀（pid 不再可被误杀·codex P1-5）
    assert!(!tr.request_stop_member(&key(), |_| panic!("missing member must not kill")));
}

#[test]
fn finalizing_member_stop_sets_flag_but_returns_no_pid() {
    let tr = TeamRunning::default();
    let k = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&k, 4321);
    tr.begin_finalize_member(&k);
    // finalizing 态：stop 不杀（防 reap 后 pid 复用误杀）·但置标志
    assert!(!tr.request_stop_member(&k, |_| panic!("finalizing member must not kill")));
    // 标志已置 → 终态算 Stopped
    assert!(tr.finish_member(&k));
}

#[test]
fn request_stop_unknown_member_does_not_kill() {
    let tr = TeamRunning::default();
    assert!(!tr.request_stop_member(&key(), |_| panic!("unknown member must not kill")));
}

#[test]
fn member_watchdog_timeout_claim_kills_and_reports_under_lock() {
    let tr = TeamRunning::default();
    tr.register(&key(), 4321);
    let actions = std::cell::RefCell::new(Vec::new());
    assert!(tr
        .claim_first_event_watchdog_timeout(
            &key(),
            4321,
            |pid| {
                assert_eq!(pid, 4321);
                assert!(
                    tr.0.try_lock().is_err(),
                    "watchdog kill must hold registry lock"
                );
                actions.borrow_mut().push("kill");
            },
            || {
                assert!(
                    tr.0.try_lock().is_err(),
                    "watchdog report must hold registry lock"
                );
                actions.borrow_mut().push("report");
            },
        )
        .unwrap());
    assert_eq!(*actions.borrow(), ["kill", "report"]);
    assert!(tr.0.lock().unwrap().members.get(&key()).unwrap().finalizing);
}

#[test]
fn member_watchdog_timeout_rejects_wrong_pid_missing_and_stopped_slots() {
    let tr = TeamRunning::default();
    tr.register(&key(), 4321);
    let killed = std::cell::Cell::new(false);
    let reported = std::cell::Cell::new(false);
    assert!(!tr
        .claim_first_event_watchdog_timeout(
            &key(),
            9999,
            |_| killed.set(true),
            || reported.set(true),
        )
        .unwrap());
    let missing = MemberKey::new("s1", "run1", "missing");
    assert!(!tr
        .claim_first_event_watchdog_timeout(
            &missing,
            4321,
            |_| killed.set(true),
            || reported.set(true),
        )
        .unwrap());
    assert!(tr.request_stop_member(&key(), |_| {}));
    assert!(!tr
        .claim_first_event_watchdog_timeout(
            &key(),
            4321,
            |_| killed.set(true),
            || reported.set(true),
        )
        .unwrap());
    assert!(!killed.get());
    assert!(!reported.get());
    assert!(!crate::should_inject_first_event_watchdog_error(
        true,
        false,
        Some("timeout")
    ));
}

#[test]
fn finish_member_reports_false_when_not_stopped() {
    let tr = TeamRunning::default();
    tr.register(&key(), 1);
    assert!(!tr.finish_member(&key())); // 没请求停 → false → 终态走 Done/Failed（非 Stopped）
}

#[test]
fn run_remaining_counter_marks_done_only_when_all_finished() {
    let tr = TeamRunning::default();
    tr.init_run("r1", 2);

    assert!(!tr.run_member_finished("r1")); // 2 -> 1，尚未全员终态
    assert!(tr.run_member_finished("r1")); // 1 -> 0，唯一一次 true
    assert!(!tr.run_member_finished("r1")); // 条目已移除，防重复 mark

    tr.init_run("r2", 1);
    assert!(tr.run_member_finished("r2"));
    assert!(!tr.run_member_finished("r1"));
}

#[test]
fn finish_member_and_run_done_tracks_last_member() {
    let tr = TeamRunning::default();
    let ka = MemberKey::new("s1", "r1", "a");
    let kb = MemberKey::new("s1", "r1", "b");
    let kc = MemberKey::new("s1", "r2", "c");
    tr.init_run("r1", 2);
    tr.init_run("r2", 1);
    tr.register(&ka, 1234);
    tr.register(&kb, 1235);
    tr.register(&kc, 1236);

    assert!(tr.request_stop_member(&ka, |_| {}));
    assert_eq!(tr.finish_member_and_run_done(&ka), (true, false));
    assert_eq!(tr.finish_member_and_run_done(&kb), (false, true));
    assert_eq!(tr.finish_member_and_run_done(&kc), (false, true));
}

#[test]
fn member_key_distinguishes_runs() {
    let tr = TeamRunning::default();
    tr.register(&MemberKey::new("s1", "runA", "a1"), 10);
    tr.register(&MemberKey::new("s1", "runB", "a1"), 20); // 同 assignment 字串·不同 run
    let killed = std::cell::RefCell::new(Vec::new());
    assert!(
        tr.request_stop_member(&MemberKey::new("s1", "runA", "a1"), |pid| {
            killed.borrow_mut().push(pid)
        })
    );
    assert!(
        tr.request_stop_member(&MemberKey::new("s1", "runB", "a1"), |pid| {
            killed.borrow_mut().push(pid)
        })
    );
    assert_eq!(*killed.borrow(), [10, 20]);
}
