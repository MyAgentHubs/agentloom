use super::*;

/// Builds `{t, request_id, subject, generation, ct, n}` for a valid token refresh success response.
/// Idempotent replay and the response after a rotation ack share this construction point.
pub(crate) fn refresh_ok_json(
    request_id: &str,
    subject: &str,
    generation: i64,
    ct: &str,
    n: &str,
) -> Value {
    serde_json::json!({
        "t": "token.refresh.ok",
        "request_id": request_id,
        "subject": subject,
        "generation": generation,
        "ct": ct,
        "n": n,
    })
}

/// Builds a valid token refresh failure response. Three or more consecutive invalid attempts use
/// `close:true`; a benign single-flight conflict omits `close`. The `close` field appears only when
/// true and is never written as `close:false`.
pub(crate) fn refresh_fail_json(
    request_id: &str,
    subject: &str,
    reason: &str,
    close: bool,
) -> Value {
    let mut frame = serde_json::json!({
        "t": "token.refresh.fail",
        "request_id": request_id,
        "subject": subject,
        "reason": reason,
    });
    if close {
        frame["close"] = Value::Bool(true);
    }
    frame
}

pub(super) fn pair_ready_json(frame: PairReadyFrame) -> Value {
    serde_json::json!({
        "t": "pair.ready",
        "room": frame.room,
        "device_id": frame.device_id,
        "ct": frame.ct,
        "n": frame.n,
    })
}

pub(super) fn pair_accept_json(frame: PairAcceptFrame) -> Value {
    serde_json::json!({
        "t": "pair.accept",
        "room": frame.room,
        "device_id": frame.device_id,
        "k_room_ct": frame.k_room_ct,
        "k_room_n": frame.k_room_n,
        "tokens_ct": frame.tokens_ct,
        "tokens_n": frame.tokens_n,
    })
}
