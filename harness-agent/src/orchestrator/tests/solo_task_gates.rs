#![cfg(test)]

use super::*;

#[tokio::test]
async fn run_solo_task_real_time_gate_allows_out_of_allowlist_with_advisory() {
    use crate::plan::write_audit::TaskScope;
    // C2: Writes outside the allowlist are soft-allowed (the file is created) and the journal contains scope.advisory (no longer hard-blocked).
    #[derive(Clone)]
    struct OneWriteProvider;
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for OneWriteProvider {
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
                text: "writing".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "w1".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "fs_write".into(),
                        arguments:
                            serde_json::json!({ "path": "out_of_scope.txt", "content": "x" })
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

    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "rt_gate", vec![]);
    let scope = Some(TaskScope {
        files_scope: vec!["allowed.txt".into()],
        forbidden_scope: vec![],
        crate_roots: vec![],
    });
    run_solo_task(
        OneWriteProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert!(
        ws.path().join("out_of_scope.txt").exists(),
        "白名单外写入·软放行·文件应生成"
    );
    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/rt_gate/events.jsonl")).unwrap();
    assert!(
        events.contains("\"type\":\"scope.advisory\""),
        "应发 scope.advisory 软提示事件：{events}"
    );
    assert!(
        !events.contains("out of task scope"),
        "白名单外不再硬挡 PermissionDenied"
    );
}

#[tokio::test]
async fn run_solo_task_real_time_gate_hard_denies_forbidden() {
    use crate::plan::write_audit::TaskScope;
    // C2: The forbidden red line remains hard-blocked (the file is not created).
    #[derive(Clone)]
    struct ForbiddenWriteProvider;
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ForbiddenWriteProvider {
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
                text: "writing".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "w1".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "fs_write".into(),
                        arguments: serde_json::json!({ "path": "secret.txt", "content": "x" })
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

    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "rt_forbidden", vec![]);
    let scope = Some(TaskScope {
        files_scope: vec!["secret.txt".into()],
        forbidden_scope: vec!["secret.txt".into()],
        crate_roots: vec![],
    });
    run_solo_task(
        ForbiddenWriteProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert!(
        !ws.path().join("secret.txt").exists(),
        "红线 forbidden 写入·硬挡·文件不该生出来"
    );
    let events = std::fs::read_to_string(
        jr.path()
            .join(".myagenthubs/runs/rt_forbidden/events.jsonl"),
    )
    .unwrap();
    assert!(events.contains("out of task scope") || events.contains("permission denied"));
}

#[tokio::test]
async fn run_solo_task_injects_task_contract_scope_and_constraints() {
    // The task contract's scope/constraints must reach the child run and be visible in goal.created.
    #[derive(Clone)]
    struct DoneProvider;
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for DoneProvider {
        async fn next_turn(
            &self,
            _m: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut EventRecorder,
        ) -> Result<crate::provider::ProviderResponse> {
            Ok(crate::provider::ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
                interruption: None,
            })
        }
        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            task_test_caps()
        }
    }
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let contract = crate::goal::GoalContract {
        objective: "edit a".into(),
        constraints: vec!["forbidden_scope（绝不能碰）: src/secret.rs".into()],
        scope: Some("src/a.rs".into()),
        criteria: vec![],
        version: 1,
        update_log: vec![],
    };
    let opts = task_test_run_options(ws.path(), jr.path(), "inject", vec![]);
    run_solo_task(
        DoneProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
        Some(contract),
        None,
    )
    .await
    .unwrap();
    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/inject/events.jsonl")).unwrap();
    assert!(
        events.contains("\"scope\":\"src/a.rs\""),
        "child goal.created 须带 scope: {events}"
    );
    assert!(
        events.contains("src/secret.rs"),
        "child goal.created 须带 forbidden 约束"
    );
}

#[tokio::test]
async fn propose_scope_change_with_paths_extends_and_continues() {
    // C3: kind=scope with paths extends the allowlist and continues the run (not NeedsDecision), emitting a scope.extended event.
    #[derive(Clone)]
    struct ExtendThenDoneProvider;

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ExtendThenDoneProvider {
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
                            "detail": "need to touch the cli too",
                            "paths": ["src/cli.rs"]
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

    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "scope_ext", passing_criteria());
    let scope = Some(crate::plan::write_audit::TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec![],
    });
    let result = run_solo_task(
        ExtendThenDoneProvider,
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
        "带 paths 的 scope 申报不该硬停"
    );
    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/scope_ext/events.jsonl"))
            .unwrap();
    assert!(
        events.contains("\"type\":\"scope.extended\""),
        "应发 scope.extended：{events}"
    );
    assert!(events.contains("src/cli.rs"));
}
