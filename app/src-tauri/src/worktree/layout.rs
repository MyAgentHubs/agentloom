use std::path::{Path, PathBuf};

use super::{
    assert_app_domain_path, finalize_session_before_cleanup, git_checked_stdout, git_ref_exists,
    run_git, session_wt_path, worktree_registered,
};

pub(crate) fn default_root() -> PathBuf {
    home_dir().join(".agentloom").join("worktrees")
}

pub fn journals_dir() -> PathBuf {
    home_dir().join(".agentloom").join("journals")
}

/// Legacy default session workspace root: ~/.agentloom/sessions/.
/// C2-A cleanup uses it only to remove legacy directories.
pub(crate) fn default_sessions_root() -> PathBuf {
    home_dir().join(".agentloom").join("sessions")
}

/// Cluster L Phase 3 plan C2-A: session workspace root for the Local namespace.
/// ~/.agentloom/local/sessions/<session_id>/; group is purely virtual and does not enter the physical path.
pub fn local_sessions_root() -> PathBuf {
    home_dir().join(".agentloom").join("local").join("sessions")
}

pub(super) fn canonical_managed_worktree(wt: &Path) -> Result<PathBuf, String> {
    let canonical_wt = std::fs::canonicalize(wt).map_err(|e| {
        format!(
            "拒绝访问非受管 worktree {}：路径无法 canonicalize：{}",
            wt.display(),
            e
        )
    })?;
    for root in [default_root(), local_sessions_root()] {
        let Ok(canonical_root) = std::fs::canonicalize(root) else {
            continue;
        };
        if canonical_wt != canonical_root && canonical_wt.starts_with(&canonical_root) {
            return Ok(canonical_wt);
        }
    }
    Err(format!(
        "拒绝访问非受管 worktree：{}",
        canonical_wt.display()
    ))
}

pub(super) fn ensure_worktree_for_default_in(
    root: &Path,
    session_id: &str,
) -> Result<PathBuf, String> {
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.session.invalidDefaultId", &[]));
    }
    let dir = root.join(&safe);
    if dir.join(".git").exists() {
        assert_app_domain_path(&dir, "ensure_default_workspace")?;
        let base_ref = format!("refs/agentloom/base/{safe}");
        if !git_ref_exists(&dir, &base_ref) {
            run_git(&dir, &["update-ref", &base_ref, "HEAD"])?;
        }
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    assert_app_domain_path(&dir, "ensure_default_workspace")?;
    // Initialize the app-managed session scaffold with signing disabled.
    let out = crate::proc::command("git")
        .current_dir(&dir)
        .args(["-c", "commit.gpgsign=false", "init", "-q"])
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.scaffold.defaultInitSpawnFailed",
                &[("detail", e.to_string())],
            )
        })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.defaultInitFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }
    // Use an empty initial commit as the base ref so review can calculate a diff immediately.
    let _ = crate::proc::command("git")
        .current_dir(&dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=agentloom@local",
            "-c",
            "user.name=AgentLoom",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "agentloom: default session init",
        ])
        .output();
    // Point the base ref at HEAD, matching the existing ensure_worktree_in policy.
    let _ = crate::proc::command("git")
        .current_dir(&dir)
        .args(["update-ref", &format!("refs/agentloom/base/{safe}"), "HEAD"])
        .output();
    Ok(dir)
}

/// Coding-loop cut 1, plan 5: recompute the base_repo for a Local session so lib.rs can resolve
/// repo_path for verify/merge. Equivalent to
/// ensure_worktree_for_default_in(local_sessions_root(), session_id); idempotent and crate-visible.
#[allow(dead_code)]
pub(crate) fn base_repo_for_local_session(session_id: &str) -> Result<PathBuf, String> {
    ensure_worktree_for_default_in(&local_sessions_root(), session_id)
}

/// Agent stderr log directory: ~/.agentloom/logs.
pub fn logs_dir() -> PathBuf {
    home_dir().join(".agentloom").join("logs")
}

pub(super) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Sanitize session_id into a safe path and branch component containing only alphanumerics and hyphens.
pub fn safe_id(session_id: &str) -> String {
    session_id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}

/// The injectable root supports tests. Worktree path = root/<repo-name>/<safe-session>.
pub(super) fn ensure_worktree_in(
    root: &Path,
    repo: &Path,
    session_id: &str,
) -> Result<PathBuf, String> {
    assert_app_domain_path(repo, "ensure_worktree")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.session.invalidId", &[]));
    }
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let wt = root.join(&repo_name).join(&safe);
    let branch = format!("agentloom/{safe}");
    let base_ref = format!("refs/agentloom/base/{safe}");

    // Determine reuse from Git's worktree list, not directory existence, to avoid stale metadata mismatch.
    if wt.exists() && worktree_registered(repo, &wt)? {
        if !git_ref_exists(&wt, &base_ref) {
            run_git(repo, &["update-ref", &base_ref, "HEAD"])?;
        }
        return Ok(wt);
    }
    std::fs::create_dir_all(wt.parent().unwrap()).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    // Prune stale metadata first; otherwise add reports 128 after a directory was deleted.
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "prune"])
        .output();
    // Section 5/D12: re-attach an existing session branch at its tip without `-B`; never reset its
    // ref and erase landed commits. Create a missing branch from repo HEAD with `-b`. Never use `-B`.
    let branch_ref = format!("refs/heads/{branch}");
    let out = if git_ref_exists(repo, &branch_ref) {
        crate::proc::command("git")
            .current_dir(repo)
            .args(["worktree", "add"])
            .arg(&wt)
            .arg(&branch)
            .output()
    } else {
        crate::proc::command("git")
            .current_dir(repo)
            .args(["worktree", "add", "-b", &branch])
            .arg(&wt)
            .output()
    }
    .map_err(|e| {
        crate::ui_msg::al_err(
            "wt.scaffold.sessionWorktreeSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !out.status.success() {
        // M1: a fatal re-attach error, such as another live worktree checking out this session branch,
        // correctly fails closed. Never fall back to `-B`, which would erase the session branch.
        // Propagate Git stderr unchanged for diagnosis.
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.sessionWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }
    // base_ref is the fork point and diff baseline. Set it only when missing so re-attaching an
    // existing session preserves the original fork point instead of resetting base_ref.
    if !git_ref_exists(repo, &base_ref) {
        let _ = crate::proc::command("git")
            .current_dir(repo)
            .args(["update-ref", &base_ref, "HEAD"])
            .output();
    }
    Ok(wt)
}

pub fn derive_continuation_workspace(
    repo: &Path,
    parent: &str,
    child: &str,
) -> Result<PathBuf, String> {
    assert_app_domain_path(repo, "derive_continuation_workspace")?;
    let parent_safe = safe_id(parent);
    let child_safe = safe_id(child);
    if parent_safe.is_empty() || child_safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.continuation.invalidIds", &[]));
    }

    finalize_session_before_cleanup(parent, repo)?;

    let parent_ref = format!("refs/heads/agentloom/{parent_safe}");
    let parent_head = git_checked_stdout(repo, &["rev-parse", &parent_ref])?
        .trim()
        .to_string();
    let child_ref = format!("refs/heads/agentloom/{child_safe}");
    if git_ref_exists(repo, &child_ref) {
        return Err(crate::ui_msg::al_err(
            "wt.continuation.childBranchExists",
            &[("child", child_ref.clone())],
        ));
    }
    let base_ref = format!("refs/agentloom/base/{child_safe}");
    if git_ref_exists(repo, &base_ref) {
        return Err(crate::ui_msg::al_err(
            "wt.continuation.baseRefExists",
            &[("base", base_ref.clone())],
        ));
    }

    let wt = session_wt_path(repo, &child_safe);
    std::fs::create_dir_all(wt.parent().unwrap()).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "prune"])
        .output();

    let branch = format!("agentloom/{child_safe}");
    let out = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "add", "-b", &branch])
        .arg(&wt)
        .arg(&parent_head)
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.scaffold.continuationWorktreeSpawnFailed",
                &[("detail", e.to_string())],
            )
        })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.continuationWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }

    if let Err(e) = run_git(repo, &["update-ref", &base_ref, &parent_head]) {
        let mut err = e;
        if let Err(cleanup_err) = cleanup_continuation_workspace(repo, child) {
            err =
                format!("{err}\ncleanup after continuation worktree failure failed: {cleanup_err}");
        }
        return Err(err);
    }
    Ok(wt)
}

pub fn cleanup_continuation_workspace(repo: &Path, child: &str) -> Result<(), String> {
    assert_app_domain_path(repo, "cleanup_continuation_workspace")?;
    let child_safe = safe_id(child);
    if child_safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.continuation.invalidChildId", &[]));
    }

    let wt = session_wt_path(repo, &child_safe);
    let mut errors = Vec::new();

    match worktree_registered(repo, &wt) {
        Ok(true) => {
            if let Some(wt_str) = wt.to_str() {
                if let Err(e) = run_git(repo, &["worktree", "remove", "--force", wt_str]) {
                    errors.push(e);
                }
            } else {
                errors.push(crate::ui_msg::al_err(
                    "wt.continuation.pathNotUtf8",
                    &[("path", wt.display().to_string())],
                ));
            }
        }
        Ok(false) => {
            if wt.exists() {
                if let Err(e) = std::fs::remove_dir_all(&wt) {
                    errors.push(crate::ui_msg::al_err(
                        "wt.continuation.removeResidualFailed",
                        &[("detail", e.to_string())],
                    ));
                }
            }
        }
        Err(e) => errors.push(e),
    }

    if let Err(e) = run_git(repo, &["worktree", "prune"]) {
        errors.push(e);
    }

    let child_ref = format!("refs/heads/agentloom/{child_safe}");
    let base_ref = format!("refs/agentloom/base/{child_safe}");
    let refs_may_be_deleted = match worktree_registered(repo, &wt) {
        Ok(false) => true,
        Ok(true) => {
            errors.push(crate::ui_msg::al_err(
                "wt.continuation.refsStillRegistered",
                &[("path", wt.display().to_string())],
            ));
            false
        }
        Err(e) => {
            errors.push(e);
            false
        }
    };

    if refs_may_be_deleted {
        let mut branch_gone = !git_ref_exists(repo, &child_ref);
        if !branch_gone {
            match run_git(repo, &["branch", "-D", &format!("agentloom/{child_safe}")]) {
                Ok(()) => branch_gone = true,
                Err(e) => errors.push(e),
            }
        }

        if branch_gone && git_ref_exists(repo, &base_ref) {
            if let Err(e) = run_git(repo, &["update-ref", "-d", &base_ref]) {
                errors.push(e);
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}
