#![cfg(test)]

use super::*;

#[tokio::test]
async fn overall_acceptance_red_escalates_exit4() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
    let mut o = opts(ws.path(), jr.path(), "plan_overall_red");
    o.checks = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap(); // 目标总验收注定红
    let res = run_plan(
        ScriptedPlanner {
            worklist: worklist.into(),
        },
        o,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(
        jr.path()
            .join(".myagenthubs/runs/plan_overall_red/events.jsonl"),
    )
    .unwrap();
    assert!(events.contains("overall_red"));
}

#[tokio::test]
async fn finalize_not_run_is_stopped_not_needs_replan() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());

    let mut c = crate::goal::parse_criteria(&["cmd: true".to_string()])
        .unwrap()
        .remove(0);
    c.authored_by = crate::goal::AuthoredBy::Agent;
    c.approval = crate::goal::Approval::Pending;

    let state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        vec![],
        vec![c],
    );
    let o = opts(ws.path(), jr.path(), "plan_finalize_not_run");
    let paths = RunPaths::new(jr.path(), "plan_finalize_not_run");
    paths.create_dirs().unwrap();
    let mut recorder = crate::events::EventRecorder::new(
        "plan_finalize_not_run".to_string(),
        None,
        Some(ws.path().to_string_lossy().into_owned()),
        &paths.events_path,
        crate::events::OutputMode::Silent,
    )
    .unwrap();

    let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
        .await
        .unwrap();

    assert!(matches!(outcome, FinalizeOutcome::Stopped { .. }));
}

#[tokio::test]
async fn finalize_pure_code_red_is_needs_replan() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());

    let checks = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap();
    let state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        vec![],
        checks,
    );
    let o = opts(ws.path(), jr.path(), "plan_finalize_code_red");
    let paths = RunPaths::new(jr.path(), "plan_finalize_code_red");
    paths.create_dirs().unwrap();
    let mut recorder = crate::events::EventRecorder::new(
        "plan_finalize_code_red".to_string(),
        None,
        Some(ws.path().to_string_lossy().into_owned()),
        &paths.events_path,
        crate::events::OutputMode::Silent,
    )
    .unwrap();

    let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
        .await
        .unwrap();

    assert!(matches!(outcome, FinalizeOutcome::NeedsReplan { .. }));
}

#[tokio::test]
async fn finalize_infra_is_typed_infra() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());

    let checks =
        crate::goal::parse_criteria(&["cmd: echo connection refused; exit 1".to_string()]).unwrap();
    let state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        vec![],
        checks,
    );
    let o = opts(ws.path(), jr.path(), "plan_finalize_infra_typed");
    let paths = RunPaths::new(jr.path(), "plan_finalize_infra_typed");
    paths.create_dirs().unwrap();
    let mut recorder = crate::events::EventRecorder::new(
        "plan_finalize_infra_typed".to_string(),
        None,
        Some(ws.path().to_string_lossy().into_owned()),
        &paths.events_path,
        crate::events::OutputMode::Silent,
    )
    .unwrap();

    let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
        .await
        .unwrap();

    assert!(matches!(outcome, FinalizeOutcome::Infra { .. }));
}

#[tokio::test]
async fn finalize_read_only_delta_is_typed_policy() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());

    let checks =
        crate::goal::parse_criteria(&["cmd: printf touched > overall.txt".to_string()]).unwrap();
    let state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        vec![],
        checks,
    );
    let o = opts(ws.path(), jr.path(), "plan_finalize_policy_typed");
    let paths = RunPaths::new(jr.path(), "plan_finalize_policy_typed");
    paths.create_dirs().unwrap();
    let mut recorder = crate::events::EventRecorder::new(
        "plan_finalize_policy_typed".to_string(),
        None,
        Some(ws.path().to_string_lossy().into_owned()),
        &paths.events_path,
        crate::events::OutputMode::Silent,
    )
    .unwrap();

    let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
        .await
        .unwrap();

    assert!(matches!(outcome, FinalizeOutcome::Policy { .. }));
}

#[tokio::test]
async fn finalize_done_task_reverify_uses_failed_by_acceptance_decision() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let tasks = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
    )
    .unwrap();
    let mut state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    );
    state.mark_status("t1", crate::plan::contract::TaskStatus::Done);
    let o = opts(ws.path(), jr.path(), "plan_reverify_decision");
    let paths = RunPaths::new(jr.path(), "plan_reverify_decision");
    paths.create_dirs().unwrap();
    let mut recorder = crate::events::EventRecorder::new(
        "plan_reverify_decision".to_string(),
        None,
        Some(ws.path().to_string_lossy().into_owned()),
        &paths.events_path,
        crate::events::OutputMode::Silent,
    )
    .unwrap();
    let res = finalize_completion(&state, &o, &mut recorder)
        .await
        .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(paths.events_path).unwrap();
    assert!(events.contains("overall_red"));
    assert!(events.contains("failed_by_acceptance"));
    assert!(!events.contains("failed_by_policy"));
}

#[tokio::test]
async fn resume_in_progress_code_red_goes_pending_not_done_or_policy() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let tasks = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
    )
    .unwrap();
    let mut state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    );
    state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
    let paths = RunPaths::new(jr.path(), "plan_resume_code_red");
    paths.create_dirs().unwrap();
    save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();
    let mut o = opts(ws.path(), jr.path(), "plan_resume_code_red");
    o.max_plan_steps = 1;
    let res = resume_plan(
        ScriptedPlanner {
            worklist: String::new(),
        },
        o,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let after: RunState =
        serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
            .unwrap();
    assert!(after.worklist.iter().any(|t| {
        t.id == "t1" && matches!(t.status, crate::plan::contract::TaskStatus::Pending)
    }));
}

#[tokio::test]
async fn overall_check_records_command_role_overall_check() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
      "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
    let mut o = opts(ws.path(), jr.path(), "plan_overall_role");
    o.checks = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap();
    let res = run_plan(
        ScriptedPlanner {
            worklist: worklist.into(),
        },
        o,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(
        jr.path()
            .join(".myagenthubs/runs/plan_overall_role/events.jsonl"),
    )
    .unwrap();
    assert!(events.contains("overall_check"));
}

#[tokio::test]
async fn finalize_infra_red_reported_as_infra_with_observed() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
    let mut o = opts(ws.path(), jr.path(), "plan_infra");
    o.checks =
        crate::goal::parse_criteria(&["cmd: echo connection refused; exit 1".to_string()]).unwrap();
    let res = run_plan(
        ScriptedPlanner {
            worklist: worklist.into(),
        },
        o,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/plan_infra/events.jsonl"))
            .unwrap();
    assert!(events.contains("infra_red"));
}

// B3：整盘重验逮「done 任务的 acceptance 后来红了」——直接构造全 Done + 一条 acceptance "false" 测 finalize
#[tokio::test]
async fn finalize_reverify_catches_broken_done_task_acceptance() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let tasks = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
    ).unwrap();
    let mut state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    );
    state.mark_status("t1", crate::plan::contract::TaskStatus::Done); // 假装已 done
    let o = opts(ws.path(), jr.path(), "plan_reverify");
    let paths = RunPaths::new(jr.path(), "plan_reverify");
    paths.create_dirs().unwrap();
    let mut recorder = crate::events::EventRecorder::new(
        "plan_reverify".to_string(),
        None,
        Some(ws.path().to_string_lossy().into_owned()),
        &paths.events_path,
        crate::events::OutputMode::Silent,
    )
    .unwrap();
    let res = finalize_completion(&state, &o, &mut recorder)
        .await
        .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(paths.events_path).unwrap();
    assert!(events.contains("overall_red") && events.contains("t1 acceptance"));
}

// B1：resume 时 in-progress 任务 acceptance 命中 infra → 挂起（不当没做完重跑）
#[tokio::test]
async fn resume_in_progress_infra_suspends() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let tasks = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "echo connection refused; exit 1", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
    ).unwrap();
    let mut state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    );
    state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
    let paths = RunPaths::new(jr.path(), "plan_resume_infra");
    paths.create_dirs().unwrap();
    save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();
    let res = resume_plan(
        ScriptedPlanner {
            worklist: String::new(),
        },
        opts(ws.path(), jr.path(), "plan_resume_infra"),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(paths.run_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("infra_red"));
}

#[tokio::test]
async fn resume_without_state_falls_back_to_fresh_run() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
    let res = resume_plan(
        ScriptedPlanner {
            worklist: worklist.into(),
        },
        opts(ws.path(), jr.path(), "plan_resume_fresh"),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
}

#[tokio::test]
async fn resume_continues_budget_does_not_reset() {
    // B5：崩溃前已用掉预算·resume 不归零
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let tasks = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 },
          { "id": "t2", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 }
        ] }"#,
    ).unwrap();
    let mut state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    );
    state.mark_status("t1", crate::plan::contract::TaskStatus::Done);
    state.steps_used = 1; // 崩溃前已用 1 步
    let paths = RunPaths::new(jr.path(), "plan_resume_budget");
    paths.create_dirs().unwrap();
    save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

    let mut o = opts(ws.path(), jr.path(), "plan_resume_budget");
    o.max_plan_steps = 1; // 预算只 1 步·已用 1 → resume 不该再跑 t2
    let res = resume_plan(
        ScriptedPlanner {
            worklist: String::new(),
        },
        o,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let after: RunState =
        serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
            .unwrap();
    assert!(after
        .worklist
        .iter()
        .any(|t| t.id == "t2" && matches!(t.status, crate::plan::contract::TaskStatus::Pending)));
}

#[tokio::test]
async fn task_acceptance_read_only_delta_is_failed_by_policy() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
      "acceptance_cmd": "printf $$ > a.rs", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
    let res = run_plan(
        ScriptedPlanner {
            worklist: worklist.into(),
        },
        opts(ws.path(), jr.path(), "plan_readonly_task"),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(
        jr.path()
            .join(".myagenthubs/runs/plan_readonly_task/events.jsonl"),
    )
    .unwrap();
    assert!(events.contains("failed_by_policy"));
    assert!(events.contains("acceptance_read_only_violation"));
}

#[tokio::test]
async fn overall_check_read_only_delta_is_failed_by_policy() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
      "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
    let mut o = opts(ws.path(), jr.path(), "plan_readonly_overall");
    o.checks = crate::goal::parse_criteria(&["cmd: printf $$ > overall.txt".to_string()]).unwrap();
    let res = run_plan(
        ScriptedPlanner {
            worklist: worklist.into(),
        },
        o,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(
        jr.path()
            .join(".myagenthubs/runs/plan_readonly_overall/events.jsonl"),
    )
    .unwrap();
    assert!(events.contains("failed_by_policy"));
    assert!(events.contains("overall.txt"));
}

#[tokio::test]
async fn finalize_done_task_reverify_read_only_delta_is_failed_by_policy() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let tasks = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "printf $$ > a.rs", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
    )
    .unwrap();
    let mut state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    );
    state.mark_status("t1", crate::plan::contract::TaskStatus::Done);
    let o = opts(ws.path(), jr.path(), "plan_readonly_finalize");
    let paths = RunPaths::new(jr.path(), "plan_readonly_finalize");
    paths.create_dirs().unwrap();
    let mut recorder = crate::events::EventRecorder::new(
        "plan_readonly_finalize".to_string(),
        None,
        Some(ws.path().to_string_lossy().into_owned()),
        &paths.events_path,
        crate::events::OutputMode::Silent,
    )
    .unwrap();
    let res = finalize_completion(&state, &o, &mut recorder)
        .await
        .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(paths.events_path).unwrap();
    assert!(events.contains("failed_by_policy"));
    assert!(events.contains("acceptance_read_only_violation"));
}

#[tokio::test]
async fn resume_reverify_read_only_delta_is_failed_by_policy() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let tasks = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "printf $$ > a.rs", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
    )
    .unwrap();
    let mut state = RunState::new(
        crate::goal::GoalState::new("big", vec![]).contract,
        tasks,
        vec![],
    );
    state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
    let paths = RunPaths::new(jr.path(), "plan_readonly_resume");
    paths.create_dirs().unwrap();
    save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();
    let res = resume_plan(
        ScriptedPlanner {
            worklist: String::new(),
        },
        opts(ws.path(), jr.path(), "plan_readonly_resume"),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(paths.run_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("failed_by_policy"));
    assert!(events.contains("acceptance_read_only_violation"));
}

#[tokio::test]
async fn infra_red_read_only_delta_becomes_policy_failure() {
    let ws = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let criterion = crate::goal::parse_criteria(&[
        "cmd: printf $$ > infra.txt; echo connection refused; exit 1".to_string(),
    ])
    .unwrap()
    .remove(0);
    let baseline = capture_baseline(ws.path()).unwrap();
    let result = criterion_command_result_readonly_checked_with_baseline(
        &criterion,
        CommandRole::OverallCheck,
        ws.path(),
        NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &baseline,
    )
    .await
    .unwrap();
    assert!(matches!(result, AcceptanceResult::PolicyFailure { .. }));
}

#[tokio::test]
async fn infra_red_without_delta_stays_infra_red() {
    let ws = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let criterion =
        crate::goal::parse_criteria(&["cmd: echo connection refused; exit 1".to_string()])
            .unwrap()
            .remove(0);
    let baseline = capture_baseline(ws.path()).unwrap();
    let result = criterion_command_result_readonly_checked_with_baseline(
        &criterion,
        CommandRole::OverallCheck,
        ws.path(),
        NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &baseline,
    )
    .await
    .unwrap();
    assert!(matches!(result, AcceptanceResult::InfraRed { .. }));
}

#[tokio::test]
async fn bounce_reasons_worklist_bounce_event_records_reasons() {
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    init_git(ws.path());
    let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": [], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
    let res = run_plan(
        ScriptedPlanner {
            worklist: worklist.into(),
        },
        opts(ws.path(), jr.path(), "plan_bounce_reasons"),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    let events = std::fs::read_to_string(
        jr.path()
            .join(".myagenthubs/runs/plan_bounce_reasons/events.jsonl"),
    )
    .unwrap();
    assert!(events.contains("\"type\":\"plan.worklist.bounced\""));
    assert!(events.contains("\"reasons\""));
    assert!(events.contains("files_scope"));
}
