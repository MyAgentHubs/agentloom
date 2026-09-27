use std::path::{Path, PathBuf};

use super::{
    assert_app_domain_path, default_root, finalize_session_before_cleanup, git_checked_stdout,
    git_read_output, git_symbolic_head, resolve_git_metadata_dirs, run_git, safe_id,
    worktree_registered,
};

pub(super) fn git_ref_exists(dir: &Path, refname: &str) -> bool {
    git_read_output(dir, &["rev-parse", "--verify", "--quiet", refname])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(crate) fn session_wt_path(repo: &Path, safe: &str) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    default_root().join(&repo_name).join(safe)
}

fn git_ref_exists_checked(repo: &Path, refname: &str) -> Result<bool, String> {
    let out =
        git_read_output(repo, &["show-ref", "--verify", "--quiet", refname]).map_err(|e| {
            crate::ui_msg::al_err(
                "wt.git.spawnFailed",
                &[
                    ("cmd", format!("show-ref --verify --quiet {refname}")),
                    ("detail", e.to_string()),
                ],
            )
        })?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("show-ref --verify --quiet {refname}")),
                ("stderr", String::from_utf8_lossy(&out.stderr).to_string()),
            ],
        )),
    }
}

/// Verify that the session workspace derived from the basename actually belongs to the repo resolved from the DB.
/// Resolve and canonicalize both sides through `git rev-parse --git-common-dir` to avoid mixing repos with the same name.
pub(crate) fn worktree_belongs_to_repo(worktree: &Path, repo: &Path) -> Result<bool, String> {
    let actual = resolve_git_metadata_dirs(worktree)?.git_common_dir;
    let expected = resolve_git_metadata_dirs(repo)?.git_common_dir;
    Ok(actual == expected)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let source = std::ffi::CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rename source contains NUL",
        )
    })?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rename destination contains NUL",
        )
    })?;

    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };

    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn rename_no_replace(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-clobber rename is unavailable on this platform",
    ))
}

fn is_rename_collision(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return true;
    }
    #[cfg(unix)]
    {
        return error
            .raw_os_error()
            .is_some_and(|code| code == libc::EEXIST || code == libc::ENOTEMPTY);
    }
    #[cfg(not(unix))]
    false
}

/// Atomically move a directory to a unique trash name. The underlying rename uses no-replace;
/// retry with a different name only when the destination is concurrently occupied. Return all
/// other errors, including EXDEV, unchanged. A failed rename leaves the source in place.
pub(crate) fn move_to_unique_trash(
    source: &Path,
    trash_root: &Path,
    session_id: &str,
    epoch: u128,
) -> Result<PathBuf, String> {
    const MAX_COLLISION_RETRIES: usize = 10;

    for retry in 0..=MAX_COLLISION_RETRIES {
        let name = if retry == 0 {
            format!("{session_id}-{epoch}")
        } else {
            format!("{session_id}-{epoch}-{retry}")
        };
        let destination = trash_root.join(name);
        match rename_no_replace(source, &destination) {
            Ok(()) => return Ok(destination),
            Err(error) if is_rename_collision(&error) => {
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "无法把悬空工地 {} 原子挪到 {}: {error}",
                    source.display(),
                    destination.display()
                ));
            }
        }
    }

    Err(format!(
        "悬空工地 trash 目标连续冲突超过 10 次：{session_id}-{epoch}",
    ))
}

/// A workspace no longer indexed by the DB can only take the most conservative cleanup path:
/// verify path and branch ownership, require a completely clean worktree and an unoccupied trash
/// ref, unregister without --force, then move heads into trash. False means the workspace is dirty
/// and the caller should only log and skip it; any unverifiable state returns Err without deletion.
pub(crate) fn trash_clean_orphan_workspace(
    session_id: &str,
    worktree: &Path,
) -> Result<bool, String> {
    assert_app_domain_path(worktree, "reconcile_orphan_workspace")?;
    let safe = safe_id(session_id);
    if safe.is_empty() || safe != session_id {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.invalidSessionDir",
            &[("session", session_id.to_string())],
        ));
    }

    let status = git_read_output(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .map_err(|e| {
        crate::ui_msg::al_err(
            "wt.reconcile.gitStatusSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !status.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.gitStatusFailed",
            &[(
                "stderr",
                String::from_utf8_lossy(&status.stderr).to_string(),
            )],
        ));
    }
    if !status.stdout.is_empty() {
        return Ok(false);
    }

    let expected_head = format!("refs/heads/agentloom/{safe}");
    match git_symbolic_head(worktree) {
        Some(head) if head == expected_head => {}
        _ => {
            return Err(crate::ui_msg::al_err(
                "wt.reconcile.unexpectedHead",
                &[("expected", expected_head.clone())],
            ))
        }
    }

    let metadata = resolve_git_metadata_dirs(worktree)?;
    if metadata.git_dir == metadata.git_common_dir
        || metadata.git_common_dir.file_name() != Some(std::ffi::OsStr::new(".git"))
    {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.notLinkedWorktree",
            &[("path", worktree.display().to_string())],
        ));
    }
    let repo = metadata
        .git_common_dir
        .parent()
        .ok_or_else(|| {
            crate::ui_msg::al_err(
                "wt.reconcile.baseRepoMissing",
                &[("path", metadata.git_common_dir.display().to_string())],
            )
        })?
        .to_path_buf();
    assert_app_domain_path(&repo, "reconcile_orphan_workspace_refs")?;
    let repo_common_dir = resolve_git_metadata_dirs(&repo)?.git_common_dir;
    if repo_common_dir != metadata.git_common_dir {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.unexpectedCommonDir",
            &[
                ("actual", metadata.git_common_dir.display().to_string()),
                ("expected", repo_common_dir.display().to_string()),
            ],
        ));
    }

    let actual = std::fs::canonicalize(worktree).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.reconcile.worktreeCanonicalizeFailed",
            &[("detail", e.to_string())],
        )
    })?;
    let expected = std::fs::canonicalize(session_wt_path(&repo, &safe)).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.reconcile.expectedPathCanonicalizeFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if actual != expected {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.unexpectedPath",
            &[
                ("actual", actual.display().to_string()),
                ("expected", expected.display().to_string()),
            ],
        ));
    }

    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists_checked(&repo, &trash)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.trashRefExists",
            &[("trash", trash)],
        ));
    }

    // Guard the deletion path and the ref-owning repo separately; remove without --force.
    assert_app_domain_path(worktree, "reconcile_orphan_workspace_remove")?;
    assert_app_domain_path(&repo, "reconcile_orphan_workspace_refs")?;
    let worktree_arg = worktree
        .to_str()
        .ok_or_else(|| "orphan worktree path is not valid UTF-8".to_string())?;
    run_git(&repo, &["worktree", "remove", worktree_arg])?;
    if worktree_registered(&repo, worktree)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.registrationIncomplete",
            &[("path", worktree.display().to_string())],
        ));
    }

    // Check again after worktree removal so a concurrent occupant cannot overwrite an old grace tip.
    if git_ref_exists_checked(&repo, &trash)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.trashRefExists",
            &[("trash", trash)],
        ));
    }
    if git_ref_exists_checked(&repo, &expected_head)? {
        run_git(&repo, &["update-ref", &trash, &expected_head])?;
        run_git(&repo, &["update-ref", "-d", &expected_head])?;
    }
    Ok(true)
}

/// Complete a partially finished soft deletion whose DB state was saved but whose session workspace
/// directory is already gone. After confirming that the path neither exists nor is registered,
/// finish moving heads to trash. A trash ref at the same tip means only heads deletion remained and
/// is safe to resume; a different tip fails closed and never overwrites the old grace snapshot.
pub(crate) fn trash_deleted_session_head_without_workspace(
    session_id: &str,
    repo: &Path,
) -> Result<bool, String> {
    assert_app_domain_path(repo, "reconcile_missing_workspace_refs")?;
    let safe = safe_id(session_id);
    if safe.is_empty() || safe != session_id {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.invalidSessionDir",
            &[("session", session_id.to_string())],
        ));
    }

    let worktree = session_wt_path(repo, &safe);
    if worktree.exists() {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.unexpectedPath",
            &[("actual", worktree.display().to_string())],
        ));
    }
    if worktree_registered(repo, &worktree)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.registrationIncomplete",
            &[("path", worktree.display().to_string())],
        ));
    }

    let heads = format!("refs/heads/agentloom/{safe}");
    if !git_ref_exists_checked(repo, &heads)? {
        return Ok(false);
    }
    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists_checked(repo, &trash)? {
        let heads_tip = git_checked_stdout(repo, &["rev-parse", "--verify", &heads])?;
        let trash_tip = git_checked_stdout(repo, &["rev-parse", "--verify", &trash])?;
        if heads_tip.trim() != trash_tip.trim() {
            return Err(crate::ui_msg::al_err(
                "wt.cleanup.trashRefExists",
                &[("trash", trash)],
            ));
        }
        eprintln!(
            "reconcile_orphan_workspaces: {trash} 已存在且与 {heads} 同 tip，跳过创建并重试删除 heads"
        );
        // The same-tip trash ref was created previously; only retry the failed heads deletion.
        run_git(repo, &["update-ref", "-d", &heads])?;
        return Ok(true);
    }

    run_git(repo, &["update-ref", &trash, &heads])?;
    run_git(repo, &["update-ref", "-d", &heads])?;
    Ok(true)
}

/// Branch destination during session-copy cleanup: Keep archives it for re-attachment and rebuild;
/// Trash soft-deletes it by moving it to the trash ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchDisposition {
    Keep,
    Trash,
}

/// Internal flow: finalize before cleanup, remove the session directory, prune its registration,
/// verify unregistration (I4), then handle the branch according to the disposition.
/// Repo sessions only. Every step fails closed: refs stay untouched until unregistration completes,
/// and ref writes use run_git so errors propagate.
pub(super) fn release_or_trash_in(
    repo: &Path,
    session_id: &str,
    disp: BranchDisposition,
) -> Result<(), String> {
    assert_app_domain_path(repo, "release_or_trash")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    // Reject a preoccupied trash ref before finalize/remove so a partial workspace stays intact.
    // Keep the later duplicate check to catch occupation that races with this preflight check.
    if disp == BranchDisposition::Trash && git_ref_exists_checked(repo, &trash)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.trashRefExists",
            &[("trash", trash)],
        ));
    }
    // Finalize unlanded work before cleanup; propagate failure without deleting the workspace.
    finalize_session_before_cleanup(session_id, repo)?;

    let wt = session_wt_path(repo, &safe);
    // Unregister via remove and prune (D9).
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .output();
    let _ = std::fs::remove_dir_all(&wt); // Fallback when remove fails.
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "prune"])
        .output();
    // I4 fail-closed: touch refs only after confirming that the worktree is no longer registered;
    // otherwise a registered worktree/HEAD would point at a deleted branch, violating D9 and blocking re-attachment.
    if worktree_registered(repo, &wt)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.registrationIncomplete",
            &[("path", wt.display().to_string())],
        ));
    }

    match disp {
        BranchDisposition::Keep => { /* Archive: retain heads and base for re-attachment and rebuild. */
        }
        BranchDisposition::Trash => {
            // M3 fail-closed (Codex and Opus review): reject an existing trash ref so update-ref
            // cannot overwrite an old grace-copy tip and lose recoverable work. Reuse of the same
            // safe ID or a partial update-ref success followed by failed heads deletion must be
            // reconciled separately rather than overwritten silently.
            if git_ref_exists_checked(repo, &trash)? {
                return Err(crate::ui_msg::al_err(
                    "wt.cleanup.trashRefExists",
                    &[("trash", trash.clone())],
                ));
            }
            // Move heads to trash without physical deletion (D8); it remains recoverable during
            // the grace period, and checked run_git propagates errors.
            // I2: do not delete the base ref here because review/discard/diff still need it after
            // restore. Leave it for GC or purge.
            if git_ref_exists_checked(repo, &heads)? {
                run_git(repo, &["update-ref", &trash, &heads])?; // trash = heads tip
                run_git(repo, &["update-ref", "-d", &heads])?; // Delete heads only after confirmed unregistration.
            }
        }
    }
    Ok(())
}

/// Archive by deleting the session directory and retaining the branch for re-attachment and rebuild.
pub fn release_session_workspace(session_id: &str, repo: &Path) -> Result<(), String> {
    release_or_trash_in(repo, session_id, BranchDisposition::Keep)
}

/// Soft-delete by removing the session directory and moving the branch to
/// refs/agentloom/trash/<safe>; it remains recoverable during the grace period and retains its base ref for GC.
pub fn trash_session_workspace(session_id: &str, repo: &Path) -> Result<(), String> {
    release_or_trash_in(repo, session_id, BranchDisposition::Trash)
}

/// Undo a soft deletion by moving the trash ref to heads. The base ref was retained and needs no
/// restoration; ensure_worktree_in re-attaches and rebuilds the directory on next use.
pub fn restore_trashed_session_branch(session_id: &str, repo: &Path) -> Result<(), String> {
    assert_app_domain_path(repo, "restore_trashed_session_branch")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists(repo, &trash) {
        // M3 fail-closed (Codex and Opus review): reject existing heads so update-ref cannot
        // overwrite a live branch and lose its commits. Coexisting trash and heads is a partial or
        // abnormal state that must be reconciled separately rather than overwritten silently.
        if git_ref_exists(repo, &heads) {
            return Err(crate::ui_msg::al_err(
                "wt.restore.headsRefExists",
                &[("heads", heads.clone())],
            ));
        }
        run_git(repo, &["update-ref", &heads, &trash])?; // checked
        run_git(repo, &["update-ref", "-d", &trash])?; // checked
        return Ok(());
    }
    // Final review, important (Codex and Opus): when trash is absent, existing heads means the
    // restore already completed and is idempotently OK. If heads is also absent, all refs are gone,
    // indicating a partial purge where GC removed trash and base but the DB tombstone remains.
    // Return Err so the caller cannot clear the tombstone and revive a ref-less shell that would
    // create an empty branch from repo HEAD on the next ensure and lose code history. Reconcile it separately.
    if git_ref_exists(repo, &heads) {
        return Ok(());
    }
    Err(crate::ui_msg::al_err(
        "wt.restore.refsMissing",
        &[("session", safe)],
    ))
}

/// DB restore failed after git restore: move heads back to trash without overwriting existing trash.
pub fn move_restored_session_branch_back_to_trash(
    session_id: &str,
    repo: &Path,
) -> Result<(), String> {
    assert_app_domain_path(repo, "move_restored_session_branch_back_to_trash")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists(repo, &trash) {
        return Err(crate::ui_msg::al_err(
            "wt.restore.compensationTrashExists",
            &[("trash", trash.clone())],
        ));
    }
    if !git_ref_exists(repo, &heads) {
        return Err(crate::ui_msg::al_err(
            "wt.restore.compensationHeadsMissing",
            &[("heads", heads.clone())],
        ));
    }
    run_git(repo, &["update-ref", &trash, &heads])?;
    run_git(repo, &["update-ref", "-d", &heads])?;
    Ok(())
}

/// GC permanently deletes the trash and base refs after grace expiry or manual clearing.
/// C4/M2 fail-closed (Codex and Opus review): first require no live worktree registration. Any
/// existing heads ref, meaning a live branch, is an error because base is its diff fork point and
/// must never be deleted while heads exists. This covers archived or restored state with heads and
/// base but no trash, plus partial state with both trash and heads after failed heads deletion; both
/// require separate reconciliation. Cleanup is allowed only when heads is absent: delete trash and
/// base when trash exists, or return idempotent success when it does not. Ref deletion uses checked run_git.
pub fn gc_trashed_session_branch(session_id: &str, repo: &Path) -> Result<(), String> {
    assert_app_domain_path(repo, "gc_trashed_session_branch")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let wt = session_wt_path(repo, &safe);
    if worktree_registered(repo, &wt)? {
        return Err(crate::ui_msg::al_err(
            "wt.gc.liveWorktree",
            &[("session", safe.clone())],
        ));
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    let base = format!("refs/agentloom/base/{safe}");
    // Critical (Codex review): any existing heads ref is live and must return Err without deleting
    // base, its diff fork point. This covers archived/restored state with heads and base but no trash,
    // and partial state where trash and heads coexist; both require separate reconciliation.
    if git_ref_exists(repo, &heads) {
        return Err(crate::ui_msg::al_err(
            "wt.gc.liveHeads",
            &[("session", safe.clone())],
        ));
    }
    // With no live heads, absent trash means GC already completed; existing trash means delete trash and base.
    if !git_ref_exists(repo, &trash) {
        return Ok(());
    }
    // Confirmed trashed state: no heads and existing trash. Delete trash, then the session's diff-fork base.
    run_git(repo, &["update-ref", "-d", &trash])?; // C4: checked; propagate errors.
    if git_ref_exists(repo, &base) {
        run_git(repo, &["update-ref", "-d", &base])?;
    }
    Ok(())
}
