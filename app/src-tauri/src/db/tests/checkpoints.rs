#![cfg(test)]

use super::*;

#[test]
fn checkpoint_entries_schema_enforces_first_preimage_per_run_path() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s1', 'r1', '/tmp/file.txt', 0, 1)",
        [],
    )
    .unwrap();
    assert!(conn
        .execute(
            "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, created_at) \
                 VALUES ('s1', 'r1', '/tmp/file.txt', 1, 2)",
            [],
        )
        .is_err());
    let indexed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_checkpoint_entries_run'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 1);
}

#[test]
fn checkpoint_entries_schema_migrates_preimage_and_undo_columns_idempotently() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE checkpoint_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                member_id TEXT,
                file_path TEXT NOT NULL,
                existed INTEGER NOT NULL,
                blob_sha TEXT,
                file_mode INTEGER,
                is_symlink INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                UNIQUE (session_id, run_id, file_path)
            );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s1', 'r1', '/tmp/legacy.txt', 1, 1)",
        [],
    )
    .unwrap();

    init_schema(&conn).unwrap();
    init_schema(&conn).unwrap();

    let columns = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(checkpoint_entries)")
            .unwrap();
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        columns
    };
    assert!(columns.iter().any(|column| column == "allowed_root"));
    assert!(columns.iter().any(|column| column == "pre_xattrs"));
    assert!(columns.iter().any(|column| column == "undone_at"));
    let defaults: i64 = conn
            .query_row(
                "SELECT (allowed_root IS NULL AND pre_xattrs IS NULL AND undone_at IS NULL) FROM checkpoint_entries \
                 WHERE session_id = 's1' AND run_id = 'r1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(defaults, 1);
}
