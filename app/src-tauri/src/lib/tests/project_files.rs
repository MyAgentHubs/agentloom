#![cfg(test)]

use super::*;

#[test]
fn stderr_tail_thread_keeps_tail_and_writes_log() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("agent.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap();
    let input = std::io::Cursor::new(format!(
        "{}\nfinal quota failure\n",
        "x".repeat(STDERR_TAIL_LIMIT + 128)
    ));

    let tail = spawn_stderr_tail_thread(input, Some(log)).join().unwrap();

    assert!(tail.contains("final quota failure"), "{tail}");
    assert!(
        tail.len() <= STDERR_TAIL_LIMIT + "final quota failure\n".len(),
        "tail should stay bounded: {}",
        tail.len()
    );
    let logged = std::fs::read_to_string(log_path).unwrap();
    assert!(logged.contains("final quota failure"));
    assert!(logged.starts_with("xxx"));
}

#[test]
fn list_project_files_skips_heavy_dirs_and_keeps_relative_paths() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::create_dir_all(tmp.path().join("node_modules/pkg")).unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "# Readme\n").unwrap();
    std::fs::write(tmp.path().join("src/main.ts"), "console.log(1)\n").unwrap();
    std::fs::write(tmp.path().join("node_modules/pkg/index.js"), "skip\n").unwrap();
    std::fs::write(tmp.path().join(".git/config"), "skip\n").unwrap();

    let listing = list_project_files(tmp.path()).unwrap();
    let paths: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();

    assert!(paths.contains(&"README.md"), "{paths:?}");
    assert!(paths.contains(&"src"), "{paths:?}");
    assert!(paths.contains(&"src/main.ts"), "{paths:?}");
    assert!(!paths.iter().any(|path| path.starts_with("node_modules")));
    assert!(!paths.iter().any(|path| path.starts_with(".git")));
    assert!(!listing.truncated);
}

#[test]
fn list_project_files_includes_gitignored_agent_artifacts_but_skips_heavy_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    git_cmd(tmp.path(), &["init", "-q"]);
    std::fs::write(tmp.path().join(".gitignore"), "*.png\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("assets")).unwrap();
    std::fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
    std::fs::write(tmp.path().join("assets/chart.png"), "chart\n").unwrap();
    std::fs::write(tmp.path().join("README.md"), "# Readme\n").unwrap();
    std::fs::write(tmp.path().join("node_modules/foo.js"), "skip\n").unwrap();

    let listing = list_project_files(tmp.path()).unwrap();
    let paths: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();

    assert!(paths.contains(&"assets/chart.png"), "{paths:?}");
    assert!(paths.contains(&"README.md"), "{paths:?}");
    assert!(!paths.contains(&"node_modules/foo.js"), "{paths:?}");
}

// 契约反转：agent 产物常落在被忽略路径导致 app 内找不到；重目录仍由 skip_project_dir 兜底。
#[test]
fn list_project_files_includes_gitignored_but_skips_heavy_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    git_cmd(tmp.path(), &["init", "-q"]);
    std::fs::write(tmp.path().join(".gitignore"), "ignored_top/\n*.log\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("ignored_top")).unwrap();
    std::fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
    std::fs::write(tmp.path().join("ignored_top/x.txt"), "x\n").unwrap();
    std::fs::write(tmp.path().join("debug.log"), "log\n").unwrap();
    std::fs::write(tmp.path().join("node_modules/package.js"), "skip\n").unwrap();
    std::fs::write(tmp.path().join("README.md"), "# Readme\n").unwrap();

    // 嵌套 .gitignore 也只作为普通文件展示，不再过滤同目录条目。
    std::fs::create_dir_all(tmp.path().join("nested")).unwrap();
    std::fs::write(tmp.path().join("nested/.gitignore"), "local_ignored.txt\n").unwrap();
    std::fs::write(tmp.path().join("nested/local_ignored.txt"), "skip\n").unwrap();
    std::fs::write(tmp.path().join("nested/kept.txt"), "keep\n").unwrap();

    let listing = list_project_files(tmp.path()).unwrap();
    let paths: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();

    assert!(paths.contains(&"ignored_top/x.txt"), "{paths:?}");
    assert!(paths.contains(&"debug.log"), "{paths:?}");
    assert!(paths.contains(&"README.md"), "{paths:?}");
    assert!(paths.contains(&"nested"), "{paths:?}");
    assert!(paths.contains(&"nested/kept.txt"), "{paths:?}");
    assert!(paths.contains(&"nested/local_ignored.txt"), "{paths:?}");
    assert!(
        !paths.iter().any(|p| p.starts_with("node_modules")),
        "{paths:?}"
    );
    // .gitignore 本身是普通受版本控制文件，应照常展示。
    assert!(paths.contains(&".gitignore"), "{paths:?}");
}

#[test]
fn list_project_files_non_git_dir_lists_everything() {
    let tmp = tempfile::tempdir().unwrap();
    // 不 git init：即便存在同名规则文件，也和 git 仓库一样展示全部普通文件。
    std::fs::write(tmp.path().join(".gitignore"), "should_still_show.txt\n").unwrap();
    std::fs::write(tmp.path().join("should_still_show.txt"), "content\n").unwrap();

    let listing = list_project_files(tmp.path()).unwrap();
    let paths: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();

    assert!(paths.contains(&"should_still_show.txt"), "{paths:?}");
    assert!(paths.contains(&".gitignore"), "{paths:?}");
    assert!(!listing.truncated);
}

#[test]
fn list_project_files_over_quota_keeps_top_level_and_marks_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    // 一个深大子树吃掉大部分配额……
    std::fs::create_dir_all(tmp.path().join("big")).unwrap();
    for i in 0..1200 {
        std::fs::write(tmp.path().join(format!("big/f{i}.txt")), "x\n").unwrap();
    }
    // ……但顶层这几个条目必须仍然全部在列。
    for name in ["a_top.txt", "b_top.txt", "c_top.txt"] {
        std::fs::write(tmp.path().join(name), "x\n").unwrap();
    }
    std::fs::create_dir_all(tmp.path().join("z_top_dir")).unwrap();
    std::fs::write(tmp.path().join("z_top_dir/inner.txt"), "x\n").unwrap();

    let listing = list_project_files(tmp.path()).unwrap();
    assert!(listing.truncated);
    assert!(listing.entries.len() <= PROJECT_FILE_MAX_ENTRIES);
    let top_level: Vec<_> = listing
        .entries
        .iter()
        .filter(|entry| entry.depth == 0)
        .map(|entry| entry.path.as_str())
        .collect();
    for expected in ["a_top.txt", "b_top.txt", "c_top.txt", "big", "z_top_dir"] {
        assert!(top_level.contains(&expected), "{top_level:?}");
    }
}

#[test]
fn list_project_files_under_quota_not_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "# Readme\n").unwrap();
    std::fs::write(tmp.path().join("src/main.ts"), "console.log(1)\n").unwrap();

    let listing = list_project_files(tmp.path()).unwrap();
    assert!(!listing.truncated);
    assert_eq!(listing.entries.len(), 3);
}

#[test]
fn list_project_files_keeps_parent_child_adjacency_in_dfs_order() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("a_dir/sub")).unwrap();
    std::fs::create_dir_all(tmp.path().join("z_dir")).unwrap();
    std::fs::write(tmp.path().join("b.txt"), "b\n").unwrap();
    std::fs::write(tmp.path().join("a_dir/x.txt"), "x\n").unwrap();
    std::fs::write(tmp.path().join("a_dir/sub/y.txt"), "y\n").unwrap();
    std::fs::write(tmp.path().join("z_dir/c.txt"), "c\n").unwrap();

    let listing = list_project_files(tmp.path()).unwrap();
    let paths: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();

    assert_eq!(
        paths,
        vec![
            "a_dir",
            "a_dir/sub",
            "a_dir/sub/y.txt",
            "a_dir/x.txt",
            "z_dir",
            "z_dir/c.txt",
            "b.txt",
        ],
        "expected front-to-back DFS order with dirs-before-files per directory"
    );

    let depths: Vec<_> = listing.entries.iter().map(|entry| entry.depth).collect();
    assert_eq!(depths, vec![0, 1, 2, 1, 0, 1, 0]);
}

#[test]
fn read_project_file_blocks_escape_and_identifies_markdown() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("README.md"), "# Title\n").unwrap();

    let readme = read_project_file(tmp.path(), "README.md").unwrap();
    assert_eq!(readme.path, "README.md");
    assert_eq!(readme.name, "README.md");
    assert!(readme.is_markdown);
    assert_eq!(readme.content, "# Title\n");

    let err = read_project_file(tmp.path(), "../README.md").unwrap_err();
    assert_eq!(err, "AL_ERR:file.pathOutOfBounds");
}

#[test]
fn list_and_read_repo_files_use_repo_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "# Repo\n").unwrap();
    std::fs::write(tmp.path().join("src/main.ts"), "console.log(1)\n").unwrap();

    let conn = crate::test_support::mem_db();
    repos_repo::add_repo(
        &conn,
        "r-files",
        "local",
        "local",
        None,
        "repo-files",
        tmp.path().to_str().unwrap(),
        None,
    )
    .unwrap();

    let listing = list_repo_files_inner(&conn, "r-files").unwrap();
    let paths: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(paths.contains(&"README.md"), "{paths:?}");
    assert!(paths.contains(&"src/main.ts"), "{paths:?}");

    let readme = read_repo_file_inner(&conn, "r-files", "README.md").unwrap();
    assert_eq!(readme.path, "README.md");
    assert_eq!(readme.content, "# Repo\n");

    let missing = list_repo_files_inner(&conn, "missing").unwrap_err();
    assert_eq!(missing, "AL_ERR:file.repoNotFound");
}
