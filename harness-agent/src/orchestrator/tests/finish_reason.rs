#![cfg(test)]

use super::*;

#[tokio::test]
async fn empty_criteria_stop_with_text_completes_without_spinning() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) = finish_reason_provider(crate::provider::FinishReason::Stop, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(dir.path().to_path_buf(), "empty criteria final text"),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"criteria_verified\":false"));
}

#[tokio::test]
async fn approved_passing_criterion_final_text_still_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) = finish_reason_provider(crate::provider::FinishReason::Stop, false);
    let mut opts = options(dir.path().to_path_buf(), "passing criterion final text");
    opts.criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"criteria_verified\":true"));
}

#[tokio::test]
async fn empty_criteria_tool_call_executes_normally() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, _, _) = finish_reason_provider(crate::provider::FinishReason::ToolCalls, true);
    let mut opts = options(dir.path().to_path_buf(), "empty criteria tool call");
    opts.permission = PermissionPolicy::Allow;
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("finish_reason_tool.txt")).unwrap(),
        "done"
    );
}

#[tokio::test]
async fn length_finish_reason_never_completes_and_injects_direct_tool_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, saw_feedback) =
        finish_reason_provider(crate::provider::FinishReason::Length, false);
    let mut opts = options(dir.path().to_path_buf(), "truncated response");
    opts.criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert!(calls.load(Ordering::SeqCst) >= 2);
    assert!(*saw_feedback.lock().unwrap());
}

#[tokio::test]
async fn empty_criteria_length_does_not_complete_before_later_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, saw_feedback) =
        finish_reason_provider(crate::provider::FinishReason::Length, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(
            dir.path().to_path_buf(),
            "truncated empty-criteria response",
        ),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(*saw_feedback.lock().unwrap());
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"turns\":2"));
    assert!(events.contains("\"criteria_verified\":false"));
}

#[tokio::test]
async fn rejected_completion_emits_observable_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) =
        finish_reason_provider(crate::provider::FinishReason::ToolCalls, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(dir.path().to_path_buf(), "observable completion rejection"),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let mut rejected = events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("completion rejection should be observable")
        .clone();
    assert_eq!(rejected["payload"]["via"], "model_final_text");
    rejected["payload"]
        .as_object_mut()
        .expect("completion rejection payload should be an object")
        .remove("via");
    assert_eq!(
        rejected["payload"],
        json!({
            "reason": "empty_criteria_not_stopped",
            "finish_reason": "tool_calls",
            "text_len": 4,
            "tool_calls": 0,
            "criteria_count": 0,
            "turn": 1,
        })
    );
}

#[tokio::test]
async fn try_finalize_rejection_emits_null_response_metadata_and_preserves_via() {
    let dir = tempfile::tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        "try_finalize_rejected",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new("engine finalize rejection", Vec::new());
    let mut eval_round = 0;
    let mut evidence = EvidenceState::new(EvidenceGate::Off);

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
        12,
        "engine_finalize",
    )
    .await
    .unwrap();

    assert_eq!(outcome, FinalizeOutcome::NotComplete);
    let events: Vec<Value> = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejected = events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("try_finalize rejection should be observable");
    assert_eq!(
        rejected["payload"],
        json!({
            "reason": "no_criteria",
            "finish_reason": null,
            "text_len": null,
            "tool_calls": null,
            "criteria_count": 0,
            "turn": 12,
            "via": "engine_finalize",
        })
    );
}

#[tokio::test]
async fn completion_rejection_via_distinguishes_model_text_and_engine_finalize_paths() {
    let model_dir = tempfile::tempdir().unwrap();
    let (provider, _, _) = finish_reason_provider(crate::provider::FinishReason::ToolCalls, false);
    run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(
            model_dir.path().to_path_buf(),
            "model completion rejection via",
        ),
    )
    .await
    .unwrap();
    let model_events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(model_dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let model_via = model_events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("model final text rejection should be observable")["payload"]["via"]
        .clone();

    let engine_dir = tempfile::tempdir().unwrap();
    let engine_events_path = engine_dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        "engine_finalize_rejected",
        None,
        Some(engine_dir.path().to_string_lossy().into_owned()),
        &engine_events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new("engine completion rejection via", Vec::new());
    let mut eval_round = 0;
    let mut evidence = EvidenceState::new(EvidenceGate::Off);
    let outcome = try_finalize(
        &mut goal,
        &mut evidence,
        crate::guardrails::ContractPolicy::TrustAll,
        engine_dir.path(),
        &crate::judge::NoopJudge,
        &mut recorder,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut eval_round,
        12,
        "engine_finalize",
    )
    .await
    .unwrap();
    assert_eq!(outcome, FinalizeOutcome::NotComplete);
    let engine_events: Vec<Value> = std::fs::read_to_string(engine_events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let engine_via = engine_events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("engine finalize rejection should be observable")["payload"]["via"]
        .clone();

    assert_eq!(model_via, "model_final_text");
    assert_eq!(engine_via, "engine_finalize");
    assert_ne!(model_via, engine_via);
}

#[tokio::test]
async fn other_finish_reason_payload_is_preserved_in_observability_events() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) =
        finish_reason_provider(crate::provider::FinishReason::Other("foo".into()), false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(
            dir.path().to_path_buf(),
            "observable provider finish reason",
        ),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let provider_turn = events
        .iter()
        .find(|event| event["type"] == "provider.turn.finished" && event["payload"]["turn"] == 1)
        .expect("provider turn should preserve the finish reason");
    let rejected = events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("completion rejection should preserve the finish reason");

    assert_eq!(provider_turn["payload"]["finish_reason"], "other:foo");
    assert_eq!(rejected["payload"]["finish_reason"], "other:foo");
}
