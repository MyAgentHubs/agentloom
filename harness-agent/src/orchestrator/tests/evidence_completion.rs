#![cfg(test)]

use super::*;

async fn evidence_completion_try_finalize(
    evidence: &mut EvidenceState,
    run_id: &str,
) -> (FinalizeOutcome, Vec<Value>) {
    let dir = tempfile::tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(
        "engine evidence completion",
        crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
    );
    let mut eval_round = 0;

    let outcome = try_finalize(
        &mut goal,
        evidence,
        crate::guardrails::ContractPolicy::TrustAll,
        dir.path(),
        &crate::judge::NoopJudge,
        &mut recorder,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut eval_round,
        7,
        "engine_finalize",
    )
    .await
    .unwrap();
    let events = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (outcome, events)
}

#[tokio::test]
async fn evidence_completion_off_preserves_engine_finalize_for_all_state_shapes() {
    let mut states = vec![EvidenceState::new(EvidenceGate::Off)];

    let mut no_edit = EvidenceState::new(EvidenceGate::Off);
    no_edit.accept_probe(evidence_completion_probe());
    states.push(no_edit);

    let mut still_red = EvidenceState::new(EvidenceGate::Off);
    still_red.accept_probe(evidence_completion_probe());
    still_red.note_edit();
    states.push(still_red);

    let mut green = EvidenceState::new(EvidenceGate::Off);
    green.accept_probe(evidence_completion_probe());
    green.note_edit();
    green.note_probe_green();
    states.push(green);

    let mut stale = EvidenceState::new(EvidenceGate::Off);
    stale.accept_probe(evidence_completion_probe());
    stale.note_edit();
    stale.note_probe_green();
    stale.note_edit();
    states.push(stale);

    let mut bypassed = EvidenceState::new(EvidenceGate::Off);
    bypassed.bypassed = true;
    states.push(bypassed);

    for (index, evidence) in states.iter_mut().enumerate() {
        let (outcome, events) =
            evidence_completion_try_finalize(evidence, &format!("evidence-off-{index}")).await;
        assert_eq!(outcome, FinalizeOutcome::Completed, "state={evidence:?}");
        assert!(events.iter().any(|event| event["type"] == "run.completed"));
        assert!(!events
            .iter()
            .any(|event| event["type"] == "completion.rejected"));
    }
}

#[tokio::test]
async fn evidence_completion_engine_finalize_denies_every_unready_state_with_epochs() {
    let mut cases = [
        (
            EvidenceState::new(EvidenceGate::On),
            "evidence_no_probe_registered",
            0,
            Value::Null,
        ),
        {
            let mut evidence = EvidenceState::new(EvidenceGate::On);
            evidence.accept_probe(evidence_completion_probe());
            (evidence, "evidence_no_edit_yet", 0, Value::Null)
        },
        {
            let mut evidence = EvidenceState::new(EvidenceGate::On);
            evidence.accept_probe(evidence_completion_probe());
            evidence.note_edit();
            (evidence, "evidence_probe_still_red", 1, Value::Null)
        },
        {
            let mut evidence = EvidenceState::new(EvidenceGate::On);
            evidence.accept_probe(evidence_completion_probe());
            evidence.note_edit();
            evidence.note_probe_green();
            evidence.note_edit();
            (evidence, "evidence_stale_green", 2, json!(1))
        },
    ];

    for (index, (evidence, reason, edit_epoch, green_epoch)) in cases.iter_mut().enumerate() {
        let (outcome, events) =
            evidence_completion_try_finalize(evidence, &format!("evidence-denied-{index}")).await;
        assert_eq!(outcome, FinalizeOutcome::NotComplete);
        let rejected = events
            .iter()
            .find(|event| event["type"] == "completion.rejected")
            .expect("evidence rejection should be observable");
        assert_eq!(rejected["payload"]["reason"], *reason);
        assert_eq!(rejected["payload"]["via"], "engine_finalize");
        assert_eq!(rejected["payload"]["edit_epoch"], *edit_epoch);
        assert_eq!(rejected["payload"]["green_epoch"], *green_epoch);
        assert!(!events.iter().any(|event| event["type"] == "run.completed"));
    }
}

#[tokio::test]
async fn evidence_liveness_completion_denials_eventually_release_gate() {
    let dir = tempfile::tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        "completion-liveness",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(
        "completion liveness",
        crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
    );
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence.accept_probe(evidence_completion_probe());
    evidence.note_edit();
    evidence.note_probe_red();
    let mut eval_round = 0;

    for turn in 1..=MAX_COMPLETION_DENIALS {
        let outcome = try_finalize(
            &mut goal,
            &mut evidence,
            crate::guardrails::ContractPolicy::TrustAll,
            dir.path(),
            &crate::judge::NoopJudge,
            &mut recorder,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut eval_round,
            turn,
            "engine_finalize",
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::NotComplete);
    }

    assert!(evidence.bypassed);
    let completed = try_finalize(
        &mut goal,
        &mut evidence,
        crate::guardrails::ContractPolicy::TrustAll,
        dir.path(),
        &crate::judge::NoopJudge,
        &mut recorder,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut eval_round,
        MAX_COMPLETION_DENIALS + 1,
        "engine_finalize",
    )
    .await
    .unwrap();
    assert_eq!(completed, FinalizeOutcome::Completed);

    let events = std::fs::read_to_string(events_path).unwrap();
    assert_eq!(events.matches("completion.rejected").count(), 3);
    assert!(events.contains("\"type\":\"evidence.gate.bypassed\""));
    assert!(events.contains("\"reason\":\"completion_no_progress\""));
    assert!(events.contains("\"type\":\"run.completed\""));
}

#[derive(Clone, Copy)]
enum EvidenceMainLoopLivenessPath {
    ModelFinalText,
    EngineFinalize,
}

#[derive(Clone)]
struct EvidenceMainLoopLivenessProvider {
    calls: Arc<AtomicUsize>,
    path: EvidenceMainLoopLivenessPath,
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceMainLoopLivenessProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(evidence_completion_response(vec![
                evidence_completion_register_call(
                    "register-main-loop-liveness-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                evidence_completion_edit_call(
                    "edit-with-probe-still-red",
                    "other.txt",
                    "old",
                    "changed",
                ),
            ]));
        }

        let should_finish =
            matches!(self.path, EvidenceMainLoopLivenessPath::ModelFinalText) && call >= 6;
        if should_finish {
            return Ok(evidence_completion_response(Vec::new()));
        }
        let command = match self.path {
            EvidenceMainLoopLivenessPath::ModelFinalText => {
                format!("printf 'liveness warmup {call}\\n'")
            }
            EvidenceMainLoopLivenessPath::EngineFinalize => "printf 'liveness warmup\\n'".into(),
        };
        Ok(evidence_completion_response(vec![ToolCall {
            id: format!("liveness-warmup-{call}"),
            call_type: "function".into(),
            function: FunctionCall {
                name: "shell_exec".into(),
                arguments: json!({ "command": command }).to_string(),
            },
        }]))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-main-loop-liveness")
    }
}

#[tokio::test]
async fn evidence_liveness_gate_releases_after_repeated_denials_through_main_loop() {
    for (path, run_id, max_turns, expected_calls, expected_via) in [
        (
            EvidenceMainLoopLivenessPath::ModelFinalText,
            "evidence-main-loop-liveness-a",
            10,
            10,
            "model_final_text",
        ),
        (
            EvidenceMainLoopLivenessPath::EngineFinalize,
            "evidence-main-loop-liveness-b",
            13,
            13,
            "engine_finalize",
        ),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let journal = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
        std::fs::write(workspace.path().join("other.txt"), "old\n").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut options = task_test_run_options(
            workspace.path(),
            journal.path(),
            run_id,
            crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
        );
        options.evidence_gate = EvidenceGate::On;
        options.max_turns = max_turns;
        options.max_eval_attempts = 3;

        let result = run_solo(
            EvidenceMainLoopLivenessProvider {
                calls: calls.clone(),
                path,
            },
            options,
        )
        .await
        .unwrap();

        assert_eq!(result.outcome, RunOutcome::Completed, "via={expected_via}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            expected_calls,
            "via={expected_via}"
        );
        let events =
            std::fs::read_to_string(RunPaths::new(journal.path(), run_id).events_path).unwrap();
        assert_eq!(
            events
                .lines()
                .filter(|line| {
                    line.contains("\"type\":\"completion.rejected\"")
                        && line.contains(&format!("\"via\":\"{expected_via}\""))
                })
                .count(),
            MAX_COMPLETION_DENIALS,
            "via={expected_via}\n{events}"
        );
        assert!(events.contains("\"reason\":\"completion_no_progress\""));
        assert!(events.contains("\"type\":\"evidence.gate.bypassed\""));
        assert!(events.contains("\"type\":\"run.completed\""));
        assert!(!events.contains("\"type\":\"run.needs_decision\""));
    }
}

#[tokio::test]
async fn evidence_completion_engine_finalize_accepts_current_green_and_bypassed() {
    let mut green = EvidenceState::new(EvidenceGate::On);
    green.accept_probe(evidence_completion_probe());
    green.note_edit();
    green.note_probe_green();

    let mut bypassed = EvidenceState::new(EvidenceGate::On);
    bypassed.bypassed = true;

    for (index, evidence) in [green, bypassed].iter_mut().enumerate() {
        let (outcome, events) =
            evidence_completion_try_finalize(evidence, &format!("evidence-ready-{index}")).await;
        assert_eq!(outcome, FinalizeOutcome::Completed);
        assert!(events.iter().any(|event| event["type"] == "run.completed"));
    }
}

#[derive(Clone)]
enum EvidenceCompletionPathAScenario {
    NoProbe,
    NoEdit,
    StillRed,
    Green,
    StaleGreen { marker: PathBuf },
    Bypassed,
}

#[derive(Clone)]
struct EvidenceCompletionPathAProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
    scenario: EvidenceCompletionPathAScenario,
}

fn evidence_completion_edit_call(id: &str, path: &str, old: &str, new: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "fs_edit".into(),
            arguments: json!({
                "path": path,
                "old_string": old,
                "new_string": new,
            })
            .to_string(),
        },
    }
}

fn evidence_completion_read_call(id: &str, path: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "fs_read".into(),
            arguments: json!({ "path": path }).to_string(),
        },
    }
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceCompletionPathAProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let expected_feedback = match self.scenario {
            EvidenceCompletionPathAScenario::NoProbe => Some(
                "You cannot finish: no confirmed-red reproduction was ever registered. Call register_issue_probe with a reproduction that fails on the current code.",
            ),
            EvidenceCompletionPathAScenario::NoEdit => {
                Some("You cannot finish: you have not changed any source code. Implement the required source-code fix before trying to finish.")
            }
            EvidenceCompletionPathAScenario::StillRed => Some(
                "You cannot finish: your frozen reproduction still fails. The bug is not fixed.",
            ),
            EvidenceCompletionPathAScenario::StaleGreen { .. } => Some(
                "You cannot finish: source changes after the last passing run invalidated that result, and the latest automatic re-run did not confirm a pass. Correct the implementation so the frozen reproduction passes.",
            ),
            EvidenceCompletionPathAScenario::Green
            | EvidenceCompletionPathAScenario::Bypassed => None,
        };
        if expected_feedback.is_some_and(|expected| {
            messages.iter().any(|message| {
                message.role == "user"
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains(expected))
            })
        }) {
            self.saw_feedback.store(1, Ordering::SeqCst);
        }

        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let ordinary_probe = || {
            evidence_completion_register_call(
                "register-completion-probe",
                "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'fixed\\n'; fi"
                    .into(),
            )
        };
        let tool_calls = match (&self.scenario, call) {
            (EvidenceCompletionPathAScenario::NoProbe, _) => Vec::new(),
            (EvidenceCompletionPathAScenario::NoEdit, 0) => vec![ordinary_probe()],
            (EvidenceCompletionPathAScenario::StillRed, 0) => vec![ordinary_probe()],
            (EvidenceCompletionPathAScenario::StillRed, 1) => vec![
                evidence_completion_read_call("read-unrelated", "other.txt"),
                evidence_completion_edit_call("edit-unrelated", "other.txt", "old", "new"),
            ],
            (EvidenceCompletionPathAScenario::Green, 0) => vec![
                ordinary_probe(),
                evidence_completion_read_call("read-target", "target.txt"),
                evidence_completion_edit_call("fix-target", "target.txt", "buggy", "fixed"),
            ],
            (EvidenceCompletionPathAScenario::StaleGreen { marker }, 0) => vec![
                evidence_completion_register_call(
                    "register-stale-probe",
                    format!(
                        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; elif [ ! -e '{}' ]; then touch '{}'; printf 'fixed\\n'; else printf 'ModuleNotFoundError: stale rerun\\n' >&2; exit 1; fi",
                        marker.display(),
                        marker.display(),
                    ),
                ),
                evidence_completion_read_call("read-before-stale", "target.txt"),
                evidence_completion_edit_call("fix-before-stale", "target.txt", "buggy", "fixed"),
            ],
            (EvidenceCompletionPathAScenario::StaleGreen { .. }, 1) => vec![
                evidence_completion_read_call("read-after-green", "other.txt"),
                evidence_completion_edit_call("edit-after-green", "other.txt", "old", "new"),
            ],
            (EvidenceCompletionPathAScenario::Bypassed, 0) => (0..MAX_FAILED_REGISTRATIONS)
                .map(|index| {
                    evidence_completion_register_call(
                        &format!("reject-probe-{index}"),
                        "printf 'already green\\n'".into(),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(evidence_completion_response(tool_calls))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-completion-path-a")
    }
}

async fn evidence_completion_run_path_a(
    scenario: EvidenceCompletionPathAScenario,
    run_id: &str,
) -> (RunOutcome, Vec<Value>, usize) {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    std::fs::write(workspace.path().join("other.txt"), "old\n").unwrap();
    let saw_feedback = Arc::new(AtomicUsize::new(0));
    let mut opts = task_test_run_options(workspace.path(), journal.path(), run_id, Vec::new());
    opts.evidence_gate = EvidenceGate::On;
    opts.max_turns = match &scenario {
        EvidenceCompletionPathAScenario::StillRed
        | EvidenceCompletionPathAScenario::StaleGreen { .. } => 4,
        _ => 3,
    };
    let result = run_solo(
        EvidenceCompletionPathAProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
            scenario,
        },
        opts,
    )
    .await
    .unwrap();
    let events = std::fs::read_to_string(RunPaths::new(journal.path(), run_id).events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (result.outcome, events, saw_feedback.load(Ordering::SeqCst))
}

#[tokio::test]
async fn evidence_completion_model_final_text_denies_unready_states_and_feeds_back() {
    let stale_dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            EvidenceCompletionPathAScenario::NoProbe,
            "evidence_no_probe_registered",
            0,
            Value::Null,
        ),
        (
            EvidenceCompletionPathAScenario::NoEdit,
            "evidence_no_edit_yet",
            0,
            Value::Null,
        ),
        (
            EvidenceCompletionPathAScenario::StillRed,
            "evidence_probe_still_red",
            1,
            Value::Null,
        ),
        (
            EvidenceCompletionPathAScenario::StaleGreen {
                marker: stale_dir.path().join("green-once"),
            },
            "evidence_stale_green",
            2,
            json!(1),
        ),
    ];

    for (index, (scenario, reason, edit_epoch, green_epoch)) in cases.into_iter().enumerate() {
        let (outcome, events, saw_feedback) =
            evidence_completion_run_path_a(scenario, &format!("evidence-path-a-{index}")).await;
        assert_ne!(outcome, RunOutcome::Completed);
        assert_eq!(saw_feedback, 1, "reason={reason}");
        let rejected = events
            .iter()
            .find(|event| {
                event["type"] == "completion.rejected" && event["payload"]["reason"] == reason
            })
            .expect("Path A evidence rejection should be observable");
        assert_eq!(rejected["payload"]["via"], "model_final_text");
        assert_eq!(rejected["payload"]["edit_epoch"], edit_epoch);
        assert_eq!(rejected["payload"]["green_epoch"], green_epoch);
        assert!(!events.iter().any(|event| event["type"] == "run.completed"));
    }
}

#[tokio::test]
async fn evidence_completion_model_final_text_accepts_current_green_and_bypassed() {
    for (index, scenario) in [
        EvidenceCompletionPathAScenario::Green,
        EvidenceCompletionPathAScenario::Bypassed,
    ]
    .into_iter()
    .enumerate()
    {
        let (outcome, events, _) =
            evidence_completion_run_path_a(scenario, &format!("evidence-path-a-ready-{index}"))
                .await;
        assert_eq!(outcome, RunOutcome::Completed);
        assert!(events.iter().any(|event| event["type"] == "run.completed"));
        assert!(!events
            .iter()
            .any(|event| event["type"] == "completion.rejected"));
    }
}
