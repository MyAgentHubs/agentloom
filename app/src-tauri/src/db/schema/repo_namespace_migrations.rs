use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    let has_repo_ns_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(repos)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "namespace_id")
    };
    let has_session_ns_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "namespace_id")
    };
    // SQLite rejects adding a REFERENCES column with a non-NULL default while foreign keys are enabled.
    // Disable foreign keys only around these ALTER statements; startup seeding supplies the Local namespace next.
    let needs_fk_alter = !has_repo_ns_id || !has_session_ns_id;
    let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if needs_fk_alter {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    }
    if !has_repo_ns_id {
        conn.execute(
            "ALTER TABLE repos ADD COLUMN namespace_id TEXT NOT NULL DEFAULT 'local' REFERENCES namespaces(id) ON DELETE CASCADE",
            [],
        )?;
    }
    // Add the denormalized `sessions.namespace_id` column to databases from before namespace support.
    // ON DELETE SET NULL preserves session history when a namespace is removed.
    // DEFAULT local assigns legacy rows to the Local namespace during migration.
    // SQLite requires a constant default when ALTER TABLE adds this column.
    if !has_session_ns_id {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN namespace_id TEXT DEFAULT 'local' REFERENCES namespaces(id) ON DELETE SET NULL",
            [],
        )?;
    }
    if needs_fk_alter && fk_was_on != 0 {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    }
    Ok(())
}
