#![cfg(test)]

use super::*;

fn assert_python3_available() {
    let output = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .expect("python3 must be installed for real syntax-probe tests");
    assert!(
        output.status.success(),
        "python3 must run successfully for real syntax-probe tests: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn recorder(dir: &Path) -> EventRecorder {
    EventRecorder::new(
        "syntax-test",
        None,
        None,
        &dir.join("events.jsonl"),
        crate::events::OutputMode::Silent,
    )
    .unwrap()
}

#[tokio::test]
async fn real_edit_gate_reports_python_syntax_with_empty_criteria() {
    assert_python3_available();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("broken.py");
    std::fs::write(&file, "def f(:\n  pass\n").unwrap();
    let paths = BTreeSet::from([file]);
    let goal = GoalState::new("fix it", vec![]);
    assert!(goal.contract.criteria.is_empty());
    let progress = crate::run_progress::RunProgress::default();
    let mut recorder = recorder(dir.path());

    let immediate = collect_immediate_edit_diagnostics(
        &goal,
        &paths,
        dir.path(),
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
        "test_immediate",
        1,
        1,
        &progress,
    )
    .await
    .unwrap();
    assert!(!immediate.verify_reflex_will_run);
    let feedback = immediate_diagnostic_feedback(&immediate.diagnostics).expect("syntax feedback");

    assert!(feedback.contains("新增编译错（改完即时检出）"));
    assert!(feedback.contains("broken.py:1"));
    assert!(feedback.contains("SyntaxError"));
}

#[tokio::test]
async fn real_edit_gate_blocks_completion_and_wrapup_nudge_on_python_syntax() {
    assert_python3_available();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("broken.py");
    std::fs::write(&file, "def f(:\n  pass\n").unwrap();
    let paths = BTreeSet::from([file]);
    let criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let mut goal = GoalState::new("fix it", criteria);
    let progress = crate::run_progress::RunProgress::default();
    let mut recorder = recorder(dir.path());

    let immediate = collect_immediate_edit_diagnostics(
        &goal,
        &paths,
        dir.path(),
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
        "test_immediate_with_verify",
        1,
        1,
        &progress,
    )
    .await
    .unwrap();
    assert!(immediate.verify_reflex_will_run);
    let feedback = immediate_diagnostic_feedback(&immediate.diagnostics).expect("syntax feedback");
    let validation = crate::evaluator::reflex_validate(
        &mut goal,
        dir.path(),
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        1,
        1,
        &mut recorder,
    )
    .await
    .unwrap();
    assert!(
        validation.feedback.is_none(),
        "the deliberately under-scoped `true` criterion must pass"
    );
    assert!(validation.checked.iter().all(|(_, passed)| *passed));

    let mut completion_gate = CompletionGate::default();
    completion_gate.note_edit(7);
    let sent_wrapup_nudge = arm_completion_gate_after_clean_reflex(
        &mut completion_gate,
        7,
        !immediate.diagnostics.is_empty(),
    );

    assert!(feedback.contains("broken.py:1"));
    assert!(feedback.contains("SyntaxError"));
    assert!(!sent_wrapup_nudge, "must not send acceptance-passed nudge");
    assert!(
        !completion_gate.ready_to_finalize(8, false),
        "syntax diagnostics must leave the completion gate disarmed"
    );
}

#[tokio::test]
async fn real_edit_gate_keeps_valid_python_feedback_quiet() {
    assert_python3_available();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("valid.py");
    std::fs::write(&file, "def f():\n    pass\n").unwrap();
    let paths = BTreeSet::from([file]);
    let goal = GoalState::new("fix it", vec![]);
    let progress = crate::run_progress::RunProgress::default();
    let mut recorder = recorder(dir.path());

    let immediate = collect_immediate_edit_diagnostics(
        &goal,
        &paths,
        dir.path(),
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
        "test_immediate",
        1,
        1,
        &progress,
    )
    .await
    .unwrap();

    assert!(immediate_diagnostic_feedback(&immediate.diagnostics).is_none());
}

#[test]
fn immediate_diagnostics_deduplicate_cargo_and_syntax_results() {
    let diagnostic = crate::diagnostics::Diagnostic {
        file: "src/lib.rs".into(),
        line: 3,
        error_code: Some("E0001".into()),
        message: "broken".into(),
        root_cause_key: "same".into(),
        symbol: None,
    };

    let merged = merge_diagnostics(vec![diagnostic.clone()], vec![diagnostic]);

    assert_eq!(merged.len(), 1);
}

struct RepeatedWriteProvider {
    content: String,
    path: &'static str,
}

#[async_trait::async_trait]
impl ProviderClient for RepeatedWriteProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        Ok(crate::provider::ProviderResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![crate::provider::ToolCall {
                id: "write".into(),
                call_type: "function".into(),
                function: crate::provider::FunctionCall {
                    name: "fs_write".into(),
                    arguments: json!({"path": self.path, "content": self.content}).to_string(),
                },
            }],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::mock::MockProvider::default().capabilities()
    }
}

fn regression_options(workspace: PathBuf, prompt: &str) -> RunOptions {
    RunOptions {
        prompt: prompt.to_string(),
        workspace: workspace.clone(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: crate::events::OutputMode::Silent,
        control_input: crate::orchestrator::ControlInputKind::Sentinel,
        evidence_gate: EvidenceGate::Off,
        permission: crate::shell::PermissionPolicy::Allow,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        extra_read_roots: Vec::new(),
        fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: crate::config::SearchChoice::Ddg,
        max_turns: 3,
        run_id: Some("run_test".into()),
        context_files: Vec::new(),
        criteria: Vec::new(),
        contract_policy: crate::guardrails::ContractPolicy::TrustAll,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        journal_root: workspace.clone(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
        images: Vec::new(),
    }
}

struct TurnFixture {
    _dir: tempfile::TempDir,
    options: RunOptions,
    paths: RunPaths,
    recorder: EventRecorder,
    goal: GoalState,
    messages: Vec<ChatMessage>,
    state: LoopState,
}

impl TurnFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let options = regression_options(dir.path().to_path_buf(), "keep writing");
        let paths = RunPaths::new(dir.path(), "run_test");
        paths.create_dirs().unwrap();
        let recorder = EventRecorder::new(
            "run_test",
            None,
            None,
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let state = LoopState::new(&options, &paths);
        Self {
            _dir: dir,
            goal: GoalState::new(options.prompt.clone(), Vec::new()),
            messages: initial_messages(&options.prompt),
            options,
            paths,
            recorder,
            state,
        }
    }

    async fn step(&mut self, turn: usize, path: &'static str, content: String) -> TurnFlow {
        let provider = RepeatedWriteProvider { path, content };
        let registry = build_default_registry_with_write_fence(
            &self.options.search,
            false,
            self.options.fs_write_fence,
        );
        let guardrails = Guardrails::new(&self.options.workspace, self.options.permission, false);
        let mut control = crate::control::QueueControlSource::new(Vec::new());
        let capabilities = provider.capabilities();
        let mut ctx = LoopCtx {
            registry: &registry,
            capabilities: &capabilities,
            options: &self.options,
            paths: &self.paths,
            run_id: "run_test",
            recorder: &mut self.recorder,
            goal: &mut self.goal,
            messages: &mut self.messages,
            judge: &crate::judge::NoopJudge,
            guardrails: &guardrails,
            control: &mut control,
            write_tools_offered: true,
            edit_format: crate::model_registry::EditFormat::Targeted,
        };
        run_turn(&mut ctx, &provider, &mut self.state, turn)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn turn_had_progress_sync_distinguishes_changed_and_unchanged_writes() {
    let mut same = TurnFixture::new();
    let mut changing = TurnFixture::new();
    for turn in 1..=3 {
        assert!(matches!(
            same.step(turn, "out.txt", "same\n".into()).await,
            TurnFlow::NextTurn
        ));
        assert!(matches!(
            changing
                .step(turn, "out.txt", format!("turn {turn}\n"))
                .await,
            TurnFlow::NextTurn
        ));
    }
    // The legacy progress streak consumes turn_had_progress; the safety net uses
    // independent edit signals, so halt timing alone cannot detect this mutation.
    assert_eq!(same.state.progress.consecutive_read_only_turns, 2);
    assert_eq!(changing.state.progress.consecutive_read_only_turns, 0);
}

#[tokio::test]
async fn immediate_diagnostics_replace_python_errors_but_retain_other_errors_across_turns() {
    assert_python3_available();
    let mut fixture = TurnFixture::new();
    fixture.options.verify_reflex_debt = 1;
    fixture.goal = GoalState::new(
        "fix syntax",
        crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
    );
    let prior = crate::diagnostics::Diagnostic {
        file: "src/lib.rs".into(),
        line: 3,
        error_code: Some("E0001".into()),
        message: "existing compile error".into(),
        root_cause_key: "existing".into(),
        symbol: None,
    };
    fixture.state.last_probe_diags.push(prior.clone());
    assert!(matches!(
        fixture
            .step(1, "broken.py", "def f(:\n  pass\n".into())
            .await,
        TurnFlow::NextTurn
    ));
    assert!(fixture.state.last_probe_diags.contains(&prior));
    assert!(fixture
        .state
        .last_probe_diags
        .iter()
        .any(|d| d.error_code.as_deref() == Some("PY_SYNTAX")));
    assert!(matches!(
        fixture
            .step(2, "broken.py", "def f():\n  pass\n".into())
            .await,
        TurnFlow::NextTurn
    ));
    assert_eq!(fixture.state.last_probe_diags, vec![prior]);
}

#[tokio::test]
async fn end_of_turn_snapshot_preserves_feedback_for_resume() {
    assert_python3_available();
    let mut fixture = TurnFixture::new();
    assert!(matches!(
        fixture
            .step(1, "broken.py", "def f(:\n  pass\n".into())
            .await,
        TurnFlow::NextTurn
    ));
    // Load the on-disk conversation at the turn boundary, before a later
    // provider response or terminal save can mask a missing end-of-turn save.
    let saved: crate::journal::SavedConversation<ChatMessage> =
        crate::journal::load_conversation(&fixture.paths.conversation_path).unwrap();
    let feedback = saved.messages.last().expect("persisted feedback");
    assert_eq!(feedback.role, "user");
    let content = feedback.content.as_deref().unwrap();
    assert!(content.contains("broken.py:1"));
    assert!(content.contains("SyntaxError"));
    assert_eq!(feedback.content, fixture.messages.last().unwrap().content);
}
