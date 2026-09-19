#![cfg(test)]

use super::*;
#[test]
fn dispatches_control_frames_and_counts_bad_or_ignored_frames() {
    let inner = test_inner(|_| None, || None);
    let state = &inner.state;

    assert!(handle_frame(
        &inner,
        r#"{"t":"replay.head","epoch":3,"headSeq":17}"#,
        None,
    )
    .is_none());
    assert_eq!(state.epoch.load(Ordering::Acquire), 3);
    assert_eq!(state.head_seq.load(Ordering::Acquire), 17);
    assert_eq!(state.frames_seen.load(Ordering::Relaxed), 1);

    handle_frame(
        &inner,
        r#"{"t":"error","reason":"stale_epoch","currentEpoch":9}"#,
        None,
    );
    assert_eq!(state.epoch.load(Ordering::Acquire), 9);
    assert_eq!(state.frames_seen.load(Ordering::Relaxed), 2);

    assert!(handle_frame(&inner, "not json{{{", None).is_none());
    assert_eq!(state.bad_frames.load(Ordering::Relaxed), 1);
    assert_eq!(state.frames_seen.load(Ordering::Relaxed), 3);

    assert!(handle_frame(&inner, r#"{"t":"msg.completed","x":1}"#, None).is_none());
    assert_eq!(state.frames_seen.load(Ordering::Relaxed), 4);
    assert_eq!(state.epoch.load(Ordering::Acquire), 9);
    assert_eq!(state.head_seq.load(Ordering::Acquire), 17);
    assert_eq!(state.bad_frames.load(Ordering::Relaxed), 1);

    assert!(handle_frame(&inner, r#"{"kind":"event","t":"ignored"}"#, None).is_none());
    assert_eq!(state.frames_seen.load(Ordering::Relaxed), 5);
    assert_eq!(state.bad_frames.load(Ordering::Relaxed), 1);
}

#[test]
fn routes_encrypted_input_send_and_maps_handler_outcomes() {
    let k_room = Zeroizing::new([7_u8; 32]);
    for (outcome, expected) in [
        (AckOutcome::Ok, "ok"),
        (AckOutcome::Queued, "queued"),
        (AckOutcome::Failed, "failed"),
    ] {
        let received = Arc::new(Mutex::new(None));
        let received_for_handler = Arc::clone(&received);
        let inner = test_inner_with_input_control_handlers(
            move |frame| {
                *received_for_handler.lock().unwrap() =
                    Some((frame.session, frame.command_id, frame.text));
                Some(outcome)
            },
            |_| AckOutcome::Failed,
        );
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-1",
            "cmd-input-1",
            &serde_json::json!({
                "t": "input.send",
                "session": "s-1",
                "text": "hello",
            }),
        );

        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(
            response,
            serde_json::json!({
                "t": "input.ack",
                "command_id": "cmd-input-1",
                "outcome": expected,
            })
        );
        assert_eq!(
            received.lock().unwrap().as_ref(),
            Some(&(
                "s-1".to_owned(),
                "cmd-input-1".to_owned(),
                "hello".to_owned(),
            ))
        );
    }
}

#[test]
fn encrypted_input_send_ack_loss_retry_uses_terminal_ledger_without_redelivery() {
    let k_room = Zeroizing::new([27_u8; 32]);
    let conn = Arc::new(Mutex::new(crate::test_support::mem_db()));
    let delivery_calls = Arc::new(AtomicU64::new(0));
    let conn_for_handler = Arc::clone(&conn);
    let delivery_calls_for_handler = Arc::clone(&delivery_calls);
    let inner = test_inner_with_input_control_handlers(
        move |frame| {
            let payload = serde_json::json!({"text": frame.text}).to_string();
            crate::remote_input_send_ack(
                || {
                    crate::db::enqueue_remote_input(
                        &lock(&conn_for_handler),
                        &frame.session,
                        &frame.command_id,
                        "input.send",
                        &payload,
                    )
                    .map_err(|e| e.to_string())
                },
                || {
                    let entry = crate::db::next_pending_remote_input(
                        &lock(&conn_for_handler),
                        &frame.session,
                    )
                    .unwrap()
                    .expect("新 input.send 应在即时排空时可见");
                    delivery_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                    crate::db::mark_remote_input_delivered(&lock(&conn_for_handler), entry.id)
                        .unwrap();
                },
                || {
                    crate::db::remote_inbox_terminal_state_by_command_id(
                        &lock(&conn_for_handler),
                        &frame.command_id,
                    )
                    .map_err(|e| e.to_string())
                },
            )
        },
        |_| AckOutcome::Failed,
    );
    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-wire-ledger",
        "cmd-wire-ledger",
        &serde_json::json!({
            "t": "input.send",
            "session": "s-wire-ledger",
            "text": "hello",
        }),
    );

    let first = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(first["outcome"], "queued");
    assert_eq!(delivery_calls.load(Ordering::Relaxed), 1);
    let delivered_at: Option<i64> = lock(&conn)
        .query_row(
            "SELECT delivered_at FROM remote_inbox WHERE command_id = 'cmd-wire-ledger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(delivered_at.is_some());

    // 模拟 relay 未收到第一次 ack 后原样重发：台账终态仍映射 ok，且绝不再次排空/投递。
    let retry = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(retry["outcome"], "ok");
    assert_eq!(delivery_calls.load(Ordering::Relaxed), 1);
    let count: i64 = lock(&conn)
        .query_row(
            "SELECT COUNT(*) FROM remote_inbox WHERE command_id = 'cmd-wire-ledger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn rejects_commands_encrypted_with_an_unpaired_key() {
    let paired_k_room = Zeroizing::new([9_u8; 32]);
    let unpaired_k_room = Zeroizing::new([10_u8; 32]);
    let input_calls = Arc::new(AtomicU64::new(0));
    let control_calls = Arc::new(AtomicU64::new(0));
    let input_calls_for_handler = Arc::clone(&input_calls);
    let control_calls_for_handler = Arc::clone(&control_calls);
    let inner = test_inner_with_input_control_handlers(
        move |_| {
            input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            Some(AckOutcome::Ok)
        },
        move |_| {
            control_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    let input = seal_command_envelope(
        &unpaired_k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-3",
        "cmd-wrong-input",
        &serde_json::json!({"t": "input.send", "session": "s-3", "text": "x"}),
    );
    let control = seal_command_envelope(
        &unpaired_k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-3",
        "cmd-wrong-control",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-3",
            "issued_at_ms": now_unix_ms(),
            "expires_at_ms": now_unix_ms().saturating_add(30_000),
        }),
    );

    for (envelope, command_id) in [(input, "cmd-wrong-input"), (control, "cmd-wrong-control")] {
        let response = handle_frame(&inner, &envelope.to_string(), Some(&paired_k_room)).unwrap();
        assert_eq!(response["command_id"], command_id);
        assert_eq!(response["outcome"], "failed");
    }
    assert_eq!(input_calls.load(Ordering::Relaxed), 0);
    assert_eq!(control_calls.load(Ordering::Relaxed), 0);
    assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 2);
}

#[test]
fn malformed_command_envelopes_fail_without_calling_handlers() {
    let k_room = Zeroizing::new([11_u8; 32]);
    let input_calls = Arc::new(AtomicU64::new(0));
    let control_calls = Arc::new(AtomicU64::new(0));
    let input_calls_for_handler = Arc::clone(&input_calls);
    let control_calls_for_handler = Arc::clone(&control_calls);
    let inner = test_inner_with_input_control_handlers(
        move |_| {
            input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            Some(AckOutcome::Ok)
        },
        move |_| {
            control_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );

    assert!(handle_frame(
        &inner,
        r#"{"kind":"input","ct":"x","n":"y"}"#,
        Some(&k_room)
    )
    .is_none());

    let mut missing_ct = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-4",
        "cmd-missing-ct",
        &serde_json::json!({"t": "input.send", "session": "s-4", "text": "x"}),
    );
    missing_ct.as_object_mut().unwrap().remove("ct");
    let missing_ct_response = handle_frame(&inner, &missing_ct.to_string(), Some(&k_room)).unwrap();
    assert_eq!(missing_ct_response["outcome"], "failed");

    let mut damaged = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-4",
        "cmd-damaged-ct",
        &serde_json::json!({
            "t": "control.stop",
            "session": "s-4",
            "issued_at_ms": now_unix_ms(),
            "expires_at_ms": now_unix_ms().saturating_add(30_000),
        }),
    );
    let mut ciphertext = STANDARD.decode(damaged["ct"].as_str().unwrap()).unwrap();
    ciphertext[0] ^= 1;
    damaged["ct"] = Value::from(STANDARD.encode(ciphertext));
    let damaged_response = handle_frame(&inner, &damaged.to_string(), Some(&k_room)).unwrap();
    assert_eq!(damaged_response["outcome"], "failed");

    let missing_key = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-4",
        "cmd-missing-key",
        &serde_json::json!({"t": "input.send", "session": "s-4", "text": "x"}),
    );
    assert!(handle_frame(&inner, &missing_key.to_string(), None).is_none());

    assert_eq!(input_calls.load(Ordering::Relaxed), 0);
    assert_eq!(control_calls.load(Ordering::Relaxed), 0);
    assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 3);
}

#[test]
fn input_send_without_k_room_returns_no_ack_and_preserves_ledger() {
    let k_room = Zeroizing::new([31_u8; 32]);
    let input_calls = Arc::new(AtomicU64::new(0));
    let input_calls_for_handler = Arc::clone(&input_calls);
    let inner = test_inner_with_input_control_handlers(
        move |_| {
            input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            Some(AckOutcome::Queued)
        },
        |_| AckOutcome::Failed,
    );
    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-key-retry",
        "cmd-key-retry",
        &serde_json::json!({
            "t": "input.send",
            "session": "s-key-retry",
            "text": "retry after key recovery",
        }),
    );
    let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

    assert!(handle_frame(&inner, &envelope.to_string(), None).is_none());
    assert_eq!(
        inner.state.bad_frames.load(Ordering::Relaxed),
        bad_frames_before
    );
    assert_eq!(input_calls.load(Ordering::Relaxed), 0);

    let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["command_id"], "cmd-key-retry");
    assert_eq!(response["outcome"], "queued");
    assert_eq!(input_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn input_send_handler_without_outcome_returns_no_ack_or_bad_frame() {
    let k_room = Zeroizing::new([34_u8; 32]);
    let input_calls = Arc::new(AtomicU64::new(0));
    let input_calls_for_handler = Arc::clone(&input_calls);
    let inner = test_inner_with_input_control_handlers(
        move |_| {
            let previous = input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            (previous > 0).then_some(AckOutcome::Queued)
        },
        |_| AckOutcome::Failed,
    );
    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-enqueue-retry",
        "cmd-enqueue-retry",
        &serde_json::json!({
            "t": "input.send",
            "session": "s-enqueue-retry",
            "text": "retry after enqueue recovery",
        }),
    );
    let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

    assert!(handle_frame(&inner, &envelope.to_string(), Some(&k_room)).is_none());
    assert_eq!(
        inner.state.bad_frames.load(Ordering::Relaxed),
        bad_frames_before
    );

    let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["command_id"], "cmd-enqueue-retry");
    assert_eq!(response["outcome"], "queued");
    assert_eq!(input_calls.load(Ordering::Relaxed), 2);
}

#[test]
fn input_send_with_pipe_in_session_is_rejected_before_handler() {
    let k_room = Zeroizing::new([32_u8; 32]);
    let input_calls = Arc::new(AtomicU64::new(0));
    let input_calls_for_handler = Arc::clone(&input_calls);
    let inner = test_inner_with_input_control_handlers(
        move |_| {
            input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            Some(AckOutcome::Ok)
        },
        |_| AckOutcome::Failed,
    );
    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s|pipe",
        "cmd-session-pipe",
        &serde_json::json!({"t": "input.send", "session": "s|pipe", "text": "x"}),
    );
    let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

    let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["outcome"], "failed");
    assert_eq!(
        inner.state.bad_frames.load(Ordering::Relaxed),
        bad_frames_before + 1
    );
    assert_eq!(input_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn input_send_with_command_id_over_128_bytes_is_rejected_before_handler() {
    let k_room = Zeroizing::new([33_u8; 32]);
    let input_calls = Arc::new(AtomicU64::new(0));
    let input_calls_for_handler = Arc::clone(&input_calls);
    let inner = test_inner_with_input_control_handlers(
        move |_| {
            input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            Some(AckOutcome::Ok)
        },
        |_| AckOutcome::Failed,
    );
    let command_id = "c".repeat(COMMAND_ID_MAX_LEN + 1);
    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-long-command-id",
        &command_id,
        &serde_json::json!({
            "t": "input.send",
            "session": "s-long-command-id",
            "text": "x",
        }),
    );
    let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

    let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["outcome"], "failed");
    assert_eq!(response["command_id"], command_id);
    assert_eq!(
        inner.state.bad_frames.load(Ordering::Relaxed),
        bad_frames_before + 1
    );
    assert_eq!(input_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn shared_wire_v1_fixture_matches_aad_and_rejects_invalid_commands_before_handlers() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .expect("wire-v1 fixture must be valid JSON");
    let fixtures = fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array");
    let input_calls = Arc::new(AtomicU64::new(0));
    let control_calls = Arc::new(AtomicU64::new(0));
    let input_calls_for_handler = Arc::clone(&input_calls);
    let control_calls_for_handler = Arc::clone(&control_calls);
    let inner = test_inner_with_input_control_handlers(
        move |_| {
            input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            Some(AckOutcome::Ok)
        },
        move |_| {
            control_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            AckOutcome::Ok
        },
    );
    let k_room = Zeroizing::new([21_u8; 32]);

    for fixture in fixtures {
        if fixture.get("layer").is_some() {
            continue;
        }
        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("fixture name must be a string");
        let envelope_value = fixture
            .get("envelope")
            .expect("fixture envelope must exist");
        let envelope = envelope_value
            .as_object()
            .expect("fixture envelope must be an object");
        for field in [
            "v",
            "room",
            "epoch",
            "kind",
            "session",
            "command_id",
            "seq",
            "ct",
            "n",
            "ts",
        ] {
            assert!(
                envelope.contains_key(field),
                "{name}: missing envelope.{field}"
            );
        }

        let expect = fixture
            .get("expect")
            .and_then(Value::as_object)
            .expect("fixture expect must be an object");
        let valid = expect
            .get("valid")
            .and_then(Value::as_bool)
            .expect("fixture expect.valid must be a bool");
        let errors = expect
            .get("errors")
            .and_then(Value::as_array)
            .expect("fixture expect.errors must be an array");
        let aad = expect.get("aad").expect("fixture expect.aad must exist");
        assert!(
            aad.is_string() || aad.is_null(),
            "{name}: aad must be string|null"
        );

        if valid {
            let session = match envelope.get("session") {
                Some(Value::String(value)) => Some(value.clone()),
                Some(Value::Null) => None,
                _ => panic!("{name}: session must be string|null"),
            };
            let command_id = match envelope.get("command_id") {
                Some(Value::String(value)) => Some(value.clone()),
                Some(Value::Null) => None,
                _ => panic!("{name}: command_id must be string|null"),
            };
            let meta = crate::remote_crypto::EnvelopeMeta {
                v: envelope
                    .get("v")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .expect("valid fixture v must fit u32"),
                room: envelope
                    .get("room")
                    .and_then(Value::as_str)
                    .expect("valid fixture room must be a string")
                    .to_owned(),
                epoch: envelope
                    .get("epoch")
                    .and_then(Value::as_u64)
                    .expect("valid fixture epoch must be u64"),
                kind: envelope
                    .get("kind")
                    .and_then(Value::as_str)
                    .expect("valid fixture kind must be a string")
                    .to_owned(),
                session,
                command_id,
            };
            let expected_aad = aad
                .as_str()
                .expect("valid fixture expect.aad must be a string");
            assert_eq!(
                crate::remote_crypto::build_aad(&meta).as_bytes(),
                expected_aad.as_bytes(),
                "{name}: AAD mismatch"
            );
        } else if errors
            .iter()
            .any(|error| error.as_str() == Some("command_id_required_for_kind"))
        {
            assert!(
                handle_command_envelope(&inner, envelope_value, Some(&k_room)).is_none(),
                "{name}: missing command_id must be rejected before ack/decryption"
            );
            assert_eq!(input_calls.load(Ordering::Relaxed), 0, "{name}");
            assert_eq!(control_calls.load(Ordering::Relaxed), 0, "{name}");
        } else if errors.iter().any(|error| {
            matches!(
                error.as_str(),
                Some("session_must_not_contain_pipe")
                    | Some("command_id_must_not_contain_pipe")
                    | Some("command_id_too_long")
            )
        }) {
            let response = handle_command_envelope(&inner, envelope_value, Some(&k_room))
                .expect("invalid command with a usable command_id must receive failed ack");
            assert_eq!(response["outcome"], "failed", "{name}");
            assert_eq!(input_calls.load(Ordering::Relaxed), 0, "{name}");
            assert_eq!(control_calls.load(Ordering::Relaxed), 0, "{name}");
        }
    }
}

#[test]
fn encrypted_input_answer_routes_handler_outcomes_without_counting_them_bad() {
    let k_room = Zeroizing::new([12_u8; 32]);
    for (outcome, expected) in [
        (AckOutcome::Queued, "queued"),
        (AckOutcome::Ok, "ok"),
        (AckOutcome::Failed, "failed"),
    ] {
        let received = Arc::new(Mutex::new(None));
        let received_for_handler = Arc::clone(&received);
        let inner = test_inner_with_input_answer_handler(move |frame| {
            *received_for_handler.lock().unwrap() = Some((
                frame.session,
                frame.command_id,
                frame.decision_id,
                frame.option,
            ));
            Some(outcome)
        });
        let answer = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-5",
            "cmd-answer",
            &serde_json::json!({
                "t": "input.answer",
                "session": "s-5",
                "decision_id": "d-1",
                "option": "yes",
            }),
        );

        let response = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["command_id"], "cmd-answer");
        assert_eq!(response["outcome"], expected);
        assert_eq!(
            received.lock().unwrap().as_ref(),
            Some(&(
                "s-5".to_owned(),
                "cmd-answer".to_owned(),
                "d-1".to_owned(),
                "yes".to_owned(),
            ))
        );
        assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn encrypted_input_answer_ack_loss_retry_uses_terminal_ledger_without_reprocessing() {
    let k_room = Zeroizing::new([28_u8; 32]);
    let conn = Arc::new(Mutex::new(crate::test_support::mem_db()));
    let processing_calls = Arc::new(AtomicU64::new(0));
    let conn_for_handler = Arc::clone(&conn);
    let processing_calls_for_handler = Arc::clone(&processing_calls);
    let inner = test_inner_with_input_answer_handler(move |frame| {
        let payload = serde_json::json!({
            "decision_id": frame.decision_id,
            "option": frame.option,
        })
        .to_string();
        crate::remote_input_send_ack(
            || {
                crate::db::enqueue_remote_input(
                    &lock(&conn_for_handler),
                    &frame.session,
                    &frame.command_id,
                    "input.answer",
                    &payload,
                )
                .map_err(|e| e.to_string())
            },
            || {
                processing_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                crate::db::mark_remote_input_delivered_by_command_id(
                    &lock(&conn_for_handler),
                    &frame.command_id,
                )
                .unwrap();
            },
            || {
                crate::db::remote_inbox_terminal_state_by_command_id(
                    &lock(&conn_for_handler),
                    &frame.command_id,
                )
                .map_err(|e| e.to_string())
            },
        )
    });
    let answer = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "input",
        "s-answer-ledger",
        "cmd-answer-ledger",
        &serde_json::json!({
            "t": "input.answer",
            "session": "s-answer-ledger",
            "decision_id": "d-ledger",
            "option": "yes",
        }),
    );

    let first = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
    assert_eq!(first["outcome"], "queued");
    assert_eq!(processing_calls.load(Ordering::Relaxed), 1);
    let delivered_at: Option<i64> = lock(&conn)
        .query_row(
            "SELECT delivered_at FROM remote_inbox WHERE command_id = 'cmd-answer-ledger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(delivered_at.is_some());

    let retry = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
    assert_eq!(retry["outcome"], "ok");
    assert_eq!(processing_calls.load(Ordering::Relaxed), 1);
    let count: i64 = lock(&conn)
        .query_row(
            "SELECT COUNT(*) FROM remote_inbox WHERE command_id = 'cmd-answer-ledger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn encrypted_input_answer_missing_fields_return_failed_and_count_bad_frames() {
    let k_room = Zeroizing::new([29_u8; 32]);
    for payload in [
        serde_json::json!({
            "t": "input.answer",
            "session": "s-bad-answer",
            "option": "yes",
        }),
        serde_json::json!({
            "t": "input.answer",
            "session": "s-bad-answer",
            "decision_id": "d-bad-answer",
        }),
        serde_json::json!({
            "t": "input.answer",
            "session": 7,
            "decision_id": "d-bad-answer",
            "option": "yes",
        }),
    ] {
        let inner = test_inner_with_input_answer_handler(|_| {
            panic!("字段校验失败时不得调用 answer handler")
        });
        let answer = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-bad-answer",
            "cmd-bad-answer",
            &payload,
        );

        let response = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["outcome"], "failed");
        assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 1);
    }
}

fn test_inner_with_input_answer_handler(
    input_answer_handler: impl Fn(InputAnswerFrame) -> Option<AckOutcome> + Send + Sync + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    with_default_active_repo(Arc::new(Inner {
        settings: Box::new(|_| None),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: test_active_room_resolver(),
        k_room_provider: Box::new(|_| None),
        session_index_snapshot_provider: Box::new(|| None),
        milestone_replay_provider: Box::new(|| None),
        session_runtime_replay_provider: Box::new(|| None),
        pair_hello_handler: Box::new(|_| None),
        pair_done_handler: Box::new(|_| PairDoneAction::Rejected),
        registry: test_registry(),
        refresh_handler: test_refresh_handler(),
        registry_snapshot_provider: test_registry_snapshot_provider(),
        registry_rebase_provider: test_registry_rebase_provider(),
        registry_high_water_provider: test_registry_high_water_provider(),
        input_send_handler: Box::new(|_| Some(AckOutcome::Failed)),
        input_answer_handler: Box::new(input_answer_handler),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(|_| AckOutcome::Failed),
        upstream_tx,
        milestone_tx,
        session_repo_provider: test_session_repo_provider_allowing_default_repo(),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    }))
}
