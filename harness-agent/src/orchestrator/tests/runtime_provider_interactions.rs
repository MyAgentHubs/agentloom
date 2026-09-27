#![cfg(test)]

use super::*;

struct RejectScriptProvider;

#[async_trait::async_trait]
impl ProviderClient for RejectScriptProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        if messages.iter().any(|message| {
            message.role == "tool"
                && message.tool_call_id.as_deref() == Some("call_reject_write")
                && message.content.as_deref() == Some("permission denied by user")
        }) {
            return Ok(ProviderResponse {
                text: "Continuing without the denied write.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "I will write the requested file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_reject_write".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "fs_write".to_string(),
                    arguments: json!({ "path": "demo.txt", "content": "VALUE=1\n" }).to_string(),
                },
            }],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "test".to_string(),
            model_id: "reject-script".to_string(),
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

struct BlockWithQuestionsProvider;

#[async_trait::async_trait]
impl ProviderClient for BlockWithQuestionsProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "I am blocked and need user input.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                "call_block_questions",
                "block_with_questions",
                json!({
                    "blocked_reason": "criterion c1 looks wrong",
                    "questions": ["Should c1 still be required?", "Can I use a fixture instead?"],
                    "agent_diagnosis": "criteria",
                    "failed_criteria": ["c1"],
                    "evidence_refs": ["events:12"]
                }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("block-with-questions")
    }
}

struct BlockWithQuestionsTrailingToolProvider;

#[async_trait::async_trait]
impl ProviderClient for BlockWithQuestionsTrailingToolProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "I am blocked and also asked for a read.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                test_tool_call(
                    "call_block_trailing",
                    "block_with_questions",
                    json!({
                        "blocked_reason": "missing user decision",
                        "questions": ["Which path should I take?"]
                    }),
                ),
                test_tool_call(
                    "call_read_after_block",
                    "fs_read",
                    json!({ "path": "after_block.txt" }),
                ),
            ],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("block-with-questions-trailing-tool")
    }
}

struct UpdateWorkingStateProvider {
    calls: Arc<AtomicUsize>,
    seen_systems: Arc<Mutex<Vec<String>>>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for UpdateWorkingStateProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        self.seen_systems.lock().unwrap().push(
            messages
                .first()
                .and_then(|message| message.content.clone())
                .unwrap_or_default(),
        );
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Updating my working notes, then editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![
                    test_tool_call(
                        "call_update_ledger",
                        "update_working_state",
                        json!({
                            "plan": "do X",
                            "known": ["target.txt exists"],
                            "unknown": ["whether final text is enough"],
                            "next_intent": "edit target.txt"
                        }),
                    ),
                    test_tool_call(
                        "call_write_after_ledger",
                        "fs_write",
                        json!({ "path": "target.txt", "content": "updated\n" }),
                    ),
                ],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Finished after using the working notes.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("update-working-state")
    }
}

struct DisallowedProposeCriterionThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for DisallowedProposeCriterionThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Hard-calling a disabled criterion tool.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_disallowed_criterion",
                    "propose_criterion",
                    json!({ "claim": "new criterion", "check_cmd": "true" }),
                )],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after disabled tool rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("disallowed-propose-criterion-then-final")
    }
}

#[tokio::test]
async fn rejected_mutating_tool_is_reported_to_model_and_run_continues() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_test");
    paths.create_dirs().unwrap();
    let opts = options(dir.path().to_path_buf(), "agentic loop");
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
    let mut control = QueueControlSource::new(vec![ControlCommand::Reject {
        run_id: "run_test".into(),
        approval_id: "approval_call_reject_write".into(),
    }]);
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let result = run_loop(
        RejectScriptProvider,
        opts.clone(),
        paths.clone(),
        "run_test",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await;

    assert!(result.is_ok());
    assert!(!dir.path().join("demo.txt").exists());
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_reject_write")
            && message.content.as_deref() == Some("permission denied by user")
    }));
    assert!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count()
            >= 2
    );

    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"tool.failed\""));
    assert!(events.contains("\"tool_call_id\":\"call_reject_write\""));
    assert!(!events.contains("\"type\":\"artifact.created\""));
}

#[tokio::test]
async fn disallowed_inline_propose_criterion_is_rejected_without_decision_and_run_continues() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try to negotiate criteria");
    opts.disallowed_tools
        .insert("propose_criterion".to_string());
    opts.max_turns = 2;
    opts.criteria = passing_criteria();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        DisallowedProposeCriterionThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"type\":\"goal.change.proposed\""));
    assert!(!events.contains("\"type\":\"run.needs_decision\""));
    assert!(events.contains("\"type\":\"tool.failed\""));
    assert!(events.contains("propose_criterion"));
    assert!(events.contains("disabled for this run"));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_disallowed_criterion")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("disabled for this run"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn block_with_questions_escalates_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_block_with_questions";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "need user input");
    opts.max_turns = 5;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(
        opts.prompt.clone(),
        crate::goal::parse_criteria(&["judge: c1 must hold".to_string()]).unwrap(),
    );
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        BlockWithQuestionsProvider,
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| {
            event["type"] == "run.needs_decision"
                && event["payload"]["reason"] == "blocked_questions"
        })
        .expect("run should emit blocked_questions needs_decision");
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "criterion c1 looks wrong"
    );
    assert_eq!(
        needs_decision["payload"]["questions"],
        json!([
            "Should c1 still be required?",
            "Can I use a fixture instead?"
        ])
    );
    assert_eq!(needs_decision["payload"]["contract_version"], 1);
    assert_eq!(needs_decision["payload"]["trigger"], "agent");
    assert_eq!(needs_decision["payload"]["agent_diagnosis"], "criteria");
    assert_eq!(needs_decision["payload"]["failed_criteria"], json!(["c1"]));
    assert_eq!(
        needs_decision["payload"]["evidence_refs"],
        json!(["events:12"])
    );
    assert_eq!(needs_decision["payload"]["attempts_summary"]["turns"], 1);
    assert!(!events.iter().any(|event| {
        event["type"] == "run.failed" && event["payload"]["error"] == "max_turns_exceeded"
    }));
}

#[tokio::test]
async fn block_with_questions_skips_trailing_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("after_block.txt"), "unused").unwrap();
    let run_id = "run_block_with_questions_trailing";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "blocked before read");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        BlockWithQuestionsTrailingToolProvider,
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);

    let block_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_block_trailing")
    });
    let block_content: Value =
        serde_json::from_str(block_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(block_content["status"], "blocked_questions");

    let trailing_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_read_after_block")
    });
    let trailing_content: Value =
        serde_json::from_str(trailing_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(trailing_content["status"], "skipped");
    assert_eq!(
        trailing_content["reason"],
        "superseded by blocked_questions"
    );
}

#[tokio::test]
async fn update_working_state_persists_and_feeds_frame() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_update_working_state";
    let paths = RunPaths::new(dir.path(), run_id);
    let mut opts = options(dir.path().to_path_buf(), "track working notes");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 2;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_systems = Arc::new(Mutex::new(Vec::new()));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let result = run_solo_with_judge(
        UpdateWorkingStateProvider {
            calls: calls.clone(),
            seen_systems: seen_systems.clone(),
            offered_tools: offered_tools.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let offered = offered_tools.lock().unwrap();
    assert!(offered[0].iter().any(|name| name == "update_working_state"));

    let ledger = crate::journal::load_working_ledger(&paths.working_ledger_path);
    assert_eq!(ledger.plan.as_deref(), Some("do X"));
    assert_eq!(ledger.next_intent.as_deref(), Some("edit target.txt"));
    assert_eq!(ledger.known, vec!["target.txt exists"]);
    assert_eq!(ledger.unknown, vec!["whether final text is enough"]);
    assert!(ledger.applied.contains("call_update_ledger"));

    let seen = seen_systems.lock().unwrap();
    assert!(seen.len() >= 2);
    assert!(seen[1].contains("Your working notes"));
    assert!(seen[1].contains("plan: do X"));
    assert!(seen[1].contains("next: edit target.txt"));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
    assert!(!saved.messages[0]
        .content
        .as_deref()
        .unwrap_or_default()
        .contains("Your working notes"));
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_write_after_ledger")
    }));
    let update_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_update_ledger")
    });
    let update_content: Value =
        serde_json::from_str(update_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(update_content["status"], "updated");
}
