use super::*;

pub(super) fn finish_single_worker_setup_failure<P, F>(
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    error: String,
    persist_failure: P,
    finalize: F,
) -> String
where
    P: FnOnce(&str) -> Result<(), String>,
    F: FnOnce() -> Result<(), String>,
{
    run_member_side_effect_best_effort(
        "persist failure report",
        session_id,
        run_id,
        &spec.assignment_id,
        || persist_failure(&error),
    );
    // The current finalize is a no-op; this assumes the caller does not create pending state
    // before an early return from profile, build, or workspace setup.
    run_member_side_effect_best_effort(
        "finalize",
        session_id,
        run_id,
        &spec.assignment_id,
        finalize,
    );
    error
}

/// Single-worker post-setup lifecycle. Production and tests share this boundary so registration,
/// execution, ledger and finalize ordering cannot drift apart.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_single_worker_lifecycle<
    L,
    Register,
    Run,
    PersistResult,
    PersistSetupFailure,
    PersistFailure,
    Finalize,
>(
    team_running: &TeamRunning,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    register: Register,
    run: Run,
    persist_result: PersistResult,
    persist_setup_failure: PersistSetupFailure,
    persist_failure: PersistFailure,
    finalize: Finalize,
) -> Result<MemberResult, String>
where
    Register: FnOnce() -> Result<L, String>,
    Run: FnOnce(L) -> Result<MemberResult, String>,
    PersistResult: FnOnce(&MemberResult) -> Result<(), String>,
    PersistSetupFailure: FnOnce(&str) -> Result<(), String>,
    PersistFailure: FnOnce(&str) -> Result<(), String>,
    Finalize: FnOnce() -> Result<(), String>,
{
    let lane = match register() {
        Ok(lane) => lane,
        Err(error) => {
            run_member_side_effect_best_effort(
                "persist failure report",
                session_id,
                run_id,
                &spec.assignment_id,
                || persist_setup_failure(&error),
            );
            return Err(error);
        }
    };
    team_running.init_run(run_id, 1);

    let result = run(lane);
    // The normal reader already decrements this counter. This second call is deliberately
    // idempotent and also covers spawn/no-result errors before the reader owns the counter.
    team_running.run_member_finished(run_id);
    match result {
        Ok(result) => {
            run_member_side_effect_best_effort(
                "persist result report",
                session_id,
                run_id,
                &spec.assignment_id,
                || persist_result(&result),
            );
            run_member_side_effect_best_effort(
                "finalize",
                session_id,
                run_id,
                &spec.assignment_id,
                finalize,
            );
            Ok(result)
        }
        Err(error) => {
            run_member_side_effect_best_effort(
                "persist failure report",
                session_id,
                run_id,
                &spec.assignment_id,
                || persist_failure(&error),
            );
            run_member_side_effect_best_effort(
                "finalize",
                session_id,
                run_id,
                &spec.assignment_id,
                finalize,
            );
            Err(error)
        }
    }
}

/// Construct the terminal Completed event: forward the real token counts from the buffered
/// Completed event; with no auto-commit, commit_sha remains None.
pub fn member_terminal_event(
    run_id: &str,
    spec: &MemberSpec,
    buffered: Option<AgentEvent>,
    status: StatusTransition,
    result: Option<MemberResult>,
    session_head_sha: Option<String>,
) -> (DispatchMeta, AgentEvent) {
    let (cost_usd, input_tokens, output_tokens, final_text) = match buffered {
        Some(AgentEvent::Completed {
            cost_usd,
            input_tokens,
            output_tokens,
            final_text,
            ..
        }) => (cost_usd, input_tokens, output_tokens, final_text),
        _ => (None, None, None, None),
    };
    let (files_changed, insertions, deletions) = match &result {
        Some(result) => (
            Some(result.changed_files.len() as u64),
            Some(result.changed_files.iter().map(|f| f.insertions).sum()),
            Some(result.changed_files.iter().map(|f| f.deletions).sum()),
        ),
        None => (None, None, None),
    };
    let interrupted = Some(matches!(status, StatusTransition::Stopped));
    (
        member_dispatch_meta(run_id, spec, Some(status)),
        AgentEvent::Completed {
            cost_usd,
            input_tokens,
            output_tokens,
            final_text,
            result: result.clone().map(Box::new),
            run_id: Some(run_id.to_string()),
            commit_sha: session_head_sha,
            files_changed,
            insertions,
            deletions,
            interrupted,
        },
    )
}
