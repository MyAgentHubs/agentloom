#![cfg(test)]

use super::*;

#[test]
fn evidence_edit_shell_exec_is_never_blocked_without_a_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let evidence = EvidenceState::new(EvidenceGate::On);
    let targets = vec![workspace.path().join("shell-created.txt")];

    assert!(!evidence_edit_should_block(
        "shell_exec",
        &targets,
        workspace.path(),
        &evidence,
    ));
}

#[derive(Clone)]
struct EvidenceEditShellProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceEditShellProvider {
    async fn next_turn(
        &self,
        _messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: "run the reproduction from the shell".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "evidence-shell".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'shell allowed\\n' > shell-created.txt"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
                interruption: None,
            });
        }
        Ok(crate::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(crate::provider::FinishReason::Stop),
            interruption: None,
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_edit_shell_exec_runs_when_gate_on_without_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut options =
        task_test_run_options(workspace.path(), journal.path(), "evidence-shell", vec![]);
    options.evidence_gate = EvidenceGate::On;

    run_solo(
        EvidenceEditShellProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("shell-created.txt")).unwrap(),
        "shell allowed\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-shell").events_path)
            .unwrap();
    assert!(!events.contains("evidence.edit.blocked"));
}

#[derive(Clone)]
struct EvidenceShellEditAfterProbeProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceShellEditAfterProbeProvider {
    async fn next_turn(
        &self,
        _messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(evidence_completion_response(vec![
                evidence_completion_register_call(
                    "register-shell-edit-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "shell-edit-target".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ]));
        }
        Ok(evidence_completion_response(Vec::new()))
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_liveness_shell_exec_edit_advances_epoch() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let mut options = task_test_run_options(
        workspace.path(),
        journal.path(),
        "evidence-shell-edit",
        vec![],
    );
    options.evidence_gate = EvidenceGate::On;

    let result = run_solo(
        EvidenceShellEditAfterProbeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "fixed\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-shell-edit").events_path)
            .unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"edit_epoch\":1"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[derive(Clone)]
struct EvidenceDirtyRewriteProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceDirtyRewriteProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let tool_calls = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                evidence_completion_register_call(
                    "register-dirty-rewrite-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "make-dirty-green".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ],
            1 => vec![ToolCall {
                id: "rewrite-same-dirty-file-red".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: json!({
                        "command": "printf 'buggy again\\n' > target.txt"
                    })
                    .to_string(),
                },
            }],
            _ => Vec::new(),
        };
        Ok(evidence_completion_response(tool_calls))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-dirty-rewrite")
    }
}

#[tokio::test]
async fn evidence_liveness_shell_exec_rewrite_of_dirty_file_invalidates_green() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=AgentLoom Test",
            "-c",
            "user.email=agentloom@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(commit.success());
    let mut options = task_test_run_options(
        workspace.path(),
        journal.path(),
        "evidence-dirty-rewrite",
        Vec::new(),
    );
    options.evidence_gate = EvidenceGate::On;
    options.max_turns = 3;

    let result = run_solo(
        EvidenceDirtyRewriteProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "buggy again\n"
    );
    let events = std::fs::read_to_string(
        RunPaths::new(journal.path(), "evidence-dirty-rewrite").events_path,
    )
    .unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"type\":\"evidence.probe.still_red\""));
    assert!(events.contains("\"edit_epoch\":2"));
    assert!(events.contains("\"green_epoch\":null"));
    assert!(events.contains("\"reason\":\"evidence_probe_still_red\""));
}

#[derive(Clone)]
struct EvidenceUnverifiableWorkspaceProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceUnverifiableWorkspaceProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let tool_calls = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                evidence_completion_register_call(
                    "register-unverifiable-workspace-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "make-unverifiable-workspace-green".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ],
            1 => vec![ToolCall {
                id: "break-git-fingerprint".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: json!({
                        "command": "chmod 000 target.txt"
                    })
                    .to_string(),
                },
            }],
            _ => {
                let saw_feedback = messages.iter().any(|message| {
                    message.content.as_deref().is_some_and(|content| {
                        content.contains("harness cannot verify the workspace state")
                            && content.contains("cannot confirm the fix")
                    })
                });
                self.saw_feedback
                    .store(usize::from(saw_feedback), Ordering::SeqCst);
                Vec::new()
            }
        };
        Ok(evidence_completion_response(tool_calls))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-unverifiable-workspace")
    }
}

#[tokio::test]
async fn evidence_unverifiable_workspace_invalidates_green_not_keeps_it() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=AgentLoom Test",
            "-c",
            "user.email=agentloom@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(commit.success());
    let saw_feedback = Arc::new(AtomicUsize::new(0));
    let mut options = task_test_run_options(
        workspace.path(),
        journal.path(),
        "evidence-unverifiable-workspace",
        Vec::new(),
    );
    options.evidence_gate = EvidenceGate::On;
    options.max_turns = 3;

    let result = run_solo(
        EvidenceUnverifiableWorkspaceProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
        },
        options,
    )
    .await
    .unwrap();

    let events = std::fs::read_to_string(
        RunPaths::new(journal.path(), "evidence-unverifiable-workspace").events_path,
    )
    .unwrap();
    assert_ne!(result.outcome, RunOutcome::Completed);
    assert_eq!(saw_feedback.load(Ordering::SeqCst), 1);
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"type\":\"evidence.workspace.unverifiable\""));
    assert!(events.contains("\"edit_epoch\":2"));
    assert!(events.contains("\"green_epoch\":1"));
    assert!(events.contains("\"reason\":\"evidence_stale_green\""));
}

#[derive(Clone)]
struct EvidenceEditBlockedProvider {
    calls: Arc<AtomicUsize>,
    saw_guidance: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceEditBlockedProvider {
    async fn next_turn(
        &self,
        messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: "edit before registering".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "evidence-blocked-edit".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "fs_edit".into(),
                        arguments: json!({
                            "path": "target.txt",
                            "old_string": "buggy",
                            "new_string": "fixed"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
                interruption: None,
            });
        }
        let saw_guidance = messages.iter().any(|message| {
            message.role == "tool"
                && message.content.as_deref().is_some_and(|content| {
                    content.contains("Blocked: you have no confirmed-red reproduction yet")
                        && content.contains("Call register_issue_probe first")
                })
        });
        self.saw_guidance
            .store(usize::from(saw_guidance), Ordering::SeqCst);
        Ok(crate::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(crate::provider::FinishReason::Stop),
            interruption: None,
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_edit_fs_edit_is_blocked_without_probe_and_emits_guidance() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let saw_guidance = Arc::new(AtomicUsize::new(0));
    let mut options =
        task_test_run_options(workspace.path(), journal.path(), "evidence-block", vec![]);
    options.evidence_gate = EvidenceGate::On;

    run_solo(
        EvidenceEditBlockedProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_guidance: saw_guidance.clone(),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(saw_guidance.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "buggy\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-block").events_path)
            .unwrap();
    assert!(events.contains("\"type\":\"evidence.edit.blocked\""));
    assert!(events.contains("\"tool\":\"fs_edit\""));
    assert!(events.contains("\"outcome\":\"require_probe\""));
    assert!(events.contains("target.txt"));
}

#[derive(Clone)]
struct EvidenceEditAcceptedProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceEditAcceptedProvider {
    async fn next_turn(
        &self,
        _messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: "register, inspect, and edit".into(),
                reasoning: String::new(),
                tool_calls: vec![
                    crate::provider::ToolCall {
                        id: "evidence-register".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "register_issue_probe".into(),
                            arguments: json!({
                                "script": "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'fixed\\n'; fi",
                                "command": "sh {probe}",
                                "red_marker": "BUG_PRESENT",
                                "marker_stream": "stdout",
                                "rationale": "target remains buggy"
                            })
                            .to_string(),
                        },
                    },
                    crate::provider::ToolCall {
                        id: "evidence-read".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "fs_read".into(),
                            arguments: json!({"path": "target.txt"}).to_string(),
                        },
                    },
                    crate::provider::ToolCall {
                        id: "evidence-allowed-edit".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "fs_edit".into(),
                            arguments: json!({
                                "path": "target.txt",
                                "old_string": "buggy",
                                "new_string": "fixed"
                            })
                            .to_string(),
                        },
                    },
                ],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
                interruption: None,
            });
        }
        Ok(crate::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(crate::provider::FinishReason::Stop),
            interruption: None,
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_edit_registered_probe_allows_fs_edit_and_auto_reruns_green() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut options =
        task_test_run_options(workspace.path(), journal.path(), "evidence-allowed", vec![]);
    options.evidence_gate = EvidenceGate::On;

    run_solo(
        EvidenceEditAcceptedProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "fixed\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-allowed").events_path)
            .unwrap();
    assert!(!events.contains("evidence.edit.blocked"));
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"edit_epoch\":1"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[test]
fn evidence_edit_gate_allows_probe_off_and_bypassed_states() {
    let workspace = tempfile::tempdir().unwrap();
    let targets = vec![workspace.path().join("target.txt")];

    let mut with_probe = EvidenceState::new(EvidenceGate::On);
    with_probe.accept_probe(ProbeManifest {
        probe_id: "probe".into(),
        script_sha256: "hash".into(),
        script: "printf BUG".into(),
        script_path: PathBuf::from("${TMPDIR:-/tmp}/agentloom-probes/test/probe.sh"),
        command: "sh ${TMPDIR:-/tmp}/agentloom-probes/test/probe.sh".into(),
        red_oracle: RedOracle {
            marker: "BUG".into(),
            stream: MarkerStream::Any,
        },
        rationale: "test".into(),
        registered_turn: 1,
    });
    assert!(!evidence_edit_should_block(
        "fs_edit",
        &targets,
        workspace.path(),
        &with_probe,
    ));

    let off = EvidenceState::new(EvidenceGate::Off);
    assert!(!evidence_edit_should_block(
        "fs_edit",
        &targets,
        workspace.path(),
        &off,
    ));

    let mut bypassed = EvidenceState::new(EvidenceGate::On);
    for _ in 0..MAX_FAILED_REGISTRATIONS {
        bypassed.note_registration_failure();
    }
    assert!(!evidence_edit_should_block(
        "fs_edit",
        &targets,
        workspace.path(),
        &bypassed,
    ));
}
