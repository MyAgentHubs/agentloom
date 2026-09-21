use super::*;
use crate::plan::contract::{CommandEvidence, CommandRole};

fn ev(stderr: &str, truncated: bool) -> CommandEvidence {
    CommandEvidence {
        role: CommandRole::AuthoritativeAcceptance,
        criterion_id: "t1_acc".into(),
        command: "cargo test --manifest-path harness-agent/Cargo.toml".into(),
        exit_code: Some(101),
        success: false,
        timed_out: false,
        stdout_summary: String::new(),
        stderr_summary: stderr.into(),
        truncated,
        environment_failure: None,
    }
}

fn single_task(json: &str) -> crate::plan::contract::PlanTask {
    crate::plan::contract::parse_worklist(json)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
}

fn state_with_worklist(
    tasks: Vec<crate::plan::contract::PlanTask>,
) -> crate::plan::state::RunState {
    crate::plan::state::RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    )
}

fn code_red_evidence(id: &str) -> crate::plan::contract::CommandEvidence {
    crate::plan::contract::CommandEvidence {
        role: crate::plan::contract::CommandRole::AuthoritativeAcceptance,
        criterion_id: id.into(),
        command: "cargo test".into(),
        exit_code: Some(101),
        success: false,
        timed_out: false,
        stdout_summary: String::new(),
        stderr_summary: format!("error[E0063]\n --> src/lib.rs:{id}:9"),
        truncated: false,
        environment_failure: None,
    }
}

#[test]
fn failure_fingerprint_is_stable_for_same_evidence() {
    let e = ev(
        "error[E0063]: missing field `c`\n --> src/lib.rs:37:9",
        false,
    );

    assert_eq!(failure_fingerprint(&e), failure_fingerprint(&e));
    assert!(failure_fingerprint(&e).contains("E0063"));
    assert!(failure_fingerprint(&e).contains("src/lib.rs:37"));
}

#[test]
fn truncated_evidence_is_marked_and_not_hard_dedup_safe() {
    let e = ev(
        "error[E0063]: missing field `c`\n --> src/lib.rs:37:9",
        true,
    );

    let fp = failure_fingerprint(&e);

    assert!(fp.contains("truncated=true"));
    assert!(!fingerprint_hard_dedup_safe(&e));
}

#[test]
fn different_file_line_changes_fingerprint() {
    let a = ev(
        "error[E0063]: missing field `c`\n --> src/lib.rs:37:9",
        false,
    );
    let b = ev(
        "error[E0063]: missing field `c`\n --> src/other.rs:41:9",
        false,
    );

    assert_ne!(failure_fingerprint(&a), failure_fingerprint(&b));
}

#[test]
fn canonical_task_hash_ignores_task_id_and_acceptance_id() {
    let a = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "fix missing field",
              "files_scope": ["src/lib.rs", "src/main.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
    )
    .unwrap()
    .into_iter()
    .next()
    .unwrap();

    let mut b = a.clone();
    b.id = "different_id".into();
    b.acceptance.id = "different_id_acc".into();
    b.files_scope.reverse();

    assert_eq!(canonical_task_hash(&a), canonical_task_hash(&b));
    assert!(is_duplicate_task(&b, &[a]));
}

#[test]
fn canonical_task_hash_changes_when_scope_changes() {
    let a = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "fix missing field",
              "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
    )
    .unwrap()
    .into_iter()
    .next()
    .unwrap();

    let b = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t2", "intent": "fix missing field",
              "files_scope": ["src/other.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
    )
    .unwrap()
    .into_iter()
    .next()
    .unwrap();

    assert_ne!(canonical_task_hash(&a), canonical_task_hash(&b));
    assert!(!is_duplicate_task(&b, &[a]));
}

#[test]
fn canonical_hash_distinguishes_artifact() {
    let base = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "grep -rq foo src", "max_turns": 5 } ] }"#;
    let other = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "grep -rq bar src", "max_turns": 5 } ] }"#;
    let none = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 5, "acceptance_kind": "invariant" } ] }"#;
    let h = |json: &str| {
        let task = crate::plan::contract::parse_worklist(json)
            .unwrap()
            .remove(0);
        canonical_task_hash(&task)
    };
    assert_ne!(h(base), h(other), "artifact 命令不同→hash 不同");
    assert_ne!(h(base), h(none), "有 artifact vs None→hash 不同");
}

#[test]
fn canonical_hash_ignores_artifact_criterion_id() {
    let json = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "grep -rq foo src", "max_turns": 5 } ] }"#;
    let task = crate::plan::contract::parse_worklist(json)
        .unwrap()
        .remove(0);
    let mut mutated = task.clone();
    mutated.artifact_check.as_mut().unwrap().id = "different_id".to_string();
    assert_eq!(
        canonical_task_hash(&task),
        canonical_task_hash(&mutated),
        "artifact criterion id 不进 hash"
    );
}

#[test]
fn validate_remediation_rejects_dependency_on_blocked_parent() {
    let mut parent = single_task(
        r#"{ "tasks": [ { "id": "t1", "intent": "parent", "files_scope": ["a.rs"],
              "acceptance_cmd": "false", "max_turns": 3 } ] }"#,
    );
    parent.status = crate::plan::contract::TaskStatus::Blocked {
        reason: "failed_by_acceptance: t1_acc".into(),
    };

    let candidate = single_task(
        r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "fix", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "max_turns": 3, "depends_on": ["t1"] } ] }"#,
    );

    let err =
        validate_remediation_append(&[candidate], &[parent], &std::collections::HashSet::new())
            .expect_err("blocked parent dep must be rejected");

    assert!(err
        .iter()
        .any(|r| r.contains("depends_on") && r.contains("not Done")));
}

#[test]
fn validate_remediation_rejects_id_collision_with_existing_worklist() {
    let existing = single_task(
        r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "old", "files_scope": ["old.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
    );
    let candidate = single_task(
        r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "new", "files_scope": ["new.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
    );

    let err =
        validate_remediation_append(&[candidate], &[existing], &std::collections::HashSet::new())
            .expect_err("id collision must be rejected");

    assert!(err
        .iter()
        .any(|r| r.contains("id") && r.contains("existing")));
}

#[test]
fn validate_remediation_allows_dependency_on_done_task() {
    let mut done = single_task(
        r#"{ "tasks": [ { "id": "t0", "intent": "done", "files_scope": ["done.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
    );
    done.status = crate::plan::contract::TaskStatus::Done;

    let candidate = single_task(
        r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "fix", "files_scope": ["fix.rs"],
              "acceptance_cmd": "true", "max_turns": 3, "depends_on": ["t0"] } ] }"#,
    );

    let done_ids = std::collections::HashSet::from(["t0".to_string()]);
    assert!(validate_remediation_append(&[candidate], &[done], &done_ids).is_ok());
}

#[test]
fn validate_remediation_allows_dependency_on_same_batch_sibling() {
    let a = single_task(
        r#"{ "tasks": [ { "id": "fix_a", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
    );
    let b = single_task(
        r#"{ "tasks": [ { "id": "fix_b", "intent": "b", "files_scope": ["b.rs"],
              "acceptance_cmd": "true", "max_turns": 3, "depends_on": ["fix_a"] } ] }"#,
    );

    assert!(validate_remediation_append(&[a, b], &[], &std::collections::HashSet::new()).is_ok());
}

#[test]
fn generated_remediation_id_is_safe_and_stable() {
    assert_eq!(
        gen_remediation_id("parent/task", 2, 3),
        "parent_task_r2_fix3"
    );
}

#[test]
fn decide_replan_escalates_when_round_budget_is_full() {
    let mut st = state_with_worklist(vec![]);
    st.replan_rounds = 3;
    let snapshot = crate::plan::state::UnmetSnapshot {
        trigger: crate::plan::state::Trigger::TaskLevel,
        checked_ids: vec!["t1_acc".into()],
        passed_ids: vec![],
        failed_ids: vec!["t1_acc".into()],
    };

    let step = decide_replan(
        &st,
        crate::plan::state::Trigger::TaskLevel,
        snapshot,
        vec![code_red_evidence("t1_acc")],
        vec![],
        3,
    );

    assert!(matches!(step, ReplanStep::Escalate { reason, .. } if reason.contains("budget")));
}

#[test]
fn decide_replan_escalates_when_no_net_progress() {
    let mut st = state_with_worklist(vec![]);
    st.last_snapshot = Some(crate::plan::state::UnmetSnapshot {
        trigger: crate::plan::state::Trigger::TaskLevel,
        checked_ids: vec!["t1_acc".into()],
        passed_ids: vec![],
        failed_ids: vec!["t1_acc".into()],
    });
    let cur = st.last_snapshot.clone().unwrap();

    let candidate = single_task(
        r#"{ "tasks": [ { "id": "fix1", "intent": "fix", "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
    );

    let step = decide_replan(
        &st,
        crate::plan::state::Trigger::TaskLevel,
        cur,
        vec![code_red_evidence("t1_acc")],
        vec![candidate],
        3,
    );

    assert!(matches!(step, ReplanStep::Escalate { reason, .. } if reason.contains("net_progress")));
}

#[test]
fn decide_replan_filters_duplicate_candidates() {
    let existing = single_task(
        r#"{ "tasks": [ { "id": "t1", "intent": "fix missing field", "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
    );
    let duplicate = {
        let mut t = existing.clone();
        t.id = "new_id".into();
        t.acceptance.id = "new_id_acc".into();
        t
    };
    let fresh = single_task(
        r#"{ "tasks": [ { "id": "fix2", "intent": "fix other site", "files_scope": ["src/other.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
    );
    let st = state_with_worklist(vec![existing]);
    let snapshot = crate::plan::state::UnmetSnapshot {
        trigger: crate::plan::state::Trigger::TaskLevel,
        checked_ids: vec!["t1_acc".into()],
        passed_ids: vec![],
        failed_ids: vec!["t1_acc".into()],
    };

    let step = decide_replan(
        &st,
        crate::plan::state::Trigger::TaskLevel,
        snapshot,
        vec![code_red_evidence("t1_acc")],
        vec![duplicate, fresh],
        3,
    );

    match step {
        ReplanStep::Append { tasks } => assert_eq!(
            tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["fix2"]
        ),
        other => panic!("expected append, got {other:?}"),
    }
}

#[test]
fn decide_replan_appends_legal_candidates() {
    let st = state_with_worklist(vec![]);
    let snapshot = crate::plan::state::UnmetSnapshot {
        trigger: crate::plan::state::Trigger::OverallLevel,
        checked_ids: vec!["c1".into()],
        passed_ids: vec![],
        failed_ids: vec!["c1".into()],
    };
    let candidate = single_task(
        r#"{ "tasks": [ { "id": "overall_r1_fix1", "intent": "fix overall", "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
    );

    let step = decide_replan(
        &st,
        crate::plan::state::Trigger::OverallLevel,
        snapshot,
        vec![code_red_evidence("c1")],
        vec![candidate],
        3,
    );

    assert!(matches!(step, ReplanStep::Append { tasks } if tasks.len() == 1));
}

// ── ensure_scope_covers_evidence tests ──

#[test]
fn scope_widens_when_evidence_path_not_covered() {
    let mut candidates = vec![single_task(
        r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
    )];

    let evidence = vec![CommandEvidence {
        role: CommandRole::AuthoritativeAcceptance,
        criterion_id: "t1_acc".into(),
        command: "cargo build".into(),
        exit_code: Some(101),
        success: false,
        timed_out: false,
        stdout_summary: String::new(),
        stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
        truncated: false,
        environment_failure: None,
    }];

    ensure_scope_covers_evidence(&mut candidates, &evidence);

    let scope = &candidates[0].files_scope;
    assert!(
        scope.contains(&"src/lib.rs".to_string()),
        "evidence path src/lib.rs should be added to scope, got {scope:?}"
    );
    assert!(
        scope.contains(&"src/main.rs".to_string()),
        "original scope entry should remain"
    );
}

#[test]
fn scope_unchanged_when_evidence_path_already_covered_by_directory() {
    let mut candidates = vec![single_task(
        r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
    )];

    let original_scope = candidates[0].files_scope.clone();

    let evidence = vec![CommandEvidence {
        role: CommandRole::AuthoritativeAcceptance,
        criterion_id: "t1_acc".into(),
        command: "cargo build".into(),
        exit_code: Some(101),
        success: false,
        timed_out: false,
        stdout_summary: String::new(),
        stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
        truncated: false,
        environment_failure: None,
    }];

    ensure_scope_covers_evidence(&mut candidates, &evidence);

    assert_eq!(
        candidates[0].files_scope, original_scope,
        "scope should not change when evidence path is under existing scope directory"
    );
}

#[test]
fn scope_unchanged_when_evidence_has_no_file_paths() {
    let mut candidates = vec![single_task(
        r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
    )];

    let original_scope = candidates[0].files_scope.clone();

    let evidence = vec![CommandEvidence {
        role: CommandRole::AuthoritativeAcceptance,
        criterion_id: "t1_acc".into(),
        command: "cargo build".into(),
        exit_code: Some(1),
        success: false,
        timed_out: false,
        stdout_summary: "Compiling crate...".into(),
        stderr_summary:
            "error: could not compile\n\nCaused by:\n  process didn't exit successfully".into(),
        truncated: false,
        environment_failure: None,
    }];

    ensure_scope_covers_evidence(&mut candidates, &evidence);

    assert_eq!(
        candidates[0].files_scope, original_scope,
        "scope should not change when evidence contains no file paths"
    );
}

#[test]
fn multiple_evidence_paths_all_uncovered_ones_added() {
    let mut candidates = vec![single_task(
        r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile errors", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
    )];

    let evidence = vec![
        CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo build".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
            truncated: false,
            environment_failure: None,
        },
        CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo test".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: "error[E0063]: missing field\n --> tests/integration.rs:15:1".into(),
            truncated: false,
            environment_failure: None,
        },
    ];

    ensure_scope_covers_evidence(&mut candidates, &evidence);

    let scope = &candidates[0].files_scope;
    assert!(
        scope.contains(&"src/main.rs".to_string()),
        "original scope entry should remain, got {scope:?}"
    );
    assert!(
        scope.contains(&"src/lib.rs".to_string()),
        "first evidence path should be added, got {scope:?}"
    );
    assert!(
        scope.contains(&"tests/integration.rs".to_string()),
        "second evidence path should be added, got {scope:?}"
    );
}

#[test]
fn scope_widening_in_decide_replan_before_validation() {
    let st = state_with_worklist(vec![]);
    let snapshot = crate::plan::state::UnmetSnapshot {
        trigger: crate::plan::state::Trigger::TaskLevel,
        checked_ids: vec!["t1_acc".into()],
        passed_ids: vec![],
        failed_ids: vec!["t1_acc".into()],
    };

    // Candidate has scope [src/main.rs], evidence mentions src/lib.rs
    let candidate = single_task(
        r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
    );

    let evidence = CommandEvidence {
        role: CommandRole::AuthoritativeAcceptance,
        criterion_id: "t1_acc".into(),
        command: "cargo build".into(),
        exit_code: Some(101),
        success: false,
        timed_out: false,
        stdout_summary: String::new(),
        stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
        truncated: false,
        environment_failure: None,
    };

    let step = decide_replan(
        &st,
        crate::plan::state::Trigger::TaskLevel,
        snapshot,
        vec![evidence],
        vec![candidate],
        3,
    );

    match step {
        ReplanStep::Append { tasks } => {
            assert_eq!(tasks.len(), 1);
            let scope = &tasks[0].files_scope;
            assert!(
                scope.contains(&"src/lib.rs".to_string()),
                "decide_replan should widen scope to cover evidence path src/lib.rs, got {scope:?}"
            );
            assert!(
                scope.contains(&"src/main.rs".to_string()),
                "original scope entry should remain, got {scope:?}"
            );
        }
        other => panic!("expected Append, got {other:?}"),
    }
}
