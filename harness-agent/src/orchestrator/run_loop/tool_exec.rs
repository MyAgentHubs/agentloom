use super::*;

pub(super) struct EditPrep {
    pub(super) write_paths_for_progress: Vec<PathBuf>,
    pub(super) pre_edit_hashes: Vec<(PathBuf, u64)>,
    pub(super) scope_advisory: Vec<String>,
}

pub(super) enum Gated {
    Proceed(EditPrep),
    Flow(CallFlow),
}

pub(super) async fn gate_mutating_call(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &TurnState,
    tool: &dyn crate::tools::Tool,
    tool_call: &crate::provider::ToolCall,
    tool_calls: &[crate::provider::ToolCall],
    tool_index: usize,
) -> Result<Gated> {
    let name = tool_call.function.name.as_str();
    // Reject shell escape attempts before requesting approval.
    // Effects must not outlive this process, even with approval.
    if name == "shell_exec" {
        if let Some(rule) = shell_exec_escape_rule(&tool_call.function.arguments) {
            push_tool_rejection(
                ctx.recorder,
                ctx.messages,
                name,
                &tool_call.id,
                json!({ "error": "blocked: escape attempt", "rule": rule }).to_string(),
                Some(json!({
                    "error": format!("blocked: escape attempt ({rule})"),
                    "rule": rule,
                })),
            )?;
            return Ok(Gated::Flow(CallFlow::Next));
        }
    }
    let approval_id = format!("approval_{}", tool_call.id);
    let write_paths =
        match tool.write_targets(&tool_call.function.arguments, &ctx.options.workspace) {
            Ok(targets) => targets,
            Err(e) => {
                let content = match &e {
                    crate::error::HarnessError::Json(je)
                        if crate::tools::is_truncated_args(&tool_call.function.arguments, je) =>
                    {
                        json!({ "error": crate::tools::truncated_args_message(name) }).to_string()
                    }
                    _ => json!({ "error": format!("invalid path or arguments: {e}") }).to_string(),
                };
                push_tool_rejection(
                    ctx.recorder,
                    ctx.messages,
                    name,
                    &tool_call.id,
                    content,
                    None,
                )?;
                return Ok(Gated::Flow(CallFlow::Next));
            }
        };
    let write_paths_for_progress = write_paths.clone();
    let e = &state.evidence;
    if evidence_edit_should_block(name, &write_paths, &ctx.options.workspace, e) {
        let targets =
            evidence_edit_targets_in_workspace(name, &write_paths, &ctx.options.workspace)
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
        ctx.recorder.emit(
            "evidence.edit.blocked",
            json!({
                "turn": ts.turn,
                "tool": name,
                "targets": targets,
                "outcome": "require_probe",
                "edit_epoch": state.evidence.edit_epoch,
                "green_epoch": state.evidence.green_epoch,
                "signature": null,
            }),
        )?;
        push_tool_rejection(
            ctx.recorder,
            ctx.messages,
            name,
            &tool_call.id,
            EVIDENCE_EDIT_BLOCKED_GUIDANCE.to_string(),
            None,
        )?;
        return Ok(Gated::Flow(CallFlow::Next));
    }
    if name == "fs_write" {
        if let Some(reason) = write_paths
            .iter()
            .find_map(|p| crate::tools::fs_write::oversized_whole_write_reason(p, ctx.edit_format))
        {
            push_tool_rejection(
                ctx.recorder,
                ctx.messages,
                name,
                &tool_call.id,
                json!({ "error": reason }).to_string(),
                None,
            )?;
            return Ok(Gated::Flow(CallFlow::Next));
        }
    }
    let summary = guardrail_summary(name, &tool_call.function.arguments, &ctx.options.workspace);
    let req = GuardrailRequest {
        tool: name,
        summary,
        cwd: &ctx.options.workspace,
        write_paths: &write_paths,
        trusted: tool.guardrail_trusted(),
    };
    let gate_decision = match ctx
        .guardrails
        .gate(ctx.recorder, ctx.control, &approval_id, &req)
    {
        Ok(decision) => decision,
        Err(HarnessError::PermissionDenied(reason)) => {
            push_tool_rejection(
                ctx.recorder,
                ctx.messages,
                name,
                &tool_call.id,
                json!({
                    "error": format!(
                        "invalid path or arguments: permission denied: {reason}"
                    )
                })
                .to_string(),
                None,
            )?;
            return Ok(Gated::Flow(CallFlow::Next));
        }
        Err(e) => return Err(e),
    };
    match gate_decision {
        crate::guardrails::GateDecision::Approved => {}
        crate::guardrails::GateDecision::Rejected { reason } => {
            return Ok(Gated::Flow(
                gate_decision_rejected(ctx, state, ts, tool_call, tool_calls, tool_index, reason)
                    .await?,
            ));
        }
        crate::guardrails::GateDecision::Interrupted => {
            return Ok(Gated::Flow(gate_decision_interrupted(
                ctx, state, ts, tool_calls, tool_index,
            )?));
        }
    }
    approved_write_metadata(ctx, name, write_paths_for_progress)
}

fn approved_write_metadata(
    ctx: &mut LoopCtx<'_>,
    name: &str,
    write_paths_for_progress: Vec<PathBuf>,
) -> Result<Gated> {
    let mut pre_edit_hashes = Vec::new();
    if matches!(name, "fs_write" | "fs_edit") {
        pre_edit_hashes = write_paths_for_progress
            .iter()
            .filter_map(|path| file_content_hash(path).map(|hash| (path.clone(), hash)))
            .collect();
    }
    let scope_advisory = ctx
        .guardrails
        .scope_advisory_paths(&write_paths_for_progress);
    if !scope_advisory.is_empty() {
        ctx.recorder.emit(
            "scope.advisory",
            json!({ "tool": name, "paths": &scope_advisory }),
        )?;
    }

    Ok(Gated::Proceed(EditPrep {
        write_paths_for_progress,
        pre_edit_hashes,
        scope_advisory,
    }))
}

fn gate_decision_interrupted(
    ctx: &mut LoopCtx<'_>,
    state: &LoopState,
    ts: &TurnState,
    tool_calls: &[crate::provider::ToolCall],
    tool_index: usize,
) -> Result<CallFlow> {
    ctx.recorder.emit(
        "run.interrupted",
        json!({
            "step_id": "tool.execution",
            "resume_command": format!("myagent resume {}", ctx.run_id),
        }),
    )?;
    append_unpaired_tool_results(
        ctx.messages,
        &tool_calls[tool_index..],
        "interrupted before execution",
    );
    snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
    let mut ledger_dirty = ts.ledger_dirty;
    save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ledger_dirty)?;
    Ok(CallFlow::Return(RunOutcome::Interrupted))
}

async fn gate_decision_rejected(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &TurnState,
    tool_call: &crate::provider::ToolCall,
    tool_calls: &[crate::provider::ToolCall],
    tool_index: usize,
    reason: crate::guardrails::RejectReason,
) -> Result<CallFlow> {
    let name = tool_call.function.name.as_str();
    let error = if reason == crate::guardrails::RejectReason::ApprovalUnavailable {
        state.approval_unavailable_seen = true;
        "approval channel unavailable"
    } else {
        state.consecutive_rejections += 1;
        "denied by user"
    };
    // Feed rejection back as a tool failure so the run can continue.
    emit_tool_rejection(
        ctx.recorder,
        name,
        &tool_call.id,
        "permission denied by user",
        Some(json!({ "error": error })),
    )?;
    let terminal = if reason == crate::guardrails::RejectReason::UserRejected
        && state.consecutive_rejections >= REJECT_SELF_STOP
    {
        Some(RunOutcome::Blocked)
    } else {
        None
    };
    let obs = StepObservation {
        source: ObservationSource::Gate,
        status: ObservationStatus::PolicyRejected,
        feedback: Some(ModelFeedback::Tool {
            tool_call_id: tool_call.id.clone(),
            content: "permission denied by user".to_string(),
        }),
        terminal,
        signature: Some(format!("gate:{name}:{error}")),
    };
    match apply_observation(ctx.messages, &mut state.watchdog, obs) {
        LoopControl::Continue => Ok(CallFlow::Next),
        LoopControl::Terminate(RunOutcome::Blocked) => {
            append_unpaired_tool_results(
                ctx.messages,
                &tool_calls[(tool_index + 1)..],
                "blocked before execution",
            );
            ctx.recorder.emit(
                "run.blocked",
                json!({
                    "turns": ts.turn,
                    "attempts": state.attempts.count(),
                    "reason": "rejected_repeatedly",
                    "criteria": ctx.goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                }),
            )?;
            snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
            let mut ledger_dirty = ts.ledger_dirty;
            save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ledger_dirty)?;
            Ok(CallFlow::Return(RunOutcome::Blocked))
        }
        LoopControl::Terminate(outcome) => Ok(CallFlow::Return(outcome)),
    }
}

pub(super) async fn execute_and_record(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &mut TurnState,
    sig: &mut ToolTurnSignals,
    tool: &dyn crate::tools::Tool,
    tool_call: &crate::provider::ToolCall,
    prep: EditPrep,
) -> Result<CallFlow> {
    let EditPrep {
        write_paths_for_progress,
        pre_edit_hashes,
        scope_advisory,
    } = prep;
    let name = tool_call.function.name.as_str();
    let mut ctx_inner = ToolContext {
        workspace: &ctx.options.workspace,
        recorder: ctx.recorder,
        file_ledger: &mut state.file_ledger,
        network: ctx.options.network,
        fs_read_scope: ctx.options.fs_read_scope,
        extra_read_roots: &ctx.options.extra_read_roots,
    };
    let tool_result = match tool.execute(&mut ctx_inner, tool_call).await {
        Ok(outcome) => outcome,
        Err(e) => return Err(e),
    };
    match tool_result.status {
        ToolStatus::Success => {
            if matches!(name, "fs_write" | "fs_edit") {
                for (path, hash) in &pre_edit_hashes {
                    let path = path.to_string_lossy();
                    state.progress.seed_edit_hash(path.as_ref(), *hash);
                }
            }
            note_success_signals(
                state,
                sig,
                tool,
                name,
                &tool_call.function.arguments,
                &write_paths_for_progress,
            );
            if tool_result.invalidates_verification {
                state.verify_debt += 1;
                ts.turn_had_mutating_call = true;
                // MCP side effects invalidate verification without implying a workspace edit.
                // Keep them out of the real-edit safety counters.
                if !tool.is_mcp() {
                    ts.turn_had_edit = true;
                }
            } else if name == "shell_exec" {
                if let Some(command) = shell_command_for_progress(&tool_call.function.arguments) {
                    // Count new commands as progress regardless of their exit status.
                    let command_is_novel =
                        state.progress.note_shell_command(&command).is_progress();
                    sig.turn_had_progress |= command_is_novel;
                    sig.turn_had_novel_shell |= command_is_novel;
                }
            }
            state.consecutive_rejections = 0;
            ctx.goal.record_evidence(format!("tool:{name} completed"));
            let content = if scope_advisory.is_empty() {
                tool_result.content
            } else {
                format!(
                    "{}\n\n[scope] 注意：{} 超出本步声明的 files_scope（已放行·任务跑完会统一核对）。确需正式扩范围请用 propose_scope_change(kind=scope, paths=[...])。",
                    tool_result.content,
                    scope_advisory.join(", ")
                )
            };
            ctx.messages
                .push(ChatMessage::tool(tool_call.id.clone(), content));
        }
        ToolStatus::FailedRecoverable | ToolStatus::Rejected => {
            ctx.messages
                .push(ChatMessage::tool(tool_call.id.clone(), tool_result.content));
            return Ok(CallFlow::Next);
        }
    }
    Ok(CallFlow::Next)
}

fn note_success_signals(
    state: &mut LoopState,
    sig: &mut ToolTurnSignals,
    tool: &dyn crate::tools::Tool,
    name: &str,
    arguments: &str,
    write_paths_for_progress: &[PathBuf],
) {
    let progress = &mut state.progress;
    match name {
        "fs_read" | "grep" | "ls" | "glob" => {
            sig.turn_had_new_read |=
                progress.note_read(name, arguments) == crate::run_progress::StepInfoGain::NewRead;
        }
        "fs_write" | "fs_edit" => {
            for path in write_paths_for_progress {
                if let Some(hash) = file_content_hash(path) {
                    let path = path.to_string_lossy();
                    sig.turn_had_progress |=
                        progress.note_edit_result(path.as_ref(), hash).is_progress();
                } else {
                    let path = path.to_string_lossy();
                    sig.turn_had_progress |= progress.note_edit(path.as_ref()).is_progress();
                }
            }
            for path in write_paths_for_progress {
                sig.edited_paths_this_turn
                    .insert(crate::tools::fs_read::canonicalize_lenient(path));
            }
        }
        _ => {
            // A companion to the three-way turn-concept split above, correcting an earlier note that mislabeled this as
            // a "known overlap" (that note was itself the root cause of this bug): `McpToolProxy` (`mcp/tool.rs`, used by
            // every regular MCP tool discovered via `tools/list`, e.g. dispatch_worker/ask_user) reports success as
            // `ToolOutcome::success_mutating` (`invalidates_verification: true`). The `if tool_result.invalidates_verification`
            // branch below used to unconditionally set `turn_had_edit` true, welding "this turn actually edited a workspace
            // file" and "this turn made a side-effecting call" into a single variable. That made `note_safety_signals`
            // treat every turn of an MCP-only run (e.g. a lead dispatching entirely through mcp__agentloom__* with
            // `--disallow-tools fs_edit,fs_write,shell_exec` removing native write tools) as "just edited", zeroing both
            // `consecutive_stale_turns` and `turns_since_last_real_edit` every turn—so the four-tier `adaptive_safety_net`
            // escalation could never reach its threshold, letting a repeat loop burn through the full 120-turn budget
            // unchecked. `invalidates_verification` now only sets the new `turn_had_mutating_call` (`turn_had_edit` is set
            // only by a non-MCP write tool), so the novelty-dedup signal described above is the only stale-counting entry
            // point an MCP-only run actually relies on, and it is no longer preempted and zeroed by this path.
            if tool.is_mcp() {
                sig.turn_had_new_read |= progress.note_mcp_call(name, arguments)
                    == crate::run_progress::StepInfoGain::NewRead;
            }
        }
    }
}
