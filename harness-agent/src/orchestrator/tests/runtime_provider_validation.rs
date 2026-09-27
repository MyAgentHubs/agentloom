#![cfg(test)]

use super::*;

struct ProposeCriterionObjectSuccessThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeCriterionObjectSuccessThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a criterion with object success.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_object_success_criterion",
                    "propose_criterion",
                    json!({
                        "claim": "new criterion",
                        "check_cmd": "true",
                        "success": { "exit_zero": true }
                    }),
                )],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after object success criterion.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-criterion-object-success-then-final")
    }
}

struct ProposeCriterionMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeCriterionMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a criterion with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "propose_criterion".to_string(),
                        arguments: r#"{"claim": "bad criterion", "check_cmd": "tru"#.to_string(),
                    },
                }],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-criterion-malformed-args-then-final")
    }
}

struct UpdateWorkingStateMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for UpdateWorkingStateMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Updating working state with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "update_working_state".to_string(),
                        arguments: r#"{"summary": "work in progress"#.to_string(),
                    },
                }],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("update-working-state-malformed-args-then-final")
    }
}

struct BlockWithQuestionsMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for BlockWithQuestionsMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Blocking with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "block_with_questions".to_string(),
                        arguments: r#"{"blocked_reason": "stuck", "questions": ["#.to_string(),
                    },
                }],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("block-with-questions-malformed-args-then-final")
    }
}

struct ProposeScopeChangeMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeScopeChangeMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a scope change with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "propose_scope_change".to_string(),
                        arguments: r#"{"kind": "scope", "detail": "expand"#.to_string(),
                    },
                }],
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-scope-change-malformed-args-then-final")
    }
}

#[tokio::test]
async fn propose_criterion_object_success_form_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try object success criterion");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        ProposeCriterionObjectSuccessThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"goal.change.proposed\""));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn propose_criterion_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed args criterion");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        ProposeCriterionMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // Neither turn can meet the empty criterion's "Stop wrap-up" condition, so the budget is exhausted; K3 grants one extra wrap-up response,
    // therefore the provider is called once more (3 = 2 normal turns + 1 wrap-up turn).
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    // The malformed call should have a tool rejection message
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn update_working_state_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed working state");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        UpdateWorkingStateMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // Neither turn can meet the empty criterion's "Stop wrap-up" condition, so the budget is exhausted; K3 grants one extra wrap-up response,
    // therefore the provider is called once more (3 = 2 normal turns + 1 wrap-up turn).
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}
#[tokio::test]
async fn block_with_questions_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed block");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        BlockWithQuestionsMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // Neither turn can meet the empty criterion's "Stop wrap-up" condition, so the budget is exhausted; K3 grants one extra wrap-up response,
    // therefore the provider is called once more (3 = 2 normal turns + 1 wrap-up turn).
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}
#[tokio::test]
async fn propose_scope_change_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed scope change");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        ProposeScopeChangeMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // Neither turn can meet the empty criterion's "Stop wrap-up" condition, so the budget is exhausted; K3 grants one extra wrap-up response,
    // therefore the provider is called once more (3 = 2 normal turns + 1 wrap-up turn).
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}
