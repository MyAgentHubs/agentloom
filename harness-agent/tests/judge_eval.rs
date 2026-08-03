use myagent::judge::{FixedJudge, JudgeDecision};
use myagent::orchestrator::{run_solo_with_judge, RunOptions};

fn opts(ws: &std::path::Path, run_id: &str, max_eval: usize) -> RunOptions {
    RunOptions {
        prompt: "hello".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
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
        max_turns: 6,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria: myagent::goal::parse_criteria(&["judge: code is idiomatic".into()]).unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: max_eval,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

#[tokio::test]
async fn judgmental_pass_completes() {
    let ws = tempfile::tempdir().unwrap();
    run_solo_with_judge(
        myagent::provider::mock::MockProvider::default(),
        Box::new(FixedJudge {
            decision: JudgeDecision::Pass,
        }),
        opts(ws.path(), "run_jp", 2),
    )
    .await
    .unwrap();
    let j =
        std::fs::read_to_string(ws.path().join(".myagenthubs/runs/run_jp/events.jsonl")).unwrap();
    assert!(j.contains("\"decision\":\"passed\"") || j.contains("\"status\":\"passed\""));
    assert!(j.contains("\"type\":\"run.completed\""));
}

#[tokio::test]
async fn judgmental_uncertain_does_not_pass_and_blocks() {
    let ws = tempfile::tempdir().unwrap();
    // mock "hello" 每轮产文本 → 每轮 judge → Uncertain → 未过 → max_eval_attempts=2 → blocked
    let res = run_solo_with_judge(
        myagent::provider::mock::MockProvider::default(),
        Box::new(FixedJudge {
            decision: JudgeDecision::Uncertain,
        }),
        opts(ws.path(), "run_ju", 2),
    )
    .await
    .unwrap();
    let _ = res;
    let j =
        std::fs::read_to_string(ws.path().join(".myagenthubs/runs/run_ju/events.jsonl")).unwrap();
    assert!(j.contains("\"type\":\"run.blocked\""));
    assert!(!j.contains("\"type\":\"run.completed\""));
    assert!(j.contains("uncertain"));
}
