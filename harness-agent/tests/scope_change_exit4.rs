use async_trait::async_trait;
use myagent::control::{ControlCommand, ControlSource};
use myagent::events::OutputMode;
use myagent::orchestrator::{
    run_solo_with_control, run_solo_with_judge, ControlInputKind, RunOptions, RunOutcome,
};
use myagent::provider::{
    ChatMessage, FinishReason, FunctionCall, ProviderCapabilities, ProviderClient,
    ProviderResponse, ToolCall,
};
use myagent::shell::PermissionPolicy;
use std::sync::{Arc, Mutex};

fn assert_provider_legal_tool_pairing(messages: &[serde_json::Value]) {
    let mut index = 0usize;
    while index < messages.len() {
        let message = &messages[index];
        let role = message["role"].as_str().unwrap_or("<missing role>");
        assert_ne!(
            role, "tool",
            "tool result at message {index} has no preceding assistant tool_calls"
        );

        let tool_calls = message
            .get("tool_calls")
            .and_then(|tool_calls| tool_calls.as_array())
            .filter(|tool_calls| !tool_calls.is_empty());
        let Some(tool_calls) = tool_calls else {
            index += 1;
            continue;
        };

        assert_eq!(
            role, "assistant",
            "tool_calls at message {index} must belong to assistant"
        );
        for (offset, tool_call) in tool_calls.iter().enumerate() {
            let tool_index = index + 1 + offset;
            let tool_message = messages.get(tool_index).unwrap_or_else(|| {
                panic!(
                    "missing tool result for {} after assistant message {index}",
                    tool_call["id"].as_str().unwrap_or("<missing id>")
                )
            });
            assert_eq!(
                tool_message["role"], "tool",
                "message {tool_index} must be an adjacent tool result"
            );
            assert_eq!(
                tool_message["tool_call_id"], tool_call["id"],
                "message {tool_index} tool_call_id must match assistant tool_call"
            );
        }

        index += 1 + tool_calls.len();
    }
}

fn opts(ws: &std::path::Path) -> RunOptions {
    RunOptions {
        prompt: "propose scope change please".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
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
        max_turns: 4,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        run_id: Some("r_scope".into()),
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

struct LiveChannel;

impl ControlSource for LiveChannel {
    fn poll(&mut self) -> Option<ControlCommand> {
        None
    }

    fn approval_channel_available(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn conversation_pairing_scope_change_proposes_then_needs_decision_exit4_when_channel_available(
) {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo_with_control(
        myagent::provider::mock::MockProvider::default(),
        Box::new(myagent::judge::NoopJudge),
        opts(ws.path()),
        Box::new(LiveChannel),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::NeedsDecision);
    assert_eq!(res.outcome.code(), 4);

    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    let types: Vec<String> = journal
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let p = types
        .iter()
        .position(|t| t == "goal.change.proposed")
        .expect("有 proposed");
    let d = types
        .iter()
        .position(|t| t == "run.needs_decision")
        .expect("有 needs_decision");
    assert!(p < d, "proposed 必须在 needs_decision 之前");
    let terminal_idx: Vec<usize> = types
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.starts_with("run.")
                && matches!(
                    t.as_str(),
                    "run.completed"
                        | "run.blocked"
                        | "run.failed"
                        | "run.interrupted"
                        | "run.needs_decision"
                )
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(terminal_idx.len(), 1, "恰一个终态");
    assert_eq!(
        *terminal_idx.last().unwrap(),
        types.len() - 1,
        "终态是最后一条"
    );
    assert_eq!(types[types.len() - 1], "run.needs_decision");

    let conversation = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("conversation.json"),
    )
    .unwrap();
    let saved: serde_json::Value = serde_json::from_str(&conversation).unwrap();
    let messages = saved["messages"].as_array().unwrap();
    assert_provider_legal_tool_pairing(messages);
    let scope_result = messages
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == "call_scope_1");
    let scope_result = scope_result.expect("scope proposal tool result must be persisted");
    let content: serde_json::Value =
        serde_json::from_str(scope_result["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["status"], "needs_decision");
    assert_eq!(content["kind"], "scope");
}

struct ScopeThenFinish;

#[async_trait]
impl ProviderClient for ScopeThenFinish {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        events: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
        if tool_msgs == 0 {
            return Ok(ProviderResponse {
                text: "proposing".into(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_scope_dc".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "propose_scope_change".into(),
                        arguments: serde_json::json!({
                            "kind": "objective",
                            "detail": "widen the objective"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: Some(FinishReason::ToolCalls),
            });
        }
        events.emit_text_delta("done")?;
        Ok(ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "cap".into(),
            model_id: "cap".into(),
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
async fn scope_change_deny_and_continue_when_no_channel() {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo_with_judge(
        ScopeThenFinish,
        Box::new(myagent::judge::NoopJudge),
        opts(ws.path()),
    )
    .await
    .unwrap();

    assert_ne!(res.outcome, RunOutcome::NeedsDecision);

    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    let evs: Vec<serde_json::Value> = journal
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let ty = |v: &serde_json::Value| v["type"].as_str().unwrap_or("").to_string();
    assert!(
        !evs.iter().any(|v| ty(v) == "run.needs_decision"),
        "通道不可用时不得硬停"
    );
    let proposed = evs
        .iter()
        .find(|v| ty(v) == "goal.change.proposed")
        .expect("有 proposed");
    assert_eq!(
        proposed["payload"]["transient"],
        serde_json::json!(true),
        "自动拒的 proposed 必须 transient (C3)"
    );
    let rejected = evs
        .iter()
        .find(|v| ty(v) == "goal.change.rejected")
        .expect("有 rejected");
    assert_eq!(rejected["payload"]["reason"], "approval_unavailable");
    assert!(
        evs.iter().all(|v| v["payload"].get("changes").is_none()),
        "deny-continue 不得把 pending 漏进任何决定事件 (C3)"
    );

    let conversation = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("conversation.json"),
    )
    .unwrap();
    let saved: serde_json::Value = serde_json::from_str(&conversation).unwrap();
    let messages = saved["messages"].as_array().unwrap();
    let tr = messages
        .iter()
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call_scope_dc")
        .expect("scope tool_result persisted");
    assert!(tr["content"]
        .as_str()
        .unwrap()
        .contains("block_with_questions"));
}

struct ToolsCaptor(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl ProviderClient for ToolsCaptor {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[serde_json::Value],
        events: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        // 只记第一轮的工具清单：这条 provider 永远给空文本、从不满足空标准的 Stop 收尾，
        // 会一路跑到预算耗尽——K3 在发终态前多打的那通「收尾电话」故意以空工具集调用
        // provider，若这里每轮都覆写，会被那通空调用把已经拿到的清单冲掉。
        let mut captured = self.0.lock().unwrap();
        if captured.is_empty() {
            *captured = tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(String::from))
                .collect();
        }
        drop(captured);
        events.emit_text_delta("done")?;
        Ok(ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "cap".into(),
            model_id: "cap".into(),
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
async fn scope_tool_injected() {
    let ws = tempfile::tempdir().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _ = run_solo_with_judge(
        ToolsCaptor(captured.clone()),
        Box::new(myagent::judge::NoopJudge),
        opts(ws.path()),
    )
    .await
    .unwrap();
    let names = captured.lock().unwrap().clone();
    assert!(
        names.iter().any(|n| n == "propose_scope_change"),
        "scope 工具必须注入给 provider"
    );
}
