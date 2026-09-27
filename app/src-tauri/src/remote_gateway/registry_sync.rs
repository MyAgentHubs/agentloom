use super::*;

pub(super) fn registry_snapshot_for_send(
    inner: &Inner,
    room_id: &str,
    now_ms: u64,
    rebase_high_water: Option<i64>,
) -> Result<RegistrySnapshot, ConnectionFailure> {
    // This is the process-wide registry lock. Future grant, rotate, and revoke producers must use
    // this exact lock before touching their DB rows or outbox entries.
    let mut registry = lock(&inner.registry);
    if registry.pairing_entry.is_some() && registry.active_pairing_entry(now_ms).is_none() {
        // Natural expiry only removes the local snapshot/outbox source. It deliberately does not
        // enqueue token.delete: the relay-side pairing grant expires on its own access_expires.
        registry.clear_pairing_entry();
    }
    let (mut snapshot, revoke_generations) = if let Some(high_water) = rebase_high_water {
        let include_pairing = registry.active_pairing_entry(now_ms).is_some();
        let revoke_subjects = registry.pending_revoke_subjects();
        let (snapshot, pairing_generation, revoke_generations) = (inner.registry_rebase_provider)(
            room_id,
            high_water,
            now_ms,
            include_pairing,
            &revoke_subjects,
        )
        .map_err(ConnectionFailure::Other)?;
        if let (Some(entry), Some(generation)) =
            (registry.pairing_entry.as_mut(), pairing_generation)
        {
            entry.generation = generation;
        }
        (snapshot, revoke_generations)
    } else {
        let snapshot = (inner.registry_snapshot_provider)(room_id, now_ms)
            .map_err(ConnectionFailure::Other)?;
        (snapshot, Vec::new())
    };
    if let Some(pairing) = registry.active_pairing_entry(now_ms) {
        snapshot.entries.push(pairing);
    }
    registry.rebase_outbox_entries(&snapshot.entries, &revoke_generations);
    drop(registry);

    if snapshot.entries.len() > MAX_REGISTRY_SYNC_ENTRIES {
        return Err(ConnectionFailure::Other(format!(
            "registry snapshot has {} entries; maximum is {MAX_REGISTRY_SYNC_ENTRIES}",
            snapshot.entries.len()
        )));
    }
    Ok(snapshot)
}

pub(super) fn registry_sync_frame(
    snapshot: &RegistrySnapshot,
) -> Result<String, ConnectionFailure> {
    let frame = serde_json::json!({
        "t": "token.sync",
        "revision": snapshot.revision,
        "entries": snapshot.entries,
    });
    let encoded = serde_json::to_string(&frame)
        .map_err(|error| ConnectionFailure::Other(format!("sync serialize failed: {error}")))?;
    if encoded.len() > MAX_REGISTRY_FRAME_BYTES {
        return Err(ConnectionFailure::Other(format!(
            "registry sync frame is {} bytes; maximum is {MAX_REGISTRY_FRAME_BYTES}",
            encoded.len()
        )));
    }
    Ok(encoded)
}

pub(super) fn read_registry_sync_ack(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    inner: &Inner,
    expected_revision: i64,
) -> Result<i64, ConnectionFailure> {
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            let _ = socket.close(None);
            drain_close(socket);
            return Err(ConnectionFailure::Other(
                "gateway shutdown while waiting for registry sync ack".to_owned(),
            ));
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let frame: Value = serde_json::from_str(text.as_ref()).map_err(|_| {
                    ConnectionFailure::Other(
                        "invalid JSON received before registry sync ack".to_owned(),
                    )
                })?;
                if frame.get("t").and_then(Value::as_str) != Some("token.sync.ack") {
                    // A mixed-version rollout with an upgraded desktop and an older relay makes
                    // the relay answer with `{t:"error", reason:"unknown_frame_type"}` instead of
                    // the expected ack — the old bare string gave the operator no way to tell
                    // that apart from any other unexpected-frame cause, so every reconnect in a
                    // mixed fleet failed with zero diagnostic signal. Frame type and reason are
                    // protocol metadata, not credentials — safe to surface (log hygiene: no
                    // token/hash material here) — reuse the existing hash-scrubbing/length-cap
                    // sanitizer so a hostile or buggy relay can't smuggle oversized/hash-shaped
                    // text into the UI-visible error string via either field.
                    let frame_type =
                        registry_ack_reason_for_log(frame.get("t").and_then(Value::as_str));
                    let reason = frame.get("reason").and_then(Value::as_str);
                    let detail = match reason {
                        Some(reason) => format!(
                            "t={frame_type}, reason={}",
                            registry_ack_reason_for_log(Some(reason))
                        ),
                        None => format!("t={frame_type}"),
                    };
                    return Err(ConnectionFailure::Other(format!(
                        "relay sent a frame before registry sync ack ({detail})"
                    )));
                }
                let revision = frame
                    .get("revision")
                    .and_then(Value::as_i64)
                    .filter(|revision| *revision > 0)
                    .ok_or_else(|| {
                        ConnectionFailure::Other("sync ack revision is invalid".to_owned())
                    })?;
                if revision != expected_revision {
                    return Err(ConnectionFailure::Other(format!(
                        "sync ack revision mismatch: expected {expected_revision}, received {revision}"
                    )));
                }
                return frame
                    .get("relay_high_water")
                    .and_then(Value::as_i64)
                    .filter(|high_water| *high_water >= 0)
                    .ok_or_else(|| {
                        ConnectionFailure::Other("sync ack relay_high_water is invalid".to_owned())
                    });
            }
            Ok(Message::Close(_)) => {
                return Err(ConnectionFailure::Other(
                    "relay closed before registry sync ack".to_owned(),
                ))
            }
            Ok(Message::Ping(_)) => {}
            Ok(Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {}
            Err(WebSocketError::Io(error)) if is_read_timeout(&error) => {}
            Err(WebSocketError::ConnectionClosed) => {
                return Err(ConnectionFailure::Other(
                    "relay closed before registry sync ack".to_owned(),
                ))
            }
            Err(error) => {
                return Err(ConnectionFailure::Other(format!(
                    "sync ack read failed: {error}"
                )))
            }
        }
    }
}

pub(super) fn synchronize_registry(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    inner: &Inner,
    room_id: &str,
) -> Result<(), ConnectionFailure> {
    let mut rebase_high_water = None;
    let mut rebases = 0_u8;
    loop {
        let snapshot =
            registry_snapshot_for_send(inner, room_id, now_unix_ms(), rebase_high_water)?;
        let frame = registry_sync_frame(&snapshot)?;
        socket
            .send(Message::Text(frame.into()))
            .map_err(|error| ConnectionFailure::Other(format!("sync write failed: {error}")))?;
        inner.state.frames_sent.fetch_add(1, Ordering::Relaxed);

        let relay_high_water = read_registry_sync_ack(socket, inner, snapshot.revision)?;
        // Enforce a fail-closed upper bound. The protocol validation in
        // `read_registry_sync_ack` checks only that `relay_high_water >= 0`, with no upper bound.
        // If a faulty or partially trusted relay returns a value near `i64::MAX`, such as
        // 9223372036854775806, absorbing it would persist it in
        // `remote_registry_counter.next_generation`. The current
        // `bump_registry_counter_to_in_transaction` call would not overflow because
        // `floor.checked_add(1)` still has room, but the next call to
        // `next_registry_generation_in_transaction` for pairing, revocation, or refresh rotation
        // would overflow at `generation.checked_add(1)`, permanently producing
        // `IntegralValueOutOfRange`. Pairing, device revocation, and refresh would all fail, and
        // switching back to an honest relay would not repair the persisted desktop DB damage.
        // Capping only at the JSON safe integer limit is insufficient because the relay requires
        // `Number.isSafeInteger(generation)`; every put beyond 2^53 would be rejected. Validate
        // against the local counter in `snapshot.revision` plus a reasonable span instead. Stop
        // immediately on overflow and never absorb it into the counter or persist it. This is the
        // intentional exception to always raising the desktop counter above
        // `relay_high_water` after an acknowledgement. Dropping this connection allows the next
        // reconnect to recover safely; do not change this branch to absorb out-of-range values.
        if relay_high_water
            > snapshot
                .revision
                .saturating_add(REGISTRY_HIGH_WATER_MAX_SPAN)
        {
            return Err(ConnectionFailure::Stopped(StopReason::new(
                REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON,
                REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_ERROR,
            )));
        }
        // Fail closed because the protocol has no lower-bound validation beyond
        // `relay_high_water >= 0`. An out-of-protocol relay may return a value below this sync's
        // revision, which must not be used directly as the floor. An honest relay always has
        // `relay_high_water >= revision`, computed as
        // `max(tableHighWater, floor, revision)`. Take the maximum with the caller's existing
        // `snapshot.revision`; otherwise, when the local counter equals the revision, absorption
        // can produce a new generation equal to that revision and enter the relay's same-
        // generation fingerprint comparison branch, causing permanent rejection.
        absorb_registry_high_water_and_rearm_revokes(
            inner,
            room_id,
            relay_high_water.max(snapshot.revision),
        )?;
        if relay_high_water <= snapshot.revision {
            return Ok(());
        }
        if rebases >= MAX_REGISTRY_REBASES {
            return Err(ConnectionFailure::Stopped(StopReason::new(
                REGISTRY_REBASE_LIMIT_STOP_REASON,
                REGISTRY_REBASE_LIMIT_STOP_ERROR,
            )));
        }
        rebases += 1;
        rebase_high_water = Some(relay_high_water);
    }
}

/// Immediately after a sync acknowledgement, after absorbing the counter and before
/// `drain_registry_outbox` drains the outbox, assigns new generations to revoke (`token.delete`)
/// items that remain pending, including rejected items. The new generations come from
/// `registry_high_water_provider` after `relay_high_water` has been absorbed into the desktop
/// counter, so they are strictly greater than both this sync's revision and the relay's reported
/// high-water mark. This must happen whether or not the current iteration enters the later rebase
/// loop. A disconnected revocation may have its old generation rejected by the first sync after
/// reconnection even when that sync is otherwise accepted and never reaches the rebase branch.
/// When no revocation remains pending, only the counter is absorbed and no extra generation is
/// allocated.
///
/// At the `synchronize_registry` call site, `relay_high_water` is fail-closed by taking its maximum
/// with `snapshot.revision`. `read_registry_sync_ack` validates only
/// `relay_high_water >= 0`, so an out-of-protocol value below the current sync revision must not
/// be trusted directly. Otherwise, when the local counter equals the revision, the new generation
/// can collapse to the revision and enter the relay's same-generation fingerprint comparison
/// branch, causing permanent rejection.
pub(super) fn absorb_registry_high_water_and_rearm_revokes(
    inner: &Inner,
    room_id: &str,
    relay_high_water: i64,
) -> Result<(), ConnectionFailure> {
    let mut registry = lock(&inner.registry);
    let revoke_subjects = registry.pending_revoke_subjects();
    let revoke_generations =
        (inner.registry_high_water_provider)(room_id, relay_high_water, &revoke_subjects)
            .map_err(ConnectionFailure::Other)?;
    registry.rearm_revoke_entries(&revoke_generations);
    Ok(())
}

pub(super) fn drain_registry_outbox(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    inner: &Inner,
) -> Result<(), String> {
    drain_registry_outbox_with(inner, |message| {
        socket
            .send(message)
            .map_err(|error| format!("registry outbox write failed: {error}"))
    })
}

pub(super) fn drain_registry_outbox_with<E>(
    inner: &Inner,
    mut send: impl FnMut(Message) -> Result<(), E>,
) -> Result<(), E> {
    let mut registry = lock(&inner.registry);
    for item in registry
        .outbox
        .iter_mut()
        .filter(|item| !item.acked && !item.rejected && item.last_sent_at.is_none())
    {
        item.attempts = item.attempts.saturating_add(1);
        item.last_sent_at = Some(now_unix_ms());
        if let Err(error) = send(Message::Text(item.frame.to_string().into())) {
            inner.registry_publish_wake.store(true, Ordering::Release);
            return Err(error);
        }
        inner.state.frames_sent.fetch_add(1, Ordering::Relaxed);
    }
    // Clear while holding the registry lock. A producer cannot enqueue between the completed
    // drain and this store; a producer that runs afterwards will publish a fresh wake.
    inner.registry_publish_wake.store(false, Ordering::Release);
    Ok(())
}
