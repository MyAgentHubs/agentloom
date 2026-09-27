#![cfg(test)]

use super::*;

#[tokio::test]
async fn provider_turn_finished_is_emitted_for_every_turn_including_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) = finish_reason_provider(crate::provider::FinishReason::Length, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(dir.path().to_path_buf(), "observable provider turns"),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let turns: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "provider.turn.finished")
        .collect();
    assert_eq!(turns.len(), 2);
    assert_eq!(
        turns[0]["payload"],
        json!({
            "turn": 1,
            "finish_reason": "length",
            "text_len": 4,
            "reasoning_len": 0,
            "tool_calls": 0,
        })
    );
    assert_eq!(
        turns[1]["payload"],
        json!({
            "turn": 2,
            "finish_reason": "stop",
            "text_len": 10,
            "reasoning_len": 0,
            "tool_calls": 0,
        })
    );
}

#[derive(Clone)]
struct FinishReasonSequenceProvider {
    calls: Arc<AtomicUsize>,
    finish_reasons: Arc<Vec<crate::provider::FinishReason>>,
}

#[async_trait::async_trait]
impl ProviderClient for FinishReasonSequenceProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: format!("response {call}"),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(
                self.finish_reasons
                    .get(call)
                    .cloned()
                    .unwrap_or(crate::provider::FinishReason::Stop),
            ),
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("finish-reason-sequence-test")
    }
}

fn finish_reason_sequence_provider(
    finish_reasons: Vec<crate::provider::FinishReason>,
) -> (FinishReasonSequenceProvider, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        FinishReasonSequenceProvider {
            calls: calls.clone(),
            finish_reasons: Arc::new(finish_reasons),
        },
        calls,
    )
}

#[tokio::test]
async fn three_consecutive_length_responses_stop_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls) = finish_reason_sequence_provider(vec![
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Length,
    ]);
    let mut opts = options(dir.path().to_path_buf(), "repeated truncation");
    opts.max_turns = 40;
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"reason\":\"consecutive_output_truncation\""));
    assert!(events.contains("\"consecutive_truncated_turns\":3"));
}

#[tokio::test]
async fn normal_response_resets_consecutive_length_counter() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls) = finish_reason_sequence_provider(vec![
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Stop,
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Stop,
    ]);
    let mut opts = options(dir.path().to_path_buf(), "non-consecutive truncation");
    opts.max_turns = 5;
    opts.max_eval_attempts = 99;
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".into()]).unwrap();
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    // K3: criteria are always false, so run_loop grants one extra wrap-up response before the five-turn budget is exhausted; the provider is therefore called once more.
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(!events.contains("consecutive_output_truncation"));
}

#[tokio::test]
async fn repeated_same_shell_result_trips_no_progress_before_budget() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_repeated_shell_no_progress";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();

    let mut opts = options(dir.path().to_path_buf(), "repeat shell");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 16;
    opts.max_eval_attempts = 99;
    opts.run_id = Some(run_id.to_string());

    let calls = Arc::new(AtomicUsize::new(0));
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
        RepeatShellProvider {
            calls: calls.clone(),
        },
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
    assert!(calls.load(Ordering::SeqCst) < 16);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"blocked_reason\":\"no_progress\""));
    assert!(!events.contains("max_turns_exceeded"));
}

#[tokio::test]
async fn final_text_failed_eval_counts_no_progress_and_trips_hard_stop() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_final_text_no_progress";
    let mut opts = options(dir.path().to_path_buf(), "plain failing final text");
    opts.max_turns = 16;
    opts.max_eval_attempts = 99;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap();

    let result = run_solo_with_judge(
        crate::provider::mock::MockProvider::default(),
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    let paths = RunPaths::new(dir.path(), run_id);
    let events = std::fs::read_to_string(paths.events_path).unwrap();
    assert!(events.contains("\"blocked_reason\":\"no_progress\""));
    assert!(!events.contains("max_turns_exceeded"));
}

fn interrupted_response(err_text: &str) -> ProviderResponse {
    ProviderResponse {
        text: String::new(),
        // Common shape of an interrupted-stream turn: reasoning has already produced a long passage, but the stream is cut off—text/tool_calls are empty.
        reasoning: format!("{err_text} 之前模型已经在长推理……").repeat(3),
        tool_calls: Vec::new(),
        finish_reason: None,
        interruption: Some(err_text.to_string()),
    }
}

fn tool_call_response(id: &str, path: &str) -> ProviderResponse {
    ProviderResponse {
        text: String::new(),
        reasoning: String::new(),
        tool_calls: vec![ToolCall {
            id: id.to_string(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "fs_write".into(),
                arguments: json!({"path": path, "content": "ok"}).to_string(),
            },
        }],
        finish_reason: Some(crate::provider::FinishReason::ToolCalls),
        interruption: None,
    }
}

/// a. Recovery after a stream interruption: interrupted turn -> normal tool_call turn -> normal wrap-up; Completed throughout,
/// with exactly one `stream_interrupted_continue`, no `run.needs_decision`,
/// and the interrupted turn does not append a partial assistant message to messages (verified by the message count seen by the subsequent request).
#[tokio::test]
async fn stream_interruption_then_recovery_completes_without_no_progress() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "resume after stream interruption");
    opts.max_turns = 8;
    opts.permission = PermissionPolicy::Allow;

    let calls = Arc::new(AtomicUsize::new(0));
    let seen_lens = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![
        interrupted_response("connection reset by peer"),
        tool_call_response("write_1", "resumed.txt"),
        final_text_response("done"),
    ];
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(responses),
        seen_message_lens: seen_lens.clone(),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let lens = seen_lens.lock().unwrap();
    assert_eq!(
        lens[0], lens[1],
        "断流轮不该把半截 assistant 消息推进 messages——重试请求看到的历史长度须与被打断那轮完全一致"
    );

    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    let interrupted_steps = events
        .lines()
        .filter(|l| {
            l.contains("\"orchestration.step.completed\"")
                && l.contains("\"stream_interrupted_continue\"")
        })
        .count();
    assert_eq!(interrupted_steps, 1);
    assert!(!events.contains("run.needs_decision"));
}

/// b. Three consecutive interruptions: the run ends as Failed, and the error text in the run.failed event includes the underlying error text
/// (proving propagation follows the existing `?` path in entry.rs, rather than fabricating a vague local error).
#[tokio::test]
async fn stream_interruption_three_consecutive_times_fails_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "three consecutive stream interruptions",
    );
    opts.max_turns = 8;

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(vec![interrupted_response("upstream closed the connection")]),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    let failed_line = events
        .lines()
        .find(|l| l.contains("\"run.failed\""))
        .expect("run.failed should be emitted after 3 consecutive stream interruptions");
    assert!(failed_line.contains("upstream closed the connection"));
    assert!(failed_line.contains("stream interrupted"));
}

/// c. Counter reset: interruptions are never consecutive more than once (normal turns are always interspersed) -> it never fails and completes normally.
#[tokio::test]
async fn stream_interruption_counter_resets_between_normal_turns() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "interruptions interleaved with normal turns",
    );
    opts.max_turns = 10;
    opts.permission = PermissionPolicy::Allow;

    let responses = vec![
        interrupted_response("read timeout"),
        tool_call_response("write_1", "progress1.txt"),
        interrupted_response("read timeout again"),
        tool_call_response("write_2", "progress2.txt"),
        interrupted_response("read timeout a third time"),
        final_text_response("wrapped up"),
    ];
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(responses),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 6);
}

/// d. Positive regression pin: a truly blank submission (interruption=None, empty text, empty tool_calls) must still be counted as
/// an idle turn—it must not be accidentally swallowed by the new branch. Related existing coverage: `final_text_failed_eval_counts_no_progress_and_trips_hard_stop`
/// (non-empty text but empty tool_calls, criteria always fail, with MockProvider—whose `interruption` is always None—
/// triggering no_progress all the way through; that test should remain green under this change because `if let Some(..)` does not match
/// None and the original logic is untouched). Add a more directly relevant case here: text is also an empty string.
#[tokio::test]
async fn empty_text_without_interruption_still_counts_as_no_progress() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "truly empty final text, no interruption",
    );
    opts.max_turns = 16;
    opts.max_eval_attempts = 99;
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(vec![ProviderResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(crate::provider::FinishReason::Stop),
            interruption: None,
        }]),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert!(calls.load(Ordering::SeqCst) < 16);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"blocked_reason\":\"no_progress\""));
    assert!(!events.contains("stream_interrupted_continue"));
}

/// e. An interrupted-stream turn does not clear `consecutive_truncations`: after two truncations (consecutive_truncations=2), insert one
/// stream interruption (which would reset the count to 0 if mistaken), then another truncation. If the interruption did not secretly reset it, the third truncation should exactly
/// reach `CONSECUTIVE_TRUNCATION_LIMIT=3` and trigger halt; if the interruption mistakenly resets it, it only reaches 1 here and does not halt.
#[tokio::test]
async fn stream_interruption_does_not_reset_truncation_counter() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "truncation counter survives an interleaved interruption",
    );
    opts.max_turns = 10;

    let truncated = ProviderResponse {
        text: String::new(),
        reasoning: "thinking forever".into(),
        tool_calls: Vec::new(),
        finish_reason: Some(crate::provider::FinishReason::Length),
        interruption: None,
    };
    let responses = vec![
        truncated.clone(),
        truncated.clone(),
        interrupted_response("brief network hiccup"),
        truncated,
    ];
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(responses),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"consecutive_output_truncation\""));
    assert!(events.contains("\"consecutive_truncated_turns\":3"));
}

/// f. (T2c Opus review M-1) Narrow interrupted-stream handling to cases where `finish_reason` is also absent: some providers / proxies may dirty-close the connection without a terminator frame after
/// sending a semantically complete response (complete tool_calls and `finish_reason` received). The transport layer also marks such a turn with `interruption: Some`,
/// but it must not be discarded and retried as an interrupted stream. Create a turn with
/// `finish_reason: Some(ToolCalls)` + a complete tool_call + `interruption: Some(..)`, then
/// assert it is handled as a normal turn (the tool executes normally and the run advances normally), with no
/// `stream_interrupted_continue` in the journal.
#[tokio::test]
async fn interruption_with_finish_reason_present_is_not_treated_as_stream_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "dirty close after a semantically complete turn",
    );
    opts.max_turns = 8;
    opts.permission = PermissionPolicy::Allow;

    let complete_but_dirty_close = ProviderResponse {
        text: String::new(),
        reasoning: String::new(),
        tool_calls: vec![ToolCall {
            id: "write_1".to_string(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "fs_write".into(),
                arguments: json!({"path": "resumed.txt", "content": "ok"}).to_string(),
            },
        }],
        // Key point: finish_reason was received—the turn's content is semantically complete, not an interrupted stream.
        finish_reason: Some(crate::provider::FinishReason::ToolCalls),
        // But the transport layer still marked interruption (the proxy dirty-closes after sending content without a terminator frame).
        interruption: Some("upstream closed without a clean terminator".to_string()),
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(vec![complete_but_dirty_close, final_text_response("done")]),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "完整轮不该被当断流原地重试——只应正常推进到下一轮"
    );
    assert!(
        dir.path().join("resumed.txt").exists(),
        "tool_call 应正常执行，不能被断流分支吞掉"
    );

    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(
        !events.contains("stream_interrupted_continue"),
        "finish_reason 已收到的完整轮不该被记成 stream_interrupted_continue；journal:\n{events}"
    );
    assert!(!events.contains("run.failed"));
}

/// g. (Add placeholder tool result in the truncation branch; reproduction pin) A truncated response can still carry fully parsed tool_calls—
/// once an assistant(tool_calls) message is appended to messages, any subsequent non-tool message (a feedback user message /
/// needs_decision snapshot) violates the pairing invariant. `validate_tool_pairing` in
/// `save_conversation_snapshot` reports "conversation pairing invalid ... non-tool message before
/// result", and the whole run crashes directly with `Err`. The interrupted-stream branch must first use `append_unpaired_tool_results` to add a
/// placeholder tool result, then append feedback / save the snapshot. Assert here that the run does not crash, wraps up normally to Completed, and in the persisted
/// conversation every assistant tool_call is immediately followed by exactly one paired tool result.
#[tokio::test]
async fn output_truncation_with_tool_calls_gets_placeholder_before_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "truncated turn carries tool_calls",
    );
    opts.max_turns = 8;

    let truncated_with_tool_call = ProviderResponse {
        text: String::new(),
        reasoning: "thinking forever".repeat(3),
        tool_calls: vec![ToolCall {
            id: "trunc_call_1".to_string(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "fs_write".into(),
                arguments: json!({"path": "truncated.txt", "content": "partial"}).to_string(),
            },
        }],
        finish_reason: Some(crate::provider::FinishReason::Length),
        interruption: None,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(vec![truncated_with_tool_call, final_text_response("done")]),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("output_truncated_continue"));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);

    let assistant_idx = saved
        .messages
        .iter()
        .position(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
        })
        .expect("truncated assistant tool_calls message should be present");
    let placeholder = &saved.messages[assistant_idx + 1];
    assert_eq!(placeholder.role, "tool");
    assert_eq!(placeholder.tool_call_id.as_deref(), Some("trunc_call_1"));
    assert_eq!(
        placeholder.content.as_deref(),
        Some("output truncated before tool results")
    );
    // The truncated-feedback user message immediately follows the placeholder tool result.
    assert_eq!(saved.messages[assistant_idx + 2].role, "user");
}

/// h. (Add placeholder tool result in the truncation branch; hit consecutive-limit path) Three consecutive turns of "truncation + complete tool_calls" should normally
/// reach `output_truncated_halt` / `run.needs_decision` and save the snapshot—it must not crash with `Err` while saving the snapshot
/// because pairing validation fails.
#[tokio::test]
async fn output_truncation_halt_path_with_tool_calls_does_not_crash_pairing() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "repeated truncation carries tool_calls",
    );
    opts.max_turns = 40;

    fn truncated_with_tool_call(id: &str) -> ProviderResponse {
        ProviderResponse {
            text: String::new(),
            reasoning: "thinking forever".repeat(3),
            tool_calls: vec![ToolCall {
                id: id.to_string(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "fs_write".into(),
                    arguments: json!({"path": "truncated.txt", "content": "partial"}).to_string(),
                },
            }],
            finish_reason: Some(crate::provider::FinishReason::Length),
            interruption: None,
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = StreamInterruptionProvider {
        calls: calls.clone(),
        responses: Arc::new(vec![
            truncated_with_tool_call("trunc_call_1"),
            truncated_with_tool_call("trunc_call_2"),
            truncated_with_tool_call("trunc_call_3"),
        ]),
        seen_message_lens: Arc::new(Mutex::new(Vec::new())),
    };

    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"reason\":\"consecutive_output_truncation\""));
    assert!(events.contains("\"consecutive_truncated_turns\":3"));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}
