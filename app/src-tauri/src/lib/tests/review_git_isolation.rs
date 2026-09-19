#![cfg(test)]

use super::*;

#[cfg(unix)]
fn write_executable(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn session_review_inplace_is_read_only_of_user_git_state() {
    #[derive(Debug, PartialEq, Eq)]
    struct GitSnapshot {
        status: String,
        head: String,
        refs: String,
        worktrees: String,
    }

    fn git_read(project: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(project)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn snapshot(project: &std::path::Path) -> GitSnapshot {
        GitSnapshot {
            status: git_read(
                project,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            ),
            head: git_read(project, &["rev-parse", "HEAD"]),
            refs: git_read(project, &["show-ref"]),
            worktrees: git_read(project, &["worktree", "list", "--porcelain"]),
        }
    }

    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-read-only");
    let tracked = project.join("tracked.md");
    let untracked = project.join("untracked.md");
    checkpoint_review_path(&conn, "review-read-only", &tracked);
    checkpoint_review_path(&conn, "review-read-only", &untracked);
    std::fs::write(&tracked, "dirty tracked\n").unwrap();
    std::fs::write(&untracked, "dirty untracked\n").unwrap();
    let before = snapshot(&project);

    let review = session_review_inner(&conn, "review-read-only").unwrap();
    let after = snapshot(&project);

    assert_eq!(
        after, before,
        "Review 不得改 status / HEAD / refs / worktree list"
    );
    assert!(review.has_changes, "只读 Review 仍必须读到真实项目改动");
}

#[cfg(unix)]
#[test]
fn session_review_disables_project_textconv_and_preserves_index() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-textconv");
    checkpoint_review_path(&conn, "review-textconv", &project.join("tracked.md"));

    let textconv = tmp.path().join("textconv.sh");
    let marker = tmp.path().join("textconv-ran");
    write_executable(
            &textconv,
            "#!/bin/sh\ngit -C \"$(dirname \"$0\")/real-project\" add tracked.md\ntouch \"$(dirname \"$0\")/textconv-ran\"\ncat \"$1\"\n",
        );
    std::fs::write(project.join(".gitattributes"), "tracked.md diff=evil\n").unwrap();
    for args in [
        vec![
            "config".to_string(),
            "diff.evil.textconv".to_string(),
            textconv.to_string_lossy().into_owned(),
        ],
        vec!["add".to_string(), ".gitattributes".to_string()],
        vec![
            "commit".to_string(),
            "-qm".to_string(),
            "attributes".to_string(),
        ],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
    }
    std::fs::write(project.join("tracked.md"), "dirty\n").unwrap();
    let index_path = project.join(".git/index");
    let index_before = std::fs::read(&index_path).unwrap();
    let mtime_before = std::fs::metadata(&index_path).unwrap().modified().unwrap();

    let review = session_review_inner(&conn, "review-textconv").unwrap();

    let index_after = std::fs::read(&index_path).unwrap();
    let mtime_after = std::fs::metadata(&index_path).unwrap().modified().unwrap();

    assert!(review.has_changes);
    assert!(
            !marker.exists() && index_after == index_before && mtime_after == mtime_before,
            "Review must disable textconv and preserve the index: marker_exists={}, index_equal={}, mtime_equal={}",
            marker.exists(),
            index_after == index_before,
            mtime_after == mtime_before
        );
}

#[cfg(unix)]
#[test]
fn session_review_disables_project_external_diff() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-external-diff");
    checkpoint_review_path(&conn, "review-external-diff", &project.join("tracked.md"));

    let external = tmp.path().join("external-diff.sh");
    let marker = tmp.path().join("external-diff-ran");
    write_executable(
        &external,
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/external-diff-ran\"\n",
    );
    std::fs::write(project.join(".gitattributes"), "tracked.md diff=evil\n").unwrap();
    for args in [
        ["add", ".gitattributes"].as_slice(),
        ["commit", "-qm", "attributes"].as_slice(),
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    for key in ["diff.external", "diff.evil.command"] {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(["config", key, &external.to_string_lossy()])
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    std::fs::write(project.join("tracked.md"), "dirty\n").unwrap();

    let review = session_review_inner(&conn, "review-external-diff").unwrap();

    assert!(
        !marker.exists(),
        "Review executed project-controlled external diff"
    );
    assert!(review.has_changes);
}

#[cfg(unix)]
#[test]
fn session_review_disables_project_clean_filter() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-clean-filter");
    checkpoint_review_path(&conn, "review-clean-filter", &project.join("tracked.md"));

    let clean_filter = tmp.path().join("clean-filter.sh");
    let marker = tmp.path().join("clean-filter-ran");
    write_executable(
        &clean_filter,
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/clean-filter-ran\"\ncat\n",
    );
    std::fs::write(project.join(".gitattributes"), "tracked.md filter=evil\n").unwrap();
    for args in [
        vec!["add".to_string(), ".gitattributes".to_string()],
        vec![
            "commit".to_string(),
            "-qm".to_string(),
            "attributes".to_string(),
        ],
        vec![
            "config".to_string(),
            "filter.evil.clean".to_string(),
            clean_filter.to_string_lossy().into_owned(),
        ],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
    }
    std::fs::write(project.join("tracked.md"), "dirty\n").unwrap();

    let review = session_review_inner(&conn, "review-clean-filter");

    assert!(
        !marker.exists(),
        "Review executed project-controlled clean filter: marker_exists={}",
        marker.exists()
    );
    assert!(review.unwrap().has_changes);
}

#[cfg(unix)]
#[test]
fn session_review_disables_project_process_filter() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-process-filter");
    checkpoint_review_path(&conn, "review-process-filter", &project.join("tracked.md"));

    let process_filter = tmp.path().join("process-filter.sh");
    let marker = tmp.path().join("process-filter-ran");
    write_executable(
        &process_filter,
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/process-filter-ran\"\nexit 1\n",
    );
    std::fs::write(project.join(".gitattributes"), "tracked.md filter=evil\n").unwrap();
    for args in [
        vec!["add".to_string(), ".gitattributes".to_string()],
        vec![
            "commit".to_string(),
            "-qm".to_string(),
            "attributes".to_string(),
        ],
        vec![
            "config".to_string(),
            "filter.evil.process".to_string(),
            process_filter.to_string_lossy().into_owned(),
        ],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
    }
    std::fs::write(project.join("tracked.md"), "dirty\n").unwrap();

    let review = session_review_inner(&conn, "review-process-filter");

    assert!(
        !marker.exists(),
        "Review executed project-controlled process filter: marker_exists={}",
        marker.exists()
    );
    assert!(review.unwrap().has_changes);
}

#[cfg(unix)]
#[test]
fn session_review_disables_project_fsmonitor_and_hooks_path() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-git-hooks");
    checkpoint_review_path(&conn, "review-git-hooks", &project.join("tracked.md"));

    let fsmonitor = tmp.path().join("fsmonitor.sh");
    let fsmonitor_marker = tmp.path().join("fsmonitor-ran");
    write_executable(
        &fsmonitor,
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/fsmonitor-ran\"\n",
    );
    let hooks = tmp.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let hook_marker = tmp.path().join("hook-ran");
    write_executable(
        &hooks.join("post-index-change"),
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/../hook-ran\"\n",
    );
    for args in [
        ["config", "core.fsmonitor", &fsmonitor.to_string_lossy()],
        ["config", "core.hooksPath", &hooks.to_string_lossy()],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    std::fs::write(project.join("tracked.md"), "dirty\n").unwrap();

    let review = session_review_inner(&conn, "review-git-hooks").unwrap();

    assert!(review.has_changes);
    assert!(
        !fsmonitor_marker.exists() && !hook_marker.exists(),
        "Review executed project-controlled git callbacks: fsmonitor_exists={}, hook_exists={}",
        fsmonitor_marker.exists(),
        hook_marker.exists()
    );
}

#[test]
fn session_review_ignores_already_undone_checkpoint_for_later_shell_edit() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-history");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-history", "run-1", "codex", &base).unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, undone_at, created_at) \
             VALUES ('review-history', 'old-run', ?1, 1, 10, 1)",
        [tracked.to_str().unwrap()],
    )
    .unwrap();
    std::fs::write(&tracked, "later shell edit\n").unwrap();
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(&project, &["commit", "-qm", "later shell commit"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-history",
        "run-1",
        &post,
        Some(1),
        Some(1),
        Some(1),
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-history").unwrap();

    assert!(
        !review_file(&review, "tracked.md").undoable,
        "an already-undone checkpoint cannot prove a later shell edit is undoable"
    );
}

/// 定罪回归（dogfood 实勘 P0）：checkpoint 记录仍是「活跃」（没被显式撤销），但它所属的 run
/// 已经提交，且此后这个文件又被提交过（400+ 行的场景原型）——preimage 已经陈旧，写回会连带
/// 抹掉后续提交的内容。这条记录不该再被标可撤销，即便它本身从未被撤销过。
#[test]
fn session_review_checkpoint_stale_after_file_recommitted_is_not_undoable() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-stale-undo");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-stale-undo", "run-old", "codex", &base).unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('review-stale-undo', 'run-old', ?1, 1, 1)",
        [tracked.to_str().unwrap()],
    )
    .unwrap();
    std::fs::write(&tracked, "run-old edit\n").unwrap();
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(&project, &["commit", "-qm", "run-old commit"]);
    let post_old = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-stale-undo",
        "run-old",
        &post_old,
        Some(1),
        Some(1),
        Some(1),
    )
    .unwrap();

    // 别人（另一个会话，或本会话之外的路径）之后又提交了同一个文件——run-old 的 preimage
    // 已经陈旧：写回会把这次提交的内容也一并抹掉。
    std::fs::write(&tracked, "later commit content worth 400+ lines\n").unwrap();
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(
        &project,
        &["commit", "-qm", "someone else recommits tracked.md"],
    );

    let review = session_review_inner(&conn, "review-stale-undo").unwrap();

    assert!(review.has_changes);
    assert!(
        !review_file(&review, "tracked.md").undoable,
        "run-old 的 preimage 已被后续提交覆盖，不应再标可撤销（写回会抹掉后续提交的内容）"
    );
}

/// 对照组：post_head 之后没有任何人再碰过这个文件——preimage 仍然新鲜，应继续可撤销。
/// 与上面那条一起跑，证明收紧判定不是「run 一旦提交过就全部锁死」的过度收紧。
#[test]
fn session_review_checkpoint_fresh_after_commit_with_no_later_change_is_undoable() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-fresh-undo");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-fresh-undo", "run-1", "codex", &base).unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('review-fresh-undo', 'run-1', ?1, 1, 1)",
        [tracked.to_str().unwrap()],
    )
    .unwrap();
    std::fs::write(&tracked, "run-1 edit\n").unwrap();
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(&project, &["commit", "-qm", "run-1 commit"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-fresh-undo",
        "run-1",
        &post,
        Some(1),
        Some(1),
        Some(1),
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-fresh-undo").unwrap();

    assert!(
        review_file(&review, "tracked.md").undoable,
        "post_head 之后没有人再改过这个文件，preimage 仍然新鲜，应保持可撤销"
    );
}

#[cfg(unix)]
#[test]
fn session_review_symlink_path_cannot_borrow_target_checkpoint() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-alias");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-alias", "run-1", "codex", &base).unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('review-alias', 'run-1', ?1, 1, 1)",
        [tracked.to_str().unwrap()],
    )
    .unwrap();
    std::os::unix::fs::symlink("tracked.md", project.join("alias.md")).unwrap();
    review_test_git(&project, &["add", "alias.md"]);
    review_test_git(&project, &["commit", "-qm", "add alias"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-alias",
        "run-1",
        &post,
        Some(1),
        Some(1),
        Some(0),
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-alias").unwrap();

    assert!(
        !review_file(&review, "alias.md").undoable,
        "a symlink path absent from the ledger cannot borrow its target's checkpoint"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn session_review_checkpoint_case_follows_filesystem_semantics() {
    use std::os::unix::fs::MetadataExt;

    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-case");
    let tracked = project.join("tracked.md");
    let differently_cased = project.join("TRACKED.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-case", "run-1", "codex", &base).unwrap();
    let same_file = std::fs::metadata(&differently_cased)
        .map(|other| {
            let tracked = std::fs::metadata(&tracked).unwrap();
            tracked.dev() == other.dev() && tracked.ino() == other.ino()
        })
        .unwrap_or(false);
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('review-case', 'run-1', ?1, 1, 1)",
        [differently_cased.to_str().unwrap()],
    )
    .unwrap();
    std::fs::remove_file(&tracked).unwrap();
    review_test_git(&project, &["add", "-u", "tracked.md"]);
    review_test_git(&project, &["commit", "-qm", "delete tracked"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-case",
        "run-1",
        &post,
        Some(1),
        Some(0),
        Some(1),
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-case").unwrap();

    assert_eq!(
        review_file(&review, "tracked.md").undoable,
        same_file,
        "undoable path matching must follow the project's filesystem case semantics"
    );
}
