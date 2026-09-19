#![cfg(test)]

use super::*;
#[test]
fn remote_registry_resync_pending_disconnects_within_hard_deadline_when_relay_stays_busy() {
    // S1i1 返工四 H1「太懒」钉死：rejected 之后 relay 持续以 < `READ_TIMEOUT`（500ms）的
    // 间隔投帧——这里用 unsolicited Pong，帧类型不影响结论（判据只认「这一轮读超时与否」，
    // 不区分 Text/Ping/Pong），`socket.read()` 因此永远不会超时、`read_timed_out_this_round`
    // 恒假。若断开判据漏掉硬截止分支（只剩 `read_timed_out_this_round`），这条连接会被
    // 持续喂着、永远不会主动断开——`join_connection_within_budget` 的预算耗尽会先于任何
    // 其它断言干净地失败。**变异自证**：把 H1 判据里的 `registry_resync_deadline_elapsed`
    // 这一项去掉，这条测试必须红。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let device_id = "88888888-8888-4888-8888-888888888888";
    let subject = format!("device:{device_id}");
    let new_generation = 13;
    let request_id = "req-busy-deadline-1";

    let server_subject = subject.clone();
    let server_request_id = request_id.to_owned();
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

        let fail_frame = recv_text_skip_control(&mut socket);
        assert_eq!(fail_frame["t"], "token.refresh.fail");
        assert_eq!(fail_frame["request_id"], server_request_id);

        // 持续以 100ms 间隔投未经请求的 Pong——总时长（60 * 100ms = 6 秒）比 2 秒硬截止
        // 长得多，逼真模拟「一直有帧到达、读永不超时」的繁忙连接。客户端一旦按硬截止
        // 主动断开，这里的 send 会因管道破裂报错，静默跳出即可——断言本体不靠这个提前
        // 退出，靠外面对 `join_connection_within_budget` 结果的检查。
        for _ in 0..60 {
            if socket.send(Message::Pong(Vec::new().into())).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
        assert!(
            closed,
            "繁忙连接也必须在硬截止内主动断开——不能被持续到达的帧无限期拖住"
        );
    });

    let provider_subject = subject.clone();
    let inner = test_inner_with_registry_providers(
        Box::new(move |_, _| {
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
            Some(&Zeroizing::new([6_u8; 32])),
        )
    });

    // 预算给足 3 秒：正确实现在 ~2 秒硬截止后很快断开（连接建立 + 处理 rejected 帧的
    // 前置耗时可忽略不计），3 秒预算留了充分余量；一旦硬截止分支被去掉，60 帧 * 100ms
    // = 6 秒的持续投帧在 3 秒预算耗尽那一刻仍在继续，`finished_in_time` 必然是 false。
    let result = join_connection_within_budget(connection, &inner, Duration::from_secs(3));
    assert_eq!(
        result,
        Err(ConnectionFailure::Other(
            "refresh put rejected; reconnecting to resync registry".to_owned()
        )),
        "繁忙连接最终仍必须走到可重试断开，且必须在硬截止预算内完成"
    );

    server.join().unwrap();
}

#[test]
fn remote_registry_resync_pending_ping_does_not_disconnect_before_pending_text() {
    // S1i1 返工四 H1「太急」钉死：rejected 之后 relay 先发一帧 Ping、紧接着（不等桌面
    // 任何响应）发一帧业务 Text（在线 input）——如果断开判据仍是返工三的
    // `!frame_delivered`，Ping 那一轮 `frame_delivered` 是假，会被当成「安静」立刻断开，
    // 永远读不到紧跟在后面、已经在缓冲区里的 Text 帧：`handle_frame` 从未被调用、没有
    // 本地落账、没有回 ack。修法把判据换成「这一轮 `socket.read()` 真的读超时了」——Ping
    // 那一轮不是超时，继续用同一套 `match` 正常处理，后面的 Text 帧必须被落账 + 回 ack。
    // **变异自证**：把判据改回 `!frame_delivered`（丢掉 `read_timed_out_this_round`
    // 语义），这条测试必须红。
    let k_room = Zeroizing::new([44_u8; 32]);
    let room_id = "0123456789abcdef0123456789abcdef".to_owned();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let device_id = "99999999-9999-4999-8999-999999999999";
    let subject = format!("device:{device_id}");
    let new_generation = 15;
    let request_id = "req-ping-not-quiet-1";
    let command_id = "cmd-ping-not-quiet-1";

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

        // 先发一帧 Ping，紧接着（不等桌面任何响应）发业务 input 帧——评审描述的「太急」
        // 场景：判据若仍按「Ping 这一轮没收到 Text」算安静，会在这里就地断开，永远读不到
        // 后面这帧 Text。
        socket.send(Message::Ping(Vec::new().into())).unwrap();
        let envelope = seal_command_envelope(
            &server_k_room,
            &server_room_id,
            7,
            "input",
            "s-ping",
            &server_command_id,
            &serde_json::json!({
                "t": "input.send",
                "session": "s-ping",
                "text": "hello after ping",
            }),
        );
        socket
            .send(Message::Text(envelope.to_string().into()))
            .unwrap();

        // 两帧都必须在同一条连接上被正常处理：先看到 fail（不带 close），再看到
        // input.ack——证明 Ping 那一轮没有触发过早断开。用 skip-control 读法容错客户端
        // 对服务端 Ping 的自动 Pong 回复可能夹在中间到达。
        let fail_frame = recv_text_skip_control(&mut socket);
        assert_eq!(fail_frame["t"], "token.refresh.fail");
        assert_eq!(fail_frame["request_id"], server_request_id);
        frames_tx.send(fail_frame).unwrap();

        let input_ack = recv_text_skip_control(&mut socket);
        frames_tx.send(input_ack).unwrap();

        // 不再送任何东西——桌面必须在这一轮真正安静下来之后（真读超时）才自己断开。
        let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
        assert!(
            closed,
            "处理完 Ping 和紧随其后的业务帧之后，桌面仍必须完成收敛断开"
        );
    });

    let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_calls_for_closure = provider_calls.clone();
    let provider_subject = subject.clone();
    let inner = test_inner_with_registry_providers(
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
        room_id: room_id.clone(),
        active_repo_id: None,
    };
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    let connection_inner = Arc::clone(&inner);
    let connection_k_room = k_room.clone();
    let connection = thread::spawn(move || {
        run_authenticated_connection(
            &connection_inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
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
        "Ping 之后本轮循环最终仍必须走到可重试断开"
    );

    let fail_frame = frames_rx.recv().unwrap();
    assert_eq!(fail_frame["t"], "token.refresh.fail");
    assert_eq!(fail_frame["reason"], "put_rejected");

    let input_ack = frames_rx.recv().unwrap();
    assert_eq!(
        input_ack["t"], "input.ack",
        "Ping 之后紧跟着的业务 input 帧必须被正常处理并回 input.ack——不能因为 Ping 那一轮 \
             被误判成安静就提前断开、永远读不到这帧"
    );
    assert_eq!(input_ack["command_id"], command_id);

    server.join().unwrap();
}
