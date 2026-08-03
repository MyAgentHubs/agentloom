use std::sync::{Arc, Mutex};

use myagent::events::EventRecorder;
use myagent::goal::parse_criteria;
use myagent::judge::{FixedJudge, JudgeDecision};
use myagent::orchestrator::{
    run_solo, run_solo_with_judge, ControlInputKind, RunOptions, RunOutcome,
};
use myagent::provider::{
    ChatMessage, FunctionCall, ProviderCapabilities, ProviderClient, ProviderResponse, ToolCall,
};
use myagent::shell::PermissionPolicy;
use serde_json::{json, Value};

fn test_caps(model: &str) -> ProviderCapabilities {
    ProviderCapabilities {
        provider_id: "test".into(),
        model_id: model.into(),
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

/// 永不停手：每轮只读同一个文件（无新进展、tool_calls 永不为空）。
#[derive(Clone)]
struct NeverStopsProvider {
    turns: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl ProviderClient for NeverStopsProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        let n = {
            let mut t = self.turns.lock().unwrap();
            *t += 1;
            *t
        };
        Ok(ProviderResponse {
            text: "still working".into(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("read_{n}"),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "fs_read".into(),
                    arguments: json!({ "path": "demo.txt" }).to_string(),
                },
            }],
            finish_reason: None,
        })
    }
    fn capabilities(&self) -> ProviderCapabilities {
        test_caps("never-stops")
    }
}

/// 一轮就停手（Path A）：返回纯文本、无工具调用。
#[derive(Clone)]
struct StopsImmediatelyProvider;

#[async_trait::async_trait]
impl ProviderClient for StopsImmediatelyProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }
    fn capabilities(&self) -> ProviderCapabilities {
        test_caps("stops")
    }
}

fn opts(ws: &std::path::Path, criteria: &[&str], run_id: &str) -> RunOptions {
    RunOptions {
        prompt: "model-agnostic completion".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "test".into(),
        model: "model-agnostic".into(),
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
        max_turns: 20,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria: parse_criteria(&criteria.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 99,
        // 高到「单次读」永远攒不够 debt → 中途验证(reflex)不触发 → 只能靠 no_progress 兜底
        verify_reflex_debt: 5,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn events(ws: &std::path::Path, run_id: &str) -> Vec<Value> {
    std::fs::read_to_string(
        ws.join(".myagenthubs")
            .join("runs")
            .join(run_id)
            .join("events.jsonl"),
    )
    .unwrap()
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect()
}

#[tokio::test]
async fn backstop_finalizes_when_model_never_stops_and_criteria_pass() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "hello").unwrap();
    let provider = NeverStopsProvider {
        turns: Arc::new(Mutex::new(0)),
    };

    let res = run_solo(
        provider,
        opts(ws.path(), &["cmd: true"], "run_backstop_pass"),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::Completed);
    let evs = events(ws.path(), "run_backstop_pass");
    // 走兜底：中途验证从未触发
    assert!(!evs.iter().any(|e| e["type"] == "validation.checked"));
    let completed = evs.iter().find(|e| e["type"] == "run.completed").unwrap();
    assert_eq!(completed["payload"]["via"], "engine_finalize");
}

#[tokio::test]
async fn backstop_fail_closed_on_failing_cmd() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "hello").unwrap();
    let provider = NeverStopsProvider {
        turns: Arc::new(Mutex::new(0)),
    };

    let res = run_solo(
        provider,
        opts(ws.path(), &["cmd: sh -c 'exit 1'"], "run_backstop_failcmd"),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::NeedsDecision);
    assert!(!events(ws.path(), "run_backstop_failcmd")
        .iter()
        .any(|e| e["type"] == "run.completed"));
}

#[tokio::test]
async fn backstop_fail_closed_on_failing_judgmental() {
    // 中途绿≠最终绿：verifiable cmd 过，但判断题失败 → 完整 evaluate 在兜底处挡下
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "hello").unwrap();
    let provider = NeverStopsProvider {
        turns: Arc::new(Mutex::new(0)),
    };
    let judge = Box::new(FixedJudge {
        decision: JudgeDecision::Fail,
    });

    let res = run_solo_with_judge(
        provider,
        judge,
        opts(
            ws.path(),
            &["cmd: true", "judge: 必须完美"],
            "run_backstop_failjudge",
        ),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::NeedsDecision);
    assert!(!events(ws.path(), "run_backstop_failjudge")
        .iter()
        .any(|e| e["type"] == "run.completed"));
}

#[tokio::test]
async fn path_a_completion_has_no_via_field() {
    // 模型停手 → Path A → run.completed payload 仍无 via（保 Path A 零回归）
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "hello").unwrap();

    let res = run_solo(
        StopsImmediatelyProvider,
        opts(ws.path(), &["cmd: true"], "run_path_a_no_via"),
    )
    .await
    .unwrap();

    assert_eq!(res.outcome, RunOutcome::Completed);
    let evs = events(ws.path(), "run_path_a_no_via");
    let completed = evs.iter().find(|e| e["type"] == "run.completed").unwrap();
    assert!(completed["payload"].get("via").is_none());
}

#[tokio::test]
async fn no_criteria_run_is_not_vacuously_completed() {
    // fail-closed：合同无任何验收标准时，兜底不得「真空完成」(decide_outcome 对空标准
    // 集 all() 真空为真)——应照旧 no_progress → NeedsDecision。
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "hello").unwrap();
    let provider = NeverStopsProvider {
        turns: Arc::new(Mutex::new(0)),
    };

    let res = run_solo(provider, opts(ws.path(), &[], "run_no_criteria"))
        .await
        .unwrap();

    assert_eq!(res.outcome, RunOutcome::NeedsDecision);
    assert!(!events(ws.path(), "run_no_criteria")
        .iter()
        .any(|e| e["type"] == "run.completed"));
}

/// 先改一次（触发中途验证全过 → 武装），之后每轮只读（不停手、不再编辑）。
#[derive(Clone)]
struct EditThenReadProvider {
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for EditThenReadProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        let turn = {
            let mut s = self.seen.lock().unwrap();
            s.push(messages.to_vec());
            s.len()
        };
        if turn == 1 {
            return Ok(ProviderResponse {
                text: "editing".into(),
                reasoning: String::new(),
                tool_calls: vec![
                    ToolCall {
                        id: "seed_read".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "fs_read".into(),
                            arguments: json!({ "path": "demo.txt" }).to_string(),
                        },
                    },
                    ToolCall {
                        id: "edit_0".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "fs_edit".into(),
                            arguments: json!({
                                "path": "demo.txt",
                                "old_string": "start",
                                "new_string": "done",
                            })
                            .to_string(),
                        },
                    },
                ],
                finish_reason: None,
            });
        }
        Ok(ProviderResponse {
            text: "still poking".into(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("read_{turn}"),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "fs_read".into(),
                    arguments: json!({ "path": "demo.txt" }).to_string(),
                },
            }],
            finish_reason: None,
        })
    }
    fn capabilities(&self) -> ProviderCapabilities {
        test_caps("edit-then-read")
    }
}

#[tokio::test]
async fn fast_path_finalizes_one_turn_after_green_without_stopping() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("demo.txt"), "start").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = EditThenReadProvider { seen: seen.clone() };

    // verify_reflex_debt=1：1 次编辑即触发中途验证 → 全过 → 武装。
    let mut o = opts(ws.path(), &["cmd: true"], "run_fast_path");
    o.verify_reflex_debt = 1;

    let res = run_solo(provider, o).await.unwrap();

    assert_eq!(res.outcome, RunOutcome::Completed);
    let evs = events(ws.path(), "run_fast_path");
    let completed_idx = evs
        .iter()
        .position(|e| e["type"] == "run.completed")
        .unwrap();
    assert_eq!(evs[completed_idx]["payload"]["via"], "engine_finalize");
    // 提前收尾：第 2 轮就完成（远早于 no_progress 边界）
    assert_eq!(evs[completed_idx]["payload"]["turns"], 2);
    // 第 2 轮输入里出现收尾提示
    let turn2_inputs = &seen.lock().unwrap()[1];
    assert!(turn2_inputs.iter().any(|m| m
        .content
        .as_deref()
        .is_some_and(|c| c.contains("验收检查已通过"))));
    // 事件顺序：validation.checked 与 completion.evaluated 都在 run.completed 之前（NICE 1）
    let vc = evs.iter().position(|e| e["type"] == "validation.checked");
    let ce = evs
        .iter()
        .rposition(|e| e["type"] == "completion.evaluated");
    assert!(vc.is_some() && vc.unwrap() < completed_idx);
    assert!(ce.is_some() && ce.unwrap() < completed_idx);
}
