use async_trait::async_trait;
use myagent::control::{ControlCommand, ControlRecv, ControlSource};
use myagent::events::OutputMode;
use myagent::orchestrator::{
    run_solo_with_control, run_solo_with_judge, ControlInputKind, RunOptions, RunOutcome,
};
use myagent::shell::PermissionPolicy;
use std::cell::Cell;
use std::time::Duration;

struct ApproveContract {
    run_id: String,
    used: Cell<bool>,
}
impl ControlSource for ApproveContract {
    fn poll(&mut self) -> Option<ControlCommand> {
        None
    }
    fn recv_approval(&mut self, _t: Duration) -> ControlRecv {
        if self.used.get() {
            return ControlRecv::Closed;
        }
        self.used.set(true);
        ControlRecv::Command(ControlCommand::Approve {
            run_id: self.run_id.clone(),
            approval_id: "approval_proposal_call_crit_1".into(),
        })
    }
}

fn opts_with_prompt(ws: &std::path::Path, run_id: &str, prompt: &str) -> RunOptions {
    RunOptions {
        prompt: prompt.into(),
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
        max_turns: 5,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn opts(ws: &std::path::Path, run_id: &str) -> RunOptions {
    opts_with_prompt(ws, run_id, "propose criterion then finish")
}

#[tokio::test]
async fn agent_criterion_not_run_until_approved_then_completes() {
    let ws = tempfile::tempdir().unwrap();
    let control = ApproveContract {
        run_id: "r_crit".into(),
        used: Cell::new(false),
    };
    let res = run_solo_with_control(
        myagent::provider::mock::MockProvider::default(),
        Box::new(myagent::judge::NoopJudge),
        opts(ws.path(), "r_crit"),
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
    let lines: Vec<serde_json::Value> = journal
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let ty = |v: &serde_json::Value| v["type"].as_str().unwrap().to_string();

    let pos = |pred: &dyn Fn(&serde_json::Value) -> bool, what: &str| {
        lines
            .iter()
            .position(pred)
            .unwrap_or_else(|| panic!("missing {what}"))
    };
    let req = pos(
        &|v| ty(v) == "approval.requested" && v["payload"]["request_kind"] == "criterion",
        "approval.requested(criterion)",
    );
    let res_ = pos(&|v| ty(v) == "approval.resolved", "approval.resolved");
    let app = pos(&|v| ty(v) == "goal.change.approved", "goal.change.approved");
    let upd = pos(&|v| ty(v) == "goal.updated", "goal.updated");
    let check = pos(
        &|v| ty(v) == "tool.started" && v["payload"]["tool"] == "check_cmd",
        "check_cmd tool.started",
    );
    assert!(
        req < res_ && res_ < app && app < upd && upd < check,
        "order must be requested<resolved<approved<updated<check_cmd"
    );

    assert_eq!(res.outcome, RunOutcome::Completed);
}

struct ApproveDualGate {
    run_id: String,
    step: Cell<usize>,
}
impl ControlSource for ApproveDualGate {
    fn poll(&mut self) -> Option<ControlCommand> {
        None
    }
    fn recv_approval(&mut self, _t: Duration) -> ControlRecv {
        let approval_id = match self.step.get() {
            0 => "approval_proposal_call_dual_crit_1",
            1 => "approval_call_dual_shell_1",
            _ => return ControlRecv::Closed,
        };
        self.step.set(self.step.get() + 1);
        ControlRecv::Command(ControlCommand::Approve {
            run_id: self.run_id.clone(),
            approval_id: approval_id.into(),
        })
    }
}

#[tokio::test]
async fn criterion_approval_does_not_satisfy_later_tool_gate() {
    let ws = tempfile::tempdir().unwrap();
    let control = ApproveDualGate {
        run_id: "r_dual".into(),
        step: Cell::new(0),
    };
    let res = run_solo_with_control(
        myagent::provider::mock::MockProvider::default(),
        Box::new(myagent::judge::NoopJudge),
        opts_with_prompt(ws.path(), "r_dual", "criterion then tool"),
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
    let lines: Vec<serde_json::Value> = journal
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let approval_requests: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|v| v["type"] == "approval.requested")
        .collect();

    assert!(
        approval_requests.iter().any(|v| {
            v["payload"]["approval_id"] == "approval_proposal_call_dual_crit_1"
                && v["payload"]["request_kind"] == "criterion"
        }),
        "criterion proposal must request contract approval"
    );
    assert!(
        approval_requests.iter().any(|v| {
            v["payload"]["approval_id"] == "approval_call_dual_shell_1"
                && v["payload"]["tool"] == "shell_exec"
                && v["payload"].get("request_kind").is_none()
        }),
        "mutating shell_exec must still request tool approval after criterion approval"
    );
    assert_eq!(res.outcome, RunOutcome::Completed);
}

struct RejectContract {
    run_id: String,
    used: Cell<bool>,
}
impl ControlSource for RejectContract {
    fn poll(&mut self) -> Option<ControlCommand> {
        None
    }
    fn recv_approval(&mut self, _t: Duration) -> ControlRecv {
        if self.used.get() {
            return ControlRecv::Closed;
        }
        self.used.set(true);
        ControlRecv::Command(ControlCommand::Reject {
            run_id: self.run_id.clone(),
            approval_id: "approval_proposal_call_crit_1".into(),
        })
    }
}

#[tokio::test]
async fn rejected_criterion_never_runs_check_cmd() {
    let ws = tempfile::tempdir().unwrap();
    let control = RejectContract {
        run_id: "r_rej".into(),
        used: Cell::new(false),
    };
    let res = run_solo_with_control(
        myagent::provider::mock::MockProvider::default(),
        Box::new(myagent::judge::NoopJudge),
        opts(ws.path(), "r_rej"),
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
        journal.contains("\"type\":\"goal.change.rejected\""),
        "must emit goal.change.rejected"
    );
    assert!(
        !journal.contains("\"tool\":\"check_cmd\""),
        "check_cmd must never run after rejection"
    );
}

#[tokio::test]
async fn criterion_unavailable_channel_gives_guidance_and_continues() {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo_with_judge(
        myagent::provider::mock::MockProvider::default(),
        Box::new(myagent::judge::NoopJudge),
        opts(ws.path(), "r_crit_dc"),
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
    assert!(journal.contains("\"type\":\"goal.change.rejected\""));
    assert!(journal.contains("approval_unavailable"));
    assert!(
        !journal.contains("\"tool\":\"check_cmd\""),
        "未批准的 criterion 不得跑 check_cmd"
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
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call_crit_1")
        .expect("criterion tool_result persisted");
    assert!(tr["content"]
        .as_str()
        .unwrap()
        .contains("block_with_questions"));
}

struct ToolsCaptor(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
#[async_trait]
impl myagent::provider::ProviderClient for ToolsCaptor {
    async fn next_turn(
        &self,
        _m: &[myagent::provider::ChatMessage],
        tools: &[serde_json::Value],
        ev: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<myagent::provider::ProviderResponse> {
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
        ev.emit_text_delta("done")?;
        Ok(myagent::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: None,
        })
    }
    fn capabilities(&self) -> myagent::provider::ProviderCapabilities {
        myagent::provider::ProviderCapabilities {
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
async fn both_governance_tools_injected() {
    let ws = tempfile::tempdir().unwrap();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let _ = run_solo_with_judge(
        ToolsCaptor(captured.clone()),
        Box::new(myagent::judge::NoopJudge),
        opts(ws.path(), "r_tools"),
    )
    .await
    .unwrap();
    let names = captured.lock().unwrap().clone();
    assert!(
        names.iter().any(|n| n == "propose_scope_change"),
        "scope tool must be injected"
    );
    assert!(
        names.iter().any(|n| n == "propose_criterion"),
        "criterion tool must be injected"
    );
}
