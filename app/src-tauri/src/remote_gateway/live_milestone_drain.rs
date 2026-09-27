use super::*;

pub(super) fn drain_milestone_queue(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    connection_generation: u64,
    deadline: Instant,
    drained_items: &mut usize,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Result<bool, String> {
    while *drained_items < MAX_DRAIN_ITEMS_PER_ROUND {
        let Ok((item_generation, item)) = milestone_rx.try_recv() else {
            break;
        };
        *drained_items += 1;
        if item_generation != connection_generation {
            state
                .upstream_stale_generation_dropped
                .fetch_add(1, Ordering::Relaxed);
        } else if let Some(k_room) = k_room {
            let MilestoneItem {
                session,
                t,
                payload,
                client_msg_id,
            } = item;
            // Check `contains('|')`, which always identifies an invalid frame, before ownership.
            // As in `drain_live_queue`, a pipe frame short-circuits to `classify_skipped` without
            // spending an ownership evaluation or database query.
            let contains_pipe = session.as_deref().is_some_and(|value| value.contains('|'));
            if contains_pipe {
                state.classify_skipped.fetch_add(1, Ordering::Relaxed);
            } else if session.is_none()
                && t == "session.index"
                && payload.get("full").and_then(Value::as_bool) != Some(true)
            {
                // Filter incremental `session.index` diffs according to their four operations; see
                // `filter_session_index_incremental_for_active_repo`. Full snapshots, where
                // `full == true`, use `filter_session_index_snapshot_for_active_repo` and do not
                // enter this branch.
                match filter_session_index_incremental_for_active_repo(
                    state,
                    session_repo_provider,
                    session_repo_cache,
                    session_repo_epoch_seen,
                    payload,
                ) {
                    Some(rewritten_payload) => {
                        send_upstream_value(
                            socket,
                            state,
                            k_room,
                            room,
                            "event",
                            session,
                            milestone_payload(&t, rewritten_payload),
                            Some(&client_msg_id),
                            None,
                        )?;
                    }
                    None => {
                        state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else {
                // Check session ownership before publishing each milestone, including
                // msg.completed, card.created, card.resolved, run.status, and snapshot. Silently
                // skip items outside the active repo because this is filtering, not an error.
                // Apart from the incremental session.index branch above, items without a session
                // should not normally occur, but remain fail-open: `is_some_and` is false for
                // `None`, so an item with no ownership to evaluate passes unchanged.
                // Snapshot milestones already pass an ownership gate in `handle_command_envelope`
                // before enqueueing. This second check happens before dequeueing, leaving a narrow
                // window in which ownership can change through an active-repo switch. A snapshot
                // already acknowledged as Ok to the remote may then be silently filtered here.
                // This is the existing enqueue-time check plus dequeue-time recheck pattern used
                // by other milestones, not a snapshot-specific gap; behavior is unchanged.
                let attribution_blocked = session.as_deref().is_some_and(|session_id| {
                    !upstream_session_allowed(
                        state,
                        session_repo_provider,
                        session_repo_cache,
                        session_repo_epoch_seen,
                        session_id,
                    )
                });
                if attribution_blocked {
                    state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
                    if let Some(session_id) = session.as_deref() {
                        eprintln!(
                            "remote gateway: upstream frame filtered — session={} type={}",
                            session_id.chars().take(8).collect::<String>(),
                            t
                        );
                    }
                } else {
                    send_upstream_value(
                        socket,
                        state,
                        k_room,
                        room,
                        "event",
                        session,
                        milestone_payload(&t, payload),
                        Some(&client_msg_id),
                        None,
                    )?;
                }
            }
        } else {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        }

        if Instant::now() >= deadline {
            state
                .upstream_budget_dropped
                .fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn drain_live_queue(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    connection_generation: u64,
    deadline: Instant,
    drained_items: &mut usize,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Result<bool, String> {
    while *drained_items < MAX_DRAIN_ITEMS_PER_ROUND {
        let Ok((item_generation, item)) = upstream_rx.try_recv() else {
            break;
        };
        *drained_items += 1;
        if item_generation != connection_generation {
            state
                .upstream_stale_generation_dropped
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let Some(k_room) = k_room else {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        };

        match item {
            LiveQueueItem::Batch(payload) => {
                // Preserve the delta path's existing per-batch, per-event classification and
                // budget behavior.
                for batch in payload.batches {
                    for sequenced in batch.events {
                        let classified = classify(&sequenced.event, sequenced.seq)
                            .map(|(kind, value)| (kind, value, None::<String>));

                        if let Some((kind, value, client_msg_id)) = classified {
                            // Filter live events by session ownership just like msg.completed.
                            // `batch.session_id` is not optional and is always present.
                            let contains_pipe = batch.session_id.contains('|');
                            let attribution_blocked = !contains_pipe
                                && !upstream_session_allowed(
                                    state,
                                    session_repo_provider,
                                    session_repo_cache,
                                    session_repo_epoch_seen,
                                    &batch.session_id,
                                );
                            if contains_pipe {
                                state.classify_skipped.fetch_add(1, Ordering::Relaxed);
                            } else if attribution_blocked {
                                state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
                                eprintln!(
                                    "remote gateway: upstream frame filtered — session={} type={}",
                                    batch.session_id.chars().take(8).collect::<String>(),
                                    kind
                                );
                            } else {
                                send_upstream_value(
                                    socket,
                                    state,
                                    k_room,
                                    room,
                                    kind,
                                    Some(batch.session_id.clone()),
                                    value,
                                    client_msg_id.as_deref(),
                                    None,
                                )?;
                            }
                        }

                        if Instant::now() >= deadline {
                            state
                                .upstream_budget_dropped
                                .fetch_add(1, Ordering::Relaxed);
                            return Ok(true);
                        }
                    }
                }
            }
            LiveQueueItem::Prebuilt(item) => {
                if let Some(frame) = prepare_prebuilt_live_for_drain(
                    state,
                    item,
                    session_repo_provider,
                    session_repo_cache,
                    session_repo_epoch_seen,
                ) {
                    send_upstream_value(
                        socket,
                        state,
                        k_room,
                        room,
                        frame.kind,
                        Some(frame.session),
                        frame.payload,
                        Some(&frame.client_msg_id),
                        None,
                    )?;
                }
                if Instant::now() >= deadline {
                    state
                        .upstream_budget_dropped
                        .fetch_add(1, Ordering::Relaxed);
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[derive(Debug, PartialEq)]
pub(super) struct PreparedLiveFrame {
    pub(super) kind: &'static str,
    pub(super) session: String,
    pub(super) payload: Value,
    pub(super) client_msg_id: String,
}

/// Route the prebuilt branch of `drain_live_queue` through this function before writing to the
/// socket. It preserves the session-ownership gate and wraps the finalized history payload with
/// kind="live". Keeping this as a pure step lets tests enforce both wire and security invariants
/// without a network; the actual drain consumes only results admitted here.
pub(super) fn prepare_prebuilt_live_for_drain(
    state: &GatewayInnerState,
    item: MilestoneItem,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Option<PreparedLiveFrame> {
    let Some(session) = item.session else {
        state.classify_skipped.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let contains_pipe = session.contains('|');
    let attribution_blocked = !contains_pipe
        && !upstream_session_allowed(
            state,
            session_repo_provider,
            session_repo_cache,
            session_repo_epoch_seen,
            &session,
        );
    if contains_pipe {
        state.classify_skipped.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if attribution_blocked {
        state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "remote gateway: upstream frame filtered — session={} type={}",
            session.chars().take(8).collect::<String>(),
            item.t
        );
        return None;
    }
    Some(PreparedLiveFrame {
        kind: "live",
        session,
        payload: milestone_payload(&item.t, item.payload),
        client_msg_id: item.client_msg_id,
    })
}

pub(super) fn send_upstream_value(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    k_room: &[u8; 32],
    room: &str,
    kind: &str,
    session: Option<String>,
    value: Value,
    client_msg_id: Option<&str>,
    // Outbound `reply` frames require a nonempty `command_id` for request correlation; existing
    // event and live frames continue to pass `None`, preserving their behavior byte for byte.
    // `drain_reply_queue` is the only caller that passes `Some(..)`.
    command_id: Option<&str>,
) -> Result<(), String> {
    let plaintext = match serde_json::to_vec(&value) {
        Ok(plaintext) => plaintext,
        Err(_) => {
            state.classify_skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };
    let meta = EnvelopeMeta {
        v: 1,
        room: room.to_owned(),
        epoch: state.epoch.load(Ordering::Acquire),
        kind: kind.to_owned(),
        session,
        command_id: command_id.map(str::to_owned),
    };
    let (ct, n) = crate::remote_crypto::seal(k_room, &meta, &plaintext);
    let envelope = build_envelope_json(&meta, &ct, &n, now_unix_ms(), client_msg_id);
    socket
        .send(Message::Text(envelope.to_string().into()))
        .map_err(|error| format!("upstream write failed: {error}"))?;
    state.frames_sent.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub(super) fn milestone_payload(t: &str, payload: Value) -> Value {
    match payload {
        Value::Object(mut fields) => {
            fields.insert("t".to_owned(), Value::String(t.to_owned()));
            Value::Object(fields)
        }
        value => serde_json::json!({"t": t, "payload": value}),
    }
}

/// Measure the complete milestone frame written to encrypted plaintext: merge `t` into the raw
/// builder payload first, then count its JSON bytes.
pub(super) fn milestone_frame_bytes(t: &str, payload: &Value) -> usize {
    serde_json::to_vec(&milestone_payload(t, payload.clone()))
        .map(|json| json.len())
        .unwrap_or(usize::MAX)
}
