#![cfg(test)]

use super::super::*;

mod activity_summaries;
mod content_blocks;
mod message_delivery;
mod profiles;
mod remote_devices;
mod remote_inbox;
mod remote_rooms;
mod repo_documents;
mod runtime;

fn mem() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    init_schema(&c).unwrap();
    c
}

fn member_report_delivery_running_card(assignment_id: &str) -> Block {
    Block::DispatchCard {
        run_id: format!("worker-run-{assignment_id}"),
        member: MemberSnapshot {
            participant_id: "worker-1".into(),
            assignment_id: assignment_id.into(),
            task_id: "task-1".into(),
            name: "Worker".into(),
            started_at: Some(1),
            status: "running".into(),
            sub: "test report rollback".into(),
            steps_total: 1,
            steps_done: 0,
            cost_usd: None,
            input_tokens: 0,
            output_tokens: 0,
            failed: false,
            blocks: vec![],
            result: None,
        },
    }
}

fn agent(id: &str, sort_order: i64, is_builtin: bool) -> AgentProfile {
    AgentProfile {
        id: id.into(),
        name: format!("Agent {id}"),
        access: "native".into(),
        provider: "openai".into(),
        primary_model: Some("gpt-5".into()),
        endpoint: Some("https://api.example.test/v1".into()),
        auth_mode: Some("bearer".into()),
        model_opus: Some("opus-model".into()),
        model_sonnet: Some("sonnet-model".into()),
        model_haiku: Some("haiku-model".into()),
        model_subagent: Some("subagent-model".into()),
        reasoning_default: "high".into(),
        max_output_tokens: Some(4096),
        api_timeout_ms: Some(120_000),
        compat_disable_betas: true,
        compat_disable_nonessential: false,
        compat_disable_thinking: true,
        compat_proxy: Some("http://127.0.0.1:7890".into()),
        custom_headers: Some(r#"{"X-Test":"yes"}"#.into()),
        extra_body: Some(r#"{"temperature":0.2}"#.into()),
        cap_reasoning: Some("native".into()),
        cap_computer_use: Some("disabled".into()),
        cap_lead: None,
        has_key: true,
        is_builtin,
        enabled: true,
        sort_order,
        created_at: 100,
        updated_at: 200,
    }
}

fn insert_min_agent(conn: &Connection, id: &str, is_builtin: i64, has_key: bool) {
    conn.execute(
        "INSERT INTO agents (id, name, access, provider, reasoning_default, \
             compat_disable_betas, compat_disable_nonessential, compat_disable_thinking, \
             has_key, is_builtin, enabled, sort_order, created_at, updated_at) \
             VALUES (?1, ?1, 'borrow', 'deepseek', 'auto', 0, 0, 0, ?2, ?3, 1, 0, 0, 0)",
        rusqlite::params![id, has_key as i64, is_builtin],
    )
    .unwrap();
}

fn insert_test_session(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
             VALUES ('local', 'local', 'Local', 1, 0)",
        [],
    )
    .unwrap();
    std::fs::create_dir_all("/tmp/agentloom-agents-local-default").unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) \
             VALUES ('local-default', 'local', 'local', 'Local', \
             '/tmp/agentloom-agents-local-default', 'active', 0)",
        [],
    )
    .unwrap();
    create_session(conn, id, id, "local-default", "local").unwrap();
}

fn create_legacy_agents_reasoning_table(conn: &Connection, table_name: &str) {
    conn.execute_batch(&format!(
        r#"
            CREATE TABLE {table_name} (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL CHECK (access IN ('native', 'borrow', 'harness')),
                provider TEXT NOT NULL,
                primary_model TEXT,
                endpoint TEXT,
                auth_mode TEXT CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
                model_opus TEXT,
                model_sonnet TEXT,
                model_haiku TEXT,
                model_subagent TEXT,
                reasoning_default TEXT NOT NULL DEFAULT 'auto'
                    CHECK (reasoning_default IN ('auto', 'low', 'medium', 'high')),
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
                has_key INTEGER NOT NULL DEFAULT 0,
                is_builtin INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            INSERT INTO {table_name}
                (id, name, access, provider, primary_model, endpoint, auth_mode,
                 reasoning_default, compat_disable_betas, compat_disable_nonessential,
                 compat_disable_thinking, has_key, is_builtin, enabled, sort_order,
                 created_at, updated_at)
            VALUES
                ('legacy-kimi', 'Kimi K2.6', 'borrow', 'kimi', 'kimi-k2.6',
                 'https://api.moonshot.cn/anthropic', 'bearer', 'auto',
                 0, 1, 0, 1, 0, 1, 3, 100, 200),
                ('legacy-glm', '智谱 GLM', 'harness', 'zhipu', 'glm-4.7',
                 'https://open.bigmodel.cn/api/paas/v4', 'bearer', 'auto',
                 0, 1, 0, 1, 0, 1, 9, 100, 200);
            "#
    ))
    .unwrap();
}
