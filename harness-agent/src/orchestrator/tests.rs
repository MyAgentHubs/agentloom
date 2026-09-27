use super::*;
use crate::control::{ControlCommand, ControlSource, QueueControlSource};
use crate::journal::{load_conversation, save_conversation, SavedConversation};
use crate::provider::pairing::validate_tool_pairing;
use crate::provider::{FunctionCall, ProviderResponse, ToolCall};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

struct LiveChannel;

impl ControlSource for LiveChannel {
    fn poll(&mut self) -> Option<ControlCommand> {
        None
    }

    fn approval_channel_available(&self) -> bool {
        true
    }
}

fn task_test_caps() -> crate::provider::ProviderCapabilities {
    crate::provider::ProviderCapabilities {
        provider_id: "mock".into(),
        model_id: "mock".into(),
        supports_streaming: false,
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

mod fixtures;
use fixtures::task_test_run_options;

fn evidence_tool_recorder(dir: &std::path::Path, run_id: &str) -> EventRecorder {
    EventRecorder::new(
        run_id,
        None,
        None,
        &dir.join("events.jsonl"),
        crate::events::OutputMode::Silent,
    )
    .unwrap()
}

fn init_git_index(workspace: &std::path::Path, paths: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(workspace)
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("git")
        .arg("add")
        .arg("--")
        .args(paths)
        .current_dir(workspace)
        .status()
        .unwrap();
    assert!(status.success());
}

async fn evidence_edit_register_probe(
    script: &str,
    workspace: &std::path::Path,
    journal: &std::path::Path,
    evidence: &mut EvidenceState,
    recorder: &mut EventRecorder,
) {
    let mut attempts = 0;
    let feedback = register_issue_probe_call(
        &json!({
            "script": script,
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "exercise the evidence edit lifecycle"
        })
        .to_string(),
        evidence,
        &mut attempts,
        workspace,
        &journal.join("probes"),
        1,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        recorder,
    )
    .await
    .unwrap();
    assert!(feedback.contains("Probe confirmed RED"), "{feedback}");
    assert!(evidence.probe.is_some());
}

fn passing_criteria() -> Vec<crate::goal::Criterion> {
    crate::goal::parse_criteria(&["cmd: true".into()]).unwrap()
}

fn assert_each_assistant_tool_call_has_exactly_one_tool_result(messages: &[ChatMessage]) {
    let mut tool_call_ids = Vec::new();
    for message in messages
        .iter()
        .filter(|message| message.role == "assistant")
    {
        if let Some(tool_calls) = &message.tool_calls {
            for tool_call in tool_calls {
                tool_call_ids.push(tool_call.id.as_str());
            }
        }
    }
    assert!(
        !tool_call_ids.is_empty(),
        "test must include assistant tool calls"
    );

    for tool_call_id in tool_call_ids {
        let matching_tool_results = messages
            .iter()
            .filter(|message| {
                message.role == "tool" && message.tool_call_id.as_deref() == Some(tool_call_id)
            })
            .count();
        assert_eq!(
            matching_tool_results, 1,
            "tool_call_id {tool_call_id} must have exactly one tool result"
        );
    }
}

struct RuntimeErrProvider;

#[async_trait::async_trait]
impl ProviderClient for RuntimeErrProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        assert!(
            !messages.iter().any(|message| message.role == "tool"),
            "runtime Err from execute must not be converted into a tool message"
        );
        Ok(ProviderResponse {
            text: "Calling the runtime-failing tool.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "runtime_err".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "runtime_err_tool".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("runtime-err")
    }
}

struct StopOnPoll {
    run_id: String,
    stop_on: usize,
    polls: usize,
}

impl ControlSource for StopOnPoll {
    fn poll(&mut self) -> Option<ControlCommand> {
        self.polls += 1;
        if self.polls == self.stop_on {
            Some(ControlCommand::Stop {
                run_id: self.run_id.clone(),
            })
        } else {
            None
        }
    }

    fn recv_approval(&mut self, _timeout: Duration) -> crate::control::ControlRecv {
        crate::control::ControlRecv::Closed
    }
}

struct CompleteImmediatelyProvider;

#[async_trait::async_trait]
impl ProviderClient for CompleteImmediatelyProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "Finished.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("complete-immediately")
    }
}

struct StateFrameCaptorProvider {
    seen_messages: Arc<Mutex<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl ProviderClient for StateFrameCaptorProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        *self.seen_messages.lock().unwrap() = messages.to_vec();
        Ok(ProviderResponse {
            text: "Finished.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("state-frame-captor")
    }
}

struct PureReaderProvider {
    calls: Arc<AtomicUsize>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for PureReaderProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "Reading another file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_read_{call}"),
                "fs_read",
                json!({ "path": format!("read_{call}.txt") }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("pure-reader")
    }
}

struct EditingProvider {
    calls: Arc<AtomicUsize>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
    edits_before_final: usize,
}

#[async_trait::async_trait]
impl ProviderClient for EditingProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= self.edits_before_final {
            return Ok(ProviderResponse {
                text: "Finished after concrete edits.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Editing the target file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                test_tool_call(
                    &format!("call_read_{call}"),
                    "fs_read",
                    json!({"path": "target.txt"}),
                ),
                test_tool_call(
                    &format!("call_edit_{call}"),
                    "fs_edit",
                    json!({
                        "path": "target.txt",
                        "old_string": format!("v{call}"),
                        "new_string": format!("v{}", call + 1),
                    }),
                ),
            ],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("editing-run")
    }
}

struct RepeatShellProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for RepeatShellProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "repeat same red command".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("repeat_shell_{call}"),
                "shell_exec",
                json!({ "command": "printf 'red\\n'; exit 7" }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("repeat-shell")
    }
}

struct RuntimeErrTool;

#[async_trait::async_trait]
impl crate::tools::Tool for RuntimeErrTool {
    fn name(&self) -> &str {
        "runtime_err_tool"
    }

    fn definition(&self) -> Value {
        json!({ "type": "function", "function": { "name": "runtime_err_tool" } })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        _ctx: &mut crate::tools::ToolContext<'_>,
        _call: &ToolCall,
    ) -> Result<crate::tools::ToolOutcome> {
        Err(HarnessError::Runtime("runtime err from tool".to_string()))
    }
}

fn test_tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

fn tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str().map(ToString::to_string))
        .collect()
}

fn messages_contain(messages: &[ChatMessage], needle: &str) -> bool {
    messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains(needle))
    })
}

fn test_capabilities(model_id: &str) -> ProviderCapabilities {
    ProviderCapabilities {
        provider_id: "test-local".to_string(),
        model_id: model_id.to_string(),
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

fn options(workspace: PathBuf, prompt: &str) -> RunOptions {
    RunOptions {
        prompt: prompt.to_string(),
        workspace: workspace.clone(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        evidence_gate: EvidenceGate::Off,
        permission: PermissionPolicy::Ask,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        extra_read_roots: Vec::new(),
        fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: crate::config::SearchChoice::Ddg,
        max_turns: 3,
        run_id: Some("run_test".into()),
        context_files: Vec::new(),
        criteria: Vec::new(),
        contract_policy: crate::guardrails::ContractPolicy::TrustAll,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        journal_root: workspace.clone(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
        images: Vec::new(),
    }
}

/// 制造 `WorkspaceChange::Unverifiable`：先靠一次真实编辑让 evidence probe 转绿，
/// 再 `chmod 000` 目标文件让 git 没法再算出内容指纹——精确复用既有
/// `evidence_unverifiable_workspace_invalidates_green_not_keeps_it` 的手法。
#[derive(Clone)]
struct WorkspaceUnverifiableSafetyCounterProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for WorkspaceUnverifiableSafetyCounterProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let tool_calls = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                evidence_completion_register_call(
                    "register-safety-counter-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "fix-safety-counter-target".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ],
            1 => vec![ToolCall {
                id: "break-git-fingerprint-safety-counter".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: json!({ "command": "chmod 000 target.txt" }).to_string(),
                },
            }],
            _ => Vec::new(),
        };
        Ok(evidence_completion_response(tool_calls))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("workspace-unverifiable-safety-counters")
    }
}
#[derive(Clone)]
struct FinishReasonProvider {
    calls: Arc<AtomicUsize>,
    first_finish_reason: crate::provider::FinishReason,
    saw_truncation_feedback: Arc<Mutex<bool>>,
    first_tool_call: bool,
}

#[async_trait::async_trait]
impl ProviderClient for FinishReasonProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if messages.iter().any(|message| {
            message.role == "user"
                && message.content.as_deref().is_some_and(|content| {
                    content.contains("输出长度上限截断") && content.contains("直接给出工具调用")
                })
        }) {
            *self.saw_truncation_feedback.lock().unwrap() = true;
        }
        if call == 0 && self.first_tool_call {
            return Ok(ProviderResponse {
                text: String::new(),
                reasoning: "ready to edit".into(),
                tool_calls: vec![ToolCall {
                    id: "write_1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "fs_write".into(),
                        arguments: json!({"path": "finish_reason_tool.txt", "content": "done"})
                            .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
                interruption: None,
            });
        }
        Ok(ProviderResponse {
            text: if call == 0 { "done" } else { "still done" }.into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: if call == 0 {
                Some(self.first_finish_reason.clone())
            } else {
                Some(crate::provider::FinishReason::Stop)
            },
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("finish-reason-test")
    }
}

fn finish_reason_provider(
    finish_reason: crate::provider::FinishReason,
    first_tool_call: bool,
) -> (FinishReasonProvider, Arc<AtomicUsize>, Arc<Mutex<bool>>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_truncation_feedback = Arc::new(Mutex::new(false));
    (
        FinishReasonProvider {
            calls: calls.clone(),
            first_finish_reason: finish_reason,
            saw_truncation_feedback: saw_truncation_feedback.clone(),
            first_tool_call,
        },
        calls,
        saw_truncation_feedback,
    )
}

fn evidence_completion_probe() -> ProbeManifest {
    ProbeManifest {
        probe_id: "completion-probe".into(),
        script_sha256: "completion-hash".into(),
        script: "printf BUG_PRESENT".into(),
        script_path: PathBuf::from("/tmp/agentloom-completion-probe.sh"),
        command: "sh /tmp/agentloom-completion-probe.sh".into(),
        red_oracle: RedOracle {
            marker: "BUG_PRESENT".into(),
            stream: MarkerStream::Any,
        },
        rationale: "completion gate test".into(),
        registered_turn: 1,
    }
}

fn evidence_completion_register_call(id: &str, script: String) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "register_issue_probe".into(),
            arguments: json!({
                "script": script,
                "command": "sh {probe}",
                "red_marker": "BUG_PRESENT",
                "marker_stream": "stdout",
                "rationale": "completion gate reproduction"
            })
            .to_string(),
        },
    }
}

fn evidence_completion_response(tool_calls: Vec<ToolCall>) -> ProviderResponse {
    if tool_calls.is_empty() {
        ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls,
            finish_reason: Some(crate::provider::FinishReason::Stop),
            interruption: None,
        }
    } else {
        ProviderResponse {
            text: "working".into(),
            reasoning: String::new(),
            tool_calls,
            finish_reason: Some(crate::provider::FinishReason::ToolCalls),
            interruption: None,
        }
    }
}

// T2b：主循环消费 `ProviderResponse.interruption`——断流轮（传输层掐断 SSE，非模型交白卷）
// 原地重试、不记空转、连断 3 次以真实错误收场。下面这组 mock 按调用序号回放一份脚本化
// 响应序列，最后一条超出序列长度时重复，方便「连续 N 次都断流」这类用例只需一条元素。
#[derive(Clone)]
struct StreamInterruptionProvider {
    calls: Arc<AtomicUsize>,
    responses: Arc<Vec<ProviderResponse>>,
    seen_message_lens: Arc<Mutex<Vec<usize>>>,
}

#[async_trait::async_trait]
impl ProviderClient for StreamInterruptionProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen_message_lens.lock().unwrap().push(messages.len());
        let idx = call.min(self.responses.len() - 1);
        Ok(self.responses[idx].clone())
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("stream-interruption-test")
    }
}

fn final_text_response(text: &str) -> ProviderResponse {
    ProviderResponse {
        text: text.to_string(),
        reasoning: String::new(),
        tool_calls: Vec::new(),
        finish_reason: Some(crate::provider::FinishReason::Stop),
        interruption: None,
    }
}

/// 永远成功、`invalidates_verification: true` 的假 MCP 工具（`is_mcp() == true`）——
/// 模拟 `McpToolProxy` 成功调用的可观察行为，不需要真起一个 MCP server。
struct FakeMcpMutatingTool {
    name: String,
}

#[async_trait::async_trait]
impl crate::tools::Tool for FakeMcpMutatingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn definition(&self) -> Value {
        json!({ "type": "function", "function": { "name": self.name } })
    }

    fn mutates(&self) -> bool {
        true
    }

    fn is_mcp(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _ctx: &mut crate::tools::ToolContext<'_>,
        _call: &ToolCall,
    ) -> Result<crate::tools::ToolOutcome> {
        Ok(crate::tools::ToolOutcome::success_mutating(
            "ok".to_string(),
        ))
    }
}

/// Calls the same MCP tool with a different set of arguments each turn (the normal lead delegation cadence—doing different work each time).
struct NovelMcpCallProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for NovelMcpCallProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "Calling the MCP tool with new arguments.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_mcp_novel_{call}"),
                "mcp__fake__do_thing",
                json!({ "n": call }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("novel-mcp-call")
    }
}

mod blocked_events;
mod budget_wrapup;
mod compile_feedback;
mod conversation_pairing;
mod evidence_baseline;
mod evidence_completion;
mod evidence_edit;
mod evidence_probe;
mod explore_budget;
mod finish_reason;
mod goal_contract_resume;
mod misc_unit;
mod preflight_and_mcp;
mod preflight_rejections;
mod progress_recovery;
mod runtime_provider_interactions;
mod runtime_provider_validation;
mod scope_change;
mod solo_task_gates;
mod tail;
mod tool_gates;
mod tool_outcome_recovery;
mod turn_progress_stream;
