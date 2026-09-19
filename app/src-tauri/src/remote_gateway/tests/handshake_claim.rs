#![cfg(test)]

use super::*;
#[test]
fn builds_websocket_urls_without_legacy_role_or_token_query_params() {
    // S1ja §9.7 后门退役：desktop 认证已完全走 `Authorization: Bearer`
    // （build_ws_request），build_ws_url 不再接受/拼接任何令牌——`?role=desktop`
    // 与 `&token=` 两个 legacy query 参数结构上不可能再出现（`&token=` 曾把
    // remote_dev_token 明文送进 CF 边缘日志，P2-1）。
    let room = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        build_ws_url("wss://relay.example.com", room),
        format!("wss://relay.example.com/room/{room}")
    );
    assert_eq!(
        build_ws_url("wss://relay.example.com/", room),
        format!("wss://relay.example.com/room/{room}")
    );
    let url = build_ws_url("wss://relay.example.com", room);
    assert!(!url.contains("role="));
    assert!(!url.contains("token="));
    assert!(!url.contains('?'));
}

#[test]
fn websocket_upgrade_request_carries_bearer_without_exposing_secret_debug() {
    let credential_text = "ab".repeat(32);
    let credential = DesktopCredential::new(Zeroizing::new(credential_text.clone()));
    let request = build_ws_request(
        "wss://relay.example.com/room/0123456789abcdef0123456789abcdef?role=desktop",
        &credential,
    )
    .unwrap();

    assert_eq!(
        request.headers().get(AUTHORIZATION).unwrap(),
        format!("Bearer {credential_text}").as_str()
    );
    assert_eq!(format!("{credential:?}"), "***");
}

#[test]
fn websocket_upgrade_refuses_redirects() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request);
        stream
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: ws://127.0.0.1:9/redirected\r\nContent-Length: 0\r\n\r\n",
            )
            .unwrap();
    });
    let credential = DesktopCredential::new(Zeroizing::new("cd".repeat(32)));
    let request = build_ws_request(
        &format!("ws://{address}/room/test?role=desktop"),
        &credential,
    )
    .unwrap();

    let error = connect_with_config(request, None, WS_MAX_REDIRECTS).unwrap_err();

    assert!(matches!(
        error,
        WebSocketError::Http(response)
            if response.status() == tungstenite::http::StatusCode::FOUND
    ));
    assert_eq!(WS_MAX_REDIRECTS, 0);
    server.join().unwrap();
}

#[test]
fn ensure_claim_200_reconnects_and_calls_claim_once() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let claim_calls = Arc::clone(&calls);
    let inner = test_inner_with_claim_handlers(
        move |_, _, hash| {
            claim_calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(
                hash,
                crate::remote_pairing::desktop_credential_hash(&"ef".repeat(32))
            );
            Ok(ClaimResponse::Claimed)
        },
        |_| Ok(false),
    );

    let action = ensure_claim(
        &inner,
        &sample_gateway_config(),
        &DesktopCredential::new(Zeroizing::new("ef".repeat(32))),
    );

    assert_eq!(action, ClaimAction::Reconnect);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn unauthorized_reconnect_cycle_claims_at_most_once() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let relay_url = format!("ws://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read])
                .to_ascii_lowercase()
                .contains("authorization: bearer "));
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        }
    });
    let claim_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let inner = test_inner_for_claim_cycle(&relay_url, Arc::clone(&claim_calls));
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let attempt = attempt_once(&inner, &upstream_rx, &milestone_rx);

    assert!(matches!(
        attempt,
        ConnectAttempt::Ran {
            result: Err(ConnectionFailure::Unauthorized),
            ..
        }
    ));
    assert_eq!(claim_calls.load(Ordering::Relaxed), 1);
    server.join().unwrap();
}

#[test]
fn websocket_upgrade_410_stops_without_claim_or_retry() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let relay_url = format!("ws://{}", listener.local_addr().unwrap());
    let upgrade_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_upgrade_calls = Arc::clone(&upgrade_calls);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        server_upgrade_calls.fetch_add(1, Ordering::Relaxed);
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..read])
            .to_ascii_lowercase()
            .contains("authorization: bearer "));
        stream
            .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
    });
    let claim_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let inner = test_inner_for_claim_cycle(&relay_url, Arc::clone(&claim_calls));
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let attempt = attempt_once(&inner, &upstream_rx, &milestone_rx);

    assert!(matches!(
        attempt,
        ConnectAttempt::Stopped(reason) if reason.code == ROOM_TOMBSTONED_STOP_REASON
    ));
    assert_eq!(upgrade_calls.load(Ordering::Relaxed), 1);
    assert_eq!(claim_calls.load(Ordering::Relaxed), 0);
    server.join().unwrap();
}

#[test]
fn stopped_loop_idles_until_settings_reload_then_reconnects() {
    use std::io::{Read, Write};

    let tombstone_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tombstone_url = format!("ws://{}", tombstone_listener.local_addr().unwrap());
    let upgrade_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_upgrade_calls = Arc::clone(&upgrade_calls);
    let tombstone_server = thread::spawn(move || {
        let (mut stream, _) = tombstone_listener.accept().unwrap();
        server_upgrade_calls.fetch_add(1, Ordering::Relaxed);
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
    });
    let enabled = Arc::new(AtomicBool::new(true));
    let settings_enabled = Arc::clone(&enabled);
    let relay_url = Arc::new(Mutex::new(tombstone_url));
    let settings_relay_url = Arc::clone(&relay_url);
    let claim_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let claim_call_counter = Arc::clone(&claim_calls);
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let inner = Arc::new(Inner {
        settings: Box::new(move |key| match key {
            "remote_control_enabled" => Some(settings_enabled.load(Ordering::Acquire).to_string()),
            "remote_relay_url" => Some(lock(&settings_relay_url).clone()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        }),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: Box::new(move |_, _, _| {
            claim_call_counter.fetch_add(1, Ordering::Relaxed);
            Ok(ClaimResponse::Claimed)
        }),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: Box::new(|_project_id| {
            Ok("0123456789abcdef0123456789abcdef".to_owned())
        }),
        k_room_provider: Box::new(|_| None),
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
        liveness_interval: Duration::from_millis(100),
    });
    let loop_inner = Arc::clone(&inner);
    let gateway_thread =
        thread::spawn(move || connect_loop(Arc::downgrade(&loop_inner), upstream_rx, milestone_rx));

    wait_until_stopped_reason(&inner, ROOM_TOMBSTONED_STOP_REASON);
    tombstone_server.join().unwrap();
    thread::sleep(BACKOFF_POLL_INTERVAL + Duration::from_millis(100));
    assert_eq!(upgrade_calls.load(Ordering::Relaxed), 1);
    assert_eq!(claim_calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        lock(&inner.state.status).stopped_reason.as_deref(),
        Some(ROOM_TOMBSTONED_STOP_REASON)
    );

    let (recovery_addr, recovery_server) = spawn_frame_pump_server();
    *lock(&relay_url) = format!("ws://{recovery_addr}");
    enabled.store(false, Ordering::Release);
    inner.reload_requested.store(true, Ordering::Release);
    wait_until_gateway_state(&inner, GatewayState::Disabled, None);

    enabled.store(true, Ordering::Release);
    inner.reload_requested.store(true, Ordering::Release);
    wait_until_connected(&inner);
    assert_eq!(lock(&inner.state.status).stopped_reason, None);

    inner.shutdown.store(true, Ordering::Release);
    gateway_thread.join().unwrap();
    recovery_server.join().unwrap();
}

#[test]
fn remote_stopped_loop_registry_publish_wake_retries_connection() {
    use std::io::{Read, Write};

    let tombstone_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tombstone_url = format!("ws://{}", tombstone_listener.local_addr().unwrap());
    let tombstone_server = thread::spawn(move || {
        let (mut stream, _) = tombstone_listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
    });
    let relay_url = Arc::new(Mutex::new(tombstone_url));
    let settings_relay_url = Arc::clone(&relay_url);
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let inner = Arc::new(Inner {
        settings: Box::new(move |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some(lock(&settings_relay_url).clone()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        }),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: Box::new(|_, _, _| Ok(ClaimResponse::Claimed)),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: Box::new(|_project_id| {
            Ok("0123456789abcdef0123456789abcdef".to_owned())
        }),
        k_room_provider: Box::new(|_| None),
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
        liveness_interval: Duration::from_millis(100),
    });
    let loop_inner = Arc::clone(&inner);
    let gateway_thread =
        thread::spawn(move || connect_loop(Arc::downgrade(&loop_inner), upstream_rx, milestone_rx));

    wait_until_stopped_reason(&inner, ROOM_TOMBSTONED_STOP_REASON);
    tombstone_server.join().unwrap();
    let (recovery_addr, recovery_server) = spawn_frame_pump_server();
    *lock(&relay_url) = format!("ws://{recovery_addr}");
    inner.registry_publish_wake.store(true, Ordering::Release);

    wait_until_connected(&inner);
    assert_eq!(lock(&inner.state.status).stopped_reason, None);
    assert!(!inner.reload_requested.load(Ordering::Acquire));

    inner.shutdown.store(true, Ordering::Release);
    gateway_thread.join().unwrap();
    recovery_server.join().unwrap();
}

#[test]
fn remote_registry_publish_wake_interrupts_backoff_without_settings_reload() {
    let inner = test_inner(|_| None, || None);
    inner.registry_publish_wake.store(true, Ordering::Release);

    let started = Instant::now();
    assert!(!interruptible_sleep(&inner, Duration::from_secs(60)));

    assert!(started.elapsed() < BACKOFF_POLL_INTERVAL);
    assert!(inner.registry_publish_wake.load(Ordering::Acquire));
    assert!(!inner.reload_requested.load(Ordering::Acquire));
}

#[test]
fn ensure_claim_409_with_devices_stops_without_regeneration() {
    let inner = test_inner_with_claim_handlers(|_, _, _| Ok(ClaimResponse::Conflict), |_| Ok(true));

    assert!(matches!(
        ensure_claim(
            &inner,
            &sample_gateway_config(),
            &DesktopCredential::new(Zeroizing::new("34".repeat(32))),
        ),
        ClaimAction::Stop(reason) if reason.code == ROOM_CLAIM_CONFLICT_STOP_REASON
    ));
}

/// M2-4d：单活跃房间模型下换房机制已撤——per-project 房间撞 conflict 且房内查无设备时，
/// 必须直接 Stop 专属码，不进任何"换房"自愈路径（那条路径连同 `room_regenerator`/
/// `ClaimAction::RoomRegenerated`/`MAX_ROOM_REGENERATIONS` 已整个删除，见 `ensure_claim`
/// 撤除说明）。
#[test]
fn ensure_claim_conflict_without_devices_stops_immediately() {
    let inner =
        test_inner_with_claim_handlers(|_, _, _| Ok(ClaimResponse::Conflict), |_| Ok(false));
    let config = GatewayConfig {
        relay_url: "wss://relay.example.com".to_owned(),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };

    assert!(matches!(
        ensure_claim(
            &inner,
            &config,
            &DesktopCredential::new(Zeroizing::new("56".repeat(32))),
        ),
        ClaimAction::Stop(reason) if reason.code == ROOM_CLAIM_CONFLICT_PROJECT_STOP_REASON
    ));
}

#[test]
fn ensure_claim_410_stops_as_tombstoned() {
    let inner =
        test_inner_with_claim_handlers(|_, _, _| Ok(ClaimResponse::Tombstoned), |_| unreachable!());

    assert!(matches!(
        ensure_claim(
            &inner,
            &sample_gateway_config(),
            &DesktopCredential::new(Zeroizing::new("56".repeat(32))),
        ),
        ClaimAction::Stop(reason) if reason.code == ROOM_TOMBSTONED_STOP_REASON
    ));
}

/// M2-4d：`ROOM_REGENERATION_FAILED_STOP_REASON`/`ROOM_REGENERATION_LIMIT_STOP_REASON` 两条
/// 分支已随换房机制一起删除（这两个字符串常量本身保留，见其定义处说明），这个测试原本
/// 覆盖的「无效房间号/换房失败」两种子情形不再可达，只剩「设备状态查询失败」这一条
/// 结构化码断言。
#[test]
fn ensure_claim_device_status_unavailable_stop_is_stable() {
    let credential = DesktopCredential::new(Zeroizing::new("90".repeat(32)));
    let config = sample_gateway_config();

    let device_status_unavailable = test_inner_with_claim_handlers(
        |_, _, _| Ok(ClaimResponse::Conflict),
        |_| Err("database unavailable".to_owned()),
    );
    assert!(matches!(
        ensure_claim(&device_status_unavailable, &config, &credential),
        ClaimAction::Stop(reason)
            if reason.code == ROOM_DEVICE_STATUS_UNAVAILABLE_STOP_REASON
    ));
}

#[test]
fn ensure_claim_429_returns_to_normal_backoff() {
    let inner = test_inner_with_claim_handlers(
        |_, _, _| Ok(ClaimResponse::RateLimited),
        |_| unreachable!(),
    );

    assert!(matches!(
        ensure_claim(
            &inner,
            &sample_gateway_config(),
            &DesktopCredential::new(Zeroizing::new("78".repeat(32))),
        ),
        ClaimAction::Backoff(message) if message.contains("429")
    ));
}

fn sample_gateway_config() -> GatewayConfig {
    GatewayConfig {
        relay_url: "wss://relay.example.com".to_owned(),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    }
}

fn test_inner_with_claim_handlers(
    claim_client: impl Fn(&str, &str, &str) -> Result<ClaimResponse, String> + Send + Sync + 'static,
    active_device_provider: impl Fn(&str) -> Result<bool, String> + Send + Sync + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    Arc::new(Inner {
        settings: Box::new(|_| None),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: Box::new(claim_client),
        active_device_provider: Box::new(active_device_provider),
        active_room_resolver: test_active_room_resolver(),
        k_room_provider: Box::new(|_| None),
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

fn test_inner_for_claim_cycle(
    relay_url: &str,
    claim_calls: Arc<std::sync::atomic::AtomicUsize>,
) -> Arc<Inner> {
    let relay_url = relay_url.to_owned();
    let settings_relay_url = relay_url.clone();
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    Arc::new(Inner {
        settings: Box::new(move |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some(settings_relay_url.clone()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        }),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: Box::new(move |_, _, _| {
            claim_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ClaimResponse::Claimed)
        }),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: Box::new(|_project_id| {
            Ok("0123456789abcdef0123456789abcdef".to_owned())
        }),
        k_room_provider: Box::new(|_| None),
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

fn wait_until_gateway_state(
    inner: &Inner,
    expected_state: GatewayState,
    expected_stopped_reason: Option<&str>,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let status = lock(&inner.state.status);
        if status.state == expected_state
            && status.stopped_reason.as_deref() == expected_stopped_reason
        {
            return;
        }
        drop(status);
        thread::sleep(Duration::from_millis(20));
    }
    panic!("gateway did not reach expected state within two seconds");
}
