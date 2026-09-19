#![cfg(test)]

use super::*;

#[test]
fn artifact_diff_text_returns_unified_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);

    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = rev_parse_head(repo).unwrap();

    std::fs::write(repo.join("base.txt"), "base\nnext\n").unwrap();
    std::fs::write(repo.join("added.txt"), "fresh\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "head"]);
    let head = rev_parse_head(repo).unwrap();

    let diff = artifact_diff_text(repo, &base, &head).unwrap();
    assert!(diff.contains("+next"), "diff 应含新增行：{diff}");
    assert!(diff.contains("base.txt"), "diff 应含文件名：{diff}");
}

#[test]
fn synthesize_hard_fields_from_worktree_diff() {
    let tmp = tempfile::tempdir().unwrap();
    git_checked(tmp.path(), &["init", "-q"]);
    git_checked(tmp.path(), &["config", "user.email", "t@t"]);
    git_checked(tmp.path(), &["config", "user.name", "t"]);
    git_checked(tmp.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git_checked(tmp.path(), &["add", "a.txt"]);
    git_checked(tmp.path(), &["commit", "-qm", "base"]);
    let base_sha = rev_parse_head(tmp.path()).unwrap();

    std::fs::write(tmp.path().join("a.txt"), "base\ntracked\n").unwrap();
    git_checked(tmp.path(), &["add", "a.txt"]);
    std::fs::write(tmp.path().join("b.txt"), "untracked\nsecond\n").unwrap();

    let (files, anchor) = synthesize_hard_fields(tmp.path(), &base_sha);

    let tracked = files
        .iter()
        .find(|f| f.path == "a.txt")
        .expect("staged tracked a.txt change should be included");
    assert_eq!(tracked.insertions, 1);
    assert_eq!(tracked.deletions, 0);

    let untracked = files
        .iter()
        .find(|f| f.path == "b.txt")
        .expect("untracked b.txt change should be included");
    assert_eq!(untracked.insertions, 2);
    assert_eq!(untracked.deletions, 0);

    assert_eq!(anchor.base_sha, base_sha);
    assert_eq!(anchor.generated_from, "worktree_diff");
    assert!(anchor.head_sha.is_none());
    assert!(anchor.diff_ref.is_none());
}

#[test]
fn review_sees_committed_uncommitted_untracked() {
    let base = std::env::temp_dir().join(format!("agentloom-rev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let root = base.join("wt-root");
    mk_repo(&repo);

    // 没建 worktree → 无可审
    assert!(!review_in(&root, &repo, "s1").unwrap().has_changes);

    let wt = ensure_worktree_in(&root, &repo, "s1").unwrap();
    // committed 改动
    std::fs::write(wt.join("a.txt"), "hello\n").unwrap();
    git(&wt, &["add", "a.txt"]);
    git(&wt, &["commit", "-q", "-m", "add a"]);
    // uncommitted 改动
    std::fs::write(wt.join("a.txt"), "hello world\n").unwrap();
    // untracked 文件
    std::fs::write(wt.join("b.txt"), "brand new\n").unwrap();

    let r = review_in(&root, &repo, "s1").unwrap();
    assert!(r.has_changes);
    assert!(r.patch.contains("a.txt"), "应含已改文件 a.txt");
    assert!(r.patch.contains("hello world"), "应含未提交内容");
    assert!(r.patch.contains("b.txt"), "应含未跟踪文件 b.txt");
    assert!(r.patch.contains("brand new"), "应含未跟踪文件内容");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn review_scoped_excludes_many_unattributed_files_and_is_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    std::fs::write(repo.join("AttributedCase.TXT"), "base\n").unwrap();
    git_checked(&repo, &["add", "AttributedCase.TXT"]);
    git_checked(&repo, &["commit", "-qm", "tracked base"]);
    let base = git_checked(&repo, &["rev-parse", "HEAD"]);
    let base = base.trim();

    std::fs::write(repo.join("AttributedCase.TXT"), "attributed tracked\n").unwrap();
    std::fs::write(repo.join("attributed-new.txt"), "attributed untracked\n").unwrap();
    for index in 0..105 {
        std::fs::write(
            repo.join(format!("unrelated-{index:03}.txt")),
            format!("unrelated {index}\n"),
        )
        .unwrap();
    }

    let status_before = git_checked(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let head_before = git_checked(&repo, &["rev-parse", "HEAD"]);
    let review = review_scoped(
        &repo,
        base,
        &[
            repo.join("AttributedCase.TXT"),
            PathBuf::from("attributed-new.txt"),
        ],
    )
    .unwrap();

    assert!(review.has_changes);
    assert_eq!(review.files_changed, 2);
    assert_eq!(
        review
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["AttributedCase.TXT", "attributed-new.txt"])
    );
    assert!(review.patch.contains("attributed tracked"));
    assert!(review.patch.contains("attributed untracked"));
    assert!(review.patch.contains("--- /dev/null"));
    assert!(review.patch.contains("+++ b/attributed-new.txt"));
    assert!(!review.patch.contains("unrelated-000.txt"));
    assert!(review.files.iter().all(|file| !file.undoable));
    assert_eq!(
        count_unattributed_dirty(
            &repo,
            &[
                repo.join("AttributedCase.TXT"),
                PathBuf::from("attributed-new.txt"),
            ],
        )
        .unwrap(),
        105
    );

    assert_eq!(
        git_checked(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        status_before
    );
    assert_eq!(git_checked(&repo, &["rev-parse", "HEAD"]), head_before);
    assert!(repo.join("unrelated-000.txt").exists());
    assert!(repo.join("unrelated-104.txt").exists());
}

#[test]
fn unreadable_no_index_path_is_excluded_from_review_files_and_count() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let mut patch = String::new();
    let mut files = Vec::new();
    let mut file_keys = std::collections::HashSet::new();

    let files_changed = append_untracked_review_files(
        &repo,
        &["deleted-before-diff.txt".to_string()],
        false,
        &mut patch,
        &mut files,
        &mut file_keys,
    )
    .unwrap();

    assert_eq!(files_changed, 0);
    assert!(files.is_empty());
    assert!(patch.is_empty());
}

#[test]
fn review_working_tree_excludes_untracked_file_deleted_after_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    std::fs::write(repo.join("deleted-before-diff.txt"), "vanishing\n").unwrap();
    std::fs::write(repo.join("empty.txt"), "").unwrap();
    let mut removed_during_diff = false;

    let review = review_working_tree_at_with_no_index(&repo, "HEAD", |worktree, path, patch| {
        if path == "deleted-before-diff.txt" {
            std::fs::remove_file(worktree.join(path)).unwrap();
            removed_during_diff = true;
        }
        append_no_index_patch(worktree, path, patch)
    })
    .unwrap();

    assert!(removed_during_diff, "测试必须在扫描后、生成 diff 前删文件");
    assert_eq!(review.files_changed, 1);
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].path, "empty.txt");
    assert!(!review.patch.contains("deleted-before-diff.txt"));
    assert!(
        review.patch.contains("new file mode"),
        "空的新文件仍应保留 metadata patch：{}",
        review.patch
    );
}

#[test]
fn review_scoped_preserves_rename_in_one_batch_and_documents_cross_batch_degradation() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    std::fs::write(repo.join("rename-old-name.txt"), "tracked\n").unwrap();
    git_checked(&repo, &["add", "rename-old-name.txt"]);
    git_checked(&repo, &["commit", "-qm", "add old"]);
    let base = git_checked(&repo, &["rev-parse", "HEAD"]);
    git_checked(&repo, &["mv", "rename-old-name.txt", "rename-new-name.txt"]);
    let attributed = [
        PathBuf::from("rename-old-name.txt"),
        PathBuf::from("rename-new-name.txt"),
    ];

    let single_batch = review_scoped(&repo, base.trim(), &attributed).unwrap();
    assert_eq!(single_batch.files_changed, 1);
    assert_eq!(
        single_batch
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["rename-new-name.txt"]
    );

    let forced_multi_batch =
        review_scoped_with_budget(&repo, base.trim(), &attributed, 16).unwrap();
    // 已知且接受的极端退化：rename 两端跨批后，git 分别报告 delete 与 add。
    assert_eq!(forced_multi_batch.files_changed, 2);
    assert_eq!(
        forced_multi_batch
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["rename-old-name.txt", "rename-new-name.txt"])
    );
}

#[test]
fn review_scoped_default_budget_matches_one_unlimited_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let paths = (0..40)
        .map(|index| format!("ordinary-path-{index:02}.txt"))
        .collect::<Vec<_>>();
    for path in &paths {
        std::fs::write(repo.join(path), "base\n").unwrap();
    }
    let add_args = std::iter::once("add")
        .chain(std::iter::once("--"))
        .chain(paths.iter().map(String::as_str))
        .collect::<Vec<_>>();
    git_checked(&repo, &add_args);
    git_checked(&repo, &["commit", "-qm", "add ordinary paths"]);
    let base = git_checked(&repo, &["rev-parse", "HEAD"]);
    for path in &paths {
        std::fs::write(repo.join(path), format!("changed {path}\n")).unwrap();
    }
    let attributed = paths.iter().map(PathBuf::from).collect::<Vec<_>>();

    let default = review_scoped(&repo, base.trim(), &attributed).unwrap();
    let unlimited = review_scoped_with_budget(&repo, base.trim(), &attributed, usize::MAX).unwrap();

    assert_eq!(default.files_changed, unlimited.files_changed);
    assert_eq!(
        default
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        unlimited
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(default.patch, unlimited.patch);
}

#[test]
fn review_scoped_keeps_a_single_path_that_exceeds_the_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let path = "this-single-path-is-longer-than-budget.txt";
    std::fs::write(repo.join(path), "base\n").unwrap();
    git_checked(&repo, &["add", path]);
    git_checked(&repo, &["commit", "-qm", "add long path"]);
    let base = git_checked(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join(path), "changed\n").unwrap();

    let review = review_scoped_with_budget(&repo, base.trim(), &[PathBuf::from(path)], 8).unwrap();

    assert_eq!(review.files_changed, 1);
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].path, path);
    assert!(review.patch.contains("changed"));
}

#[test]
fn review_scoped_treats_unusual_names_as_literal_pathspecs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let paths = ["-leading.txt", "star*.txt", "with space.txt"];
    let decoy = "star-decoy.txt";
    for path in paths {
        std::fs::write(repo.join(path), "base\n").unwrap();
    }
    std::fs::write(repo.join(decoy), "base\n").unwrap();
    git_checked(
        &repo,
        &[
            "--literal-pathspecs",
            "add",
            "--",
            "-leading.txt",
            "star*.txt",
            "with space.txt",
            decoy,
        ],
    );
    git_checked(&repo, &["commit", "-qm", "add unusual names"]);
    let base = git_checked(&repo, &["rev-parse", "HEAD"]);
    for path in paths {
        std::fs::write(repo.join(path), format!("changed {path}\n")).unwrap();
    }
    std::fs::write(repo.join(decoy), "changed decoy\n").unwrap();

    let review = review_scoped(&repo, base.trim(), &paths.map(PathBuf::from)).unwrap();

    assert_eq!(review.files_changed, 3);
    assert_eq!(
        review
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(paths)
    );
    assert!(!review.files.iter().any(|file| file.path == decoy));
    assert!(!review.patch.contains(decoy));
}

#[test]
fn review_scoped_drops_parent_and_outside_absolute_attributions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    std::fs::write(repo.join("inside.txt"), "base\n").unwrap();
    std::fs::write(repo.join("outside.txt"), "base\n").unwrap();
    git_checked(&repo, &["add", "inside.txt", "outside.txt"]);
    git_checked(&repo, &["commit", "-qm", "add inside"]);
    let base = git_checked(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("inside.txt"), "inside changed\n").unwrap();
    std::fs::write(repo.join("outside.txt"), "repo decoy changed\n").unwrap();
    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, "outside changed\n").unwrap();

    let review = review_scoped(
        &repo,
        base.trim(),
        &[
            PathBuf::from("inside.txt"),
            PathBuf::from("../outside.txt"),
            outside,
        ],
    )
    .unwrap();

    assert_eq!(review.files_changed, 1);
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].path, "inside.txt");
    assert!(review.patch.contains("inside changed"));
    assert!(!review.files.iter().any(|file| file.path == "outside.txt"));
    assert!(!review.patch.contains("outside.txt"));
}

#[test]
fn review_scoped_combines_committed_and_uncommitted_changes_since_base() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    std::fs::write(repo.join("a.txt"), "base a\n").unwrap();
    std::fs::write(repo.join("b.txt"), "base b\n").unwrap();
    git_checked(&repo, &["add", "a.txt", "b.txt"]);
    git_checked(&repo, &["commit", "-qm", "base"]);
    let base = git_checked(&repo, &["rev-parse", "HEAD"]);

    std::fs::write(repo.join("a.txt"), "base a\ncommitted a\n").unwrap();
    git_checked(&repo, &["add", "a.txt"]);
    git_checked(&repo, &["commit", "-qm", "commit a"]);
    std::fs::write(repo.join("a.txt"), "base a\ncommitted a\nuncommitted a\n").unwrap();
    std::fs::write(repo.join("b.txt"), "base b\nuncommitted b\n").unwrap();

    let review = review_scoped(
        &repo,
        base.trim(),
        &[repo.join("a.txt"), PathBuf::from("b.txt")],
    )
    .unwrap();

    assert_eq!(review.files_changed, 2);
    assert!(review.patch.contains("committed a"));
    assert!(review.patch.contains("uncommitted a"));
    assert!(review.patch.contains("uncommitted b"));
}

#[test]
fn review_scoped_empty_attribution_returns_empty_without_resolving_base() {
    let tmp = tempfile::tempdir().unwrap();
    let review = review_scoped(tmp.path(), "not-a-valid-base", &[]).unwrap();

    assert!(!review.has_changes);
    assert_eq!(review.files_changed, 0);
    assert!(review.files.is_empty());
    assert!(review.diff_available);
}

#[test]
fn count_unattributed_dirty_counts_rename_once_and_is_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    std::fs::write(repo.join("old.txt"), "tracked\n").unwrap();
    git_checked(&repo, &["add", "old.txt"]);
    git_checked(&repo, &["commit", "-qm", "add old"]);
    git_checked(&repo, &["mv", "old.txt", "new.txt"]);
    let status_before = git_checked(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );

    assert_eq!(count_unattributed_dirty(&repo, &[]).unwrap(), 1);
    assert_eq!(
        git_checked(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        status_before
    );
    assert!(repo.join("new.txt").exists());
    assert!(!repo.join("old.txt").exists());
}
#[test]
fn review_default_works_on_default_session_worktree() {
    let base = std::env::temp_dir().join(format!("agentloom-defr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    mark_test_app_domain(&base);
    let sessions_root = base.join("sessions");
    let p = ensure_worktree_for_default_in(&sessions_root, "s1").unwrap();
    // 起手没有改动
    let r0 = review_dispatch_in(&sessions_root, &std::path::PathBuf::new(), "s1", None).unwrap();
    assert!(!r0.has_changes);
    // 加文件 + commit · 再加 untracked
    std::fs::write(p.join("a.txt"), "hi\n").unwrap();
    git(&p, &["add", "a.txt"]);
    git(
        &p,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
    );
    // base ref 是 ensure_worktree_for_default_in 在首建时建的（指向 git init 后空树）
    std::fs::write(p.join("b.txt"), "untracked\n").unwrap();
    let r1 = review_dispatch_in(&sessions_root, &std::path::PathBuf::new(), "s1", None).unwrap();
    assert!(r1.has_changes, "默认 session 也要能 review");
    assert!(r1.patch.contains("a.txt") || r1.patch.contains("b.txt"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn run_numstat_counts_files_insertions_deletions() {
    let base = std::env::temp_dir().join(format!("agentloom-numstat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    // 第一轮 commit
    std::fs::write(base.join("a.txt"), "l1\nl2\n").unwrap();
    git(&base, &["add", "a.txt"]);
    git(&base, &["commit", "-q", "-m", "c1"]);
    let h0 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // 改 a.txt（+1 行）+ 新文件 b.txt（+2 行）
    std::fs::write(base.join("a.txt"), "l1\nl2\nl3\n").unwrap();
    std::fs::write(base.join("b.txt"), "x\ny\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "c2"]);
    let h1 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let n = run_numstat(&base, &h0, &h1).unwrap();
    assert_eq!(n.files, 2, "应 2 个文件变更");
    assert_eq!(n.insertions, 3, "应 +3 行（a +1, b +2）");
    assert_eq!(n.deletions, 0);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn landing_stats_counts_commits_files_and_lines() {
    let base = std::env::temp_dir().join(format!("agentloom-landing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    std::fs::write(base.join("a.txt"), "l1\n").unwrap();
    git(&base, &["add", "a.txt"]);
    git(&base, &["commit", "-q", "-m", "c1"]);
    let h0 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    std::fs::write(base.join("a.txt"), "l1\nl2\nl3\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "c2"]);
    let h1 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let stats = landing_stats(&base, &h0, &h1).unwrap();
    assert_eq!(stats.commit_count, 1);
    assert_eq!(stats.files_changed, 1);
    assert_eq!(stats.insertions, 2);
    assert_eq!(stats.deletions, 0);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn run_numstat_counts_deletions() {
    // Task 13D：现有两测 del 都 =0；这里造一次删行的 commit，断言 del>0。
    let base = std::env::temp_dir().join(format!("agentloom-numdel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    // c1：3 行
    std::fs::write(base.join("a.txt"), "l1\nl2\nl3\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "c1"]);
    let h0 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // c2：删 2 行（只留 l1）
    std::fs::write(base.join("a.txt"), "l1\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "c2"]);
    let h1 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let n = run_numstat(&base, &h0, &h1).unwrap();
    assert_eq!(n.files, 1, "应 1 个文件变更");
    assert_eq!(n.insertions, 0, "无新增行");
    assert_eq!(n.deletions, 2, "应 -2 行（l2/l3 删掉）");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn run_numstat_binary_counts_zero_lines_but_counts_file() {
    let base = std::env::temp_dir().join(format!("agentloom-numbin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    let h0 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // 写一个含 NUL 的「二进制」文件 → git numstat 给 -\t-\t
    std::fs::write(base.join("bin.dat"), [0u8, 1, 2, 0, 3]).unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "bin"]);
    let h1 = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let n = run_numstat(&base, &h0, &h1).unwrap();
    assert_eq!(n.files, 1, "binary 文件计入 files");
    assert_eq!(n.insertions, 0, "binary 行不计 insertions");
    assert_eq!(n.deletions, 0, "binary 行不计 deletions");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn review_reports_files_changed_count() {
    let base = std::env::temp_dir().join(format!("agentloom-revfc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let root = base.join("wt-root");
    mk_repo(&repo);
    let wt = ensure_worktree_in(&root, &repo, "s1").unwrap();
    // 2 个改动文件：1 个 committed 改动 + 1 个 untracked
    std::fs::write(wt.join("a.txt"), "hello\n").unwrap();
    git(&wt, &["add", "a.txt"]);
    git(&wt, &["commit", "-q", "-m", "add a"]);
    std::fs::write(wt.join("b.txt"), "brand new\n").unwrap();

    let r = review_in(&root, &repo, "s1").unwrap();
    assert!(r.has_changes);
    assert_eq!(
        r.files_changed, 2,
        "应数出 2 个变更文件（a tracked + b untracked）"
    );

    // 无改动 → 0
    let base2 = std::env::temp_dir().join(format!("agentloom-revfc0-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base2);
    let repo2 = base2.join("repo");
    let root2 = base2.join("wt-root");
    mk_repo(&repo2);
    ensure_worktree_in(&root2, &repo2, "s2").unwrap();
    let r0 = review_in(&root2, &repo2, "s2").unwrap();
    assert_eq!(r0.files_changed, 0);

    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&base2);
}
