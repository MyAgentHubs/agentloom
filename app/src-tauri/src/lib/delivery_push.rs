use super::*;

pub(super) fn apply_run_to_current_branch_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> Result<String, String> {
    let (repo, is_local) = match resolve_session_workspace(conn, session_id)? {
        SessionWorkspace::Repo(p) => (p, false),
        SessionWorkspace::Local => (
            crate::worktree::base_repo_for_local_session(session_id)?,
            true,
        ),
    };
    crate::worktree::assert_app_domain_path(&repo, "apply_run_to_current_branch")?;
    let pre_head = crate::worktree::rev_parse_head(&repo)?;
    let landed_head = crate::worktree::apply_staging_ff_only(&repo, run_id)?;
    let stats = crate::worktree::landing_stats(&repo, &pre_head, &landed_head)?;
    let artifact_id = crate::db::merged_artifact_for_run(conn, session_id, run_id)
        .map_err(|e| e.to_string())?
        .map(|a| a.id);
    crate::db::insert_landing_commit(
        conn,
        &crate::db::LandingCommit {
            id: crate::new_run_id(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            artifact_id,
            pre_head,
            landed_head: landed_head.clone(),
            commit_count: stats.commit_count,
            files_changed: stats.files_changed,
            insertions: stats.insertions,
            deletions: stats.deletions,
            created_at: crate::db::now_secs(),
        },
    )
    .map_err(|e| e.to_string())?;
    // Hygiene: on successful landing, best-effort clean up this run's footprint in the agentloom/<name> namespace; cleanup failures never roll back the landing.
    cleanup_run_workspaces(conn, session_id, run_id, &repo, is_local)?;
    Ok(landed_head)
}

/// Hygiene: after landing, clean up this run's footprint in the agentloom/ namespace — the staging branch plus each member's worktree, branch, and base ref.
/// This may run only inside the app domain; out-of-domain paths return a structured error, while in-domain cleanup remains best-effort step by step.
pub(super) fn cleanup_run_workspaces(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    repo: &std::path::Path,
    is_local: bool,
) -> Result<(), String> {
    crate::worktree::assert_app_domain_path(repo, "cleanup_run_workspaces")?;
    let _ = crate::worktree::delete_staging_branch(repo, run_id);
    let repo_opt = if is_local { None } else { Some(repo) };
    for assignment_id in team_run_assignment_ids(conn, session_id, run_id) {
        let _ = crate::worktree::cleanup_member_workspace(
            session_id,
            &assignment_id,
            repo_opt,
            is_local,
        );
    }
    Ok(())
}

/// Read assignment IDs from this run's pending team record for member-workspace cleanup.
/// Missing or invalid data is nonfatal and yields an empty list. Parsing uses the same
/// `assignment_id` field as startup recovery and includes members with no changes or failed runs.
fn team_run_assignment_ids(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> Vec<String> {
    let json = match crate::db::team_run_pending_assignments(conn, session_id, run_id) {
        Ok(Some(j)) => j,
        _ => return Vec::new(),
    };
    serde_json::from_str::<Vec<serde_json::Value>>(&json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            v.get("assignment_id")
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .collect()
}

#[tauri::command]
pub(super) fn apply_run_to_current_branch(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    run_id: String,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    apply_run_to_current_branch_inner(&conn, &session_id, &run_id)
}

// The push, pull-request, and publish commands share a precondition. In-place sessions use the
// current branch after verifying that changes are committed; isolated workspaces apply through
// the idempotent `needs_landing` guard. Stage-prefixed errors distinguish landing failures from
// failures that occur after a successful landing.

/// Read the repository's default branch from `origin/HEAD`, falling back to `master`.
fn default_base_branch(repo: &std::path::Path) -> String {
    let out = crate::worktree::git_read_output(repo, &["rev-parse", "--abbrev-ref", "origin/HEAD"]);
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            // Convert a value such as `origin/master` to `master`.
            if let Some(b) = s.trim().rsplit('/').next() {
                if !b.is_empty() {
                    return b.to_string();
                }
            }
        }
    }
    "master".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InplaceDeliveryDecision {
    Allow,
    Reject { count: usize },
}

/// Allow delivery when the checkpoint list is empty or all files are clean; otherwise return the dirty-file count.
pub(super) fn decide_inplace_delivery(
    checkpoint_states: &[(std::path::PathBuf, bool)],
) -> InplaceDeliveryDecision {
    let count = checkpoint_states.iter().filter(|(_, dirty)| *dirty).count();
    if count == 0 {
        InplaceDeliveryDecision::Allow
    } else {
        InplaceDeliveryDecision::Reject { count }
    }
}

pub(super) fn format_inplace_dirty_files(
    project: &std::path::Path,
    states: &[(std::path::PathBuf, bool)],
) -> String {
    const MAX_FILES: usize = 3;
    let canonical_project =
        std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let mut files = states
        .iter()
        .filter(|(_, dirty)| *dirty)
        .take(MAX_FILES)
        .map(|(path, _)| {
            path.strip_prefix(&canonical_project)
                .or_else(|_| path.strip_prefix(project))
                .ok()
                .map(|relative| relative.display().to_string())
                .or_else(|| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "<invalid checkpoint path>".to_string())
        })
        .collect::<Vec<_>>();
    if states.iter().filter(|(_, dirty)| *dirty).count() > MAX_FILES {
        files.push("…".to_string());
    }
    files.join(", ")
}

/// Shared fail-closed gate for all three in-place delivery paths.
/// The checkpoint file list covers every run in the session and is deduplicated in SQL. Non-in-place
/// sessions return before any additional checkpoint or Git reads, preserving their prior behavior.
pub(super) fn require_inplace_delivery_committed(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<(), String> {
    let Some(project) = inplace_project_path(conn, session_id)? else {
        return Ok(());
    };
    let checkpoint_paths = list_session_undo_paths_inner(conn, session_id)?;
    let states = crate::worktree::checkpoint_path_dirty_states(&project, &checkpoint_paths)?;
    match decide_inplace_delivery(&states) {
        InplaceDeliveryDecision::Allow => Ok(()),
        InplaceDeliveryDecision::Reject { count } => Err(ui_msg::al_err(
            "run.inplaceDeliveryUncommitted",
            &[
                ("count", count.to_string()),
                ("files", format_inplace_dirty_files(&project, &states)),
            ],
        )),
    }
}

/// Resolve the repository session workspace and token, ensuring delivery content is on the current branch.
/// In-place changes are already on that branch and must skip run or staging landing used only by isolated workspaces.
/// Returns `(repo_path, branch, gh_token)`. Local sessions use publish instead of push or pull requests.
fn ensure_landed_repo_session(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> Result<(std::path::PathBuf, String, String), String> {
    ensure_landed_repo_session_with_token_resolver(
        conn,
        session_id,
        run_id,
        git_ops::gh_token_for_session,
    )
}

pub(super) fn ensure_landed_repo_session_with_token_resolver(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    resolve_token: impl FnOnce(&rusqlite::Connection, &str) -> Result<String, String>,
) -> Result<(std::path::PathBuf, String, String), String> {
    let repo = match resolve_session_workspace(conn, session_id)? {
        SessionWorkspace::Repo(p) => p,
        SessionWorkspace::Local => {
            return Err("LOCAL_SESSION_NOT_PUSHABLE".to_string());
        }
    };
    let branch = delivery_branch(&repo)?;
    require_inplace_delivery_committed(conn, session_id)?;
    let is_inplace = inplace_project_path(conn, session_id)?.is_some();
    let token = resolve_token(conn, session_id)?;
    if !is_inplace && git_ops::needs_landing(conn, session_id, run_id)? {
        apply_run_to_current_branch_inner(conn, session_id, run_id).map_err(|e| {
            if e.starts_with("AL_ERR:") {
                e
            } else {
                format!("LAND_FAILED:{e}")
            }
        })?;
    }
    Ok((repo, branch, token))
}

/// Expose staged, unapplied change counts in a serializable form for the change bar.
/// `NumstatCount` is not serializable, so this local DTO exposes its field names directly without renaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DiffStats {
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// Keep staged-change counting independent of application state so its logic can be tested in isolation.
/// Count changes already merged into staging but not yet applied, over `base_sha..merged_sha`.
/// Return `None` when no merged artifact, merge candidate, or merged SHA exists. Local sessions
/// also return `None` because their change bar primarily represents already-landed work.
pub(super) fn staging_diff_stats_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> Result<Option<DiffStats>, String> {
    let artifact = match crate::db::merged_artifact_for_run(conn, session_id, run_id)
        .map_err(|e| e.to_string())?
    {
        Some(a) => a,
        None => return Ok(None),
    };
    let merged_sha = match crate::db::get_merge_candidate_by_artifact(conn, &artifact.id)
        .map_err(|e| e.to_string())?
        .and_then(|mc| mc.merged_sha)
    {
        Some(s) => s,
        None => return Ok(None),
    };
    let repo = match resolve_session_workspace(conn, session_id)? {
        SessionWorkspace::Repo(p) => p,
        SessionWorkspace::Local => return Ok(None),
    };
    let count = crate::worktree::run_numstat(&repo, &artifact.base_sha, &merged_sha)?;
    Ok(Some(DiffStats {
        files: count.files,
        insertions: count.insertions,
        deletions: count.deletions,
    }))
}

/// Return file and line counts for staged changes so the change bar distinguishes pending work from applied changes.
#[tauri::command]
pub(super) fn staging_diff_stats(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    run_id: String,
) -> Result<Option<DiffStats>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    staging_diff_stats_inner(&conn, &session_id, &run_id)
}

/// Push this run, already landed on the current branch, to `origin` and return a summary.
#[tauri::command]
pub(super) fn push_run(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    run_id: String,
    confirmed: bool,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    push_run_inner(&conn, &session_id, &run_id, confirmed)
}

pub(super) fn push_run_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    confirmed: bool,
) -> Result<String, String> {
    git_ops::require_explicit_confirmation(confirmed, "git push")?;
    let (repo, branch, _token) = ensure_landed_repo_session(conn, session_id, run_id)?;
    git_ops::git_push(&repo, "origin", &branch, confirmed)
        .map_err(|e| format!("PUSH_FAILED:{e}"))?;
    Ok(ui_msg::al_err("publish.pushed", &[("branch", branch)]))
}

/// Ensure landing, push, and then create a pull request for the current branch against the repository's default branch.
#[tauri::command]
pub(super) fn create_pr_run(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    run_id: String,
    title: Option<String>,
    body: Option<String>,
    confirmed: bool,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    create_pr_run_inner(&conn, &session_id, &run_id, title, body, confirmed)
}

pub(super) fn create_pr_run_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    title: Option<String>,
    body: Option<String>,
    confirmed: bool,
) -> Result<String, String> {
    git_ops::require_explicit_confirmation(confirmed, "create pull request")?;
    let (repo, branch, token) = ensure_landed_repo_session(conn, session_id, run_id)?;
    // Push the head branch first so `gh pr create` can find it remotely.
    git_ops::git_push(&repo, "origin", &branch, confirmed)
        .map_err(|e| format!("PUSH_FAILED:{e}"))?;
    let base = default_base_branch(&repo);
    // Prefer the supplied title, then the run's goal title, and finally the branch name.
    let resolved_title = title
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            db::goal_title_for_run(conn, session_id, run_id)
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| format!("AgentLoom: {branch}"));
    git_ops::gh_pr_create(
        &repo,
        &branch,
        &base,
        &resolved_title,
        body.as_deref(),
        &token,
    )
    .map_err(|e| format!("PR_FAILED:{e}"))
}

/// Publish a Local session's repository as a new GitHub repository and return its URL.
/// Local sessions have no namespace account, so use the single logged-in GitHub account and return a clear error when there are zero or multiple accounts.
#[tauri::command]
pub(super) fn publish_local_run(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    run_id: String,
    repo_name: Option<String>,
    private: Option<bool>,
    confirmed: bool,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    publish_local_run_inner(&conn, &session_id, &run_id, repo_name, private, confirmed)
}

pub(super) fn publish_local_run_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    repo_name: Option<String>,
    private: Option<bool>,
    confirmed: bool,
) -> Result<String, String> {
    git_ops::require_explicit_confirmation(confirmed, "publish")?;
    require_inplace_delivery_committed(conn, session_id)?;
    // Publishing is only for Local sessions; GitHub-backed sessions already have an origin and use push or pull requests.
    let (repo, is_inplace) = match resolve_session_workspace(conn, session_id)? {
        SessionWorkspace::Local => match inplace_project_path(conn, session_id)? {
            Some(project) => (project, true),
            None => (
                crate::worktree::base_repo_for_local_session(session_id)?,
                false,
            ),
        },
        SessionWorkspace::Repo(_) => {
            return Err(ui_msg::al_err("publish.failed.boundRepo", &[]));
        }
    };
    delivery_branch(&repo)?;
    // A Local session has no namespace account, so automatically use only a single logged-in GitHub account.
    let accounts = crate::github::read_gh_accounts()
        .map_err(|e| ui_msg::al_err("publish.failed", &[("detail", e)]))?;
    let login = match accounts.as_slice() {
        [one] => one.login.clone(),
        [] => {
            return Err(ui_msg::al_err("publish.needsAccount.missing", &[]));
        }
        many => {
            let logins: Vec<&str> = many.iter().map(|a| a.login.as_str()).collect();
            return Err(ui_msg::al_err(
                "publish.needsAccount.multiple",
                &[("list", logins.join(", "))],
            ));
        }
    };
    let token = crate::github::gh_token_for(&login)
        .map_err(|e| ui_msg::al_err("publish.failed", &[("detail", e)]))?;
    // Prefer the supplied repository name, then the run's goal title, or report that a name is required.
    let name = repo_name
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            db::goal_title_for_run(conn, session_id, run_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| ui_msg::al_err("publish.failed.missingRepoName", &[]))?;
    // In-place work is already on the current branch; only isolated workspaces must apply staging first.
    if !is_inplace && git_ops::needs_landing(conn, session_id, run_id)? {
        apply_run_to_current_branch_inner(conn, session_id, run_id).map_err(|e| {
            if e.starts_with("AL_ERR:") {
                e
            } else {
                format!("LAND_FAILED:{e}")
            }
        })?;
    }
    git_ops::gh_repo_create(&repo, &name, private.unwrap_or(true), &token)
        .map_err(|e| ui_msg::al_err("publish.failed", &[("detail", e)]))
}

/// Data source for the change bar's two states and labels.
/// JSON fields follow the repository's camelCase convention: `hasRemote`, `repoLabel`, `branch`, and `account`.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionRemoteInfo {
    has_remote: bool,
    repo_label: String,
    branch: String,
    account: Option<String>,
}

pub(super) fn local_repo_label(locale: Locale) -> &'static str {
    match locale {
        Locale::Zh => "本地",
        Locale::En => "Local",
    }
}

/// Report whether the session has an origin, its repository label, current branch, and GitHub account.
/// Local sessions have no remote or account and read the branch from the local base repository.
/// Repository sessions probe the origin and combine the account or namespace with the short repository name.
#[tauri::command]
pub(super) fn session_remote_info(
    app: AppHandle,
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
) -> Result<SessionRemoteInfo, String> {
    let locale = current_locale(&app);
    // Keep the original single-lock implementation. This registered command currently has no UI
    // caller, so splitting the lock would add an acquisition and weaken read consistency without benefit.
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    match resolve_session_workspace(&conn, &session_id)? {
        SessionWorkspace::Local => {
            // The Local base repository may not exist before the first commit; fall back to the em dash branch label.
            let branch = crate::worktree::base_repo_for_local_session(&session_id)
                .ok()
                .filter(|p| p.exists())
                .map(|p| current_branch(&p))
                .unwrap_or_else(|| "—".to_string());
            Ok(SessionRemoteInfo {
                has_remote: false,
                repo_label: local_repo_label(locale).to_string(),
                branch,
                account: None,
            })
        }
        SessionWorkspace::Repo(path) => {
            let has_remote = git_ops::has_remote(&path);
            let branch = current_branch(&path);
            let account = git_ops::resolve_gh_account_for_session(&conn, &session_id)?;
            // Prefer the GitHub account for the label prefix, then the namespace name, then an empty prefix.
            let prefix = match &account {
                Some(a) => a.clone(),
                None => db::get_session_namespace_id(&conn, &session_id)
                    .map_err(|e| e.to_string())?
                    .and_then(|nid| {
                        namespaces_repo::get_namespace_by_id(&conn, &nid)
                            .ok()
                            .flatten()
                            .map(|ns| ns.name)
                    })
                    .unwrap_or_default(),
            };
            // Prefer the repository table's name, falling back to the path-derived name.
            let repo_name = db::get_session_repo_id(&conn, &session_id)
                .map_err(|e| e.to_string())?
                .and_then(|rid| repos_repo::get_repo_by_id(&conn, &rid).ok().flatten())
                .map(|r| r.name)
                .unwrap_or_else(|| path.to_string_lossy().to_string());
            Ok(SessionRemoteInfo {
                has_remote,
                repo_label: git_ops::compose_repo_label(&prefix, &repo_name),
                branch,
                account,
            })
        }
    }
}
