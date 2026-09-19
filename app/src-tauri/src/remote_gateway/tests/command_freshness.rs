#![cfg(test)]

use super::*;
#[test]
fn control_stop_uses_authenticated_freshness_and_ignores_envelope_ts() {
    let k_room = Zeroizing::new([8_u8; 32]);
    let calls = Arc::new(AtomicU64::new(0));
    let replay_calls = Arc::new(AtomicU64::new(0));
    let received = Arc::new(Mutex::new(None));
    let calls_for_handler = Arc::clone(&calls);
    let replay_calls_for_handler = Arc::clone(&replay_calls);
    let received_for_handler = Arc::clone(&received);
    let inner = test_inner_with_input_control_replay_handlers(
        |_| Some(AckOutcome::Failed),
        move |_, _| {
            replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            true
        },
        move |frame| {
            calls_for_handler.fetch_add(1, Ordering::Relaxed);
            *received_for_handler.lock().unwrap() = Some((frame.session, frame.command_id));
            AckOutcome::Ok
        },
    );
    inner.state.epoch.store(7, Ordering::Release);
    let now = now_unix_ms();
    let mut expired = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-2",
        "cmd-stop-expired",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-2",
            "issued_at_ms": now.saturating_sub(160_000),
            "expires_at_ms": now.saturating_sub(130_000),
        }),
    );
    expired["ts"] = Value::from(now.saturating_sub(300_000));
    let expired_response = handle_frame(&inner, &expired.to_string(), Some(&k_room)).unwrap();
    assert_eq!(
        expired_response,
        serde_json::json!({
            "t": "input.ack",
            "command_id": "cmd-stop-expired",
            "outcome": "failed",
        })
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(replay_calls.load(Ordering::Relaxed), 0);

    let original_ct = expired["ct"].clone();
    let original_nonce = expired["n"].clone();
    expired["ts"] = Value::from(now_unix_ms());
    assert_eq!(expired["ct"], original_ct);
    assert_eq!(expired["n"], original_nonce);
    let rewritten_ts_response = handle_frame(&inner, &expired.to_string(), Some(&k_room)).unwrap();
    assert_eq!(
        rewritten_ts_response,
        serde_json::json!({
            "t": "input.ack",
            "command_id": "cmd-stop-expired",
            "outcome": "failed",
        })
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(replay_calls.load(Ordering::Relaxed), 0);

    let fresh = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-2",
        "cmd-stop-fresh",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-2",
            "issued_at_ms": now,
            "expires_at_ms": now.saturating_add(30_000),
        }),
    );
    let fresh_response = handle_frame(&inner, &fresh.to_string(), Some(&k_room)).unwrap();
    assert_eq!(
        fresh_response,
        serde_json::json!({
            "t": "input.ack",
            "command_id": "cmd-stop-fresh",
            "outcome": "ok",
        })
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        received.lock().unwrap().as_ref(),
        Some(&("s-2".to_owned(), "cmd-stop-fresh".to_owned()))
    );
}

#[test]
fn legacy_ttl_counterfactual_proves_rewritten_envelope_ts_was_accepted() {
    fn legacy_is_command_ttl_expired(ts_ms: u64, ttl_s: u64, now_ms: u64) -> bool {
        now_ms > ts_ms.saturating_add(ttl_s.saturating_mul(1000))
    }

    let now_ms = 1_000_000_u64;
    let captured_ts_ms = now_ms - 60_000;
    let ttl_s = 30;
    assert!(legacy_is_command_ttl_expired(captured_ts_ms, ttl_s, now_ms));
    assert!(!legacy_is_command_ttl_expired(now_ms, ttl_s, now_ms));
}

#[test]
fn control_stop_replay_ledger_rejects_resealed_duplicate_command_id() {
    let k_room = Zeroizing::new([18_u8; 32]);
    let calls = Arc::new(AtomicU64::new(0));
    let calls_for_handler = Arc::clone(&calls);
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let seen_for_handler = Arc::clone(&seen);
    let inner = test_inner_with_input_control_replay_handlers(
        |_| Some(AckOutcome::Failed),
        move |session, command_id| {
            let mut seen = seen_for_handler.lock().unwrap();
            let key = (session.to_owned(), command_id.to_owned());
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        },
        move |_| {
            calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    inner.state.epoch.store(7, Ordering::Release);
    let now = now_unix_ms();
    let first = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-dup",
        "cmd-dup-1",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-dup",
            "issued_at_ms": now,
            "expires_at_ms": now.saturating_add(30_000),
        }),
    );
    let second = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-dup",
        "cmd-dup-1",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-dup",
            "issued_at_ms": now.saturating_add(1),
            "expires_at_ms": now.saturating_add(30_001),
        }),
    );

    let first_response = handle_frame(&inner, &first.to_string(), Some(&k_room)).unwrap();
    assert_eq!(first_response["outcome"], "ok");
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    let second_response = handle_frame(&inner, &second.to_string(), Some(&k_room)).unwrap();
    assert_eq!(second_response["outcome"], "failed");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn control_stop_rejects_reversed_authenticated_times_before_replay_claim() {
    let k_room = Zeroizing::new([21_u8; 32]);
    let replay_calls = Arc::new(AtomicU64::new(0));
    let stop_calls = Arc::new(AtomicU64::new(0));
    let replay_calls_for_handler = Arc::clone(&replay_calls);
    let stop_calls_for_handler = Arc::clone(&stop_calls);
    let inner = test_inner_with_input_control_replay_handlers(
        |_| Some(AckOutcome::Failed),
        move |_, _| {
            replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            true
        },
        move |_| {
            stop_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    inner.state.epoch.store(7, Ordering::Release);
    let now = now_unix_ms();
    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-reversed",
        "cmd-reversed-time",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-reversed",
            "issued_at_ms": now.saturating_add(5_000),
            "expires_at_ms": now,
        }),
    );

    let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["outcome"], "failed");
    assert_eq!(replay_calls.load(Ordering::Relaxed), 0);
    assert_eq!(stop_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn control_stop_enforces_max_lifetime_at_thirty_seconds() {
    let k_room = Zeroizing::new([22_u8; 32]);
    let replay_calls = Arc::new(AtomicU64::new(0));
    let stop_calls = Arc::new(AtomicU64::new(0));
    let replay_calls_for_handler = Arc::clone(&replay_calls);
    let stop_calls_for_handler = Arc::clone(&stop_calls);
    let inner = test_inner_with_input_control_replay_handlers(
        |_| Some(AckOutcome::Failed),
        move |_, _| {
            replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            true
        },
        move |_| {
            stop_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    inner.state.epoch.store(7, Ordering::Release);
    let now = now_unix_ms();
    let at_limit = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-lifetime",
        "cmd-lifetime-30000",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-lifetime",
            "issued_at_ms": now,
            "expires_at_ms": now.saturating_add(30_000),
        }),
    );
    let over_limit = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-lifetime",
        "cmd-lifetime-30001",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-lifetime",
            "issued_at_ms": now,
            "expires_at_ms": now.saturating_add(30_001),
        }),
    );

    let at_limit_response = handle_frame(&inner, &at_limit.to_string(), Some(&k_room)).unwrap();
    assert_eq!(at_limit_response["outcome"], "ok");
    assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
    assert_eq!(stop_calls.load(Ordering::Relaxed), 1);

    let over_limit_response = handle_frame(&inner, &over_limit.to_string(), Some(&k_room)).unwrap();
    assert_eq!(over_limit_response["outcome"], "failed");
    assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
    assert_eq!(stop_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn control_stop_stale_includes_clock_skew_boundaries() {
    let issued_at_ms = 1_990_000;
    let expires_at_ms = 2_000_000;

    assert!(!is_control_stop_stale(
        issued_at_ms,
        expires_at_ms,
        2_119_999
    ));
    assert!(is_control_stop_stale(
        issued_at_ms,
        expires_at_ms,
        2_120_000
    ));

    let now_ms = 2_000_000;
    assert!(!is_control_stop_stale(2_119_999, 2_129_999, now_ms));
    assert!(is_control_stop_stale(2_120_000, 2_130_000, now_ms));
}

#[test]
fn control_stop_accepts_mismatched_epoch_when_aead_time_window_and_ledger_all_pass() {
    let k_room = Zeroizing::new([23_u8; 32]);
    let replay_calls = Arc::new(AtomicU64::new(0));
    let stop_calls = Arc::new(AtomicU64::new(0));
    let replay_calls_for_handler = Arc::clone(&replay_calls);
    let stop_calls_for_handler = Arc::clone(&stop_calls);
    let inner = test_inner_with_input_control_replay_handlers(
        |_| Some(AckOutcome::Failed),
        move |_, _| {
            replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            true
        },
        move |_| {
            stop_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    inner.state.epoch.store(5, Ordering::Release);
    let now = now_unix_ms();
    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        4,
        "control",
        "s-old-epoch",
        "cmd-old-epoch",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-old-epoch",
            "issued_at_ms": now,
            "expires_at_ms": now.saturating_add(30_000),
        }),
    );

    // v1.7.3 裁定：真防线是 AEAD 时间窗 + 持久重放账本，撤销桌面 epoch 前置门，staleness 改由 relay 权威执行。
    let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["outcome"], "ok");
    assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
    assert_eq!(stop_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn control_stop_honors_clock_skew_boundaries() {
    let k_room = Zeroizing::new([19_u8; 32]);
    let calls = Arc::new(AtomicU64::new(0));
    let calls_for_handler = Arc::clone(&calls);
    let inner = test_inner_with_input_control_handlers(
        |_| Some(AckOutcome::Failed),
        move |_| {
            calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    inner.state.epoch.store(7, Ordering::Release);
    let now = now_unix_ms();
    let cases = [
        (
            "cmd-skew-expired-within",
            now.saturating_sub(110_000),
            now.saturating_sub(100_000),
            "ok",
            1,
        ),
        (
            "cmd-skew-future-within",
            now.saturating_add(100_000),
            now.saturating_add(110_000),
            "ok",
            2,
        ),
        (
            "cmd-skew-expired-beyond",
            now.saturating_sub(140_000),
            now.saturating_sub(130_000),
            "failed",
            2,
        ),
        (
            "cmd-skew-future-beyond",
            now.saturating_add(130_000),
            now.saturating_add(140_000),
            "failed",
            2,
        ),
    ];

    for (command_id, issued_at_ms, expires_at_ms, expected, expected_calls) in cases {
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-skew",
            command_id,
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-skew",
                "issued_at_ms": issued_at_ms,
                "expires_at_ms": expires_at_ms,
            }),
        );
        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["outcome"], expected, "case {command_id}");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            expected_calls,
            "case {command_id}"
        );
    }
}

#[test]
fn control_stop_rejects_missing_non_integer_zero_or_unsafe_authenticated_times() {
    let k_room = Zeroizing::new([20_u8; 32]);
    let calls = Arc::new(AtomicU64::new(0));
    let replay_calls = Arc::new(AtomicU64::new(0));
    let calls_for_handler = Arc::clone(&calls);
    let replay_calls_for_handler = Arc::clone(&replay_calls);
    let inner = test_inner_with_input_control_replay_handlers(
        |_| Some(AckOutcome::Failed),
        move |_, _| {
            replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            true
        },
        move |_| {
            calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    inner.state.epoch.store(7, Ordering::Release);
    let now = now_unix_ms();
    let payloads = [
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "expires_at_ms": now.saturating_add(30_000),
        }),
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "issued_at_ms": now,
        }),
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "issued_at_ms": "not-an-integer",
            "expires_at_ms": now.saturating_add(30_000),
        }),
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "issued_at_ms": now,
            "expires_at_ms": "not-an-integer",
        }),
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "issued_at_ms": 0,
            "expires_at_ms": now.saturating_add(30_000),
        }),
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "issued_at_ms": now,
            "expires_at_ms": 0,
        }),
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "issued_at_ms": JSON_SAFE_INTEGER_MAX + 1,
            "expires_at_ms": now.saturating_add(30_000),
        }),
        serde_json::json!({
            "t": "control.stop",
            "session": "s-bad-time",
            "issued_at_ms": now,
            "expires_at_ms": JSON_SAFE_INTEGER_MAX + 1,
        }),
    ];

    for (index, payload) in payloads.into_iter().enumerate() {
        let command_id = format!("cmd-bad-time-{index}");
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-bad-time",
            &command_id,
            &payload,
        );
        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["outcome"], "failed", "case {index}");
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(replay_calls.load(Ordering::Relaxed), 0);
}
