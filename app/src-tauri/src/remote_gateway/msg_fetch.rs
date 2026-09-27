use super::*;

pub(super) fn admit_command_id(
    state: &GatewayInnerState,
    session: &str,
    command_id: &str,
    connection_generation: u64,
) -> Option<u64> {
    // Step 0 (rework 2, skeptic follow-up review): the command_id reuse ledger rejects every
    // `(session, command_id)` that has been processed before, returning busy to direct the client
    // to use a new command_id. This must come first: once admitted, every early-return path below
    // records this `(session, command_id)` in `msg_fetch_inflight` or `reply_queue`, and correct
    // `generation` decisions require that the same `(session, command_id)` be processed by this
    // function only once.
    if !msg_fetch_command_ledger_admit(state, session, command_id) {
        // This busy reply itself receives a fresh generation. It is never written to
        // `msg_fetch_inflight`, because processing never reaches step 4. Its `generation` field
        // only satisfies the `ReplyQueueItem` shape and takes part in no later comparison.
        let generation = state
            .msg_fetch_generation_counter
            .fetch_add(1, Ordering::Relaxed);
        if !try_enqueue_reply(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: msg_fetch_error_payload("busy", None),
                final_frame: true,
                generation,
                connection_generation,
            },
        ) {
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
        }
        return None;
    }
    Some(
        state
            .msg_fetch_generation_counter
            .fetch_add(1, Ordering::Relaxed),
    )
}

pub(super) fn lookup_message(
    inner: &Inner,
    session: &str,
    message_id: i64,
    reply_error: &impl Fn(&str, Option<Value>),
) -> Option<(String, i64)> {
    // Step 1: the active-repo ownership gate precedes every message-level query. An unauthorized
    // request cannot even probe whether this message_id exists, so existence differences are not
    // leaked, matching the established `command_session_allowed` pattern.
    if !command_session_allowed(inner, session) {
        reply_error("forbidden", None);
        return None;
    }
    let (content_raw, revision, session_deleted) =
        match (inner.message_fetch_provider)(session, message_id) {
            Ok(MessageForFetchResult::Found {
                content_raw,
                revision,
                session_deleted,
            }) => (content_raw, revision, session_deleted),
            Ok(MessageForFetchResult::WrongSession) => {
                reply_error("forbidden", None);
                return None;
            }
            Ok(MessageForFetchResult::NotFound) => {
                reply_error("not_found", None);
                return None;
            }
            Err(error) => {
                // If the query itself fails with a DB error, fail closed rather than treating an
                // unknown result as permission. Do not leak internal error details to the remote
                // peer; of the six existing codes, `forbidden` most closely means "reject without
                // explaining why."
                eprintln!("remote gateway: msg.fetch lookup failed — {error}");
                reply_error("forbidden", None);
                return None;
            }
        };
    if session_deleted {
        reply_error("soft_deleted", None);
        return None;
    }
    Some((content_raw, revision))
}

pub(super) fn validate_message(
    message_id: i64,
    requested_revision: i64,
    offset: usize,
    content_raw: &str,
    revision: i64,
    reply_error: &impl Fn(&str, Option<Value>),
) -> bool {
    // Step 2: validate the `offset` resume against the current revision; a mismatch is stale.
    if requested_revision != revision {
        reply_error(
            "stale_revision",
            Some(build_content_ref(message_id, revision, content_raw)),
        );
        return false;
    }

    // Step 3: enforce the total_bytes limit.
    if content_raw.len() > MSG_FETCH_TOTAL_BYTES_LIMIT {
        reply_error("too_large", None);
        return false;
    }

    // Step 3.5 (rework 4, new M0 §10.4 clause): an offset strictly greater than total_bytes is
    // protocol misuse. It is not the valid terminal state "all content has already been received";
    // that case is `offset == total_bytes` and proceeds to step 5 to produce one zero-length final
    // frame, as documented by `build_msg_chunks`. Return `not_found` instead of the former silent
    // clamping. This honestly tells the client that the request itself is invalid instead of
    // pretending to succeed while doing nothing.
    if offset > content_raw.len() {
        reply_error("not_found", None);
        return false;
    }
    true
}

pub(super) fn admit_inflight_and_budget(
    state: &GatewayInnerState,
    session: &str,
    command_id: &str,
    generation: u64,
    now_ms: u64,
    total_bytes: usize,
    reply_error: &impl Fn(&str, Option<Value>),
) -> bool {
    // Enforce one in-flight fetch per session and a gateway-wide 60-second byte budget rather than
    // separate per-session buckets. Acquire and release the two locks in a fixed order, inflight
    // before budget, without holding nested locks across business logic. This prevents lock-order
    // disagreement with other locking paths.
    {
        let mut inflight = lock(&state.msg_fetch_inflight);
        if let Some(existing) = inflight.get(session) {
            if msg_fetch_inflight_is_active(existing.accepted_at_ms, now_ms) {
                drop(inflight);
                reply_error("busy", None);
                return false;
            }
        }
        // Rework 2 (skeptic follow-up review): before taking over, record the generation of the
        // superseded admission. Immediately after releasing the lock, use it to remove fragments
        // with that generation from `reply_queue`; see `purge_stale_reply_queue_generation` docs.
        // This prevents fragments from the old fetch from being drained normally to a client that
        // no longer cares about them.
        let superseded_generation = inflight.get(session).map(|entry| entry.generation);
        inflight.insert(
            session.to_owned(),
            MsgFetchInflightEntry {
                command_id: command_id.to_owned(),
                accepted_at_ms: now_ms,
                generation,
            },
        );
        drop(inflight);
        if let Some(superseded_generation) = superseded_generation {
            purge_stale_reply_queue_generation(state, session, superseded_generation);
        }
    }
    let admitted = {
        let mut budget_slot = lock(&state.msg_fetch_byte_budget);
        let window = std::mem::take(&mut *budget_slot);
        let (admit, window) = msg_fetch_budget_admit(window, now_ms, total_bytes);
        *budget_slot = window;
        admit
    };
    if !admitted {
        // If the budget rejects the request, release the just-claimed single-flight slot. This
        // request never actually began transferring and must not occupy the slot until the
        // 30-second timeout releases it passively. Then return busy.
        clear_msg_fetch_inflight_if_matches(state, session, generation);
        reply_error("busy", None);
        return false;
    }
    true
}

pub(super) fn enqueue_chunks(
    state: &GatewayInnerState,
    session: &str,
    command_id: &str,
    message: (i64, i64, usize, &str),
    generation: u64,
    connection_generation: u64,
) {
    let (message_id, revision, offset, content_raw) = message;
    // Step 5: all checks passed, so enqueue the chunks. `build_msg_chunks` always returns a
    // nonempty sequence, as documented, so `last_index` is always defined and `final_frame` always
    // has a destination.
    let chunks = build_msg_chunks(message_id, revision, content_raw.as_bytes(), offset);
    let last_index = chunks.len() - 1;
    let mut fully_enqueued = true;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let enqueued = try_enqueue_reply_chunk(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: chunk,
                final_frame: index == last_index,
                generation,
                connection_generation,
            },
        );
        if !enqueued {
            fully_enqueued = false;
            break;
        }
    }
    if !fully_enqueued {
        // M0 §10.9: when full, drop the entire transfer and return a busy final state so the
        // failure is observable rather than silent. Already enqueued prefix chunks are still sent
        // normally. For the same command_id, the client receives "some chunks plus a busy final
        // state"; the whole-transfer SHA-256 check required by §10.9 necessarily fails, so the
        // client discards the fetch and may retry with the same or a new command_id. It never
        // displays partial content.
        if !try_enqueue_reply(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: msg_fetch_error_payload("busy", None),
                final_frame: true,
                generation,
                connection_generation,
            },
        ) {
            // Double saturation: even the fallback error cannot be enqueued. Release the
            // single-flight slot so it does not remain stuck until timeout. Count the event
            // accurately rather than pretending that the client was notified.
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
            clear_msg_fetch_inflight_if_matches(state, session, generation);
        }
    }
}
