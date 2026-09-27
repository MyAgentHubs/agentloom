#![cfg(test)]

use super::*;

struct ShellEditingProvider {
    calls: Arc<AtomicUsize>,
    shell_turns_before_final: usize,
}

#[async_trait::async_trait]
impl ProviderClient for ShellEditingProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= self.shell_turns_before_final {
            return Ok(ProviderResponse {
                text: "Finished after shell work.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Changing the workspace with shell.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_shell_{call}"),
                "shell_exec",
                json!({
                    "command": format!("printf 'shell {call}\\n' >> shell.log"),
                }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("shell-editing-run")
    }
}

struct NovelShellProvider {
    calls: Arc<AtomicUsize>,
    shell_turns_before_final: usize,
}

#[async_trait::async_trait]
impl ProviderClient for NovelShellProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= self.shell_turns_before_final {
            return Ok(ProviderResponse {
                text: "Finished after novel shell work.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            });
        }

        Ok(ProviderResponse {
            text: "Running distinct shell-only work.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_novel_shell_{call}"),
                "shell_exec",
                json!({
                    "command": format!("true # novel shell {call}"),
                }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("novel-shell-run")
    }
}

struct ReadThenEditRecoveryProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ReadThenEditRecoveryProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let names = tool_names(tools);
        match call {
            0 => Ok(ProviderResponse {
                text: "Reading the target before editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_recovery_read_target",
                    "fs_read",
                    json!({"path": "target.txt"}),
                )],
                finish_reason: None,
                interruption: None,
            }),
            1..=6 => Ok(ProviderResponse {
                text: "Re-reading before editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    &format!("call_recovery_read_{call}"),
                    "fs_read",
                    json!({"path": "target.txt"}),
                )],
                finish_reason: None,
                interruption: None,
            }),
            7 => {
                for tool in ["grep", "ls", "glob"] {
                    assert!(
                        names.iter().all(|name| name != tool),
                        "narrow threshold should hide {tool}"
                    );
                }
                assert!(
                    names.iter().any(|name| name == "fs_read"),
                    "narrow threshold should keep fs_read visible"
                );
                Ok(ProviderResponse {
                    text: "Editing once exploration is narrowed.".to_string(),
                    reasoning: String::new(),
                    tool_calls: vec![test_tool_call(
                        "call_recovery_edit",
                        "fs_edit",
                        json!({
                            "path": "target.txt",
                            "old_string": "v0",
                            "new_string": "v1",
                        }),
                    )],
                    finish_reason: None,
                    interruption: None,
                })
            }
            8 => {
                assert!(
                    names.iter().any(|name| name == "fs_read"),
                    "successful edit should clear the streak and re-offer fs_read"
                );
                assert!(
                    names.iter().any(|name| name == "grep"),
                    "successful edit should clear the streak and re-offer exploration tools"
                );
                Ok(ProviderResponse {
                    text: "Finished after tools recovered.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    interruption: None,
                })
            }
            _ => panic!("provider should have completed on turn 9"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("read-then-edit-recovery")
    }
}

#[tokio::test]
async fn normal_editing_run_not_tripped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "v0").unwrap();
    let run_id = "run_no_progress_normal_editing";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "normal editing");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        EditingProvider {
            calls,
            offered_tools: offered_tools.clone(),
            edits_before_final: 6,
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

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
        "v6"
    );
    let snapshots = offered_tools.lock().unwrap();
    assert!(snapshots
        .iter()
        .all(|names| names.iter().any(|name| name == "fs_read")));
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.blocked\""));
}

#[tokio::test]
async fn shell_exec_workspace_work_counts_as_progress() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_no_progress_shell_work";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "shell edits workspace");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop(
        ShellEditingProvider {
            calls: calls.clone(),
            shell_turns_before_final: 6,
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

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 7);
    let shell_log = std::fs::read_to_string(dir.path().join("shell.log")).unwrap();
    assert!(shell_log.contains("shell 5"));
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.blocked\""));
}

#[tokio::test]
async fn novel_shell_commands_cross_no_progress_threshold_through_run_solo() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_novel_shell_crosses_no_progress_threshold";
    let calls = Arc::new(AtomicUsize::new(0));
    let mut opts = options(dir.path().to_path_buf(), "distinct shell-only work");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();

    let result = run_solo(
        NovelShellProvider {
            calls: calls.clone(),
            // Ten shell-only turns cross the eight-turn no-progress halt while
            // remaining below the twelve-reset novel-shell quota.
            shell_turns_before_final: 10,
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 11);
    let events = std::fs::read_to_string(RunPaths::new(dir.path(), run_id).events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.needs_decision\""));
}

#[tokio::test]
async fn progress_after_narrowing_restores_exploration_tools_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "v0").unwrap();
    let run_id = "run_no_progress_recovery";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "recover after edit");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop(
        ReadThenEditRecoveryProvider {
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

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 9);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
        "v1"
    );
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.blocked\""));
}
