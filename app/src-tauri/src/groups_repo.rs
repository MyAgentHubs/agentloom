//! session_groups 表 CRUD（Local virtual groups 持久层）。

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct GroupMeta {
    pub id: String,
    pub namespace_id: String,
    pub repo_id: String,
    pub name: String,
    pub position: i64,
    pub created_at: i64,
}

pub fn create_group(
    conn: &Connection,
    id: &str,
    repo_id: &str,
    name: &str,
    position: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO session_groups (id, namespace_id, repo_id, name, position, created_at)
         VALUES (?1, (SELECT namespace_id FROM repos WHERE id = ?2), ?2, ?3, ?4, strftime('%s','now'))",
        (id, repo_id, name, position),
    )?;
    Ok(())
}

pub fn rename_group(conn: &Connection, id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE session_groups SET name = ?2 WHERE id = ?1",
        (id, name),
    )?;
    Ok(())
}

/// 删 group · 下属 sessions.group_id 归 NULL（Ungrouped）。
pub fn delete_group(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET group_id = NULL WHERE group_id = ?1",
        [id],
    )?;
    conn.execute("DELETE FROM session_groups WHERE id = ?1", [id])?;
    Ok(())
}

pub fn list_by_repo(conn: &Connection, repo_id: &str) -> rusqlite::Result<Vec<GroupMeta>> {
    let mut stmt = conn.prepare(
        "SELECT id, namespace_id, repo_id, name, position, created_at
         FROM session_groups
         WHERE repo_id = ?1
         ORDER BY position ASC, created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([repo_id], |r| {
        Ok(GroupMeta {
            id: r.get(0)?,
            namespace_id: r.get(1)?,
            repo_id: r.get(2)?,
            name: r.get(3)?,
            position: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn next_position(conn: &Connection, repo_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM session_groups WHERE repo_id = ?1",
        [repo_id],
        |r| r.get(0),
    )
}

/// 移动 session 入/出 group · None = Ungrouped。
pub fn move_session_to_group(
    conn: &Connection,
    session_id: &str,
    group_id: Option<&str>,
) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let session_repo_id: String = tx
        .query_row(
            "SELECT repo_id FROM sessions WHERE id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
    let chain_ids =
        crate::db::continuation_chain_ids(&tx, session_id).map_err(|e| e.to_string())?;

    if let Some(group_id) = group_id {
        let group_repo_id: String = tx
            .query_row(
                "SELECT repo_id FROM session_groups WHERE id = ?1",
                [group_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "GROUP_NOT_FOUND".to_string())?;

        if session_repo_id != group_repo_id {
            return Err("GROUP_REPO_MISMATCH".to_string());
        }
        for id in &chain_ids {
            let member_repo_id: String = tx
                .query_row("SELECT repo_id FROM sessions WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "SESSION_NOT_FOUND".to_string())?;
            if member_repo_id != group_repo_id {
                return Err("GROUP_REPO_MISMATCH".to_string());
            }
        }
    }

    for id in &chain_ids {
        tx.execute(
            "UPDATE sessions SET group_id = ?2 WHERE id = ?1",
            (id.as_str(), group_id),
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::test_support::mem_db;

    #[test]
    fn create_list_rename_delete_group() {
        let c = mem_db();

        create_group(&c, "g1", "local-default", "头脑风暴", 0).unwrap();
        create_group(&c, "g2", "local-default", "待办", 1).unwrap();

        let gs = list_by_repo(&c, "local-default").unwrap();
        assert_eq!(gs.len(), 2);
        assert_eq!(gs[0].id, "g1");
        assert_eq!(gs[0].namespace_id, "local");
        assert_eq!(gs[0].repo_id, "local-default");
        assert_eq!(gs[0].name, "头脑风暴");
        assert_eq!(gs[0].position, 0);
        assert!(gs[0].created_at > 0);
        assert_eq!(gs[1].id, "g2");

        rename_group(&c, "g1", "脑暴改名").unwrap();
        assert_eq!(
            list_by_repo(&c, "local-default").unwrap()[0].name,
            "脑暴改名"
        );

        delete_group(&c, "g1").unwrap();
        let remaining = list_by_repo(&c, "local-default").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "g2");
    }

    #[test]
    fn move_session_and_delete_group_sets_null() {
        let c = mem_db();
        db::create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        create_group(&c, "g1", "local-default", "组", 0).unwrap();

        move_session_to_group(&c, "s1", Some("g1")).unwrap();
        let gid: Option<String> = c
            .query_row("SELECT group_id FROM sessions WHERE id='s1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(gid, Some("g1".to_string()));

        delete_group(&c, "g1").unwrap();
        let gid2: Option<String> = c
            .query_row("SELECT group_id FROM sessions WHERE id='s1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(gid2, None);

        move_session_to_group(&c, "s1", None).unwrap();
        let gid3: Option<String> = c
            .query_row("SELECT group_id FROM sessions WHERE id='s1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(gid3, None);
    }

    #[test]
    fn move_continuation_child_moves_whole_chain_group() {
        let c = mem_db();
        db::create_session(&c, "move-root", "root", "local-default", "local").unwrap();
        db::create_session(&c, "move-child", "child", "local-default", "local").unwrap();
        db::set_session_parent(&c, "move-child", Some("move-root")).unwrap();
        db::set_session_continued_to(&c, "move-root", Some("move-child")).unwrap();
        create_group(&c, "g-thread", "local-default", "Thread", 0).unwrap();

        move_session_to_group(&c, "move-child", Some("g-thread")).unwrap();

        let rows: Vec<(String, Option<String>)> = c
            .prepare(
                "SELECT id, group_id FROM sessions
                 WHERE id IN ('move-root','move-child')
                 ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("move-child".to_string(), Some("g-thread".to_string())),
                ("move-root".to_string(), Some("g-thread".to_string())),
            ]
        );
    }

    #[test]
    fn group_bound_to_repo_and_listed_by_repo() {
        let c = mem_db();
        c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('repoA', 'local', 'local', 'Repo A', '/tmp/repoA', 'active', 0)",
            [],
        ).unwrap();
        c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('repoB', 'local', 'local', 'Repo B', '/tmp/repoB', 'active', 0)",
            [],
        ).unwrap();
        create_group(&c, "g1", "repoA", "前端", 0).unwrap();
        create_group(&c, "g2", "repoB", "后端", 0).unwrap();
        let a = list_by_repo(&c, "repoA").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].repo_id, "repoA");
        assert_eq!(a[0].position, 0);
        let pos = next_position(&c, "repoA").unwrap();
        create_group(&c, "g3", "repoA", "调研", pos).unwrap();
        let a2 = list_by_repo(&c, "repoA").unwrap();
        assert_eq!(a2.len(), 2);
        assert!(a2[1].position > a2[0].position);
    }

    #[test]
    fn move_guards_repo_consistency() {
        let c = mem_db();
        c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('repoA', 'local', 'local', 'Repo A', '/tmp/repoA', 'active', 0)",
            [],
        ).unwrap();
        c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('repoB', 'local', 'local', 'Repo B', '/tmp/repoB', 'active', 0)",
            [],
        ).unwrap();

        db::create_session(&c, "s1", "测试", "repoA", "local").unwrap();
        create_group(&c, "gA", "repoA", "组A", 0).unwrap();
        create_group(&c, "gB", "repoB", "组B", 0).unwrap();

        assert!(move_session_to_group(&c, "s1", Some("gA")).is_ok());
        assert!(move_session_to_group(&c, "s1", Some("gB"))
            .unwrap_err()
            .contains("GROUP_REPO_MISMATCH"));
        assert!(move_session_to_group(&c, "s1", None).is_ok());
        assert!(move_session_to_group(&c, "no-such", Some("gA"))
            .unwrap_err()
            .contains("SESSION_NOT_FOUND"));
        assert!(move_session_to_group(&c, "s1", Some("no-group"))
            .unwrap_err()
            .contains("GROUP_NOT_FOUND"));
    }
}
