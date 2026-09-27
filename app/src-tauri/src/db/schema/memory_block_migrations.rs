use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    let memory_block_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(memory_blocks)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    if !memory_block_cols.iter().any(|c| c == "revision") {
        conn.execute(
            "ALTER TABLE memory_blocks ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !memory_block_cols.iter().any(|c| c == "updated_run_id") {
        conn.execute(
            "ALTER TABLE memory_blocks ADD COLUMN updated_run_id TEXT",
            [],
        )?;
    }
    Ok(())
}
