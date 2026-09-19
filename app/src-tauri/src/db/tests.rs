#![cfg(test)]

use super::*;

mod agents;
mod artifacts;
mod checkpoints;
mod commit_ledger;
mod continuations;
mod decision_cards;
mod dispatch_cards;
mod memory_blocks;
mod memory_entries;
mod message_history;
mod messages;
mod session_lifecycle;
mod settings;
mod team_contracts;
mod team_state;
mod workspace_schema;

fn mem() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    init_schema(&c).unwrap();
    // cluster L Phase 2 plan A Task 2 必修 #3：seed Local namespace + local-default repo
    // 防 repos.namespace_id / sessions.repo_id FK 约束失败崩既有 db::tests
    // rusqlite 0.32 bundled 默认 PRAGMA foreign_keys = 1 · 实测确认
    c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local', 'local', 'Local', 1, 0)",
            [],
        )
        .unwrap();
    std::fs::create_dir_all("/tmp/agentloom-mem-local-default").unwrap();
    c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default', 'local', 'local', '我的项目', '/tmp/agentloom-mem-local-default', 'active', 0)",
            [],
        )
        .unwrap();
    c
}

fn running_dispatch_card(assignment_id: &str) -> Block {
    Block::DispatchCard {
        run_id: format!("worker-run-{assignment_id}"),
        member: MemberSnapshot {
            participant_id: "worker-1".into(),
            assignment_id: assignment_id.into(),
            task_id: "task-1".into(),
            name: "Codex Worker".into(),
            started_at: Some(1_785_500_450_123),
            status: "running".into(),
            sub: "实现终态收敛".into(),
            steps_total: 3,
            steps_done: 1,
            cost_usd: Some(0.25),
            input_tokens: 17,
            output_tokens: 29,
            failed: false,
            blocks: vec![],
            result: None,
        },
    }
}
