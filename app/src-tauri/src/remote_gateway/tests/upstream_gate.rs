#![cfg(test)]

use super::*;
fn multi_event_payload(
    batch_count: usize,
    events_per_batch: usize,
) -> crate::event_transport::BatchPayload {
    crate::event_transport::BatchPayload {
        batches: (0..batch_count)
            .map(|batch_index| crate::event_transport::RunBatch {
                session_id: format!("sess-{batch_index}"),
                run_id: format!("run-{batch_index}"),
                dispatch: None,
                events: (0..events_per_batch)
                    .map(|event_index| crate::event_transport::SequencedEvent {
                        seq: event_index as u64,
                        event: crate::agent_event::AgentEvent::TextDelta {
                            text: "hi".to_owned(),
                        },
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[test]
fn sink_gate_moves_owned_payload_unchanged_and_counts_full_queue() {
    let state = GatewayInnerState::default();
    let (tx, rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    let gated_payload = text_delta_payload("gated");

    enqueue_batch_payload_for_upstream(&state, &tx, &milestone_tx, gated_payload);
    assert!(rx.try_recv().is_err());
    assert_eq!(state.classify_skipped.load(Ordering::Relaxed), 0);
    assert_eq!(state.upstream_dropped.load(Ordering::Relaxed), 0);

    state.upstream_state.fetch_or(1, Ordering::Release);
    let first = text_delta_payload("sess-1");
    enqueue_batch_payload_for_upstream(&state, &tx, &milestone_tx, first.clone());
    enqueue_batch_payload_for_upstream(&state, &tx, &milestone_tx, text_delta_payload("sess-2"));

    assert_eq!(rx.try_recv().unwrap(), (0, LiveQueueItem::Batch(first)));
    assert_eq!(state.upstream_dropped.load(Ordering::Relaxed), 1);
    assert_eq!(state.classify_skipped.load(Ordering::Relaxed), 0);
}

#[test]
fn upstream_gate_guard_disables_flag_on_normal_scope_exit() {
    let state = AtomicU64::new(1);
    {
        let _guard = UpstreamGateGuard::new(&state);
        assert!(state.load(Ordering::Acquire) & 1 == 1);
    }
    assert_eq!(state.load(Ordering::Acquire), 0);
}

#[test]
fn upstream_gate_guard_disables_flag_on_panic_unwind() {
    let state = AtomicU64::new(1);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = UpstreamGateGuard::new(&state);
        panic!("boom");
    }));

    assert!(result.is_err());
    assert_eq!(state.load(Ordering::Acquire), 0);
}

#[test]
fn drain_upstream_processes_at_most_one_bounded_round() {
    let (addr, server) = spawn_discarding_server();
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
        .expect("client should connect to discarding server");
    set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
    set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();

    let state = GatewayInnerState::default();
    // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是每轮最多处理
    // MAX_DRAIN_ITEMS_PER_ROUND 条的预算上限），必须配一个 active repo，不然全部 100 条
    // "sess-N" 都会被 fail-closed 挡下，永远数不出 MAX_DRAIN_ITEMS_PER_ROUND 条已发送帧。
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let (tx, rx) = mpsc::sync_channel(100);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    for index in 0..100 {
        tx.try_send((
            0,
            LiveQueueItem::Batch(text_delta_payload(&format!("sess-{index}"))),
        ))
        .unwrap();
    }
    let k_room = Zeroizing::new([7_u8; 32]);

    drain_upstream(
        &mut socket,
        &state,
        &rx,
        &milestone_rx,
        Some(&k_room),
        "0123456789abcdef0123456789abcdef",
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    assert_eq!(
        state.frames_sent.load(Ordering::Relaxed),
        MAX_DRAIN_ITEMS_PER_ROUND as u64
    );
    let mut remaining = 0;
    while rx.try_recv().is_ok() {
        remaining += 1;
    }
    assert_eq!(remaining, 100 - MAX_DRAIN_ITEMS_PER_ROUND);
    drop(socket);
    server.join().expect("discarding server should not panic");
}

#[test]
fn drain_upstream_budget_drops_the_rest_of_a_multi_event_payload() {
    let (addr, server) = spawn_discarding_server();
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
        .expect("client should connect to discarding server");
    set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
    set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();

    let state = GatewayInnerState::default();
    // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是单条多事件 payload 被
    // budget 截断），必须配一个 active repo，不然 payload 里的 session 会被 fail-closed
    // 挡下。
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let (tx, rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    tx.try_send((0, LiveQueueItem::Batch(multi_event_payload(5, 10))))
        .unwrap();
    let k_room = Zeroizing::new([7_u8; 32]);

    drain_upstream_with_budget(
        &mut socket,
        &state,
        &rx,
        &milestone_rx,
        Some(&k_room),
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

#[test]
fn write_timeout_is_fixed_at_five_hundred_milliseconds() {
    assert_eq!(WRITE_TIMEOUT, Duration::from_millis(500));
}

#[test]
fn attempt_without_k_room_disables_upstream() {
    let room = "0123456789abcdef0123456789abcdef".to_owned();
    let resolved_room = room.clone();
    // M2-4d：legacy 全局 `remote_room_id` 回落已撤，`current_config` 只认
    // `remote_active_repo_id`——这里改走 active 房解析路径，不然设置里的 `remote_room_id`
    // 不会再被读到，`attempt_once` 会在 current_config 判"未配置"那一步直接短路返回
    // `Waiting`（也会 disable_upstream_gate，断言会"因为没跑到"而不是"因为真的没有
    // k_room"而通过——见本测试改写说明），测试就测不到本该覆盖的"缺 K_room"路径了。
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("ws://127.0.0.1:1".to_owned()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        },
        move |_project_id| Ok(resolved_room.clone()),
    );
    let (_tx, rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let _ = attempt_once(&inner, &rx, &milestone_rx);

    assert!(!inner.state.upstream_enabled_snapshot());
}

#[test]
fn failed_attempt_with_k_room_keeps_upstream_disabled() {
    let room = "0123456789abcdef0123456789abcdef".to_owned();
    let resolved_room = room.clone();
    let provider_room = room.clone();
    // M2-4d：同上一条测试，改走 active 房解析路径（不再是 legacy `remote_room_id` 回落）。
    let inner = test_inner_with_active_room_resolver_and_k_room(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("ws://127.0.0.1:1".to_owned()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        },
        move |_project_id| Ok(resolved_room.clone()),
        move |room_id| (room_id == provider_room).then(|| Zeroizing::new([1_u8; 32])),
    );
    let (_tx, rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let _ = attempt_once(&inner, &rx, &milestone_rx);

    assert!(!inner.state.upstream_enabled_snapshot());
}

/// M2-4d：跟 `test_inner_with_active_room_resolver` 同一套默认 fixture，额外把
/// `k_room_provider` 也换成调用方提供的实现——需要同时控制"active 房解析结果"与"这间房
/// 有没有 K_room"两个维度的测试专用（k_room 缺失/连接失败类场景，legacy 回落撤除后不能
/// 再靠只读 `remote_room_id` 让 `current_config` 解出配置了，必须真的把 active 房解析
/// 通道接上）。
fn test_inner_with_active_room_resolver_and_k_room(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    active_room_resolver: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
    k_room_provider: impl Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    Arc::new(Inner {
        settings: Box::new(settings),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: Box::new(active_room_resolver),
        k_room_provider: Box::new(k_room_provider),
        session_index_snapshot_provider: Box::new(|| None),
        milestone_replay_provider: Box::new(|| None),
        session_runtime_replay_provider: Box::new(|| None),
        pair_hello_handler: Box::new(|_| None),
        pair_done_handler: Box::new(|_| PairDoneAction::Rejected),
        registry: test_registry(),
        refresh_handler: test_refresh_handler(),
        registry_snapshot_provider: test_registry_snapshot_provider(),
        registry_rebase_provider: test_registry_rebase_provider(),
        registry_high_water_provider: test_registry_high_water_provider(),
        input_send_handler: Box::new(|_| Some(AckOutcome::Failed)),
        input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(|_| AckOutcome::Failed),
        upstream_tx,
        milestone_tx,
        session_repo_provider: test_session_repo_provider(),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    })
}
