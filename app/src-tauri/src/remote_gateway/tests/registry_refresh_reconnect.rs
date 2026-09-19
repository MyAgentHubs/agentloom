#![cfg(test)]

use super::*;
#[test]
fn remote_registry_reconnect_resends_unacked_put_after_sync_then_releases_ready() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        ack_initial_registry_sync(&mut socket);
        for _ in 0..2 {
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("registry frame must be text")
            };
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            frames_tx.send(frame.clone()).unwrap();
            if frame["t"] == "token.put" {
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "t": "token.ack",
                            "subject": frame["subject"],
                            "generation": frame["generation"],
                            "result": "idempotent",
                        })
                        .to_string()
                        .into(),
                    ))
                    .unwrap();
            }
        }
        let _ = socket.close(None);
    });
    let inner = test_inner_with_registry_providers(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 7,
                entries: Vec::new(),
            })
        }),
        test_registry_rebase_provider(),
    );
    let device_id = "11111111-1111-4111-8111-111111111111";
    lock(&inner.registry).enqueue_token_put(
        TokenSyncEntry {
            subject: format!("device:{device_id}"),
            generation: 7,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        Some(PairReadyFrame {
            room: "0123456789abcdef0123456789abcdef".to_owned(),
            device_id: device_id.to_owned(),
            ct: "ready-ct".to_owned(),
            n: "ready-n".to_owned(),
        }),
    );
    lock(&inner.registry).outbox[0].attempts = 1;
    lock(&inner.registry).outbox[0].last_sent_at = Some(1);
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
        Some(&Zeroizing::new([4_u8; 32])),
    );

    assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
    assert_eq!(frames_rx.recv().unwrap()["t"], "token.put");
    assert_eq!(frames_rx.recv().unwrap()["t"], "pair.ready");
    server.join().unwrap();
}

#[test]
fn remote_registry_rejected_refresh_put_disconnects_so_reconnect_carries_db_truth() {
    // S1i1 返工三：refresh 轮换成功后挂着回执的 put 被 relay 判 `rejected`——桌面 DB
    // 已经不可逆地轮换到新代号（下面 provider 第二次调用起返回新代号 9），relay 那边还停在
    // 旧代号（第一次调用返回代号 5，就是这条连接建立时 relay 学到的状态）。返工二在这条
    // 存活连接里原地重发一次 token.sync 换收敛，被评审判定 BLOCKER（等 ack 期间会把 relay
    // 直投的在线 input 帧当协议违规吞掉，本文件另一条回归测试专门钉这一点）。返工三改为：
    // 回一帧 fail 之后主动断开，让**下一次连接**（既有重连路径）的首次 sync 把 DB 真相
    // 交给 relay，之后手机同 request_id 的重试才能命中 relay §9.6 第 246 行「回执.generation
    // == subject 当前 generation」的投递谓词。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let device_id = "66666666-6666-4666-8666-666666666666";
    let subject = format!("device:{device_id}");
    let new_generation = 9;
    let request_id = "req-resync-1";

    let server_subject = subject.clone();
    let server_request_id = request_id.to_owned();
    let server = thread::spawn(move || {
        // ── 第一条连接：refresh 轮换的 put 被判 rejected ──
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        // 连接建立时的首次 sync：relay 学到 subject 还停在旧代号 5。
        ack_initial_registry_sync(&mut socket);

        // 首次 sync 之后，outbox 里挂着 refresh 回执的 put（代号 9，来自
        // enqueue_token_put_for_refresh，见下方 registry 预置）被正常 drain 出来。
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("token.put must be text");
        };
        let put_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(put_frame["t"], "token.put");
        assert_eq!(put_frame["subject"], server_subject);
        assert_eq!(put_frame["generation"], new_generation);
        frames_tx.send(put_frame).unwrap();

        // relay 判 rejected（模拟它手上仍是代号 5 的旧注册表，拒绝了这次代号 9 的 put）。
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.ack",
                    "subject": server_subject,
                    "generation": new_generation,
                    "result": "rejected",
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        // 桌面回一帧 fail{put_rejected}（不带 close）——R2 既有行为，本轮不许削弱。
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("refresh fail must be text");
        };
        let fail_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(fail_frame["t"], "token.refresh.fail");
        assert_eq!(fail_frame["reason"], "put_rejected");
        assert_eq!(fail_frame["request_id"], server_request_id);
        assert!(
            fail_frame.get("close").is_none(),
            "R2 既有行为：put_rejected 自愈帧不带 close，不许被本轮削弱"
        );
        frames_tx.send(fail_frame).unwrap();

        // 返工三本体：桌面**不**在这条连接上重发 sync——relay 侧再读不到任何后续应用帧，
        // 只会看到桌面主动断开这条连接（EOF/关闭），不是收到一帧第二次 sync。
        let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
        assert!(
            closed,
            "返工三：处理完 rejected 的 refresh put 之后必须主动断开这条连接，\
                 不能在原地等第二次 sync.ack"
        );

        // ── 第二条连接：既有重连路径的首次 sync 必须带上 DB 当前真相（新代号）──
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let resync_frame = ack_initial_registry_sync(&mut socket);
        frames_tx.send(resync_frame).unwrap();
        let _ = socket.close(None);
    });

    let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_calls_for_closure = provider_calls.clone();
    let provider_subject = subject.clone();
    let inner = test_inner_with_registry_providers(
        Box::new(move |_, _| {
            let call = provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                // 第一条连接建立时的首次 sync：DB 快照仍是这次 refresh 发生之前的旧状态。
                Ok(RegistrySnapshot {
                    revision: 7,
                    entries: vec![TokenSyncEntry {
                        subject: provider_subject.clone(),
                        generation: 5,
                        scope: "remote".to_owned(),
                        current: TokenSyncCurrent {
                            token_hash: "aa".repeat(32),
                            access_expires: 1_765_000_000_000,
                            refresh_until: Some(1_768_000_000_000),
                        },
                        prev: None,
                    }],
                })
            } else {
                // 第二条连接（重连）建立时的首次 sync：provider 重新读 DB，这次看到的
                // 已经是轮换后的真相（代号 9）——真实实现里 provider 就是读当前 DB 行，
                // 这里用调用计数模拟「两次连接之间 DB 状态推进了」。
                Ok(RegistrySnapshot {
                    revision: new_generation,
                    entries: vec![TokenSyncEntry {
                        subject: provider_subject.clone(),
                        generation: new_generation,
                        scope: "remote".to_owned(),
                        current: TokenSyncCurrent {
                            token_hash: "bb".repeat(32),
                            access_expires: 1_765_434_000_000,
                            refresh_until: Some(1_768_022_400_000),
                        },
                        prev: None,
                    }],
                })
            }
        }),
        test_registry_rebase_provider(),
    );

    lock(&inner.registry).enqueue_token_put_for_refresh(
        TokenSyncEntry {
            subject: subject.clone(),
            generation: new_generation,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "bb".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        RefreshOkFrame {
            request_id: request_id.to_owned(),
            subject: subject.clone(),
            generation: new_generation,
            ct: "refresh-ct".to_owned(),
            n: "refresh-n".to_owned(),
        },
    );

    let config = GatewayConfig {
        relay_url: format!("ws://{address}"),
        room_id: "0123456789abcdef0123456789abcdef".to_owned(),
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);

    // 第一条连接：处理 rejected 的 refresh put 之后必须在有限时间内主动断开——用
    // `join_connection_within` 把「连接是否真的结束」变成一条硬断言，不靠超时/panic 兜底
    // （G2 变异自证：把下方收敛动作临时改回 no-op，这条 `assert!(finished_in_time, ...)`
    // 会先于其它断言干净地失败）。
    let (_upstream_tx_1, upstream_rx_1) = mpsc::sync_channel(1);
    let (_milestone_tx_1, milestone_rx_1) = mpsc::sync_channel(1);
    let connection_inner = Arc::clone(&inner);
    let connection_url = url.clone();
    let connection_config = config.clone();
    let connection = thread::spawn(move || {
        run_authenticated_connection(
            &connection_inner,
            &connection_url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connection_config,
            None,
            &upstream_rx_1,
            &milestone_rx_1,
            Some(&Zeroizing::new([6_u8; 32])),
        )
    });
    let result = join_connection_within(connection, &inner);
    assert_eq!(
        result,
        Err(ConnectionFailure::Other(
            "refresh put rejected; reconnecting to resync registry".to_owned()
        )),
        "rejected 且挂着 refresh 回执的 put 之后必须是一次可重试的断开（Other），\
             不能变成 Stopped 那种硬停，也不能在原地等第二次 ack"
    );

    let put_frame = frames_rx.recv().unwrap();
    assert_eq!(put_frame["t"], "token.put");

    let fail_frame = frames_rx.recv().unwrap();
    assert_eq!(fail_frame["t"], "token.refresh.fail");

    // 第二条连接：既有重连路径（`attempt_once` 重新调 `run_connection_request`）的首次
    // sync 必须带上 DB 当前真相（新代号），不是第一条连接建立时那次 sync 还带着的旧代号。
    let (_upstream_tx_2, upstream_rx_2) = mpsc::sync_channel(1);
    let (_milestone_tx_2, milestone_rx_2) = mpsc::sync_channel(1);
    let connection_inner_2 = Arc::clone(&inner);
    let connection_url_2 = url.clone();
    let connection_config_2 = config.clone();
    let connection_2 = thread::spawn(move || {
        run_authenticated_connection(
            &connection_inner_2,
            &connection_url_2,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connection_config_2,
            None,
            &upstream_rx_2,
            &milestone_rx_2,
            Some(&Zeroizing::new([6_u8; 32])),
        )
    });
    let result_2 = join_connection_within(connection_2, &inner);
    assert_eq!(result_2, Ok(ConnectionExit::ClosedByPeer));

    let resync_frame = frames_rx.recv().unwrap();
    assert_eq!(
        resync_frame["t"], "token.sync",
        "断开之后，下一次连接的首次 sync 就是既有重连路径本身，必须真实发生"
    );
    assert_eq!(resync_frame["revision"], new_generation);
    let entries = resync_frame["entries"].as_array().unwrap();
    let synced_entry = entries
        .iter()
        .find(|entry| entry["subject"] == subject)
        .expect("reconnect sync entries must include the rotated subject");
    assert_eq!(
        synced_entry["generation"], new_generation,
        "重连后的 token.sync 必须携带 DB 当前代号（轮换后的新代号），\
             不能还是第一条连接建立时那次 sync 的旧代号"
    );
    assert!(
        provider_calls.load(Ordering::Relaxed) >= 2,
        "重连必须真的再打一次 registry_snapshot_provider（重新读 DB 真相），\
             不能复用第一条连接缓存的旧快照"
    );

    server.join().unwrap();
}

#[test]
fn remote_registry_rejected_refresh_put_disconnect_does_not_drop_relay_pushed_input() {
    // S1i1 返工三丢帧回归测试（评审点名要求）：relay 判 rejected 之后、不等桌面任何响应，
    // 紧接着直投一帧在线 input——这正是评审描述的「插在收敛动作之间」的危险位置。返工二
    // 的老实现在这里会去原地等第二次 sync.ack，这帧会被 `read_registry_sync_ack` 当协议
    // 违规吞掉、永久静默丢失（从未进 `handle_frame`、没有本地落账、没有回 ack）。返工三
    // 的新实现只用同一套 `match socket.read()` 分发继续正常处理——这帧必须被正常处理
    // （落账 + 回 input.ack），断开只发生在这一轮真正安静下来之后。这条测试就是钉死
    // 「不要再回到原地等 ack」这个结论的核心验收。
    //
    // S1i1 返工四 H2：光凭「收到 command_id 匹配的 input.ack」分不清「真的解密并派发到
    // 业务 handler」与「解密/解析就先失败了」——两条路径都会产出同一个 command_id 的
    // `input.ack`（解密/解析失败见 `handle_command_envelope` 的 `failed()` 早退，固定回
    // `AckOutcome::Failed`；默认 fixture handler 本身也固定返回 `Some(AckOutcome::Failed)`，
    // 两者长得一模一样）。这里改用可注入的 fixture handler，记调用次数 + 收到的
    // command_id/text，并返回一个跟失败路径可区分的 outcome（`AckOutcome::Ok`，
    // `input.ack.outcome` 会是 `"ok"` 而不是解密失败路径固定吐出的 `"failed"`）。
    let k_room = Zeroizing::new([42_u8; 32]);
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let device_id = "77777777-7777-4777-8777-777777777777";
    let subject = format!("device:{device_id}");
    let new_generation = 11;
    let request_id = "req-input-race-1";
    let command_id = "cmd-input-race-1";

    let server_subject = subject.clone();
    let server_request_id = request_id.to_owned();
    let server_room_id = room_id.clone();
    let server_k_room = k_room.clone();
    let server_command_id = command_id.to_owned();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        ack_initial_registry_sync(&mut socket);

        let Message::Text(text) = socket.read().unwrap() else {
            panic!("token.put must be text");
        };
        let put_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(put_frame["t"], "token.put");
        assert_eq!(put_frame["subject"], server_subject);
        assert_eq!(put_frame["generation"], new_generation);

        // relay 判 rejected——桌面即将进入「该收敛了」的状态。
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.ack",
                    "subject": server_subject,
                    "generation": new_generation,
                    "result": "rejected",
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        // 不等桌面任何回应，紧接着直投一帧在线 input——正是评审描述的「插在收敛动作之间」
        // 的位置。
        let envelope = seal_command_envelope(
            &server_k_room,
            &server_room_id,
            7,
            "input",
            "s-race",
            &server_command_id,
            &serde_json::json!({
                "t": "input.send",
                "session": "s-race",
                "text": "hello from race",
            }),
        );
        socket
            .send(Message::Text(envelope.to_string().into()))
            .unwrap();

        // 两帧都必须在同一条连接上被正常处理：先看到 fail（不带 close），再看到
        // input.ack——证明「正要收敛」的这一刻，relay 紧跟着送来的帧没有被当协议违规吞掉。
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("refresh fail must be text");
        };
        let fail_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(fail_frame["request_id"], server_request_id);
        frames_tx.send(fail_frame).unwrap();

        let Message::Text(text) = socket.read().unwrap() else {
            panic!("input ack must be text");
        };
        let input_ack: Value = serde_json::from_str(text.as_ref()).unwrap();
        frames_tx.send(input_ack).unwrap();

        // 不再送任何东西——桌面必须在这一轮真正安静下来之后自己断开（既有重连路径接手）。
        let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
        assert!(
            closed,
            "处理完 relay 插进来的 input 帧之后，桌面仍必须完成收敛断开——不能因为多处理了\
                 一帧就卡住不断"
        );
    });

    let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_calls_for_closure = provider_calls.clone();
    let provider_subject = subject.clone();
    // S1i1 返工四 H2：记业务 handler 真实被调用的次数与收到的内容——跟失败路径的固定
    // `Failed` outcome区分开。
    let input_handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let input_handler_calls_for_closure = input_handler_calls.clone();
    let input_handler_seen: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let input_handler_seen_for_closure = input_handler_seen.clone();
    let inner = test_inner_with_registry_providers_and_input_handler(
        Box::new(move |_, _| {
            let call = provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
            let (revision, entry_generation): (i64, i64) = if call == 0 {
                (7, 5)
            } else {
                (new_generation, new_generation)
            };
            Ok(RegistrySnapshot {
                revision,
                entries: vec![TokenSyncEntry {
                    subject: provider_subject.clone(),
                    generation: entry_generation,
                    scope: "remote".to_owned(),
                    current: TokenSyncCurrent {
                        token_hash: "aa".repeat(32),
                        access_expires: 1_765_000_000_000,
                        refresh_until: Some(1_768_000_000_000),
                    },
                    prev: None,
                }],
            })
        }),
        test_registry_rebase_provider(),
        move |frame: InputSendFrame| {
            input_handler_calls_for_closure.fetch_add(1, Ordering::Relaxed);
            *lock(&input_handler_seen_for_closure) =
                Some((frame.command_id.clone(), frame.text.clone()));
            Some(AckOutcome::Ok)
        },
    );

    lock(&inner.registry).enqueue_token_put_for_refresh(
        TokenSyncEntry {
            subject: subject.clone(),
            generation: new_generation,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "bb".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        RefreshOkFrame {
            request_id: request_id.to_owned(),
            subject: subject.clone(),
            generation: new_generation,
            ct: "refresh-ct".to_owned(),
            n: "refresh-n".to_owned(),
        },
    );

    // M2-4d：`run_connection_request` 建连时会用这份 config 的 active_repo_id 覆盖
    // `active_repo_id_for_gating`（`with_default_active_repo` 建的初值会被这里盖掉），必须
    // 跟 builder 那份默认值一致，不然 "s-race" 的 input 帧会被归属闸 fail-closed 挡下。
    let config = GatewayConfig {
        relay_url: format!("ws://{address}"),
        room_id: room_id.clone(),
        active_repo_id: Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned()),
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let connection_inner = Arc::clone(&inner);
    let connection_url = url.clone();
    let connection_config = config.clone();
    let connection_k_room = k_room.clone();
    let connection = thread::spawn(move || {
        run_authenticated_connection(
            &connection_inner,
            &connection_url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &connection_config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&connection_k_room),
        )
    });
    let result = join_connection_within(connection, &inner);
    assert_eq!(
        result,
        Err(ConnectionFailure::Other(
            "refresh put rejected; reconnecting to resync registry".to_owned()
        )),
        "本轮循环最终仍必须走到可重试断开——多处理一帧 input 不该改变收敛结论"
    );

    let fail_frame = frames_rx.recv().unwrap();
    assert_eq!(fail_frame["t"], "token.refresh.fail");
    assert_eq!(fail_frame["reason"], "put_rejected");
    assert!(
        fail_frame.get("close").is_none(),
        "put_rejected 自愈帧不带 close，不许被本轮削弱"
    );

    let input_ack = frames_rx.recv().unwrap();
    assert_eq!(
        input_ack["t"], "input.ack",
        "relay 紧跟着 rejected ack 直投的在线 input 帧，必须像平时一样落账并回 \
             input.ack——不能因为桌面正要收敛就静默丢弃"
    );
    assert_eq!(input_ack["command_id"], command_id);
    // S1i1 返工四 H2：outcome 必须是业务 handler 返回的 "ok"，不是解密/解析失败路径
    // （`handle_command_envelope` 的 `failed()` 早退）固定吐出的 "failed"——否则这条
    // ack 分不清是「真的派发到业务 handler」还是「半路解密就失败了」。
    assert_eq!(
        input_ack["outcome"], "ok",
        "input.ack 的 outcome 必须是业务 handler 返回的值，不能跟解密失败路径撞成一样的 \
             \"failed\""
    );
    assert_eq!(
        input_handler_calls.load(Ordering::Relaxed),
        1,
        "业务 handler（input_send_handler）必须真的被调用恰好一次"
    );
    assert_eq!(
        lock(&input_handler_seen).clone(),
        Some((command_id.to_owned(), "hello from race".to_owned())),
        "业务 handler 收到的 command_id/text 必须与 relay 投递的一致"
    );

    server.join().unwrap();
}

/// S1i1 H2：跟 `test_inner_with_registry_providers` 同一套 registry provider 组合，
/// 但 `input_send_handler` 可由调用方注入——用来在丢帧回归测试里证明业务 handler
/// 真的被调用，而不是靠固定返回 `AckOutcome::Failed` 的默认 fixture（那样分不清
/// 「真的解密并派发」与「解密/解析就先失败了」，两条路径产出的 `input.ack` 长得一样）。
fn test_inner_with_registry_providers_and_input_handler(
    registry_snapshot_provider: RegistrySnapshotProvider,
    registry_rebase_provider: RegistryRebaseProvider,
    input_send_handler: impl Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    with_default_active_repo(Arc::new(Inner {
        settings: Box::new(|_| None),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: test_active_room_resolver(),
        k_room_provider: Box::new(|_| Some(Zeroizing::new([9_u8; 32]))),
        session_index_snapshot_provider: Box::new(|| None),
        milestone_replay_provider: Box::new(|| None),
        session_runtime_replay_provider: Box::new(|| None),
        pair_hello_handler: Box::new(|_| None),
        pair_done_handler: Box::new(|_| PairDoneAction::Rejected),
        registry: test_registry(),
        refresh_handler: test_refresh_handler(),
        registry_snapshot_provider,
        registry_rebase_provider,
        registry_high_water_provider: test_registry_high_water_provider(),
        input_send_handler: Box::new(input_send_handler),
        input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(|_| AckOutcome::Failed),
        upstream_tx,
        milestone_tx,
        session_repo_provider: test_session_repo_provider_allowing_default_repo(),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    }))
}
