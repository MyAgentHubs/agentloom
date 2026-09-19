#![cfg(test)]

use super::*;

/// S1i1 §9.6 refresh 编排测试的公共起点：跑一遍真实配对（hello → done → token.ack=ok）
/// 拿到一个"刚配对完成"的设备——DB 行、TokenBook、钥匙串里的 K_pair 全部就绪，跟真实桌面
/// 进程配对成功后的状态一致，refresh 测试不必再手工缝合半成品状态。
struct RefreshTestFixture {
    conn: Connection,
    key_store: FakeKeyStore,
    token_book: Mutex<remote_pairing::TokenBook>,
    device_id: String,
    room_id: String,
    generation: i64,
    refresh_token: String,
    access_token: String,
}

fn pair_device_for_refresh_test() -> RefreshTestFixture {
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
    let (access_token, refresh_token) = decrypt_pair_accept_tokens(&slot, &accept);
    let k_room = remote_pairing::store::resolve_k_room(&key_store, PAIR_TEST_ROOM).unwrap();
    let (confirm_ct, confirm_n) =
        remote_pairing::seal_pair_done_confirm(&k_room, &accept.room, &accept.device_id);
    let mut throwaway_registry = remote_gateway::RegistryState::default();
    process_pair_done_with_registry(
        &slot,
        &mut throwaway_registry,
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
    let row = db::list_remote_devices(&conn).unwrap().remove(0);
    let generation = row.generation.expect("done must persist a generation");

    RefreshTestFixture {
        conn,
        key_store,
        token_book,
        device_id: accept.device_id,
        room_id: accept.room,
        generation,
        refresh_token,
        access_token,
    }
}

fn seal_refresh_request(
    k_pair: &[u8; 32],
    room_id: &str,
    device_id: &str,
    request_id: &str,
    refresh_token: &str,
) -> (String, String) {
    let meta = remote_pairing::token_refresh_meta(room_id, device_id, request_id);
    let plaintext = serde_json::json!({ "refresh_token": refresh_token }).to_string();
    seal(k_pair, &meta, plaintext.as_bytes())
}

fn open_refresh_ok_tokens(
    k_pair: &[u8; 32],
    room_id: &str,
    device_id: &str,
    request_id: &str,
    ct: &str,
    n: &str,
) -> (String, String) {
    let meta = remote_pairing::token_refresh_ok_meta(room_id, device_id, request_id);
    let plaintext = open(k_pair, &meta, ct, n).expect("refresh.ok body must decrypt under K_pair");
    let tokens: serde_json::Value =
        serde_json::from_slice(&plaintext).expect("refresh.ok body must be JSON");
    (
        tokens["capability_token"].as_str().unwrap().to_owned(),
        tokens["refresh_token"].as_str().unwrap().to_owned(),
    )
}

/// 打一次完整轮换：forward → 断言 `Pending` → 用新 generation 消费 `token.ack("ok")` →
/// 断言吐出 `RefreshOk` → 解出新令牌明文。返回 `(新 generation, 新 access, 新 refresh)`。
fn perform_one_refresh_rotation(
    fixture: &RefreshTestFixture,
    registry: &mut remote_gateway::RegistryState,
    request_id: &str,
    refresh_token: &str,
    now_ms: u64,
) -> (i64, String, String) {
    let k_pair = remote_pairing::store::load_k_pair(&fixture.key_store, &fixture.device_id)
        .unwrap()
        .unwrap();
    let (ct, n) = seal_refresh_request(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        request_id,
        refresh_token,
    );
    let frame = remote_gateway::RefreshForwardFrame {
        request_id: request_id.to_owned(),
        subject: format!("device:{}", fixture.device_id),
        request_generation: 0,
        ct,
        n,
    };
    let mut token_book = fixture.token_book.lock().unwrap();
    let outcome = process_token_refresh_with_registry(
        registry,
        &fixture.conn,
        &fixture.key_store,
        &mut token_book,
        &frame,
        now_ms,
    );
    drop(token_book);
    assert!(
        matches!(outcome, remote_gateway::RefreshOutcome::Pending),
        "命中当前 refresh hash 必须先挂 outbox、等 ack 才回执"
    );

    let row = db::get_remote_device(&fixture.conn, &fixture.device_id)
        .unwrap()
        .unwrap();
    let new_generation = row.generation.unwrap();
    let subject = format!("device:{}", fixture.device_id);
    let action = registry.consume_token_ack(&subject, new_generation, "ok");
    let remote_gateway::TokenAckAction::RefreshOk(refresh_ok) = action else {
        panic!("expected RefreshOk after ack, got {action:?}");
    };
    assert_eq!(refresh_ok.request_id, request_id);
    assert_eq!(refresh_ok.subject, subject);
    assert_eq!(refresh_ok.generation, new_generation);
    let (new_access, new_refresh) = open_refresh_ok_tokens(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        request_id,
        &refresh_ok.ct,
        &refresh_ok.n,
    );
    (new_generation, new_access, new_refresh)
}

#[test]
fn remote_refresh_rotates_current_hash_and_new_tokens_supersede_old_ones() {
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    let old_access = fixture.access_token.clone();
    let old_generation = fixture.generation;

    let (new_generation, new_access, new_refresh) = perform_one_refresh_rotation(
        &fixture,
        &mut registry,
        "req-rotate-1",
        &fixture.refresh_token,
        PAIR_TEST_NOW_MS + 2_000,
    );

    assert!(new_generation > old_generation, "领代必须严格前移");
    assert_ne!(new_access, old_access);
    assert_ne!(new_refresh, fixture.refresh_token);
    let token_book = fixture.token_book.lock().unwrap();
    assert_eq!(
        token_book.verify_access(&fixture.device_id, &new_access, PAIR_TEST_NOW_MS + 2_000),
        Ok(())
    );
    assert_eq!(
        token_book.verify_access(&fixture.device_id, &old_access, PAIR_TEST_NOW_MS + 2_000),
        Err(remote_pairing::PairingError::TokenMismatch),
        "轮换后旧 access token 必须立刻失效"
    );
    let row = db::get_remote_device(&fixture.conn, &fixture.device_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, Some(new_generation));
    assert!(row.refresh_until.unwrap() > row.access_expires_at);
}

#[test]
fn remote_refresh_replay_with_same_request_id_reuses_stored_response_without_mutating_state() {
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    let (new_generation, new_access, new_refresh) = perform_one_refresh_rotation(
        &fixture,
        &mut registry,
        "req-replay-1",
        &fixture.refresh_token,
        PAIR_TEST_NOW_MS + 2_000,
    );
    let row_before = db::get_remote_device(&fixture.conn, &fixture.device_id).unwrap();
    let journal_before = db::load_refresh_journal(&fixture.conn, &fixture.device_id).unwrap();

    let k_pair = remote_pairing::store::load_k_pair(&fixture.key_store, &fixture.device_id)
        .unwrap()
        .unwrap();
    let (ct, n) = seal_refresh_request(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        "req-replay-1",
        &fixture.refresh_token,
    );
    let frame = remote_gateway::RefreshForwardFrame {
        request_id: "req-replay-1".to_owned(),
        subject: format!("device:{}", fixture.device_id),
        request_generation: 0,
        ct,
        n,
    };
    let mut token_book = fixture.token_book.lock().unwrap();
    let outcome = process_token_refresh_with_registry(
        &mut registry,
        &fixture.conn,
        &fixture.key_store,
        &mut token_book,
        &frame,
        PAIR_TEST_NOW_MS + 3_000,
    );
    drop(token_book);
    let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
        panic!("prev 命中 + 相同 request_id 必须立即回执，不再等 ack");
    };
    assert_eq!(value["t"], "token.refresh.ok");
    assert_eq!(value["request_id"], "req-replay-1");
    assert_eq!(value["generation"].as_i64().unwrap(), new_generation);
    let (replayed_access, replayed_refresh) = open_refresh_ok_tokens(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        "req-replay-1",
        value["ct"].as_str().unwrap(),
        value["n"].as_str().unwrap(),
    );
    assert_eq!(replayed_access, new_access, "重放必须原样吐出同一份回执");
    assert_eq!(replayed_refresh, new_refresh);

    // S1i1 R4 返工：只比对解密后的明文抓不住「重放时用新 nonce 重新 seal」这种变异——两次
    // seal 明文可以一致但密文体不同。逐字节比对 ct/n 与 journal 里存的值，才是真正验证了
    // 「原样吐出同一份回执」而不是「重新生成了一份等价的回执」。
    let journal_before_ref = journal_before.as_ref().expect("轮换后 journal 必须已落库");
    assert_eq!(
        value["ct"].as_str().unwrap(),
        journal_before_ref.response_ct,
        "重放回执的密文体必须与 journal 存的逐字节相同"
    );
    assert_eq!(
        value["n"].as_str().unwrap(),
        journal_before_ref.response_n,
        "重放回执的 nonce 必须与 journal 存的逐字节相同"
    );

    assert_eq!(
        db::get_remote_device(&fixture.conn, &fixture.device_id).unwrap(),
        row_before,
        "重放不得写库"
    );
    assert_eq!(
        db::load_refresh_journal(&fixture.conn, &fixture.device_id).unwrap(),
        journal_before,
        "重放不得覆盖 journal"
    );
}

#[test]
fn remote_refresh_replay_after_rebase_uses_current_generation_not_frozen_journal_generation() {
    // S1i1 R1 返工：轮换与重放之间若发生一次 rebase（设备重新领号），重放回执必须用「本次
    // 请求刚读到的设备行当前代号」，不能用 journal 里冻结的轮换时刻旧代号——否则回执带着
    // 陈旧代号出门，被 relay 侧 §9.6 第 246 行的投递谓词丢弃。
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    let (new_generation, _new_access, _new_refresh) = perform_one_refresh_rotation(
        &fixture,
        &mut registry,
        "req-rebase-replay-1",
        &fixture.refresh_token,
        PAIR_TEST_NOW_MS + 2_000,
    );
    let journal_before = db::load_refresh_journal(&fixture.conn, &fixture.device_id)
        .unwrap()
        .expect("轮换后 journal 必须已落库");
    assert_eq!(
        journal_before.generation, new_generation,
        "前提：journal 冻结的就是轮换那一刻的代号"
    );

    // 模拟 rebase：设备在 ack/重放之间重新领号（S1h §9.3 每次 rebase 都给每台设备领一个
    // 新代号）——只需要 DB 侧的重新编号，不涉及 outbox（那是 R1 point 1 的另一半，已有独立
    // 单测覆盖 `rebase_outbox_entries`）。
    rebase_remote_registry(
        &fixture.conn,
        &fixture.room_id,
        0,
        PAIR_TEST_NOW_MS + 2_500,
        false,
        &[],
    )
    .unwrap();
    let rebased_generation = db::get_remote_device(&fixture.conn, &fixture.device_id)
        .unwrap()
        .unwrap()
        .generation
        .unwrap();
    assert!(
        rebased_generation > new_generation,
        "前提：rebase 必须严格前移代号，测试才有意义"
    );

    // 旧 refresh 用同一个 request_id 重放——命中 §2c 幂等重放分支。
    let k_pair = remote_pairing::store::load_k_pair(&fixture.key_store, &fixture.device_id)
        .unwrap()
        .unwrap();
    let (ct, n) = seal_refresh_request(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        "req-rebase-replay-1",
        &fixture.refresh_token,
    );
    let frame = remote_gateway::RefreshForwardFrame {
        request_id: "req-rebase-replay-1".to_owned(),
        subject: format!("device:{}", fixture.device_id),
        request_generation: 0,
        ct,
        n,
    };
    let mut token_book = fixture.token_book.lock().unwrap();
    let outcome = process_token_refresh_with_registry(
        &mut registry,
        &fixture.conn,
        &fixture.key_store,
        &mut token_book,
        &frame,
        PAIR_TEST_NOW_MS + 3_000,
    );
    drop(token_book);
    let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
        panic!("prev 命中 + 相同 request_id 必须立即回执，不再等 ack");
    };
    assert_eq!(value["t"], "token.refresh.ok");
    assert_eq!(
        value["generation"].as_i64().unwrap(),
        rebased_generation,
        "重放回执的 generation 必须是 rebase 后 DB 的当前代号"
    );
    assert_ne!(
        value["generation"].as_i64().unwrap(),
        journal_before.generation,
        "回归防护：确认这条断言真的在验证「不是轮换时刻冻结的旧值」"
    );
    // ct/n 逐字节不变——重放不能因为代号变了就重新 seal（AAD 五元组不含 generation）。
    assert_eq!(value["ct"].as_str().unwrap(), journal_before.response_ct);
    assert_eq!(value["n"].as_str().unwrap(), journal_before.response_n);
}

#[test]
fn remote_refresh_rotation_commit_failure_does_not_burn_invalid_streak() {
    // S1i1 返工二 F2：轮换事务本身失败（DB 报错）是桌面自己的故障，不该烧手机的连续无效
    // 计数。这里用 `remote_registry_rebase_failure_rolls_back_counter_and_all_device_generations`
    // 同款手法（`CREATE TRIGGER ... RAISE(ABORT, ...)`）真注入一次「事务已开始、写到一半
    // SQL 失败」：`refresh_device_tokens`（remote_pairing.rs）在同一个事务里依次领号
    // （`next_registry_generation_in_transaction`）、写 token 哈希（`update_remote_device_tokens`）、
    // 写代号/refresh_until（`set_remote_device_registry_in_transaction`），最后才落 journal
    // （`store_refresh_journal`，`UPDATE remote_devices SET journal_request_id = ...`）——
    // 触发器钉在这条最后写入上，前面几步都已在事务里真正执行过，只有 commit 前最后一条
    // 语句失败，`tx.commit()` 永远不会跑到，整个事务连同前面的写入一起回滚。跟旧版
    // （`now_ms = u64::MAX-10`）不同：旧版在 `refresh_prev_alias_expires_at_ms` 的
    // `i64::try_from` 上就出错（remote_pairing.rs `prepare_refresh`/该函数早于
    // `conn.unchecked_transaction()` 开事务那一行），根本没轮到事务失败，两者只是共用同一个
    // `Err → count_invalid=false` 出口，名不副实——这里改成真的在事务中途失败，测试名才算
    // 名实相符。
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    let k_pair = remote_pairing::store::load_k_pair(&fixture.key_store, &fixture.device_id)
        .unwrap()
        .unwrap();
    let subject = format!("device:{}", fixture.device_id);

    // S1i1 返工三 G3：事务前的快照——`remote_devices` 行的 token_hash/refresh_hash/
    // generation/refresh_until 与 `remote_registry_counter.next_generation`，事务失败后
    // 逐项跟这份快照比对，证明整个事务（不只是 journal 那一笔）真的原样回滚了。
    let device_before = db::get_remote_device(&fixture.conn, &fixture.device_id)
        .unwrap()
        .expect("fixture 必须已落库设备行");
    let counter_before = db::current_registry_revision(&fixture.conn, &fixture.room_id).unwrap();

    fixture
        .conn
        .execute_batch(&format!(
            "CREATE TRIGGER fail_refresh_journal_commit \
                 BEFORE UPDATE OF journal_request_id ON remote_devices \
                 WHEN NEW.device_id = '{}' \
                 BEGIN SELECT RAISE(ABORT, 'injected refresh commit failure'); END;",
            fixture.device_id
        ))
        .unwrap();

    let (ct, n) = seal_refresh_request(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        "req-overflow",
        &fixture.refresh_token,
    );
    let frame = remote_gateway::RefreshForwardFrame {
        request_id: "req-overflow".to_owned(),
        subject: subject.clone(),
        request_generation: 0,
        ct,
        n,
    };
    let mut token_book = fixture.token_book.lock().unwrap();
    let outcome = process_token_refresh_with_registry(
        &mut registry,
        &fixture.conn,
        &fixture.key_store,
        &mut token_book,
        &frame,
        PAIR_TEST_NOW_MS + 3_500,
    );
    drop(token_book);
    let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
        panic!("轮换事务失败必须立即回 fail")
    };
    assert_eq!(value["t"], "token.refresh.fail");
    assert_eq!(value["reason"], "invalid");
    assert!(
        value.get("close").is_none(),
        "桌面自身的轮换事务失败不该带 close"
    );
    // 事务确认真的整体回滚了：不只是 journal 没有落地，事务里更早执行的几步（领号/
    // token 哈希/registry 代号与 refresh_until）也必须原样退回快照前的值——否则触发器
    // 只挡住了最后一条语句，前面几步的写入却悄悄留在了库里，是比「journal 缺一笔」更
    // 隐蔽的半提交 bug（S1i1 返工三 G3：原先只断言 journal == None，证明不了领号/
    // access 哈希/refresh 哈希/registry 代号也回滚了）。
    assert_eq!(
        db::load_refresh_journal(&fixture.conn, &fixture.device_id).unwrap(),
        None,
        "前提：注入的事务失败必须连同前面几步写入一起整体回滚，不能留下半条 journal"
    );
    let device_after = db::get_remote_device(&fixture.conn, &fixture.device_id)
        .unwrap()
        .expect("设备行本身不会被这次失败的轮换删除");
    assert_eq!(
        device_after.token_hash, device_before.token_hash,
        "事务必须整体回滚：token_hash 不能停在轮换写到一半的新值"
    );
    assert_eq!(
        device_after.refresh_hash, device_before.refresh_hash,
        "事务必须整体回滚：refresh_hash 不能停在轮换写到一半的新值"
    );
    assert_eq!(
        device_after.generation, device_before.generation,
        "事务必须整体回滚：registry 代号不能停在轮换写到一半的新值"
    );
    assert_eq!(
        device_after.refresh_until, device_before.refresh_until,
        "事务必须整体回滚：refresh_until 不能停在轮换写到一半的新值"
    );
    let counter_after = db::current_registry_revision(&fixture.conn, &fixture.room_id).unwrap();
    assert_eq!(
        counter_after, counter_before,
        "事务必须整体回滚：领号已经在同一事务内前移的 next_generation 也必须退回原值，\
             不能只回滚 journal 那一笔"
    );
    fixture
        .conn
        .execute_batch("DROP TRIGGER fail_refresh_journal_commit;")
        .unwrap();

    // 紧接着最多 3 次真无效才该触发 close（阈值 3）——如果上面那次事务失败悄悄烧了一次
    // 计数，第 2 次真无效就会提前触发 close。逐次断言前两次都不带 close，只有第 3 次才带，
    // 才是真正能分辨「事务失败有没有计数」的写法（只看最后有没有到 3 分辨不出来——不管
    // 计不计数，多打几次总会到 3）。
    let mut send_bogus = |request_id: &str, now_ms: u64| -> serde_json::Value {
        let (ct, n) = seal_refresh_request(
            &k_pair,
            &fixture.room_id,
            &fixture.device_id,
            request_id,
            &"f".repeat(64),
        );
        let frame = remote_gateway::RefreshForwardFrame {
            request_id: request_id.to_owned(),
            subject: subject.clone(),
            request_generation: 0,
            ct,
            n,
        };
        let mut token_book = fixture.token_book.lock().unwrap();
        let outcome = process_token_refresh_with_registry(
            &mut registry,
            &fixture.conn,
            &fixture.key_store,
            &mut token_book,
            &frame,
            now_ms,
        );
        let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
            panic!("无效 token 必须立即回 fail")
        };
        value
    };
    let first_real = send_bogus("req-real-1", PAIR_TEST_NOW_MS + 4_000);
    assert!(
        first_real.get("close").is_none(),
        "事务失败没计数的话，这应该只是第 1 次真无效"
    );
    let second_real = send_bogus("req-real-2", PAIR_TEST_NOW_MS + 5_000);
    assert!(
        second_real.get("close").is_none(),
        "事务失败没计数的话，这应该只是第 2 次真无效，还不该 close"
    );
    let third_real = send_bogus("req-real-3", PAIR_TEST_NOW_MS + 6_000);
    assert_eq!(third_real["close"], true, "第 3 次真无效才该触发 close");
}

#[test]
fn remote_refresh_unknown_subject_does_not_grow_refresh_quota_map() {
    // S1i1 R5-3 返工：`refresh_quota` map 的 key 是 relay 盖章转发的 frame.subject，未经
    // 桌面自己的 DB 确认——失控/恶意 relay 换着花样报不同的假 subject，此前会让这张内存 map
    // 无限增长（DoS）。只对「DB 里真实存在的设备 subject」记账，查不到的一律不建条目。
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    assert_eq!(registry.refresh_quota_entry_count_for_test(), 0);

    for i in 0..5_u64 {
        let bogus_subject = format!("device:bogus-{i}");
        let frame = remote_gateway::RefreshForwardFrame {
            request_id: format!("req-bogus-{i}"),
            subject: bogus_subject,
            request_generation: 0,
            ct: "whatever-ct".to_owned(),
            n: "whatever-n".to_owned(),
        };
        let mut token_book = fixture.token_book.lock().unwrap();
        let outcome = process_token_refresh_with_registry(
            &mut registry,
            &fixture.conn,
            &fixture.key_store,
            &mut token_book,
            &frame,
            PAIR_TEST_NOW_MS + 2_000 + i,
        );
        drop(token_book);
        assert!(
            matches!(outcome, remote_gateway::RefreshOutcome::Reply(_)),
            "未知设备必须立即回 fail"
        );
    }

    assert_eq!(
        registry.refresh_quota_entry_count_for_test(),
        0,
        "relay 报回的未知/伪造 subject 不该在配额 map 里落地"
    );

    // 真实设备该记的账不受影响——照常能记上一次成功轮换。
    perform_one_refresh_rotation(
        &fixture,
        &mut registry,
        "req-real-after-bogus",
        &fixture.refresh_token,
        PAIR_TEST_NOW_MS + 3_000,
    );
    assert_eq!(
        registry.refresh_quota_entry_count_for_test(),
        1,
        "真实设备的成功轮换仍然要正常记账"
    );
}

#[test]
fn remote_refresh_in_flight_with_different_request_id_fails_without_close_or_journal_mutation() {
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    perform_one_refresh_rotation(
        &fixture,
        &mut registry,
        "req-inflight-1",
        &fixture.refresh_token,
        PAIR_TEST_NOW_MS + 2_000,
    );
    let journal_before = db::load_refresh_journal(&fixture.conn, &fixture.device_id).unwrap();

    let k_pair = remote_pairing::store::load_k_pair(&fixture.key_store, &fixture.device_id)
        .unwrap()
        .unwrap();
    // 仍然拿"旧"（现在已经是 prev）的 refresh_token，但换一个不同的 request_id——这是
    // §9.6 第 251 行说的"同 subject 单飞行中的违反行为"，不是幂等重试。
    let (ct, n) = seal_refresh_request(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        "req-inflight-2",
        &fixture.refresh_token,
    );
    let frame = remote_gateway::RefreshForwardFrame {
        request_id: "req-inflight-2".to_owned(),
        subject: format!("device:{}", fixture.device_id),
        request_generation: 0,
        ct,
        n,
    };
    let mut token_book = fixture.token_book.lock().unwrap();
    let outcome = process_token_refresh_with_registry(
        &mut registry,
        &fixture.conn,
        &fixture.key_store,
        &mut token_book,
        &frame,
        PAIR_TEST_NOW_MS + 3_000,
    );
    drop(token_book);
    let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
        panic!("in_flight 必须立即回 fail，不进 outbox")
    };
    assert_eq!(value["t"], "token.refresh.fail");
    assert_eq!(value["request_id"], "req-inflight-2");
    assert_eq!(value["reason"], "in_flight");
    assert!(
        value.get("close").is_none(),
        "in_flight 是良性单飞行冲突，不带 close"
    );
    assert_eq!(
        db::load_refresh_journal(&fixture.conn, &fixture.device_id).unwrap(),
        journal_before,
        "in_flight 绝不能覆盖 journal（否则第一笔的重放保证失效）"
    );
}

#[test]
fn remote_refresh_unknown_token_fails_and_third_consecutive_invalid_closes() {
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    let k_pair = remote_pairing::store::load_k_pair(&fixture.key_store, &fixture.device_id)
        .unwrap()
        .unwrap();
    let subject = format!("device:{}", fixture.device_id);

    let mut send_bogus = |request_id: &str, now_ms: u64| -> serde_json::Value {
        // 一个跟当前/prev 都不沾边的合法 hex64——AEAD 能正常开封，只是哈希对不上任何已知值。
        let (ct, n) = seal_refresh_request(
            &k_pair,
            &fixture.room_id,
            &fixture.device_id,
            request_id,
            &"f".repeat(64),
        );
        let frame = remote_gateway::RefreshForwardFrame {
            request_id: request_id.to_owned(),
            subject: subject.clone(),
            request_generation: 0,
            ct,
            n,
        };
        let mut token_book = fixture.token_book.lock().unwrap();
        let outcome = process_token_refresh_with_registry(
            &mut registry,
            &fixture.conn,
            &fixture.key_store,
            &mut token_book,
            &frame,
            now_ms,
        );
        let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
            panic!("无效 token 必须立即回 fail")
        };
        assert_eq!(value["t"], "token.refresh.fail");
        value
    };

    let first = send_bogus("req-bad-1", PAIR_TEST_NOW_MS + 2_000);
    assert_eq!(
        first["reason"], "invalid",
        "第 1 次无效 reason 仍是 invalid"
    );
    assert!(first.get("close").is_none(), "第 1 次无效不该带 close");
    let second = send_bogus("req-bad-2", PAIR_TEST_NOW_MS + 3_000);
    assert_eq!(
        second["reason"], "invalid",
        "第 2 次无效 reason 仍是 invalid"
    );
    assert!(second.get("close").is_none(), "第 2 次无效不该带 close");
    let third = send_bogus("req-bad-3", PAIR_TEST_NOW_MS + 4_000);
    assert_eq!(
        third["close"], true,
        "连续第 3 次无效必须带 close:true（§9.6 第 252 行）"
    );
    assert_eq!(
        third["reason"], "invalid_repeated",
        "S1i1 R5-1：带 close 的第 3 次无效 reason 必须是 invalid_repeated，\
             与 fixtures/wire-v1.json 的 token_refresh_fail_valid 样张同词"
    );
}

#[test]
fn remote_refresh_rate_limited_after_six_rotations_does_not_close_or_burn_invalid_streak() {
    let fixture = pair_device_for_refresh_test();
    let mut registry = remote_gateway::RegistryState::default();
    let mut refresh_token = fixture.refresh_token.clone();
    let mut now_ms = PAIR_TEST_NOW_MS + 1_000;

    for round in 0..6 {
        let (_, _, new_refresh) = perform_one_refresh_rotation(
            &fixture,
            &mut registry,
            &format!("req-quota-{round}"),
            &refresh_token,
            now_ms,
        );
        refresh_token = new_refresh;
        now_ms += 1_000;
    }

    // 第 7 次：用的是刚轮换出来、货真价实有效的 refresh token，唯一的问题是配额已经用满。
    let k_pair = remote_pairing::store::load_k_pair(&fixture.key_store, &fixture.device_id)
        .unwrap()
        .unwrap();
    let (ct, n) = seal_refresh_request(
        &k_pair,
        &fixture.room_id,
        &fixture.device_id,
        "req-quota-7th",
        &refresh_token,
    );
    let subject = format!("device:{}", fixture.device_id);
    let frame = remote_gateway::RefreshForwardFrame {
        request_id: "req-quota-7th".to_owned(),
        subject: subject.clone(),
        request_generation: 0,
        ct,
        n,
    };
    let mut token_book = fixture.token_book.lock().unwrap();
    let outcome = process_token_refresh_with_registry(
        &mut registry,
        &fixture.conn,
        &fixture.key_store,
        &mut token_book,
        &frame,
        now_ms,
    );
    drop(token_book);
    let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
        panic!("配额超限必须立即回 fail，不得再轮换")
    };
    assert_eq!(value["t"], "token.refresh.fail");
    assert_eq!(value["reason"], "rate_limited");
    assert!(
        value.get("close").is_none(),
        "配额超限不是「无效请求」，不该带 close"
    );

    // 配额超限不该污染连续无效计数——紧接着两次真无效仍然不该到 3 次上限触发 close。
    let mut send_bogus = |request_id: &str, now_ms: u64| -> serde_json::Value {
        let (ct, n) = seal_refresh_request(
            &k_pair,
            &fixture.room_id,
            &fixture.device_id,
            request_id,
            &"e".repeat(64),
        );
        let frame = remote_gateway::RefreshForwardFrame {
            request_id: request_id.to_owned(),
            subject: subject.clone(),
            request_generation: 0,
            ct,
            n,
        };
        let mut token_book = fixture.token_book.lock().unwrap();
        let outcome = process_token_refresh_with_registry(
            &mut registry,
            &fixture.conn,
            &fixture.key_store,
            &mut token_book,
            &frame,
            now_ms,
        );
        let remote_gateway::RefreshOutcome::Reply(value) = outcome else {
            panic!("无效 token 必须立即回 fail")
        };
        value
    };
    let after_rate_limit_1 = send_bogus("req-quota-bad-1", now_ms + 1_000);
    assert!(after_rate_limit_1.get("close").is_none());
    let after_rate_limit_2 = send_bogus("req-quota-bad-2", now_ms + 2_000);
    assert!(
        after_rate_limit_2.get("close").is_none(),
        "配额超限不计入连续无效计数——这里应该还只是第 2 次真无效"
    );
}
