use super::now_ms;
use rusqlite::{Connection, OptionalExtension};

/// Looks up only the per-project room table and returns `None` when no row exists.
/// **It does not fall back to the global `remote_room_id`.** That fallback is migration-period
/// behavior and is not implemented here. There are currently no callers of this newly added
/// function, so no call sites are changed.
pub fn remote_room_for_project(
    conn: &Connection,
    project_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT room_id FROM project_remote_rooms WHERE project_id = ?1",
        [project_id],
        |row| row.get(0),
    )
    .optional()
}

/// Returns an existing room directly. If none exists, generates a new one by reusing
/// `remote_pairing::generate_room_id` rather than creating a second generator, persists it, and
/// returns it.
///
/// Concurrency-safe approach: **`INSERT OR IGNORE` followed by another `SELECT`**. This avoids
/// a check-then-insert window in which two concurrent calls could each generate a different
/// room_id and both try to insert it for the same project_id. `project_id` is the primary key, so
/// `INSERT OR IGNORE` silently skips the write on a primary-key conflict. Whether this call wins
/// by inserting its own row or loses because another concurrent call persisted first and this
/// candidate is silently discarded, the subsequent `SELECT` returns the row actually stored in
/// the table. Concurrent calls therefore converge on the same room_id. This guarantee does not
/// depend on callers holding the same lock: SQLite detects PRIMARY KEY and UNIQUE conflicts
/// atomically in the engine, across connections and even across processes. This also covers paths
/// such as `cli_path_override_for_spawn`, where the app occasionally opens a separate connection
/// for a single query instead of using the global `Db(Mutex<Connection>)`.
///
/// **There are two causes for an empty read-back, and only one is normal.** `INSERT OR IGNORE`
/// can silently skip a write because of either a `project_id` primary-key conflict or a `room_id`
/// UNIQUE conflict. A primary-key conflict means another concurrent call persisted a room for the
/// same project first. That is the expected path, and the read-back must find the winning row, so
/// it cannot reach the empty-read-back case. A UNIQUE conflict means the generated candidate
/// matched a room_id already held by a different project. A 128-bit CSPRNG collision is negligible
/// but not impossible and must be handled, or allocation could silently fail while the caller
/// believes it succeeded. Only the second cause can produce an empty read-back, and retrying with a
/// new candidate fixes it. This function retries at most three times. Three collisions indicate a
/// systemic failure, such as a broken uniqueness constraint, so it must report an error instead of
/// retrying silently. **The return value deliberately does not use
/// `rusqlite::Error::QueryReturnedNoRows` as the exhausted-retry sentinel.** Elsewhere this
/// variant conventionally means "row not found = None" and is consumed as `Ok(None)` by
/// `.optional()`. Reusing it here could silently turn a real exhausted-retry failure into an
/// apparently normal `None`. A textual `Result<String, String>` has no `.optional()` method,
/// preventing that mistake. **Callers likewise must not apply `.optional()` to this result.**
///
/// Forward-looking constraints for follow-up work, recorded here to avoid omissions:
/// 1. **The room row itself may be persisted before its credential.** No other consumer of this
///    table will misuse a room merely because its row exists before its credential. The actual
///    invariant is the **configuration publication gate**: `remote_gateway::current_config`
///    includes the room in the `GatewayConfig` returned to the caller only after both the room-row
///    ensure and the room-credential ensure succeed. It must not return configuration when the
///    credential ensure fails. In that case, a resolver error yields no available configuration
///    instead of connecting to or claiming a room whose credential cannot be found. If a crash
///    occurs between persisting the room row and ensuring the credential, no caller can observe
///    that intermediate state because no configuration has been returned. On the next startup,
///    resolving the active project again makes this function idempotently return the same room_id
///    and makes credential ensure idempotently rebuild the credential. The system self-heals
///    without leaving stranded state. This is implemented by
///    `remote_gateway_active_room_resolver` and
///    `ensure_desktop_credential_for_room_cached`.
/// 2. If insertion and read-back are wrapped in an explicit transaction, it must use
///    `TransactionBehavior::Immediate`, not the default Deferred behavior. When a deferred
///    transaction collides during lock escalation, it is not guaranteed to use the
///    `busy_timeout` busy handler. A rollback would also turn the `String` already returned by
///    this function into a ghost room: the result appears successful although the row was rolled
///    back. A caller embedding this function in a larger transaction must either guarantee that it
///    will not roll back or call this function outside the transaction boundary.
/// 3. Deleting a project must also remove its row from this table and revoke the room's
///    `remote_devices` and keychain credential. This table deliberately has no
///    `REFERENCES repos(id)` foreign key, as documented by the table definition in `init_schema`,
///    so `DELETE FROM repos`, including the path in `delete_repo_forever_inner`, does not cascade
///    here. If cleanup is omitted, an orphaned row in this table is merely garbage, but retained
///    `remote_devices` create the false impression that access was revoked even though a device
///    can still connect to the deleted project's room. The existing minimum cleanup only clears
///    `remote_active_repo_id` from `app_settings` when it points to the deleted project. Full
///    cleanup of this table's row, `remote_devices`, and the keychain credential remains undone.
/// 4. `project_id` stores the value of `repos.id`. It is the only column in the database named
///    `project_id` rather than the otherwise common `repo_id`; ownership validation must not
///    mistake it for a second ID system independent of `repos.id`.
pub fn ensure_remote_room_for_project(
    conn: &Connection,
    project_id: &str,
) -> Result<String, String> {
    if let Some(existing) =
        remote_room_for_project(conn, project_id).map_err(|error| error.to_string())?
    {
        return Ok(existing);
    }
    const MAX_ATTEMPTS: u8 = 3;
    for _ in 0..MAX_ATTEMPTS {
        let candidate = crate::remote_pairing::generate_room_id();
        conn.execute(
            "INSERT OR IGNORE INTO project_remote_rooms (project_id, room_id, created_at_ms) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![project_id, candidate, now_ms()],
        )
        .map_err(|error| error.to_string())?;
        if let Some(existing) =
            remote_room_for_project(conn, project_id).map_err(|error| error.to_string())?
        {
            return Ok(existing);
        }
        // The read-back is still empty: the candidate collided with another project's room_id UNIQUE column. Retry with a new candidate.
    }
    Err(format!(
        "ensure_remote_room_for_project: room_id 分配在 {MAX_ATTEMPTS} 次尝试后仍未落库\
         （project_id={project_id}）——大概率是 room_id UNIQUE 约束持续撞列，需要人工核查"
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInboxEntry {
    pub id: i64,
    pub session_id: String,
    pub command_id: String,
    pub kind: String,
    pub payload: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteInboxTerminalState {
    Pending,
    Delivered,
    Failed,
}

/// Queues input while busy. A `command_id` UNIQUE collision is treated as duplicate delivery of
/// the same command, caused by relay retransmission or desktop reconnection backfill, and is
/// discarded idempotently. `Ok(true)` means a new pending row was inserted; `Ok(false)` means a
/// duplicate was found and nothing was written. Its semantics and style match
/// `append_message_dedup`: INSERT OR IGNORE followed by conn.changes().
pub fn enqueue_remote_input(
    conn: &Connection,
    session_id: &str,
    command_id: &str,
    kind: &str,
    payload: &str,
) -> rusqlite::Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO remote_inbox \
         (session_id, command_id, kind, payload, created_at, delivered_at) \
         VALUES (?1, ?2, ?3, ?4, strftime('%s','now'), NULL)",
        (session_id, command_id, kind, payload),
    )?;
    Ok(conn.changes() > 0)
}

/// Re-reads the terminal state from the remote_inbox ledger by `command_id`, allowing the gateway
/// to map acknowledgements consistently after enqueueing or immediate draining. Treating failed as
/// higher priority than delivered is defensive; normal write paths never make both columns non-NULL.
pub fn remote_inbox_terminal_state_by_command_id(
    conn: &Connection,
    command_id: &str,
) -> rusqlite::Result<Option<RemoteInboxTerminalState>> {
    conn.query_row(
        "SELECT delivered_at IS NOT NULL, failed_at IS NOT NULL \
           FROM remote_inbox WHERE command_id = ?1",
        [command_id],
        |row| {
            let delivered = row.get::<_, bool>(0)?;
            let failed = row.get::<_, bool>(1)?;
            Ok(if failed {
                RemoteInboxTerminalState::Failed
            } else if delivered {
                RemoteInboxTerminalState::Delivered
            } else {
                RemoteInboxTerminalState::Pending
            })
        },
    )
    .optional()
}

/// Persistently records a control command that passed the freshness check. A `command_id` UNIQUE
/// collision is treated as a replay. `delivered_at` is set when the new row is inserted, so the
/// row is terminal from creation. Both `next_pending_remote_input` and
/// `sessions_with_pending_remote_input` read only rows where
/// `delivered_at IS NULL AND failed_at IS NULL`, so the remote_inbox drain pipeline can never
/// mistake this control ledger record for input to deliver.
pub fn record_control_command_seen(
    conn: &Connection,
    session_id: &str,
    command_id: &str,
    payload: &str,
) -> rusqlite::Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO remote_inbox \
         (session_id, command_id, kind, payload, created_at, delivered_at) \
         VALUES (?1, ?2, 'control', ?3, strftime('%s','now'), strftime('%s','now'))",
        (session_id, command_id, payload),
    )?;
    Ok(conn.changes() > 0)
}

/// Fetches the next pending `input.send` in FIFO order: not yet delivered or failed, ordered by
/// ascending `id`, which is insertion order. The drain loop calls this one row at a time, delivers
/// and marks each row according to the result, then fetches the next. `None` means draining is complete.
pub fn next_pending_remote_input(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<RemoteInboxEntry>> {
    conn.query_row(
        "SELECT id, session_id, command_id, kind, payload, created_at \
           FROM remote_inbox \
          WHERE session_id = ?1 AND kind = 'input.send' \
            AND delivered_at IS NULL AND failed_at IS NULL \
          ORDER BY id ASC LIMIT 1",
        [session_id],
        |r| {
            Ok(RemoteInboxEntry {
                id: r.get(0)?,
                session_id: r.get(1)?,
                command_id: r.get(2)?,
                kind: r.get(3)?,
                payload: r.get(4)?,
                created_at: r.get(5)?,
            })
        },
    )
    .optional()
}

pub fn mark_remote_input_delivered(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE remote_inbox SET delivered_at = strftime('%s','now') WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// `input.answer` is processed by a dedicated answer thread that carries only the receipt's
/// `command_id`, which is used to write back the successful terminal state.
pub fn mark_remote_input_delivered_by_command_id(
    conn: &Connection,
    command_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE remote_inbox SET delivered_at = strftime('%s','now') WHERE command_id = ?1",
        [command_id],
    )?;
    Ok(())
}

/// Records a genuine, retryable delivery failure and returns the updated attempt count.
pub fn record_remote_input_failure(
    conn: &Connection,
    id: i64,
    error: &str,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "UPDATE remote_inbox \
            SET attempts = attempts + 1, last_error = ?2 \
          WHERE id = ?1 \
          RETURNING attempts",
        (id, error),
        |row| row.get(0),
    )
}

/// Marks a non-retryable message, or one with exhausted retries, as terminally failed so it no
/// longer participates in pending queries.
pub fn mark_remote_input_failed(conn: &Connection, id: i64, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE remote_inbox \
            SET failed_at = strftime('%s','now'), last_error = ?2 \
          WHERE id = ?1",
        (id, error),
    )?;
    Ok(())
}

/// `input.answer` is processed by a dedicated answer thread that carries only the receipt's
/// `command_id`, which is used to write back the failed terminal state.
pub fn mark_remote_input_failed_by_command_id(
    conn: &Connection,
    command_id: &str,
    error: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE remote_inbox SET failed_at = strftime('%s','now'), last_error = ?2 WHERE command_id = ?1",
        (command_id, error),
    )?;
    Ok(())
}

/// Used for restart rescans: returns a deduplicated list of sessions with undelivered pending rows
/// across the table. At process startup all Running and TeamRunning states are idle, so triggering
/// one drain per session is inherently safe. A drain stops if it encounters a busy state and
/// resumes after the real runtime state releases the bottleneck.
pub fn sessions_with_pending_remote_input(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT remote_inbox.session_id \
           FROM remote_inbox \
           JOIN sessions ON sessions.id = remote_inbox.session_id \
          WHERE remote_inbox.delivered_at IS NULL \
            AND remote_inbox.failed_at IS NULL \
            AND sessions.deleted_at IS NULL \
          ORDER BY remote_inbox.session_id",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Used to recover `input.answer` rows at startup: reports only sessions that still have pending
/// answers and have not been soft-deleted. Answers are processed directly by dedicated threads and
/// do not enter the `input.send` FIFO, so they require a separate rescan and cannot rely on the
/// existing drain loop to consume them incidentally.
pub fn sessions_with_pending_remote_answer(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT remote_inbox.session_id \
           FROM remote_inbox \
           JOIN sessions ON sessions.id = remote_inbox.session_id \
          WHERE remote_inbox.kind = 'input.answer' \
            AND remote_inbox.delivered_at IS NULL \
            AND remote_inbox.failed_at IS NULL \
            AND sessions.deleted_at IS NULL \
          ORDER BY remote_inbox.session_id",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Used to recover `input.answer` rows at startup: fetches all pending answers for the session at
/// once and returns them in ledger insertion order. Answers do not require FIFO serialization with
/// one another, and the caller starts a dedicated processing thread for each row. Send rows and
/// rows from other sessions are never included.
pub fn pending_remote_answers(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<RemoteInboxEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, command_id, kind, payload, created_at \
           FROM remote_inbox \
          WHERE session_id = ?1 AND kind = 'input.answer' \
            AND delivered_at IS NULL AND failed_at IS NULL \
          ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(RemoteInboxEntry {
            id: r.get(0)?,
            session_id: r.get(1)?,
            command_id: r.get(2)?,
            kind: r.get(3)?,
            payload: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    rows.collect()
}
