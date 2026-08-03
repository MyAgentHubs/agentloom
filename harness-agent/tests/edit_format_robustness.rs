use myagent::provider::{
    ChatMessage, FunctionCall, ProviderCapabilities, ProviderClient, ProviderResponse, ToolCall,
};

struct OneFsWriteThenStop {
    args: String,
}

#[async_trait::async_trait]
impl ProviderClient for OneFsWriteThenStop {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        events: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        if messages.iter().any(|m| m.role == "tool") {
            events.emit_text_delta("done")?;
            return Ok(ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            });
        }
        events.emit_text_delta("writing")?;
        Ok(ProviderResponse {
            text: "writing".into(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_efr_1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "fs_write".into(),
                    arguments: self.args.clone(),
                },
            }],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "mock".into(),
            model_id: "mock-model".into(),
            supports_streaming: true,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: Some(128_000),
            output_token_limit: Some(8_192),
            server_side_search: false,
        }
    }
}

fn opts_for(ws: &std::path::Path, prompt: &str) -> myagent::orchestrator::RunOptions {
    myagent::orchestrator::RunOptions {
        prompt: prompt.into(),
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
        native_search_enabled: false,
        disallowed_tools: Default::default(),
        memory_enabled: false,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 4,
        run_id: None,
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 1,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn events_text(ws: &std::path::Path, run_id: &str) -> String {
    std::fs::read_to_string(
        ws.join(".myagenthubs/runs")
            .join(run_id)
            .join("events.jsonl"),
    )
    .unwrap()
}

#[tokio::test]
async fn oversized_whole_write_rejected_at_pregate() {
    let ws = tempfile::tempdir().unwrap();
    let big = ws.path().join("big.rs");
    std::fs::write(&big, vec![b'a'; 70 * 1024]).unwrap();
    let provider = OneFsWriteThenStop {
        args: serde_json::json!({ "path": "big.rs", "content": "fn main() {}\n" }).to_string(),
    };
    let res = myagent::orchestrator::run_solo(provider, opts_for(ws.path(), "oversized write"))
        .await
        .unwrap();
    let j = events_text(ws.path(), &res.run_id);
    assert!(
        j.contains("fs_write refused"),
        "expected size-gate rejection, got:\n{j}"
    );
    assert!(j.contains("fs_edit"), "rejection should steer to fs_edit");
    assert_eq!(
        std::fs::metadata(&big).unwrap().len(),
        70 * 1024,
        "big file must be unchanged"
    );
}

#[tokio::test]
async fn truncated_fs_write_args_get_friendly_message() {
    let ws = tempfile::tempdir().unwrap();
    let truncated = format!("{{\"path\":\"x.rs\",\"content\":\"{}", "y".repeat(400));
    let provider = OneFsWriteThenStop { args: truncated };
    let res = myagent::orchestrator::run_solo(provider, opts_for(ws.path(), "truncated write"))
        .await
        .unwrap();
    let j = events_text(ws.path(), &res.run_id);
    assert!(
        j.contains("cut off"),
        "expected friendly truncation msg, got:\n{j}"
    );
    assert!(
        j.contains("fs_edit"),
        "truncation msg should steer to fs_edit"
    );
    assert!(
        !j.contains("invalid path or arguments"),
        "should not be the opaque message"
    );
}
