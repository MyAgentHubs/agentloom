use std::path::{Path, PathBuf};

use super::{
    assert_app_domain_path, default_root, ensure_worktree_for_default_in, git_ref_exists,
    local_sessions_root, run_git, safe_id, worktree_registered,
};

/// Explicit-path variant (testable): add a member worktree from `base_repo` with an isolated branch and base ref.
/// `wt` must be a sibling of the session worktree; the caller guarantees it is not nested.
#[allow(dead_code)]
pub(super) fn add_member_worktree(
    base_repo: &Path,
    wt: &Path,
    tag: &str,
    start_ref: Option<&str>,
) -> Result<PathBuf, String> {
    assert_app_domain_path(base_repo, "add_member_worktree")?;
    let branch = format!("agentloom/{tag}");
    let base_ref = format!("refs/agentloom/base/{tag}");
    if wt.exists() && worktree_registered(base_repo, wt)? {
        if !git_ref_exists(wt, &base_ref) {
            run_git(base_repo, &["update-ref", &base_ref, "HEAD"])?;
        }
        return Ok(wt.to_path_buf());
    }
    std::fs::create_dir_all(wt.parent().unwrap()).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    let _ = crate::proc::command("git")
        .current_dir(base_repo)
        .args(["worktree", "prune"])
        .output();
    // D12: Repo sessions derive from the session branch tip when `start_ref` is present;
    // Local sessions retain the existing behavior and derive from `base_repo` HEAD.
    let mut cmd = crate::proc::command("git");
    cmd.current_dir(base_repo)
        .args(["worktree", "add", "-B", &branch])
        .arg(wt);
    if let Some(sr) = start_ref {
        cmd.arg(sr);
    }
    let out = cmd.output().map_err(|e| {
        crate::ui_msg::al_err(
            "wt.scaffold.memberWorktreeSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.memberWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }
    // `base_ref` records the derivation point (`start_ref` when present, otherwise repo HEAD)
    // as the post-run diff baseline.
    match start_ref {
        Some(sr) => {
            let _ = run_git(base_repo, &["update-ref", &base_ref, sr]);
        }
        None => {
            let _ = crate::proc::command("git")
                .current_dir(base_repo)
                .args(["update-ref", &base_ref, "HEAD"])
                .output();
        }
    }
    Ok(wt.to_path_buf())
}

/// Public entry point: resolve the base repo (upstream repo for Repo sessions, the session's
/// initialized directory for Local sessions) and calculate the sibling member path.
#[allow(dead_code)]
pub fn ensure_member_workspace(
    session_id: &str,
    assignment_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<PathBuf, String> {
    let s_safe = safe_id(session_id);
    let a_safe = safe_id(assignment_id);
    if s_safe.is_empty() || a_safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.session.invalidMemberIds", &[]));
    }
    let tag = format!("{s_safe}-m-{a_safe}");
    if is_local {
        // Local: first ensure the session's own Git repository exists; it is the base.
        let session_repo = ensure_worktree_for_default_in(&local_sessions_root(), session_id)?;
        let wt = local_sessions_root()
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        add_member_worktree(&session_repo, &wt, &tag, None)
    } else {
        let repo = repo_path.ok_or("github_org session 缺 repo path")?;
        assert_app_domain_path(repo, "ensure_member_workspace")?;
        let repo_name = repo
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        // D12: ensure the session branch ref `agentloom/<session>` exists. The normal flow creates
        // it when the session starts; this is an idempotent, non-destructive fallback.
        // Create only the branch ref, not the session worktree, so tests do not leave session
        // worktrees in the real `~/.agentloom` directory.
        // ensure_session_workspace owns session worktree creation; create a missing branch at repository HEAD.
        // The normal flow does not hit this fallback. Preserve an existing branch without resetting
        // it, which would erase changes that a previous worker already landed on the session branch.
        let session_branch = format!("agentloom/{s_safe}");
        let session_ref = format!("refs/heads/{session_branch}");
        if !git_ref_exists(repo, &session_ref) {
            let _ = run_git(repo, &["branch", &session_branch]); // Defaults to the repo's current HEAD.
        }
        let wt = default_root()
            .join(&repo_name)
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        add_member_worktree(repo, &wt, &tag, Some(&session_ref))
    }
}

pub fn cleanup_member_workspace(
    session_id: &str,
    assignment_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<(), String> {
    let s_safe = safe_id(session_id);
    let a_safe = safe_id(assignment_id);
    if s_safe.is_empty() || a_safe.is_empty() {
        return Ok(());
    }
    let tag = format!("{s_safe}-m-{a_safe}");
    let (base_repo, wt) = if is_local {
        let base_repo = local_sessions_root().join(&s_safe);
        let wt = local_sessions_root()
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        (base_repo, wt)
    } else {
        let Some(repo) = repo_path else {
            return Ok(());
        };
        let repo_name = repo
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let wt = default_root()
            .join(&repo_name)
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        (repo.to_path_buf(), wt)
    };
    assert_app_domain_path(&base_repo, "cleanup_member_workspace")?;
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .output();
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["worktree", "prune"])
        .output();
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["branch", "-D", &format!("agentloom/{tag}")])
        .output();
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["update-ref", "-d", &format!("refs/agentloom/base/{tag}")])
        .output();
    // D32: remove the empty `<session>__members` parent shell. `remove_dir` only removes an empty
    // directory, so this is a no-op while other members remain.
    if let Some(parent) = wt.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}
