use super::*;

pub(super) struct ReviewInputs {
    pub(super) session_id: String,
    pub(super) workspace: SessionWorkspace,
    pub(super) inplace_project: Option<std::path::PathBuf>,
    pub(super) landing_commit_ranges: Vec<(String, String)>,
    pub(super) run_commit_ranges: Vec<(String, String)>,
    pub(super) staged_unlanded: Option<(String, String, String)>,
    pub(super) checkpoint_paths: Vec<std::path::PathBuf>,
    /// Each active checkpoint path together with the complete lifecycle of its run
    /// (state / pre_head / post_head / commit_sha). This tightens undo eligibility independently
    /// of `checkpoint_paths`, which controls which files appear in the uncommitted diff. A file
    /// can still have uncommitted changes worth showing even when its preimage is stale.
    pub(super) checkpoint_entries_with_run_lifecycle: Vec<(
        std::path::PathBuf,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )>,
}

fn prefetch_review_inputs(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<ReviewInputs, String> {
    let workspace = resolve_session_workspace(conn, session_id)?;
    let inplace_project = inplace_project_path(conn, session_id)?;
    let staged_unlanded =
        db::latest_staged_unlanded_run(conn, session_id).map_err(|e| e.to_string())?;
    let (
        landing_commit_ranges,
        run_commit_ranges,
        checkpoint_paths,
        checkpoint_entries_with_run_lifecycle,
    ) = if inplace_project.is_some() {
        (
            db::landing_commit_ranges_for_session(conn, session_id)
                .map_err(|error| error.to_string())?,
            db::recorded_run_commit_ranges_for_session(conn, session_id)
                .map_err(|error| error.to_string())?,
            db::list_checkpoint_file_paths_for_session(conn, session_id)
                .map_err(|error| error.to_string())?,
            db::list_active_checkpoint_paths_with_run_lifecycle_for_session(conn, session_id)
                .map_err(|error| error.to_string())?,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    };
    Ok(ReviewInputs {
        session_id: session_id.to_string(),
        workspace,
        inplace_project,
        landing_commit_ranges,
        run_commit_ranges,
        staged_unlanded,
        checkpoint_paths,
        checkpoint_entries_with_run_lifecycle,
    })
}

pub(super) fn compute_review(inputs: ReviewInputs) -> Result<worktree::Review, String> {
    let ReviewInputs {
        session_id,
        workspace,
        inplace_project,
        landing_commit_ranges,
        run_commit_ranges,
        staged_unlanded,
        checkpoint_paths,
        checkpoint_entries_with_run_lifecycle,
    } = inputs;
    let mut review = match &inplace_project {
        Some(project) => {
            let head =
                worktree::git_read_output(project, &["rev-parse", "--verify", "--quiet", "HEAD"])
                    .map_err(|e| {
                    ui_msg::al_err("wt.git.revParseSpawnFailed", &[("detail", e.to_string())])
                })?;
            if !head.status.success() {
                return Ok(worktree::Review::unavailable());
            }
            // Sum the attributed changes instead of diffing from one shared base: committed
            // content from each session-owned pre_i..post_i range plus current uncommitted
            // content (`git diff HEAD -- checkpoint_paths`). The old implementation computed one
            // shared base and applied a pathspec to the base-to-worktree diff. `git diff base --
            // path` compares the base and current worktree trees, so any intervening commit to the
            // same file, including commits from other sessions, leaked into Review. Diffing each
            // recorded range separately narrows that exposure from the entire base-to-worktree
            // interval to the inside of each pre_i..post_i range, without relying on base selection
            // or a pathspec as a fallback.
            //
            // Known remaining window: `pre_head` is fixed when a run starts in
            // `insert_run_pending`, while `record_run_commit` updates only `post_head`. If someone
            // else commits to the same repository after this run starts but before it commits,
            // that commit falls inside `pre_head..post_head` and
            // `landed_review(pre_head, post_head)` includes it unchanged. This differs from
            // `session_review_excludes_other_sessions_later_commit_to_same_file`, which covers a
            // commit made after this session's post_head. A commit made inside this session's
            // range remains an accepted boundary of the segmented model.
            // Exact overlap is known to occur for an in-place team run: a member works from
            // base_sha=H0, the lead commits through the delivery broker and records
            // run_commits(H0..H1), then the frontend coding loop finalizes the same changes as a
            // landing. `record_inplace_artifact_landing` uses the project HEAD at finalization as
            // landed_head and the member's spawn HEAD as pre_head, so both ledgers can contain the
            // identical (H0, H1) range. Without deduplication, `combine_reviews` appends the same
            // diff twice; stat and patch text are not content-deduplicated, and only the file list
            // is path-deduplicated, so line counts double. Handle exact duplicate endpoint strings
            // only. There is no evidence of partially overlapping ranges with different endpoints,
            // and handling those would reintroduce the complexity of the former shared-base model.
            let mut seen_ranges: std::collections::HashSet<(&str, &str)> =
                std::collections::HashSet::new();
            let mut attributed = Vec::new();
            let mut range_reviews = Vec::new();
            for (pre_head, post_head) in
                run_commit_ranges.iter().chain(landing_commit_ranges.iter())
            {
                if !seen_ranges.insert((pre_head.as_str(), post_head.as_str())) {
                    continue;
                }
                if !worktree::is_ancestor(project, pre_head, post_head)
                    || !worktree::is_ancestor(project, post_head, "HEAD")
                {
                    continue;
                }
                attributed.extend(
                    worktree::changed_paths_between_no_renames(project, pre_head, post_head)?
                        .into_iter()
                        .map(std::path::PathBuf::from),
                );
                range_reviews.push(worktree::landed_review(project, pre_head, post_head)?);
            }
            // Preserve the exact path spelling returned by Git, then add absolute checkpoint paths.
            attributed.extend(checkpoint_paths.iter().cloned());

            // The uncommitted half must use `attributed`, the union of paths touched by this
            // session's committed ranges and checkpoint paths, rather than only
            // `checkpoint_paths`. For example, if this session commits X and X is later edited in
            // the terminal without a checkpoint, `review_scoped(HEAD, checkpoint_paths)` cannot
            // see X. Since X is already attributed through the committed range,
            // `count_unattributed_dirty` also excludes it from other_dirty_count, silently hiding
            // the change. Expanding the pathspec to `attributed` restores such dirty committed
            // files. Committed files that remain clean naturally yield no diff and are not shown
            // twice.
            let committed_files_changed = worktree::count_unique_files(project, &range_reviews);
            let uncommitted = worktree::review_scoped(project, "HEAD", &attributed)?;
            let uncommitted_files_changed =
                worktree::count_unique_files(project, std::slice::from_ref(&uncommitted));
            range_reviews.push(uncommitted);

            let mut scoped = worktree::combine_reviews(project, range_reviews);
            scoped.other_dirty_count = worktree::count_unattributed_dirty(project, &attributed)?;
            scoped.committed_files_changed = committed_files_changed;
            scoped.uncommitted_files_changed = uncommitted_files_changed;
            scoped
        }
        // Only legacy sessions without a bound project continue to use app-isolated worktrees.
        None => match &workspace {
            SessionWorkspace::Local => worktree::review_workspace(&session_id, None, true)?,
            SessionWorkspace::Repo(path) => {
                worktree::review_workspace(&session_id, Some(path), false)?
            }
        },
    };
    // Review fallback when automatic landing is disabled: for a repository session with an empty
    // current Review whose changes were merged into staging but not landed, use the staged diff
    // (base_sha..merged_sha). This applies only when all three conditions hold: a repository
    // session, an empty attributed or isolated-worktree Review, and a staged unlanded run. Landed
    // runs, existing attributed changes, and local sessions keep the behavior above.
    if !review.has_changes {
        if let SessionWorkspace::Repo(path) = &workspace {
            if let Some((_run_id, base_sha, merged_sha)) = staged_unlanded {
                if let Ok(staged) = worktree::landed_review(path, &base_sha, &merged_sha) {
                    if staged.has_changes {
                        let other_dirty_count = review.other_dirty_count;
                        review = staged;
                        review.other_dirty_count = other_dirty_count;
                    }
                }
            }
        }
    }
    if let Some(project) = &inplace_project {
        // An active checkpoint alone is not enough for undo eligibility. If its run committed at
        // post_head and the file was committed again afterward by anyone, the preimage is stale.
        // `undo_run` writes preimage bytes directly to disk and would erase that later content.
        // Mark only active records that remain fresh as undoable.
        let fresh_checkpoint_paths =
            filter_fresh_checkpoint_paths(project, &checkpoint_entries_with_run_lifecycle)?;
        review.mark_undoable_paths(project, &fresh_checkpoint_paths);
    }
    Ok(review)
}

/// Filter active checkpoint paths and their complete run lifecycles down to the paths that remain
/// fresh and can safely be marked undoable. If a path has active records from multiple runs, one
/// fresh record is sufficient. A stale record must not suppress another record that is safe.
///
/// Reference branches corresponding to `RunLifecycle.state`:
/// - `active` with both post_head and commit_sha: the run committed, so use `post_head..HEAD` to
///   determine whether another commit touched the file afterward.
/// - `running`: the run is still running and has not committed. This is normal for in-place runs,
///   because only the delivery broker calls `record_run_commit`; many completed runs leave their
///   `run_commits` row in running state with a NULL `post_head`. `pre_head` exists from
///   `insert_run_pending` onward because the column is NOT NULL. Use `pre_head..HEAD` to determine
///   whether someone else committed the file afterward. Treating a missing post_head as
///   unconditionally safe would leave the most common case unprotected.
/// - All other cases, including no matching run_commits row and non-running terminal states such as
///   `failed`, `undone`, `kept`, or `discarded`: safety cannot be verified, so fail closed and do
///   not include the path as fresh or mark it undoable.
///
/// `ReviewFile.undoable` is a non-optional `bool`, and `mark_undoable_paths` explicitly assigns
/// true or false to every item in `review.files`. This only narrows the paths eligible for true; it
/// cannot turn an explicit false into a missing or undefined value.
pub(super) fn filter_fresh_checkpoint_paths(
    project: &std::path::Path,
    entries: &[(
        std::path::PathBuf,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )],
) -> Result<Vec<std::path::PathBuf>, String> {
    let mut by_reference: std::collections::HashMap<&str, Vec<&std::path::PathBuf>> =
        std::collections::HashMap::new();
    for (path, state, pre_head, post_head, commit_sha) in entries {
        let reference = match state.as_deref() {
            Some("active") => match (post_head.as_deref(), commit_sha.as_deref()) {
                (Some(post_head), Some(_)) => Some(post_head),
                // An active run without post_head or commit_sha is not corrupt data; it is a common
                // normal completion state. `finalize_run_pending_without_git_writes` changes a run
                // with checkpoints but no broker commit directly from running to active, leaving
                // post_head and commit_sha NULL throughout. The production entry point
                // `finish_run_without_git_writes` is used for main-run and lead-run completion.
                // Such a run has no post_head, but pre_head exists from `insert_run_pending`, so use
                // pre_head..HEAD just as for a running run to detect later commits to the file.
                _ => pre_head.as_deref(),
            },
            Some("running") => pre_head.as_deref(),
            // Fail closed when no run_commits row matches or the run is in a terminal state such as
            // failed, undone, kept, or discarded. An accepted conservative boundary occurs when an
            // agent bypasses the delivery broker and commits directly: its own commit after
            // pre_head is treated like an external touch, so an otherwise undoable record appears
            // stale. This only removes undo capability temporarily; the content remains in Git
            // history and no data is lost.
            _ => None,
        };
        if let Some(reference) = reference {
            by_reference.entry(reference).or_default().push(path);
        }
    }

    let mut fresh: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for (reference, paths) in by_reference {
        if !worktree::is_ancestor(project, reference, "HEAD") {
            // If the safety boundary cannot be verified, for example after history was rewritten,
            // conservatively skip the paths instead of adding them to fresh.
            continue;
        }
        // Git returns project-relative paths with their original case, while checkpoints store
        // absolute paths. Normalize both sides with the same case-aware function so comparisons do
        // not always miss or overlook a touch on a case-insensitive file system.
        let touched_after: std::collections::HashSet<String> =
            worktree::changed_paths_between_no_renames(project, reference, "HEAD")?
                .into_iter()
                .filter_map(|path| {
                    worktree::normalize_checkpoint_path_key(project, std::path::Path::new(&path))
                })
                .collect();
        for path in paths {
            let touched = worktree::normalize_checkpoint_path_key(project, path)
                .map(|key| touched_after.contains(&key))
                .unwrap_or(true); // If normalization fails, conservatively treat the path as touched.
            if !touched {
                fresh.insert(path.clone());
            }
        }
    }
    Ok(fresh.into_iter().collect())
}

#[cfg(test)]
pub(super) fn session_review_inner(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<worktree::Review, String> {
    compute_review(prefetch_review_inputs(conn, session_id)?)
}

#[tauri::command]
pub(super) async fn session_review(
    db: State<'_, Db>,
    session_id: String,
) -> Result<worktree::Review, String> {
    let inputs = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        prefetch_review_inputs(&conn, &session_id)?
    };
    tauri::async_runtime::spawn_blocking(move || compute_review(inputs))
        .await
        .map_err(|e| e.to_string())?
}
