#![cfg(test)]

use super::*;

struct ApprovalOnlyQueue {
    queue: QueueControlSource,
    stop_requested: bool,
}

impl ControlSource for ApprovalOnlyQueue {
    fn poll(&mut self) -> Option<ControlCommand> {
        self.stop_requested.then(|| ControlCommand::Stop {
            run_id: "run_test".into(),
        })
    }

    fn recv_approval(&mut self, timeout: Duration) -> crate::control::ControlRecv {
        if self.stop_requested {
            crate::control::ControlRecv::Timeout
        } else {
            match self.queue.recv_approval(timeout) {
                crate::control::ControlRecv::Command(ControlCommand::Stop { .. }) => {
                    self.stop_requested = true;
                    crate::control::ControlRecv::Timeout
                }
                received => received,
            }
        }
    }
}

async fn run_gate_calls(
    tool_calls: Vec<ToolCall>,
    registry: ToolRegistry,
    control: &mut dyn ControlSource,
) -> (RunOutcome, Vec<ChatMessage>, Vec<Value>) {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_test");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "exercise tool gates");
    opts.contract_policy = crate::guardrails::ContractPolicy::Ask;
    let mut recorder = EventRecorder::new(
        "run_test",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let provider = StreamInterruptionProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        responses: Arc::new(vec![
            ProviderResponse {
                text: "Calling tools.".into(),
                reasoning: String::new(),
                tool_calls,
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
                interruption: None,
            },
            final_text_response("done"),
        ]),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };
    let outcome = run_loop_with_registry(
        registry,
        provider,
        opts,
        paths.clone(),
        "run_test",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        control,
    )
    .await
    .unwrap();
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    let events = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (outcome, messages, events)
}

fn criterion_call() -> ToolCall {
    test_tool_call(
        "call_criterion",
        "propose_criterion",
        json!({ "claim": "command succeeds", "check_cmd": "true" }),
    )
}

#[tokio::test]
async fn propose_criterion_contract_gate_interrupted() {
    let mut control = ApprovalOnlyQueue {
        queue: QueueControlSource::new(vec![ControlCommand::Stop {
            run_id: "run_test".into(),
        }]),
        stop_requested: false,
    };
    let (outcome, messages, events) =
        run_gate_calls(vec![criterion_call()], ToolRegistry::new(), &mut control).await;
    assert_eq!(outcome, RunOutcome::Interrupted);
    assert!(events.iter().any(|event| {
        event["type"] == "run.interrupted" && event["payload"]["step_id"] == "contract.gate"
    }));
    assert!(messages.iter().any(|message| {
        message.tool_call_id.as_deref() == Some("call_criterion")
            && message.content.as_deref() == Some("interrupted before execution")
    }));
}

#[tokio::test]
async fn propose_criterion_user_rejected_preserves_feedback() {
    let call = criterion_call();
    let proposal_id = format!("proposal_{}", call.id);
    let mut control = ApprovalOnlyQueue {
        queue: QueueControlSource::new(vec![ControlCommand::Reject {
            run_id: "run_test".into(),
            approval_id: format!("approval_{proposal_id}"),
        }]),
        stop_requested: false,
    };
    let (outcome, messages, events) =
        run_gate_calls(vec![call], ToolRegistry::new(), &mut control).await;
    assert_ne!(outcome, RunOutcome::Interrupted);
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_criterion")
            && message.content.as_deref() == Some("criterion rejected")
    }));
    assert!(events.iter().any(|event| {
        event["type"] == "goal.change.rejected" && event["payload"]["reason"] == "user_rejected"
    }));
}

struct CountingSearchBackend(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl crate::tools::search::SearchBackend for CountingSearchBackend {
    async fn search(
        &self,
        query: &str,
        _count: usize,
    ) -> std::result::Result<
        Vec<crate::tools::search::SearchResult>,
        crate::tools::search::SearchError,
    > {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![crate::tools::search::SearchResult {
            title: query.into(),
            url: "https://example.com/result".into(),
            snippet: "Search succeeded.".into(),
        }])
    }
}

#[tokio::test]
async fn network_tool_gate_refuses_sixth_search_in_one_turn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(
        crate::tools::web_search::WebSearchTool::with_backend(Box::new(CountingSearchBackend(
            calls.clone(),
        ))),
    ));
    let tool_calls = (1..=6)
        .map(|index| {
            test_tool_call(
                &format!("search_{index}"),
                "web_search",
                json!({ "query": format!("query {index}") }),
            )
        })
        .collect();
    let mut control = QueueControlSource::new(Vec::new());
    let (_, messages, _) = run_gate_calls(tool_calls, registry, &mut control).await;
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    for index in 1..=6 {
        let id = format!("search_{index}");
        let results: Vec<_> = messages
            .iter()
            .filter(|message| message.tool_call_id.as_deref() == Some(&id))
            .collect();
        assert_eq!(results.len(), 1);
        let content = results[0].content.as_deref().unwrap();
        if index == 6 {
            assert!(content.contains("per-turn search limit reached"));
        } else {
            let result: Value = serde_json::from_str(content).unwrap();
            assert_eq!(result["query"], format!("query {index}"));
            assert_eq!(result["results"][0]["snippet"], "Search succeeded.");
        }
    }
}

#[derive(Clone)]
struct ShellEditAfterProbeProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ShellEditAfterProbeProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let tool_calls = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![test_tool_call(
                "register-shell-edit-probe",
                "register_issue_probe",
                json!({
                    "script": "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi",
                    "command": "sh {probe}",
                    "red_marker": "BUG_PRESENT",
                    "marker_stream": "stdout",
                    "rationale": "target.txt still contains the buggy value"
                }),
            )],
            1 => vec![test_tool_call(
                "shell-edit-target",
                "shell_exec",
                json!({ "command": "printf 'fixed\n' > target.txt" }),
            )],
            _ => Vec::new(),
        };
        Ok(ProviderResponse {
            text: String::new(),
            reasoning: String::new(),
            finish_reason: Some(if tool_calls.is_empty() {
                crate::provider::FinishReason::Stop
            } else {
                crate::provider::FinishReason::ToolCalls
            }),
            tool_calls,
            interruption: None,
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn shell_edit_workspace_change_triggers_same_turn_immediate_diagnostics() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=AgentLoom Test",
            "-c",
            "user.email=agentloom@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(commit.success());

    let mut criteria = crate::goal::parse_criteria(&["cmd: cargo check".into()]).unwrap();
    criteria[0].approval = crate::goal::Approval::Approved;
    let run_id = "shell-edit-immediate-diagnostics";
    let mut options = task_test_run_options(workspace.path(), journal.path(), run_id, criteria);
    options.evidence_gate = EvidenceGate::On;
    options.max_turns = 4;

    run_solo(
        ShellEditAfterProbeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(journal.path(), run_id).events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let shell_edit_turn = 2;
    assert!(events.iter().any(|event| {
        event["type"] == "tool.started"
            && event["payload"]["tool"] == "diagnostic_probe"
            && event["payload"]["tool_call_id"] == format!("immediate_{shell_edit_turn}")
    }));
    assert!(events.iter().any(|event| {
        event["payload"]["turn"] == shell_edit_turn
            && (event["type"] == "safety_net.checkpoint"
                || event["type"] == "safety_net.checkpoint_skipped")
    }));
}
