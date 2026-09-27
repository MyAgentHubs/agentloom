use rusqlite::{Connection, OptionalExtension, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDeviceRow {
    pub device_id: String,
    pub name: String,
    pub token_hash: String,
    pub refresh_hash: String,
    /// Unix milliseconds.
    pub access_expires_at: i64,
    /// Unix seconds. This is a legacy UI and audit column; token-expiration columns do not reuse this unit.
    pub created_at: i64,
    /// Unix seconds. This is a legacy UI and audit column.
    pub revoked_at: Option<i64>,
    pub room_id: Option<String>,
    pub generation: Option<i64>,
    /// Unix milliseconds.
    pub refresh_until: Option<i64>,
    pub journal_request_id: Option<String>,
    pub journal_generation: Option<i64>,
    pub journal_prev_generation: Option<i64>,
    pub journal_prev_access_hash: Option<String>,
    pub journal_prev_refresh_hash: Option<String>,
    pub journal_response_ct: Option<String>,
    pub journal_response_n: Option<String>,
    /// Unix milliseconds.
    pub journal_prev_expires_at: Option<i64>,
    /// Unix milliseconds.
    pub journal_response_expires: Option<i64>,
}

/// The current refresh receipt journal for one remote_devices subject.
/// Both expires fields use Unix milliseconds. An entirely NULL column group means there is no
/// in-flight journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRefreshJournal {
    pub request_id: String,
    pub generation: i64,
    pub prev_generation: i64,
    pub prev_access_hash: String,
    pub prev_refresh_hash: String,
    pub response_ct: String,
    pub response_n: String,
    pub prev_expires_at: i64,
    pub response_expires: i64,
}

/// Persists a device-list row after pairing completes. `token_hash` and `refresh_hash` are
/// SHA-256 hexadecimal strings already computed by the caller, `remote_pairing::store`; this
/// function only persists them and does not perform hashing.
pub(super) const ACCESS_EXPIRES_MILLIS_THRESHOLD: i64 = 100_000_000_000;

fn normalize_access_expires_at_millis(access_expires_at_ms: i64) -> rusqlite::Result<i64> {
    if access_expires_at_ms <= 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(
            0,
            access_expires_at_ms,
        ));
    }
    if access_expires_at_ms < ACCESS_EXPIRES_MILLIS_THRESHOLD {
        Ok(access_expires_at_ms * 1000)
    } else {
        Ok(access_expires_at_ms)
    }
}

pub fn insert_remote_device(
    conn: &Connection,
    device_id: &str,
    room_id: Option<&str>,
    name: &str,
    token_hash: &str,
    refresh_hash: &str,
    access_expires_at_ms: i64,
    created_at_secs: i64,
) -> rusqlite::Result<()> {
    // Keep this guard as a write-boundary fallback even after all upstream callers use milliseconds natively, preventing legacy callers from writing seconds again.
    let access_expires_at_ms = normalize_access_expires_at_millis(access_expires_at_ms)?;
    conn.execute(
        "INSERT INTO remote_devices \
         (device_id, room_id, name, token_hash, refresh_hash, access_expires_at, created_at, revoked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
        (
            device_id,
            room_id,
            name,
            token_hash,
            refresh_hash,
            access_expires_at_ms,
            created_at_secs,
        ),
    )?;
    Ok(())
}

/// Returns the complete device list, including revoked devices, in ascending `created_at` order.
/// The UI list and TokenBook reconstruction share this source table; callers filter `revoked_at`
/// as needed to determine whether a device is revoked.
pub fn list_remote_devices(conn: &Connection) -> rusqlite::Result<Vec<RemoteDeviceRow>> {
    let mut stmt = conn.prepare(
        "SELECT device_id, name, token_hash, refresh_hash, access_expires_at, created_at, revoked_at, \
                room_id, generation, refresh_until, journal_request_id, journal_generation, \
                journal_prev_generation, journal_prev_access_hash, journal_prev_refresh_hash, \
                journal_response_ct, journal_response_n, journal_prev_expires_at, \
                journal_response_expires \
           FROM remote_devices ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(RemoteDeviceRow {
            device_id: r.get(0)?,
            name: r.get(1)?,
            token_hash: r.get(2)?,
            refresh_hash: r.get(3)?,
            access_expires_at: r.get(4)?,
            created_at: r.get(5)?,
            revoked_at: r.get(6)?,
            room_id: r.get(7)?,
            generation: r.get(8)?,
            refresh_until: r.get(9)?,
            journal_request_id: r.get(10)?,
            journal_generation: r.get(11)?,
            journal_prev_generation: r.get(12)?,
            journal_prev_access_hash: r.get(13)?,
            journal_prev_refresh_hash: r.get(14)?,
            journal_response_ct: r.get(15)?,
            journal_response_n: r.get(16)?,
            journal_prev_expires_at: r.get(17)?,
            journal_response_expires: r.get(18)?,
        })
    })?;
    rows.collect()
}

/// Fetches one row by device_id. Refresh rotation needs room_id, generation, and journal together,
/// without scanning the full list as `has_active_remote_device_in_room` does. The selected columns
/// match `list_remote_devices`, with only an additional `WHERE device_id = ?1`.
pub fn get_remote_device(
    conn: &Connection,
    device_id: &str,
) -> rusqlite::Result<Option<RemoteDeviceRow>> {
    conn.query_row(
        "SELECT device_id, name, token_hash, refresh_hash, access_expires_at, created_at, revoked_at, \
                room_id, generation, refresh_until, journal_request_id, journal_generation, \
                journal_prev_generation, journal_prev_access_hash, journal_prev_refresh_hash, \
                journal_response_ct, journal_response_n, journal_prev_expires_at, \
                journal_response_expires \
           FROM remote_devices WHERE device_id = ?1",
        [device_id],
        |r| {
            Ok(RemoteDeviceRow {
                device_id: r.get(0)?,
                name: r.get(1)?,
                token_hash: r.get(2)?,
                refresh_hash: r.get(3)?,
                access_expires_at: r.get(4)?,
                created_at: r.get(5)?,
                revoked_at: r.get(6)?,
                room_id: r.get(7)?,
                generation: r.get(8)?,
                refresh_until: r.get(9)?,
                journal_request_id: r.get(10)?,
                journal_generation: r.get(11)?,
                journal_prev_generation: r.get(12)?,
                journal_prev_access_hash: r.get(13)?,
                journal_prev_refresh_hash: r.get(14)?,
                journal_response_ct: r.get(15)?,
                journal_response_n: r.get(16)?,
                journal_prev_expires_at: r.get(17)?,
                journal_response_expires: r.get(18)?,
            })
        },
    )
    .optional()
}

/// Binds the device to a room and generation and writes `refresh_until` in milliseconds.
pub fn set_remote_device_registry(
    conn: &Connection,
    device_id: &str,
    room_id: &str,
    generation: i64,
    refresh_until_ms: i64,
) -> rusqlite::Result<bool> {
    require_millis_timestamp(refresh_until_ms)?;
    let changed = conn.execute(
        "UPDATE remote_devices \
            SET room_id = ?2, generation = ?3, refresh_until = ?4 \
          WHERE device_id = ?1",
        (device_id, room_id, generation, refresh_until_ms),
    )?;
    Ok(changed > 0)
}

fn require_millis_timestamp(value: i64) -> rusqlite::Result<i64> {
    if value < ACCESS_EXPIRES_MILLIS_THRESHOLD {
        return Err(rusqlite::Error::IntegralValueOutOfRange(0, value));
    }
    Ok(value)
}

/// Used by `rebase_remote_registry`: the caller has already opened a transaction, and generation
/// allocation and the device write-back must remain in that same transaction.
pub(crate) fn set_remote_device_registry_in_transaction(
    tx: &Transaction<'_>,
    device_id: &str,
    room_id: &str,
    generation: i64,
    refresh_until_ms: i64,
) -> rusqlite::Result<bool> {
    require_millis_timestamp(refresh_until_ms)?;
    let changed = tx.execute(
        "UPDATE remote_devices \
            SET room_id = ?2, generation = ?3, refresh_until = ?4 \
          WHERE device_id = ?1",
        (device_id, room_id, generation, refresh_until_ms),
    )?;
    Ok(changed > 0)
}

/// Atomically replaces the complete refresh-journal column family for one subject.
pub fn store_refresh_journal(
    conn: &Connection,
    device_id: &str,
    journal: &RemoteRefreshJournal,
) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE remote_devices SET \
            journal_request_id = ?2, journal_generation = ?3, journal_prev_generation = ?4, \
            journal_prev_access_hash = ?5, journal_prev_refresh_hash = ?6, \
            journal_response_ct = ?7, journal_response_n = ?8, \
            journal_prev_expires_at = ?9, journal_response_expires = ?10 \
          WHERE device_id = ?1",
        rusqlite::params![
            device_id,
            journal.request_id,
            journal.generation,
            journal.prev_generation,
            journal.prev_access_hash,
            journal.prev_refresh_hash,
            journal.response_ct,
            journal.response_n,
            journal.prev_expires_at,
            journal.response_expires,
        ],
    )?;
    Ok(changed > 0)
}

/// Reads the current journal. A NULL request_id means there is no in-flight journal.
pub fn load_refresh_journal(
    conn: &Connection,
    device_id: &str,
) -> rusqlite::Result<Option<RemoteRefreshJournal>> {
    conn.query_row(
        "SELECT journal_request_id, journal_generation, journal_prev_generation, \
                journal_prev_access_hash, journal_prev_refresh_hash, journal_response_ct, \
                journal_response_n, journal_prev_expires_at, journal_response_expires \
           FROM remote_devices \
          WHERE device_id = ?1 AND journal_request_id IS NOT NULL",
        [device_id],
        |row| {
            Ok(RemoteRefreshJournal {
                request_id: row.get(0)?,
                generation: row.get(1)?,
                prev_generation: row.get(2)?,
                prev_access_hash: row.get(3)?,
                prev_refresh_hash: row.get(4)?,
                response_ct: row.get(5)?,
                response_n: row.get(6)?,
                prev_expires_at: row.get(7)?,
                response_expires: row.get(8)?,
            })
        },
    )
    .optional()
}

/// Atomically clears the complete refresh-journal column family for one subject.
pub fn clear_refresh_journal(conn: &Connection, device_id: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE remote_devices SET \
            journal_request_id = NULL, journal_generation = NULL, journal_prev_generation = NULL, \
            journal_prev_access_hash = NULL, journal_prev_refresh_hash = NULL, \
            journal_response_ct = NULL, journal_response_n = NULL, \
            journal_prev_expires_at = NULL, journal_response_expires = NULL \
          WHERE device_id = ?1",
        [device_id],
    )?;
    Ok(changed > 0)
}

/// Claims the room's current generation within one transaction and advances `next_generation` by one.
pub fn next_registry_generation(conn: &Connection, room_id: &str) -> rusqlite::Result<i64> {
    let tx = conn.unchecked_transaction()?;
    let generation = next_registry_generation_in_transaction(&tx, room_id)?;
    tx.commit()?;
    Ok(generation)
}

/// The caller has already opened a transaction; do not nest another transaction here. Originally
/// used only by `rebase_remote_registry`, this is also reused by
/// `absorb_registry_high_water_and_reissue_revokes` to allocate new generations to revoke items
/// that remain pending in the outbox after sync.ack.
pub(crate) fn next_registry_generation_in_transaction(
    tx: &Transaction<'_>,
    room_id: &str,
) -> rusqlite::Result<i64> {
    let generation: i64 = tx
        .query_row(
            "SELECT next_generation FROM remote_registry_counter WHERE room_id = ?1",
            [room_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(1);
    let following_generation = generation
        .checked_add(1)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, generation))?;
    tx.execute(
        "INSERT INTO remote_registry_counter (room_id, next_generation) VALUES (?1, ?2) \
         ON CONFLICT(room_id) DO UPDATE SET next_generation = excluded.next_generation",
        (room_id, following_generation),
    )?;
    Ok(generation)
}

/// Uses the current `next_generation` as the snapshot revision, which is always strictly greater
/// than every generation already allocated for this room.
pub fn current_registry_revision(conn: &Connection, room_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT next_generation FROM remote_registry_counter WHERE room_id = ?1",
        [room_id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or(1))
}

/// During rebase, raises the next allocation value to max(current value, floor + 1). Repeated or
/// lower floor values never move it backward.
pub fn bump_registry_counter_to(
    conn: &Connection,
    room_id: &str,
    floor: i64,
) -> rusqlite::Result<()> {
    let next_generation = floor
        .checked_add(1)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, floor))?
        .max(1);
    conn.execute(
        "INSERT INTO remote_registry_counter (room_id, next_generation) VALUES (?1, ?2) \
         ON CONFLICT(room_id) DO UPDATE SET \
            next_generation = max(remote_registry_counter.next_generation, excluded.next_generation)",
        (room_id, next_generation),
    )?;
    Ok(())
}

/// The caller has already opened a transaction, so high-water absorption and generation
/// reallocation commit atomically. Originally used only by `rebase_remote_registry`, this is also
/// reused by `absorb_registry_high_water_and_reissue_revokes` to absorb `relay_high_water`
/// unconditionally after every sync.ack.
pub(crate) fn bump_registry_counter_to_in_transaction(
    tx: &Transaction<'_>,
    room_id: &str,
    floor: i64,
) -> rusqlite::Result<()> {
    let next_generation = floor
        .checked_add(1)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, floor))?
        .max(1);
    tx.execute(
        "INSERT INTO remote_registry_counter (room_id, next_generation) VALUES (?1, ?2) \
         ON CONFLICT(room_id) DO UPDATE SET \
            next_generation = max(remote_registry_counter.next_generation, excluded.next_generation)",
        (room_id, next_generation),
    )?;
    Ok(())
}

/// Revokes a device idempotently. An already revoked row does not have `revoked_at` overwritten,
/// preserving the first revocation time. `Ok(true)` means this call actually changed the device
/// from active to revoked. `Ok(false)` means the device does not exist or was already revoked.
pub fn revoke_remote_device(
    conn: &Connection,
    device_id: &str,
    now_secs: i64,
) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE remote_devices SET revoked_at = ?2 WHERE device_id = ?1 AND revoked_at IS NULL",
        (device_id, now_secs),
    )?;
    Ok(changed > 0)
}

/// Writes back the new token hashes and access expiration after refresh rotation, providing the
/// persistence half of the semantics used by `TokenBook::refresh`. Only non-revoked devices are
/// updated. Refreshes for revoked devices should already be rejected by the Revoked branch in
/// `TokenBook::refresh`; the WHERE clause is a defensive fallback that does not overwrite revoked rows.
pub fn update_remote_device_tokens(
    conn: &Connection,
    device_id: &str,
    token_hash: &str,
    refresh_hash: &str,
    access_expires_at_ms: i64,
) -> rusqlite::Result<bool> {
    // Keep this guard as a write-boundary fallback even after all upstream callers use milliseconds natively, preventing legacy callers from writing seconds again.
    let access_expires_at_ms = normalize_access_expires_at_millis(access_expires_at_ms)?;
    let changed = conn.execute(
        "UPDATE remote_devices \
            SET token_hash = ?2, refresh_hash = ?3, access_expires_at = ?4 \
          WHERE device_id = ?1 AND revoked_at IS NULL",
        (device_id, token_hash, refresh_hash, access_expires_at_ms),
    )?;
    Ok(changed > 0)
}
