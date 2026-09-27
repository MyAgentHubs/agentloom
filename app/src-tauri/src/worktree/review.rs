use std::path::{Path, PathBuf};

use super::{
    assert_app_domain_path, filesystem_is_case_insensitive, git_checked_stdout, git_ok,
    git_read_output, normalize_project_relative_path, nul_paths, rev_parse_head, safe_id, Review,
    ReviewFile,
};

const REVIEW_PATHSPEC_BUDGET_BYTES: usize = 128 * 1024;

pub(super) fn review_pathspec_batches<'a>(
    pathspecs: &'a [String],
    budget_bytes: usize,
) -> Vec<&'a [String]> {
    let mut batches = Vec::new();
    let mut batch_start = 0;
    let mut batch_bytes = 0_usize;

    for (index, pathspec) in pathspecs.iter().enumerate() {
        let pathspec_bytes = pathspec.len().saturating_add(1);
        if index > batch_start && batch_bytes.saturating_add(pathspec_bytes) > budget_bytes {
            batches.push(&pathspecs[batch_start..index]);
            batch_start = index;
            batch_bytes = 0;
        }
        // A single over-budget pathspec still occupies its own batch; the next entry starts a new batch.
        batch_bytes = batch_bytes.saturating_add(pathspec_bytes);
    }
    if batch_start < pathspecs.len() {
        batches.push(&pathspecs[batch_start..]);
    }
    batches
}

pub(super) fn attributed_pathspecs(project: &Path, attributed: &[PathBuf]) -> Vec<String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut seen = std::collections::HashSet::new();
    attributed
        .iter()
        // Pathspecs must retain their original ledger casing; lowercase normalization is only for set keys.
        .filter_map(|path| normalize_project_relative_path(project, path, false))
        .filter(|path| !path.is_empty())
        .filter(|path| {
            let key = if case_insensitive {
                path.to_ascii_lowercase()
            } else {
                path.clone()
            };
            seen.insert(key)
        })
        .collect()
}

pub(super) fn attributed_path_keys(
    project: &Path,
    attributed: &[PathBuf],
) -> std::collections::HashSet<String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    attributed_pathspecs(project, attributed)
        .into_iter()
        .filter_map(|path| {
            normalize_project_relative_path(project, Path::new(&path), case_insensitive)
        })
        .collect()
}

/// Parse `status --porcelain=v1 -z`. The second rename/copy field is the old name; consume it without creating another entry.
pub(super) fn porcelain_v1_z_entries(output: &str) -> Vec<(String, String)> {
    let mut fields = output.split('\0');
    let mut entries = Vec::new();
    while let Some(record) = fields.next() {
        if record.is_empty() || record.len() < 3 {
            continue;
        }
        let status = record[..2].to_string();
        let path = record[3..].to_string();
        let is_rename_or_copy = status
            .as_bytes()
            .iter()
            .any(|code| matches!(code, b'R' | b'C'));
        if is_rename_or_copy {
            let _old_path = fields.next();
        }
        entries.push((status, path));
    }
    entries
}

pub(super) fn append_no_index_patch(
    project: &Path,
    path: &str,
    patch: &mut String,
) -> Result<bool, String> {
    let args = [
        "--literal-pathspecs",
        "-c",
        "core.quotepath=false",
        "diff",
        "--no-index",
        "--no-color",
        "--",
        "/dev/null",
        path,
    ];
    let output = git_read_output(project, &args).map_err(|error| {
        crate::ui_msg::al_err(
            "wt.git.spawnFailed",
            &[("cmd", format!("{args:?}")), ("detail", error.to_string())],
        )
    })?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("{args:?}")),
                (
                    "stderr",
                    String::from_utf8_lossy(&output.stderr).to_string(),
                ),
            ],
        ));
    }
    if output.status.code() == Some(1) && output.stdout.is_empty() && !output.stderr.is_empty() {
        return Ok(false);
    }
    patch.push_str(&String::from_utf8_lossy(&output.stdout));
    Ok(true)
}

pub(super) fn append_untracked_review_files(
    project: &Path,
    paths: &[String],
    case_insensitive: bool,
    patch: &mut String,
    files: &mut Vec<ReviewFile>,
    file_keys: &mut std::collections::HashSet<String>,
) -> Result<u64, String> {
    let mut files_changed = 0_u64;
    for path in paths {
        if !append_no_index_patch(project, path, patch)? {
            continue;
        }
        files_changed += 1;
        let key = if case_insensitive {
            path.to_ascii_lowercase()
        } else {
            path.clone()
        };
        if file_keys.insert(key) {
            files.push(ReviewFile {
                path: path.clone(),
                undoable: false,
            });
        }
    }
    Ok(files_changed)
}

/// Attribution-scoped, read-only review from `base` (commit-ish) to the current worktree,
/// containing only files covered by `attributed`.
pub(crate) fn review_scoped(
    project: &Path,
    base: &str,
    attributed: &[std::path::PathBuf],
) -> Result<Review, String> {
    review_scoped_with_budget(project, base, attributed, REVIEW_PATHSPEC_BUDGET_BYTES)
}

pub(super) fn review_scoped_with_budget(
    project: &Path,
    base: &str,
    attributed: &[std::path::PathBuf],
    budget_bytes: usize,
) -> Result<Review, String> {
    if attributed.is_empty() {
        return Ok(Review::empty());
    }
    let pathspecs = attributed_pathspecs(project, attributed);
    if pathspecs.is_empty() {
        return Ok(Review::empty());
    }

    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut stat = String::new();
    let mut patch = String::new();
    let mut tracked_files_changed = 0_u64;
    let mut files = Vec::new();
    let mut file_keys = std::collections::HashSet::new();
    let mut untracked_paths = Vec::new();
    let mut untracked_keys = std::collections::HashSet::new();

    // Only extreme sets are batched. Across batches, renames degrade to delete+add and overlapping
    // pathspecs may be counted more than once.
    for batch in review_pathspec_batches(&pathspecs, budget_bytes) {
        let mut stat_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "diff",
            "--stat",
            base,
            "--",
        ];
        stat_args.extend(batch.iter().map(String::as_str));
        stat.push_str(&git_checked_stdout(project, &stat_args)?);

        let mut patch_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=never",
            "diff",
            base,
            "--",
        ];
        patch_args.extend(batch.iter().map(String::as_str));
        patch.push_str(&git_checked_stdout(project, &patch_args)?);

        let mut numstat_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "diff",
            "--numstat",
            base,
            "--",
        ];
        numstat_args.extend(batch.iter().map(String::as_str));
        let numstat = git_checked_stdout(project, &numstat_args)?;
        tracked_files_changed += numstat
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u64;

        let mut names_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-only",
            "-z",
            base,
            "--",
        ];
        names_args.extend(batch.iter().map(String::as_str));
        for path in nul_paths(&git_checked_stdout(project, &names_args)?) {
            let key = if case_insensitive {
                path.to_ascii_lowercase()
            } else {
                path.clone()
            };
            if file_keys.insert(key) {
                files.push(ReviewFile {
                    path,
                    undoable: false,
                });
            }
        }

        let mut status_args = vec![
            "--literal-pathspecs",
            "status",
            "--porcelain=v1",
            "-uall",
            "-z",
            "--",
        ];
        status_args.extend(batch.iter().map(String::as_str));
        for (status, path) in porcelain_v1_z_entries(&git_checked_stdout(project, &status_args)?) {
            if status != "??" {
                continue;
            }
            let key = if case_insensitive {
                path.to_ascii_lowercase()
            } else {
                path.clone()
            };
            if untracked_keys.insert(key) {
                untracked_paths.push(path);
            }
        }
    }

    let untracked_files_changed = append_untracked_review_files(
        project,
        &untracked_paths,
        case_insensitive,
        &mut patch,
        &mut files,
        &mut file_keys,
    )?;

    Ok(Review {
        has_changes: !patch.trim().is_empty(),
        stat,
        patch,
        files_changed: tracked_files_changed + untracked_files_changed,
        files,
        other_dirty_count: 0,
        diff_available: true,
        // `review_scoped` inherently compares `base` with the current worktree. The caller decides
        // whether `base` is HEAD (commit 1 uses it specifically for the uncommitted half), so the
        // default here consistently follows uncommitted semantics.
        committed_files_changed: 0,
        uncommitted_files_changed: tracked_files_changed + untracked_files_changed,
    })
}

/// Attribution sum (the correct approach): combine independently computed Review segments (each
/// run/landing's own `pre..post` range diff plus the current uncommitted diff) into the final view.
/// Each input is already a correct, isolated diff. This function only concatenates stat/patch data
/// and deduplicates by path; it never queries Git again against a shared base. The old implementation
/// leaked intervening commits because `git diff base -- path` compares base with the current state and
/// cannot identify who made intermediate commits. When the same file changes in multiple segments,
/// `files` retains the first entry only. `stat` and `patch` are concatenated unchanged and may contain
/// the same path multiple times; the frontend's `parseUnifiedDiff` merges those segments into one card.
///
/// Known F6 cost, identified by adversarial review and documented without optimization: segmented
/// summation changes Review from one Git invocation into one `landed_review` per valid range (four
/// subprocesses for stat/patch/numstat/name-only) plus one `review_scoped`. The segment count grows
/// linearly with recorded runs/landings, so a long 50-round session may spawn over 200 subprocesses;
/// concatenated patch size is also unbounded. If this path becomes hot, cache results by
/// `(HEAD, attribution-set fingerprint)` or impose soft limits on range count or patch size instead
/// of trying to speed up each individual diff.
pub(crate) fn combine_reviews(project: &Path, reviews: Vec<Review>) -> Review {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut stat = String::new();
    let mut patch = String::new();
    let mut files: Vec<ReviewFile> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for review in reviews {
        stat.push_str(&review.stat);
        patch.push_str(&review.patch);
        for file in review.files {
            let key = if case_insensitive {
                file.path.to_ascii_lowercase()
            } else {
                file.path.clone()
            };
            if seen.insert(key) {
                files.push(file);
            }
        }
    }
    Review {
        has_changes: !patch.trim().is_empty(),
        stat,
        patch,
        files_changed: files.len() as u64,
        files,
        other_dirty_count: 0,
        diff_available: true,
        // The caller (`compute_review`) explicitly overwrites these fields after combining. Segmented
        // summation computes committed and uncommitted portions separately; this merge stage does not
        // distinguish them, so zero is a placeholder.
        committed_files_changed: 0,
        uncommitted_files_changed: 0,
    }
}

/// Count unique files in precomputed Review segments for the attribution sum's committed/uncommitted
/// status summary. Reads only the existing `files` and does not query Git again.
pub(crate) fn count_unique_files(project: &Path, reviews: &[Review]) -> u64 {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut seen = std::collections::HashSet::new();
    for review in reviews {
        for file in &review.files {
            let key = if case_insensitive {
                file.path.to_ascii_lowercase()
            } else {
                file.path.clone()
            };
            seen.insert(key);
        }
    }
    seen.len() as u64
}

/// Count dirty worktree files not in `attributed`, including untracked but excluding ignored files.
pub(crate) fn count_unattributed_dirty(
    project: &Path,
    attributed: &[std::path::PathBuf],
) -> Result<u64, String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let attributed_keys = attributed_path_keys(project, attributed);
    let status = git_checked_stdout(project, &["status", "--porcelain=v1", "-uall", "-z"])?;
    Ok(porcelain_v1_z_entries(&status)
        .into_iter()
        .filter(|(_, path)| {
            normalize_project_relative_path(project, Path::new(path), case_insensitive)
                .is_none_or(|key| !attributed_keys.contains(&key))
        })
        .count() as u64)
}

/// Read-only composition of tracked and untracked changes in the specified worktree relative to `base`.
/// Uses only diff and ls-files; it does not modify the index, HEAD, refs, or worktree registration.
pub(super) fn review_working_tree_at(wt: &Path, base: &str) -> Result<Review, String> {
    review_working_tree_at_with_no_index(wt, base, append_no_index_patch)
}

pub(super) fn review_working_tree_at_with_no_index(
    wt: &Path,
    base: &str,
    mut append_untracked_patch: impl FnMut(&Path, &str, &mut String) -> Result<bool, String>,
) -> Result<Review, String> {
    let stat = git_checked_stdout(wt, &["-c", "core.quotepath=false", "diff", "--stat", base])?;
    let mut patch = git_checked_stdout(
        wt,
        &[
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=never",
            "diff",
            base,
        ],
    )?;

    let tracked_names = git_checked_stdout(
        wt,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-only",
            "-z",
            base,
        ],
    )?;
    let others = git_checked_stdout(
        wt,
        &[
            "-c",
            "core.quotepath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let tracked_paths = nul_paths(&tracked_names);
    let untracked_paths = nul_paths(&others);

    // Compose each untracked file as a new-file diff with `--no-index`; paths that disappear or
    // become unreadable after scanning are omitted.
    let mut readable_untracked_paths = Vec::new();
    for file in untracked_paths {
        if append_untracked_patch(wt, &file, &mut patch)? {
            readable_untracked_paths.push(file);
        }
    }

    // Retain the numstat-based structured count; `Review.files` separately uses name-only for
    // per-file capability metadata.
    let tracked_numstat = git_checked_stdout(
        wt,
        &["-c", "core.quotepath=false", "diff", "--numstat", base],
    )?;
    let tracked_files = tracked_numstat
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64;
    let files_changed = tracked_files + readable_untracked_paths.len() as u64;
    let files = tracked_paths
        .into_iter()
        .chain(readable_untracked_paths)
        .map(|path| ReviewFile {
            path,
            undoable: false,
        })
        .collect();
    Ok(Review {
        has_changes: !patch.trim().is_empty(),
        stat,
        patch,
        files_changed,
        files,
        other_dirty_count: 0,
        diff_available: true,
        // Legacy isolated worktrees (before in-place operation) do not track Git commits and
        // therefore naturally have only uncommitted semantics.
        committed_files_changed: 0,
        uncommitted_files_changed: files_changed,
    })
}

pub(super) fn review_in(root: &Path, repo: &Path, session_id: &str) -> Result<Review, String> {
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(Review::empty());
    }
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let wt = root.join(&repo_name).join(&safe);
    if !wt.exists() {
        return Ok(Review::empty());
    }
    let base_ref = format!("refs/agentloom/base/{safe}");
    let base_ok = git_read_output(&wt, &["rev-parse", "--verify", "--quiet", &base_ref])
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !base_ok {
        return Ok(Review::empty());
    }
    review_working_tree_at(&wt, &base_ref)
}

pub(crate) fn apply_staging_ff_only(repo: &Path, run_id: &str) -> Result<String, String> {
    assert_app_domain_path(repo, "apply_staging_ff_only")?;
    // Qualify with `refs/heads` to avoid ambiguity from slashes or duplicate names in `run_id`.
    let staging = format!("refs/heads/agentloom/run/{run_id}");
    if !git_ok(repo, &["rev-parse", "--verify", "--quiet", &staging]) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.stagingBranchMissing",
            &[("staging", staging)],
        ));
    }
    // Guard against detached HEAD, where fast-forwarding would update the detached head rather than the user's branch.
    let on_branch =
        git_read_output(repo, &["symbolic-ref", "-q", "HEAD"]).map_err(|e| e.to_string())?;
    if !on_branch.status.success() {
        return Err(crate::ui_msg::al_err("apply.repoDetached", &[]));
    }
    // Require a clean worktree so D32 never swallows the user's uncommitted changes.
    let dirty = git_read_output(repo, &["status", "--porcelain"]).map_err(|e| e.to_string())?;
    if !dirty.stdout.is_empty() {
        return Err(crate::ui_msg::al_err("apply.repoDirty", &[]));
    }

    let staged_sha = git_checked_stdout(repo, &["rev-parse", &staging])?
        .trim()
        .to_string();
    let head_now = rev_parse_head(repo)?;
    if head_now != staged_sha && git_ok(repo, &["merge-base", "--is-ancestor", &staging, "HEAD"]) {
        return Err(crate::ui_msg::al_err("apply.branchAdvanced", &[]));
    }

    // Fast-forward only. If the current branch advanced, Git reports it and we return an explicit error.
    let out = crate::proc::command("git")
        .current_dir(repo)
        .args(["merge", "--ff-only", &staging])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "apply.fastForwardFailed",
            &[(
                "detail",
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )],
        ));
    }
    rev_parse_head(repo)
}

/// D32 cleanup: after landing, best-effort delete this run's `agentloom/run/<run_id>` staging branch;
/// absence is a no-op. After the fast-forward merge its commits are reachable from the user's branch,
/// so deletion is lossless. Undo uses the database's pre_head/landed_head and does not depend on this branch.
pub(crate) fn delete_staging_branch(repo: &Path, run_id: &str) -> Result<(), String> {
    assert_app_domain_path(repo, "delete_staging_branch")?;
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["branch", "-D", &format!("agentloom/run/{run_id}")])
        .output();
    Ok(())
}
