use rusqlite::{Connection, OptionalExtension};

use super::{GeneratedRepoDocument, AGENT_COLS};

fn upsert_generated_repo_document(
    conn: &Connection,
    table: &str,
    document: &GeneratedRepoDocument,
) -> rusqlite::Result<()> {
    debug_assert!(matches!(table, "project_intro" | "daily_report"));
    conn.execute(
        &format!(
            "INSERT INTO {table} (repo_id, content, generated_at, head_sha)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repo_id) DO UPDATE SET
                content = excluded.content,
                generated_at = excluded.generated_at,
                head_sha = excluded.head_sha"
        ),
        rusqlite::params![
            document.repo_id,
            document.content,
            document.generated_at,
            document.head_sha
        ],
    )?;
    Ok(())
}

fn get_generated_repo_document(
    conn: &Connection,
    table: &str,
    repo_id: &str,
) -> rusqlite::Result<Option<GeneratedRepoDocument>> {
    debug_assert!(matches!(table, "project_intro" | "daily_report"));
    conn.query_row(
        &format!("SELECT repo_id, content, generated_at, head_sha FROM {table} WHERE repo_id = ?1"),
        [repo_id],
        |row| {
            Ok(GeneratedRepoDocument {
                repo_id: row.get(0)?,
                content: row.get(1)?,
                generated_at: row.get(2)?,
                head_sha: row.get(3)?,
            })
        },
    )
    .optional()
}

pub fn upsert_project_intro(
    conn: &Connection,
    document: &GeneratedRepoDocument,
) -> rusqlite::Result<()> {
    upsert_generated_repo_document(conn, "project_intro", document)
}

pub fn get_project_intro(
    conn: &Connection,
    repo_id: &str,
) -> rusqlite::Result<Option<GeneratedRepoDocument>> {
    get_generated_repo_document(conn, "project_intro", repo_id)
}

pub fn upsert_daily_report(
    conn: &Connection,
    document: &GeneratedRepoDocument,
) -> rusqlite::Result<()> {
    upsert_generated_repo_document(conn, "daily_report", document)
}

pub fn get_daily_report(
    conn: &Connection,
    repo_id: &str,
) -> rusqlite::Result<Option<GeneratedRepoDocument>> {
    get_generated_repo_document(conn, "daily_report", repo_id)
}

/// One-time v1-to-v2 migration: assign sessions with a null repo_id to local-default.
/// Precondition: the local-default repo exists (the setup hook seed has run). Returns the number of migrated rows.
pub fn migrate_null_repo_id_to_local_default(conn: &Connection) -> rusqlite::Result<usize> {
    let n = conn.execute(
        "UPDATE sessions SET repo_id = 'local-default' WHERE repo_id IS NULL",
        [],
    )?;
    Ok(n)
}

/// One-time backfill of dedup_key for existing user/assistant messages so historical messages enter the connection replay batch.
///
/// `messages.id` is the table-wide primary key, so `backfill:<id>` is unique across the table; in-repository producers also do not use
/// the `backfill:` prefix, so it cannot conflict with keys in the `(session_id, dedup_key)` partial unique index. Only
/// NULL rows are updated; repeated execution is a no-op.
pub fn migrate_backfill_dedup_keys(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE messages SET dedup_key = 'backfill:' || id \
         WHERE dedup_key IS NULL AND role IN ('user', 'assistant')",
        [],
    )
}

/// Rename local-default to “My Project” when it still has the original seeded name.
/// Match both the id and old name to avoid overwriting a user-modified name. Idempotent: a completed rename is a no-op.
pub fn migrate_local_default_name(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE repos SET name = '我的项目' WHERE id = 'local-default' AND name = 'Local 默认'",
        [],
    )
}

/// Delete the placeholder built-in deepseek agent when it has no key. Idempotent: an earlier deletion is a no-op.
/// Preserve deepseek variants with user-configured keys and user-created non-built-in agents.
pub fn migrate_remove_placeholder_deepseek(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM agents WHERE id = 'deepseek' AND is_builtin = 1 AND has_key = 0",
        [],
    )
}

/// Allow 'harness' in agents.access for the sidecar. SQLite cannot alter a CHECK constraint,
/// so old databases whose CHECK omits 'harness' rebuild the table; new databases already include it and are a no-op.
/// Returns true when a rebuild occurred. Idempotent.
pub fn migrate_agents_access_allow_harness(conn: &Connection) -> rusqlite::Result<bool> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='agents'",
        [],
        |r| r.get(0),
    )?;
    if sql.contains("'harness'") {
        return Ok(false);
    }
    let agent_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(agents)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    let has_col = |name: &str| agent_cols.iter().any(|col| col == name);
    let col_or = |name: &str, fallback: &str| {
        if has_col(name) {
            name.to_string()
        } else {
            fallback.to_string()
        }
    };
    let access_expr = if has_col("access") {
        "CASE WHEN access IN ('native', 'harness') THEN access ELSE 'borrow' END".to_string()
    } else {
        "'borrow'".to_string()
    };
    let auth_mode_expr = if has_col("auth_mode") {
        "CASE WHEN auth_mode IN ('bearer', 'x_api_key') THEN auth_mode ELSE NULL END".to_string()
    } else {
        "NULL".to_string()
    };
    let reasoning_expr = if has_col("reasoning_default") {
        "CASE WHEN reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max') THEN reasoning_default ELSE 'auto' END".to_string()
    } else {
        "'auto'".to_string()
    };
    let select_cols = vec![
        col_or("id", "''"),
        col_or("name", "id"),
        access_expr,
        col_or("provider", "'deepseek'"),
        col_or("primary_model", "NULL"),
        col_or("endpoint", "NULL"),
        auth_mode_expr,
        col_or("model_opus", "NULL"),
        col_or("model_sonnet", "NULL"),
        col_or("model_haiku", "NULL"),
        col_or("model_subagent", "NULL"),
        reasoning_expr,
        col_or("max_output_tokens", "NULL"),
        col_or("api_timeout_ms", "NULL"),
        col_or("compat_disable_betas", "0"),
        col_or("compat_disable_nonessential", "0"),
        col_or("compat_disable_thinking", "0"),
        col_or("compat_proxy", "NULL"),
        col_or("custom_headers", "NULL"),
        col_or("extra_body", "NULL"),
        col_or("cap_reasoning", "NULL"),
        col_or("cap_computer_use", "NULL"),
        col_or("cap_lead", "NULL"),
        col_or("has_key", "0"),
        col_or("is_builtin", "0"),
        col_or("enabled", "1"),
        col_or("sort_order", "0"),
        col_or("created_at", "0"),
        col_or("updated_at", "0"),
    ]
    .join(", ");
    let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if fk_was_on == 1 {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    }
    let rebuild_sql = format!(
        "DROP TABLE IF EXISTS agents_new;
        CREATE TABLE agents_new (
            id TEXT NOT NULL PRIMARY KEY,
            name TEXT NOT NULL,
            access TEXT NOT NULL
                CHECK (access IN ('native', 'borrow', 'harness')),
            provider TEXT NOT NULL,
            primary_model TEXT,
            endpoint TEXT,
            auth_mode TEXT
                CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
            model_opus TEXT,
            model_sonnet TEXT,
            model_haiku TEXT,
            model_subagent TEXT,
            reasoning_default TEXT NOT NULL DEFAULT 'auto'
                CHECK (reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max')),
            max_output_tokens INTEGER,
            api_timeout_ms INTEGER,
            compat_disable_betas INTEGER NOT NULL DEFAULT 0,
            compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
            compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
            compat_proxy TEXT,
            custom_headers TEXT,
            extra_body TEXT,
            cap_reasoning TEXT,
            cap_computer_use TEXT,
            cap_lead TEXT,
            has_key INTEGER NOT NULL DEFAULT 0,
            is_builtin INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        INSERT INTO agents_new ({AGENT_COLS}) SELECT {select_cols} FROM agents;
        DROP TABLE agents;
        ALTER TABLE agents_new RENAME TO agents;"
    );
    conn.execute_batch(&rebuild_sql)?;
    if fk_was_on == 1 {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    }
    Ok(true)
}

/// One-time backfill: populate sessions.namespace_id from repo.namespace_id.
/// Two UPDATEs: first fix mismatched rows via a join, then assign remaining NULL values to Local. Returns the total number of updated rows.
pub fn backfill_session_namespace_id(conn: &Connection) -> rusqlite::Result<usize> {
    let n1 = conn.execute(
        "UPDATE sessions
         SET namespace_id = (SELECT namespace_id FROM repos WHERE repos.id = sessions.repo_id)
         WHERE sessions.repo_id IS NOT NULL
           AND EXISTS (
               SELECT 1 FROM repos
               WHERE repos.id = sessions.repo_id
                 AND repos.namespace_id IS NOT NULL
                 AND repos.namespace_id != sessions.namespace_id
           )",
        [],
    )?;
    let n2 = conn.execute(
        "UPDATE sessions SET namespace_id = 'local' WHERE namespace_id IS NULL",
        [],
    )?;
    Ok(n1 + n2)
}
