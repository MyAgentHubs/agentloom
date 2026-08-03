use async_trait::async_trait;
use myagent::control::{ControlCommand, ControlRecv, ControlSource};
use myagent::events::OutputMode;
use myagent::orchestrator::{
    run_solo_with_control, run_solo_with_judge, ControlInputKind, RunOptions, RunOutcome,
};
use myagent::provider::{
    ChatMessage, FunctionCall, ProviderCapabilities, ProviderClient, ProviderResponse, ToolCall,
};
use myagent::shell::PermissionPolicy;
use std::cell::Cell;
use std::time::Duration;

/// 每轮都提一个 mutating fs_write（固定 tool_call id "call_probe"），永不收尾。
struct AlwaysWriteProvider;
#[async_trait]
impl ProviderClient for AlwaysWriteProvider {
    async fn next_turn(
        &self,
        _m: &[ChatMessage],
        _t: &[serde_json::Value],
        events: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        events.emit_text_delta("propose write")?;
        Ok(ProviderResponse {
            text: "propose write".into(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_probe".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "fs_write".into(),
                    arguments: serde_json::json!({"path": "out.txt", "content": "x\n"}).to_string(),
                },
            }],
            finish_reason: None,
        })
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "aw".into(),
            model_id: "aw".into(),
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

/// 按脚本回审批决定，approval_id 固定匹配 "approval_call_probe"。
struct ScriptedApprovals {
    decisions: Vec<bool>,
    idx: Cell<usize>,
    run_id: String,
}
impl ControlSource for ScriptedApprovals {
    fn poll(&mut self) -> Option<ControlCommand> {
        None
    }
    fn recv_approval(&mut self, _t: Duration) -> ControlRecv {
        let i = self.idx.get();
        if i >= self.decisions.len() {
            return ControlRecv::Closed;
        }
        self.idx.set(i + 1);
        let approve = self.decisions[i];
        let cmd = if approve {
            ControlCommand::Approve {
                run_id: self.run_id.clone(),
                approval_id: "approval_call_probe".into(),
            }
        } else {
            ControlCommand::Reject {
                run_id: self.run_id.clone(),
                approval_id: "approval_call_probe".into(),
            }
        };
        ControlRecv::Command(cmd)
    }
}

fn base_opts(
    ws: &std::path::Path,
    permission: PermissionPolicy,
    max_turns: usize,
    run_id: Option<String>,
) -> RunOptions {
    RunOptions {
        prompt: "loop".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "aw".into(),
        model: "aw".into(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        permission,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: myagent::config::SearchChoice::Ddg,
        max_turns,
        max_eval_attempts: 99,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        run_id,
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

#[tokio::test]
async fn three_consecutive_user_rejections_self_stop_blocked() {
    let ws = tempfile::tempdir().unwrap();
    let opts = base_opts(ws.path(), PermissionPolicy::Deny, 8, None);
    let res = run_solo_with_judge(
        AlwaysWriteProvider,
        Box::new(myagent::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, RunOutcome::Blocked);
    let journal = std::fs::read_to_string(
        ws.path()
            .join(".myagenthubs/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(
        journal.contains("\"reason\":\"rejected_repeatedly\""),
        "应发 rejected_repeatedly 终态"
    );
}

#[tokio::test]
async fn successful_tool_resets_consecutive_rejection_counter() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "r_reset".to_string();
    // 拒/拒/批/拒/拒 —— 若批准后不归零，则第4次连拒触发 rejected_repeatedly（强对照）；正确归零则绝不自停。
    let control = ScriptedApprovals {
        decisions: vec![false, false, true, false, false],
        idx: Cell::new(0),
        run_id: run_id.clone(),
    };
    let opts = base_opts(ws.path(), PermissionPolicy::Ask, 5, Some(run_id.clone()));
    let res = run_solo_with_control(
        AlwaysWriteProvider,
        Box::new(myagent::judge::NoopJudge),
        opts,
        Box::new(control),
    )
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
        !journal.contains("rejected_repeatedly"),
        "中途成功工具应归零、不得自停"
    );
    assert!(
        journal.contains("\"type\":\"tool.completed\"") && journal.contains("\"bytes\":"),
        "批准轮 fs_write 须真成功执行"
    );
}

#[tokio::test]
async fn run_result_carries_always_used_false_on_normal_run() {
    let ws = tempfile::tempdir().unwrap();
    let opts = base_opts(ws.path(), PermissionPolicy::Allow, 2, None);
    let res = run_solo_with_judge(
        AlwaysWriteProvider,
        Box::new(myagent::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();
    assert!(!res.always_used, "未按 a，always_used 应为 false");
}
