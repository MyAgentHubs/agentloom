#![cfg(test)]

use super::*;

#[test]
fn gc_expired_trash_inner_runs_and_purges_expired() {
    // 🔴 GUI 验逮的 strftime bug 回归锁:gc_expired_trash_inner 真跑(此前从没被调用·测试没覆盖·
    // 接启动才暴 Invalid column type Text)。grace 过期(deleted_at 很久前)的软删会话应被真删·活会话不动。
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(&c, "s-old", "t", "local-default", "local").unwrap();
    db::create_session(&c, "s-live", "t", "local-default", "local").unwrap();
    // s-old 软删且 deleted_at 设成 1(1970·远超 30 天 grace·必过期)
    c.execute("UPDATE sessions SET deleted_at = 1 WHERE id='s-old'", [])
        .unwrap();

    let purged = gc_expired_trash_inner(&c).unwrap(); // 修前此处 Err(strftime 类型错)→panic

    let old_gone: i64 = c
        .query_row("SELECT COUNT(*) FROM sessions WHERE id='s-old'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let live_there: i64 = c
        .query_row("SELECT COUNT(*) FROM sessions WHERE id='s-live'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        purged >= 1,
        "🔴 grace 过期软删会话应被 gc 真删(strftime 不再类型错)"
    );
    assert_eq!(old_gone, 0, "过期软删会话 purge 后行清掉");
    assert_eq!(live_there, 1, "活会话不动");
}

#[test]
fn gc_expired_trash_inner_cleans_up_journal_dir() {
    // 刀 R R5:30 天到期 GC 同款级联清 journal 目录(与 purge_session_inner 同一路径)。
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    use crate::test_support::mem_db;
    let c = mem_db();
    let old_id = "gc-journal-old";
    let live_id = "gc-journal-live";
    db::create_session(&c, old_id, "t", "local-default", "local").unwrap();
    db::create_session(&c, live_id, "t", "local-default", "local").unwrap();
    c.execute("UPDATE sessions SET deleted_at = 1 WHERE id = ?1", [old_id])
        .unwrap();

    let old_journal_dir = crate::worktree::journals_dir().join(old_id);
    let live_journal_dir = crate::worktree::journals_dir().join(live_id);
    std::fs::create_dir_all(&old_journal_dir).unwrap();
    std::fs::write(old_journal_dir.join("run.jsonl"), b"{}").unwrap();
    std::fs::create_dir_all(&live_journal_dir).unwrap();
    std::fs::write(live_journal_dir.join("run.jsonl"), b"{}").unwrap();

    let purged = gc_expired_trash_inner(&c).unwrap();

    assert!(purged >= 1, "过期软删会话应被 gc 清掉");
    assert!(
        !old_journal_dir.exists(),
        "🔴 gc 到期硬删必须级联清掉过期会话的 journal 目录"
    );
    assert!(
        live_journal_dir.exists(),
        "活会话(未过期)的 journal 目录不应被动"
    );
}

#[test]
fn gc_expired_inplace_parent_preserves_user_git_and_purges_db() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let parent = "gc-parent-trashed-child";
    let child = "gc-child-trashed";
    let db = setup_trashed_parent_with_trashed_child(&repo, parent, child);
    let refs_before = git_out(
        &repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);
    {
        let conn = db.0.lock().unwrap();
        conn.execute("UPDATE sessions SET deleted_at = 1 WHERE id = ?1", [parent])
            .unwrap();
    }

    let purged = {
        let conn = db.0.lock().unwrap();
        gc_expired_trash_inner(&conn).unwrap()
    };

    assert_eq!(
        purged, 1,
        "GC should purge expired parent with only trashed child"
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
    let conn = db.0.lock().unwrap();
    let parent_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1",
            [parent],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent_count, 0, "GC should remove parent DB row");
    let child_deleted_at: Option<i64> = conn
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = ?1",
            [child],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        child_deleted_at.is_some(),
        "GC parent purge must leave trashed child row recoverable"
    );
}

#[test]
fn ensure_local_seed_is_idempotent() {
    use crate::test_support::{mem_db, tmp_root};
    let _home_lock = crate::worktree::test_home_lock();
    let c = mem_db();
    let (_g, root) = tmp_root();
    let _home_guard = TestHomeGuard::set(&root);
    let local_path = local_default_path();
    // 首次跑 · 全建好
    ensure_local_namespace_and_default_repo(&c, &local_path).unwrap();
    // namespace 进表
    let n = namespaces_repo::get_namespace_by_id(&c, "local")
        .unwrap()
        .expect("Local namespace 应建");
    assert_eq!(n.kind, "local");
    assert_eq!(n.name, "Local");
    assert_eq!(n.is_builtin, 1);
    // local-default repo 进表
    let r = repos_repo::get_repo_by_id(&c, "local-default")
        .unwrap()
        .expect("local-default repo 应建");
    assert_eq!(r.name, "我的项目");
    assert_eq!(r.source, "local");
    // 目录 + .git 建好
    assert!(local_path.exists());
    assert!(local_path.join(".git").exists(), "应自动 git init");

    // 二次跑幂等（不重复 INSERT · 不 panic · 不破 .git）
    ensure_local_namespace_and_default_repo(&c, &local_path).unwrap();
    // 仍只 1 个 Local namespace
    let count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM namespaces WHERE id='local'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    // 仍只 1 个 local-default repo
    let r_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM repos WHERE id='local-default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(r_count, 1);
}

#[test]
fn ensure_local_seed_creates_dir_if_missing() {
    use crate::test_support::{mem_db, tmp_root};
    let _home_lock = crate::worktree::test_home_lock();
    let c = mem_db();
    let (_g, root) = tmp_root();
    let _home_guard = TestHomeGuard::set(&root);
    // 故意不预建 local_path · 让 ensure 函数 mkdir
    let local_path = local_default_path();
    assert!(!local_path.exists());
    ensure_local_namespace_and_default_repo(&c, &local_path).unwrap();
    assert!(local_path.exists());
    assert!(local_path.join(".git").exists());
}

#[test]
fn create_session_business_with_explicit_repo_namespace() {
    use crate::test_support::mem_db;
    let c = mem_db();
    create_session_business(
        &c,
        "s-explicit",
        "title",
        Some("local-default"),
        Some("local"),
    )
    .unwrap();
    let (rid, nsid): (String, String) = c
        .query_row(
            "SELECT repo_id, namespace_id FROM sessions WHERE id='s-explicit'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rid, "local-default");
    assert_eq!(nsid, "local");
}

#[test]
fn create_session_business_defaults_when_none() {
    use crate::test_support::mem_db;
    let c = mem_db();
    create_session_business(&c, "s-default", "title", None, None).unwrap();
    let (rid, nsid): (String, String) = c
        .query_row(
            "SELECT repo_id, namespace_id FROM sessions WHERE id='s-default'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rid, "local-default", "default fallback 应到 local-default");
    assert_eq!(nsid, "local", "default fallback 应到 local");
}

#[test]
fn create_session_business_rejects_missing_namespace() {
    use crate::test_support::mem_db;
    let c = mem_db();
    let err = create_session_business(&c, "s-bad", "x", Some("local-default"), Some("missing-ns"))
        .unwrap_err();
    assert!(err.starts_with("NAMESPACE_NOT_FOUND:"), "{err}");
}

#[test]
fn create_session_business_empty_string_repo_id_falls_back_to_default() {
    use crate::test_support::mem_db;
    let c = mem_db();
    create_session_business(&c, "s-empty", "x", Some(""), Some("")).unwrap();
    let (rid, nsid): (String, String) = c
        .query_row(
            "SELECT repo_id, namespace_id FROM sessions WHERE id='s-empty'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rid, "local-default");
    assert_eq!(nsid, "local");
}

#[test]
fn create_session_business_rejects_pipe_in_id() {
    use crate::test_support::mem_db;
    let c = mem_db();

    create_session_business(&c, "s|pipe", "x", None, None).unwrap_err();
}

#[test]
fn cleanup_legacy_local_repos_preserves_gui_projects_sessions_and_messages() {
    use crate::test_support::{mem_db, tmp_root};

    let c = mem_db();
    let (_tmp_guard, tmp) = tmp_root();

    let user_root = tmp.join("user-repos");
    let user_old1 = user_root.join("old1");
    let user_old2 = user_root.join("old2");
    std::fs::create_dir_all(&user_old1).unwrap();
    std::fs::create_dir_all(&user_old2).unwrap();
    std::fs::write(user_old1.join("keep.txt"), "do not delete").unwrap();
    c.execute(
        "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at)
             VALUES ('r-old1', 'local', 'local', 'old1', ?1, 'active', 0)",
        [user_old1.to_str().unwrap()],
    )
    .unwrap();
    c.execute(
        "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at)
             VALUES ('r-old2', 'local', 'local', 'old2', ?1, 'active', 0)",
        [user_old2.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&c, "s-old", "老会话", "r-old1", "local").unwrap();
    db::append_message(
        &c,
        "s-old",
        "user",
        &[Block::Text { text: "hi".into() }],
        None,
        None,
        None,
    )
    .unwrap();

    let wt_root = tmp.join("agentloom-worktrees");
    let sessions_root = tmp.join("agentloom-sessions");
    let old_worktree = wt_root.join("old1");
    let old_session = sessions_root.join("s-old");
    std::fs::create_dir_all(&old_worktree).unwrap();
    std::fs::create_dir_all(&old_session).unwrap();

    let n = cleanup_legacy_local_repos_in(&c, &wt_root, &sessions_root).unwrap();
    assert_eq!(n, 0, "无可靠遗留标记时必须 fail-closed");
    assert!(repos_repo::get_repo_by_id(&c, "local-default")
        .unwrap()
        .is_some());
    assert!(repos_repo::get_repo_by_id(&c, "r-old1").unwrap().is_some());
    assert!(repos_repo::get_repo_by_id(&c, "r-old2").unwrap().is_some());

    let sess_cnt: i64 = c
        .query_row("SELECT COUNT(*) FROM sessions WHERE id='s-old'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(sess_cnt, 1);
    let msg_cnt: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='s-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(msg_cnt, 1);

    assert!(old_worktree.exists(), "无遗留标记时不得删任何目录");
    assert!(old_session.exists(), "无遗留标记时不得删任何目录");
    assert!(user_old1.exists(), "绝不能删用户原 repo.path 目录");
    assert!(user_old1.join("keep.txt").exists());
    assert!(user_old2.exists(), "绝不能删用户原 repo.path 目录");

    assert_eq!(
        cleanup_legacy_local_repos_in(&c, &wt_root, &sessions_root).unwrap(),
        0,
        "二次 cleanup 应幂等"
    );
}

#[test]
fn resolve_active_repo_for_namespace_uses_last_active_first() {
    use crate::test_support::mem_db;
    let c = mem_db();
    namespaces_repo::add_namespace(&c, "ns-a", "github_org", "org-a", 0).unwrap();
    repos_repo::add_repo(&c, "r-1", "ns-a", "github", None, "r1", "/tmp/r1", None).unwrap();
    repos_repo::add_repo(&c, "r-2", "ns-a", "github", None, "r2", "/tmp/r2", None).unwrap();
    namespaces_repo::set_last_active_repo(&c, "ns-a", Some("r-2")).unwrap();
    let active = resolve_active_repo_for_namespace(&c, "ns-a").unwrap();
    assert_eq!(active, Some("r-2".into()));
}

#[test]
fn resolve_active_repo_for_namespace_fallback_first_when_last_invalid() {
    use crate::test_support::mem_db;
    let c = mem_db();
    namespaces_repo::add_namespace(&c, "ns-a", "github_org", "org-a", 0).unwrap();
    repos_repo::add_repo(&c, "r-1", "ns-a", "github", None, "r1", "/tmp/r1", None).unwrap();
    let active = resolve_active_repo_for_namespace(&c, "ns-a").unwrap();
    assert_eq!(active, Some("r-1".into()));

    namespaces_repo::set_last_active_repo(&c, "ns-a", Some("r-deleted")).unwrap();
    let active2 = resolve_active_repo_for_namespace(&c, "ns-a").unwrap();
    assert_eq!(active2, Some("r-1".into()));

    namespaces_repo::add_namespace(&c, "ns-empty", "github_org", "empty", 0).unwrap();
    let active3 = resolve_active_repo_for_namespace(&c, "ns-empty").unwrap();
    assert_eq!(active3, None);
}
