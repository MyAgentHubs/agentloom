#![cfg(test)]

use super::*;
use crate::remote_crypto::{seal, unwrap_key};

const NOW: u64 = 1_700_000_000;
const NOW_MS: u64 = 1_700_000_000_000;
const RELAY_URL: &str = "wss://relay.example.test";
const ROOM_ID: &str = "room-test-123";

#[test]
fn remote_pairing_connect_hash_matches_shared_kdf_fixture() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../remote-relay/fixtures/connect-kdf-v1.json"
    ))
    .unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let pairing_token = fixture["pairing_token_hex"].as_str().unwrap();
        let expected_hash = fixture["expect"]["token_hash_hex"].as_str().unwrap();
        assert_eq!(
            pairing_connect_token_hash(pairing_token).unwrap(),
            expected_hash
        );
    }
}

#[test]
fn pair_accept_tokens_aad_matches_kat_and_seal_round_trips_exact_json() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let fixture = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "aad_kat_pair_accept_tokens")
        .expect("pair.accept token AAD KAT");
    let room = fixture["meta"]["room"].as_str().unwrap();
    let device_id = fixture["device_id"].as_str().unwrap();
    let expected_aad = fixture["expect"]["aad"].as_str().unwrap();
    assert_eq!(
        crate::remote_crypto::build_aad(&pair_accept_tokens_meta(room, device_id)),
        expected_aad
    );

    let key = [0x42_u8; 32];
    let capability_token = "a1".repeat(32);
    let refresh_token = "b2".repeat(32);
    let (tokens_ct, tokens_n) =
        seal_pair_accept_tokens(&key, room, device_id, &capability_token, &refresh_token);
    let plaintext = open(
        &key,
        &pair_accept_tokens_meta(room, device_id),
        &tokens_ct,
        &tokens_n,
    )
    .expect("desktop seal must be readable by a conforming peer");
    assert_eq!(
        plaintext,
        format!(r#"{{"capability_token":"{capability_token}","refresh_token":"{refresh_token}"}}"#)
            .as_bytes()
    );
}

#[test]
fn pair_ready_aad_matches_shared_kat() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let fixture = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "aad_kat_pair_ready")
        .expect("pair.ready AAD KAT");
    let room = fixture["meta"]["room"].as_str().unwrap();
    let device_id = fixture["device_id"].as_str().unwrap();

    assert_eq!(
        crate::remote_crypto::build_aad(&pair_ready_meta(room, device_id)),
        fixture["expect"]["aad"].as_str().unwrap()
    );
}

/// 测试专用 hex64 解码——`pair_ready`/`pair_accept_tokens` 的姊妹 KAT 测试沿用同一个仓库
/// 里没有 `hex` crate 依赖，跟 `remote_gateway.rs` 测试模块里的 `wire_v1_decode_hex_32`
/// 手法一致，各自私有不共享是因为两边分属不同模块、这个 helper 小到不值得开一条
/// `pub(crate)` 通道。
fn decode_hex_32(value: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64, "kat.key_hex 必须是 64 个十六进制字符");
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).expect("kat.key_hex 必须是 ASCII");
        decoded[index] = u8::from_str_radix(pair, 16).expect("kat.key_hex 必须是合法十六进制");
    }
    decoded
}

/// S1i1 R3 返工：此前只有 `remote_gateway::shared_wire_v1_aad_kat_fixtures_build_and_decrypt`
/// 这一条通用测试覆盖 `aad_kat_token_refresh`，但它的 `EnvelopeMeta` 是直接从 fixture JSON
/// 自己拼的，不经过生产 helper `token_refresh_meta`——把生产代码里的 `kind` 从
/// `"token.refresh"` 改错成 `"token-refresh"` 那条测试也照样全绿（实证见本单收尾报告）。
/// 跟 `pair_accept_tokens_aad_matches_kat_and_seal_round_trips_exact_json`/
/// `pair_ready_aad_matches_shared_kat` 同款手法：直接调生产 helper 构造 meta，真正把测试
/// 绑定到实现上。
#[test]
fn token_refresh_aad_and_plaintext_match_shared_kat_via_production_helper() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let fixture = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "aad_kat_token_refresh")
        .expect("token.refresh AAD KAT");
    let room = fixture["meta"]["room"].as_str().unwrap();
    let device_id = fixture["device_id"].as_str().unwrap();
    let request_id = fixture["request_id"].as_str().unwrap();
    let meta = token_refresh_meta(room, device_id, request_id);
    assert_eq!(
        crate::remote_crypto::build_aad(&meta),
        fixture["expect"]["aad"].as_str().unwrap(),
        "token.refresh AAD 必须与生产 helper token_refresh_meta 构造的一致"
    );

    let key = decode_hex_32(fixture["kat"]["key_hex"].as_str().unwrap());
    let plaintext = open(
        &key,
        &meta,
        fixture["kat"]["ct_b64"].as_str().unwrap(),
        fixture["kat"]["n_b64"].as_str().unwrap(),
    )
    .expect("生产 helper 构造的 meta 必须能解开 token.refresh KAT 密文");
    assert_eq!(
        plaintext,
        fixture["kat"]["plaintext"].as_str().unwrap().as_bytes()
    );
}

/// 同上，覆盖 `aad_kat_token_refresh_ok`/`token_refresh_ok_meta`。
#[test]
fn token_refresh_ok_aad_and_plaintext_match_shared_kat_via_production_helper() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let fixture = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "aad_kat_token_refresh_ok")
        .expect("token.refresh.ok AAD KAT");
    let room = fixture["meta"]["room"].as_str().unwrap();
    let device_id = fixture["device_id"].as_str().unwrap();
    let request_id = fixture["request_id"].as_str().unwrap();
    let meta = token_refresh_ok_meta(room, device_id, request_id);
    assert_eq!(
        crate::remote_crypto::build_aad(&meta),
        fixture["expect"]["aad"].as_str().unwrap(),
        "token.refresh.ok AAD 必须与生产 helper token_refresh_ok_meta 构造的一致"
    );

    let key = decode_hex_32(fixture["kat"]["key_hex"].as_str().unwrap());
    let plaintext = open(
        &key,
        &meta,
        fixture["kat"]["ct_b64"].as_str().unwrap(),
        fixture["kat"]["n_b64"].as_str().unwrap(),
    )
    .expect("生产 helper 构造的 meta 必须能解开 token.refresh.ok KAT 密文");
    assert_eq!(
        plaintext,
        fixture["kat"]["plaintext"].as_str().unwrap().as_bytes()
    );
}

/// S1i1 R3 返工：`refresh_ok_json` 产出的键集合必须与 wire-v1 样张 `token_refresh_ok_valid`
/// 的 frame 键集合完全一致——防止两边形状悄悄漂移（多字段/少字段）而没有测试兜底。
#[test]
fn refresh_ok_json_key_set_matches_token_refresh_ok_valid_fixture_frame() {
    use std::collections::BTreeSet;

    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let fixture = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "token_refresh_ok_valid")
        .expect("token_refresh_ok_valid frame fixture");
    let expected_keys: BTreeSet<&str> = fixture["frame"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();

    let produced = crate::remote_gateway::refresh_ok_json("req", "device:x", 1, "ct", "n");
    let produced_keys: BTreeSet<&str> = produced
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(produced_keys, expected_keys);
}

/// S1i1 R4 返工：`PREV_ALIAS_LIFETIME_MS` 此前没有任何测试把它跟 fixtures 里的
/// `ttl_prev_over_cap_clamped.cap_ms` 绑在一起——`refresh_device_tokens_writes_registry_columns_and_journal_in_one_transaction`
/// 那条测试只自比自（`journal.prev_expires_at == rotated.prev_expires_at_ms`，两者都是同一次
/// 调用算出来的，常量改成 24h 照样全绿）。这里直接断言常量本身。
#[test]
fn prev_alias_lifetime_ms_matches_ttl_prev_cap_fixture() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .unwrap();
    let fixture = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["name"] == "ttl_prev_over_cap_clamped")
        .expect("ttl_prev_over_cap_clamped fixture");
    let cap_ms = fixture["cap_ms"].as_u64().expect("cap_ms must be u64");

    assert_eq!(
        PREV_ALIAS_LIFETIME_MS, cap_ms,
        "PREV_ALIAS_LIFETIME_MS 必须与 fixtures/wire-v1.json 的 prev cap 同值——\
             48h 悄悄改成 24h 不该全绿"
    );
}

fn hello_with_plaintext(
    session: &PairingSession,
    plaintext: &[u8],
) -> (HelloFrame, Zeroizing<[u8; 32]>) {
    let (remote_secret, remote_public) = generate_x25519_keypair();
    let remote_k_pair = derive_k_pair(
        &remote_secret,
        &session.desktop_public,
        &session.pairing_token,
    )
    .expect("remote should derive a contributory pairing key");
    let (token_ct_b64, token_n_b64) = seal(
        &remote_k_pair,
        &pairing_envelope_meta(&session.room_id),
        plaintext,
    );
    (
        HelloFrame {
            remote_pub: remote_public,
            token_ct_b64,
            token_n_b64,
        },
        remote_k_pair,
    )
}

fn valid_hello(session: &PairingSession) -> (HelloFrame, Zeroizing<[u8; 32]>) {
    hello_with_plaintext(session, session.pairing_token.as_bytes())
}

fn assert_remote_pairing_rejection_has_no_persistence(
    mut session: PairingSession,
    hello: HelloFrame,
    now_secs: u64,
    expected_error: PairingError,
) {
    use crate::keychain::KeyStore as _;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let key_store = crate::keychain::FakeKeyStore::default();

    let result =
        remote_pairing_handle_hello(&conn, &key_store, &mut session, &hello, now_secs, NOW_MS);
    let expected_message = expected_error.to_string();
    assert_eq!(result.err().as_deref(), Some(expected_message.as_str()));
    assert_eq!(key_store.get(&format!("remote-kroom-{ROOM_ID}")), Ok(None));
    assert!(crate::db::list_remote_devices(&conn).unwrap().is_empty());
}

#[test]
fn qr_payload_uses_protocol_field_names_and_public_key_shape() {
    let (session, qr_payload) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let json = serde_json::to_value(&qr_payload).expect("QR payload should serialize");
    let object = json.as_object().expect("QR payload should be an object");

    for key in ["v", "relay_url", "room", "pairing_token", "desktop_pub"] {
        assert!(object.contains_key(key), "missing QR field: {key}");
    }
    let decoded_public = STANDARD
        .decode(&qr_payload.desktop_pub)
        .expect("desktop public key should be standard base64");
    assert_eq!(decoded_public.len(), 32);
    assert_eq!(qr_payload.pairing_token, session.pairing_token);
    assert_eq!(qr_payload.pairing_token.len(), 64);
    assert!(qr_payload
        .pairing_token
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
}

#[test]
fn pairing_roundtrip_derives_same_key_and_wraps_room_key() {
    let (mut session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello, remote_k_pair) = valid_hello(&session);
    let k_room = *generate_key_32();

    let outcome =
        handle_hello(&mut session, &hello, &k_room, NOW).expect("valid hello should be accepted");

    assert_eq!(&*outcome.device_record.k_pair, &*remote_k_pair);
    let unwrapped = unwrap_key(
        &remote_k_pair,
        &outcome.k_room_wrapped_ct,
        &outcome.k_room_wrapped_n,
    )
    .expect("remote should unwrap the room key");
    assert_eq!(&*unwrapped, &k_room);
    assert_eq!(outcome.capability_token.len(), 64);
    assert_eq!(outcome.refresh_token.len(), 64);
    assert_eq!(outcome.device_record.device_id.len(), 36);
}

#[test]
fn decrypted_wrong_token_is_rejected_without_consuming_session() {
    let (mut session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello, _) = hello_with_plaintext(&session, b"definitely-not-the-pairing-token");
    let k_room = [7_u8; 32];

    assert!(matches!(
        handle_hello(&mut session, &hello, &k_room, NOW),
        Err(PairingError::TokenMismatch)
    ));
    assert!(!session.used);
}

#[test]
fn ciphertext_sealed_with_wrong_key_is_rejected_without_consuming_session() {
    let (mut session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (_, advertised_public) = generate_x25519_keypair();
    let (wrong_secret, _) = generate_x25519_keypair();
    let wrong_k_pair = derive_k_pair(
        &wrong_secret,
        &session.desktop_public,
        &session.pairing_token,
    )
    .expect("wrong secret should still produce a contributory key");
    let (token_ct_b64, token_n_b64) = seal(
        &wrong_k_pair,
        &pairing_envelope_meta(&session.room_id),
        session.pairing_token.as_bytes(),
    );
    let hello = HelloFrame {
        remote_pub: advertised_public,
        token_ct_b64,
        token_n_b64,
    };

    assert!(matches!(
        handle_hello(&mut session, &hello, &[8_u8; 32], NOW),
        Err(PairingError::DecryptFailed)
    ));
    assert!(!session.used);
}

#[test]
fn expired_pairing_is_rejected() {
    let (mut session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello, _) = valid_hello(&session);
    let expired_now = session.expires_at_secs + 1;

    assert!(matches!(
        handle_hello(&mut session, &hello, &[9_u8; 32], expired_now),
        Err(PairingError::Expired)
    ));
    assert!(!session.used);
}

#[test]
fn pairing_expires_at_half_open_boundary() {
    let (mut valid_session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (last_valid_hello, _) = valid_hello(&valid_session);
    let last_valid_instant = valid_session.expires_at_secs - 1;
    assert!(handle_hello(
        &mut valid_session,
        &last_valid_hello,
        &[9_u8; 32],
        last_valid_instant,
    )
    .is_ok());

    let (mut expired_session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (expired_hello, _) = valid_hello(&expired_session);
    let expires_at = expired_session.expires_at_secs;
    assert!(matches!(
        handle_hello(
            &mut expired_session,
            &expired_hello,
            &[9_u8; 32],
            expires_at,
        ),
        Err(PairingError::Expired)
    ));
    assert!(!expired_session.used);
}

#[test]
fn pairing_token_can_only_be_used_once() {
    let (mut session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (first_hello, _) = valid_hello(&session);
    handle_hello(&mut session, &first_hello, &[10_u8; 32], NOW)
        .expect("first hello should succeed");
    let (second_hello, _) = valid_hello(&session);

    assert!(matches!(
        handle_hello(&mut session, &second_hello, &[10_u8; 32], NOW),
        Err(PairingError::AlreadyUsed)
    ));
}

#[test]
fn all_zero_remote_public_key_is_rejected_before_decryption() {
    let (mut session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let hello = HelloFrame {
        remote_pub: [0_u8; 32],
        token_ct_b64: "not-base64".to_owned(),
        token_n_b64: "also-not-base64".to_owned(),
    };

    assert!(matches!(
        handle_hello(&mut session, &hello, &[11_u8; 32], NOW),
        Err(PairingError::NonContributory)
    ));
    assert!(!session.used);
}

#[test]
fn constant_time_equality_has_correct_results() {
    assert!(constant_time_eq(b"same", b"same"));
    assert!(!constant_time_eq(b"same", b"diff"));
    assert!(!constant_time_eq(b"short", b"longer"));
}

#[test]
fn token_book_access_token_expires_after_one_hour() {
    let mut book = TokenBook::new();
    book.insert("device-one".to_owned(), "access-one", "refresh-one", NOW_MS);

    assert_eq!(
        book.verify_access("device-one", "access-one", NOW_MS),
        Ok(())
    );
    assert_eq!(
        book.verify_access("device-one", "access-one", NOW_MS + 3_600_001),
        Err(PairingError::Expired)
    );
}

#[test]
fn token_book_access_expires_at_half_open_boundary() {
    let mut book = TokenBook::new();
    book.insert("device-boundary".to_owned(), "access", "refresh", NOW_MS);

    assert_eq!(
        book.verify_access("device-boundary", "access", NOW_MS + ACCESS_LIFETIME_MS - 1,),
        Ok(())
    );
    assert_eq!(
        book.verify_access("device-boundary", "access", NOW_MS + ACCESS_LIFETIME_MS,),
        Err(PairingError::Expired)
    );
}

#[test]
fn token_book_refresh_rotates_both_tokens_and_burns_old_refresh() {
    let mut book = TokenBook::new();
    book.insert("device-two".to_owned(), "access-old", "refresh-old", NOW_MS);

    let candidate = book
        .prepare_refresh("device-two", "refresh-old", NOW_MS + 10)
        .expect("valid refresh should prepare rotated tokens");
    let new_access = candidate.access_token.clone();
    let new_refresh = candidate.refresh_token.clone();
    book.commit_refresh("device-two", &candidate);
    assert_ne!(new_access, "access-old");
    assert_ne!(new_refresh, "refresh-old");
    assert!(matches!(
        book.prepare_refresh("device-two", "refresh-old", NOW_MS + 11),
        Err(PairingError::TokenMismatch)
    ));
    assert_eq!(
        book.verify_access("device-two", &new_access, NOW_MS + 11),
        Ok(())
    );
}

#[test]
fn token_book_revoke_rejects_access_and_refresh() {
    let mut book = TokenBook::new();
    book.insert(
        "device-three".to_owned(),
        "access-three",
        "refresh-three",
        NOW_MS,
    );
    book.revoke("device-three");

    assert_eq!(
        book.verify_access("device-three", "access-three", NOW_MS),
        Err(PairingError::Revoked)
    );
    assert!(matches!(
        book.prepare_refresh("device-three", "refresh-three", NOW_MS),
        Err(PairingError::Revoked)
    ));
}

#[test]
fn non_contributory_hello_does_not_persist_pairing_state() {
    let (session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let hello = HelloFrame {
        remote_pub: [0_u8; 32],
        token_ct_b64: "not-base64".to_owned(),
        token_n_b64: "also-not-base64".to_owned(),
    };

    assert_remote_pairing_rejection_has_no_persistence(
        session,
        hello,
        NOW,
        PairingError::NonContributory,
    );
}

#[test]
fn wrongly_encrypted_hello_does_not_persist_pairing_state() {
    let (session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (_, advertised_public) = generate_x25519_keypair();
    let (wrong_secret, _) = generate_x25519_keypair();
    let wrong_k_pair = derive_k_pair(
        &wrong_secret,
        &session.desktop_public,
        &session.pairing_token,
    )
    .expect("wrong secret should still produce a contributory key");
    let (token_ct_b64, token_n_b64) = seal(
        &wrong_k_pair,
        &pairing_envelope_meta(&session.room_id),
        session.pairing_token.as_bytes(),
    );
    let hello = HelloFrame {
        remote_pub: advertised_public,
        token_ct_b64,
        token_n_b64,
    };

    assert_remote_pairing_rejection_has_no_persistence(
        session,
        hello,
        NOW,
        PairingError::DecryptFailed,
    );
}

#[test]
fn expired_hello_does_not_persist_pairing_state() {
    let (session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello, _) = valid_hello(&session);
    let expires_at = session.expires_at_secs;

    assert_remote_pairing_rejection_has_no_persistence(
        session,
        hello,
        expires_at,
        PairingError::Expired,
    );
}

#[test]
fn mismatched_token_hello_does_not_persist_pairing_state() {
    let (session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello, _) = hello_with_plaintext(&session, b"definitely-not-the-pairing-token");

    assert_remote_pairing_rejection_has_no_persistence(
        session,
        hello,
        NOW,
        PairingError::TokenMismatch,
    );
}

#[test]
fn remote_pairing_handle_hello_resolves_k_room_and_persists_device() {
    use crate::keychain::KeyStore as _;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let key_store = crate::keychain::FakeKeyStore::default();
    let (mut session, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello, remote_k_pair) = valid_hello(&session);

    let outcome = remote_pairing_handle_hello(&conn, &key_store, &mut session, &hello, NOW, NOW_MS)
        .expect("valid hello should be accepted and persisted");

    assert_eq!(&*outcome.device_record.k_pair, &*remote_k_pair);
    let stored_devices = crate::db::list_remote_devices(&conn).unwrap();
    assert_eq!(stored_devices.len(), 1);
    assert_eq!(stored_devices[0].device_id, outcome.device_record.device_id);
    assert!(key_store
        .get(&format!("remote-kroom-{ROOM_ID}"))
        .unwrap()
        .is_some());
}

#[test]
fn remote_pairing_handle_hello_reuses_existing_k_room_for_same_room() {
    use crate::keychain::KeyStore as _;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let key_store = crate::keychain::FakeKeyStore::default();

    // 第一台设备配对：钥匙串里还没有 K_room，会新生成一把。
    let (mut session_a, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello_a, _) = valid_hello(&session_a);
    remote_pairing_handle_hello(&conn, &key_store, &mut session_a, &hello_a, NOW, NOW_MS).unwrap();
    let k_room_after_first = key_store
        .get(&format!("remote-kroom-{ROOM_ID}"))
        .unwrap()
        .unwrap();

    // 第二台设备配对同一房间：必须复用同一把 K_room，钥匙串里的值不变。
    let (mut session_b, _) = PairingSession::begin(RELAY_URL, ROOM_ID, NOW);
    let (hello_b, _) = valid_hello(&session_b);
    remote_pairing_handle_hello(&conn, &key_store, &mut session_b, &hello_b, NOW, NOW_MS).unwrap();
    let k_room_after_second = key_store
        .get(&format!("remote-kroom-{ROOM_ID}"))
        .unwrap()
        .unwrap();

    assert_eq!(
        k_room_after_first, k_room_after_second,
        "同房间第二台设备必须复用同一把 K_room"
    );
}
