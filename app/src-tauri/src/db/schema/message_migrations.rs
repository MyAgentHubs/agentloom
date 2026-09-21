use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Add the legacy `messages.dedup_key` column before creating its partial unique index.
    // New databases already have the column; old databases reach the index only after the ALTER.
    let has_dedup_key = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "dedup_key")
    };
    if !has_dedup_key {
        conn.execute("ALTER TABLE messages ADD COLUMN dedup_key TEXT", [])?;
    }
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_dedup \
         ON messages(session_id, dedup_key) WHERE dedup_key IS NOT NULL",
        [],
    )?;
    // Idempotently add `messages.revision` to existing databases so persisted messages have a content version.
    // New databases define messages.revision with DEFAULT 1 above; add it for legacy databases.
    // The default also backfills existing rows, so no separate UPDATE is needed.
    let has_revision = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "revision")
    };
    if !has_revision {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN revision INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    Ok(())
}
