use super::*;

pub(super) async fn handle_update_working_state(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &mut TurnState,
    tool_call: &crate::provider::ToolCall,
) -> Result<CallFlow> {
    let update: crate::working_ledger::LedgerUpdate =
        match serde_json::from_str(&tool_call.function.arguments) {
            Ok(u) => u,
            Err(e) => {
                ctx.messages.push(ChatMessage::tool(
                    tool_call.id.clone(),
                    format!(
                        "update_working_state: malformed arguments; {e}. \
                         Provide valid JSON for the working-state update."
                    ),
                ));
                return Ok(CallFlow::Next);
            }
        };
    let applied = state.ledger.apply(&tool_call.id, update);
    if applied {
        ts.ledger_dirty = true;
    }
    ctx.messages.push(ChatMessage::tool(
        tool_call.id.clone(),
        json!({
            "status": if applied { "updated" } else { "duplicate_ignored" },
        })
        .to_string(),
    ));
    Ok(CallFlow::Next)
}

pub(super) async fn handle_register_issue_probe(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &TurnState,
    tool_call: &crate::provider::ToolCall,
) -> Result<CallFlow> {
    let feedback = register_issue_probe_call(
        &tool_call.function.arguments,
        &mut state.evidence,
        &mut state.probe_registration_attempts,
        &ctx.options.workspace,
        &ctx.paths.run_dir.join("probes"),
        ts.turn,
        ctx.options.network,
        ctx.options.fs_write_fence,
        ctx.recorder,
    )
    .await?;
    ctx.messages
        .push(ChatMessage::tool(tool_call.id.clone(), feedback));
    Ok(CallFlow::Next)
}

pub(super) async fn handle_block_with_questions(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &TurnState,
    tool_calls: &[crate::provider::ToolCall],
    tool_index: usize,
) -> Result<CallFlow> {
    let tool_call = &tool_calls[tool_index];
    #[derive(serde::Deserialize)]
    struct BlockWithQuestionsArgs {
        blocked_reason: String,
        questions: Vec<String>,
        #[serde(default)]
        agent_diagnosis: Option<String>,
        #[serde(default)]
        failed_criteria: Vec<String>,
        #[serde(default)]
        evidence_refs: Vec<String>,
    }
    let mut args: BlockWithQuestionsArgs = match serde_json::from_str(&tool_call.function.arguments)
    {
        Ok(a) => a,
        Err(e) => {
            ctx.messages.push(ChatMessage::tool(
                tool_call.id.clone(),
                format!(
                    "block_with_questions: malformed arguments; {e}. \
                         Provide valid JSON with `blocked_reason` and `questions`."
                ),
            ));
            return Ok(CallFlow::Next);
        }
    };
    args.questions.truncate(3);
    ctx.recorder.emit(
        "run.needs_decision",
        json!({
            "reason": "blocked_questions",
            "contract_version": ctx.goal.contract.version,
            "blocked_reason": args.blocked_reason,
            "questions": args.questions,
            "agent_diagnosis": args.agent_diagnosis,
            "failed_criteria": args.failed_criteria,
            "evidence_refs": args.evidence_refs,
            "attempts_summary": { "turns": ts.turn, "attempts": state.attempts.count() },
            "trigger": "agent",
        }),
    )?;
    ctx.messages.push(ChatMessage::tool(
        tool_call.id.clone(),
        json!({
            "status": "blocked_questions",
            "reason": args.blocked_reason
        })
        .to_string(),
    ));
    append_unpaired_tool_results(
        ctx.messages,
        &tool_calls[(tool_index + 1)..],
        &json!({
            "status": "skipped",
            "reason": "superseded by blocked_questions",
        })
        .to_string(),
    );
    snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
    let mut ledger_dirty = ts.ledger_dirty;
    save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ledger_dirty)?;
    Ok(CallFlow::Return(RunOutcome::NeedsDecision))
}

pub(super) async fn handle_propose_scope_change(
    ctx: &mut LoopCtx<'_>,
    state: &LoopState,
    ts: &TurnState,
    tool_calls: &[crate::provider::ToolCall],
    tool_index: usize,
) -> Result<CallFlow> {
    let tool_call = &tool_calls[tool_index];
    #[derive(serde::Deserialize, Clone, Copy)]
    #[serde(rename_all = "snake_case")]
    enum BoundaryKind {
        Scope,
        Objective,
        Constraint,
    }
    #[derive(serde::Deserialize)]
    struct ScopeArgs {
        kind: BoundaryKind,
        detail: String,
        #[serde(default)]
        paths: Vec<String>,
    }
    let args: ScopeArgs = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(a) => a,
        Err(e) => {
            ctx.messages.push(ChatMessage::tool(
                tool_call.id.clone(),
                format!(
                    "propose_scope_change: malformed arguments; {e}. \
                         Provide valid JSON with `kind` and `detail` (optionally `paths`)."
                ),
            ));
            return Ok(CallFlow::Next);
        }
    };

    // Extend the live file allowlist and continue when concrete scope paths are provided.
    if matches!(args.kind, BoundaryKind::Scope) && !args.paths.is_empty() {
        let outcome = scope_change_result::build_scope_change_outcome(
            &args.paths,
            &args.detail,
            ctx.guardrails.extend_files_scope(&args.paths),
        );
        if let Some(event) = outcome.extended_event {
            ctx.recorder.emit("scope.extended", event)?;
        }
        ctx.messages.push(ChatMessage::tool(
            tool_call.id.clone(),
            outcome.tool_result.to_string(),
        ));
        return Ok(CallFlow::Next);
    }

    // Other boundary changes require a decision.
    let kind = match args.kind {
        BoundaryKind::Scope => crate::goal::ChangeKind::Scope,
        BoundaryKind::Objective => crate::goal::ChangeKind::Objective,
        BoundaryKind::Constraint => crate::goal::ChangeKind::Constraint,
    };
    let summary = args.detail.lines().next().unwrap_or("").to_string();
    let proposal_id = format!("proposal_{}", tool_call.id);
    let detail = crate::goal::ChangeDetail {
        text: args.detail.clone(),
        summary: summary.clone(),
    };

    if ctx.guardrails.decision_channel_available(&*ctx.control) {
        // Record the pending proposal and stop when a decision channel is available.
        ctx.goal.propose_change(crate::goal::ChangeProposal {
            proposal_id: proposal_id.clone(),
            kind,
            detail: detail.clone(),
        })?;
        ctx.recorder.emit(
            "goal.change.proposed",
            json!({
                "proposal_id": proposal_id,
                "kind": kind,
                "summary": summary,
                "authored_by": "agent",
                "detail": detail,
            }),
        )?;
        ctx.recorder.emit(
            "run.needs_decision",
            json!({
                "reason": "scope_change",
                "changes": &ctx.goal.pending_changes,
            }),
        )?;
        ctx.messages.push(ChatMessage::tool(
            tool_call.id.clone(),
            json!({
                "status": "needs_decision",
                "kind": "scope",
                "proposal_id": proposal_id,
                "summary": summary,
                "change_kind": kind,
            })
            .to_string(),
        ));
        append_unpaired_tool_results(
            ctx.messages,
            &tool_calls[(tool_index + 1)..],
            &json!({
                "status": "skipped",
                "reason": "superseded by needs_decision",
            })
            .to_string(),
        );
        snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
        let mut ledger_dirty = ts.ledger_dirty;
        save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ledger_dirty)?;
        return Ok(CallFlow::Return(RunOutcome::NeedsDecision));
    }

    // Deny and continue when the decision channel is unavailable.
    // Keep pending changes empty and emit transient proposal and rejection events.
    ctx.recorder.emit(
        "goal.change.proposed",
        json!({
            "proposal_id": proposal_id,
            "kind": kind,
            "summary": summary,
            "authored_by": "agent",
            "detail": detail,
            "transient": true,
        }),
    )?;
    ctx.recorder.emit(
        "goal.change.rejected",
        json!({
            "proposal_id": proposal_id,
            "kind": kind,
            "reason": "approval_unavailable",
        }),
    )?;
    debug_assert!(
        ctx.goal.pending_changes.is_empty(),
        "denying a proposal must not leave pending changes"
    );
    ctx.messages.push(ChatMessage::tool(
        tool_call.id.clone(),
        GOVERNANCE_DENY_CONTINUE_GUIDANCE,
    ));
    Ok(CallFlow::Next)
}

#[derive(serde::Deserialize)]
struct CritArgs {
    claim: String,
    check_cmd: String,
    #[serde(default)]
    success: Option<serde_json::Value>,
    #[serde(default)]
    timeout_s: Option<u64>,
}

pub(super) async fn handle_propose_criterion(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &TurnState,
    tool_calls: &[crate::provider::ToolCall],
    tool_index: usize,
) -> Result<CallFlow> {
    let tool_call = &tool_calls[tool_index];
    let a: CritArgs = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(a) => a,
        Err(e) => {
            ctx.messages.push(ChatMessage::tool(
                tool_call.id.clone(),
                format!(
                    "propose_criterion: malformed arguments; {e}. \
                     Provide valid JSON with `claim`, `check_cmd`, and optionally `success`."
                ),
            ));
            return Ok(CallFlow::Next);
        }
    };
    let success = match crate::goal::success_rule_from_json(a.success.as_ref()) {
        Some(rule) => rule,
        None => {
            ctx.messages.push(ChatMessage::tool(
                tool_call.id.clone(),
                "propose_criterion: unsupported `success`; use \"exit_zero\" or {\"contains\":\"<text>\"}",
            ));
            return Ok(CallFlow::Next);
        }
    };
    let proposal_id = format!("proposal_{}", tool_call.id);
    let crit_id = format!("c{}", ctx.goal.contract.criteria.len() + 1);
    let criterion = crate::goal::Criterion {
        id: crit_id.clone(),
        claim: a.claim.clone(),
        scope: None,
        authored_by: crate::goal::AuthoredBy::Agent,
        approval: crate::goal::Approval::Pending,
        verifier: crate::goal::Verifier::Verifiable {
            check_cmd: a.check_cmd.clone(),
            success,
            timeout_s: a.timeout_s.unwrap_or(120),
            network: None,
        },
        status: crate::goal::CriterionStatus::Pending,
        evidence_ref: None,
    };
    ctx.recorder.emit(
        "goal.change.proposed",
        json!({
            "proposal_id": proposal_id,
            "kind": "criterion",
            "summary": a.claim,
            "authored_by": "agent",
            "draft": criterion,
        }),
    )?;
    let approval_id = format!("approval_{}", proposal_id);
    let req = GuardrailRequest {
        tool: "propose_criterion",
        summary: a.claim.clone(),
        cwd: &ctx.options.workspace,
        write_paths: &[],
        trusted: false,
    };
    match ctx.guardrails.gate_contract(
        ctx.recorder,
        ctx.control,
        ctx.options.contract_policy,
        &approval_id,
        &proposal_id,
        &req,
    )? {
        crate::guardrails::GateDecision::Approved => {
            ctx.goal.add_agent_criterion(criterion);
            ctx.goal.approve_criterion(&crit_id);
            ctx.recorder.emit(
                "goal.change.approved",
                json!({
                    "proposal_id": proposal_id,
                    "kind": "criterion",
                    "criterion_id": crit_id,
                    "applied": true
                }),
            )?;
            ctx.recorder.emit(
                "goal.updated",
                json!({
                    "proposal_id": proposal_id,
                    "criteria": ctx.goal.contract.criteria
                }),
            )?;
            let _ = crate::journal::save_contract(&ctx.paths.contract_path, &ctx.goal.contract);
            ctx.messages.push(ChatMessage::tool(
                tool_call.id.clone(),
                "criterion approved and will be enforced",
            ));
        }
        crate::guardrails::GateDecision::Rejected { reason } => {
            let reason_str = match reason {
                crate::guardrails::RejectReason::ApprovalUnavailable => "approval_unavailable",
                crate::guardrails::RejectReason::UserRejected => "user_rejected",
            };
            ctx.recorder.emit(
                "goal.change.rejected",
                json!({
                    "proposal_id": proposal_id,
                    "kind": "criterion",
                    "reason": reason_str,
                }),
            )?;
            // Give continuation guidance only when approval is unavailable; preserve user rejection feedback.
            let feedback = match reason {
                crate::guardrails::RejectReason::ApprovalUnavailable => {
                    GOVERNANCE_DENY_CONTINUE_GUIDANCE
                }
                crate::guardrails::RejectReason::UserRejected => "criterion rejected",
            };
            ctx.messages
                .push(ChatMessage::tool(tool_call.id.clone(), feedback));
        }
        crate::guardrails::GateDecision::Interrupted => {
            let run_id = ctx.run_id;
            ctx.recorder.emit(
                "run.interrupted",
                json!({
                    "step_id": "contract.gate",
                    "resume_command": format!("myagent resume {run_id}")
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
            return Ok(CallFlow::Return(RunOutcome::Interrupted));
        }
    }
    Ok(CallFlow::Next)
}
