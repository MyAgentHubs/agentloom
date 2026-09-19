#![cfg(test)]

use super::*;

#[test]
fn resolve_remote_room_id_generates_once_and_reuses() {
    let conn = cli_path_test_db();

    let first = resolve_remote_room_id(&conn).unwrap();
    let second = resolve_remote_room_id(&conn).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.len(), 32);
    assert!(first
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

// ---------------------------------------------------------------------------------
// M24DR 返工·项 3/5/6：`resolve_active_pairing_room_id`（配对 begin 领号路径）此前零
// 覆盖——下面补齐 active 已设/未设/repo 已删/空白/remote 未启用五条分支。
// ---------------------------------------------------------------------------------

/// 项 3①：active 已设时，`resolve_active_pairing_room_id` 解出的房间必须是该 project 的
/// per-project 房——跟 `db::ensure_remote_room_for_project` 回读同一个值（不是另生成的
/// 随机房），且这个 room_id 就是 `remote_pairing_begin` 会喂进 QR payload 的那个值
/// （`PairingSession::begin` 原样把它塞进 `QrPayload::room`，这里直接验证这条传递关系）。
#[test]
fn resolve_active_pairing_room_id_uses_active_project_per_project_room() {
    let conn = remote_active_project_test_db();
    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();

    let room_id = resolve_active_pairing_room_id(&conn).unwrap();

    let expected_room_id = db::ensure_remote_room_for_project(&conn, "repo-1").unwrap();
    assert_eq!(
        room_id, expected_room_id,
        "配对必须用 active project 的 per-project 房间，跟 ensure_remote_room_for_project \
             回读同一个值"
    );
    let (_session, qr_payload) =
        remote_pairing::PairingSession::begin("wss://relay.example.com", &room_id, 1_700_000_000);
    assert_eq!(
        qr_payload.room, room_id,
        "remote_pairing_begin 的 QR payload.room 必须是 resolve_active_pairing_room_id \
             解出的房间"
    );
}

/// relay 内置公共中继单：`remote_pairing_begin` 收到空 `relay_url`（前端未填/纯空白）时
/// 必须以官方公共中继开始这轮配对会话——`remote_pairing_begin` 本体对 relay_url 唯一做的
/// 事就是 `remote_gateway::effective_relay_url(Some(relay_url))` 再喂给
/// `PairingSession::begin`，这里原样复刻这两行生产逻辑（同上一个测试"手工重放 QR payload
/// 构造流程"的做法一致——命令层吃 `State<Db>` 不便直调）。
#[test]
fn remote_pairing_begin_falls_back_to_default_relay_when_relay_url_empty() {
    let relay_url = remote_gateway::effective_relay_url(Some(String::new()))
        .expect("effective_relay_url always returns Some");
    let (_session, qr_payload) = remote_pairing::PairingSession::begin(
        &relay_url,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        1_700_000_000,
    );
    assert_eq!(
        qr_payload.relay_url,
        remote_gateway::DEFAULT_PUBLIC_RELAY_URL,
        "relay_url 留空时，配对会话必须以官方公共中继开始"
    );
}

/// 项 3②：active project 未设时，配对必须拒绝——`AL_ERR:remoteControl.
/// pairingNeedsActiveProject` 信封开头，不是静默判"未配置"（用户点了"开始配对"这个显式
/// 动作，值得一个看得懂的错误）。
#[test]
fn resolve_active_pairing_room_id_rejects_when_active_project_unset() {
    let conn = remote_active_project_test_db();
    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    // 故意不设 REMOTE_ACTIVE_REPO_ID_SETTING。

    let error = resolve_active_pairing_room_id(&conn).unwrap_err();

    assert!(
        error.starts_with("AL_ERR:remoteControl.pairingNeedsActiveProject"),
        "active 未设时配对必须拒绝，实际={error}"
    );
}

/// 项 3③：active 指向的 repo 在 `repos` 表里查无（手改 DB / repo 已被删但 setting 是陈旧
/// 值）时必须拒绝——复用 `remoteControl.activeProjectMissing` 错误码，跟
/// `remote_set_active_project_in_conn` 的既有场景一致。绕开 `remote_set_active_project_
/// in_conn`（它自己会校验存在性、根本写不进去这个值）直接摆 app_setting，模拟"曾经存在、
/// 后来被删"的陈旧值。
#[test]
fn resolve_active_pairing_room_id_rejects_when_active_project_repo_missing() {
    let conn = remote_active_project_test_db();
    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    db::set_app_setting(&conn, REMOTE_ACTIVE_REPO_ID_SETTING, "repo-does-not-exist").unwrap();

    let error = resolve_active_pairing_room_id(&conn).unwrap_err();

    assert!(
        error.starts_with("AL_ERR:remoteControl.activeProjectMissing:"),
        "active 指向的 repo 查无时必须拒绝，实际={error}"
    );
}

/// 项 6②：纯空白 `remote_active_repo_id`（跟 `remote_control_get_settings_in_conn_treats_
/// whitespace_active_repo_id_as_unset` 同一类"手改库留下的陈旧空白值"场景）必须按"未设"
/// 处理，配对路径要拒绝——不能被 trim 前的非空字符串长度骗过去当成"已设置"。
#[test]
fn resolve_active_pairing_room_id_rejects_when_active_project_is_whitespace() {
    let conn = remote_active_project_test_db();
    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    db::set_app_setting(&conn, REMOTE_ACTIVE_REPO_ID_SETTING, "   ").unwrap();

    let error = resolve_active_pairing_room_id(&conn).unwrap_err();

    assert!(
        error.starts_with("AL_ERR:remoteControl.pairingNeedsActiveProject"),
        "纯空白 active repo id 必须按未设处理，配对路径要拒绝，实际={error}"
    );
}

/// 项 5（审查 nit F6）：remote 未启用时，即使 active project 已设，配对也必须拒绝——跟
/// active 未设走同一条错误路径（都还没到"能配对"的地步，没有专门的"remote 未启用"配对
/// 错误码）。更关键的是**不能顺手建房**：`db::remote_room_for_project` 之后必须仍然查无
/// 这个 project 的房间——不然每次用户手滑点"开始配对"却没先启用 remote，都会白白在
/// `project_remote_rooms` 里烧一行 + 在 `remote_registry_counter` 里烧一个 generation，
/// 这行房从此再也用不上（跟同 commit `current_config_does_not_ensure_active_room_when_
/// remote_control_disabled` 是同一条纪律，配对路径此前漏了）。
#[test]
fn resolve_active_pairing_room_id_rejects_and_does_not_create_room_when_remote_control_disabled() {
    let conn = remote_active_project_test_db();
    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();
    // remote_control_enabled 故意不设（等价 "false"）。

    let error = resolve_active_pairing_room_id(&conn).unwrap_err();

    assert!(
        error.starts_with("AL_ERR:remoteControl.pairingNeedsActiveProject"),
        "remote 未启用时必须走跟 active 未设一样的拒绝路径，实际={error}"
    );
    assert_eq!(
        db::remote_room_for_project(&conn, "repo-1").unwrap(),
        None,
        "remote 未启用时绝不能顺手建房——同 commit 的『未启用不白白建房』纪律，配对路径也\
             要遵守"
    );
}

#[test]
fn remote_set_active_project_in_conn_write_read_roundtrip_and_clear() {
    let conn = remote_active_project_test_db();

    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();
    assert_eq!(
        db::get_app_setting(&conn, REMOTE_ACTIVE_REPO_ID_SETTING).unwrap(),
        Some("repo-1".to_owned())
    );

    // None = 清除（DELETE，不留空字符串行）。
    remote_set_active_project_in_conn(&conn, None).unwrap();
    assert_eq!(
        db::get_app_setting(&conn, REMOTE_ACTIVE_REPO_ID_SETTING).unwrap(),
        None,
        "None 必须清除该 app_setting 行，而不是写空字符串"
    );

    // 空白字符串同样按"清除"处理（对齐 `set_cli_path_in_conn` 的 trim+filter 惯例）。
    remote_set_active_project_in_conn(&conn, Some("repo-2")).unwrap();
    remote_set_active_project_in_conn(&conn, Some("   ")).unwrap();
    assert_eq!(
        db::get_app_setting(&conn, REMOTE_ACTIVE_REPO_ID_SETTING).unwrap(),
        None,
        "空白字符串按清除处理"
    );
}

/// R3/R7③：垃圾 repo id（`repos` 表里查无）必须整体拒写——命令返回 `Err`，且
/// `remote_active_repo_id` 这个 app_setting 一行都不写。M2-4d：错误必须走
/// `ui_msg::al_err` 信封（不是普通 `format!` 字符串），前端才拿得到 zh/en 文案。
#[test]
fn remote_set_active_project_in_conn_rejects_repo_id_that_does_not_exist() {
    let conn = remote_active_project_test_db();

    let result = remote_set_active_project_in_conn(&conn, Some("repo-does-not-exist"));

    let error = result.unwrap_err();
    assert!(
        error.starts_with("AL_ERR:remoteControl.activeProjectMissing:"),
        "校验失败必须返回 al_err 信封，实际={error}"
    );
    assert!(
        error.contains("repo-does-not-exist"),
        "al_err 信封必须带上 repoId 参数，实际={error}"
    );
    assert_eq!(
        db::get_app_setting(&conn, REMOTE_ACTIVE_REPO_ID_SETTING).unwrap(),
        None,
        "校验失败必须整体不落库，不能先写 setting 再报错"
    );
}

/// M2-4a doc 约束①·M2-4b 任务 3：凭据幂等——已有不重建、丢失重建。直接窥探钥匙串里的
/// 原始 key（不经过 `resolve_desktop_credential`，那个函数本身也是"查无则建"语义，用它
/// 来验证会失去"ensure 之前真的还没有凭据"这个前置条件的证明力）。
#[test]
fn remote_ensure_desktop_credential_for_room_creates_when_missing_and_is_idempotent_when_present() {
    let store = FakeKeyStore::default();
    let room_id = "0123456789abcdef0123456789abcdef";
    // key 格式字面量故意跟 remote_pairing::store 里的私有 `desktop_credential_key_id`
    // 重复（同 `remote_gateway_k_room_provider` 附近注释对 `k_room_key_id` 的既有先例）——
    // 只是为了在测试里直接窥探钥匙串到底有没有这一条，不经过任何"顺带创建"的 ensure 语义。
    let raw_key_id = format!("remote-desktop-credential-{room_id}");

    assert_eq!(
        store.get(&raw_key_id).unwrap(),
        None,
        "前置条件：钥匙串里还没有这个房间的凭据"
    );

    ensure_desktop_credential_for_room(&store, room_id).unwrap();
    let created = store
        .get(&raw_key_id)
        .unwrap()
        .expect("ensure 必须在缺失时新建凭据");
    assert_eq!(created.len(), 64, "凭据必须是 256-bit → 64 位 hex");

    // 再 ensure 一次：已有凭据必须原样保留，不重建。
    ensure_desktop_credential_for_room(&store, room_id).unwrap();
    let after_second_ensure = store.get(&raw_key_id).unwrap().unwrap();
    assert_eq!(
        after_second_ensure, created,
        "已有凭据时 ensure 不得重建，credential 必须原样保留"
    );
}

/// R7④ 用的哨兵：包一层 `FakeKeyStore`，数 `.get()` 被调用了几次——
/// `ensure_desktop_credential_for_room`/`resolve_desktop_credential` 的"已有则返回"分支
/// 必经 `.get()`，缓存命中时这个数字不该再涨；缓存被清后再 ensure 一次，数字必须涨。
#[derive(Default)]
struct CountingKeyStore {
    inner: FakeKeyStore,
    get_calls: std::sync::atomic::AtomicUsize,
}

impl KeyStore for CountingKeyStore {
    fn set(&self, id: &str, key: &str) -> Result<(), String> {
        self.inner.set(id, key)
    }
    fn get(&self, id: &str) -> Result<Option<String>, String> {
        self.get_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.get(id)
    }
    fn delete(&self, id: &str) -> Result<(), String> {
        self.inner.delete(id)
    }
}

/// R2/R7④：`ensure_desktop_credential_for_room_cached` 缓存命中期间不得重复摸钥匙串；
/// 缓存被清空（模拟 `remote_set_active_project` 写入成功后的清空动作）后，下一次 ensure
/// 必须重新真的摸一次钥匙串——不能继续信任一个可能已经过期的"已确认过"标记。
#[test]
fn remote_ensure_desktop_credential_for_room_cached_reconfirms_after_cache_cleared() {
    let store = CountingKeyStore::default();
    let credential_ensured: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    let room_id = "0123456789abcdef0123456789abcdef";

    ensure_desktop_credential_for_room_cached(&store, &credential_ensured, room_id).unwrap();
    let calls_after_first = store.get_calls.load(std::sync::atomic::Ordering::SeqCst);
    assert!(calls_after_first >= 1, "首次 ensure 必须真的摸一次钥匙串");

    ensure_desktop_credential_for_room_cached(&store, &credential_ensured, room_id).unwrap();
    assert_eq!(
        store.get_calls.load(std::sync::atomic::Ordering::SeqCst),
        calls_after_first,
        "缓存命中期间重复 ensure 不得再碰钥匙串"
    );

    // R2：清缓存——模拟 remote_set_active_project 写入成功后的清空动作。
    credential_ensured.lock().unwrap().clear();

    ensure_desktop_credential_for_room_cached(&store, &credential_ensured, room_id).unwrap();
    assert!(
        store.get_calls.load(std::sync::atomic::Ordering::SeqCst) > calls_after_first,
        "缓存被清空后，下一次 ensure 必须重新摸一次钥匙串确认凭据仍在"
    );
}

#[test]
fn compute_pairing_status_idle_by_default() {
    let mut slot = PairingSlot::Idle;
    assert_eq!(
        compute_pairing_status(&mut slot, 1_700_000_000),
        RemotePairingStatus::Idle
    );
}

#[test]
fn gateway_status_ipc_view_maps_memory_status() {
    let counters = remote_gateway::GatewayCounters {
        frames_seen: 1,
        frames_sent: 2,
        keepalive_pings_sent: 19,
        bad_frames: 3,
        upstream_dropped: 4,
        upstream_stale_generation_dropped: 5,
        upstream_budget_dropped: 6,
        milestone_dropped: 7,
        session_index_snapshot_unavailable: 8,
        snapshot_worker_spawn_count: 9,
        tool_correlation_dropped: 10,
        classify_skipped: 11,
        connection_failures: 12,
        panics: 13,
        disconnect_config_stale: 20,
        disconnect_closed_by_peer: 21,
        disconnect_error: 22,
        last_disconnect_reason: "read failed: redacted diagnostic".to_owned(),
        upstream_repo_filtered: 14,
        partial_snapshot_capacity_dropped: 15,
        snapshot_oversized_dropped: 16,
        history_oversized_dropped: 17,
        replay_oversized_dropped: 18,
    };
    let memory_status = remote_gateway::GatewayStatus {
        state: remote_gateway::GatewayState::Disabled,
        last_error: Some("redacted diagnostic".to_owned()),
        stopped_reason: Some("room_tombstoned".to_owned()),
        counters: counters.clone(),
    };

    let ipc_status = remote_gateway_status_view(memory_status);
    assert_eq!(
        ipc_status,
        RemoteGatewayStatus {
            running: false,
            stopped_reason: Some("room_tombstoned".to_owned()),
            last_error: Some("redacted diagnostic".to_owned()),
            counters: counters.clone(),
        }
    );
    let serialized = serde_json::to_value(&ipc_status).unwrap();
    assert!(serialized["counters"].is_object());
    assert_eq!(serialized["counters"]["frames_seen"], 1);
    assert_eq!(serialized["counters"]["replay_oversized_dropped"], 18);
    assert_eq!(serialized["last_error"], "redacted diagnostic");

    let memory_status = remote_gateway::status();
    assert_eq!(
        remote_gateway_status(),
        remote_gateway_status_view(memory_status)
    );
}

#[test]
fn compute_pairing_status_reports_waiting_before_expiry() {
    let (session, _) =
        remote_pairing::PairingSession::begin("wss://relay.example.test", "room-x", 1_700_000_000);
    let expires_at = session.expires_at_secs;
    let mut slot = PairingSlot::Waiting(session);

    assert_eq!(
        compute_pairing_status(&mut slot, 1_700_000_000),
        RemotePairingStatus::WaitingForHello { expires_at }
    );
}

#[test]
fn compute_pairing_status_auto_expires_waiting_to_idle() {
    let (session, _) =
        remote_pairing::PairingSession::begin("wss://relay.example.test", "room-x", 1_700_000_000);
    let expires_at = session.expires_at_secs;
    let mut slot = PairingSlot::Waiting(session);

    let status = compute_pairing_status(&mut slot, expires_at);

    assert_eq!(status, RemotePairingStatus::Idle);
    assert!(
        matches!(slot, PairingSlot::Idle),
        "过期的等待态必须把槽本身也降级为 Idle"
    );
}

#[test]
fn pairing_gateway_rejects_bad_token_without_accept_or_device_persistence() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, b"wrong-token")
    };

    let result = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW);

    assert!(result.is_err(), "bad token must not produce pair.accept");
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Waiting(_)));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert!(key_store
        .get(&format!("remote-kroom-{PAIR_TEST_ROOM}"))
        .unwrap()
        .is_none());
}

#[test]
fn pairing_gateway_rejects_expired_hello_without_side_effects() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let (hello, expired_now) = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        (
            pairing_gateway_hello(session, session.pairing_token.as_bytes()),
            session.expires_at_secs + 1,
        )
    };

    let result = process_pair_hello(&slot, &key_store, hello, expired_now);

    assert!(
        result.is_err(),
        "expired hello must not produce pair.accept"
    );
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Waiting(_)));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert!(key_store
        .get(&format!("remote-kroom-{PAIR_TEST_ROOM}"))
        .unwrap()
        .is_none());
}

#[test]
fn pairing_gateway_rejects_non_contributory_hello_without_side_effects() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let hello = remote_gateway::PairHelloFrame {
        room: PAIR_TEST_ROOM.to_owned(),
        remote_pub: [0_u8; 32],
        token_ct: "not-base64".to_owned(),
        token_n: "also-not-base64".to_owned(),
        origin_connection_id: "conn-pairing-test".to_owned(),
    };

    let result = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW);

    assert!(
        result.is_err(),
        "non-contributory hello must not produce pair.accept"
    );
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Waiting(_)));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert!(key_store
        .get(&format!("remote-kroom-{PAIR_TEST_ROOM}"))
        .unwrap()
        .is_none());
}

#[test]
fn process_pair_hello_ignores_hello_when_slot_is_idle() {
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    *slot.lock().unwrap() = PairingSlot::Idle;

    let result = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW).unwrap();

    assert!(result.is_none());
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Idle));
}

#[test]
fn process_pair_hello_ignores_hello_when_slot_is_done() {
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    let completed_device_id = "already-paired-device".to_owned();
    *slot.lock().unwrap() = PairingSlot::Done {
        room_id: PAIR_TEST_ROOM.to_owned(),
        device_id: completed_device_id.clone(),
        completed_at_secs: PAIR_TEST_NOW,
    };

    let result = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW).unwrap();

    assert!(result.is_none());
    assert!(matches!(
        &*slot.lock().unwrap(),
        PairingSlot::Done { device_id, .. } if device_id == &completed_device_id
    ));
}

#[test]
fn process_pair_hello_ignores_second_hello_while_waiting_for_done() {
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let first_hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    let first_accept = process_pair_hello(&slot, &key_store, first_hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("first hello should produce pair.accept");
    let (capability_token, refresh_token) = decrypt_pair_accept_tokens(&slot, &first_accept);
    let (second_session, _) = remote_pairing::PairingSession::begin(
        "wss://relay.example.test",
        PAIR_TEST_ROOM,
        PAIR_TEST_NOW + 1,
    );
    let second_hello =
        pairing_gateway_hello(&second_session, second_session.pairing_token.as_bytes());

    let result = process_pair_hello(&slot, &key_store, second_hello, PAIR_TEST_NOW + 1).unwrap();

    assert!(result.is_none());
    let guard = slot.lock().unwrap();
    let PairingSlot::SentAccept {
        outcome,
        room_id,
        sent_at_secs,
        ..
    } = &*guard
    else {
        panic!("second hello must leave the original SentAccept in place")
    };
    assert_eq!(outcome.device_record.device_id, first_accept.device_id);
    assert_eq!(outcome.k_room_wrapped_ct, first_accept.k_room_ct);
    assert_eq!(outcome.k_room_wrapped_n, first_accept.k_room_n);
    assert_eq!(outcome.capability_token, capability_token);
    assert_eq!(outcome.refresh_token, refresh_token);
    assert_eq!(room_id, &first_accept.room);
    assert_eq!(*sent_at_secs, PAIR_TEST_NOW);
}

#[test]
fn process_pair_done_ignores_done_when_slot_is_idle_without_side_effects() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = Mutex::new(PairingSlot::Idle);
    let device_id = "unexpected-device";

    let result = process_pair_done(
        &slot,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: PAIR_TEST_ROOM.to_owned(),
            device_id: device_id.to_owned(),
            ..Default::default()
        },
        PAIR_TEST_NOW,
        PAIR_TEST_NOW_MS,
    )
    .unwrap();

    assert!(result.is_none());
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Idle));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert_eq!(
        token_book
            .lock()
            .unwrap()
            .verify_access(device_id, "unused-token", PAIR_TEST_NOW_MS),
        Err(remote_pairing::PairingError::NotFound)
    );
}

#[test]
fn process_pair_done_ignores_done_when_slot_is_waiting_without_side_effects() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = fresh_pairing_slot();
    let original_token = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        session.pairing_token.clone()
    };
    let device_id = "unexpected-device";

    let result = process_pair_done(
        &slot,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: PAIR_TEST_ROOM.to_owned(),
            device_id: device_id.to_owned(),
            ..Default::default()
        },
        PAIR_TEST_NOW,
        PAIR_TEST_NOW_MS,
    )
    .unwrap();

    assert!(result.is_none());
    assert!(matches!(
        &*slot.lock().unwrap(),
        PairingSlot::Waiting(session) if session.pairing_token == original_token
    ));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert_eq!(
        token_book
            .lock()
            .unwrap()
            .verify_access(device_id, "unused-token", PAIR_TEST_NOW_MS),
        Err(remote_pairing::PairingError::NotFound)
    );
}

#[test]
fn process_pair_done_ignores_mismatched_done_without_side_effects() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    let accept = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("valid hello should produce pair.accept");
    let (capability_token, refresh_token) = decrypt_pair_accept_tokens(&slot, &accept);
    let mismatched_device_id = "different-device";

    let result = process_pair_done(
        &slot,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: accept.room.clone(),
            device_id: mismatched_device_id.to_owned(),
            ..Default::default()
        },
        PAIR_TEST_NOW + 1,
        PAIR_TEST_NOW_MS + 1_000,
    )
    .unwrap();

    assert!(result.is_none());
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert_eq!(
        token_book.lock().unwrap().verify_access(
            &accept.device_id,
            &capability_token,
            PAIR_TEST_NOW_MS + 1_000,
        ),
        Err(remote_pairing::PairingError::NotFound)
    );
    assert_eq!(
        token_book.lock().unwrap().verify_access(
            mismatched_device_id,
            &capability_token,
            PAIR_TEST_NOW_MS + 1_000,
        ),
        Err(remote_pairing::PairingError::NotFound)
    );
    let guard = slot.lock().unwrap();
    let PairingSlot::SentAccept {
        outcome,
        room_id,
        sent_at_secs,
        ..
    } = &*guard
    else {
        panic!("mismatched done must leave SentAccept in place")
    };
    assert_eq!(outcome.device_record.device_id, accept.device_id);
    assert_eq!(outcome.k_room_wrapped_ct, accept.k_room_ct);
    assert_eq!(outcome.k_room_wrapped_n, accept.k_room_n);
    assert_eq!(outcome.capability_token, capability_token);
    assert_eq!(outcome.refresh_token, refresh_token);
    assert_eq!(room_id, &accept.room);
    assert_eq!(*sent_at_secs, PAIR_TEST_NOW);
}

#[test]
fn pairing_gateway_persists_and_authorizes_only_after_done() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };

    let accept = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("valid hello should produce pair.accept");
    let (capability_token, _) = decrypt_pair_accept_tokens(&slot, &accept);
    assert!(matches!(
        *slot.lock().unwrap(),
        PairingSlot::SentAccept { .. }
    ));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert_eq!(
        token_book.lock().unwrap().verify_access(
            &accept.device_id,
            &capability_token,
            PAIR_TEST_NOW_MS,
        ),
        Err(remote_pairing::PairingError::NotFound)
    );
    let k_room = remote_pairing::store::resolve_k_room(&key_store, PAIR_TEST_ROOM).unwrap();
    let (confirm_ct, confirm_n) =
        remote_pairing::seal_pair_done_confirm(&k_room, &accept.room, &accept.device_id);

    let completed = process_pair_done(
        &slot,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: accept.room.clone(),
            device_id: accept.device_id.clone(),
            confirm_ct: Some(confirm_ct),
            confirm_n: Some(confirm_n),
            origin_connection_id: "conn-pairing-test".to_owned(),
        },
        PAIR_TEST_NOW + 1,
        PAIR_TEST_NOW_MS + 1_000,
    )
    .unwrap();

    assert_eq!(completed.as_deref(), Some(accept.device_id.as_str()));
    let devices = db::list_remote_devices(&conn).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_id, accept.device_id);
    assert_eq!(
        token_book.lock().unwrap().verify_access(
            &accept.device_id,
            &capability_token,
            PAIR_TEST_NOW_MS + 1_000,
        ),
        Ok(())
    );
    assert!(matches!(
        &*slot.lock().unwrap(),
        PairingSlot::Done { device_id, .. } if device_id == &accept.device_id
    ));
}

#[test]
fn remote_pair_done_waits_for_token_ack_then_replays_ready_idempotently() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    let accept = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("valid hello should produce pair.accept");
    let k_room = remote_pairing::store::resolve_k_room(&key_store, PAIR_TEST_ROOM).unwrap();
    let (confirm_ct, confirm_n) =
        remote_pairing::seal_pair_done_confirm(&k_room, &accept.room, &accept.device_id);
    let done = remote_gateway::PairDoneFrame {
        room: accept.room.clone(),
        device_id: accept.device_id.clone(),
        confirm_ct: Some(confirm_ct),
        confirm_n: Some(confirm_n),
        origin_connection_id: "conn-pairing-test".to_owned(),
    };
    let mut registry = remote_gateway::RegistryState::default();

    let first = process_pair_done_with_registry(
        &slot,
        &mut registry,
        &conn,
        &key_store,
        &token_book,
        done.clone(),
        PAIR_TEST_NOW + 1,
        PAIR_TEST_NOW_MS + 1_000,
    )
    .unwrap();
    assert!(matches!(
        first,
        remote_gateway::PairDoneAction::Accepted {
            newly_paired_device_id: Some(_)
        }
    ));
    let row = db::list_remote_devices(&conn).unwrap().remove(0);
    let generation = row
        .generation
        .expect("done must persist a registry generation");
    let revision_after_first_done = db::current_registry_revision(&conn, PAIR_TEST_ROOM).unwrap();
    assert_eq!(
        row.refresh_until,
        Some((PAIR_TEST_NOW_MS + 1_000 + 2_592_000_000) as i64)
    );
    let subject = format!("device:{}", accept.device_id);
    assert_eq!(
        registry.consume_token_ack(&subject, generation, "rejected"),
        remote_gateway::TokenAckAction::Rejected
    );
    assert!(matches!(
        process_pair_done_with_registry(
            &slot,
            &mut registry,
            &conn,
            &key_store,
            &token_book,
            done.clone(),
            PAIR_TEST_NOW + 2,
            PAIR_TEST_NOW_MS + 2_000,
        )
        .unwrap(),
        remote_gateway::PairDoneAction::Accepted {
            newly_paired_device_id: None
        }
    ));
    let rows_after_replay = db::list_remote_devices(&conn).unwrap();
    assert_eq!(rows_after_replay.len(), 1);
    assert_eq!(rows_after_replay[0].generation, Some(generation));
    assert_eq!(
        db::current_registry_revision(&conn, PAIR_TEST_ROOM).unwrap(),
        revision_after_first_done,
        "done replay must not claim a new registry generation"
    );

    assert!(matches!(
        registry.consume_token_ack(&subject, generation, "ok"),
        remote_gateway::TokenAckAction::PairReady(_)
    ));
    let mut replay_done = done;
    replay_done.origin_connection_id = "conn-pairing-reconnected".to_owned();
    let replay = process_pair_done_with_registry(
        &slot,
        &mut registry,
        &conn,
        &key_store,
        &token_book,
        replay_done,
        PAIR_TEST_NOW + 3,
        PAIR_TEST_NOW_MS + 3_000,
    )
    .unwrap();
    let remote_gateway::PairDoneAction::Ready(ready) = replay else {
        panic!("done replay after token.ack must reproduce pair.ready")
    };
    assert_eq!(ready.room, accept.room);
    assert_eq!(ready.device_id, accept.device_id);
    assert!(!ready.ct.is_empty());
    assert!(!ready.n.is_empty());
}
