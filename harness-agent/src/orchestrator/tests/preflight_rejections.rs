#![cfg(test)]

use super::*;

struct PreflightRejectProvider {
    model_id: &'static str,
    tool_call_id: &'static str,
    tool_name: &'static str,
    arguments: String,
    expected_feedback: &'static [&'static str],
}

#[async_trait::async_trait]
impl ProviderClient for PreflightRejectProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        if messages.iter().any(|message| {
            message.role == "tool"
                && message.tool_call_id.as_deref() == Some(self.tool_call_id)
                && message.content.as_deref().is_some_and(|content| {
                    self.expected_feedback
                        .iter()
                        .all(|expected| content.contains(expected))
                })
        }) {
            return Ok(ProviderResponse {
                text: "Continuing after preflight rejection.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Calling a tool that should be rejected during preflight.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: self.tool_call_id.to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: self.tool_name.to_string(),
                    arguments: self.arguments.clone(),
                },
            }],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities(self.model_id)
    }
}

async fn run_preflight_reject_case(
    provider: PreflightRejectProvider,
) -> (RunOutcome, Vec<ChatMessage>) {
    let run_id = provider.model_id;
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "preflight reject");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 4;
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
        provider,
        opts,
        paths,
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

    (outcome, messages)
}

#[tokio::test]
async fn preflight_reject_unsupported_tool_does_not_crash() {
    let (outcome, messages) = run_preflight_reject_case(PreflightRejectProvider {
        model_id: "preflight_reject_unsupported_tool",
        tool_call_id: "call_unknown_tool",
        tool_name: "missing_tool",
        arguments: "{}".to_string(),
        expected_feedback: &["unsupported tool"],
    })
    .await;

    assert_ne!(outcome, RunOutcome::Failed);
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_unknown_tool")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("unsupported tool"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}

#[tokio::test]
async fn preflight_reject_write_targets_path_escape_does_not_crash() {
    let (outcome, messages) = run_preflight_reject_case(PreflightRejectProvider {
        model_id: "preflight_reject_write_targets_path_escape",
        tool_call_id: "call_escape_write",
        tool_name: "fs_write",
        arguments: json!({ "path": "../escape", "content": "nope" }).to_string(),
        expected_feedback: &["invalid path", "outside"],
    })
    .await;

    assert_ne!(outcome, RunOutcome::Failed);
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_escape_write")
            && message.content.as_deref().is_some_and(|content| {
                content.contains("invalid path") && content.contains("outside")
            })
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}

#[tokio::test]
async fn preflight_reject_write_targets_bad_args_does_not_crash() {
    let (outcome, messages) = run_preflight_reject_case(PreflightRejectProvider {
        model_id: "preflight_reject_write_targets_bad_args",
        tool_call_id: "call_bad_args_write",
        tool_name: "fs_write",
        arguments: json!({ "path": "missing-content.txt" }).to_string(),
        expected_feedback: &["invalid path or arguments"],
    })
    .await;

    assert_ne!(outcome, RunOutcome::Failed);
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_bad_args_write")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("invalid path or arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}
