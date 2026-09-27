use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Add message ownership columns idempotently for databases created before the agent pool.
    let message_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    if !message_cols.iter().any(|c| c == "agent_id") {
        conn.execute("ALTER TABLE messages ADD COLUMN agent_id TEXT", [])?;
    }
    if !message_cols.iter().any(|c| c == "agent_name_snapshot") {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN agent_name_snapshot TEXT",
            [],
        )?;
    }
    Ok(())
}
