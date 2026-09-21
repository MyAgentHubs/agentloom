use super::*;
use crate::goal::Approval;

const VALID: &str = r#"{ "tasks": [
      { "id": "t1", "intent": "add retry helper", "files_scope": ["src/provider/retry.rs"],
        "acceptance_cmd": "cargo test --manifest-path harness-agent/Cargo.toml retry", "max_turns": 12 }
    ] }"#;
const TWO_LANE: &str = r#"{ "tasks": [
      { "id": "t1", "intent": "add mcp_servers field", "files_scope": ["src", "tests"],
        "acceptance_cmd": "cargo test --manifest-path harness-agent/Cargo.toml mcp",
        "artifact_check_cmd": "grep -rq 'mcp_servers' harness-agent/src/orchestrator.rs",
        "max_turns": 8 }
    ] }"#;

#[test]
fn parses_minimal_task_and_builds_approved_acceptance() {
    let tasks = parse_worklist(VALID).expect("valid worklist parses");
    assert_eq!(tasks.len(), 1);
    let t = &tasks[0];
    assert_eq!(t.id, "t1");
    assert_eq!(t.files_scope, vec!["src/provider/retry.rs".to_string()]);
    assert_eq!(t.max_turns, 12);
    assert_eq!(t.status, TaskStatus::Pending);
    assert!(t.forbidden_scope.is_empty());
    assert!(t.depends_on.is_empty());
    assert_eq!(t.acceptance.approval, Approval::Approved);
    assert!(t.acceptance.is_executable_verifiable());
}

#[test]
fn parses_two_lanes_behavior_and_artifact() {
    let tasks = parse_worklist(TWO_LANE).expect("two-lane worklist parses");
    let t = &tasks[0];
    assert_eq!(t.acceptance.id, "t1_acc");
    assert!(t.acceptance.is_executable_verifiable());
    let art = t.artifact_check.as_ref().expect("artifact lane present");
    assert_eq!(art.id, "t1_art");
    assert!(art.is_executable_verifiable());
    match &art.verifier {
        crate::goal::Verifier::Verifiable { check_cmd, .. } => {
            assert!(check_cmd.contains("mcp_servers"));
        }
        _ => panic!("artifact must be Verifiable"),
    }
}

#[test]
fn legacy_single_acceptance_lands_behavior_artifact_none() {
    let tasks = parse_worklist(VALID).expect("legacy worklist parses");
    let t = &tasks[0];
    assert_eq!(t.acceptance.id, "t1_acc");
    assert!(t.artifact_check.is_none());
}

#[test]
fn plantask_serde_omits_artifact_when_none() {
    let tasks = parse_worklist(VALID).unwrap();
    let json = serde_json::to_string(&tasks[0]).unwrap();
    assert!(
        json.contains("\"acceptance\""),
        "behavior lane key stays 'acceptance'"
    );
    assert!(
        !json.contains("artifact_check"),
        "None artifact lane not serialized"
    );
}

#[test]
fn plantask_deserializes_old_state_without_artifact_key() {
    let tasks = parse_worklist(VALID).unwrap();
    let mut v = serde_json::to_value(&tasks[0]).unwrap();
    assert!(v.get("artifact_check").is_none());
    let back: PlanTask = serde_json::from_value(v.take()).unwrap();
    assert!(back.artifact_check.is_none());
    assert_eq!(back.acceptance.id, "t1_acc");
}

#[test]
fn parse_worklist_sets_remediation_none_for_normal_tasks() {
    let tasks = parse_worklist(VALID).expect("valid worklist parses");
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0].remediation.is_none());
}

#[test]
fn parse_worklist_defaults_acceptance_kind_to_change_required() {
    let tasks = parse_worklist(VALID).expect("valid worklist parses");
    assert_eq!(tasks[0].acceptance_kind, AcceptanceKind::ChangeRequired);
}

#[test]
fn parse_worklist_reads_explicit_invariant_acceptance_kind() {
    let json = r#"{ "tasks": [
          { "id": "t1", "intent": "keep build green", "files_scope": ["src/lib.rs"],
            "acceptance_cmd": "cargo build", "max_turns": 4, "acceptance_kind": "invariant" }
        ] }"#;
    let tasks = parse_worklist(json).expect("valid worklist parses");
    assert_eq!(tasks[0].acceptance_kind, AcceptanceKind::Invariant);
}

#[test]
fn acceptance_kind_round_trips_on_plan_task() {
    let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
    task.acceptance_kind = AcceptanceKind::Invariant;
    let json = serde_json::to_string(&task).unwrap();
    assert!(json.contains("\"acceptance_kind\""));
    let back: PlanTask = serde_json::from_str(&json).unwrap();
    assert_eq!(back.acceptance_kind, AcceptanceKind::Invariant);
}

#[test]
fn legacy_plan_task_without_acceptance_kind_defaults_to_change_required() {
    let task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
    let mut value = serde_json::to_value(task).unwrap();
    value.as_object_mut().unwrap().remove("acceptance_kind");
    let back: PlanTask = serde_json::from_value(value).unwrap();
    assert_eq!(back.acceptance_kind, AcceptanceKind::ChangeRequired);
}

#[test]
fn superseded_status_round_trips() {
    let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
    task.status = TaskStatus::Superseded {
        by: vec!["t1_r1_fix1".into()],
        reason: "acceptance_passed_before_execution".into(),
    };
    let json = serde_json::to_string(&task).unwrap();
    let back: PlanTask = serde_json::from_str(&json).unwrap();
    assert_eq!(back.status, task.status);
}

#[test]
fn rejected_acceptance_status_round_trips() {
    let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
    task.status = TaskStatus::RejectedAcceptance {
        reason: "preflight_refine_exhausted".into(),
    };
    let json = serde_json::to_string(&task).unwrap();
    let back: PlanTask = serde_json::from_str(&json).unwrap();
    assert_eq!(back.status, task.status);
}

#[test]
fn advisory_note_serde_round_trips() {
    let note = AdvisoryNote {
        lane: "artifact".into(),
        result: AdvisoryResult::CodeRed,
        detail: Some("grep 未匹配".into()),
        evidence: None,
    };
    let json = serde_json::to_string(&note).unwrap();
    let back: AdvisoryNote = serde_json::from_str(&json).unwrap();
    assert_eq!(note, back);
    assert!(
        json.contains("\"result\":\"code_red\""),
        "result 应是 snake_case enum"
    );
}

#[test]
fn passed_by_acceptance_without_advisory_omits_key_zero_drift() {
    let ev = CommandEvidence {
        role: CommandRole::AuthoritativeAcceptance,
        criterion_id: "t1_acc".into(),
        command: "cargo test".into(),
        exit_code: Some(0),
        success: true,
        timed_out: false,
        stdout_summary: String::new(),
        stderr_summary: String::new(),
        truncated: false,
        environment_failure: None,
    };
    let d = TaskDecision::PassedByAcceptance {
        acceptance: ev,
        advisory: None,
    };
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(
        v.get("kind").and_then(|k| k.as_str()),
        Some("passed_by_acceptance")
    );
    assert!(
        v.get("advisory").is_none(),
        "advisory=None 必须不写出·保 golden 零漂移"
    );
}

#[test]
fn remediation_meta_round_trips_on_plan_task() {
    let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
    task.remediation = Some(RemediationMeta {
        parent: "t1".into(),
        evidence_fingerprint: "cargo test:E0063:src/lib.rs:37".into(),
        attempt_no: 1,
        round: 2,
    });

    let json = serde_json::to_string(&task).unwrap();
    assert!(json.contains("\"remediation\""));
    let back: PlanTask = serde_json::from_str(&json).unwrap();

    assert_eq!(back.remediation, task.remediation);
}

#[test]
fn legacy_plan_task_without_remediation_defaults_to_none() {
    let task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
    let mut value = serde_json::to_value(task).unwrap();
    value.as_object_mut().unwrap().remove("remediation");

    let back: PlanTask = serde_json::from_value(value).unwrap();
    assert!(back.remediation.is_none());
}

#[test]
fn missing_required_field_errors() {
    let bad = r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a"] } ] }"#;
    assert!(parse_worklist(bad).is_err());
}

#[test]
fn malformed_json_errors() {
    assert!(parse_worklist("not json").is_err());
}

fn command_ev(success: bool) -> CommandEvidence {
    CommandEvidence {
        role: CommandRole::AuthoritativeAcceptance,
        criterion_id: "t1_acc".into(),
        command: "true".into(),
        exit_code: if success { Some(0) } else { Some(1) },
        success,
        timed_out: false,
        stdout_summary: String::new(),
        stderr_summary: String::new(),
        truncated: false,
        environment_failure: None,
    }
}

fn pass_ev(id: &str) -> CommandEvidence {
    let mut e = command_ev(true);
    e.criterion_id = id.into();
    e
}

fn red_ev(id: &str) -> CommandEvidence {
    let mut e = command_ev(false);
    e.criterion_id = id.into();
    e
}

fn report_with_changes(changes: ChangeSet) -> TaskReport {
    TaskReport {
        schema_version: TASK_REPORT_SCHEMA_VERSION,
        task_id: "t1".into(),
        child_run_id: "plan__t1".into(),
        status: TaskReportStatus::BlockedCandidate,
        acceptance: harness_approved_criterion("t1", "true"),
        child_outcome: ChildRunOutcome::Blocked,
        child_evaluation: None,
        stop: None,
        changes,
        evidence: vec![],
        narrative: TaskNarrative::default(),
    }
}

#[test]
fn decide_ignores_report_status_when_acceptance_passes() {
    let report = report_with_changes(ChangeSet::default());
    let d = decide_task(
        &report,
        AcceptanceResult::Pass {
            acceptance: command_ev(true),
        },
    );
    assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
    assert_eq!(d.task_status(), Some(TaskStatus::Done));
}

#[test]
fn behavior_pass_artifact_code_red_is_done_with_advisory() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::CodeRed {
            acceptance: red_ev("t1_art"),
        }),
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    match d {
        TaskDecision::PassedByAcceptance {
            advisory: Some(n), ..
        } => {
            assert_eq!(n.result, AdvisoryResult::CodeRed);
            assert_eq!(n.lane, "artifact");
            assert!(n.evidence.is_some());
        }
        other => panic!("expected Done+advisory, got {other:?}"),
    }
}

#[test]
fn behavior_pass_artifact_none_is_done() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        None,
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
    assert_eq!(d.task_status(), Some(TaskStatus::Done));
}

#[test]
fn behavior_pass_artifact_pass_is_done() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::Pass {
            acceptance: pass_ev("t1_art"),
        }),
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
}

#[test]
fn behavior_code_red_wins_over_any_artifact() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::InfraRed {
            signature: "net".into(),
            acceptance: None,
        }),
        AcceptanceResult::CodeRed {
            acceptance: red_ev("t1_acc"),
        },
    );
    assert!(matches!(d, TaskDecision::FailedByAcceptance { .. }));
}

#[test]
fn behavior_pass_artifact_not_run_is_done_with_advisory_not_run() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::NotRun {
            reason: "未批".into(),
        }),
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    match d {
        TaskDecision::PassedByAcceptance {
            advisory: Some(n), ..
        } => {
            assert_eq!(n.result, AdvisoryResult::NotRun);
            assert_eq!(n.detail.as_deref(), Some("未批"));
            assert!(n.evidence.is_none());
        }
        other => panic!("expected Done+advisory(not_run), got {other:?}"),
    }
}

#[test]
fn behavior_pass_artifact_infra_red_is_done_with_advisory_infra() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::InfraRed {
            signature: "operation timed out".into(),
            acceptance: Some(red_ev("t1_art")),
        }),
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    match d {
        TaskDecision::PassedByAcceptance {
            advisory: Some(n), ..
        } => {
            assert_eq!(n.result, AdvisoryResult::InfraRed);
            assert_eq!(n.detail.as_deref(), Some("operation timed out"));
            assert!(n.evidence.is_some());
        }
        other => panic!("expected Done+advisory(infra_red), got {other:?}"),
    }
}

#[test]
fn behavior_pass_artifact_pass_or_none_is_done_no_advisory() {
    let report = report_with_changes(ChangeSet::default());
    for artifact in [
        None,
        Some(AcceptanceResult::Pass {
            acceptance: pass_ev("t1_art"),
        }),
    ] {
        let d = merge_task_acceptance(
            &report,
            artifact,
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        assert!(
            matches!(d, TaskDecision::PassedByAcceptance { advisory: None, .. }),
            "{d:?}"
        );
    }
}

#[test]
fn behavior_code_red_still_fails_regardless_of_artifact() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::Pass {
            acceptance: pass_ev("t1_art"),
        }),
        AcceptanceResult::CodeRed {
            acceptance: red_ev("t1_acc"),
        },
    );
    assert!(
        matches!(d, TaskDecision::FailedByAcceptance { .. }),
        "{d:?}"
    );
}

#[test]
fn scope_violation_is_policy_even_if_both_pass() {
    let report = report_with_changes(ChangeSet {
        changed_files: vec!["src/a.rs".into(), "src/secret.rs".into()],
        scope_violations: vec![ScopeViolation {
            path: "src/secret.rs".into(),
            reason: "超出 files_scope".into(),
        }],
    });
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::Pass {
            acceptance: pass_ev("t1_art"),
        }),
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    assert!(matches!(d, TaskDecision::FailedByPolicy { .. }));
}

#[test]
fn artifact_policy_failure_is_policy() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::PolicyFailure {
            reason: "wrote".into(),
            changed_files: vec!["x".into()],
            acceptance: None,
        }),
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    assert!(matches!(d, TaskDecision::FailedByPolicy { .. }));
}

#[test]
fn artifact_policy_with_behavior_code_red_keeps_behavior_evidence() {
    let report = report_with_changes(ChangeSet::default());
    let d = merge_task_acceptance(
        &report,
        Some(AcceptanceResult::PolicyFailure {
            reason: "wrote".into(),
            changed_files: vec!["x".into()],
            acceptance: None,
        }),
        AcceptanceResult::CodeRed {
            acceptance: red_ev("t1_acc"),
        },
    );
    match d {
        TaskDecision::FailedByPolicy {
            acceptance: Some(ev),
            ..
        } => assert_eq!(ev.criterion_id, "t1_acc"),
        other => panic!("expected FailedByPolicy with behavior evidence, got {other:?}"),
    }
}

#[test]
fn decide_task_is_behavior_only_merge() {
    let report = report_with_changes(ChangeSet::default());
    let d = decide_task(
        &report,
        AcceptanceResult::Pass {
            acceptance: pass_ev("t1_acc"),
        },
    );
    assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
}

#[test]
fn decide_code_red_is_failed_by_acceptance() {
    let report = report_with_changes(ChangeSet::default());
    let d = decide_task(
        &report,
        AcceptanceResult::CodeRed {
            acceptance: command_ev(false),
        },
    );
    assert!(matches!(d, TaskDecision::FailedByAcceptance { .. }));
    assert!(matches!(d.task_status(), Some(TaskStatus::Blocked { .. })));
}

#[test]
fn decide_infra_and_not_run_are_unvalidated_not_blocked() {
    let report = report_with_changes(ChangeSet::default());
    let infra = decide_task(
        &report,
        AcceptanceResult::InfraRed {
            signature: "connection refused".into(),
            acceptance: Some(command_ev(false)),
        },
    );
    assert!(matches!(infra, TaskDecision::UnvalidatedInfraError { .. }));
    assert_eq!(infra.task_status(), None);

    let stopped = decide_task(
        &report,
        AcceptanceResult::NotRun {
            reason: "blocked by escape_scan".into(),
        },
    );
    assert!(matches!(stopped, TaskDecision::StoppedUnvalidated { .. }));
    assert_eq!(stopped.task_status(), None);
}

#[test]
fn write_audit_violation_is_failed_by_policy_even_if_acceptance_passes() {
    let report = report_with_changes(ChangeSet {
        changed_files: vec!["src/a.rs".into(), "src/secret.rs".into()],
        scope_violations: vec![ScopeViolation {
            path: "src/secret.rs".into(),
            reason: "超出 files_scope 白名单：src/secret.rs".into(),
        }],
    });
    let d = decide_task(
        &report,
        AcceptanceResult::Pass {
            acceptance: command_ev(true),
        },
    );
    assert!(matches!(d, TaskDecision::FailedByPolicy { .. }));
    match d.task_status() {
        Some(TaskStatus::Blocked { reason }) => assert!(reason.contains("failed_by_policy")),
        other => panic!("expected policy blocked status, got {other:?}"),
    }
}

#[test]
fn read_only_delta_acceptance_result_maps_to_failed_by_policy() {
    let report = report_with_changes(ChangeSet::default());
    let d = decide_task(
        &report,
        AcceptanceResult::PolicyFailure {
            reason: "acceptance_read_only_violation".into(),
            changed_files: vec!["a.rs".into()],
            acceptance: Some(command_ev(true)),
        },
    );
    match d {
        TaskDecision::FailedByPolicy {
            violations,
            acceptance,
        } => {
            assert_eq!(violations[0].path, "a.rs");
            assert!(violations[0]
                .reason
                .contains("acceptance_read_only_violation"));
            assert!(acceptance.is_some());
        }
        other => panic!("expected FailedByPolicy, got {other:?}"),
    }
}
