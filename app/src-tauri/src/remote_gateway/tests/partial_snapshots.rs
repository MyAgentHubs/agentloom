#![cfg(test)]

use super::*;
// ---- P0-b：maintain_partial_snapshots（sink 归约态维护，同层 extract_tool_milestones）----

#[test]
fn partial_snapshot_accumulates_across_multiple_sink_calls_and_tracks_last_seq() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(2);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(2);

    // 第一次 sink 调用（模拟一轮 drain tick 的 coalesce 批）。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-p1".to_owned(),
                run_id: "run-p1".to_owned(),
                dispatch: None,
                events: vec![
                    crate::event_transport::SequencedEvent {
                        seq: 3,
                        event: AgentEvent::TextDelta {
                            text: "hello ".to_owned(),
                        },
                    },
                    crate::event_transport::SequencedEvent {
                        seq: 7,
                        event: AgentEvent::TextDelta {
                            text: "world".to_owned(),
                        },
                    },
                ],
            }],
        },
    );
    // 第二次 sink 调用（下一轮 tick）——归约态必须跨调用持续累积，不是每次重建。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-p1".to_owned(),
                run_id: "run-p1".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 9,
                    event: AgentEvent::TextDelta {
                        text: "!".to_owned(),
                    },
                }],
            }],
        },
    );

    let snapshots = lock(&state.partial_snapshots);
    let entry = snapshots
        .get("sess-p1")
        .expect("entry must exist after two sink calls");
    assert_eq!(entry.run_id, "run-p1");
    assert_eq!(
        entry.last_seq, 9,
        "through_run_seq 水位必须是 sink 看到的最后一条 seq"
    );
    assert_eq!(
        entry.reducer.snapshot_blocks(),
        vec![crate::db::Block::Text {
            text: "hello world!".to_owned()
        }]
    );
}

#[test]
fn partial_snapshot_rebuilds_reducer_when_run_id_changes_for_same_session() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(2);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(2);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-old",
            "sess-p2",
            AgentEvent::TextDelta {
                text: "stale".to_owned(),
            },
        ),
    );
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-p2".to_owned(),
                run_id: "run-new".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 4,
                    event: AgentEvent::TextDelta {
                        text: "fresh".to_owned(),
                    },
                }],
            }],
        },
    );

    let snapshots = lock(&state.partial_snapshots);
    let entry = snapshots.get("sess-p2").unwrap();
    assert_eq!(entry.run_id, "run-new");
    assert_eq!(entry.last_seq, 4);
    assert_eq!(
        entry.reducer.snapshot_blocks(),
        vec![crate::db::Block::Text {
            text: "fresh".to_owned()
        }],
        "旧 run 的归约态必须被整个丢弃重建，不能跟新 run 的内容合并"
    );
}

#[test]
fn partial_snapshot_cleared_when_completed_or_run_closeout_arrives() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![
                crate::event_transport::RunBatch {
                    session_id: "sess-completed".to_owned(),
                    run_id: "run-completed".to_owned(),
                    dispatch: None,
                    events: vec![
                        crate::event_transport::SequencedEvent {
                            seq: 1,
                            event: AgentEvent::TextDelta {
                                text: "hi".to_owned(),
                            },
                        },
                        crate::event_transport::SequencedEvent {
                            seq: 2,
                            event: AgentEvent::Completed {
                                cost_usd: None,
                                input_tokens: None,
                                output_tokens: None,
                                final_text: None,
                                result: None,
                                run_id: None,
                                commit_sha: None,
                                files_changed: None,
                                insertions: None,
                                deletions: None,
                                interrupted: None,
                            },
                        },
                    ],
                },
                crate::event_transport::RunBatch {
                    session_id: "sess-closeout".to_owned(),
                    run_id: "run-closeout".to_owned(),
                    dispatch: None,
                    events: vec![
                        crate::event_transport::SequencedEvent {
                            seq: 1,
                            event: AgentEvent::TextDelta {
                                text: "hi".to_owned(),
                            },
                        },
                        crate::event_transport::SequencedEvent {
                            seq: 2,
                            event: AgentEvent::RunCloseout {
                                run_id: "run-closeout".to_owned(),
                                commit_sha: None,
                                files_changed: None,
                                insertions: None,
                                deletions: None,
                                interrupted: None,
                            },
                        },
                    ],
                },
                crate::event_transport::RunBatch {
                    session_id: "sess-active".to_owned(),
                    run_id: "run-active".to_owned(),
                    dispatch: None,
                    events: vec![crate::event_transport::SequencedEvent {
                        seq: 1,
                        event: AgentEvent::TextDelta {
                            text: "still going".to_owned(),
                        },
                    }],
                },
            ],
        },
    );

    let snapshots = lock(&state.partial_snapshots);
    assert!(
        !snapshots.contains_key("sess-completed"),
        "Completed 必须清掉该 session 的 partial 条目——下次 snapshot 回退到 idle"
    );
    assert!(
        !snapshots.contains_key("sess-closeout"),
        "RunCloseout 必须清掉该 session 的 partial 条目"
    );
    assert!(
        snapshots.contains_key("sess-active"),
        "仍在跑的其它 session 不受影响"
    );
}

// ---- 缺口⑥（msgfix2 U2）：team 多 lane——保 lead/latest 活跃 lane，member 终态不清 ----

#[test]
fn partial_snapshot_foreign_lane_terminal_does_not_clear_current_lane() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(2);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(2);

    // lead 的 run 先声明该 session 的 partial 槽位。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-lead",
            "sess-team",
            AgentEvent::TextDelta {
                text: "lead progress".to_owned(),
            },
        ),
    );

    // member lane（不同 run_id，同一 session_id）单独一条 batch 携带自己的终态事件。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team".to_owned(),
                run_id: "run-member".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 1,
                    event: AgentEvent::Completed {
                        cost_usd: None,
                        input_tokens: None,
                        output_tokens: None,
                        final_text: None,
                        result: None,
                        run_id: None,
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

    let snapshots = lock(&state.partial_snapshots);
    let entry = snapshots
        .get("sess-team")
        .expect("member lane 的终态不属于当前占用槽位的 run——不得清掉 lead 的 partial 条目");
    assert_eq!(
        entry.run_id, "run-lead",
        "槽位占用者必须仍是 lead，未被 member 的终态 batch 触碰"
    );
    assert_eq!(
        entry.last_seq, 1,
        "lead 的归约态不应被 member 的 batch 推进"
    );
    assert_eq!(
        entry.reducer.snapshot_blocks(),
        vec![crate::db::Block::Text {
            text: "lead progress".to_owned()
        }],
        "lead 自己的归约内容必须原样保留，不被 member 的终态 batch 覆盖或清空"
    );
}

#[test]
fn partial_snapshot_own_lane_terminal_still_clears_normally() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(2);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(2);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-lead",
            "sess-team-2",
            AgentEvent::TextDelta {
                text: "lead progress".to_owned(),
            },
        ),
    );
    assert!(
        lock(&state.partial_snapshots).contains_key("sess-team-2"),
        "前置：lead 的非终态事件必须先建立 partial 条目"
    );

    // 同一个 run_id（lead 自己）的终态——必须正常清空，缺口⑥的收窄只保护"别的 lane"。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team-2".to_owned(),
                run_id: "run-lead".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 2,
                    event: AgentEvent::RunCloseout {
                        run_id: "run-lead".to_owned(),
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

    assert!(
        !lock(&state.partial_snapshots).contains_key("sess-team-2"),
        "lead 自身的终态必须正常清掉该 session 的 partial 条目"
    );
}

#[test]
fn partial_snapshot_member_lane_streaming_batch_does_not_seize_or_wipe_lead_slot() {
    use crate::agent_event::AgentEvent;

    // R3（缺口⑥扩展·msgfix2 整盘审 P1）：完整轨迹——lead 事件先占住槽位 → member lane
    // 的非终态（流式）事件穿插到达（裸 run_id 是复合 lane id，跟占用者不同，但
    // `dispatch.run_id` 折回 lead 的真实 run_id）→ member lane 自己的终态紧随其后。修复
    // 前：member 的流式事件命中旧 `needs_rebuild`（只比较裸 run_id）、把整个槽位重建成
    // member 自己的归约态，lead 的内容当场丢失；member 终态随后到达时，占用者已经变成
    // member 自己（裸 run_id 相等），缺口⑥那道"occupant != batch.run_id"终态守卫反而
    // 不成立，正常清空——两步绕开同一道防线。修复后：member 的流式 + 终态 batch 全部对
    // lead 的槽位完全不生效，lead 的归约态必须原样在。
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);

    // ① lead 事件先占住 sess-team-3 的槽位。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-lead",
            "sess-team-3",
            AgentEvent::TextDelta {
                text: "lead progress".to_owned(),
            },
        ),
    );

    // ② member lane 的非终态流式事件穿插——裸 run_id 是复合 lane id，`dispatch.run_id`
    // 折回 lead 的真实 run_id。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team-3".to_owned(),
                run_id: "member:run-lead:assignment-1".to_owned(),
                dispatch: Some(crate::agent_event::DispatchMeta {
                    run_id: Some("run-lead".to_owned()),
                    assignment_id: Some("assignment-1".to_owned()),
                    ..Default::default()
                }),
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 1,
                    event: AgentEvent::TextDelta {
                        text: "member progress should not appear".to_owned(),
                    },
                }],
            }],
        },
    );

    {
        let snapshots = lock(&state.partial_snapshots);
        let entry = snapshots
            .get("sess-team-3")
            .expect("member lane 的流式事件不得清掉/重建占用者的槽位");
        assert_eq!(
            entry.run_id, "run-lead",
            "槽位占用者必须仍是 lead，未被 member 的流式 batch 顶替"
        );
        assert_eq!(
            entry.last_seq, 1,
            "lead 的归约态水位不应被 member 的流式 batch 推进"
        );
        assert_eq!(
            entry.reducer.snapshot_blocks(),
            vec![crate::db::Block::Text {
                text: "lead progress".to_owned()
            }],
            "lead 自己的归约内容必须原样保留，不被 member 的流式事件混入/覆盖"
        );
    }

    // ③ member lane 自己的终态紧随其后——同一条子 lane，同样不得触碰占用者。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-team-3".to_owned(),
                run_id: "member:run-lead:assignment-1".to_owned(),
                dispatch: Some(crate::agent_event::DispatchMeta {
                    run_id: Some("run-lead".to_owned()),
                    assignment_id: Some("assignment-1".to_owned()),
                    ..Default::default()
                }),
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 2,
                    event: AgentEvent::Completed {
                        cost_usd: None,
                        input_tokens: None,
                        output_tokens: None,
                        final_text: None,
                        result: None,
                        run_id: None,
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

    let snapshots = lock(&state.partial_snapshots);
    let entry = snapshots
        .get("sess-team-3")
        .expect("member lane 自己的终态也不得清掉占用者的槽位");
    assert_eq!(entry.run_id, "run-lead");
    assert_eq!(
        entry.last_seq, 1,
        "lead 的水位不应被 member 的终态 batch 推进"
    );
    assert_eq!(
        entry.reducer.snapshot_blocks(),
        vec![crate::db::Block::Text {
            text: "lead progress".to_owned()
        }],
        "member 终态 batch 之后，lead partial 内容必须原样在"
    );
}

// P0-b 返工①【阻断修复】：gate 关闭（桌面断连）期间归约态必须照常推进/清理——这是修复
// 项①的核心回归测试；把 `maintain_partial_snapshots` 挪回 gate 判断之后会让这条测试变红
// （变异自证，见任务书硬约束）。
#[test]
fn maintain_partial_snapshots_runs_even_when_upstream_gate_is_closed() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    assert!(
        !state.upstream_enabled_snapshot(),
        "前置：GatewayInnerState::default() 的 gate 必须是关闭的"
    );
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    // 断连期间喂一条非终态事件——归约态必须照常推进，即使 gate 关闭、事件不入上行队列。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-disconnected".to_owned(),
                run_id: "run-disc".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 3,
                    event: AgentEvent::TextDelta {
                        text: "offline work".to_owned(),
                    },
                }],
            }],
        },
    );
    {
        let snapshots = lock(&state.partial_snapshots);
        let entry = snapshots
            .get("sess-disconnected")
            .expect("gate 关闭也必须维护归约态——否则重连后 control.snapshot 会回假 idle/陈旧态");
        assert_eq!(entry.last_seq, 3);
        assert_eq!(
            entry.reducer.snapshot_blocks(),
            vec![crate::db::Block::Text {
                text: "offline work".to_owned()
            }]
        );
    }
    assert!(
        upstream_rx.try_recv().is_err(),
        "gate 关闭时事件不得入上行队列"
    );
    assert!(milestone_rx.try_recv().is_err(), "gate 关闭时不产生里程碑");

    // 断连期间也喂一条 Completed 终态——归约态必须照常清理，不能卡在断连窗内永久泄漏。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-disconnected".to_owned(),
                run_id: "run-disc".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 4,
                    event: AgentEvent::Completed {
                        cost_usd: None,
                        input_tokens: None,
                        output_tokens: None,
                        final_text: None,
                        result: None,
                        run_id: None,
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
    assert!(
        !lock(&state.partial_snapshots).contains_key("sess-disconnected"),
        "gate 关闭时 Completed 也必须清理归约态条目，不能永久泄漏"
    );

    // 重连开 gate 后，归约态维护继续正常工作（不是被"卡死"）。
    state.advance_generation_and_set_gate(true);
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-after-reconnect",
            "sess-after-reconnect",
            AgentEvent::TextDelta {
                text: "back online".to_owned(),
            },
        ),
    );
    assert!(
        lock(&state.partial_snapshots).contains_key("sess-after-reconnect"),
        "重连开 gate 后归约态维护必须继续正常工作"
    );
}

#[test]
fn extract_tool_milestones_runs_even_when_upstream_gate_is_closed_and_can_seal_terminal() {
    // R4（msgfix2 整盘审 P1）：`extract_tool_milestones`（L1 聚合器 delta 唯一产地）挪到
    // gate 判断之前，与 `maintain_partial_snapshots_runs_even_when_upstream_gate_is_closed`
    // 同一姿势的回归测试——旧实现把整个函数挂在 gate 判断之后，手机没连（gate 关闭）期间
    // `ToolCompleted`/`Completed` 之类事件从未走到这里，聚合器永远拿不到计数/终态信号，
    // 活动摘要永久卡在 running。本测试锁：gate 关闭期间 ToolCompleted 仍产计数 delta、
    // Completed 仍产 Terminal delta 并成功封口——不依赖任何上行连接。
    let state = GatewayInnerState::default();
    assert!(
        !state.upstream_enabled_snapshot(),
        "前置：GatewayInnerState::default() 的 gate 必须是关闭的"
    );
    let (tx, rx) = mpsc::sync_channel::<ActivitySummaryDelta>(8);
    configure_activity_summary_writer_test_hook(&state, tx);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-gate-closed".to_owned(),
                run_id: "run-gate-closed".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 1,
                    event: crate::agent_event::AgentEvent::ToolStarted {
                        id: "tool-1".to_owned(),
                        tool: "shell".to_owned(),
                        summary: "run command".to_owned(),
                        card: crate::agent_event::CardKind::Command,
                    },
                }],
            }],
        },
    );
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-gate-closed".to_owned(),
                run_id: "run-gate-closed".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 2,
                    event: crate::agent_event::AgentEvent::ToolCompleted {
                        id: "tool-1".to_owned(),
                        status: crate::agent_event::ToolStatus::Ok,
                        exit_code: Some(0),
                        output: None,
                    },
                }],
            }],
        },
    );
    let counted = rx
        .try_recv()
        .expect("gate 关闭也必须产出 ToolCompleted 计数 delta——聚合器不该被上行连接门控");
    assert!(matches!(
        counted.kind,
        ActivitySummaryDeltaKind::ToolCompleted { .. }
    ));

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-gate-closed".to_owned(),
                run_id: "run-gate-closed".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 3,
                    event: crate::agent_event::AgentEvent::Completed {
                        cost_usd: None,
                        input_tokens: None,
                        output_tokens: None,
                        final_text: None,
                        result: None,
                        run_id: None,
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
        .expect("gate 关闭也必须产出 Terminal delta——不能永久卡在 running");
    assert!(matches!(
        terminal.kind,
        ActivitySummaryDeltaKind::Terminal { failed: false }
    ));
}

#[test]
fn partial_snapshot_capacity_rejects_new_session_but_keeps_updating_existing_ones() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

    for index in 0..PARTIAL_SNAPSHOT_CAPACITY {
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            single_event_payload(
                &format!("run-{index}"),
                &format!("sess-{index}"),
                AgentEvent::TextDelta {
                    text: "x".to_owned(),
                },
            ),
        );
    }
    assert_eq!(
        lock(&state.partial_snapshots).len(),
        PARTIAL_SNAPSHOT_CAPACITY
    );

    // 超限：新 session 必须被拒收，表大小不越界。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-overflow",
            "sess-overflow",
            AgentEvent::TextDelta {
                text: "y".to_owned(),
            },
        ),
    );
    {
        let snapshots_after_overflow = lock(&state.partial_snapshots);
        assert_eq!(
            snapshots_after_overflow.len(),
            PARTIAL_SNAPSHOT_CAPACITY,
            "满表时新 session 必须被拒收，不能越界增长"
        );
        assert!(!snapshots_after_overflow.contains_key("sess-overflow"));
    }
    // P0-b 返工⑤：拒收计原子计数器（原为 eprintln! 无界刷屏）。
    assert_eq!(
        state
            .partial_snapshot_capacity_dropped
            .load(Ordering::Relaxed),
        1
    );

    // 满表状态下，已有 session 仍必须能正常更新（只拒收*新*键，不冻结旧键）。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-0".to_owned(),
                run_id: "run-0".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 5,
                    event: AgentEvent::TextDelta {
                        text: " more".to_owned(),
                    },
                }],
            }],
        },
    );
    let snapshots_final = lock(&state.partial_snapshots);
    assert_eq!(snapshots_final.len(), PARTIAL_SNAPSHOT_CAPACITY);
    let entry0 = snapshots_final.get("sess-0").unwrap();
    assert_eq!(entry0.last_seq, 5);
    assert_eq!(
        entry0.reducer.snapshot_blocks(),
        vec![crate::db::Block::Text {
            text: "x more".to_owned()
        }]
    );
}
