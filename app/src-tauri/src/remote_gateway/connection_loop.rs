use super::*;

pub(super) struct KeepaliveIdle {
    last_activity_at: Instant,
}

impl KeepaliveIdle {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            last_activity_at: now,
        }
    }

    pub(super) fn record_activity(&mut self, now: Instant) {
        self.last_activity_at = now;
    }

    pub(super) fn send_ping_if_due<E>(
        &mut self,
        now: Instant,
        state: &GatewayInnerState,
        send: impl FnOnce(Message) -> Result<(), E>,
    ) -> Result<bool, E> {
        if now.saturating_duration_since(self.last_activity_at) < KEEPALIVE_IDLE_INTERVAL {
            return Ok(false);
        }

        send(Message::Ping(Vec::new().into()))?;
        state.frames_sent.fetch_add(1, Ordering::Relaxed);
        state.keepalive_pings_sent.fetch_add(1, Ordering::Relaxed);
        self.record_activity(now);
        Ok(true)
    }
}

pub(super) fn run_connection_request(
    inner: &Arc<Inner>,
    request: tungstenite::handshake::client::Request,
    connected_config: &GatewayConfig,
    connected_token: Option<&SecretToken>,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Result<ConnectionExit, ConnectionFailure> {
    let (mut socket, mut active_k_room, connection_generation) =
        connection_request::prepare_connection(inner, request, connected_config, k_room)?;
    // Generation filtering makes disconnect-time queue draining unnecessary, while this guard
    // closes the gate on both ordinary returns and panic unwinds.
    let _upstream_gate_guard = UpstreamGateGuard::new(&inner.state.upstream_state);
    request_session_index_snapshot(inner, connection_generation);
    let connection_loop_started_at = Instant::now();
    let mut last_liveness_check = connection_loop_started_at;
    let mut keepalive_idle = KeepaliveIdle::new(connection_loop_started_at);
    let mut last_observed_frames_sent = inner.state.frames_sent.load(Ordering::Relaxed);
    // After a rejected put with a pending refresh receipt (the `RefreshDropped` branch of
    // `consume_token_ack`), the relay's registry still has the old codename. The relay must learn
    // the current DB truth before a phone retry using the old refresh can proceed. Record only the
    // intent to disconnect here; defer the actual disconnect until the criterion below is met
    // (this iteration times out reading, or the pending state exceeds the hard deadline).
    let mut registry_resync_pending = false;
    // Record when `registry_resync_pending` changes from false to true as the starting point for
    // the hard deadline's `Instant::elapsed` check; `None` means it has never been pending.
    let mut registry_resync_pending_since: Option<Instant> = None;
    // Cache session-to-repo ownership for the lifetime of the connection. It lives here rather
    // than inside each drain iteration so hits accumulate across `drain_upstream` calls instead
    // of degrading into one query per item. `sessions.repo_id` can change through the registered
    // `update_session_repo` IPC in `lib.rs`, so cached ownership must be invalidated. Production
    // frontend code currently has no call site, but the ordinary core function remains available
    // to tests and a future "move a session to another project" feature; treating the value as
    // immutable would create a dangerous false security invariant. The local variable below holds
    // the `SESSION_REPO_EPOCH` baseline as a plain `u64` private to this connection thread, rather
    // than an atomic in `GatewayInnerState`. `upstream_session_allowed` is called only from this
    // connection's drain loop, so the state is not shared and needs no atomic or lock. This also
    // makes the two-phase check, synchronizing before and after ownership evaluation, explicit.
    // Each call compares the baseline with the global epoch, clearing and rebuilding the cache on
    // the next check by this connection after ownership is rebound.
    let mut session_repo_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut session_repo_epoch_seen: u64 = SESSION_REPO_EPOCH.load(Ordering::Acquire);

    // The closure returns `ConnectionFailure` instead of `String`. Because `ConnectionFailure`
    // implements `From<String>`, every existing `?` on an early `Result<(), String>` return uses
    // that conversion and preserves the externally observable error text byte for byte. The
    // resync branch below also constructs `ConnectionFailure::Other` directly to trigger a
    // retryable disconnect, and the closure itself is the `run_connection_request` return value,
    // so their `Result<ConnectionExit, ConnectionFailure>` types must match.
    (|| -> Result<ConnectionExit, ConnectionFailure> {
        loop {
            if let Some(exit) = connection_request::take_loop_control(inner, &mut socket) {
                return Ok(exit);
            }

            let read_timed_out_this_round = match connection_request::process_socket_read(
                &mut socket,
                inner,
                &mut active_k_room,
                connection_generation,
                &mut keepalive_idle,
            )? {
                connection_request::SocketRead::Continue(read_timed_out_this_round) => {
                    read_timed_out_this_round
                }
                connection_request::SocketRead::Exit(exit) => return Ok(exit),
            };
            connection_request::check_registry_resync(
                inner,
                read_timed_out_this_round,
                &mut registry_resync_pending,
                &mut registry_resync_pending_since,
            )?;

            if let Some(exit) = connection_request::take_shutdown(inner, &mut socket) {
                return Ok(exit);
            }

            if let Err(error) = drain_upstream(
                &mut socket,
                &inner.state,
                upstream_rx,
                milestone_rx,
                active_k_room.as_ref(),
                &connected_config.room_id,
                &inner.session_repo_provider,
                &mut session_repo_cache,
                &mut session_repo_epoch_seen,
            ) {
                return Err(error.into());
            }
            drain_registry_outbox(&mut socket, inner)?;
            drain_input_ack_outbox(&mut socket, inner)?;

            connection_request::maintain_keepalive(
                &mut socket,
                &inner.state,
                &mut keepalive_idle,
                &mut last_observed_frames_sent,
                read_timed_out_this_round,
            )?;

            if let Some(exit) = connection_request::check_liveness(
                &mut socket,
                inner,
                connected_config,
                connected_token,
                active_k_room.as_ref(),
                &mut last_liveness_check,
                connection_loop_started_at,
            ) {
                return Ok(exit);
            }
        }
    })()
}

pub(super) fn drain_upstream(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Result<(), String> {
    drain_upstream_with_budget(
        socket,
        state,
        upstream_rx,
        milestone_rx,
        k_room,
        room,
        session_repo_provider,
        session_repo_cache,
        session_repo_epoch_seen,
        DRAIN_ROUND_BUDGET,
    )
}

pub(super) fn drain_upstream_with_budget(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
    budget: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    let connection_generation = state.connection_generation_snapshot();
    let mut drained_items = 0;

    // Drain replies before milestone/live traffic so explicit remote `msg.fetch` requests receive
    // prompt responses instead of waiting behind background milestone/live broadcasts. The
    // `reply_queue` is backed by its own `Mutex<VecDeque<_>>`, but all three drains share a single
    // `drained_items` counter and `deadline`, i.e. one `MAX_DRAIN_ITEMS_PER_ROUND` budget and one
    // `DRAIN_ROUND_BUDGET` deadline for the whole round. Placing replies first means that when the
    // round's shared budget is exhausted, milestone/live traffic may not get to send anything this
    // round.
    if drain_reply_queue(
        socket,
        state,
        k_room,
        room,
        connection_generation,
        deadline,
        &mut drained_items,
        session_repo_provider,
        session_repo_cache,
        session_repo_epoch_seen,
    )? {
        return Ok(());
    }
    if drain_milestone_queue(
        socket,
        state,
        milestone_rx,
        k_room,
        room,
        connection_generation,
        deadline,
        &mut drained_items,
        session_repo_provider,
        session_repo_cache,
        session_repo_epoch_seen,
    )? {
        return Ok(());
    }
    drain_live_queue(
        socket,
        state,
        upstream_rx,
        k_room,
        room,
        connection_generation,
        deadline,
        &mut drained_items,
        session_repo_provider,
        session_repo_cache,
        session_repo_epoch_seen,
    )?;
    Ok(())
}

/// Drain the bounded `state.reply_queue`, sealing each item as a `reply` envelope through
/// `send_upstream_value` after rechecking connection generation and session ownership.
///
/// Ownership is checked again when dequeuing. Because `reply_queue` persists, a fragment can span
/// a reconnect (`connection_generation` changes) or an active-repo switch (a previously allowed
/// session no longer belongs to the active repo). The admission check in `handle_msg_fetch_at`
/// proves only that the item was valid when enqueued. Following the existing two-phase pattern in
/// `drain_milestone_queue` and `drain_live_queue`, this function verifies both that
/// `connection_generation` is unchanged and that `upstream_session_allowed` still accepts the
/// session. A rejected fragment is dropped without sending or touching the socket and is counted
/// in `reply_stale_connection_dropped` or `reply_repo_filtered_dropped`. Separate counters avoid
/// confusing the diagnostic source because `upstream_stale_generation_dropped` and
/// `upstream_repo_filtered` are documented specifically for upstream milestone and live events.
///
/// The session's `msg_fetch_inflight` slot is cleared only after processing an item with
/// `final_frame == true`; see `ReplyQueueItem::final_frame`. This is the sole path that both
/// processes the terminal frame and releases the single-flight slot. The clear matches on
/// `generation`, not `command_id`; see `clear_msg_fetch_inflight_if_matches`, which prevents an
/// old terminal fragment from clearing a newer owner when a command ID is reused. Release is still
/// attempted when sending is skipped because `k_room` is missing or the ownership recheck fails.
/// Holding the slot provides no extra protection when the connection lacks a usable key or the
/// session no longer belongs to the active repo, matching the existing `upstream_dropped` and
/// `milestone_dropped` behavior when `k_room` is absent.
#[allow(clippy::too_many_arguments)]
fn drain_reply_queue(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
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
        let Some(item) = lock(&state.reply_queue).pop_front() else {
            break;
        };
        *drained_items += 1;
        let ReplyQueueItem {
            session,
            command_id,
            payload,
            final_frame,
            generation,
            connection_generation: enqueued_connection_generation,
        } = item;
        let session_for_inflight = session.clone();

        let stale_connection = enqueued_connection_generation != connection_generation;
        let repo_denied = !stale_connection
            && session.as_deref().is_some_and(|session_id| {
                !upstream_session_allowed(
                    state,
                    session_repo_provider,
                    session_repo_cache,
                    session_repo_epoch_seen,
                    session_id,
                )
            });
        if stale_connection {
            state
                .reply_stale_connection_dropped
                .fetch_add(1, Ordering::Relaxed);
        } else if repo_denied {
            state
                .reply_repo_filtered_dropped
                .fetch_add(1, Ordering::Relaxed);
        } else if let Some(k_room) = k_room {
            send_upstream_value(
                socket,
                state,
                k_room,
                room,
                "reply",
                session,
                payload,
                None,
                Some(&command_id),
            )?;
        } else {
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
        }
        if final_frame {
            if let Some(session_id) = session_for_inflight {
                clear_msg_fetch_inflight_if_matches(state, &session_id, generation);
            }
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

/// Release a session single-flight slot only when its current generation matches this request.
/// `handle_msg_fetch_at` assigns a new, globally increasing value from
/// `GatewayInnerState::msg_fetch_generation_counter` to every accepted request. Matching by
/// `command_id` allowed a delayed terminal fragment from an expired request to clear the slot of
/// its replacement when the client reused the same command ID. A generation is unique to each
/// admission and cannot collide with an earlier or later one, eliminating that path. Rejecting
/// command ID reuse in `msg_fetch_command_ledger_admit` provides a second defense, but generation
/// matching remains correct even if ledger entries are evicted for capacity.
pub(super) fn clear_msg_fetch_inflight_if_matches(
    state: &GatewayInnerState,
    session: &str,
    generation: u64,
) {
    let mut inflight = lock(&state.msg_fetch_inflight);
    if inflight
        .get(session)
        .is_some_and(|entry| entry.generation == generation)
    {
        inflight.remove(session);
    }
}

/// Try to enqueue a single reply in the independent bounded `state.reply_queue`, whose capacity is
/// `REPLY_QUEUE_CAPACITY`. Return `false` when full so the caller can stop the current fragment
/// sequence and attempt a terminal `msg.fetch.error{busy}` through the `handle_msg_fetch_at`
/// fallback instead of silently dropping a frame. This function handles single-frame replies,
/// including `msg.fetch.error`, the `busy` fallback itself, and ordinary replies. Multi-fragment
/// transfers use `try_enqueue_reply_chunk`, which reserves one slot for the fallback error.
pub(super) fn try_enqueue_reply(state: &GatewayInnerState, item: ReplyQueueItem) -> bool {
    let mut queue = lock(&state.reply_queue);
    if queue.len() >= REPLY_QUEUE_CAPACITY {
        return false;
    }
    queue.push_back(item);
    true
}

/// Reserve one reply queue slot when enqueueing chunks so a capacity failure can be followed by a
/// terminal `msg.fetch.error{busy}` instead of failing silently. If a chunk were allowed to fill
/// the queue completely before failure, the fallback error would immediately fail under the same
/// saturation, leaving even the busy response unsent. That narrow edge still exists when the queue
/// is already prefilled exactly to capacity; see `reply_queue_dropped` and the two tests
/// `handle_msg_fetch_reply_queue_full_aborts_transfer_and_appends_busy_terminal`/
/// `handle_msg_fetch_double_saturation_drops_and_releases_inflight_when_even_the_busy_error_
/// cannot_fit`. During ordinary saturation partway through a fragment sequence, this reservation
/// guarantees one available slot.
pub(super) fn try_enqueue_reply_chunk(state: &GatewayInnerState, item: ReplyQueueItem) -> bool {
    let mut queue = lock(&state.reply_queue);
    if queue.len() + 1 >= REPLY_QUEUE_CAPACITY {
        return false;
    }
    queue.push_back(item);
    true
}

/// When a new fetch takes over an expired single-flight slot, purge all unsent fragments from the
/// superseded generation in `reply_queue`. The client abandoned that old fetch after receiving no
/// response within `MSG_FETCH_INFLIGHT_TIMEOUT_MS`; draining its fragments would send data
/// unrelated to the current request. Because `msg_fetch_command_ledger_admit` prevents resubmitting
/// the old command ID, the client cannot be awaiting later frames for it. Matching the generation
/// removes only fragments for the displaced admission in this session, without affecting earlier
/// or later generations. Normally only one generation is queued at a time, but filtering by
/// generation instead of clearing the entire session remains precise under extreme timing.
pub(super) fn purge_stale_reply_queue_generation(
    state: &GatewayInnerState,
    session: &str,
    generation: u64,
) {
    let mut queue = lock(&state.reply_queue);
    let before = queue.len();
    queue.retain(|item| {
        !(item.session.as_deref() == Some(session) && item.generation == generation)
    });
    let purged = before - queue.len();
    if purged > 0 {
        state
            .reply_queue_stale_generation_purged
            .fetch_add(purged as u64, Ordering::Relaxed);
    }
}
