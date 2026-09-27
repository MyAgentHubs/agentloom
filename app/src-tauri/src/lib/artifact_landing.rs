use super::*;

pub(super) fn finalize_member_artifact_inner(
    conn: &rusqlite::Connection,
    run_id: &str,
    session_id: &str,
    member_assignment_id: &str,
    base_sha: &str,
) -> Result<String, String> {
    if let Some(existing) =
        crate::db::get_artifact_by_member(conn, session_id, run_id, member_assignment_id)
            .map_err(|e| e.to_string())?
    {
        if existing.state == "ready" || existing.state == "merged" {
            return Ok(existing.id);
        }
    }

    let in_place = session_is_in_place(conn, session_id)?;
    let wt = resolve_member_wt(conn, session_id, member_assignment_id)?;
    let (commit_sha, files_changed) = if in_place {
        // In-place writes are already physically landed. Completion comes from the checkpoint
        // ledger, regardless of whether the user's worktree is clean or the agent made a commit.
        // HEAD is only optional display metadata.
        let current_head = crate::worktree::rev_parse_head(&wt).ok();
        let files_changed = list_run_undo_entries_inner(conn, session_id, run_id)?.len() as i64;
        (current_head, files_changed)
    } else {
        // The legacy app-domain scaffold still uses an isolated-branch commit as the artifact handoff contract.
        let commit_sha = crate::worktree::rev_parse_head(&wt)
            .map_err(|_| ui_msg::al_err("finalize.gitUnavailable", &[]))?;
        if crate::worktree::worktree_is_dirty(&wt) {
            return Err(ui_msg::al_err("finalize.uncommittedChanges", &[]));
        }
        if base_sha.is_empty() || commit_sha == base_sha {
            return Err(ui_msg::al_err("finalize.noChanges", &[]));
        }
        let files_changed = crate::worktree::run_numstat(&wt, base_sha, &commit_sha)?.files as i64;
        (Some(commit_sha), files_changed)
    };

    let art_id = crate::new_run_id();
    crate::db::insert_artifact(
        conn,
        &crate::db::Artifact {
            id: art_id.clone(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            member_assignment_id: member_assignment_id.into(),
            branch: current_branch(&wt),
            base_sha: base_sha.into(),
            commit_sha: None,
            files_changed: 0,
            state: "finalizing".into(),
            created_at: crate::db::now_secs(),
        },
    )
    .map_err(|e| e.to_string())?;

    if in_place {
        let landed_head = commit_sha
            .as_deref()
            .filter(|head| !head.is_empty())
            .or_else(|| (!base_sha.is_empty()).then_some(base_sha))
            .unwrap_or(&art_id);
        record_inplace_artifact_landing(
            conn,
            &art_id,
            session_id,
            run_id,
            base_sha,
            landed_head,
            commit_sha.as_deref(),
            files_changed,
        )?;
    } else {
        let commit_sha =
            commit_sha.ok_or_else(|| ui_msg::al_err("finalize.gitUnavailable", &[]))?;
        crate::db::set_artifact_state(
            conn,
            &art_id,
            "ready",
            Some(&commit_sha),
            Some(files_changed),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(art_id)
}

/// For in-place completion, record only artifact and landing metadata; the app neither checks for a dirty tree nor requires or creates a commit.
pub(super) fn record_inplace_artifact_landing(
    conn: &rusqlite::Connection,
    art_id: &str,
    session_id: &str,
    run_id: &str,
    base_sha: &str,
    landed_head: &str,
    commit_sha: Option<&str>,
    files_changed: i64,
) -> Result<(), String> {
    crate::db::set_artifact_state(conn, art_id, "merged", commit_sha, Some(files_changed))
        .map_err(|e| e.to_string())?;
    crate::db::insert_landing_commit(
        conn,
        &crate::db::LandingCommit {
            id: crate::new_run_id(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            artifact_id: Some(art_id.into()),
            pre_head: base_sha.into(),
            landed_head: landed_head.into(),
            commit_count: 0,
            files_changed,
            insertions: 0,
            deletions: 0,
            created_at: crate::db::now_secs(),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Read the worktree's current branch name so an attached member worktree yields its real `agentloom/<tag>` branch without duplicating naming rules.
pub(super) fn current_branch(wt: &std::path::Path) -> String {
    crate::worktree::git_read_output(wt, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "agentloom/unknown".into())
}

/// Delivery safety boundary: accept only a real attached branch resolved by `symbolic-ref`, never the display-only `agentloom/unknown` fallback as a push refspec.
pub(super) fn delivery_branch(wt: &std::path::Path) -> Result<String, String> {
    crate::worktree::git_read_output(wt, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        })
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| {
            "DELIVERY_DETACHED_HEAD:detached HEAD；请先 checkout 一个分支再交付".to_string()
        })
}

#[tauri::command]
pub(super) fn finalize_member_artifact(
    db: tauri::State<'_, crate::db::Db>,
    run_id: String,
    session_id: String,
    member_assignment_id: String,
    base_sha: String,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    finalize_member_artifact_inner(
        &conn,
        &run_id,
        &session_id,
        &member_assignment_id,
        &base_sha,
    )
}

/// First locked phase: read only the SHA and repository path needed by the verifier, without running slow work.
/// This is isolated for direct testing and is the only work the command performs while holding the lock.
pub(super) fn prepare_verifier_run(
    conn: &rusqlite::Connection,
    artifact_id: &str,
) -> Result<(String, std::path::PathBuf), String> {
    let art = crate::db::get_artifact(conn, artifact_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("artifact.notFound", &[("id", artifact_id.to_string())]))?;
    let sha = art
        .commit_sha
        .ok_or_else(|| ui_msg::al_err("artifact.notReadyVerify", &[]))?;
    let repo_path = resolve_repo_path_for_artifact(conn, artifact_id)?;
    crate::worktree::assert_app_domain_path(&repo_path, "run_verifier_artifact")?;
    Ok((sha, repo_path))
}

/// Second locked phase: persist the `verifications` row after the verifier finishes.
fn finalize_verifier_run(
    conn: &rusqlite::Connection,
    artifact_id: &str,
    cmd: &str,
    sha: &str,
    res: crate::worktree::VerifyResult,
) -> Result<String, String> {
    let ver_id = crate::new_run_id();
    crate::db::insert_verification(
        conn,
        &crate::db::Verification {
            id: ver_id.clone(),
            artifact_id: artifact_id.into(),
            cmd: cmd.into(),
            artifact_sha: sha.into(),
            exit_code: res.exit_code,
            output_ref: Some(res.output),
            verdict: res.verdict,
            created_at: crate::db::now_secs(),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(ver_id)
}

/// `run_verifier` may execute a user's build or test command for minutes. Read its inputs while
/// holding the database lock, release the lock for the subprocess, then reacquire it to persist
/// the result. The unlocked interval is a check-to-use boundary: concurrent deletion can leave an
/// orphaned verification row because the schema does not enforce foreign keys for these records.
/// This low-impact case is acceptable because it does not fail other writes, is not visible in the
/// UI, requires a purged session to race an active verification, and costs only one unreachable row.
/// Keep command forwarding separate from the inner implementation so the actual multi-stage
/// operation can be tested directly, including argument ordering during finalization.
pub(super) fn run_verifier_artifact_inner(
    db: &Db,
    artifact_id: &str,
    cmd: &str,
) -> Result<String, String> {
    let (sha, repo_path) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        prepare_verifier_run(&conn, artifact_id)?
    };
    let res = crate::worktree::run_verifier(&repo_path, &sha, cmd, None)?;
    // Lock-error propagation must be assessed against whether the preceding unlocked operation requires compensation.
    // This is intentionally low risk because the unlocked operation is read-only and needs no
    // compensation. If the lock is poisoned afterward, the verification result is simply not
    // persisted; the caller receives an explicit error and can retry, with no half-finished state.
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    finalize_verifier_run(&conn, artifact_id, cmd, &sha, res)
}

#[tauri::command]
pub(super) fn run_verifier_artifact(
    db: tauri::State<'_, crate::db::Db>,
    artifact_id: String,
    cmd: String,
) -> Result<String, String> {
    run_verifier_artifact_inner(&db, &artifact_id, &cmd)
}

/// Before landing, distinguish hard failures (protected paths always return `Err`) from soft warnings (missing change evidence or changes beyond the declared scope).
///
/// With `trust == true`, return soft warnings for UI review and allow landing to continue.
/// With `trust == false`, promote soft warnings to `Err` to preserve the strict-review contract.
/// Protected paths return `Err` in both modes.
///
/// `Ok(warnings)` contains the collected soft warnings when landing is allowed.
pub(super) fn preflight_artifact_landing(
    conn: &rusqlite::Connection,
    artifact_id: &str,
    trust: bool,
) -> Result<Vec<String>, String> {
    let art = crate::db::get_artifact(conn, artifact_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("artifact.notFound", &[("id", artifact_id.to_string())]))?;
    let repo_path = resolve_repo_path_for_artifact(conn, artifact_id)?;
    crate::worktree::assert_app_domain_path(&repo_path, "merge_artifact_to_staging")?;
    let commit = art
        .commit_sha
        .clone()
        .ok_or_else(|| ui_msg::al_err("artifact.noShaPreflight", &[]))?;
    let actual = crate::worktree::changed_paths_between(&repo_path, &art.base_sha, &commit)?;
    // A protected path is always a hard failure, including in trusted mode.
    let protected = crate::worktree::protected_landing_paths(&actual);
    if !protected.is_empty() {
        return Err(ui_msg::al_err(
            "landing.protectedPath",
            &[("paths", protected.join(", "))],
        ));
    }
    let mut warnings: Vec<String> = Vec::new();
    // Missing change evidence is a warning in trusted mode and an error in strict mode.
    let expected = crate::db::member_changed_paths_from_messages(
        conn,
        &art.session_id,
        &art.run_id,
        &art.member_assignment_id,
    )
    .map_err(|e| e.to_string())?;
    if expected.is_empty() {
        let msg = ui_msg::al_err("landing.noEvidence", &[]);
        if trust {
            warnings.push(msg);
        } else {
            return Err(msg);
        }
    } else {
        // Check for changes beyond the declared scope only when evidence is available.
        let expected: std::collections::BTreeSet<_> = expected.into_iter().collect();
        let unexpected: Vec<_> = actual
            .iter()
            .filter(|p| !expected.contains(p.as_str()))
            .cloned()
            .collect();
        if !unexpected.is_empty() {
            let msg = ui_msg::al_err("landing.scopeExceeded", &[("files", unexpected.join(", "))]);
            if trust {
                warnings.push(msg);
            } else {
                return Err(msg);
            }
        }
    }
    Ok(warnings)
}

/// Merge a ready artifact into the staging branch.
///
/// With `trust == true`, skip the requirement for a passed verification tied to the SHA and treat
/// preflight soft failures as warnings; protected paths remain blocked. With `trust == false`,
/// preserve the strict verification contract. Neither path writes a synthetic verification row.
pub(super) fn merge_artifact_to_staging_inner(
    conn: &rusqlite::Connection,
    artifact_id: &str,
    trust: bool,
) -> Result<String, String> {
    let art = crate::db::get_artifact(conn, artifact_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("artifact.notFound", &[("id", artifact_id.to_string())]))?;
    let repo_path = resolve_repo_path_for_artifact(conn, artifact_id)?;
    crate::worktree::assert_app_domain_path(&repo_path, "merge_artifact_to_staging")?;
    // Database idempotency: return an existing merged candidate and synchronize the artifact state
    // if a prior attempt stopped between the candidate upsert and the state update.
    if let Some(mc) =
        crate::db::get_merge_candidate_by_artifact(conn, artifact_id).map_err(|e| e.to_string())?
    {
        if mc.state == "merged" {
            if art.state != "merged" {
                crate::db::set_artifact_state(conn, artifact_id, "merged", None, None)
                    .map_err(|e| e.to_string())?;
            }
            return Ok(mc.id);
        }
    }
    if art.state != "ready" && art.state != "merged" {
        return Err(ui_msg::al_err(
            "artifact.notReadyMerge",
            &[("state", art.state.clone())],
        ));
    }
    let commit = art
        .commit_sha
        .clone()
        .ok_or_else(|| ui_msg::al_err("artifact.noShaMerge", &[]))?;
    // In strict mode, require a passed verification tied to the current artifact commit SHA.
    if !trust {
        let l1_ok = crate::db::latest_verification_for_artifact(conn, artifact_id)
            .map_err(|e| e.to_string())?
            .map(|v| v.verdict == "passed" && v.artifact_sha == commit)
            .unwrap_or(false);
        if !l1_ok {
            return Err(ui_msg::al_err("landing.l1NotGreen", &[]));
        }
    }
    // Trusted-mode preflight warnings are retained for review without blocking; protected paths
    // remain hard failures inside the preflight check.
    let _landing_warnings = preflight_artifact_landing(conn, artifact_id, trust)?;

    let staging_branch = format!("agentloom/run/{}", art.run_id);
    let mc_id = crate::new_run_id();
    let now = crate::db::now_secs();
    match crate::worktree::merge_artifact_to_staging(
        &repo_path,
        &art.run_id,
        &commit,
        &art.base_sha,
    )? {
        crate::worktree::MergeOutcome::Merged { merged_sha }
        | crate::worktree::MergeOutcome::AlreadyMerged { merged_sha } => {
            crate::db::upsert_merge_candidate(
                conn,
                &crate::db::MergeCandidate {
                    id: mc_id.clone(),
                    artifact_id: artifact_id.into(),
                    staging_branch,
                    state: "merged".into(),
                    merged_sha: Some(merged_sha),
                    created_at: now,
                },
            )
            .map_err(|e| e.to_string())?;
            crate::db::set_artifact_state(conn, artifact_id, "merged", None, None)
                .map_err(|e| e.to_string())?;
            Ok(mc_id)
        }
        crate::worktree::MergeOutcome::Conflict => {
            crate::db::upsert_merge_candidate(
                conn,
                &crate::db::MergeCandidate {
                    id: mc_id.clone(),
                    artifact_id: artifact_id.into(),
                    staging_branch,
                    state: "rejected".into(),
                    merged_sha: None,
                    created_at: now,
                },
            )
            .map_err(|e| e.to_string())?;
            Err(ui_msg::al_err("merge.stagingConflict", &[]))
        }
    }
}

/// Command wrapper for merging an artifact; the scope gate allows missing evidence in trusted mode.
#[tauri::command]
pub(super) fn merge_artifact_to_staging(
    db: tauri::State<'_, crate::db::Db>,
    artifact_id: String,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    // Automatic mode lands worker changes directly, skipping the strict verification gate and returning soft warnings.
    merge_artifact_to_staging_inner(&conn, &artifact_id, true)
}

/// Return the artifact's latest complete verification record for verdict and SHA checks.
#[tauri::command]
pub(super) fn latest_verification_for_artifact_cmd(
    db: tauri::State<'_, crate::db::Db>,
    artifact_id: String,
) -> Result<Option<crate::db::Verification>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    crate::db::latest_verification_for_artifact(&conn, &artifact_id).map_err(|e| e.to_string())
}

// Command entry point for applying a run to the current branch.
