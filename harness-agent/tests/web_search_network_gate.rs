//! 联网关时即便模型硬调 web_search 也不执行、不打真网、不丢整轮（run 不 Failed）。

#[tokio::test]
async fn web_search_refused_when_network_off_no_live_call() {
    let ws = tempfile::tempdir().unwrap();
    let opts = myagent::orchestrator::RunOptions {
        prompt: "web_search demo".into(),
        workspace: ws.path().to_path_buf(),
        journal_root: ws.path().to_path_buf(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
        control_input: myagent::orchestrator::ControlInputKind::Sentinel,
        permission: myagent::shell::PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::Off,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 4,
        run_id: None,
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::TrustUser,
        max_eval_attempts: 2,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };
    let res =
        myagent::orchestrator::run_solo(myagent::provider::mock::MockProvider::default(), opts)
            .await
            .unwrap();
    let j = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();

    assert!(
        j.contains("\"type\":\"tool.failed\"") && j.contains("network off"),
        "expected network-off tool.failed"
    );
    assert!(
        !j.contains("\"type\":\"tool.started\""),
        "web_search must not run under network off"
    );
    assert!(!j.contains("\"type\":\"run.failed\""), "run must not fail");
}
