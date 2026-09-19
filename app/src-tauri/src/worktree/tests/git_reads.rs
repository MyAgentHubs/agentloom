#![cfg(test)]

use super::*;

#[path = "../../lib/tests/source_scanner.rs"]
mod source_scanner;

#[test]
fn broker_t1_read_head_present() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let contents = b"HEAD contents\n";
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("tracked.txt"), contents).unwrap();
    run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    assert_eq!(
        read_head_entry(repo, Path::new("tracked.txt")),
        Ok(Some(HeadEntry {
            mode: 0o100644,
            bytes: contents.to_vec(),
        }))
    );
}

#[test]
fn broker_t1_read_head_literal_pathspec() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let magic_name = ":(glob)star*.txt";
    let contents = b"literal pathspec contents\n";
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join(magic_name), contents).unwrap();
    std::fs::write(repo.join("star-normal.txt"), b"pathspec match\n").unwrap();
    run_git(
        repo,
        &[
            "--literal-pathspecs",
            "add",
            "--",
            magic_name,
            "star-normal.txt",
        ],
    )
    .unwrap();
    run_git(repo, &["update-index", "--chmod=+x", "star-normal.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "literal pathspec"]).unwrap();

    assert_eq!(
        read_head_entry(repo, Path::new(magic_name)),
        Ok(Some(HeadEntry {
            mode: 0o100644,
            bytes: contents.to_vec(),
        }))
    );
}

#[test]
fn broker_t1_read_head_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("tracked.txt"), "tracked\n").unwrap();
    run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    assert_eq!(read_head_entry(repo, Path::new("missing.txt")), Ok(None));
}

#[test]
fn broker_t1_read_head_unborn() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();

    assert_eq!(read_head_entry(repo, Path::new("missing.txt")), Ok(None));
}

#[test]
fn broker_t1_read_head_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let contents = b"binary\0contents\xff\n";
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("binary.dat"), contents).unwrap();
    run_git(repo, &["add", "--", "binary.dat"]).unwrap();
    run_git(repo, &["commit", "-qm", "binary"]).unwrap();

    assert_eq!(
        read_head_entry(repo, Path::new("binary.dat")),
        Ok(Some(HeadEntry {
            mode: 0o100644,
            bytes: contents.to_vec(),
        }))
    );
}

#[cfg(unix)]
#[test]
fn broker_t1_read_head_exec_mode() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let path = repo.join("executable.sh");
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(&path, b"#!/bin/sh\n").unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    run_git(repo, &["add", "--", "executable.sh"]).unwrap();
    run_git(repo, &["commit", "-qm", "executable"]).unwrap();

    assert_eq!(
        read_head_entry(repo, Path::new("executable.sh")),
        Ok(Some(HeadEntry {
            mode: 0o100755,
            bytes: b"#!/bin/sh\n".to_vec(),
        }))
    );
}

#[cfg(unix)]
#[test]
fn broker_t1_read_head_symlink_mode() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("target.txt"), b"target\n").unwrap();
    symlink("target.txt", repo.join("link.txt")).unwrap();
    run_git(repo, &["add", "--", "target.txt", "link.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "symlink"]).unwrap();

    assert_eq!(
        read_head_entry(repo, Path::new("link.txt")),
        Ok(Some(HeadEntry {
            mode: 0o120000,
            bytes: b"target.txt".to_vec(),
        }))
    );
}

#[test]
fn broker_p1_head_tracked_subset_present() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("tracked.txt"), b"contents\n").unwrap();
    run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    let tracked = head_tracked_subset(repo, &[Path::new("tracked.txt")]).unwrap();
    assert!(tracked.contains(Path::new("tracked.txt")));
}

#[test]
fn broker_p1_head_tracked_subset_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("tracked.txt"), b"contents\n").unwrap();
    run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    let tracked = head_tracked_subset(repo, &[Path::new("missing.txt")]).unwrap();
    assert!(!tracked.contains(Path::new("missing.txt")));
}

#[test]
fn broker_p1_head_tracked_subset_unborn() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();

    let tracked = head_tracked_subset(repo, &[Path::new("missing.txt")]).unwrap();
    assert!(tracked.is_empty());
}

#[test]
fn broker_p1_head_tracked_subset_uses_literal_pathspecs() {
    // `git ls-tree` doesn't even support glob/`:/` pathspec magic — without
    // `--literal-pathspecs` a magic-looking string like ":(glob)star*" makes the whole
    // invocation fail with "pathspec magic not supported", not silently match a
    // differently-named file (verified empirically: `git ls-tree HEAD --
    // ":(glob)star*"` exits 128 with that exact message). What actually makes this safe
    // is defense in depth: `--literal-pathspecs` keeps the call itself well-defined
    // (matches only a literal tree entry named exactly ":(glob)star*", of which there is
    // none here), AND the caller only ever treats a requested path as "tracked" via exact
    // Rust string/PathBuf equality against the returned entry name — never by trusting
    // that "ls-tree returned something" implies "the path I asked about is tracked".
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("starfile.txt"), b"unrelated\n").unwrap();
    run_git(repo, &["add", "--", "starfile.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    let tracked = head_tracked_subset(repo, &[Path::new(":(glob)star*")]).unwrap();
    assert!(tracked.is_empty());
    assert!(!tracked.contains(Path::new("starfile.txt")));
}

#[test]
fn broker_p1_head_tracked_subset_excludes_directory_tree_entries() {
    // Security regression: `git ls-tree HEAD -- subdir` reports a `tree` entry for
    // `subdir` itself when `subdir` is a real (possibly now-deleted-from-disk) tracked
    // directory. That must NOT count as "subdir is a trackable deletion target" — a bare
    // directory name is never a valid single-file commit pathspec.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::create_dir(repo.join("subdir")).unwrap();
    std::fs::write(repo.join("subdir/a.txt"), b"a\n").unwrap();
    run_git(repo, &["add", "--", "subdir/a.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    let tracked = head_tracked_subset(repo, &[Path::new("subdir")]).unwrap();
    assert!(
        !tracked.contains(Path::new("subdir")),
        "a directory's tree entry must not count as a tracked blob"
    );
}

#[test]
fn broker_p1_head_tracked_subset_excludes_gitlink_commit_entries() {
    // Security regression: a submodule gitlink (mode 160000, `ls-tree` type `commit`)
    // must not count as a trackable single-file deletion either — same class of bug as
    // the directory case, different tree-entry type. A synthetic gitlink via
    // `update-index --cacheinfo` is enough to exercise this; it doesn't require a real
    // submodule checkout.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    let fake_sha = "a".repeat(40);
    run_git(
        repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{fake_sha},sub"),
        ],
    )
    .unwrap();
    run_git(repo, &["commit", "-qm", "gitlink"]).unwrap();

    let tracked = head_tracked_subset(repo, &[Path::new("sub")]).unwrap();
    assert!(
        !tracked.contains(Path::new("sub")),
        "a gitlink's commit entry must not count as a tracked blob"
    );
}

#[test]
fn broker_p2_head_tracked_subset_chunks_large_batches() {
    // Proves the chunked implementation doesn't drop or corrupt results across a chunk
    // boundary (`MAX_CHUNK_PATHS` is 1,000) — this is the primitive underlying the E2BIG
    // fix, exercised here directly rather than through a slow multi-thousand-file commit.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("tracked-first.txt"), b"a\n").unwrap();
    std::fs::write(repo.join("tracked-last.txt"), b"b\n").unwrap();
    run_git(
        repo,
        &["add", "--", "tracked-first.txt", "tracked-last.txt"],
    )
    .unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    let mut owned_paths: Vec<PathBuf> = vec![PathBuf::from("tracked-first.txt")];
    owned_paths.extend((0..2_500).map(|index| PathBuf::from(format!("missing-{index}.txt"))));
    owned_paths.push(PathBuf::from("tracked-last.txt"));
    let paths: Vec<&Path> = owned_paths.iter().map(PathBuf::as_path).collect();

    let tracked = head_tracked_subset(repo, &paths).unwrap();
    assert_eq!(tracked.len(), 2);
    assert!(tracked.contains(Path::new("tracked-first.txt")));
    assert!(tracked.contains(Path::new("tracked-last.txt")));
}

#[test]
fn broker_p3_head_tracked_entries_classifies_blob_vs_non_blob_vs_absent() {
    // The commit broker's better error message depends on `head_tracked_entries`
    // correctly sorting three missing-on-disk candidates into: a real file (blob), a
    // whole directory (tree, non-blob), and a path that's in neither set at all
    // (genuinely absent from HEAD).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::create_dir(repo.join("subdir")).unwrap();
    std::fs::write(repo.join("subdir/a.txt"), b"a\n").unwrap();
    std::fs::write(repo.join("tracked.txt"), b"tracked\n").unwrap();
    run_git(repo, &["add", "--", "subdir/a.txt", "tracked.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();

    let (blobs, non_blobs) = head_tracked_entries(
        repo,
        &[
            Path::new("tracked.txt"),
            Path::new("subdir"),
            Path::new("never-existed.txt"),
        ],
    )
    .unwrap();

    assert!(blobs.contains(Path::new("tracked.txt")));
    assert!(!blobs.contains(Path::new("subdir")));
    assert!(non_blobs.contains(Path::new("subdir")));
    assert!(!non_blobs.contains(Path::new("tracked.txt")));
    assert!(!blobs.contains(Path::new("never-existed.txt")));
    assert!(!non_blobs.contains(Path::new("never-existed.txt")));
}

#[test]
fn resolve_git_metadata_dirs_for_standalone_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();

    let dirs = resolve_git_metadata_dirs(&repo).unwrap();
    let expected = std::fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(dirs.git_dir, expected);
    assert_eq!(dirs.git_common_dir, expected);
    assert!(dirs.git_dir.is_absolute());
    assert_eq!(std::fs::canonicalize(&dirs.git_dir).unwrap(), dirs.git_dir);
}

#[test]
fn resolve_git_metadata_dirs_for_linked_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    let linked = tmp.path().join("linked");
    std::fs::create_dir(&main).unwrap();
    run_git(&main, &["init", "-q"]).unwrap();
    run_git(&main, &["config", "user.email", "test@example.com"]).unwrap();
    run_git(&main, &["config", "user.name", "Test User"]).unwrap();
    std::fs::write(main.join("base.txt"), "base\n").unwrap();
    run_git(&main, &["add", "base.txt"]).unwrap();
    run_git(&main, &["commit", "-qm", "base"]).unwrap();
    run_git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-test",
            linked.to_str().unwrap(),
        ],
    )
    .unwrap();

    assert!(linked.join(".git").is_file());
    let dirs = resolve_git_metadata_dirs(&linked).unwrap();
    let expected_common = std::fs::canonicalize(main.join(".git")).unwrap();
    let expected_git_dir =
        std::fs::canonicalize(expected_common.join("worktrees").join("linked")).unwrap();

    assert_eq!(dirs.git_dir, expected_git_dir);
    assert_eq!(dirs.git_common_dir, expected_common);
    assert_ne!(dirs.git_dir, dirs.git_common_dir);
    for path in [&dirs.git_dir, &dirs.git_common_dir] {
        assert!(path.exists());
        assert!(path.is_absolute());
        assert_eq!(std::fs::canonicalize(path).unwrap(), *path);
    }
}

#[test]
fn git_metadata_dirs_rejects_non_absolute_common_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git-dir");
    std::fs::create_dir(&git_dir).unwrap();
    let stdout = format!("{}\n../relative-common\n", git_dir.display()).into_bytes();

    let error = git_metadata_dirs_from_stdout(stdout).unwrap_err();
    assert!(
        error.contains("non-absolute GIT_COMMON_DIR"),
        "unexpected error: {error}"
    );
}

#[test]
fn changed_paths_between_lists_relative_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.email", "t@t"]).unwrap();
    run_git(repo, &["config", "user.name", "t"]).unwrap();
    run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(repo, &["add", "."]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();
    let base = rev_parse_head(repo).unwrap();

    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "hello\n").unwrap();
    run_git(repo, &["add", "."]).unwrap();
    run_git(repo, &["commit", "-qm", "change"]).unwrap();
    let head = rev_parse_head(repo).unwrap();

    let paths = changed_paths_between(repo, &base, &head).unwrap();
    assert_eq!(paths, vec!["src/lib.rs"]);
}

#[test]
fn changed_paths_between_no_renames_lists_both_sides_of_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.email", "t@t"]).unwrap();
    run_git(repo, &["config", "user.name", "t"]).unwrap();
    run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
    std::fs::write(repo.join("old.txt"), "same content\n").unwrap();
    run_git(repo, &["add", "old.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();
    let base = rev_parse_head(repo).unwrap();

    run_git(repo, &["mv", "old.txt", "new.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "rename"]).unwrap();
    let head = rev_parse_head(repo).unwrap();

    let paths = changed_paths_between_no_renames(repo, &base, &head).unwrap();
    assert_eq!(paths, vec!["new.txt", "old.txt"]);
}

#[test]
fn changed_paths_between_no_renames_preserves_non_ascii_and_spaced_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let base = rev_parse_head(&repo).unwrap();

    std::fs::write(repo.join("日本語.txt"), "日本語 content\n").unwrap();
    std::fs::write(repo.join("with space.txt"), "spaced content\n").unwrap();
    git_checked(
        &repo,
        &[
            "--literal-pathspecs",
            "add",
            "--",
            "日本語.txt",
            "with space.txt",
        ],
    );
    git_checked(&repo, &["commit", "-qm", "add unusual paths"]);
    let head = rev_parse_head(&repo).unwrap();

    let paths = changed_paths_between_no_renames(&repo, &base, &head).unwrap();
    assert_eq!(paths, vec!["with space.txt", "日本語.txt"]);

    let review = review_scoped(
        &repo,
        &base,
        &paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(review.files_changed, 2);
    assert_eq!(
        review
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["日本語.txt", "with space.txt"])
    );
    assert!(review.patch.contains("日本語 content"));
    assert!(review.patch.contains("spaced content"));
}

#[test]
fn changed_paths_between_flags_protected_workflow_path() {
    let paths = vec![
        ".github/workflows/ci.yml".to_string(),
        "src/lib.rs".to_string(),
    ];
    assert_eq!(
        protected_landing_paths(&paths),
        vec![".github/workflows/ci.yml"]
    );
}

#[test]
fn git_read_command_applies_security_prefix_and_renderer_flags() {
    let command = git_read_command(
        Path::new("/tmp"),
        &["-c", "core.quotepath=false", "diff", "--stat", "HEAD"],
    );
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert!(args
        .windows(HARDENED_GIT_READ_PREFIX.len())
        .any(|window| window == HARDENED_GIT_READ_PREFIX));
    assert!(args
        .windows(4)
        .any(|window| { window == ["diff", "--no-textconv", "--no-ext-diff", "--stat"] }));
    assert!(command.get_envs().any(|(key, value)| {
        key == "GIT_OPTIONAL_LOCKS" && value.is_some_and(|value| value == "0")
    }));

    for renderer in ["show", "log", "blame"] {
        let command = git_read_command(Path::new("/tmp"), &[renderer, "HEAD"]);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(3)
            .any(|window| { window == [renderer, "--no-textconv", "--no-ext-diff"] }));
    }

    let grep = git_read_command(Path::new("/tmp"), &["grep", "needle"]);
    let grep_args = grep
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(grep_args
        .windows(3)
        .any(|window| window == ["grep", "--no-textconv", "needle"]));
}

#[test]
fn production_content_rendering_git_reads_use_hardened_runner() {
    source_scanner::assert_boundaries();
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_hardened_git_reads(&source_scanner::production_sources(&src_dir));
}

fn assert_hardened_git_reads(sources: &[source_scanner::Source]) {
    let helper = source_scanner::unique_item(sources, "fn", "git_read_command");
    for (file, source) in sources.iter().enumerate() {
        let production = &source.production;
        let without_helper_body = if file == helper.file {
            format!(
                "{}{}",
                &production[..helper.body.start],
                &production[helper.body.end..]
            )
        } else {
            production.to_string()
        };

        let mut remaining = without_helper_body.as_str();
        while let Some(start) = remaining.find("Command::new(\"git\")") {
            let block = &remaining[start..];
            let end = block.find(".output()").unwrap_or(block.len());
            let invocation = &block[..end];
            for subcommand in ["diff", "show", "log", "blame", "grep"] {
                assert!(
                    !invocation.contains(&format!("\"{subcommand}\"")),
                    "{} has raw git {subcommand} bypassing git_read_command: {invocation}",
                    source.path.display()
                );
            }
            remaining = &block[end..];
        }
    }

    let member_diff = source_scanner::unique_item(sources, "fn", "member_artifact_diff_inner");
    let member_diff = &sources[member_diff.file].production[member_diff.range];
    assert!(member_diff.contains("worktree::artifact_diff_text"));
}

#[cfg(unix)]
#[test]
fn shared_git_read_hardening_covers_landed_artifact_and_numstat_diffs() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    std::fs::write(repo.join(".gitattributes"), "tracked.txt diff=evil\n").unwrap();
    std::fs::write(repo.join("tracked.txt"), "before\n").unwrap();
    git_checked(&repo, &["add", ".gitattributes", "tracked.txt"]);
    git_checked(&repo, &["commit", "-qm", "base"]);
    let base = rev_parse_head(&repo).unwrap();
    std::fs::write(repo.join("tracked.txt"), "after\n").unwrap();
    git_checked(&repo, &["add", "tracked.txt"]);
    git_checked(&repo, &["commit", "-qm", "head"]);
    let head = rev_parse_head(&repo).unwrap();

    let textconv = tmp.path().join("textconv.sh");
    let marker = tmp.path().join("textconv-ran");
    std::fs::write(
        &textconv,
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/textconv-ran\"\ncat \"$1\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&textconv).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&textconv, permissions).unwrap();
    git_checked(
        &repo,
        &["config", "diff.evil.textconv", &textconv.to_string_lossy()],
    );

    assert!(!artifact_diff_text(&repo, &base, &head).unwrap().is_empty());
    assert!(!landed_review(&repo, &base, &head).unwrap().patch.is_empty());
    assert_eq!(run_numstat(&repo, &base, &head).unwrap().files, 1);
    assert!(
        !marker.exists(),
        "a named user-project diff path bypassed shared hardening"
    );
}
