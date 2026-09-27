use super::*;

/// The sink callback contract, documented by `EventTransport::add_sink`, permits only nonblocking
/// enqueueing and forbids `send()`, network I/O, internal EventTransport locks, and panics. Beyond
/// atomic reads and bounded `try_send`, the extraction step added here acquires only the private,
/// short-lived Mutex justified separately above, with no lock-order crossover, and performs only
/// bounded in-memory work, so it still has no blocking path.
pub(crate) fn install_event_sink(transport: &crate::event_transport::EventTransport) {
    transport.add_sink(move |payload: crate::event_transport::BatchPayload| {
        let Some(inner) = GATEWAY.get() else {
            return;
        };
        enqueue_batch_payload_for_upstream(
            &inner.state,
            &inner.upstream_tx,
            &inner.milestone_tx,
            payload,
        );
    });
}

/// Production wiring hook for the L1 aggregator. `GatewayInnerState` is module-private and
/// `GATEWAY` is a module-private static, so lib.rs cannot obtain `&GatewayInnerState` and cannot
/// call `configure_activity_summary_writer` directly. This `pub(crate)` wrapper is the sole
/// cross-module entry point, following the same convention as `install_event_sink`. `GATEWAY.get()`
/// becomes available only after `setup()`; immediately after `remote_gateway::setup(...)`, the
/// caller in lib.rs invokes this function with a writer that captures the real DB connection.
pub(crate) fn install_activity_summary_writer(writer: ActivitySummaryWriter) {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    configure_activity_summary_writer(&inner.state, writer);
}

pub(super) fn drain_close(socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>) {
    for _ in 0..CLOSE_DRAIN_ATTEMPTS {
        match socket.read() {
            Err(WebSocketError::ConnectionClosed) => return,
            Err(WebSocketError::Io(error)) if is_read_timeout(&error) => continue,
            Err(_) => return,
            Ok(_) => continue,
        }
    }
}

pub(super) fn set_read_timeout(
    stream: &MaybeTlsStream<TcpStream>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported TLS stream for read timeout",
        )),
    }
}

pub(super) fn set_write_timeout(
    stream: &MaybeTlsStream<TcpStream>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_write_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_write_timeout(timeout),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported TLS stream for write timeout",
        )),
    }
}

pub(super) fn is_read_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

pub(super) fn handle_frame(
    inner: &Inner,
    raw: &str,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Option<Value> {
    let state = &inner.state;
    state.frames_seen.fetch_add(1, Ordering::Relaxed);
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        return None;
    };

    if matches!(
        value.get("kind").and_then(Value::as_str),
        Some("input" | "control")
    ) {
        return handle_command_envelope(inner, &value, k_room);
    }

    match value.get("t").and_then(Value::as_str) {
        Some("pair.hello") => {
            let Some(frame) = parse_pair_hello(&value) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(accept) = (inner.pair_hello_handler)(frame) else {
                // Authentication failures and pair.hello frames received outside Waiting are
                // protocol-invalid for the current desktop state, so reuse bad_frames.
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            lock(&inner.registry).stage_pairing_k_room(accept.k_room.clone());
            Some(pair_accept_json(accept))
        }
        Some("pair.done") => {
            let Some(frame) = parse_pair_done(&value) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            match (inner.pair_done_handler)(frame) {
                PairDoneAction::Rejected => {
                    state.bad_frames.fetch_add(1, Ordering::Relaxed);
                    None
                }
                PairDoneAction::Accepted { .. } => None,
                PairDoneAction::Ready(ready) => Some(pair_ready_json(ready)),
            }
        }
        Some("token.ack") => {
            let Some(subject) = value.get("subject").and_then(Value::as_str) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(generation) = value
                .get("generation")
                .and_then(Value::as_i64)
                .filter(|generation| *generation > 0)
            else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(result) = value
                .get("result")
                .and_then(Value::as_str)
                .filter(|result| matches!(*result, "ok" | "idempotent" | "rejected"))
            else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let action = lock(&inner.registry).consume_token_ack(subject, generation, result);
            match action {
                TokenAckAction::PairReady(ready) => Some(pair_ready_json(ready)),
                // The rotating token.put has received its acknowledgement, so the receipt may now
                // be sent, preserving the fixed put -> acknowledgement -> receipt order.
                TokenAckAction::RefreshOk(refresh_ok) => Some(refresh_ok_json(
                    &refresh_ok.request_id,
                    &refresh_ok.subject,
                    refresh_ok.generation,
                    &refresh_ok.ct,
                    &refresh_ok.n,
                )),
                TokenAckAction::Rejected => {
                    let reason =
                        registry_ack_reason_for_log(value.get("reason").and_then(Value::as_str));
                    eprintln!(
                        "remote registry token.ack rejected: subject={subject}, generation={generation}, reason={reason}"
                    );
                    None
                }
                // The put was rejected while a refresh receipt was pending. Recover immediately
                // with a fail frame and no close. This is benign because the problem is between
                // the desktop and relay, not an invalid device request. Do not pass through
                // refresh_fail_reply/record_refresh_invalid or increment the consecutive-invalid
                // counter.
                TokenAckAction::RefreshDropped {
                    request_id,
                    subject,
                } => Some(refresh_fail_json(
                    &request_id,
                    &subject,
                    "put_rejected",
                    false,
                )),
                TokenAckAction::Ignored | TokenAckAction::Consumed => None,
            }
        }
        Some("token.refresh.forward") => {
            let Some(frame) = parse_refresh_forward(&value) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            match (inner.refresh_handler)(frame) {
                RefreshOutcome::Reply(value) => Some(value),
                RefreshOutcome::Pending => None,
            }
        }
        Some("replay.head") => {
            let Some(epoch) = value.get("epoch").and_then(Value::as_u64) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(head_seq) = value.get("headSeq").and_then(Value::as_u64) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            state.epoch.store(epoch, Ordering::Release);
            state.head_seq.store(head_seq, Ordering::Release);
            None
        }
        Some("error") if value.get("reason").and_then(Value::as_str) == Some("stale_epoch") => {
            let Some(epoch) = value.get("currentEpoch").and_then(Value::as_u64) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            state.epoch.store(epoch, Ordering::Release);
            None
        }
        _ => None,
    }
}
