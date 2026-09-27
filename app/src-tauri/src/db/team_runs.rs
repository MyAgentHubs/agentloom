use super::{map_run_commit_row, RunCloseoutMetadata, RUN_COMMIT_COLS};
use rusqlite::{Connection, OptionalExtension};

/// Agent Team M2 §5.3: one `team_run_pending` row (used for cleanup after recovery / rendering after reload).
/// Pending team run rows support startup recovery, cleanup, and rendering after reload.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct TeamRunPendingRow {
    pub session_id: String,
    pub run_id: String,
    pub goal: Option<String>,
    pub lead_participant_id: Option<String>,
    pub assignments_json: String,
}

#[allow(dead_code)] // Keep a shared column order for pending team run recovery and listing.
const TEAM_RUN_PENDING_COLS: &str =
    "session_id, run_id, goal, lead_participant_id, assignments_json";

#[allow(dead_code)] // Retained to map pending team run rows during recovery.
fn map_team_run_pending_row(r: &rusqlite::Row) -> rusqlite::Result<TeamRunPendingRow> {
    Ok(TeamRunPendingRow {
        session_id: r.get(0)?,
        run_id: r.get(1)?,
        goal: r.get(2)?,
        lead_participant_id: r.get(3)?,
        assignments_json: r.get(4)?,
    })
}

/// Write a pending row before starting a team run (`state='running'`).
#[allow(dead_code)] // Retained for `start_team_run` wiring to persist the pending run.
pub fn insert_team_run_pending(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    goal: &str,
    lead_participant_id: &str,
    assignments_json: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO team_run_pending \
         (session_id, run_id, goal, lead_participant_id, assignments_json, started_at, state, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), 'running', strftime('%s','now'))",
        (
            session_id,
            run_id,
            goal,
            lead_participant_id,
            assignments_json,
        ),
    )?;
    Ok(())
}

/// ④ D32 hygiene: read a given run's `team_run_pending` `assignments_json` (used to clean member workspaces after landing; any state).
pub fn team_run_pending_assignments(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT assignments_json FROM team_run_pending WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
        |r| r.get(0),
    )
    .optional()
}

/// Team run all-member terminal state: mark it done so later recovery no longer processes it.
#[allow(dead_code)] // Retained for the hook that marks a run done when all members finish.
pub fn mark_team_run_done(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE team_run_pending SET state = 'done' \
         WHERE session_id = ?1 AND run_id = ?2 AND state = 'running'",
        (session_id, run_id),
    )?;
    Ok(())
}

/// Startup recovery: scan all running team runs, return their row data first, and then mark them interrupted; idempotent.
#[allow(dead_code)] // Retained for startup recovery wiring.
pub fn recover_interrupted_team_runs(
    conn: &Connection,
) -> rusqlite::Result<Vec<TeamRunPendingRow>> {
    let rows = {
        let sql = format!(
            "SELECT {TEAM_RUN_PENDING_COLS} FROM team_run_pending WHERE state = 'running' ORDER BY id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_team_run_pending_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    conn.execute(
        "UPDATE team_run_pending SET state = 'interrupted' WHERE state = 'running'",
        [],
    )?;
    Ok(rows)
}

/// M2 §5.3: list interrupted team runs for a session (used to render "previous run interrupted" after reload + clean redispatch for C).
pub fn list_interrupted_team_runs(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<TeamRunPendingRow>> {
    let sql = format!(
        "SELECT {TEAM_RUN_PENDING_COLS} FROM team_run_pending \
         WHERE session_id = ?1 AND state = 'interrupted' ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([session_id], map_team_run_pending_row)?;
    rows.collect()
}

/// Empty run: delete the pending row.
pub fn delete_run_pending(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM run_commits WHERE session_id = ?1 AND run_id = ?2 AND state = 'running'",
        (session_id, run_id),
    )?;
    Ok(())
}

/// Solo closeout: keep a checkpoint-hit run active and return RunCard metadata; still delete the pending row for an empty run.
pub fn finalize_run_pending_without_git_writes(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    interrupted: bool,
) -> rusqlite::Result<RunCloseoutMetadata> {
    let recorded = {
        let sql = format!(
            "SELECT {RUN_COMMIT_COLS} FROM run_commits \
             WHERE session_id = ?1 AND run_id = ?2 AND post_head IS NOT NULL"
        );
        conn.query_row(&sql, (session_id, run_id), map_run_commit_row)
            .optional()?
    };
    if let Some(row) = recorded {
        conn.execute(
            "UPDATE run_commits SET interrupted = ?3 WHERE session_id = ?1 AND run_id = ?2",
            rusqlite::params![session_id, run_id, if interrupted { 1_i64 } else { 0_i64 }],
        )?;
        return Ok(RunCloseoutMetadata {
            commit_sha: row.commit_sha,
            files_changed: row.files_changed,
            insertions: row.insertions,
            deletions: row.deletions,
        });
    }

    let checkpoint_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM checkpoint_entries WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
        |row| row.get(0),
    )?;
    if checkpoint_count == 0 {
        delete_run_pending(conn, session_id, run_id)?;
        return Ok(RunCloseoutMetadata::default());
    }

    let changed = conn.execute(
        "UPDATE run_commits SET state = 'active', files_changed = ?3, insertions = 0, deletions = 0, interrupted = ?4 \
         WHERE session_id = ?1 AND run_id = ?2 AND state = 'running'",
        rusqlite::params![
            session_id,
            run_id,
            checkpoint_count,
            if interrupted { 1_i64 } else { 0_i64 }
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }

    Ok(RunCloseoutMetadata {
        commit_sha: None,
        files_changed: Some(checkpoint_count as u64),
        insertions: Some(0),
        deletions: Some(0),
    })
}

/// Crash recovery: mark any leftover running row as failed.
pub fn mark_run_failed(conn: &Connection, session_id: &str, run_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE run_commits SET state = 'failed' WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
    )?;
    Ok(())
}
