use rusqlite::Connection;

use super::super::GENERATED_REPORTS_SCHEMA_VERSION;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Generated documents never touch the repository worktree. This versioned migration upgrades
    // old app databases once; CREATE IF NOT EXISTS also makes an interrupted upgrade retry-safe.
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version < GENERATED_REPORTS_SCHEMA_VERSION {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS project_intro (
                repo_id TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,
                content TEXT NOT NULL,
                generated_at INTEGER NOT NULL,
                head_sha TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS daily_report (
                repo_id TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,
                content TEXT NOT NULL,
                generated_at INTEGER NOT NULL,
                head_sha TEXT NOT NULL
            );
            PRAGMA user_version = 1;",
        )?;
    }
    Ok(())
}
