#![cfg(test)]

use super::*;

#[test]
fn agents_schema_pragma_columns() {
    let c = mem();
    let mut stmt = c.prepare("PRAGMA table_info(agents)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in [
        "id",
        "name",
        "access",
        "provider",
        "primary_model",
        "endpoint",
        "auth_mode",
        "model_opus",
        "model_sonnet",
        "model_haiku",
        "model_subagent",
        "reasoning_default",
        "max_output_tokens",
        "api_timeout_ms",
        "compat_disable_betas",
        "compat_disable_nonessential",
        "compat_disable_thinking",
        "compat_proxy",
        "custom_headers",
        "extra_body",
        "cap_reasoning",
        "cap_computer_use",
        "cap_lead",
        "has_key",
        "is_builtin",
        "enabled",
        "sort_order",
        "created_at",
        "updated_at",
    ] {
        assert!(
            cols.contains(&name.into()),
            "agents 应含列 {name}：实际 {cols:?}"
        );
    }
}

#[test]
fn fresh_schema_allows_harness_access() {
    let c = mem();
    let r = c.execute(
        "INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
             VALUES ('h','H','harness','deepseek','auto',0,0)",
        [],
    );
    assert!(
        r.is_ok(),
        "fresh schema should allow access='harness': {r:?}"
    );
}

#[test]
fn migrate_rebuilds_old_check_and_preserves_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            r#"
            CREATE TABLE agents (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL CHECK (access IN ('native', 'borrow')),
                provider TEXT NOT NULL,
                primary_model TEXT,
                endpoint TEXT,
                auth_mode TEXT CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
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
            INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
                VALUES ('b','Borrow','borrow','deepseek','auto',0,0);
            "#,
        )
        .unwrap();

    let changed = migrate_agents_access_allow_harness(&conn).unwrap();

    assert!(changed, "old CHECK should be rebuilt");
    assert_eq!(get_agent(&conn, "b").unwrap().unwrap().access, "borrow");
    conn.execute(
        "INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
             VALUES ('h','H','harness','deepseek','auto',0,0)",
        [],
    )
    .unwrap();
}

#[test]
fn migrate_rebuilds_after_leftover_agents_new_table() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            r#"
            CREATE TABLE agents (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL CHECK (access IN ('native', 'borrow')),
                provider TEXT NOT NULL,
                primary_model TEXT,
                endpoint TEXT,
                auth_mode TEXT CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
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
            CREATE TABLE agents_new (id TEXT);
            "#,
        )
        .unwrap();

    let changed = migrate_agents_access_allow_harness(&conn).unwrap();

    assert!(changed, "leftover agents_new should not block rebuild");
    conn.execute(
        "INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
             VALUES ('h','H','harness','deepseek','auto',0,0)",
        [],
    )
    .unwrap();
}

#[test]
fn session_agent_configs_schema_pragma_columns() {
    let c = mem();
    let mut stmt = c
        .prepare("PRAGMA table_info(session_agent_configs)")
        .unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in ["session_id", "lead_agent_id", "member_agent_ids"] {
        assert!(
            cols.contains(&name.into()),
            "session_agent_configs 应含列 {name}：实际 {cols:?}"
        );
    }
}

#[test]
fn init_schema_resets_session_agent_configs_with_dead_legacy_agent_fk() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        r#"
            PRAGMA foreign_keys = OFF;
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE agents_old_reasoning_check (
                id TEXT NOT NULL PRIMARY KEY
            );
            CREATE TABLE session_agent_configs (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                lead_agent_id TEXT REFERENCES agents_old_reasoning_check(id) ON DELETE SET NULL,
                member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
            );
            INSERT INTO sessions (id, title, created_at) VALUES ('session-a', 'Session A', 0);
            INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
                VALUES ('session-a', NULL, '["codex"]');
            DROP TABLE agents_old_reasoning_check;
            PRAGMA foreign_keys = ON;
            "#,
    )
    .unwrap();

    init_schema(&c).unwrap();
    seed_builtin_agents(&c).unwrap();

    let fks: Vec<(String, String, String)> = c
        .prepare("PRAGMA foreign_key_list(session_agent_configs)")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        fks.iter().any(|(table, from, to)| {
            table == "agents" && from == "lead_agent_id" && to == "id"
        }),
        "session_agent_configs.lead_agent_id 应重新指向 agents(id)：{fks:?}"
    );
    assert_eq!(
        get_session_agent_config(&c, "session-a").unwrap(),
        SessionAgentConfig {
            session_id: "session-a".into(),
            lead_agent_id: None,
            member_agent_ids: Vec::new(),
        }
    );

    let saved = set_session_agent_config(
        &c,
        "session-a",
        Some("claude".to_string()),
        vec!["codex".to_string()],
    )
    .unwrap();
    assert_eq!(saved.lead_agent_id.as_deref(), Some("claude"));
    assert_eq!(saved.member_agent_ids, vec!["codex"]);
}

#[test]
fn init_schema_resets_session_agent_configs_when_agents_table_is_rebuilt() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            INSERT INTO sessions (id, title, created_at) VALUES ('session-a', 'Session A', 0);
            "#,
    )
    .unwrap();
    create_legacy_agents_reasoning_table(&c, "agents");
    c.execute_batch(
        r#"
            CREATE TABLE session_agent_configs (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                lead_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
                member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
            );
            INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
                VALUES ('session-a', 'legacy-glm', '["legacy-kimi"]');
            "#,
    )
    .unwrap();

    init_schema(&c).unwrap();
    init_schema(&c).unwrap();
    c.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at)
             VALUES ('local', 'local', 'Local', 1, 0)",
        [],
    )
    .unwrap();

    let fk_on: i64 = c
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk_on, 1);
    let old_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'agents_old_reasoning_check'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(old_exists, 0);
    let fk_errors: Vec<String> = c
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(fk_errors.is_empty(), "foreign_key_check: {fk_errors:?}");

    let fks: Vec<(String, String, String)> = c
        .prepare("PRAGMA foreign_key_list(session_agent_configs)")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        fks.iter().any(|(table, from, to)| {
            table == "agents" && from == "lead_agent_id" && to == "id"
        }),
        "session_agent_configs.lead_agent_id 应指向 agents(id)：{fks:?}"
    );
    assert_eq!(
        get_session_agent_config(&c, "session-a").unwrap(),
        SessionAgentConfig {
            session_id: "session-a".into(),
            lead_agent_id: None,
            member_agent_ids: Vec::new(),
        }
    );
}

#[test]
fn init_schema_adds_cap_lead_to_agents_idempotent() {
    let c = Connection::open_in_memory().unwrap();
    c.execute(
        "CREATE TABLE agents (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL,
                provider TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        [],
    )
    .unwrap();

    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(agents)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"cap_lead".into()),
        "agents 应含 cap_lead：{cols:?}"
    );

    init_schema(&c).unwrap();
}

#[test]
fn init_schema_preserves_legacy_harness_access() {
    let c = Connection::open_in_memory().unwrap();
    create_legacy_agents_reasoning_table(&c, "agents");

    init_schema(&c).unwrap();

    let kimi = get_agent(&c, "legacy-kimi").unwrap().unwrap();
    let glm = get_agent(&c, "legacy-glm").unwrap().unwrap();
    assert_eq!(kimi.access, "borrow");
    assert_eq!(glm.access, "harness");
    assert_eq!(glm.provider, "zhipu");
    assert_eq!(glm.primary_model.as_deref(), Some("glm-4.7"));
    let old_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'agents_old_reasoning_check'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(old_exists, 0);
}

#[test]
fn init_schema_recovers_leftover_agents_old_reasoning_check_table() {
    let c = mem();
    seed_builtin_agents(&c).unwrap();
    create_legacy_agents_reasoning_table(&c, "agents_old_reasoning_check");

    init_schema(&c).unwrap();

    let ids: Vec<String> = list_agents(&c)
        .unwrap()
        .into_iter()
        .map(|agent| agent.id)
        .collect();
    assert_eq!(ids, vec!["claude", "codex", "legacy-kimi", "legacy-glm"]);
    assert_eq!(
        get_agent(&c, "legacy-glm").unwrap().unwrap().access,
        "harness"
    );
    let old_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'agents_old_reasoning_check'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(old_exists, 0);
}

#[test]
fn agents_check_rejects_invalid_access() {
    let c = mem();
    let err = c
        .execute(
            "INSERT INTO agents (id, name, access, provider, created_at, updated_at) \
                 VALUES ('a1', 'Agent 1', 'x', 'openai', 1, 1)",
            [],
        )
        .expect_err("invalid access 应被 CHECK 拒绝");
    assert!(
        err.to_string().contains("CHECK constraint failed"),
        "应由 CHECK constraint 拦截，实际错误：{err}"
    );
}

#[test]
fn agents_int_bool_defaults_applied() {
    let c = mem();
    c.execute(
        "INSERT INTO agents (id, name, access, provider, created_at, updated_at) \
             VALUES ('a1', 'Agent 1', 'native', 'openai', 1, 2)",
        [],
    )
    .unwrap();

    let (
        enabled,
        has_key,
        is_builtin,
        reasoning_default,
        sort_order,
        compat_disable_betas,
        compat_disable_nonessential,
        compat_disable_thinking,
    ): (i64, i64, i64, String, i64, i64, i64, i64) = c
        .query_row(
            "SELECT enabled, has_key, is_builtin, reasoning_default, sort_order, \
                 compat_disable_betas, compat_disable_nonessential, compat_disable_thinking \
                 FROM agents WHERE id = 'a1'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(enabled, 1);
    assert_eq!(has_key, 0);
    assert_eq!(is_builtin, 0);
    assert_eq!(reasoning_default, "auto");
    assert_eq!(sort_order, 0);
    assert_eq!(compat_disable_betas, 0);
    assert_eq!(compat_disable_nonessential, 0);
    assert_eq!(compat_disable_thinking, 0);
}

#[test]
fn agents_upsert_roundtrips_all_fields() {
    let c = mem();
    let mut a = agent("a1", 7, false);
    a.cap_lead = Some("native_cli".into());

    upsert_agent(&c, &a).unwrap();

    assert_eq!(get_agent(&c, "a1").unwrap(), Some(a));
}

#[test]
fn agent_profile_persists_lead_capability() {
    let c = mem();
    seed_builtin_agents(&c).unwrap();

    let agents = list_agents(&c).unwrap();
    let claude = agents.iter().find(|agent| agent.id == "claude").unwrap();
    let codex = agents.iter().find(|agent| agent.id == "codex").unwrap();

    assert_eq!(claude.cap_lead.as_deref(), Some("native_cli"));
    assert_eq!(codex.cap_lead.as_deref(), None);
}

#[test]
fn session_agent_config_defaults_to_solo() {
    let c = mem();
    seed_builtin_agents(&c).unwrap();
    insert_test_session(&c, "session-a");

    let config = get_session_agent_config(&c, "session-a").unwrap();

    assert_eq!(config.session_id, "session-a");
    assert_eq!(config.lead_agent_id, None);
    assert!(config.member_agent_ids.is_empty());
}

#[test]
fn session_agent_config_roundtrips_deduped_members() {
    let c = mem();
    seed_builtin_agents(&c).unwrap();
    insert_test_session(&c, "session-a");

    let saved = set_session_agent_config(
        &c,
        "session-a",
        Some("claude".to_string()),
        vec![
            "".to_string(),
            "codex".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ],
    )
    .unwrap();

    assert_eq!(saved.lead_agent_id.as_deref(), Some("claude"));
    assert_eq!(saved.member_agent_ids, vec!["codex"]);
    assert_eq!(get_session_agent_config(&c, "session-a").unwrap(), saved);
}

#[test]
fn session_agent_config_accepts_enabled_agent_as_lead_without_cap_lead() {
    let c = mem();
    seed_builtin_agents(&c).unwrap();
    insert_test_session(&c, "session-a");

    let saved =
        set_session_agent_config(&c, "session-a", Some("codex".to_string()), vec![]).unwrap();

    assert_eq!(saved.lead_agent_id.as_deref(), Some("codex"));
}

#[test]
fn session_agent_config_rejects_disabled_member() {
    let c = mem();
    seed_builtin_agents(&c).unwrap();
    insert_test_session(&c, "session-a");
    set_agent_enabled(&c, "codex", false).unwrap();

    let err = set_session_agent_config(
        &c,
        "session-a",
        Some("claude".to_string()),
        vec!["codex".to_string()],
    )
    .unwrap_err();

    assert!(err.to_string().contains("disabled"));
}

#[test]
fn copy_session_agent_config_team() {
    let c = mem();
    insert_min_agent(&c, "claude", 0, true);
    insert_min_agent(&c, "m1", 0, true);
    insert_min_agent(&c, "m2", 0, true);
    insert_test_session(&c, "p1");
    insert_test_session(&c, "c1");
    set_session_agent_config(
        &c,
        "p1",
        Some("claude".to_string()),
        vec!["m1".to_string(), "m2".to_string()],
    )
    .unwrap();

    copy_session_agent_config(&c, "p1", "c1").unwrap();

    let child = get_session_agent_config(&c, "c1").unwrap();
    assert_eq!(child.lead_agent_id.as_deref(), Some("claude"));
    assert_eq!(child.member_agent_ids, vec!["m1", "m2"]);
}

#[test]
fn copy_session_agent_config_solo_default() {
    let c = mem();
    insert_test_session(&c, "p2");
    insert_test_session(&c, "c2");

    copy_session_agent_config(&c, "p2", "c2").unwrap();

    let child = get_session_agent_config(&c, "c2").unwrap();
    assert_eq!(child.lead_agent_id, None);
    assert!(child.member_agent_ids.is_empty());
    let count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM session_agent_configs WHERE session_id = 'c2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn session_mode_team_vs_solo() {
    let c = mem();
    insert_min_agent(&c, "claude", 0, true);
    insert_min_agent(&c, "m1", 0, true);
    insert_min_agent(&c, "m2", 0, true);
    insert_test_session(&c, "team");
    insert_test_session(&c, "solo");
    set_session_agent_config(
        &c,
        "team",
        Some("claude".to_string()),
        vec!["m1".to_string(), "m2".to_string()],
    )
    .unwrap();

    assert_eq!(
        session_mode(&c, "team").unwrap(),
        SessionMode::Team {
            lead_agent_id: "claude".to_string(),
            member_ids: vec!["m1".to_string(), "m2".to_string()],
        }
    );
    assert_eq!(session_mode(&c, "solo").unwrap(), SessionMode::Solo);
}

#[test]
fn agents_list_ordered() {
    let c = mem();
    upsert_agent(&c, &agent("middle", 20, false)).unwrap();
    upsert_agent(&c, &agent("first", 10, false)).unwrap();
    upsert_agent(&c, &agent("last", 30, false)).unwrap();

    let ids: Vec<String> = list_agents(&c).unwrap().into_iter().map(|a| a.id).collect();

    assert_eq!(ids, vec!["first", "middle", "last"]);
}

#[test]
fn agents_get_missing_none() {
    let c = mem();

    assert_eq!(get_agent(&c, "missing").unwrap(), None);
}

#[test]
fn agents_delete_borrow_builtin_ok() {
    let c = mem();
    let mut a = agent("a1", 0, true);
    a.access = "borrow".into();
    upsert_agent(&c, &a).unwrap();

    delete_agent(&c, "a1").unwrap();

    assert_eq!(get_agent(&c, "a1").unwrap(), None);
}

#[test]
fn agents_delete_native_rejected() {
    let c = mem();
    let a = agent("native", 0, false);
    upsert_agent(&c, &a).unwrap();

    assert!(delete_agent(&c, "native").is_err());

    assert_eq!(get_agent(&c, "native").unwrap(), Some(a));
}

#[test]
fn agents_upsert_update_preserves_created_at() {
    let c = mem();
    let first = agent("a1", 0, false);
    upsert_agent(&c, &first).unwrap();

    let mut second = agent("a1", 1, false);
    second.created_at = 999;
    second.updated_at = 300;
    upsert_agent(&c, &second).unwrap();

    let got = get_agent(&c, "a1").unwrap().unwrap();
    assert_eq!(got.created_at, first.created_at);
    assert_eq!(got.updated_at, second.updated_at);
}

#[test]
fn seed_inserts_two_when_empty() {
    let c = mem();

    seed_builtin_agents(&c).unwrap();

    let ids: Vec<String> = list_agents(&c).unwrap().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, vec!["claude", "codex"]);
}

#[test]
fn seed_builtin_agents_excludes_deepseek() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    seed_builtin_agents(&conn).unwrap();
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM agents ORDER BY sort_order")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ids, vec!["claude".to_string(), "codex".to_string()]);
    assert!(!ids.contains(&"deepseek".to_string()));
}

#[test]
fn migrate_remove_placeholder_deepseek_only_deletes_keyless_builtin() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    insert_min_agent(&conn, "deepseek", 1, false);
    insert_min_agent(&conn, "deepseek-keyed", 1, true);
    insert_min_agent(&conn, "DeepSeekPro", 0, false);
    let n = migrate_remove_placeholder_deepseek(&conn).unwrap();
    assert_eq!(n, 1);
    let remaining: Vec<String> = conn
        .prepare("SELECT id FROM agents ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        remaining,
        vec!["DeepSeekPro".to_string(), "deepseek-keyed".to_string()]
    );
    assert_eq!(migrate_remove_placeholder_deepseek(&conn).unwrap(), 0);
}

#[test]
fn seed_idempotent() {
    let c = mem();

    seed_builtin_agents(&c).unwrap();
    seed_builtin_agents(&c).unwrap();

    let ids: Vec<String> = list_agents(&c).unwrap().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, vec!["claude", "codex"]);
}

#[test]
fn last_session_agent_id_returns_last_agent() {
    use crate::test_support::mem_db;
    let c = mem_db();
    create_session(&c, "s1", "repo", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[],
        Some("claude"),
        Some("claude"),
        Some("Claude"),
    )
    .unwrap();
    let result = last_session_agent_id(&c, "s1").unwrap();
    assert_eq!(result, Some("claude".to_string()));
}

#[test]
fn last_session_agent_id_returns_none_when_no_messages() {
    use crate::test_support::mem_db;
    let c = mem_db();
    create_session(&c, "s2", "repo", "local-default", "local").unwrap();
    let result = last_session_agent_id(&c, "s2").unwrap();
    assert_eq!(result, None);
}
