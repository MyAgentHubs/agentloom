use super::*;

/// Coding closed loop, Blade 1 (spec §L1 line 58): the result of merging the artifact into the run staging branch.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum MergeOutcome {
    Merged { merged_sha: String }, // Merged successfully (including ff when the branch is first created)
    AlreadyMerged { merged_sha: String }, // Idempotent: the artifact commit is already in staging (crash-recover retry)
    Conflict, // Conflict with staging; already rolled back with merge --abort; reject
}

/// Parses `git worktree list --porcelain` and returns the paths of all worktrees attached to branch_ref.
/// Used for stale-recover (codex BLOCK): a staging worktree left behind by a crash during merge occupies the branch and must be cleared first.
fn worktree_paths_on_branch(base_repo: &Path, branch_ref: &str) -> Vec<PathBuf> {
    let list = git_stdout(base_repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let mut paths = Vec::new();
    let mut cur: Option<PathBuf> = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b.trim() == branch_ref {
                if let Some(p) = cur.take() {
                    paths.push(p);
                }
            }
        }
    }
    paths
}

/// Merges artifact_commit into this run's staging branch `agentloom/run/<run_id>` (honors D32; only modifies agentloom/*).
/// Creates the branch at base_sha the first time, or attaches if it already exists; idempotent (returns AlreadyMerged if already merged); aborts on conflict and returns Conflict.
#[allow(dead_code)]
pub fn merge_artifact_to_staging(
    base_repo: &Path,
    run_id: &str,
    artifact_commit: &str,
    base_sha: &str,
) -> Result<MergeOutcome, String> {
    assert_app_domain_path(base_repo, "merge_artifact_to_staging")?;
    // Base check (path one): the artifact must actually be based on base_sha (guards against passing the wrong base).
    if !git_ok(
        base_repo,
        &["merge-base", "--is-ancestor", base_sha, artifact_commit],
    ) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.artifactBaseMismatch",
            &[
                ("artifact", artifact_commit.to_string()),
                ("base", base_sha.to_string()),
            ],
        ));
    }

    let staging_branch = format!("agentloom/run/{run_id}");
    let staging_ref = format!("refs/heads/{staging_branch}");

    // stale-recover (codex BLOCK): clear any leftover worktree occupying this staging branch (left by a crash during merge).
    // Otherwise, the next attach will inevitably fail with "already used by worktree". The staging branch is exclusively owned by the app; every existing worktree is stale.
    for stale in worktree_paths_on_branch(base_repo, &staging_ref) {
        let _ = crate::proc::command("git")
            .current_dir(base_repo)
            .args(["worktree", "remove", "--force"])
            .arg(&stale)
            .output();
    }
    let _ = crate::proc::command("git")
        .current_dir(base_repo)
        .args(["worktree", "prune"])
        .output();

    let branch_exists = git_ok(
        base_repo,
        &["show-ref", "--verify", "--quiet", &staging_ref],
    );

    // Unique temporary path (prefix agentloom-merge-; does not contain "verify"; pid + nanoseconds + atomic sequence number prevent concurrent collisions).
    static MERGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "agentloom-merge-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        MERGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));

    // Create/attach the staging worktree.
    let add = if branch_exists {
        crate::proc::command("git")
            .current_dir(base_repo)
            .args(["worktree", "add"])
            .arg(&tmp)
            .arg(&staging_branch)
            .output()
    } else {
        crate::proc::command("git")
            .current_dir(base_repo)
            .args(["worktree", "add", "-b", &staging_branch])
            .arg(&tmp)
            .arg(base_sha)
            .output()
    }
    .map_err(|e| {
        crate::ui_msg::al_err(
            "wt.scaffold.worktreeAddSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !add.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.stagingWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&add.stderr).to_string())],
        ));
    }
    // Reuse run_verifier's temporary-worktree RAII (remove worktree + prune + remove_dir_all; do not delete the branch; appropriate for merging staging).
    let _guard = TempVerifyWorktree {
        base_repo,
        path: tmp.clone(),
    };

    // Base check (path two; codex P2): an existing staging branch must also be based on the same base_sha (guards against staging for the same run_id coming from a different base).
    if !git_ok(&tmp, &["merge-base", "--is-ancestor", base_sha, "HEAD"]) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.stagingBaseMismatch",
            &[
                ("staging", staging_branch.clone()),
                ("base", base_sha.to_string()),
            ],
        ));
    }

    // Idempotence: artifact_commit is already an ancestor of staging HEAD → already merged (crash-recover retries take this path).
    if git_ok(
        &tmp,
        &["merge-base", "--is-ancestor", artifact_commit, "HEAD"],
    ) {
        let merged_sha = rev_parse_head(&tmp)?;
        return Ok(MergeOutcome::AlreadyMerged { merged_sha });
    }

    // merge: disable hooks (prevents a user repo hook from making a clean merge fail spuriously; codex P1) + machine identity.
    let merged = run_git(
        &tmp,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=agentloom@local",
            "-c",
            "user.name=AgentLoom",
            "merge",
            "--no-edit",
            artifact_commit,
        ],
    );
    if let Err(e) = merged {
        // Return Conflict only for a real conflict (an unmerged entry); otherwise propagate the non-conflict git failure as Err (do not incorrectly classify it as rejected; codex P1).
        let unmerged = git_stdout(&tmp, &["ls-files", "-u"]).unwrap_or_default();
        let _ = run_git(&tmp, &["merge", "--abort"]);
        if unmerged.trim().is_empty() {
            return Err(format!("merge 失败（非冲突）：{e}"));
        }
        return Ok(MergeOutcome::Conflict);
    }
    let merged_sha = rev_parse_head(&tmp)?;
    Ok(MergeOutcome::Merged { merged_sha })
}

/// Blade 1 Stage ① (design draft §4.1 / D10): ff-merge the member branch into the session branch head—**inside the session worktree**.
/// 🔴 It is strictly forbidden to merge from the base repo (user repo) cwd (this would silently ff the user's main and violate D26/D32): assert fail-closed before merge that
///   ① session_wt is in the app domain (under ~/.agentloom); ② session_wt's HEAD ∈ refs/heads/agentloom/*; reject detached (Blade 1 mode A; the session wt is always attached).
/// Sequential dispatch + the session integration lock naturally yields linear ff; idempotent (member is already an ancestor of session head → AlreadyMerged).
/// **Do not reuse merge_artifact_to_staging** (that merges agentloom/run/<run_id>, which the session wt cannot read).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SessionMergeOutcome {
    Merged { session_head: String },
    AlreadyMerged { session_head: String },
    NotFastForward,
}

/// The symbolic reference name of HEAD (for example, refs/heads/agentloom/s); detached HEAD → None.
#[allow(dead_code)]
pub(super) fn git_symbolic_head(wt: &Path) -> Option<String> {
    let out = git_read_output(wt, &["symbolic-ref", "--quiet", "HEAD"]).ok()?;
    if !out.status.success() {
        return None;
    } // detached
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Session integration lock: an in-process mutex keyed by the normalized session_wt path (sequential dispatch is already linear; this is a safety belt against concurrent Stage ① operations).
#[allow(dead_code)]
pub(super) fn session_integration_guard(session_wt: &Path) -> std::sync::MutexGuard<'static, ()> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, &'static Mutex<()>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = std::fs::canonicalize(session_wt).unwrap_or_else(|_| session_wt.to_path_buf());
    let m: &'static Mutex<()> = {
        let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(key)
            .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
    };
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[allow(dead_code)]
pub fn merge_artifact_to_session_head(
    session_wt: &Path,
    member_branch: &str,
) -> Result<SessionMergeOutcome, String> {
    let _guard = session_integration_guard(session_wt); // Session integration lock

    // 🔴 Fail-closed assertion ①: session_wt must be in the app domain.
    if !is_app_domain_path(session_wt) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.outsideAppDomain",
            &[("path", session_wt.display().to_string())],
        ));
    }
    // 🔴 Fail-closed assertion ②: HEAD must be attached and ∈ refs/heads/agentloom/*; reject detached (Blade 1 mode A; the session wt is always attached; for mode B detached, Blade 2/Blade 5 will separately implement atomic ref updates).
    match git_symbolic_head(session_wt) {
        Some(r) if r.starts_with("refs/heads/agentloom/") => {}
        _ => return Err(crate::ui_msg::al_err("wt.sessionMerge.invalidHead", &[])),
    }

    let member_ref = if member_branch.starts_with("refs/") {
        member_branch.to_string()
    } else {
        format!("refs/heads/{member_branch}")
    };
    if !git_ref_exists(session_wt, &member_ref) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.memberMissing",
            &[("member", member_ref.clone())],
        ));
    }
    // The session wt should be clean while idle (the agent does not run in the session wt); dirty → fail-closed.
    if !git_stdout(session_wt, &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        return Err(crate::ui_msg::al_err("wt.sessionMerge.dirtyWorktree", &[]));
    }
    // Idempotence: member is already an ancestor of session HEAD → already merged (crash-recover retries take this path).
    if git_ok(
        session_wt,
        &["merge-base", "--is-ancestor", &member_ref, "HEAD"],
    ) {
        return Ok(SessionMergeOutcome::AlreadyMerged {
            session_head: rev_parse_head(session_wt)?,
        });
    }
    // ff-only merge: disable hooks (prevents spurious failures caused by a user repo hook) + machine identity.
    let merged = run_git(
        session_wt,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=agentloom@local",
            "-c",
            "user.name=AgentLoom",
            "merge",
            "--ff-only",
            &member_ref,
        ],
    );
    if merged.is_err() {
        // A clean --ff-only failure because the merge is not ff (no worktree changes) → NotFastForward (stale-base; Blade 1 does not resolve it; report it).
        let unmerged = git_stdout(session_wt, &["ls-files", "-u"]).unwrap_or_default();
        if !unmerged.trim().is_empty() {
            let _ = run_git(session_wt, &["merge", "--abort"]); // Defensive fallback (ff-only should theoretically leave nothing unmerged)
        }
        return Ok(SessionMergeOutcome::NotFastForward);
    }
    Ok(SessionMergeOutcome::Merged {
        session_head: rev_parse_head(session_wt)?,
    })
}

/// Before deleting/archiving/trashing a session, only relay member branches that the agent itself has already committed.
/// The app no longer automatically commits a dirty worktree; when uncommitted changes are found, fail-closed and preserve the scene for the user to handle.
/// Called only for Repo sessions (Local shares the project in place and has no member worktree model).
#[allow(dead_code)]
pub fn finalize_session_before_cleanup(session_id: &str, repo: &Path) -> Result<(), String> {
    assert_app_domain_path(repo, "finalize_session_before_cleanup")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let session_wt = default_root().join(&repo_name).join(&safe);
    let members_dir = default_root()
        .join(&repo_name)
        .join(format!("{safe}__members"));

    // 🔴 C1 fail-closed: enumerate member branches (checked; git failure → Err; do not treat it as having no members).
    let listing = git_checked_stdout(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/heads/agentloom/{safe}-m-*"),
        ],
    )?;
    let member_refs: Vec<String> = listing
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // The session wt has been released: no members pending merge → all state is on the session branch (Ok); members still pending merge → fail-closed Err.
    if !session_wt.exists() {
        if member_refs.is_empty() {
            return Ok(());
        }
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.sessionWorktreeReleased",
            &[("pending", member_refs.len().to_string())],
        ));
    }

    // 🔴 Critical fix (caught by dual review; detached-member work loss): locate the member worktree by its **deterministic path**
    // (member ref = agentloom/<safe>-m-<assignment> → wt = <members_dir>/<assignment>; same as ensure/cleanup_member_workspace).
    // Do not build the mapping from `branch` lines in `worktree list`: a detached worktree has no `branch` line, which would miss dirty work.
    let member_prefix = format!("refs/heads/agentloom/{safe}-m-");
    for member_ref in &member_refs {
        let assignment = match member_ref.strip_prefix(&member_prefix) {
            Some(a) if !a.is_empty() => a,
            _ => {
                return Err(crate::ui_msg::al_err(
                    "wt.cleanup.invalidMemberRef",
                    &[("member", member_ref.clone())],
                ))
            }
        };
        let mwt = members_dir.join(assignment);
        // A member worktree exists on disk → it must be attached to the exact member_ref and clean before it can be relayed;
        // the app never commits uncommitted changes on behalf of the user/agent.
        if mwt.exists() {
            match git_symbolic_head(&mwt) {
                Some(h) if h == *member_ref => {
                    if worktree_is_dirty(&mwt) {
                        return Err(crate::ui_msg::al_err(
                            "wt.cleanup.uncommittedMemberChanges",
                            &[("path", mwt.display().to_string())],
                        ));
                    }
                }
                _ => {
                    return Err(crate::ui_msg::al_err(
                        "wt.cleanup.memberWorktreeDetached",
                        &[
                            ("path", mwt.display().to_string()),
                            ("member", member_ref.clone()),
                        ],
                    ));
                }
            }
        }
        // ff into the session head (idempotent; fail-closed)
        match merge_artifact_to_session_head(&session_wt, member_ref)? {
            SessionMergeOutcome::Merged { .. } | SessionMergeOutcome::AlreadyMerged { .. } => {}
            SessionMergeOutcome::NotFastForward => {
                return Err(crate::ui_msg::al_err(
                    "wt.cleanup.notFastForward",
                    &[("member", member_ref.clone())],
                ));
            }
        }
        // Safely merged (the worktree is clean or did not exist in the first place) → clean up this member.
        cleanup_member_workspace(session_id, assignment, Some(repo), false)?;
    }
    if worktree_is_dirty(&session_wt) {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.uncommittedSessionChanges",
            &[("path", session_wt.display().to_string())],
        ));
    }
    Ok(())
}
