use myagent::orchestrator::{run_solo, RunOptions, RunOutcome};
use myagent::provider::mock::MockProvider;

fn opts(ws: &std::path::Path, prompt: &str, criteria: &[&str]) -> RunOptions {
    RunOptions {
        prompt: prompt.into(),
        workspace: ws.to_path_buf(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
        control_input: myagent::orchestrator::ControlInputKind::Sentinel,
        permission: myagent::shell::PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 8,
        run_id: None,
        context_files: vec![],
        criteria: myagent::goal::parse_criteria(
            &criteria.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 1,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        journal_root: ws.to_path_buf(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

#[tokio::test]
async fn run_outcome_reports_completed_for_verifiable_pass() {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo(
        MockProvider::default(),
        opts(ws.path(), "say hi", &["cmd: true"]),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, RunOutcome::Completed);
}

#[tokio::test]
async fn run_outcome_reports_blocked_when_criteria_unmet() {
    // cmd: 恒不达标 + max_eval_attempts=1 -> 首轮 eval Failed 即 attempts 超限 -> Blocked（非 max_turns）
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo(
        MockProvider::default(),
        opts(ws.path(), "say hi", &["cmd: test -f definitely-missing"]),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, RunOutcome::Blocked);
}
