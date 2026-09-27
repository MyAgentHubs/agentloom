#![cfg(test)]

use super::*;

struct TwoReadCallsProvider;

#[async_trait::async_trait]
impl ProviderClient for TwoReadCallsProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "Reading two files.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "call_read_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_read".to_string(),
                        arguments: json!({ "path": "first.txt" }).to_string(),
                    },
                },
                ToolCall {
                    id: "call_read_2".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_read".to_string(),
                        arguments: json!({ "path": "second.txt" }).to_string(),
                    },
                },
            ],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("two-read-calls")
    }
}

struct ScopeChangeWithTrailingToolProvider;

#[async_trait::async_trait]
impl ProviderClient for ScopeChangeWithTrailingToolProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "Proposing a scope change and another tool.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                test_tool_call(
                    "call_scope_trailing",
                    "propose_scope_change",
                    json!({
                        "kind": "scope",
                        "detail": "Include the trailing tool pairing case"
                    }),
                ),
                test_tool_call(
                    "call_read_after_scope",
                    "fs_read",
                    json!({ "path": "after_scope.txt" }),
                ),
            ],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("scope-change-with-trailing-tool")
    }
}

struct PairingAssertResumeProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for PairingAssertResumeProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        validate_tool_pairing(messages).expect("resume must repair conversation before provider");
        assert!(
            !messages.iter().any(|message| {
                message.role == "assistant"
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| calls.iter().any(|call| call.id == "legacy_unpaired"))
            }),
            "unpaired legacy assistant must be dropped before resume prompt"
        );
        Ok(ProviderResponse {
            text: "resumed cleanly".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("resume-pairing")
    }
}

#[tokio::test]
async fn conversation_pairing_runtime_fatal_keeps_saved_conversation_provider_legal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_conversation_pairing_runtime_err");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "call bad tool");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_conversation_pairing_runtime_err".into());
    let mut recorder = EventRecorder::new(
        "run_conversation_pairing_runtime_err",
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
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(RuntimeErrTool));

    let result = run_loop_with_registry(
        registry,
        RuntimeErrProvider,
        opts,
        paths.clone(),
        "run_conversation_pairing_runtime_err",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await;

    assert!(result.is_err());
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert!(
        !saved.messages.iter().any(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| call.id == "runtime_err"))
        }),
        "fatal tool turn must not be persisted without a paired tool result"
    );
}

#[tokio::test]
async fn conversation_pairing_multi_tool_interrupt_saves_completed_and_placeholder_results() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("first.txt"), "first content").unwrap();
    std::fs::write(dir.path().join("second.txt"), "second content").unwrap();
    let paths = RunPaths::new(dir.path(), "run_conversation_pairing_interrupt");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "read files");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_conversation_pairing_interrupt".into());
    let mut recorder = EventRecorder::new(
        "run_conversation_pairing_interrupt",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = StopOnPoll {
        run_id: "run_conversation_pairing_interrupt".into(),
        stop_on: 3,
        polls: 0,
    };
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        TwoReadCallsProvider,
        opts,
        paths.clone(),
        "run_conversation_pairing_interrupt",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Interrupted);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_read_1")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("first content"))
    }));
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_read_2")
            && message.content.as_deref() == Some("interrupted before execution")
    }));
}

#[tokio::test]
async fn conversation_pairing_scope_change_with_trailing_tool_call_saves_placeholder_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("after_scope.txt"), "unused").unwrap();
    let paths = RunPaths::new(dir.path(), "run_conversation_pairing_scope_trailing");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "propose scope change");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_conversation_pairing_scope_trailing".into());
    let mut recorder = EventRecorder::new(
        "run_conversation_pairing_scope_trailing",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = LiveChannel;
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        ScopeChangeWithTrailingToolProvider,
        opts,
        paths.clone(),
        "run_conversation_pairing_scope_trailing",
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

    let scope_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_scope_trailing")
    });
    let scope_content: Value =
        serde_json::from_str(scope_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(scope_content["status"], "needs_decision");

    let trailing_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_read_after_scope")
    });
    let trailing_content: Value =
        serde_json::from_str(trailing_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(trailing_content["status"], "skipped");
    assert_eq!(trailing_content["reason"], "superseded by needs_decision");
}

#[tokio::test]
async fn conversation_pairing_resume_repairs_legacy_unpaired_tail_before_provider() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_conversation_pairing_resume";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let bad_messages = vec![
        ChatMessage::system("system"),
        ChatMessage::user("start"),
        ChatMessage::assistant(
            "will call",
            None,
            vec![test_tool_call(
                "legacy_unpaired",
                "fs_read",
                json!({"path":"missing.txt"}),
            )],
        ),
    ];
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-pairing".to_string(),
            messages: bad_messages,
        },
    )
    .unwrap();
    let contract = GoalState::new("start", passing_criteria()).contract;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = resume_solo_with_judge(
        PairingAssertResumeProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        Some("resume now".to_string()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        2,
        ControlInputKind::Sentinel,
        true,
        true,
        crate::config::SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert!(
        !saved.messages.iter().any(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| call.id == "legacy_unpaired"))
        }),
        "repaired journal must not keep the legacy unpaired assistant"
    );
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"provider.warning\""));
    assert!(events.contains("\"warning\":\"conversation_repaired\""));
    assert!(events.contains("\"dropped_messages\":1"));
}
