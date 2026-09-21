use super::*;

type ConnectionSocket = tungstenite::WebSocket<MaybeTlsStream<TcpStream>>;
type PreparedConnection = (ConnectionSocket, Option<Zeroizing<[u8; 32]>>, u64);

pub(super) enum SocketRead {
    Continue(bool),
    Exit(ConnectionExit),
}

pub(super) fn prepare_connection(
    inner: &Arc<Inner>,
    request: tungstenite::handshake::client::Request,
    connected_config: &GatewayConfig,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Result<PreparedConnection, ConnectionFailure> {
    // M2-4c/M2-4d: Carry the active-repo context resolved for this connection into the
    // shared state used for ownership checks, as early as possible. This must happen before
    // `request_session_index_snapshot` below; otherwise, the background snapshot worker might
    // build a snapshot using gating state left by the previous connection, or even the previous
    // project. Downstream `handle_command_envelope`, upstream drain, and the snapshot provider
    // all read only this value instead of each rereading the `remote_active_repo_id` setting. This
    // avoids two competing sources of truth between the setting's "current" value and the room
    // actually used by this connection. Under the single-active-room model, `active_repo_id` is
    // always `Some`: successful construction of `GatewayConfig` means active-repo resolution
    // succeeded. The command-ownership gate is therefore always enabled and needs no separate
    // boolean switch.
    *lock(&inner.state.active_repo_id_for_gating) = connected_config.active_repo_id.clone();

    let pairing_k_room_is_staged = lock(&inner.registry).pairing_k_room_is_staged();
    let active_k_room = if pairing_k_room_is_staged {
        None
    } else {
        k_room.cloned()
    };
    // Never accept tungstenite's much larger defaults: a bad relay must not allocate huge frames.
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_INCOMING_BYTES))
        .max_frame_size(Some(MAX_INCOMING_BYTES));
    // Known limitation: DNS, TCP, TLS, and WebSocket handshakes do not yet have a timeout.
    let (mut socket, _) = match connect_with_config(request, Some(config), WS_MAX_REDIRECTS) {
        Ok(connection) => connection,
        Err(WebSocketError::Http(response))
            if response.status() == tungstenite::http::StatusCode::UNAUTHORIZED =>
        {
            return Err(ConnectionFailure::Unauthorized);
        }
        Err(WebSocketError::Http(response))
            if response.status() == tungstenite::http::StatusCode::GONE =>
        {
            return Err(ConnectionFailure::Tombstoned);
        }
        Err(error) => return Err(ConnectionFailure::Other(format!("connect failed: {error}"))),
    };

    // Blocking tungstenite has no cancellation primitive. A TCP read timeout lets this thread
    // wake periodically to observe shutdown without adding an async runtime or a wake-up socket.
    set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT))
        .map_err(|error| format!("failed to set read timeout: {error}"))?;
    set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT))
        .map_err(|error| format!("failed to set write timeout: {error}"))?;
    // §9.4 registry_ready: token.sync is the first desktop application frame, and no existing
    // activation side effect is published until the matching ack (including any rebase loop).
    synchronize_registry(&mut socket, inner, &connected_config.room_id)?;
    lock(&inner.registry).prepare_outbox_for_reconnect();
    drain_registry_outbox(&mut socket, inner).map_err(ConnectionFailure::Other)?;
    set_status(&inner.state, GatewayState::Connected, None);
    // Generation and gate are published together in a single AtomicU64 store so that a sink
    // reading upstream_state in one Acquire load can never observe a (gate, generation) pair
    // that spans two different connections.
    let connection_generation = inner
        .state
        .advance_generation_and_set_gate(active_k_room.is_some());
    Ok((socket, active_k_room, connection_generation))
}

pub(super) fn take_loop_control(
    inner: &Inner,
    socket: &mut ConnectionSocket,
) -> Option<ConnectionExit> {
    if inner.shutdown.load(Ordering::Acquire) {
        let _ = socket.close(None);
        drain_close(socket);
        return Some(ConnectionExit::ClosedByPeer);
    }
    if inner.reload_requested.swap(false, Ordering::AcqRel) {
        let _ = socket.close(None);
        drain_close(socket);
        return Some(ConnectionExit::PairingReloadRequested);
    }
    None
}

pub(super) fn process_socket_read(
    socket: &mut ConnectionSocket,
    inner: &Arc<Inner>,
    active_k_room: &mut Option<Zeroizing<[u8; 32]>>,
    connection_generation: u64,
    keepalive_idle: &mut KeepaliveIdle,
) -> Result<SocketRead, ConnectionFailure> {
    // S1i1 rework four: Track whether `socket.read()` genuinely timed out in this iteration. Only
    // that branch proves that the relay has no new frame in flight at this moment. Ping, Pong, or
    // Binary merely means that this iteration did not read an application Text frame; it does not
    // mean the connection is quiet. The peer is still alive, and another Text frame may already
    // follow in the buffer. Such an iteration must no longer be treated as "quiet". Rework three's
    // `!frame_delivered` also classified a Ping/Pong iteration as quiet, which review found "too
    // eager."
    let mut read_timed_out_this_round = false;
    let read_result = socket.read();
    if read_result.is_ok() {
        keepalive_idle.record_activity(Instant::now());
    }
    match read_result {
        Ok(Message::Text(text)) => {
            if let Some(response) = handle_frame(inner, text.as_ref(), active_k_room.as_ref()) {
                let activates_pairing =
                    response.get("t").and_then(Value::as_str) == Some("pair.ready");
                socket
                    .send(Message::Text(response.to_string().into()))
                    .map_err(|error| format!("frame response write failed: {error}"))?;
                inner.state.frames_sent.fetch_add(1, Ordering::Relaxed);
                if activates_pairing {
                    if active_k_room.is_none() {
                        *active_k_room = lock(&inner.registry).take_staged_pairing_k_room();
                    } else {
                        let _ = lock(&inner.registry).take_staged_pairing_k_room();
                    }
                    if active_k_room.is_some() {
                        inner.state.enable_upstream_gate();
                        request_session_index_snapshot(inner, connection_generation);
                    }
                }
            }
        }
        Ok(Message::Close(_)) => {
            let _ = socket.close(None);
            drain_close(socket);
            return Ok(SocketRead::Exit(ConnectionExit::ClosedByPeer));
        }
        Ok(Message::Ping(_)) => {
            // tungstenite 0.30 queues the matching Pong in read() and flushes it itself on
            // the next read/flush call; sending another Pong here would duplicate the reply.
        }
        Ok(Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {}
        Err(WebSocketError::ConnectionClosed) => {
            return Ok(SocketRead::Exit(ConnectionExit::ClosedByPeer));
        }
        Err(WebSocketError::Io(error)) if is_read_timeout(&error) => {
            read_timed_out_this_round = true;
        }
        Err(error) => return Err(format!("read failed: {error}").into()),
    }
    Ok(SocketRead::Continue(read_timed_out_this_round))
}

pub(super) fn check_registry_resync(
    inner: &Inner,
    read_timed_out_this_round: bool,
    registry_resync_pending: &mut bool,
    registry_resync_pending_since: &mut Option<Instant>,
) -> Result<(), ConnectionFailure> {
    // S1i1 rework three: In the `RefreshDropped` branch, `consume_token_ack` sets the
    // `resync_required` gate. This retains rework two's field and `take` semantics: read and clear
    // the flag, so one rejection realizes at most one convergence intent. Rework two resent
    // `synchronize_registry` in place here and blocked for its `token.sync.ack`. However, while
    // waiting for that ack, `read_registry_sync_ack` above treats every non-ack frame as a protocol
    // violation and immediately disconnects with an error. The relay delivers an online `input`
    // frame directly rather than adding it to pending (`remote-relay/src/room-do.js:576-589` and
    // specification §3, lines 76-77). If such a frame arrives between "resend sync" and "ack
    // returns," the ack waiter consumes it and disconnects with an error. The frame never reaches
    // `handle_frame`, is never recorded locally, and receives no ack, so the instruction sent from
    // the user's phone is silently lost forever. Review classified this as a BLOCKER.
    //
    // Change of approach: do not wait for a second ack on this live connection. Record only an
    // intent to disconnect (`registry_resync_pending`), while this loop iteration continues to
    // process subsequent frames normally through the same `match socket.read()` dispatcher.
    // `handle_frame` has exactly one interpretation throughout; there is no second, parallel state
    // machine that treats unexpected frames as protocol violations. This is precisely the
    // "duplicate the main-loop dispatch logic while waiting for an ack" approach that review
    // explicitly prohibited. Let the existing reconnect path, where `attempt_once` calls
    // `run_connection_request` again, perform that sync. The initial `synchronize_registry` call
    // below follows the established, well-tested connection setup path and sends the DB truth, the
    // new codename, to the relay. That path also handles `prepare_outbox_for_reconnect` immediately
    // after the initial `synchronize_registry`, so it need not be called again here. Return
    // `ConnectionFailure::Other` rather than `Stopped`: this is a retryable disconnect, so
    // `attempt_once`/`connect_loop` uses the existing failure counter and exponential reconnect
    // backoff (`record_failure`/`backoff_delay`). This naturally prevents a hot loop if rejections
    // recur, rather than hammering the relay's protocol-violation budget without limit
    // (`room-do.js:724-726`).
    //
    // S1i1 rework four: Rework three's criterion, "disconnect only when this exact iteration
    // happens not to receive a new application frame (`!frame_delivered`)," has two opposing
    // defects, which review classified as a BLOCKER:
    // (1) Too lazy: `!frame_delivered` requires only that this iteration did not read Text, while
    // `READ_TIMEOUT` (500 ms) bounds only the blocking read itself. If the relay continuously sends
    // Text at intervals below 500 ms, the read never times out, `frame_delivered` stays true, the
    // disconnect never triggers, and convergence has no upper bound.
    // (2) Too eager: `frame_delivered` is also false in a Ping/Pong/Binary iteration, which is then
    // treated as "quiet" and disconnected immediately, potentially stranding an application Text
    // frame that immediately follows and is already buffered.
    // Fix: accept only "this iteration of `socket.read()` genuinely timed out" as the criterion.
    // `read_timed_out_this_round` is set only in the `is_read_timeout` branch. Ping/Pong/Binary does
    // not count as quiet: the peer is still alive and more frames may follow, so normal processing
    // continues through the same `match`. Also add the hard `REGISTRY_RESYNC_DRAIN_DEADLINE` of two
    // seconds, measured from `registry_resync_pending_since` when `registry_resync_pending` is first
    // set. When complete frames keep arriving, even a busy connection must disconnect
    // unconditionally by that deadline; a stream of complete Text frames cannot delay it forever.
    // This bounds both ends when frames are complete: a quiet connection disconnects on its first
    // read timeout (at most `READ_TIMEOUT` = 500 ms), a busy connection disconnects within two
    // seconds, and Ping/Pong no longer causes a premature disconnect.
    //
    // However, this hard deadline can be evaluated only if `socket.read()` returns. `READ_TIMEOUT`
    // (500 ms) bounds one underlying read, not the arrival of one complete message. If the relay
    // continuously feeds an unfinished fragmented message, with bytes arriving less than 500 ms
    // apart but the message never becoming complete, `socket.read()` remains stuck inside
    // tungstenite and never returns. The hard-deadline check after this block, the shutdown and
    // liveness checks below, and `drain_upstream` then cannot be evaluated, so convergence has no
    // finite upper bound. This is not a defect unique to this criterion; it is a property of the
    // existing main-loop structure, which evaluates its criteria only after `socket.read()`
    // returns. Triggering it requires a malicious or broken relay, which could already deny
    // service simply by dropping every frame, so this grants it no new capability. A complete fix
    // requires a read deadline or watchdog at the socket layer and is separate work, recorded in
    // BACKLOG.
    let was_registry_resync_pending = *registry_resync_pending;
    *registry_resync_pending |= lock(&inner.registry).take_resync_required();
    if *registry_resync_pending && !was_registry_resync_pending {
        *registry_resync_pending_since = Some(Instant::now());
    }
    let registry_resync_deadline_elapsed = registry_resync_pending_since
        .is_some_and(|since| since.elapsed() >= REGISTRY_RESYNC_DRAIN_DEADLINE);
    if *registry_resync_pending && (read_timed_out_this_round || registry_resync_deadline_elapsed) {
        return Err(ConnectionFailure::Other(
            "refresh put rejected; reconnecting to resync registry".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn take_shutdown(
    inner: &Inner,
    socket: &mut ConnectionSocket,
) -> Option<ConnectionExit> {
    if inner.shutdown.load(Ordering::Acquire) {
        let _ = socket.close(None);
        drain_close(socket);
        return Some(ConnectionExit::ClosedByPeer);
    }
    None
}

pub(super) fn maintain_keepalive(
    socket: &mut ConnectionSocket,
    state: &GatewayInnerState,
    keepalive_idle: &mut KeepaliveIdle,
    last_observed_frames_sent: &mut u64,
    read_timed_out_this_round: bool,
) -> Result<(), ConnectionFailure> {
    let frames_sent = state.frames_sent.load(Ordering::Relaxed);
    if frames_sent != *last_observed_frames_sent {
        keepalive_idle.record_activity(Instant::now());
        *last_observed_frames_sent = frames_sent;
    }
    if read_timed_out_this_round {
        keepalive_idle
            .send_ping_if_due(Instant::now(), state, |message| socket.send(message))
            .map_err(|error| format!("keepalive ping write failed: {error}"))?;
        *last_observed_frames_sent = state.frames_sent.load(Ordering::Relaxed);
    }
    Ok(())
}

pub(super) fn check_liveness(
    socket: &mut ConnectionSocket,
    inner: &Inner,
    connected_config: &GatewayConfig,
    connected_token: Option<&SecretToken>,
    active_k_room: Option<&Zeroizing<[u8; 32]>>,
    last_liveness_check: &mut Instant,
    connection_loop_started_at: Instant,
) -> Option<ConnectionExit> {
    if last_liveness_check.elapsed() >= inner.liveness_interval {
        *last_liveness_check = Instant::now();
        let (enabled, fresh_config) = current_config(inner);
        let fresh_token = (inner.token_provider)().map(SecretToken::new);
        // Once K_room is held, do not reread the keychain: keychain IPC must not block the WebSocket
        // read thread. Without a key, keep polling for self-recovery. Key revocation is not detected
        // here.
        let pairing_k_room_is_staged = lock(&inner.registry).pairing_k_room_is_staged();
        let fresh_k_room_available = active_k_room.is_some()
            || (!pairing_k_room_is_staged
                && (inner.k_room_provider)(&connected_config.room_id).is_some());
        if evaluate_connection_liveness(
            enabled,
            fresh_config.as_ref(),
            connected_config,
            fresh_token.as_ref(),
            connected_token,
            active_k_room.is_some(),
            fresh_k_room_available,
        ) == ConnectionDecision::Disconnect
        {
            let _ = socket.close(None);
            drain_close(socket);
            return Some(ConnectionExit::ConfigStale {
                connected_for: connection_loop_started_at.elapsed(),
            });
        }
    }
    None
}
