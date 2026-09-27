use rusqlite::{Connection, OptionalExtension};

/// These are the only valid `session_runtime.status` values, matching the database CHECK constraint literals.
/// They are constants so every write path across the crate can reference them instead of scattering
/// raw string literals.
pub const SESSION_RUNTIME_RUNNING: &str = "running";
pub const SESSION_RUNTIME_IDLE: &str = "idle";

/// `set_session_runtime` is reserved for slot reservation paths that can explicitly write the full status and known run ID.
/// These paths are the solo `reserve_new_session_run` slot reservation and subsequent run_id
/// backfill, and the team `start_team_run` registration.
/// Release paths must not call this full-row setter: whether the session is idle requires a fresh runtime reconciliation.
/// Whether the session is actually idle cannot be determined from the local information held by
/// the current caller. For example, the lead may release a Running slot while a member dispatch
/// intent is still in flight. All such paths must use `crate::refresh_session_runtime` to recompute
/// the truth and then call `upsert_session_runtime_status` below. Callers uniformly use
/// `let _ =` or `eprintln!` so failures remain silent and do not block the main flow. This table
/// is only a best-effort mirror, and a real error must not break message delivery or dispatch.
pub fn set_session_runtime(
    conn: &Connection,
    session_id: &str,
    status: &str,
    run_id: Option<&str>,
) -> rusqlite::Result<bool> {
    let current = conn
        .query_row(
            "SELECT status, run_id FROM session_runtime WHERE session_id = ?1",
            [session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let changed = match &current {
        Some((current_status, current_run_id)) => {
            current_status.as_str() != status || current_run_id.as_deref() != run_id
        }
        None => true,
    };

    conn.execute(
        "INSERT INTO session_runtime (session_id, status, run_id, updated_at) \
         VALUES (?1, ?2, ?3, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET \
            status = excluded.status, \
            run_id = excluded.run_id, \
            updated_at = excluded.updated_at",
        (session_id, status, run_id),
    )?;
    if changed {
        crate::remote_gateway::publish_run_status_milestone(session_id, status, run_id);
    }
    Ok(changed)
}

/// The sole recomputation write path used by `crate::refresh_session_runtime`. It recomputes only
/// the `status` column and preserves the current `run_id` value in the table. Only a new row gets
/// NULL, matching the idle-transition convention in `set_session_runtime`. This is deliberate:
/// the recomputation path itself does not have a run_id because most call sites remove or release a
/// slot after `set_session_runtime` already wrote the run_id during reservation. Without a new
/// value, it must not overwrite the existing one with NULL, or a refresh from an unrelated
/// slot-removal path could mistakenly clear the run_id of a run that is still active.
pub fn upsert_session_runtime_status(
    conn: &Connection,
    session_id: &str,
    status: &str,
) -> rusqlite::Result<bool> {
    let current = conn
        .query_row(
            "SELECT status, run_id FROM session_runtime WHERE session_id = ?1",
            [session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let changed = match &current {
        Some((current_status, _)) => current_status.as_str() != status,
        None => true,
    };

    conn.execute(
        "INSERT INTO session_runtime (session_id, status, run_id, updated_at) \
         VALUES (?1, ?2, NULL, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET \
            status = excluded.status, \
            updated_at = excluded.updated_at",
        (session_id, status),
    )?;
    if changed {
        let run_id = current
            .as_ref()
            .and_then(|(_, current_run_id)| current_run_id.as_deref());
        crate::remote_gateway::publish_run_status_milestone(session_id, status, run_id);
    }
    Ok(changed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRuntime {
    pub session_id: String,
    pub status: String,
    pub run_id: Option<String>,
    pub updated_at: i64,
}

/// Queries one session's runtime state. The `run_slots::get_session_run_state` Tauri command
/// uses this for read-only frontend polling of session run state.
pub fn get_session_runtime(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<SessionRuntime>> {
    conn.query_row(
        "SELECT session_id, status, run_id, updated_at FROM session_runtime WHERE session_id = ?1",
        [session_id],
        |r| {
            Ok(SessionRuntime {
                session_id: r.get(0)?,
                status: r.get(1)?,
                run_id: r.get(2)?,
                updated_at: r.get(3)?,
            })
        },
    )
    .optional()
}

/// Returns all running sessions in the table. There are currently no production call sites; it is
/// retained for a future remote_gateway `session.index` aggregation flow.
#[allow(dead_code)]
pub fn list_running_sessions(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT session_id FROM session_runtime WHERE status = 'running'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Startup reconciliation resets every stale `running` row left by a crash or forced termination
/// in the previous process to `idle`. This performs no data migration; it only restores runtime
/// state and clears `run_id` at the same time, matching the "release slot, then write idle with
/// None" semantics of the four bottleneck paths.
pub fn reconcile_session_runtime_on_startup(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE session_runtime SET status = 'idle', run_id = NULL, updated_at = strftime('%s','now') \
         WHERE status != 'idle'",
        [],
    )?;
    Ok(())
}
