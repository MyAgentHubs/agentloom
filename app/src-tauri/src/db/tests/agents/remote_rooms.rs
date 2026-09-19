#![cfg(test)]

use super::*;

// ---------------------------------------------------------------------------------
// M2-4a：project_remote_rooms schema + resolver + 分配
// ---------------------------------------------------------------------------------

#[test]
fn project_remote_room_schema_migration_is_idempotent() {
    let conn = mem();
    let room = ensure_remote_room_for_project(&conn, "proj-1").unwrap();

    // 重跑 init_schema（模拟应用重启再次建表）不得报错，也不得动已有行。
    init_schema(&conn).unwrap();
    init_schema(&conn).unwrap();

    assert_eq!(
        remote_room_for_project(&conn, "proj-1").unwrap(),
        Some(room)
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_remote_rooms", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1, "重跑迁移不得复制/丢失已有行");
}

/// R2 · 确定性替补：24 线程 barrier 测试只是「概率性」证明并发路径不产两个房——
/// PK/UNIQUE 这两条防线本身此前零直接覆盖（谁都可能手滑把它们从迁移 SQL 里删掉，
/// 而并发测试依然大概率绿，只是命中率从必红变成偶尔红）。这条测试直接读
/// `PRAGMA table_info`/`PRAGMA index_list` 断言约束「在」，两条任一被删都会确定性
/// 转红。
#[test]
fn project_remote_room_schema_enforces_project_id_pk_and_room_id_unique_not_null() {
    let conn = mem();

    // (name, notnull, pk) —— PRAGMA table_info 列序：cid,name,type,notnull,dflt_value,pk。
    let mut stmt = conn
        .prepare("PRAGMA table_info(project_remote_rooms)")
        .unwrap();
    let cols: Vec<(String, i64, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    drop(stmt);

    let project_id_col = cols
        .iter()
        .find(|(name, _, _)| name == "project_id")
        .expect("project_remote_rooms 必须有 project_id 列");
    assert_eq!(
        project_id_col.2, 1,
        "project_id 必须是主键（pk=1）：{cols:?}"
    );

    let room_id_col = cols
        .iter()
        .find(|(name, _, _)| name == "room_id")
        .expect("project_remote_rooms 必须有 room_id 列");
    assert_eq!(room_id_col.1, 1, "room_id 必须 NOT NULL：{cols:?}");

    // room_id 的 UNIQUE：TEXT NOT NULL UNIQUE 会让 SQLite 自动建一条
    // sqlite_autoindex_* 唯一索引，覆盖单列 room_id。
    let mut idx_stmt = conn
        .prepare("PRAGMA index_list(project_remote_rooms)")
        .unwrap();
    let indexes: Vec<(String, i64)> = idx_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    drop(idx_stmt);

    let mut found_room_id_unique_index = false;
    for (index_name, is_unique) in &indexes {
        if *is_unique != 1 {
            continue;
        }
        let mut cols_stmt = conn
            .prepare(&format!("PRAGMA index_info('{index_name}')"))
            .unwrap();
        let idx_cols: Vec<String> = cols_stmt
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        if idx_cols == vec!["room_id".to_string()] {
            found_room_id_unique_index = true;
        }
    }
    assert!(
        found_room_id_unique_index,
        "room_id 必须有单列 UNIQUE 索引：indexes={indexes:?}"
    );
}

/// R2 · 裸 SQL 语义测试：不经过 `ensure_remote_room_for_project`，直接验证
/// `INSERT OR IGNORE` 撞主键时的行为本身——已有行原样保留、不被覆盖、不报错。这是
/// `ensure_remote_room_for_project` 并发方案成立的底层前提，单独钉死。
#[test]
fn project_remote_room_insert_or_ignore_does_not_overwrite_existing_row() {
    let conn = mem();
    conn.execute(
        "INSERT INTO project_remote_rooms (project_id, room_id, created_at_ms) \
             VALUES ('p1', 'r1', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO project_remote_rooms (project_id, room_id, created_at_ms) \
             VALUES ('p1', 'r2', 2)",
        [],
    )
    .unwrap();

    let room: String = conn
        .query_row(
            "SELECT room_id FROM project_remote_rooms WHERE project_id = 'p1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        room, "r1",
        "INSERT OR IGNORE 撞主键必须原样保留旧行，不覆盖"
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM project_remote_rooms WHERE project_id = 'p1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "撞主键的 INSERT OR IGNORE 不得多插一行");
}

#[test]
fn project_remote_room_resolver_returns_none_when_unassigned() {
    let conn = mem();
    assert_eq!(
        remote_room_for_project(&conn, "proj-never-assigned").unwrap(),
        None
    );
}

#[test]
fn project_remote_room_assignment_is_idempotent_per_project() {
    let conn = mem();
    let first = ensure_remote_room_for_project(&conn, "proj-1").unwrap();
    let second = ensure_remote_room_for_project(&conn, "proj-1").unwrap();
    assert_eq!(first, second, "同一 project 两次分配必须拿到同一个 room_id");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM project_remote_rooms WHERE project_id = 'proj-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "同一 project 不得插出第二行");
}

#[test]
fn project_remote_room_assignment_differs_across_projects() {
    let conn = mem();
    let room_a = ensure_remote_room_for_project(&conn, "proj-a").unwrap();
    let room_b = ensure_remote_room_for_project(&conn, "proj-b").unwrap();
    assert_ne!(room_a, room_b, "不同 project 不得分到同一个 room_id");
}

#[test]
fn project_remote_room_id_shape_is_32_lowercase_hex() {
    let conn = mem();
    let room = ensure_remote_room_for_project(&conn, "proj-shape").unwrap();
    assert_eq!(room.len(), 32, "room_id 必须是 128-bit → 32 位 hex：{room}");
    assert!(
        room.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "room_id 必须是小写 hex：{room}"
    );
}

/// 并发安全的真实多连接实证：不用共享同一个 `Connection`（那样 app 层
/// `Db(Mutex<Connection>)` 早把并发串行化了，测不出 `ensure_remote_room_for_project`
/// 自身这层防线），而是给每个线程各开一条指向同一 sqlite 文件的独立连接，模拟
/// lib.rs 里偶尔另开连接（`cli_path_override_for_spawn`）那种不经过全局锁的路径。
#[test]
fn project_remote_room_assignment_is_concurrency_safe() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("m24a-concurrency.db");

    {
        let setup = Connection::open(&db_path).unwrap();
        init_schema(&setup).unwrap();
    }

    const THREAD_COUNT: usize = 24;
    // Barrier 让所有线程先各自开好连接、卡在同一起跑线，`wait()` 放行后几乎同一
    // 瞬间一起调用 `ensure_remote_room_for_project`——不这样做的话线程创建本身的
    // 调度抖动会把「先查后插」那个窗口拉开，race 命中率会掉到不可靠。
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREAD_COUNT));
    let handles: Vec<_> = (0..THREAD_COUNT)
        .map(|_| {
            let path = db_path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let conn = Connection::open(&path).unwrap();
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                barrier.wait();
                ensure_remote_room_for_project(&conn, "proj-race").unwrap()
            })
        })
        .collect();
    let results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let first = results[0].clone();
    assert!(
        results.iter().all(|room| *room == first),
        "并发分配必须收敛到同一个 room_id，实际观测到:{results:?}"
    );

    let verify = Connection::open(&db_path).unwrap();
    let count: i64 = verify
        .query_row(
            "SELECT COUNT(*) FROM project_remote_rooms WHERE project_id = 'proj-race'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "并发分配不得在表里落两行");
}
