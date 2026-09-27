use super::*;

#[derive(Serialize)]
pub(super) struct RunCommitState {
    run_id: String,
    state: String,
    undo_total: u64,
    undo_undone: u64,
}

#[tauri::command]
pub(super) fn list_run_commits(
    db: State<Db>,
    session_id: String,
) -> Result<Vec<RunCommitState>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_run_commit_states(&conn, &session_id)
        .map(|states| {
            states
                .into_iter()
                .map(|(run_id, state, undo_total, undo_undone)| RunCommitState {
                    run_id,
                    state,
                    undo_total,
                    undo_undone,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// Read-only Overview aggregation of run_commits activity from all sessions over the last seven days.
/// The frontend passes the local time-zone offset as `tz_offset_minutes` using
/// `-new Date().getTimezoneOffset()`. The server does not infer a time zone; it uses the value only
/// as a SQLite date modifier, as documented by `db::recent_activity_by_day`.
#[tauri::command]
pub(super) fn recent_activity(
    db: State<Db>,
    tz_offset_minutes: i64,
) -> Result<Vec<db::RecentActivityDay>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::recent_activity_by_day(&conn, tz_offset_minutes).map_err(|e| e.to_string())
}

/// Apply the same stale-checkpoint rule to the list that actually drives `undo_run_edits`, rather
/// than only to the badge in Review, which has no per-file undo action. `list_run_commit_states`
/// computes `undo_total` with a plain `COUNT(ce.id)` and does not check freshness, so the RunCard
/// gate alone cannot prevent a stale run from overwriting a later commit. Reuse
/// `filter_fresh_checkpoint_paths` for every entry in this run. A run has one lifecycle, so each
/// entry can use the same (state, pre_head, post_head, commit_sha) tuple without grouping by path.
/// Mark stale entries with `stale = true`; the frontend must disable their selection and show only
/// the reason, so an old snapshot cannot be written back.
pub(super) fn list_run_undo_entries_inner(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> Result<Vec<checkpoint::UndoEntry>, String> {
    let mut entries =
        checkpoint::CheckpointStore::new(conn)?.list_undo_entries(session_id, run_id)?;
    if entries.is_empty() {
        return Ok(entries);
    }
    // Only in-place projects can use Git history to verify freshness. Legacy isolated-worktree
    // sessions have no such concept, so keep stale=false for that out-of-scope path.
    if let Some(project) = inplace_project_path(conn, session_id)? {
        let lifecycle = db::run_lifecycle_for_run(conn, session_id, run_id)
            .map_err(|error| error.to_string())?;
        let (state, pre_head, post_head, commit_sha) = match lifecycle {
            Some(lifecycle) => (
                Some(lifecycle.state),
                Some(lifecycle.pre_head),
                lifecycle.post_head,
                lifecycle.commit_sha,
            ),
            None => (None, None, None, None),
        };
        let tuples: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.file_path.clone(),
                    state.clone(),
                    pre_head.clone(),
                    post_head.clone(),
                    commit_sha.clone(),
                )
            })
            .collect();
        let fresh: std::collections::HashSet<_> = filter_fresh_checkpoint_paths(&project, &tuples)?
            .into_iter()
            .collect();
        for entry in &mut entries {
            entry.stale = !fresh.contains(&entry.file_path);
        }
    }
    Ok(entries)
}

/// Check every checkpoint path written by the session when delivering the current branch; SQL
/// DISTINCT deduplicates paths across runs.
pub(super) fn list_session_undo_paths_inner(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<std::path::PathBuf>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT file_path FROM checkpoint_entries \
             WHERE session_id = ?1 ORDER BY file_path",
        )
        .map_err(|error| error.to_string())?;
    let paths = stmt
        .query_map([session_id], |row| {
            Ok(std::path::PathBuf::from(row.get::<_, String>(0)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    Ok(paths)
}

fn ensure_undo_session_idle(
    conn: &Connection,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
) -> Result<(), String> {
    let solo_running = running
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .contains_key(session_id);
    let team_in_memory = team_running.is_session_running(session_id)?;
    let team_in_db = conn
        .query_row(
            "SELECT 1 FROM team_run_pending WHERE session_id = ?1 AND state = 'running' LIMIT 1",
            [session_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .is_some();
    if solo_running || team_in_memory || team_in_db {
        Err(format!("UNDO_RUN_ACTIVE:{session_id}"))
    } else {
        Ok(())
    }
}

pub(super) fn list_run_undo_entries_checked(
    conn: &Connection,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    run_id: &str,
) -> Result<Vec<checkpoint::UndoEntry>, String> {
    ensure_undo_session_idle(conn, running, team_running, session_id)?;
    list_run_undo_entries_inner(conn, session_id, run_id)
}

/// List one run's preimages alongside the current files for a user-reviewed undo diff.
#[tauri::command]
pub(super) fn list_run_undo_entries(
    db: State<Db>,
    running: State<Running>,
    team_running: State<member_runner::TeamRunning>,
    session_id: String,
    run_id: String,
) -> Result<Vec<checkpoint::UndoEntry>, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    list_run_undo_entries_checked(&conn, &running, &team_running, &session_id, &run_id)
}

/// Recheck freshness in the backend immediately before writing preimage bytes, even if the
/// frontend stale badge was bypassed through direct IPC or a new commit appeared between viewing
/// the list and submitting it. The normal UI cannot reach this branch because
/// `list_run_undo_entries` marks stale entries and the frontend disables them. This is a separate
/// defense from the digest-based optimistic lock in `undo_run`: the digest detects drift after the
/// list was viewed, while this check detects entries already stale beforehand. Most in-place runs
/// do not commit themselves, so another commit may have touched a file after post_head or pre_head.
/// Put stale paths directly in `skipped` rather than passing them to `checkpoint::undo_run`.
pub(super) fn undo_run_edits_inner(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    paths: Vec<String>,
    expected_digests: Vec<String>,
) -> Result<checkpoint::UndoReport, String> {
    let paths = paths
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();

    let stale_paths: std::collections::HashSet<std::path::PathBuf> = if let Some(project) =
        inplace_project_path(conn, session_id)?
    {
        let lifecycle = db::run_lifecycle_for_run(conn, session_id, run_id)
            .map_err(|error| error.to_string())?;
        let (state, pre_head, post_head, commit_sha) = match lifecycle {
            Some(lifecycle) => (
                Some(lifecycle.state),
                Some(lifecycle.pre_head),
                lifecycle.post_head,
                lifecycle.commit_sha,
            ),
            None => (None, None, None, None),
        };
        let tuples: Vec<_> = paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    state.clone(),
                    pre_head.clone(),
                    post_head.clone(),
                    commit_sha.clone(),
                )
            })
            .collect();
        let fresh: std::collections::HashSet<_> = filter_fresh_checkpoint_paths(&project, &tuples)?
            .into_iter()
            .collect();
        paths
            .iter()
            .filter(|path| !fresh.contains(*path))
            .cloned()
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut report = checkpoint::UndoReport::default();
    let mut fresh_paths = Vec::new();
    let mut fresh_digests = Vec::new();
    for (path, digest) in paths.into_iter().zip(expected_digests) {
        if stale_paths.contains(&path) {
            report.skipped.push(checkpoint::UndoSkip {
                file_path: path,
                reason: "checkpoint entry is stale: the file was committed again after this \
                         checkpoint; undoing would overwrite that later commit"
                    .into(),
            });
        } else {
            fresh_paths.push(path);
            fresh_digests.push(digest);
        }
    }

    let inner = checkpoint::CheckpointStore::new(conn)?.undo_run(
        session_id,
        run_id,
        &fresh_paths,
        &fresh_digests,
    )?;
    report.restored.extend(inner.restored);
    report.failed.extend(inner.failed);
    report.skipped.extend(inner.skipped);
    Ok(report)
}

pub(super) fn undo_run_edits_checked(
    conn: &Connection,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    run_id: &str,
    paths: Vec<String>,
    expected_digests: Vec<String>,
) -> Result<checkpoint::UndoReport, String> {
    ensure_undo_session_idle(conn, running, team_running, session_id)?;
    let _guard = reserve_mutation(running, session_id, "undo_run_edits")?;
    undo_run_edits_inner(conn, session_id, run_id, paths, expected_digests)
}

/// Restore only the checkpoint entries selected by the user.
#[tauri::command]
pub(super) fn undo_run_edits(
    db: State<Db>,
    running: State<Running>,
    team_running: State<member_runner::TeamRunning>,
    session_id: String,
    run_id: String,
    paths: Vec<String>,
    expected_digests: Vec<String>,
) -> Result<checkpoint::UndoReport, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    undo_run_edits_checked(
        &conn,
        &running,
        &team_running,
        &session_id,
        &run_id,
        paths,
        expected_digests,
    )
}

#[tauri::command]
pub(super) fn waive_acceptance(
    db: State<Db>,
    session_id: String,
    run_id: String,
    criterion_id: String,
    reason: String,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_acceptance_waiver(&conn, &session_id, &run_id, &criterion_id, &reason)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn list_acceptance(
    db: State<Db>,
    session_id: String,
    run_id: String,
) -> Result<Vec<db::AcceptanceCriterion>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_acceptance_by_run(&conn, &session_id, &run_id).map_err(|e| e.to_string())
}

/// Freeze the edited goal, assignments, and criteria transactionally so execution uses a consistent contract revision.
/// Thin wrapper: acquire the lock and call `db::freeze_team_contract`; the database layer owns the
/// transaction, enforces the state machine, and writes to the app database.
#[tauri::command]
pub(super) fn freeze_team_plan(
    db: State<Db>,
    session_id: String,
    run_id: String,
    goal: String,
    assignments_json: String,
    criteria: Vec<db::AcceptanceCriterion>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::freeze_team_contract(
        &conn,
        &session_id,
        &run_id,
        &goal,
        &assignments_json,
        &criteria,
    )
    .map_err(|e| e.to_string())
}

/// Insert a missing manually entered contract as a draft before freezing so freezing remains an update-only transition.
/// This preserves the one-way, update-only semantics of `freeze_team_contract`. The wrapper calls
/// `db::insert_goal_contract_if_absent`.
#[tauri::command]
pub(super) fn insert_goal_contract_row(
    db: State<Db>,
    contract_id: String,
    session_id: String,
    run_id: String,
    goal: String,
    lead_id: String,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::insert_goal_contract_if_absent(
        &conn,
        &db::GoalContract {
            id: contract_id,
            session_id,
            run_id,
            goal,
            lead_participant_id: lead_id,
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: db::now_secs(),
        },
    )
    .map_err(|e| e.to_string())
}
