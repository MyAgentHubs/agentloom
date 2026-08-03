#[tokio::test]
async fn verifiable_passing_check_cmd_completes() {
    let ws = tempfile::tempdir().unwrap();
    let opts = myagent::orchestrator::RunOptions {
        prompt: "hello".into(),
        workspace: ws.path().to_path_buf(),
        journal_root: ws.path().to_path_buf(),
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
        max_turns: 4,
        max_eval_attempts: 4,
        run_id: None,
        context_files: vec![],
        criteria: myagent::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };

    let res =
        myagent::orchestrator::run_solo(myagent::provider::mock::MockProvider::default(), opts)
            .await
            .unwrap();
    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();

    assert!(journal.contains("\"passed\":true"));
    assert!(journal.contains("\"type\":\"run.completed\""));
}

#[tokio::test]
async fn verifiable_failing_check_cmd_does_not_complete() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "run_fail_b9".to_string();
    let opts = myagent::orchestrator::RunOptions {
        prompt: "hello".into(),
        workspace: ws.path().to_path_buf(),
        journal_root: ws.path().to_path_buf(),
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
        max_turns: 2,
        max_eval_attempts: 8,
        run_id: Some(run_id.clone()),
        context_files: vec![],
        criteria: myagent::goal::parse_criteria(&["cmd: false".into()]).unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };

    let res =
        myagent::orchestrator::run_solo(myagent::provider::mock::MockProvider::default(), opts)
            .await;

    let res = res.unwrap();
    assert_eq!(
        res.outcome,
        myagent::orchestrator::RunOutcome::NeedsDecision
    );
    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(journal.contains("\"passed\":false"));
    assert!(journal.contains("\"type\":\"run.needs_decision\""));
    assert!(journal.contains("\"reason\":\"blocked_questions\""));
    assert!(journal.contains("\"blocked_reason\":\"no_progress\""));
    assert!(!journal.contains("\"type\":\"run.failed\""));
    assert!(!journal.contains("max_turns_exceeded"));
    assert!(!journal.contains("\"type\":\"run.completed\""));
}

#[tokio::test]
async fn blocked_when_check_cmd_keeps_failing() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "run_blocked_b10".to_string();
    let opts = myagent::orchestrator::RunOptions {
        prompt: "hello".into(),
        workspace: ws.path().to_path_buf(),
        journal_root: ws.path().to_path_buf(),
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
        max_eval_attempts: 2,
        run_id: Some(run_id.clone()),
        context_files: vec![],
        criteria: myagent::goal::parse_criteria(&["cmd: false".into()]).unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };

    myagent::orchestrator::run_solo(myagent::provider::mock::MockProvider::default(), opts)
        .await
        .unwrap();
    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(journal.contains("\"type\":\"run.blocked\""));
    assert!(!journal.contains("\"type\":\"run.completed\""));
}
