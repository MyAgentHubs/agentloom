//! namespaces 表 CRUD（与 db.rs / repos_repo.rs 同层、纯数据层、无业务逻辑）。
//! 业务（is_builtin 守门 / 启动 seed / fallback 规则）落在 lib.rs IPC 入口 + 业务函数。
//!
//! cluster L Phase 2 plan A Task 3 新增 · spec §3.2 / §3.3 / §4.2

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// namespace 记录（前端用 NamespaceMeta；后端 sqlite 表 = namespaces）。
/// spec §3.2 完整 7 字段。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct NamespaceMeta {
    pub id: String,
    pub kind: String, // 'local' | 'github_org'（SQL CHECK 约束）
    pub name: String,
    pub is_builtin: i64, // 0 | 1（rusqlite INTEGER）· 1=Local 不可删
    pub last_active_repo_id: Option<String>,
    pub added_at: i64,
    pub last_used_at: Option<i64>,
}

/// 新增 namespace 记录。调用方负责业务校验；SQL CHECK 兜底 kind 枚举。
#[allow(dead_code)] // plan A 先落数据层；GitHub org 接入时由业务 IPC 调用。
pub fn add_namespace(
    conn: &Connection,
    id: &str,
    kind: &str,
    name: &str,
    is_builtin: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO namespaces (id, kind, name, is_builtin, added_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%s','now'), NULL)",
        (id, kind, name, is_builtin),
    )?;
    Ok(())
}

/// github_org namespace upsert（同 owner 多次 connect 复用 · 不复用裸 add_namespace 防主键冲突）。
pub fn ensure_github_namespace(conn: &Connection, id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
         VALUES (?1, 'github_org', ?2, 0, strftime('%s','now'))",
        (id, name),
    )?;
    Ok(())
}

/// 按 id 查（找不到返 None）。
pub fn get_namespace_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<NamespaceMeta>> {
    conn.query_row(
        "SELECT id, kind, name, is_builtin, last_active_repo_id, added_at, last_used_at
         FROM namespaces WHERE id = ?1",
        [id],
        row_to_meta,
    )
    .optional()
}

/// 所有 namespaces（Local 顶 + 其余按 last_used_at desc）。
pub fn list_active_namespaces(conn: &Connection) -> rusqlite::Result<Vec<NamespaceMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, is_builtin, last_active_repo_id, added_at, last_used_at
         FROM namespaces
         ORDER BY (CASE WHEN is_builtin = 1 THEN 0 ELSE 1 END) ASC,
                  COALESCE(last_used_at, added_at) DESC",
    )?;
    let rows = stmt.query_map([], row_to_meta)?;
    rows.collect()
}

/// 归档 namespace。当前 schema 无 status，实施为物理删除；业务层负责拦 builtin。
#[allow(dead_code)] // plan A 先落数据层；plan B/后续 namespace 管理入口调用。
pub fn archive_namespace(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM namespaces WHERE id = ?1", [id])?;
    Ok(())
}

#[allow(dead_code)] // 当前 schema 无 status 字段；预留给后续 plan 2c 恢复语义。
pub fn restore_namespace(_conn: &Connection, _id: &str) -> rusqlite::Result<()> {
    Ok(())
}

/// per-namespace last_active_repo_id 记忆。
pub fn set_last_active_repo(
    conn: &Connection,
    namespace_id: &str,
    repo_id: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE namespaces SET last_active_repo_id = ?2 WHERE id = ?1",
        (namespace_id, repo_id),
    )?;
    Ok(())
}

/// 切到该 namespace 时刷 last_used_at。
pub fn touch_last_used(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE namespaces SET last_used_at = strftime('%s','now') WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

fn row_to_meta(r: &rusqlite::Row) -> rusqlite::Result<NamespaceMeta> {
    Ok(NamespaceMeta {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        is_builtin: r.get(3)?,
        last_active_repo_id: r.get(4)?,
        added_at: r.get(5)?,
        last_used_at: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mem_db;

    #[test]
    fn add_then_get_by_id_roundtrips() {
        let c = mem_db();
        add_namespace(&c, "ns-1", "github_org", "myagenthubs", 0).unwrap();
        let n = get_namespace_by_id(&c, "ns-1").unwrap().unwrap();
        assert_eq!(n.id, "ns-1");
        assert_eq!(n.kind, "github_org");
        assert_eq!(n.name, "myagenthubs");
        assert_eq!(n.is_builtin, 0);
        assert_eq!(n.last_active_repo_id, None);
    }

    #[test]
    fn ensure_github_namespace_is_idempotent() {
        let c = crate::test_support::mem_db();
        ensure_github_namespace(&c, "gh:acme", "acme").unwrap();
        ensure_github_namespace(&c, "gh:acme", "acme").unwrap(); // 第二次不报错
        let n = get_namespace_by_id(&c, "gh:acme").unwrap().unwrap();
        assert_eq!(n.kind, "github_org");
        assert_eq!(n.name, "acme");
        assert_eq!(n.is_builtin, 0);
    }

    #[test]
    fn get_by_id_missing_returns_none() {
        let c = mem_db();
        assert_eq!(get_namespace_by_id(&c, "nope").unwrap(), None);
    }

    #[test]
    fn list_active_namespaces_local_first_then_last_used_desc() {
        let c = mem_db();
        // Local 已由 mem_db seed；另加 2 个 github_org。
        add_namespace(&c, "ns-a", "github_org", "org-a", 0).unwrap();
        add_namespace(&c, "ns-b", "github_org", "org-b", 0).unwrap();
        c.execute(
            "UPDATE namespaces SET last_used_at = 200 WHERE id = 'ns-a'",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE namespaces SET last_used_at = 100 WHERE id = 'ns-b'",
            [],
        )
        .unwrap();
        let list = list_active_namespaces(&c).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, "local", "Local 必排首");
        assert_eq!(list[1].id, "ns-a", "github_org 按 last_used desc");
        assert_eq!(list[2].id, "ns-b");
    }

    #[test]
    fn kind_check_rejects_unknown_value() {
        let c = mem_db();
        let err = add_namespace(&c, "ns-x", "gitlab", "x", 0).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("check"), "{err}");
    }

    #[test]
    fn archive_namespace_physically_deletes_row() {
        let c = mem_db();
        add_namespace(&c, "ns-del", "github_org", "to-delete", 0).unwrap();
        archive_namespace(&c, "ns-del").unwrap();
        assert_eq!(get_namespace_by_id(&c, "ns-del").unwrap(), None);
    }

    #[test]
    fn set_last_active_repo_updates_field() {
        let c = mem_db();
        add_namespace(&c, "ns-1", "github_org", "x", 0).unwrap();
        set_last_active_repo(&c, "ns-1", Some("r-foo")).unwrap();
        let n = get_namespace_by_id(&c, "ns-1").unwrap().unwrap();
        assert_eq!(n.last_active_repo_id, Some("r-foo".into()));
        set_last_active_repo(&c, "ns-1", None).unwrap();
        let n2 = get_namespace_by_id(&c, "ns-1").unwrap().unwrap();
        assert_eq!(n2.last_active_repo_id, None);
    }

    #[test]
    fn touch_last_used_updates_field() {
        let c = mem_db();
        add_namespace(&c, "ns-1", "github_org", "x", 0).unwrap();
        let before = get_namespace_by_id(&c, "ns-1")
            .unwrap()
            .unwrap()
            .last_used_at;
        assert_eq!(before, None);
        touch_last_used(&c, "ns-1").unwrap();
        let after = get_namespace_by_id(&c, "ns-1")
            .unwrap()
            .unwrap()
            .last_used_at;
        assert!(after.is_some());
    }

    #[test]
    fn delete_namespace_cascades_to_repos() {
        let c = mem_db();
        add_namespace(&c, "ns-cascade", "github_org", "cascade-test", 0).unwrap();
        c.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('r-c', 'ns-cascade', 'local', 'r-c', '/tmp/cascade', 'active', 100)",
            [],
        )
        .unwrap();
        archive_namespace(&c, "ns-cascade").unwrap();
        let r: Option<String> = c
            .query_row("SELECT id FROM repos WHERE id='r-c'", [], |row| row.get(0))
            .optional()
            .unwrap();
        assert_eq!(r, None, "ON DELETE CASCADE 应连带删 repos");
    }
}
