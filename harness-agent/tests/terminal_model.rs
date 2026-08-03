use myagent::orchestrator::{run_solo, RunOptions, RunOutcome};
use myagent::provider::mock::MockProvider;
use myagent::provider::{ChatMessage, ProviderCapabilities, ProviderClient, ProviderResponse};

fn base_opts(ws: &std::path::Path, prompt: &str) -> RunOptions {
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
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        journal_root: ws.to_path_buf(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

#[tokio::test]
async fn interrupt_yields_ok_interrupted_not_err() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "run_intr_test";
    let run_dir = ws.path().join(".myagenthubs/runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("interrupt.request"), b"").unwrap();
    let mut opts = base_opts(ws.path(), "say something");
    opts.run_id = Some(run_id.into());
    let res = run_solo(MockProvider::default(), opts).await.unwrap();
    assert_eq!(res.outcome, RunOutcome::Interrupted);
    let j = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(j.contains("\"type\":\"run.interrupted\""));
    assert!(j.contains("\"step_id\":\"provider.next_turn\""));
}

#[tokio::test]
async fn max_turns_yields_ok_needs_decision_not_err() {
    let ws = tempfile::tempdir().unwrap();
    let mut opts = base_opts(ws.path(), "dispatch the shell command");
    opts.max_turns = 1;
    opts.criteria = myagent::goal::parse_criteria(&["cmd: test -f never".into()]).unwrap();

    let res = run_solo(MockProvider::default(), opts).await.unwrap();

    assert_eq!(res.outcome, RunOutcome::NeedsDecision);
    let run_dir = ws.path().join(".myagenthubs/runs").join(&res.run_id);
    let j = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(j.contains("\"type\":\"run.needs_decision\""));
    assert!(j.contains("\"reason\":\"blocked_questions\""));
    assert!(
        j.contains("\"blocked_reason\":\"budget_exhausted_still_progressing\"")
            || j.contains("\"blocked_reason\":\"no_progress\"")
    );
    assert!(!j.contains("\"type\":\"run.failed\""));
    assert!(!j.contains("max_turns_exceeded"));
}

#[derive(Debug, Clone)]
struct ErroringProvider;

#[async_trait::async_trait]
impl ProviderClient for ErroringProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        Err(myagent::error::HarnessError::Provider("boom".into()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider::default().capabilities()
    }
}

const TERMINALS: &[&str] = &[
    "run.completed",
    "run.blocked",
    "run.failed",
    "run.interrupted",
    "run.needs_decision",
];

fn count_terminals(j: &str) -> usize {
    j.lines()
        .filter(|line| {
            TERMINALS
                .iter()
                .any(|terminal| line.contains(&format!("\"type\":\"{terminal}\"")))
        })
        .count()
}

#[tokio::test]
async fn post_start_provider_error_becomes_run_failed_ok() {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo(ErroringProvider, base_opts(ws.path(), "anything"))
        .await
        .unwrap();
    assert_eq!(res.outcome, RunOutcome::Failed);
    let run_dir = ws.path().join(".myagenthubs/runs").join(&res.run_id);
    let j = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(j.contains("\"type\":\"run.started\""));
    assert!(j.contains("\"type\":\"run.failed\""));
    assert_eq!(count_terminals(&j), 1, "exactly one run.* terminal");
}

#[tokio::test]
async fn post_start_context_file_error_becomes_run_failed_ok() {
    let ws = tempfile::tempdir().unwrap();
    let mut opts = base_opts(ws.path(), "anything");
    opts.context_files = vec![ws.path().join("nonexistent-context.md")];
    let res = run_solo(MockProvider::default(), opts).await.unwrap();
    assert_eq!(res.outcome, RunOutcome::Failed);
    let run_dir = ws.path().join(".myagenthubs/runs").join(&res.run_id);
    let j = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(j.contains("\"type\":\"run.failed\""));
    assert_eq!(count_terminals(&j), 1);
}
