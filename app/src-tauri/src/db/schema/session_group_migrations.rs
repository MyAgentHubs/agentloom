use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Idempotently add `session_groups.repo_id` to existing databases to scope groups to a repository.
    {
        let has_sg_repo_id = {
            let mut stmt = conn.prepare("PRAGMA table_info(session_groups)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<_>>()?;
            cols.iter().any(|c| c == "repo_id")
        };
        if !has_sg_repo_id {
            // The new column must start nullable because SQLite cannot add NOT NULL without a default.
            conn.execute(
                "ALTER TABLE session_groups ADD COLUMN repo_id TEXT REFERENCES repos(id)",
                [],
            )?;
            // First map Local namespace groups to local-default.
            conn.execute(
                "UPDATE session_groups SET repo_id = 'local-default' WHERE namespace_id = 'local' AND repo_id IS NULL",
                [],
            )?;
            // Then map a namespace that has exactly one repository to that repository.
            conn.execute(
                "UPDATE session_groups SET repo_id = (
                    SELECT r.id FROM repos r
                    WHERE r.namespace_id = session_groups.namespace_id
                    GROUP BY r.namespace_id HAVING COUNT(*) = 1
                    LIMIT 1
                ) WHERE repo_id IS NULL",
                [],
            )?;
            // Finally detach sessions from unmappable groups before deleting those groups.
            conn.execute(
                "UPDATE sessions SET group_id = NULL WHERE group_id IN (
                    SELECT id FROM session_groups WHERE repo_id IS NULL
                )",
                [],
            )?;
            conn.execute("DELETE FROM session_groups WHERE repo_id IS NULL", [])?;
        }
    }
    Ok(())
}
