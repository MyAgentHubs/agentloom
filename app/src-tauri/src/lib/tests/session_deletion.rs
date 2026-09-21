#![cfg(test)]

use super::*;

#[test]
fn inplace_archive_and_delete_preserve_user_worktree_registry_and_refs() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);

    let conn = crate::test_support::mem_db();
    conn.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
             VALUES ('gh-delete-live-child', 'github_org', 'GitHub Delete Test', 0, 0)",
        [],
    )
    .unwrap();
    conn.execute(
            "INSERT INTO repos (id, namespace_id, source, owner, name, path, status, added_at) \
             VALUES ('repo-delete-live-child', 'gh-delete-live-child', 'github', 'owner', 'repo', ?1, 'active', 0)",
            [repo.to_str().unwrap()],
        )
        .unwrap();
    db::create_session(
        &conn,
        "repo-parent-live-child",
        "parent",
        "repo-delete-live-child",
        "gh-delete-live-child",
    )
    .unwrap();
    db::create_session(
        &conn,
        "repo-child-live",
        "child",
        "repo-delete-live-child",
        "gh-delete-live-child",
    )
    .unwrap();
    db::set_session_parent(&conn, "repo-child-live", Some("repo-parent-live-child")).unwrap();
    db::set_session_continued_to(&conn, "repo-parent-live-child", Some("repo-child-live")).unwrap();

    // 保留一个用户自己的 stale worktree 注册；任何 prune 都会改变下面的快照。
    let external = repo_tmp.path().join("user-external-worktree");
    git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "user/external",
            external.to_str().unwrap(),
        ],
    );
    std::fs::remove_dir_all(&external).unwrap();
    let snapshot = || {
        (
            git_out(
                &repo,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            ),
            git_out(&repo, &["symbolic-ref", "--short", "HEAD"]),
            git_out(&repo, &["worktree", "list", "--porcelain"]),
            git_out(
                &repo,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            ),
        )
    };
    let before = snapshot();

    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    let running = Running::default();
    set_session_archived_inner(&db, &running, "repo-parent-live-child", true).unwrap();
    assert_eq!(snapshot(), before, "归档 in-place 会话不得 prune 或改 refs");
    set_session_archived_inner(&db, &running, "repo-parent-live-child", false).unwrap();
    assert_eq!(snapshot(), before, "取消归档 in-place 会话也不得改用户 git");
    delete_session_inner(&db, &running, "repo-parent-live-child").unwrap();
    assert_eq!(snapshot(), before, "删除 in-place 会话不得 prune 或改 refs");
    let conn = db.0.lock().unwrap();
    let (deleted_at, child_parent): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT
                    (SELECT deleted_at FROM sessions WHERE id = 'repo-parent-live-child'),
                    (SELECT parent_session_id FROM sessions WHERE id = 'repo-child-live')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(deleted_at.is_some(), "parent should be tombstoned");
    assert_eq!(child_parent, None, "child should remain live and detached");
}

#[test]
fn purge_inplace_parent_with_trashed_child_preserves_user_git_and_purges_db() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let parent = "purge-parent-trashed-child";
    let child = "purge-child-trashed";
    let db = setup_trashed_parent_with_trashed_child(&repo, parent, child);
    let refs_before = git_out(
        &repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);

    {
        let conn = db.0.lock().unwrap();
        purge_session_inner(&conn, parent).unwrap();
    }

    assert_eq!(
        git_out(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before
    );
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before
    );
    let conn = db.0.lock().unwrap();
    let parent_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1",
            [parent],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent_count, 0, "purge should remove parent DB row");
    let child_deleted_at: Option<i64> = conn
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = ?1",
            [child],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        child_deleted_at.is_some(),
        "parent purge must leave trashed child row recoverable"
    );
}

#[test]
fn restore_inplace_child_rejects_deleted_parent_without_touching_user_git() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let parent = "restore-parent-trashed";
    let child = "restore-child-parent-trashed";
    let db = setup_trashed_parent_with_trashed_child(&repo, parent, child);
    let running = Running::default();
    let refs_before = git_out(
        &repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);

    let err = restore_session_inner(&db, &running, child).unwrap_err();

    assert!(
        err.contains("AL_ERR:db.restore.parentDeleted"),
        "restore should reject with deleted-parent lineage error: {err}"
    );
    let conn = db.0.lock().unwrap();
    let child_deleted_at: Option<i64> = conn
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = ?1",
            [child],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        child_deleted_at.is_some(),
        "rejected restore must keep child tombstoned"
    );
    assert_eq!(
        git_out(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before
    );
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before
    );
}

/// Exercise the repository trash primitives directly to verify that successful deletion
/// records a tombstone, creates the trash ref, and removes the original heads ref.
/// `delete_session_inner` cannot currently reach its repository branch: a non-NULL
/// repo_id selects the in-place tombstone-only path before workspace resolution,
/// while resolving a repository workspace requires that same non-NULL repo_id.
/// Calling `trash_session_workspace` and `finalize_session_trash` directly protects
/// their filesystem/database contract without claiming coverage of that unreachable branch.
#[test]
fn delete_session_repo_branch_end_to_end_tombstones_and_trashes_ref() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    crate::worktree::mark_test_app_domain(&repo);

    let conn = crate::test_support::mem_db();
    namespaces_repo::add_namespace(&conn, "ns-m4b", "github_org", "M4b", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-m4b",
        "ns-m4b",
        "github",
        None,
        "repo-m4b",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    let sid = "s-m4b";
    db::create_session(&conn, sid, "M4b", "repo-m4b", "ns-m4b").unwrap();
    worktree::ensure_workspace(sid, Some(&repo), false).unwrap();

    let safe = crate::worktree::safe_id(sid);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    let ref_exists = |r: &str| {
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "--verify", "--quiet", r])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    assert!(
        ref_exists(&heads),
        "前置条件：ensure_workspace 应该已经真的建出 heads 分支"
    );

    // 先如实验证上面 doc 注释里的发现：走 delete_session_inner 顶层，这条会话（repo_id 已绑）
    // 会直接命中 session_is_in_place 早返回，heads 分支纹丝不动——留证据、不是本测的主断言。
    {
        let probe_db = Db(crate::perf_probe::TimedMutex::new(
            crate::test_support::mem_db(),
        ));
        {
            let c = probe_db.0.lock().unwrap();
            namespaces_repo::add_namespace(&c, "ns-m4b-probe", "github_org", "M4b probe", 0)
                .unwrap();
            repos_repo::add_repo(
                &c,
                "repo-m4b-probe",
                "ns-m4b-probe",
                "github",
                None,
                "repo-m4b-probe",
                repo.to_str().unwrap(),
                None,
            )
            .unwrap();
            db::create_session(&c, "s-m4b-probe", "probe", "repo-m4b-probe", "ns-m4b-probe")
                .unwrap();
        }
        let running = Running::default();
        delete_session_inner(&probe_db, &running, "s-m4b-probe").unwrap();
        assert!(
            ref_exists(&heads),
            "佐证：delete_session_inner 顶层对绑了 repo_id 的会话走的是纯墓碑早返回，\
                 不会碰这条 heads 分支（这是本条测试改走底层原语的原因）"
        );
    }

    // 真正的断言主体：直接调 H2 改的两个底层原语（trash_session_workspace + finalize_session_
    // trash），等价于"如果 delete_session_inner 的 Repo 分支真的被触发，它会做什么"。
    crate::worktree::trash_session_workspace(sid, &repo).unwrap();
    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    finalize_session_trash(&db, sid, &repo).unwrap();

    let conn = db.0.lock().unwrap();
    let deleted_at: Option<i64> = conn
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = ?1",
            [sid],
            |r| r.get(0),
        )
        .unwrap();
    assert!(deleted_at.is_some(), "删完 deleted_at 必须真落库");
    drop(conn);

    assert!(
        !ref_exists(&heads),
        "删完 heads 分支应该已经不在了（挪去 trash 了）"
    );
    assert!(ref_exists(&trash), "删完 trash ref 应该真的存在");
}

/// A poisoned database lock during trash finalization must report an error and restore
/// the trash ref to heads. Otherwise the workspace remains orphaned without a tombstone,
/// preventing deletion retries and leaving it ineligible for purge or garbage collection.
/// Trash the workspace first, then poison the lock before finalization to exercise
/// compensation deterministically without depending on a concurrent race.
#[test]
fn delete_session_repo_relock_poisoned_still_compensates_trash() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    crate::worktree::mark_test_app_domain(&repo);

    let sid = "s-poison";
    worktree::ensure_workspace(sid, Some(&repo), false).unwrap();

    let safe = crate::worktree::safe_id(sid);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    let ref_exists = |r: &str| {
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "--verify", "--quiet", r])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    assert!(ref_exists(&heads), "前置条件：heads 分支应该已经真的建出来");

    // 锁还没坏时，先真跑一遍 trash——这时 repo 已经处在"worktree 挪进 trash ref、DB 墓碑还没
    // 落库"的真实中间态，跟生产代码阶段二和阶段三之间的窗口完全一致。
    crate::worktree::trash_session_workspace(sid, &repo).unwrap();
    assert!(!ref_exists(&heads), "前置条件：trash 后 heads 应该已经挪走");
    assert!(
        ref_exists(&trash),
        "前置条件：trash 后 trash ref 应该已经存在"
    );

    // 人为毒化 db 锁（同 lead_tools.rs::dispatch_worker_recovers_from_poisoned_ledger_lock 手法）。
    let db = std::sync::Arc::new(Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    )));
    {
        let db2 = db.clone();
        let _ = std::thread::spawn(move || {
            let _g = db2.0.lock().unwrap();
            panic!("poison db lock on purpose");
        })
        .join();
    }
    assert!(db.0.lock().is_err(), "前置条件：锁应已中毒");

    let err = finalize_session_trash(&db, sid, &repo).unwrap_err();
    assert!(!err.is_empty(), "拿锁失败也必须返回明确错误，不能静默");

    // 核心断言：补偿生效——trash ref 已经被挪回 heads，不是永久孤儿。
    assert!(
        ref_exists(&heads),
        "拿锁失败也应该走 C3 补偿：heads 应该已经被 restore_trashed_session_branch 挪回来了"
    );
    assert!(
        !ref_exists(&trash),
        "补偿之后 trash ref 应该已经清空（挪回 heads 时会删掉）"
    );
}

#[test]
fn delete_session_busy_returns_session_busy() {
    // 🔴 终审 Critical busy-gate:运行中会话软删必拒(SESSION_BUSY)·防 finalize 拍快照后
    // worktree remove --force 删掉 member 进程后续写入丢活(G1)。
    use crate::test_support::mem_db;
    let c = mem_db();
    let running = Running::default();
    db::create_session(&c, "s-del-busy", "x", "local-default", "local").unwrap();
    let _busy = reserve_mutation(&running, "s-del-busy", "undo").expect("预占 mutating slot");
    let db = Db(crate::perf_probe::TimedMutex::new(c));
    let err = delete_session_inner(&db, &running, "s-del-busy").unwrap_err();
    assert_eq!(err, "SESSION_BUSY:delete");
}

#[test]
fn team_run_slot_busy_blocks_delete_session() {
    // 🔴 G1 补丁回归锁·钉子①：team run 起跑占槽期间（member 还在跑），delete_session 必须
    // 像 solo 一样返回 SESSION_BUSY——修前 start_team_run 从不占 Running 槽，reserve_mutation
    // 只查 Running（`m.contains_key`），团队跑与删除完全不互斥，delete 会直接成功（丢活）。
    // 这里用 reserve_team_run_slot 直接模拟 start_team_run 已占槽这一刻的状态，不依赖真实
    // spawn 子进程（`start_team_run` 是 #[tauri::command]，需要真实 AppHandle/State，仓库里
    // 目前也没有为这类命令搭 mock Tauri app 的测试设施——同 delete_session_busy_returns_session_busy
    // 用 reserve_mutation 模拟"运行中"的既有先例手法）。
    use crate::test_support::mem_db;
    let c = mem_db();
    let running = Running::default();
    db::create_session(&c, "s-team-busy", "x", "local-default", "local").unwrap();
    reserve_team_run_slot(&running, "s-team-busy").expect("模拟 start_team_run 已占槽");
    let db = Db(crate::perf_probe::TimedMutex::new(c));
    let err = delete_session_inner(&db, &running, "s-team-busy").unwrap_err();
    assert_eq!(err, "SESSION_BUSY:delete");
}

#[test]
fn team_run_slot_released_after_finalize_allows_delete() {
    // 钉子②：team run 全部队员正常终态后（`spawn_member` reader 线程 / start_team_run 同步
    // 全失败分支都在 run_member_finished 判定"最后一个"时调用 release_team_run_slot），槽已
    // 释放，delete 应恢复正常成功——不能因为曾经跑过 team run 就永久卡死。
    use crate::test_support::mem_db;
    let c = mem_db();
    let running = Running::default();
    db::create_session(&c, "s-team-done", "x", "local-default", "local").unwrap();
    reserve_team_run_slot(&running, "s-team-done").unwrap();
    release_team_run_slot(&running, "s-team-done"); // 模拟收尾点的释放调用
    let db = Db(crate::perf_probe::TimedMutex::new(c));
    delete_session_inner(&db, &running, "s-team-done").expect("释放后应可正常删除");
}
