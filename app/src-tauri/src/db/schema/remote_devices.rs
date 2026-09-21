use rusqlite::Connection;

use super::super::ACCESS_EXPIRES_MILLIS_THRESHOLD;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Store paired-device metadata and token hashes only; actual room and pairing keys remain
    // in the keychain. A NULL revoked_at means the device is still active.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS remote_devices (
            device_id TEXT PRIMARY KEY,
            name TEXT NOT NULL DEFAULT '',
            token_hash TEXT NOT NULL,
            refresh_hash TEXT NOT NULL,
            access_expires_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            revoked_at INTEGER
        )",
        [],
    )?;
    // Extend remote_devices with the token-plane columns. Expiry values use Unix milliseconds;
    // the update below converts legacy seconds to avoid mixed-unit reads during startup.
    let remote_device_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(remote_devices)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        columns
    };
    for (column, declaration) in [
        ("room_id", "TEXT"),
        ("generation", "INTEGER"),
        ("refresh_until", "INTEGER"),
        ("journal_request_id", "TEXT"),
        ("journal_generation", "INTEGER"),
        ("journal_prev_generation", "INTEGER"),
        ("journal_prev_access_hash", "TEXT"),
        ("journal_prev_refresh_hash", "TEXT"),
        ("journal_response_ct", "TEXT"),
        ("journal_response_n", "TEXT"),
        ("journal_prev_expires_at", "INTEGER"),
        ("journal_response_expires", "INTEGER"),
    ] {
        if !remote_device_cols.iter().any(|existing| existing == column) {
            conn.execute(
                &format!("ALTER TABLE remote_devices ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
    }
    conn.execute(
        "UPDATE remote_devices \
            SET access_expires_at = access_expires_at * 1000 \
          WHERE access_expires_at > 0 AND access_expires_at < ?1",
        [ACCESS_EXPIRES_MILLIS_THRESHOLD],
    )?;
    // Older releases had one global room, so NULL rows can be backfilled from the configured room.
    // Without a configured remote_room_id, rows remain NULL and fail closed in room snapshots.
    conn.execute(
        "UPDATE remote_devices \
            SET room_id = (SELECT value FROM app_settings WHERE key = 'remote_room_id') \
          WHERE room_id IS NULL \
            AND EXISTS (SELECT 1 FROM app_settings WHERE key = 'remote_room_id')",
        [],
    )?;
    Ok(())
}
