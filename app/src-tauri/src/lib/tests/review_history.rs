#![cfg(test)]

use super::*;

fn review_test_git_output(project: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(project)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn record_review_run_file(
    conn: &rusqlite::Connection,
    project: &std::path::Path,
    session_id: &str,
    run_id: &str,
    path: &str,
    content: &str,
) -> (String, String) {
    let pre = worktree::rev_parse_head(project).unwrap();
    db::insert_run_pending(conn, session_id, run_id, "codex", &pre).unwrap();
    std::fs::write(project.join(path), content).unwrap();
    review_test_git(project, &["add", path]);
    review_test_git(project, &["commit", "-qm", run_id]);
    let post = worktree::rev_parse_head(project).unwrap();
    db::record_run_commit(conn, session_id, run_id, &post, Some(1), Some(1), Some(0)).unwrap();
    (pre, post)
}

#[test]
fn session_review_new_session_excludes_many_historical_untracked_files() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-new-session");
    let historical = project.join("historical");
    std::fs::create_dir_all(&historical).unwrap();
    for index in 0..135 {
        std::fs::write(historical.join(format!("old-{index}.json")), "old\n").unwrap();
    }

    let review = session_review_inner(&conn, "review-new-session").unwrap();

    assert!(!review.has_changes);
    assert!(review.files.is_empty());
    assert!(!review.patch.contains("historical"));
    assert_eq!(review.other_dirty_count, 135);
}

#[test]
fn session_review_checkpoint_scope_excludes_history_and_counts_other_dirty() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-checkpoint-scope");
    let tracked = project.join("tracked.md");
    let added = project.join("added.md");
    checkpoint_review_path(&conn, "review-checkpoint-scope", &tracked);
    checkpoint_review_path(&conn, "review-checkpoint-scope", &added);
    std::fs::write(&tracked, "session edit\n").unwrap();
    std::fs::write(&added, "session addition\n").unwrap();
    let historical = project.join("historical");
    std::fs::create_dir_all(&historical).unwrap();
    for index in 0..135 {
        std::fs::write(historical.join(format!("old-{index}.json")), "old\n").unwrap();
    }

    let review = session_review_inner(&conn, "review-checkpoint-scope").unwrap();

    assert_eq!(review.files_changed, 2);
    assert_eq!(
        review
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["added.md", "tracked.md"])
    );
    assert!(!review.patch.contains("historical"));
    assert_eq!(review.other_dirty_count, 135);
}

#[test]
fn session_review_run_commit_scope_excludes_historical_dirty_files() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-committed-scope");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-committed-scope", "run-1", "codex", &base).unwrap();
    std::fs::write(project.join("tracked.md"), "committed A\n").unwrap();
    std::fs::write(project.join("added.md"), "committed B\n").unwrap();
    review_test_git(&project, &["add", "tracked.md", "added.md"]);
    review_test_git(&project, &["commit", "-qm", "session commit"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-committed-scope",
        "run-1",
        &post,
        Some(2),
        Some(2),
        Some(1),
    )
    .unwrap();
    std::fs::write(project.join("historical.tmp"), "old dirty file\n").unwrap();

    let review = session_review_inner(&conn, "review-committed-scope").unwrap();

    assert_eq!(review.files_changed, 2);
    assert!(review.files.iter().any(|file| file.path == "tracked.md"));
    assert!(review.files.iter().any(|file| file.path == "added.md"));
    assert!(!review.patch.contains("historical.tmp"));
}

#[test]
fn session_review_excludes_other_commits_inside_session_base_to_head() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-own-ranges");
    record_review_run_file(
        &conn,
        &project,
        "review-own-ranges",
        "run-1",
        "a.txt",
        "from session A\n",
    );
    std::fs::write(project.join("b.txt"), "from another source\n").unwrap();
    review_test_git(&project, &["add", "b.txt"]);
    review_test_git(&project, &["commit", "-qm", "another source"]);

    let review = session_review_inner(&conn, "review-own-ranges").unwrap();

    assert_eq!(review.files_changed, 1);
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].path, "a.txt");
    assert!(review.patch.contains("from session A"));
    assert!(!review.patch.contains("b.txt"));
    assert!(!review.patch.contains("from another source"));
}

/// F3 定罪回归：本会话已提交过 X（走 committed range），之后 X 在工作区又被终端/手改，
/// 且这次改动**没有走 checkpoint 记录**——pathspec 若只用 checkpoint_paths，这类改动
/// 既不会出现在 diff 里，也不会被 count_unattributed_dirty 算成「未纳入」，是纯粹的静默
/// 丢失（旧单 base 实现天然显示，因为它就是拿 base..工作区整棵树 diff；本条不是测
/// 「已提交 + 提交后又改」的主路径——那条路径两个文件各表一枝，早已被
/// `session_review_combines_committed_and_later_uncommitted_changes` 覆盖——本条测的是
/// 「又改的是同一个文件、且这次改动没有 checkpoint 记录」这个更窄的子情形）。
#[test]
fn session_review_shows_later_uncommitted_edit_to_a_committed_file_without_checkpoint() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-recommitted-no-checkpoint");
    record_review_run_file(
        &conn,
        &project,
        "review-recommitted-no-checkpoint",
        "run-1",
        "tracked.md",
        "committed by run-1\n",
    );
    // 之后有人直接在工作区改了 tracked.md，未提交、也没有 checkpoint 记录
    // （比如终端 sed，或者 agent 走 shell 而非编辑工具）。
    std::fs::write(
        project.join("tracked.md"),
        "committed by run-1\nuncommitted terminal edit, never checkpointed\n",
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-recommitted-no-checkpoint").unwrap();

    assert!(
        review
            .patch
            .contains("uncommitted terminal edit, never checkpointed"),
        "已提交文件之后的未提交改动不该静默丢失：{}",
        review.patch
    );
    assert_eq!(
        review.other_dirty_count, 0,
        "这个文件属于本会话的归因集合（提交过），不该被算进『未纳入本次 Review』"
    );
}

/// 重叠去重回归（opus 对抗审提出的理论怀疑，实勘已证实会发生）：in-place 的 Team run，
/// lead 通过交付 broker 提交（写 run_commits(H0..H1)），随后前端 coding-loop 的
/// finalize 又把同一批改动记成 landing（同样的 H0..H1 区间）——两条账本指向完全相同的
/// commit range。若不去重，`combine_reviews` 会把同一段 diff 拼两遍，± 行数翻倍。
#[test]
fn session_review_dedups_identical_range_recorded_by_both_run_and_landing_ledger() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-dup-ledger");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-dup-ledger", "run-1", "codex", &base).unwrap();
    std::fs::write(project.join("tracked.md"), "team edit landed twice\n").unwrap();
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(&project, &["commit", "-qm", "broker commit"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-dup-ledger",
        "run-1",
        &post,
        Some(1),
        Some(1),
        Some(1),
    )
    .unwrap();
    // 同一批提交（完全一样的 H0..H1）又被 coding-loop 的 finalize 记成了 landing。
    db::insert_landing_commit(
        &conn,
        &db::LandingCommit {
            id: "landing-dup".into(),
            session_id: "review-dup-ledger".into(),
            run_id: "run-1".into(),
            artifact_id: None,
            pre_head: base,
            landed_head: post,
            commit_count: 1,
            files_changed: 1,
            insertions: 1,
            deletions: 0,
            created_at: 1,
        },
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-dup-ledger").unwrap();

    assert_eq!(
        review.files_changed, 1,
        "同一区间被两套账本重复记录，不该重复计数文件"
    );
    let occurrences = review.patch.matches("team edit landed twice").count();
    assert_eq!(
        occurrences, 1,
        "同一区间被两套账本重复记录，不该让 diff 内容翻倍：{}",
        review.patch
    );
}

/// 定罪回归（dogfood 实勘）：本会话自己的 run 提交了 tracked.md 之后，别的会话/进程
/// 直接对**同一文件**又提交了大段内容——旧实现 `git diff base -- tracked.md` 管不到
/// "中间是谁提交的"，会把后面那段也一并展示（426/507 行污染的根因）。新实现按会话自己
/// 记的 pre_i..post_i 分段求和，天生只含这段范围内真正发生的改动。
#[test]
fn session_review_excludes_other_sessions_later_commit_to_same_file() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-same-file-pollution");
    record_review_run_file(
        &conn,
        &project,
        "review-same-file-pollution",
        "run-1",
        "tracked.md",
        "this session's own edit\n",
    );
    // 别的会话/进程在本会话 post_head 之后又直接提交了同一个文件——不经过 db 记账。
    std::fs::write(
        project.join("tracked.md"),
        "this session's own edit\nalien content merged in by another session\n",
    )
    .unwrap();
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(&project, &["commit", "-qm", "another session's own commit"]);

    let review = session_review_inner(&conn, "review-same-file-pollution").unwrap();

    assert_eq!(
        review.files_changed, 1,
        "同一文件跨段只应算一个改动文件，不应因为两次提交而重复计数"
    );
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].path, "tracked.md");
    assert!(
        review.patch.contains("this session's own edit"),
        "应展示本会话自己那段内容：{}",
        review.patch
    );
    assert!(
        !review
            .patch
            .contains("alien content merged in by another session"),
        "不得展示别的会话在同一文件里后续提交的内容：{}",
        review.patch
    );
}

#[test]
fn session_review_unions_all_recorded_run_commit_ranges() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-many-runs");
    record_review_run_file(
        &conn,
        &project,
        "review-many-runs",
        "run-1",
        "a.txt",
        "run one\n",
    );
    record_review_run_file(
        &conn,
        &project,
        "review-many-runs",
        "run-2",
        "b.txt",
        "run two\n",
    );

    let review = session_review_inner(&conn, "review-many-runs").unwrap();

    assert_eq!(review.files_changed, 2);
    assert!(review.files.iter().any(|file| file.path == "a.txt"));
    assert!(review.files.iter().any(|file| file.path == "b.txt"));
    assert!(review.patch.contains("run one"));
    assert!(review.patch.contains("run two"));
}

#[test]
fn session_review_skips_invalid_range_without_affecting_valid_ranges() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-skip-range");
    let (_, valid_post) = record_review_run_file(
        &conn,
        &project,
        "review-skip-range",
        "run-valid",
        "a.txt",
        "valid run\n",
    );
    record_review_run_file(
        &conn,
        &project,
        "review-skip-range",
        "run-removed",
        "removed.txt",
        "removed history\n",
    );
    review_test_git(
        &project,
        &["checkout", "-q", "-b", "other-source", &valid_post],
    );
    std::fs::write(project.join("other.txt"), "other source\n").unwrap();
    review_test_git(&project, &["add", "other.txt"]);
    review_test_git(&project, &["commit", "-qm", "other source"]);

    let review = session_review_inner(&conn, "review-skip-range").unwrap();

    assert_eq!(review.files_changed, 1);
    assert!(review.files.iter().any(|file| file.path == "a.txt"));
    assert!(!review.files.iter().any(|file| file.path == "removed.txt"));
    assert!(!review.files.iter().any(|file| file.path == "other.txt"));
    assert!(review.patch.contains("valid run"));
    assert!(!review.patch.contains("removed history"));
    assert!(!review.patch.contains("other source"));
}

/// F4 改名（原名 session_review_uses_later_valid_base_when_earliest_range_is_invalid）：
/// 旧名字描述的是「共享 base 选择 fold 跳过无效候选、退到下一个有效 base」——那段选 base
/// 的代码已经在 commit 1 整段删掉了，现在这条测试只是靠分段求和的 `is_ancestor` 过滤
/// 恰好给出同样的可见结果，测试名跟被测行为已经脱节（哪怕把「选 base」相关代码整个删了，
/// 这条测试也不会红）。改回描述分段模型下真正在验证的东西：某个 run 的 range 因为历史被
/// 改写（orphan checkout）而不再是当前 HEAD 的祖先时，这段 range 必须被跳过、不产生任何
/// 内容或报错；后续独立有效的 range 仍应正常展示。
#[test]
fn session_review_skips_a_range_unreachable_from_head_after_history_rewrite() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-later-valid-base");
    record_review_run_file(
        &conn,
        &project,
        "review-later-valid-base",
        "run-old-history",
        "old-history.txt",
        "removed history\n",
    );

    review_test_git(
        &project,
        &["checkout", "-q", "--orphan", "rewritten-history"],
    );
    std::fs::write(project.join("new-root.txt"), "new root\n").unwrap();
    review_test_git(&project, &["add", "new-root.txt"]);
    review_test_git(&project, &["commit", "-qm", "new root"]);
    record_review_run_file(
        &conn,
        &project,
        "review-later-valid-base",
        "run-current-history",
        "current.txt",
        "current history content\n",
    );
    assert!(
        review_test_git_output(&project, &["status", "--porcelain"])
            .trim()
            .is_empty(),
        "回归前提要求工作树干净"
    );

    let review = session_review_inner(&conn, "review-later-valid-base").unwrap();

    assert!(review.has_changes);
    assert_eq!(
        review.files_changed, 1,
        "被改写掉的历史那段 range 不应贡献任何文件"
    );
    assert!(review.files.iter().any(|file| file.path == "current.txt"));
    assert!(
        review.patch.contains("current history content"),
        "被改写掉历史的那个 range 应被跳过，独立有效的后续 range 仍应正常展示：{}",
        review.patch
    );
    assert!(
        !review.patch.contains("removed history"),
        "不可达的 range（pre/post 都在被抛弃的历史分支上）绝不能贡献内容"
    );
}

/// F4 改名（原名 session_review_uses_earlier_run_base_when_landing_starts_later）：
/// 同上，旧名字描述的也是已删除的「选更早 base」逻辑。这条测试实际验证的是分段求和的
/// 核心行为——一个 run range（早）+ 一个 landing range（晚，且不是从同一个起点算的）
/// 各自独立 diff 后按并集合并展示，互不覆盖、互不丢失。
#[test]
fn session_review_unions_a_run_range_and_a_later_landing_range() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-earlier-base");
    let (_, run_post) = record_review_run_file(
        &conn,
        &project,
        "review-earlier-base",
        "run-solo",
        "tracked.md",
        "solo run content\n",
    );
    std::fs::write(project.join("team.txt"), "team run content\n").unwrap();
    review_test_git(&project, &["add", "team.txt"]);
    review_test_git(&project, &["commit", "-qm", "team run"]);
    let landed_post = worktree::rev_parse_head(&project).unwrap();
    db::insert_landing_commit(
        &conn,
        &db::LandingCommit {
            id: "landing-later".into(),
            session_id: "review-earlier-base".into(),
            run_id: "run-team".into(),
            artifact_id: None,
            pre_head: run_post,
            landed_head: landed_post,
            commit_count: 1,
            files_changed: 1,
            insertions: 1,
            deletions: 0,
            created_at: 1,
        },
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-earlier-base").unwrap();

    assert_eq!(review.files_changed, 2);
    assert!(review.files.iter().any(|file| file.path == "tracked.md"));
    assert!(review.files.iter().any(|file| file.path == "team.txt"));
    assert!(
        review.patch.contains("solo run content"),
        "应从更早的 run base 展示 r1 内容：{}",
        review.patch
    );
    assert!(review.patch.contains("team run content"));
}

#[test]
fn session_review_committed_rename_is_attributed_as_one_rename() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-rename");
    std::fs::write(project.join("old.txt"), "same content\n").unwrap();
    review_test_git(&project, &["add", "old.txt"]);
    review_test_git(&project, &["commit", "-qm", "add old"]);
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-rename", "run-1", "codex", &base).unwrap();

    review_test_git(&project, &["mv", "old.txt", "new.txt"]);
    review_test_git(&project, &["commit", "-qm", "rename"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-rename",
        "run-1",
        &post,
        Some(1),
        Some(0),
        Some(0),
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-rename").unwrap();

    assert_eq!(review.files_changed, 1);
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].path, "new.txt");
    assert!(
        review.patch.contains("rename from old.txt") && review.patch.contains("rename to new.txt"),
        "Review 应把改名呈现为一条 rename：{}",
        review.patch
    );
}

#[test]
fn session_review_invalid_run_ledger_never_falls_back_to_whole_tree() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-invalid-ledger");
    db::insert_run_pending(
        &conn,
        "review-invalid-ledger",
        "run-1",
        "codex",
        "missing-pre-head",
    )
    .unwrap();
    db::record_run_commit(
        &conn,
        "review-invalid-ledger",
        "run-1",
        "missing-post-head",
        Some(1),
        Some(1),
        Some(0),
    )
    .unwrap();
    std::fs::write(project.join("historical.tmp"), "old dirty file\n").unwrap();

    let review = session_review_inner(&conn, "review-invalid-ledger").unwrap();

    assert!(!review.has_changes);
    assert!(review.files.is_empty());
    assert!(!review.patch.contains("historical.tmp"));
    assert_eq!(review.other_dirty_count, 1);
}

#[test]
fn session_review_combines_committed_and_later_uncommitted_changes() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-mixed");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-mixed", "run-1", "codex", &base).unwrap();
    std::fs::write(project.join("tracked.md"), "committed A\n").unwrap();
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(&project, &["commit", "-qm", "commit A"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-mixed",
        "run-1",
        &post,
        Some(1),
        Some(1),
        Some(1),
    )
    .unwrap();
    let added = project.join("later-b.md");
    checkpoint_review_path(&conn, "review-mixed", &added);
    std::fs::write(&added, "uncommitted B\n").unwrap();

    let review = session_review_inner(&conn, "review-mixed").unwrap();

    assert!(review.has_changes);
    assert!(review.files.iter().any(|file| file.path == "tracked.md"));
    assert!(review.files.iter().any(|file| file.path == "later-b.md"));
    assert!(review.patch.contains("committed A"));
    assert!(review.patch.contains("uncommitted B"));
}
