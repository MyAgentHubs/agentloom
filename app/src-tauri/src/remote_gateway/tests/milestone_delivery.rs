#![cfg(test)]

use super::*;
#[test]
fn entropy_failure_uses_existing_invalid_client_msg_id_drop_path() {
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel(1);
    let client_msg_id = {
        let _guard = ForceClientMsgIdEntropyFailureGuard::new();
        try_random_client_msg_id().unwrap_or_default()
    };

    enqueue_milestone_for_upstream(
        &state,
        &tx,
        MilestoneItem {
            session: Some("sess-1".to_owned()),
            t: "run.status".to_owned(),
            payload: serde_json::json!({"status": "running"}),
            client_msg_id,
        },
    );

    assert_eq!(state.milestone_dropped.load(Ordering::Relaxed), 1);
    assert_eq!(rx.try_iter().count(), 0);
}

#[test]
fn entropy_failure_drops_session_index_snapshot_without_panicking() {
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
    );
    let connection_generation = inner.state.advance_generation_and_set_gate(true);

    {
        let _guard = ForceClientMsgIdEntropyFailureGuard::new();
        publish_session_index_snapshot_on_connect(&inner, connection_generation);
    }

    assert_eq!(inner.state.milestone_dropped.load(Ordering::Relaxed), 1);
    assert_eq!(milestone_rx.try_iter().count(), 0);
}

#[test]
fn entropy_failure_does_not_panic_in_random_id_publish_facades() {
    // `GATEWAY` is intentionally not installed in unit tests, so this only proves that every
    // random-ID facade survives entropy failure; enqueue/drop accounting is covered directly.
    let _guard = ForceClientMsgIdEntropyFailureGuard::new();

    publish_run_status_milestone("sess-1", "running", Some("run-1"));
    publish_session_index_created("sess-1", "Session", "repo-1", "namespace-1", None);
    publish_session_index_renamed("sess-1", "Renamed");
    publish_session_index_deleted("sess-1");
    publish_session_index_archived(&["sess-1".to_owned()], true);
}

#[test]
fn published_milestone_round_trips_without_client_msg_id_in_aad() {
    let room = "0123456789abcdef0123456789abcdef";
    let client_msg_id = "73996db9-9424-5e73-acb6-965bf87bfb80";
    let k_room = Zeroizing::new([41_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是 client_msg_id 不进 AAD 的
    // 加密不变量），必须配一个 active repo，不然 "sess-1" 会被 fail-closed 挡下。
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: Some("sess-1".to_owned()),
            t: "msg.completed".to_owned(),
            payload: serde_json::json!({"message_id": "message-1"}),
            client_msg_id: client_msg_id.to_owned(),
        },
    );

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();
    let envelope = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(envelope["kind"], "event");
    assert_eq!(envelope["client_msg_id"], client_msg_id);
    assert_eq!(envelope["seq"], Value::Null);
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    assert_eq!(plaintext["t"], "msg.completed");
    assert_eq!(plaintext["message_id"], "message-1");

    let mut changed_id = envelope.clone();
    changed_id["client_msg_id"] = Value::String("different-client-id".to_owned());
    assert_eq!(open_upstream_envelope(&k_room, &changed_id), plaintext);
    drop(upstream_tx);
    drop(socket);
    server.join().unwrap();
}

#[test]
fn milestone_channel_full_is_counted_and_never_blocks() {
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (tx, rx) = mpsc::sync_channel(1);
    let item = |client_msg_id: &str| MilestoneItem {
        session: Some("sess-1".to_owned()),
        t: "msg.completed".to_owned(),
        payload: serde_json::json!({"message_id": "message-1"}),
        client_msg_id: client_msg_id.to_owned(),
    };

    enqueue_milestone_for_upstream(&state, &tx, item("client-1"));
    enqueue_milestone_for_upstream(&state, &tx, item("client-2"));

    assert_eq!(state.milestone_dropped.load(Ordering::Relaxed), 1);
    assert_eq!(rx.try_iter().count(), 1);
}

#[test]
fn drain_sends_milestones_before_live_frames_in_the_same_round() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([42_u8; 32]);
    let state = GatewayInnerState::default();
    // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是同一轮 drain 内里程碑先于
    // live 帧发出的顺序），必须配一个 active repo，不然两条 session 都会被 fail-closed
    // 挡下。
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let generation = state.advance_generation_and_set_gate(true);
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    upstream_tx
        .try_send((
            generation,
            LiveQueueItem::Batch(text_delta_payload("sess-live")),
        ))
        .unwrap();
    milestone_tx
        .try_send((
            generation,
            MilestoneItem {
                session: Some("sess-milestone".to_owned()),
                t: "msg.completed".to_owned(),
                payload: serde_json::json!({"message_id": "message-1"}),
                client_msg_id: "client-priority".to_owned(),
            },
        ))
        .unwrap();
    let (addr, frames, server) = spawn_recording_server(2);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();

    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let first = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(first["kind"], "event");
    assert_eq!(
        open_upstream_envelope(&k_room, &first)["t"],
        "msg.completed"
    );
    assert_eq!(second["kind"], "live");
    assert_eq!(open_upstream_envelope(&k_room, &second)["t"], "text_delta");
    drop(socket);
    server.join().unwrap();
}
