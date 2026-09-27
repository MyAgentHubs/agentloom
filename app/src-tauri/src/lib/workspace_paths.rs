use crate::{db, namespaces_repo, repos_repo, ui_msg, worktree};
use rusqlite::OptionalExtension;
use std::io::Write;
use std::process::Command;

/// Writes the agent's stderr to `~/.agentloom/logs/<session>.log`, preserving diagnostics when Claude fails.
/// Returns `None` when the file cannot be obtained, allowing spawn to fall back to `Stdio::null`.
pub(super) fn log_file_for(session_id: &str) -> Option<std::fs::File> {
    let dir = worktree::logs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let safe = worktree::safe_id(session_id);
    let name = if safe.is_empty() {
        "session".to_string()
    } else {
        safe
    };
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{name}.log")))
        .ok()
}

pub(crate) fn log_claude_bin(session_id: &str, claude_bin: &str) {
    if let Some(mut log) = log_file_for(session_id) {
        let _ = writeln!(log, "claude-bin: {claude_bin}");
        let _ = log.flush();
    }
}

pub(crate) fn member_log_file(session_id: &str, assignment_id: &str) -> Option<std::fs::File> {
    let dir = worktree::logs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let s = worktree::safe_id(session_id);
    let a = worktree::safe_id(assignment_id);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!(
            "{}-{}.log",
            if s.is_empty() { "session" } else { &s },
            a
        )))
        .ok()
}

// Keep search backend settings in the database and API keys in the keychain so secrets stay out of ordinary settings storage.

// Write-operation business functions and five IPC endpoints.

/// On startup, seeds the Local namespace, local-default repository, directory, and Git repository.
/// This is idempotent and is called defensively on every startup.
/// The caller provides `local_path`; the setup hook uses `~/.agentloom/local/default/`, while tests use `tmp_root`.
/// At startup, scans every active repository and marks repositories whose paths do not exist as invalid.
/// Returns the number marked invalid; it does not block startup or panic on errors.
pub(crate) fn scan_invalid_paths(conn: &rusqlite::Connection) -> Result<usize, String> {
    let actives = repos_repo::list_active(conn).map_err(|e| e.to_string())?;
    let mut n = 0;
    for r in actives {
        if !std::path::Path::new(&r.path).exists() {
            repos_repo::set_repo_invalid(conn, &r.id).map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    Ok(n)
}

/// Recover pending ledger rows still marked running at startup because a crash may have prevented finalization.
/// Marks that row failed and sets the corresponding session's git_state to 'commit_failed' so the user can retry or discard.
/// This is database-only, does not depend on Git or the Tauri runtime, and is idempotent. Returns the number of running rows marked failed.
/// Multiple running rows for the same session are each counted, so this is a row count rather than a deduplicated session count.
pub(crate) fn recover_interrupted_runs(conn: &rusqlite::Connection) -> Result<usize, String> {
    for intent in db::list_run_commit_intents(conn).map_err(|e| e.to_string())? {
        let project = match inplace_project_path(conn, &intent.session_id) {
            Ok(project) => project,
            Err(_) => {
                let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                db::mark_run_failed(conn, &intent.session_id, &intent.run_id)
                    .map_err(|e| e.to_string())?;
                db::set_git_state(conn, &intent.session_id, "commit_failed")
                    .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                continue;
            }
        };
        let Some(project) = project else {
            db::mark_run_failed(conn, &intent.session_id, &intent.run_id)
                .map_err(|e| e.to_string())?;
            db::set_git_state(conn, &intent.session_id, "commit_failed")
                .map_err(|e| e.to_string())?;
            continue;
        };
        let current_head = worktree::rev_parse_head(&project).unwrap_or_default();
        if current_head == intent.expected_head {
            db::delete_run_commit_intent(conn, &intent.session_id, &intent.run_id)
                .map_err(|e| e.to_string())?;
            continue;
        }
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        db::mark_run_failed(conn, &intent.session_id, &intent.run_id).map_err(|e| e.to_string())?;
        db::set_git_state(conn, &intent.session_id, "commit_failed").map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }

    let stuck: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT session_id, run_id FROM run_commits WHERE state = 'running'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(|e| e.to_string())?
    };
    // Atomically applies mark_run_failed and set_git_state to the entire batch: either all rows are recovered or none are changed. The logic is equivalent.
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for (sid, rid) in &stuck {
        db::mark_run_failed(conn, sid, rid).map_err(|e| e.to_string())?;
        db::set_git_state(conn, sid, "commit_failed").map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(stuck.len())
}

/// Use a stable git_state rejection prefix so the frontend can recognize blocked states and offer retry or discard.
pub(super) const GIT_STATE_BLOCKED: &str = "GIT_STATE_BLOCKED";

/// Reject commit_failed and diverged states with GIT_STATE_BLOCKED:<state> so unresolved Git state blocks gated operations.
/// Allow clean and running states; discard and retry bypass this gate so blocked state can still be resolved.
pub(super) fn gate_git_state(conn: &rusqlite::Connection, session_id: &str) -> Result<(), String> {
    let state = db::get_git_state(conn, session_id).map_err(|e| e.to_string())?;
    if state == "commit_failed" || state == "diverged" {
        return Err(format!("{GIT_STATE_BLOCKED}:{state}"));
    }
    Ok(())
}

/// Reconcile session Git state with its ledger so recorded state agrees with repository reality.
/// Passes the last active row's post_head to `worktree::reconcile`; a Diverged result sets git_state to diverged.
/// A Clean result does not implicitly change commit_failed back to clean; retry or discard explicitly recovers bad states.
pub(super) fn reconcile_session(
    conn: &rusqlite::Connection,
    session_id: &str,
    wt: &std::path::Path,
) -> Result<(), String> {
    let last_post_head = db::last_active_run_commit(conn, session_id)
        .map_err(|e| e.to_string())?
        .and_then(|row| row.post_head);
    match worktree::reconcile(wt, last_post_head.as_deref()) {
        worktree::ReconcileVerdict::Clean => Ok(()),
        worktree::ReconcileVerdict::Diverged { reason } => {
            eprintln!("reconcile_session {session_id} diverged：{reason}");
            db::set_git_state(conn, session_id, "diverged").map_err(|e| e.to_string())
        }
    }
}

/// An earlier migration deleted every repository in the `local` namespace except `local-default`
/// as legacy data. User projects later created by the GUI have exactly the same data shape,
/// and the schema has no reliable marker that distinguishes them. Therefore, fail closed and delete nothing.
pub(super) fn cleanup_legacy_local_repos_in(
    _conn: &rusqlite::Connection,
    _wt_root: &std::path::Path,
    _sessions_root: &std::path::Path,
) -> Result<usize, String> {
    Ok(0)
}

pub(crate) fn cleanup_legacy_local_repos(conn: &rusqlite::Connection) -> Result<usize, String> {
    cleanup_legacy_local_repos_in(
        conn,
        &worktree::default_root(),
        &worktree::default_sessions_root(),
    )
}

/// Looks up the absolute path of the project associated with a session.
/// `None` means there is no associated project, so the caller routes to the app-domain per-session scaffold.
/// `Some(path)` means there is an active associated project, and cwd points directly to that directory.
/// `Err("PROJECT_INVALID:<id>")` means the project is invalid, prompting the frontend to offer path correction or archiving.
/// `Err("PROJECT_ARCHIVED:<id>")` means the project is archived, prompting the frontend to offer restoration or switching to the default session.
pub(super) fn resolve_repo_path_for_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    let rid = match db::get_session_repo_id(conn, session_id).map_err(|e| e.to_string())? {
        Some(r) => r,
        None => return Ok(None),
    };
    let r = repos_repo::get_repo_by_id(conn, &rid)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("run.repoNotFound", &[("id", rid.to_string())]))?;
    match r.status.as_str() {
        "active" => Ok(Some(std::path::PathBuf::from(r.path))),
        "invalid" => Err(format!("PROJECT_INVALID:{}", r.id)),
        "archived" => Err(format!("PROJECT_ARCHIVED:{}", r.id)),
        other => Err(format!("PROJECT_UNKNOWN_STATUS:{other}")),
    }
}

pub(super) fn repo_id_is_in_place(repo_id: Option<&str>) -> bool {
    repo_id.is_some()
}

/// Whether the session is bound to a project directory. `local-default` is the user-visible "My Project" and also runs in place.
/// Only legacy data with a NULL value that has not completed migration uses the app-domain scaffold.
pub(super) fn session_is_in_place(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<bool, String> {
    let repo_id = db::get_session_repo_id(conn, session_id).map_err(|e| e.to_string())?;
    Ok(repo_id_is_in_place(repo_id.as_deref()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionWorkspace {
    /// Local namespace.
    Local,
    /// github_org namespace: a project directory bound by the user.
    Repo(std::path::PathBuf),
}

impl SessionWorkspace {
    /// In in-place mode, the app no longer applies the old Git-ledger gate to the user's worktree.
    /// User projects may already contain staged, unstaged, or untracked changes.
    pub fn requires_git_gate(&self) -> bool {
        false
    }
}

/// Routes the session workspace according to namespace.kind.
pub(crate) fn resolve_session_workspace(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<SessionWorkspace, String> {
    let namespace_id = db::get_session_namespace_id(conn, session_id)
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| "local".to_string());
    let namespace = namespaces_repo::get_namespace_by_id(conn, &namespace_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("NAMESPACE_NOT_FOUND:{namespace_id}"))?;
    if namespace.kind == "local" {
        return Ok(SessionWorkspace::Local);
    }

    match resolve_repo_path_for_session(conn, session_id)? {
        Some(path) => Ok(SessionWorkspace::Repo(path)),
        None => Ok(SessionWorkspace::Local),
    }
}

pub(super) fn inplace_project_path(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    if !session_is_in_place(conn, session_id)? {
        return Ok(None);
    }

    match resolve_repo_path_for_session(conn, session_id)? {
        Some(path) if path.is_dir() => Ok(Some(path)),
        Some(path) => Err(ui_msg::al_err(
            "run.projectPathUnavailable",
            &[("path", path.display().to_string())],
        )),
        None => Err(ui_msg::al_err("run.projectPathUnavailable", &[])),
    }
}

/// To isolate sessions that share the local-default working directory, narrows only sessions in the built-in
/// "My Project" (`local-default`) repository to a per-session subdirectory `<root>/<session_id>/` on top of
/// `inplace_project_path`; real repositories bound by the user pass through the result of
/// `inplace_project_path` unchanged.
///
/// This fixes the fact that the built-in default project has a single physical directory shared by every session
/// without a selected project, allowing unrelated sessions to see each other's artifacts. On-device inspection found
/// a guitar session entering another session's cloned `hermes-agent/` directory and being misled by its AGENTS.md.
///
/// Usage boundary: use this function only for the agent's actual cwd, spawn working directory, sandbox workspace
/// argument, and attachment-resolution base—cases that ask which directory is cwd and where new content belongs.
/// Do not use it for repository-boundary cases such as review or checkpoint freshness. The diff/checkpoint machinery
/// used by `session_review_inner` passes paths reported by Git, relative to the repository root, unchanged into the
/// next Git command, implicitly assuming that the supplied directory is the Git root. `local-default` is one repository
/// shared by multiple sessions: its per-session directory is only nested within the repository and is not a new root.
/// Passing that directory as cwd makes paths relative to the Git root disagree with the command's cwd, silently losing
/// diffs for new untracked files. A minimal reproduction confirmed that
/// `git diff --no-index -- /dev/null <path-relative-to-repository-root>` cannot find the file when cwd is the session
/// subdirectory and silently concludes that there is no diff. Those consumers must continue to use the original
/// project-root result of `inplace_project_path`.
///
/// Pure resolution version: computes the path without creating directories or touching the filesystem. It is for
/// read-only IPC consumers such as landing info, artifact-diff display prefixes, attachment resolution, and continuation
/// resolution. Those call sites often still hold the database lock, and merely viewing a session must not quietly create
/// an empty subdirectory in the project root. `create_dir_all` would also fail on a read-only filesystem, and a pure path
/// lookup must not be brought down by that side effect. Paths that will actually be written to or used to start a process,
/// including spawn cwd, `ensure_session_workspace`, attachment writes, and verify/merge recalculation, must use
/// `ensure_inplace_session_workdir` below, which ensures the directory exists. Do not restore `create_dir_all` here.
///
/// Preserve the three workspace_scope meanings so resumed sessions retain access to their existing workspace files.
/// A local-default session has three states: `'root'` means the project root, with no per-session subdirectory appended
/// or created. This supports sessions that predate per-session isolation, whose old artifacts are spread across the
/// project root and would be inaccessible from a per-session sandbox; see `db::get_session_workspace_scope`.
/// `NULL` uses the session's own `session_id` as the subdirectory key, which is the default for newly created sessions.
/// Any other nonempty string is itself the subdirectory key, producing `<root>/<safe_id(key)>/`. This state is reserved
/// for continuation chains: a child session stores its parent's key in `workspace_scope`, resolving to exactly the same
/// working directory as the parent instead of opening a new empty directory named for the child. See the three-state
/// inheritance logic in `start_continuation_session_inner_for_locale`. Newly created sessions and sessions bound to real
/// repositories are unaffected; real repositories always use the project root and never read this column.
pub(super) fn inplace_session_workdir(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    let repo_id = db::get_session_repo_id(conn, session_id).map_err(|e| e.to_string())?;
    let Some(project) = inplace_project_path(conn, session_id)? else {
        return Ok(None);
    };
    if repo_id.as_deref() == Some("local-default") {
        let scope = db::get_session_workspace_scope(conn, session_id).map_err(|e| e.to_string())?;
        let key: &str = match scope.as_deref() {
            Some("root") => return Ok(Some(project)),
            // In the second state, the nonempty string other than "root" is itself the subdirectory key.
            // The continuation path sets it to the original ancestor's session_id so both resolve to the same directory.
            Some(other) if !other.is_empty() => other,
            // In the third state, including NULL and a defensively handled empty string, the session's own ID is the key.
            _ => session_id,
        };
        let safe = crate::worktree::safe_id(key);
        if safe.is_empty() {
            return Err(ui_msg::al_err("wt.session.invalidDefaultId", &[]));
        }
        return Ok(Some(project.join(&safe)));
    }

    Ok(Some(project))
}

/// The ensure-exists version of `inplace_session_workdir`: actually calls idempotent `create_dir_all` on the resolved path.
/// Use it only for paths that will actually be written to or used to start a process: spawn cwd for team plans, lead steps,
/// and member dispatch, `ensure_session_workspace`, attachment writes, and verify/merge recalculation.
/// Read-only IPC consumers must use the pure resolution version above; do not switch casually between the two.
pub(super) fn ensure_inplace_session_workdir(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    let Some(dir) = inplace_session_workdir(conn, session_id)? else {
        return Ok(None);
    };
    std::fs::create_dir_all(&dir).map_err(|e| {
        ui_msg::al_err(
            "run.projectPathUnavailable",
            &[
                ("path", dir.display().to_string()),
                ("detail", e.to_string()),
            ],
        )
    })?;
    Ok(Some(dir))
}

/// Looks up an artifact's session repository path for verify/merge recalculation.
/// In-place sessions, including local-default, use the same project directory; only unbound legacy data uses the app-domain scaffold.
pub(super) fn resolve_repo_path_for_artifact(
    conn: &rusqlite::Connection,
    artifact_id: &str,
) -> Result<std::path::PathBuf, String> {
    let art = crate::db::get_artifact(conn, artifact_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("artifact.notFound", &[("id", artifact_id.to_string())]))?;
    if let Some(project) = ensure_inplace_session_workdir(conn, &art.session_id)? {
        return Ok(project);
    }
    match resolve_session_workspace(conn, &art.session_id)? {
        SessionWorkspace::Repo(p) => Ok(p),
        SessionWorkspace::Local => crate::worktree::base_repo_for_local_session(&art.session_id),
    }
}

pub(super) fn ensure_inplace_or_app_workspace(
    session_id: &str,
    project: Option<std::path::PathBuf>,
) -> Result<std::path::PathBuf, String> {
    match project {
        Some(project) => Ok(project),
        None => crate::worktree::ensure_workspace(session_id, None, true),
    }
}

/// Recalculates a member's cwd deterministically during finalization.
/// The session-level in-place project path depends only on session_id, not assignment_id.
/// Every member in one start_team_run therefore has the same value, which can be computed once while holding the lock
/// instead of recomputed for every member, reducing repeated reads. `resolve_session_workspace` and
/// `inplace_project_path` remain database-only and semantically unchanged.
/// If it returns `Some`, the session is in place and all members share this project path without creating another
/// worktree. If it returns `None`, each member still needs its own ensure_member_workspace call.
pub(crate) fn session_inplace_wt(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    let _workspace = resolve_session_workspace(conn, session_id)?;
    ensure_inplace_session_workdir(conn, session_id)
}

pub(super) fn resolve_member_wt(
    conn: &rusqlite::Connection,
    session_id: &str,
    assignment_id: &str,
) -> Result<std::path::PathBuf, String> {
    if let Some(project) = session_inplace_wt(conn, session_id)? {
        return Ok(project);
    }
    crate::worktree::ensure_member_workspace(session_id, assignment_id, None, true)
}

pub(crate) fn ensure_session_workspace(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<(SessionWorkspace, std::path::PathBuf), String> {
    ensure_session_live(conn, session_id)?;
    let workspace = resolve_session_workspace(conn, session_id)?;
    let wt = ensure_inplace_or_app_workspace(
        session_id,
        ensure_inplace_session_workdir(conn, session_id)?,
    )?;
    Ok((workspace, wt))
}

pub(crate) fn ensure_session_live(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<(), String> {
    // Soft-deleted sessions must not create a workspace, which prevents resurrection as an orphan.
    // restore_session clears the tombstone before this gate runs.
    let deleted_at: Option<i64> = conn
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    if deleted_at.is_some() {
        return Err(format!("SESSION_DELETED:{session_id}"));
    }
    // Archived sessions must not reattach a workspace. Archiving released the folder, so a stray frontend file-viewer
    // access through list_session_files or read_session_file must not rebuild it and make archiving fail to stick.
    // Reattachment is valid only during unarchive, which clears `archived` before set_session_archived calls ensure,
    // allowing this gate to pass.
    let archived: bool = conn
        .query_row(
            "SELECT archived FROM sessions WHERE id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(false);
    if archived {
        return Err(format!("SESSION_ARCHIVED:{session_id}"));
    }
    Ok(())
}

/// Explicit-cwd version: sets the command's working directory to the provided path, for the Codex backend.
pub(crate) fn apply_workdir(cmd: &mut Command, wt: &std::path::Path) {
    cmd.current_dir(wt);
}

/// Resolves and applies the session working directory uniformly for simple engine branches.
///
/// Returns the absolute cwd for callers to record or test; does not handle sandboxing or environment cleanup.
#[allow(dead_code)]
pub(crate) fn apply_session_workdir(
    cmd: &mut Command,
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<std::path::PathBuf, String> {
    let (_, wt) = ensure_session_workspace(conn, session_id)?;
    apply_workdir(cmd, &wt);
    Ok(wt)
}
