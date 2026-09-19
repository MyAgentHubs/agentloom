#![cfg(test)]

use super::*;

#[test]
fn draining_guard_excludes_same_session_until_drop() {
    let session_id = "s-draining-guard-mutual-exclusion";
    let first = try_begin_draining(session_id).expect("第一次应取得排空资格");
    assert!(
        try_begin_draining(session_id).is_none(),
        "首个 guard 存活时，同 session 第二次进入必须被拒绝"
    );
    drop(first);
    let third = try_begin_draining(session_id).expect("guard drop 后应可再次排空");
    drop(third);
}

#[test]
fn draining_guard_removes_session_during_panic_unwind() {
    let session_id = "s-draining-guard-panic-cleanup";
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = try_begin_draining(session_id).expect("应取得排空资格");
        panic!("intentional panic to verify DrainingGuard::drop");
    }));
    assert!(panicked.is_err());
    let after_unwind =
        try_begin_draining(session_id).expect("panic unwind 时 guard 应已摘除 session");
    drop(after_unwind);
}

#[test]
fn draining_guard_generation_prevents_aba_drop_from_removing_new_registration() {
    let session_id = "s-draining-guard-generation-aba";

    // 1. A 取得第一代排空资格；guard 暂不 drop，模拟它仍在返回栈上。
    let a_guard = try_begin_draining(session_id).expect("A 应取得第一代排空资格");
    // 2. A 的最后一轮确认无脏位并摘除登记，但旧 guard 尚未走到作用域末尾。
    assert!(!drain_round_dirty_and_continue(session_id));
    // 3. B 趁正常收尾窗口取得空位，登记新一代排空资格。
    let b_guard = try_begin_draining(session_id).expect("B 应在窗口内取得新一代排空资格");
    // 4. A 的旧 guard 此刻才 drop；generation 不匹配时必须是 no-op。
    drop(a_guard);
    // 5. C 不得因 A 误删 B 的登记而并发取得排空资格。
    let c_attempt = try_begin_draining(session_id);
    assert!(c_attempt.is_none(), "A 的旧 guard 不得误删 B 的新一代登记");
    // 6. C 的失败尝试已合并为脏位，B 收尾时必须原地重放一轮。
    assert!(drain_round_dirty_and_continue(session_id));
    // 7. B 重放后没有更多脏位，应正常摘除登记并结束。
    assert!(!drain_round_dirty_and_continue(session_id));
    // 8. B 的 guard 随后 drop；自己的登记已摘除，因此应为 no-op。
    drop(b_guard);
    // 9. map 已干净，后续触发可重新登记；显式 drop，避免污染其它测试。
    let final_guard = try_begin_draining(session_id).expect("B 正常收尾后应允许重新取得排空资格");
    drop(final_guard);
}

#[test]
fn drain_with_dirty_replay_replays_notification_merged_during_first_round() {
    let session_id = "s-draining-dirty-replay";
    let _guard = try_begin_draining(session_id).expect("外层应先取得排空资格");
    let mut rounds = 0;

    drain_with_dirty_replay(session_id, || {
        rounds += 1;
        if rounds == 1 {
            assert!(
                try_begin_draining(session_id).is_none(),
                "进行中的第二次释放通知仍不得绕过互斥"
            );
        }
    });

    assert_eq!(rounds, 2, "合并的脏位应触发恰好一次原地重放");
    let after_replay = try_begin_draining(session_id)
        .expect("重放完成后正常路径应已摘除登记，不依赖外层 guard drop");
    drop(after_replay);
}

#[test]
fn drain_with_dirty_replay_stops_after_clean_round() {
    let session_id = "s-draining-clean-round";
    let _guard = try_begin_draining(session_id).expect("外层应先取得排空资格");
    let mut rounds = 0;

    drain_with_dirty_replay(session_id, || rounds += 1);

    assert_eq!(rounds, 1, "无脏位时不应额外重放");
}

// T7：worker 报告落账（M1 台账 pending 行）与 lead run 槽释放各自触发同一 session 的
// `drain_after_run_release`——两个触发谁先谁后都可能发生（worker settled 回调与 lead
// 收尾释放槽是两条并发路径）。这两条测试用 `try_begin_draining` + `drain_with_dirty_replay`
// 这套既有排空引擎（同 `drain_with_dirty_replay_replays_notification_merged_during_first_round`
// 手法）分别模拟「谁先取得排空资格」的两种顺序，断言：无论哪一种，晚到的那次触发都只能
// 合并为脏位、换来恰好一次原地重放（不是零次——不丢唤醒；也不是多次——不空转），而它携带的
// 报告最终被消费恰好一次（`consumed == 1`），不丢也不重复起跑。

#[test]
fn worker_settled_race_settled_trigger_wins_first_lead_release_merges_as_dirty() {
    let session_id = "s-worker-settled-race-settled-first";
    // `report_pending` 模拟 member_report_delivery 台账里这条报告的 pending 状态；
    // `consumed` 记录它被一次完整续跑轮次真实消费（ack）的次数。
    let mut report_pending = false;
    let mut consumed = 0;
    let mut rounds = 0;

    // worker 报告刚落账 ack、`on_worker_settled` 抢先取得本 session 的排空资格。
    let _settled_guard = try_begin_draining(session_id).expect("settled 触发应先取得排空资格");

    drain_with_dirty_replay(session_id, || {
        rounds += 1;
        // 模拟 `try_resume_pending` 的原子快照：只认本轮开始那一刻已经落账的状态。
        let snapshot = report_pending;
        if rounds == 1 {
            // lead 收尾释放槽之后也调用 `drain_after_run_release`——本轮快照已经拍过，
            // 这条报告要等到下一轮才可能被看到；同时这次触发撞上正在跑的 settled 排空，
            // 必须被拒绝、只能合并为脏位（不丢：脏位会换来一次原地重放）。
            report_pending = true;
            assert!(
                try_begin_draining(session_id).is_none(),
                "lead 收尾释放触发撞上进行中的 settled 排空必须被拒绝、合并为脏位"
            );
        }
        if snapshot {
            report_pending = false;
            consumed += 1;
        }
    });

    assert_eq!(
        rounds, 2,
        "晚到的释放触发必须换来恰好一次原地重放，不多不少"
    );
    assert_eq!(
        consumed, 1,
        "报告必须被消费恰好一次：round 1 快照拍早了消费不到，脏位重放的 round 2 补上，不丢唤醒"
    );
    let after =
        try_begin_draining(session_id).expect("重放完成后排空互斥登记应已摘除，可正常重新取得资格");
    drop(after);
}

#[test]
fn worker_settled_race_lead_release_trigger_wins_first_settled_merges_as_dirty() {
    let session_id = "s-worker-settled-race-release-first";
    let mut report_pending = false;
    let mut consumed = 0;
    let mut rounds = 0;

    // lead 收尾释放槽抢先取得本 session 的排空资格（此刻 worker 报告尚未落账 ack）。
    let _release_guard = try_begin_draining(session_id).expect("释放触发应先取得排空资格");

    drain_with_dirty_replay(session_id, || {
        rounds += 1;
        let snapshot = report_pending;
        if rounds == 1 {
            // worker 报告随后落账 ack、`on_worker_settled` 也调用 `drain_after_run_release`——
            // 撞上正在跑的释放排空，必须被拒绝、只能合并为脏位。
            report_pending = true;
            assert!(
                try_begin_draining(session_id).is_none(),
                "worker settled 触发撞上进行中的释放排空必须被拒绝、合并为脏位"
            );
        }
        if snapshot {
            report_pending = false;
            consumed += 1;
        }
    });

    assert_eq!(
        rounds, 2,
        "晚到的 settled 触发必须换来恰好一次原地重放，不多不少"
    );
    assert_eq!(
        consumed, 1,
        "报告必须被消费恰好一次：round 1 快照拍早了消费不到，脏位重放的 round 2 补上，不丢唤醒"
    );
    let after =
        try_begin_draining(session_id).expect("重放完成后排空互斥登记应已摘除，可正常重新取得资格");
    drop(after);
}

#[test]
fn remote_input_drain_storm_claims_before_spawning_and_replays_dirty_round() {
    // handler 必须在 ws 读线程先 claim；回退成旧式无条件 spawn 时，本测试应先在结构护栏变红。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let handler = production
        .split("fn remote_gateway_input_send_handler(")
        .nth(1)
        .unwrap()
        .split("\nfn remote_gateway_control_stop_handler(")
        .next()
        .unwrap();
    let claim_idx = handler
        .find("let Some(guard) = try_begin_draining(&session)")
        .expect("remote input drain 必须在线程创建前 claim");
    let spawn_idx = handler
        .find("spawn_remote_input_drain(")
        .expect("claim 成功后必须创建排空线程");
    let owned_idx = handler
        .find("drain_owned(app, session, guard)")
        .expect("排空线程必须直接消费已取得的 guard，不能二次 claim");
    assert!(
        claim_idx < spawn_idx && spawn_idx < owned_idx,
        "remote input drain 必须先 claim，再 spawn 并把 guard 移交给 drain_owned"
    );

    let session_id = "s-remote-input-drain-spawn-storm";
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let actual_spawns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rounds = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let guard = try_begin_draining(session_id).expect("第一次触发必须取得排空资格");
    let drain_session = session_id.to_string();
    let drain_spawns = actual_spawns.clone();
    let drain_rounds = rounds.clone();
    spawn_remote_input_drain(move || {
        let _guard = guard;
        drain_spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        started_tx.send(()).unwrap();
        let mut first_round = true;
        drain_with_dirty_replay(&drain_session, || {
            drain_rounds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if first_round {
                first_round = false;
                gate_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("test must release the first drain round");
            }
        });
        done_tx.send(()).unwrap();
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("first remote input drain must start");

    let mut spawn_decisions = 1;
    for _ in 1..8 {
        let Some(extra_guard) = try_begin_draining(session_id) else {
            continue;
        };
        spawn_decisions += 1;
        let extra_spawns = actual_spawns.clone();
        spawn_remote_input_drain(move || {
            let _guard = extra_guard;
            extra_spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
    }

    assert_eq!(spawn_decisions, 1, "8 次快速触发必须只作出 1 次 spawn 决策");
    assert_eq!(
        actual_spawns.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "同 session 风暴期间必须只实际启动 1 个排空线程"
    );

    gate_tx.send(()).unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("dirty replay must finish within the timeout");
    assert_eq!(
        rounds.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "被合并的 7 次触发必须通过脏位让现有线程原地重放一轮"
    );
}

#[test]
fn startup_remote_inbox_rescan_acquires_draining_guard_before_drain() {
    // 启动重扫必须先取同 session 排空互斥，避免与正常 run-release 路径并发消费 FIFO。
    // 变异自证：去掉 try_begin_draining 包裹、恢复直接 drain_remote_inbox，这条测试会变红。
    // 启发式源码断言：只挡启动循环内缺少 guard 或两个调用文本整体调换的粗糙回归；不验证
    // guard 的运行时生命周期，也挡不住把调用藏进其他函数或更绕控制流的改法，仍需人工 review。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let loop_body = production
        .split("for session_id in pending_remote_sessions {")
        .nth(1)
        .unwrap()
        .split("\n                }\n            });")
        .next()
        .unwrap();
    let begin_idx = loop_body
        .find("try_begin_draining(")
        .expect("启动重扫必须先取得同 session 排空互斥");
    let drain_idx = loop_body
        .find("drain_remote_inbox(")
        .expect("启动重扫必须调用 drain_remote_inbox");
    assert!(
        begin_idx < drain_idx,
        "启动重扫必须先 try_begin_draining，再 drain_remote_inbox"
    );
}

#[test]
fn drain_owned_runs_resume_pending_before_remote_inbox_and_inbox_not_gated() {
    // T4 C2：旧「autofeed → 迟到答案挂账 → inbox」三段顺序已合并为「try_resume_pending →
    // remote_inbox」两段；排空序仍固定，且 inbox 段不能被包进依赖 try_resume_pending 返回值
    // 的条件分支——remote inbox 不受自动恢复的 not_before 门影响（F.6）。
    // 启发式源码断言：只挡把 inbox 段整体挪到 try_resume_pending 之前、或把它塞进条件分支
    // 这类粗糙回归；不验证调用是否藏在更绕的控制流里，也不证明运行时真的按此顺序执行。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn drain_owned(")
        .nth(1)
        .unwrap()
        .split("\n/// 纯循环内核")
        .next()
        .unwrap();
    let resume_idx = body
        .find("try_resume_pending(")
        .expect("必须调用 try_resume_pending");
    let inbox_idx = body
        .find("drain_remote_inbox(")
        .expect("必须调用 drain_remote_inbox");
    assert!(
        resume_idx < inbox_idx,
        "try_resume_pending 必须先于 remote_inbox 排空段"
    );
    assert!(
        !body.contains("if try_resume_pending("),
        "remote inbox 不能被包进依赖 try_resume_pending 返回值的条件分支——它不受自动恢复\
             的 not_before 门影响"
    );
}

#[test]
fn drain_remote_inbox_next_pending_releases_db_lock_before_deliver_closure() {
    // 三轮真实死锁事故原址护栏：next_pending 必须用独立闭包短锁取一条即释放，绝不能把 conn
    // 提到闭包外、跨进 deliver（它会再走加 db 锁的投递路径）。本启发式断言能挡闭包整体被拆、
    // conn 被直接上提及闭包边界被抹掉的回归；挡不住有人在闭包外另 lock 一次藏进别的变量等
    // 更绕写法，那仍需人工 review。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn drain_remote_inbox(")
        .nth(1)
        .unwrap()
        .split("\n/// 纯循环内核")
        .next()
        .unwrap();

    let next_closure_idx = body
        .find("drain_remote_inbox_loop(\n        || {")
        .expect("next_pending 必须是 drain_remote_inbox_loop 的独立闭包");
    let lock_idx = body
        .find("db_state.0.lock()")
        .expect("next_pending 闭包内必须拿 db 锁");
    let fetch_idx = body
        .find("db::next_pending_remote_input(&conn, session_id)")
        .expect("next_pending 必须用局部 conn 取一条记录");
    // P0-c 返工（rustfmt 副作用）：`deliver_remote_inbox_entry` 调用行原来单行超长，跑
    // `rustfmt --edition 2021`（本轮测试硬度钉③）后被拆成 `|kind, payload, command_id| {`
    // 起头的多行闭包体——下面两处字面匹配串跟着改成新的多行形状，护栏意图（next_pending
    // 闭包必须先收口释放锁，deliver 闭包才开始）不变。
    let closure_end_idx = body
            .find(
                "\n        },\n        |kind, payload, command_id| {\n            deliver_remote_inbox_entry",
            )
            .expect("next_pending 闭包必须在 deliver 闭包开始前独立收口");
    let deliver_idx = body
        .find("|kind, payload, command_id| {\n            deliver_remote_inbox_entry")
        .expect("必须找到 deliver 闭包");
    assert!(
        next_closure_idx < lock_idx
            && lock_idx < fetch_idx
            && fetch_idx < closure_end_idx
            && closure_end_idx < deliver_idx,
        "必须是 next_pending 闭包内 lock/fetch → 闭包收口释放 conn → deliver 闭包"
    );
    assert_eq!(
        body[..deliver_idx].matches("db_state.0.lock()").count(),
        1,
        "deliver 之前只应有 next_pending 闭包内部这一处 db lock"
    );
}

#[test]
fn spawn_and_stream_solo_run_release_triggers_drain_after_run_release() {
    // 启发式源码断言：只挡「spawn_and_stream 里 emit 之后完全没有 drain」这类回归；挡不住
    // 把 drain 藏进某个被误认为安全、实际在锁内执行的子闭包等绕法，那仍需人工 review。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn spawn_and_stream(")
        .nth(1)
        .unwrap()
        .split("\n#[tauri::command]")
        .next()
        .unwrap();
    let emit_idx = body
        .find("emit_terminal_after_releasing_run_slot(")
        .expect("spawn_and_stream 必须释放 run 槽并 emit terminal");
    let drain_idx = body
        .find("drain_after_run_release(")
        .expect("solo run 槽释放后必须触发统一排空");
    assert!(
        emit_idx < drain_idx,
        "solo drain 必须出现在 emit_terminal_after_releasing_run_slot 之后"
    );
}

#[test]
fn spawn_remote_input_drain_returns_without_waiting_and_names_thread() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let (name_tx, name_rx) = std::sync::mpsc::channel();
    let (returned_tx, returned_rx) = std::sync::mpsc::channel::<()>();

    std::thread::spawn(move || {
        spawn_remote_input_drain(move || {
            gate_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("test must release the drain gate");
            name_tx
                .send(std::thread::current().name().map(String::from))
                .unwrap();
        });
        returned_tx.send(()).unwrap();
    });

    let returned_without_waiting = returned_rx
        .recv_timeout(std::time::Duration::from_millis(200))
        .is_ok();
    let _ = gate_tx.send(());
    let thread_name = name_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("remote input drain must run within the timeout");

    assert!(
        returned_without_waiting,
        "spawn_remote_input_drain must return without waiting for the drain"
    );
    assert_eq!(thread_name, Some("remote-input-drain".to_string()));
}

#[test]
fn spawn_remote_answer_processing_returns_without_waiting_and_names_thread() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let (name_tx, name_rx) = std::sync::mpsc::channel();
    let (returned_tx, returned_rx) = std::sync::mpsc::channel::<()>();

    std::thread::spawn(move || {
        spawn_remote_answer_processing(move || {
            gate_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("test must release the answer gate");
            name_tx
                .send(std::thread::current().name().map(String::from))
                .unwrap();
        });
        returned_tx.send(()).unwrap();
    });

    let returned_without_waiting = returned_rx
        .recv_timeout(std::time::Duration::from_millis(200))
        .is_ok();
    let _ = gate_tx.send(());
    let thread_name = name_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("remote answer must run within the timeout");

    assert!(
        returned_without_waiting,
        "spawn_remote_answer_processing must return without waiting for answer handling"
    );
    assert_eq!(thread_name, Some("remote-answer".to_string()));
}
