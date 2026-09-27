#![cfg(test)]

use super::*;

struct ProposeCriterionThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeCriterionThenFinalProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a criterion.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_new_criterion",
                    "propose_criterion",
                    json!({ "claim": "new criterion", "check_cmd": "true" }),
                )],
                finish_reason: None,
                interruption: None,
            });
        }

        assert!(
            messages.iter().any(|message| {
                message.role == "tool"
                    && message.tool_call_id.as_deref() == Some("call_new_criterion")
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("criterion approved"))
            }),
            "provider must see the approved criterion tool result"
        );
        Ok(ProviderResponse {
            text: "Criterion accepted; finishing.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-criterion-then-final")
    }
}

struct ResumeContractReflexProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ResumeContractReflexProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Restoring progress with a write.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_resume_write",
                    "fs_write",
                    json!({ "path": "touched.txt", "content": "ok\n" }),
                )],
                finish_reason: None,
                interruption: None,
            });
        }

        assert!(
            messages.iter().any(|message| {
                message.role == "tool"
                    && message.tool_call_id.as_deref() == Some("call_resume_write")
            }),
            "resume provider must see the write result before finalizing"
        );
        Ok(ProviderResponse {
            text: "Finished after the resumed write.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("resume-contract-reflex")
    }
}

struct ResumeRealignProvider {
    calls: Arc<AtomicUsize>,
    seen_messages: Arc<Mutex<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl ProviderClient for ResumeRealignProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            *self.seen_messages.lock().unwrap() = messages.to_vec();
            return Ok(ProviderResponse {
                text: "Applying the realigned contract.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_realign_write",
                    "fs_write",
                    json!({ "path": "realigned.txt", "content": "ok\n" }),
                )],
                finish_reason: None,
                interruption: None,
            });
        }

        assert!(
            messages.iter().any(|message| {
                message.role == "tool"
                    && message.tool_call_id.as_deref() == Some("call_realign_write")
            }),
            "realign provider must see the write result before finalizing"
        );
        Ok(ProviderResponse {
            text: "Finished after the realigned write.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("resume-realign")
    }
}

#[tokio::test]
async fn resume_loads_goal_contract_sidecar_for_reflex_criteria() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_resume_contract_reflex";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-contract-reflex".to_string(),
            messages: initial_messages("persist objective"),
        },
    )
    .unwrap();
    let criteria = crate::goal::parse_criteria(&["cmd: test -f touched.txt".into()]).unwrap();
    let mut contract = GoalState::new("persist objective", criteria).contract;
    contract.version = 7;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = resume_solo_with_judge(
        ResumeContractReflexProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        2,
        ControlInputKind::Sentinel,
        true,
        true,
        crate::config::SearchChoice::Ddg,
        Default::default(),
        1,
        0,
        None,
        Vec::new(),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"validation.checked\""));
    assert!(events.contains("\"tool_call_id\":\"check_reflex_1_c1\""));
    assert!(events.contains("\"criterion_id\":\"c1\""));
    let restored = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(restored.version, 7);
    assert_eq!(restored.criteria.len(), 1);
}

#[tokio::test]
async fn resume_with_realign_bumps_contract_emits_goal_updated_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_resume_realign";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    std::fs::write(dir.path().join("old.txt"), "ok\n").unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-realign".to_string(),
            messages: initial_messages("old objective"),
        },
    )
    .unwrap();
    let criteria = crate::goal::parse_criteria(&["cmd: test -f old.txt".into()]).unwrap();
    let contract = GoalState::new("old objective", criteria).contract;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_messages = Arc::new(Mutex::new(Vec::new()));

    let result = resume_solo_with_judge(
        ResumeRealignProvider {
            calls: calls.clone(),
            seen_messages: seen_messages.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        3,
        ControlInputKind::Sentinel,
        true,
        true,
        crate::config::SearchChoice::Ddg,
        Default::default(),
        1,
        0,
        Some(crate::goal::ReAlignInput {
            objective: Some("new objective".into()),
            add_criteria: crate::goal::parse_criteria(&["cmd: test -f realigned.txt".into()])
                .unwrap(),
            reason: "user clarified after stuck_repeating".into(),
            ..Default::default()
        }),
        Vec::new(),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let restored = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(restored.version, 2);
    assert_eq!(restored.objective, "new objective");
    assert_eq!(restored.update_log.len(), 1);
    let ids: Vec<_> = restored
        .criteria
        .iter()
        .map(|criterion| criterion.id.as_str())
        .collect();
    assert_eq!(ids, vec!["c1", "c2"]);

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let updated = events
        .iter()
        .find(|event| event["type"] == "goal.updated" && event["payload"]["trigger"] == "realign")
        .expect("resume realign should emit goal.updated");
    assert_eq!(updated["payload"]["version"], 2);
    assert_eq!(updated["payload"]["latest_update"]["version"], 2);
    assert_eq!(
        updated["payload"]["latest_update"]["reason"],
        "user clarified after stuck_repeating"
    );
    assert_eq!(updated["payload"]["criteria"].as_array().unwrap().len(), 2);
    assert!(events.iter().any(|event| {
        event["type"] == "tool.completed"
            && event["payload"]["criterion_id"] == "c2"
            && event["payload"]["passed"] == true
    }));

    let seen = seen_messages.lock().unwrap().clone();
    let provider_system = seen[0].content.as_deref().unwrap();
    assert!(provider_system.contains("Objective: new objective"));
    assert!(provider_system.contains("[pending] c2 - 验收检查（须 exit 0）: test -f realigned.txt"));
    assert!(!provider_system.contains("cmd: test -f realigned.txt")); // C1: the state frame no longer omits cmd:
}

#[tokio::test]
async fn resume_without_realign_keeps_contract_version_and_emits_no_realign_update() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_resume_without_realign";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-without-realign".to_string(),
            messages: initial_messages("persist objective"),
        },
    )
    .unwrap();
    let mut contract = GoalState::new("persist objective", passing_criteria()).contract;
    contract.version = 7;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));

    let result = resume_solo_with_judge(
        StateFrameCaptorProvider {
            seen_messages: seen_messages.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        1,
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
    let restored = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(restored.version, 7);
    assert!(restored.update_log.is_empty());

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!events.iter().any(|event| {
        event["type"] == "goal.updated" && event["payload"]["trigger"] == "realign"
    }));
}

#[tokio::test]
async fn run_solo_writes_goal_contract_sidecar_under_journal_root() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let run_id = "run_contract_sidecar_new";
    let mut opts = options(workspace.path().to_path_buf(), "persist objective");
    opts.journal_root = journal.path().to_path_buf();
    opts.run_id = Some(run_id.to_string());
    opts.criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();

    let result = run_solo_with_judge(
        CompleteImmediatelyProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    let paths = RunPaths::new(journal.path(), run_id);
    assert!(paths.contract_path.starts_with(journal.path()));
    assert!(!paths.contract_path.starts_with(workspace.path()));
    let contract = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(contract.objective, "persist objective");
    assert_eq!(contract.version, 1);
    assert_eq!(contract.criteria.len(), 1);
}

#[tokio::test]
async fn approved_criteria_update_rewrites_goal_contract_sidecar() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let run_id = "run_contract_sidecar_update";
    let calls = Arc::new(AtomicUsize::new(0));
    let mut opts = options(workspace.path().to_path_buf(), "approve criterion");
    opts.journal_root = journal.path().to_path_buf();
    opts.run_id = Some(run_id.to_string());
    opts.max_turns = 2;
    opts.contract_policy = crate::guardrails::ContractPolicy::TrustAll;

    let result = run_solo_with_judge(
        ProposeCriterionThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let paths = RunPaths::new(journal.path(), run_id);
    let contract = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(contract.version, 1);
    assert_eq!(contract.criteria.len(), 1);
    assert_eq!(contract.criteria[0].claim, "new criterion");
}

struct OrderCaptor(Arc<Mutex<Vec<(String, bool)>>>);

#[async_trait::async_trait]
impl ProviderClient for OrderCaptor {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        events: &mut crate::events::EventRecorder,
    ) -> crate::error::Result<ProviderResponse> {
        // The second value means "this message itself is the <env> block";
        // system guidance can legitimately mention <env>.
        *self.0.lock().unwrap() = messages
            .iter()
            .map(|m| {
                (
                    m.role.clone(),
                    m.content
                        .as_deref()
                        .unwrap_or("")
                        .trim_start()
                        .starts_with("<env>"),
                )
            })
            .collect();
        events.emit_text_delta("done")?;
        Ok(ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "cap".into(),
            model_id: "cap".into(),
            supports_streaming: true,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }
}

#[tokio::test]
async fn fresh_run_wire_order_is_system_env_task() {
    let ws = tempfile::tempdir().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut opts = options(ws.path().to_path_buf(), "do the task");
    opts.memory_enabled = false;
    opts.prompt = "do the task".into();

    let _ = run_solo_with_judge(
        OrderCaptor(captured.clone()),
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    let seq = captured.lock().unwrap().clone();
    assert_eq!(
        seq[0].0, "system",
        "first message must be system (executor prompt + state-frame)"
    );
    assert!(
        !seq[0].1,
        "system is not itself the <env> terrain block, even when it points to it"
    );
    assert_eq!(seq[1].0, "user");
    assert!(
        seq[1].1,
        "second message must be the user <env> terrain block"
    );
    assert_eq!(seq[2].0, "user", "third message is the task");
    assert!(!seq[2].1, "task is not an <env> block");
}
