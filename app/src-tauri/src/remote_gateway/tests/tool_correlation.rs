#![cfg(test)]

use super::*;
#[test]
fn tool_started_name_survives_generation_change_and_completed_removes_it() {
    use crate::agent_event::{AgentEvent, CardKind, ToolStatus};

    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([43_u8; 32]);
    let state = GatewayInnerState::default();
    // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是 tool 名字关联跨代号存活
    // + completed 清理关联表），必须配一个 active repo，不然 "sess-1" 会被 fail-closed
    // 挡下。
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let generation_a = state.advance_generation_and_set_gate(true);
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (milestone_tx_a, milestone_rx_a) = mpsc::sync_channel(2);
    assert_eq!(generation_a, 1);
    enqueue_batch_payload_for_upstream(
        &state,
        &started_tx,
        &milestone_tx_a,
        single_event_payload(
            "run-1",
            "sess-1",
            AgentEvent::ToolStarted {
                id: "tool-1".to_owned(),
                tool: "shell".to_owned(),
                summary: "run command".to_owned(),
                card: CardKind::Command,
            },
        ),
    );
    let (addr_a, server_a) = spawn_discarding_server();
    let (mut socket_a, _) = connect_with_config(format!("ws://{addr_a}"), None, 3).unwrap();
    drain_upstream(
        &mut socket_a,
        &state,
        &started_rx,
        &milestone_rx_a,
        Some(&k_room),
        room,
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();
    assert_eq!(state.frames_sent.load(Ordering::Relaxed), 0);
    drop(socket_a);
    server_a.join().unwrap();

    state.disable_upstream_gate();
    let generation_b = state.advance_generation_and_set_gate(true);
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let (milestone_tx_b, milestone_rx_b) = mpsc::sync_channel(2);
    assert_eq!(generation_b, 2);
    enqueue_batch_payload_for_upstream(
        &state,
        &completed_tx,
        &milestone_tx_b,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-1".to_owned(),
                run_id: "run-1".to_owned(),
                dispatch: None,
                events: vec![
                    crate::event_transport::SequencedEvent {
                        seq: 2,
                        event: AgentEvent::ToolCompleted {
                            id: "tool-1".to_owned(),
                            status: ToolStatus::Ok,
                            exit_code: Some(0),
                            output: Some("done".to_owned()),
                        },
                    },
                    crate::event_transport::SequencedEvent {
                        seq: 3,
                        event: AgentEvent::ToolCompleted {
                            id: "unknown".to_owned(),
                            status: ToolStatus::Failed,
                            exit_code: None,
                            output: None,
                        },
                    },
                ],
            }],
        },
    );
    let (addr_b, frames, server_b) = spawn_recording_server(2);
    let (mut socket_b, _) = connect_with_config(format!("ws://{addr_b}"), None, 3).unwrap();
    drain_upstream(
        &mut socket_b,
        &state,
        &completed_rx,
        &milestone_rx_b,
        Some(&k_room),
        room,
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let known_envelope = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(known_envelope["kind"], "event");
    assert_eq!(
        known_envelope["client_msg_id"],
        "d0c166ac-91e7-53b3-a992-73d8f4246a0e"
    );
    assert_eq!(known_envelope["seq"], Value::Null);
    let known = open_upstream_envelope(&k_room, &known_envelope);
    assert_eq!(known["t"], "tool.completed");
    assert_eq!(known["tool"], "shell");
    assert_eq!(known["status"], "ok");
    assert_eq!(known["exit_code"], 0);
    assert_eq!(known["output"], "done");

    let unknown = open_upstream_envelope(
        &k_room,
        &frames.recv_timeout(Duration::from_secs(2)).unwrap(),
    );
    assert_eq!(unknown["tool"], "");
    assert_eq!(unknown["status"], "failed");
    assert_eq!(unknown["exit_code"], Value::Null);
    assert_eq!(unknown["output"], Value::Null);
    let correlation = lock(&state.tool_correlation);
    assert!(correlation.names.is_empty());
    assert!(correlation.order.is_empty());
    drop(correlation);
    drop(socket_b);
    server_b.join().unwrap();
}

#[test]
fn tool_correlation_capacity_evicts_oldest_orphan_and_admits_new_key() {
    let state = GatewayInnerState::default();
    for index in 0..TOOL_CORRELATION_CAPACITY {
        remember_tool_name(
            &state,
            "run-x",
            &format!("tool-{index}"),
            &format!("name-{index}"),
        );
    }
    remember_tool_name(&state, "run-x", "overflow", "must-not-drop");

    assert_eq!(
        lock(&state.tool_correlation).names.len(),
        TOOL_CORRELATION_CAPACITY
    );
    assert_eq!(state.tool_correlation_dropped.load(Ordering::Relaxed), 1);
    assert_eq!(take_tool_name(&state, "run-x", "tool-0"), "");
    assert_eq!(take_tool_name(&state, "run-x", "overflow"), "must-not-drop");
}

#[test]
fn tool_correlation_key_keeps_reused_tool_id_isolated_by_run() {
    let state = GatewayInnerState::default();
    remember_tool_name(&state, "run-a", "tool-1", "shell-a");
    remember_tool_name(&state, "run-b", "tool-1", "shell-b");

    assert_eq!(take_tool_name(&state, "run-a", "tool-1"), "shell-a");
    assert_eq!(take_tool_name(&state, "run-b", "tool-1"), "shell-b");
    let correlation = lock(&state.tool_correlation);
    assert!(correlation.names.is_empty());
    assert!(correlation.order.is_empty());
}

#[test]
fn completed_and_run_closeout_purge_only_their_run_correlations() {
    use crate::agent_event::AgentEvent;

    let state = GatewayInnerState::default();
    remember_tool_name(&state, "run-completed", "tool-1", "shell-completed");
    remember_tool_name(&state, "run-closeout", "tool-1", "shell-closeout");
    remember_tool_name(&state, "run-active", "tool-1", "shell-active");
    state.upstream_state.fetch_or(1, Ordering::Release);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![
                crate::event_transport::RunBatch {
                    session_id: "sess-1".to_owned(),
                    run_id: "run-completed".to_owned(),
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
                },
                crate::event_transport::RunBatch {
                    session_id: "sess-1".to_owned(),
                    run_id: "run-closeout".to_owned(),
                    dispatch: None,
                    events: vec![crate::event_transport::SequencedEvent {
                        seq: 2,
                        event: AgentEvent::RunCloseout {
                            run_id: "run-closeout".to_owned(),
                            commit_sha: None,
                            files_changed: None,
                            insertions: None,
                            deletions: None,
                            interrupted: None,
                        },
                    }],
                },
            ],
        },
    );

    assert_eq!(take_tool_name(&state, "run-completed", "tool-1"), "");
    assert_eq!(take_tool_name(&state, "run-closeout", "tool-1"), "");
    assert_eq!(
        take_tool_name(&state, "run-active", "tool-1"),
        "shell-active"
    );
    let correlation = lock(&state.tool_correlation);
    assert!(correlation.names.is_empty());
    assert!(correlation.order.is_empty());
}
