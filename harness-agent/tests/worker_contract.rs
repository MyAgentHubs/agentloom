use std::collections::BTreeSet;

use async_trait::async_trait;
use myagent::events::{EventRecorder, OutputMode};
use myagent::orchestrator::{run_solo_with_judge, ControlInputKind, RunOptions, RunOutcome};
use myagent::provider::{
    ChatMessage, FinishReason, FunctionCall, ProviderCapabilities, ProviderClient,
    ProviderResponse, ToolCall,
};

#[test]
fn registry_exposes_exactly_eight_tool_whitelist_no_scheduler() {
    let defs =
        myagent::orchestrator::build_default_registry(&myagent::config::SearchChoice::Ddg, false)
            .definitions();
    let names: BTreeSet<String> = defs
        .iter()
        .map(|d| {
            d["function"]["name"]
                .as_str()
                .expect("tool def 有 function.name")
                .to_string()
        })
        .collect();
    let expected: BTreeSet<String> = [
        "shell_exec",
        "fs_read",
        "ls",
        "glob",
        "grep",
        "fs_write",
        "fs_edit",
        "web_search",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        names, expected,
        "工具白名单漂移（任何排程类工具入册都会红）"
    );
    for forbidden in [
        "schedule", "cron", "at", "loop", "wakeup", "setsid", "nohup",
    ] {
        assert!(
            !names.iter().any(|n| n.contains(forbidden)),
            "禁止的排程/逃逸类工具名: {forbidden}"
        );
    }
}

struct PgidProbeProvider;

#[async_trait]
impl ProviderClient for PgidProbeProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        if messages.last().map(|m| m.role.as_str()) == Some("tool") {
            events.emit_text_delta("done")?;
            return Ok(ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
            });
        }

        let cmd =
            r#"test "$(ps -o pgid= -p $$ | tr -d ' ')" = "$(ps -o pgid= -p $PPID | tr -d ' ')""#;
        Ok(ProviderResponse {
            text: "probe".into(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_pgid".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: serde_json::json!({"command": cmd, "timeout_ms": 5000}).to_string(),
                },
            }],
            finish_reason: Some(FinishReason::ToolCalls),
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "pgid".into(),
            model_id: "pgid".into(),
            supports_streaming: true,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }
}

#[tokio::test]
async fn shell_exec_child_stays_in_same_process_group() {
    let ws = tempfile::tempdir().unwrap();
    let opts = RunOptions {
        prompt: "probe pgid".into(),
        workspace: ws.path().to_path_buf(),
        journal_root: ws.path().to_path_buf(),
        provider_id: "pgid".into(),
        model: "pgid".into(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
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
    let res = run_solo_with_judge(PgidProbeProvider, Box::new(myagent::judge::NoopJudge), opts)
        .await
        .unwrap();
    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(
        matches!(res.outcome, RunOutcome::Completed),
        "unexpected outcome {:?}; events:\n{journal}",
        res.outcome
    );
    assert!(journal.contains("\"type\":\"tool.completed\""));
    assert!(
        journal.contains("\"exit_code\":0"),
        "子进程 pgid≠父 → 可能 setsid 脱组（killpg 不友好）"
    );
}
