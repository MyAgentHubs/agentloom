#![cfg(test)]

use super::*;
#[test]
fn tool_completed_uses_milestone_queue_when_live_queue_is_full() {
    use crate::agent_event::{AgentEvent, ToolStatus};

    let state = GatewayInnerState::default();
    let generation = state.advance_generation_and_set_gate(true);
    remember_tool_name(&state, "run-1", "tool-1", "shell");
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    upstream_tx
        .try_send((
            generation,
            LiveQueueItem::Batch(text_delta_payload("occupied")),
        ))
        .unwrap();
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-1",
            "sess-1",
            AgentEvent::ToolCompleted {
                id: "tool-1".to_owned(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: Some("done".to_owned()),
            },
        ),
    );

    assert_eq!(state.upstream_dropped.load(Ordering::Relaxed), 1);
    assert_eq!(
        upstream_rx.try_recv().unwrap().1,
        LiveQueueItem::Batch(text_delta_payload("occupied"))
    );
    let (item_generation, item) = milestone_rx.try_recv().unwrap();
    assert_eq!(item_generation, generation);
    assert_eq!(item.session.as_deref(), Some("sess-1"));
    assert_eq!(item.t, "tool.completed");
    assert_eq!(item.payload["id"], "tool-1");
    assert_eq!(item.payload["tool"], "shell");
    assert_eq!(item.payload["status"], "ok");
    assert_eq!(item.payload["exit_code"], 0);
    assert_eq!(item.payload["output"], "done");
}

#[test]
fn tool_completed_milestone_exists_before_live_budget_drain() {
    use crate::agent_event::{AgentEvent, ToolStatus};

    let (addr, server) = spawn_discarding_server();
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
        .expect("client should connect to discarding server");
    set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
    set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();

    let state = GatewayInnerState::default();
    // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是 tool.completed 里程碑先于
    // budget 耗尽的 live drain 存在），必须配一个 active repo，不然 "sess-1" 会被
    // fail-closed 挡下。
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    state.advance_generation_and_set_gate(true);
    remember_tool_name(&state, "run-1", "tool-1", "shell");
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let payload = crate::event_transport::BatchPayload {
        batches: vec![crate::event_transport::RunBatch {
            session_id: "sess-1".to_owned(),
            run_id: "run-1".to_owned(),
            dispatch: None,
            events: vec![
                crate::event_transport::SequencedEvent {
                    seq: 1,
                    event: AgentEvent::TextDelta {
                        text: "before completion".to_owned(),
                    },
                },
                crate::event_transport::SequencedEvent {
                    seq: 2,
                    event: AgentEvent::ToolCompleted {
                        id: "tool-1".to_owned(),
                        status: ToolStatus::Ok,
                        exit_code: Some(0),
                        output: Some("done".to_owned()),
                    },
                },
            ],
        }],
    };

    enqueue_batch_payload_for_upstream(&state, &upstream_tx, &milestone_tx, payload);

    let (_, milestone) = milestone_rx
        .try_recv()
        .expect("tool.completed milestone must exist before any live drain");
    assert_eq!(milestone.t, "tool.completed");
    assert_eq!(milestone.payload["tool"], "shell");
    assert_eq!(milestone.payload["status"], "ok");
    assert_eq!(milestone.payload["exit_code"], 0);
    assert_eq!(milestone.payload["output"], "done");

    drain_upstream_with_budget(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&Zeroizing::new([7_u8; 32])),
        "0123456789abcdef0123456789abcdef",
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
        Duration::ZERO,
    )
    .unwrap();

    assert_eq!(state.frames_sent.load(Ordering::Relaxed), 1);
    assert_eq!(state.upstream_budget_dropped.load(Ordering::Relaxed), 1);
    drop(socket);
    server.join().expect("discarding server should not panic");
}
