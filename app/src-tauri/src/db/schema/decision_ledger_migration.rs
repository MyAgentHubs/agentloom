use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Rebuild legacy decision_ledger tables whose run_id column is NOT NULL.
    // The table was not wired to production when this migration shipped, so rebuilding was safe.
    let decision_run_id_notnull = {
        let mut stmt = conn.prepare("PRAGMA table_info(decision_ledger)")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)?)))?;
        let mut notnull = false;
        for row in rows {
            let (name, nn) = row?;
            if name == "run_id" && nn == 1 {
                notnull = true;
            }
        }
        notnull
    };
    if decision_run_id_notnull {
        conn.execute_batch(
            // Remove an orphaned replacement left by a crash between CREATE and DROP.
            "DROP TABLE IF EXISTS decision_ledger_new;
            CREATE TABLE decision_ledger_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                run_id TEXT,
                source_assignment_id TEXT,
                text TEXT NOT NULL,
                source_refs_json TEXT NOT NULL DEFAULT '[]',
                supersedes_json TEXT NOT NULL DEFAULT '[]',
                source_kind TEXT,
                confidence TEXT,
                created_at INTEGER NOT NULL
            );
            INSERT INTO decision_ledger_new
                (id, session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at)
                SELECT id, session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at
                FROM decision_ledger;
            DROP TABLE decision_ledger;
            ALTER TABLE decision_ledger_new RENAME TO decision_ledger;
            CREATE INDEX IF NOT EXISTS idx_decision_ledger_session ON decision_ledger(session_id, id);",
        )?;
    }
    Ok(())
}
