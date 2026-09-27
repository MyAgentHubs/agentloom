use super::*;

/// Provide a real undo operation so automatically applied changes remain reversible without a confirmation gate.
#[tauri::command]
pub(super) fn get_run_goal_title(
    db: tauri::State<'_, db::Db>,
    session_id: String,
    run_id: String,
) -> Result<Option<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::goal_title_for_run(&conn, &session_id, &run_id).map_err(|e| e.to_string())
}

/// Associate applied changes with their commit, line counts, and files so the review panel shows what actually landed.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct LandingFile {
    pub(super) path: String,
    pub(super) insertions: i64,
    pub(super) deletions: i64,
}

#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct LandingInfo {
    pub(super) landed_head: String,
    pub(super) pre_head: String,
    pub(super) files_changed: i64,
    pub(super) insertions: i64,
    pub(super) deletions: i64,
    /// List of changed files (per-file line counts). In-place reads the **project directory**.
    pub(super) files: Vec<LandingFile>,
}

/// Convert absolute paths recorded by checkpoints into project-relative paths for frontend display: first strip the actual session cwd (under local-default,
/// this is the per-session subdirectory, and checkpoints created in this run record that prefix); if it does not match, fall back to
/// the project root (for compatibility with old checkpoints created before switching to a subdirectory, which record the root prefix); if neither prefix matches, fall back to displaying only
/// the filename—never bake the host's absolute path verbatim into the frontend / delivery document.
fn strip_checkpoint_display_path(
    absolute: &std::path::Path,
    session_workdir: Option<&std::path::Path>,
    project_root: &std::path::Path,
) -> String {
    session_workdir
        .and_then(|base| absolute.strip_prefix(base).ok())
        .or_else(|| absolute.strip_prefix(project_root).ok())
        .map(|relative| relative.to_string_lossy().into_owned())
        .or_else(|| {
            absolute
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "<invalid checkpoint path>".to_string())
}

/// Build review-panel details from the latest landing record so the panel reflects actual applied changes.
/// - in-place: obtain the file list from the checkpoint; only old records without a checkpoint compatibly read git numstat from the project directory.
/// - repo / non-in-place: use the values stored in LandingCommit for line counts (correctly calculated when apply landed); read changed files from the landed repo working root.
/// No LandingCommit (never landed / already reverted) → Ok(None); the frontend accordingly does not show "landed/reverted."
pub(super) fn run_landing_info_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> Result<Option<LandingInfo>, String> {
    let Some(lc) =
        db::latest_landing_commit(conn, session_id, run_id).map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };

    // Target directory for git commands: always use the repository root (`inplace_project_path`)—under in-place, the per-session
    // directory is only a nested directory, not a new top level, and git subprocesses report relative paths from the repository root; using the subdirectory as cwd makes
    // commands such as numstat disagree with the repository top level (confirmed by reproduction: when the user configures `git config diff.relative=true`,
    // `git diff --numstat` from the subdirectory cwd silently omits changes on the repository-root side, while anchoring at the root is immune). Only old artifact
    // data falls back to the managed repo.
    let (target, in_place) = match inplace_project_path(conn, session_id)? {
        Some(project) => (project, true),
        None => (
            match resolve_session_workspace(conn, session_id)? {
                SessionWorkspace::Repo(p) => p,
                SessionWorkspace::Local => {
                    crate::worktree::base_repo_for_local_session(session_id)?
                }
            },
            false,
        ),
    };
    // Prefix stripping for the display layer: the actual session cwd (under local-default, the per-session subdirectory) and the git
    // cwd above are variables with two different frames of reference; do not combine them again—read-only resolution, with no directory creation.
    let session_workdir = if in_place {
        inplace_session_workdir(conn, session_id)?
    } else {
        None
    };

    // New in-place records use the checkpoint as the source of truth for file attribution and never include the user's other git
    // changes from the same period in this run. Only old data without a checkpoint compatibly falls back to pre_head..landed_head numstat.
    let canonical_target = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
    let canonical_session_workdir = session_workdir
        .as_ref()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
    let checkpoint_files = if in_place {
        list_run_undo_entries_inner(conn, session_id, run_id)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| LandingFile {
                path: strip_checkpoint_display_path(
                    &entry.file_path,
                    canonical_session_workdir.as_deref(),
                    &canonical_target,
                ),
                insertions: 0,
                deletions: 0,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let files: Vec<LandingFile> = if checkpoint_files.is_empty() {
        match crate::worktree::numstat_files_between(&target, &lc.pre_head, &lc.landed_head) {
            Ok(rows) => rows
                .into_iter()
                .map(|(path, insertions, deletions)| LandingFile {
                    path,
                    insertions,
                    deletions,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        checkpoint_files
    };

    // Prefer recomputed line counts when available and retain stored counts when recomputation yields no files.
    let (insertions, deletions, files_changed) = if files.is_empty() {
        (lc.insertions, lc.deletions, lc.files_changed)
    } else {
        let ins: i64 = files.iter().map(|f| f.insertions).sum();
        let del: i64 = files.iter().map(|f| f.deletions).sum();
        (ins, del, files.len() as i64)
    };

    Ok(Some(LandingInfo {
        landed_head: lc.landed_head,
        pre_head: lc.pre_head,
        files_changed,
        insertions,
        deletions,
        files,
    }))
}

#[tauri::command]
pub(super) fn run_landing_info(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    run_id: String,
) -> Result<Option<LandingInfo>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    run_landing_info_inner(&conn, &session_id, &run_id)
}

pub(super) fn member_artifact_diff_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    member_assignment_id: &str,
) -> Result<String, String> {
    let art =
        match crate::db::get_artifact_by_member(conn, session_id, run_id, member_assignment_id)
            .map_err(|e| e.to_string())?
        {
            Some(a) => a,
            None => return Ok(String::new()),
        };
    let head = match art.commit_sha {
        Some(h) => h,
        None => return Ok(String::new()),
    };
    // An in-place artifact's base/commit are both in the same git repository (written in place); diff must read the **repository root**
    // (`inplace_project_path`), never base_repo_for_local_session (an empty sessions repo that cannot read
    // these two commits). This is purely a git perspective and does not involve display-layer stripping: under local-default, the per-session subdirectory
    // is only a nested directory, not a new top level; using it as the git cwd when the user has configured `git config diff.relative=true`
    // makes diff output silently omit changes on the repository-root side—anchoring at the root is immune, so return to root anchoring.
    let repo = match inplace_project_path(conn, session_id)? {
        Some(project) => project,
        None => match resolve_session_workspace(conn, session_id)? {
            SessionWorkspace::Repo(p) => p,
            SessionWorkspace::Local => crate::worktree::base_repo_for_local_session(session_id)?,
        },
    };
    crate::worktree::artifact_diff_text(&repo, &art.base_sha, &head)
}

#[tauri::command]
pub(super) fn member_artifact_diff(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    run_id: String,
    member_assignment_id: String,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    member_artifact_diff_inner(&conn, &session_id, &run_id, &member_assignment_id)
}

#[tauri::command]
pub(super) fn list_interrupted_team_runs(
    db: State<Db>,
    session_id: String,
) -> Result<Vec<db::TeamRunPendingRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_interrupted_team_runs(&conn, &session_id).map_err(|e| e.to_string())
}
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGoal {
    pub text: String,
    pub title: Option<String>,
}

pub(super) fn session_goal_from_block(b: Option<db::MemoryBlock>) -> Option<SessionGoal> {
    b.map(|b| SessionGoal {
        text: b.text,
        title: b.title,
    })
}

#[tauri::command]
pub(super) fn get_session_goal(
    db: State<Db>,
    session_id: String,
) -> Result<Option<SessionGoal>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let blk = db::get_memory_block(&conn, &session_id, "goal").map_err(|e| e.to_string())?;
    Ok(session_goal_from_block(blk))
}
