use super::*;

pub(super) fn handle_input_send(inner: &Inner, payload: &Value, command_id: &str) -> Option<Value> {
    let state = &inner.state;
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(command_id, AckOutcome::Failed))
    };
    let Some(session) = payload.get("session").and_then(Value::as_str) else {
        return failed();
    };
    // M2-4c: The downstream ownership gate fails closed for every session that does not belong to
    // the current active repo.
    if !command_session_allowed(inner, session) {
        return failed();
    }
    let Some(text) = payload.get("text").and_then(Value::as_str) else {
        return failed();
    };
    let Some(outcome) = (inner.input_send_handler)(InputSendFrame {
        session: session.to_owned(),
        command_id: command_id.to_owned(),
        text: text.to_owned(),
    }) else {
        // If the receipt cannot be persisted or its final state is temporarily unknown, do not
        // return an ack; let the relay retain and redeliver it. This is not a bad frame, so do not
        // increment bad_frames.
        return None;
    };
    Some(input_ack_json(command_id, outcome))
}

pub(super) fn handle_control_stop(
    inner: &Inner,
    payload: &Value,
    command_id: &str,
) -> Option<Value> {
    let state = &inner.state;
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(command_id, AckOutcome::Failed))
    };
    let Some(session) = payload.get("session").and_then(Value::as_str) else {
        return failed();
    };
    // M2-4c: Apply the same downstream ownership gate as `input.send`.
    if !command_session_allowed(inner, session) {
        return failed();
    }
    let Some(issued_at_ms) = payload
        .get("issued_at_ms")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= JSON_SAFE_INTEGER_MAX)
    else {
        return failed();
    };
    let Some(expires_at_ms) = payload
        .get("expires_at_ms")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= JSON_SAFE_INTEGER_MAX)
    else {
        return failed();
    };
    if issued_at_ms > expires_at_ms {
        return failed();
    }
    if expires_at_ms - issued_at_ms > CONTROL_STOP_MAX_LIFETIME_MS {
        return failed();
    }
    if is_control_stop_stale(issued_at_ms, expires_at_ms, now_unix_ms()) {
        return failed();
    }
    if !(inner.control_replay_handler)(session, command_id) {
        return failed();
    }
    let outcome = (inner.control_stop_handler)(ControlStopFrame {
        session: session.to_owned(),
        command_id: command_id.to_owned(),
    });
    Some(input_ack_json(command_id, outcome))
}

pub(super) fn handle_input_answer(
    inner: &Inner,
    payload: &Value,
    command_id: &str,
) -> Option<Value> {
    let state = &inner.state;
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(command_id, AckOutcome::Failed))
    };
    let Some(session) = payload.get("session").and_then(Value::as_str) else {
        return failed();
    };
    // M2-4c: Apply the same downstream ownership gate as `input.send`.
    if !command_session_allowed(inner, session) {
        return failed();
    }
    let Some(decision_id) = payload.get("decision_id").and_then(Value::as_str) else {
        return failed();
    };
    let Some(option) = payload.get("option").and_then(Value::as_str) else {
        return failed();
    };
    let Some(outcome) = (inner.input_answer_handler)(InputAnswerFrame {
        session: session.to_owned(),
        command_id: command_id.to_owned(),
        decision_id: decision_id.to_owned(),
        option: option.to_owned(),
    }) else {
        return None;
    };
    Some(input_ack_json(command_id, outcome))
}

pub(super) fn handle_control_history(
    inner: &Inner,
    payload: &Value,
    command_id: &str,
) -> Option<Value> {
    let state = &inner.state;
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(command_id, AckOutcome::Failed))
    };
    let Some(session) = payload.get("session").and_then(Value::as_str) else {
        return failed();
    };
    if session.len() > SESSION_ID_MAX_BYTES {
        return failed();
    }
    if !command_session_allowed(inner, session) {
        return failed();
    }
    let before_message_id = match payload.get("before_message_id") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let Some(cursor) = value.as_i64().filter(|cursor| *cursor >= 0) else {
                return failed();
            };
            Some(cursor)
        }
    };
    // Fetch one extra row only to determine whether earlier messages really exist; the wire page
    // still contains at most 50 rows. If every row in a window is deliberately dropped because an
    // individual row exceeds the budget, continue to the next window using the oldest scanned id.
    // Otherwise, a null cursor on the empty page would make earlier messages unreachable forever.
    // The response's before_message_id always echoes the phone's original request, not the internal
    // scan cursor.
    let mut scan_before = before_message_id;
    let mut oversized_dropped = 0_u64;
    let page = loop {
        let Ok(rows) = (inner.session_history_provider)(
            session,
            scan_before,
            (HISTORY_PAGE_MAX_ROWS + 1) as i64,
        ) else {
            return failed();
        };
        let page = build_history_page(session, before_message_id, rows);
        oversized_dropped = oversized_dropped.saturating_add(page.oversized_dropped);
        let emitted = page
            .payload
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| !messages.is_empty())
            .unwrap_or(false);
        let Some(next_scan_before) = page.next_scan_before else {
            break page;
        };
        if emitted {
            break page;
        }
        if scan_before.is_some_and(|cursor| next_scan_before >= cursor) {
            return failed();
        }
        scan_before = Some(next_scan_before);
    };
    state
        .history_oversized_dropped
        .fetch_add(oversized_dropped, Ordering::Relaxed);
    let cursor_name = before_message_id
        .map(|cursor| cursor.to_string())
        .unwrap_or_else(|| "latest".to_owned());
    let client_msg_id =
        derive_client_msg_id(&format!("history|{session}|{command_id}|{cursor_name}"));
    // Enqueue failure from a closed gate, full queue, or disconnected channel must produce a
    // failure acknowledgment so clients are not promised unsent frames. The previous unconditional
    // `Ok` made clients believe that a frame was in flight when it was never enqueued and would
    // never arrive.
    let enqueued = enqueue_prebuilt_live_for_upstream(
        state,
        &inner.upstream_tx,
        MilestoneItem {
            session: Some(session.to_owned()),
            t: "history".to_owned(),
            payload: page.payload,
            client_msg_id,
        },
    );
    Some(input_ack_json(
        command_id,
        if enqueued {
            AckOutcome::Ok
        } else {
            AckOutcome::Failed
        },
    ))
}

pub(super) fn handle_control_snapshot(
    inner: &Inner,
    payload: &Value,
    command_id: &String,
) -> Option<Value> {
    let state = &inner.state;
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(command_id, AckOutcome::Failed))
    };
    let Some(session) = payload.get("session").and_then(Value::as_str) else {
        return failed();
    };
    // P0-b micro-rework round 4: defense-in-depth for session length; see the full derivation at
    // `SESSION_ID_MAX_BYTES`. Place this before the ownership gate: a session value that already
    // violates the protocol does not warrant an ownership lookup.
    if session.len() > SESSION_ID_MAX_BYTES {
        return failed();
    }
    // P0-b: Apply the same downstream ownership gate as
    // `input.send`/`control.stop`/`input.answer`.
    if !command_session_allowed(inner, session) {
        return failed();
    }
    // P0-b: Atomically obtain this session's current reduced state in one critical section; a
    // missing entry means idle.
    let (run, blocks) = {
        let snapshots = lock(&state.partial_snapshots);
        match snapshots.get(session) {
            Some(entry) => (
                Some((entry.run_id.clone(), entry.last_seq)),
                entry.reducer.snapshot_blocks(),
            ),
            None => (None, Vec::new()),
        }
    };
    let snapshot_payload = build_snapshot_payload(
        session,
        run.as_ref()
            .map(|(run_id, through_run_seq)| (run_id.as_str(), *through_run_seq)),
        &blocks,
    );
    // P0-b rework 2(c), micro-rework round 3, rewritten send-side fallback:
    // `build_snapshot_payload` already converges the payload, so this protects only the edge case
    // that remains over budget after convergence. Never leave an oversized frame for the relay to
    // decide: code 1009 disconnects the desktop, and a client retry would create a disconnect loop.
    // `SNAPSHOT_SEND_BUDGET_BYTES` (44 KiB) is a plaintext budget adjusted for envelope expansion,
    // not a direct comparison of a bare payload against the relay's hard 64 KB gate; see the full
    // derivation at the constant's definition. Once `SNAPSHOT_PAYLOAD_BUDGET_BYTES` convergence is
    // correct, this branch is nearly unreachable on the normal path and acts purely as a fuse. It
    // protects only the snapshot path and does not alter established enqueue behavior for other
    // milestones.
    //
    // P0-b micro-rework round 4: measure with `snapshot_frame_bytes`, using the same yardstick as
    // `shrink_snapshot_blocks_to_budget` convergence; both include `t`. Do not measure the bare
    // `snapshot_payload` without merged `t` here. The old form split the accounting in two ways:
    // (1) convergence decided that the size including `t` was within budget, but (2) this fallback
    // tested the bare size against the 44 KiB threshold. A real boundary frame near the threshold
    // could pass (1), then have the smaller bare size in (2) misclassified as not over budget and
    // be sent unchanged.
    //
    // Accurately record the narrow window: when this branch is hit, the function returns
    // `AckOutcome::Ok` below even though the corresponding snapshot frame was never enqueued or
    // sent. This belongs to the same family of "ack returned but frame not delivered" windows as a
    // drop during drain filtering. It is outside this fix's scope; this documents the existing
    // tradeoff without adding new semantics.
    let snapshot_payload_bytes = snapshot_frame_bytes(&snapshot_payload);
    if snapshot_payload_bytes > SNAPSHOT_SEND_BUDGET_BYTES {
        state
            .snapshot_oversized_dropped
            .fetch_add(1, Ordering::Relaxed);
        return Some(input_ack_json(command_id, AckOutcome::Ok));
    }
    // v1.8.11: Derive client_msg_id deterministically from the request command_id, making
    // redelivery of the same request naturally idempotent and deduplicated.
    let client_msg_id = derive_client_msg_id(&format!("snapshot|{session}|{command_id}"));
    // Send the snapshot response through the established milestone-queue exit, where milestones
    // precede live items in the same drain iteration. Enqueue directly through the
    // inner.state/inner.milestone_tx already held by this call instead of looking it up again via
    // the global `publish_milestone` singleton. In production both refer to the same `Inner`, while
    // the local reference is testable: unit tests do not register the global `GATEWAY` singleton,
    // so a global lookup would silently prevent them from verifying the constructed payload.
    enqueue_milestone_for_upstream(
        state,
        &inner.milestone_tx,
        MilestoneItem {
            session: Some(session.to_owned()),
            t: "snapshot".to_owned(),
            payload: snapshot_payload,
            client_msg_id,
        },
    );
    Some(input_ack_json(command_id, AckOutcome::Ok))
}

pub(super) fn handle_msg_fetch_kind(
    inner: &Inner,
    payload: &Value,
    command_id: &str,
) -> Option<Value> {
    let state = &inner.state;
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(command_id, AckOutcome::Failed))
    };
    // Validate all four fields `{session, message_id, revision, offset}`. Missing or mistyped
    // fields are protocol violations handled by the existing `failed()`, a separate failure layer
    // from business rejections such as unauthorized access, soft deletion, excessive size, stale,
    // or busy. Those produce a `msg.fetch.error` reply instead of failing here.
    let Some(session) = payload.get("session").and_then(Value::as_str) else {
        return failed();
    };
    if session.len() > SESSION_ID_MAX_BYTES {
        return failed();
    }
    let Some(message_id) = payload.get("message_id").and_then(Value::as_i64) else {
        return failed();
    };
    let Some(requested_revision) = payload.get("revision").and_then(Value::as_i64) else {
        return failed();
    };
    let Some(offset) = payload
        .get("offset")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .and_then(|value| usize::try_from(value).ok())
    else {
        return failed();
    };
    // Unlike `input.send`/`control.stop`/`control.history`/`control.snapshot`, `msg.fetch` responds
    // through the reply queue rather than returning an `input.ack`. Per M0 §10.5, its only
    // response forms are `msg.chunk` or `msg.fetch.error` under the `reply` kind. `handle_msg_fetch`
    // sends all of them through `try_enqueue_reply` into the independent `reply_queue` specified by
    // M0 §10.9, and `drain_reply_queue` emits them asynchronously. Returning `None` here does not
    // mean processing failed; it means the response uses another channel, so this `handle_frame`
    // call has no Value to write directly to the socket.
    handle_msg_fetch(
        inner,
        session,
        command_id,
        message_id,
        requested_revision,
        offset,
    );
    None
}
