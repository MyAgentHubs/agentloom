#![cfg(test)]

use super::*;
#[test]
fn disconnects_when_disabled_or_connected_config_or_token_changes() {
    let connected = GatewayConfig {
        relay_url: "wss://relay.example.com".to_owned(),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let changed = GatewayConfig {
        relay_url: connected.relay_url.clone(),
        room_id: "fedcba9876543210fedcba9876543210".to_owned(),
        active_repo_id: None,
    };
    let initial_token = SecretToken::new("initial-token".to_owned());
    let rotated_token = SecretToken::new("rotated-token".to_owned());

    assert_eq!(
        evaluate_connection_liveness(
            false,
            Some(&connected),
            &connected,
            Some(&initial_token),
            Some(&initial_token),
            true,
            true,
        ),
        ConnectionDecision::Disconnect
    );
    assert_eq!(
        evaluate_connection_liveness(
            true,
            Some(&changed),
            &connected,
            Some(&initial_token),
            Some(&initial_token),
            true,
            true,
        ),
        ConnectionDecision::Disconnect
    );
    assert_eq!(
        evaluate_connection_liveness(
            true,
            Some(&connected),
            &connected,
            Some(&rotated_token),
            Some(&initial_token),
            true,
            true,
        ),
        ConnectionDecision::Disconnect
    );
    assert_eq!(
        evaluate_connection_liveness(
            true,
            Some(&connected),
            &connected,
            None,
            Some(&initial_token),
            true,
            true,
        ),
        ConnectionDecision::Disconnect
    );
    assert_eq!(
        evaluate_connection_liveness(
            true,
            Some(&connected),
            &connected,
            Some(&initial_token),
            Some(&initial_token),
            true,
            true,
        ),
        ConnectionDecision::Continue
    );
}

#[test]
fn disconnects_when_k_room_becomes_available_after_connect() {
    let connected = GatewayConfig {
        relay_url: "wss://relay.example.com".to_owned(),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let initial_token = SecretToken::new("initial-token".to_owned());

    for (connected_k_room_available, current_k_room_available, expected) in [
        (false, true, ConnectionDecision::Disconnect),
        (false, false, ConnectionDecision::Continue),
        (true, true, ConnectionDecision::Continue),
    ] {
        assert_eq!(
            evaluate_connection_liveness(
                true,
                Some(&connected),
                &connected,
                Some(&initial_token),
                Some(&initial_token),
                connected_k_room_available,
                current_k_room_available,
            ),
            expected
        );
    }
}

#[test]
fn continuous_frames_do_not_starve_liveness_check_when_disabled() {
    let (addr, server) = spawn_frame_pump_server();
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let enabled = Arc::new(AtomicBool::new(true));
    let settings_enabled = Arc::clone(&enabled);
    let settings_relay_url = relay_url.clone();
    let settings_room_id = room_id.clone();
    let inner = test_inner_with_interval(
        move |key| match key {
            "remote_control_enabled" => Some(settings_enabled.load(Ordering::Acquire).to_string()),
            "remote_relay_url" => Some(settings_relay_url.clone()),
            "remote_room_id" => Some(settings_room_id.clone()),
            _ => None,
        },
        || None,
        Duration::from_millis(150),
    );
    let connected_config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: None,
    };
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connected_config,
            None,
            &upstream_rx,
            &milestone_rx,
            None,
        )
    });

    wait_until_connected(&inner);
    enabled.store(false, Ordering::Release);

    let result = join_connection_within(connection, &inner);
    assert!(matches!(result, Ok(ConnectionExit::ConfigStale { .. })));
    server.join().expect("frame pump server should not panic");
}

#[test]
fn liveness_does_not_poll_k_room_provider_when_connection_has_k_room() {
    let (addr, server) = spawn_frame_pump_server();
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let enabled = Arc::new(AtomicBool::new(true));
    let settings_enabled = Arc::clone(&enabled);
    let settings_relay_url = relay_url.clone();
    let settings_room_id = room_id.clone();
    let liveness_checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let token_provider_checks = Arc::clone(&liveness_checks);
    let k_room_provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_calls = Arc::clone(&k_room_provider_calls);
    let resolver_room_id = settings_room_id.clone();
    let inner = test_inner_with_interval_k_room_and_active_room_resolver(
        move |key| match key {
            "remote_control_enabled" => Some(settings_enabled.load(Ordering::Acquire).to_string()),
            "remote_relay_url" => Some(settings_relay_url.clone()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        },
        move || {
            token_provider_checks.fetch_add(1, Ordering::Relaxed);
            None
        },
        move |_| {
            provider_calls.fetch_add(1, Ordering::Relaxed);
            None
        },
        move |_project_id| Ok(resolver_room_id.clone()),
        Duration::from_millis(150),
    );
    // active_repo_id 必须跟 current_config 每轮 liveness 重新解析出的值一致（都是
    // "proj-1"）——GatewayConfig 的 PartialEq 把这个字段也比进去了，不一致会让
    // evaluate_connection_liveness 在第一轮就判定配置陈旧并断开，测试永远等不到期望的
    // 轮询次数。
    let connected_config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: Some("proj-1".to_owned()),
    };
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        let k_room = Zeroizing::new([7u8; 32]);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connected_config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&k_room),
        )
    });

    wait_until_connected(&inner);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && liveness_checks.load(Ordering::Relaxed) < 3 {
        thread::sleep(Duration::from_millis(20));
    }
    let checks_before_disconnect = liveness_checks.load(Ordering::Relaxed);
    let provider_calls_before_disconnect = k_room_provider_calls.load(Ordering::Relaxed);
    enabled.store(false, Ordering::Release);

    let result = join_connection_within(connection, &inner);
    assert!(matches!(result, Ok(ConnectionExit::ConfigStale { .. })));
    server.join().expect("frame pump server should not panic");
    assert!(
        checks_before_disconnect >= 3,
        "expected at least three liveness checks, got {checks_before_disconnect}"
    );
    assert_eq!(provider_calls_before_disconnect, 0);
}

#[test]
fn token_rotation_disconnects_a_live_connection_with_continuous_frames() {
    let (addr, server) = spawn_frame_pump_server();
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let token = Arc::new(Mutex::new("initial-token".to_owned()));
    let token_provider_value = Arc::clone(&token);
    let settings_relay_url = relay_url.clone();
    let settings_room_id = room_id.clone();
    let inner = test_inner_with_interval(
        move |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some(settings_relay_url.clone()),
            "remote_room_id" => Some(settings_room_id.clone()),
            _ => None,
        },
        move || Some(lock(&token_provider_value).clone()),
        Duration::from_millis(150),
    );
    let connected_config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: None,
    };
    let connected_token = SecretToken::new("initial-token".to_owned());
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connected_config,
            Some(&connected_token),
            &upstream_rx,
            &milestone_rx,
            None,
        )
    });

    wait_until_connected(&inner);
    *lock(&token) = "rotated-token".to_owned();

    let result = join_connection_within(connection, &inner);
    assert!(matches!(result, Ok(ConnectionExit::ConfigStale { .. })));
    server.join().expect("frame pump server should not panic");
}

#[test]
fn pre_ack_error_frame_from_relay_carries_frame_type_and_reason_in_the_message() {
    // S1ja F3 (批次审 3④)：mixed-version rollout — desktop upgraded, relay hasn't yet —
    // makes relay answer the initial `token.sync` with `{t:"error",
    // reason:"unknown_frame_type"}` instead of `token.sync.ack`. Before this fix the
    // resulting error was a bare, undiagnosable "relay sent a frame before registry sync
    // ack"; it must now name the frame type and reason it actually received.
    let (addr, server) = spawn_pre_ack_decoy_server(serde_json::json!({
        "t": "error",
        "reason": "unknown_frame_type",
    }));
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let inner = test_inner(move |_| None, || None);
    let config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            None,
        )
    });
    let result = join_connection_within(connection, &inner);
    assert_eq!(
        result,
        Err(ConnectionFailure::Other(
            "relay sent a frame before registry sync ack (t=error, reason=unknown_frame_type)"
                .to_owned()
        ))
    );
    server
        .join()
        .expect("pre-ack decoy server should not panic");
}

#[test]
fn pre_ack_frame_missing_reason_still_names_the_frame_type() {
    // Companion to the above: no `reason` field present — the message must still name the
    // frame type without a dangling/placeholder reason.
    let (addr, server) =
        spawn_pre_ack_decoy_server(serde_json::json!({ "t": "presence", "role": "desktop" }));
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let inner = test_inner(move |_| None, || None);
    let config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            None,
        )
    });
    let result = join_connection_within(connection, &inner);
    assert_eq!(
        result,
        Err(ConnectionFailure::Other(
            "relay sent a frame before registry sync ack (t=presence)".to_owned()
        ))
    );
    server
        .join()
        .expect("pre-ack decoy server should not panic");
}

#[test]
fn pre_ack_decoy_token_ack_frame_survives_record_failure_redact_intact() {
    // R3 (双路审): F4's `scrub_after_marker("token.")` pass and F3's new
    // frame-type-carrying error string collide unless the hex-length floor exists — a
    // decoy `{t:"token.ack"}` frame produces the message `"...(t=token.ack)"`, and
    // without a floor the "token." marker inside "t=token.ack" would eat "ac" (the
    // first two hex-looking characters of "ack") and turn it into
    // "t=token.***k)" — F4 clobbering F3's own diagnostic. This drives the real
    // read_registry_sync_ack → ConnectionFailure → record_failure → redact() chain end
    // to end and asserts the frame type survives every step unmangled.
    let (addr, server) = spawn_pre_ack_decoy_server(serde_json::json!({ "t": "token.ack" }));
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let inner = test_inner(move |_| None, || None);
    let config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            None,
        )
    });
    let result = join_connection_within(connection, &inner);
    let Err(ConnectionFailure::Other(message)) = result else {
        panic!("expected a connection failure carrying the frame-type detail, got {result:?}");
    };
    assert!(
        message.contains("t=token.ack"),
        "pre-redact message must name the frame type: {message}"
    );

    let mut failed_attempts = 0;
    record_failure(
        &inner,
        FailureKind::Connection,
        message,
        None,
        &mut failed_attempts,
    );
    let status = lock(&inner.state.status).clone();
    let last_error = status
        .last_error
        .expect("connection failure should be recorded");
    assert!(
        last_error.contains("t=token.ack"),
        "redact()'s hex-length floor must not clobber F3's frame-type detail: {last_error}"
    );
    server
        .join()
        .expect("pre-ack decoy server should not panic");
}

#[test]
fn pairing_reload_requested_disconnects_a_live_connection_with_continuous_frames() {
    let (addr, server) = spawn_frame_pump_server();
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let active_repo_id = "pairing-reload-repo".to_owned();
    let settings_relay_url = relay_url.clone();
    let settings_active_repo_id = active_repo_id.clone();
    let resolver_room_id = room_id.clone();
    let inner = test_inner_with_interval_k_room_and_active_room_resolver(
        move |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some(settings_relay_url.clone()),
            "remote_active_repo_id" => Some(settings_active_repo_id.clone()),
            _ => None,
        },
        || None,
        |_| None,
        move |_| Ok(resolver_room_id.clone()),
        Duration::from_millis(150),
    );
    let connected_config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: Some(active_repo_id),
    };
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connected_config,
            None,
            &upstream_rx,
            &milestone_rx,
            None,
        )
    });

    wait_until_connected(&inner);
    inner.reload_requested.store(true, Ordering::Release);

    let result = join_connection_within(connection, &inner);
    assert_eq!(result, Ok(ConnectionExit::PairingReloadRequested));
    server.join().expect("frame pump server should not panic");
}

#[test]
fn remote_registry_publish_wake_keeps_live_connection_generation_and_sends_put() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (frame_tx, frame_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        ack_initial_registry_sync(&mut socket);
        loop {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                    if frame["t"] == "token.put" {
                        frame_tx.send(frame).unwrap();
                        break;
                    }
                }
                Ok(_) => {}
                Err(error) => panic!("live relay should receive token.put: {error}"),
            }
        }
        let _ = socket.close(None);
    });
    let relay_url = format!("ws://{addr}");
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let settings_relay_url = relay_url.clone();
    let settings_room_id = room_id.clone();
    let inner = test_inner_with_interval(
        move |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some(settings_relay_url.clone()),
            "remote_room_id" => Some(settings_room_id.clone()),
            _ => None,
        },
        || None,
        Duration::from_secs(30),
    );
    let connected_config = GatewayConfig {
        relay_url,
        room_id,
        active_repo_id: None,
    };
    let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection = thread::spawn(move || {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connected_config,
            None,
            &upstream_rx,
            &milestone_rx,
            None,
        )
    });

    wait_until_connected(&inner);
    let generation_before = inner.state.connection_generation_snapshot();
    let epoch_before = inner.state.epoch.load(Ordering::Acquire);
    lock(&inner.registry).enqueue_token_put(
        TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 8,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_430_700_000,
                refresh_until: None,
            },
            prev: None,
        },
        None,
    );
    inner.registry_publish_wake.store(true, Ordering::Release);

    let frame = frame_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(frame["subject"], "pairing");
    assert_eq!(
        inner.state.connection_generation_snapshot(),
        generation_before
    );
    assert_eq!(inner.state.epoch.load(Ordering::Acquire), epoch_before);
    assert!(!inner.reload_requested.load(Ordering::Acquire));
    assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
    server.join().unwrap();
}

/// S1ja F3: mixed-version fixture — reads the desktop's initial `token.sync` (like
/// `ack_initial_registry_sync`) but answers with `decoy_frame` instead of the expected
/// `token.sync.ack`, simulating an old relay that doesn't yet understand the registry
/// sync handshake and replies with a generic protocol error.
fn spawn_pre_ack_decoy_server(
    decoy_frame: Value,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("pre-ack decoy listener should bind");
    let addr = listener
        .local_addr()
        .expect("pre-ack decoy listener should have an address");
    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            if let Ok(mut socket) = tungstenite::accept(stream) {
                let _initial_sync = recv_text_skip_control(&mut socket);
                let _ = socket.send(Message::Text(decoy_frame.to_string().into()));
                // Keep the socket open briefly so the client's read doesn't race a TCP
                // reset instead of observing the decoy frame's WebSocketError path.
                thread::sleep(Duration::from_millis(200));
            }
        }
    });
    (addr, handle)
}
