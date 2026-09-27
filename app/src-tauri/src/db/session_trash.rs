use rusqlite::{Connection, OptionalExtension};

pub fn session_has_live_children(conn: &Connection, parent: &str) -> rusqlite::Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sessions WHERE parent_session_id = ?1 AND deleted_at IS NULL
         )",
        [parent],
        |r| r.get(0),
    )?;
    Ok(exists != 0)
}

fn restore_lineage_error(message: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(message.to_string()),
    )
}

/// Soft-delete by setting a second-resolution tombstone (`deleted_at = now`). Keep the session row so
/// it can be restored during the grace period.
pub fn set_session_deleted(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE sessions SET deleted_at = strftime('%s','now') WHERE id = ?1",
        [id],
    )?;
    tx.execute(
        "UPDATE sessions
            SET parent_session_id = NULL
          WHERE parent_session_id = ?1 AND deleted_at IS NULL",
        [id],
    )?;
    tx.execute(
        "UPDATE sessions
            SET continued_to_session_id = NULL
          WHERE id = ?1 OR continued_to_session_id = ?1",
        [id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Undo a soft deletion by clearing the tombstone (`deleted_at = NULL`).
fn preflight_restore_session_lineage(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<Option<String>> {
    let parent_id: Option<Option<String>> = conn
        .query_row(
            "SELECT parent_session_id FROM sessions WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;

    let parent_id = match parent_id {
        None | Some(None) => return Ok(None),
        Some(Some(parent_id)) => parent_id,
    };

    let parent: Option<(Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT deleted_at, continued_to_session_id FROM sessions WHERE id = ?1",
            [parent_id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((parent_deleted_at, parent_continued_to)) = parent else {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.parentMissing",
            &[],
        )));
    };
    if parent_deleted_at.is_some() {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.parentDeleted",
            &[],
        )));
    }
    if parent_continued_to
        .as_deref()
        .is_some_and(|child| child != id)
    {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.parentPointsElsewhere",
            &[],
        )));
    }

    let has_other_live_child: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sessions
             WHERE parent_session_id = ?1 AND id <> ?2 AND deleted_at IS NULL
         )",
        (parent_id.as_str(), id),
        |r| {
            let exists: i64 = r.get(0)?;
            Ok(exists != 0)
        },
    )?;
    if has_other_live_child {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.liveChildExists",
            &[],
        )));
    }

    Ok(Some(parent_id))
}

pub fn preflight_restore_session(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    preflight_restore_session_lineage(conn, id).map(|_| ())
}

pub fn restore_session(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let parent_id = preflight_restore_session_lineage(&tx, id)?;
    tx.execute("UPDATE sessions SET deleted_at = NULL WHERE id = ?1", [id])?;
    if let Some(parent_id) = parent_id {
        tx.execute(
            "UPDATE sessions SET continued_to_session_id = ?2 WHERE id = ?1",
            (parent_id.as_str(), id),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Return IDs of soft-deleted sessions whose grace period has expired (`deleted_at` is non-null and
/// at or before `cutoff`). The caller calculates `cutoff` as now minus the grace duration in seconds,
/// which keeps this function testable.
pub fn list_expired_trashed_sessions(
    conn: &Connection,
    cutoff: i64,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT id FROM sessions WHERE deleted_at IS NOT NULL AND deleted_at <= ?1")?;
    let ids = stmt
        .query_map([cutoff], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

pub fn delete_session(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    // Purge by hard-deleting every session-related row. Soft deletion uses `set_session_deleted`;
    // this function is called only after the grace period expires or for a manual empty-trash action.
    // I3: do not rely on FK CASCADE because `init_schema` does not guarantee `PRAGMA foreign_keys` on
    // every connection. Delete each table explicitly; repeated deletion is harmless and idempotent.
    // I1 (destructive and irreversible atomicity, dual-reviewed by Codex and Opus): wrap every
    // cascading DELETE and the session deletion in one transaction. Any mid-operation failure, such
    // as I/O or SQLITE_BUSY, rolls back everything and never leaves a partially deleted session,
    // following this repository's `unchecked_transaction` convention. An early `?` drops `tx` and
    // rolls back automatically; reaching the end commits everything at once with `tx.commit()`.
    let tx = conn.unchecked_transaction()?;
    // Step 1: collect artifact ids for this session (artifact-scoped tables use artifact_id, not session_id)
    let artifact_ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM artifacts WHERE session_id = ?1")?;
        let ids = stmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    // Step 2: delete artifact-scoped rows
    for aid in &artifact_ids {
        tx.execute(
            "DELETE FROM verifications WHERE artifact_id = ?1",
            [aid.as_str()],
        )?;
        tx.execute("DELETE FROM reviews WHERE artifact_id = ?1", [aid.as_str()])?;
        tx.execute(
            "DELETE FROM merge_candidates WHERE artifact_id = ?1",
            [aid.as_str()],
        )?;
    }
    // Step 3: delete session-scoped rows
    tx.execute(
        "DELETE FROM member_report_delivery WHERE session_id = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM messages WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM attachments WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM memory_blocks WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM memory_entries WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM run_commits WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM run_commit_intents WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM checkpoint_entries WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM team_run_pending WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM decision_ledger WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM lead_loop_state WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM landing_commits WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM goal_contracts WHERE session_id = ?1", [id])?;
    tx.execute(
        "DELETE FROM acceptance_criteria WHERE session_id = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM artifacts WHERE session_id = ?1", [id])?;
    tx.execute(
        "DELETE FROM session_agent_configs WHERE session_id = ?1",
        [id],
    )?;
    // Delete the independent `session_runtime` mirror by the same session key to prevent orphaned runtime state after purge.
    // This table is keyed by `session_id`; omitting this line would leave a permanent orphan after
    // purge, and the remote `session.index` aggregation stream would report a nonexistent session as
    // still running or idle.
    tx.execute("DELETE FROM session_runtime WHERE session_id = ?1", [id])?;
    // T-4b: `remote_inbox` is likewise keyed by `session_id` and declares no FK, so omitting this
    // cascade carries the same risk of a permanent orphan row.
    tx.execute("DELETE FROM remote_inbox WHERE session_id = ?1", [id])?;
    tx.execute(
        "UPDATE sessions
            SET parent_session_id = NULL
          WHERE parent_session_id = ?1 AND deleted_at IS NULL",
        [id],
    )?;
    tx.execute(
        "UPDATE sessions SET continued_to_session_id = NULL WHERE continued_to_session_id = ?1",
        [id],
    )?;
    // Step 4: finally delete the session itself
    let session_row_deleted = tx.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    tx.commit()?;
    if session_row_deleted > 0 {
        crate::remote_gateway::publish_session_index_deleted(id);
    }
    Ok(())
}
