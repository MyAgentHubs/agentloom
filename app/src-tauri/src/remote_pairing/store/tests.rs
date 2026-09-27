#![cfg(test)]

use super::*;
use crate::keychain::FakeKeyStore;
use crate::remote_pairing::{sha256_hex, token_refresh_ok_meta, DeviceRecord, PairingError};
use rusqlite::Connection;
use zeroize::Zeroizing;

const NOW_SECS: u64 = 1_700_000_000;
const NOW_MS: u64 = 1_700_000_000_000;
const ROOM_ID: &str = "room-store-test";

fn mem() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    conn
}
/// Test-only: open the token.refresh.ok receipt ciphertext body. This is not a public API for
/// production code to reuse; it only lets tests retrieve the new plaintext tokens from
/// RotatedTokens::response_ct/n (which do not carry plaintext) while also exercising the actual
/// seal/open core on this path rather than hand-reimplementing an equivalent assertion.
fn decrypt_refresh_ok(
    k_pair: &[u8; 32],
    room_id: &str,
    device_id: &str,
    request_id: &str,
    ct: &str,
    n: &str,
) -> (String, String) {
    let meta = token_refresh_ok_meta(room_id, device_id, request_id);
    let plaintext = crate::remote_crypto::open(k_pair, &meta, ct, n).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
    (
        value["capability_token"].as_str().unwrap().to_owned(),
        value["refresh_token"].as_str().unwrap().to_owned(),
    )
}

fn sample_outcome(device_id: &str) -> AcceptOutcome {
    AcceptOutcome {
        k_room_wrapped_ct: "ct".into(),
        k_room_wrapped_n: "n".into(),
        capability_token: "cap".into(),
        refresh_token: "refresh".into(),
        device_record: DeviceRecord {
            device_id: device_id.to_string(),
            k_pair: Zeroizing::new([7_u8; 32]),
            token_hash: format!("hash-{device_id}"),
            refresh_hash: format!("refresh-hash-{device_id}"),
        },
    }
}

#[test]
fn resolve_k_room_generates_once_and_reuses_afterwards() {
    let store = FakeKeyStore::default();

    let first = resolve_k_room(&store, ROOM_ID).unwrap();
    let second = resolve_k_room(&store, ROOM_ID).unwrap();

    assert_eq!(first, second, "同一房间第二次必须拿到同一把 K_room");
    assert!(store.get(&k_room_key_id(ROOM_ID)).unwrap().is_some());
}

#[test]
fn resolve_k_room_differs_across_rooms() {
    let store = FakeKeyStore::default();

    let room_a = resolve_k_room(&store, "room-a").unwrap();
    let room_b = resolve_k_room(&store, "room-b").unwrap();

    assert_ne!(room_a, room_b);
}

#[test]
fn desktop_credential_is_generated_once_per_room() {
    let store = FakeKeyStore::default();

    let first = resolve_desktop_credential(&store, ROOM_ID).unwrap();
    let second = resolve_desktop_credential(&store, ROOM_ID).unwrap();

    assert_eq!(first.as_str(), second.as_str());
    assert_eq!(first.len(), 64);
    assert!(first
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
}

#[test]
fn desktop_credentials_are_isolated_by_room() {
    let store = FakeKeyStore::default();

    let room_a = resolve_desktop_credential(&store, "room-a").unwrap();
    let room_b = resolve_desktop_credential(&store, "room-b").unwrap();

    assert_ne!(room_a.as_str(), room_b.as_str());
}

#[test]
fn persist_pairing_outcome_writes_keychain_and_db_row() {
    let conn = mem();
    let store = FakeKeyStore::default();
    let outcome = sample_outcome("dev-1");

    persist_pairing_outcome(&conn, &store, ROOM_ID, &outcome, NOW_SECS, NOW_MS).unwrap();

    let stored_k_pair = store.get(&k_pair_key_id("dev-1")).unwrap().unwrap();
    assert_eq!(
        STANDARD.decode(stored_k_pair).unwrap(),
        outcome.device_record.k_pair.to_vec()
    );
    let rows = db::list_remote_devices(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].device_id, "dev-1");
    assert_eq!(rows[0].room_id.as_deref(), Some(ROOM_ID));
    assert_eq!(rows[0].token_hash, "hash-dev-1");
    assert_eq!(
        rows[0].access_expires_at,
        (NOW_MS + ACCESS_LIFETIME_MS) as i64
    );
    assert_eq!(rows[0].revoked_at, None);
}

#[test]
fn persist_pairing_outcome_db_failure_does_not_leave_orphan_k_pair() {
    let conn = mem();
    let store = FakeKeyStore::default();
    let outcome = sample_outcome("dev-duplicate");
    db::insert_remote_device(
        &conn,
        "dev-duplicate",
        None,
        "existing",
        "existing-token-hash",
        "existing-refresh-hash",
        (NOW_MS + ACCESS_LIFETIME_MS) as i64,
        NOW_SECS as i64,
    )
    .unwrap();

    assert!(persist_pairing_outcome(&conn, &store, ROOM_ID, &outcome, NOW_SECS, NOW_MS,).is_err());
    assert_eq!(store.get(&k_pair_key_id("dev-duplicate")).unwrap(), None);
}

#[test]
fn refresh_rotation_survives_token_book_reload_without_reviving_old_refresh() {
    let conn = mem();
    let k_pair = [7_u8; 32];
    db::insert_remote_device(
        &conn,
        "dev-refresh",
        Some(ROOM_ID),
        "",
        &sha256_hex("access-old"),
        &sha256_hex("refresh-old"),
        (NOW_MS + ACCESS_LIFETIME_MS) as i64,
        NOW_SECS as i64,
    )
    .unwrap();
    // Keep the allocated-generation counter aligned with the generation=1 manually written on the row:
    // in the real flow they are always created together through process_pair_done_with_registry, but tests
    // that set up state manually must preserve this alignment or
    // next_registry_generation_in_transaction will issue 1 again.
    db::bump_registry_counter_to(&conn, ROOM_ID, 1).unwrap();
    assert!(db::set_remote_device_registry(
        &conn,
        "dev-refresh",
        ROOM_ID,
        1,
        (NOW_MS + super::super::REFRESH_LIFETIME_MS) as i64,
    )
    .unwrap());
    let mut book = load_token_book(&conn).unwrap();

    let rotated = refresh_device_tokens(
        &conn,
        &mut book,
        "dev-refresh",
        ROOM_ID,
        1,
        &sha256_hex("access-old"),
        &k_pair,
        "req-1",
        "refresh-old",
        NOW_MS + 10,
    )
    .unwrap();
    assert_eq!(rotated.generation, 2, "领代必须严格高于轮换前的 1");
    assert_eq!(rotated.prev_generation, 1);
    let (new_access, new_refresh) = decrypt_refresh_ok(
        &k_pair,
        ROOM_ID,
        "dev-refresh",
        "req-1",
        &rotated.response_ct,
        &rotated.response_n,
    );
    let mut reloaded = load_token_book(&conn).unwrap();
    // The old refresh token now matches "prev" rather than "current": store::refresh_device_tokens
    // handles only the composite write for the current-match branch, while upper-level orchestration
    // handles prev matches. Here we only assert that TokenBook no longer recognizes it as current.
    assert!(reloaded
        .prepare_refresh("dev-refresh", "refresh-old", NOW_MS + 11)
        .is_err());
    assert_eq!(
        reloaded.verify_access("dev-refresh", &new_access, NOW_MS + 11),
        Ok(())
    );
    let row_after_first_rotation = db::get_remote_device(&conn, "dev-refresh")
        .unwrap()
        .unwrap();
    let generation_2 = row_after_first_rotation.generation.unwrap();
    assert_eq!(generation_2, 2);
    let rotated_again = refresh_device_tokens(
        &conn,
        &mut reloaded,
        "dev-refresh",
        ROOM_ID,
        generation_2,
        &row_after_first_rotation.token_hash,
        &k_pair,
        "req-2",
        &new_refresh,
        NOW_MS + 11,
    )
    .unwrap();
    assert_eq!(rotated_again.generation, 3);
    assert_eq!(rotated_again.prev_generation, 2);
}

#[test]
fn refresh_db_rejection_leaves_in_memory_tokens_unchanged() {
    let conn = mem();
    let k_pair = [7_u8; 32];
    db::insert_remote_device(
        &conn,
        "dev-db-rejected",
        Some(ROOM_ID),
        "",
        &sha256_hex("access-old"),
        &sha256_hex("refresh-old"),
        (NOW_MS + ACCESS_LIFETIME_MS) as i64,
        NOW_SECS as i64,
    )
    .unwrap();
    db::bump_registry_counter_to(&conn, ROOM_ID, 1).unwrap();
    assert!(db::set_remote_device_registry(
        &conn,
        "dev-db-rejected",
        ROOM_ID,
        1,
        (NOW_MS + super::super::REFRESH_LIFETIME_MS) as i64,
    )
    .unwrap());
    let mut book = load_token_book(&conn).unwrap();
    let before = book.devices.get("dev-db-rejected").unwrap();
    let before_access_hash = before.access_token_hash.clone();
    let before_refresh_hash = before.refresh_token_hash.clone();
    let before_expires_at = before.access_expires_at_ms;
    db::revoke_remote_device(&conn, "dev-db-rejected", (NOW_SECS + 5) as i64).unwrap();

    assert!(refresh_device_tokens(
        &conn,
        &mut book,
        "dev-db-rejected",
        ROOM_ID,
        1,
        &sha256_hex("access-old"),
        &k_pair,
        "req-1",
        "refresh-old",
        NOW_MS + 10,
    )
    .is_err());

    let after = book.devices.get("dev-db-rejected").unwrap();
    assert_eq!(after.access_token_hash, before_access_hash);
    assert_eq!(after.refresh_token_hash, before_refresh_hash);
    assert_eq!(after.access_expires_at_ms, before_expires_at);
    assert_eq!(
        book.verify_access("dev-db-rejected", "access-old", NOW_MS + 11),
        Ok(())
    );
    assert!(book
        .prepare_refresh("dev-db-rejected", "refresh-old", NOW_MS + 11)
        .is_ok());
    assert_eq!(
        db::load_refresh_journal(&conn, "dev-db-rejected").unwrap(),
        None,
        "DB 拒绝的轮换不得留下 journal 半成品"
    );
}

#[test]
fn refresh_device_tokens_writes_registry_columns_and_journal_in_one_transaction() {
    let conn = mem();
    let k_pair = [9_u8; 32];
    db::insert_remote_device(
        &conn,
        "dev-tx",
        Some(ROOM_ID),
        "",
        &sha256_hex("access-old"),
        &sha256_hex("refresh-old"),
        (NOW_MS + ACCESS_LIFETIME_MS) as i64,
        NOW_SECS as i64,
    )
    .unwrap();
    db::bump_registry_counter_to(&conn, ROOM_ID, 5).unwrap();
    assert!(db::set_remote_device_registry(
        &conn,
        "dev-tx",
        ROOM_ID,
        5,
        (NOW_MS + super::super::REFRESH_LIFETIME_MS) as i64,
    )
    .unwrap());
    let mut book = load_token_book(&conn).unwrap();

    let rotated = refresh_device_tokens(
        &conn,
        &mut book,
        "dev-tx",
        ROOM_ID,
        5,
        &sha256_hex("access-old"),
        &k_pair,
        "req-tx",
        "refresh-old",
        NOW_MS,
    )
    .unwrap();

    let row = db::get_remote_device(&conn, "dev-tx").unwrap().unwrap();
    assert_eq!(row.generation, Some(6));
    assert_eq!(row.token_hash, rotated.access_token_hash);
    let journal = db::load_refresh_journal(&conn, "dev-tx").unwrap().unwrap();
    assert_eq!(journal.request_id, "req-tx");
    assert_eq!(journal.generation, 6);
    assert_eq!(journal.prev_generation, 5);
    assert_eq!(journal.prev_access_hash, sha256_hex("access-old"));
    assert_eq!(journal.prev_refresh_hash, sha256_hex("refresh-old"));
    assert_eq!(journal.response_ct, rotated.response_ct);
    assert_eq!(journal.response_n, rotated.response_n);
    assert_eq!(journal.prev_expires_at, rotated.prev_expires_at_ms);
    assert_eq!(journal.response_expires, rotated.prev_expires_at_ms);
}

#[test]
fn load_token_book_rebuilds_only_unrevoked_devices() {
    let conn = mem();
    let store = FakeKeyStore::default();
    persist_pairing_outcome(
        &conn,
        &store,
        ROOM_ID,
        &sample_outcome("dev-active"),
        NOW_SECS,
        NOW_MS,
    )
    .unwrap();
    persist_pairing_outcome(
        &conn,
        &store,
        ROOM_ID,
        &sample_outcome("dev-revoked"),
        NOW_SECS,
        NOW_MS,
    )
    .unwrap();
    db::revoke_remote_device(&conn, "dev-revoked", NOW_SECS as i64).unwrap();

    let book = load_token_book(&conn).unwrap();

    assert_eq!(
        book.verify_access("dev-active", "irrelevant", NOW_MS),
        Err(PairingError::TokenMismatch),
        "设备应存在于重建后的 TokenBook 里（校验会走到 TokenMismatch 而非 NotFound）"
    );
    assert_eq!(
        book.verify_access("dev-revoked", "irrelevant", NOW_MS),
        Err(PairingError::NotFound),
        "已吊销设备不该出现在重建后的 TokenBook 里"
    );
}

#[test]
fn load_token_book_skips_a_bad_device_without_losing_good_devices() {
    let conn = mem();
    for (device_id, expires_at) in [
        ("dev-good-a", (NOW_MS + 1_000) as i64),
        ("dev-bad", 0),
        ("dev-good-b", (NOW_MS + 2_000) as i64),
    ] {
        conn.execute(
            "INSERT INTO remote_devices \
             (device_id, name, token_hash, refresh_hash, access_expires_at, created_at) \
             VALUES (?1, '', ?2, ?3, ?4, ?5)",
            (
                device_id,
                sha256_hex(&format!("access-{device_id}")),
                sha256_hex(&format!("refresh-{device_id}")),
                expires_at,
                NOW_SECS as i64,
            ),
        )
        .unwrap();
    }

    let book = load_token_book(&conn).unwrap();

    assert_eq!(book.devices.len(), 2);
    assert!(book.devices.contains_key("dev-good-a"));
    assert!(book.devices.contains_key("dev-good-b"));
    assert!(!book.devices.contains_key("dev-bad"));
}

#[test]
fn load_token_book_loads_all_valid_devices() {
    let conn = mem();
    for (offset, device_id) in ["dev-good-a", "dev-good-b"].into_iter().enumerate() {
        db::insert_remote_device(
            &conn,
            device_id,
            None,
            "",
            &sha256_hex(&format!("access-{device_id}")),
            &sha256_hex(&format!("refresh-{device_id}")),
            (NOW_MS + 1_000 + offset as u64) as i64,
            NOW_SECS as i64,
        )
        .unwrap();
    }

    let book = load_token_book(&conn).unwrap();

    assert_eq!(book.devices.len(), 2);
    assert!(book.devices.contains_key("dev-good-a"));
    assert!(book.devices.contains_key("dev-good-b"));
}

#[test]
fn revoke_device_marks_db_and_deletes_keychain_entry() {
    let conn = mem();
    let store = FakeKeyStore::default();
    persist_pairing_outcome(
        &conn,
        &store,
        ROOM_ID,
        &sample_outcome("dev-1"),
        NOW_SECS,
        NOW_MS,
    )
    .unwrap();
    assert!(store.get(&k_pair_key_id("dev-1")).unwrap().is_some());

    revoke_device(&conn, &store, "dev-1", (NOW_SECS + 10) as i64).unwrap();

    let rows = db::list_remote_devices(&conn).unwrap();
    assert_eq!(rows[0].revoked_at, Some((NOW_SECS + 10) as i64));
    assert!(store.get(&k_pair_key_id("dev-1")).unwrap().is_none());
}

#[test]
fn revoke_device_and_sync_invalidates_loaded_and_reloaded_token_books() {
    let conn = mem();
    let store = FakeKeyStore::default();
    let mut outcome = sample_outcome("dev-revoke-sync");
    outcome.device_record.token_hash = sha256_hex("access-active");
    outcome.device_record.refresh_hash = sha256_hex("refresh-active");
    persist_pairing_outcome(&conn, &store, ROOM_ID, &outcome, NOW_SECS, NOW_MS).unwrap();
    let mut book = load_token_book(&conn).unwrap();

    revoke_device_and_sync(
        &conn,
        &store,
        &mut book,
        "dev-revoke-sync",
        (NOW_SECS + 10) as i64,
    )
    .unwrap();

    assert_eq!(
        book.verify_access("dev-revoke-sync", "access-active", NOW_MS + 11),
        Err(PairingError::Revoked)
    );
    let reloaded = load_token_book(&conn).unwrap();
    assert_eq!(
        reloaded.verify_access("dev-revoke-sync", "access-active", NOW_MS + 11),
        Err(PairingError::NotFound)
    );
}
