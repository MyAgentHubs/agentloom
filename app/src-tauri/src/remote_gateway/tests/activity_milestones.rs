#![cfg(test)]

use super::*;
#[test]
fn extract_tool_milestones_l0_protection_permission_prompt_never_carries_approval_content() {
    // L0 保护（设计稿 §4.1）：permission prompt 只贡献计数，approval 事件的实际内容
    // （approval_id/command/summary 等）绝不进入 activity_summary delta——反向锁死。
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    configure_activity_summary_writer_test_hook(&state, tx);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-l0",
            "sess-l0",
            crate::agent_event::AgentEvent::ApprovalRequested {
                approval_id: "appr-1".to_owned(),
                run_id: "run-l0".to_owned(),
                tool: "shell".to_owned(),
                command: "rm -rf /".to_owned(),
                summary: "danger".to_owned(),
                cwd: "/".to_owned(),
                request_kind: None,
                proposal_id: None,
            },
        ),
    );

    let delta = rx.try_recv().expect("ApprovalRequested must emit a delta");
    assert_eq!(delta.kind, ActivitySummaryDeltaKind::PermissionPrompt);
    // ActivitySummaryDeltaKind::PermissionPrompt 是无字段的枚举变体——approval_id/command/
    // summary/cwd 这些字段在类型层面就没有位置可以泄漏进来，编译期即锁死（不是运行时字符串
    // 扫描断言）。
}

#[test]
fn extract_tool_milestones_emits_activity_summary_deltas_for_tool_completed_and_terminal() {
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    configure_activity_summary_writer_test_hook(&state, tx);
    remember_tool_name(&state, "run-agg", "tool-1", "mcp__agentloom__commit");
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-agg",
            "sess-agg",
            crate::agent_event::AgentEvent::ToolCompleted {
                id: "tool-1".to_owned(),
                status: crate::agent_event::ToolStatus::Failed,
                exit_code: Some(1),
                output: None,
            },
        ),
    );
    let delta = rx.try_recv().expect("ToolCompleted must emit a delta");
    assert_eq!(delta.session_id, "sess-agg");
    assert_eq!(delta.run_id, "run-agg");
    assert_eq!(
        delta.kind,
        ActivitySummaryDeltaKind::ToolCompleted {
            mcp: true,
            failed: true,
        },
        "mcp__ 前缀工具名必须判 mcp=true，ToolStatus::Failed 必须判 failed=true"
    );

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-agg",
            "sess-agg",
            crate::agent_event::AgentEvent::RunCloseout {
                run_id: "run-agg".to_owned(),
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            },
        ),
    );
    let terminal_delta = rx
        .try_recv()
        .expect("RunCloseout must emit a terminal delta");
    assert_eq!(
        terminal_delta.kind,
        ActivitySummaryDeltaKind::Terminal { failed: false },
        "RunCloseout 是正常终态，不是失败终态"
    );
}

#[test]
fn extract_tool_milestones_error_event_emits_failed_terminal_delta() {
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    configure_activity_summary_writer_test_hook(&state, tx);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-err",
            "sess-err",
            crate::agent_event::AgentEvent::Error {
                message: "boom".to_owned(),
            },
        ),
    );
    let delta = rx.try_recv().expect("Error must emit a terminal delta");
    assert_eq!(
        delta.kind,
        ActivitySummaryDeltaKind::Terminal { failed: true }
    );
}

#[test]
fn extract_tool_milestones_member_lane_rolls_up_into_dispatch_run_id_not_lane_run_id() {
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    configure_activity_summary_writer_test_hook(&state, tx);
    remember_tool_name(&state, "member:lead-run-9:assignment-1", "tool-1", "shell");
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);

    let payload = crate::event_transport::BatchPayload {
        batches: vec![crate::event_transport::RunBatch {
            session_id: "sess-team".to_owned(),
            run_id: "member:lead-run-9:assignment-1".to_owned(),
            dispatch: Some(crate::agent_event::DispatchMeta {
                run_id: Some("lead-run-9".to_owned()),
                assignment_id: Some("assignment-1".to_owned()),
                ..Default::default()
            }),
            events: vec![crate::event_transport::SequencedEvent {
                seq: 1,
                event: crate::agent_event::AgentEvent::ToolCompleted {
                    id: "tool-1".to_owned(),
                    status: crate::agent_event::ToolStatus::Ok,
                    exit_code: Some(0),
                    output: None,
                },
            }],
        }],
    };
    enqueue_batch_payload_for_upstream(&state, &upstream_tx, &milestone_tx, payload);

    let delta = rx
        .try_recv()
        .expect("member lane ToolCompleted must emit a delta");
    assert_eq!(
        delta.run_id, "lead-run-9",
        "member lane 计数必须并入父（lead）run，不能用传输层复合 lane id"
    );
}

#[test]
fn extract_tool_milestones_member_lane_terminal_does_not_seal_parent_run() {
    // msgfix2 U1 修单 F1（spec §4.1「粒度=逻辑 run」）：team 会话 lead+2 member——member1
    // 先完成（自己的 RunCloseout）绝不能把父（lead）run 判成终态；摘要必须继续吃
    // member2/lead 自己的计数，直到 lead 自己的 batch（不带 dispatch）真正到达终态才封口
    // 一次。修复前：member 的 Completed/RunCloseout 被 `activity_summary_logical_run_id`
    // 映射成父 run 的 Terminal delta，member1 一完成就把父 run sealed，之后 member2/lead
    // 的计数被 sealed tombstone 全部丢弃。
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(16);
    configure_activity_summary_writer_test_hook(&state, tx);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(8);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(8);

    fn member_batch(
        assignment: &str,
        event: crate::agent_event::AgentEvent,
    ) -> crate::event_transport::BatchPayload {
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team".to_owned(),
                run_id: format!("member:lead-run-1:{assignment}"),
                dispatch: Some(crate::agent_event::DispatchMeta {
                    run_id: Some("lead-run-1".to_owned()),
                    assignment_id: Some(assignment.to_owned()),
                    ..Default::default()
                }),
                events: vec![crate::event_transport::SequencedEvent { seq: 1, event }],
            }],
        }
    }

    // ① member1 ToolCompleted——计数并入父 run。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        member_batch(
            "assignment-1",
            crate::agent_event::AgentEvent::ToolCompleted {
                id: "tool-1".to_owned(),
                status: crate::agent_event::ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ),
    );
    let d1 = rx
        .try_recv()
        .expect("member1 ToolCompleted must emit a delta");
    assert_eq!(d1.run_id, "lead-run-1");
    assert_eq!(
        d1.kind,
        ActivitySummaryDeltaKind::ToolCompleted {
            mcp: false,
            failed: false,
        }
    );

    // ② member1 自己的 RunCloseout（这条 member lane 结束）——核心断言：绝不能给父 run
    //    产生 Terminal delta（修复前会把父 run 提前 sealed）。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        member_batch(
            "assignment-1",
            crate::agent_event::AgentEvent::RunCloseout {
                run_id: "member:lead-run-1:assignment-1".to_owned(),
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            },
        ),
    );
    assert!(
        rx.try_recv().is_err(),
        "member lane 自己的 RunCloseout 不得给父 run 产生任何 delta（尤其不能是 Terminal）"
    );

    // ③ member2 ToolCompleted——父 run 摘要仍是 running，没有被 member1 提前封死，继续吃
    //    计数。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        member_batch(
            "assignment-2",
            crate::agent_event::AgentEvent::ToolCompleted {
                id: "tool-2".to_owned(),
                status: crate::agent_event::ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ),
    );
    let d3 = rx
        .try_recv()
        .expect("member2 ToolCompleted must still emit a delta after member1's own RunCloseout");
    assert_eq!(d3.run_id, "lead-run-1");

    // ④ lead 自己的 batch（不带 dispatch）到达终态——现在才允许封父 run。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team".to_owned(),
                run_id: "lead-run-1".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 1,
                    event: crate::agent_event::AgentEvent::Completed {
                        cost_usd: None,
                        input_tokens: None,
                        output_tokens: None,
                        final_text: None,
                        result: None,
                        run_id: Some("lead-run-1".to_owned()),
                        commit_sha: None,
                        files_changed: None,
                        insertions: None,
                        deletions: None,
                        interrupted: None,
                    },
                }],
            }],
        },
    );
    let terminal = rx
        .try_recv()
        .expect("lead 自己的 Completed 必须产生 Terminal delta 封父 run");
    assert_eq!(terminal.run_id, "lead-run-1");
    assert_eq!(
        terminal.kind,
        ActivitySummaryDeltaKind::Terminal { failed: false }
    );

    // 把以上四条 delta 按顺序喂进真实聚合态，进一步锁死"摘要仍 running 且继续吃计数、
    // 直到 lead 终态才封口一次"这条完整状态轨迹（不只是"有没有 delta"，是"状态演化对
    // 不对"）。
    let mut agg_state = ActivitySummaryAggregatorState::default();
    apply_activity_summary_delta(&mut agg_state, d1);
    assert!(!agg_state.runs["lead-run-1"].sealed);
    assert_eq!(agg_state.runs["lead-run-1"].counters.tool_calls, 1);
    apply_activity_summary_delta(&mut agg_state, d3);
    assert!(
        !agg_state.runs["lead-run-1"].sealed,
        "member1 RunCloseout 之后父 run 仍必须是 running"
    );
    assert_eq!(agg_state.runs["lead-run-1"].counters.tool_calls, 2);
    let flush = apply_activity_summary_delta(&mut agg_state, terminal)
        .expect("lead 终态必须触发一次封口 flush");
    assert_eq!(flush.state, "done");
    assert_eq!(
        flush.counters.tool_calls, 2,
        "封口摘要必须带上 member1+member2 的累计计数"
    );
    assert!(agg_state.runs["lead-run-1"].sealed);
}

#[test]
fn extract_tool_milestones_member_lane_error_does_not_seal_parent_run() {
    // msgfix2 U1 修单三（G3·独立审查残余 P2）：F1 只在 `extract_tool_milestones_member_
    // lane_terminal_does_not_seal_parent_run` 里覆盖了 `RunCloseout` 这一条终态路径——
    // `AgentEvent::Error` 是另一条独立的终态分支（见 F1 分支代码，`Completed`/
    // `RunCloseout` 和 `Error` 各自都判了一次 `batch.dispatch.is_none()`），此前没有测试
    // 直接锁死 member lane 自己的 Error 同样不得封父 run。逻辑上两个分支目前是对称实现
    // 的（都判同一个门槛），但没有测试就没有回归保护——本测试补上这条独立于 RunCloseout
    // 的红线：member1 自己的 Error 结束一条 lane，不得给父（lead）run 产生任何 delta，
    // 摘要必须继续吃 member2 的计数直到 lead 自己终态才封口。
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(16);
    configure_activity_summary_writer_test_hook(&state, tx);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(8);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(8);

    fn member_batch(
        assignment: &str,
        event: crate::agent_event::AgentEvent,
    ) -> crate::event_transport::BatchPayload {
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team-err".to_owned(),
                run_id: format!("member:lead-run-err:{assignment}"),
                dispatch: Some(crate::agent_event::DispatchMeta {
                    run_id: Some("lead-run-err".to_owned()),
                    assignment_id: Some(assignment.to_owned()),
                    ..Default::default()
                }),
                events: vec![crate::event_transport::SequencedEvent { seq: 1, event }],
            }],
        }
    }

    // ① member1 ToolCompleted——计数并入父 run（先给 run 建立"有活动"的前提，见 M0
    //    §10.11 零活动 run 不产生摘要的裁决）。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        member_batch(
            "assignment-1",
            crate::agent_event::AgentEvent::ToolCompleted {
                id: "tool-1".to_owned(),
                status: crate::agent_event::ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ),
    );
    let d1 = rx
        .try_recv()
        .expect("member1 ToolCompleted must emit a delta");
    assert_eq!(d1.run_id, "lead-run-err");

    // ② member1 自己的 Error（这条 member lane 异常结束）——核心断言：绝不能给父 run
    //    产生任何 delta（尤其不能是 Terminal），也不能因为它是"失败"就把父 run 判 failed。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        member_batch(
            "assignment-1",
            crate::agent_event::AgentEvent::Error {
                message: "member1 boom".to_owned(),
            },
        ),
    );
    assert!(
        rx.try_recv().is_err(),
        "member lane 自己的 Error 不得给父 run 产生任何 delta（尤其不能是 Terminal）"
    );

    // ③ member2 ToolCompleted——父 run 摘要仍是 running，没有被 member1 的 Error 提前
    //    封死（不管 sealed 还是 failed），继续吃计数。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        member_batch(
            "assignment-2",
            crate::agent_event::AgentEvent::ToolCompleted {
                id: "tool-2".to_owned(),
                status: crate::agent_event::ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ),
    );
    let d3 = rx
        .try_recv()
        .expect("member2 ToolCompleted must still emit a delta after member1's own Error");
    assert_eq!(d3.run_id, "lead-run-err");

    // ④ lead 自己的 batch（不带 dispatch）到达终态——现在才允许封父 run，且是正常终态
    //    （不是被 member1 的 Error 拖成 failed）。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team-err".to_owned(),
                run_id: "lead-run-err".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 1,
                    event: crate::agent_event::AgentEvent::Completed {
                        cost_usd: None,
                        input_tokens: None,
                        output_tokens: None,
                        final_text: None,
                        result: None,
                        run_id: Some("lead-run-err".to_owned()),
                        commit_sha: None,
                        files_changed: None,
                        insertions: None,
                        deletions: None,
                        interrupted: None,
                    },
                }],
            }],
        },
    );
    let terminal = rx
        .try_recv()
        .expect("lead 自己的 Completed 必须产生 Terminal delta 封父 run");
    assert_eq!(terminal.run_id, "lead-run-err");
    assert_eq!(
        terminal.kind,
        ActivitySummaryDeltaKind::Terminal { failed: false },
        "member1 的 Error 不得把父 run 的最终状态拖成 failed——父 run 的终态只看 lead 自己"
    );

    // 把以上三条 delta 按顺序喂进真实聚合态，锁死"计数继续累计、直到 lead 终态才封口一次"
    // 这条完整状态轨迹。
    let mut agg_state = ActivitySummaryAggregatorState::default();
    apply_activity_summary_delta(&mut agg_state, d1);
    apply_activity_summary_delta(&mut agg_state, d3);
    assert!(
        !agg_state.runs["lead-run-err"].sealed,
        "member1 Error 之后父 run 仍必须是 running"
    );
    assert_eq!(agg_state.runs["lead-run-err"].counters.tool_calls, 2);
    let flush = apply_activity_summary_delta(&mut agg_state, terminal)
        .expect("lead 终态必须触发一次封口 flush");
    assert_eq!(flush.state, "done");
    assert_eq!(flush.counters.tool_calls, 2);
    assert!(agg_state.runs["lead-run-err"].sealed);
}

#[test]
fn extract_tool_milestones_l0_single_source_of_truth_actionable_events_never_join_l1() {
    // msgfix2 U1 修单 F4（spec §4.1「L0 分级保护」）：聚合器对"哪些块/事件永不折进 L1"的
    // 判定改走单点函数 `is_actionable_block_type`（复用刀 1 白名单），不是另写一份隔离
    // 逻辑。反向锁死：approval/decision_card/scope_change 三个 actionable 块型都在这个
    // 单点名单里；其中 scope_change 对应的 `AgentEvent::NeedsDecision` 走聚合路径时必须
    // 零计数贡献（decision_card 本身不经过 `extract_tool_milestones`——它走
    // `lead_step::build_decision_card_block` 独立卡片路径，不是这里要挡的对象，但仍在
    // 单点名单里一并断言，保持"三个 actionable 类型是同一份名单"这件事可见）。
    assert!(is_actionable_block_type("approval"));
    assert!(is_actionable_block_type("decision_card"));
    assert!(is_actionable_block_type("scope_change"));

    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    configure_activity_summary_writer_test_hook(&state, tx);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-scope",
            "sess-scope",
            crate::agent_event::AgentEvent::NeedsDecision {
                run_id: "run-scope".to_owned(),
                reason: "scope_change".to_owned(),
                changes: vec![crate::agent_event::ScopeChange {
                    proposal_id: "prop-1".to_owned(),
                    kind: "expand".to_owned(),
                    detail_text: "add feature X".to_owned(),
                    detail_summary: None,
                }],
            },
        ),
    );

    assert!(
        rx.try_recv().is_err(),
        "scope_change 事件（NeedsDecision）绝不能给聚合器产生任何 delta——原卡走既有独立\
             通道下发，L1 摘要不是它的替代通道"
    );
}

#[test]
fn extract_tool_milestones_without_configured_writer_is_pure_noop_for_activity_summary() {
    // 未调用 configure_activity_summary_writer——activity_summary_tx 是 None，整套聚合器
    // 功能应完全不产生任何 delta（不是"产了但没人收也不报错"——是根本不产），同时既有
    // tool.completed 里程碑路径必须继续正常工作（回归保护）。
    let state = GatewayInnerState::default();
    let generation = state.advance_generation_and_set_gate(true);
    remember_tool_name(&state, "run-noagg", "tool-1", "shell");
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(4);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-noagg",
            "sess-noagg",
            crate::agent_event::AgentEvent::ToolCompleted {
                id: "tool-1".to_owned(),
                status: crate::agent_event::ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ),
    );
    let (item_generation, item) = milestone_rx
        .try_recv()
        .expect("tool.completed 里程碑必须不受影响照常发出");
    assert_eq!(item_generation, generation);
    assert_eq!(item.t, "tool.completed");
    assert_eq!(state.activity_summary_dropped.load(Ordering::Relaxed), 0);
}
