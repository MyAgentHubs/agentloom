#![cfg(test)]

use super::*;

fn required_strings_error(frame: &Value, fields: &[&str]) -> Option<&'static str> {
    fields
        .iter()
        .find_map(|field| wire_v1_required_string(frame, field).err())
}

fn token_subject_error(frame: &Value) -> Option<&'static str> {
    match wire_v1_required_string(frame, "subject") {
        Ok(subject) => wire_v1_subject_error(subject),
        Err(error) => Some(error),
    }
}

fn token_generation_error(
    frame: &Value,
    field: &str,
    missing: &'static str,
    non_positive: &'static str,
) -> Option<&'static str> {
    wire_v1_positive_integer(frame, field, missing, non_positive).err()
}

fn token_entries_error(frame: &Value, limit_error: &'static str) -> Option<&'static str> {
    let Some(items) = frame.get("entries").and_then(Value::as_array) else {
        return Some("entries_required");
    };
    if items.len() > 256 {
        return Some(limit_error);
    }
    for entry in items {
        if !entry.is_object() {
            return Some("entry_invalid");
        }
        if let Some(error) = wire_v1_put_body_error(entry) {
            return Some(error);
        }
    }
    None
}

fn token_mutation_frame_error(frame: &Value, kind: &str) -> Option<&'static str> {
    match kind {
        "token.put" => wire_v1_put_body_error(frame),
        "token.delete" => token_subject_error(frame)
            .or_else(|| {
                token_generation_error(
                    frame,
                    "generation",
                    "generation_required",
                    "generation_must_be_positive",
                )
            })
            .or_else(|| {
                frame
                    .get("close")
                    .is_some_and(|value| !value.is_boolean())
                    .then_some("close_invalid")
            }),
        "token.ack" => token_subject_error(frame)
            .or_else(|| {
                token_generation_error(
                    frame,
                    "generation",
                    "generation_required",
                    "generation_must_be_positive",
                )
            })
            .or_else(|| {
                (!matches!(
                    frame.get("result").and_then(Value::as_str),
                    Some("ok" | "idempotent" | "rejected")
                ))
                .then_some("result_invalid")
            })
            .or_else(|| {
                frame
                    .get("reason")
                    .is_some_and(|value| !value.is_string())
                    .then_some("reason_invalid")
            }),
        "token.sync" | "token.reset" => token_generation_error(
            frame,
            "revision",
            "revision_required",
            "revision_must_be_positive",
        )
        .or_else(|| {
            token_entries_error(
                frame,
                if kind == "token.sync" {
                    "sync_entries_too_many"
                } else {
                    "reset_entries_too_many"
                },
            )
        }),
        "token.sync.ack" => token_generation_error(
            frame,
            "revision",
            "revision_required",
            "revision_must_be_positive",
        )
        .or_else(|| {
            (!frame
                .get("relay_high_water")
                .is_some_and(|value| value.as_u64().is_some()))
            .then_some("relay_high_water_required")
        }),
        _ => unreachable!("token mutation frame kind checked"),
    }
}

fn token_refresh_frame_error(frame: &Value, kind: &str) -> Option<&'static str> {
    match kind {
        "token.refresh" => required_strings_error(frame, &["request_id", "ct", "n"]),
        "token.refresh.forward" => required_strings_error(frame, &["request_id"])
            .or_else(|| token_subject_error(frame))
            .or_else(|| {
                token_generation_error(
                    frame,
                    "request_generation",
                    "request_generation_required",
                    "request_generation_must_be_positive",
                )
            })
            .or_else(|| required_strings_error(frame, &["ct", "n"])),
        "token.refresh.ok" => required_strings_error(frame, &["request_id"])
            .or_else(|| token_subject_error(frame))
            .or_else(|| {
                token_generation_error(
                    frame,
                    "generation",
                    "generation_required",
                    "generation_must_be_positive",
                )
            })
            .or_else(|| required_strings_error(frame, &["ct", "n"])),
        "token.refresh.fail" => required_strings_error(frame, &["request_id"])
            .or_else(|| token_subject_error(frame))
            .or_else(|| required_strings_error(frame, &["reason"]))
            .or_else(|| {
                frame
                    .get("close")
                    .is_some_and(|value| !value.is_boolean())
                    .then_some("close_invalid")
            }),
        _ => unreachable!("token refresh frame kind checked"),
    }
}

fn room_error(frame: &Value) -> Option<&'static str> {
    (!frame["room"].as_str().is_some_and(|room| {
        room.len() == 32
            && room
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }))
    .then_some("room_invalid")
}

fn pairing_frame_error(frame: &Value, kind: &str) -> Option<&'static str> {
    match kind {
        // S1i3 K3.5: These two only validate the shape of static samples in wire-v1.json ("origin_connection_id
        // must exist"); they neither drive nor consume the relay-side room-do.js's actual forwarding,
        // stamping, and routing logic—"samples have tests" does not mean "forwarding code has tests." What actually drives the real path and pins the frames
        // actually forwarded by the relay is in remote-relay/test/room-do.test.js:
        // the two real-path tests, "pair.hello: relay stamps origin_connection_id before forwarding," and
        // "pair.done: relay stamps origin_connection_id before forwarding."
        // (Do not hard-code line numbers again—line numbers drift as tests are subsequently inserted; stale line-number references are worse than no references.)
        "pair.hello" => required_strings_error(
            frame,
            &[
                "room",
                "remote_pub",
                "token_ct",
                "token_n",
                "origin_connection_id",
            ],
        )
        .or_else(|| room_error(frame)),
        "pair.done" => required_strings_error(
            frame,
            &[
                "room",
                "device_id",
                "confirm_ct",
                "confirm_n",
                "origin_connection_id",
            ],
        )
        .or_else(|| room_error(frame)),
        "pair.ready" => required_strings_error(frame, &["room", "device_id", "ct", "n"])
            .or_else(|| room_error(frame)),
        "pair.accept" => {
            if frame.get("capability_token").is_some() || frame.get("refresh_token").is_some() {
                return Some("plaintext_token_forbidden");
            }
            required_strings_error(
                frame,
                &[
                    "room",
                    "device_id",
                    "k_room_ct",
                    "k_room_n",
                    "tokens_ct",
                    "tokens_n",
                ],
            )
            .or_else(|| room_error(frame))
        }
        _ => unreachable!("pairing frame kind checked"),
    }
}

pub(super) fn wire_v1_token_frame_error(frame: &Value) -> Option<&'static str> {
    match frame.get("t").and_then(Value::as_str) {
        Some(
            kind @ ("token.put" | "token.delete" | "token.ack" | "token.sync" | "token.reset"
            | "token.sync.ack"),
        ) => token_mutation_frame_error(frame, kind),
        Some(
            kind @ ("token.refresh"
            | "token.refresh.forward"
            | "token.refresh.ok"
            | "token.refresh.fail"),
        ) => token_refresh_frame_error(frame, kind),
        Some(kind @ ("pair.hello" | "pair.done" | "pair.ready" | "pair.accept")) => {
            pairing_frame_error(frame, kind)
        }
        _ => Some("frame_type_invalid"),
    }
}

pub(super) fn assert_aad_kat_fixture_layer<'a>(
    fixture: &'a Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    aad_kat_count: &mut u32,
    aad_kat_kinds: &mut std::collections::HashSet<&'a str>,
) {
    *aad_kat_count += 1;
    let meta = fixture
        .get("meta")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name}: meta must be an object"));
    assert_eq!(
        meta.get("v").and_then(Value::as_u64),
        Some(1),
        "{name}: meta.v"
    );
    assert!(
        meta.get("room")
            .and_then(Value::as_str)
            .is_some_and(|room| room.len() == 32
                && room
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))),
        "{name}: meta.room"
    );
    assert_eq!(
        meta.get("epoch").and_then(Value::as_u64),
        Some(0),
        "{name}: meta.epoch"
    );
    let kind = meta
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{name}: meta.kind must be a string"));
    assert!(
        [
            "pair-ready",
            "pair-accept-tokens",
            "token.refresh",
            "token.refresh.ok"
        ]
        .contains(&kind),
        "{name}: meta.kind"
    );
    aad_kat_kinds.insert(kind);
    let device_id = fixture
        .get("device_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{name}: device_id must be a string"));
    assert_eq!(
        meta.get("session").and_then(Value::as_str),
        Some(device_id),
        "{name}: meta.session/device_id"
    );
    if ["pair-ready", "pair-accept-tokens"].contains(&kind) {
        assert_eq!(
            meta.get("command_id"),
            Some(&Value::Null),
            "{name}: meta.command_id"
        );
        assert!(
            fixture.get("request_id").is_none(),
            "{name}: request_id must be absent"
        );
    } else {
        let request_id = fixture
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: request_id must be a string"));
        assert_eq!(
            meta.get("command_id").and_then(Value::as_str),
            Some(request_id),
            "{name}: meta.command_id/request_id"
        );
    }
    assert!(
        expect.get("aad").is_some_and(Value::is_string),
        "{name}: expect.aad"
    );
    let kat = fixture
        .get("kat")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name}: kat must be an object"));
    assert!(
        kat.get("key_hex")
            .and_then(Value::as_str)
            .is_some_and(wire_v1_is_hex64),
        "{name}: kat.key_hex"
    );
    assert_eq!(
        kat.get("n_b64")
            .and_then(Value::as_str)
            .and_then(|value| STANDARD.decode(value).ok())
            .map(|bytes| bytes.len()),
        Some(12),
        "{name}: kat.n_b64"
    );
    assert!(
        kat.get("ct_b64")
            .and_then(Value::as_str)
            .and_then(|value| STANDARD.decode(value).ok())
            .is_some_and(|bytes| bytes.len() > 16),
        "{name}: kat.ct_b64"
    );
    let plaintext = kat
        .get("plaintext")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{name}: kat.plaintext must be a string"));
    if kind == "pair-accept-tokens" {
        let plaintext: Value = serde_json::from_str(plaintext)
            .unwrap_or_else(|error| panic!("{name}: plaintext JSON: {error}"));
        let plaintext = plaintext
            .as_object()
            .unwrap_or_else(|| panic!("{name}: plaintext must be an object"));
        assert_eq!(plaintext.len(), 2, "{name}: plaintext field count");
        for field in ["capability_token", "refresh_token"] {
            assert!(
                plaintext
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(wire_v1_is_hex64),
                "{name}: plaintext.{field}"
            );
        }
    }
}

pub(super) fn assert_chain_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
) {
    let token_hex = fixture
        .get("connect_token_hex")
        .or_else(|| fixture.get("capability_token_hex"))
        .and_then(Value::as_str);
    assert!(
        token_hex.is_some_and(wire_v1_is_hex64),
        "{name}: access token"
    );
    if fixture.get("connect_token_hex").is_some() {
        assert!(
            fixture
                .get("pairing_token_hex")
                .and_then(Value::as_str)
                .is_some_and(wire_v1_is_hex64),
            "{name}: pairing_token_hex"
        );
    }
    assert!(
        fixture
            .get("token_hash_hex")
            .and_then(Value::as_str)
            .is_some_and(wire_v1_is_hex64),
        "{name}: token_hash_hex"
    );
    assert!(
        fixture
            .get("subprotocol_offer")
            .is_some_and(Value::is_array),
        "{name}: subprotocol_offer"
    );
    assert!(
        fixture.get("put_frame").is_some_and(Value::is_object),
        "{name}: put_frame"
    );
    assert!(
        fixture.get("window").is_some_and(Value::is_object),
        "{name}: window"
    );
    assert_eq!(
        expect
            .get("scope")
            .and_then(Value::as_str)
            .is_some_and(|scope| ["pairing", "remote"].contains(&scope)),
        true,
        "{name}: expect.scope"
    );
}

fn assert_token_frame_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    let frame = fixture
        .get("frame")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name}: frame must be an object"));
    assert!(
        frame.get("t").is_some_and(Value::is_string),
        "{name}: frame.t"
    );
    let valid = expect
        .get("valid")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("{name}: expect.valid must be a bool"));
    let errors = expect
        .get("errors")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{name}: expect.errors must be an array"));
    assert!(
        errors.iter().all(Value::is_string),
        "{name}: errors must be strings"
    );
    assert_eq!(valid, errors.is_empty(), "{name}: valid/errors disagree");

    let mut hashes = Vec::new();
    wire_v1_collect_named_values(
        fixture.get("frame").expect("frame exists"),
        "token_hash",
        &mut hashes,
    );
    let hash_invalid = errors
        .iter()
        .any(|error| error.as_str() == Some("token_hash_invalid"));
    if hash_invalid {
        assert!(
            hashes
                .iter()
                .any(|hash| { hash.as_str().map_or(true, |value| !wire_v1_is_hex64(value)) }),
            "{name}: malformed hash missing"
        );
    } else {
        assert!(
            hashes
                .iter()
                .all(|hash| hash.as_str().is_some_and(wire_v1_is_hex64)),
            "{name}: token_hash"
        );
    }
    if valid {
        direction.0 += 1;
    } else {
        direction.1 += 1;
    }
}

fn assert_subprotocol_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    let offers = fixture
        .get("offers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{name}: offers must be an array"));
    assert!(offers.iter().all(Value::is_string), "{name}: offers");
    let accept = expect
        .get("accept")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("{name}: expect.accept must be a bool"));
    if accept {
        assert_eq!(
            expect.get("echo").and_then(Value::as_str),
            Some("agentloom-rc-v1"),
            "{name}: echo"
        );
        assert!(
            expect
                .get("token_hex")
                .and_then(Value::as_str)
                .is_some_and(wire_v1_is_hex64),
            "{name}: token_hex"
        );
    } else {
        assert_eq!(
            expect.get("status").and_then(Value::as_u64),
            Some(401),
            "{name}: reject status"
        );
    }
    if accept {
        direction.0 += 1;
    } else {
        direction.1 += 1;
    }
}

fn assert_http_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    let request = fixture
        .get("request")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name}: request must be an object"));
    assert!(
        request.get("method").is_some_and(Value::is_string),
        "{name}: request.method"
    );
    assert!(
        request.get("path").is_some_and(Value::is_string),
        "{name}: request.path"
    );
    let pre_state = fixture
        .get("pre_state")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name}: pre_state must be an object"));
    assert!(
        pre_state
            .get("owner")
            .and_then(Value::as_str)
            .is_some_and(|owner| ["none", "same", "other", "tombstoned"].contains(&owner)),
        "{name}: pre_state.owner"
    );
    if let Some(rate_limited) = pre_state.get("rate_limited") {
        assert!(rate_limited.is_boolean(), "{name}: pre_state.rate_limited");
    }
    if let Some(tombstoned) = pre_state.get("tombstoned") {
        assert!(tombstoned.is_boolean(), "{name}: pre_state.tombstoned");
    }
    if let Some(credential) = fixture.get("credential_hex") {
        assert!(
            credential.as_str().is_some_and(wire_v1_is_hex64),
            "{name}: credential_hex"
        );
    }
    let status = expect
        .get("status")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{name}: expect.status must be u64"));
    assert_eq!(wire_v1_http_status_decision(fixture), status, "{name}");
    if status < 400 {
        direction.0 += 1;
    } else {
        direction.1 += 1;
    }
}

fn assert_desktop_upgrade_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    assert!(
        fixture.get("authorization") == Some(&Value::Null)
            || fixture.get("authorization").is_some_and(Value::is_string),
        "{name}: authorization"
    );
    if let Some(credential) = fixture.get("credential_hex") {
        assert!(
            credential.as_str().is_some_and(wire_v1_is_hex64),
            "{name}: credential_hex"
        );
    }
    let pre_state = fixture
        .get("pre_state")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name}: pre_state must be an object"));
    assert!(
        pre_state
            .get("owner_credential_hash")
            .and_then(Value::as_str)
            .is_some_and(wire_v1_is_hex64),
        "{name}: owner_credential_hash"
    );
    assert!(
        pre_state.get("tombstoned").is_some_and(Value::is_boolean),
        "{name}: tombstoned"
    );
    let accept = expect
        .get("accept")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("{name}: expect.accept must be a bool"));
    if accept {
        assert_eq!(
            expect.get("role").and_then(Value::as_str),
            Some("desktop"),
            "{name}: role"
        );
        assert_eq!(
            expect.get("epoch_bump").and_then(Value::as_bool),
            Some(true),
            "{name}: epoch_bump"
        );
        direction.0 += 1;
    } else {
        assert!(
            expect
                .get("status")
                .and_then(Value::as_u64)
                .is_some_and(|status| [401, 410].contains(&status)),
            "{name}: status"
        );
        direction.1 += 1;
    }
}

fn assert_inbound_matrix_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    assert!(
        fixture
            .get("scope")
            .and_then(Value::as_str)
            .is_some_and(|scope| { ["pairing", "remote", "refresh", "desktop"].contains(&scope) }),
        "{name}: scope"
    );
    assert!(
        fixture.get("frame_t").is_some_and(Value::is_string),
        "{name}: frame_t"
    );
    let allowed = expect
        .get("allowed")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("{name}: expect.allowed must be a bool"));
    if allowed {
        direction.0 += 1;
    } else {
        assert_eq!(
            expect.get("error").and_then(Value::as_str),
            Some("role_forbidden"),
            "{name}: expect.error"
        );
        direction.1 += 1;
    }
}

fn assert_time_window_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    assert!(
        fixture.get("now_ms").is_some_and(wire_v1_is_safe_u64),
        "{name}: now_ms"
    );
    let row = fixture
        .get("row")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{name}: row must be an object"));
    assert!(
        row.get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| ["current", "prev"].contains(&kind)),
        "{name}: row.kind"
    );
    assert!(
        row.get("scope")
            .and_then(Value::as_str)
            .is_some_and(|scope| ["remote", "pairing"].contains(&scope)),
        "{name}: row.scope"
    );
    assert!(
        row.get("subject_state")
            .and_then(Value::as_str)
            .is_some_and(|state| ["active", "revoked"].contains(&state)),
        "{name}: row.subject_state"
    );
    assert!(
        row.get("access_expires").is_some_and(wire_v1_is_safe_u64),
        "{name}: access_expires"
    );
    assert!(
        row.get("valid_until").is_some_and(wire_v1_is_safe_u64),
        "{name}: valid_until"
    );
    let has_generation = row.get("generation").is_some();
    assert_eq!(
        has_generation,
        row.get("current_generation").is_some(),
        "{name}: generation fields must appear together"
    );
    if has_generation {
        assert!(
            row.get("generation").is_some_and(wire_v1_is_safe_u64),
            "{name}: generation"
        );
        assert!(
            row.get("current_generation")
                .is_some_and(wire_v1_is_safe_u64),
            "{name}: current_generation"
        );
    }
    let decision = expect
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{name}: expect.decision must be a string"));
    assert!(
        [
            "scope:remote",
            "scope:pairing",
            "scope:refresh",
            "reject:401",
            "reject:stale_generation"
        ]
        .contains(&decision),
        "{name}: expect.decision"
    );
    if decision == "reject:stale_generation" {
        assert_eq!(
            expect.get("close").and_then(Value::as_bool),
            Some(true),
            "{name}: stale generation must close"
        );
    }
    if decision.starts_with("reject:") {
        direction.1 += 1;
    } else {
        direction.0 += 1;
    }
}

fn assert_ttl_clamp_fixture_layer(
    fixture: &Value,
    name: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    let cap = fixture
        .get("cap")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{name}: cap must be a string"));
    assert!(wire_v1_ttl_cap_ms(cap).is_some(), "{name}: cap");
    for field in ["cap_ms", "relay_now_ms", "input_ms"] {
        assert!(
            fixture.get(field).is_some_and(wire_v1_is_safe_u64),
            "{name}: {field}"
        );
    }
    let input_ms = fixture
        .get("input_ms")
        .and_then(Value::as_u64)
        .expect("input_ms checked");
    assert!(
        expect.get("stored_ms").is_some_and(wire_v1_is_safe_u64),
        "{name}: stored_ms"
    );
    let stored_ms = expect
        .get("stored_ms")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{name}: stored_ms must be u64"));
    if stored_ms == input_ms {
        direction.0 += 1;
    } else {
        direction.1 += 1;
    }
}

pub(super) fn assert_default_token_layer_fixture(
    fixture: &Value,
    name: &str,
    layer: &str,
    expect: &serde_json::Map<String, Value>,
    direction: &mut (u32, u32),
) {
    match layer {
        "token-frame" => assert_token_frame_fixture_layer(fixture, name, expect, direction),
        "subprotocol" => assert_subprotocol_fixture_layer(fixture, name, expect, direction),
        "http" => assert_http_fixture_layer(fixture, name, expect, direction),
        "desktop-upgrade" => {
            assert_desktop_upgrade_fixture_layer(fixture, name, expect, direction);
        }
        "inbound-matrix" => {
            assert_inbound_matrix_fixture_layer(fixture, name, expect, direction);
        }
        "time-window" => assert_time_window_fixture_layer(fixture, name, expect, direction),
        "ttl-clamp" => assert_ttl_clamp_fixture_layer(fixture, name, expect, direction),
        _ => unreachable!("layer whitelist checked"),
    }
}
