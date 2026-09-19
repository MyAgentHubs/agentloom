#![cfg(test)]

//! `propose_scope_change{kind:"scope"}` must not report success when nothing
//! was actually merged into the live scope allowlist. See run_loop.rs's
//! `propose_scope_change` dispatch and `Guardrails::extend_files_scope`.

use super::*;

#[derive(Clone)]
struct ScopeChangeOnceProvider {
    paths: Vec<String>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for ScopeChangeOnceProvider {
    async fn next_turn(
        &self,
        messages: &[crate::provider::ChatMessage],
        _t: &[serde_json::Value],
        _e: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if messages.iter().any(|m| m.role == "tool") {
            return Ok(crate::provider::ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
                interruption: None,
            });
        }
        Ok(crate::provider::ProviderResponse {
            text: "need more files".into(),
            reasoning: String::new(),
            tool_calls: vec![crate::provider::ToolCall {
                id: "s1".into(),
                call_type: "function".into(),
                function: crate::provider::FunctionCall {
                    name: "propose_scope_change".into(),
                    arguments: serde_json::json!({
                        "kind": "scope",
                        "detail": "need more files",
                        "paths": self.paths,
                    })
                    .to_string(),
                },
            }],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

fn last_tool_message(jr: &std::path::Path, run_id: &str) -> serde_json::Value {
    let paths = RunPaths::new(jr, run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    let msg = saved
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "tool")
        .expect("expected a tool result message");
    let content = msg
        .content
        .as_deref()
        .expect("tool message must have content");
    serde_json::from_str(content).expect("tool result must be JSON")
}

#[tokio::test]
async fn propose_scope_change_all_absolute_paths_is_honestly_rejected() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "scope_ext", passing_criteria());
    let scope = Some(crate::plan::write_audit::TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec![],
    });
    let result = run_solo_task(
        ScopeChangeOnceProvider {
            paths: vec!["/tmp/baby.svg".into(), "/tmp/baby.png".into()],
        },
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert_ne!(
        result.outcome,
        RunOutcome::NeedsDecision,
        "an all-rejected scope proposal must not hard-stop the run"
    );

    let tool_result = last_tool_message(jr.path(), "scope_ext");
    assert_eq!(tool_result["status"], "scope_extend_rejected");
    assert_eq!(tool_result["added"].as_array().unwrap().len(), 0);
    let rejected = tool_result["rejected"].as_array().unwrap();
    assert_eq!(
        rejected.len(),
        2,
        "both absolute paths must be rejected: {rejected:?}"
    );
    for entry in rejected {
        assert!(
            entry["reason"].as_str().unwrap().len() > 0,
            "every rejected path needs a non-empty reason: {entry:?}"
        );
    }

    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/scope_ext/events.jsonl"))
            .unwrap();
    assert!(
        !events.contains("\"type\":\"scope.extended\""),
        "nothing was added, scope.extended must not be emitted: {events}"
    );
}

#[tokio::test]
async fn propose_scope_change_without_task_scope_is_rejected_with_reason() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "scope_ext", passing_criteria());

    let result = run_solo_task(
        ScopeChangeOnceProvider {
            paths: vec!["src/cli.rs".into()],
        },
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        None, // no task scope on this run
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::NeedsDecision);
    let tool_result = last_tool_message(jr.path(), "scope_ext");
    assert_eq!(tool_result["status"], "scope_extend_rejected");
    let rejected = tool_result["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0]["reason"]
            .as_str()
            .unwrap()
            .contains("no task scope"),
        "reason must explain there is no task scope: {rejected:?}"
    );

    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/scope_ext/events.jsonl"))
            .unwrap();
    assert!(!events.contains("\"type\":\"scope.extended\""));
}

#[tokio::test]
async fn propose_scope_change_mixed_paths_adds_legal_and_rejects_absolute() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "scope_ext", passing_criteria());
    let scope = Some(crate::plan::write_audit::TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec![],
    });

    let result = run_solo_task(
        ScopeChangeOnceProvider {
            paths: vec!["src/cli.rs".into(), "/tmp/baby.svg".into()],
        },
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::NeedsDecision);
    let tool_result = last_tool_message(jr.path(), "scope_ext");
    assert_eq!(tool_result["status"], "scope_extended");
    let added = tool_result["added"].as_array().unwrap();
    assert_eq!(added, &vec![serde_json::json!("src/cli.rs")]);
    let rejected = tool_result["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["path"], "/tmp/baby.svg");
    assert!(rejected[0]["reason"].as_str().unwrap().len() > 0);

    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/scope_ext/events.jsonl"))
            .unwrap();
    assert!(
        events.contains("\"type\":\"scope.extended\""),
        "the legal path was still added, scope.extended must be emitted: {events}"
    );
}

#[tokio::test]
async fn propose_scope_change_all_already_in_scope_is_not_a_rejection() {
    // A path already in scope is not a rejection: the agent can already write it. So this
    // must still report success (nothing new was added, but nothing was denied either), and
    // must not emit `scope.extended` since nothing actually widened.
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "scope_ext", passing_criteria());
    let scope = Some(crate::plan::write_audit::TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec![],
    });

    let result = run_solo_task(
        ScopeChangeOnceProvider {
            paths: vec!["src/a.rs".into()],
        },
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::NeedsDecision);
    let tool_result = last_tool_message(jr.path(), "scope_ext");
    assert_eq!(tool_result["status"], "scope_extended");
    assert_eq!(tool_result["added"].as_array().unwrap().len(), 0);
    let already_in_scope = tool_result["already_in_scope"].as_array().unwrap();
    assert_eq!(already_in_scope, &vec![serde_json::json!("src/a.rs")]);
    assert_eq!(tool_result["rejected"].as_array().unwrap().len(), 0);

    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/scope_ext/events.jsonl"))
            .unwrap();
    assert!(
        !events.contains("\"type\":\"scope.extended\""),
        "nothing was newly added, scope.extended must not be emitted: {events}"
    );
}
