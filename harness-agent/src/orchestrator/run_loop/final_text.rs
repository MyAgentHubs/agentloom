use super::*;

struct JudgedFinalText {
    outcome: crate::evaluator::EvalOutcome,
    evidence_denial: Option<EvidenceDenial>,
    evidence_gate_released: bool,
    passed_before: usize,
    passed_after: usize,
}

pub(super) async fn handle_final_text<P: ProviderClient>(
    ctx: &mut LoopCtx<'_>,
    provider: &P,
    state: &mut LoopState,
    ts: &mut TurnState,
    response: &crate::provider::ProviderResponse,
) -> Result<TurnFlow> {
    let judged = judge_final_text(ctx, state, ts, response).await?;
    let turn = ts.turn;
    match judged.outcome {
        crate::evaluator::EvalOutcome::Complete => {
            if state.approval_unavailable_seen {
                ctx.recorder.emit(
                    "orchestration.step.completed",
                    json!({ "step_id": format!("solo.turn.{turn}"), "turn": turn, "outcome": "blocked" }),
                )?;
                ctx.recorder.emit(
                    "run.blocked",
                    json!({
                        "turns": turn,
                        "attempts": state.attempts.count(),
                        "reason": "approval_unavailable",
                        "criteria": ctx.goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                    }),
                )?;
                return Ok(TurnFlow::Return(RunOutcome::Blocked));
            }
            ctx.recorder.emit(
                "orchestration.step.completed",
                json!({ "step_id": format!("solo.turn.{turn}"), "turn": turn, "outcome": "completed" }),
            )?;
            ctx.recorder.emit(
                "run.completed",
                json!({
                    "turns": turn,
                    "criteria_verified": !ctx.goal.contract.criteria.is_empty(),
                }),
            )?;
            Ok(TurnFlow::Return(RunOutcome::Completed))
        }
        crate::evaluator::EvalOutcome::Blocked => {
            ctx.recorder.emit(
                "run.blocked",
                json!({
                    "turns": turn, "attempts": state.attempts.count(),
                    "reason": "max_eval_attempts exceeded without progress",
                    "criteria": ctx.goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                }),
            )?;
            Ok(TurnFlow::Return(RunOutcome::Blocked))
        }
        crate::evaluator::EvalOutcome::Continue => {
            continue_after_unmet(ctx, provider, state, ts, judged).await
        }
    }
}

async fn judge_final_text(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &mut TurnState,
    response: &crate::provider::ProviderResponse,
) -> Result<JudgedFinalText> {
    let turn = ts.turn;
    let passed_before = ctx
        .goal
        .contract
        .criteria
        .iter()
        .filter(|c| c.status == crate::goal::CriterionStatus::Passed)
        .count();
    ctx.goal.record_progress("provider produced final text");
    crate::evaluator::evaluate_criteria(
        ctx.goal,
        ctx.options.contract_policy,
        &ctx.options.workspace,
        &response.text,
        ctx.judge,
        ctx.recorder,
        ctx.options.network,
        ctx.options.fs_write_fence,
        state.eval_round,
    )
    .await?;
    verify_reflex_clear_debt(&mut state.verify_debt);
    state.eval_round += 1;
    let passed_after = ctx
        .goal
        .contract
        .criteria
        .iter()
        .filter(|c| c.status == crate::goal::CriterionStatus::Passed)
        .count();
    if passed_after > passed_before {
        state.consecutive_rejections = 0;
    }
    let exceeded = state.attempts.record(ctx.goal);
    let evaluated_outcome = crate::evaluator::decide_outcome(ctx.goal, exceeded);
    let mut evidence_denial = None;
    let mut evidence_gate_released = false;
    let outcome = match finalize_verdict(ctx.goal, Some(response)) {
        Ok(()) => match state.evidence.ready() {
            Ok(()) => crate::evaluator::EvalOutcome::Complete,
            Err(denial) => {
                evidence_denial = Some(denial);
                ctx.recorder.emit(
                    "completion.rejected",
                    json!({
                        "reason": evidence_denial_reason(denial),
                        "finish_reason": finish_reason_str(response.finish_reason.as_ref()),
                        "text_len": response.text.chars().count(),
                        "tool_calls": response.tool_calls.len(),
                        "criteria_count": ctx.goal.contract.criteria.len(),
                        "turn": turn,
                        "via": "model_final_text",
                        "edit_epoch": state.evidence.edit_epoch,
                        "green_epoch": state.evidence.green_epoch,
                    }),
                )?;
                evidence_gate_released = note_evidence_completion_denial(
                    &mut state.evidence,
                    ctx.recorder,
                    turn,
                    "model_final_text",
                )?;
                // Evidence denial means "continue working", even when the generic
                // evaluation-attempt budget has run out. The evidence liveness escape
                // hatch owns this case and releases the gate after repeated denials.
                crate::evaluator::EvalOutcome::Continue
            }
        },
        Err(denial) => {
            ctx.recorder.emit(
                "completion.rejected",
                json!({
                    "reason": denial,
                    "finish_reason": finish_reason_str(response.finish_reason.as_ref()),
                    "text_len": response.text.chars().count(),
                    "tool_calls": response.tool_calls.len(),
                    "criteria_count": ctx.goal.contract.criteria.len(),
                    "turn": turn,
                    "via": "model_final_text",
                }),
            )?;
            if evaluated_outcome == crate::evaluator::EvalOutcome::Blocked {
                crate::evaluator::EvalOutcome::Blocked
            } else {
                crate::evaluator::EvalOutcome::Continue
            }
        }
    };
    Ok(JudgedFinalText {
        outcome,
        evidence_denial,
        evidence_gate_released,
        passed_before,
        passed_after,
    })
}

async fn continue_after_unmet<P: ProviderClient>(
    ctx: &mut LoopCtx<'_>,
    provider: &P,
    state: &mut LoopState,
    ts: &mut TurnState,
    judged: JudgedFinalText,
) -> Result<TurnFlow> {
    let turn = ts.turn;
    let failed_summary = unmet_summary(ctx.goal);
    let feedback = if judged.evidence_gate_released {
        EVIDENCE_COMPLETION_BYPASS_FEEDBACK.to_string()
    } else {
        judged.evidence_denial.map_or_else(
            || format!("Acceptance not yet met:\n{failed_summary}\nContinue."),
            |denial| evidence_denial_feedback(denial).to_string(),
        )
    };
    let obs = StepObservation {
        source: ObservationSource::Evaluator,
        status: ObservationStatus::RecoverableFailure,
        feedback: Some(ModelFeedback::User { content: feedback }),
        terminal: None,
        signature: None,
    };
    let control = apply_observation(ctx.messages, &mut state.watchdog, obs);
    debug_assert!(matches!(control, LoopControl::Continue));
    ts.end_of_turn_conversation_changed = true;
    ctx.recorder.emit(
        "orchestration.step.completed",
        json!({ "step_id": format!("solo.turn.{turn}"), "turn": turn, "outcome": "criteria_failed_continue" }),
    )?;
    let progress = &mut state.progress;
    progress.note_turn(judged.passed_after > judged.passed_before, false);
    // A final-text turn has no real edit: criterion progress must not reset the edit-based safety counters.
    progress.note_safety_signals(false, false, false);
    let d_halt = crate::adaptive_safety_net::decide(
        progress,
        ctx.goal,
        &state.ledger,
        turn,
        &crate::adaptive_safety_net::Thresholds::DEFAULT,
        ctx.write_tools_offered,
    );
    // A denied evidence completion carries concrete next-step feedback. Do not
    // let the generic no-progress safety net terminate the same turn, including
    // the turn that just released the gate and asks the model to finish again.
    if d_halt.halt && judged.evidence_denial.is_none() {
        offer_wrapup_turn(
            provider,
            ctx.capabilities,
            ctx.goal,
            progress,
            &state.ledger,
            ctx.messages,
            ctx.recorder,
            turn,
            ctx.options.max_turns,
            ctx.write_tools_offered,
        )
        .await?;
        let attempts = &state.attempts;
        emit_no_progress_needs_decision(ctx.recorder, ctx.goal, progress, attempts)?;
        snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
        save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ts.ledger_dirty)?;
        return Ok(TurnFlow::Return(RunOutcome::NeedsDecision));
    }
    Ok(TurnFlow::NextTurn)
}
