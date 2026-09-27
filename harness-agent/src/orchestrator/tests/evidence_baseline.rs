#![cfg(test)]

use super::*;

#[tokio::test]
async fn evidence_edit_success_increments_epoch_and_green_probe_counts() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-green");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'fixed\\n'; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 1);
    assert_eq!(evidence.green_epoch, Some(1));
    assert!(feedback.contains("now PASSES"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"outcome\":\"green\""));
    assert!(events.contains("\"edit_epoch\":1"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[tokio::test]
async fn evidence_edit_still_red_clears_green_and_emits_output() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-red");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "printf 'BUG_PRESENT still broken\\n'",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 1);
    assert_eq!(evidence.green_epoch, None);
    assert!(feedback.contains("still FAILS"));
    assert!(feedback.contains("still broken"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.still_red\""));
}

#[tokio::test]
async fn evidence_edit_second_edit_invalidates_old_green_on_infra() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-stale");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "case \"$(cat target.txt)\" in buggy*) printf 'BUG_PRESENT\\n';; fixed*) printf 'fixed\\n';; *) printf 'ModuleNotFoundError: missing dependency\\n' >&2; exit 1;; esac",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap();
    assert_eq!(evidence.green_epoch, Some(1));

    std::fs::write(workspace.path().join("target.txt"), "infra\n").unwrap();
    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        3,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 2);
    assert_eq!(evidence.green_epoch, Some(1));
    assert_ne!(evidence.green_epoch, Some(evidence.edit_epoch));
    assert!(feedback.contains("environment problem"));
    assert!(feedback.contains("Do not grind on package installation"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.infra\""));
    assert!(events.contains("\"signature\":\"ModuleNotFoundError\""));
    assert!(events.contains("\"edit_epoch\":2"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[tokio::test]
async fn evidence_liveness_repeated_infra_rerun_discards_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "infra-discard");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'ModuleNotFoundError: missing dependency\n' >&2; exit 1; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "infra-one\n").unwrap();
    let first = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(first.contains("environment problem"));
    assert!(evidence.probe.is_some());

    std::fs::write(workspace.path().join("target.txt"), "infra-two\n").unwrap();
    let second = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        3,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert!(second.contains("no longer evidence and has been discarded"));
    assert!(second.contains("Register a new one"));
    assert!(evidence.probe.is_none());
    assert_eq!(evidence.failed_registrations, 1);
    assert_eq!(evidence.green_epoch, None);
}

#[tokio::test]
async fn evidence_baseline_refreshes_after_legit_edit() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let mut recorder = evidence_tool_recorder(journal.path(), "baseline-refresh");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap();

    let mut attempts = 0;
    let feedback = register_issue_probe_call(
        &json!({
            "script": "if grep -q fixed target.txt; then printf 'NEW_BUG\n'; fi",
            "command": "sh {probe}",
            "red_marker": "NEW_BUG",
            "marker_stream": "stdout",
            "rationale": "register again after a legitimate edit"
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

    assert!(feedback.contains("Probe confirmed RED"), "{feedback}");
    assert!(!feedback.contains("modified the workspace"));
    assert!(evidence.probe.is_some());
}

#[tokio::test]
async fn evidence_edit_workspace_mutation_is_not_green_and_keeps_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-mutated");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'probe write\\n' >> probe-mutated.txt; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 1);
    assert_eq!(evidence.green_epoch, None);
    assert!(evidence.probe.is_some());
    assert!(feedback.contains("wrote to the workspace"));
    assert!(feedback.contains("does not count as green"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.workspace_mutated\""));
    assert!(events.contains("\"outcome\":\"workspace_mutated\""));
}

#[tokio::test]
async fn evidence_edit_off_skips_epoch_probe_and_events_entirely() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-off");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "printf 'BUG_PRESENT\\n'",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;
    evidence.mode = EvidenceGate::Off;

    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap();

    assert_eq!(feedback, None);
    assert_eq!(evidence.edit_epoch, 0);
    assert_eq!(evidence.green_epoch, None);
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(!events.contains("evidence.probe.green"));
    assert!(!events.contains("evidence.probe.still_red"));
    assert!(!events.contains("evidence.probe.infra"));
    assert!(!events.contains("evidence.probe.workspace_mutated"));
}

#[tokio::test]
async fn evidence_gate_off_skips_workspace_edit_detection() {
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "off-no-status");
    let mut evidence = EvidenceState::new(EvidenceGate::Off);
    evidence.probe = Some(evidence_completion_probe());
    let missing_workspace = journal.path().join("does-not-exist");

    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        &missing_workspace,
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        1,
        &mut recorder,
    )
    .await
    .unwrap();

    assert_eq!(feedback, None);
    assert_eq!(evidence.edit_epoch, 0);
    assert_eq!(evidence.green_epoch, None);
}
