use rusqlite::{Connection, OptionalExtension};

use super::{now_ms, AgentProfile, SessionAgentConfig};

#[allow(dead_code)] // Reserved for future Agent pool use through the public API.
pub(super) const AGENT_COLS: &str =
    "id, name, access, provider, primary_model, endpoint, auth_mode, model_opus, model_sonnet, \
     model_haiku, model_subagent, reasoning_default, max_output_tokens, api_timeout_ms, \
     compat_disable_betas, compat_disable_nonessential, compat_disable_thinking, compat_proxy, \
     custom_headers, extra_body, cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin, \
     enabled, sort_order, created_at, updated_at";

#[allow(dead_code)] // Reserved for future Agent pool use through the public API.
fn map_agent_row(r: &rusqlite::Row) -> rusqlite::Result<AgentProfile> {
    Ok(AgentProfile {
        id: r.get(0)?,
        name: r.get(1)?,
        access: r.get(2)?,
        provider: r.get(3)?,
        primary_model: r.get(4)?,
        endpoint: r.get(5)?,
        auth_mode: r.get(6)?,
        model_opus: r.get(7)?,
        model_sonnet: r.get(8)?,
        model_haiku: r.get(9)?,
        model_subagent: r.get(10)?,
        reasoning_default: r.get(11)?,
        max_output_tokens: r.get(12)?,
        api_timeout_ms: r.get(13)?,
        compat_disable_betas: r.get::<_, i64>(14)? != 0,
        compat_disable_nonessential: r.get::<_, i64>(15)? != 0,
        compat_disable_thinking: r.get::<_, i64>(16)? != 0,
        compat_proxy: r.get(17)?,
        custom_headers: r.get(18)?,
        extra_body: r.get(19)?,
        cap_reasoning: r.get(20)?,
        cap_computer_use: r.get(21)?,
        cap_lead: r.get(22)?,
        has_key: r.get::<_, i64>(23)? != 0,
        is_builtin: r.get::<_, i64>(24)? != 0,
        enabled: r.get::<_, i64>(25)? != 0,
        sort_order: r.get(26)?,
        created_at: r.get(27)?,
        updated_at: r.get(28)?,
    })
}

fn table_exists(conn: &Connection, table_name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table_name],
        |r| r.get::<_, i64>(0),
    )
    .map(|count| count > 0)
}

fn table_has_column(conn: &Connection, table_name: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table_name})"))?;
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(cols.iter().any(|name| name == column))
}

fn session_agent_configs_lead_fk_target(
    conn: &Connection,
) -> rusqlite::Result<Option<(String, String)>> {
    if !table_exists(conn, "session_agent_configs")? {
        return Ok(None);
    }

    let mut stmt = conn.prepare("PRAGMA foreign_key_list(session_agent_configs)")?;
    let fks: Vec<(String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(fks
        .iter()
        .find(|(_, from, _)| from == "lead_agent_id")
        .map(|(table, _, to)| (table.clone(), to.clone())))
}

pub(super) fn reset_session_agent_configs_if_bad_fk(conn: &Connection) -> rusqlite::Result<()> {
    let Some((lead_fk_table, lead_fk_column)) = session_agent_configs_lead_fk_target(conn)? else {
        return Ok(());
    };
    if lead_fk_table == "agents" && lead_fk_column == "id" {
        return Ok(());
    }
    if !(lead_fk_table == "agents_old_reasoning_check" && lead_fk_column == "id") {
        return Ok(());
    }

    let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if fk_was_on != 0 {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    }
    let reset_result = conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS session_agent_configs;
        CREATE TABLE session_agent_configs (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            lead_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
            member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
        );
        "#,
    );
    if fk_was_on != 0 {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    }
    reset_result
}

pub(super) fn recover_agents_old_reasoning_check(conn: &Connection) -> rusqlite::Result<()> {
    if !table_exists(conn, "agents_old_reasoning_check")? {
        return Ok(());
    }
    if !table_has_column(conn, "agents_old_reasoning_check", "cap_lead")? {
        conn.execute(
            "ALTER TABLE agents_old_reasoning_check ADD COLUMN cap_lead TEXT",
            [],
        )?;
    }
    conn.execute_batch(
        r#"
        INSERT OR IGNORE INTO agents (
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
    )
}

#[allow(dead_code)] // Reserved for future Agent pool use through the public API.
pub fn list_agents(conn: &Connection) -> rusqlite::Result<Vec<AgentProfile>> {
    let sql = format!("SELECT {AGENT_COLS} FROM agents ORDER BY sort_order ASC, id ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_agent_row)?;
    rows.collect()
}

#[allow(dead_code)] // Reserved for future Agent pool use through the public API.
pub fn get_agent(conn: &Connection, id: &str) -> rusqlite::Result<Option<AgentProfile>> {
    let sql = format!("SELECT {AGENT_COLS} FROM agents WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([id], map_agent_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

#[allow(dead_code)] // Reserved for future Agent pool use through the public API.
pub fn upsert_agent(conn: &Connection, a: &AgentProfile) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agents (
            id, name, access, provider, primary_model, endpoint, auth_mode, model_opus,
            model_sonnet, model_haiku, model_subagent, reasoning_default, max_output_tokens,
            api_timeout_ms, compat_disable_betas, compat_disable_nonessential,
            compat_disable_thinking, compat_proxy, custom_headers, extra_body, cap_reasoning,
            cap_computer_use, cap_lead, has_key, is_builtin, enabled, sort_order, created_at,
            updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
            ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29
        )
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            access = excluded.access,
            provider = excluded.provider,
            primary_model = excluded.primary_model,
            endpoint = excluded.endpoint,
            auth_mode = excluded.auth_mode,
            model_opus = excluded.model_opus,
            model_sonnet = excluded.model_sonnet,
            model_haiku = excluded.model_haiku,
            model_subagent = excluded.model_subagent,
            reasoning_default = excluded.reasoning_default,
            max_output_tokens = excluded.max_output_tokens,
            api_timeout_ms = excluded.api_timeout_ms,
            compat_disable_betas = excluded.compat_disable_betas,
            compat_disable_nonessential = excluded.compat_disable_nonessential,
            compat_disable_thinking = excluded.compat_disable_thinking,
            compat_proxy = excluded.compat_proxy,
            custom_headers = excluded.custom_headers,
            extra_body = excluded.extra_body,
            cap_reasoning = excluded.cap_reasoning,
            cap_computer_use = excluded.cap_computer_use,
            cap_lead = excluded.cap_lead,
            has_key = excluded.has_key,
            is_builtin = excluded.is_builtin,
            enabled = excluded.enabled,
            sort_order = excluded.sort_order,
            updated_at = excluded.updated_at",
        rusqlite::params![
            a.id.as_str(),
            a.name.as_str(),
            a.access.as_str(),
            a.provider.as_str(),
            a.primary_model.as_deref(),
            a.endpoint.as_deref(),
            a.auth_mode.as_deref(),
            a.model_opus.as_deref(),
            a.model_sonnet.as_deref(),
            a.model_haiku.as_deref(),
            a.model_subagent.as_deref(),
            a.reasoning_default.as_str(),
            a.max_output_tokens,
            a.api_timeout_ms,
            a.compat_disable_betas as i64,
            a.compat_disable_nonessential as i64,
            a.compat_disable_thinking as i64,
            a.compat_proxy.as_deref(),
            a.custom_headers.as_deref(),
            a.extra_body.as_deref(),
            a.cap_reasoning.as_deref(),
            a.cap_computer_use.as_deref(),
            a.cap_lead.as_deref(),
            a.has_key as i64,
            a.is_builtin as i64,
            a.enabled as i64,
            a.sort_order,
            a.created_at,
            a.updated_at,
        ],
    )?;
    Ok(())
}

pub fn seed_builtin_agents(conn: &Connection) -> rusqlite::Result<()> {
    let now = now_ms();

    let profiles = [
        AgentProfile {
            id: "claude".into(),
            name: "Claude".into(),
            access: "native".into(),
            provider: "claude".into(),
            primary_model: None,
            endpoint: None,
            auth_mode: None,
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
            model_subagent: None,
            reasoning_default: "auto".into(),
            max_output_tokens: None,
            api_timeout_ms: None,
            compat_disable_betas: false,
            compat_disable_nonessential: false,
            compat_disable_thinking: false,
            compat_proxy: None,
            custom_headers: None,
            extra_body: None,
            cap_reasoning: Some("low,medium,high,xhigh,max".into()),
            cap_computer_use: None,
            cap_lead: Some("native_cli".into()),
            has_key: false,
            is_builtin: true,
            enabled: true,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        },
        AgentProfile {
            id: "codex".into(),
            name: "Codex".into(),
            access: "native".into(),
            provider: "codex".into(),
            primary_model: None,
            endpoint: None,
            auth_mode: None,
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
            model_subagent: None,
            reasoning_default: "auto".into(),
            max_output_tokens: None,
            api_timeout_ms: None,
            compat_disable_betas: false,
            compat_disable_nonessential: false,
            compat_disable_thinking: false,
            compat_proxy: None,
            custom_headers: None,
            extra_body: None,
            cap_reasoning: Some("minimal,low,medium,high,xhigh".into()),
            cap_computer_use: None,
            cap_lead: None,
            has_key: false,
            is_builtin: true,
            enabled: true,
            sort_order: 1,
            created_at: now,
            updated_at: now,
        },
    ];

    for profile in profiles {
        if get_agent(conn, &profile.id)?.is_none() {
            upsert_agent(conn, &profile)?;
        } else {
            conn.execute(
                "UPDATE agents SET cap_lead = ?2, cap_reasoning = ?3 WHERE id = ?1 AND is_builtin = 1",
                rusqlite::params![
                    profile.id.as_str(),
                    profile.cap_lead.as_deref(),
                    profile.cap_reasoning.as_deref()
                ],
            )?;
        }
    }

    Ok(())
}

fn db_constraint(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(message.into()),
    )
}

fn normalize_agent_id(id: String) -> Option<String> {
    let id = id.trim().to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn normalize_member_agent_ids(
    member_agent_ids: Vec<String>,
    lead_agent_id: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    for member_id in member_agent_ids {
        let Some(member_id) = normalize_agent_id(member_id) else {
            continue;
        };
        if Some(member_id.as_str()) == lead_agent_id {
            continue;
        }
        if !out.iter().any(|seen| seen == &member_id) {
            out.push(member_id);
        }
    }
    out
}

fn require_session_exists(conn: &Connection, session_id: &str) -> rusqlite::Result<()> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sessions WHERE id = ?1 LIMIT 1",
            [session_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(db_constraint(format!(
            "session {session_id} does not exist"
        )))
    }
}

fn require_config_agent(conn: &Connection, id: &str) -> rusqlite::Result<AgentProfile> {
    match get_agent(conn, id)? {
        Some(agent) => Ok(agent),
        None => Err(db_constraint(format!("agent {id} does not exist"))),
    }
}

fn require_lead_agent(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let agent = require_config_agent(conn, id)?;
    if !agent.enabled {
        return Err(db_constraint(format!(
            "agent {id} cannot act as Lead: disabled"
        )));
    }
    Ok(())
}

fn require_member_agent(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let agent = require_config_agent(conn, id)?;
    if agent.enabled {
        Ok(())
    } else {
        Err(db_constraint(format!("member agent {id} is disabled")))
    }
}

#[allow(dead_code)] // Reserved for future command use; currently covered by database-layer tests.
pub fn set_agent_enabled(conn: &Connection, id: &str, enabled: bool) -> rusqlite::Result<()> {
    let updated = conn.execute(
        "UPDATE agents SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, enabled as i64, now_ms()],
    )?;
    if updated == 0 {
        Err(db_constraint(format!("agent {id} does not exist")))
    } else {
        Ok(())
    }
}

#[allow(dead_code)] // The command will be exposed later; establish the database helper first.
pub fn get_session_agent_config(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<SessionAgentConfig> {
    require_session_exists(conn, session_id)?;
    let row = conn
        .query_row(
            "SELECT lead_agent_id, member_agent_ids FROM session_agent_configs WHERE session_id = ?1",
            [session_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;

    let Some((lead_agent_id, member_agent_ids_json)) = row else {
        return Ok(SessionAgentConfig {
            session_id: session_id.to_string(),
            lead_agent_id: None,
            member_agent_ids: Vec::new(),
        });
    };

    let member_agent_ids =
        serde_json::from_str::<Vec<String>>(&member_agent_ids_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;

    Ok(SessionAgentConfig {
        session_id: session_id.to_string(),
        lead_agent_id: lead_agent_id.and_then(normalize_agent_id),
        member_agent_ids,
    })
}

#[allow(dead_code)] // The command will be exposed later; establish the database helper first.
pub fn set_session_agent_config(
    conn: &Connection,
    session_id: &str,
    lead_agent_id: Option<String>,
    member_agent_ids: Vec<String>,
) -> rusqlite::Result<SessionAgentConfig> {
    require_session_exists(conn, session_id)?;
    let lead_agent_id = lead_agent_id.and_then(normalize_agent_id);
    if let Some(lead_agent_id) = lead_agent_id.as_deref() {
        require_lead_agent(conn, lead_agent_id)?;
    }

    let member_agent_ids = normalize_member_agent_ids(member_agent_ids, lead_agent_id.as_deref());
    for member_agent_id in &member_agent_ids {
        require_member_agent(conn, member_agent_id)?;
    }

    let member_agent_ids_json =
        serde_json::to_string(&member_agent_ids).expect("member agent ids 序列化失败");
    conn.execute(
        "INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET
            lead_agent_id = excluded.lead_agent_id,
            member_agent_ids = excluded.member_agent_ids",
        rusqlite::params![session_id, lead_agent_id.as_deref(), member_agent_ids_json],
    )?;

    Ok(SessionAgentConfig {
        session_id: session_id.to_string(),
        lead_agent_id,
        member_agent_ids,
    })
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMode {
    Solo,
    Team {
        lead_agent_id: String,
        member_ids: Vec<String>,
    },
}

#[allow(dead_code)]
pub fn session_mode(conn: &Connection, session_id: &str) -> Result<SessionMode, String> {
    let config = get_session_agent_config(conn, session_id).map_err(|e| e.to_string())?;
    Ok(match config.lead_agent_id {
        Some(lead) => SessionMode::Team {
            lead_agent_id: lead,
            member_ids: config.member_agent_ids,
        },
        None => SessionMode::Solo,
    })
}

#[allow(dead_code)]
pub fn copy_session_agent_config(
    conn: &Connection,
    parent_session_id: &str,
    child_session_id: &str,
) -> Result<(), String> {
    let parent_config =
        get_session_agent_config(conn, parent_session_id).map_err(|e| e.to_string())?;
    let member_json = serde_json::to_string(&parent_config.member_agent_ids)
        .expect("member_agent_ids serialization failed");
    conn.execute(
        "INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET
             lead_agent_id = excluded.lead_agent_id,
             member_agent_ids = excluded.member_agent_ids",
        rusqlite::params![
            child_session_id,
            parent_config.lead_agent_id.as_deref(),
            member_json
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[allow(dead_code)] // Reserved for future Agent pool use through the public API.
pub fn delete_agent(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let access = match conn.query_row("SELECT access FROM agents WHERE id = ?1", [id], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
        Err(e) => return Err(e),
    };
    if access == "native" {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("native agent cannot be deleted".into()),
        ));
    }
    conn.execute("DELETE FROM agents WHERE id = ?1", [id])?;
    Ok(())
}
