#![cfg(test)]

use super::*;
#[test]
fn remote_legacy_active_device_with_null_registry_metadata_aborts_sync_before_send() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let room_id = "0123456789abcdef0123456789abcdef";
    let device_id = "11111111-1111-4111-8111-111111111111";
    crate::db::insert_remote_device(
        &conn,
        device_id,
        Some(room_id),
        "",
        &"aa".repeat(32),
        &"bb".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    let db = Arc::new(Mutex::new(conn));
    let db_for_snapshot = Arc::clone(&db);
    let inner = test_inner_with_registry_providers(
        Box::new(move |requested_room, now_ms| {
            crate::load_remote_registry_snapshot(&lock(&db_for_snapshot), requested_room, now_ms)
        }),
        Box::new(|_, _, _, _, _| panic!("rebase must not run")),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (received_tx, received_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        received_tx
            .send(matches!(socket.read(), Ok(Message::Text(_))))
            .unwrap();
    });
    let config = GatewayConfig {
        relay_url: format!("ws://{address}"),
        room_id: room_id.to_owned(),
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let result = run_authenticated_connection(
        &inner,
        &url,
        &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
        &config,
        None,
        &upstream_rx,
        &milestone_rx,
        Some(&Zeroizing::new([2_u8; 32])),
    );

    assert!(matches!(
        result,
        Err(ConnectionFailure::Other(ref error)) if error.contains("generation_missing")
    ));
    assert!(
        !received_rx.recv().unwrap(),
        "relay must receive no token.sync"
    );
    server.join().unwrap();
}

#[test]
fn remote_registry_mock_relay_high_water_rebases_and_resends_before_activation() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        for relay_high_water in [10_i64, 12_i64] {
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("sync must be text");
            };
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            frames_tx.send(frame.clone()).unwrap();
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": frame["revision"],
                        "relay_high_water": relay_high_water,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
        }
        let _ = socket.read();
    });
    let rebase_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rebase_call_counter = Arc::clone(&rebase_calls);
    let initial_entry = TokenSyncEntry {
        subject: "device:11111111-1111-4111-8111-111111111111".to_owned(),
        generation: 1,
        scope: "remote".to_owned(),
        current: TokenSyncCurrent {
            token_hash: "aa".repeat(32),
            access_expires: 1_765_434_000_000,
            refresh_until: Some(1_768_022_400_000),
        },
        prev: Some(TokenSyncPrev {
            token_hash: "bb".repeat(32),
            generation: 7,
            prev_expires: 1_765_606_800_000,
        }),
    };
    let initial_for_snapshot = initial_entry.clone();
    let initial_for_rebase = initial_entry.clone();
    let inner = test_inner_with_registry_providers(
        Box::new(move |_, _| {
            Ok(RegistrySnapshot {
                revision: 2,
                entries: vec![initial_for_snapshot.clone()],
            })
        }),
        Box::new(move |_, high_water, _, include_pairing, _| {
            assert_eq!(high_water, 10);
            assert!(!include_pairing);
            rebase_call_counter.fetch_add(1, Ordering::Relaxed);
            let mut entry = initial_for_rebase.clone();
            entry.generation = 11;
            Ok((
                RegistrySnapshot {
                    revision: 12,
                    entries: vec![entry],
                },
                None,
                Vec::new(),
            ))
        }),
    );
    let config = GatewayConfig {
        relay_url: format!("ws://{address}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection_config = config.clone();
    let connection = thread::spawn(move || {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connection_config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([3_u8; 32])),
        )
    });

    wait_until_connected(&inner);
    assert!(inner.state.upstream_enabled_snapshot());
    let first = frames_rx.recv().unwrap();
    let second = frames_rx.recv().unwrap();
    assert_eq!(first["revision"], 2);
    assert_eq!(first["entries"][0]["generation"], 1);
    assert_eq!(second["revision"], 12);
    assert_eq!(second["entries"][0]["generation"], 11);
    assert_eq!(second["entries"][0]["prev"]["generation"], 7);
    assert_eq!(rebase_calls.load(Ordering::Relaxed), 1);

    inner.shutdown.store(true, Ordering::Release);
    assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
    server.join().unwrap();
}

#[test]
fn remote_registry_ack_equal_revision_is_absorbed_before_activation() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release_ack_tx, release_ack_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("sync must be text");
        };
        let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        release_ack_rx.recv().unwrap();
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": frame["revision"],
                    "relay_high_water": 5,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        let _ = socket.read();
    });
    let next_generation = Arc::new(AtomicU64::new(5));
    let counter_for_ack = Arc::clone(&next_generation);
    let snapshot_provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_provider = Arc::clone(&snapshot_provider_calls);
    let inner = test_inner_with_registry_sync_providers(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 5,
                entries: Vec::new(),
            })
        }),
        Box::new(|_, _, _, _, _| panic!("rebase must not run")),
        Box::new(move |_, high_water, _revoke_subjects: &[String]| {
            counter_for_ack.fetch_max((high_water + 1) as u64, Ordering::AcqRel);
            Ok(Vec::new())
        }),
        Box::new(move || {
            calls_for_provider.fetch_add(1, Ordering::Relaxed);
            None
        }),
    );
    let config = GatewayConfig {
        relay_url: format!("ws://{address}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection_config = config.clone();
    let connection = thread::spawn(move || {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connection_config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([4_u8; 32])),
        )
    });

    thread::sleep(Duration::from_millis(50));
    assert_ne!(lock(&inner.state.status).state, GatewayState::Connected);
    assert!(!inner.state.upstream_enabled_snapshot());
    assert_eq!(snapshot_provider_calls.load(Ordering::Relaxed), 0);
    release_ack_tx.send(()).unwrap();
    wait_until_connected(&inner);
    assert!(inner.state.upstream_enabled_snapshot());
    let claimed_generation = next_generation.fetch_add(1, Ordering::AcqRel);
    assert!(claimed_generation > 5);

    inner.shutdown.store(true, Ordering::Release);
    assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
    server.join().unwrap();
}

#[test]
fn remote_registry_high_water_out_of_range_stops_without_persisting() {
    // S1i3 F2：relay 若回一个远超本地计数器的 relay_high_water（半可信/故障 relay，
    // 或纯粹的协议误用），必须 fail-closed 停机、绝不吸收进桌面计数器——否则
    // `bump_registry_counter_to_in_transaction` 会把 `next_generation` 抬到接近
    // `i64::MAX`，后续每次真正领号都会在 `checked_add(1)` 上溢出、永久
    // `IntegralValueOutOfRange`（配对开不了、设备撤不掉、refresh 全死），换回诚实
    // relay 也不恢复（损坏已经落进桌面 DB）。两个 panic 探针（rebase provider /
    // high_water provider）不是断言手段，是硬性前提：只要越界检查没有在两者之前
    // 拦下，这条测试本身就会因为探针触发而失败，比事后查询 DB 计数器更直接地证明
    // 「绝不落库」。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("initial sync must be text");
        };
        let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(frame["revision"], 100);
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": frame["revision"],
                    "relay_high_water": 100_i64 + REGISTRY_HIGH_WATER_MAX_SPAN + 1,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        // 桌面判定越界后应当 fail-closed 放弃这条连接，不会再发别的帧——等它挂断
        // （读到错误/EOF）即可，不需要断言具体错误种类。
        let _ = socket.read();
    });

    let inner = test_inner_with_registry_sync_providers(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 100,
                entries: Vec::new(),
            })
        }),
        Box::new(|_, _, _, _, _| {
            panic!("relay_high_water 越界必须在触发任何 rebase 之前就 fail-closed")
        }),
        Box::new(|_, high_water, _revoke_subjects: &[String]| {
            panic!("relay_high_water 越界（{high_water}）必须 fail-closed，绝不能吸收进桌面计数器")
        }),
        Box::new(|| None),
    );
    let config = GatewayConfig {
        relay_url: format!("ws://{address}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let result = run_authenticated_connection(
        &inner,
        &url,
        &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
        &config,
        None,
        &upstream_rx,
        &milestone_rx,
        Some(&Zeroizing::new([9_u8; 32])),
    );

    match result {
        Err(ConnectionFailure::Stopped(reason)) => {
            assert_eq!(reason.code, REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON);
        }
        other => panic!("expected Stopped(registry_high_water_out_of_range)，got {other:?}"),
    }
    server.join().unwrap();
}

#[test]
fn remote_terminal_stop_with_unacked_outbox_keeps_wait_for_reload_asleep() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let reconnect_probe_listener = listener.try_clone().unwrap();
    let accepted = Arc::new(AtomicU64::new(0));
    let server_accepted = Arc::clone(&accepted);
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        server_accepted.fetch_add(1, Ordering::Relaxed);
        let mut socket = tungstenite::accept(stream).unwrap();
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("initial sync must be text");
        };
        let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(frame["revision"], 100);
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": frame["revision"],
                    "relay_high_water": 100_i64 + REGISTRY_HIGH_WATER_MAX_SPAN + 1,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        // 桌面判定越界后应当 fail-closed 放弃这条连接，不会再发别的帧——等它挂断
        // （读到错误/EOF）即可，不需要断言具体错误种类。
        let _ = socket.read();
    });

    let relay_url = format!("ws://{address}");
    let settings_relay_url = relay_url.clone();
    let inner = test_inner_with_registry_sync_providers_and_settings(
        move |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some(settings_relay_url.clone()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        },
        Box::new(|_project_id| Ok("0123456789abcdef0123456789abcdef".to_owned())),
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 100,
                entries: Vec::new(),
            })
        }),
        Box::new(|_, _, _, _, _| panic!("terminal high-water stop must not rebase")),
        Box::new(|_, _, _| panic!("terminal high-water stop must not persist")),
        Box::new(|| None),
    );
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
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let loop_inner = Arc::clone(&inner);
    let gateway_thread =
        thread::spawn(move || connect_loop(Arc::downgrade(&loop_inner), upstream_rx, milestone_rx));

    wait_until_stopped_reason(&inner, REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON);
    server.join().unwrap();
    let (reconnect_accepted_tx, reconnect_accepted_rx) = mpsc::sync_channel(1);
    let (release_reconnect_probe_tx, release_reconnect_probe_rx) = mpsc::sync_channel(1);
    let reconnect_probe = thread::spawn(move || {
        let _connection = reconnect_probe_listener.accept().unwrap();
        reconnect_accepted_tx.send(()).unwrap();
        release_reconnect_probe_rx.recv().unwrap();
    });
    let reconnected = reconnect_accepted_rx
        .recv_timeout(BACKOFF_POLL_INTERVAL + Duration::from_millis(100))
        .is_ok();
    let accepted_count = accepted.load(Ordering::Relaxed) + u64::from(reconnected);
    let wake_rearmed = inner.registry_publish_wake.load(Ordering::Acquire);
    let stopped_reason = lock(&inner.state.status).stopped_reason.clone();

    inner.shutdown.store(true, Ordering::Release);
    if !reconnected {
        let _ = std::net::TcpStream::connect(address).unwrap();
    }
    release_reconnect_probe_tx.send(()).unwrap();
    reconnect_probe.join().unwrap();
    gateway_thread.join().unwrap();

    assert_eq!(
        accepted_count, 1,
        "terminal Stop must remain in wait_for_reload instead of reconnecting"
    );
    assert!(
        !wake_rearmed,
        "an unacked outbox must not rearm wake for a terminal Stop"
    );
    assert_eq!(
        stopped_reason.as_deref(),
        Some(REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON)
    );
}

#[test]
fn remote_registry_high_water_at_max_span_boundary_is_absorbed_not_stopped() {
    // S1i3 K3.3：上面那条测试钉死了 `revision + REGISTRY_HIGH_WATER_MAX_SPAN + 1`
    // fail-closed；这条补另一半——恰好等于上界（`+1` 之前那个值）必须放行、正常吸收。
    // 判定用的是 `>`，不是 `>=`：只测「超一点必挂」不能防住有人手滑把 `>` 改成
    // `>=`（那样恰好等于上界也会被拦，仍然全绿）。用 registry_high_water_provider
    // 断言实际吸收到的 high_water 就是边界值本身，证明代码走过了那一行判断，不是
    // 靠事后猜。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let boundary_high_water = 100_i64 + REGISTRY_HIGH_WATER_MAX_SPAN; // 恰好等于上界
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        // 第一轮 sync：relay 报回恰好等于上界的 relay_high_water——必须被吸收，不停机。
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("initial sync must be text");
        };
        let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(frame["revision"], 100);
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": frame["revision"],
                    "relay_high_water": boundary_high_water,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        // relay_high_water（边界值）> revision（100），桌面必须再发一轮 rebase
        // sync；这一轮直接把 relay_high_water 报成跟新 revision 相等，让循环收敛、
        // 连接进入 Connected 态，不需要再模拟第三轮。
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("rebase sync must be text");
        };
        let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(frame["revision"], boundary_high_water);
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": frame["revision"],
                    "relay_high_water": boundary_high_water,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        let _ = socket.read();
    });

    let absorbed_high_water = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let absorbed_for_provider = Arc::clone(&absorbed_high_water);
    let inner = test_inner_with_registry_providers_and_high_water(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 100,
                entries: Vec::new(),
            })
        }),
        Box::new(move |_, high_water, _, include_pairing, _| {
            assert_eq!(high_water, boundary_high_water);
            assert!(!include_pairing);
            Ok((
                RegistrySnapshot {
                    revision: boundary_high_water,
                    entries: Vec::new(),
                },
                None,
                Vec::new(),
            ))
        }),
        Box::new(move |_, high_water, _revoke_subjects: &[String]| {
            // 每次吸收都会调用；第一轮吸收的正是边界值本身——这才是本测试真正要
            // 证明的事：`>` 判定放行了它，代码走到了这里而不是提前 fail-closed
            // 返回（对照上一条测试：越界值会在这里 panic，走不到这一行）。
            absorbed_for_provider.store(high_water, Ordering::Relaxed);
            Ok(Vec::new())
        }),
    );

    let config = GatewayConfig {
        relay_url: format!("ws://{address}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let connection_inner = Arc::clone(&inner);
    let connection_config = config.clone();
    let connection = thread::spawn(move || {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connection_config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([9_u8; 32])),
        )
    });

    wait_until_connected(&inner);
    assert!(inner.state.upstream_enabled_snapshot());
    assert_eq!(
        absorbed_high_water.load(Ordering::Relaxed),
        boundary_high_water,
        "边界值必须被吸收进桌面计数器，而不是被上界判定拦下"
    );

    inner.shutdown.store(true, Ordering::Release);
    let _ = connection.join().unwrap();
    server.join().unwrap();
}
