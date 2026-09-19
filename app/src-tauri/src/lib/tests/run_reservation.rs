#![cfg(test)]

use super::*;

#[test]
fn run_slot_expresses_launching_and_running_pid() {
    let mut slot = RunSlot::Launching {
        stop_requested: false,
    };
    if let RunSlot::Launching { stop_requested } = &mut slot {
        *stop_requested = true;
    } else {
        panic!("slot 应处于 Launching");
    }
    assert!(matches!(
        slot,
        RunSlot::Launching {
            stop_requested: true
        }
    ));

    let slot = RunSlot::Running(123);
    match slot {
        RunSlot::Running(pid) => assert_eq!(pid, 123),
        RunSlot::Launching { .. }
        | RunSlot::Finalizing { .. }
        | RunSlot::Mutating { .. }
        | RunSlot::TeamRun => {
            panic!("slot 应处于 Running")
        }
    }
}

#[test]
fn try_reserve_rejects_same_session_twice() {
    let running = Running::default();
    try_reserve(&running, "s-reserve").unwrap();
    let err = try_reserve(&running, "s-reserve").unwrap_err();
    assert_eq!(err, "SESSION_ALREADY_RUNNING:s-reserve");
}

#[test]
fn new_run_reservation_rejects_active_team_members_and_recovers_after_finish() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let member_key = member_runner::MemberKey::new("s-team-active", "r1", "a1");
    team_running.register(&member_key, 43);

    let err = reserve_new_session_run(&conn, &running, &team_running, "s-team-active", Locale::Zh)
        .unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:run.teamMembersActive:{"detail":"队员仍在执行上一轮派单"}"#
    );
    assert!(
        !running.0.lock().unwrap().contains_key("s-team-active"),
        "member 活跃时不得占用并启动新 run"
    );

    team_running.finish_member(&member_key);
    reserve_new_session_run(&conn, &running, &team_running, "s-team-active", Locale::Zh).unwrap();
    assert!(running.0.lock().unwrap().contains_key("s-team-active"));
}

/// M1-T1（remote control M0 §4c）：占槽咽喉——`reserve_new_session_run` 是 solo/lead 共用的
/// send_message 唯一占槽成功出口，成功后必须落一条 session_runtime running 行（run_id 此刻
/// 还没现场生成，写 None——调用方稍后自己回填）。
#[test]
fn reserve_new_session_run_writes_session_runtime_running() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();

    assert!(
        db::get_session_runtime(&conn, "s-runtime-reserve")
            .unwrap()
            .is_none(),
        "reserve 之前不该有运行态行"
    );

    reserve_new_session_run(
        &conn,
        &running,
        &team_running,
        "s-runtime-reserve",
        Locale::Zh,
    )
    .unwrap();

    let row = db::get_session_runtime(&conn, "s-runtime-reserve")
        .unwrap()
        .expect("reserve 成功后必须落一条 session_runtime 行");
    assert_eq!(row.status, "running");
}

/// idlefix-T1 缺口①（round-trip）：`reserve_new_session_run` 占槽写 run_id=None 之后，
/// `start_lead_session` 现在会在拿到真实 run_id 后用同一个 `db::set_session_runtime` 回填
/// （lead 分支，仿 solo lib.rs:11058 的写法）——这里直接验证这条 UPSERT 序列本身：
/// None → Some(run_id) 生效，不会被 `run_id = excluded.run_id` 的 UPSERT 语义卡在 NULL。
#[test]
fn session_runtime_run_id_backfill_after_reserve_overwrites_null() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();

    reserve_new_session_run(
        &conn,
        &running,
        &team_running,
        "s-runtime-lead-backfill",
        Locale::Zh,
    )
    .unwrap();
    assert_eq!(
        db::get_session_runtime(&conn, "s-runtime-lead-backfill")
            .unwrap()
            .unwrap()
            .run_id,
        None,
        "reserve 当时 run_id 还没现场生成，应先落 None"
    );

    db::set_session_runtime(
        &conn,
        "s-runtime-lead-backfill",
        db::SESSION_RUNTIME_RUNNING,
        Some("run-lead-42"),
    )
    .unwrap();

    let row = db::get_session_runtime(&conn, "s-runtime-lead-backfill")
        .unwrap()
        .expect("行必须存在");
    assert_eq!(
            row.run_id.as_deref(),
            Some("run-lead-42"),
            "lead 起跑回填后 run_id 不该再是 NULL——否则手机端 runId===null 守卫会丢光这条会话的 live delta"
        );
}

/// 占槽失败（team 仍活跃）不得写 running——否则远端会看到一个从没真正跑起来的会话。
#[test]
fn reserve_new_session_run_does_not_write_session_runtime_on_rejection() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let member_key = member_runner::MemberKey::new("s-runtime-rejected", "r1", "a1");
    team_running.register(&member_key, 43);

    reserve_new_session_run(
        &conn,
        &running,
        &team_running,
        "s-runtime-rejected",
        Locale::Zh,
    )
    .unwrap_err();

    assert!(
        db::get_session_runtime(&conn, "s-runtime-rejected")
            .unwrap()
            .is_none(),
        "被拒绝的占槽不该留下 session_runtime 行"
    );
}

// M1 修复轮 P1-1（opus 深审·2026-08-11）：`compute_session_runtime` 纯函数单测——
// 验收要求的四条口径：solo 槽在→running；全空→idle；槽已释放但 dispatch intent 仍在
// →running（P1-1 窗口，最关键的一条）；team member 在→running。

#[test]
fn compute_session_runtime_running_when_solo_slot_present() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    try_reserve(&running, "s-compute-solo").unwrap();

    assert_eq!(
        compute_session_runtime(&running, &team_running, "s-compute-solo"),
        db::SESSION_RUNTIME_RUNNING
    );
}

#[test]
fn compute_session_runtime_idle_when_both_empty() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();

    assert_eq!(
        compute_session_runtime(&running, &team_running, "s-compute-empty"),
        db::SESSION_RUNTIME_IDLE
    );
}

/// P1-1 窗口·最关键的一条：Running 槽已经释放（lead 收尾），但队员派单 intent 仍在途——
/// 必须仍判 running，不能因为 Running 槽空了就误判 idle（旧写口在此刻会错写 idle）。
#[test]
fn compute_session_runtime_running_when_slot_released_but_dispatch_intent_still_active() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-compute-intent-window";
    try_reserve(&running, session_id).unwrap();
    let intent = team_running.begin_dispatch_intent(session_id).unwrap();

    // 模拟 lead 收尾：Running 槽已摘掉，但 intent guard 还没 drop（队员仍在跑）。
    running.0.lock().unwrap().remove(session_id);

    assert_eq!(
        compute_session_runtime(&running, &team_running, session_id),
        db::SESSION_RUNTIME_RUNNING,
        "槽已释放但 dispatch intent 仍在途时必须仍判 running"
    );

    drop(intent);
    assert_eq!(
        compute_session_runtime(&running, &team_running, session_id),
        db::SESSION_RUNTIME_IDLE,
        "intent 也释放后才真正转 idle"
    );
}

#[test]
fn compute_session_runtime_running_when_team_member_registered() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-compute-team-member";
    let member_key = member_runner::MemberKey::new(session_id, "run-a", "assignment-a");
    team_running.register(&member_key, 4242);

    assert_eq!(
        compute_session_runtime(&running, &team_running, session_id),
        db::SESSION_RUNTIME_RUNNING,
        "Running 槽为空但有活跃队员时必须判 running"
    );
}

#[test]
fn new_run_reservation_rejects_dispatch_intent_and_recovers_after_drop() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();

    let intent = team_running
        .begin_dispatch_intent("s-dispatch-intent")
        .unwrap();
    let error = reserve_new_session_run(
        &conn,
        &running,
        &team_running,
        "s-dispatch-intent",
        Locale::En,
    )
    .unwrap_err();
    assert!(error.starts_with("AL_ERR:run.teamMembersActive:"));
    assert!(!running.0.lock().unwrap().contains_key("s-dispatch-intent"));

    drop(intent);
    reserve_new_session_run(
        &conn,
        &running,
        &team_running,
        "s-dispatch-intent",
        Locale::En,
    )
    .unwrap();
    assert!(running.0.lock().unwrap().contains_key("s-dispatch-intent"));
}

#[test]
fn terminated_lead_worker_rejects_before_work_and_releases_dispatch_intent() {
    let team_running = member_runner::TeamRunning::default();
    let running = Running::default();
    let terminated = AtomicBool::new(true);
    let work_called = std::cell::Cell::new(false);

    let error = run_lead_worker_with_dispatch_intent(
        &team_running,
        &running,
        None,
        "s-terminated-lead",
        &terminated,
        || {
            work_called.set(true);
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error, "lead 已终结·派单中止");
    assert!(
        !work_called.get(),
        "terminated lead must not spawn or do work"
    );
    assert!(
        !team_running
            .is_session_running("s-terminated-lead")
            .unwrap(),
        "error return must drop the dispatch intent guard"
    );
}

#[test]
fn lead_common_finalizing_point_sets_terminated_before_slot_transition() {
    let running = Running::default();
    running
        .0
        .lock()
        .unwrap()
        .insert("s-lead-finalize".into(), RunSlot::Running(1234));
    let terminated = AtomicBool::new(false);

    assert!(!begin_lead_finalizing(
        &running,
        &terminated,
        "s-lead-finalize"
    ));

    assert!(terminated.load(Ordering::SeqCst));
    assert!(matches!(
        running.0.lock().unwrap().get("s-lead-finalize"),
        Some(RunSlot::Finalizing {
            stop_requested: false
        })
    ));
}
