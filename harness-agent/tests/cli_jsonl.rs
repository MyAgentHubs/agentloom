use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn no_subcommand_starts_interactive_session_and_resumes_active_run() {
    let workspace = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("myagent").unwrap();
    let assert = cmd
        .args([
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            workspace.path().to_str().unwrap(),
        ])
        .write_stdin("hello\ncontinue\n/exit\n")
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("myagent interactive"));
    assert!(stdout.contains("Mock response for: hello"));
    assert!(stdout.contains("Mock response for: hello\n"));
    assert!(stdout.contains("Mock response for: continue"));

    let runs_dir = workspace.path().join(".myagenthubs").join("runs");
    let run_dirs = std::fs::read_dir(runs_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    assert_eq!(run_dirs.len(), 1);

    let journal = std::fs::read_to_string(run_dirs[0].join("events.jsonl")).unwrap();
    assert!(journal.contains("\"type\":\"run.started\""));
    assert!(journal.contains("\"type\":\"run.resumed\""));
}

#[test]
fn mock_run_emits_jsonl_journal_and_tool_events() {
    let workspace = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("myagent").unwrap();
    let output = cmd
        .args([
            "run",
            "ship dispatch handoff",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            workspace.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    let event_types: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    for expected in [
        "run.started",
        "goal.created",
        "capabilities.declared",
        "agent.note.delta",
        "approval.requested",
        "approval.resolved",
        "tool.started",
        "tool.stdout.delta",
        "tool.completed",
        "completion.evaluated",
        "run.completed",
    ] {
        assert!(
            event_types.contains(&expected),
            "missing {expected}; got {event_types:?}"
        );
    }

    let run_id = events[0]["run_id"].as_str().unwrap();
    let run_dir = workspace
        .path()
        .join(".myagenthubs")
        .join("runs")
        .join(run_id);
    assert!(run_dir.join("events.jsonl").exists());
    assert!(run_dir.join("conversation.json").exists());
    assert!(!run_dir.join("artifacts").join("final.md").exists());
}

#[test]
fn resume_uses_saved_provider_and_appends_to_journal() {
    let workspace = tempdir().unwrap();
    let mut first = Command::cargo_bin("myagent").unwrap();
    let first_output = first
        .args([
            "run",
            "hello",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            workspace.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(first_output.status.success());
    let first_events = parse_jsonl(&first_output.stdout);
    let run_id = first_events[0]["run_id"].as_str().unwrap().to_string();

    let mut resume = Command::cargo_bin("myagent").unwrap();
    let resume_output = resume
        .args([
            "resume",
            &run_id,
            "continue",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            workspace.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        resume_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&resume_output.stderr)
    );
    let resumed_events = parse_jsonl(&resume_output.stdout);
    assert_eq!(resumed_events[0]["type"], "run.resumed");
    let first_max_seq = first_events
        .iter()
        .filter_map(|event| event["seq"].as_u64())
        .max()
        .unwrap();
    assert!(resumed_events[0]["seq"].as_u64().unwrap() > first_max_seq);
    assert!(resumed_events
        .iter()
        .any(|event| event["type"] == "run.completed"));
}

#[test]
fn shell_non_zero_exit_is_returned_to_agent_instead_of_killing_run() {
    let workspace = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("myagent").unwrap();
    let output = cmd
        .args([
            "run",
            "fail shell",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            workspace.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    assert!(events.iter().any(|event| {
        event["type"] == "tool.completed" && event["payload"]["exit_code"].as_i64() == Some(7)
    }));
    assert!(events.iter().any(|event| event["type"] == "run.completed"));
}

#[test]
fn ask_permission_fails_closed_in_non_interactive_jsonl_mode() {
    let workspace = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("myagent").unwrap();
    let output = cmd
        .args([
            "run",
            "ship dispatch handoff",
            "--provider",
            "mock",
            "--jsonl",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            workspace.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let events = parse_jsonl(&output.stdout);
    assert!(events.iter().any(|event| {
        event["type"] == "approval.resolved"
            && event["payload"]["decision"] == "rejected"
            && event["payload"]["reason"] == "channel_closed"
    }));
    assert!(events.iter().any(|event| {
        event["type"] == "run.blocked" && event["payload"]["reason"] == "approval_unavailable"
    }));
}

#[test]
fn plain_chat_no_tool_no_criteria_exits_0() {
    let workspace = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hello",
            "--provider",
            "mock",
            "--jsonl",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            workspace.path().to_str().unwrap(),
        ])
        .assert()
        .code(0);
}

#[test]
fn config_provider_writes_provider_file_under_myagent_home() {
    let home = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("myagent").unwrap();
    let output = cmd
        .env("MYAGENT_HOME", home.path())
        .args([
            "config",
            "provider",
            "deepseek",
            "--api-key",
            "sk-test",
            "--model",
            "deepseek-v4-flash",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let config_path = home.path().join("config.json");
    let config: Value = serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
    assert_eq!(config["providers"][0]["id"], "deepseek");
    assert_eq!(config["providers"][0]["api_key"], "sk-test");
}

fn parse_jsonl(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn text_only_prompt_creates_no_artifact() {
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
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        run_id: None,
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };
    let res =
        myagent::orchestrator::run_solo(myagent::provider::mock::MockProvider::default(), opts)
            .await
            .unwrap();
    let run_dir = ws.path().join(".myagenthubs/runs").join(&res.run_id);
    assert!(
        !run_dir.join("artifacts/final.md").exists(),
        "must not write final.md"
    );
    let journal = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(
        !journal.contains("\"artifact.created\""),
        "must not emit artifact.created"
    );
}

#[tokio::test]
async fn interrupt_via_control_source_emits_run_interrupted() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "run_intr_test".to_string();
    myagent::orchestrator::request_interrupt(ws.path(), &run_id).unwrap();
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
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        run_id: Some(run_id.clone()),
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };

    let res =
        myagent::orchestrator::run_solo(myagent::provider::mock::MockProvider::default(), opts)
            .await;

    let res = res.unwrap();
    assert_eq!(res.outcome, myagent::orchestrator::RunOutcome::Interrupted);
    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(
        journal.contains("\"run.interrupted\""),
        "must emit run.interrupted"
    );
}
