use super::*;

pub(super) fn handle_command_envelope(
    inner: &Inner,
    value: &Value,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Option<Value> {
    let state = &inner.state;
    let Some(command_id) = value
        .get("command_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(&command_id, AckOutcome::Failed))
    };
    if command_id.contains('|') || command_id.len() > COMMAND_ID_MAX_LEN {
        return failed();
    }

    let Some(v) = value
        .get("v")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
    else {
        return failed();
    };
    let Some(room) = value.get("room").and_then(Value::as_str) else {
        return failed();
    };
    let Some(epoch) = value.get("epoch").and_then(Value::as_u64) else {
        return failed();
    };
    let Some(kind) = value.get("kind").and_then(Value::as_str) else {
        return failed();
    };
    let session = match value.get("session") {
        Some(Value::String(session)) => Some(session.clone()),
        Some(Value::Null) => None,
        _ => return failed(),
    };
    if let Some(session_value) = session.as_deref() {
        if session_value.contains('|') {
            return failed();
        }
    }
    let Some(ct) = value.get("ct").and_then(Value::as_str) else {
        return failed();
    };
    let Some(n) = value.get("n").and_then(Value::as_str) else {
        return failed();
    };
    let Some(k_room) = k_room else {
        // When K_room is unavailable because the keychain read failed or the device is not paired,
        // do not return an ack. Returning failed would make the relay delete this pending row and
        // permanently destroy a message sent while the user was offline. Without an ack, the relay
        // retains the row until the 30-minute TTL or retransmits it after the desktop reconnects.
        // Do not increment bad_frames because this is not a bad frame; the desktop simply cannot
        // obtain the key at this moment.
        return None;
    };
    let meta = EnvelopeMeta {
        v,
        room: room.to_owned(),
        epoch,
        kind: kind.to_owned(),
        session,
        command_id: Some(command_id.clone()),
    };
    let Ok(plaintext) = crate::remote_crypto::open(k_room, &meta, ct, n) else {
        return failed();
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&plaintext) else {
        return failed();
    };

    match (kind, payload.get("t").and_then(Value::as_str)) {
        ("input", Some("input.send")) => {
            command_envelope::handle_input_send(inner, &payload, &command_id)
        }
        ("control", Some("control.stop")) => {
            command_envelope::handle_control_stop(inner, &payload, &command_id)
        }
        ("input", Some("input.answer")) => {
            command_envelope::handle_input_answer(inner, &payload, &command_id)
        }
        ("control", Some("control.history")) => {
            command_envelope::handle_control_history(inner, &payload, &command_id)
        }
        ("control", Some("control.snapshot")) => {
            command_envelope::handle_control_snapshot(inner, &payload, &command_id)
        }
        ("control", Some("msg.fetch")) => {
            command_envelope::handle_msg_fetch_kind(inner, &payload, &command_id)
        }
        _ => failed(),
    }
}

pub(super) fn is_control_stop_stale(issued_at_ms: u64, expires_at_ms: u64, now_ms: u64) -> bool {
    now_ms >= expires_at_ms.saturating_add(CONTROL_STOP_SKEW_MS)
        || issued_at_ms >= now_ms.saturating_add(CONTROL_STOP_SKEW_MS)
}

pub(super) fn input_ack_json(command_id: &str, outcome: AckOutcome) -> Value {
    input_ack_json_with_reason(command_id, outcome, None)
}

pub(super) fn input_ack_json_with_reason(
    command_id: &str,
    outcome: AckOutcome,
    reason: Option<&str>,
) -> Value {
    let outcome = match outcome {
        AckOutcome::Ok => "ok",
        AckOutcome::Queued => "queued",
        AckOutcome::Failed => "failed",
    };
    let mut ack = serde_json::json!({
        "t": "input.ack",
        "command_id": command_id,
        "outcome": outcome,
    });
    if let Some(reason) = reason {
        ack["reason"] = Value::String(reason.to_owned());
    }
    ack
}

pub(super) fn parse_pair_hello(value: &Value) -> Option<PairHelloFrame> {
    let room = value.get("room")?.as_str()?.to_owned();
    let remote_pub: [u8; 32] = STANDARD
        .decode(value.get("remote_pub")?.as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    Some(PairHelloFrame {
        room,
        remote_pub,
        token_ct: value.get("token_ct")?.as_str()?.to_owned(),
        token_n: value.get("token_n")?.as_str()?.to_owned(),
        origin_connection_id: value.get("origin_connection_id")?.as_str()?.to_owned(),
    })
}

pub(super) fn parse_pair_done(value: &Value) -> Option<PairDoneFrame> {
    Some(PairDoneFrame {
        room: value.get("room")?.as_str()?.to_owned(),
        device_id: value.get("device_id")?.as_str()?.to_owned(),
        confirm_ct: value
            .get("confirm_ct")
            .and_then(Value::as_str)
            .map(str::to_owned),
        confirm_n: value
            .get("confirm_n")
            .and_then(Value::as_str)
            .map(str::to_owned),
        origin_connection_id: value.get("origin_connection_id")?.as_str()?.to_owned(),
    })
}

/// Parses a relay-stamped `token.refresh.forward`; missing or incorrectly typed fields follow the
/// gateway's existing bad-frame path because the caller short-circuits on `None` in `handle_frame`
/// without entering the refresh handler.
pub(super) fn parse_refresh_forward(value: &Value) -> Option<RefreshForwardFrame> {
    Some(RefreshForwardFrame {
        request_id: value.get("request_id")?.as_str()?.to_owned(),
        subject: value.get("subject")?.as_str()?.to_owned(),
        request_generation: value.get("request_generation")?.as_i64()?,
        ct: value.get("ct")?.as_str()?.to_owned(),
        n: value.get("n")?.as_str()?.to_owned(),
    })
}
