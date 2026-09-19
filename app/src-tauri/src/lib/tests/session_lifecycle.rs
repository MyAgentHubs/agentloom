#![cfg(test)]

use super::*;

#[test]
fn readonly_gate_rejects_continued_parent_with_exact_message() {
    use crate::test_support::mem_db;
    let conn = mem_db();
    crate::db::create_session(&conn, "readonly-parent", "parent", "local-default", "local")
        .unwrap();
    crate::db::create_session(&conn, "readonly-child", "child", "local-default", "local").unwrap();
    crate::db::set_session_continued_to(&conn, "readonly-parent", Some("readonly-child")).unwrap();

    let err = ensure_session_not_continued(&conn, "readonly-parent", Locale::Zh).unwrap_err();

    assert_eq!(err, "会话已交接到新会话·只读·请到新会话继续");
}

#[test]
fn readonly_gate_rejects_parent_with_live_child_even_without_pointer() {
    use crate::test_support::mem_db;
    let conn = mem_db();
    crate::db::create_session(
        &conn,
        "readonly-orphan-parent",
        "parent",
        "local-default",
        "local",
    )
    .unwrap();
    crate::db::create_session(
        &conn,
        "readonly-orphan-child",
        "child",
        "local-default",
        "local",
    )
    .unwrap();
    crate::db::set_session_parent(
        &conn,
        "readonly-orphan-child",
        Some("readonly-orphan-parent"),
    )
    .unwrap();

    let err =
        ensure_session_not_continued(&conn, "readonly-orphan-parent", Locale::Zh).unwrap_err();

    assert_eq!(err, "会话已交接到新会话·只读·请到新会话继续");
}

#[test]
fn readonly_gate_allows_ordinary_session_without_continuation() {
    use crate::test_support::mem_db;
    let conn = mem_db();
    crate::db::create_session(
        &conn,
        "readonly-ordinary",
        "ordinary",
        "local-default",
        "local",
    )
    .unwrap();

    assert!(ensure_session_not_continued(&conn, "readonly-ordinary", Locale::Zh).is_ok());
}

#[test]
fn readonly_continued_parent_still_passes_ensure_session_workspace() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    crate::db::create_session(
        &conn,
        "readonly-ensure-parent",
        "parent",
        "local-default",
        "local",
    )
    .unwrap();
    crate::db::create_session(
        &conn,
        "readonly-ensure-child",
        "child",
        "local-default",
        "local",
    )
    .unwrap();
    crate::db::set_session_continued_to(
        &conn,
        "readonly-ensure-parent",
        Some("readonly-ensure-child"),
    )
    .unwrap();

    let result = ensure_session_workspace(&conn, "readonly-ensure-parent");

    assert!(
        !matches!(&result, Err(e) if e == "会话已交接到新会话·只读·请到新会话继续"),
        "continued parent file workspace access must not be blocked by readonly gate"
    );
    assert!(
        result.is_ok(),
        "continued parent should still ensure workspace for read-only file access: {result:?}"
    );
}

#[test]
fn list_sessions_returns_repo_id_and_continued_to_fields() {
    use crate::test_support::mem_db;
    let c = mem_db();
    // 默认 session（自动归 local-default）+ 关联项目 session（手动 UPDATE 绑）
    db::create_session(&c, "s_default", "默认", "local-default", "local").unwrap();
    repos_repo::add_repo(&c, "r_x", "local", "local", None, "x", "/tmp/x", None).unwrap();
    db::create_session(&c, "s_bound", "绑项目", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET repo_id = 'r_x' WHERE id = 's_bound'",
        [],
    )
    .unwrap();

    groups_repo::create_group(&c, "g-list", "local-default", "分组", 0).unwrap();
    c.execute(
        "UPDATE sessions SET group_id = 'g-list' WHERE id = 's_bound'",
        [],
    )
    .unwrap();
    db::create_session(&c, "s_child", "子会话", "local-default", "local").unwrap();
    db::set_session_parent(&c, "s_child", Some("s_bound")).unwrap();
    db::set_session_continued_to(&c, "s_bound", Some("s_child")).unwrap();

    // 直接调 sql 模拟 list_sessions（IPC 不便单测 · 验底层 SQL + Session 结构）
    let mut stmt = c
            .prepare(
                "SELECT id, title, repo_id, namespace_id, group_id, created_at, \
                 pinned, unread, archived, archived_at, \
                 parent_session_id, continued_to_session_id \
                 FROM sessions WHERE deleted_at IS NULL ORDER BY pinned DESC, created_at DESC, id DESC",
            )
            .unwrap();
    let rows: Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        bool,
        bool,
        bool,
        Option<i64>,
        Option<String>,
        Option<String>,
    )> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows.len(), 3);
    let bound = rows.iter().find(|r| r.0 == "s_bound").unwrap();
    assert_eq!(bound.2, Some("r_x".into()));
    assert_eq!(bound.4, Some("g-list".into()));
    assert_eq!(bound.11, Some("s_child".into()));
    let child = rows.iter().find(|r| r.0 == "s_child").unwrap();
    assert_eq!(child.10, Some("s_bound".into()));
    let default_s = rows.iter().find(|r| r.0 == "s_default").unwrap();
    assert_eq!(default_s.2, Some("local-default".into()));
    assert_eq!(default_s.4, None);
    let sessions = list_sessions_inner(&c).unwrap();
    let flags = sessions
        .iter()
        .map(|session| (session.id.as_str(), session.in_place))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(flags.get("s_bound"), Some(&true));
    assert_eq!(flags.get("s_default"), Some(&true));
    assert_eq!(flags.get("s_child"), Some(&true));

    // 验 db::Session struct 真有 repo_id/血缘 field（编译期锁 schema）
    let s = db::Session {
        id: "x".into(),
        title: "y".into(),
        repo_id: Some("r_x".into()),
        namespace_id: Some("local".into()), // 必修 #2：Phase 2 Task 2 加字段后必须同步加
        in_place: true,
        group_id: Some("g-list".into()),
        parent_session_id: Some("parent".into()),
        continued_to_session_id: Some("child".into()),
        total_input_tokens: 0,
        total_output_tokens: 0,
        created_at: 0,
        pinned: false,
        unread: false,
        archived: false,
        archived_at: None,
    };
    assert_eq!(s.repo_id, Some("r_x".into()));
    assert_eq!(s.group_id, Some("g-list".into()));
    assert_eq!(s.parent_session_id, Some("parent".into()));
    assert_eq!(s.continued_to_session_id, Some("child".into()));
}

#[test]
fn session_usage_new_session_defaults_zero() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(
        &c,
        "s-default-usage",
        "默认 token",
        "local-default",
        "local",
    )
    .unwrap();

    let session = c
        .query_row(
            "SELECT id, title, repo_id, namespace_id, group_id, created_at, \
                 pinned, unread, archived, archived_at, \
                 parent_session_id, continued_to_session_id, \
                 total_input_tokens, total_output_tokens \
                 FROM sessions WHERE id = 's-default-usage'",
            [],
            |r| {
                Ok(db::Session {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    repo_id: r.get(2)?,
                    namespace_id: r.get(3)?,
                    in_place: false,
                    group_id: r.get(4)?,
                    created_at: r.get(5)?,
                    pinned: r.get(6)?,
                    unread: r.get(7)?,
                    archived: r.get(8)?,
                    archived_at: r.get(9)?,
                    parent_session_id: r.get(10)?,
                    continued_to_session_id: r.get(11)?,
                    total_input_tokens: r.get(12)?,
                    total_output_tokens: r.get(13)?,
                })
            },
        )
        .unwrap();

    assert_eq!(session.total_input_tokens, 0);
    assert_eq!(session.total_output_tokens, 0);
}

#[test]
fn session_usage_normal_finalizer_accumulates_and_list_sessions_maps_columns() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(
        &c,
        "s-finalizer-usage",
        "finalizer token",
        "local-default",
        "local",
    )
    .unwrap();

    for (dedup_key, input_tokens, output_tokens) in [("run-1", 7, 13), ("run-2", 5, 11)] {
        let msg = display_reduce::ReducedMessage {
            dedup_key: dedup_key.into(),
            blocks: vec![Block::Text {
                text: format!("done {dedup_key}"),
            }],
        };
        persist_normal_finalizer(
            &c,
            "s-finalizer-usage",
            "claude",
            Some("Claude"),
            Some(&msg),
            Some((Some(input_tokens), Some(output_tokens))),
        );
    }

    // 直接调 SQL 模拟 list_sessions（IPC 不便单测 · 锁 SELECT 顺序与 Session 映射）。
    let session = c
        .query_row(
            "SELECT id, title, repo_id, namespace_id, group_id, created_at, \
                 pinned, unread, archived, archived_at, \
                 parent_session_id, continued_to_session_id, \
                 total_input_tokens, total_output_tokens \
                 FROM sessions WHERE id = 's-finalizer-usage'",
            [],
            |r| {
                Ok(db::Session {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    repo_id: r.get(2)?,
                    namespace_id: r.get(3)?,
                    in_place: false,
                    group_id: r.get(4)?,
                    created_at: r.get(5)?,
                    pinned: r.get(6)?,
                    unread: r.get(7)?,
                    archived: r.get(8)?,
                    archived_at: r.get(9)?,
                    parent_session_id: r.get(10)?,
                    continued_to_session_id: r.get(11)?,
                    total_input_tokens: r.get(12)?,
                    total_output_tokens: r.get(13)?,
                })
            },
        )
        .unwrap();

    assert_eq!(session.total_input_tokens, 12);
    assert_eq!(session.total_output_tokens, 24);
    let message_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 's-finalizer-usage'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(message_count, 2);
}

#[test]
fn session_usage_normal_finalizer_usage_only_accumulates_without_message() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(
        &c,
        "s-finalizer-usage-only",
        "finalizer usage only",
        "local-default",
        "local",
    )
    .unwrap();
    let db = Db(crate::perf_probe::TimedMutex::new(c));

    persist_normal_finalizer_if_needed(
        &db,
        "s-finalizer-usage-only",
        "claude",
        None,
        Some((Some(17), Some(29))),
    );

    let c = db.0.lock().unwrap();
    let (total_input_tokens, total_output_tokens): (i64, i64) = c
        .query_row(
            "SELECT total_input_tokens, total_output_tokens \
                 FROM sessions WHERE id = 's-finalizer-usage-only'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((total_input_tokens, total_output_tokens), (17, 29));

    let message_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 's-finalizer-usage-only'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(message_count, 0);
}

#[test]
fn ensure_session_workspace_refuses_soft_deleted() {
    use crate::test_support::mem_db;
    let conn = mem_db();
    crate::db::create_session(&conn, "s-x", "t", "local-default", "local").unwrap();
    // Live session: must NOT be rejected with SESSION_DELETED (may fail for other reasons like missing namespace)
    let live = ensure_session_workspace(&conn, "s-x");
    assert!(
        !matches!(&live, Err(e) if e.starts_with("SESSION_DELETED")),
        "live session must not be blocked by tombstone gate"
    );
    // Soft-delete -> gate must reject
    crate::db::set_session_deleted(&conn, "s-x").unwrap();
    let r = ensure_session_workspace(&conn, "s-x");
    assert!(
        matches!(&r, Err(e) if e.starts_with("SESSION_DELETED")),
        "soft-deleted session must be rejected by ensure gate (prevent orphan resurrect)"
    );
}

#[test]
fn ensure_session_workspace_refuses_archived() {
    // Bug2 (归档不粘): after archive RELEASES the session folder, a stray frontend file-viewer
    // access (list_session_files / read_session_file) must NOT rebuild it via ensure -- otherwise
    // the archive "doesn't stick". Gate at the source. re-attach is legit only on UNARCHIVE, which
    // clears `archived` (set_session_archived ..., false) BEFORE calling ensure, so the gate passes there.
    use crate::test_support::mem_db;
    let conn = mem_db();
    crate::db::create_session(&conn, "s-arch", "t", "local-default", "local").unwrap();
    // Non-archived session: must NOT be blocked by the archived gate (may fail for other reasons).
    let live = ensure_session_workspace(&conn, "s-arch");
    assert!(
        !matches!(&live, Err(e) if e.starts_with("SESSION_ARCHIVED")),
        "non-archived session must not be blocked by archived gate"
    );
    // Archive -> gate must reject (folder stays released; no stray re-attach).
    crate::db::set_session_archived(&conn, "s-arch", true).unwrap();
    let r = ensure_session_workspace(&conn, "s-arch");
    assert!(
        matches!(&r, Err(e) if e.starts_with("SESSION_ARCHIVED")),
        "archived session must be rejected by ensure gate (Bug2: folder must not pop back)"
    );
}

#[test]
fn set_session_archived_from_child_archives_whole_local_chain() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "local-arch-root", "root", "local-default", "local").unwrap();
    db::create_session(&c, "local-arch-child", "child", "local-default", "local").unwrap();
    db::set_session_parent(&c, "local-arch-child", Some("local-arch-root")).unwrap();
    db::set_session_continued_to(&c, "local-arch-root", Some("local-arch-child")).unwrap();
    let db = Db(crate::perf_probe::TimedMutex::new(c));
    let running = Running::default();

    set_session_archived_inner(&db, &running, "local-arch-child", true).unwrap();

    let conn = db.0.lock().unwrap();
    let archived_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions
                 WHERE id IN ('local-arch-root','local-arch-child')
                   AND archived = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived_count, 2);
}

#[test]
fn list_sessions_excludes_soft_deleted_and_restores() {
    use crate::test_support::mem_db;
    let c = mem_db();
    crate::db::create_session(&c, "s-live", "live", "local-default", "local").unwrap();
    crate::db::create_session(&c, "s-del", "deleted", "local-default", "local").unwrap();
    crate::db::set_session_deleted(&c, "s-del").unwrap();

    // Run same SQL as list_sessions (with deleted_at IS NULL filter)
    let mut stmt = c
        .prepare(
            "SELECT id FROM sessions WHERE deleted_at IS NULL ORDER BY created_at DESC, id DESC",
        )
        .unwrap();
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        ids.contains(&"s-live".to_string()),
        "live session must be in list"
    );
    assert!(
        !ids.contains(&"s-del".to_string()),
        "soft-deleted session must not be in list"
    );

    // After restore, soft-deleted session must reappear in list
    crate::db::restore_session(&c, "s-del").unwrap();
    let mut stmt2 = c
        .prepare(
            "SELECT id FROM sessions WHERE deleted_at IS NULL ORDER BY created_at DESC, id DESC",
        )
        .unwrap();
    let ids2: Vec<String> = stmt2
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        ids2.contains(&"s-del".to_string()),
        "restored session must reappear in list"
    );
}

#[test]
fn purge_session_refuses_live_session() {
    // 🔴 C-1(codex+opus T5 审):purge 只对软删(tombstoned)会话——live 会话调 purge 必拒
    // (SESSION_NOT_TRASHED)·DB 行不动(防 db::delete_session 无条件级联硬删 live·不可逆)。
    // 前置门在 resolve 之前·与 workspace 类型(Local/Repo/Err)无关·故测试 resolve-env 无关。
    use crate::test_support::mem_db;
    let c = mem_db();
    crate::db::create_session(&c, "s-live", "t", "local-default", "local").unwrap();
    let r = purge_session_inner(&c, "s-live");
    assert!(
        matches!(&r, Err(e) if e.starts_with("SESSION_NOT_TRASHED")),
        "🔴 live 会话 purge 必拒(SESSION_NOT_TRASHED)"
    );
    let exists: i64 = c
        .query_row("SELECT COUNT(*) FROM sessions WHERE id='s-live'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(exists, 1, "🔴 purge 被拒后 DB 行必须还在(没被不可逆硬删)");
}

#[test]
fn purge_session_cleans_up_journal_dir() {
    // 刀 R R5:硬删(purge)级联清 ~/.agentloom/journals/<id>/ ——存储大头(单 run ~3.3MB)。
    // Local workspace 免 git gc 干扰。HOME 隔离(TestHomeGuard + test_home_lock)绝不碰真实 HOME。
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    use crate::test_support::mem_db;
    let c = mem_db();
    let id = "purge-journal-cleanup";
    db::create_session(&c, id, "t", "local-default", "local").unwrap();
    db::set_session_deleted(&c, id).unwrap();

    let journal_dir = crate::worktree::journals_dir().join(id);
    std::fs::create_dir_all(&journal_dir).unwrap();
    std::fs::write(journal_dir.join("run.jsonl"), b"{}").unwrap();
    assert!(
        journal_dir.exists(),
        "test setup must create fake journal file"
    );

    purge_session_inner(&c, id).unwrap();

    assert!(
        !journal_dir.exists(),
        "🔴 purge 必须级联删掉 journal 目录(存储泄漏)"
    );
}

#[test]
fn purge_session_succeeds_when_journal_dir_missing() {
    // 目录不存在时 purge 照常成功(best-effort·不因缺目录报错)。
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    use crate::test_support::mem_db;
    let c = mem_db();
    let id = "purge-journal-missing";
    db::create_session(&c, id, "t", "local-default", "local").unwrap();
    db::set_session_deleted(&c, id).unwrap();

    let journal_dir = crate::worktree::journals_dir().join(id);
    assert!(
        !journal_dir.exists(),
        "test setup must start without journal dir"
    );

    let r = purge_session_inner(&c, id);
    assert!(r.is_ok(), "journal 目录缺失不应挡 purge: {r:?}");
}
