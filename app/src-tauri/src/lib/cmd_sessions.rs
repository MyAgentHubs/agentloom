//! Session lifecycle, workspace reconciliation, trash collection, and group commands.

use super::{
    create_session_business, db, ensure_session_workspace, groups_repo,
    reconcile_orphan_workspaces_in, repo_id_is_in_place, reserve_mutation,
    reserve_thread_mutations, resolve_session_workspace, session_is_in_place, ui_msg, Connection,
    Db, OptionalExtension, Running, SessionWorkspace, State,
};

#[tauri::command]
pub(super) fn create_session(
    db: State<Db>,
    id: String,
    title: String,
    repo_id: Option<String>,
    namespace_id: Option<String>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    create_session_business(
        &conn,
        &id,
        &title,
        repo_id.as_deref(),
        namespace_id.as_deref(),
    )
}

#[tauri::command]
pub(super) fn rename_session(db: State<Db>, id: String, title: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::rename_session(&conn, &id, &title).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn set_session_pinned(db: State<Db>, id: String, pinned: bool) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_session_pinned(&conn, &id, pinned).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn set_session_unread(db: State<Db>, id: String, unread: bool) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_session_unread(&conn, &id, unread).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn set_session_archived(
    db: State<Db>,
    running: State<Running>,
    id: String,
    archived: bool,
) -> Result<(), String> {
    set_session_archived_inner(&db, running.inner(), &id, archived)
}

pub(super) fn set_session_archived_inner(
    db: &Db,
    running: &Running,
    id: &str,
    archived: bool,
) -> Result<(), String> {
    let chain_ids = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::continuation_chain_ids(&conn, id).map_err(|e| e.to_string())?
    };
    // Busy gate: do not archive while any session in the chain is running and its agent or
    // members may still write to the worktree. Release finalization snapshots only the current
    // state; a subsequent worktree remove --force would discard later process writes.
    //
    // The narrower DB lock protects only entry points sharing `reserve_mutation` or
    // `reserve_thread_mutations`, such as `delete_session_inner`; this is not a repository-wide
    // gate. Testing demonstrated three counterexamples. Two writers remain outside it:
    // - `gc_expired_trash_inner` runs git subprocesses in a loop under the DB lock and is also
    //   called directly at startup. Archive and GC previously serialized through that global
    //   lock; with narrower locking, both can touch worktrees/refs in the same repo concurrently.
    // - `update_session_repo` does not reserve mutations. Concurrent rebinding can release the
    //   old repo's workspace while setting archived on the session already bound to another repo.
    // The third case, team runs, has been addressed: `start_team_run` now reserves a `Running`
    // slot via `reserve_team_run_slot`, released by `release_team_run_slot` once every member
    // is terminal. Previously it checked only `TeamRunning`. Team runs now participate in this
    // mutation gate, so a conflicting session in the chain produces SESSION_BUSY.
    // The remaining two cases are outside this lock-scope change. Their worst outcomes are git
    // lock contention causing an error or a skipped operation; they fail closed and are
    // recoverable without losing persisted data. They require reservation gates of their own.
    // The global DB lock protects the short consistent-read and write phases. Keeping it across
    // 128 git subprocesses for a long chain freezes the whole app; narrowing it outweighs these
    // recoverable interleavings.
    let _guards = reserve_thread_mutations(running, &chain_ids, "archive")?;
    // `release_session_workspace` runs git subprocesses (finalize and `worktree remove --force`).
    // A chain can contain 128 sessions; releasing all of them under the global DB lock could
    // freeze the app for 128 subprocesses. Release needs only session ids and repo paths, not
    // the connection. Split the operation into three phases:
    // - Under the lock, quickly read workspace ownership and in-place status for the whole chain.
    // - Outside the lock, perform the slow releases sequentially, only when archived=true.
    // - Under the lock, set archived only after every release succeeds. Program order and early
    //   return through `?` preserve fail-closed ordering without holding one lock throughout.
    let workspaces = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        chain_ids
            .iter()
            .map(|sid| -> Result<(String, SessionWorkspace, bool), String> {
                Ok((
                    sid.clone(),
                    resolve_session_workspace(&conn, sid)?,
                    session_is_in_place(&conn, sid)?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if archived {
        // Fail closed: release workspace first (includes finalize-before-cleanup), only set the
        // archived flag if release succeeds. resolve Err -> fail-closed Err (don't set flag by
        // Reject an unresolvable Repo session instead of silently treating it as Local, preserving its workspace boundary.
        for (sid, workspace, in_place) in &workspaces {
            if *in_place {
                continue;
            }
            match workspace {
                SessionWorkspace::Repo(repo) => {
                    crate::worktree::release_session_workspace(sid, repo)?;
                }
                SessionWorkspace::Local => {}
            }
        }
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::set_sessions_archived(&conn, &chain_ids, true).map_err(|e| e.to_string())?;
    } else {
        // Unarchive: clear the flag first, then best-effort reattach; ensure retries on next access.
        // `ensure_session_workspace` can also launch git subprocesses, but errors are intentionally
        // ignored with `let _ =`, and `ensure_session_live` must recheck the tombstone. Splitting
        // this branch costs more and benefits less than the archived=true path, so it remains
        // under the lock as before, with this note documenting the decision.
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::set_sessions_archived(&conn, &chain_ids, false).map_err(|e| e.to_string())?;
        for (sid, workspace, in_place) in &workspaces {
            if !*in_place && matches!(workspace, SessionWorkspace::Repo(_)) {
                let _ = ensure_session_workspace(&conn, sid);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub(super) fn list_sessions(db: State<Db>) -> Result<Vec<db::Session>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    list_sessions_inner(&conn)
}

pub(super) fn list_sessions_inner(conn: &rusqlite::Connection) -> Result<Vec<db::Session>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, repo_id, namespace_id, group_id, created_at, \
             pinned, unread, archived, archived_at, \
             parent_session_id, continued_to_session_id, \
             total_input_tokens, total_output_tokens \
             FROM sessions WHERE deleted_at IS NULL ORDER BY pinned DESC, created_at DESC, id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let repo_id: Option<String> = r.get(2)?;
            Ok(db::Session {
                id: r.get(0)?,
                title: r.get(1)?,
                in_place: repo_id_is_in_place(repo_id.as_deref()),
                repo_id,
                namespace_id: r.get(3)?,
                group_id: r.get(4)?,
                created_at: r.get(5)?,
                pinned: r.get(6)?,
                unread: r.get(7)?,
                archived: r.get(8)?,
                archived_at: r.get(9)?,
                parent_session_id: r.get(10)?,
                continued_to_session_id: r.get(11)?,
                total_input_tokens: r.get(12)?,
                total_output_tokens: r.get(13)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn delete_session(
    db: State<Db>,
    running: State<Running>,
    id: String,
) -> Result<(), String> {
    delete_session_inner(&db, running.inner(), &id)
}

/// Testable soft-delete core without Tauri State. The busy gate rejects running sessions:
/// otherwise trash finalization snapshots the current state, and worktree remove --force
/// would discard subsequent writes by member subprocesses.
///
/// Following the `lead_step` precedent, `trash_session_workspace` launches git/filesystem
/// subprocesses for finalization and worktree removal. Holding `db.0.lock()` throughout
/// occupied the sole global DB connection and blocked checkpoint hooks and other commands.
/// Read the branch decision under the lock, drop the guard, trash outside the lock, then
/// reacquire it to persist the `deleted_at` tombstone.
///
/// The TOCTOU guarantee covers only entry points sharing the reservation gate, not all
/// session mutations. `_g`, obtained from `reserve_mutation`, stays held for the entire
/// function. It reserves a session_id in `Running`, a separate in-memory mutex from `db.0`.
/// `delete_session`, `restore_session`, and `purge_session` share this gate and reject the
/// same id with SESSION_BUSY. `start_team_run` now reserves the same `Running` slot, so team
/// dispatch also conflicts. Repo rebinding and artifact commands such as
/// `run_verifier_artifact` and `merge_artifact_to_staging` still bypass the gate and can
/// modify the same session/repo while the DB lock is released. Such ungated interleavings
/// were already possible before or after deletion under the old whole-function lock. The
/// serialized region is now each of the three locked phases instead of the entire function;
/// that pre-existing class of risk is neither eliminated nor newly broadened.
/// The newly visible interval is after trash moves the worktree into a trash ref but before
/// `deleted_at` is persisted: read-only queries such as `list_sessions` can briefly see a
/// missing workspace on a session that still appears live. This visibility lag is acceptable
/// and is not data corruption; the third phase converges immediately after writing the
/// tombstone. The old lock blocked all DB readers and writers, including unrelated session
/// listing, throughout trash handling, which was the problem the narrower scope addressed.
///
/// Route DB lock reacquisition failure through compensation. A bare `?` on a poisoned lock
/// would skip compensation after the workspace had already moved into a trash ref, leaving
/// a permanent orphan without a tombstone. Retrying deletion hits `wt.cleanup.trashRefExists`,
/// purge rejects `deleted_at IS NULL`, and GC cannot find that unmarked intermediate state.
/// `finalize_session_trash` therefore handles lock failure like `set_session_deleted` failure:
/// try `restore_trashed_session_branch` to move the ref back to heads, escalating to the
/// combined `run.tombstoneRestoreFailed` error only if compensation also fails.
pub(super) fn delete_session_inner(db: &Db, running: &Running, id: &str) -> Result<(), String> {
    let _g = reserve_mutation(running, id, "delete")?;
    // Phase one, under the lock: tombstone in-place sessions and return; otherwise read the
    // workspace decision, then release the lock.
    let workspace = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        if session_is_in_place(&conn, id)? {
            return db::set_session_deleted(&conn, id).map_err(|e| e.to_string());
        }
        // Fail closed on resolution errors so guessing Local cannot silently skip trash handling.
        resolve_session_workspace(&conn, id)
    };
    // Soft-delete: git-trash (includes finalize-before-cleanup, fails loudly) then DB tombstone.
    // if trash succeeds but tombstone fails, compensate by restoring the trash ref back to heads
    // (prevents session appearing alive but branch stuck in trash; orphan on next ensure).
    // Local sessions share the project worktree -- never trash them.
    match workspace {
        Ok(SessionWorkspace::Repo(repo)) => {
            // Phase two, outside the lock: slow git worktree trash, including finalization and subprocesses.
            crate::worktree::trash_session_workspace(id, &repo)?;
            // Phase three: reacquire the lock and persist the tombstone; compensate even if locking fails.
            finalize_session_trash(db, id, &repo)
        }
        // Local: shares the project worktree -- never trash, just tombstone.
        Ok(SessionWorkspace::Local) => {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            db::set_session_deleted(&conn, id).map_err(|e| e.to_string())
        }
        // Fail closed on resolution errors so guessing Local cannot silently skip trash handling.
        Err(e) => Err(e),
    }
}

/// Phase three of `delete_session_inner`, extracted for direct testing. A test can establish
/// that the repo has really been trashed and the DB lock poisoned, then verify compensation
/// without starting threads or winning a race; see
/// `delete_session_repo_relock_poisoned_still_compensates_trash`.
///
/// Failure of `db.0.lock()` through poisoning takes the same compensation path as failure of
/// `set_session_deleted`: trash already happened but the tombstone could not be persisted.
/// Both must move the trash ref back to heads to avoid a permanent orphan.
pub(super) fn finalize_session_trash(
    db: &Db,
    id: &str,
    repo: &std::path::Path,
) -> Result<(), String> {
    match db.0.lock() {
        Ok(conn) => {
            if let Err(e) = db::set_session_deleted(&conn, id) {
                // Compensation: restore trash ref back to heads. If THAT also fails, surface a
                // Preserve both errors so the caller can detect the orphan and reconcile it.
                return match crate::worktree::restore_trashed_session_branch(id, repo) {
                    Ok(()) => Err(e.to_string()),
                    Err(re) => Err(ui_msg::al_err(
                        "run.tombstoneRestoreFailed",
                        &[("tombstone", e.to_string()), ("restore", re.to_string())],
                    )),
                };
            }
            Ok(())
        }
        Err(lock_err) => {
            // Lock poisoning also requires compensation, as described in the function documentation.
            let lock_err = lock_err.to_string();
            match crate::worktree::restore_trashed_session_branch(id, repo) {
                Ok(()) => Err(lock_err),
                Err(re) => Err(ui_msg::al_err(
                    "run.tombstoneRestoreFailed",
                    &[("tombstone", lock_err), ("restore", re.to_string())],
                )),
            }
        }
    }
}

#[tauri::command]
pub(super) fn restore_session(
    db: State<Db>,
    running: State<Running>,
    id: String,
) -> Result<(), String> {
    restore_session_inner(&db, running.inner(), &id)
}

pub(super) fn restore_session_inner(db: &Db, running: &Running, id: &str) -> Result<(), String> {
    // Consistent busy gate for defense in depth: tombstoned sessions should not run because ensure blocks them.
    let _g = reserve_mutation(running, id, "restore")?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::preflight_restore_session(&conn, id).map_err(|e| e.to_string())?;
    if session_is_in_place(&conn, id)? {
        return db::restore_session(&conn, id).map_err(|e| e.to_string());
    }
    // Preflight DB lineage before touching git. If final DB restore still fails after git restore,
    // compensate by moving heads back to trash so DB tombstone and git refs stay consistent.
    let restored_repo = match resolve_session_workspace(&conn, id) {
        Ok(SessionWorkspace::Repo(repo)) => {
            crate::worktree::restore_trashed_session_branch(id, &repo)?;
            Some(repo)
        }
        Ok(SessionWorkspace::Local) => None, // Local: no git ref to restore
        Err(e) => return Err(e), // Restoration is unsafe when the workspace cannot be resolved.
    };
    if let Err(e) = db::restore_session(&conn, id) {
        let db_err = e.to_string();
        if let Some(repo) = restored_repo {
            if let Err(restore_err) =
                crate::worktree::move_restored_session_branch_back_to_trash(id, &repo)
            {
                return Err(format!(
                    "DB_RESTORE_FAILED_GIT_COMPENSATION_FAILED:db={db_err};git={restore_err}"
                ));
            }
        }
        return Err(db_err);
    }
    Ok(())
}

/// Permanently delete a trashed session (irreversible): gc trash ref + base (checked) then cascade purge DB.
#[tauri::command]
pub(super) fn purge_session(
    db: State<Db>,
    running: State<Running>,
    id: String,
) -> Result<(), String> {
    // Consistent busy gate for defense in depth: irreversible purge always rejects running sessions.
    let _g = reserve_mutation(running.inner(), &id, "purge")?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    purge_session_inner(&conn, &id)
}

/// Hard deletion cascades to the session journal directory at ~/.agentloom/journals/<session_id>.
/// These app-domain files account for much of artifact storage, about 3.3 MB per run.
/// Cleanup is best-effort: a missing directory is harmless, and removal failures only log via
/// eprintln without blocking purge. Leaked journals waste storage but do not affect correctness;
/// a later purge or GC will retry.
fn cleanup_session_journals(session_id: &str) {
    let dir = crate::worktree::journals_dir().join(session_id);
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            eprintln!("[purge] 清 journal 目录失败 {dir:?}: {e}");
        }
    }
}

/// Reject purge unless the session is tombstoned, protecting live sessions from irreversible deletion.
/// `db::delete_session` is an unconditional, irreversible cascading deletion. GC guards do not
/// protect Local or pristine Repo sessions and cannot replace the deleted_at precondition.
/// Resolution errors fail closed: never delete the DB recovery index before refs are collected.
/// `gc_expired_trash` needs no such gate because it only visits expired sessions with non-NULL deleted_at.
pub(super) fn purge_session_inner(conn: &Connection, id: &str) -> Result<(), String> {
    let deleted_at: Option<i64> = conn
        .query_row("SELECT deleted_at FROM sessions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    if deleted_at.is_none() {
        return Err(format!("SESSION_NOT_TRASHED:{id}"));
    }
    if !session_is_in_place(conn, id)? {
        match resolve_session_workspace(conn, id) {
            // gc fails (live worktree / heads still live) -> Err without purging DB.
            Ok(SessionWorkspace::Repo(repo)) => {
                crate::worktree::gc_trashed_session_branch(id, &repo)?
            }
            Ok(SessionWorkspace::Local) => {} // Local: no git refs to gc
            Err(e) => return Err(e),
        }
    }
    db::delete_session(conn, id).map_err(|e| e.to_string())?;
    cleanup_session_journals(id); // Hard deletion cascades to journal cleanup (best-effort).
    Ok(())
}

/// GC grace-expired soft-deleted sessions (call at startup or on a timer; grace = 30 days).
/// Fail closed: sessions whose git gc fails are skipped (DB not purged, recoverable index kept).
#[tauri::command]
pub(super) fn gc_expired_trash(db: State<Db>) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    gc_expired_trash_inner(&conn)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReconcileWorkspaceResult {
    Processed,
    NothingToClean,
    InPlaceNoop,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ReconcileStats {
    pub(super) processed: usize,
    pub(super) in_place_noop: usize,
    pub(super) skipped: usize,
}

pub(super) fn reconcile_soft_deleted_workspace(
    conn: &Connection,
    session_id: &str,
    worktree_path: Option<&std::path::Path>,
) -> Result<ReconcileWorkspaceResult, String> {
    if db::session_has_live_children(conn, session_id).map_err(|e| e.to_string())? {
        return Err(format!("软删会话 {session_id} 仍有活子会话"));
    }
    let repo_id: Option<String> = conn
        .query_row(
            "SELECT repo_id FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if repo_id.is_none() {
        return Ok(ReconcileWorkspaceResult::NothingToClean);
    }
    let repo = match resolve_session_workspace(conn, session_id)? {
        SessionWorkspace::Repo(repo) => repo,
        SessionWorkspace::Local => {
            return Err(format!(
                "软删会话 {session_id} 的 repo_id 非 NULL 但工作区解析为 Local"
            ));
        }
    };
    if crate::worktree::assert_app_domain_path(&repo, "reconcile_soft_deleted_workspace_refs")
        .is_err()
    {
        return Ok(ReconcileWorkspaceResult::InPlaceNoop);
    }
    let expected = crate::worktree::session_wt_path(&repo, session_id);
    match worktree_path {
        Some(worktree_path) => {
            let matches_expected = std::fs::canonicalize(worktree_path)
                .and_then(|actual| {
                    std::fs::canonicalize(&expected).map(|expected| actual == expected)
                })
                .unwrap_or(false);
            if !matches_expected {
                return Err(format!(
                    "会话 {session_id} 工地布局不匹配：{}",
                    worktree_path.display()
                ));
            }
            if !crate::worktree::worktree_belongs_to_repo(worktree_path, &repo)? {
                return Err(format!(
                    "会话 {session_id} 工地 common-dir 不属于解析出的 Repo：{}",
                    worktree_path.display()
                ));
            }
            crate::worktree::trash_session_workspace(session_id, &repo)?;
            Ok(ReconcileWorkspaceResult::Processed)
        }
        None => {
            if expected.exists() {
                return Err(format!(
                    "会话 {session_id} 工地存在但未从目录扫描确认：{}",
                    expected.display()
                ));
            }
            crate::worktree::trash_deleted_session_head_without_workspace(session_id, &repo).map(
                |processed| {
                    if processed {
                        ReconcileWorkspaceResult::Processed
                    } else {
                        ReconcileWorkspaceResult::NothingToClean
                    }
                },
            )
        }
    }
}

/// If a DB-orphaned workspace contains only a standard `.git` pointer to missing git metadata,
/// move the whole directory into `_trash` under the app workspace root for manual recovery or
/// later collection. Only check the target gitdir's existence, without writing to it. Return
/// false for any unconfirmed layout so the caller preserves the original gitStatusFailed and state.
pub(super) fn trash_dangling_gitdir_orphan(
    root: &std::path::Path,
    session_id: &str,
    worktree: &std::path::Path,
) -> Result<bool, String> {
    crate::worktree::assert_app_domain_path(root, "reconcile_dangling_gitdir_root")?;
    crate::worktree::assert_app_domain_path(worktree, "reconcile_dangling_gitdir_workspace")?;

    let canonical_root = std::fs::canonicalize(root).map_err(|e| {
        format!(
            "reconcile_orphan_workspaces: 无法规范化工地根 {}: {e}",
            root.display()
        )
    })?;
    let canonical_worktree = std::fs::canonicalize(worktree).map_err(|e| {
        format!(
            "reconcile_orphan_workspaces: 无法规范化悬空工地 {}: {e}",
            worktree.display()
        )
    })?;
    let relative = match canonical_worktree.strip_prefix(&canonical_root) {
        Ok(relative) => relative,
        Err(_) => return Ok(false),
    };
    let components = relative.components().collect::<Vec<_>>();
    if components.len() != 2
        || canonical_worktree.file_name() != Some(std::ffi::OsStr::new(session_id))
    {
        return Ok(false);
    }

    let git_file = canonical_worktree.join(".git");
    let metadata = match std::fs::symlink_metadata(&git_file) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) | Err(_) => return Ok(false),
    };
    if metadata.len() > 64 * 1024 {
        return Ok(false);
    }
    let contents = match std::fs::read_to_string(&git_file) {
        Ok(contents) => contents,
        Err(_) => return Ok(false),
    };
    let line = contents.strip_suffix('\n').unwrap_or(&contents);
    let Some(gitdir_value) = line.strip_prefix("gitdir: ").filter(|value| {
        !value.is_empty() && *value == value.trim() && !value.contains(['\n', '\r'])
    }) else {
        return Ok(false);
    };
    let gitdir = std::path::Path::new(gitdir_value);
    let gitdir = if gitdir.is_absolute() {
        gitdir.to_path_buf()
    } else {
        canonical_worktree.join(gitdir)
    };

    let gitdir_is_missing = || match std::fs::symlink_metadata(&gitdir) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!(
            "reconcile_orphan_workspaces: 无法判断 gitdir {} 是否存在: {error}",
            gitdir.display()
        )),
    };
    if !gitdir_is_missing()? {
        return Ok(false);
    }

    let trash_root = canonical_root.join("_trash");
    std::fs::create_dir_all(&trash_root).map_err(|e| {
        format!(
            "reconcile_orphan_workspaces: 无法创建悬空工地 trash {}: {e}",
            trash_root.display()
        )
    })?;
    crate::worktree::assert_app_domain_path(&trash_root, "reconcile_dangling_gitdir_trash")?;
    let canonical_trash_root = std::fs::canonicalize(&trash_root).map_err(|e| {
        format!(
            "reconcile_orphan_workspaces: 无法规范化悬空工地 trash {}: {e}",
            trash_root.display()
        )
    })?;
    if canonical_trash_root.parent() != Some(canonical_root.as_path())
        || canonical_trash_root.file_name() != Some(std::ffi::OsStr::new("_trash"))
    {
        return Err(format!(
            "reconcile_orphan_workspaces: 悬空工地 trash 不是工地根的直接子目录：{}",
            canonical_trash_root.display()
        ));
    }
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("reconcile_orphan_workspaces: 系统时间早于 epoch: {e}"))?
        .as_nanos();
    // After creating trash, recheck read-only that the external gitdir has not returned; never create or write to it.
    if !gitdir_is_missing()? {
        return Ok(false);
    }
    crate::worktree::move_to_unique_trash(
        &canonical_worktree,
        &canonical_trash_root,
        session_id,
        epoch,
    )?;
    Ok(true)
}

/// Startup reconciliation scans second-level session directories under the app workspace root
/// and supplements them with soft-deleted DB sessions whose directories are gone but heads
/// remain. Fail closed per entry, skipping individual errors; return the number of workspaces
/// or branches successfully moved to trash. Startup precedes Running initialization, so the
/// available run-safety gate is session_has_live_children; live sessions are always untouched.
pub(super) fn reconcile_orphan_workspaces(conn: &Connection) -> Result<usize, String> {
    let stats = reconcile_orphan_workspaces_in(conn, &crate::worktree::default_root())?;
    eprintln!(
        "reconcile_orphan_workspaces: 处理 {} 个孤儿工地，{} 个 in-place 会话无需清理，跳过 {} 个",
        stats.processed, stats.in_place_noop, stats.skipped
    );
    Ok(stats.processed)
}

/// Testable GC core, also called directly at startup without Tauri State. The grace period is
/// 30 days. Collect git refs of expired soft-deleted sessions, then cascade purge the DB.
/// Skip GC failures, such as live worktrees, and resolution failures to retain the recovery index.
pub(super) fn gc_expired_trash_inner(conn: &Connection) -> Result<usize, String> {
    const GRACE_SECS: i64 = 30 * 24 * 60 * 60;
    // GUI verification caught that strftime('%s','now') returns TEXT, so r.get::<i64> fails with
    // "Invalid column type Text". CAST AS INTEGER makes it readable as i64. Other strftime uses
    // insert/update INTEGER columns, whose affinity already performs the conversion.
    let now: i64 = conn
        .query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let expired =
        db::list_expired_trashed_sessions(conn, now - GRACE_SECS).map_err(|e| e.to_string())?;
    let mut purged = 0usize;
    for id in &expired {
        match db::session_has_live_children(conn, id) {
            Ok(false) => {}
            Ok(true) | Err(_) => continue, // live child or DB read failure -> skip before any git GC
        }
        match session_is_in_place(conn, id) {
            Ok(true) => {}
            Ok(false) => match resolve_session_workspace(conn, id) {
                Ok(SessionWorkspace::Repo(repo)) => {
                    if crate::worktree::gc_trashed_session_branch(id, &repo).is_err() {
                        continue; // gc failed (e.g. live worktree still exists) -> skip, keep DB recoverable
                    }
                }
                Ok(SessionWorkspace::Local) => {} // Local: no git refs to gc
                Err(_) => continue,
            },
            Err(_) => continue,
        }
        if db::delete_session(conn, id).is_ok() {
            cleanup_session_journals(id); // Hard deletion cascades to journal cleanup (best-effort).
            purged += 1;
        }
    }
    Ok(purged)
}

#[tauri::command]
pub(super) fn list_groups(
    db: State<Db>,
    repo_id: String,
) -> Result<Vec<groups_repo::GroupMeta>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    groups_repo::list_by_repo(&conn, &repo_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn create_group(
    db: State<Db>,
    id: String,
    repo_id: String,
    name: String,
    position: Option<i64>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let pos = match position {
        Some(p) => p,
        None => groups_repo::next_position(&conn, &repo_id).map_err(|e| e.to_string())?,
    };
    groups_repo::create_group(&conn, &id, &repo_id, &name, pos).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn rename_group(db: State<Db>, id: String, name: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    groups_repo::rename_group(&conn, &id, &name).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn delete_group(db: State<Db>, id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    groups_repo::delete_group(&conn, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn move_session_to_group(
    db: State<Db>,
    session_id: String,
    group_id: Option<String>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    groups_repo::move_session_to_group(&conn, &session_id, group_id.as_deref())
}
