#![cfg(test)]

use super::*;

#[test]
fn session_review_inplace_excludes_unattributed_shell_file_from_project() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-untracked");
    std::fs::write(project.join("test.md"), "shell wrote this\n").unwrap();

    let review = session_review_inner(&conn, "review-untracked").unwrap();

    assert!(!review.has_changes);
    assert!(!review.patch.contains("test.md"));
    assert_eq!(review.other_dirty_count, 1);
}

#[test]
fn session_review_inplace_checkpointed_tracked_file_is_undoable() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-edit-tool");
    let tracked = project.join("tracked.md");
    // F2 修复：真实生产流程里，任何 run 开始执行前 prepare_run_ledger 都会先
    // insert_run_pending（run_commits 行先于任何 checkpoint 写入存在）——这里补上，
    // 让夹具跟现实一致；否则查不到匹配的 run_commits 行会被新逻辑 fail-closed。
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-edit-tool", "run-1", "codex", &base).unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES (?1, 'run-1', ?2, 1, 1)",
        rusqlite::params!["review-edit-tool", tracked.to_str().unwrap()],
    )
    .unwrap();
    std::fs::write(&tracked, "edited by tool\n").unwrap();

    let review = session_review_inner(&conn, "review-edit-tool").unwrap();

    assert!(review.has_changes, "编辑工具改 tracked 文件应进入 Review");
    assert!(
        review_file(&review, "tracked.md").undoable,
        "run 仍在跑（running）、pre_head..HEAD 之间没有人碰过这个文件，应保持可撤销"
    );
}

/// R-B2 项 2a（Major-4 接缝测试·新 scope 会话闭环）：NULL scope（方案 A 新行为）会话的
/// agent 实际写文件目录是 per-session 子目录，但 Review 走的是根锚定的
/// `session_review_inner`（`inplace_project_path` = 项目根，不受 scope 影响）；子目录里的
/// 新文件对 git 而言只是仓库内部一个普通嵌套路径的未跟踪文件，`git status`/`diff` 天然能
/// 看到——这条测试证明这条接缝真的接得上，不是「两半各自绿、接缝裸奔」。
#[test]
fn session_review_sees_changes_written_into_new_scope_session_subdir() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-subdir-scope");

    assert_eq!(
        db::get_session_workspace_scope(&conn, "review-subdir-scope").unwrap(),
        None,
        "前提：新建会话应是 NULL scope（新行为）"
    );
    let session_dir = ensure_inplace_session_workdir(&conn, "review-subdir-scope")
        .unwrap()
        .unwrap();
    assert_eq!(
        session_dir,
        project.join("review-subdir-scope"),
        "前提：NULL scope 解析到 per-session 子目录，不是项目根"
    );

    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-subdir-scope", "run-1", "codex", &base).unwrap();
    let new_file = session_dir.join("notes.md");
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES (?1, 'run-1', ?2, 0, 1)",
        rusqlite::params!["review-subdir-scope", new_file.to_str().unwrap()],
    )
    .unwrap();
    std::fs::write(&new_file, "written in per-session subdir\n").unwrap();

    let review = session_review_inner(&conn, "review-subdir-scope").unwrap();

    assert!(
        review.has_changes,
        "子目录里的新文件应该被根锚定 review 看见"
    );
    let expected_path = "review-subdir-scope/notes.md";
    assert!(
        review.files.iter().any(|f| f.path == expected_path),
        "review 应含子目录相对路径 {expected_path}：{:?}",
        review.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
}

/// R-B2 项 2a（Major-4 接缝测试·新 scope 会话闭环）：undo 往返——checkpoint 账本记的是
/// per-session 子目录下的绝对路径（方案 A 新行为），`undo_run_edits_inner` 最终把这些字节
/// 写回磁盘时必须精确命中子目录里的文件，不能因为路径多了一层子目录前缀就撤销失败或
/// 写错地方。
#[test]
fn undo_run_edits_restores_checkpoint_recorded_in_new_scope_session_subdir() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "undo-subdir-scope");

    let session_dir = ensure_inplace_session_workdir(&conn, "undo-subdir-scope")
        .unwrap()
        .unwrap();
    assert_eq!(session_dir, project.join("undo-subdir-scope"));

    // 子目录里已有一份被跟踪的文件（这个会话之前的产物），先提交进项目根的同一个仓库。
    let tracked_in_subdir = session_dir.join("notes.md");
    std::fs::write(&tracked_in_subdir, "original content\n").unwrap();
    review_test_git(&project, &["add", "undo-subdir-scope/notes.md"]);
    review_test_git(&project, &["commit", "-qm", "seed subdir file"]);

    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "undo-subdir-scope", "run-1", "codex", &base).unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("undo-subdir-scope", "run-1", &project, &tracked_in_subdir)
        .unwrap();
    std::fs::write(&tracked_in_subdir, "edited by agent in subdir\n").unwrap();

    let entries = list_run_undo_entries_inner(&conn, "undo-subdir-scope", "run-1").unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry.file_path.ends_with("notes.md"))
        .unwrap_or_else(|| panic!("undo 清单缺子目录里的 notes.md：{entries:?}"));
    assert!(!entry.stale, "刚记录的 preimage 应该新鲜");

    let report = undo_run_edits_inner(
        &conn,
        "undo-subdir-scope",
        "run-1",
        vec![entry.file_path.to_str().unwrap().to_string()],
        vec![entry.current_digest.clone()],
    )
    .unwrap();

    assert_eq!(
        report.restored.len(),
        1,
        "子目录里的 checkpoint 记录必须能正常撤销往返：{report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&tracked_in_subdir).unwrap(),
        "original content\n",
        "撤销后子目录文件内容应恢复成 agent 编辑前的原样"
    );
}

/// F2 定罪回归（走 F1 真正会写回磁盘的那条路径 `list_run_undo_entries`，不是走
/// Review 徽标）：run 仍在跑（running，未走交付 broker 提交）——in-place 下这是
/// **最常见的情形**——之后又有人（无论是不是同一会话）直接提交了这个文件，preimage
/// 已经陈旧。旧版 `filter_fresh_checkpoint_paths` 只要 post_head 是 None 就无条件判
/// 新鲜，对这个最常见的场景完全没设防；改用 pre_head..HEAD 校验后必须能抓到。
/// （用 `list_run_undo_entries_inner` 而非 `session_review_inner`：文件一旦被提交，
/// 既不在任何「已提交 range」的归因集合里（run-1 没有 post_head，不进
/// `run_commit_ranges`），也不再是「未提交」，Review 面板压根不会展示这个文件——但
/// `list_run_undo_entries` 认的是 checkpoint 账本，不管 Review 显不显示都要给出
/// 正确的 stale 判定，这正是 F1 点名「Review 面板根本没有逐文件撤销动作」的落点。）
#[test]
fn list_run_undo_entries_marks_pending_run_stale_after_unrelated_commit() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-pending-stale");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-pending-stale", "run-1", "codex", &base).unwrap();
    // 走真正的 record_preimage（而非裸 SQL insert）：list_undo_entries 需要读实际写在
    // 磁盘上的 preimage blob 才能生成预览，裸插入没有 blob 会直接报错。
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("review-pending-stale", "run-1", &project, &tracked)
        .unwrap();
    std::fs::write(&tracked, "edited by tool, run never committed\n").unwrap();

    // run-1 从未 record_run_commit（state 一直是 running）——但同一个文件被直接
    // git commit 了（不管是谁做的，broker、终端、还是别的会话）。
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(
        &project,
        &["commit", "-qm", "someone commits while run-1 still running"],
    );

    let entries = list_run_undo_entries_inner(&conn, "review-pending-stale", "run-1").unwrap();
    // record_preimage 内部会 canonicalize 路径（macOS 上 /var → /private/var），
    // 跟裸 tracked 比较可能因这层符号链接差一个前缀，按文件名找更稳。
    let entry = entries
        .iter()
        .find(|entry| entry.file_path.ends_with("tracked.md"))
        .unwrap_or_else(|| panic!("undo 清单缺 tracked.md：{entries:?}"));

    assert!(
        entry.stale,
        "run 仍处于 running、pre_head 之后这个文件已经被提交过，preimage 已陈旧，\
             必须标 stale——点撤销会把这次提交的内容覆盖掉"
    );
}

/// 对照组：run 仍在跑、pre_head 之后没有任何人碰过这个文件——preimage 仍然新鲜，
/// `list_run_undo_entries` 不应误伤，证明上一条不是「running 就全锁死」的过度收紧。
#[test]
fn list_run_undo_entries_keeps_pending_run_fresh_without_later_commit() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-pending-fresh");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-pending-fresh", "run-1", "codex", &base).unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("review-pending-fresh", "run-1", &project, &tracked)
        .unwrap();
    std::fs::write(&tracked, "edited by tool, still uncommitted\n").unwrap();

    let entries = list_run_undo_entries_inner(&conn, "review-pending-fresh", "run-1").unwrap();
    // record_preimage 内部会 canonicalize 路径（macOS 上 /var → /private/var），
    // 跟裸 tracked 比较可能因这层符号链接差一个前缀，按文件名找更稳。
    let entry = entries
        .iter()
        .find(|entry| entry.file_path.ends_with("tracked.md"))
        .unwrap_or_else(|| panic!("undo 清单缺 tracked.md：{entries:?}"));

    assert!(
        !entry.stale,
        "pre_head 之后没有人提交过这个文件，preimage 仍然新鲜，不应被误标 stale"
    );
}

/// F1 纵深防御回归：正常 UI 流程走不到这条分支——`list_run_undo_entries` 已经把陈旧
/// 条目标 `stale`，前端据此禁止勾选，压根提交不出这个请求。这条测试模拟「绕过前端，
/// 直接拿 list_run_undo_entries 给出的 path/digest 去调 undo_run_edits_inner」，
/// 证明即便如此，真正会把 preimage 字节写回磁盘的 `undo_run_edits_inner` 自己也会
/// 拦下来——不能只靠前端隐藏按钮，得让这个行为本身消失。
#[test]
fn undo_run_edits_inner_refuses_to_restore_a_stale_path_even_if_ui_check_is_bypassed() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "undo-stale-defense");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "undo-stale-defense", "run-1", "codex", &base).unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("undo-stale-defense", "run-1", &project, &tracked)
        .unwrap();
    std::fs::write(&tracked, "edited by tool, run never committed\n").unwrap();

    // run-1 从未 record_run_commit——但别人已经直接提交了这个文件，preimage 陈旧。
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(
        &project,
        &["commit", "-qm", "someone commits while run-1 is pending"],
    );

    let entries = list_run_undo_entries_inner(&conn, "undo-stale-defense", "run-1").unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry.file_path.ends_with("tracked.md"))
        .unwrap_or_else(|| panic!("undo 清单缺 tracked.md：{entries:?}"));
    assert!(entry.stale, "前提：这条记录此时应该已经被标 stale");

    let content_before = std::fs::read_to_string(&tracked).unwrap();

    // 假装绕过前端勾选限制，直接拿这条 entry 自己的 path/digest 提交撤销请求
    // （digest 跟当前磁盘状态一致，不是「查看后又变了」的那种漂移——纯粹测 stale 拦截）。
    let report = undo_run_edits_inner(
        &conn,
        "undo-stale-defense",
        "run-1",
        vec![entry.file_path.to_str().unwrap().to_string()],
        vec![entry.current_digest.clone()],
    )
    .unwrap();

    assert!(
        report.restored.is_empty(),
        "陈旧路径绝不能真的被撤销写回磁盘：{report:?}"
    );
    assert_eq!(report.skipped.len(), 1);
    assert!(
        report.skipped[0].reason.contains("stale"),
        "跳过原因应该明确指向陈旧，而不是别的（比如 digest 漂移）：{:?}",
        report.skipped[0].reason
    );
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        content_before,
        "磁盘上的文件内容不应该被这次被拒绝的撤销请求改动"
    );
}

/// F2 定罪回归：run 处于终态但没有成功提交（failed/undone/kept/discarded）——旧 JOIN
/// 用 `state='active'` 过滤，这些状态查不到匹配行、退化成 None，被无条件当新鲜。
/// 这些状态下我们既没有「提交后」也没有「运行中」的清晰参照点可以信任，必须 fail-closed。
#[test]
fn session_review_checkpoint_terminal_non_active_run_state_fails_closed() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-terminal-state");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-terminal-state", "run-1", "codex", &base).unwrap();
    db::mark_run_failed(&conn, "review-terminal-state", "run-1").unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('review-terminal-state', 'run-1', ?1, 1, 1)",
        [tracked.to_str().unwrap()],
    )
    .unwrap();
    std::fs::write(&tracked, "edited before the run failed\n").unwrap();
    // 之后完全没有人碰过这个文件——即便如此，failed 状态本身就不可信任，仍必须不可撤销。

    let review = session_review_inner(&conn, "review-terminal-state").unwrap();

    assert!(
        !review_file(&review, "tracked.md").undoable,
        "run 处于 failed 等终态、既非已提交也非仍在运行，无法安全验证，应 fail-closed"
    );
}

/// BLOCKER-1 定罪回归（reviewer 探针实证·本刀新引入的大面积误杀）：一轮 run 用编辑工具
/// 改了文件、全程没走交付 broker 提交，正常收尾——`finish_run_without_git_writes` →
/// `db::finalize_run_pending_without_git_writes` 把 state 从 running 改成 active，但
/// post_head/commit_sha 全程留 NULL（生产入口：主 run 收尾 / lead run 收尾都走这条路，
/// 既有测试 `finish_run_without_git_writes_activates_checkpointed_run_and_hydrates_undo_
/// counts` 已经证明这是最常见的正常收尾形态，不是异常数据）。上一版把 active 分支缺
/// post_head/commit_sha 当「数据异常」fail-closed，会把这类完全正常、没人碰过的撤销记录
/// 也判死。**必须真正走 `finish_run_without_git_writes` 这条收尾路径**（不能停在
/// running）才能复现——这正是上一轮两条 running 测试对这个场景是纸老虎的原因。
#[test]
fn session_review_checkpoint_stays_undoable_after_finalize_without_native_commit() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "finalize-fresh");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "finalize-fresh", "run-1", "codex", &base).unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("finalize-fresh", "run-1", &project, &tracked)
        .unwrap();
    std::fs::write(&tracked, "edited by tool, never committed\n").unwrap();

    // 真正的收尾路径——run 结束时没有产生任何 git 提交，全靠 checkpoint 记账。
    finish_run_without_git_writes(&conn, "finalize-fresh", "run-1", false).unwrap();
    let row = db::last_run_commit(&conn, "finalize-fresh")
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "active", "前提：收尾后 state 应该是 active");
    assert_eq!(
        row.post_head, None,
        "前提：全程没提交，post_head 应该仍是 None"
    );

    let review = session_review_inner(&conn, "finalize-fresh").unwrap();

    assert!(
        review_file(&review, "tracked.md").undoable,
        "收尾之后没有人碰过这个文件，preimage 仍然新鲜，不该被判不可撤销"
    );
}

/// 上一条的对照组：收尾（同样没走 broker 提交）之后，这个文件又被提交过——这时候才应该
/// 判 stale。跟上一条一起证明修法不是「active 缺 post_head 就一律放行」的过度放宽。
/// 用 `list_run_undo_entries_inner` 而非 `session_review_inner`：文件一旦被提交，既不在
/// 任何「已提交 range」的归因集合里（这个 run 没有 post_head，不进 run_commit_ranges），
/// 也不再是「未提交」，Review 面板压根不会展示——跟上一轮 F2 的 running 分支同一个道理。
#[test]
fn list_run_undo_entries_marks_finalized_without_native_commit_run_stale_after_commit() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "finalize-then-commit");
    let tracked = project.join("tracked.md");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "finalize-then-commit", "run-1", "codex", &base).unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("finalize-then-commit", "run-1", &project, &tracked)
        .unwrap();
    std::fs::write(&tracked, "edited by tool, never committed\n").unwrap();

    finish_run_without_git_writes(&conn, "finalize-then-commit", "run-1", false).unwrap();
    let row = db::last_run_commit(&conn, "finalize-then-commit")
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "active");
    assert_eq!(row.post_head, None);

    // 收尾之后，别人（或本会话之外的路径）直接提交了这个文件。
    review_test_git(&project, &["add", "tracked.md"]);
    review_test_git(
        &project,
        &[
            "commit",
            "-qm",
            "someone commits after finalize without native commit",
        ],
    );

    let entries = list_run_undo_entries_inner(&conn, "finalize-then-commit", "run-1").unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry.file_path.ends_with("tracked.md"))
        .unwrap_or_else(|| panic!("undo 清单缺 tracked.md：{entries:?}"));

    assert!(
        entry.stale,
        "收尾之后这个文件又被提交过，preimage 已陈旧，应标 stale"
    );
}

#[test]
fn session_review_inplace_shell_file_is_not_undoable() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&conn, &project, "review-shell");
    let base = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&conn, "review-shell", "run-1", "codex", &base).unwrap();
    std::fs::write(project.join("shell.txt"), "not checkpointed\n").unwrap();
    review_test_git(&project, &["add", "shell.txt"]);
    review_test_git(&project, &["commit", "-qm", "shell commit"]);
    let post = worktree::rev_parse_head(&project).unwrap();
    db::record_run_commit(
        &conn,
        "review-shell",
        "run-1",
        &post,
        Some(1),
        Some(1),
        Some(0),
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-shell").unwrap();

    assert!(review.has_changes, "提交范围可归因的 shell 文件应可见");
    assert!(!review_file(&review, "shell.txt").undoable);
}

#[test]
fn session_review_inplace_reads_project_not_legacy_workspace() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    let session_id = "review-right-directory";
    setup_inplace_review_session(&conn, &project, session_id);
    let project_only = project.join("project-only.md");
    checkpoint_review_path(&conn, session_id, &project_only);
    std::fs::write(&project_only, "right directory\n").unwrap();
    let legacy = worktree::ensure_workspace(session_id, None, true).unwrap();
    std::fs::write(legacy.join("legacy-only.md"), "wrong directory\n").unwrap();

    let review = session_review_inner(&conn, session_id).unwrap();

    assert!(
        review.patch.contains("project-only.md"),
        "应读用户项目：{}",
        review.patch
    );
    assert!(
        !review.patch.contains("legacy-only.md"),
        "不得读旧隔离目录：{}",
        review.patch
    );
}

#[test]
fn session_review_inplace_non_git_project_reports_unavailable() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("plain-directory");
    std::fs::create_dir_all(&project).unwrap();
    db::create_session(&conn, "review-non-git", "review", "local-default", "local").unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-non-git").unwrap();

    assert!(!review.diff_available);
    assert!(!review.has_changes);
}

#[test]
fn session_review_inplace_unborn_head_reports_unavailable() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home = ReviewTestHome::set(tmp.path());
    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("unborn-repository");
    std::fs::create_dir_all(&project).unwrap();
    review_test_git(&project, &["init", "-q"]);
    db::create_session(
        &conn,
        "review-unborn-head",
        "review",
        "local-default",
        "local",
    )
    .unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();

    let review = session_review_inner(&conn, "review-unborn-head").unwrap();

    assert!(!review.diff_available);
    assert!(!review.has_changes);
}

#[test]
fn session_review_inplace_missing_project_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("missing-project");
    let inputs = ReviewInputs {
        session_id: "review-missing-project".into(),
        workspace: SessionWorkspace::Repo(project.clone()),
        inplace_project: Some(project),
        landing_commit_ranges: Vec::new(),
        run_commit_ranges: Vec::new(),
        staged_unlanded: None,
        checkpoint_paths: Vec::new(),
        checkpoint_entries_with_run_lifecycle: Vec::new(),
    };

    let error = match compute_review(inputs) {
        Ok(_) => panic!("不存在的项目目录不应静默返回 Review"),
        Err(error) => error,
    };

    assert!(
        error.starts_with("AL_ERR:wt.git.revParseSpawnFailed:"),
        "不存在的项目目录应暴露 git 启动故障：{error}"
    );
}
