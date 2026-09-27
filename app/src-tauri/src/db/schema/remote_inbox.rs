use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Queue remote input received while a session is busy. `command_id` is the idempotency key
    // for relay retries and reconnect delivery, while delivered and failed rows remain the
    // deduplication ledger. There is intentionally no sessions foreign key because this table
    // mirrors transient runtime state and may briefly outlive a deleted session.
    // Any future garbage collection must retain terminal command IDs for a dedicated window.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS remote_inbox (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            command_id TEXT NOT NULL UNIQUE,
            kind TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            delivered_at INTEGER,
            attempts INTEGER NOT NULL DEFAULT 0,
            failed_at INTEGER,
            last_error TEXT
        )",
        [],
    )?;
    // Early remote_inbox schemas lacked these columns; add them before queries can fail open
    // through callers that intentionally discard lookup errors.
    let remote_inbox_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(remote_inbox)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        columns
    };
    for (column, declaration) in [
        ("attempts", "INTEGER NOT NULL DEFAULT 0"),
        ("failed_at", "INTEGER"),
        ("last_error", "TEXT"),
    ] {
        if !remote_inbox_cols.iter().any(|existing| existing == column) {
            conn.execute(
                &format!("ALTER TABLE remote_inbox ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
    }
    conn.execute("DROP INDEX IF EXISTS idx_remote_inbox_pending", [])?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_remote_inbox_pending \
         ON remote_inbox(session_id, id) \
         WHERE delivered_at IS NULL AND failed_at IS NULL",
        [],
    )?;
    Ok(())
}
