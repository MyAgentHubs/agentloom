use super::governance_calls::{
    handle_block_with_questions, handle_propose_criterion, handle_propose_scope_change,
    handle_register_issue_probe, handle_update_working_state,
};
use super::tool_exec::{execute_and_record, gate_mutating_call, EditPrep, Gated};
use super::*;

pub(super) async fn dispatch_tool_call(
    ctx: &mut LoopCtx<'_>,
    state: &mut LoopState,
    ts: &mut TurnState,
    sig: &mut ToolTurnSignals,
    tool_calls: &[crate::provider::ToolCall],
    tool_index: usize,
    effective_disallowed: &std::collections::BTreeSet<String>,
) -> Result<CallFlow> {
    let tool_call = &tool_calls[tool_index];
    if handle_control(ctx.control, ctx.recorder, ctx.run_id, "tool.execution")? {
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
    let name = tool_call.function.name.as_str();
    if tool_disallowed(name, effective_disallowed) {
        push_tool_rejection(
            ctx.recorder,
            ctx.messages,
            name,
            &tool_call.id,
            disallowed_tool_rejection(name),
            None,
        )?;
        return Ok(CallFlow::Next);
    }
    if name == "update_working_state" {
        return handle_update_working_state(ctx, state, ts, tool_call).await;
    }
    if name == "register_issue_probe" {
        return handle_register_issue_probe(ctx, state, ts, tool_call).await;
    }
    if name == "block_with_questions" {
        return handle_block_with_questions(ctx, state, ts, tool_calls, tool_index).await;
    }
    if name == "propose_scope_change" {
        return handle_propose_scope_change(ctx, state, ts, tool_calls, tool_index).await;
    }
    if name == "propose_criterion" {
        return handle_propose_criterion(ctx, state, ts, tool_calls, tool_index).await;
    }
    let tool = match ctx.registry.get(name) {
        Some(tool) => tool,
        None => {
            push_tool_rejection(
                ctx.recorder,
                ctx.messages,
                name,
                &tool_call.id,
                json!({"error": format!("unsupported tool: {name}")}).to_string(),
                None,
            )?;
            return Ok(CallFlow::Next);
        }
    };

    match network_tool_gate(
        tool.requires_network(),
        ctx.options.network,
        sig.net_tool_calls_this_turn,
        MAX_NETWORK_TOOL_CALLS_PER_TURN,
    ) {
        NetworkGate::Execute => {
            if tool.requires_network() {
                sig.net_tool_calls_this_turn += 1;
            }
        }
        NetworkGate::RefuseNetworkOff => {
            let msg = "network off: this tool requires network and is disabled";
            push_tool_rejection(
                ctx.recorder,
                ctx.messages,
                name,
                &tool_call.id,
                json!({ "error": msg }).to_string(),
                None,
            )?;
            return Ok(CallFlow::Next);
        }
        NetworkGate::RefuseCap => {
            let msg = "per-turn search limit reached";
            push_tool_rejection(
                ctx.recorder,
                ctx.messages,
                name,
                &tool_call.id,
                json!({ "error": msg }).to_string(),
                None,
            )?;
            return Ok(CallFlow::Next);
        }
    }

    let edit_prep = if tool.mutates() {
        match gate_mutating_call(ctx, state, ts, tool, tool_call, tool_calls, tool_index).await? {
            Gated::Proceed(prep) => prep,
            Gated::Flow(flow) => return Ok(flow),
        }
    } else {
        EditPrep {
            write_paths_for_progress: Vec::new(),
            pre_edit_hashes: Vec::new(),
            scope_advisory: Vec::new(),
        }
    };
    execute_and_record(ctx, state, ts, sig, tool, tool_call, edit_prep).await
}
