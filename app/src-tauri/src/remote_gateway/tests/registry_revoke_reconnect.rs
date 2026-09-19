#![cfg(test)]

use super::*;
#[test]
fn remote_registry_absorb_floor_is_fail_closed_when_relay_high_water_below_revision() {
    // S1h 返工二 F2：`relay_high_water` 协议上没有下界校验——`read_registry_sync_ack` 只
    // 查 `>= 0`。一个协议外的 relay 若回了比本次 sync revision 还低的 H，
    // `synchronize_registry` 传给 `absorb_registry_high_water_and_rearm_revokes` 的 floor
    // 不能原样信它，必须跟 `snapshot.revision` 取 max，否则会在本地计数器等于 revision
    // 时把新代号退化成等于 revision，撞上 relay「同代号比 fingerprint」分支，永久
    // rejected。这里 revision(10) > relay_high_water(8)：H(8) <= revision(10) 让循环在
    // 第一轮就返回（不牵扯 rebase，rebase provider 故意 panic 钉死这一点），新代号必须
    // 严格大于 revision（10），不能只满足严格大于 relay_high_water（8+1=9 会被下面的
    // 断言当场抓到——那正是 floor 没有跟 revision 取 max 时会产出的值）。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();

        let Message::Text(text) = socket.read().unwrap() else {
            panic!("sync must be text");
        };
        let sync_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(sync_frame["revision"], 10);
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": sync_frame["revision"],
                    "relay_high_water": 8,
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        let Message::Text(text) = socket.read().unwrap() else {
            panic!("delete resend must be text");
        };
        let delete_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        frames_tx.send(delete_frame.clone()).unwrap();
        let _ = socket.close(None);
    });

    let device_id = "44444444-4444-4444-8444-444444444444";
    let subject = format!("device:{device_id}");
    let inner = test_inner_with_registry_providers_and_high_water(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 10,
                entries: Vec::new(),
            })
        }),
        Box::new(|_, _, _, _, _| {
            panic!("rebase must not run: relay_high_water(8) <= revision(10)")
        }),
        Box::new(|_, high_water, revoke_subjects: &[String]| {
            Ok(revoke_subjects
                .iter()
                .map(|subject| (subject.clone(), high_water + 1))
                .collect())
        }),
    );
    lock(&inner.registry).enqueue_token_delete(subject.clone(), 3, true);

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
        Some(&Zeroizing::new([7_u8; 32])),
    );

    assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
    let delete_frame = frames_rx.recv().unwrap();
    assert_eq!(delete_frame["t"], "token.delete");
    assert_eq!(delete_frame["subject"], subject);
    let sent_generation = delete_frame["generation"].as_i64().unwrap();
    assert!(
        sent_generation > 10,
        "relay_high_water（8）< revision（10）时新代号仍必须严格大于 revision，\
             实际 {sent_generation}"
    );
    assert_ne!(
        sent_generation, 9,
        "9 = relay_high_water(8)+1，意味着 floor 没有跟 revision 取 max——正是本刀要堵的\
             fail-open 回归"
    );
    server.join().unwrap();
}

#[test]
fn remote_registry_rejected_revoke_rearmed_and_resent_after_reconnect_sync_ack() {
    // S1h 返工二 F3：§9.3「revoke 独立重试直到 ack」在跨重连这一半——delete 被 relay 回
    // rejected 后不是永久停发；下一次连接的 sync.ack 后（`absorb_registry_high_water_and_
    // rearm_revokes`，每轮 sync.ack 后都会跑，不需要触发 rebase）必须被重新武装（rejected
    // 清掉、换新代号、last_sent_at 清空）并重发。（活连接内 rejected 的 put/代号类拒绝不
    // 重试是另一件事——复审已判定结构性不可达，本单不修，Lead 记 BACKLOG。）
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let sync_frame = ack_initial_registry_sync(&mut socket);
        assert_eq!(sync_frame["revision"], 10);

        let Message::Text(text) = socket.read().unwrap() else {
            panic!("delete resend must be text");
        };
        let delete_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        frames_tx.send(delete_frame.clone()).unwrap();
        let _ = socket.close(None);
    });

    let device_id = "55555555-5555-4555-8555-555555555555";
    let subject = format!("device:{device_id}");
    let inner = test_inner_with_registry_providers_and_high_water(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 10,
                entries: Vec::new(),
            })
        }),
        Box::new(|_, _, _, _, _| {
            panic!("rebase must not run: relay_high_water(10) <= revision(10)")
        }),
        Box::new(|_, high_water, revoke_subjects: &[String]| {
            Ok(revoke_subjects
                .iter()
                .map(|subject| (subject.clone(), high_water + 1))
                .collect())
        }),
    );
    // 这条 delete 曾经真的发出去过一次，被 relay 拒绝——rejected=true，不是「从未送达」。
    lock(&inner.registry).enqueue_token_delete(subject.clone(), 5, true);
    assert_eq!(
        lock(&inner.registry).consume_token_ack(&subject, 5, "rejected"),
        TokenAckAction::Rejected
    );
    assert!(lock(&inner.registry).outbox_snapshot_for_test()[0].rejected);

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
        Some(&Zeroizing::new([8_u8; 32])),
    );

    assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
    let delete_frame = frames_rx.recv().unwrap();
    assert_eq!(delete_frame["t"], "token.delete");
    assert_eq!(delete_frame["subject"], subject);
    let sent_generation = delete_frame["generation"].as_i64().unwrap();
    assert_ne!(sent_generation, 5, "rejected 的旧代号不能原样重发");
    assert!(sent_generation > 10);

    let outbox = lock(&inner.registry).outbox_snapshot_for_test();
    assert_eq!(
        outbox.len(),
        1,
        "relay 还没 ack 这次重发，revoke 项仍应留在 outbox 里"
    );
    assert!(
        !outbox[0].rejected,
        "跨重连 sync.ack 后 rejected 必须被清掉，否则 drain 的发送闸门永远不会放行"
    );
    server.join().unwrap();
}

#[test]
fn remote_registry_revoke_reconnect_resends_delete_with_generation_above_sync_revision() {
    // S1h R4 返工：现有两条 revoke 单测都绕过了「sync → revision 墓碑 → 代号比较」这段真实
    // 链路——一条（rebase_reissues_with_new_generation）直接把 rebase 后的新代号注入，另一
    // 条（reconnect_rearms_unacked_item_for_resend）只调 `prepare_outbox_for_reconnect`，
    // 都没有真正走一遍连接。这里补一条连接级测试，复刻 S1h §9.3 证据链②-④描述的场景：
    // 撤销时最初领到的代号（5）早于这次重连要发的 sync revision（10）。
    //
    // S1h 返工二 F4：原版本让 relay_high_water 跟 revision 恒等（10=10），于是「只按
    // revision+1 领号、彻底忽略 relay_high_water」的错误实现也能巧合地满足唯一那条
    // 「严格大于」断言——两个上界重合就测不出谁被忽略了。这里改成 revision(10) 与
    // relay_high_water(12) 取不同值且 H > revision：这在真实 `synchronize_registry`
    // 里会如实触发一次 rebase（`relay_high_water > snapshot.revision` 时循环不会在第一轮
    // 就返回），所以 mock relay 也要如实走完第二轮 sync/ack，而不是回避它——第二轮
    // relay_high_water(12) 与 rebase 后的 revision(12) 相等，循环到此正常收敛，不再牵扯
    // 第三轮。rebase provider 这次不再 panic：它必须存在且被真实调用一次，只是刻意不去
    // 重新领 revoke 代号（`revoke_generations` 传空 `Vec`），把「新代号是否正确纳入两个
    // 不同上界」这件事完全留给 `registry_high_water_provider`（`absorb_registry_high_
    // water_and_rearm_revokes` 每轮 sync.ack 后都会调它）去回答。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();

        let Message::Text(text) = socket.read().unwrap() else {
            panic!("initial sync must be text");
        };
        let sync_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(sync_frame["revision"], 10);
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": sync_frame["revision"],
                    "relay_high_water": 12,
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        // relay_high_water(12) > revision(10)：真实客户端必须再发一轮 rebase sync 才能
        // 推进，这里如实模拟 relay 侧对应的第二轮应答（回同一代号 12，循环到此收敛）。
        let Message::Text(text) = socket.read().unwrap() else {
            panic!("rebase sync must be text");
        };
        let rebase_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(rebase_frame["revision"], 12);
        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.sync.ack",
                    "revision": rebase_frame["revision"],
                    "relay_high_water": 12,
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        let Message::Text(text) = socket.read().unwrap() else {
            panic!("delete resend must be text");
        };
        let delete_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
        frames_tx.send(delete_frame.clone()).unwrap();

        socket
            .send(Message::Text(
                serde_json::json!({
                    "t": "token.ack",
                    "subject": delete_frame["subject"],
                    "generation": delete_frame["generation"],
                    "result": "ok",
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        let _ = socket.close(None);
    });

    let device_id = "11111111-1111-4111-8111-111111111111";
    let subject = format!("device:{device_id}");
    // S1h 收尾：记录每次 `registry_high_water_provider` 被调用时收到的 floor 入参——
    // 正确实现（floor = relay_high_water.max(snapshot.revision)）两轮都应传 12；若生产
    // 代码把 floor 错改成只用 `snapshot.revision`，第一轮会传成 10，序列会变成
    // `[10, 12]`。最终 `sent_generation` 只看最后一轮（两种实现最后一轮都恰好是
    // 12），所以只断言 `sent_generation` 测不出这处回归，必须直接断言入参序列。
    let high_water_floors = Arc::new(Mutex::new(Vec::<i64>::new()));
    let high_water_floors_for_provider = Arc::clone(&high_water_floors);
    let inner = test_inner_with_registry_providers_and_high_water(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 10,
                entries: Vec::new(),
            })
        }),
        Box::new(|_, high_water, _, include_pairing, _revoke_subjects| {
            assert_eq!(high_water, 12, "rebase 必须拿到吸收后的 relay_high_water");
            assert!(!include_pairing);
            // 刻意不重新领 revoke 代号：这条 delete 最终发出的代号只能来自
            // `registry_high_water_provider`（每轮 sync.ack 后都跑一次的吸收步骤），
            // 不能靠 rebase 这条支路掩盖 absorb 有没有做对。
            Ok((
                RegistrySnapshot {
                    revision: high_water,
                    entries: Vec::new(),
                },
                None,
                Vec::new(),
            ))
        }),
        Box::new(move |_, high_water, revoke_subjects: &[String]| {
            high_water_floors_for_provider
                .lock()
                .unwrap()
                .push(high_water);
            Ok(revoke_subjects
                .iter()
                .map(|subject| (subject.clone(), high_water + 7))
                .collect())
        }),
    );
    // 「首次 delete 未送达（连接断）」：不经过一次真实连接，直接把撤销时领到的旧代号（5）
    // 放进 outbox，代表它是从更早、已经死掉的连接遗留下来的撤销意图。
    lock(&inner.registry).enqueue_token_delete(subject.clone(), 5, true);

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
        Some(&Zeroizing::new([6_u8; 32])),
    );

    assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
    let delete_frame = frames_rx.recv().unwrap();
    assert_eq!(delete_frame["t"], "token.delete");
    assert_eq!(delete_frame["subject"], subject);
    let sent_generation = delete_frame["generation"].as_i64().unwrap();
    // 12 已经覆盖 10（12 > 10），单独再断言 `> 10` 是冗余的，故只留 `> 12` 这一半。
    assert!(
        sent_generation > 12,
        "delete 代号必须严格大于两轮 sync 里较大的那个上界 relay_high_water（12），\
             实际 {sent_generation}"
    );
    assert_ne!(sent_generation, 5, "不能沿用撤销时领到的旧代号");
    // 只看 `sent_generation` 测不出「floor 错改成只用 revision」的回归：两种实现最后一轮
    // 传给 provider 的 high_water 恰好都是 12（错误实现只有第一轮的 10 被吞掉），所以最终
    // 代号照样是 19。真正能区分两者的是 provider 两轮各自收到的入参序列。
    assert_eq!(
        *high_water_floors.lock().unwrap(),
        vec![12, 12],
        "两轮 sync.ack 后传给 registry_high_water_provider 的 floor 都应是吸收后的 12；\
             若 floor 被错改成只用 revision，第一轮会传成 10"
    );

    let outbox = lock(&inner.registry).outbox_snapshot_for_test();
    assert!(
        outbox.is_empty(),
        "收到 ok ack 后 revoke 项必须被删除，不是继续挂在 outbox 里"
    );
    server.join().unwrap();
}

#[test]
fn remote_omitted_pairing_ack_makes_next_pairing_snapshot_generation_exceed_high_water() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let frame = ack_initial_registry_sync(&mut socket);
        assert!(frame["entries"].as_array().unwrap().is_empty());
        let _ = socket.read();
    });
    let next_generation = Arc::new(AtomicU64::new(5));
    let counter_for_snapshot = Arc::clone(&next_generation);
    let counter_for_ack = Arc::clone(&next_generation);
    let inner = test_inner_with_registry_providers_and_high_water(
        Box::new(move |_, _| {
            Ok(RegistrySnapshot {
                revision: counter_for_snapshot.load(Ordering::Acquire) as i64,
                entries: Vec::new(),
            })
        }),
        Box::new(|_, _, _, _, _| panic!("rebase must not run")),
        Box::new(move |_, high_water, _revoke_subjects: &[String]| {
            counter_for_ack.fetch_max((high_water + 1) as u64, Ordering::AcqRel);
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
            Some(&Zeroizing::new([5_u8; 32])),
        )
    });

    wait_until_connected(&inner);
    let pairing_generation = next_generation.fetch_add(1, Ordering::AcqRel) as i64;
    lock(&inner.registry).set_pairing_entry(TokenSyncEntry {
        subject: "pairing".to_owned(),
        generation: pairing_generation,
        scope: "pairing".to_owned(),
        current: TokenSyncCurrent {
            token_hash: "aa".repeat(32),
            access_expires: 1_800_000_000_000,
            refresh_until: None,
        },
        prev: None,
    });
    let next_snapshot =
        registry_snapshot_for_send(&inner, &config.room_id, 1_700_000_000_000, None).unwrap();
    let pairing = next_snapshot
        .entries
        .iter()
        .find(|entry| entry.subject == "pairing")
        .unwrap();
    assert!(pairing.generation > 5);

    inner.shutdown.store(true, Ordering::Release);
    assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
    server.join().unwrap();
}
