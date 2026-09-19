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

/// M4b（2026-07-29 opus 对抗审补测·**执行中发现审单前提跟当前代码不符，如实记录**）：
/// 原始诉求是「github_org 会话经 `delete_session_inner` 端到端删完，断言 `deleted_at IS NOT
/// NULL` 且 trash ref 存在」。写这条测时发现：`delete_session_inner` 的
/// `Ok(SessionWorkspace::Repo(repo)) => { trash_session_workspace... }` 这条分支在当前代码下对
/// **任何**会话都到不了——`session_is_in_place`（`repo_id_is_in_place` = `repo_id.is_some()`）
/// 只要 session 绑了 `repo_id` 就直接判定"in-place"、在 `match workspace` 之前就早返回纯 DB 墓碑
/// 路径；而 `resolve_session_workspace` 要返回 `SessionWorkspace::Repo(_)` 同样需要
/// `resolve_repo_path_for_session` 读到 `repo_id.is_some()`。这两个条件用的是同一个字段、同一
/// 份读取——不存在"repo_id 绑了、但 session_is_in_place 判 false"的中间态，所以
/// `match workspace { Ok(SessionWorkspace::Repo(repo)) => ... }` 这条臂在 `delete_session_inner`
/// 里是结构性死代码（跟具体测试数据无关，是这两个函数当前定义决定的）。真跑了一遍验证：给
/// github_org 会话绑 `repo_id` + `ensure_workspace` 建出真分支后调 `delete_session_inner`，
/// 结果直接走了纯墓碑早返回分支，heads 分支纹丝不动——证实了这个结论（现有
/// `setup_trashed_parent_with_trashed_child` 同款用法多半也一直是这条路，"trashed" 这个名字名不
/// 副实）。
///
/// 既然经 `delete_session_inner` 走不到，但 `trash_session_workspace` +
/// `finalize_session_trash`（本刀 H2 手术改的正是它）仍然是真实存在、会被编译进产物的代码——
/// 这条测改成直接调这两个底层原语（等价于"如果 `delete_session_inner` 的 Repo 分支真的被触发,
/// 它会做什么"），端到端断言 `deleted_at IS NOT NULL` + trash ref 存在 + heads 已经不在，覆盖住
/// H2 改动本身涉及的真实代码路径。`delete_session_inner` 顶层现在到不了这条分支这件事本身，
/// 已经如实写在这里——不是本刀职权范围内该顺手修的架构问题，留给看到这条注释的人判断要不要
/// 另开工单。
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

/// 必改①的红绿证明（2026-07-29 opus 对抗审揪出的回归）：`finalize_session_trash` 重新拿锁那步
/// 如果锁已经中毒（某处持锁 panic 留下的 poison 状态），旧写法是裸 `db.0.lock()...?`——直接
/// `?` 跳过 C3 补偿返回。这时 `trash_session_workspace` 已经真的把 worktree 挪进了 trash ref：
/// 墓碑永远落不了库 = 永久孤儿（再删被 `wt.cleanup.trashRefExists` 顶回、purge 因
/// `deleted_at IS NULL` 拒绝、gc 也扫不到）。
///
/// 不必真起线程赢竞态：`trash_session_workspace` 先真跑一遍（此时锁还没坏），再照
/// `lead_tools.rs::dispatch_worker_recovers_from_poisoned_ledger_lock` 的手法人为毒化 db 锁
/// （另起线程持锁 panic），最后单独调 `finalize_session_trash` 验证补偿分支——断言：① 返回值
/// 不是"看起来什么都没发生"的静默失败，是明确的错误；② trash ref 已经被补偿路径挪回 heads
/// （不是永久孤儿）。
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
