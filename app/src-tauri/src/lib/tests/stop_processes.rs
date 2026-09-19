#![cfg(test)]

use super::*;

#[test]
fn run_slot_finalizing_variant_holds_stop_flag() {
    let mut slot = RunSlot::Finalizing {
        stop_requested: false,
    };
    if let RunSlot::Finalizing { stop_requested } = &mut slot {
        *stop_requested = true;
    } else {
        panic!("slot 应处于 Finalizing");
    }
    assert!(matches!(
        slot,
        RunSlot::Finalizing {
            stop_requested: true
        }
    ));
}

#[test]
fn request_stop_on_finalizing_sets_flag_without_kill() {
    let running = Running::default();
    {
        let mut m = running.0.lock().unwrap();
        m.insert(
            "s-fin".to_string(),
            RunSlot::Finalizing {
                stop_requested: false,
            },
        );
    }
    let kill_count = std::cell::Cell::new(0);
    request_stop(
        &running,
        "s-fin",
        |_| kill_count.set(kill_count.get() + 1),
        |_| {},
    )
    .unwrap();
    assert_eq!(kill_count.get(), 0, "Finalizing 态 stop 不应 kill");
    assert!(matches!(
        running.0.lock().unwrap().get("s-fin"),
        Some(RunSlot::Finalizing {
            stop_requested: true
        })
    ));
}

#[test]
fn request_stop_on_running_kills_once_under_lock_and_transitions_to_finalizing() {
    let running = Running::default();
    {
        let mut m = running.0.lock().unwrap();
        m.insert("s-run".to_string(), RunSlot::Running(4321));
    }
    let kill_count = std::cell::Cell::new(0);
    request_stop(
        &running,
        "s-run",
        |pid| {
            assert_eq!(pid, 4321);
            assert!(
                running.0.try_lock().is_err(),
                "Running 态 kill 必须发生在槽锁临界区内"
            );
            kill_count.set(kill_count.get() + 1);
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(kill_count.get(), 1, "Running 态 stop 应恰好 kill 一次");
    // 不再 remove：转 Finalizing{stop_requested:true}，让 finalizer 标 interrupted +
    // slot 全程占位消 None-window（新轮在收尾完成前 reserve 不到）。
    assert!(
        matches!(
            running.0.lock().unwrap().get("s-run"),
            Some(RunSlot::Finalizing {
                stop_requested: true
            })
        ),
        "Running 态 stop 应转 Finalizing{{stop_requested:true}}（不再移除 slot）"
    );
}

#[test]
#[cfg(unix)]
fn user_stop_reports_background_process_count_after_kill_without_holding_slot_lock() {
    let db = test_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-stop-background-report";
    running
        .0
        .lock()
        .unwrap()
        .insert(session_id.into(), RunSlot::Running(4321));
    let rows = vec![
        checkpoint_hook::PsRow {
            pid: 4321,
            pgid: 4321,
            ppid: 1,
            stat: "Ss".into(),
            command: "claude".into(),
        },
        checkpoint_hook::PsRow {
            pid: 5001,
            pgid: 4321,
            ppid: 4321,
            stat: "S".into(),
            command: "codex exec worker-one".into(),
        },
        checkpoint_hook::PsRow {
            pid: 5002,
            pgid: 9000,
            ppid: 5001,
            stat: "S".into(),
            command: "python worker-two.py".into(),
        },
    ];
    let killed = std::cell::Cell::new(false);
    let reported = std::cell::RefCell::new(Vec::new());

    stop_session_with_background_inspection(
        &db,
        &running,
        &team_running,
        session_id,
        Locale::Zh,
        |pid| {
            assert_eq!(pid, 4321);
            killed.set(true);
        },
        |pid, locale| {
            assert!(
                running.0.try_lock().is_ok(),
                "ps 枚举必须发生在 running slot 锁外"
            );
            Ok(background_process_stop_notice(&rows, pid, locale))
        },
        |notice| {
            assert!(killed.get(), "必须先 kill，再上报连带停止结果");
            reported.borrow_mut().push(notice.to_string());
            Ok(())
        },
        |_| {},
    )
    .unwrap();

    let reported = reported.into_inner();
    assert_eq!(reported.len(), 1);
    let notice = &reported[0];
    assert!(notice.contains("检测到同一进程组内有 1 个"));
    assert!(notice.contains("检测到 1 个由 Agent 启动的进程不在该进程组"));
    let stopped_list = notice.find("已停止进程：").unwrap();
    let still_running_list = notice.find("仍在运行的进程：").unwrap();
    let stopped_command = notice.find("codex exec worker-one").unwrap();
    let still_running_command = notice.find("python worker-two.py").unwrap();
    assert!(stopped_list < stopped_command && stopped_command < still_running_list);
    assert!(still_running_list < still_running_command);
}

#[test]
#[cfg(unix)]
fn background_process_stop_notice_covers_all_count_class_combinations() {
    fn row(pid: u32, pgid: u32, ppid: u32, command: &str) -> checkpoint_hook::PsRow {
        checkpoint_hook::PsRow {
            pid,
            pgid,
            ppid,
            stat: "S".into(),
            command: command.into(),
        }
    }

    let agent_pid = 4321;
    let agent = || row(agent_pid, agent_pid, 1, "claude");

    let none = vec![agent()];
    assert_eq!(
        background_process_stop_notice(&none, agent_pid, Locale::Zh),
        None
    );
    assert_eq!(
        background_process_stop_notice(&none, agent_pid, Locale::En),
        None
    );

    let stopped_only = vec![
        agent(),
        row(5001, agent_pid, agent_pid, "stopped-worker-one"),
        row(5002, agent_pid, agent_pid, "stopped-worker-two"),
    ];
    assert_eq!(
            background_process_stop_notice(&stopped_only, agent_pid, Locale::Zh).as_deref(),
            Some(
                "停止会话时，检测到同一进程组内有 2 个由 Agent 启动的后台进程，已随会话一并终止。 已停止进程：stopped-worker-one; stopped-worker-two"
            )
        );
    assert_eq!(
            background_process_stop_notice(&stopped_only, agent_pid, Locale::En).as_deref(),
            Some(
                "When stopping the session, detected 2 background processes started by the agent in the same process group; they were terminated along with the session. Stopped processes: stopped-worker-one; stopped-worker-two"
            )
        );

    let still_running_only = vec![agent(), row(6001, 9000, agent_pid, "still-running-worker")];
    assert_eq!(
            background_process_stop_notice(&still_running_only, agent_pid, Locale::Zh).as_deref(),
            Some(
                "检测到 1 个由 Agent 启动的进程不在该进程组，未被终止，可能仍在运行。 仍在运行的进程：still-running-worker"
            )
        );
    assert_eq!(
            background_process_stop_notice(&still_running_only, agent_pid, Locale::En).as_deref(),
            Some(
                "Detected 1 process started by the agent outside that process group; it was not terminated and may still be running. Still-running processes: still-running-worker"
            )
        );

    let both = vec![
        agent(),
        row(5001, agent_pid, agent_pid, "stopped-worker"),
        row(6001, 9000, agent_pid, "still-running-worker-one"),
        row(6002, 9000, agent_pid, "still-running-worker-two"),
    ];
    assert_eq!(
            background_process_stop_notice(&both, agent_pid, Locale::Zh).as_deref(),
            Some(
                "停止会话时，检测到同一进程组内有 1 个由 Agent 启动的后台进程，已随会话一并终止。 已停止进程：stopped-worker 检测到 2 个由 Agent 启动的进程不在该进程组，未被终止，可能仍在运行。 仍在运行的进程：still-running-worker-one; still-running-worker-two"
            )
        );
    assert_eq!(
            background_process_stop_notice(&both, agent_pid, Locale::En).as_deref(),
            Some(
                "When stopping the session, detected 1 background process started by the agent in the same process group; it was terminated along with the session. Stopped processes: stopped-worker Detected 2 processes started by the agent outside that process group; they were not terminated and may still be running. Still-running processes: still-running-worker-one; still-running-worker-two"
            )
        );
}

#[test]
#[cfg(unix)]
fn user_stop_with_zero_background_processes_emits_no_notice() {
    let db = test_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-stop-no-background";
    running
        .0
        .lock()
        .unwrap()
        .insert(session_id.into(), RunSlot::Running(4322));
    let rows = vec![checkpoint_hook::PsRow {
        pid: 4322,
        pgid: 4322,
        ppid: 1,
        stat: "Ss".into(),
        command: "claude".into(),
    }];
    let report_count = std::cell::Cell::new(0);

    stop_session_with_background_inspection(
        &db,
        &running,
        &team_running,
        session_id,
        Locale::En,
        |_| {},
        |pid, locale| Ok(background_process_stop_notice(&rows, pid, locale)),
        |_| {
            report_count.set(report_count.get() + 1);
            Ok(())
        },
        |_| {},
    )
    .unwrap();

    assert_eq!(report_count.get(), 0);
}

#[test]
fn user_stop_still_kills_when_background_enumeration_fails() {
    let db = test_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-stop-ps-failed";
    running
        .0
        .lock()
        .unwrap()
        .insert(session_id.into(), RunSlot::Running(4323));
    let killed = std::cell::Cell::new(false);
    let report_count = std::cell::Cell::new(0);

    stop_session_with_background_inspection(
        &db,
        &running,
        &team_running,
        session_id,
        Locale::Zh,
        |pid| {
            assert_eq!(pid, 4323);
            killed.set(true);
        },
        |_, _| Err("ps unavailable".into()),
        |_| {
            report_count.set(report_count.get() + 1);
            Ok(())
        },
        |_| {},
    )
    .unwrap();

    assert!(killed.get(), "枚举失败不能阻断原有 stop");
    assert_eq!(report_count.get(), 0);
    assert!(matches!(
        running.0.lock().unwrap().get(session_id),
        Some(RunSlot::Finalizing {
            stop_requested: true
        })
    ));
}

#[test]
fn user_stop_without_running_pid_still_marks_launching_slot_stopped() {
    let db = test_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-stop-no-pid";
    try_reserve(&running, session_id).unwrap();
    let report_count = std::cell::Cell::new(0);

    stop_session_with_background_inspection(
        &db,
        &running,
        &team_running,
        session_id,
        Locale::Zh,
        |pid| panic!("Launching slot must not kill pid {pid}"),
        |pid, _| panic!("Launching slot must not inspect pid {pid}"),
        |_| {
            report_count.set(report_count.get() + 1);
            Ok(())
        },
        |_| {},
    )
    .unwrap();

    assert_eq!(report_count.get(), 0);
    assert!(matches!(
        running.0.lock().unwrap().get(session_id),
        Some(RunSlot::Launching {
            stop_requested: true
        })
    ));
}

#[test]
fn background_stop_notice_message_is_persisted_for_later_session_context() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-stop-notice-history",
        "stop notice",
        "local-default",
        "local",
    )
    .unwrap();

    let message = append_background_stop_notice_message(
            &conn,
            "s-stop-notice-history",
            "When stopping the session, detected 2 background processes in the same process group; they were terminated along with the session.",
        )
        .unwrap();

    assert_eq!(message.role, "assistant");
    assert_eq!(message.engine, None);
    assert_eq!(
        db::get_messages(&conn, "s-stop-notice-history").unwrap(),
        vec![message]
    );
}

#[test]
fn background_stop_notice_persist_failure_is_non_fatal_to_stop() {
    let db = test_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-stop-notice-persist-failed";
    running
        .0
        .lock()
        .unwrap()
        .insert(session_id.into(), RunSlot::Running(4324));

    let notice_conn = crate::test_support::mem_db();
    db::create_session(
        &notice_conn,
        session_id,
        "stop notice failure",
        "local-default",
        "local",
    )
    .unwrap();
    notice_conn
        .execute_batch("PRAGMA query_only = ON;")
        .unwrap();

    let killed = std::cell::Cell::new(false);
    let persist_attempted = std::cell::Cell::new(false);
    let emitted = std::cell::Cell::new(false);
    let result = stop_session_with_background_inspection(
        &db,
        &running,
        &team_running,
        session_id,
        Locale::En,
        |pid| {
            assert_eq!(pid, 4324);
            killed.set(true);
        },
        |_, _| Ok(Some("background stop notice".to_string())),
        |notice| {
            persist_attempted.set(true);
            append_background_stop_notice_message(&notice_conn, session_id, notice).map(|_| {
                emitted.set(true);
            })
        },
        |_| {},
    );

    assert!(result.is_ok(), "落库失败不应让停止流程返回错误：{result:?}");
    assert!(killed.get());
    assert!(persist_attempted.get());
    assert!(!emitted.get(), "落库失败后不应发送未持久化的消息");
    assert!(matches!(
        running.0.lock().unwrap().get(session_id),
        Some(RunSlot::Finalizing {
            stop_requested: true
        })
    ));
}
