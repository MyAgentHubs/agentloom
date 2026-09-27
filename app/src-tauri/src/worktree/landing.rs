use std::path::Path;

use super::{
    git_checked_stdout, git_ok, git_read_output, git_stdout, nul_paths, Review, ReviewFile,
};

/// T5a: deterministically derive hard fields from the member worktree's uncommitted diff; T5b then connects the reader's final state.
// Wire up T5b.
#[allow(dead_code)]
pub fn synthesize_hard_fields(
    worktree: &std::path::Path,
    base_sha: &str,
) -> (
    Vec<crate::agent_event::ChangedFile>,
    crate::agent_event::ResultAnchor,
) {
    let mut files = Vec::new();

    let tracked = git_stdout(
        worktree,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--numstat",
            base_sha,
            "--",
        ],
    )
    .unwrap_or_default();
    for line in tracked.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let insertions = parts.next().unwrap_or("-").parse::<u64>().unwrap_or(0);
        let deletions = parts.next().unwrap_or("-").parse::<u64>().unwrap_or(0);
        let path = parts.next().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        files.push(crate::agent_event::ChangedFile {
            path,
            insertions,
            deletions,
        });
    }

    let others =
        git_stdout(worktree, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
    for f in others.lines().filter(|l| !l.is_empty()) {
        let out = git_read_output(
            worktree,
            &[
                "-c",
                "core.quotepath=false",
                "diff",
                "--no-index",
                "--numstat",
                "--",
                "/dev/null",
                f,
            ],
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
        // --no-index exits with code 1 when differences exist; take only stdout and ignore the status.
        let insertions = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                line.split('\t')
                    .next()
                    .unwrap_or("-")
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .sum();
        files.push(crate::agent_event::ChangedFile {
            path: f.to_string(),
            insertions,
            deletions: 0,
        });
    }

    let anchor = crate::agent_event::ResultAnchor {
        base_sha: base_sha.to_string(),
        head_sha: None,
        diff_ref: None,
        generated_from: "worktree_diff".to_string(),
    };
    (files, anchor)
}

/// Count diff changes structurally; binary files contribute to the file count but not line counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumstatCount {
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// Compute counts structurally from `git diff --numstat <from>..<to>`.
/// Each line is `<ins>\t<del>\t<path>`; binary is `-\t-\t<path>` (count 0 lines and 1 file).
pub fn run_numstat(dir: &Path, from: &str, to: &str) -> Result<NumstatCount, String> {
    let range = format!("{from}..{to}");
    let out = git_stdout(dir, &["diff", "--numstat", &range])?;
    let mut count = NumstatCount {
        files: 0,
        insertions: 0,
        deletions: 0,
    };
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("-");
        let del = parts.next().unwrap_or("-");
        // The third field is the path (a rename looks like a => b; the entire line counts as 1 file).
        let _path = parts.next().unwrap_or("");
        count.files += 1;
        count.insertions += ins.parse::<u64>().unwrap_or(0);
        count.deletions += del.parse::<u64>().unwrap_or(0);
    }
    Ok(count)
}

pub(crate) struct LandingStats {
    pub commit_count: i64,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
}

/// Return (path, insertions, deletions) per file in `<from>..<to>` for structured diff reporting.
/// A binary line `-\t-\t<path>` counts 0 lines but retains the file entry. A rename `a => b` counts as one entry; path takes the entire field.
/// Used by run_landing_info to list changed files for Review (Local reads the project directory).
pub(crate) fn numstat_files_between(
    repo: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<(String, i64, i64)>, String> {
    let range = format!("{from}..{to}");
    let out = git_checked_stdout(
        repo,
        &["-c", "core.quotepath=false", "diff", "--numstat", &range],
    )?;
    let mut files = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("-");
        let del = parts.next().unwrap_or("-");
        let path = match parts.next() {
            Some(p) if !p.trim().is_empty() => p.trim().replace('\\', "/"),
            _ => continue,
        };
        files.push((
            path,
            ins.parse::<i64>().unwrap_or(0),
            del.parse::<i64>().unwrap_or(0),
        ));
    }
    Ok(files)
}

pub(crate) fn landing_stats(repo: &Path, pre: &str, post: &str) -> Result<LandingStats, String> {
    let range = format!("{pre}..{post}");
    let count = git_checked_stdout(repo, &["rev-list", "--count", &range])?
        .trim()
        .parse::<i64>()
        .unwrap_or(0);
    let numstat = git_checked_stdout(repo, &["diff", "--numstat", &range])?;
    let mut files = 0_i64;
    let mut insertions = 0_i64;
    let mut deletions = 0_i64;
    for line in numstat.lines() {
        let mut parts = line.split('\t');
        let ins = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
        let del = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
        if parts.next().is_some() {
            files += 1;
            insertions += ins;
            deletions += del;
        }
    }
    Ok(LandingStats {
        commit_count: count,
        files_changed: files,
        insertions,
        deletions,
    })
}

pub(crate) fn is_ancestor(repo: &Path, base: &str, tip: &str) -> bool {
    !base.is_empty() && !tip.is_empty() && git_ok(repo, &["merge-base", "--is-ancestor", base, tip])
}

pub(crate) fn changed_paths_between(
    repo: &std::path::Path,
    base: &str,
    tip: &str,
) -> Result<Vec<String>, String> {
    let range = format!("{base}..{tip}");
    let out = git_checked_stdout(repo, &["diff", "--name-only", &range])?;
    let mut paths: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.replace('\\', "/"))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Same as `changed_paths_between`, but uses `--no-renames` to expand both sides of a rename (old name + new name),
/// for use by the Review attribution set: only after both sides enter the pathspec can `review_scoped` reconstruct a single rename.
pub(crate) fn changed_paths_between_no_renames(
    repo: &std::path::Path,
    base: &str,
    tip: &str,
) -> Result<Vec<String>, String> {
    let range = format!("{base}..{tip}");
    let out = git_checked_stdout(repo, &["diff", "--no-renames", "--name-only", "-z", &range])?;
    let mut paths = nul_paths(&out);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub(crate) fn protected_landing_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| {
            let p = p.as_str();
            p.starts_with(".github/workflows/")
                || p == ".env"
                || p.starts_with(".env.")
                || p.ends_with("/.env")
                || p.contains("/.env.")
        })
        .cloned()
        .collect()
}

pub(crate) fn artifact_diff_text(repo: &Path, base: &str, head: &str) -> Result<String, String> {
    git_stdout(repo, &["-c", "core.quotepath=false", "diff", base, head])
}

/// Review integration: render an already-landed range `pre..landed` (in the repo directory; Local = project directory) as a Review.
/// Used for Local in-place sessions in the "already committed; clean worktree" scenario—the working-tree diff is empty, but the changes have landed;
/// the Review tab cannot be empty. stat/patch uses `pre..landed`; files_changed uses the numstat line count (structured; does not parse stat).
pub(crate) fn landed_review(repo: &Path, pre: &str, landed: &str) -> Result<Review, String> {
    let range = format!("{pre}..{landed}");
    let stat = git_checked_stdout(
        repo,
        &["-c", "core.quotepath=false", "diff", "--stat", &range],
    )?;
    let patch = git_checked_stdout(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=never",
            "diff",
            &range,
        ],
    )?;
    let names = git_checked_stdout(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-only",
            "-z",
            &range,
        ],
    )?;
    let files_changed = numstat_files_between(repo, pre, landed)?.len() as u64;
    let has_changes = !patch.trim().is_empty();
    Ok(Review {
        has_changes,
        stat,
        patch,
        files_changed,
        files: nul_paths(&names)
            .into_iter()
            .map(|path| ReviewFile {
                path,
                undoable: false,
            })
            .collect(),
        other_dirty_count: 0,
        diff_available: true,
        // landed_review describes a range diff that has already landed in git history, so it is inherently "committed" content;
        // when the caller (compute_review) needs the "uncommitted" half, it calculates it separately and explicitly overrides these two fields.
        committed_files_changed: files_changed,
        uncommitted_files_changed: 0,
    })
}

/// Cut one, Stage ①: whether the member worktree has uncommitted changes (is dirty).
/// Fail closed: treat a git status failure as dirty too (unsafe → do not merge; prevent silently losing changes, G1).
pub(crate) fn worktree_is_dirty(wt: &Path) -> bool {
    match git_checked_stdout(wt, &["status", "--porcelain"]) {
        Ok(s) => !s.trim().is_empty(),
        Err(_) => true, // git failure → treat as dirty → do not allow merge (fail closed; prevent silently losing changes, G1)
    }
}

/// Check whether each file in the in-place checkpoint list still has staged / unstaged / untracked changes;
/// ignored new checkpoint files likewise cannot have entered the commit and are also treated as uncommitted.
///
/// Constrain the pathspec separately for each file to avoid counting the user's other changes in the same workspace in this run. All git reads
/// go through the read-only hardening of `git_checked_stdout` → `git_read_command`; a git/status failure returns Err directly,
/// and the delivery gate fails closed to reject remote operations.
///
/// Nested repositories can hide checkpoint paths from the outer repository's status checks.
/// whatever gets cloned into it). Once a checkpoint path falls inside such a nested repository, `git status` does not descend at all for a pathspec pointing
/// inside the nested repository—it always emits nothing, reports no error, and exits with code 0 (confirmed with a minimal reproduction:
/// when `sub/` is a nested repository, `git status --porcelain -- sub/file` produces an empty string even if `sub/file` really
/// exists on disk and has never been committed). The empty output was originally treated as "clean" and delivery was allowed, making this blind spot
/// fail open. Use `git ls-files --error-unmatch` to verify whether the path is actually tracked by the outer repository's index—
/// even if a committed / previously `git add`ed file's directory later becomes a nested repository, its index entry remains and still matches
/// (also confirmed with a minimal reproduction: first commit `sub/file.txt`, then run `git init` in `sub/`; `ls-files
/// --error-unmatch` still succeeds because it reads the outer repository's index rather than walking directories). Tracked → trust
/// "clean"; not tracked but the file really exists on disk → treat as "uncommitted", restoring fail-closed behavior.
pub(crate) fn checkpoint_path_dirty_states(
    repo: &Path,
    checkpoint_paths: &[std::path::PathBuf],
) -> Result<Vec<(std::path::PathBuf, bool)>, String> {
    let canonical_repo = std::fs::canonicalize(repo).map_err(|error| {
        crate::ui_msg::al_err(
            "run.workspaceCanonicalizeFailed",
            &[("detail", error.to_string())],
        )
    })?;
    checkpoint_paths
        .iter()
        .map(|path| {
            let Some(relative) = path
                .strip_prefix(&canonical_repo)
                .ok()
                .and_then(Path::to_str)
            else {
                // Delivery must also be rejected when a ledger path escapes the project or cannot be represented as a Git pathspec.
                return Ok((path.clone(), true));
            };
            let status = git_checked_stdout(
                &canonical_repo,
                &[
                    "--literal-pathspecs",
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored=matching",
                    "--",
                    relative,
                ],
            )?;
            if !status.is_empty() {
                return Ok((path.clone(), true));
            }
            // An empty status does not mean "truly clean": verify whether this path is actually tracked by the outer repository's index; see the
            // nested-repository blind-spot comment above. Trust "clean" only when tracked; if untracked but the file really exists on disk, treat it as uncommitted.
            let tracked = git_checked_stdout(
                &canonical_repo,
                &[
                    "--literal-pathspecs",
                    "ls-files",
                    "--error-unmatch",
                    "--",
                    relative,
                ],
            )
            .is_ok();
            // `Path::exists` follows symlinks and can incorrectly treat a dangling link as absent.
            // A dangling symlink whose target does not exist does itself really exist on disk (`git status`
            // with `--untracked-files=all` also lists it as an untracked entry), but `exists()` returns false when target resolution fails,
            // causing the "untracked but the file really exists on disk" fail-closed check to fail for a dangling symlink
            // and incorrectly classify it as clean. Use `symlink_metadata` instead (do not follow the link; ask only whether this path itself
            // has an inode) to close this blind spot.
            let path_present = std::fs::symlink_metadata(path).is_ok();
            Ok((path.clone(), !tracked && path_present))
        })
        .collect()
}
