use super::*;

/// Production entry point for the complete `msg.fetch` validation and chunk enqueue path; `now_ms` uses the real clock and replies are sent asynchronously.
/// There is **no direct return value**: unlike `input.send` or `control.stop`, `msg.fetch` does not
/// return an `input.ack`. Its only responses are `msg.chunk` or `msg.fetch.error` under the
/// `reply` kind. All responses go through `try_enqueue_reply` into `state.reply_queue` and are
/// sent asynchronously by `drain_reply_queue`.
pub(super) fn handle_msg_fetch(
    inner: &Inner,
    session: &str,
    command_id: &str,
    message_id: i64,
    requested_revision: i64,
    offset: usize,
) {
    handle_msg_fetch_at(
        inner,
        session,
        command_id,
        message_id,
        requested_revision,
        offset,
        now_unix_ms(),
    );
}

/// Testable core of `handle_msg_fetch`. `now_ms` is passed explicitly so unit tests can pin the
/// boundaries of the single-flight timeout and byte-budget window, as in
/// `is_control_stop_stale`. The validation chain has a fixed order: reused `command_id` ledger
/// entries produce `busy`; active-repo ownership, message existence and ownership, and session
/// soft deletion produce `forbidden`, `not_found`, or `soft_deleted`; revision validation
/// produces `stale_revision` with `current_ref`; the `total_bytes` limit produces `too_large`;
/// an out-of-range offset produces `not_found`; single-flight or the 60-second byte budget
/// produces `busy`; and successful validation enqueues the chunks.
pub(super) fn handle_msg_fetch_at(
    inner: &Inner,
    session: &str,
    command_id: &str,
    message_id: i64,
    requested_revision: i64,
    offset: usize,
    now_ms: u64,
) {
    let state = &inner.state;
    // Store the connection generation with every queued reply so draining can reject data left over from an earlier connection.
    // `drain_reply_queue` uses the generation on `ReplyQueueItem` to detect data left over from a
    // previous connection.
    let connection_generation = state.connection_generation_snapshot();
    let Some(generation) =
        msg_fetch::admit_command_id(state, session, command_id, connection_generation)
    else {
        return;
    };

    let reply_error = |code: &str, current_ref: Option<Value>| {
        if !try_enqueue_reply(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: msg_fetch_error_payload(code, current_ref),
                final_frame: true,
                generation,
                connection_generation,
            },
        ) {
            // The reply queue is so full that even this terminal error cannot be enqueued, an
            // extreme double-saturation case. Count it accurately instead of pretending it was
            // sent. This request has not yet occupied the session's single-flight slot because
            // this early return occurs before admission, so no additional release is needed.
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
        }
    };

    let Some((content_raw, revision)) =
        msg_fetch::lookup_message(inner, session, message_id, &reply_error)
    else {
        return;
    };
    if !msg_fetch::validate_message(
        message_id,
        requested_revision,
        offset,
        &content_raw,
        revision,
        &reply_error,
    ) {
        return;
    }
    if !msg_fetch::admit_inflight_and_budget(
        state,
        session,
        command_id,
        generation,
        now_ms,
        content_raw.len(),
        &reply_error,
    ) {
        return;
    }
    msg_fetch::enqueue_chunks(
        state,
        session,
        command_id,
        (message_id, revision, offset, &content_raw),
        generation,
        connection_generation,
    );
}
