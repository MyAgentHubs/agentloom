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

struct GiantResultProvider {
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    turn: Mutex<usize>,
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

#[async_trait::async_trait]
impl ProviderClient for GiantResultProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        self.seen.lock().unwrap().push(messages.to_vec());
        let mut turn = self.turn.lock().unwrap();
        *turn += 1;

        if *turn == 1 {
            return Ok(ProviderResponse {
                text: String::new(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "giant-read".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "fs_read".into(),
                        arguments: serde_json::json!({ "path": "big.txt" }).to_string(),
                    },
                }],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
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

#[tokio::test]
async fn giant_single_turn_tool_result_does_not_exhaust_budget() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("big.txt"), "x".repeat(64 * 1024)).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = GiantResultProvider {
        seen: seen.clone(),
        turn: Mutex::new(0),
    };

    let mut options = opts(ws.path(), "do work", &["cmd: false"]);
    options.max_eval_attempts = 2;
    let result = run_solo(provider, options)
        .await
        .expect("run_solo should not error");

    let events_path = ws
        .path()
        .join(".myagenthubs/runs")
        .join(&result.run_id)
        .join("events.jsonl");
    let events: Vec<Value> = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let context_exhaustion = events.iter().find(|event| {
        event["type"] == "run.needs_decision"
            && event["payload"]["reason"] == "context_budget_exhausted"
    });
    assert!(
        context_exhaustion.is_none(),
        "run should not exhaust the context budget: {}",
        context_exhaustion.unwrap()
    );

    let wires = seen.lock().unwrap();
    assert!(
        wires.len() >= 3,
        "expected the run to continue after the giant tool result, got {} provider turns",
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

    let middle_was_elided = wires.iter().any(|wire| {
        wire.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("bytes elided from the middle"))
        })
    });
    assert!(
        middle_was_elided,
        "giant tool result should be middle-elided"
    );

    let last = wires.last().unwrap();
    assert_eq!(last[0].role, "system");
    let system = last[0].content.as_deref().unwrap_or("");
    assert!(system.contains("Objective"));
    assert!(system.contains("Acceptance criteria"));
}
