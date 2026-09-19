#![cfg(test)]

use super::*;
#[test]
fn flush_activity_summary_success_clears_dirty_and_advances_throttle_clock_for_running() {
    let mut state = ActivitySummaryAggregatorState::default();
    state.runs.insert(
        "r1".to_owned(),
        RunActivityEntry {
            session_id: "s1".to_owned(),
            counters: ActivityCounters {
                tool_calls: 2,
                ..Default::default()
            },
            state: "running",
            sealed: false,
            terminal_pending: false,
            dirty: true,
            last_flushed_at_ms: None,
            consecutive_failures: 2,
            last_attempt_at_ms: Some(1_000),
        },
    );
    let calls: Arc<Mutex<Vec<(String, String, i64, i64, i64, i64, String)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let calls_clone = Arc::clone(&calls);
    let writer: ActivitySummaryWriter = Box::new(move |session, run, tc, f, mc, pp, st| {
        calls_clone.lock().unwrap().push((
            session.to_owned(),
            run.to_owned(),
            tc,
            f,
            mc,
            pp,
            st.to_owned(),
        ));
        Ok(())
    });
    let failures = AtomicU64::new(0);
    let flush = ActivitySummaryFlush {
        run_id: "r1".to_owned(),
        session_id: "s1".to_owned(),
        counters: ActivityCounters {
            tool_calls: 2,
            ..Default::default()
        },
        state: "running",
    };
    flush_activity_summary(&mut state, &writer, &failures, flush, 5_000);

    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        calls.lock().unwrap()[0].2,
        2,
        "tool_calls 必须原样传给 writer"
    );
    let entry = &state.runs["r1"];
    assert!(!entry.dirty, "写成功必须清 dirty");
    assert!(entry.last_flushed_at_ms.is_some(), "写成功必须推进节流时钟");
    assert_eq!(
        entry.last_flushed_at_ms,
        Some(5_000),
        "推进的时钟必须是调用方传入的 now_ms，不是内部另取的系统时钟"
    );
    assert_eq!(
        entry.last_attempt_at_ms,
        Some(5_000),
        "写成功也要推进 last_attempt_at_ms（R6③退避窗口的计时起点）"
    );
    assert_eq!(
        entry.consecutive_failures, 0,
        "写成功必须把此前累积的连续失败计数清零（R6③）——之前 2 次失败不该继续拖累退避"
    );
    assert_eq!(failures.load(Ordering::Relaxed), 0);
}

#[test]
fn flush_activity_summary_success_for_sealed_run_keeps_persistent_tombstone_and_blocks_late_running_delta(
) {
    // msgfix2 U1 修单 F2②：过去终态写成功后会把条目从内存表移除，导致「迟到 200ms 的
    // running」在 `apply_activity_summary_delta` 里因为 entry 不存在而被 `or_insert_with`
    // 重新造出一个未 sealed 的新条目——run 被悄悄"复活"成 running 重新发布。修法是终态写
    // 成功后条目**永久留在** `state.runs`（sealed=true 的持久 tombstone），本测试正反两段
    // 锁死：① 写成功后条目仍在、仍 sealed；② 之后到达的 ToolCompleted delta 必须被直接
    // 丢弃（`apply_activity_summary_delta` 返回 `None`、计数不累加、不产生新 flush），不
    // 会重新变成 "running"。
    // msgfix2 U1 修单三（G1）追加：初始 `terminal_pending: true`（模拟"终态已到达但这次
    // 写才第一次真正成功落库"——例如之前重试过若干次），断言写成功后必须清成 false，不
    // 再被 `activity_summary_due_flushes` 收进任何批次。
    let mut state = ActivitySummaryAggregatorState::default();
    state.runs.insert(
        "r1".to_owned(),
        RunActivityEntry {
            session_id: "s1".to_owned(),
            counters: ActivityCounters::default(),
            state: "done",
            sealed: true,
            terminal_pending: true,
            dirty: false,
            last_flushed_at_ms: None,
            consecutive_failures: 0,
            last_attempt_at_ms: None,
        },
    );
    let writer: ActivitySummaryWriter = Box::new(|_, _, _, _, _, _, _| Ok(()));
    let failures = AtomicU64::new(0);
    flush_activity_summary(
        &mut state,
        &writer,
        &failures,
        ActivitySummaryFlush {
            run_id: "r1".to_owned(),
            session_id: "s1".to_owned(),
            counters: ActivityCounters::default(),
            state: "done",
        },
        0,
    );
    assert!(
        state.runs.contains_key("r1"),
        "终态写成功后必须保留持久 tombstone，不能从内存表移除"
    );
    assert!(state.runs["r1"].sealed, "tombstone 必须保持 sealed=true");
    assert!(
        !state.runs["r1"].terminal_pending,
        "终态写成功后必须清 terminal_pending，不再被 tick 扫描收进重试批次（G1）"
    );
    assert!(
        activity_summary_due_flushes(&state, 999_999).is_empty(),
        "写成功后的 tombstone 既不 dirty 也不再 pending，不得再出现在待写批次里"
    );

    // 迟到的 running delta——不得复活这个 run。
    let late = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: false,
                failed: false,
            },
        },
    );
    assert_eq!(late, None, "sealed 后到达的 running delta 必须被直接丢弃");
    assert_eq!(
        state.runs["r1"].counters.tool_calls, 0,
        "迟到 delta 不得累加计数——tombstone 必须挡住复活"
    );
    assert_eq!(
        state.runs["r1"].state, "done",
        "迟到 delta 不得把 state 改回 running"
    );
}

#[test]
fn flush_activity_summary_write_failure_keeps_dirty_for_retry_and_counts_failure() {
    let mut state = ActivitySummaryAggregatorState::default();
    state.runs.insert(
        "r1".to_owned(),
        RunActivityEntry {
            session_id: "s1".to_owned(),
            counters: ActivityCounters::default(),
            state: "running",
            sealed: false,
            terminal_pending: false,
            dirty: true,
            last_flushed_at_ms: None,
            consecutive_failures: 0,
            last_attempt_at_ms: None,
        },
    );
    let writer: ActivitySummaryWriter = Box::new(|_, _, _, _, _, _, _| Err("db busy".into()));
    let failures = AtomicU64::new(0);
    flush_activity_summary(
        &mut state,
        &writer,
        &failures,
        ActivitySummaryFlush {
            run_id: "r1".to_owned(),
            session_id: "s1".to_owned(),
            counters: ActivityCounters::default(),
            state: "running",
        },
        7_000,
    );
    assert!(
        state.runs["r1"].dirty,
        "写失败必须保留 dirty 供下轮 tick 重试"
    );
    assert!(state.runs["r1"].last_flushed_at_ms.is_none());
    assert_eq!(failures.load(Ordering::Relaxed), 1);
    // R6③：写失败也必须推进 last_attempt_at_ms 并累加 consecutive_failures——旧实现完全
    // 不碰这两个字段，`activity_summary_due_flushes` 只能靠 `last_flushed_at_ms` 恒 None
    // 判定"到期"，原地空转重试。
    assert_eq!(
        state.runs["r1"].last_attempt_at_ms,
        Some(7_000),
        "写失败也要记录本次尝试的时刻，退避窗口据此计时"
    );
    assert_eq!(
        state.runs["r1"].consecutive_failures, 1,
        "第一次写失败，连续失败计数必须从 0 变成 1"
    );
}

#[test]
fn terminal_write_failure_is_retried_until_success_exactly_once_while_late_running_stays_blocked() {
    // msgfix2 U1 修单三（G1·独立审查 P1）：终态写失败永不重试 = 永久漏封。修复前 terminal
    // 在写库前就置 `sealed=true, dirty=false`，writer 失败直接 `return`，tick 扫描又把
    // sealed 一律排除在待写批次外——该 run 从此永远停在"内存已 sealed、DB 仍是 running"
    // 的状态，违反 M0「有活动 run 终态必封口恰好一次」。
    //
    // 本测试锁死完整轨迹：immediate attempt 失败（第 1 次）→ 期间迟到的 running delta
    // 仍必须被挡（sealed 语义不受 pending 影响，计数不累加）→ tick 扫描重试第 2 次仍失败
    // → writer 恢复后第 3 次重试成功，terminal_pending 清掉、不再被任何后续 tick 收进
    // 批次——终态最终恰好落库一次，不多不少。R6③ 之后重试之间要满足指数退避窗口
    // （`activity_summary_retry_due`：第 1 次失败后退避 = THROTTLE_MS(2000) * 2^1 =
    // 4000ms，第 2 次失败后退避 = THROTTLE_MS * 2^2 = 8000ms），下面每次 `flush_activity_
    // summary`/`activity_summary_due_flushes` 调用显式推进模拟时钟满足这个窗口——不是
    // 为了测退避本身（那是 `activity_summary_due_flushes_backs_off_exponentially_
    // after_repeated_failures_and_caps_at_30s` 的职责），只是让这条既有的"最终恰好落库
    // 一次"轨迹在退避生效后依然成立。
    let mut state = ActivitySummaryAggregatorState::default();
    apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: false,
                failed: false,
            },
        },
    );
    let terminal_flush = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    )
    .expect("终态 delta 必须立即返回待写快照");
    assert!(state.runs["r1"].sealed);
    assert!(
        state.runs["r1"].terminal_pending,
        "终态到达即置 pending——尚未成功落库前必须保持待重试"
    );

    let writer: ActivitySummaryWriter = Box::new({
        let attempts = AtomicU64::new(0);
        move |_, _, _, _, _, _, _| {
            let n = attempts.fetch_add(1, Ordering::Relaxed);
            if n < 2 {
                Err("db busy".into())
            } else {
                Ok(())
            }
        }
    });
    let failures = AtomicU64::new(0);

    // 第 1 次：immediate attempt（调用方紧接着 Terminal delta 触发的那次写）失败，模拟
    // 时刻 t=0。
    flush_activity_summary(&mut state, &writer, &failures, terminal_flush, 0);
    assert!(
        state.runs["r1"].sealed,
        "sealed 不受写失败影响，继续挡迟到 running"
    );
    assert!(
        state.runs["r1"].terminal_pending,
        "写失败必须保留 pending 供下轮 tick 重试"
    );
    assert_eq!(failures.load(Ordering::Relaxed), 1);

    // 期间迟到的 running delta——sealed 语义不受 pending 影响，必须继续被挡，不累加计数。
    let late = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: false,
                failed: false,
            },
        },
    );
    assert_eq!(
        late, None,
        "终态写失败重试期间，迟到的 running delta 仍必须被挡"
    );
    assert_eq!(
        state.runs["r1"].counters.tool_calls, 1,
        "迟到 delta 不得累加计数——sealed tombstone 必须挡住复活"
    );

    // 第 2 次：tick 扫描必须把 sealed && terminal_pending 的条目重新收进待写批次；退避
    // 窗口未到（第 1 次失败后退避 4000ms）时不该被收进，恰好到期（t=4_000）才收进；
    // writer 仍未恢复，再次失败。
    assert!(
        activity_summary_due_flushes(&state, 3_999).is_empty(),
        "第 1 次失败后的退避窗口（4000ms）未到，不该被收进重试批次"
    );
    let due = activity_summary_due_flushes(&state, 4_000);
    assert_eq!(
        due.len(),
        1,
        "sealed 但 terminal_pending 的条目，退避窗口到期后必须被 tick 扫描收进重试批次"
    );
    flush_activity_summary(
        &mut state,
        &writer,
        &failures,
        due.into_iter().next().unwrap(),
        4_000,
    );
    assert!(
        state.runs["r1"].terminal_pending,
        "第二次仍失败，pending 必须保留供下一轮重试"
    );
    assert_eq!(failures.load(Ordering::Relaxed), 2);

    // 第 3 次：writer 恢复——退避窗口到期（第 2 次失败后退避 8000ms，起点 t=4_000，
    // 到期 t=12_000）后 tick 扫描再次收进批次，这次写成功。
    assert!(
        activity_summary_due_flushes(&state, 11_999).is_empty(),
        "第 2 次失败后的退避窗口（8000ms，起点 4_000）未到，不该被收进重试批次"
    );
    let due2 = activity_summary_due_flushes(&state, 12_000);
    assert_eq!(due2.len(), 1, "退避窗口到期后必须持续被扫进重试批次");
    flush_activity_summary(
        &mut state,
        &writer,
        &failures,
        due2.into_iter().next().unwrap(),
        12_000,
    );
    assert!(
        !state.runs["r1"].terminal_pending,
        "写成功后必须清 terminal_pending，不再重试"
    );
    assert!(
        state.runs["r1"].sealed,
        "sealed 必须保持——已成功落库的终态不能被复活"
    );
    assert_eq!(
        state.runs["r1"].state, "done",
        "最终落库状态必须是 done，不能因为中途重试被改写"
    );

    // 终态成功落库后，due_flushes 不得再把它收进任何批次。
    assert!(
        activity_summary_due_flushes(&state, 999_999).is_empty(),
        "终态成功落库后不得再被扫进任何待写批次"
    );
    assert_eq!(
        failures.load(Ordering::Relaxed),
        2,
        "写失败计数必须恰好 2（第 1、2 次失败，第 3 次成功）"
    );
}

#[test]
fn drain_activity_summary_deltas_terminal_supersedes_queued_running_in_same_batch() {
    // msgfix2 U1 修单 F2①：过去 worker 主循环每收一条 delta 就单独检查一次节流到期——
    // 如果 channel 里已经排着 `[running, terminal]`（同一个 run），处理完 running 后
    // 立即扫描 `activity_summary_due_flushes` 会先把 running 发布出去（`last_flushed_at_ms`
    // 是 `None`，首次发布恒判到期），terminal 还没被看到。`drain_activity_summary_deltas`
    // 把一批已经排队的 delta 整批喂给聚合态、批内只在结尾统一交给调用方决定要不要再扫
    // 一次节流窗口——本测试断言"同 run 的 running+terminal 同批到达"时，只产生一次
    // flush，且是 terminal（state=="done"），不会有单独的 running 快照被发布出去。
    let mut state = ActivitySummaryAggregatorState::default();
    let batch = vec![
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: false,
                failed: false,
            },
        },
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    ];
    let flushes = drain_activity_summary_deltas(&mut state, batch);
    assert_eq!(
        flushes.len(),
        1,
        "同批 running+terminal 只应产生一次 flush，不能先发一次 running 再发一次 terminal"
    );
    assert_eq!(flushes[0].state, "done");
    assert_eq!(flushes[0].counters.tool_calls, 1);
    assert!(state.runs["r1"].sealed);
}

#[test]
fn drain_activity_summary_deltas_running_only_batch_never_flushes_here() {
    // 对照组：批里只有非终态 delta 时，本函数本身不产生任何 flush——节流到期的判断是
    // `activity_summary_due_flushes` 的职责（worker 循环末尾单独调用），不是本函数的。
    let mut state = ActivitySummaryAggregatorState::default();
    let batch = vec![
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: true,
                failed: false,
            },
        },
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::PermissionPrompt,
        },
    ];
    let flushes = drain_activity_summary_deltas(&mut state, batch);
    assert!(flushes.is_empty());
    assert!(state.runs["r1"].dirty);
    assert_eq!(state.runs["r1"].counters.tool_calls, 1);
    assert_eq!(state.runs["r1"].counters.mcp_calls, 1);
    assert_eq!(state.runs["r1"].counters.permission_prompts, 1);
}
