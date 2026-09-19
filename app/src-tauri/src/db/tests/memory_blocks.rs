#![cfg(test)]

use super::*;

#[test]
fn memory_block_upsert_overwrites_and_get() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    // not exists -> None
    assert!(get_memory_block(&conn, "s1", "goal").unwrap().is_none());
    // first write
    upsert_memory_block(&conn, "s1", "goal", "重构感知管线", None, Some("app")).unwrap();
    let b = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
    assert_eq!(b.text, "重构感知管线");
    assert_eq!(b.title, None);
    assert_eq!(b.updated_by.as_deref(), Some("app"));
    // overwrite (same session+slot unique, upsert replaces)
    upsert_memory_block(
        &conn,
        "s1",
        "goal",
        "改登录流程",
        Some("登录流程"),
        Some("lead"),
    )
    .unwrap();
    let b2 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
    assert_eq!(b2.text, "改登录流程");
    assert_eq!(b2.title.as_deref(), Some("登录流程"));
    // still only one row
    let cnt: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_blocks WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cnt, 1);
    // different slot is independent
    assert!(get_memory_block(&conn, "s1", "state").unwrap().is_none());
}

#[test]
fn memory_blocks_old_schema_migrated_adds_revision_updated_run_id() {
    // 手造旧 7 列表（无 revision/updated_run_id）→ 插一行 → init_schema → 断言两列存在 + 旧行 revision=0 + 数据保留。
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(
            "CREATE TABLE memory_blocks (
                session_id TEXT NOT NULL,
                slot TEXT NOT NULL,
                text TEXT NOT NULL,
                title TEXT,
                anchor_refs_json TEXT NOT NULL DEFAULT '[]',
                updated_by TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (session_id, slot)
            );
            INSERT INTO memory_blocks (session_id, slot, text, updated_at) VALUES ('s1', 'goal', '旧文本', 0);",
        ).unwrap();
    // Must create prereqs for init_schema to succeed
    init_schema(&c).unwrap();
    // Check columns exist
    let cols: Vec<String> = {
        let mut stmt = c.prepare("PRAGMA table_info(memory_blocks)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert!(
        cols.iter().any(|c| c == "revision"),
        "revision column missing"
    );
    assert!(
        cols.iter().any(|c| c == "updated_run_id"),
        "updated_run_id column missing"
    );
    // Old row has revision=0 and data preserved
    let (text, rev): (String, i64) = c
        .query_row(
            "SELECT text, revision FROM memory_blocks WHERE session_id='s1' AND slot='goal'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(text, "旧文本");
    assert_eq!(rev, 0);
}

#[test]
fn memory_set_cas_rejects_stale_base_revision() {
    // 空库 memory_set(base=0) → Applied{1}；再 base=0 → Conflict{1}；base=1 → Applied{2} + 文本变。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    // First write: row doesn't exist, base_revision=0 → Applied
    let r1 = memory_set(&conn, "s1", "goal", "初始文本", None, Some("app"), None, 0).unwrap();
    assert_eq!(r1, MemorySetOutcome::Applied { revision: 1 });

    // Stale write: base_revision=0 again → Conflict
    let r2 = memory_set(&conn, "s1", "goal", "新文本", None, Some("app"), None, 0).unwrap();
    assert_eq!(
        r2,
        MemorySetOutcome::Conflict {
            current_revision: 1
        }
    );

    // Verify text and revision unchanged
    let b = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
    assert_eq!(b.text, "初始文本");
    assert_eq!(b.revision, 1);

    // Correct base_revision=1 → Applied{2} + text changed
    let r3 = memory_set(&conn, "s1", "goal", "新文本", None, Some("app"), None, 1).unwrap();
    assert_eq!(r3, MemorySetOutcome::Applied { revision: 2 });
    let b2 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
    assert_eq!(b2.text, "新文本");
    assert_eq!(b2.revision, 2);
}

#[test]
fn upsert_memory_block_bumps_revision() {
    // upsert 同格两次 → revision 从 1 升到 2，仍只 1 行。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    upsert_memory_block(&conn, "s1", "goal", "第一次", None, Some("app")).unwrap();
    let b1 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
    assert_eq!(b1.revision, 1);

    upsert_memory_block(&conn, "s1", "goal", "第二次", None, Some("lead")).unwrap();
    let b2 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
    assert_eq!(b2.revision, 2);
    assert_eq!(b2.text, "第二次");

    // Still only one row
    let cnt: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_blocks WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cnt, 1);
}

#[test]
fn compact_state_roundtrip_write_read_consistent() {
    let conn = mem();

    upsert_compact_state(&conn, "s1", "滚动摘要", 42, Some("run-1")).unwrap();

    assert_eq!(
        get_compact_state(&conn, "s1").unwrap(),
        Some(CompactState {
            summary: "滚动摘要".to_string(),
            through_message_id: 42,
            revision: 1,
        })
    );
    let block = get_memory_block(&conn, "s1", "compact").unwrap().unwrap();
    assert_eq!(block.anchor_refs_json, r#"[{"kind":"message","ref":42}]"#);
    assert_eq!(block.updated_by.as_deref(), Some("autocompact"));
    assert_eq!(block.updated_run_id.as_deref(), Some("run-1"));
}

#[test]
fn compact_state_missing_returns_none() {
    let conn = mem();

    assert_eq!(get_compact_state(&conn, "missing").unwrap(), None);
}

#[test]
fn compact_state_malformed_anchor_returns_none_without_panicking() {
    let conn = mem();

    for (session_id, anchor_refs_json) in [
        ("bad-json", "not-json"),
        ("empty", "[]"),
        ("string-ref", r#"[{"kind":"message","ref":"42"}]"#),
    ] {
        conn.execute(
            "INSERT INTO memory_blocks \
                 (session_id, slot, text, anchor_refs_json, updated_by, updated_at, revision) \
                 VALUES (?1, 'compact', '摘要', ?2, 'autocompact', 0, 1)",
            rusqlite::params![session_id, anchor_refs_json],
        )
        .unwrap();
        assert_eq!(get_compact_state(&conn, session_id).unwrap(), None);
    }
}

#[test]
fn compact_state_overwrite_increments_revision() {
    let conn = mem();
    upsert_compact_state(&conn, "s1", "摘要一", 10, Some("run-1")).unwrap();

    upsert_compact_state(&conn, "s1", "摘要二", 20, Some("run-2")).unwrap();

    assert_eq!(
        get_compact_state(&conn, "s1").unwrap(),
        Some(CompactState {
            summary: "摘要二".to_string(),
            through_message_id: 20,
            revision: 2,
        })
    );
}

#[test]
fn compact_state_watermark_regression_is_rejected() {
    let conn = mem();
    upsert_compact_state(&conn, "s1", "较新摘要", 20, Some("run-new")).unwrap();

    upsert_compact_state(&conn, "s1", "过期摘要", 10, Some("run-old")).unwrap();

    assert_eq!(
        get_compact_state(&conn, "s1").unwrap(),
        Some(CompactState {
            summary: "较新摘要".to_string(),
            through_message_id: 20,
            revision: 1,
        })
    );
    let block = get_memory_block(&conn, "s1", "compact").unwrap().unwrap();
    assert_eq!(block.updated_run_id.as_deref(), Some("run-new"));
}

#[test]
fn compact_state_equal_watermark_allows_overwrite() {
    let conn = mem();
    upsert_compact_state(&conn, "s1", "摘要一", 20, Some("run-1")).unwrap();

    upsert_compact_state(&conn, "s1", "摘要二", 20, Some("run-2")).unwrap();

    assert_eq!(
        get_compact_state(&conn, "s1").unwrap(),
        Some(CompactState {
            summary: "摘要二".to_string(),
            through_message_id: 20,
            revision: 2,
        })
    );
    let block = get_memory_block(&conn, "s1", "compact").unwrap().unwrap();
    assert_eq!(block.updated_run_id.as_deref(), Some("run-2"));
}

#[test]
fn memory_set_stores_updated_run_id() {
    // memory_set 带 updated_run_id → Applied 后 get_memory_block 读回正确。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    let r = memory_set(
        &conn,
        "s1",
        "goal",
        "目标文本",
        Some("标题"),
        Some("lead"),
        Some("run-abc"),
        0,
    )
    .unwrap();
    assert_eq!(r, MemorySetOutcome::Applied { revision: 1 });

    let b = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
    assert_eq!(b.updated_run_id.as_deref(), Some("run-abc"));
    assert_eq!(b.title.as_deref(), Some("标题"));
    assert_eq!(b.revision, 1);
}

#[test]
fn init_schema_idempotent_on_memory_blocks() {
    // 连调两次 init_schema 不报错。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    init_schema(&conn).unwrap(); // should not panic or error
}
