#[tokio::test]
async fn read_edit_test_fix_runs_to_completion_unattended() {
    let ws = tempfile::tempdir().unwrap();
    let opts = myagent::orchestrator::RunOptions {
        prompt: "run the agentic loop demo".into(),
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
        run_id: None,
        context_files: vec![],
        criteria: myagent::goal::parse_criteria(&["contains:VALUE=2: cat demo.txt".into()])
            .unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };
    let res =
        myagent::orchestrator::run_solo(myagent::provider::mock::MockProvider::default(), opts)
            .await
            .unwrap();
    let run_dir = ws.path().join(".myagenthubs/runs").join(&res.run_id);
    // 1) 文件确实被写+改
    assert_eq!(
        std::fs::read_to_string(ws.path().join("demo.txt"))
            .unwrap()
            .trim(),
        "VALUE=2"
    );
    // 2) 事件链
    let j = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    for needle in [
        "\"tool\":\"fs_write\"",
        "\"tool\":\"fs_read\"",
        "\"tool\":\"fs_edit\"",
        "\"type\":\"artifact.created\"",
        "\"passed\":true",
        "\"type\":\"run.completed\"",
    ] {
        assert!(j.contains(needle), "missing {needle}");
    }
    // 3) >=3 个工具轮
    let tool_turns = j
        .lines()
        .filter(|l| l.contains("tool_results_added"))
        .count();
    assert!(tool_turns >= 3, "expected >=3 tool turns, got {tool_turns}");
}
