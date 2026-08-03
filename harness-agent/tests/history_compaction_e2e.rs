use std::sync::{Arc, Mutex};

use myagent::context_budget::{estimate_tokens, BudgetLimits};
use myagent::events::EventRecorder;
use myagent::orchestrator::{run_solo, ControlInputKind, RunOptions};
use myagent::provider::{
    ChatMessage, FunctionCall, ProviderCapabilities, ProviderClient, ProviderResponse, ToolCall,
};
use serde_json::Value;

struct RecordingProvider {
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    turn: Mutex<usize>,
    max_script: usize,
}

fn rec_caps() -> ProviderCapabilities {
    ProviderCapabilities {
        provider_id: "rec".into(),
        model_id: "rec".into(),
        supports_streaming: false,
        supports_reasoning_deltas: false,
        supports_tool_calling: true,
        supports_images: false,
        supports_computer_use: false,
        supports_shell_tool: true,
        max_context_tokens: Some(12_000),
        output_token_limit: Some(500),
        server_side_search: false,
    }
}

#[async_trait::async_trait]
impl ProviderClient for RecordingProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        self.seen.lock().unwrap().push(messages.to_vec());
        let mut turn = self.turn.lock().unwrap();
        *turn += 1;

        if *turn > self.max_script {
            return Ok(ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("c{}", *turn),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: serde_json::json!({
                        "command": "yes x | head -c 8000"
                    })
                    .to_string(),
                },
            }],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        rec_caps()
    }
}

fn opts(ws: &std::path::Path, prompt: &str, criteria: &[&str]) -> RunOptions {
    RunOptions {
        prompt: prompt.into(),
        workspace: ws.to_path_buf(),
        provider_id: "rec".into(),
        model: "rec".into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
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
        max_turns: 12,
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
async fn long_run_compacts_and_keeps_frame() {
    let ws = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        seen: seen.clone(),
        turn: Mutex::new(0),
        max_script: 30,
    };

    let res = run_solo(provider, opts(ws.path(), "do work", &["cmd: false"])).await;

    assert!(res.is_ok());

    let wires = seen.lock().unwrap();
    assert!(
        wires.len() >= 3,
        "expected the run to reach multiple provider turns, got {}",
        wires.len()
    );

    let limits = BudgetLimits::from_capabilities(&rec_caps());
    for (index, wire) in wires.iter().enumerate() {
        let estimated = estimate_tokens(wire, &limits);
        assert!(
            estimated <= limits.budget(),
            "wire {index} estimated {estimated} tokens, above budget {}",
            limits.budget()
        );
    }

    let any_compacted = wires.iter().any(|wire| {
        wire.iter().any(|message| {
            message.content.as_deref().is_some_and(|content| {
                content.contains("elided to save context") || content.contains("earlier turn")
            })
        })
    });
    assert!(any_compacted, "compaction should have engaged");

    let last = wires.last().unwrap();
    assert_eq!(last[0].role, "system");
    let system = last[0].content.as_deref().unwrap_or("");
    assert!(system.contains("Objective"));
    assert!(system.contains("Acceptance criteria"));
}
