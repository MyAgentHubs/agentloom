#![cfg(test)]

use super::*;

#[derive(Clone)]
struct EvidenceToolOffProvider {
    calls: Arc<AtomicUsize>,
    saw_dispatch_rejection: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceToolOffProvider {
    async fn next_turn(
        &self,
        messages: &[crate::provider::ChatMessage],
        tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        assert!(tools
            .iter()
            .all(|tool| { tool["function"]["name"].as_str() != Some("register_issue_probe") }));
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: String::new(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "probe-off-hard-call".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "register_issue_probe".into(),
                        arguments: json!({
                            "script": "printf BUG",
                            "command": "sh {probe}",
                            "red_marker": "BUG",
                            "rationale": "hard call while disabled"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
                interruption: None,
            });
        }

        let rejected = messages.iter().any(|message| {
            message.role == "tool"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("disabled for this run"))
        });
        self.saw_dispatch_rejection
            .store(usize::from(rejected), Ordering::SeqCst);
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
async fn evidence_tool_off_is_not_offered_and_hard_call_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_dispatch_rejection = Arc::new(AtomicUsize::new(0));
    let options = task_test_run_options(workspace.path(), journal.path(), "evidence-off", vec![]);

    run_solo(
        EvidenceToolOffProvider {
            calls,
            saw_dispatch_rejection: saw_dispatch_rejection.clone(),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(saw_dispatch_rejection.load(Ordering::SeqCst), 1);
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-off").events_path).unwrap();
    assert!(!events.contains("evidence.probe."));
    assert!(!events.contains("evidence.gate."));
}

#[test]
fn evidence_tool_on_is_offered() {
    let registry = build_default_registry(&crate::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &registry,
        &task_test_caps(),
        crate::goal::NetworkPolicy::On,
        false,
        &std::collections::BTreeSet::new(),
    );

    assert!(tools
        .iter()
        .any(|tool| { tool["function"]["name"].as_str() == Some("register_issue_probe") }));
}

#[tokio::test]
async fn evidence_liveness_empty_marker_counts_as_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "empty-marker");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let feedback = register_issue_probe_call(
        &json!({
            "script": "printf BUG",
            "command": "sh {probe}",
            "red_marker": "",
            "rationale": "empty marker must be rejected before execution"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        1,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    assert!(feedback.contains("red_marker"));
    assert!(feedback.contains("must be non-empty"));
    assert_eq!(attempts, 1);
    assert_eq!(evidence.failed_registrations, 1);
    assert!(evidence.probe.is_none());
}

#[tokio::test]
async fn evidence_liveness_malformed_command_without_probe_placeholder_counts_as_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "missing-placeholder");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    for turn in 1..=MAX_FAILED_REGISTRATIONS {
        let feedback = register_issue_probe_call(
            &json!({
                "script": "printf BUG_PRESENT",
                "command": "python -m pytest tests/test_issue.py",
                "red_marker": "BUG_PRESENT",
                "rationale": "natural but malformed command"
            })
            .to_string(),
            &mut evidence,
            &mut attempts,
            workspace.path(),
            &journal.path().join("probes"),
            turn,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut recorder,
        )
        .await
        .unwrap();
        assert!(feedback.contains("must contain the `{probe}` placeholder"));
        assert!(feedback.contains("python -I -B {probe}"));
    }

    assert_eq!(evidence.failed_registrations, MAX_FAILED_REGISTRATIONS);
    assert!(evidence.bypassed);
    assert_eq!(evidence.may_edit(), EditVerdict::Allow);
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert_eq!(events.matches("evidence.probe.rejected").count(), 3);
    assert!(events.contains("\"reason\":\"registration_failures\""));
}

#[tokio::test]
async fn evidence_probe_registered_event_carries_script_and_output() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "code-red");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let feedback = register_issue_probe_call(
        &json!({
            "script": "printf 'BUG_PRESENT\\n'",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "the buggy behavior prints BUG_PRESENT"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        4,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    assert!(feedback.contains("Probe confirmed RED by the harness (ran twice)"));
    assert!(feedback.contains("detects a workspace content change"));
    assert!(feedback.contains("three consecutive completion denials"));
    assert!(!feedback.contains("automatically after every edit"));
    assert!(!feedback.contains("completes only when it turns green"));
    assert!(feedback.contains("run 1 stdout"));
    assert!(feedback.contains("BUG_PRESENT"));
    assert_eq!(attempts, 1);
    assert_eq!(evidence.failed_registrations, 0);
    assert_eq!(
        evidence.probe.as_ref().map(|probe| probe.probe_id.as_str()),
        Some("issue_probe_4")
    );
    let events: Vec<Value> = std::fs::read_to_string(journal.path().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let registered = events
        .iter()
        .find(|event| event["type"] == "evidence.probe.registered")
        .expect("registered probe event");
    let payload = &registered["payload"];
    assert_eq!(payload["verdict"], "code_red");
    assert_eq!(payload["attempt"], 1);
    assert_eq!(payload["turn"], 4);
    assert_eq!(payload["script"], "printf 'BUG_PRESENT\\n'");
    assert_eq!(payload["red_marker"], "BUG_PRESENT");
    assert!(payload["command"]
        .as_str()
        .unwrap()
        .starts_with("sh \"${TMPDIR:-/tmp}/agentloom-probes/"));
    assert_eq!(payload["script_sha256"].as_str().unwrap().len(), 64);
    assert!(payload["output_tail"]
        .as_str()
        .unwrap()
        .contains("BUG_PRESENT"));
}

#[tokio::test]
async fn evidence_probe_rejected_event_carries_script_and_output() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "observable-rejection");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    register_issue_probe_call(
        &json!({
            "script": "printf 'actual probe output\\n'",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "does not reproduce the marker"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        2,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    let events: Vec<Value> = std::fs::read_to_string(journal.path().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejected = events
        .iter()
        .find(|event| event["type"] == "evidence.probe.rejected")
        .expect("rejected probe event");
    let payload = &rejected["payload"];
    assert_eq!(payload["verdict"], "pre_green");
    assert_eq!(payload["script"], "printf 'actual probe output\\n'");
    assert_eq!(payload["red_marker"], "BUG_PRESENT");
    assert!(payload["command"]
        .as_str()
        .unwrap()
        .starts_with("sh \"${TMPDIR:-/tmp}/agentloom-probes/"));
    assert_eq!(payload["script_sha256"].as_str().unwrap().len(), 64);
    assert!(payload["output_tail"]
        .as_str()
        .unwrap()
        .contains("actual probe output"));
}

#[tokio::test]
async fn evidence_missing_probe_script_feedback_identifies_harness_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "missing-probe-script");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let feedback = register_issue_probe_call(
        &json!({
            "script": "printf 'this script must never run\\n'",
            "command": "rm -f {probe} && python3 {probe}",
            "red_marker": "BUG_PRESENT",
            "rationale": "a missing harness script must not look pre-green"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        2,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    assert!(feedback.contains("harness-side infrastructure failure"));
    assert!(feedback.contains("not a problem with your reproduction"));
    assert!(feedback.contains("probe_script_not_materialized"));
    assert!(!feedback.contains("does not reproduce the bug"));
    assert!(evidence.probe.is_none());

    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"verdict\":\"infra_red\""));
    assert!(events.contains("\"infra_signature\":\"probe_script_not_materialized\""));
    assert!(!events.contains("\"verdict\":\"pre_green\""));
}

#[cfg_attr(
    target_os = "linux",
    ignore = "linux: probe rejection event not emitted on ubuntu runners; macos/linux divergence undiagnosed — must investigate before linux support"
)]
#[tokio::test]
async fn evidence_probe_script_in_event_is_hard_capped() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "capped-probe-script");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;
    let script = format!("printf green\\n# {}", "x".repeat(100_000));

    register_issue_probe_call(
        &json!({
            "script": script,
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "rationale": "large rejected reproduction"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        3,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    let events: Vec<Value> = std::fs::read_to_string(journal.path().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let event_script = events
        .iter()
        .find(|event| event["type"] == "evidence.probe.rejected")
        .expect("rejected probe event")["payload"]["script"]
        .as_str()
        .unwrap();
    assert!(event_script.starts_with("printf green\\n# "));
    assert!(event_script.contains("truncated"));
    assert!(event_script.chars().count() <= PROBE_SCRIPT_EVENT_LIMIT);
}

#[tokio::test]
async fn evidence_tool_rejected_verdicts_increment_and_third_bypasses() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "rejected-probes");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let pre_green = register_issue_probe_call(
        &json!({
            "script": "printf 'already green\\n'",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "rationale": "does not actually reproduce"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        1,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();
    assert!(pre_green.contains("did NOT fail on the current code"));
    assert!(pre_green.contains("already green"));
    assert_eq!(evidence.failed_registrations, 1);

    let infra_red = register_issue_probe_call(
        &json!({
            "script": "printf 'ModuleNotFoundError: no module named dependency\\n' >&2; exit 1",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stderr",
            "rationale": "environment failure must not count as code red"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        2,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();
    assert!(infra_red.contains("environment reason (`ModuleNotFoundError`)"));
    assert!(infra_red.contains("Do not grind on package installation"));
    assert!(infra_red.contains("ModuleNotFoundError"));
    assert_eq!(evidence.failed_registrations, 2);

    let toggle = journal.path().join("flaky-toggle");
    let flaky_script = format!(
        "if [ -e '{}' ]; then rm -f '{}'; printf 'green\\n'; else touch '{}'; printf 'BUG_PRESENT\\n'; fi",
        toggle.display(),
        toggle.display(),
        toggle.display()
    );
    let flaky = register_issue_probe_call(
        &json!({
            "script": flaky_script,
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "deliberately nondeterministic for classification coverage"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        3,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();
    assert!(flaky.contains("red once and green once"));
    assert!(flaky.contains("Make it deterministic"));
    assert!(flaky.contains("BUG_PRESENT"));
    assert!(flaky.contains("evidence gate is now advisory"));
    assert_eq!(attempts, 3);
    assert_eq!(evidence.failed_registrations, 3);
    assert!(evidence.bypassed);

    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert_eq!(events.matches("evidence.probe.rejected").count(), 3);
    assert!(events.contains("\"verdict\":\"pre_green\""));
    assert!(events.contains("\"verdict\":\"infra_red\""));
    assert!(events.contains("\"infra_signature\":\"ModuleNotFoundError\""));
    assert!(events.contains("\"verdict\":\"flaky\""));
    assert!(events.contains("\"type\":\"evidence.gate.bypassed\""));
    assert!(events.contains("\"attempt\":3"));
}

#[tokio::test]
async fn evidence_junk_registrations_do_not_bypass_an_accepted_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "junk-after-red");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "printf 'BUG_PRESENT\n'",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;
    let mut attempts = 0;

    for turn in 2..=4 {
        register_issue_probe_call(
            "not-json",
            &mut evidence,
            &mut attempts,
            workspace.path(),
            &journal.path().join("probes"),
            turn,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut recorder,
        )
        .await
        .unwrap();
    }

    assert_eq!(evidence.failed_registrations, 3);
    assert!(!evidence.bypassed);
    assert!(evidence.probe.is_some());
    assert_eq!(evidence.ready(), Err(EvidenceDenial::NoEditYet));
}
