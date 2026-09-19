#![cfg(test)]

use super::*;

#[test]
fn broker_t1_resolve_identity_present() {
    let _env_lock = super::super::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();

    assert_eq!(
        resolve_git_author_identity(repo),
        Ok((
            "Broker Test User".to_string(),
            "broker@example.com".to_string()
        ))
    );
}

#[test]
fn broker_t1_resolve_identity_missing_email() {
    let _env_lock = super::super::test_home_lock();
    let _config_guard = GitConfigIsolationGuard::install();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();

    assert_eq!(
        resolve_git_author_identity(repo),
        Ok((
            "Broker Test User".to_string(),
            "agentloom@localhost".to_string()
        ))
    );
}

#[test]
fn broker_t1_resolve_identity_falls_back_when_unset() {
    let _env_lock = super::super::test_home_lock();
    let _config_guard = GitConfigIsolationGuard::install();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();

    assert_eq!(
        resolve_git_author_identity(repo),
        Ok(("AgentLoom".to_string(), "agentloom@localhost".to_string()))
    );
}

/// R-B1 项 1（Major-1·交付闸门 fail-open 修复）：agent 在全新空子目录里 `git init` 是
/// 常规动作（准备 clone 点什么进去）。一旦某个 checkpoint 路径落在这样一个嵌套仓内部，
/// `git status` 对指向嵌套仓内部的 pathspec 恒吐空、退出码 0——不是「无变化」，是外层
/// 仓库的 status 压根不下钻进这条边界。修复前，空输出被直接当「干净」放行；修复后必须
/// 用 `git ls-files --error-unmatch` 核实索引真跟踪与否，未跟踪 + 磁盘上有文件 → 判脏。
#[test]
fn checkpoint_path_dirty_states_treats_nested_git_repo_blind_spot_as_dirty() {
    let _env_lock = super::super::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Nested Repo Test"]).unwrap();
    run_git(repo, &["config", "user.email", "nested@example.com"]).unwrap();
    run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
    run_git(repo, &["commit", "--allow-empty", "-qm", "init"]).unwrap();

    // agent 在全新空子目录里 git init（本刀现场复现的常规动作）。
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    run_git(&sub, &["init", "-q"]).unwrap();
    std::fs::write(sub.join("file.txt"), "never committed\n").unwrap();

    let canonical_repo = std::fs::canonicalize(repo).unwrap();
    let checkpoint_paths = vec![canonical_repo.join("sub").join("file.txt")];
    let states = checkpoint_path_dirty_states(repo, &checkpoint_paths).unwrap();

    assert_eq!(states.len(), 1);
    assert!(
        states[0].1,
        "嵌套仓边界内的未提交文件必须判脏——git status 对这条 pathspec 恒吐空，不能被当成干净"
    );
}

/// R-B3 项 4（Minor-4·悬空符号链接 fail-open）：与上一条同款嵌套仓盲区，但账本路径是一个
/// 指向不存在目标的悬空 symlink——`Path::exists()` 跟随链接、解析目标失败会返回 false，
/// 让「没跟踪但磁盘上确实有文件」这条 fail-closed 兜底对悬空 symlink 失效、误判成干净。
/// 改用 `symlink_metadata`（不跟随链接，只问「这个路径本身是否有 inode」）后必须判脏。
#[test]
fn checkpoint_path_dirty_states_treats_dangling_symlink_in_blind_spot_as_dirty() {
    let _env_lock = super::super::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Dangling Symlink Test"]).unwrap();
    run_git(repo, &["config", "user.email", "dangling@example.com"]).unwrap();
    run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
    run_git(repo, &["commit", "--allow-empty", "-qm", "init"]).unwrap();

    // 同款嵌套仓盲区：agent 在全新空子目录里 git init。
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    run_git(&sub, &["init", "-q"]).unwrap();
    // 账本路径本身是一个悬空 symlink（指向从未存在过的目标）。
    std::os::unix::fs::symlink("never-existed-target", sub.join("dangling.txt")).unwrap();

    let canonical_repo = std::fs::canonicalize(repo).unwrap();
    let checkpoint_paths = vec![canonical_repo.join("sub").join("dangling.txt")];
    let states = checkpoint_path_dirty_states(repo, &checkpoint_paths).unwrap();

    assert_eq!(states.len(), 1);
    assert!(
        states[0].1,
        "嵌套仓盲区里的悬空 symlink 必须判脏——path.exists() 跟随链接会因目标不存在而误判成干净"
    );
}

/// 正例配对（别把闸门修成永远关死）：普通、非嵌套场景下真正已提交、工作树干净的文件，
/// 仍必须判干净——`ls-files --error-unmatch` 核实应当放行，不能因为加了这层核实就统统判脏。
#[test]
fn checkpoint_path_dirty_states_still_reports_committed_files_as_clean() {
    let _env_lock = super::super::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Clean Test"]).unwrap();
    run_git(repo, &["config", "user.email", "clean@example.com"]).unwrap();
    run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
    std::fs::write(repo.join("agent.txt"), "committed content\n").unwrap();
    run_git(repo, &["add", "agent.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "commit agent file"]).unwrap();

    let canonical_repo = std::fs::canonicalize(repo).unwrap();
    let checkpoint_paths = vec![canonical_repo.join("agent.txt")];
    let states = checkpoint_path_dirty_states(repo, &checkpoint_paths).unwrap();

    assert_eq!(states.len(), 1);
    assert!(
        !states[0].1,
        "真正已提交、工作树干净的文件必须仍判干净——嵌套仓修法不能把闸门改成永远关死"
    );
}

/// 边界正例：文件先在外层仓库提交，所在目录之后才变成嵌套仓（orphan tracked file）。
/// 外层仓库索引里的记录不受嵌套 `.git` 影响，`ls-files --error-unmatch` 依然命中——
/// 必须继续判干净，不能被「有嵌套仓就一律判脏」的粗暴修法误伤。
#[test]
fn checkpoint_path_dirty_states_still_clean_when_tracked_dir_later_becomes_nested_repo() {
    let _env_lock = super::super::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Orphan Test"]).unwrap();
    run_git(repo, &["config", "user.email", "orphan@example.com"]).unwrap();
    run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("tracked.txt"), "already committed\n").unwrap();
    run_git(repo, &["add", "sub/tracked.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "commit before nested init"]).unwrap();

    // 之后 agent 在这个已经有被跟踪文件的子目录里 git init。
    run_git(&sub, &["init", "-q"]).unwrap();

    let canonical_repo = std::fs::canonicalize(repo).unwrap();
    let checkpoint_paths = vec![canonical_repo.join("sub").join("tracked.txt")];
    let states = checkpoint_path_dirty_states(repo, &checkpoint_paths).unwrap();

    assert_eq!(states.len(), 1);
    assert!(
        !states[0].1,
        "外层仓库索引里本就跟踪的文件，即使所在目录后来变成嵌套仓，仍必须判干净"
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires running sandbox-exec outside the Codex sandbox"]
fn commit_succeeds_when_git_identity_unset() {
    let _env_lock = super::super::test_home_lock();
    let _config_guard = GitConfigIsolationGuard::install();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    std::fs::write(repo.join("a.txt"), "committed content\n").unwrap();
    let dirs = resolve_git_metadata_dirs(repo).unwrap();
    let (name, email) = resolve_git_author_identity(repo).unwrap();

    let output = run_sandboxed_git_commit(
        repo,
        &dirs.git_dir,
        &dirs.git_common_dir,
        None,
        "identity fallback",
        &name,
        &email,
        &[PathBuf::from("a.txt")],
    )
    .unwrap();

    assert!(output.status.success());
    assert_eq!(
        git_checked_stdout(repo, &["log", "-1", "--format=%an|%ae"])
            .unwrap()
            .trim(),
        "AgentLoom|agentloom@localhost"
    );
}

#[test]
fn local_git_filter_drivers_unions_local_and_worktree_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();
    run_git(&repo, &["config", "extensions.worktreeConfig", "true"]).unwrap();
    run_git(&repo, &["config", "--local", "filter.local.clean", "cat"]).unwrap();
    run_git(
        &repo,
        &["config", "--worktree", "filter.per-wt.process", "cat"],
    )
    .unwrap();
    let git_bin = crate::sandbox::resolve_git_bin().unwrap();
    let empty_home = empty_git_home().unwrap();

    let drivers = local_git_filter_drivers(&git_bin, &repo, &empty_home).unwrap();
    let _ = std::fs::remove_dir(&empty_home);

    assert_eq!(
        drivers,
        ["local".to_string(), "per-wt".to_string()]
            .into_iter()
            .collect()
    );
}

#[test]
fn local_git_filter_drivers_tolerates_unavailable_worktree_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();
    let git_bin = crate::sandbox::resolve_git_bin().unwrap();
    let empty_home = empty_git_home().unwrap();

    let drivers = local_git_filter_drivers(&git_bin, &repo, &empty_home).unwrap();
    let _ = std::fs::remove_dir(&empty_home);

    assert!(drivers.is_empty());
}

#[test]
fn git_write_hardening_disables_fsmonitor_and_sets_safe_environment() {
    assert!(HARDENED_GIT_WRITE_PREFIX.contains(&"core.fsmonitor=false"));
    assert!(!HARDENED_GIT_WRITE_PREFIX.contains(&"core.fsmonitor="));

    let mut command = Command::new("git");
    command.env("VISUAL", "/tmp/evil-editor");
    command.env("DEVELOPER_DIR", "/tmp/evil-developer-dir");
    configure_git_write_environment(&mut command, Path::new("/tmp/empty-git-home"));
    assert!(command
        .get_envs()
        .any(|(key, value)| { key == std::ffi::OsStr::new("VISUAL") && value.is_none() }));
    // DEVELOPER_DIR 能改写 Xcode 转发壳的转发目标（见 sandbox.rs resolve_git_bin
    // 一带的注释）；这里钉住它必须被 env_remove，防止将来有人重排清单时漏删。
    assert!(command
        .get_envs()
        .any(|(key, value)| { key == std::ffi::OsStr::new("DEVELOPER_DIR") && value.is_none() }));
    assert!(command.get_envs().any(|(key, value)| {
        key == std::ffi::OsStr::new("GIT_LITERAL_PATHSPECS")
            && value == Some(std::ffi::OsStr::new("1"))
    }));
}

#[test]
fn sandboxed_git_commit_builds_update_index_and_identity_injected_commit_argvs() {
    let paths = [PathBuf::from("a.txt"), PathBuf::from("dir/b.txt")];
    let add_argv = build_add_argv(&paths);
    let commit_argv = build_commit_argv("safe message", "Real User", "real@example.com", &paths);

    assert_eq!(
        add_argv,
        [
            "update-index",
            "--add",
            "--remove",
            "--",
            "a.txt",
            "dir/b.txt",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>()
    );
    assert_eq!(
        commit_argv,
        [
            "-c",
            "user.name=Real User",
            "-c",
            "user.email=real@example.com",
            "commit",
            "--only",
            "--no-gpg-sign",
            "-m",
            "safe message",
            "--",
            "a.txt",
            "dir/b.txt",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>()
    );
    for argv in [&add_argv, &commit_argv] {
        let separator = argv.iter().position(|arg| arg == "--").unwrap();
        assert!(argv[separator + 1..]
            .iter()
            .map(|arg| arg.as_os_str())
            .eq(paths.iter().map(|path| path.as_os_str())));
    }
    assert!(!commit_argv.iter().any(|arg| arg == "--amend"));
    assert!(!commit_argv.iter().any(|arg| arg == "-F"));
    assert!(!commit_argv.iter().any(|arg| arg == "--template"));
    assert!(!commit_argv.iter().any(|arg| arg == "--gpg-sign"));
    assert_eq!(
        commit_argv
            .iter()
            .filter(|arg| *arg == "safe message")
            .count(),
        1
    );
}

#[test]
fn sandboxed_git_commit_rejects_unsafe_path_shapes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("directory")).unwrap();

    for (path, expected) in [
        (PathBuf::from("directory"), "is a directory"),
        (PathBuf::from("."), "contains '.'"),
        (PathBuf::from(".."), "contains '..'"),
        (PathBuf::from("/absolute.txt"), "must be relative"),
    ] {
        let error =
            validate_sandboxed_commit_inputs(tmp.path(), "Real User", "real@example.com", &[path])
                .unwrap_err();
        assert!(error.contains(expected), "unexpected error: {error}");
    }
}

#[cfg(unix)]
#[test]
fn sandboxed_git_commit_accepts_a_leaf_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("target.txt"), "target\n").unwrap();
    std::os::unix::fs::symlink("target.txt", tmp.path().join("link.txt")).unwrap();

    validate_sandboxed_commit_inputs(
        tmp.path(),
        "Real User",
        "real@example.com",
        &[PathBuf::from("link.txt")],
    )
    .unwrap();
}

#[test]
fn sandboxed_git_commit_rejects_blank_identity_before_spawning() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "content\n").unwrap();

    for (name, email, expected) in [
        ("  ", "real@example.com", "author name"),
        ("Real User", "\t", "author email"),
    ] {
        let error = run_sandboxed_git_commit(
            tmp.path(),
            &tmp.path().join(".git"),
            &tmp.path().join(".git"),
            None,
            "message",
            name,
            email,
            &[PathBuf::from("a.txt")],
        )
        .unwrap_err();
        assert!(error.contains(expected), "unexpected error: {error}");
    }
}

#[test]
fn reject_ignored_exact_paths_checks_gitignore_without_live_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();

    let error = reject_ignored_exact_paths(repo, &[PathBuf::from("x.env")]).unwrap_err();
    assert!(error.contains("x.env"), "unexpected error: {error}");
    assert!(
        error.contains("ignored by .gitignore"),
        "unexpected error: {error}"
    );

    reject_ignored_exact_paths(repo, &[PathBuf::from("a.txt")]).unwrap();
}

#[test]
fn reject_ignored_exact_paths_does_not_deadlock_on_large_ignored_output() {
    // Repo has a real HEAD (a committed, unrelated file) so this actually exercises the
    // missing-path partition + `head_tracked_subset` chunking path added for the deletion
    // exemption, rather than short-circuiting on an unborn HEAD without ever touching
    // `ls-tree` (which is what an empty, commit-less repo would do — that used to be this
    // test's setup, and it made the test blind to the new code entirely).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("unrelated.txt"), b"unrelated\n").unwrap();
    run_git(repo, &["add", "--", "unrelated.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();
    std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
    let paths = (0..5_000)
        .map(|index| PathBuf::from(format!("ignored-{index}.env")))
        .collect::<Vec<_>>();

    let error = reject_ignored_exact_paths(repo, &paths).unwrap_err();
    assert!(error.contains("ignored-0.env"), "unexpected error: {error}");
    assert!(
        error.contains("ignored by .gitignore"),
        "unexpected error: {error}"
    );
}

#[test]
fn reject_ignored_exact_paths_exempts_head_tracked_deletion_staged_removal() {
    // A file that WAS tracked, now has a matching `.gitignore` rule added after the fact,
    // and has since had its removal staged (`git rm --cached`, mirroring what `git rm`
    // would do to the index) as well as being gone from disk: this is a legitimate
    // deletion and must not be blocked by the ignore wall.
    //
    // The index state matters here, not just the disk state: empirically, `git
    // check-ignore` reports a path as NOT ignored as long as the index still has an entry
    // for it, regardless of `.gitignore` content — so a plain `rm` (leaving the stale
    // index entry in place) already returns "not ignored" with no help from this
    // function's exemption logic at all. Only once the index entry is also gone does
    // `check-ignore` start reporting "ignored", which is the scenario that actually needs
    // (and exercises) the HEAD-tracked-deletion exemption. See
    // `reject_ignored_exact_paths_exempts_head_tracked_deletion_disk_only_predates_fix`
    // below for the weaker, pre-existing-behavior variant.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("secret.env"), b"secret\n").unwrap();
    run_git(repo, &["add", "--", "secret.env"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();
    std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
    run_git(repo, &["rm", "--cached", "-q", "--", "secret.env"]).unwrap();
    std::fs::remove_file(repo.join("secret.env")).unwrap();

    reject_ignored_exact_paths(repo, &[PathBuf::from("secret.env")]).unwrap();
}

#[test]
fn reject_ignored_exact_paths_exempts_head_tracked_deletion_disk_only_predates_fix() {
    // Weaker sibling of the staged-removal test above: only the working-tree file is
    // removed, the index entry is left untouched. This passes even on the pre-fix
    // implementation (`git check-ignore` already treats an index-tracked path as "not
    // ignored" on its own) — kept as a regression guard for that pre-existing behavior,
    // NOT as coverage for this round's exemption logic.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("secret.env"), b"secret\n").unwrap();
    run_git(repo, &["add", "--", "secret.env"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();
    std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
    std::fs::remove_file(repo.join("secret.env")).unwrap();

    reject_ignored_exact_paths(repo, &[PathBuf::from("secret.env")]).unwrap();
}

#[test]
fn reject_ignored_exact_paths_still_rejects_untracked_missing_ignored_path() {
    // Same shape as the exemption above (missing from disk, matches `.gitignore`), but
    // this path was never tracked in HEAD. It must still be rejected — the exemption is
    // specifically for proven deletions, not for "absent" in general.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    run_git(repo, &["init", "-q"]).unwrap();
    run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
    run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
    std::fs::write(repo.join("tracked.txt"), b"tracked\n").unwrap();
    run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
    run_git(repo, &["commit", "-qm", "base"]).unwrap();
    std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();

    let error =
        reject_ignored_exact_paths(repo, &[PathBuf::from("never-tracked.env")]).unwrap_err();
    assert!(
        error.contains("ignored by .gitignore"),
        "unexpected error: {error}"
    );
}

#[test]
fn sandboxed_git_commit_rejects_empty_exact_paths_before_spawning() {
    let error = run_sandboxed_git_commit(
        Path::new("/does/not/exist"),
        Path::new("/does/not/exist/.git"),
        Path::new("/does/not/exist/.git"),
        None,
        "message",
        "Real User",
        "real@example.com",
        &[],
    )
    .unwrap_err();

    assert_eq!(error, "sandboxed commit requires at least one path");
}

// Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
// sandbox-exec is unavailable there. This is the live proof for the narrow write cage.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn sandboxed_git_commit_injects_identity_without_extra_worktree_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();
    run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
    run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
    std::fs::write(repo.join("a.txt"), "committed content\n").unwrap();
    let dirs = resolve_git_metadata_dirs(&repo).unwrap();

    let output = run_sandboxed_git_commit(
        &repo,
        &dirs.git_dir,
        &dirs.git_common_dir,
        None,
        "live-test",
        "Real User",
        "real@example.com",
        &[PathBuf::from("a.txt")],
    )
    .unwrap();
    assert!(
        output.status.success(),
        "sandboxed git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git_checked_stdout(&repo, &["log", "-1", "--format=%s"])
            .unwrap()
            .trim(),
        "live-test"
    );
    assert_eq!(
        git_checked_stdout(&repo, &["log", "-1", "--format=%an|%ae"])
            .unwrap()
            .trim(),
        "Real User|real@example.com"
    );
    assert_eq!(
        git_checked_stdout(&repo, &["log", "-1", "--format=%cn|%ce"])
            .unwrap()
            .trim(),
        "Real User|real@example.com"
    );
    assert_eq!(
        git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
            .unwrap()
            .trim(),
        "a.txt"
    );
    let worktree_entries = std::fs::read_dir(&repo)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != ".git")
        .collect::<Vec<_>>();
    assert_eq!(
        worktree_entries,
        [std::ffi::OsString::from("a.txt")],
        "commit must not create extra worktree files"
    );
}

// Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
// sandbox-exec is unavailable there. The ignored-path refusal happens before staging.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn sandboxed_git_commit_rejects_gitignored_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();
    std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
    std::fs::write(repo.join("x.env"), "secret\n").unwrap();
    let dirs = resolve_git_metadata_dirs(&repo).unwrap();

    let error = run_sandboxed_git_commit(
        &repo,
        &dirs.git_dir,
        &dirs.git_common_dir,
        None,
        "must-not-commit",
        "Real User",
        "real@example.com",
        &[PathBuf::from("x.env")],
    )
    .unwrap_err();

    assert!(error.contains("x.env"), "unexpected error: {error}");
    assert!(
        error.contains("ignored by .gitignore"),
        "unexpected error: {error}"
    );
    assert!(!git_ok(&repo, &["rev-parse", "--verify", "HEAD"]));
}

// Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
// sandbox-exec is unavailable there. This proves a repository hook cannot execute.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn sandboxed_git_commit_commits_without_executing_pre_commit_hook() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();
    run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
    run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
    let marker = repo.join(".git/hook-ran");
    let hook = repo.join(".git/hooks/pre-commit");
    std::fs::write(&hook, format!("#!/bin/sh\n: > \"{}\"\n", marker.display())).unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();
    std::fs::write(repo.join("a.txt"), "committed content\n").unwrap();
    let dirs = resolve_git_metadata_dirs(&repo).unwrap();

    let output = run_sandboxed_git_commit(
        &repo,
        &dirs.git_dir,
        &dirs.git_common_dir,
        None,
        "hook-live-test",
        "Test User",
        "test@example.com",
        &[PathBuf::from("a.txt")],
    )
    .unwrap();

    assert!(
        output.status.success(),
        "sandboxed git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
            .unwrap()
            .trim(),
        "a.txt"
    );
    assert!(!marker.exists(), "pre-commit hook created its marker");
}

// Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
// sandbox-exec is unavailable there. This proves path-limited commit preserves the user's
// unrelated staged content while committing an agent-created file.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn sandboxed_git_commit_preserves_unrelated_staged_content() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();
    run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
    run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "--", "base.txt"]).unwrap();
    run_git(&repo, &["commit", "-qm", "base"]).unwrap();

    std::fs::write(repo.join("staged.txt"), "user staged content\n").unwrap();
    run_git(&repo, &["add", "--", "staged.txt"]).unwrap();
    std::fs::write(repo.join("new.txt"), "agent content\n").unwrap();
    let dirs = resolve_git_metadata_dirs(&repo).unwrap();

    let output = run_sandboxed_git_commit(
        &repo,
        &dirs.git_dir,
        &dirs.git_common_dir,
        None,
        "agent commit",
        "Test User",
        "test@example.com",
        &[PathBuf::from("new.txt")],
    )
    .unwrap();

    assert!(output.status.success());
    assert_eq!(
        git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
            .unwrap()
            .trim(),
        "new.txt"
    );
    assert_eq!(
        git_checked_stdout(&repo, &["diff", "--cached", "--name-only"])
            .unwrap()
            .trim(),
        "staged.txt"
    );
}

// Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
// sandbox-exec is unavailable there. This proves an unborn repository can commit a new file.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn sandboxed_git_commit_can_create_initial_commit_with_new_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]).unwrap();
    run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
    run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
    std::fs::write(repo.join("new.txt"), "initial content\n").unwrap();
    assert!(!git_ok(&repo, &["rev-parse", "--verify", "HEAD"]));
    let dirs = resolve_git_metadata_dirs(&repo).unwrap();

    let output = run_sandboxed_git_commit(
        &repo,
        &dirs.git_dir,
        &dirs.git_common_dir,
        None,
        "initial commit",
        "Test User",
        "test@example.com",
        &[PathBuf::from("new.txt")],
    )
    .unwrap();

    assert!(output.status.success());
    assert_eq!(
        git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
            .unwrap()
            .trim(),
        "new.txt"
    );
}
