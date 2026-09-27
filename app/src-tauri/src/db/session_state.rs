use rusqlite::{Connection, OptionalExtension};

/// Read-only listing of preimage file paths that remain undoable across all runs in a session.
/// An undone entry cannot prove that later shell changes to the same path are still undoable.
pub fn list_checkpoint_file_paths_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<std::path::PathBuf>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path FROM checkpoint_entries \
         WHERE session_id = ?1 AND undone_at IS NULL ORDER BY file_path",
    )?;
    let rows = stmt.query_map([session_id], |row| {
        row.get::<_, String>(0).map(std::path::PathBuf::from)
    })?;
    rows.collect()
}

/// Commit 2 (undoability tightening, corrected F2/F9 version): the path of each active (not
/// undone) checkpoint record plus the full lifecycle of its run—state / pre_head / post_head /
/// commit_sha.
///
/// **Do not filter the JOIN by state** (the previous bug: `ON ... AND rc.state = 'active' AND
/// rc.post_head IS NOT NULL` made every non-active row, including
/// running/failed/undone/kept/discarded, impossible to find and reduced it to NULL; the caller
/// uniformly treated NULL as "pending and unconditionally fresh," making the check ineffective).
/// State is now returned unchanged, and the branching logic for freshness determination is left
/// to the caller (`filter_fresh_checkpoint_paths`):
/// - `state='active'`: committed; use post_head for the determination (`commit_sha` is also
///   returned, aligning the criteria with the `commit_sha IS NOT NULL` requirement of
///   `recorded_run_commit_ranges_for_session`).
/// - `state='running'`: still running and not yet committed—this is normal for in-place operation
///   (`record_run_commit` occurs only through the delivery broker); `pre_head` is fixed when
///   `insert_run_pending` runs and is NOT NULL in the table definition, so it is sufficient as a
///   reference point for determining whether another commit occurred afterward.
/// - All other cases (`failed`/`undone`/`kept`/`discarded`, or no matching run_commits row at all):
///   cannot be safely verified, so the caller handles them fail-closed.
pub fn list_active_checkpoint_paths_with_run_lifecycle_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<
    Vec<(
        std::path::PathBuf,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )>,
> {
    let mut stmt = conn.prepare(
        "SELECT ce.file_path, rc.state, rc.pre_head, rc.post_head, rc.commit_sha \
         FROM checkpoint_entries ce \
         LEFT JOIN run_commits rc \
           ON rc.session_id = ce.session_id AND rc.run_id = ce.run_id \
         WHERE ce.session_id = ?1 AND ce.undone_at IS NULL",
    )?;
    let rows = stmt.query_map([session_id], |row| {
        Ok((
            std::path::PathBuf::from(row.get::<_, String>(0)?),
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    rows.collect()
}

/// F1 fix: a run's own lifecycle (state / pre_head / post_head / commit_sha), used by
/// `list_run_undo_entries` to determine whether this entire round of checkpoint records is stale
/// because "the file was committed again afterward"—all entries in the same run share one
/// reference point, so unlike the Review-side query that mixes multiple runs, this query does not
/// need to group by path.
pub struct RunLifecycle {
    pub state: String,
    pub pre_head: String,
    pub post_head: Option<String>,
    pub commit_sha: Option<String>,
}

pub fn run_lifecycle_for_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<RunLifecycle>> {
    conn.query_row(
        "SELECT state, pre_head, post_head, commit_sha FROM run_commits \
         WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![session_id, run_id],
        |row| {
            Ok(RunLifecycle {
                state: row.get(0)?,
                pre_head: row.get(1)?,
                post_head: row.get(2)?,
                commit_sha: row.get(3)?,
            })
        },
    )
    .optional()
}

/// Set sessions.git_state (clean | running | commit_failed | diverged).
pub fn set_git_state(conn: &Connection, session_id: &str, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET git_state = ?2 WHERE id = ?1",
        (session_id, state),
    )?;
    Ok(())
}

/// Query sessions.git_state; fall back to 'clean' if the session does not exist or the column is NULL.
pub fn get_git_state(conn: &Connection, session_id: &str) -> rusqlite::Result<String> {
    let mut stmt = conn.prepare("SELECT git_state FROM sessions WHERE id = ?1")?;
    let res = stmt.query_row([session_id], |r| r.get::<_, Option<String>>(0));
    match res {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Ok("clean".to_string()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok("clean".to_string()),
        Err(e) => Err(e),
    }
}

/// session-hover-menu §5: pinned toggle.
pub fn set_session_pinned(conn: &Connection, id: &str, pinned: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET pinned = ?2 WHERE id = ?1",
        (id, pinned),
    )?;
    Ok(())
}

/// session-hover-menu §5: mark-unread toggle (purely a manual marker).
pub fn set_session_unread(conn: &Connection, id: &str, unread: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET unread = ?2 WHERE id = ?1",
        (id, unread),
    )?;
    Ok(())
}

/// session-hover-menu §5: archive toggle. archived=true sets archived_at=now (second precision);
/// false clears it to NULL.
#[cfg(test)]
pub fn set_session_archived(conn: &Connection, id: &str, archived: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET archived = ?2, \
         archived_at = CASE WHEN ?2 THEN strftime('%s','now') ELSE NULL END \
         WHERE id = ?1",
        (id, archived),
    )?;
    Ok(())
}

/// When removing a project, also soft-archive that repo's currently unarchived sessions
/// (archived=1 + archived_at=now at second precision).
/// Non-destructive: does not delete rows, touch deleted_at, or modify other repos. Returns the
/// number of affected rows.
pub fn archive_sessions_for_repo(conn: &Connection, repo_id: &str) -> rusqlite::Result<usize> {
    let ids: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT id FROM sessions WHERE repo_id = ?1 AND archived = 0")?;
        let ids = stmt
            .query_map([repo_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids
    };
    let changed = conn.execute(
        "UPDATE sessions SET archived = 1, archived_at = strftime('%s','now') \
         WHERE repo_id = ?1 AND archived = 0",
        [repo_id],
    )?;
    if !ids.is_empty() {
        // Known boundary: this function does not own the outer transaction in the caller's
        // lib.rs archive_repo_inner—that transaction wraps repos_repo::archive_repo and this
        // function and commits them together. The publish here fires immediately when this
        // function returns, before the outer tx.commit(); if the outer transaction subsequently
        // fails and rolls back, this already-enqueued session.index milestone will not be
        // retracted. The impact is very small, and the full snapshot on the next reconnect will
        // naturally correct it, so it does not block completion of this task.
        crate::remote_gateway::publish_session_index_archived(&ids, true);
    }
    Ok(changed)
}

/// When restoring a project, unarchive all archived sessions in that repo
/// (archived=0 + archived_at=NULL).
/// Known tradeoff: this also unarchives sessions the user manually archived (accepted; KISS).
/// Returns the number of affected rows.
pub fn unarchive_sessions_for_repo(conn: &Connection, repo_id: &str) -> rusqlite::Result<usize> {
    let ids: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT id FROM sessions WHERE repo_id = ?1 AND archived = 1")?;
        let ids = stmt
            .query_map([repo_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids
    };
    let changed = conn.execute(
        "UPDATE sessions SET archived = 0, archived_at = NULL \
         WHERE repo_id = ?1 AND archived = 1",
        [repo_id],
    )?;
    if !ids.is_empty() {
        // Known boundary: this function does not own the outer transaction in the caller's lib.rs
        // restore_repo_inner—the publish precedes the outer tx.commit(); if the outer transaction
        // subsequently fails and rolls back, the enqueued event will not be retracted, and the
        // full snapshot on reconnect will correct it.
        crate::remote_gateway::publish_session_index_archived(&ids, false);
    }
    Ok(changed)
}

pub fn set_sessions_archived(
    conn: &Connection,
    ids: &[String],
    archived: bool,
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let mut changed_ids: Vec<String> = Vec::new();
    for id in ids {
        let changed = tx.execute(
            "UPDATE sessions SET archived = ?2, \
             archived_at = CASE WHEN ?2 THEN strftime('%s','now') ELSE NULL END \
             WHERE id = ?1",
            (id.as_str(), archived),
        )?;
        if changed > 0 {
            changed_ids.push(id.clone());
        }
    }
    tx.commit()?;
    if !changed_ids.is_empty() {
        crate::remote_gateway::publish_session_index_archived(&changed_ids, archived);
    }
    Ok(())
}
