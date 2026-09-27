#![allow(dead_code)]

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CommittableSelection {
    pub exact_paths: Vec<PathBuf>,
    /// The subset of `exact_paths` that are deletions (tracked in HEAD, absent on disk).
    /// Surfaced so the one-time commit-authorization preview (this is the only
    /// human-in-the-loop checkpoint before a repo's commits are auto-approved) can call out
    /// deletions distinctly from adds/modifies — deleting is destructive and irreversible, and
    /// a path string alone doesn't tell a reviewer which kind of change it is.
    pub deleted_paths: std::collections::HashSet<PathBuf>,
}

/// Walks up from `absolute_path`'s parent until it finds a directory entry that actually
/// exists on disk (checked with `symlink_metadata`, i.e. lstat: a dangling symlink counts as
/// "exists" here — its brokenness surfaces one step later when the caller canonicalizes it and
/// the containment check fails). Only used for missing-on-disk (deletion) commit paths, whose
/// leaf has nothing left to canonicalize directly.
///
/// A non-`NotFound` I/O error while probing an ancestor is propagated rather than swallowed —
/// fail closed: an ancestor we can't even stat is not evidence the deletion is safe.
fn nearest_existing_ancestor(absolute_path: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut current = absolute_path.parent();
    while let Some(dir) = current {
        match std::fs::symlink_metadata(dir) {
            Ok(_) => return Ok(Some(dir.to_path_buf())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = dir.parent();
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

pub(crate) fn compute_committable_selection(
    worktree: &Path,
    requested_paths: &[PathBuf],
) -> Result<CommittableSelection, String> {
    compute_committable_selection_from_entries(worktree, requested_paths)
        .map_err(map_outside_workspace_error)
}

fn map_outside_workspace_error(error: String) -> String {
    const CUR_DIR_PREFIX: &str = "sandboxed commit path contains '.': ";
    const SYMLINK_ESCAPE_PREFIX: &str = "sandboxed commit path escapes worktree: ";
    const OUTSIDE_WORKSPACE_PREFIXES: [&str; 3] = [
        "sandboxed commit path must be relative: ",
        "sandboxed commit path contains '..': ",
        "sandboxed commit path contains a root or prefix: ",
    ];

    if let Some(path) = error.strip_prefix(CUR_DIR_PREFIX) {
        return format!(
            "commit: 路径 {path} 含有不允许的 `.` 路径段。commit 工具要求使用不带 `./` 前缀的工作区根相对路径。(path contains a '.' component; use a workspace-root-relative path without a './' prefix)"
        );
    }

    if let Some(path) = error.strip_prefix(SYMLINK_ESCAPE_PREFIX) {
        return format!(
            "commit: 路径 {path} 经符号链接解析后落在本会话工作区之外，commit 工具不能跟随指向工作区外的符号链接提交内容。(path resolves through a symlink to a location outside this session's workspace)"
        );
    }

    let Some(path) = OUTSIDE_WORKSPACE_PREFIXES
        .iter()
        .find_map(|prefix| error.strip_prefix(prefix))
    else {
        return error;
    };

    format!(
        "commit: 路径 {path} 不在本会话工作区内。commit 工具只能提交本会话工作区内的文件；请改用相对于工作区根的相对路径。(path is outside this session's workspace; use a path relative to the workspace root)"
    )
}

fn compute_committable_selection_from_entries(
    worktree: &Path,
    requested_paths: &[PathBuf],
) -> Result<CommittableSelection, String> {
    let worktree = std::fs::canonicalize(worktree)
        .map_err(|error| format!("规范化 worktree 失败: {error}"))?;
    if requested_paths.is_empty() {
        return Ok(CommittableSelection::default());
    }

    crate::worktree::validate_sandboxed_commit_inputs(
        &worktree,
        "AgentLoom",
        "agentloom@localhost",
        requested_paths,
    )?;

    // Pass 1: classify each path by on-disk presence. Present paths get their existing
    // three-step escape check immediately; missing paths are only *candidates* for deletion
    // and are collected for a single batched HEAD-membership lookup below — this keeps the
    // whole function to one `git` invocation for the deletion judgment no matter how many
    // paths are missing (see `nearest_existing_ancestor`'s neighbor, `head_tracked_subset`, for
    // why this must be batched rather than one `git ls-tree` per path).
    let mut missing_candidates: Vec<&PathBuf> = Vec::new();
    for path in requested_paths {
        let absolute_path = worktree.join(path);
        match std::fs::symlink_metadata(&absolute_path) {
            Ok(_) => {
                // Present on disk (file or symlink; `validate_sandboxed_commit_inputs` above
                // already rejected directories and other node types) — unchanged three-step
                // escape check: canonicalize the leaf, then its parent, and require both to
                // stay inside the worktree.
                let canonical_path = std::fs::canonicalize(&absolute_path).map_err(|error| {
                    format!(
                        "could not canonicalize sandboxed commit path {}: {error}",
                        path.display()
                    )
                })?;
                if !canonical_path.starts_with(&worktree) {
                    return Err(format!(
                        "sandboxed commit path escapes worktree: {}",
                        path.display()
                    ));
                }

                let parent = absolute_path.parent().ok_or_else(|| {
                    format!("sandboxed commit path has no parent: {}", path.display())
                })?;
                let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
                    format!(
                        "could not canonicalize sandboxed commit path parent {}: {error}",
                        path.display()
                    )
                })?;
                if !canonical_parent.starts_with(&worktree) {
                    return Err(format!(
                        "sandboxed commit path escapes worktree: {}",
                        path.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Nothing on disk at this path. Note: on the default case-insensitive APFS,
                // `symlink_metadata` also matches a differently-cased path that IS still on
                // disk (e.g. requesting "Foo.txt" when only "foo.txt" exists) — that hits the
                // `Ok(_)` arm above, not this one, so this branch only ever runs when there is
                // truly no on-disk entry under any casing variant `symlink_metadata` would
                // find. Deferred to pass 2 below.
                missing_candidates.push(path);
            }
            Err(error) => {
                return Err(format!(
                    "could not inspect sandboxed commit path {}: {error}",
                    path.display()
                ));
            }
        }
    }

    // Pass 2: for every missing-on-disk candidate, two independent judgments must both hold
    // before it counts as a legitimate deletion — mirroring the existing-path branch's own
    // two-step check.
    //
    // 1. The path must be tracked as a **blob** in HEAD (checked in one batched call for all
    //    candidates at once). A path that's simply absent — never existed, or hallucinated by
    //    an agent — is not a deletion; without this gate `git update-index --remove` would
    //    silently no-op for a typo'd path instead of surfacing it as an error. Blob-only
    //    matters: `git ls-tree` also reports a `tree` entry for an entire removed directory
    //    pathspec (e.g. `subdir`) and a `commit` entry for a removed submodule gitlink — a bare
    //    directory or gitlink name is never a valid single-file deletion pathspec.
    //    `head_tracked_entries` (same batched `ls-tree` calls, no extra `git` process) also
    //    hands back which of the misses hit HEAD as one of those non-blob entries, so the
    //    error message below can tell an LLM agent the honest reason — "this is a directory or
    //    submodule, address the files inside it" — rather than sending it off to second-guess
    //    a path spelling that was never the problem.
    let missing_refs: Vec<&Path> = missing_candidates
        .iter()
        .map(|path| path.as_path())
        .collect();
    let (tracked_deletions, non_blob_hits) =
        crate::worktree::head_tracked_entries(&worktree, &missing_refs).map_err(|error| {
            format!("could not verify sandboxed commit deletion candidates against HEAD: {error}")
        })?;

    for path in &missing_candidates {
        if !tracked_deletions.contains(path.as_path()) {
            if non_blob_hits.contains(path.as_path()) {
                return Err(format!(
                    "sandboxed commit path {} is tracked in HEAD as a directory or submodule entry, not a single file (只能提交单个文件路径，不能传目录或子模块); commit the files inside it individually instead (请改为逐个提交其中的具体文件)",
                    path.display()
                ));
            }
            return Err(format!(
                "路径既不在磁盘上也不在版本库里，无法提交删除: {}",
                path.display()
            ));
        }

        // 2. The nearest still-existing ancestor directory must resolve inside the worktree.
        //    The path itself is gone, so there is no leaf to canonicalize; walking up to the
        //    nearest real filesystem entry and checking *that* catches the same class of
        //    escape the existing-path branch catches on its parent (e.g.
        //    `escape/nested/file.txt` where `escape` is a symlink that now — or always did —
        //    point outside the worktree, even though the file itself is legitimately tracked
        //    in HEAD).
        let absolute_path = worktree.join(path);
        let ancestor = nearest_existing_ancestor(&absolute_path)
            .map_err(|error| {
                format!(
                    "could not inspect sandboxed commit path ancestor for {}: {error}",
                    path.display()
                )
            })?
            .ok_or_else(|| {
                format!(
                    "could not find an existing ancestor directory for sandboxed commit path {}",
                    path.display()
                )
            })?;
        let canonical_ancestor = std::fs::canonicalize(&ancestor).map_err(|error| {
            format!(
                "could not canonicalize sandboxed commit path ancestor for {}: {error}",
                path.display()
            )
        })?;
        if !canonical_ancestor.starts_with(&worktree) {
            return Err(format!(
                "sandboxed commit path escapes worktree: {}",
                path.display()
            ));
        }
    }

    crate::worktree::reject_ignored_exact_paths(&worktree, requested_paths)?;

    Ok(CommittableSelection {
        exact_paths: requested_paths.to_vec(),
        deleted_paths: missing_candidates.into_iter().cloned().collect(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommitResult {
    Committed {
        sha: String,
        committed_paths: Vec<PathBuf>,
    },
    Refused {
        reason: String,
    },
}

pub(crate) fn mediate_commit_for_session(
    worktree: &Path,
    app_data_dir: Option<&Path>,
    message: &str,
    requested_paths: &[PathBuf],
    authorized: bool,
) -> Result<CommitResult, String> {
    if !authorized {
        return Ok(CommitResult::Refused {
            reason: "未授权本地提交".into(),
        });
    }

    let selection = compute_committable_selection(worktree, requested_paths)?;
    commit_selection(selection, worktree, app_data_dir, message)
}

fn commit_selection(
    selection: CommittableSelection,
    worktree: &Path,
    app_data_dir: Option<&Path>,
    message: &str,
) -> Result<CommitResult, String> {
    if selection.exact_paths.is_empty() {
        return Ok(CommitResult::Refused {
            reason: "无可安全提交的文件".into(),
        });
    }

    let (name, email) = crate::worktree::resolve_git_author_identity(worktree)?;
    let dirs = crate::worktree::resolve_git_metadata_dirs(worktree)?;
    let output = crate::worktree::run_sandboxed_git_commit(
        worktree,
        &dirs.git_dir,
        &dirs.git_common_dir,
        app_data_dir,
        message,
        &name,
        &email,
        &selection.exact_paths,
    )?;
    if !output.status.success() {
        return Err(format!(
            "提交失败: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let sha_output = crate::worktree::git_read_output(worktree, &["rev-parse", "HEAD"])
        .map_err(|error| format!("读取新提交 SHA 失败: {error}"))?;
    if !sha_output.status.success() {
        return Err(format!(
            "读取新提交 SHA 失败: {}",
            String::from_utf8_lossy(&sha_output.stderr)
        ));
    }
    let sha = String::from_utf8_lossy(&sha_output.stdout)
        .trim()
        .to_string();
    if sha.is_empty() {
        return Err("读取新提交 SHA 失败: 输出为空".into());
    }

    Ok(CommitResult::Committed {
        sha,
        committed_paths: selection.exact_paths,
    })
}

#[cfg(test)]
mod tests;
