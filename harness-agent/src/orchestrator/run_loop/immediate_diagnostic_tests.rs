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
