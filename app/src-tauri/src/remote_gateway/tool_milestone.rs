use super::*;

pub(super) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn build_envelope_json(
    meta: &EnvelopeMeta,
    ct: &str,
    n: &str,
    ts_ms: u64,
    client_msg_id: Option<&str>,
) -> serde_json::Value {
    // Echo `meta.command_id` exactly so the envelope matches the authenticated metadata; hardcoding a null value would break replies.
    // The only caller, `send_upstream_value`, serves event/live traffic, whose
    // `EnvelopeMeta.command_id` is always `None`. `reply` is the first outbound kind that
    // actually needs a non-null `command_id`. Its value must match byte-for-byte the
    // `meta.command_id` used by `crate::remote_crypto::seal` to calculate AAD because the AAD
    // includes `command_id`; reading it once avoids separate JSON and AAD sources drifting apart.
    let command_id = match meta.command_id.as_deref() {
        Some(command_id) => Value::String(command_id.to_owned()),
        None => Value::Null,
    };
    let mut envelope = serde_json::json!({
        "v": meta.v,
        "room": meta.room,
        "epoch": meta.epoch,
        "kind": meta.kind,
        "session": meta.session,
        "command_id": command_id,
        "seq": serde_json::Value::Null,
        "ct": ct,
        "n": n,
        "ts": ts_ms,
    });
    if let Some(client_msg_id) = client_msg_id {
        envelope
            .as_object_mut()
            .expect("envelope is constructed as a JSON object")
            .insert(
                "client_msg_id".to_owned(),
                Value::String(client_msg_id.to_owned()),
            );
    }
    envelope
}

pub(super) fn classify(
    event: &crate::agent_event::AgentEvent,
    seq: u64,
) -> Option<(&'static str, serde_json::Value)> {
    use crate::agent_event::AgentEvent;

    match event {
        AgentEvent::TextDelta { text } => Some((
            "live",
            serde_json::json!({
                "t": "text_delta",
                "seq": seq,
                "text": truncate_utf8(text, OUTPUT_TRUNCATE_BYTES),
            }),
        )),
        AgentEvent::ThinkingDelta { text } => Some((
            "live",
            serde_json::json!({
                "t": "thinking_delta",
                "seq": seq,
                "text": truncate_utf8(text, OUTPUT_TRUNCATE_BYTES),
            }),
        )),
        AgentEvent::ToolOutputDelta { id, text } => Some((
            "live",
            serde_json::json!({
                "t": "tool_output_delta",
                "seq": seq,
                "id": id,
                "text": truncate_utf8(text, OUTPUT_TRUNCATE_BYTES),
            }),
        )),
        AgentEvent::UsageDelta {
            input_tokens,
            output_tokens,
        } => Some((
            "live",
            serde_json::json!({
                "t": "usage_delta",
                "seq": seq,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }),
        )),
        // ToolStarted/ToolCompleted are handled by extract_tool_milestones on the sink enqueue
        // path. Returning None here prevents duplicates when the original batch passes through
        // the live queue. msg.completed / card.created / card.resolved / run.status /
        // session.index must each be wired at their real choke point, so they also return None.
        //
        // The remaining variants (SessionStarted / Completed / RunCloseout / GoalDeclared /
        // CriteriaUpdated / GoalUpdated / NeedsDecision / ApprovalRequested / ApprovalResolved /
        // Error / Blocked) are in neither the milestone nor live catalog. Internal diagnostics
        // and the protocol do not cover them yet, so they are not sent upstream.
        _ => None,
    }
}

fn tool_status_wire_str(status: &crate::agent_event::ToolStatus) -> &'static str {
    match status {
        crate::agent_event::ToolStatus::Ok => "ok",
        crate::agent_event::ToolStatus::Failed => "failed",
    }
}

pub(super) fn remember_tool_name(
    state: &GatewayInnerState,
    run_id: &str,
    tool_id: &str,
    tool: &str,
) {
    let key = (run_id.to_owned(), tool_id.to_owned());
    let mut correlation = lock(&state.tool_correlation);
    if correlation.names.contains_key(&key) {
        correlation.names.insert(key, tool.to_owned());
        return;
    }
    if correlation.names.len() >= TOOL_CORRELATION_CAPACITY {
        // MEDIUM#6: evict the oldest orphan to admit the new key instead of permanently rejecting
        // all new correlations once the table fills.
        if let Some(oldest) = correlation.order.pop_front() {
            correlation.names.remove(&oldest);
            state
                .tool_correlation_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    correlation.order.push_back(key.clone());
    correlation.names.insert(key, tool.to_owned());
}

pub(super) fn take_tool_name(state: &GatewayInnerState, run_id: &str, tool_id: &str) -> String {
    let key = (run_id.to_owned(), tool_id.to_owned());
    let mut correlation = lock(&state.tool_correlation);
    let name = correlation.names.remove(&key);
    if name.is_some() {
        if let Some(pos) = correlation.order.iter().position(|entry| entry == &key) {
            correlation.order.remove(pos);
        }
    }
    name.unwrap_or_default()
}

/// Run-terminal cleanup (MEDIUM#6): remove every orphaned correlation for this run when its
/// Completed or RunCloseout event arrives, even if no matching ToolCompleted was emitted.
fn purge_tool_correlation_for_run(state: &GatewayInnerState, run_id: &str) {
    let mut correlation = lock(&state.tool_correlation);
    correlation
        .names
        .retain(|(entry_run_id, _), _| entry_run_id != run_id);
    correlation
        .order
        .retain(|(entry_run_id, _)| entry_run_id != run_id);
}

pub(super) fn derive_client_msg_id(name: &str) -> String {
    uuid::Uuid::new_v5(&CLIENT_MSG_ID_NAMESPACE, name.as_bytes()).to_string()
}

#[cfg(test)]
thread_local! {
    static FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Test-only guard that forces `try_random_client_msg_id` to return `None` on this thread,
/// simulating an OS CSPRNG (`getrandom`) failure. The RAII guard resets on drop and is not
/// reentrant, following the style of `CliPathOverrideTestGuard` in `detect.rs`.
#[cfg(test)]
pub(super) struct ForceClientMsgIdEntropyFailureGuard;

#[cfg(test)]
impl ForceClientMsgIdEntropyFailureGuard {
    pub(super) fn new() -> Self {
        FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE.with(|flag| {
            assert!(
                !flag.replace(true),
                "entropy failure guard is not reentrant"
            );
        });
        Self
    }
}

#[cfg(test)]
impl Drop for ForceClientMsgIdEntropyFailureGuard {
    fn drop(&mut self) {
        FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE.with(|flag| flag.set(false));
    }
}

/// Creates a random client_msg_id without panicking. `uuid::Uuid::new_v4()` panics internally
/// when the OS CSPRNG (`getrandom`) fails. Callers here may hold the DB mutex immediately after
/// writing a row and before publishing its milestone, so unwinding would poison that mutex for
/// every later holder. Instead, obtain 16 fallible random bytes and construct the UUID with
/// `Builder::from_random_bytes`, which only sets version and variant bits and needs no entropy.
/// On failure, return `None` so the caller falls back to an empty string. The existing
/// `is_valid_client_msg_id` check in `enqueue_milestone_for_upstream` treats it as invalid,
/// drops it, and increments `milestone_dropped`, using the same nonblocking, nonretrying,
/// nonpanicking path as a full channel or caller-supplied invalid id. No new counter is needed.
pub(super) fn try_random_client_msg_id() -> Option<String> {
    #[cfg(test)]
    if FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE.with(std::cell::Cell::get) {
        return None;
    }
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(
        uuid::Builder::from_random_bytes(bytes)
            .into_uuid()
            .to_string(),
    )
}

pub(super) fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// When tool output exceeds `OUTPUT_TRUNCATE_BYTES`, mark truncation visibly so readers do not mistake incomplete output for the complete result.
/// Simply removing trailing bytes creates invisible information loss: remote readers may assume
/// the content naturally ended there and make decisions from incomplete tool output. Append the
/// fixed `TOOL_OUTPUT_TRUNCATION_MARKER` only when truncation actually occurs.
/// Full-content recovery through references is not implemented here; this helper only makes truncation visible.
///
/// The marker must count toward the `max_bytes` budget. Appending it after truncating the body to
/// `max_bytes` would exceed the limit and fail the caller's subsequent
/// `milestone_frame_bytes(...) > SNAPSHOT_SEND_BUDGET_BYTES` check in
/// `enqueue_milestone_item`. Reserve `max_bytes - marker.len()` bytes for the body, then append
/// the marker so the total always remains at most `max_bytes`.
pub(super) const TOOL_OUTPUT_TRUNCATION_MARKER: &str = "…[输出已截断]";

pub(super) fn truncate_utf8_with_marker(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let body_budget = max_bytes.saturating_sub(TOOL_OUTPUT_TRUNCATION_MARKER.len());
    let mut truncated = truncate_utf8(text, body_budget);
    truncated.push_str(TOOL_OUTPUT_TRUNCATION_MARKER);
    truncated
}

/// On the sink enqueue path, synchronously called by the emitter thread, identifies
/// ToolStarted/ToolCompleted events in a batch, maintains `(run_id, tool_id)` name correlation,
/// converts ToolCompleted into milestone-priority MilestoneItems, and uses try_send to enqueue
/// them on milestone_tx. It does not wait for the original batch to drain through the
/// lower-priority live channel. This structurally avoids starvation from an exhausted milestone
/// budget, whole-batch drops when the live queue is full, and permanent loss of ToolCompleted
/// events late in a batch when the drain budget runs out (HIGH#2). Completed/RunCloseout also
/// clears residual correlations for that run (MEDIUM#6).
///
/// Each lane accumulates at most `event_transport::LANE_CAPACITY` (512) events per tick, and the
/// total batch size is also bounded by the active lane count, approximately the number of
/// concurrent runs and small in practice. This function performs one bounded O(event count)
/// pass. Each ToolCompleted triggers one milestone enqueue backed by try_send. Apart from the
/// short private mutex already justified on the field, it neither blocks nor performs I/O and
/// satisfies the substantive add_sink contract.
///
/// The original batch, including both event types, is not filtered and is still passed unchanged
/// to upstream_tx with try_send. `classify` handles the live drain and returns None for
/// ToolStarted/ToolCompleted, so no duplicate is sent upstream.
///
/// This function is also the delta extraction point for the L1 activity-summary aggregator.
/// Combining the work is intentional rather than rescanning the batch in another function: the
/// ToolCompleted branch already calls the consuming lookup `take_tool_name`, while activity
/// summaries need the same tool name to test the `mcp__` prefix. If two functions traversed the
/// batch independently, the first call would remove the correlation and the other could no
/// longer obtain the name. Both concerns therefore share the same `take_tool_name` result here.
/// `extract_activity_summary_delta` actually uses try_send only after `state.activity_summary_tx`
/// has been configured by `configure_activity_summary_writer`; otherwise the feature is a no-op
/// and records no drops because disabled is distinct from enabled but dropped.
pub(super) fn extract_tool_milestones(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    payload: &crate::event_transport::BatchPayload,
) {
    use crate::agent_event::AgentEvent;

    let Some(activity_summary_tx) = state.activity_summary_tx.get() else {
        // The aggregator is not configured; use the original tool.completed-only extraction path
        // with no additional overhead.
        for batch in &payload.batches {
            for sequenced in &batch.events {
                match &sequenced.event {
                    AgentEvent::ToolStarted { id, tool, .. } => {
                        remember_tool_name(state, &batch.run_id, id, tool);
                    }
                    AgentEvent::ToolCompleted {
                        id,
                        status,
                        exit_code,
                        output,
                    } => {
                        let tool = take_tool_name(state, &batch.run_id, id);
                        publish_tool_completed_milestone(
                            state,
                            milestone_tx,
                            batch,
                            id,
                            &tool,
                            status,
                            *exit_code,
                            output.as_deref(),
                        );
                    }
                    AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. } => {
                        purge_tool_correlation_for_run(state, &batch.run_id);
                    }
                    _ => {}
                }
            }
        }
        return;
    };

    for batch in &payload.batches {
        let logical_run_id = activity_summary_logical_run_id(batch);
        for sequenced in &batch.events {
            match &sequenced.event {
                AgentEvent::ToolStarted { id, tool, .. } => {
                    remember_tool_name(state, &batch.run_id, id, tool);
                }
                AgentEvent::ToolCompleted {
                    id,
                    status,
                    exit_code,
                    output,
                } => {
                    let tool = take_tool_name(state, &batch.run_id, id);
                    publish_tool_completed_milestone(
                        state,
                        milestone_tx,
                        batch,
                        id,
                        &tool,
                        status,
                        *exit_code,
                        output.as_deref(),
                    );
                    send_activity_summary_delta(
                        state,
                        activity_summary_tx,
                        batch.session_id.clone(),
                        logical_run_id.clone(),
                        ActivitySummaryDeltaKind::ToolCompleted {
                            mcp: tool.starts_with("mcp__"),
                            failed: matches!(status, crate::agent_event::ToolStatus::Failed),
                        },
                    );
                }
                AgentEvent::ApprovalRequested { .. } => {
                    // Runtime routing decides whether approval content may join L1 aggregation
                    // through `event_joins_l1_aggregation`, including in release builds rather
                    // than only under debug assertions. Approval is actionable, so the function
                    // returns false and only the restricted path runs: it emits a fieldless
                    // `PermissionPrompt` count delta. Original card content such as approval_id,
                    // command, summary, and cwd can never enter activity_summary because the
                    // variant has no fields capable of carrying it.
                    if !event_joins_l1_aggregation("approval") {
                        send_activity_summary_delta(
                            state,
                            activity_summary_tx,
                            batch.session_id.clone(),
                            logical_run_id.clone(),
                            ActivitySummaryDeltaKind::PermissionPrompt,
                        );
                    } else {
                        // Unreachable in principle: the current allowlist always classifies
                        // approval as actionable. If the allowlist drifts, debug builds fail here
                        // instead of letting release routing change silently.
                        debug_assert!(
                            false,
                            "白名单漂移：approval 不再被判定为 actionable，L1 路由需要重新设计（本刀未实现）"
                        );
                    }
                }
                AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. } => {
                    purge_tool_correlation_for_run(state, &batch.run_id);
                    // A member lane's terminal event means only that the member lane ended, not
                    // that the entire logical lead run ended. Only a lead/solo batch without
                    // dispatch may seal the parent run. Nonterminal member-lane deltas from the
                    // ToolCompleted/PermissionPrompt branches still join the parent counters;
                    // otherwise one member finishing early would seal the lead run and discard
                    // later counters and terminal state from other members or the lead.
                    if batch.dispatch.is_none() {
                        send_activity_summary_delta(
                            state,
                            activity_summary_tx,
                            batch.session_id.clone(),
                            logical_run_id.clone(),
                            ActivitySummaryDeltaKind::Terminal { failed: false },
                        );
                    }
                }
                AgentEvent::Error { .. } => {
                    // Likewise, a member lane's error terminal must not seal the parent run.
                    if batch.dispatch.is_none() {
                        send_activity_summary_delta(
                            state,
                            activity_summary_tx,
                            batch.session_id.clone(),
                            logical_run_id.clone(),
                            ActivitySummaryDeltaKind::Terminal { failed: true },
                        );
                    }
                }
                AgentEvent::NeedsDecision { .. } => {
                    // Use explicit runtime routing for scope changes so aggregation eligibility and delta emission remain separate, observable decisions.
                    // `event_joins_l1_aggregation` decides at runtime whether scope_change may
                    // join L1. As in ApprovalRequested, the if/else separates the routing
                    // decision from delta emission into explicit paths instead of relying on a
                    // silent match-arm omission. Although scope_change currently has no dedicated
                    // delta kind and the actionable branch explicitly emits nothing, the choice
                    // remains visible and testable. It uses the separate
                    // `db::Block::ScopeChange` card path constructed by lead orchestration in
                    // lib.rs, outside this file.
                    if !event_joins_l1_aggregation("scope_change") {
                        // Correct routing result: scope_change is actionable, so it emits no
                        // delta. There is no dedicated delta kind, and `PermissionPrompt` is
                        // semantically specific to approval and cannot be reused.
                    } else {
                        // Unreachable in principle: the current allowlist always classifies
                        // scope_change as actionable. If the allowlist drifts, debug builds fail
                        // immediately, as described for ApprovalRequested.
                        debug_assert!(
                            false,
                            "白名单漂移：scope_change 不再被判定为 actionable，L1 路由需要重新设计（本刀未实现）"
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

/// Shared construction tail for the `ToolCompleted` to `tool.completed` milestone conversion.
/// Both configured and unconfigured aggregator paths in `extract_tool_milestones` use it, which
/// avoids duplicated `enqueue_milestone_for_upstream` calls whose field lists could drift.
#[allow(clippy::too_many_arguments)]
fn publish_tool_completed_milestone(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    batch: &crate::event_transport::RunBatch,
    id: &str,
    tool: &str,
    status: &crate::agent_event::ToolStatus,
    exit_code: Option<i64>,
    output: Option<&str>,
) {
    let client_msg_id = derive_client_msg_id(&format!("tool.completed|{}|{}", batch.run_id, id));
    enqueue_milestone_for_upstream(
        state,
        milestone_tx,
        MilestoneItem {
            session: Some(batch.session_id.clone()),
            t: "tool.completed".to_owned(),
            payload: serde_json::json!({
                "id": id,
                "tool": tool,
                "status": tool_status_wire_str(status),
                "exit_code": exit_code,
                "output": output.map(|value| truncate_utf8(value, OUTPUT_TRUNCATE_BYTES)),
            }),
            client_msg_id,
        },
    );
}
