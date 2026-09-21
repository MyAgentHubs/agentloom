use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Keep session runtime state in a separate mirror table rather than adding columns to `sessions`; it starts empty.
    // A separate table avoids expanding sessions and starts empty; solo and team slot handling
    // plus startup reconciliation share the runtime helpers below.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS session_runtime (
            session_id TEXT PRIMARY KEY,
            status TEXT NOT NULL CHECK (status IN ('running', 'idle')),
            run_id TEXT,
            updated_at INTEGER NOT NULL
        )",
        [],
    )?;
    Ok(())
}
