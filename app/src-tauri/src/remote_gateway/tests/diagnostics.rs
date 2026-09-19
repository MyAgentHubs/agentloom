#![cfg(test)]

use super::*;
#[test]
fn redacts_query_tokens_and_unknown_token_shapes() {
    // S1ja §9.7: build_ws_url no longer accepts a token to embed in the URL (the whole
    // point of the retirement), so this test now synthesizes the shape a legacy-relay
    // error message used to have by hand — `redact()` itself is still a real, generic
    // string scrubber (F4 extends it to headers in this same batch) and stays exercised.
    let token = "SENTINEL-TOKEN-ABC123";
    let room = "0123456789abcdef0123456789abcdef";
    let url = format!("wss://relay.example.com/room/{room}?token={token}");
    let error = format!("connect failed: URL error: Unable to connect to {url}");
    let redacted = redact(&error, Some(token));

    assert!(!redacted.contains(token));
    assert!(!redacted.contains(&format!("token={token}")));
    assert!(redacted.contains("token=***"));

    let unknown_shape = format!("relay rejected capability {token} during handshake");
    assert_eq!(
        redact(&unknown_shape, Some(token)),
        "relay rejected capability *** during handshake"
    );

    assert_eq!(
        redact("token=first&reason=retry&token=second&done=true", None),
        "token=***&reason=retry&token=***&done=true"
    );
}

#[test]
fn redacts_authorization_bearer_header_values() {
    // S1ja F4: once the query-string `?token=` backdoor is retired, `Authorization:
    // Bearer <credential>` is the desktop's only credential channel — a Debug-formatted
    // request/headers dump ending up in a panic or connection-failure string must not
    // leak the 64-hex desktop credential.
    let credential = "a1".repeat(32);
    let dump = format!(
        r#"handshake failed: request Request {{ headers: {{"authorization": "Bearer {credential}"}} }}"#
    );
    let redacted = redact(&dump, None);
    assert!(!redacted.contains(&credential));
    assert!(redacted.contains("Bearer ***"));

    // Raw HTTP header line shape too, not just the Debug-quoted one.
    let raw_line = format!("Authorization: Bearer {credential}\r\nHost: relay.example.com");
    let redacted_raw = redact(&raw_line, None);
    assert!(!redacted_raw.contains(&credential));
    assert_eq!(
        redacted_raw,
        "Authorization: Bearer ***\r\nHost: relay.example.com"
    );
}

#[test]
fn redacts_sec_websocket_protocol_token_dot_offers() {
    // S1ja F4: `Sec-WebSocket-Protocol: agentloom-rc-v1, token.<hex>` (§9.1 remote scope
    // subprotocol) must be scrubbed the same way — only the `token.<hex>` value is
    // sensitive, the `agentloom-rc-v1` version offer alongside it is not and must survive.
    let capability_token = "d".repeat(64);
    let dump = format!(
        "upgrade rejected: Sec-WebSocket-Protocol: agentloom-rc-v1, token.{capability_token}"
    );
    let redacted = redact(&dump, None);
    assert!(!redacted.contains(&capability_token));
    assert_eq!(
        redacted,
        "upgrade rejected: Sec-WebSocket-Protocol: agentloom-rc-v1, token.***"
    );
}

#[test]
fn redact_leaves_short_token_dot_suffixed_reasons_alone() {
    // R3 (双路审): `reason=token.refresh_failed` isn't a `token.<hex64>` credential —
    // "refresh_failed" isn't even hex-shaped ('r' isn't a hex digit) — but the
    // un-floored scrubber still matched on the bare "token." marker and spliced in a
    // spurious "***" (0-length match still triggered the unconditional `push_str("***")`
    // in the pre-R3 implementation), corrupting a legitimate diagnostic reason string.
    // The hex-length floor (MIN_SCRUBBED_HEX_LEN) fixes this: a run below the floor is
    // left untouched instead of being replaced.
    let message = "relay rejected refresh: reason=token.refresh_failed";
    assert_eq!(
        redact(message, None),
        message,
        "token.refresh_failed must survive redact() byte-for-byte — it was never a credential"
    );
}

#[test]
fn registry_ack_diagnostic_reason_never_logs_hash_like_material() {
    assert_eq!(
        registry_ack_reason_for_log(Some("generation_conflict")),
        "generation_conflict"
    );
    assert_eq!(
        registry_ack_reason_for_log(Some(&"ab".repeat(32))),
        "redacted"
    );
    assert_eq!(
        registry_ack_reason_for_log(Some("bad reason with spaces")),
        "redacted"
    );
}

#[test]
fn secret_token_debug_and_display_never_expose_the_value() {
    let token = SecretToken::new("SENTINEL-TOKEN-ABC123".to_owned());
    assert_eq!(format!("{token:?}"), "***");
    assert_eq!(format!("{token}"), "***");
    assert_eq!(token.expose(), "SENTINEL-TOKEN-ABC123");
}

#[test]
fn redact_panic_message_strips_active_token_from_panic_text() {
    let message = "settings callback failed for token=SENTINEL-TOKEN-ABC123";
    let redacted = redact_panic_message(message, Some("SENTINEL-TOKEN-ABC123"));
    assert!(!redacted.contains("SENTINEL-TOKEN-ABC123"));
    assert!(redacted.contains("token=***"));
}

#[test]
fn leaves_errors_without_tokens_unchanged() {
    let error = "read failed: connection reset by peer";
    assert_eq!(redact(error, Some("unused-secret")), error);
    assert_eq!(redact(error, None), error);
}

#[test]
fn connection_failure_is_redacted_before_reaching_status() {
    // S1ja §9.7: build_ws_url structurally cannot embed a token anymore, so this test
    // hand-builds the legacy shape to keep exercising record_failure's redaction call.
    let token = "SENTINEL-TOKEN-ABC123";
    let room = "0123456789abcdef0123456789abcdef";
    let inner = test_inner(|_| None, || None);
    let url = format!("wss://relay.example.com/room/{room}?token={token}");
    let error = format!("connect failed: URL error: Unable to connect to {url}");
    let mut failed_attempts = 0;

    assert_eq!(
        record_failure(
            &inner,
            FailureKind::Connection,
            error,
            Some(token),
            &mut failed_attempts,
        ),
        0
    );

    let status = lock(&inner.state.status).clone();
    assert_eq!(status.state, GatewayState::Backoff);
    let last_error = status
        .last_error
        .expect("connection failure should be recorded");
    assert!(!last_error.contains(token));
    assert!(last_error.contains("token=***"));
    assert_eq!(inner.state.connection_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn settings_reader_panic_is_caught_and_recovered_as_backoff() {
    let inner = test_inner(
        |_| {
            panic!(
                "settings callback failed for token={}",
                SecretToken::new("SENTINEL-TOKEN-ABC123".to_owned())
            )
        },
        || None,
    );

    assert_panicking_attempt_enters_backoff(&inner);
    let last_error = lock(&inner.state.status)
        .last_error
        .clone()
        .expect("panic should be recorded");
    assert!(!last_error.contains("SENTINEL-TOKEN-ABC123"));
    assert!(last_error.contains("token=***"));
}

#[test]
fn token_provider_panic_is_caught_and_recovered_as_backoff() {
    let room = "0123456789abcdef0123456789abcdef".to_owned();
    let inner = test_inner_with_token_provider_and_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        },
        || panic!("token provider failed"),
        move |_project_id| Ok(room.clone()),
    );

    assert_panicking_attempt_enters_backoff(&inner);
}

fn assert_panicking_attempt_enters_backoff(inner: &Arc<Inner>) {
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let result = catch_unwind(AssertUnwindSafe(|| {
        attempt_once(inner, &upstream_rx, &milestone_rx)
    }));
    let payload = result.expect_err("callback panic must be caught around the whole attempt");
    let mut failed_attempts = 0;
    assert_eq!(
        record_failure(
            inner,
            FailureKind::Panic,
            panic_message(payload),
            None,
            &mut failed_attempts,
        ),
        0
    );

    assert_eq!(inner.state.panics.load(Ordering::Relaxed), 1);
    assert_eq!(inner.state.connection_failures.load(Ordering::Relaxed), 1);
    assert_eq!(lock(&inner.state.status).state, GatewayState::Backoff);
    assert_eq!(failed_attempts, 1);
}
