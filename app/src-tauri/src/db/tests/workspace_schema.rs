#![cfg(test)]

use super::*;

#[test]
fn init_schema_creates_repos_table_with_all_columns() {
    let c = mem();
    let mut stmt = c.prepare("PRAGMA table_info(repos)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // spec §3.2 完整 8 列
    for name in [
        "id",
        "source",
        "owner",
        "name",
        "path",
        "status",
        "added_at",
        "last_used_at",
    ] {
        assert!(
            cols.contains(&name.into()),
            "repos 应含列 {name}：实际 {cols:?}"
        );
    }
}

#[test]
fn project_first_migration_adds_icon_column_idempotent() {
    let c = Connection::open_in_memory().unwrap();
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(repos)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"icon".into()),
        "repos 应含 icon 列：{cols:?}"
    );
    drop(stmt);

    init_schema(&c).unwrap();
}

#[test]
fn project_first_migration_renames_color_to_icon_and_clears_hex() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE repos (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL DEFAULT 'local',
                owner TEXT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL DEFAULT 'active',
                added_at INTEGER NOT NULL,
                last_used_at INTEGER,
                color TEXT
            );
            INSERT INTO repos (id, name, path, added_at, color)
            VALUES ('hex', 'hex', '/tmp/hex', 1, '#7c3aed');
            INSERT INTO repos (id, name, path, added_at, color)
            VALUES ('emoji', 'emoji', '/tmp/emoji', 2, '📕');",
    )
    .unwrap();

    init_schema(&c).unwrap();
    init_schema(&c).unwrap();

    let cols: Vec<String> = c
        .prepare("PRAGMA table_info(repos)")
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(cols.contains(&"icon".into()));
    assert!(!cols.contains(&"color".into()));
    let hex: Option<String> = c
        .query_row("SELECT icon FROM repos WHERE id = 'hex'", [], |r| r.get(0))
        .unwrap();
    let emoji: Option<String> = c
        .query_row("SELECT icon FROM repos WHERE id = 'emoji'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(hex, None);
    assert_eq!(emoji.as_deref(), Some("📕"));
}

/// R-B2 项 1（隔离刀返工二·祖父条款）迁移测试：模拟老 DB（本列刚加时那一刻）——
/// 迁移前已存在的 local-default 会话必须回填 `workspace_scope='root'`；同一时刻已存在的
/// 非 local-default 会话不受祖父条款影响，留 NULL；迁移**之后**新建的 local-default 会话
/// 必须留 NULL（走新行为 · per-session 子目录）；再跑一次 `init_schema`（列已存在）必须
/// 是纯 no-op，不得把刚建的正常新会话误判成祖父、回填成 root。
/// R-B3 项 3（Minor-2·迁移原子性）确认：加列 + 回填现已包进 `unchecked_transaction`——
/// 事务只改变「中途崩溃是否留半吊子状态」这一失败路径，不改变成功路径的可观察结果，所以
/// 本测试原有的「加列→回填→幂等复跑」断言链本身就是事务化后行为的回归覆盖，未新增用例。
#[test]
fn workspace_scope_migration_backfills_only_sessions_that_predate_the_column() {
    let c = mem();
    // 模拟老 DB：这一列还不存在（真实历史升级路径 = 从没有这列的旧 schema 启动）。
    c.execute_batch("ALTER TABLE sessions DROP COLUMN workspace_scope")
        .unwrap();

    // 迁移前已存在的 local-default 会话（旧产物散落项目根的老数据）。
    create_session(&c, "s-legacy-local", "t", "local-default", "local").unwrap();
    // 迁移前已存在的真实 repo 会话——祖父条款只认 local-default，这条不该被动。
    crate::namespaces_repo::add_namespace(&c, "ns-legacy-real", "github_org", "Real", 0).unwrap();
    crate::repos_repo::add_repo(
        &c,
        "repo-legacy-real",
        "ns-legacy-real",
        "github",
        Some("owner"),
        "repo",
        "/tmp/legacy-real",
        None,
    )
    .unwrap();
    create_session(
        &c,
        "s-legacy-real",
        "t",
        "repo-legacy-real",
        "ns-legacy-real",
    )
    .unwrap();

    // 触发迁移：列刚创建的这一刻，加列 + 回填。
    init_schema(&c).unwrap();

    let scope_of = |id: &str| -> Option<String> {
        c.query_row(
            "SELECT workspace_scope FROM sessions WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        scope_of("s-legacy-local").as_deref(),
        Some("root"),
        "迁移前已存在的 local-default 会话必须回填 root"
    );
    assert_eq!(
        scope_of("s-legacy-real"),
        None,
        "非 local-default 会话不受祖父条款影响，应留 NULL"
    );

    // 迁移后（列已存在）新建的 local-default 会话必须留 NULL——走新行为。
    create_session(&c, "s-fresh-local", "t", "local-default", "local").unwrap();
    assert_eq!(
        scope_of("s-fresh-local"),
        None,
        "迁移后新建会话必须留 NULL（新行为 · per-session 子目录），不能被祖父条款误伤"
    );

    // 幂等：列已存在后再跑一次 init_schema，不得把刚建的新会话回填成 root。
    init_schema(&c).unwrap();
    assert_eq!(
        scope_of("s-fresh-local"),
        None,
        "列已存在之后的启动必须整段跳过回填，否则会把正常新会话打回祖父模式"
    );
    assert_eq!(
        scope_of("s-legacy-local").as_deref(),
        Some("root"),
        "幂等：老会话的 root 回填不应被第二次 init_schema 改变"
    );
}

#[test]
fn project_first_migration_renames_seed_local_default() {
    let c = mem();
    c.execute(
        "UPDATE repos SET name = 'Local 默认' WHERE id = 'local-default'",
        [],
    )
    .unwrap();

    let n = migrate_local_default_name(&c).unwrap();
    assert_eq!(n, 1);
    let name: String = c
        .query_row(
            "SELECT name FROM repos WHERE id = 'local-default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "我的项目");
    assert_eq!(migrate_local_default_name(&c).unwrap(), 0);
}

#[test]
fn project_first_migration_preserves_user_renamed() {
    let c = mem();
    c.execute(
        "UPDATE repos SET name = 'foo' WHERE id = 'local-default'",
        [],
    )
    .unwrap();

    assert_eq!(migrate_local_default_name(&c).unwrap(), 0);
    let name: String = c
        .query_row(
            "SELECT name FROM repos WHERE id = 'local-default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "foo");
}

#[test]
fn init_schema_adds_repo_id_to_sessions_idempotent() {
    // 模拟「旧库无 repo_id 列」→ 调 init_schema 两次都应 OK 且加上列
    let c = Connection::open_in_memory().unwrap();
    // 先用旧 schema 建 sessions（无 repo_id）
    c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();
    // 跑 init_schema → 应给 sessions 加 repo_id 列
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"repo_id".into()),
        "sessions 应含 repo_id 列：实际 {cols:?}"
    );
    // 再跑一次（幂等性 · 旧库二次启动）应不报错
    init_schema(&c).unwrap();
}

#[test]
fn session_usage_migration_idempotent() {
    let c = Connection::open_in_memory().unwrap();
    c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();

    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<(String, i64, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(1)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in ["total_input_tokens", "total_output_tokens"] {
        let (_, not_null, default) = cols
            .iter()
            .find(|(column, _, _)| column == name)
            .unwrap_or_else(|| panic!("sessions 应含 {name} 列：实际 {cols:?}"));
        assert_eq!(*not_null, 1, "{name} 应为 NOT NULL");
        assert_eq!(default.as_deref(), Some("0"), "{name} 应 DEFAULT 0");
    }

    init_schema(&c).unwrap();
}

#[test]
fn session_usage_accumulates() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();

    add_session_usage(&c, "s1", Some(100), Some(50)).unwrap();
    add_session_usage(&c, "s1", Some(10), None).unwrap();

    let totals: (i64, i64) = c
        .query_row(
            "SELECT total_input_tokens, total_output_tokens FROM sessions WHERE id = 's1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(totals, (110, 50));
}

#[test]
fn session_usage_none_as_zero() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET total_input_tokens = 17, total_output_tokens = 29 WHERE id = 's1'",
        [],
    )
    .unwrap();

    add_session_usage(&c, "s1", None, None).unwrap();
    add_session_usage(&c, "missing", Some(3), Some(5)).unwrap();

    let totals: (i64, i64) = c
        .query_row(
            "SELECT total_input_tokens, total_output_tokens FROM sessions WHERE id = 's1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(totals, (17, 29));
}

#[test]
fn get_session_repo_id_returns_default_when_created() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    assert_eq!(
        get_session_repo_id(&c, "s1").unwrap(),
        Some("local-default".into())
    );
}

#[test]
fn get_session_repo_id_returns_some_when_set() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // 手动插一个 repo 然后绑定（不依赖 repos_repo 模块，Task 3 才写）
    c.execute(
            "INSERT INTO repos (id, source, name, path, status, added_at) VALUES ('r1', 'local', 'demo', '/tmp/demo', 'active', strftime('%s','now'))",
            [],
        )
        .unwrap();
    c.execute("UPDATE sessions SET repo_id = 'r1' WHERE id = 's1'", [])
        .unwrap();
    assert_eq!(get_session_repo_id(&c, "s1").unwrap(), Some("r1".into()));
}

#[test]
fn init_schema_creates_namespaces_table_with_all_columns() {
    let c = mem();
    let mut stmt = c.prepare("PRAGMA table_info(namespaces)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // spec §3.2 完整 7 列
    for name in [
        "id",
        "kind",
        "name",
        "is_builtin",
        "last_active_repo_id",
        "added_at",
        "last_used_at",
    ] {
        assert!(
            cols.contains(&name.into()),
            "namespaces 应含列 {name}：实际 {cols:?}"
        );
    }
}

#[test]
fn init_schema_creates_session_groups_table() {
    let c = Connection::open_in_memory().unwrap();
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(session_groups)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in [
        "id",
        "namespace_id",
        "repo_id",
        "name",
        "position",
        "created_at",
    ] {
        assert!(
            cols.contains(&name.into()),
            "session_groups 应含列 {name}：实际 {cols:?}"
        );
    }
}

#[test]
fn init_schema_adds_namespace_id_to_repos_idempotent() {
    // 模拟「plan 1 旧库无 namespace_id 列」→ init_schema 应加上 · 二次跑幂等
    let c = Connection::open_in_memory().unwrap();
    // 先用 plan 1 旧 schema 建 repos（无 namespace_id）
    c.execute(
        "CREATE TABLE repos (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL DEFAULT 'local',
                owner TEXT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL DEFAULT 'active',
                added_at INTEGER NOT NULL,
                last_used_at INTEGER
            )",
        [],
    )
    .unwrap();
    // 老 row 入（模拟 plan 1 用户既有 repo）
    c.execute(
            "INSERT INTO repos (id, source, name, path, status, added_at) VALUES ('r-old', 'local', 'old-proj', '/tmp/old', 'active', 100)",
            [],
        )
        .unwrap();
    // 跑 init_schema → 应给 repos 加 namespace_id 列 DEFAULT 'local'
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(repos)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"namespace_id".into()),
        "repos 应含 namespace_id 列：{cols:?}"
    );
    // 老 row 自动归 'local'（DEFAULT 生效）
    let ns: String = c
        .query_row("SELECT namespace_id FROM repos WHERE id='r-old'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(ns, "local");
    // 二次跑幂等
    init_schema(&c).unwrap();
}

#[test]
fn init_schema_adds_namespace_id_to_sessions_idempotent() {
    // plan 2a 后旧库已有 sessions.repo_id · 但无 namespace_id
    let c = Connection::open_in_memory().unwrap();
    c.execute(
        "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                repo_id TEXT
            )",
        [],
    )
    .unwrap();
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"namespace_id".into()),
        "sessions 应含 namespace_id 列：{cols:?}"
    );
    // 二次跑幂等
    init_schema(&c).unwrap();
}

#[test]
fn init_schema_adds_group_id_to_sessions_idempotent() {
    let c = Connection::open_in_memory().unwrap();
    c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"group_id".into()),
        "sessions 应含 group_id 列：{cols:?}"
    );
    init_schema(&c).unwrap();
}

#[test]
fn init_schema_adds_continued_to_session_id_to_sessions_idempotent() {
    let c = Connection::open_in_memory().unwrap();
    c.execute(
        "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                repo_id TEXT,
                namespace_id TEXT,
                group_id TEXT,
                git_state TEXT,
                parent_session_id TEXT,
                pinned INTEGER NOT NULL DEFAULT 0,
                unread INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,
                archived_at INTEGER,
                deleted_at INTEGER
            )",
        [],
    )
    .unwrap();

    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"continued_to_session_id".into()),
        "sessions 应含 continued_to_session_id 列：{cols:?}"
    );
    init_schema(&c).unwrap();
}

#[test]
fn migrate_null_repo_id_to_local_default_handles_old_sessions() {
    // 模拟 plan 1 / plan 2a 旧库：sessions 有 repo_id NULL 的 row（默认 session 概念）
    let c = mem();
    // 先建 local-default repo（模拟 seed 已跑 · migration 前置条件）
    c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local', 'local', 'Local', 1, 100)",
            [],
        ).unwrap();
    c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default', 'local', 'local', 'Local 默认', '/tmp/local-default', 'active', 100)",
            [],
        ).unwrap();
    // 老 session repo_id NULL
    c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id) VALUES ('s-null', '默认会话', 100, NULL)",
            [],
        ).unwrap();
    // 老 session 已绑 repo（不该被改）
    c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id) VALUES ('s-bound', '已绑会话', 100, 'local-default')",
            [],
        ).unwrap();

    let n = migrate_null_repo_id_to_local_default(&c).unwrap();
    assert_eq!(n, 1, "应迁 1 个 null repo_id session");
    let s_null_rid: String = c
        .query_row("SELECT repo_id FROM sessions WHERE id='s-null'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(s_null_rid, "local-default");
    let s_bound_rid: String = c
        .query_row("SELECT repo_id FROM sessions WHERE id='s-bound'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(s_bound_rid, "local-default");

    let n2 = migrate_null_repo_id_to_local_default(&c).unwrap();
    assert_eq!(n2, 0);
}

#[test]
fn backfill_session_namespace_id_joins_via_repos() {
    let c = mem();
    c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local', 'local', 'Local', 1, 100)",
            [],
        ).unwrap();
    c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('ns-a', 'github_org', 'org-a', 0, 100)",
            [],
        ).unwrap();
    c.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('r-local', 'local', 'local', 'r-local', '/tmp/r-local', 'active', 100)",
            [],
        ).unwrap();
    c.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('r-ns-a', 'ns-a', 'github', 'r-ns-a', '/tmp/r-ns-a', 'active', 100)",
            [],
        ).unwrap();
    c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id, namespace_id) VALUES ('s-1', 's-1', 100, 'r-ns-a', 'local')",
            [],
        ).unwrap();
    c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id, namespace_id) VALUES ('s-2', 's-2', 100, 'r-local', 'local')",
            [],
        ).unwrap();

    let n = backfill_session_namespace_id(&c).unwrap();
    assert_eq!(n, 1, "应 backfill 1 个错的 namespace_id（s-1 应改成 ns-a）");
    let s1_ns: String = c
        .query_row(
            "SELECT namespace_id FROM sessions WHERE id='s-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(s1_ns, "ns-a");
    let s2_ns: String = c
        .query_row(
            "SELECT namespace_id FROM sessions WHERE id='s-2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(s2_ns, "local");

    let n2 = backfill_session_namespace_id(&c).unwrap();
    assert_eq!(n2, 0);
}
