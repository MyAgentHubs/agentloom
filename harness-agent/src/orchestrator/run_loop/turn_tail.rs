use super::*;

pub(super) async fn run_format_reflex_step(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    edited_paths_this_turn: &std::collections::BTreeSet<std::path::PathBuf>,
    ts: &mut TurnState,
) -> Result<Option<RunOutcome>> {
    if !edited_paths_this_turn.is_empty() {
        let format_result = crate::format_reflex::run_format_reflex(
            edited_paths_this_turn,
            &ctx.options.workspace,
            ctx.options.fs_write_fence,
        )
        .await;
        for oc in &format_result.outcomes {
            if oc.changed {
                // Ledger sync (hard requirement): prevents the model's edit from being bounced back with a spurious "file changed, please re-read"
                state.file_ledger.record(
                    &oc.path.to_string_lossy(),
                    &oc.after,
                    crate::tools::fs_edit::mtime_ms(&oc.path),
                    true,
                );
                ctx.recorder.emit(
                    "format.reflex.applied",
                    json!({ "path": oc.path.to_string_lossy() }),
                )?;
            }
        }
        let mut fmt_feedback = String::new();
        for oc in &format_result.outcomes {
            if oc.changed {
                if !fmt_feedback.is_empty() {
                    fmt_feedback.push('\n');
                }
                fmt_feedback.push_str(&crate::format_reflex::format_change_feedback(
                    &oc.path, &oc.before, &oc.after,
                ));
            }
        }
        for failure in &format_result.failures {
            if !fmt_feedback.is_empty() {
                fmt_feedback.push('\n');
            }
            fmt_feedback.push_str(&crate::format_reflex::format_failure_feedback(failure));
        }
        if !fmt_feedback.is_empty() {
            ctx.recorder
                .emit("format.reflex.feedback", json!({ "text": &fmt_feedback }))?;
            let obs = StepObservation {
                source: ObservationSource::Validation,
                status: ObservationStatus::Ok,
                feedback: Some(ModelFeedback::User {
                    content: fmt_feedback,
                }),
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
    }
    Ok(None)
}

pub(super) async fn apply_evidence_workspace_change(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    turn: usize,
    ts: &mut TurnState,
) -> Result<()> {
    let evidence_workspace_change =
        if state.evidence.mode == EvidenceGate::On && state.evidence.probe.is_some() {
            if state.evidence.workspace_baseline.is_some() {
                crate::orchestrator::probe_runner::workspace_changed_since(
                    &ctx.options.workspace,
                    state.evidence.workspace_baseline.as_deref(),
                    ISSUE_PROBE_TIMEOUT_S,
                    ctx.options.network,
                    ctx.options.fs_write_fence,
                )
                .await?
            } else if ts.turn_had_mutating_call {
                // Non-Git workspaces cannot produce a controlled snapshot. Preserve the
                // existing fs_write/fs_edit behavior there without guessing about shell_exec.
                // Read `ts.turn_had_mutating_call` (the conservative reading, covering
                // fs_write/fs_edit union MCP mutating calls) rather than the narrower
                // `ts.turn_had_edit` — with no git snapshot to diff against, it's better
                // to conservatively assume "the workspace may have changed, stale evidence
                // should be invalidated" than to under-detect via the narrower signal and
                // leave evidence that should have been invalidated still marked valid.
                crate::orchestrator::probe_runner::WorkspaceChange::Changed
            } else {
                crate::orchestrator::probe_runner::WorkspaceChange::Unavailable
            }
        } else {
            crate::orchestrator::probe_runner::WorkspaceChange::Unavailable
        };
    match evidence_workspace_change {
        crate::orchestrator::probe_runner::WorkspaceChange::Changed => {
            // git actually confirms a workspace change: it's both a real edit and, of course, a mutating call.
            ts.turn_had_edit = true;
            ts.turn_had_mutating_call = true;
            if let Some(feedback) = rerun_evidence_after_edit(
                &mut state.evidence,
                &ctx.options.workspace,
                ISSUE_PROBE_TIMEOUT_S,
                ctx.options.network,
                ctx.options.fs_write_fence,
                turn,
                ctx.recorder,
            )
            .await?
            {
                ctx.messages.push(ChatMessage::user(feedback));
                ts.end_of_turn_conversation_changed = true;
            }
        }
        crate::orchestrator::probe_runner::WorkspaceChange::Unverifiable(reason) => {
            // Verification failure is not proof of an edit, so it must not reset the
            // completion-denial liveness streak. It still invalidates any old green.
            // Only set `ts.turn_had_mutating_call` — the safety-net counters driven by
            // `ts.turn_had_edit` (`note_safety_signals`) and the git checkpoint both
            // require evidence of a "real edit", and workspace-unverifiable is precisely
            // not that evidence (the English comment above makes this same point; the
            // prior code contradicted it by still setting `ts.turn_had_edit` true here).
            state.evidence.note_workspace_unverifiable();
            ts.turn_had_mutating_call = true;
            emit_evidence_workspace_unverifiable(&state.evidence, ctx.recorder, turn, &reason)?;
            ctx.messages
                .push(ChatMessage::user(evidence_workspace_unverifiable_feedback(
                    &reason,
                )));
            ts.end_of_turn_conversation_changed = true;
        }
        crate::orchestrator::probe_runner::WorkspaceChange::Unchanged
        | crate::orchestrator::probe_runner::WorkspaceChange::Unavailable => {}
    }
    Ok(())
}

pub(super) async fn run_immediate_diagnostics_step(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    turn: usize,
    edited_paths_this_turn: &std::collections::BTreeSet<std::path::PathBuf>,
    ts: &mut TurnState,
) -> Result<Option<RunOutcome>> {
    if ts.turn_had_edit {
        let tool_call_tag = format!("immediate_{turn}");
        let immediate = collect_immediate_edit_diagnostics(
            ctx.goal,
            edited_paths_this_turn,
            &ctx.options.workspace,
            ctx.options.network,
            ctx.options.fs_write_fence,
            ctx.recorder,
            &tool_call_tag,
            ctx.options.verify_reflex_debt,
            state.verify_debt,
            &state.progress,
        )
        .await?;
        let verify_reflex_will_run = immediate.verify_reflex_will_run;
        let now = immediate.diagnostics;
        ts.turn_had_immediate_diagnostics = !now.is_empty();
        let new_diags: Vec<_> = now
            .iter()
            .filter(|d| {
                !state
                    .last_probe_diags
                    .iter()
                    .any(|p| p.root_cause_key == d.root_cause_key)
            })
            .cloned()
            .collect();
        if verify_reflex_will_run {
            let d = &mut state.last_probe_diags;
            d.retain(|x| x.error_code.as_deref() != Some("PY_SYNTAX"));
            state.last_probe_diags =
                merge_diagnostics(std::mem::take(&mut state.last_probe_diags), now);
        } else {
            state.last_probe_diags = now;
        }
        if let Some(lines) = immediate_diagnostic_feedback(&new_diags) {
            let obs = StepObservation {
                source: ObservationSource::Validation,
                status: ObservationStatus::ValidationFailed,
                feedback: Some(ModelFeedback::User { content: lines }),
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
    }

    if ts.turn_had_edit || ts.turn_had_mutating_call {
        // The completion gate wants "did this turn do anything worth a second look" —
        // both a real edit and an MCP-style side-effecting call count. Behavior here
        // stays bit-for-bit the same as before the narrowing (an MCP-only turn still
        // won't be rushed toward "ready to finish").
        state.completion_gate.note_edit(turn);
    }
    Ok(None)
}

pub(super) async fn run_verify_reflex_step(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    turn: usize,
    ts: &mut TurnState,
    sig: &mut ToolTurnSignals,
) -> Result<Option<RunOutcome>> {
    let progress = &mut state.progress;
    let verify_debt = state.verify_debt;
    let verify_reflex_will_run = verify_reflex_should_run(
        ctx.options.verify_reflex_debt,
        verify_debt,
        ctx.goal,
        progress,
    );
    if verify_reflex_will_run {
        state.reflex_round += 1;
        let validation = crate::evaluator::reflex_validate(
            ctx.goal,
            &ctx.options.workspace,
            ctx.options.network,
            ctx.options.fs_write_fence,
            state.reflex_round,
            state.verify_debt,
            ctx.recorder,
        )
        .await?;
        let mut status_changed = false;
        for (criterion_id, passed) in &validation.checked {
            status_changed |= progress.note_criterion_check(criterion_id, *passed);
        }
        sig.turn_had_progress |= progress.note_check(status_changed).is_progress();
        if let Some(feedback) = validation.feedback {
            let crate::evaluator::ReflexFeedback {
                feedback,
                signature,
                diagnostics: _,
                candidates,
            } = feedback;
            progress.set_ripple_candidates(candidates);
            let obs = StepObservation {
                source: ObservationSource::Validation,
                status: ObservationStatus::ValidationFailed,
                feedback: Some(ModelFeedback::User { content: feedback }),
                terminal: None,
                signature: Some(signature),
            };
            match apply_observation(ctx.messages, &mut state.watchdog, obs) {
                LoopControl::Continue => {
                    ts.end_of_turn_conversation_changed = true;
                }
                LoopControl::Terminate(RunOutcome::Blocked) => {
                    let (signature, repeats) = state.watchdog.tripped().unwrap_or(("unknown", 0));
                    ctx.recorder.emit(
                        "run.needs_decision",
                        json!({
                            "reason": "blocked_questions",
                            "contract_version": ctx.goal.contract.version,
                            "blocked_reason": "stuck_repeating",
                            "questions": [],
                            "agent_diagnosis": null,
                            "evidence_refs": [],
                            "signature": signature,
                            "repeats": repeats,
                            "failed_criteria": ctx.goal.contract.criteria.iter()
                                .filter(|c| !matches!(
                                    c.status,
                                    crate::goal::CriterionStatus::Passed
                                        | crate::goal::CriterionStatus::Waived
                                ))
                                .map(|c| c.id.clone())
                                .collect::<Vec<_>>(),
                            "criteria": ctx.goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                            "attempts_summary": { "turns": turn, "attempts": state.attempts.count() },
                            "trigger": "harness",
                        }),
                    )?;
                    snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
                    save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ts.ledger_dirty)?;
                    return Ok(Some(RunOutcome::NeedsDecision));
                }
                LoopControl::Terminate(outcome) => return Ok(Some(outcome)),
            }
        } else {
            progress.clear_ripple_candidates();
            state.watchdog.reset();
            if arm_completion_gate_after_clean_reflex(
                &mut state.completion_gate,
                turn,
                ts.turn_had_immediate_diagnostics,
            ) {
                let obs = StepObservation {
                    source: ObservationSource::Validation,
                    status: ObservationStatus::Ok,
                    feedback: Some(ModelFeedback::User {
                        content: WRAPUP_NUDGE.to_string(),
                    }),
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
        }
        verify_reflex_clear_debt(&mut state.verify_debt);
    }
    Ok(None)
}

pub(super) fn emit_edit_checkpoint(
    ctx: &mut LoopCtx<'_>,
    turn: usize,
    ts: &TurnState,
) -> Result<()> {
    if ts.turn_had_edit {
        match crate::git_archive::checkpoint(&ctx.options.workspace) {
            Some(b) => ctx.recorder.emit(
                "safety_net.checkpoint",
                json!({ "turn": turn, "stash_ref": b.pre_ref, "untracked": b.pre_untracked.len() }),
            )?,
            None => ctx
                .recorder
                .emit("safety_net.checkpoint_skipped", json!({ "turn": turn }))?,
        };
    }
    Ok(())
}
