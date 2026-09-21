use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_budget_exhausted<P: ProviderClient>(
    provider: &P,
    capabilities: &crate::provider::ProviderCapabilities,
    options: &RunOptions,
    paths: &RunPaths,
    run_id: &str,
    recorder: &mut EventRecorder,
    goal: &GoalState,
    messages: &mut Vec<ChatMessage>,
    state: &LoopState,
    write_tools_offered: bool,
) -> Result<RunOutcome> {
    // Give one wrap-up turn (K3) before the turn budget is exhausted—when the lead hits max_turns, it otherwise disappears immediately,
    // and the user sees no closing message; this is exactly one turn, then it unconditionally proceeds to the terminal state whether or not the model cooperates.
    offer_wrapup_turn(
        provider,
        capabilities,
        goal,
        &state.progress,
        &state.ledger,
        messages,
        recorder,
        options.max_turns,
        options.max_turns,
        write_tools_offered,
    )
    .await?;
    snapshot(paths, run_id, options, messages)?;
    emit_budget_exhausted_needs_decision(
        recorder,
        goal,
        &state.progress,
        &state.attempts,
        write_tools_offered,
    )?;
    Ok(RunOutcome::NeedsDecision)
}

pub(super) async fn decide_completion<P: ProviderClient>(
    ctx: &mut LoopCtx<'_>,
    provider: &P,
    state: &mut LoopState,
    turn: usize,
    ts: &mut TurnState,
    sig: &ToolTurnSignals,
) -> Result<Option<RunOutcome>> {
    let progress = &mut state.progress;
    progress.note_turn(sig.turn_had_progress, sig.turn_had_new_read);
    progress.note_safety_signals(
        ts.turn_had_edit,
        sig.turn_had_new_read,
        sig.turn_had_novel_shell,
    );
    let d_halt = crate::adaptive_safety_net::decide(
        progress,
        ctx.goal,
        &state.ledger,
        turn,
        &crate::adaptive_safety_net::Thresholds::DEFAULT,
        ctx.write_tools_offered,
    );
    let completion_gate = &state.completion_gate;
    if completion_gate.ready_to_finalize(turn, ts.turn_had_edit || ts.turn_had_mutating_call)
        || d_halt.halt
    {
        let evidence_was_bypassed = state.evidence.bypassed;
        let evidence_denials_before = state.evidence.consecutive_completion_denials;
        let finalize_outcome = try_finalize(
            ctx.goal,
            &mut state.evidence,
            ctx.options.contract_policy,
            &ctx.options.workspace,
            ctx.judge,
            ctx.recorder,
            ctx.options.network,
            ctx.options.fs_write_fence,
            &mut state.eval_round,
            turn,
            "engine_finalize",
        )
        .await?;
        if finalize_outcome == FinalizeOutcome::Completed {
            snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
            save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ts.ledger_dirty)?;
            return Ok(Some(RunOutcome::Completed));
        }
        let evidence_gate_released = !evidence_was_bypassed && state.evidence.bypassed;
        let evidence_completion_denied =
            state.evidence.consecutive_completion_denials > evidence_denials_before;
        // Not fully passed: revoke the candidate
        state.completion_gate.disarm();
        if d_halt.halt && !evidence_completion_denied {
            // no_progress backstop: exit via the original path (give one wrap-up turn before cutting it off — K3)
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
            return Ok(Some(RunOutcome::NeedsDecision));
        }
        // Fast-path grace triggered but not fully passed: feed back the unmet items and continue the loop
        let feedback = if evidence_gate_released {
            Some(EVIDENCE_COMPLETION_BYPASS_FEEDBACK.to_string())
        } else if finalize_verdict(ctx.goal, None).is_ok() {
            let evidence = &state.evidence;
            evidence
                .ready()
                .err()
                .map(evidence_denial_feedback)
                .map(str::to_string)
        } else {
            None
        }
        .unwrap_or_else(|| {
            format!(
                "Acceptance not yet met:\n{}\nContinue.",
                unmet_summary(ctx.goal)
            )
        });
        let obs = StepObservation {
            source: ObservationSource::Evaluator,
            status: ObservationStatus::RecoverableFailure,
            feedback: Some(ModelFeedback::User { content: feedback }),
            terminal: None,
            signature: None,
        };
        match apply_observation(ctx.messages, &mut state.watchdog, obs) {
            LoopControl::Continue => {
                ts.end_of_turn_conversation_changed = true;
            }
            LoopControl::Terminate(outcome) => return Ok(Some(outcome)),
        }
    }
    Ok(None)
}

pub(super) fn emit_tool_results_added(ctx: &mut LoopCtx<'_>, turn: usize) -> Result<()> {
    ctx.recorder.emit(
        "orchestration.step.completed",
        json!({
            "step_id": format!("solo.turn.{turn}"),
            "turn": turn,
            "outcome": "tool_results_added",
        }),
    )?;
    Ok(())
}
