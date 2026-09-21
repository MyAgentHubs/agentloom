#![cfg(test)]

use super::*;

fn assert_blocked_has_no_prior_step_completed(events: &str) {
    let lines = events.lines().collect::<Vec<_>>();
    let blocked_index = lines
        .iter()
        .position(|line| line.contains("\"type\":\"run.blocked\""))
        .expect("max-eval path must emit run.blocked");
    assert!(blocked_index > 0);
    assert!(
        !lines[blocked_index - 1].contains("\"type\":\"orchestration.step.completed\""),
        "EvalOutcome::Blocked must not emit step.completed before run.blocked"
    );
}

#[tokio::test]
async fn max_eval_blocked_final_text_does_not_emit_step_completed() {
    let workspace = tempfile::tempdir().unwrap();
    let mut opts = options(workspace.path().to_path_buf(), "unmet");
    opts.criteria = crate::goal::parse_criteria(&["judge: criterion stays unmet".into()]).unwrap();
    opts.contract_policy = crate::guardrails::ContractPolicy::TrustAll;
    opts.max_eval_attempts = 1;
    let judge = crate::judge::FixedJudge {
        decision: crate::judge::JudgeDecision::Uncertain,
    };
    let result = run_solo_with_judge(
        crate::provider::mock::MockProvider::default(),
        Box::new(judge),
        opts,
    )
    .await
    .unwrap();
    assert_eq!(result.outcome, RunOutcome::Blocked);
    let path = RunPaths::new(workspace.path(), "run_test").events_path;
    let events = std::fs::read_to_string(path).unwrap();
    assert_blocked_has_no_prior_step_completed(&events);
}
