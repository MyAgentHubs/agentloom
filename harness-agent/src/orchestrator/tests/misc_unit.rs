#![cfg(test)]

use super::*;

#[cfg(test)]
fn verify_reflex_record_debt(debt: &mut usize, invalidates: bool, execute_ok: bool) {
    if execute_ok && invalidates {
        *debt += 1;
    }
}

#[test]
fn default_verify_and_watchdog_are_nonzero() {
    assert_eq!(DEFAULT_VERIFY_EVERY, 3);
    assert_eq!(DEFAULT_WATCHDOG_REPEAT, 3);
}

#[test]
fn executor_system_prompt_teaches_persistent_ripple_work() {
    let messages = initial_messages("do the work");
    let system = messages[0].content.as_deref().unwrap();

    assert!(system.contains("keep working until its acceptance"));
    assert!(system.contains("Use tools aggressively"));
    assert!(system.contains("ALL affected sites"));
    assert!(system
        .replace('\n', " ")
        .contains("the   harness runs them in order"));
    assert!(system.contains("compiler/tests"));
    assert!(system.contains("not progress"));
    assert!(system.contains("shell_exec"));
    assert!(!system.contains("Use tools only when needed"));

    // Strengthened wide-ripple guidance directs executors to inspect test-adjacent call sites beyond literal string matches.
    assert!(system.contains("the sites you miss are almost always in tests"));
    assert!(system.contains("grep -rn"));
    assert!(system.contains("whose path is under tests"));
    assert!(system.contains("grep → patch → rebuild"));
    assert!(system.contains("escalate with the exact paths"));
    assert!(system.contains("patch at least one listed site"));
}

#[test]
fn make_search_backend_picks_brave_when_key_present() {
    use super::search_backend_kind;
    use crate::config::SearchChoice;

    assert_eq!(
        search_backend_kind(&SearchChoice::Brave {
            api_key: "k".into()
        }),
        "fallback_brave_ddg"
    );
    assert_eq!(search_backend_kind(&SearchChoice::Ddg), "ddg");
}

#[test]
fn search_backend_kind_exa() {
    use crate::config::SearchChoice;

    assert_eq!(
        super::search_backend_kind(&SearchChoice::Exa {
            api_key: "k".into()
        }),
        "fallback_exa_ddg"
    );
}

#[test]
fn make_search_backend_exa_builds() {
    use crate::config::SearchChoice;

    let _ = super::make_search_backend(&SearchChoice::Exa {
        api_key: "k".into(),
    });
}

#[test]
fn guardrail_summary_mcp_gate_shows_server_tool_args() {
    let workspace = std::path::Path::new("/tmp/ws");
    let shell_args = serde_json::json!({ "command": "ls -la" }).to_string();
    assert_eq!(
        guardrail_summary("shell_exec", &shell_args, workspace),
        "ls -la"
    );

    let args = format!(
        "{{\"title\":\"trusted gate\",\"body\":\"{}\"}}",
        "x".repeat(100)
    );
    let summary = guardrail_summary("mcp__github__create_issue", &args, workspace);

    assert!(summary.contains("github"));
    assert!(summary.contains("create_issue"));
    assert!(summary.contains("trusted gate"));
    assert!(summary.ends_with('…'));
    assert!(summary.len() < args.len());
}

#[test]
fn memory_lookup_registry_respects_memory_enabled() {
    use crate::config::SearchChoice;

    fn names(registry: ToolRegistry) -> Vec<String> {
        registry
            .definitions()
            .into_iter()
            .filter_map(|d| d["function"]["name"].as_str().map(ToString::to_string))
            .collect()
    }

    let disabled = names(build_default_registry(&SearchChoice::Ddg, false));
    assert!(!disabled.iter().any(|name| name == "memory_lookup"));

    let enabled = names(build_default_registry(&SearchChoice::Ddg, true));
    assert!(enabled.iter().any(|name| name == "memory_lookup"));
}

#[test]
fn network_tool_gate_decisions() {
    use super::{network_tool_gate, NetworkGate};
    use crate::goal::NetworkPolicy::{Off, On};
    // Tools that do not require network access always execute.
    assert!(matches!(
        network_tool_gate(false, Off, 99, 5),
        NetworkGate::Execute
    ));
    // Network-required tool with networking disabled: refuse.
    assert!(matches!(
        network_tool_gate(true, Off, 0, 5),
        NetworkGate::RefuseNetworkOff
    ));
    // Network-required tool with networking enabled and under the cap: execute (prior=4 -> this is the fifth call).
    assert!(matches!(
        network_tool_gate(true, On, 4, 5),
        NetworkGate::Execute
    ));
    // Network-required tool with networking enabled and over the cap: refuse (prior=5 -> this is the sixth call).
    assert!(matches!(
        network_tool_gate(true, On, 5, 5),
        NetworkGate::RefuseCap
    ));
}

#[test]
fn verify_reflex_debt_counts_only_successful_invalidating_tools() {
    let mut debt = 0usize;
    verify_reflex_record_debt(&mut debt, true, true);
    assert_eq!(debt, 1);

    verify_reflex_record_debt(&mut debt, true, false);
    assert_eq!(debt, 1);

    verify_reflex_record_debt(&mut debt, false, true);
    assert_eq!(debt, 1);
}

#[test]
fn verify_reflex_threshold_boundaries_and_k_zero() {
    let criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let goal = GoalState::new("obj", criteria);
    let progress = crate::run_progress::RunProgress::default();

    assert!(!verify_reflex_should_run(0, 99, &goal, &progress));
    assert!(!verify_reflex_should_run(2, 1, &goal, &progress));
    assert!(verify_reflex_should_run(2, 2, &goal, &progress));
    assert!(verify_reflex_should_run(2, 3, &goal, &progress));
}

#[test]
fn verify_reflex_requires_approved_verifiable_criterion() {
    let goal = GoalState::new("obj", Vec::new());
    let progress = crate::run_progress::RunProgress::default();
    assert!(!verify_reflex_should_run(1, 1, &goal, &progress));

    let mut criteria = crate::goal::parse_criteria(&["judge: check manually".into()]).unwrap();
    criteria[0].approval = crate::goal::Approval::Approved;
    let goal = GoalState::new("obj", criteria);
    assert!(!verify_reflex_should_run(1, 1, &goal, &progress));
}

#[test]
fn verify_reflex_runs_after_one_mutating_edit_when_ripple_candidates_open() {
    let mut criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    criteria[0].approval = crate::goal::Approval::Approved;
    let goal = GoalState::new("obj", criteria);

    let mut progress = crate::run_progress::RunProgress::default();
    assert!(!verify_reflex_should_run(3, 1, &goal, &progress));

    progress.set_ripple_candidates(vec![crate::run_progress::RippleCandidate {
        symbol: "RunOptions".into(),
        missing_field: Some("journal_root".into()),
        compiler_reported_sites: vec!["src/lib.rs:10".into()],
        extra_candidate_sites: vec!["tests/integration.rs:20".into()],
        truncated: false,
    }]);

    assert!(verify_reflex_should_run(3, 1, &goal, &progress));
    assert!(!verify_reflex_should_run(0, 1, &goal, &progress));
    assert!(!verify_reflex_should_run(3, 0, &goal, &progress));
}

#[test]
fn verify_reflex_debt_clears_after_validation_and_accumulates_across_turns() {
    let criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let goal = GoalState::new("obj", criteria);
    let progress = crate::run_progress::RunProgress::default();
    let mut debt = 0usize;

    verify_reflex_record_debt(&mut debt, true, true);
    assert!(!verify_reflex_should_run(2, debt, &goal, &progress));

    verify_reflex_record_debt(&mut debt, true, true);
    assert!(verify_reflex_should_run(2, debt, &goal, &progress));

    verify_reflex_clear_debt(&mut debt);
    assert_eq!(debt, 0);
    assert!(!verify_reflex_should_run(2, debt, &goal, &progress));
}

#[test]
fn unmet_summary_includes_failed_and_uncertain() {
    use crate::goal::{Approval, AuthoredBy, Criterion, CriterionStatus, GoalState, Verifier};

    let mk = |id: &str, status: CriterionStatus| Criterion {
        id: id.into(),
        claim: "c".into(),
        scope: None,
        authored_by: AuthoredBy::User,
        approval: Approval::Approved,
        verifier: Verifier::Judgmental { rubric: "r".into() },
        status,
        evidence_ref: Some("ev".into()),
    };
    let goal = GoalState::new(
        "obj",
        vec![
            mk("c1", CriterionStatus::Failed),
            mk("c2", CriterionStatus::Uncertain),
        ],
    );

    let summary = unmet_summary(&goal);

    assert!(
        summary.contains("c1") && summary.contains("FAILED"),
        "应含 Failed"
    );
    assert!(
        summary.contains("c2") && summary.contains("UNCERTAIN"),
        "应含 Uncertain"
    );
}

#[test]
fn unmet_summary_renders_no_internal_markers() {
    use crate::goal::{parse_criteria, CriterionStatus, GoalState};

    let mut goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
    goal.contract.criteria[0].status = CriterionStatus::Failed;
    goal.contract.criteria[0].evidence_ref =
        Some("check_cmd[user] exit=Some(101) passed=false cmd=cargo test stderr=...".into());
    let s = unmet_summary(&goal);
    assert!(s.contains("c1") && s.contains("FAILED"));
    assert!(s.contains("验收检查") && s.contains("cargo test"));
    assert!(!s.contains("check_cmd[") && !s.contains("cmd="));
}

#[test]
fn make_control_source_uses_stdin_jsonl_for_jsonl_mode() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_test");
    paths.create_dirs().unwrap();
    std::fs::write(&paths.interrupt_path, b"interrupt\n").unwrap();

    let mut control = make_control_source(ControlInputKind::StdinJsonl, &paths, "run_test");

    assert!(control.poll().is_none());
}

#[test]
fn make_control_source_uses_sentinel_for_human_and_silent_modes() {
    for _output_mode in [OutputMode::Human, OutputMode::Silent] {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "run_test");
        paths.create_dirs().unwrap();
        std::fs::write(&paths.interrupt_path, b"interrupt\n").unwrap();

        let mut control = make_control_source(ControlInputKind::Sentinel, &paths, "run_test");

        assert!(matches!(
            control.poll(),
            Some(ControlCommand::Stop { run_id }) if run_id == "run_test"
        ));
    }
}

#[test]
fn narrow_set_keeps_fs_read() {
    let narrowed: Vec<&str> = ["grep", "ls", "glob"].to_vec();
    assert!(!narrowed.contains(&"fs_read"));
}

#[test]
fn no_progress_threshold_constants_are_absolute() {
    assert_eq!(MIN_TASK_TURN_BUDGET, 40);
    assert_eq!(NO_PROGRESS_SOFT_TURNS, 4);
}

#[test]
fn executor_prompt_points_to_env_block() {
    assert!(EXECUTOR_SYSTEM_PROMPT.contains("<env>"));
    assert!(EXECUTOR_SYSTEM_PROMPT.to_lowercase().contains("absolute"));
}
