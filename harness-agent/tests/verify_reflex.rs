use std::sync::{Arc, Mutex};

use myagent::events::EventRecorder;
use myagent::goal::parse_criteria;
use myagent::orchestrator::{run_solo, ControlInputKind, RunOptions, RunOutcome};
use myagent::provider::{
    ChatMessage, FunctionCall, ProviderCapabilities, ProviderClient, ProviderResponse, ToolCall,
};
use myagent::shell::PermissionPolicy;
use serde_json::{json, Value};

#[derive(Clone)]
struct VerifyReflexEditProvider {
    edits: usize,
    seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[derive(Clone)]
struct WatchdogRepeatingEditProvider {
    seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for VerifyReflexEditProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        let turn = {
            let mut seen = self.seen_messages.lock().unwrap();
            seen.push(messages.to_vec());
            seen.len()
        };

        if turn == 1 {
            let mut tool_calls = Vec::new();
            // 先读后改（A2）：编辑 demo.txt 前同轮先 fs_read 它，登记进 FileLedger。
            tool_calls.push(ToolCall {
                id: "seed_read".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "fs_read".into(),
                    arguments: json!({ "path": "demo.txt" }).to_string(),
                },
            });
            for idx in 0..self.edits {
                let old_string = if idx == 0 {
                    "start".to_string()
                } else {
                    format!("value-{idx}")
                };
                let new_string = format!("value-{}", idx + 1);
                tool_calls.push(ToolCall {
                    id: format!("edit_{idx}"),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "fs_edit".into(),
                        arguments: json!({
                            "path": "demo.txt",
                            "old_string": old_string,
                            "new_string": new_string,
                        })
                        .to_string(),
                    },
                });
            }
            return Ok(ProviderResponse {
                text: "editing".into(),
                reasoning: String::new(),
                tool_calls,
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "final".into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "test".into(),
            model_id: "verify-reflex".into(),
            supports_streaming: false,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: false,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }
}

#[async_trait::async_trait]
impl ProviderClient for WatchdogRepeatingEditProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        let turn = {
            let mut seen = self.seen_messages.lock().unwrap();
            seen.push(messages.to_vec());
            seen.len()
        };
        let previous = format!("value-{}", turn - 1);
        let next = format!("value-{turn}");

        Ok(ProviderResponse {
            text: format!("editing {turn}"),
            reasoning: String::new(),
            // 先读后改（A2）：每轮编辑 demo.txt 前同轮先 fs_read 它，登记进 FileLedger。
            tool_calls: vec![
                ToolCall {
                    id: format!("read_{turn}"),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "fs_read".into(),
                        arguments: json!({ "path": "demo.txt" }).to_string(),
                    },
                },
                ToolCall {
                    id: format!("edit_{turn}"),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "fs_edit".into(),
                        arguments: json!({
                            "path": "demo.txt",
                            "old_string": previous,
                            "new_string": next,
                        })
                        .to_string(),
                    },
                },
            ],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "test".into(),
            model_id: "watchdog-reflex".into(),
            supports_streaming: false,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: false,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }
}

fn opts(
    ws: &std::path::Path,
    criteria: &[&str],
    verify_reflex_debt: usize,
    run_id: &str,
) -> RunOptions {
    RunOptions {
        prompt: "edit without stopping".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "test".into(),
        model: "verify-reflex".into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        permission: PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 3,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria: parse_criteria(&criteria.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 1,
        verify_reflex_debt,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn run_events(ws: &std::path::Path, run_id: &str) -> String {
    std::fs::read_to_string(
        ws.join(".myagenthubs")
            .join("runs")
            .join(run_id)
            .join("events.jsonl"),
    )
    .unwrap()
}

fn run_event_values(ws: &std::path::Path, run_id: &str) -> Vec<Value> {
    run_events(ws, run_id)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn validation_checked_event(ws: &std::path::Path, run_id: &str) -> Value {
    let events: Vec<_> = run_event_values(ws, run_id)
        .into_iter()
        .filter(|event| event["type"] == "validation.checked")
        .collect();
    assert_eq!(events.len(), 1);
    events.into_iter().next().unwrap()
}

fn validation_messages(seen: &Arc<Mutex<Vec<Vec<ChatMessage>>>>) -> Vec<String> {
    seen.lock().unwrap()[1]
        .iter()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.clone())
        .filter(|content| content.contains("Validation after your recent edits failed"))
        .collect()
}

#[tokio::test]
async fn validation_checked_emitted_for_reflex_failed_check() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "start").unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = VerifyReflexEditProvider {
        edits: 2,
        seen_messages,
    };

    let res = run_solo(
        provider,
        opts(
            ws.path(),
            &["cmd: sh -c 'echo verify-reflex-real-error >&2; exit 5'"],
            2,
            "run_validation_checked_fail",
        ),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::Blocked);
    let event = validation_checked_event(ws.path(), "run_validation_checked_fail");
    let payload = &event["payload"];
    assert_eq!(payload["trigger"], "reflex");
    assert_eq!(payload["debt"], 2);
    assert_eq!(payload["reflex_round"], 1);
    assert_eq!(payload["passed"], false);
    let failed = payload["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0]["cmd"],
        "sh -c 'echo verify-reflex-real-error >&2; exit 5'"
    );
    assert_eq!(failed[0]["exit_code"], 5);
}

#[tokio::test]
async fn validation_checked_emitted_for_reflex_passing_check() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "start").unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = VerifyReflexEditProvider {
        edits: 2,
        seen_messages,
    };

    let res = run_solo(
        provider,
        opts(ws.path(), &["cmd: true"], 2, "run_validation_checked_pass"),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::Completed);
    let event = validation_checked_event(ws.path(), "run_validation_checked_pass");
    let payload = &event["payload"];
    assert_eq!(payload["trigger"], "reflex");
    assert_eq!(payload["debt"], 2);
    assert_eq!(payload["reflex_round"], 1);
    assert_eq!(payload["passed"], true);
    assert!(payload["failed"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn verify_reflex_integration_failing_check_feedback_message() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "start").unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = VerifyReflexEditProvider {
        edits: 2,
        seen_messages: seen_messages.clone(),
    };

    let res = run_solo(
        provider,
        opts(
            ws.path(),
            &["cmd: sh -c 'echo verify-reflex-real-error >&2; exit 1'"],
            2,
            "run_verify_reflex_fail",
        ),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::Blocked);
    let messages = validation_messages(&seen_messages);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("verify-reflex-real-error"));
    assert!(messages[0].contains("(exit 1)"));
    let events = run_events(ws.path(), "run_verify_reflex_fail");
    assert!(events.contains("\"tool_call_id\":\"check_reflex_1_c1\""));
    assert!(events.contains("\"tool\":\"check_cmd\""));
    assert!(events.contains("\"passed\":false"));
}

#[tokio::test]
async fn verify_reflex_integration_passing_check_adds_no_feedback() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "start").unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = VerifyReflexEditProvider {
        edits: 2,
        seen_messages: seen_messages.clone(),
    };

    let res = run_solo(
        provider,
        opts(ws.path(), &["cmd: true"], 2, "run_verify_reflex_pass"),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::Completed);
    assert!(validation_messages(&seen_messages).is_empty());
    let events = run_events(ws.path(), "run_verify_reflex_pass");
    assert!(events.contains("\"tool_call_id\":\"check_reflex_1_c1\""));
    assert!(events.contains("\"passed\":true"));
}

#[tokio::test]
async fn verify_reflex_integration_k_zero_no_reflex() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "start").unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = VerifyReflexEditProvider {
        edits: 2,
        seen_messages: seen_messages.clone(),
    };

    let res = run_solo(
        provider,
        opts(
            ws.path(),
            &["cmd: sh -c 'echo verify-reflex-real-error >&2; exit 1'"],
            0,
            "run_verify_reflex_zero",
        ),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::Blocked);
    assert!(validation_messages(&seen_messages).is_empty());
    let events = run_events(ws.path(), "run_verify_reflex_zero");
    assert!(!events.contains("check_reflex_"));
}

#[tokio::test]
async fn watchdog_stuck_repeating_needs_decision() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "value-0").unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = WatchdogRepeatingEditProvider {
        seen_messages: seen_messages.clone(),
    };
    let mut options = opts(
        ws.path(),
        &["cmd: test -f never"],
        1,
        "run_watchdog_stuck_repeating",
    );
    options.max_turns = 20;
    options.max_eval_attempts = 99;
    options.watchdog_repeat_threshold = 3;

    let res = run_solo(provider, options).await.unwrap();

    assert_eq!(res.outcome, RunOutcome::NeedsDecision);
    assert!(seen_messages.lock().unwrap().len() < 20);
    let events = run_event_values(ws.path(), "run_watchdog_stuck_repeating");
    let needs_decision = events
        .last()
        .expect("last event should be run.needs_decision for watchdog");
    assert_eq!(needs_decision["type"], "run.needs_decision");
    assert_eq!(needs_decision["payload"]["reason"], "blocked_questions");
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "stuck_repeating"
    );
    assert_eq!(needs_decision["payload"]["questions"], json!([]));
    assert_eq!(needs_decision["payload"]["agent_diagnosis"], Value::Null);
    assert_eq!(needs_decision["payload"]["evidence_refs"], json!([]));
    assert_eq!(needs_decision["payload"]["trigger"], "harness");
    assert_eq!(needs_decision["payload"]["attempts_summary"]["turns"], 3);
    assert_eq!(needs_decision["payload"]["repeats"], 3);
    assert_eq!(
        needs_decision["payload"]["signature"],
        "reflex:[[\"test -f never\",\"1\"]]"
    );
    assert!(
        needs_decision["payload"]["attempts_summary"]["turns"]
            .as_u64()
            .unwrap()
            < 20
    );
    let validation_checks = events
        .iter()
        .filter(|event| event["type"] == "validation.checked")
        .count();
    assert_eq!(validation_checks, 3);
}
