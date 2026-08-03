//! repos 表 CRUD（与 db.rs 同层、纯数据层、无业务逻辑）。
//! 项目注册与交互业务落在 lib.rs IPC 入口；项目目录不要求是 git 仓库。

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// 项目记录（前端用 RepoMeta；后端 sqlite 表 = repos）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RepoMeta {
    pub id: String,
    pub source: String,        // 'local' | 'github'（MVP 仅 local）
    pub owner: Option<String>, // MVP NULL；未来 GitHub 接入版本 填
    pub name: String,          // display name（默认 = path 末段目录名）
    pub path: String,          // 本地项目目录绝对路径（UNIQUE）
    pub status: String,        // 'active' | 'archived' | 'invalid'
    pub added_at: i64,
    pub last_used_at: Option<i64>,
    /// cluster L Phase 2 plan A Task 6：所属 namespace（DEFAULT 'local' · plan 1 老 repo 自动归 Local）。
    pub namespace_id: String,
    pub icon: Option<String>,
}

/// 新增项目记录。调用方负责生成 UUID（lib.rs IPC 入口）；
/// 此函数只做 sqlite INSERT；path UNIQUE / namespace_id FK 冲突由调用方捕获并语义化。
pub fn add_repo(
    conn: &Connection,
    id: &str,
    namespace_id: &str,
    source: &str,
    owner: Option<&str>,
    name: &str,
    path: &str,
    icon: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO repos (id, namespace_id, source, owner, name, path, status, added_at, last_used_at, icon)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', strftime('%s','now'), NULL, ?7)",
        (id, namespace_id, source, owner, name, path, icon),
    )?;
    Ok(())
}

/// 按 id 查（找不到返回 None）。
pub fn get_repo_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<RepoMeta>> {
    conn.query_row(
        "SELECT id, source, owner, name, path, status, added_at, last_used_at, namespace_id, icon FROM repos WHERE id = ?1",
        [id],
        row_to_meta,
    )
    .optional()
}

/// 按 path 查（用于 UNIQUE check · add_repo 重复关联场景）。
pub fn get_repo_by_path(conn: &Connection, path: &str) -> rusqlite::Result<Option<RepoMeta>> {
    conn.query_row(
        "SELECT id, source, owner, name, path, status, added_at, last_used_at, namespace_id, icon FROM repos WHERE path = ?1",
        [path],
        row_to_meta,
    )
    .optional()
}

/// 仅 active 项目（crumb dropdown 主列表 · last_used_at desc）。
pub fn list_active(conn: &Connection) -> rusqlite::Result<Vec<RepoMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, source, owner, name, path, status, added_at, last_used_at, namespace_id, icon
         FROM repos WHERE status = 'active'
         ORDER BY COALESCE(last_used_at, added_at) DESC",
    )?;
    let rows = stmt.query_map([], row_to_meta)?;
    rows.collect()
}

/// 按 status 查（archived / invalid 给设置页或 invalid 修正对话框）。
pub fn list_by_status(conn: &Connection, status: &str) -> rusqlite::Result<Vec<RepoMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, source, owner, name, path, status, added_at, last_used_at, namespace_id, icon
         FROM repos WHERE status = ?1
         ORDER BY COALESCE(last_used_at, added_at) DESC",
    )?;
    let rows = stmt.query_map([status], row_to_meta)?;
    rows.collect()
}

/// cluster L Phase 2 plan A Task 6：list active repos by namespace（plan B 智能形态计算 + sidebar 分组用）。
pub fn list_active_by_namespace(
    conn: &Connection,
    namespace_id: &str,
) -> rusqlite::Result<Vec<RepoMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, source, owner, name, path, status, added_at, last_used_at, namespace_id, icon
         FROM repos WHERE status = 'active' AND namespace_id = ?1
         ORDER BY COALESCE(last_used_at, added_at) DESC",
    )?;
    let rows = stmt.query_map([namespace_id], row_to_meta)?;
    rows.collect()
}

/// 归档（不删 · ON DELETE SET NULL 留 session 历史绑定关系）。
pub fn archive_repo(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE repos SET status = 'archived' WHERE id = ?1", [id])?;
    Ok(())
}

/// 从 archived / invalid 恢复成 active（用户手动修正路径后）。
pub fn restore_repo(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE repos SET status = 'active' WHERE id = ?1", [id])?;
    Ok(())
}

/// 标 invalid（启动扫描 path 不存在时 · 不删）。
pub fn set_repo_invalid(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE repos SET status = 'invalid' WHERE id = ?1", [id])?;
    Ok(())
}

/// 切到该项目时刷 last_used_at（影响 crumb dropdown 排序）。
pub fn touch_last_used(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE repos SET last_used_at = strftime('%s','now') WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

pub fn rename_repo(conn: &Connection, id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE repos SET name = ?2 WHERE id = ?1", (id, name))?;
    Ok(())
}

pub fn set_repo_icon(conn: &Connection, id: &str, icon: Option<&str>) -> rusqlite::Result<()> {
    conn.execute("UPDATE repos SET icon = ?2 WHERE id = ?1", (id, icon))?;
    Ok(())
}

fn row_to_meta(r: &rusqlite::Row) -> rusqlite::Result<RepoMeta> {
    Ok(RepoMeta {
        id: r.get(0)?,
        source: r.get(1)?,
        owner: r.get(2)?,
        name: r.get(3)?,
        path: r.get(4)?,
        status: r.get(5)?,
        added_at: r.get(6)?,
        last_used_at: r.get(7)?,
        namespace_id: r.get(8)?,
        icon: r.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mem_db;

    #[test]
    fn add_then_get_by_id_roundtrips() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "demo", "/tmp/demo", None).unwrap();
        let r = get_repo_by_id(&c, "r1").unwrap().unwrap();
        assert_eq!(r.id, "r1");
        assert_eq!(r.source, "local");
        assert_eq!(r.name, "demo");
        assert_eq!(r.path, "/tmp/demo");
        assert_eq!(r.status, "active");
        assert_eq!(r.last_used_at, None);
    }

    #[test]
    fn get_by_id_missing_returns_none() {
        let c = mem_db();
        assert_eq!(get_repo_by_id(&c, "nope").unwrap(), None);
    }

    #[test]
    fn add_repo_path_unique_errors_on_duplicate() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "demo", "/tmp/demo", None).unwrap();
        let err =
            add_repo(&c, "r2", "local", "local", None, "demo2", "/tmp/demo", None).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("unique"), "{err}");
    }

    #[test]
    fn get_by_path_finds_existing() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "demo", "/tmp/demo", None).unwrap();
        let r = get_repo_by_path(&c, "/tmp/demo").unwrap().unwrap();
        assert_eq!(r.id, "r1");
        assert_eq!(get_repo_by_path(&c, "/tmp/nope").unwrap(), None);
    }

    #[test]
    fn list_active_returns_only_active_in_last_used_desc() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "a", "/tmp/a", None).unwrap();
        add_repo(&c, "r2", "local", "local", None, "b", "/tmp/b", None).unwrap();
        add_repo(&c, "r3", "local", "local", None, "c", "/tmp/c", None).unwrap();
        archive_repo(&c, "r2").unwrap();
        // 显式设 last_used_at 让顺序可预测（不依赖 added_at 的秒级精度）
        c.execute("UPDATE repos SET last_used_at = 200 WHERE id = 'r1'", [])
            .unwrap();
        c.execute("UPDATE repos SET last_used_at = 100 WHERE id = 'r3'", [])
            .unwrap();
        let list = list_active(&c).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, "r1");
        assert_eq!(list[1].id, "r3");
        assert_eq!(list[2].id, "local-default");
    }

    #[test]
    fn list_by_status_filters() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "a", "/tmp/a", None).unwrap();
        add_repo(&c, "r2", "local", "local", None, "b", "/tmp/b", None).unwrap();
        set_repo_invalid(&c, "r2").unwrap();
        let inv = list_by_status(&c, "invalid").unwrap();
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].id, "r2");
        let arch = list_by_status(&c, "archived").unwrap();
        assert_eq!(arch.len(), 0);
    }

    #[test]
    fn archive_restore_round_trip() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "a", "/tmp/a", None).unwrap();
        archive_repo(&c, "r1").unwrap();
        assert_eq!(
            get_repo_by_id(&c, "r1").unwrap().unwrap().status,
            "archived"
        );
        restore_repo(&c, "r1").unwrap();
        assert_eq!(get_repo_by_id(&c, "r1").unwrap().unwrap().status, "active");
    }

    #[test]
    fn set_invalid_keeps_row() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "a", "/tmp/a", None).unwrap();
        set_repo_invalid(&c, "r1").unwrap();
        let r = get_repo_by_id(&c, "r1").unwrap().unwrap();
        assert_eq!(r.status, "invalid");
        assert_eq!(r.path, "/tmp/a"); // 不删 path，留给修正对话框
    }

    #[test]
    fn touch_last_used_updates_field() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "a", "/tmp/a", None).unwrap();
        let before = get_repo_by_id(&c, "r1").unwrap().unwrap().last_used_at;
        assert_eq!(before, None);
        touch_last_used(&c, "r1").unwrap();
        let after = get_repo_by_id(&c, "r1").unwrap().unwrap().last_used_at;
        assert!(after.is_some());
    }

    #[test]
    fn source_check_rejects_unknown_value() {
        let c = mem_db();
        let err = add_repo(&c, "r1", "local", "ftp", None, "a", "/tmp/a", None).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("check"), "{err}");
    }

    #[test]
    fn delete_repo_cascades_to_session_repo_id_null() {
        // ON DELETE SET NULL · spec §3.2 关键不变量
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "a", "/tmp/a", None).unwrap();
        crate::db::create_session(&c, "s1", "x", "local-default", "local").unwrap();
        c.execute("UPDATE sessions SET repo_id = 'r1' WHERE id = 's1'", [])
            .unwrap();
        // 需要 FK 开（sqlite 默认关）
        c.execute("PRAGMA foreign_keys = ON", []).unwrap();
        c.execute("DELETE FROM repos WHERE id = 'r1'", []).unwrap();
        let rid = crate::db::get_session_repo_id(&c, "s1").unwrap();
        assert_eq!(rid, None, "ON DELETE SET NULL 应把 repo_id 置 NULL");
    }

    #[test]
    fn add_repo_with_namespace_id_stored_correctly() {
        let c = mem_db();
        add_repo(&c, "r1", "local", "local", None, "demo", "/tmp/demo", None).unwrap();
        let r = get_repo_by_id(&c, "r1").unwrap().unwrap();
        assert_eq!(r.namespace_id, "local");
    }

    #[test]
    fn list_active_by_namespace_filters_correctly() {
        let c = mem_db();
        archive_repo(&c, "local-default").unwrap();
        crate::namespaces_repo::add_namespace(&c, "ns-a", "github_org", "org-a", 0).unwrap();
        add_repo(
            &c,
            "r-local-1",
            "local",
            "local",
            None,
            "l1",
            "/tmp/l1",
            None,
        )
        .unwrap();
        add_repo(
            &c,
            "r-local-2",
            "local",
            "local",
            None,
            "l2",
            "/tmp/l2",
            None,
        )
        .unwrap();
        add_repo(&c, "r-a-1", "ns-a", "github", None, "a1", "/tmp/a1", None).unwrap();
        add_repo(
            &c,
            "r-a-arch",
            "ns-a",
            "github",
            None,
            "arch",
            "/tmp/arch",
            None,
        )
        .unwrap();
        archive_repo(&c, "r-a-arch").unwrap();

        let local_repos = list_active_by_namespace(&c, "local").unwrap();
        assert_eq!(local_repos.len(), 2);
        assert!(local_repos.iter().all(|r| r.namespace_id == "local"));

        let ns_a_repos = list_active_by_namespace(&c, "ns-a").unwrap();
        assert_eq!(ns_a_repos.len(), 1);
        assert_eq!(ns_a_repos[0].id, "r-a-1");
    }

    #[test]
    fn list_active_by_namespace_empty_returns_empty_vec() {
        let c = mem_db();
        crate::namespaces_repo::add_namespace(&c, "ns-empty", "github_org", "empty", 0).unwrap();
        let r = list_active_by_namespace(&c, "ns-empty").unwrap();
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn project_first_cmd_icon_roundtrips() {
        let c = mem_db();
        add_repo(
            &c,
            "r-icon",
            "local",
            "local",
            None,
            "图标项目",
            "/tmp/icon",
            Some("📕"),
        )
        .unwrap();
        add_repo(
            &c,
            "r-no-icon",
            "local",
            "local",
            None,
            "无图标项目",
            "/tmp/no-icon",
            None,
        )
        .unwrap();

        assert_eq!(
            get_repo_by_id(&c, "r-icon").unwrap().unwrap().icon,
            Some("📕".into())
        );
        assert_eq!(get_repo_by_id(&c, "r-no-icon").unwrap().unwrap().icon, None);
    }
}
