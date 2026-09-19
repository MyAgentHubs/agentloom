#![cfg(test)]

use super::*;

// T5e2（remote control M0 §5）：remote_devices 表 helper 单测。

fn remote_devices_column_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn.prepare("PRAGMA table_info(remote_devices)").unwrap();
    stmt.query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn remote_devices_insert_and_list_roundtrip() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-1",
        Some("room-a"),
        "iPhone",
        "hash-a",
        "refresh-a",
        1_700_003_600,
        1_700_000_000,
    )
    .unwrap();
    insert_remote_device(
        &conn,
        "dev-2",
        None,
        "",
        "hash-b",
        "refresh-b",
        1_700_003_601_000,
        1_700_000_001,
    )
    .unwrap();

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].device_id, "dev-1");
    assert_eq!(rows[0].name, "iPhone");
    assert_eq!(rows[0].token_hash, "hash-a");
    assert_eq!(rows[0].refresh_hash, "refresh-a");
    assert_eq!(rows[0].access_expires_at, 1_700_003_600_000);
    assert_eq!(rows[0].revoked_at, None);
    assert_eq!(rows[0].room_id.as_deref(), Some("room-a"));
    assert_eq!(rows[0].generation, None);
    assert_eq!(rows[0].refresh_until, None);
    assert_eq!(rows[1].device_id, "dev-2");
    assert_eq!(rows[1].access_expires_at, 1_700_003_601_000);
}

#[test]
fn remote_devices_revoke_is_idempotent_and_preserves_first_revoked_at() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-1",
        None,
        "",
        "hash-a",
        "refresh-a",
        1_700_003_600,
        1_700_000_000,
    )
    .unwrap();

    assert!(revoke_remote_device(&conn, "dev-1", 1_700_001_000).unwrap());
    assert!(!revoke_remote_device(&conn, "dev-1", 1_700_002_000).unwrap());

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows[0].revoked_at, Some(1_700_001_000));
}

#[test]
fn remote_devices_revoke_unknown_device_returns_false() {
    let conn = mem();
    assert!(!revoke_remote_device(&conn, "does-not-exist", 1_700_001_000).unwrap());
}

#[test]
fn remote_devices_update_tokens_rotates_hashes_and_expiry() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-1",
        None,
        "",
        "hash-old",
        "refresh-old",
        1_700_003_600,
        1_700_000_000,
    )
    .unwrap();

    let changed =
        update_remote_device_tokens(&conn, "dev-1", "hash-new", "refresh-new", 1_700_007_200)
            .unwrap();
    assert!(changed);

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows[0].token_hash, "hash-new");
    assert_eq!(rows[0].refresh_hash, "refresh-new");
    assert_eq!(rows[0].access_expires_at, 1_700_007_200_000);

    let changed = update_remote_device_tokens(
        &conn,
        "dev-1",
        "hash-newer",
        "refresh-newer",
        1_700_010_800_000,
    )
    .unwrap();
    assert!(changed);
    assert_eq!(
        list_remote_devices(&conn).unwrap()[0].access_expires_at,
        1_700_010_800_000,
        "毫秒入参必须原样落库，不能再乘一次"
    );
}

#[test]
fn remote_devices_reject_non_positive_access_expiry_writes() {
    let conn = mem();

    for expires_at in [-1, 0] {
        assert!(insert_remote_device(
            &conn,
            &format!("dev-{expires_at}"),
            None,
            "",
            "hash",
            "refresh",
            expires_at,
            1_700_000_000,
        )
        .is_err());
    }

    insert_remote_device(
        &conn,
        "dev-valid",
        None,
        "",
        "hash-old",
        "refresh-old",
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    for expires_at in [-1, 0] {
        assert!(update_remote_device_tokens(
            &conn,
            "dev-valid",
            "hash-new",
            "refresh-new",
            expires_at,
        )
        .is_err());
    }
    assert_eq!(
        list_remote_devices(&conn).unwrap()[0].access_expires_at,
        1_700_003_600_000
    );
}

#[test]
fn remote_devices_update_tokens_does_not_resurrect_revoked_device() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-1",
        None,
        "",
        "hash-old",
        "refresh-old",
        1_700_003_600,
        1_700_000_000,
    )
    .unwrap();
    revoke_remote_device(&conn, "dev-1", 1_700_001_000).unwrap();

    let changed =
        update_remote_device_tokens(&conn, "dev-1", "hash-new", "refresh-new", 1_700_007_200)
            .unwrap();
    assert!(!changed, "已吊销设备不应被 refresh 悄悄复活");

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows[0].token_hash, "hash-old");
}

#[test]
fn remote_devices_init_schema_is_idempotent_for_new_database() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-survives",
        None,
        "",
        "hash-a",
        "refresh-a",
        1_700_003_600,
        1_700_000_000,
    )
    .unwrap();
    init_schema(&conn).unwrap();

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows[0].access_expires_at, 1_700_003_600_000);

    init_schema(&conn).unwrap();

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].device_id, "dev-survives");

    let columns = remote_devices_column_names(&conn);
    for expected in [
        "room_id",
        "generation",
        "refresh_until",
        "journal_request_id",
        "journal_generation",
        "journal_prev_generation",
        "journal_prev_access_hash",
        "journal_prev_refresh_hash",
        "journal_response_ct",
        "journal_response_n",
        "journal_prev_expires_at",
        "journal_response_expires",
    ] {
        assert!(columns.iter().any(|column| column == expected));
    }
}

#[test]
fn remote_devices_without_room_config_keep_legacy_room_id_null() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE remote_devices (
                device_id TEXT PRIMARY KEY,
                name TEXT NOT NULL DEFAULT '',
                token_hash TEXT NOT NULL,
                refresh_hash TEXT NOT NULL,
                access_expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                revoked_at INTEGER
            );
            INSERT INTO remote_devices
                (device_id, name, token_hash, refresh_hash, access_expires_at, created_at, revoked_at)
            VALUES
                ('legacy-device', 'Legacy', 'legacy-access', 'legacy-refresh', 1700003600, 1700000000, 1700000100);",
        )
        .unwrap();

    init_schema(&conn).unwrap();
    assert_eq!(
        list_remote_devices(&conn).unwrap()[0].access_expires_at,
        1_700_003_600_000,
        "旧秒值必须在第一次迁移时回填成毫秒"
    );

    init_schema(&conn).unwrap();

    let columns = remote_devices_column_names(&conn);
    assert_eq!(columns.len(), 19);
    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].device_id, "legacy-device");
    assert_eq!(rows[0].name, "Legacy");
    assert_eq!(rows[0].token_hash, "legacy-access");
    assert_eq!(rows[0].refresh_hash, "legacy-refresh");
    assert_eq!(
        rows[0].access_expires_at, 1_700_003_600_000,
        "迁移重跑不得把已经是毫秒的值再乘 1000"
    );
    assert_eq!(rows[0].created_at, 1_700_000_000);
    assert_eq!(rows[0].revoked_at, Some(1_700_000_100));
    assert_eq!(rows[0].room_id, None);
    assert_eq!(rows[0].generation, None);
    assert_eq!(rows[0].refresh_until, None);
}

#[test]
fn remote_devices_migration_backfills_current_room_without_changing_other_columns() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings(key, value) VALUES('remote_room_id', 'room-current');
             CREATE TABLE remote_devices (
                device_id TEXT PRIMARY KEY,
                name TEXT NOT NULL DEFAULT '',
                token_hash TEXT NOT NULL,
                refresh_hash TEXT NOT NULL,
                access_expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                revoked_at INTEGER
             );
             INSERT INTO remote_devices
                (device_id, name, token_hash, refresh_hash, access_expires_at, created_at, revoked_at)
             VALUES
                ('legacy-device', 'Legacy', 'access-hash', 'refresh-hash',
                 1700003600000, 1700000000, 1700000100);",
        )
        .unwrap();

    init_schema(&conn).unwrap();
    init_schema(&conn).unwrap();

    let row = list_remote_devices(&conn).unwrap().remove(0);
    assert_eq!(row.device_id, "legacy-device");
    assert_eq!(row.name, "Legacy");
    assert_eq!(row.token_hash, "access-hash");
    assert_eq!(row.refresh_hash, "refresh-hash");
    assert_eq!(row.access_expires_at, 1_700_003_600_000);
    assert_eq!(row.created_at, 1_700_000_000);
    assert_eq!(row.revoked_at, Some(1_700_000_100));
    assert_eq!(row.room_id.as_deref(), Some("room-current"));
    assert_eq!(row.generation, None);
    assert_eq!(row.refresh_until, None);
    assert_eq!(row.journal_request_id, None);
    assert_eq!(row.journal_generation, None);
    assert_eq!(row.journal_prev_generation, None);
    assert_eq!(row.journal_prev_access_hash, None);
    assert_eq!(row.journal_prev_refresh_hash, None);
    assert_eq!(row.journal_response_ct, None);
    assert_eq!(row.journal_response_n, None);
    assert_eq!(row.journal_prev_expires_at, None);
    assert_eq!(row.journal_response_expires, None);
}

#[test]
fn remote_devices_migration_leaves_non_positive_expiry_for_fail_closed_loading() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE remote_devices (
                device_id TEXT PRIMARY KEY,
                name TEXT NOT NULL DEFAULT '',
                token_hash TEXT NOT NULL,
                refresh_hash TEXT NOT NULL,
                access_expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                revoked_at INTEGER
            );
            INSERT INTO remote_devices
                (device_id, token_hash, refresh_hash, access_expires_at, created_at)
            VALUES
                ('negative', 'hash-negative', 'refresh-negative', -1, 1700000000),
                ('zero', 'hash-zero', 'refresh-zero', 0, 1700000000);",
    )
    .unwrap();

    init_schema(&conn).unwrap();

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows[0].access_expires_at, -1);
    assert_eq!(rows[1].access_expires_at, 0);
}

#[test]
fn remote_devices_registry_fields_roundtrip() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-registry",
        None,
        "",
        "hash-a",
        "refresh-a",
        1_700_003_600,
        1_700_000_000,
    )
    .unwrap();

    assert!(
        set_remote_device_registry(&conn, "dev-registry", "room-1", 7, 1_700_000_000_000,).unwrap()
    );

    let rows = list_remote_devices(&conn).unwrap();
    assert_eq!(rows[0].room_id.as_deref(), Some("room-1"));
    assert_eq!(rows[0].generation, Some(7));
    assert_eq!(rows[0].refresh_until, Some(1_700_000_000_000));
}

#[test]
fn remote_device_registry_rejects_second_scale_refresh_until() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-registry-seconds",
        None,
        "",
        "hash-a",
        "refresh-a",
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();

    let error =
        set_remote_device_registry(&conn, "dev-registry-seconds", "room-1", 7, 1_700_000_000)
            .unwrap_err();

    assert!(matches!(
        error,
        rusqlite::Error::IntegralValueOutOfRange(_, 1_700_000_000)
    ));
    let row = list_remote_devices(&conn).unwrap().remove(0);
    assert_eq!(row.room_id, None);
    assert_eq!(row.generation, None);
    assert_eq!(row.refresh_until, None);
}

#[test]
fn get_remote_device_returns_none_for_unknown_id() {
    let conn = mem();
    assert_eq!(get_remote_device(&conn, "dev-missing").unwrap(), None);
}

#[test]
fn get_remote_device_matches_the_row_from_list_remote_devices() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-single",
        None,
        "",
        "hash-a",
        "refresh-a",
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    assert!(
        set_remote_device_registry(&conn, "dev-single", "room-1", 3, 1_700_000_000_000,).unwrap()
    );

    let single = get_remote_device(&conn, "dev-single").unwrap().unwrap();
    let listed = list_remote_devices(&conn).unwrap().remove(0);
    assert_eq!(single, listed);
    assert_eq!(single.room_id.as_deref(), Some("room-1"));
    assert_eq!(single.generation, Some(3));
}

#[test]
fn remote_devices_refresh_journal_roundtrip_and_clear() {
    let conn = mem();
    insert_remote_device(
        &conn,
        "dev-journal",
        None,
        "",
        "hash-a",
        "refresh-a",
        1_700_003_600,
        1_700_000_000,
    )
    .unwrap();
    let journal = RemoteRefreshJournal {
        request_id: "request-1".to_string(),
        generation: 8,
        prev_generation: 7,
        prev_access_hash: "prev-access".to_string(),
        prev_refresh_hash: "prev-refresh".to_string(),
        response_ct: "ciphertext".to_string(),
        response_n: "nonce".to_string(),
        prev_expires_at: 1_700_172_800_000,
        response_expires: 1_700_003_600_000,
    };

    assert!(store_refresh_journal(&conn, "dev-journal", &journal).unwrap());
    assert_eq!(
        load_refresh_journal(&conn, "dev-journal").unwrap(),
        Some(journal)
    );
    assert!(clear_refresh_journal(&conn, "dev-journal").unwrap());
    assert_eq!(load_refresh_journal(&conn, "dev-journal").unwrap(), None);
}

#[test]
fn remote_registry_counter_starts_at_one_and_is_monotonic_per_room() {
    let conn = mem();
    assert_eq!(next_registry_generation(&conn, "room-a").unwrap(), 1);
    assert_eq!(next_registry_generation(&conn, "room-a").unwrap(), 2);
    assert_eq!(next_registry_generation(&conn, "room-b").unwrap(), 1);
    assert_eq!(next_registry_generation(&conn, "room-a").unwrap(), 3);
}

#[test]
fn remote_registry_bump_to_is_idempotent_and_never_moves_backward() {
    let conn = mem();
    bump_registry_counter_to(&conn, "room-a", 10).unwrap();
    bump_registry_counter_to(&conn, "room-a", 10).unwrap();
    bump_registry_counter_to(&conn, "room-a", 4).unwrap();
    assert_eq!(next_registry_generation(&conn, "room-a").unwrap(), 11);
    assert_eq!(next_registry_generation(&conn, "room-a").unwrap(), 12);
}
