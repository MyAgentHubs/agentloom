#![cfg(test)]

use super::*;
// ========================================================================================
// msgfix2 U1（设计稿 v4.1 §4.1）：L1 活动摘要聚合器。
// ========================================================================================

#[test]
fn activity_summary_logical_run_id_uses_dispatch_run_id_when_present_else_batch_run_id() {
    // member lane：RunBatch.run_id 是传输层复合 lane id，dispatch.run_id 才是真正的逻辑
    // （lead）run。
    let member_batch = crate::event_transport::RunBatch {
        session_id: "s".to_owned(),
        run_id: "member:lead-run-1:assignment-1".to_owned(),
        dispatch: Some(crate::agent_event::DispatchMeta {
            run_id: Some("lead-run-1".to_owned()),
            ..Default::default()
        }),
        events: vec![],
    };
    assert_eq!(activity_summary_logical_run_id(&member_batch), "lead-run-1");

    // lead/solo 自己的 lane：不带 dispatch，batch.run_id 本身就是真实 run_id。
    let lead_batch = crate::event_transport::RunBatch {
        session_id: "s".to_owned(),
        run_id: "lead-run-1".to_owned(),
        dispatch: None,
        events: vec![],
    };
    assert_eq!(activity_summary_logical_run_id(&lead_batch), "lead-run-1");
}

#[test]
fn apply_activity_summary_delta_accumulates_counts_and_flags_dirty_without_flushing() {
    let mut state = ActivitySummaryAggregatorState::default();
    let flush = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: true,
                failed: false,
            },
        },
    );
    assert_eq!(flush, None, "非终态 delta 本身不触发立即写——节流留给 tick");
    let entry = &state.runs["r1"];
    assert_eq!(entry.counters.tool_calls, 1);
    assert_eq!(entry.counters.mcp_calls, 1);
    assert_eq!(entry.counters.failed, 0);
    assert!(entry.dirty);
    assert_eq!(entry.state, "running");

    apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::ToolCompleted {
                mcp: false,
                failed: true,
            },
        },
    );
    apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::PermissionPrompt,
        },
    );
    let entry = &state.runs["r1"];
    assert_eq!(entry.counters.tool_calls, 2);
    assert_eq!(entry.counters.failed, 1);
    assert_eq!(entry.counters.mcp_calls, 1);
    assert_eq!(entry.counters.permission_prompts, 1);
}

#[test]
fn apply_activity_summary_delta_terminal_seals_immediately_and_ignores_later_deltas() {
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

    let flush = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    )
    .expect("终态 delta 必须立即返回待写快照——设计稿「终态写取消/压过排队中的节流更新」");
    assert_eq!(flush.state, "done");
    assert_eq!(flush.counters.tool_calls, 1);
    assert!(state.runs["r1"].sealed);
    assert!(
        !state.runs["r1"].dirty,
        "终态写不受节流约束，落地即清 dirty"
    );

    // 终态置位后到达的 running 更新一律丢弃——不是"忽略并保留旧终态"这么简单，是根本不
    // 再累积计数（防止一个 run 已经报告完结后又冒出的迟到事件把摘要悄悄改回"进行中"）。
    let ignored = apply_activity_summary_delta(
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
    assert_eq!(ignored, None);
    assert_eq!(
        state.runs["r1"].counters.tool_calls, 1,
        "sealed 后的 delta 不得再累积计数"
    );

    // 重复终态（例如同一 run 两个不同来源都上报了终态事件）同样被忽略，不重复触发写。
    let duplicate_terminal = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: true },
        },
    );
    assert_eq!(
        duplicate_terminal, None,
        "已 sealed 的终态不得被覆盖/重复触发"
    );
    assert_eq!(
        state.runs["r1"].state, "done",
        "state 不能被后来的重复终态改写"
    );
}

#[test]
fn apply_activity_summary_delta_terminal_error_seals_failed() {
    let mut state = ActivitySummaryAggregatorState::default();
    apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::PermissionPrompt,
        },
    );
    let flush = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: true },
        },
    )
    .unwrap();
    assert_eq!(flush.state, "failed");
}

#[test]
fn apply_activity_summary_delta_terminal_for_unknown_run_is_noop() {
    // msgfix2 U1 修单 F3 裁决（M0 远程控制协议 §10.11）：
    // 一个从未见过任何 ToolCompleted/ApprovalRequested 的 run（零活动）直接收到
    // 终态（例如零工具调用的极短 run）——这不是"该丢的实现细节"，是 M0 §10.11 条文本身
    // 的契约：activity_summary 只在该 run 有 ≥1 条被计数活动时才创建，零活动 run 不产生
    // 摘要 = 正确行为，不是丢失。旧条文措辞「每个逻辑 run 恰好一条」与本实现矛盾——裁决
    // 已把条文改向「每个有活动的逻辑 run 至多/恰好一条·零活动 run 不产生」，这个测试锁的
    // 是修正后的条文语义，不是待修的 bug。
    let mut state = ActivitySummaryAggregatorState::default();
    let flush = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "never-seen".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    );
    assert_eq!(flush, None, "零活动 run 终态必须是 no-op，不产生摘要");
    assert!(state.runs.is_empty());
}

#[test]
fn activity_summary_zero_activity_run_no_summary_versus_active_run_must_seal_m0_10_11() {
    // msgfix2 U1 修单 F3：把裁决锁成正反一对（M0 §10.11 措辞修正版）——
    // 反向：零活动 run 终态 = 无摘要无报错；正向：有过 ≥1 条计数活动的 run 终态必须恰好
    // 封口一次。两条断言合在同一个测试里，直接对应设计裁决原文的两半，避免以后有人只
    // 改了实现的一半就让测试悄悄绿掉。
    let mut state = ActivitySummaryAggregatorState::default();

    let noop = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "zero-activity-run".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    );
    assert_eq!(noop, None, "零活动 run 终态必须是 no-op，不产生摘要");
    assert!(!state.runs.contains_key("zero-activity-run"));

    apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "active-run".to_owned(),
            kind: ActivitySummaryDeltaKind::PermissionPrompt,
        },
    );
    let sealed = apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "active-run".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    )
    .expect("有过 ≥1 条计数活动的 run 终态必须恰好封口一次");
    assert_eq!(sealed.state, "done");
    assert!(state.runs["active-run"].sealed);
}

#[test]
fn activity_summary_due_flushes_respects_two_second_throttle_and_never_publishes_first_time() {
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

    // 从未尝试过写（last_attempt_at_ms=None）——即便 now_ms=0，也立即算"到期"，不必等 2s。
    let due = activity_summary_due_flushes(&state, 0);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].run_id, "r1");

    // 模拟刚刚成功写过一次：last_flushed_at_ms/last_attempt_at_ms = 1_000，无连续失败。
    state.runs.get_mut("r1").unwrap().last_flushed_at_ms = Some(1_000);
    state.runs.get_mut("r1").unwrap().last_attempt_at_ms = Some(1_000);
    state.runs.get_mut("r1").unwrap().dirty = false;
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
    assert!(state.runs["r1"].dirty);

    // 距上次发布仅 1.5s——节流窗口未到，不该出现在待写批次里。
    assert!(activity_summary_due_flushes(&state, 2_500).is_empty());
    // 距上次发布恰好 2.0s——到期。
    assert_eq!(activity_summary_due_flushes(&state, 3_000).len(), 1);

    // 不 dirty 的 run 即便节流窗口已过，也不该被收进待写批次（没有变化，没什么可发的）。
    state.runs.get_mut("r1").unwrap().dirty = false;
    assert!(activity_summary_due_flushes(&state, 10_000).is_empty());
}

#[test]
fn activity_summary_due_flushes_excludes_sealed_runs_once_terminal_write_confirmed() {
    // msgfix2 U1 修单三（G1）修正版：旧版本断言"sealed 就绝不进 tick 扫描"——这句话在
    // 终态**已经成功落库**之后才成立；终态刚到达、尚未确认写成功（`terminal_pending`
    // 仍是 true）的窗口期，sealed 的条目必须能被 tick 扫描收进重试批次（见下一个测试
    // `activity_summary_due_flushes_includes_sealed_run_with_pending_terminal_write`），
    // 否则就是独立审查 G1 抓到的那个"终态写失败永不重试"缺口。本测试只锁"写已确认成功
    // 后"这一半：即便手动把 dirty 重新标脏，也绝不再进 tick 扫描——终态只该被写一次。
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
    apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    );
    // 模拟"终态写已经确认成功"（真实场景下由 `flush_activity_summary` 清掉）。
    state.runs.get_mut("r1").unwrap().terminal_pending = false;
    // Terminal 分支已经把 dirty 清掉，但即便手动重新标脏，已确认写成功的 sealed run 也
    // 绝不再进 tick 扫描——终态只该被写一次。
    state.runs.get_mut("r1").unwrap().dirty = true;
    assert!(activity_summary_due_flushes(&state, 999_999).is_empty());
}

#[test]
fn activity_summary_due_flushes_includes_sealed_run_with_pending_terminal_write() {
    // msgfix2 U1 修单三（G1·独立审查 P1）：终态一到达就 sealed，但如果这次落库还没有
    // 被确认成功（`terminal_pending` 仍是 true——例如 immediate attempt 刚失败过），
    // tick 扫描必须仍然把它收进待写批次，否则该 run 永远停在"内存已 sealed、DB 仍是
    // running"的状态，违反 M0「有活动 run 终态必封口恰好一次」。
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
    apply_activity_summary_delta(
        &mut state,
        ActivitySummaryDelta {
            session_id: "s1".to_owned(),
            run_id: "r1".to_owned(),
            kind: ActivitySummaryDeltaKind::Terminal { failed: false },
        },
    );
    assert!(state.runs["r1"].sealed);
    assert!(
        state.runs["r1"].terminal_pending,
        "终态到达即置 pending，直到写成功才清"
    );
    let due = activity_summary_due_flushes(&state, 999_999);
    assert_eq!(
        due.len(),
        1,
        "sealed 但 terminal_pending 的条目必须被 tick 扫描收进重试批次"
    );
    assert_eq!(due[0].run_id, "r1");
    assert_eq!(due[0].state, "done");
}

#[test]
fn activity_summary_due_flushes_backs_off_exponentially_after_repeated_failures_and_caps_at_30s() {
    // R6③（msgfix2 整盘审 P2 顺手）：旧实现只看 `last_flushed_at_ms`（只在成功时推进）——
    // 一个从未成功过的条目（比如目标 DB 一直忙）该字段恒 `None`，`activity_summary_due_
    // flushes` 的节流过滤 `last_flushed_at_ms.map_or(true, ..)` 让它每个 tick
    // （`ACTIVITY_SUMMARY_TICK_MS`=250ms，4Hz）都判定"到期"，写线程对着注定失败的目标
    // 原地空转重试。修复后退避窗口按 `ACTIVITY_SUMMARY_THROTTLE_MS * 2^consecutive_
    // failures` 增长、以 `last_attempt_at_ms`（每次真正尝试写都推进，无论成败）为起点，
    // 上限 `ACTIVITY_SUMMARY_MAX_BACKOFF_MS`（30s）。
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
            consecutive_failures: 3,
            last_attempt_at_ms: Some(10_000),
        },
    );

    // 第 3 次连续失败后的退避窗口 = 2000 * 2^3 = 16_000ms，起点 10_000。
    assert!(
        activity_summary_due_flushes(&state, 10_000 + 15_999)
            .iter()
            .all(|f| f.run_id != "r1"),
        "退避窗口内（<16s）不该被再次收进待写批次"
    );
    assert_eq!(
        activity_summary_due_flushes(&state, 10_000 + 16_000).len(),
        1,
        "退避窗口恰好到期必须重新被收进待写批次"
    );

    // 大量连续失败必须封顶在 ACTIVITY_SUMMARY_MAX_BACKOFF_MS（30s），不会因为失败次数
    // 继续攀升就无限拉长下一次重试的等待时间。
    state.runs.get_mut("r1").unwrap().consecutive_failures = 20;
    state.runs.get_mut("r1").unwrap().last_attempt_at_ms = Some(10_000);
    assert!(
        activity_summary_due_flushes(&state, 10_000 + ACTIVITY_SUMMARY_MAX_BACKOFF_MS - 1)
            .is_empty(),
        "退避窗口封顶前一毫秒仍不该到期"
    );
    assert_eq!(
        activity_summary_due_flushes(&state, 10_000 + ACTIVITY_SUMMARY_MAX_BACKOFF_MS).len(),
        1,
        "退避窗口必须封顶在 30s，不会因为失败次数继续无限增长"
    );

    // 从未失败过（consecutive_failures==0）仍沿用既有 2s 常规节流，不受本次改动影响。
    state.runs.get_mut("r1").unwrap().consecutive_failures = 0;
    state.runs.get_mut("r1").unwrap().last_attempt_at_ms = Some(10_000);
    assert!(
        activity_summary_due_flushes(&state, 10_000 + ACTIVITY_SUMMARY_THROTTLE_MS - 1).is_empty(),
        "无失败时仍是 2s 常规节流，不该提前到期"
    );
    assert_eq!(
        activity_summary_due_flushes(&state, 10_000 + ACTIVITY_SUMMARY_THROTTLE_MS).len(),
        1,
        "无失败时 2s 节流窗口到期必须正常收进待写批次"
    );
}
