#![cfg(test)]

use super::*;

struct ToolOutcomeRecoverProvider;

#[async_trait::async_trait]
impl ProviderClient for ToolOutcomeRecoverProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let seed_read_done = messages.iter().any(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("seed_read")
        });
        if !seed_read_done {
            return Ok(ProviderResponse {
                text: "Reading the file before editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "seed_read".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_read".to_string(),
                        arguments: json!({"path": "target.txt"}).to_string(),
                    },
                }],
                finish_reason: None,
                interruption: None,
            });
        }

        if messages.iter().any(|message| {
            message.role == "tool"
                && message.tool_call_id.as_deref() == Some("bad_edit")
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("no match"))
        }) {
            if messages.iter().any(|message| {
                message.role == "tool" && message.tool_call_id.as_deref() == Some("good_edit")
            }) {
                return Ok(ProviderResponse {
                    text: "The file has been corrected.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    interruption: None,
                });
            }

            return Ok(ProviderResponse {
                text: "Retrying with the exact string.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "good_edit".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_edit".to_string(),
                        arguments: json!({
                            "path": "target.txt",
                            "old_string": "alpha beta",
                            "new_string": "alpha gamma"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Trying an edit with the wrong old string.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "bad_edit".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "fs_edit".to_string(),
                    arguments: json!({
                        "path": "target.txt",
                        "old_string": "not present",
                        "new_string": "alpha gamma"
                    })
                    .to_string(),
                },
            }],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("tool-outcome-recover")
    }
}

struct CheckpointFatalWriteThenFollowUpProvider {
    calls: Arc<AtomicUsize>,
    saw_tool_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for CheckpointFatalWriteThenFollowUpProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => {
                assert!(
                    !messages.iter().any(|message| message.role == "tool"),
                    "fatal checkpoint failures must abort before any tool result reaches the model"
                );
                Ok(ProviderResponse {
                    text: "Attempting the write first.".to_string(),
                    reasoning: String::new(),
                    tool_calls: vec![test_tool_call(
                        "call_checkpoint_write",
                        "fs_write",
                        json!({
                            "path": "nested/out.txt",
                            "content": "hello from checkpoint fatal path\n"
                        }),
                    )],
                    finish_reason: None,
                    interruption: None,
                })
            }
            1 => {
                if messages.iter().any(|message| {
                    message.role == "tool"
                        && message.tool_call_id.as_deref() == Some("call_checkpoint_write")
                }) {
                    self.saw_tool_feedback.store(1, Ordering::SeqCst);
                }
                Ok(ProviderResponse {
                    text: "Following up after the write.".to_string(),
                    reasoning: String::new(),
                    tool_calls: vec![test_tool_call(
                        "call_after_checkpoint_failure",
                        "fs_read",
                        json!({ "path": "nested/out.txt" }),
                    )],
                    finish_reason: None,
                    interruption: None,
                })
            }
            _ => panic!("checkpoint fatal provider should stop after the follow-up turn"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("checkpoint-fatal-fs-write")
    }
}

#[tokio::test]
async fn tool_outcome_recoverable_fs_edit_feedback_continues_and_preserves_pairing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "alpha beta").unwrap();
    let paths = RunPaths::new(dir.path(), "run_tool_outcome_recover");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "edit target");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 4;
    opts.run_id = Some("run_tool_outcome_recover".into());
    let mut recorder = EventRecorder::new(
        "run_tool_outcome_recover",
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
        ToolOutcomeRecoverProvider,
        opts,
        paths.clone(),
        "run_tool_outcome_recover",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_ne!(outcome, RunOutcome::Failed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
        "alpha gamma"
    );
    let failed_tool_results: Vec<&ChatMessage> = messages
        .iter()
        .filter(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("bad_edit")
        })
        .collect();
    assert_eq!(failed_tool_results.len(), 1);
    assert!(failed_tool_results[0]
        .content
        .as_deref()
        .unwrap()
        .contains("no match"));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}

#[tokio::test]
async fn tool_outcome_execute_runtime_err_remains_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_tool_outcome_runtime_err");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "call bad tool");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_tool_outcome_runtime_err".into());
    let mut recorder = EventRecorder::new(
        "run_tool_outcome_runtime_err",
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
        "run_tool_outcome_runtime_err",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await;

    assert!(
        matches!(result, Err(HarnessError::Runtime(message)) if message.contains("runtime err from tool"))
    );
    assert!(!messages.iter().any(|message| message.role == "tool"));
}

#[tokio::test]
async fn checkpoint_failure_from_fs_write_fails_run_before_follow_up_turn() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_checkpoint_failure_from_fs_write";
    let mut opts = options(dir.path().to_path_buf(), "try the guarded write");
    opts.permission = PermissionPolicy::Allow;
    opts.memory_enabled = false;
    opts.max_turns = 3;
    opts.run_id = Some(run_id.into());
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_tool_feedback = Arc::new(AtomicUsize::new(0));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "http://127.0.0.1:{}/checkpoint",
        listener.local_addr().unwrap().port()
    );
    drop(listener);

    let result = crate::tools::with_checkpoint_env_override_for_test(
        Some(endpoint),
        Some("secret-token".into()),
        async {
            run_solo_with_judge(
                CheckpointFatalWriteThenFollowUpProvider {
                    calls: calls.clone(),
                    saw_tool_feedback: saw_tool_feedback.clone(),
                },
                Box::new(crate::judge::NoopJudge),
                opts,
            )
            .await
        },
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(saw_tool_feedback.load(Ordering::SeqCst), 0);

    let target = dir.path().join("nested/out.txt");
    let parent = target.parent().unwrap();
    assert!(
        !parent.exists(),
        "checkpoint failure must not create the parent directory"
    );
    assert!(
        !target.exists(),
        "checkpoint failure must not create the target file"
    );

    let paths = RunPaths::new(dir.path(), run_id);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "provider.turn.finished")
            .count(),
        1,
        "fatal checkpoint failure must stop the loop before a second provider turn"
    );
    assert!(events.iter().any(|event| {
        event["type"] == "tool.failed"
            && event["payload"]["tool_call_id"] == "call_checkpoint_write"
            && event["payload"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("checkpoint"))
    }));
    assert!(events.iter().any(|event| {
        event["type"] == "run.failed"
            && event["payload"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("checkpoint"))
    }));
    assert!(!events
        .iter()
        .any(|event| { event["payload"]["tool_call_id"] == "call_after_checkpoint_failure" }));
}
