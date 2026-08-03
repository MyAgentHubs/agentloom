use super::*;
use crate::events::OutputMode;
use crate::shell::PermissionPolicy;

#[tokio::test]
async fn jsonl_output_with_sentinel_control_honors_interrupt_file() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "r_ci_t1".to_string();
    request_interrupt(ws.path(), &run_id).unwrap();
    let opts = RunOptions {
        prompt: "hello".into(),
        workspace: ws.path().to_path_buf(),
        journal_root: ws.path().to_path_buf(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: OutputMode::Jsonl,
        control_input: ControlInputKind::Sentinel,
        evidence_gate: EvidenceGate::Off,
        permission: PermissionPolicy::Allow,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: crate::config::SearchChoice::Ddg,
        max_turns: 4,
        max_eval_attempts: 4,
        run_id: Some(run_id.clone()),
        context_files: vec![],
        criteria: vec![],
        contract_policy: crate::guardrails::ContractPolicy::Ask,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };
    let res = run_solo(crate::provider::mock::MockProvider::default(), opts)
        .await
        .unwrap();
    assert_eq!(res.outcome, RunOutcome::Interrupted);
}
