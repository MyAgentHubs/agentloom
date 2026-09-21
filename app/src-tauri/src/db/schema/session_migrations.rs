use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Add the nullable `sessions.group_id`; NULL represents an ungrouped session.
    let has_group_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "group_id")
    };
    if !has_group_id {
        let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
        if fk_was_on != 0 {
            conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
        }
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN group_id TEXT REFERENCES session_groups(id) ON DELETE SET NULL",
            [],
        )?;
        if fk_was_on != 0 {
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        }
    }
    // Add `sessions.git_state` with application-level states: clean, running, commit_failed, and diverged.
    // The default is clean because SQLite requires a default for a new NOT NULL column.
    // SQLite cannot add this CHECK constraint with ALTER TABLE, so application code enforces
    // the clean/running/commit_failed/diverged states. The initial run_commits table can use CHECK.
    let has_git_state = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "git_state")
    };
    if !has_git_state {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN git_state TEXT NOT NULL DEFAULT 'clean'",
            [],
        )?;
    }
    // Reserve nullable `sessions.parent_session_id` for the dispatch runtime spine.
    let has_parent_session_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "parent_session_id")
    };
    if !has_parent_session_id {
        conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", [])?;
    }
    // Add the nullable parent-to-live-child continuation pointer.
    let has_continued_to_session_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "continued_to_session_id")
    };
    if !has_continued_to_session_id {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN continued_to_session_id TEXT",
            [],
        )?;
    }
    // Add session lifecycle flags and cumulative token columns idempotently.
    // These columns have no REFERENCES clauses, so foreign keys can remain enabled.
    // SQLite permits constant defaults for the NOT NULL columns; archived_at remains nullable.
    for (col, decl) in [
        ("pinned", "INTEGER NOT NULL DEFAULT 0"),
        ("unread", "INTEGER NOT NULL DEFAULT 0"),
        ("archived", "INTEGER NOT NULL DEFAULT 0"),
        ("archived_at", "INTEGER"),
        ("deleted_at", "INTEGER"),
        ("total_input_tokens", "INTEGER NOT NULL DEFAULT 0"),
        ("total_output_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let has = {
            let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<_>>()?;
            cols.iter().any(|c| c == col)
        };
        if !has {
            conn.execute(&format!("ALTER TABLE sessions ADD COLUMN {col} {decl}"), [])?;
        }
    }
    // The nullable `sessions.workspace_scope` preserves legacy project-root access through the `'root'` value.
    // Legacy local-default sessions used the project root before per-session workspaces.
    // Backfill those preexisting rows to `root` only when the nullable column is first created.
    // Rows inserted after this migration remain NULL and use per-session directories.
    // Keeping the backfill inside the add-column branch is essential: a standalone migration
    // would misclassify every later local-default row whose workspace_scope is still NULL.
    let has_workspace_scope = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "workspace_scope")
    };
    // Add the column and backfill it in one transaction so a partial failure cannot permanently skip the legacy scope backfill.
    // ALTER TABLE is transactional; without this transaction, a crash after adding the column
    // but before completing the UPDATE would leave the column present and permanently skip
    // the legacy backfill on restart. Use the repository migration convention of an unchecked
    // transaction so adding the column and restoring access to legacy artifacts are atomic.
    if !has_workspace_scope {
        let tx = conn.unchecked_transaction()?;
        tx.execute("ALTER TABLE sessions ADD COLUMN workspace_scope TEXT", [])?;
        tx.execute(
            "UPDATE sessions SET workspace_scope = 'root' WHERE repo_id = 'local-default'",
            [],
        )?;
        tx.commit()?;
    }
    Ok(())
}
