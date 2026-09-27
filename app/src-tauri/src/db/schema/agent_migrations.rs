use rusqlite::{Connection, OptionalExtension};

use super::super::{
    migrate_agents_access_allow_harness, recover_agents_old_reasoning_check,
    reset_session_agent_configs_if_bad_fk,
};

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    let agent_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(agents)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    if !agent_cols.iter().any(|c| c == "cap_lead") {
        conn.execute("ALTER TABLE agents ADD COLUMN cap_lead TEXT", [])?;
    }
    {
        let agents_sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'agents'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if agents_sql.as_deref().is_some_and(|sql| {
            sql.contains("reasoning_default IN ('auto', 'low', 'medium', 'high')")
        }) {
            let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
            if fk_was_on != 0 {
                conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
            }
            conn.execute_batch(
                r#"
                ALTER TABLE agents RENAME TO agents_old_reasoning_check;
                CREATE TABLE agents (
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
                INSERT INTO agents (
                    id, name, access, provider, primary_model, endpoint, auth_mode,
                    model_opus, model_sonnet, model_haiku, model_subagent,
                    reasoning_default, max_output_tokens, api_timeout_ms,
                    compat_disable_betas, compat_disable_nonessential,
                    compat_disable_thinking, compat_proxy, custom_headers, extra_body,
                    cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin,
                    enabled, sort_order, created_at, updated_at
                )
                SELECT
                    id, name,
                    CASE WHEN access IN ('native', 'harness') THEN access ELSE 'borrow' END,
                    provider, primary_model, endpoint,
                    CASE WHEN auth_mode IN ('bearer', 'x_api_key') THEN auth_mode ELSE NULL END,
                    model_opus, model_sonnet, model_haiku, model_subagent,
                    reasoning_default, max_output_tokens, api_timeout_ms,
                    compat_disable_betas, compat_disable_nonessential,
                    compat_disable_thinking, compat_proxy, custom_headers, extra_body,
                    cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin,
                    enabled, sort_order, created_at, updated_at
                FROM agents_old_reasoning_check;
                DROP TABLE agents_old_reasoning_check;
                "#,
            )?;
            if fk_was_on != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
        }
    }
    recover_agents_old_reasoning_check(conn)?;
    migrate_agents_access_allow_harness(conn)?;
    reset_session_agent_configs_if_bad_fk(conn)?;
    Ok(())
}
