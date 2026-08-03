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
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_output(repo: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap()
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = git_output(repo, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo() -> TempDir {
        let repo = TempDir::new().unwrap();
        run_git(repo.path(), &["init", "-q"]);
        run_git(repo.path(), &["config", "user.name", "Broker Test"]);
        run_git(repo.path(), &["config", "user.email", "broker@example.com"]);
        repo
    }

    fn commit_file(repo: &Path, relative_path: &str, contents: &[u8]) {
        fs::write(repo.join(relative_path), contents).unwrap();
        run_git(repo, &["add", "--", relative_path]);
        run_git(repo, &["commit", "-qm", "base"]);
    }

    fn commit_file_in_dir(repo: &Path, relative_path: &str, contents: &[u8]) {
        let full_path = repo.join(relative_path);
        fs::create_dir_all(full_path.parent().unwrap()).unwrap();
        fs::write(&full_path, contents).unwrap();
        run_git(repo, &["add", "--", relative_path]);
        run_git(repo, &["commit", "-qm", "base"]);
    }

    #[test]
    fn selection_includes_pre_session_dirty_file() {
        let repo = init_repo();
        commit_file(repo.path(), "tracked.txt", b"HEAD contents\n");
        fs::write(repo.path().join("tracked.txt"), b"user changes\n").unwrap();
        fs::write(repo.path().join("tracked.txt"), b"agent changes\n").unwrap();

        let selection =
            compute_committable_selection(repo.path(), &[PathBuf::from("tracked.txt")]).unwrap();

        assert_eq!(selection.exact_paths, vec![PathBuf::from("tracked.txt")]);
    }

    #[test]
    fn commit_of_gitignored_path_still_rejected() {
        let repo = init_repo();
        fs::write(repo.path().join(".gitignore"), "*.env\n").unwrap();
        fs::write(repo.path().join("secret.env"), "secret\n").unwrap();

        let error =
            compute_committable_selection(repo.path(), &[PathBuf::from("secret.env")]).unwrap_err();

        assert!(error.contains("ignored by .gitignore"), "{error}");
    }

    #[test]
    fn commit_of_directory_still_rejected() {
        let repo = init_repo();
        fs::create_dir(repo.path().join("directory")).unwrap();

        let error =
            compute_committable_selection(repo.path(), &[PathBuf::from("directory")]).unwrap_err();

        assert!(error.contains("is a directory"), "{error}");
    }

    fn assert_outside_workspace_error(error: &str, original_path: &Path) {
        assert!(error.contains("不在本会话工作区内"), "{error}");
        assert!(
            error.contains(&original_path.display().to_string()),
            "{error}"
        );
        assert!(
            error.contains("use a path relative to the workspace root"),
            "{error}"
        );
    }

    fn assert_symlink_escape_error(error: &str, original_path: &Path) {
        assert_eq!(
            error,
            format!(
                "commit: 路径 {} 经符号链接解析后落在本会话工作区之外，commit 工具不能跟随指向工作区外的符号链接提交内容。(path resolves through a symlink to a location outside this session's workspace)",
                original_path.display()
            )
        );
    }

    #[test]
    fn commit_of_absolute_path_reports_actionable_workspace_error() {
        let repo = init_repo();
        let absolute_path = repo.path().parent().unwrap().join("outside.txt");

        let error =
            compute_committable_selection(repo.path(), std::slice::from_ref(&absolute_path))
                .unwrap_err();

        assert_outside_workspace_error(&error, &absolute_path);
    }

    #[test]
    fn commit_of_dotdot_escape_reports_actionable_workspace_error() {
        let repo = init_repo();
        let dotdot_path = PathBuf::from("../outside.txt");

        let error = compute_committable_selection(repo.path(), std::slice::from_ref(&dotdot_path))
            .unwrap_err();

        assert_outside_workspace_error(&error, &dotdot_path);
    }

    #[test]
    fn commit_of_cur_dir_path_reports_actionable_relative_path_error() {
        let repo = init_repo();
        let cur_dir_path = PathBuf::from("./x.txt");

        let error = compute_committable_selection(repo.path(), std::slice::from_ref(&cur_dir_path))
            .unwrap_err();

        assert!(error.contains("含有不允许的 `.` 路径段"), "{error}");
        assert!(error.contains("./x.txt"), "{error}");
        assert!(error.contains("不带 `./` 前缀"), "{error}");
    }

    #[test]
    fn commit_of_missing_path_is_rejected() {
        // Case 3 + case 8: `init_repo` makes no commits, so HEAD is unborn — "missing.txt" is
        // neither on disk nor in any version-controlled tree. This is a hallucinated path, not
        // a deletion, and it also exercises the unborn-HEAD route (no HEAD to check a
        // deletion candidate against at all) since the two scenarios collapse to the same
        // assertion here.
        let repo = init_repo();

        let error = compute_committable_selection(repo.path(), &[PathBuf::from("missing.txt")])
            .unwrap_err();

        assert!(
            error.contains("missing.txt") && error.contains("既不在磁盘上也不在版本库里"),
            "{error}"
        );
    }

    #[test]
    fn commit_of_missing_path_not_in_head_is_rejected_even_with_prior_commits() {
        // Same hallucinated-path case, but against a repo that DOES have a HEAD (and other
        // tracked files) — proves the HEAD lookup is path-specific, not just "HEAD exists".
        let repo = init_repo();
        commit_file(repo.path(), "tracked.txt", b"HEAD contents\n");

        let error = compute_committable_selection(repo.path(), &[PathBuf::from("missing.txt")])
            .unwrap_err();

        assert!(
            error.contains("missing.txt") && error.contains("既不在磁盘上也不在版本库里"),
            "{error}"
        );
    }

    #[test]
    fn commit_of_pure_deletion_is_selected() {
        // Case 1: HEAD has the path, disk doesn't. This is the real-world scenario the delete
        // support exists for (an agent `rm`s a tracked file, then asks the broker to commit
        // the deletion).
        let repo = init_repo();
        commit_file(repo.path(), "tracked.txt", b"HEAD contents\n");
        fs::remove_file(repo.path().join("tracked.txt")).unwrap();

        let selection =
            compute_committable_selection(repo.path(), &[PathBuf::from("tracked.txt")]).unwrap();

        assert_eq!(selection.exact_paths, vec![PathBuf::from("tracked.txt")]);
    }

    #[test]
    fn commit_of_mixed_deletion_and_modification_is_selected() {
        // Case 2 (selection layer): a deleted file and a modified file requested together must
        // both be selected — the deletion branch must not short-circuit or otherwise disturb
        // the sibling existing-path branch's handling in the same call.
        let repo = init_repo();
        commit_file(repo.path(), "deleted.txt", b"gone soon\n");
        commit_file(repo.path(), "modified.txt", b"before\n");
        fs::remove_file(repo.path().join("deleted.txt")).unwrap();
        fs::write(repo.path().join("modified.txt"), b"after\n").unwrap();

        let mut selection = compute_committable_selection(
            repo.path(),
            &[PathBuf::from("deleted.txt"), PathBuf::from("modified.txt")],
        )
        .unwrap();
        selection.exact_paths.sort();

        assert_eq!(
            selection.exact_paths,
            vec![PathBuf::from("deleted.txt"), PathBuf::from("modified.txt")]
        );
    }

    #[test]
    fn commit_of_deleted_directory_is_selected() {
        // Case 4a: the whole containing directory is gone too (not just the leaf file) — the
        // nearest existing ancestor should bottom out at the worktree root itself.
        let repo = init_repo();
        commit_file_in_dir(repo.path(), "subdir/file.txt", b"nested\n");
        fs::remove_dir_all(repo.path().join("subdir")).unwrap();

        let selection =
            compute_committable_selection(repo.path(), &[PathBuf::from("subdir/file.txt")])
                .unwrap();

        assert_eq!(
            selection.exact_paths,
            vec![PathBuf::from("subdir/file.txt")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn commit_of_deletion_through_ancestor_symlink_outside_worktree_is_rejected() {
        // Case 4b / 6b: "escape/nested/file.txt" was genuinely tracked while `escape` was a
        // real directory. Later, `escape` is replaced by a symlink pointing outside the
        // worktree, and the file is gone. The HEAD-membership judgment alone would pass this —
        // it really is tracked — so this specifically exercises the second, independent
        // ancestor-containment judgment.
        let repo = init_repo();
        commit_file_in_dir(repo.path(), "escape/nested/file.txt", b"nested\n");
        fs::remove_dir_all(repo.path().join("escape")).unwrap();
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join("escape")).unwrap();

        let error =
            compute_committable_selection(repo.path(), &[PathBuf::from("escape/nested/file.txt")])
                .unwrap_err();

        assert_symlink_escape_error(&error, Path::new("escape/nested/file.txt"));
    }

    #[test]
    fn commit_of_deletion_ignored_by_gitignore_is_no_longer_rejected() {
        // Case 5 (positive): a file was tracked, a `.gitignore` rule added afterward now
        // matches it, and it has since been deleted from disk. `git check-ignore` doesn't know
        // it used to be tracked — the deletion must not be blocked by the ignore wall.
        let repo = init_repo();
        commit_file(repo.path(), "secret.env", b"secret\n");
        fs::write(repo.path().join(".gitignore"), "*.env\n").unwrap();
        fs::remove_file(repo.path().join("secret.env")).unwrap();

        let selection =
            compute_committable_selection(repo.path(), &[PathBuf::from("secret.env")]).unwrap();

        assert_eq!(selection.exact_paths, vec![PathBuf::from("secret.env")]);
    }

    #[cfg(unix)]
    #[test]
    fn commit_of_deleted_tracked_symlink_is_selected() {
        // Case 6a: HEAD has a symlink entry (mode 120000); the symlink itself was removed from
        // disk. Deleting a symlink is exactly as legitimate as deleting a regular file.
        let repo = init_repo();
        fs::write(repo.path().join("target.txt"), b"target\n").unwrap();
        std::os::unix::fs::symlink("target.txt", repo.path().join("link.txt")).unwrap();
        run_git(repo.path(), &["add", "--", "target.txt", "link.txt"]);
        run_git(repo.path(), &["commit", "-qm", "symlink"]);
        fs::remove_file(repo.path().join("link.txt")).unwrap();

        let selection =
            compute_committable_selection(repo.path(), &[PathBuf::from("link.txt")]).unwrap();

        assert_eq!(selection.exact_paths, vec![PathBuf::from("link.txt")]);
    }

    #[test]
    fn commit_of_pathspec_magic_missing_path_is_rejected() {
        // Case 7: `git ls-tree` doesn't support glob/`:/` pathspec magic at all — without
        // `--literal-pathspecs` this would fail the whole `ls-tree` call outright ("pathspec
        // magic not supported"), not silently match a differently-named file like
        // "starfile.txt" below. What actually keeps this safe is defense in depth:
        // `--literal-pathspecs` makes the call well-defined (matches only a literal entry
        // named exactly ":(glob)star*", of which there is none), and `head_tracked_subset`
        // only ever treats a path as tracked via exact string equality against what `ls-tree`
        // returned — never by assuming "the call didn't error" implies "my path matched".
        let repo = init_repo();
        commit_file(repo.path(), "starfile.txt", b"unrelated\n");

        let error = compute_committable_selection(repo.path(), &[PathBuf::from(":(glob)star*")])
            .unwrap_err();

        assert!(error.contains("既不在磁盘上也不在版本库里"), "{error}");
    }

    #[test]
    fn commit_of_deleted_directory_path_itself_is_rejected() {
        // Security regression: deleting an entire tracked directory and then requesting the
        // *directory's own name* (not a file under it) as the commit path must be rejected.
        // `git ls-tree HEAD -- subdir` reports a `tree` entry for `subdir` — that's a real HEAD
        // hit, but it is not a single tracked *file*, and `git commit --only -- subdir` would
        // otherwise happily commit the removal of every file under it in one pathspec.
        //
        // The error message must say so honestly (it IS in HEAD, just not as a file) rather
        // than reusing the "not on disk, not in HEAD either" hallucinated-path wording — an
        // LLM agent reading "not in the repository" would go check its spelling, when the
        // actually-correct next move is "commit each file under subdir individually".
        let repo = init_repo();
        commit_file_in_dir(repo.path(), "subdir/a.txt", b"a\n");
        commit_file_in_dir(repo.path(), "subdir/inner/b.txt", b"b\n");
        fs::remove_dir_all(repo.path().join("subdir")).unwrap();

        let error =
            compute_committable_selection(repo.path(), &[PathBuf::from("subdir")]).unwrap_err();

        assert!(
            error.contains("subdir") && error.contains("不能传目录或子模块"),
            "{error}"
        );
        assert!(
            !error.contains("既不在磁盘上也不在版本库里"),
            "directory hit must not be phrased as a hallucinated path: {error}"
        );
    }

    #[test]
    fn commit_of_deleted_submodule_gitlink_is_rejected() {
        // Same class of bug as the directory case above, different tree-entry type: a
        // submodule gitlink (mode 160000) reports as an `ls-tree` `commit` entry, not `blob`.
        // A synthetic gitlink via `update-index --cacheinfo` is enough to exercise this
        // without a real submodule checkout.
        let repo = init_repo();
        let fake_sha = "a".repeat(40);
        run_git(
            repo.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{fake_sha},sub"),
            ],
        );
        run_git(repo.path(), &["commit", "-qm", "gitlink"]);

        let error =
            compute_committable_selection(repo.path(), &[PathBuf::from("sub")]).unwrap_err();

        assert!(
            error.contains("sub") && error.contains("不能传目录或子模块"),
            "{error}"
        );
        assert!(
            !error.contains("既不在磁盘上也不在版本库里"),
            "gitlink hit must not be phrased as a hallucinated path: {error}"
        );
    }

    #[test]
    fn commit_of_directory_and_hallucinated_path_produce_distinguishable_messages() {
        // Direct check that the two "missing on disk" failure modes read differently to
        // whoever (or whatever LLM agent) sees them: a real-but-wrong-shape HEAD hit (a
        // directory) versus a path that isn't in the repository at all.
        let repo = init_repo();
        commit_file_in_dir(repo.path(), "subdir/a.txt", b"a\n");
        fs::remove_dir_all(repo.path().join("subdir")).unwrap();

        let directory_error =
            compute_committable_selection(repo.path(), &[PathBuf::from("subdir")]).unwrap_err();
        let hallucinated_error =
            compute_committable_selection(repo.path(), &[PathBuf::from("never-existed.txt")])
                .unwrap_err();

        assert_ne!(directory_error, hallucinated_error);
        assert!(directory_error.contains("directory or submodule"));
        assert!(hallucinated_error.contains("既不在磁盘上也不在版本库里"));
        assert!(!hallucinated_error.contains("directory or submodule"));
        assert!(!directory_error.contains("既不在磁盘上也不在版本库里"));
    }

    #[cfg(unix)]
    #[test]
    fn commit_path_through_parent_symlink_outside_worktree_is_rejected() {
        let repo = init_repo();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("outside.txt"), "outside\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join("escape")).unwrap();

        let error =
            compute_committable_selection(repo.path(), &[PathBuf::from("escape/outside.txt")])
                .unwrap_err();

        assert_symlink_escape_error(&error, Path::new("escape/outside.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn commit_rejects_leaf_symlink_escaping_worktree() {
        let repo = init_repo();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "secret\n").unwrap();
        std::os::unix::fs::symlink(&outside_file, repo.path().join("leak")).unwrap();

        let error =
            compute_committable_selection(repo.path(), &[PathBuf::from("leak")]).unwrap_err();

        assert_symlink_escape_error(&error, Path::new("leak"));
    }

    #[test]
    fn unauthorized_commit_short_circuits_before_selection() {
        let nonexistent = Path::new("/does/not/exist");

        let result = mediate_commit_for_session(
            nonexistent,
            None,
            "must not run",
            &[PathBuf::from("outside.txt")],
            false,
        )
        .unwrap();

        assert_eq!(
            result,
            CommitResult::Refused {
                reason: "未授权本地提交".into()
            }
        );
    }

    #[test]
    fn empty_selection_is_refused() {
        let repo = init_repo();

        let result =
            mediate_commit_for_session(repo.path(), None, "must not run", &[], true).unwrap();

        assert_eq!(
            result,
            CommitResult::Refused {
                reason: "无可安全提交的文件".into()
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires running sandbox-exec outside the Codex sandbox"]
    fn commit_includes_pre_session_dirty_file() {
        let repo = init_repo();
        commit_file(repo.path(), "tracked.txt", b"HEAD contents\n");
        fs::write(repo.path().join("tracked.txt"), b"user changes\n").unwrap();
        fs::write(repo.path().join("tracked.txt"), b"agent changes\n").unwrap();

        let result = mediate_commit_for_session(
            repo.path(),
            None,
            "commit requested dirty file",
            &[PathBuf::from("tracked.txt")],
            true,
        )
        .unwrap();

        let CommitResult::Committed {
            sha,
            committed_paths,
        } = result
        else {
            panic!("pre-session dirty file was not committed");
        };
        assert_eq!(committed_paths, vec![PathBuf::from("tracked.txt")]);
        assert_eq!(
            String::from_utf8(
                git_output(repo.path(), &["show", &format!("{sha}:tracked.txt")]).stdout
            )
            .unwrap(),
            "agent changes\n"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires running sandbox-exec outside the Codex sandbox"]
    fn commit_of_pure_deletion_lands_on_disk() {
        let repo = init_repo();
        commit_file(repo.path(), "tracked.txt", b"HEAD contents\n");
        fs::remove_file(repo.path().join("tracked.txt")).unwrap();

        let result = mediate_commit_for_session(
            repo.path(),
            None,
            "commit deletion",
            &[PathBuf::from("tracked.txt")],
            true,
        )
        .unwrap();

        let CommitResult::Committed {
            sha,
            committed_paths,
        } = result
        else {
            panic!("deletion was not committed");
        };
        assert_eq!(committed_paths, vec![PathBuf::from("tracked.txt")]);

        let tree_output = git_output(repo.path(), &["ls-tree", &sha, "--", "tracked.txt"]);
        assert!(tree_output.status.success());
        assert!(
            tree_output.stdout.is_empty(),
            "tracked.txt should no longer be in the tree: {:?}",
            String::from_utf8_lossy(&tree_output.stdout)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires running sandbox-exec outside the Codex sandbox"]
    fn commit_of_mixed_deletion_and_modification_lands_on_disk() {
        let repo = init_repo();
        commit_file(repo.path(), "deleted.txt", b"gone soon\n");
        commit_file(repo.path(), "modified.txt", b"before\n");
        fs::remove_file(repo.path().join("deleted.txt")).unwrap();
        fs::write(repo.path().join("modified.txt"), b"after\n").unwrap();

        let result = mediate_commit_for_session(
            repo.path(),
            None,
            "commit mixed delete + modify",
            &[PathBuf::from("deleted.txt"), PathBuf::from("modified.txt")],
            true,
        )
        .unwrap();

        let CommitResult::Committed {
            sha,
            mut committed_paths,
        } = result
        else {
            panic!("mixed delete+modify was not committed");
        };
        committed_paths.sort();
        assert_eq!(
            committed_paths,
            vec![PathBuf::from("deleted.txt"), PathBuf::from("modified.txt")]
        );

        let tree_output = git_output(repo.path(), &["ls-tree", &sha, "--", "deleted.txt"]);
        assert!(tree_output.status.success());
        assert!(
            tree_output.stdout.is_empty(),
            "deleted.txt should no longer be in the tree: {:?}",
            String::from_utf8_lossy(&tree_output.stdout)
        );

        assert_eq!(
            String::from_utf8(
                git_output(repo.path(), &["show", &format!("{sha}:modified.txt")]).stdout
            )
            .unwrap(),
            "after\n"
        );
    }
}
