use super::*;

pub(super) enum Prepared {
    Ready {
        effective_disallowed: std::collections::BTreeSet<String>,
        tools: Vec<serde_json::Value>,
        wire: Vec<ChatMessage>,
    },
    Return(RunOutcome),
}

pub(super) fn prepare_turn(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    turn: usize,
) -> Result<Prepared> {
    if handle_control(ctx.control, ctx.recorder, ctx.run_id, "provider.next_turn")? {
        return Ok(Prepared::Return(RunOutcome::Interrupted));
    }
    let d = crate::adaptive_safety_net::decide(
        &state.progress,
        ctx.goal,
        &state.ledger,
        turn,
        &crate::adaptive_safety_net::Thresholds::DEFAULT,
        ctx.write_tools_offered,
    );
    let mut effective_disallowed = ctx.options.disallowed_tools.clone();
    if ctx.options.evidence_gate == EvidenceGate::Off {
        effective_disallowed.insert("register_issue_probe".to_string());
    }
    // F1 follows the K2 precedent: narrow_explore removes grep/ls/glob only when write
    // tools are available. Runs without fs_write/fs_edit, such as MCP-only lead runs,
    // have no other recovery mechanism: a novel read is their remaining way to clear
    // stale counters before halt. Runs with write tools retain the existing behavior.
    if d.narrow_explore && ctx.write_tools_offered {
        for tool in ["grep", "ls", "glob"] {
            effective_disallowed.insert(tool.to_string());
        }
    }
    let tools = build_offered_tools_with_roots(
        ctx.registry,
        ctx.capabilities,
        ctx.options.network,
        ctx.options.native_search_enabled,
        &effective_disallowed,
        &ctx.options.extra_read_roots,
    );
    ctx.recorder.emit(
        "orchestration.step.started",
        json!({
            "step_id": format!("solo.turn.{turn}"),
            "turn": turn,
        }),
    )?;

    let frame = crate::context_builder::render_state_frame(
        ctx.goal,
        &state.progress,
        turn,
        ctx.options.max_turns,
        d.level,
        &state.ledger,
        ctx.write_tools_offered,
    );
    let wire = crate::context_builder::build_wire_messages(ctx.messages, &frame);
    // Fit the temporary wire to the budget before calling the model; canonical messages and the journal stay unchanged.
    let limits = crate::context_budget::BudgetLimits::from_capabilities(ctx.capabilities);
    // Reserve context for the tool schemas sent on this turn, especially large MCP schemas.
    let tools_reserve = crate::context_budget::estimate_tools_tokens(&tools, &limits);
    let wire = match crate::context_budget::fit_to_budget(wire, &limits, tools_reserve) {
        crate::context_budget::FitOutcome::Fit(msgs) => msgs,
        crate::context_budget::FitOutcome::Overflow { estimate, budget } => {
            crate::context_budget::autocompact::emit_context_budget_exhausted(
                ctx.recorder,
                turn,
                estimate,
                budget,
            )?;
            return Ok(Prepared::Return(RunOutcome::NeedsDecision));
        }
    };
    Ok(Prepared::Ready {
        effective_disallowed,
        tools,
        wire,
    })
}

pub(super) enum Admitted {
    Response(crate::provider::ProviderResponse),
    Flow(TurnFlow),
}

pub(super) async fn call_provider<P: ProviderClient>(
    ctx: &mut LoopCtx<'_>,
    provider: &P,
    state: &mut LoopState,
    ts: &mut TurnState,
    wire: Vec<ChatMessage>,
    tools: &[serde_json::Value],
) -> Result<Admitted> {
    let response = provider.next_turn(&wire, tools, ctx.recorder).await?;
    ctx.recorder.emit(
        "provider.turn.finished",
        json!({
            "turn": ts.turn,
            "finish_reason": finish_reason_str(response.finish_reason.as_ref()),
            "text_len": response.text.chars().count(),
            "reasoning_len": response.reasoning.chars().count(),
            "tool_calls": response.tool_calls.len(),
        }),
    )?;
    if let Some(flow) = handle_stream_interruption(ctx.recorder, state, ts, &response)? {
        return Ok(Admitted::Flow(flow));
    }
    state.consecutive_stream_interruptions = 0;
    let reasoning_content = {
        let r = response.reasoning.trim();
        if r.is_empty() {
            None
        } else {
            Some(response.reasoning.clone())
        }
    };
    ctx.messages.push(ChatMessage::assistant(
        response.text.clone(),
        reasoning_content,
        response.tool_calls.clone(),
    ));
    if response.tool_calls.is_empty() {
        snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
    }

    if let Some(flow) = handle_truncated_response(ctx, state, ts, &response)? {
        return Ok(Admitted::Flow(flow));
    }
    state.consecutive_truncations = 0;
    Ok(Admitted::Response(response))
}

fn handle_stream_interruption(
    recorder: &mut EventRecorder,
    state: &mut LoopState,
    ts: &mut TurnState,
    response: &crate::provider::ProviderResponse,
) -> Result<Option<TurnFlow>> {
    // A transport-level SSE interruption is not an empty model turn. Partial reasoning
    // must not enter messages, run evaluation, emit ModelFeedback, or change truncation
    // and safety-net counters. Retry in place and surface a real provider error only
    // after CONSECUTIVE_STREAM_INTERRUPTION_LIMIT consecutive interruptions.
    //
    // M-1 narrowing: classify an interruption only when finish_reason is also absent.
    // Some providers or proxies close a stream after complete text/tool calls and a
    // finish reason; those responses remain usable even when interruption is Some.
    // A missing finish reason is the signature of the actual interrupted-stream case.
    if response.finish_reason.is_none() {
        if let Some(err_text) = response.interruption.as_deref() {
            state.consecutive_stream_interruptions += 1;
            if state.consecutive_stream_interruptions >= CONSECUTIVE_STREAM_INTERRUPTION_LIMIT {
                return Err(HarnessError::Provider(format!(
                    "stream interrupted {} consecutive times: {err_text}",
                    state.consecutive_stream_interruptions
                )));
            }
            recorder.emit(
                "orchestration.step.completed",
                json!({
                    "step_id": format!("solo.turn.{}", ts.turn),
                    "turn": ts.turn,
                    "outcome": "stream_interrupted_continue",
                }),
            )?;
            return Ok(Some(TurnFlow::NextTurn));
        }
    }
    Ok(None)
}

fn handle_truncated_response(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &mut TurnState,
    response: &crate::provider::ProviderResponse,
) -> Result<Option<TurnFlow>> {
    if response.finish_reason == Some(crate::provider::FinishReason::Length) {
        // A truncated response can still contain fully parsed tool calls in the assistant
        // message. Both paths below append a non-tool message next, so fill missing tool
        // results first to preserve the pairing invariant required by conversation snapshots.
        append_unpaired_tool_results(
            ctx.messages,
            &response.tool_calls,
            "output truncated before tool results",
        );
        state.consecutive_truncations += 1;
        state.progress.note_turn(false, false);
        state.progress.note_safety_signals(false, false, false);
        let consecutive_truncations = state.consecutive_truncations;
        if consecutive_truncations >= CONSECUTIVE_TRUNCATION_LIMIT {
            ctx.recorder.emit(
                "orchestration.step.completed",
                json!({
                    "step_id": format!("solo.turn.{}", ts.turn),
                    "turn": ts.turn,
                    "outcome": "output_truncated_halt",
                }),
            )?;
            ctx.recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "consecutive_output_truncation",
                    "turn": ts.turn,
                    "consecutive_truncated_turns": consecutive_truncations,
                    "detail": format!("连续 {consecutive_truncations} 轮输出被截断，模型可能陷入失控推理"),
                }),
            )?;
            snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
            return Ok(Some(TurnFlow::Return(RunOutcome::NeedsDecision)));
        }
        let obs = StepObservation {
            source: ObservationSource::Evaluator,
            status: ObservationStatus::RecoverableFailure,
            feedback: Some(ModelFeedback::User {
                content: "你上一轮的输出被输出长度上限截断了（想得太长）。请不要继续长篇推理，直接给出工具调用（例如 fs_edit），把你已经想好的改动落地。".to_string(),
            }),
            terminal: None,
            signature: None,
        };
        let control = apply_observation(ctx.messages, &mut state.watchdog, obs);
        debug_assert!(matches!(control, LoopControl::Continue));
        ctx.recorder.emit(
            "orchestration.step.completed",
            json!({
                "step_id": format!("solo.turn.{}", ts.turn),
                "turn": ts.turn,
                "outcome": "output_truncated_continue",
            }),
        )?;
        snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
        return Ok(Some(TurnFlow::NextTurn));
    }
    Ok(None)
}
