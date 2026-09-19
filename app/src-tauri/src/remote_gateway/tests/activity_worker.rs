#![cfg(test)]

use super::*;
#[test]
fn run_activity_summary_worker_end_to_end_first_delta_flushes_promptly() {
    // 真正起线程的端到端接线冒烟测试：channel → 独立写线程 → 注入 writer。首次 dirty 不需要
    // 等 2s 节流窗口（activity_summary_due_flushes/activity_summary_retry_due 对
    // last_attempt_at_ms=None 恒判"到期"），只需等 tick 周期（250ms）追上，真实 sleep
    // 保持在亚秒级。
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    let calls: Arc<Mutex<Vec<(String, String, i64, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = Arc::clone(&calls);
    let writer: ActivitySummaryWriter = Box::new(move |session, run, tc, _f, _mc, _pp, st| {
        calls_clone
            .lock()
            .unwrap()
            .push((session.to_owned(), run.to_owned(), tc, st.to_owned()));
        Ok(())
    });
    let handle = thread::spawn(move || run_activity_summary_worker(rx, writer));

    tx.send(ActivitySummaryDelta {
        session_id: "s-e2e".to_owned(),
        run_id: "r-e2e".to_owned(),
        kind: ActivitySummaryDeltaKind::ToolCompleted {
            mcp: false,
            failed: false,
        },
    })
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !calls.lock().unwrap().is_empty() || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "首次 dirty 必须在一个 tick 周期内落库");
    assert_eq!(
        recorded[0],
        (
            "s-e2e".to_owned(),
            "r-e2e".to_owned(),
            1,
            "running".to_owned()
        )
    );

    drop(tx);
    handle.join().unwrap();
}

#[test]
fn run_activity_summary_worker_end_to_end_multi_delta_sequence_throttle_and_terminal_suppression() {
    // msgfix2 U1 修单 F5：真正起线程的端到端整链测试，覆盖"多 delta 序列 + 节流窗口 +
    // 终态压制"三件事在同一条真实 worker 生命周期里都对——不只是各自独立的纯函数单测。
    // 时序：① 首条 ToolCompleted 首次 dirty 立即（下一个 tick 内）落库为 running；
    // ② 紧接着再来一条 ToolCompleted——2s 节流窗口内不得重复发布（F2 节流仍然有效，
    // drain-then-check 的改动不能误伤正常节流）；③ Terminal 到达——不受节流约束，立即
    // 落库为 done；④ Terminal 写库成功之后再来的 ToolCompleted（"迟到的 running"）——
    // 必须被 sealed tombstone 挡住，不产生第三次写（F2②整链验证）。
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    let calls: Arc<Mutex<Vec<(String, String, i64, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = Arc::clone(&calls);
    let writer: ActivitySummaryWriter = Box::new(move |session, run, tc, _f, _mc, _pp, st| {
        calls_clone
            .lock()
            .unwrap()
            .push((session.to_owned(), run.to_owned(), tc, st.to_owned()));
        Ok(())
    });
    let handle = thread::spawn(move || run_activity_summary_worker(rx, writer));

    let wait_for_len = |n: usize, budget: Duration| -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if calls.lock().unwrap().len() >= n {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    };

    // ① 首条 ToolCompleted——首次 dirty 立即落库（running, tool_calls=1）。
    tx.send(ActivitySummaryDelta {
        session_id: "s-e2e2".to_owned(),
        run_id: "r-e2e2".to_owned(),
        kind: ActivitySummaryDeltaKind::ToolCompleted {
            mcp: false,
            failed: false,
        },
    })
    .unwrap();
    assert!(
        wait_for_len(1, Duration::from_secs(2)),
        "首次 dirty 必须在一个 tick 周期内落库"
    );
    {
        let recorded = calls.lock().unwrap();
        assert_eq!(
            recorded[0],
            (
                "s-e2e2".to_owned(),
                "r-e2e2".to_owned(),
                1,
                "running".to_owned()
            )
        );
    }

    // ② 紧接着再来一条 ToolCompleted——2s 节流窗口内不得重复发布。只等一小段（远小于
    //    ACTIVITY_SUMMARY_THROTTLE_MS=2000ms）确认没有过早发布，不真的等满 2s。
    tx.send(ActivitySummaryDelta {
        session_id: "s-e2e2".to_owned(),
        run_id: "r-e2e2".to_owned(),
        kind: ActivitySummaryDeltaKind::ToolCompleted {
            mcp: false,
            failed: false,
        },
    })
    .unwrap();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "节流窗口内不得对同一 run 重复发布 running 快照"
    );

    // ③ Terminal 到达——不受节流约束，立即落库为 done，tool_calls 累计到 2。
    // msgfix2 U1 修单三（G3·独立审查残余 P2）：这里的等待预算必须**远小于**
    // `ACTIVITY_SUMMARY_THROTTLE_MS`(2000ms)，否则"终态立即落库"和"终态被误按节流窗口
    // 排队、凑巧在窗口关闭前也到期了"这两种情况在断言层面无法区分——旧版本这里等的也是
    // 2s，等于把"立即"和"最迟 2s 内"混为一谈。tick 周期是 250ms，600ms 预算足够覆盖
    // 调度抖动，同时严格小于节流窗口，真正证明了"不等节流"。
    tx.send(ActivitySummaryDelta {
        session_id: "s-e2e2".to_owned(),
        run_id: "r-e2e2".to_owned(),
        kind: ActivitySummaryDeltaKind::Terminal { failed: false },
    })
    .unwrap();
    assert!(
        wait_for_len(2, Duration::from_millis(600)),
        "终态必须立即（远早于 2s 节流窗口）落库，不是恰好卡着节流窗口到期"
    );
    {
        let recorded = calls.lock().unwrap();
        assert_eq!(
            recorded[1],
            (
                "s-e2e2".to_owned(),
                "r-e2e2".to_owned(),
                2,
                "done".to_owned()
            )
        );
    }

    // ④ Terminal 写库成功之后再来的 ToolCompleted（"迟到的 running"）——sealed tombstone
    //    必须挡住它，不产生第三次写。
    // msgfix2 U1 修单三（G3·独立审查残余 P2）：断言必须在 worker 线程仍存活时验证（本
    // 测试确实如此——`drop(tx)`/`handle.join()` 在最后才调用），而不是先 drop/join 让
    // worker 退出、再检查一个"反正不会再变"的静态快照，那样测不出"worker 活着的时候会
    // 不会自己触发第三次写"。且等待窗口刻意跨过一整个 `ACTIVITY_SUMMARY_THROTTLE_MS`
    // (2000ms) 节流周期（本刀 G1 把 `activity_summary_due_flushes` 的过滤条件从"未 sealed"
    // 改成了"未 sealed || (sealed && terminal_pending)"——如果这个改动有回归、误把已经
    // 成功落库的 sealed 条目也收进重试批次，只等 400ms 未必能在下一个节流窗口到期前捕捉
    // 到多余的第三次写；跨过整个窗口才是真正的回归保护）。全程断言写库次数恰好 2（首条
    // running + 终态），不再增长。
    tx.send(ActivitySummaryDelta {
        session_id: "s-e2e2".to_owned(),
        run_id: "r-e2e2".to_owned(),
        kind: ActivitySummaryDeltaKind::ToolCompleted {
            mcp: false,
            failed: false,
        },
    })
    .unwrap();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "终态写库成功后到达的迟到 running delta 不得复活该 run 或触发新的写（worker 仍存活时验证）"
    );
    thread::sleep(Duration::from_millis(ACTIVITY_SUMMARY_THROTTLE_MS + 300));
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "跨过一整个节流窗口后，写库次数仍必须恰好 2——不能因为 G1 的 sealed&&pending 重试\
             路径误把已成功落库的 tombstone 又收进批次"
    );

    drop(tx);
    handle.join().unwrap();
}

/// msgfix2 U1b Task A：装配级接线证明——本文件（乃至整个测试二进制）里唯一一处真正调用
/// 生产入口 `setup()` 的测试（`GATEWAY` 是进程级 `OnceLock`，其余全部测试要么直接构造
/// `Inner{..}`、要么用 `configure_activity_summary_writer_test_hook` 绕开它，都刻意不碰
/// 这个单例；本测试反过来专门验证单例这条真实装配路径）。settings 恒 `|_| None` →
/// `connect_loop` 线程永远停在 `Disabled`（`upstream_state` 永不置位，见
/// `enqueue_milestone_for_upstream`/`publish_milestone` 文档"gate 关闭时是 no-op"），因此
/// 这条后台线程不会对其余测试产生任何可观察副作用，可以安全地让它随进程活到测试结束
/// （同 `run_session_index_snapshot_worker` 既有"随 Inner 生命周期自然收尾"惯例）。
///
/// 证明的不是"聚合器纯函数本身对不对"（那些已经有专门单测），是"`setup()` → 唯一跨模块
/// 入口 `install_activity_summary_writer` → `configure_activity_summary_writer` → 独立写
/// 线程 → 注入的 writer"这条**装配链路**本身接得上——writer 落的是一个真实
/// `db::upsert_activity_summary_and_publish` 调用（内容签名与 lib.rs 生产 provider 完全
/// 一致，唯一区别是生产版本从 `AppHandle`/`Db` state 拿连接、这里直接捕获测试 DB 连接），
/// 不是一个只返回 `Ok(())` 的假 writer。
#[test]
fn setup_wires_real_activity_summary_writer_through_test_db_not_a_mock() {
    use rusqlite::OptionalExtension;

    assert!(
        GATEWAY.get().is_none(),
        "GATEWAY 已被其它测试设置过——本测试要求是本进程内唯一调用 setup() 的测试，\
             否则下面的 idempotent get_or_init 会直接短路、测不出真实装配链路"
    );

    let conn = Arc::new(Mutex::new(crate::test_support::mem_db()));
    crate::db::create_session(
        &lock(&conn),
        "s-u1b-setup-wire",
        "x",
        "local-default",
        "local",
    )
    .unwrap();

    let writer_conn = Arc::clone(&conn);
    let writer: ActivitySummaryWriter = Box::new(
        move |session_id, run_id, tool_calls, failed, mcp_calls, permission_prompts, state| {
            let guard = lock(&writer_conn);
            crate::db::upsert_activity_summary_and_publish(
                &guard,
                session_id,
                run_id,
                tool_calls,
                failed,
                mcp_calls,
                permission_prompts,
                state,
            )
            .map_err(|e| e.to_string())
        },
    );

    setup(
        Box::new(|_| None),
        Box::new(|| None),
        test_desktop_credential_provider(),
        test_claim_client(),
        test_active_device_provider(),
        test_active_room_resolver(),
        Box::new(|_| None),
        Box::new(|| None),
        Box::new(|| None),
        Box::new(|| None),
        Box::new(|_| None),
        Box::new(|_| PairDoneAction::Rejected),
        test_registry(),
        test_registry_snapshot_provider(),
        test_registry_rebase_provider(),
        test_registry_high_water_provider(),
        test_refresh_handler(),
        Box::new(|_| Some(AckOutcome::Failed)),
        Box::new(|_| Some(AckOutcome::Failed)),
        Box::new(|_, _| true),
        Box::new(|_| AckOutcome::Failed),
        test_session_repo_provider(),
        test_session_history_provider(),
        test_message_fetch_provider(),
    );
    assert!(GATEWAY.get().is_some(), "setup() 必须建立 GATEWAY 单例");

    // Task A 验收点①：真实 setup() 之后，install_activity_summary_writer 是唯一能配置
    // writer 的跨模块入口——直接调它，不绕开。
    install_activity_summary_writer(writer);
    let inner = GATEWAY.get().expect("checked above");
    assert!(
        inner.state.activity_summary_tx.get().is_some(),
        "install_activity_summary_writer 之后 activity_summary_tx 必须已配置——这正是\
             `extract_tool_milestones` 判断'聚合器是否启用'的唯一依据"
    );

    // B3（G3）时序锚：记录 delta 发出时刻，断言"真实落库时刻"严格早于节流窗口
    // （ACTIVITY_SUMMARY_THROTTLE_MS=2000ms）到期——不是"反正等到 3s 兜底超时就算过"的
    // 平凡实现（那样测不出写线程是不是绕了一圈节流才凑巧赶上宽松 deadline）。
    let sent_at = Instant::now();
    inner
        .state
        .activity_summary_tx
        .get()
        .unwrap()
        .send(ActivitySummaryDelta {
            session_id: "s-u1b-setup-wire".to_owned(),
            run_id: "run-u1b-setup-wire".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: false,
                failed: false,
            },
        })
        .unwrap();

    let poll_deadline = sent_at + Duration::from_secs(3);
    let mut landed_at = None;
    while Instant::now() < poll_deadline {
        let row: Option<String> = lock(&conn)
            .query_row(
                "SELECT content FROM messages WHERE session_id = ?1 AND dedup_key = ?2",
                ("s-u1b-setup-wire", "activity_summary:run-u1b-setup-wire"),
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        if row.is_some() {
            landed_at = row.map(|content| (Instant::now(), content));
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let (landed_at, content_raw) = landed_at.expect(
        "真实 setup() 装配出的 writer 必须把聚合器产出的写真正落到测试 DB——不是靠聚合器\
             自身单测已绿就假定生产装配链路也接得上",
    );
    assert!(
        landed_at.duration_since(sent_at) < Duration::from_millis(ACTIVITY_SUMMARY_THROTTLE_MS),
        "首次 delta 落库必须严格早于节流窗口（{ACTIVITY_SUMMARY_THROTTLE_MS}ms）到期，\
             证明走的是 tick 周期立即写，不是凑巧撞在一个宽松 deadline 里"
    );
    let content: serde_json::Value = serde_json::from_str(&content_raw).unwrap();
    assert_eq!(content[0]["run_id"], "run-u1b-setup-wire");
    assert_eq!(content[0]["tool_calls"], 1);
    assert_eq!(content[0]["state"], "running");
}
