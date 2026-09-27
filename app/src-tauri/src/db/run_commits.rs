use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// A `run_commits` ledger row also supplies the persisted data for the inline change card.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RunCommitRow {
    pub session_id: String,
    pub run_id: String,
    pub engine: String,
    pub pre_head: String,
    pub post_head: Option<String>,
    pub commit_sha: Option<String>,
    pub files_changed: Option<u64>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
    pub interrupted: bool,
    pub state: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunCloseoutMetadata {
    pub commit_sha: Option<String>,
    pub files_changed: Option<u64>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
}

pub(super) fn map_run_commit_row(r: &rusqlite::Row) -> rusqlite::Result<RunCommitRow> {
    Ok(RunCommitRow {
        session_id: r.get(0)?,
        run_id: r.get(1)?,
        engine: r.get(2)?,
        pre_head: r.get(3)?,
        post_head: r.get(4)?,
        commit_sha: r.get(5)?,
        files_changed: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        insertions: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        deletions: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        interrupted: r.get::<_, i64>(9)? != 0,
        state: r.get(10)?,
    })
}

pub(super) const RUN_COMMIT_COLS: &str =
    "session_id, run_id, engine, pre_head, post_head, commit_sha, files_changed, insertions, deletions, interrupted, state";

/// Write a pending row before spawning (`state='running'`).
pub fn insert_run_pending(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    engine: &str,
    pre_head: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO run_commits (session_id, run_id, engine, pre_head, state, created_at) \
         VALUES (?1, ?2, ?3, ?4, 'running', strftime('%s','now'))",
        (session_id, run_id, engine, pre_head),
    )?;
    Ok(())
}

pub fn record_run_commit(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    post_head: &str,
    files_changed: Option<u64>,
    insertions: Option<u64>,
    deletions: Option<u64>,
) -> rusqlite::Result<()> {
    let changed = conn.execute(
        "UPDATE run_commits \
         SET post_head = ?3, commit_sha = ?3, files_changed = ?4, insertions = ?5, \
             deletions = ?6, state = 'active' \
         WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![
            session_id,
            run_id,
            post_head,
            files_changed,
            insertions,
            deletions
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

pub fn run_commit(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql =
        format!("SELECT {RUN_COMMIT_COLS} FROM run_commits WHERE session_id = ?1 AND run_id = ?2");
    conn.query_row(&sql, (session_id, run_id), map_run_commit_row)
        .optional()
}

pub fn set_run_commit_state(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    state: &str,
) -> rusqlite::Result<()> {
    let changed = conn.execute(
        "UPDATE run_commits SET state = ?3 WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id, state),
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCommitIntent {
    pub session_id: String,
    pub run_id: String,
    pub expected_head: String,
    pub previous_state: String,
}

pub fn begin_run_commit_intent(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    expected_head: &str,
    previous_state: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO run_commit_intents \
         (session_id, run_id, expected_head, previous_state, created_at) \
         VALUES (?1, ?2, ?3, ?4, strftime('%s','now')) \
         ON CONFLICT(session_id, run_id) DO UPDATE SET \
           expected_head=excluded.expected_head, previous_state=excluded.previous_state, \
           created_at=excluded.created_at",
        (session_id, run_id, expected_head, previous_state),
    )?;
    Ok(())
}

pub fn delete_run_commit_intent(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM run_commit_intents WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
    )?;
    Ok(())
}

pub fn has_run_commit_intent(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM run_commit_intents WHERE session_id = ?1 AND run_id = ?2)",
        (session_id, run_id),
        |row| row.get(0),
    )
}

pub fn list_run_commit_intents(conn: &Connection) -> rusqlite::Result<Vec<RunCommitIntent>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, run_id, expected_head, previous_state \
         FROM run_commit_intents ORDER BY created_at, session_id, run_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RunCommitIntent {
            session_id: row.get(0)?,
            run_id: row.get(1)?,
            expected_head: row.get(2)?,
            previous_state: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// Query the session's last `run_commits` row (by descending id, in any state).
pub fn last_run_commit(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql = format!(
        "SELECT {RUN_COMMIT_COLS} FROM run_commits WHERE session_id = ?1 ORDER BY id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([session_id], map_run_commit_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn last_session_agent_id(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT agent_id FROM messages WHERE session_id = ?1 AND agent_id IS NOT NULL ORDER BY id DESC LIMIT 1",
        [session_id],
        |r| r.get(0),
    ).optional()
}

/// Query the session's last `run_commits` row with `state='active'` (used for undo / reconcile).
pub fn last_active_run_commit(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql = format!(
        "SELECT {RUN_COMMIT_COLS} FROM run_commits \
         WHERE session_id = ?1 AND state = 'active' ORDER BY id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([session_id], map_run_commit_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn latest_recorded_run_commit(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql = format!(
        "SELECT {RUN_COMMIT_COLS} FROM run_commits \
         WHERE session_id = ?1 AND state = 'active' \
           AND post_head IS NOT NULL AND commit_sha IS NOT NULL \
         ORDER BY id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([session_id], map_run_commit_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Review attribution: list all recorded native commit ranges for a session in insertion order.
///
/// The valid states are consistent with `latest_recorded_run_commit`.
pub fn recorded_run_commit_ranges_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT pre_head, post_head FROM run_commits \
         WHERE session_id = ?1 AND state = 'active' \
           AND post_head IS NOT NULL AND commit_sha IS NOT NULL \
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect();
    rows
}

/// Review attribution: read the session starting point from the session's earliest run; runs that produced no commit must also participate.
pub fn earliest_run_pre_head_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT pre_head FROM run_commits \
         WHERE session_id = ?1 ORDER BY id ASC LIMIT 1",
        [session_id],
        |row| row.get(0),
    )
    .optional()
}

/// List every run's state and checkpoint undo counts for a session in ledger insertion order.
///
/// `undo_total` / `undo_undone` are aggregated only from `checkpoint_entries`; this query does not modify the ledger.
pub fn list_run_commit_states(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<(String, String, u64, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT rc.run_id, rc.state, COUNT(ce.id), \
                COALESCE(SUM(CASE WHEN ce.undone_at IS NOT NULL THEN 1 ELSE 0 END), 0) \
         FROM run_commits rc \
         LEFT JOIN checkpoint_entries ce \
           ON ce.session_id = rc.session_id AND ce.run_id = rc.run_id \
         WHERE rc.session_id = ?1 \
         GROUP BY rc.id, rc.run_id, rc.state \
         ORDER BY rc.id",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u64>(2)?,
            r.get::<_, u64>(3)?,
        ))
    })?;
    rows.collect()
}

/// G3-B Overview "Recent Activity": aggregate `run_commits` by the (client's) local calendar day for the most recent 7 days.
///
/// `created_at` is stored uniformly throughout the database as `strftime('%s','now')` (UTC Unix seconds, consistent everywhere in db.rs).
/// The frontend does not know a timezone-independent display convention, so the caller passes `tz_offset_minutes`
/// (= `-new Date().getTimezoneOffset()`, pass 480 for UTC+8).
/// Use SQLite `date()` time modifiers to shift the seconds timestamp to local time before extracting the date,
/// avoiding the invention of new timezone logic on the server (the repository has no chrono/time dependency).
/// This is a read-only aggregation; skip `state='running'` rows that are still running (they do not yet have files_changed/insertions/deletions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecentActivityDay {
    /// Local calendar day, "YYYY-MM-DD".
    pub date: String,
    pub commits: i64,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
    pub failed: i64,
}

pub fn recent_activity_by_day(
    conn: &Connection,
    tz_offset_minutes: i64,
) -> rusqlite::Result<Vec<RecentActivityDay>> {
    let modifier = format!("{tz_offset_minutes:+} minutes");
    let mut stmt = conn.prepare(
        "SELECT date(created_at, 'unixepoch', ?1) AS day, \
                COUNT(*), \
                COALESCE(SUM(files_changed), 0), \
                COALESCE(SUM(insertions), 0), \
                COALESCE(SUM(deletions), 0), \
                COALESCE(SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END), 0) \
         FROM run_commits \
         WHERE state != 'running' \
           AND created_at >= strftime('%s','now') - 7 * 86400 \
         GROUP BY day \
         ORDER BY day DESC \
         LIMIT 7",
    )?;
    let rows = stmt.query_map([&modifier], |r| {
        Ok(RecentActivityDay {
            date: r.get(0)?,
            commits: r.get(1)?,
            files_changed: r.get(2)?,
            insertions: r.get(3)?,
            deletions: r.get(4)?,
            failed: r.get(5)?,
        })
    })?;
    rows.collect()
}
