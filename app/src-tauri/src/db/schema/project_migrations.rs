use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Add `sessions.repo_id` idempotently for databases created before repositories were attached to sessions.
    // Probe the schema first so repeated initialization never attempts the same ALTER TABLE twice.
    let has_repo_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "repo_id")
    };
    if !has_repo_id {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL",
            [],
        )?;
    }
    // Add `goal_contracts.assignments_json` to existing databases so assignment drafts are persisted with the contract.
    {
        let mut stmt = conn.prepare("PRAGMA table_info(goal_contracts)")?;
        let has_col = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == "assignments_json");
        drop(stmt);
        if !has_col {
            conn.execute(
                "ALTER TABLE goal_contracts ADD COLUMN assignments_json TEXT NOT NULL DEFAULT '[]'",
                [],
            )?;
        }
    }
    // Add `goal_contracts.goal_title` to existing databases for the lead-generated summary displayed in the top bar.
    {
        let mut stmt = conn.prepare("PRAGMA table_info(goal_contracts)")?;
        let has_col = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == "goal_title");
        drop(stmt);
        if !has_col {
            conn.execute("ALTER TABLE goal_contracts ADD COLUMN goal_title TEXT", [])?;
        }
    }
    // Rename the repository marker from color to icon, preserving non-hex values and clearing legacy colors.
    let repo_columns = {
        let mut stmt = conn.prepare("PRAGMA table_info(repos)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        cols
    };
    if !repo_columns.iter().any(|c| c == "icon") {
        if repo_columns.iter().any(|c| c == "color") {
            conn.execute("ALTER TABLE repos RENAME COLUMN color TO icon", [])?;
            conn.execute("UPDATE repos SET icon = NULL WHERE icon LIKE '#%'", [])?;
        } else {
            conn.execute("ALTER TABLE repos ADD COLUMN icon TEXT", [])?;
        }
    }
    Ok(())
}
