#![cfg(test)]

use super::*;

#[tokio::test]
async fn budget_exhausted_after_real_edits_reports_still_progressing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "v0").unwrap();
    let run_id = "run_budget_exhausted_still_progressing";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();

    let mut opts = options(dir.path().to_path_buf(), "keep editing");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 5;
    opts.max_eval_attempts = 99;
    opts.run_id = Some(run_id.to_string());

    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));
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
        EditingProvider {
            calls: calls.clone(),
            offered_tools,
            edits_before_final: 99,
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
    // K3: grants one extra wrap-up response before budget exhaustion, so the provider is called once more than max_turns(5).
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "budget_exhausted_still_progressing"
    );
    assert_eq!(needs_decision["payload"]["attempts_summary"]["turns"], 5);
    assert!(!events.iter().any(|event| event["type"] == "run.failed"));
}

#[tokio::test]
async fn budget_exhausted_read_only_run_reports_no_progress_before_absolute_hard() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..5 {
        std::fs::write(
            dir.path().join(format!("read_{index}.txt")),
            format!("content {index}"),
        )
        .unwrap();
    }
    let run_id = "run_no_progress_hard_boundary";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "pure reader boundary");
    opts.permission = PermissionPolicy::Allow;
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
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        PureReaderProvider {
            calls,
            offered_tools,
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
        .expect("run should emit no_progress run.needs_decision");
    assert_eq!(needs_decision["payload"]["blocked_reason"], "no_progress");
    assert_eq!(needs_decision["payload"]["contract_version"], 1);
    assert_eq!(needs_decision["payload"]["questions"], json!([]));
    assert_eq!(needs_decision["payload"]["evidence_refs"], json!([]));
    assert_eq!(needs_decision["payload"]["agent_diagnosis"], Value::Null);
    assert_eq!(needs_decision["payload"]["trigger"], "harness");
    assert_eq!(needs_decision["payload"]["attempts_summary"]["turns"], 5);
    assert_eq!(needs_decision["payload"]["consecutive_stale_turns"], 0);
    assert_eq!(needs_decision["payload"]["turns_since_last_real_edit"], 5);
    assert!(
        needs_decision["payload"]["consecutive_read_only_turns"].is_null(),
        "旧计数器字段不该再出现在事件 payload"
    );
    assert!(
        !events.iter().any(|event| {
            event["type"] == "run.failed" && event["payload"]["error"] == "max_turns_exceeded"
        }),
        "hard no-progress boundary must not fall through to max_turns_exceeded"
    );
}

/// K3 probe: rather than precomputing which invocation is the wrap-up turn from a threshold (fragile), identify it by K3's own behavioral signature.
/// The wrap-up turn always calls the provider with an empty tool set (forcing the model to produce text only). When it matches: (1) it still attempts a tool call
/// (to verify that it is ignored and not executed); (2) it provides identifiable wrap-up text (to verify that it lands in the final conversation).
/// Every other turn keeps rereading the same file, creating genuine stagnation to trigger the stale halt.
struct StaleHaltWrapupProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for StaleHaltWrapupProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if tools.is_empty() {
            return Ok(ProviderResponse {
                text: "Wrap-up: kept re-reading the same file with no new information, made no edits. Recommend narrowing scope next time.".to_string(),
                reasoning: String::new(),
                // K3 promises that tool calls in the wrap-up turn are not executed; deliberately try one here for the test assertion to verify.
                tool_calls: vec![test_tool_call("call_ignored", "fs_read", json!({ "path": "const.txt" }))],
                finish_reason: None,
                interruption: None,
            });
        }
        Ok(ProviderResponse {
            text: "Reading the same file again.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_{call}"),
                "fs_read",
                json!({ "path": "const.txt" }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("stale-halt-wrapup-probe")
    }
}

#[tokio::test]
async fn halt_wrapup_turn_offered_once_lands_final_text_and_ignores_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("const.txt"), "constant").unwrap();
    let run_id = "run_halt_wrapup";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "halt wrapup probe");
    opts.permission = PermissionPolicy::Allow;
    // Leave enough budget to ensure that the stale halt (eight genuinely stagnant turns), rather than budget exhaustion, triggers first.
    opts.max_turns = 30;
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
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop(
        StaleHaltWrapupProvider {
            calls: calls.clone(),
        },
        opts.clone(),
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
    // Exactly one wrap-up turn: once an empty-tool-set invocation occurs, the run terminates immediately—there is no second one.
    assert!(
        calls.load(Ordering::SeqCst) < opts.max_turns,
        "halt 必须早于 max_turns 触发，否则这条测试没测到 halt 路径本身"
    );

    // final_text is persisted: the wrap-up turn's text appears as the final assistant message in the saved conversation.
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    let last_assistant = saved
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .expect("K3 收尾轮应留下一条 assistant 消息");
    assert!(
        last_assistant
            .content
            .as_deref()
            .unwrap_or_default()
            .contains("Wrap-up"),
        "最后一条 assistant 消息应是收尾文案：{last_assistant:?}"
    );
    // R2: when wrap-up text is truly persisted, the nudge must land adjacent to the assistant response as a pair in canonical messages
    // (the nudge immediately precedes the wrap-up response, rather than being stranded somewhere in the middle).
    assert_eq!(
        saved.messages.last().map(|m| m.role.as_str()),
        Some("assistant"),
        "对话最后一条必须是收尾回复本身"
    );
    let second_to_last = &saved.messages[saved.messages.len() - 2];
    assert_eq!(second_to_last.role, "user");
    assert_eq!(
        second_to_last.content.as_deref(),
        Some(crate::orchestrator::HALT_WRAPUP_NUDGE),
        "收尾回复前一条必须正是 nudge 本身"
    );
    // Even if the wrap-up turn attempts a tool call, it is not executed—there must be no corresponding tool-result message.
    assert!(
        !saved.messages.iter().any(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("call_ignored")
        }),
        "K3 收尾轮的工具调用必须被忽略、不执行"
    );

    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"blocked_reason\":\"no_progress\""));
    assert!(events.contains("\"step_id\":\"solo.wrapup\""));
    assert!(events.contains("\"outcome\":\"wrapup_given\""));
}

/// K3 negative probe: even if the model is completely uncooperative during the wrap-up turn (responding only with whitespace), the run must still terminate unconditionally—no retry,
/// no hang, and no extra second turn.
struct UncooperativeWrapupProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for UncooperativeWrapupProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if tools.is_empty() {
            // Wrap-up turn: respond only with whitespace and say nothing useful.
            return Ok(ProviderResponse {
                text: "   ".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            });
        }
        Ok(ProviderResponse {
            text: format!("attempt {call}"),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("uncooperative-wrapup-probe")
    }
}

#[tokio::test]
async fn budget_exhausted_wrapup_terminates_even_when_model_gives_empty_response() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "uncooperative wrapup");
    opts.max_turns = 3;
    opts.max_eval_attempts = 99;
    // An always-unsatisfied criterion forces this run to end only through budget exhaustion (rather than coincidentally being judged complete on a turn).
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".into()]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        UncooperativeWrapupProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    // Three normal turns plus exactly one wrap-up turn: an uncooperative model does not earn a second chance.
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events
        .iter()
        .any(|event| event["type"] == "orchestration.step.started"
            && event["payload"]["step_id"] == "solo.wrapup"));
    // The model returned only whitespace, so the wrap-up turn must not insert an empty-text assistant message into the conversation.
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(
        !saved
            .messages
            .last()
            .is_some_and(|message| message.role == "assistant"
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()),
        "空白收尾文本不该落进对话：{:?}",
        saved.messages.last()
    );
    // R2: when the model is uncooperative (whitespace response), the nudge itself must never be persisted alone—there cannot be a nudge without its paired
    // response stranded anywhere in the conversation (not merely as the final message), otherwise a resumed run would be misled by this dangling
    // "Do not call any more tools" instruction.
    assert!(
        !saved
            .messages
            .iter()
            .any(|message| message.content.as_deref()
                == Some(crate::orchestrator::HALT_WRAPUP_NUDGE)),
        "收尾不配合时不该留下任何一条悬空 nudge：{:?}",
        saved.messages
    );
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    // Nothing was ever edited (the criterion is always false and the provider never issues tool calls), so blocked_reason should be no_progress,
    // not still_progressing (K3's wrap-up turn itself does not count as an edit and must not skew this determination).
    assert_eq!(needs_decision["payload"]["blocked_reason"], "no_progress");
}

struct TextToolCallWrapupProvider;

#[async_trait::async_trait]
impl ProviderClient for TextToolCallWrapupProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let text = if tools.is_empty() {
            "<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"fs_write\">ignored"
        } else {
            "Still working."
        };
        Ok(ProviderResponse {
            text: text.to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("text-tool-call-wrapup-probe")
    }
}

#[tokio::test]
async fn budget_exhausted_wrapup_hides_text_tool_call_and_records_detection() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "text tool call wrapup");
    opts.max_turns = 1;
    opts.max_eval_attempts = 99;
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".into()]).unwrap();

    let result = run_solo_with_judge(
        TextToolCallWrapupProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    let wrapup = saved
        .messages
        .last()
        .and_then(|message| message.content.as_deref())
        .expect("收尾轮应落下一条可读说明");
    assert!(!wrapup.contains("DSML"));
    assert_eq!(wrapup, crate::orchestrator::TEXT_TOOL_CALL_HIDDEN_NOTICE);

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let completed = events
        .iter()
        .find(|event| {
            event["type"] == "orchestration.step.completed"
                && event["payload"]["step_id"] == "solo.wrapup"
        })
        .expect("收尾轮应发 completed 事件");
    assert_eq!(completed["payload"]["text_tool_call_detected"], true);
}
