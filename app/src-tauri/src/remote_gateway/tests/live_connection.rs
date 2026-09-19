#![cfg(test)]

use super::*;
#[test]
fn live_connection_gates_upstream_and_raii_guard_disables_it_on_disconnect() {
    let (addr, release_server, server) = spawn_holding_server();
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let connected_config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: None,
    };
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let inner = test_inner(|_| None, || None);
    let (_tx, rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let k_room = Zeroizing::new([1_u8; 32]);

    let result = thread::scope(|scope| {
        let connection_inner = &inner;
        let connection_url = &url;
        let connection_config = &connected_config;
        let connection_k_room = &k_room;
        let connection = scope.spawn(move || {
            let result = run_authenticated_connection(
                connection_inner,
                connection_url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                connection_config,
                None,
                &rx,
                &milestone_rx,
                Some(connection_k_room),
            );
            result
        });

        wait_until_connected(&inner);
        assert!(inner.state.upstream_enabled_snapshot());
        inner.shutdown.store(true, Ordering::Release);
        release_server.send(()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !connection.is_finished() {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            connection.is_finished(),
            "connection did not finish within three seconds"
        );
        connection
            .join()
            .expect("connection thread should not panic")
    });

    assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
    assert!(!inner.state.upstream_enabled_snapshot());
    server.join().expect("holding server should not panic");
}

#[test]
fn live_connection_publishes_session_index_snapshot() {
    // M2-4d：归属闸恒启用——这条测试不关心归属过滤本身（覆盖的是快照帧的信封/负载形状），
    // 必须显式配一个 active repo，且会话的 repo_id 要跟它一致，不然全会被
    // `filter_session_index_snapshot_for_active_repo` fail-closed 成空数组。
    let sessions = serde_json::json!([{
        "id": "s1",
        "title": "T",
        "repo_id": "repo-a",
        "archived": false,
        "status": Value::Null,
        "run_id": Value::Null,
        "updated_at": 1,
    }]);
    let provider_sessions = sessions.clone();
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([17_u8; 32])),
        move || Some(provider_sessions.clone()),
    );
    *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (addr, frames, server) = spawn_recording_server_after_sync(1);
    let connected_config = GatewayConfig {
        relay_url: format!("ws://{addr}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: Some("repo-a".to_owned()),
    };
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let k_room = Zeroizing::new([17_u8; 32]);

    let frame_result = thread::scope(|scope| {
        let connection_inner = &inner;
        let connection_url = &url;
        let connection_config = &connected_config;
        let connection_k_room = &k_room;
        let connection = scope.spawn(move || {
            run_authenticated_connection(
                connection_inner,
                connection_url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                connection_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(connection_k_room),
            )
        });

        let frame_result = frames.recv_timeout(Duration::from_secs(3));
        inner.shutdown.store(true, Ordering::Release);
        let _ = connection
            .join()
            .expect("connection thread should not panic");
        frame_result
    });

    let server_result = server.join();
    let envelope = frame_result.expect("session.index snapshot frame should arrive");
    assert_eq!(envelope["kind"], "event");
    assert_eq!(envelope["session"], Value::Null);
    assert_eq!(envelope["seq"], Value::Null);
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    assert_eq!(plaintext["t"], "session.index");
    assert_eq!(plaintext["full"], true);
    assert_eq!(plaintext["sessions"], sessions);
    server_result.expect("recording server should not panic");
}

#[test]
fn live_connection_survives_unavailable_session_index_snapshot() {
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([18_u8; 32])),
        || None,
    );
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (addr, release_server, server) = spawn_holding_server();
    let connected_config = GatewayConfig {
        relay_url: format!("ws://{addr}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let k_room = Zeroizing::new([18_u8; 32]);

    let result = thread::scope(|scope| {
        let connection_inner = &inner;
        let connection_url = &url;
        let connection_config = &connected_config;
        let connection_k_room = &k_room;
        let connection = scope.spawn(move || {
            run_authenticated_connection(
                connection_inner,
                connection_url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                connection_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(connection_k_room),
            )
        });

        wait_until_connected(&inner);
        wait_until_counter_at_least(&inner.state.session_index_snapshot_unavailable, 1);
        assert_eq!(
            inner
                .state
                .session_index_snapshot_unavailable
                .load(Ordering::Relaxed),
            1
        );
        release_server.send(()).unwrap();
        inner.shutdown.store(true, Ordering::Release);
        connection
            .join()
            .expect("connection thread should not panic")
    });

    assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
    server.join().expect("holding server should not panic");
}

#[test]
fn sink_snapshot_generation_is_immune_to_later_connection_switch() {
    let state = GatewayInnerState::default();
    let (tx, rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

    let generation_a = state.advance_generation_and_set_gate(true);
    assert_eq!(generation_a, 1);
    let payload = text_delta_payload("from-a");
    enqueue_batch_payload_for_upstream(&state, &tx, &milestone_tx, payload.clone());

    state.disable_upstream_gate();
    let generation_b = state.advance_generation_and_set_gate(true);
    assert_eq!(generation_b, 2);

    let (item_generation, queued_payload) = rx.try_recv().unwrap();
    assert_eq!(queued_payload, LiveQueueItem::Batch(payload));
    assert_eq!(item_generation, generation_a);
    assert_ne!(item_generation, generation_b);
    assert_eq!(state.connection_generation_snapshot(), generation_b);
    assert_ne!(item_generation, state.connection_generation_snapshot());
}

#[test]
fn stale_generation_payload_is_not_replayed_into_the_next_connection() {
    let inner = test_inner(|_| None, || None);
    let (tx, rx) = mpsc::sync_channel(1);
    let k_room = Zeroizing::new([1_u8; 32]);

    let (addr_a, release_a, server_a) = spawn_holding_server();
    let config_a = GatewayConfig {
        relay_url: format!("ws://{addr_a}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url_a = build_ws_url(&config_a.relay_url, &config_a.room_id);
    let inner_a = Arc::clone(&inner);
    let k_room_a = k_room.clone();
    let connection_a = thread::spawn(move || {
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let result = run_authenticated_connection(
            &inner_a,
            &url_a,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config_a,
            None,
            &rx,
            &milestone_rx,
            Some(&k_room_a),
        );
        (result, rx)
    });

    wait_until_generation(&inner, 1);
    let generation_a = inner.state.connection_generation_snapshot();
    inner.shutdown.store(true, Ordering::Release);
    release_a.send(()).unwrap();
    let (result_a, rx) = connection_a
        .join()
        .expect("connection A thread should not panic");
    assert_eq!(result_a, Ok(ConnectionExit::ClosedByPeer));
    assert!(!inner.state.upstream_enabled_snapshot());
    server_a.join().expect("holding server A should not panic");

    inner.shutdown.store(false, Ordering::Release);
    tx.try_send((
        generation_a,
        LiveQueueItem::Batch(text_delta_payload("late-from-a")),
    ))
    .unwrap();

    let (addr_b, release_b, server_b) = spawn_holding_server();
    let config_b = GatewayConfig {
        relay_url: format!("ws://{addr_b}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url_b = build_ws_url(&config_b.relay_url, &config_b.room_id);
    let inner_b = Arc::clone(&inner);
    let k_room_b = k_room.clone();
    let connection_b = thread::spawn(move || {
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let result = run_authenticated_connection(
            &inner_b,
            &url_b,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config_b,
            None,
            &rx,
            &milestone_rx,
            Some(&k_room_b),
        );
        (result, rx)
    });

    wait_until_generation(&inner, generation_a + 1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && inner
            .state
            .upstream_stale_generation_dropped
            .load(Ordering::Relaxed)
            == 0
    {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        inner
            .state
            .upstream_stale_generation_dropped
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        inner.state.frames_sent.load(Ordering::Relaxed),
        2,
        "only the two mandatory connection sync frames should have been sent"
    );

    inner.shutdown.store(true, Ordering::Release);
    release_b.send(()).unwrap();
    let (result_b, rx) = connection_b
        .join()
        .expect("connection B thread should not panic");
    assert_eq!(result_b, Ok(ConnectionExit::ClosedByPeer));
    assert!(rx.try_recv().is_err());
    assert!(!inner.state.upstream_enabled_snapshot());
    server_b.join().expect("holding server B should not panic");
}

fn spawn_recording_server_after_sync(
    expected_frames: usize,
) -> (
    std::net::SocketAddr,
    mpsc::Receiver<Value>,
    thread::JoinHandle<()>,
) {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("recording listener should bind");
    let addr = listener
        .local_addr()
        .expect("recording listener should have an address");
    let (frame_tx, frame_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("recording server should accept");
        let mut socket = tungstenite::accept(stream).expect("websocket handshake should pass");
        ack_initial_registry_sync(&mut socket);
        for _ in 0..expected_frames {
            let message = socket
                .read()
                .expect("recording server should receive a frame");
            let Message::Text(text) = message else {
                panic!("recording server expected a text frame");
            };
            let value =
                serde_json::from_str(text.as_ref()).expect("recorded upstream text must be JSON");
            frame_tx.send(value).unwrap();
        }
    });
    (addr, frame_rx, handle)
}

fn spawn_holding_server() -> (
    std::net::SocketAddr,
    mpsc::Sender<()>,
    thread::JoinHandle<()>,
) {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("holding listener should bind");
    let addr = listener
        .local_addr()
        .expect("holding listener should have an address");
    let (release_tx, release_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            if let Ok(mut socket) = tungstenite::accept(stream) {
                ack_initial_registry_sync(&mut socket);
                let _ = release_rx.recv();
                let _ = socket.close(None);
            }
        }
    });
    (addr, release_tx, handle)
}

fn wait_until_generation(inner: &Inner, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if inner.state.connection_generation_snapshot() == expected
            && inner.state.upstream_enabled_snapshot()
        {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("connection generation did not reach {expected} within two seconds");
}
