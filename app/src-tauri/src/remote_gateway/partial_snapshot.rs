use super::*;

/// Maintains each session's current reduced run state for atomic `control.snapshot` reads.
///
/// Each batch rebuilds the entry when its raw `run_id` changes, feeds every sequenced event,
/// and records the last live sequence number as the snapshot watermark. A terminal event removes
/// the entry after the full batch is processed, allowing snapshots to fall back to the idle state.
///
/// Team sessions can interleave member lanes whose sequence numbers are not comparable. A batch
/// from a different raw run may replace accumulated state unless it is terminal or resolves to the
/// current occupant's logical run. Those protected batches leave the occupant untouched, while an
/// unrelated nonterminal run may claim the slot. An empty slot can be claimed by any run.
///
/// This reducer and milestone extraction run before the upstream gate so disconnected periods
/// still update local snapshot and aggregation state. The gate controls only actual upstream
/// queue admission.
pub(super) fn maintain_partial_snapshots(
    state: &GatewayInnerState,
    payload: &crate::event_transport::BatchPayload,
) {
    use crate::agent_event::AgentEvent;

    if payload.batches.is_empty() {
        return;
    }
    let mut snapshots = lock(&state.partial_snapshots);
    for batch in &payload.batches {
        if batch.events.is_empty() {
            continue;
        }
        let batch_is_terminal = batch.events.iter().any(|sequenced| {
            matches!(
                sequenced.event,
                AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. }
            )
        });
        let occupant_run_id = snapshots.get(&batch.session_id).map(|e| e.run_id.clone());
        if let Some(occupant) = &occupant_run_id {
            if occupant != &batch.run_id {
                // Preserve an existing occupant when a different raw run is terminal or belongs
                // to the same logical run. This prevents a member lane from replacing the lead
                // slot with a streaming batch and then clearing it with its terminal batch.
                // Unrelated nonterminal batches continue through the normal rebuild path so the
                // snapshot owner and activity aggregation use the same logical-run definition.
                if batch_is_terminal || activity_summary_logical_run_id(batch) == *occupant {
                    continue;
                }
            }
        }
        let needs_rebuild = occupant_run_id
            .as_deref()
            .map(|occupant| occupant != batch.run_id)
            .unwrap_or(true);
        if needs_rebuild {
            if !snapshots.contains_key(&batch.session_id)
                && snapshots.len() >= PARTIAL_SNAPSHOT_CAPACITY
            {
                // Capacity rejection degrades this session to the idle snapshot without corrupting
                // existing entries. Unconditional maintenance before the gate prevents disconnected
                // entries from lingering beyond their liveness window.
                state
                    .partial_snapshot_capacity_dropped
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            snapshots.insert(
                batch.session_id.clone(),
                PartialSnapshotState {
                    run_id: batch.run_id.clone(),
                    last_seq: 0,
                    reducer: crate::display_reduce::DisplayReducer::new(&batch.run_id),
                },
            );
        }
        let Some(entry) = snapshots.get_mut(&batch.session_id) else {
            continue;
        };
        for sequenced in &batch.events {
            entry.reducer.feed(&sequenced.event);
            entry.last_seq = sequenced.seq;
        }
        if batch_is_terminal {
            snapshots.remove(&batch.session_id);
        }
    }
}

pub(super) fn enqueue_batch_payload_for_upstream(
    state: &GatewayInnerState,
    upstream_tx: &SyncSender<(u64, LiveQueueItem)>,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    payload: crate::event_transport::BatchPayload,
) {
    // Snapshot reduction must run unconditionally before the gate; the gate only controls
    // admission to the upstream queue.
    maintain_partial_snapshots(state, &payload);

    // Milestone extraction also runs before the outer gate. Its upstream frames perform their own
    // gate check, while activity-summary deltas use a separate database channel and must continue
    // to be produced when no remote client is connected.
    extract_tool_milestones(state, milestone_tx, &payload);

    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return;
    }
    let generation = snapshot >> 1;

    match upstream_tx.try_send((generation, LiveQueueItem::Batch(payload))) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Queues a prebuilt `control.history` live item through the same bounded FIFO, generation tag,
/// and drain ownership gate as deltas. Classification is skipped because the history contract has
/// already shaped the payload.
///
/// Returns `true` only when `try_send` queues the item. This is best-effort admission rather than
/// a delivery guarantee; invalid IDs, a closed gate, or a full or disconnected queue return `false`.
pub(super) fn enqueue_prebuilt_live_for_upstream(
    state: &GatewayInnerState,
    upstream_tx: &SyncSender<(u64, LiveQueueItem)>,
    item: MilestoneItem,
) -> bool {
    if !is_valid_client_msg_id(&item.client_msg_id) {
        state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return false;
    }
    let generation = snapshot >> 1;
    match upstream_tx.try_send((generation, LiveQueueItem::Prebuilt(item))) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

/// Performs final milestone admission: validates the client message ID, tries the bounded send,
/// and counts full or disconnected drops. Callers are responsible for their own gate semantics.
pub(super) fn enqueue_milestone_item(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    generation: u64,
    mut item: MilestoneItem,
) {
    if !is_valid_client_msg_id(&item.client_msg_id) {
        state.milestone_dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if item.t == "msg.completed" {
        if let Some(blocks) = item.payload.get_mut("blocks") {
            *blocks = truncate_history_tool_outputs(blocks.clone());
        }
        // `build_msg_completed_payload` carries a precomputed content reference under a private
        // key. Remove it before measuring or serializing so it affects neither frame size nor the
        // wire shape of messages that do not need a preview.
        let content_ref_source = item
            .payload
            .as_object_mut()
            .and_then(|obj| obj.remove(MSG_COMPLETED_REF_SOURCE_KEY));
        if milestone_frame_bytes(&item.t, &item.payload) > SNAPSHOT_SEND_BUDGET_BYTES {
            match content_ref_source {
                Some(content_ref) => {
                    // Oversized messages degrade to a block-level preview with a content reference
                    // instead of disappearing. The counter therefore measures preview degradation.
                    item.payload =
                        downgrade_to_preview_payload(&item.payload, content_ref, &item.t);
                    state
                        .replay_oversized_dropped
                        .fetch_add(1, Ordering::Relaxed);
                    // Continue through the normal queueing path with the degraded payload.
                }
                None => {
                    // Defensive fallback: production callers normally provide the reference source.
                    // Without it, a compliant revision and digest cannot be reconstructed, so retain
                    // the existing drop behavior rather than fabricating metadata.
                    state
                        .replay_oversized_dropped
                        .fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
    match milestone_tx.try_send((generation, item)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.milestone_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Queues an item with the generation captured by the caller instead of rereading the current
/// generation. This closes the preemption window for callers that perform potentially slow work
/// between capturing connection ownership and queueing. Downstream generation filters discard
/// stale items. The current gate decides whether to send now, while the supplied generation
/// identifies which connection owns the item.
pub(super) fn enqueue_milestone_with_generation(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    generation: u64,
    item: MilestoneItem,
) {
    if !state.upstream_enabled_snapshot() {
        return;
    }
    enqueue_milestone_item(state, milestone_tx, generation, item);
}

/// Queues an item using one packed upstream-state load so the gate bit and generation come from
/// the same atomic snapshot. A closed gate is a no-op, and a generation from one connection cannot
/// be paired with an enabled bit from another.
pub(super) fn enqueue_milestone_for_upstream(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    item: MilestoneItem,
) {
    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return;
    }
    let generation = snapshot >> 1;
    enqueue_milestone_item(state, milestone_tx, generation, item);
}

pub(super) fn is_valid_client_msg_id(client_msg_id: &str) -> bool {
    !client_msg_id.is_empty() && client_msg_id.len() <= 64 && !client_msg_id.contains('|')
}

/// Publishes a durable remote milestone without blocking.
///
/// This function is safe to call while a database transaction lock is held: before gateway
/// setup or while upstream is disabled it is a no-op; otherwise it performs one atomic snapshot
/// and one bounded-channel `try_send`. A full/disconnected channel or invalid client message ID
/// is counted and dropped, never retried, blocked, or panicked here.
pub(crate) fn publish_milestone(
    session: Option<&str>,
    t: &str,
    payload: Value,
    client_msg_id: String,
) {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    enqueue_milestone_for_upstream(
        &inner.state,
        &inner.milestone_tx,
        MilestoneItem {
            session: session.map(str::to_owned),
            t: t.to_owned(),
            payload,
            client_msg_id,
        },
    );
}

#[cfg(test)]
thread_local! {
    static TEST_PUBLISH_LOG: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
    pub(super) static TEST_RUN_STATUS_PAYLOAD_LOG: std::cell::RefCell<Vec<Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
    pub(super) static TEST_SESSION_INDEX_ARCHIVED_PAYLOAD_LOG: std::cell::RefCell<Vec<Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
    pub(super) static TEST_SESSION_INDEX_CREATED_PAYLOAD_LOG: std::cell::RefCell<Vec<Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(super) fn record_test_publish(t: &'static str) {
    TEST_PUBLISH_LOG.with(|log| log.borrow_mut().push(t));
}

#[cfg(not(test))]
pub(super) fn record_test_publish(_t: &'static str) {}

#[cfg(test)]
pub(crate) fn test_take_publish_log() -> Vec<&'static str> {
    TEST_PUBLISH_LOG.with(|log| log.borrow_mut().drain(..).collect())
}

#[cfg(test)]
pub(crate) fn test_take_run_status_payload_log() -> Vec<Value> {
    TEST_RUN_STATUS_PAYLOAD_LOG.with(|log| log.borrow_mut().drain(..).collect())
}

#[cfg(test)]
pub(crate) fn test_take_session_index_archived_payload_log() -> Vec<Value> {
    TEST_SESSION_INDEX_ARCHIVED_PAYLOAD_LOG.with(|log| log.borrow_mut().drain(..).collect())
}

#[cfg(test)]
pub(crate) fn test_take_session_index_created_payload_log() -> Vec<Value> {
    TEST_SESSION_INDEX_CREATED_PAYLOAD_LOG.with(|log| log.borrow_mut().drain(..).collect())
}
