#![cfg(test)]

use super::*;
#[test]
fn remote_registry_sync_frame_matches_wire_v1_fixture_sample() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let expected = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "token_sync_two_entries_valid")
        .unwrap()["frame"]
        .clone();
    let snapshot = RegistrySnapshot {
        revision: 106,
        entries: vec![
            TokenSyncEntry {
                subject: "device:11111111-1111-4111-8111-111111111111".to_owned(),
                generation: 100,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            TokenSyncEntry {
                subject: "device:22222222-2222-4222-8222-222222222222".to_owned(),
                generation: 105,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
        ],
    };

    let actual: Value = serde_json::from_str(&registry_sync_frame(&snapshot).unwrap()).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn remote_registry_outbox_higher_generation_cancels_only_lower_same_subject() {
    let mut registry = RegistryState::default();
    for (subject, generation) in [("device:a", 4), ("device:b", 2), ("device:a", 7)] {
        registry.enqueue_outbox(RegistryOutboxItem {
            subject: subject.to_owned(),
            generation,
            frame: serde_json::json!({"generation": generation}),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: None,
        });
    }
    registry.enqueue_outbox(RegistryOutboxItem {
        subject: "device:a".to_owned(),
        generation: 6,
        frame: serde_json::json!({"generation": 6}),
        attempts: 0,
        last_sent_at: None,
        acked: false,
        rejected: false,
        pair_ready: None,
        refresh_ok: None,
    });

    assert_eq!(registry.outbox.len(), 2);
    assert!(registry
        .outbox
        .iter()
        .any(|item| item.subject == "device:a" && item.generation == 7));
    assert!(registry
        .outbox
        .iter()
        .any(|item| item.subject == "device:b" && item.generation == 2));
}

#[test]
fn remote_registry_outbox_same_generation_keeps_retry_metadata() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: "device:a".to_owned(),
            generation: 7,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        None,
    );
    registry.outbox[0].attempts = 3;
    registry.outbox[0].last_sent_at = Some(1_765_430_400_000);

    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: "device:a".to_owned(),
            generation: 7,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "bb".repeat(32),
                access_expires: 1_765_434_000_001,
                refresh_until: Some(1_768_022_400_001),
            },
            prev: None,
        },
        None,
    );

    assert_eq!(registry.outbox.len(), 1);
    assert_eq!(registry.outbox[0].attempts, 3);
    assert_eq!(registry.outbox[0].last_sent_at, Some(1_765_430_400_000));
    assert_eq!(
        registry.outbox[0].frame["current"]["token_hash"],
        "aa".repeat(32)
    );
}

#[test]
fn pairing_token_put_matches_wire_fixture_and_uses_absolute_millis() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let expected = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "token_put_pairing_valid")
        .unwrap()["frame"]
        .clone();
    let entry = TokenSyncEntry {
        subject: "pairing".to_owned(),
        generation: 102,
        scope: "pairing".to_owned(),
        current: TokenSyncCurrent {
            token_hash: "52b6419d27bd7f547cee3b92f8c17a908b8a49601ecbec161e5030de1dfe9e0a"
                .to_owned(),
            access_expires: 1_765_430_700_000,
            refresh_until: None,
        },
        prev: None,
    };

    let actual = token_put_frame(&entry);

    assert_eq!(actual, expected);
    assert!(actual["current"]["access_expires"].as_i64().unwrap() >= 100_000_000_000);
}

#[test]
fn token_ack_ok_removes_matching_item_but_rejected_stays_separately_marked() {
    let mut registry = RegistryState::default();
    for generation in [1, 2] {
        registry.enqueue_outbox(RegistryOutboxItem {
            subject: format!("device:{generation}"),
            generation,
            frame: serde_json::json!({"t": "token.put", "generation": generation}),
            attempts: 1,
            last_sent_at: Some(100),
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: None,
        });
    }

    assert!(matches!(
        registry.consume_token_ack("device:1", 1, "ok"),
        TokenAckAction::Consumed
    ));
    assert!(matches!(
        registry.consume_token_ack("device:2", 2, "rejected"),
        TokenAckAction::Rejected
    ));
    assert_eq!(registry.outbox.len(), 1);
    assert_eq!(registry.outbox[0].subject, "device:2");
    assert!(!registry.outbox[0].acked);
    assert!(registry.outbox[0].rejected);
}

// S1h 2c：revoke（token.delete）独立重试通道一致性测试。

#[test]
fn remote_registry_revoke_delete_ack_ok_removes_item() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_delete("device:a".to_owned(), 5, true);

    assert!(matches!(
        registry.consume_token_ack("device:a", 5, "ok"),
        TokenAckAction::Consumed
    ));
    assert!(
        registry.outbox.is_empty(),
        "ok ack 必须把 revoke 项从 outbox 删掉"
    );
}

#[test]
fn remote_registry_revoke_delete_ack_idempotent_removes_item_same_as_ok() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_delete("device:b".to_owned(), 6, true);

    assert!(matches!(
        registry.consume_token_ack("device:b", 6, "idempotent"),
        TokenAckAction::Consumed
    ));
    assert!(
        registry.outbox.is_empty(),
        "idempotent ack 与 ok 皆算成，必须同样删项"
    );
}

#[test]
fn remote_registry_revoke_outbox_survives_higher_generation_put() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_delete("device:c".to_owned(), 5, true);

    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: "device:c".to_owned(),
            generation: 9,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        None,
    );

    assert_eq!(
        registry.outbox.len(),
        1,
        "撤销后设备不复活：更高代的 put 必须被作废，不得挤掉已排队的 revoke"
    );
    assert_eq!(registry.outbox[0].frame["t"], "token.delete");
    assert_eq!(registry.outbox[0].generation, 5);
    assert!(!registry.outbox[0].acked);
    assert!(!registry.outbox[0].rejected);
}

#[test]
fn remote_registry_revoke_enqueue_cancels_unsent_put_same_subject() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: "device:d".to_owned(),
            generation: 3,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "bb".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        None,
    );

    registry.enqueue_token_delete("device:d".to_owned(), 9, true);

    assert_eq!(
        registry.outbox.len(),
        1,
        "高水位方向一致：revoke 入队必须取消同 subject 未发的 put"
    );
    assert_eq!(registry.outbox[0].frame["t"], "token.delete");
    assert_eq!(registry.outbox[0].generation, 9);
}

#[test]
fn remote_registry_revoke_reconnect_rearms_unacked_item_for_resend() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_delete("device:e".to_owned(), 7, true);
    registry.outbox[0].attempts = 2;
    registry.outbox[0].last_sent_at = Some(1_765_430_400_000);

    registry.prepare_outbox_for_reconnect();

    assert_eq!(registry.outbox.len(), 1);
    assert_eq!(
        registry.outbox[0].last_sent_at, None,
        "重连后未 ack 的 revoke 项必须随未 ack 项一起重发"
    );
    assert_eq!(
        registry.outbox[0].attempts, 2,
        "重连只解锁发送闸门，不清零重试计数"
    );
    assert!(!registry.outbox[0].acked);
    assert!(!registry.outbox[0].rejected);
}

#[test]
fn remote_registry_revoke_rebase_reissues_with_new_generation_instead_of_dropping() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_delete("device:f".to_owned(), 3, true);
    registry.outbox[0].attempts = 1;
    registry.outbox[0].last_sent_at = Some(1_765_430_400_000);

    registry.rebase_outbox_entries(&[], &[("device:f".to_owned(), 42)]);

    assert_eq!(
        registry.outbox.len(),
        1,
        "rebase 对 revoke 项的处置=重臂照发，不是 rejected 内容不能被丢弃"
    );
    assert_eq!(registry.outbox[0].generation, 42);
    assert_eq!(registry.outbox[0].frame["generation"], 42);
    assert_eq!(registry.outbox[0].frame["subject"], "device:f");
    assert_eq!(registry.outbox[0].frame["close"], true);
    assert_eq!(registry.outbox[0].last_sent_at, None);
    assert!(!registry.outbox[0].acked);
}

#[test]
fn remote_registry_pending_revoke_subjects_includes_rejected_delete() {
    // S1h R2 返工：rejected 的 delete 仍要算「待送达」，否则永远没有机会重新领号重试
    // （按 §9.3「revoke 类独立重试直到 ack」，跟 put 的「rejected 停发」惯例不同）。
    let mut registry = RegistryState::default();
    registry.enqueue_token_delete("device:h".to_owned(), 3, true);
    assert_eq!(
        registry.consume_token_ack("device:h", 3, "rejected"),
        TokenAckAction::Rejected
    );

    assert_eq!(
        registry.pending_revoke_subjects(),
        vec!["device:h".to_owned()],
        "rejected 但未 ack 的 revoke 项仍属于待重试的 pending 集合"
    );
}

#[test]
fn remote_registry_rebase_keeps_rejected_revoke_item_and_rearms_it_with_new_generation() {
    // S1h R2/R3 返工：put 被拒仍然停发丢弃，但 revoke（token.delete）被拒不能被 rebase 的
    // 清理规则连坐丢弃——它要留在 outbox 里参与下一轮重臂，并且重新领号后 rejected 标记
    // 要清掉，不然即使代号刷新了，drain_registry_outbox 的 `!item.rejected` 闸门还是不会
    // 把它发出去。
    let mut registry = RegistryState::default();
    registry.enqueue_token_delete("device:g".to_owned(), 3, true);
    assert_eq!(
        registry.consume_token_ack("device:g", 3, "rejected"),
        TokenAckAction::Rejected
    );
    assert!(registry.outbox[0].rejected);

    registry.rebase_outbox_entries(&[], &[("device:g".to_owned(), 9)]);

    assert_eq!(
        registry.outbox.len(),
        1,
        "rejected 的 revoke 项不能被 rebase 连坐丢弃"
    );
    assert_eq!(registry.outbox[0].generation, 9);
    assert_eq!(registry.outbox[0].frame["generation"], 9);
    assert_eq!(registry.outbox[0].frame["subject"], "device:g");
    assert!(
        !registry.outbox[0].rejected,
        "重新领号后必须把 rejected 翻回 false，否则 drain 的闸门永远不会把它发出去"
    );
    assert!(!registry.outbox[0].acked);
    assert_eq!(registry.outbox[0].last_sent_at, None);
}

#[test]
fn remote_registry_revoke_delete_frame_matches_wire_v1_fixture_sample() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let expected = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "token_delete_close_valid")
        .unwrap()["frame"]
        .clone();

    let mut registry = RegistryState::default();
    registry.enqueue_token_delete(
        "device:11111111-1111-4111-8111-111111111111".to_owned(),
        104,
        true,
    );

    assert_eq!(registry.outbox[0].frame, expected);
}

#[test]
fn remote_registry_rebase_drops_rejected_outbox_item_instead_of_rearming_it() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: "device:a".to_owned(),
            generation: 7,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        None,
    );
    assert_eq!(
        registry.consume_token_ack("device:a", 7, "rejected"),
        TokenAckAction::Rejected
    );

    registry.rebase_outbox_entries(
        &[TokenSyncEntry {
            subject: "device:a".to_owned(),
            generation: 11,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "bb".repeat(32),
                access_expires: 1_765_434_000_100,
                refresh_until: Some(1_768_022_400_100),
            },
            prev: None,
        }],
        &[],
    );

    assert!(registry.outbox.is_empty());
}

#[test]
fn remote_registry_rebase_updates_generation_on_mounted_refresh_ok_receipt() {
    // S1i1 R1 返工：rebase 此前只改 item.generation/item.frame，没碰挂着的 refresh_ok——
    // 回执带着轮换那一刻冻结的旧代号出门，relay 侧 §9.6 第 246 行「回执.generation ==
    // subject 当前 generation」校验不过，被丢弃，手机凭旧 refresh 重试又只拿到同一份
    // 陈旧回执，48h journal 窗内死循环，只能重新配对。
    let mut registry = RegistryState::default();
    let refresh_ok = RefreshOkFrame {
        request_id: "req-rebase-1".to_owned(),
        subject: "device:i".to_owned(),
        generation: 5,
        ct: "response-ct".to_owned(),
        n: "response-n".to_owned(),
    };
    registry.enqueue_token_put_for_refresh(
        TokenSyncEntry {
            subject: "device:i".to_owned(),
            generation: 5,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "cc".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        refresh_ok,
    );

    registry.rebase_outbox_entries(
        &[TokenSyncEntry {
            subject: "device:i".to_owned(),
            generation: 9,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "dd".repeat(32),
                access_expires: 1_765_434_000_100,
                refresh_until: Some(1_768_022_400_100),
            },
            prev: None,
        }],
        &[],
    );

    assert_eq!(registry.outbox.len(), 1);
    assert_eq!(registry.outbox[0].generation, 9);
    let mounted = registry.outbox[0]
        .refresh_ok
        .as_ref()
        .expect("refresh_ok 必须仍然挂在 rebase 后的项上");
    assert_eq!(
        mounted.generation, 9,
        "rebase 后挂载的回执代号必须同步更新，不能停在轮换时刻冻结的 5"
    );
    // ct/n 不含 generation，AAD 五元组也不含 generation——密文体必须原样保留。
    assert_eq!(mounted.ct, "response-ct");
    assert_eq!(mounted.n, "response-n");

    let action = registry.consume_token_ack("device:i", 9, "ok");
    let TokenAckAction::RefreshOk(refresh_ok) = action else {
        panic!("ack 命中 rebase 后的代号必须吐出 RefreshOk，实际是 {action:?}");
    };
    assert_eq!(
        refresh_ok.generation, 9,
        "ack 释放的回执 generation 必须等于 rebase 后 DB 的当前代号"
    );
}

#[test]
fn device_token_ack_rejected_with_mounted_refresh_ok_self_heals_via_fail_frame_without_burning_invalid_streak(
) {
    // S1i1 R2 返工：put 被 relay 拒绝时，挂在项上的 refresh_ok 不能被 Rejected 静默吞掉——
    // 桌面 DB/TokenBook 已经轮换成功，手机既收不到 ok 也收不到 fail，只能干等超时；本单要求
    // handle_frame 立即回一帧 fail，让手机凭旧 refresh 马上重试。
    let inner = test_inner(|_| None, || None);
    let subject = "device:11111111-1111-4111-8111-111111111111".to_owned();
    {
        let mut registry = lock(&inner.registry);
        // 先攒 1 次「真」无效（count=1），方便后面用「rejected 自愈之后的下一次真无效
        // 是否提前跨过阈值」来判定 rejected 有没有偷偷计数——阈值是 3，如果只留判断
        // 「最终有没有到 3」是分辨不出来的（不管 rejected 计不计数，多打几次总会到 3）；
        // 必须看 rejected 之后紧接着那一次真无效是不是还没到阈值。
        assert!(!registry.record_refresh_invalid(&subject));
        registry.enqueue_token_put_for_refresh(
            TokenSyncEntry {
                subject: subject.clone(),
                generation: 7,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            RefreshOkFrame {
                request_id: "req-rejected-1".to_owned(),
                subject: subject.clone(),
                generation: 7,
                ct: "response-ct".to_owned(),
                n: "response-n".to_owned(),
            },
        );
    }

    let response = handle_frame(
        &inner,
        &format!(r#"{{"t":"token.ack","subject":"{subject}","generation":7,"result":"rejected"}}"#),
        None,
    )
    .expect("put_rejected 自愈必须立即回一帧，不能悄悄丢弃");

    assert_eq!(response["t"], "token.refresh.fail");
    assert_eq!(response["request_id"], "req-rejected-1");
    assert_eq!(response["subject"], subject);
    assert_eq!(response["reason"], "put_rejected");
    assert!(
        response.get("close").is_none(),
        "put_rejected 是良性自愈路径，不带 close"
    );

    let mut registry = lock(&inner.registry);
    assert!(
        registry.outbox_snapshot_for_test()[0].rejected,
        "outbox 项仍要标 rejected，交给既有 drain/rebase 清理惯例"
    );
    // rejected 自愈之前已经攒了 1 次真无效（count=1）。如果 rejected 自愈没有偷偷计数，
    // 这里紧接着的一次真无效只是第 2 次（count=2），还不该到阈值 3；如果 rejected 悄悄
    // 计了一次（count 提前变成 2），这次就会是第 3 次，提前触发 close——这才是真正能分辨
    // 出「计没计数」的断言。
    assert!(
        !registry.record_refresh_invalid(&subject),
        "put_rejected 自愈不该计入连续无效计数——如果计了，这里会提前到第 3 次触发 close"
    );
    assert!(
        registry.record_refresh_invalid(&subject),
        "紧接着真正的第 3 次无效才该跨过阈值"
    );
}

#[test]
fn remote_done_replay_rearms_timed_out_pair_ready_put_and_ack_releases_ready() {
    let ready = PairReadyFrame {
        room: "0123456789abcdef0123456789abcdef".to_owned(),
        device_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        ct: "ready-ct".to_owned(),
        n: "ready-n".to_owned(),
    };
    let subject = format!("device:{}", ready.device_id);
    let mut registry = RegistryState::default();
    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: subject.clone(),
            generation: 7,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        Some(ready.clone()),
    );
    registry.outbox[0].attempts = 3;
    registry.outbox[0].last_sent_at = Some(123);
    let original_frame = registry.outbox[0].frame.clone();

    assert_eq!(registry.replay_pair_ready(&subject), None);
    assert_eq!(registry.outbox[0].generation, 7);
    assert_eq!(registry.outbox[0].frame, original_frame);
    assert_eq!(registry.outbox[0].attempts, 3);
    assert_eq!(registry.outbox[0].last_sent_at, None);
    assert!(!registry.outbox[0].acked);

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = tungstenite::connect(format!("ws://{addr}"))
        .expect("test client should connect to recording relay");
    let inner = test_inner(|_| None, || None);
    *lock(&inner.registry) = registry;
    drain_registry_outbox(&mut socket, &inner).unwrap();
    let resent = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(resent, original_frame);
    assert_eq!(lock(&inner.registry).outbox[0].attempts, 4);
    assert!(matches!(
        lock(&inner.registry).consume_token_ack(&subject, 7, "ok"),
        TokenAckAction::PairReady(released) if released == ready
    ));
    drop(socket);
    server.join().unwrap();
}

#[test]
fn remote_done_replay_rearms_rejected_pair_ready_put_without_changing_payload_or_attempts() {
    let ready = PairReadyFrame {
        room: "0123456789abcdef0123456789abcdef".to_owned(),
        device_id: "22222222-2222-4222-8222-222222222222".to_owned(),
        ct: "ready-ct".to_owned(),
        n: "ready-n".to_owned(),
    };
    let subject = format!("device:{}", ready.device_id);
    let mut registry = RegistryState::default();
    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: subject.clone(),
            generation: 11,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "bb".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        Some(ready.clone()),
    );
    registry.outbox[0].attempts = 2;
    registry.outbox[0].last_sent_at = Some(456);
    let original = registry.outbox[0].clone();
    assert_eq!(
        registry.consume_token_ack(&subject, 11, "rejected"),
        TokenAckAction::Rejected
    );
    assert!(!registry.outbox[0].acked);
    assert!(registry.outbox[0].rejected);

    assert_eq!(registry.replay_pair_ready(&subject), None);
    let rearmed = &registry.outbox[0];
    assert_eq!(rearmed.subject, original.subject);
    assert_eq!(rearmed.generation, original.generation);
    assert_eq!(rearmed.frame, original.frame);
    assert_eq!(rearmed.attempts, original.attempts);
    assert_eq!(rearmed.pair_ready, original.pair_ready);
    assert_eq!(rearmed.last_sent_at, None);
    assert!(!rearmed.acked);
    assert!(!rearmed.rejected);

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = tungstenite::connect(format!("ws://{addr}"))
        .expect("test client should connect to recording relay");
    let inner = test_inner(|_| None, || None);
    *lock(&inner.registry) = registry;
    drain_registry_outbox(&mut socket, &inner).unwrap();
    let resent = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(resent, original.frame);
    drop(socket);
    server.join().unwrap();
}

#[test]
fn device_token_ack_is_the_pair_ready_barrier() {
    let ready = PairReadyFrame {
        room: "0123456789abcdef0123456789abcdef".to_owned(),
        device_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        ct: "ready-ct".to_owned(),
        n: "ready-n".to_owned(),
    };
    let inner = test_inner_with_pair_handlers(
        |_| None,
        |_| PairDoneAction::Accepted {
            newly_paired_device_id: None,
        },
    );
    lock(&inner.registry).enqueue_token_put(
        TokenSyncEntry {
            subject: "device:11111111-1111-4111-8111-111111111111".to_owned(),
            generation: 7,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: None,
        },
        Some(ready.clone()),
    );

    assert!(handle_frame(
        &inner,
        r#"{"t":"pair.done","room":"0123456789abcdef0123456789abcdef","device_id":"11111111-1111-4111-8111-111111111111","confirm_ct":"ct","confirm_n":"n","origin_connection_id":"conn-pairing-1"}"#,
        None,
    )
    .is_none());
    let response = handle_frame(
        &inner,
        r#"{"t":"token.ack","subject":"device:11111111-1111-4111-8111-111111111111","generation":7,"result":"ok"}"#,
        None,
    )
    .expect("matching successful token.ack must release pair.ready");

    assert_eq!(response, pair_ready_json(ready));
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let expected = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "pair_ready_valid")
        .unwrap()["frame"]
        .clone();
    assert_eq!(response, expected);
}

#[test]
fn remote_registry_reset_cancels_whole_room_outbox() {
    let mut registry = RegistryState::default();
    registry.enqueue_outbox(RegistryOutboxItem {
        subject: "device:a".to_owned(),
        generation: 1,
        frame: serde_json::json!({"t": "token.put"}),
        attempts: 0,
        last_sent_at: None,
        acked: false,
        rejected: false,
        pair_ready: None,
        refresh_ok: None,
    });
    registry.cancel_outbox_before_reset();
    assert!(registry.outbox.is_empty());
}

#[test]
fn remote_pairing_natural_cleanup_drops_put_but_keeps_explicit_delete() {
    let mut registry = RegistryState::default();
    registry.enqueue_token_put(
        TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 1,
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
    registry.clear_pairing_entry();

    assert!(registry.outbox.is_empty());
    registry.enqueue_token_delete("pairing".to_owned(), 2, true);
    registry.clear_pairing_entry();

    assert_eq!(registry.outbox.len(), 1);
    assert_eq!(registry.outbox[0].frame["t"], "token.delete");
    assert_eq!(registry.outbox[0].frame["subject"], "pairing");
    assert_eq!(registry.outbox[0].frame["generation"], 2);
    assert_eq!(registry.outbox[0].frame["close"], true);
}

#[test]
fn remote_pairing_new_round_put_survives_stale_cancel_delete_in_outbox() {
    // S1h 返工二 F1：pairing 是唯一会被复用的 subject——begin#1 排 put(gen 10) → cancel
    // 排 delete(gen 11，顺手挤掉未发的 gen 10 put) → begin#2 之前，这条陈旧 delete 不能
    // 继续堵在 outbox 里：`enqueue_outbox` 的豁免规则（同 subject 已排 delete → 后续 put
    // 一律作废）会把 begin#2 的新 put 吞掉，只剩这条陈旧 delete 会在下次重连 absorb 时被
    // 重新领到一个高于本次 sync revision 的代号发出去，把刚开始的新一轮配对当场撤销
    // （详见 `set_pairing_entry` 处的返工二 F1 注释与失败时序）。
    let mut registry = RegistryState::default();
    let pairing_entry = |generation: i64| TokenSyncEntry {
        subject: "pairing".to_owned(),
        generation,
        scope: "pairing".to_owned(),
        current: TokenSyncCurrent {
            token_hash: "aa".repeat(32),
            access_expires: 1_765_430_700_000,
            refresh_until: None,
        },
        prev: None,
    };

    // begin#1
    registry.set_pairing_entry(pairing_entry(10));
    registry.enqueue_token_put(pairing_entry(10), None);
    assert_eq!(registry.outbox.len(), 1);
    assert_eq!(registry.outbox[0].frame["t"], "token.put");

    // cancel（断线/退避态：不建模 request_registry_publish，只关心 outbox 状态）
    registry.clear_pairing_entry();
    registry.enqueue_token_delete("pairing".to_owned(), 11, true);
    assert_eq!(registry.outbox.len(), 1);
    assert_eq!(registry.outbox[0].frame["t"], "token.delete");
    assert_eq!(registry.outbox[0].generation, 11);

    // begin#2
    registry.set_pairing_entry(pairing_entry(12));
    registry.enqueue_token_put(pairing_entry(12), None);

    assert_eq!(
        registry.outbox.len(),
        1,
        "新一轮配对的 put 必须落进 outbox，不能被陈旧 delete 的豁免规则吞掉"
    );
    assert_eq!(
        registry.outbox[0].frame["t"], "token.put",
        "陈旧 delete 必须被 set_pairing_entry 作废，不能继续挂在 outbox 里等重连时被\
             重新领号打死刚开始的新一轮配对"
    );
    assert_eq!(registry.outbox[0].generation, 12);
    assert!(
        registry.pending_revoke_subjects().is_empty(),
        "作废后不该再有 pairing 的待送达 revoke，重连 absorb 不应再给它重新领号"
    );
}

#[test]
fn remote_registry_snapshot_includes_only_unexpired_pairing_window() {
    let inner = test_inner_with_registry_providers(
        Box::new(|_, _| {
            Ok(RegistrySnapshot {
                revision: 2,
                entries: Vec::new(),
            })
        }),
        test_registry_rebase_provider(),
    );
    lock(&inner.registry).set_pairing_entry(TokenSyncEntry {
        subject: "pairing".to_owned(),
        generation: 1,
        scope: "pairing".to_owned(),
        current: TokenSyncCurrent {
            token_hash: "aa".repeat(32),
            access_expires: 1_000,
            refresh_until: None,
        },
        prev: None,
    });

    let active = registry_snapshot_for_send(&inner, "room", 999, None).unwrap();
    assert_eq!(active.entries.len(), 1);
    assert_eq!(active.entries[0].subject, "pairing");
    assert_eq!(active.entries[0].current.refresh_until, None);

    let expired = registry_snapshot_for_send(&inner, "room", 1_000, None).unwrap();
    assert!(expired.entries.is_empty());
}
