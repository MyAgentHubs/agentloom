#![cfg(test)]

use super::*;
const WIRE_V1_TOKEN_LAYERS: [&str; 9] = [
    "token-frame",
    "subprotocol",
    "http",
    "desktop-upgrade",
    "inbound-matrix",
    "time-window",
    "ttl-clamp",
    "aad-kat",
    "chain",
];

const WIRE_V1_JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

fn wire_v1_is_safe_u64(value: &Value) -> bool {
    value
        .as_u64()
        .is_some_and(|value| value <= WIRE_V1_JSON_SAFE_INTEGER_MAX)
}

fn wire_v1_required_string_error(field: &str) -> &'static str {
    match field {
        "subject" => "subject_required",
        "request_id" => "request_id_required",
        "ct" => "ct_required",
        "n" => "n_required",
        "room" => "room_required",
        "device_id" => "device_id_required",
        "k_room_ct" => "k_room_ct_required",
        "k_room_n" => "k_room_n_required",
        "tokens_ct" => "tokens_ct_required",
        "tokens_n" => "tokens_n_required",
        "reason" => "reason_required",
        "remote_pub" => "remote_pub_required",
        "token_ct" => "token_ct_required",
        "token_n" => "token_n_required",
        "origin_connection_id" => "origin_connection_id_required",
        "confirm_ct" => "confirm_ct_required",
        "confirm_n" => "confirm_n_required",
        _ => "string_field_required",
    }
}

fn wire_v1_required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, &'static str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| wire_v1_required_string_error(field))
}

fn wire_v1_positive_integer(
    value: &Value,
    field: &str,
    missing: &'static str,
    non_positive: &'static str,
) -> Result<(), &'static str> {
    let Some(number) = value.get(field) else {
        return Err(missing);
    };
    if number.as_u64().is_some_and(|number| number > 0) {
        Ok(())
    } else {
        Err(non_positive)
    }
}

fn wire_v1_timestamp_error(value: Option<&Value>) -> Option<&'static str> {
    let Some(timestamp) = value.and_then(Value::as_u64) else {
        return Some("timestamp_must_be_positive");
    };
    if timestamp == 0 {
        Some("timestamp_must_be_positive")
    } else if timestamp > WIRE_V1_JSON_SAFE_INTEGER_MAX {
        Some("timestamp_exceeds_json_safe_integer")
    } else {
        None
    }
}

fn wire_v1_subject_error(subject: &str) -> Option<&'static str> {
    if subject == "pairing" {
        return None;
    }
    let Some(uuid) = subject.strip_prefix("device:") else {
        return Some("subject_invalid");
    };
    let valid = uuid.len() == 36
        && uuid.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        });
    (!valid).then_some("subject_invalid")
}

fn wire_v1_put_body_error(body: &Value) -> Option<&'static str> {
    let subject = match wire_v1_required_string(body, "subject") {
        Ok(subject) => subject,
        Err(error) => return Some(error),
    };
    if let Some(error) = wire_v1_subject_error(subject) {
        return Some(error);
    }
    if let Err(error) = wire_v1_positive_integer(
        body,
        "generation",
        "generation_required",
        "generation_must_be_positive",
    ) {
        return Some(error);
    }
    let Some(scope) = body.get("scope").and_then(Value::as_str) else {
        return Some("scope_invalid");
    };
    if !["remote", "pairing"].contains(&scope) {
        return Some("scope_invalid");
    }
    if subject == "pairing" && scope != "pairing" {
        return Some("pairing_scope_required");
    }
    let Some(current) = body.get("current").filter(|value| value.is_object()) else {
        return Some("current_required");
    };
    let Some(token_hash) = current.get("token_hash") else {
        return Some("current_token_hash_required");
    };
    if !token_hash.as_str().is_some_and(wire_v1_is_hex64) {
        return Some("token_hash_invalid");
    }
    if let Some(error) = wire_v1_timestamp_error(current.get("access_expires")) {
        return Some(error);
    }
    if scope == "remote" {
        if let Some(error) = wire_v1_timestamp_error(current.get("refresh_until")) {
            return Some(error);
        }
        if current["access_expires"].as_u64() > current["refresh_until"].as_u64() {
            return Some("access_expires_after_refresh_until");
        }
    }
    if let Some(prev) = body.get("prev") {
        if subject == "pairing" {
            return Some("pairing_prev_forbidden");
        }
        if !prev.is_object() {
            return Some("prev_invalid");
        }
        let Some(token_hash) = prev.get("token_hash") else {
            return Some("prev_token_hash_required");
        };
        if !token_hash.as_str().is_some_and(wire_v1_is_hex64) {
            return Some("token_hash_invalid");
        }
        if let Err(error) = wire_v1_positive_integer(
            prev,
            "generation",
            "generation_required",
            "generation_must_be_positive",
        ) {
            return Some(error);
        }
        if let Some(error) = wire_v1_timestamp_error(prev.get("prev_expires")) {
            return Some(error);
        }
    }
    None
}

fn wire_v1_token_frame_error(frame: &Value) -> Option<&'static str> {
    let required_strings = |fields: &[&str]| {
        fields
            .iter()
            .find_map(|field| wire_v1_required_string(frame, field).err())
    };
    let subject = || match wire_v1_required_string(frame, "subject") {
        Ok(subject) => wire_v1_subject_error(subject),
        Err(error) => Some(error),
    };
    let generation = |field, missing, non_positive| {
        wire_v1_positive_integer(frame, field, missing, non_positive).err()
    };
    let entries = |limit_error: &'static str| {
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
    };

    match frame.get("t").and_then(Value::as_str) {
        Some("token.put") => wire_v1_put_body_error(frame),
        Some("token.delete") => subject()
            .or_else(|| {
                generation(
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
        Some("token.ack") => subject()
            .or_else(|| {
                generation(
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
        Some(kind @ ("token.sync" | "token.reset")) => {
            generation("revision", "revision_required", "revision_must_be_positive").or_else(|| {
                entries(if kind == "token.sync" {
                    "sync_entries_too_many"
                } else {
                    "reset_entries_too_many"
                })
            })
        }
        Some("token.sync.ack") => {
            generation("revision", "revision_required", "revision_must_be_positive").or_else(|| {
                (!frame
                    .get("relay_high_water")
                    .is_some_and(|value| value.as_u64().is_some()))
                .then_some("relay_high_water_required")
            })
        }
        Some("token.refresh") => required_strings(&["request_id", "ct", "n"]),
        Some("token.refresh.forward") => required_strings(&["request_id"])
            .or_else(subject)
            .or_else(|| {
                generation(
                    "request_generation",
                    "request_generation_required",
                    "request_generation_must_be_positive",
                )
            })
            .or_else(|| required_strings(&["ct", "n"])),
        Some("token.refresh.ok") => required_strings(&["request_id"])
            .or_else(subject)
            .or_else(|| {
                generation(
                    "generation",
                    "generation_required",
                    "generation_must_be_positive",
                )
            })
            .or_else(|| required_strings(&["ct", "n"])),
        Some("token.refresh.fail") => required_strings(&["request_id"])
            .or_else(subject)
            .or_else(|| required_strings(&["reason"]))
            .or_else(|| {
                frame
                    .get("close")
                    .is_some_and(|value| !value.is_boolean())
                    .then_some("close_invalid")
            }),
        // S1i3 K3.5：这两条只校验 wire-v1.json 里静态样张的形状（"origin_connection_id
        // 必须存在"），不驱动、也不消费 relay 侧 room-do.js 真实的转发/盖章/落路由
        // 逻辑——「样张有测试」不等于「转发代码有测试」。真正驱动真路径、钉住 relay
        // 实际转发出的帧的是 remote-relay/test/room-do.test.js 里
        // 「pair.hello：relay 转发前盖章 origin_connection_id」与
        // 「pair.done：relay 转发前盖章 origin_connection_id」两条真路径测试
        // （别再写死行号——行号会随后续插入测试漂移，陈旧的行号引用比没有引用更坏）。
        Some("pair.hello") => required_strings(&[
            "room",
            "remote_pub",
            "token_ct",
            "token_n",
            "origin_connection_id",
        ])
        .or_else(|| {
            (!frame["room"].as_str().is_some_and(|room| {
                room.len() == 32
                    && room
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }))
            .then_some("room_invalid")
        }),
        Some("pair.done") => required_strings(&[
            "room",
            "device_id",
            "confirm_ct",
            "confirm_n",
            "origin_connection_id",
        ])
        .or_else(|| {
            (!frame["room"].as_str().is_some_and(|room| {
                room.len() == 32
                    && room
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }))
            .then_some("room_invalid")
        }),
        Some("pair.ready") => required_strings(&["room", "device_id", "ct", "n"]).or_else(|| {
            (!frame["room"].as_str().is_some_and(|room| {
                room.len() == 32
                    && room
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }))
            .then_some("room_invalid")
        }),
        Some("pair.accept") => {
            if frame.get("capability_token").is_some() || frame.get("refresh_token").is_some() {
                return Some("plaintext_token_forbidden");
            }
            required_strings(&[
                "room",
                "device_id",
                "k_room_ct",
                "k_room_n",
                "tokens_ct",
                "tokens_n",
            ])
            .or_else(|| {
                (!frame["room"].as_str().is_some_and(|room| {
                    room.len() == 32
                        && room
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }))
                .then_some("room_invalid")
            })
        }
        _ => Some("frame_type_invalid"),
    }
}

fn wire_v1_collect_named_values<'a>(value: &'a Value, field: &str, values: &mut Vec<&'a Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                wire_v1_collect_named_values(item, field, values);
            }
        }
        Value::Object(object) => {
            for (key, child) in object {
                if key == field {
                    values.push(child);
                }
                wire_v1_collect_named_values(child, field, values);
            }
        }
        _ => {}
    }
}

fn wire_v1_subprotocol_decision<'a>(offers: &[&'a str]) -> Option<&'a str> {
    let token_offers = offers
        .iter()
        .copied()
        .filter(|offer| offer.starts_with("token."))
        .collect::<Vec<_>>();
    if !offers.contains(&"agentloom-rc-v1") || token_offers.len() != 1 {
        return None;
    }
    let token_hex = token_offers[0].strip_prefix("token.")?;
    wire_v1_is_hex64(token_hex).then_some(token_hex)
}

fn wire_v1_sha256_ascii_hex(value: &str) -> String {
    use sha2::Digest as _;

    wire_v1_lower_hex(&sha2::Sha256::digest(value.as_bytes()))
}

fn wire_v1_desktop_upgrade_decision(fixture: &Value) -> Value {
    let credential_matches = fixture
        .get("authorization")
        .and_then(Value::as_str)
        .and_then(|authorization| authorization.strip_prefix("Bearer "))
        .is_some_and(|credential| {
            wire_v1_is_hex64(credential)
                && fixture.get("credential_hex").and_then(Value::as_str) == Some(credential)
                && fixture["pre_state"]["owner_credential_hash"].as_str()
                    == Some(wire_v1_sha256_ascii_hex(credential).as_str())
        });
    if fixture["pre_state"]["tombstoned"] == Value::Bool(true) {
        return serde_json::json!({ "accept": false, "status": 410 });
    }
    if !credential_matches {
        return serde_json::json!({ "accept": false, "status": 401 });
    }
    serde_json::json!({ "accept": true, "role": "desktop", "epoch_bump": true })
}

fn wire_v1_http_body_byte_len(body: &Value) -> usize {
    body.as_str().map_or_else(
        || {
            serde_json::to_vec(body)
                .expect("HTTP fixture body must serialize")
                .len()
        },
        |body| body.len(),
    )
}

fn wire_v1_http_status_decision(fixture: &Value) -> u64 {
    let request = &fixture["request"];
    let pre_state = &fixture["pre_state"];
    if pre_state["rate_limited"] == Value::Bool(true) {
        return 429;
    }
    if pre_state["tombstoned"] == Value::Bool(true)
        || pre_state["owner"].as_str() == Some("tombstoned")
    {
        return 410;
    }

    match request["method"].as_str() {
        Some("POST") => {
            let body = &request["body"];
            if body.as_object().is_some_and(|body| {
                !body
                    .get("credential_hash")
                    .and_then(Value::as_str)
                    .is_some_and(wire_v1_is_hex64)
            }) {
                return 400;
            }
            if wire_v1_http_body_byte_len(body) > 1024 {
                return 413;
            }
            if !body.is_object() {
                return 400;
            }
            match pre_state["owner"].as_str() {
                Some("none") => 200,
                Some("same" | "other") => {
                    if body["credential_hash"] == pre_state["owner_credential_hash"] {
                        200
                    } else {
                        409
                    }
                }
                _ => 401,
            }
        }
        Some("DELETE") => {
            let credential_matches = request["headers"]["authorization"]
                .as_str()
                .and_then(|authorization| authorization.strip_prefix("Bearer "))
                .is_some_and(|credential| {
                    wire_v1_is_hex64(credential)
                        && fixture.get("credential_hex").and_then(Value::as_str) == Some(credential)
                        && pre_state["owner_credential_hash"].as_str()
                            == Some(wire_v1_sha256_ascii_hex(credential).as_str())
                });
            if credential_matches {
                200
            } else {
                401
            }
        }
        _ => 400,
    }
}

fn wire_v1_inbound_matrix_decision(scope: &str, frame_type: &str) -> Value {
    let allowed = match scope {
        "pairing" => ["pair.hello", "pair.done"].contains(&frame_type),
        "remote" => ["input", "control", "presence", "token.refresh"].contains(&frame_type),
        "refresh" => frame_type == "token.refresh",
        "desktop" => [
            "event",
            "live",
            "control.notify_hint",
            "input.ack",
            "pair.accept",
            "pair.ready",
            "token.put",
            "token.delete",
            "token.sync",
            "token.reset",
            "token.refresh.ok",
            "token.refresh.fail",
        ]
        .contains(&frame_type),
        _ => false,
    };
    if allowed {
        serde_json::json!({ "allowed": true })
    } else {
        serde_json::json!({ "allowed": false, "error": "role_forbidden" })
    }
}

fn wire_v1_time_window_decision(now_ms: u64, row: &Value) -> &'static str {
    let kind = row
        .get("kind")
        .and_then(Value::as_str)
        .expect("time-window row.kind must be a string");
    let scope = row
        .get("scope")
        .and_then(Value::as_str)
        .expect("time-window row.scope must be a string");
    let subject_state = row
        .get("subject_state")
        .and_then(Value::as_str)
        .expect("time-window row.subject_state must be a string");
    let access_expires = row
        .get("access_expires")
        .and_then(Value::as_u64)
        .expect("time-window row.access_expires must be u64");
    let valid_until = row
        .get("valid_until")
        .and_then(Value::as_u64)
        .expect("time-window row.valid_until must be u64");

    if subject_state != "active" {
        return "reject:401";
    }
    if row.get("generation").is_some()
        && row.get("generation").and_then(Value::as_u64)
            != row.get("current_generation").and_then(Value::as_u64)
    {
        return "reject:stale_generation";
    }
    if scope == "pairing" {
        if kind == "current" && valid_until == access_expires && now_ms < access_expires {
            return "scope:pairing";
        }
        return "reject:401";
    }
    if kind == "current" && now_ms < access_expires {
        return "scope:remote";
    }
    if kind == "current" && now_ms < valid_until {
        return "scope:refresh";
    }
    if kind == "prev" && now_ms < valid_until {
        return "scope:refresh";
    }
    "reject:401"
}

fn wire_v1_ttl_cap_ms(cap: &str) -> Option<u64> {
    match cap {
        "pairing" => Some(330_000),
        "access" => Some(3_900_000),
        "prev" => Some(172_800_000),
        "refresh_until" => Some(2_592_000_000),
        _ => None,
    }
}

#[test]
fn shared_wire_v1_token_plane_layers_structurally_valid() {
    let fixtures = wire_v1_fixtures();
    let fixtures = fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array");
    let mut names = std::collections::HashSet::new();
    let mut directions = WIRE_V1_TOKEN_LAYERS
        .into_iter()
        .filter(|layer| !["aad-kat", "chain"].contains(layer))
        .map(|layer| (layer, (0_u32, 0_u32)))
        .collect::<HashMap<_, _>>();
    let mut aad_kat_count = 0_u32;
    let mut aad_kat_kinds = std::collections::HashSet::new();

    for fixture in fixtures {
        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("fixture name must be a string");
        assert!(names.insert(name), "duplicate fixture name: {name}");

        let Some(layer_value) = fixture.get("layer") else {
            continue;
        };
        let layer = layer_value
            .as_str()
            .unwrap_or_else(|| panic!("{name}: layer must be a string"));
        assert!(
            WIRE_V1_TOKEN_LAYERS.contains(&layer),
            "{name}: unknown layer {layer}"
        );
        let expect = fixture
            .get("expect")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{name}: expect must be an object"));
        if layer == "aad-kat" {
            aad_kat_count += 1;
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
            continue;
        }
        if layer == "chain" {
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
            continue;
        }
        let direction = directions
            .get_mut(layer)
            .unwrap_or_else(|| panic!("{name}: layer direction missing"));

        match layer {
            "token-frame" => {
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
                        hashes.iter().any(|hash| {
                            hash.as_str().map_or(true, |value| !wire_v1_is_hex64(value))
                        }),
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
            "subprotocol" => {
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
            "http" => {
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
                        .is_some_and(
                            |owner| ["none", "same", "other", "tombstoned"].contains(&owner)
                        ),
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
            "desktop-upgrade" => {
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
            "inbound-matrix" => {
                assert!(
                    fixture
                        .get("scope")
                        .and_then(Value::as_str)
                        .is_some_and(|scope| {
                            ["pairing", "remote", "refresh", "desktop"].contains(&scope)
                        }),
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
            "time-window" => {
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
            "ttl-clamp" => {
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
            _ => unreachable!("layer whitelist checked"),
        }
    }

    assert_eq!(aad_kat_count, 4, "aad-kat: exactly four cases required");
    assert_eq!(
        aad_kat_kinds,
        std::collections::HashSet::from([
            "pair-ready",
            "pair-accept-tokens",
            "token.refresh",
            "token.refresh.ok"
        ]),
        "aad-kat: §9.5 kind coverage"
    );

    for (layer, (passing, rejecting)) in directions {
        assert!(
            passing > 0,
            "{layer}: at least one passing/unchanged case required"
        );
        assert!(
            rejecting > 0,
            "{layer}: at least one rejecting/clamped case required"
        );
    }
}

#[test]
fn shared_wire_v1_token_frame_expectations_recomputed_from_spec() {
    let fixtures = wire_v1_fixtures();
    for fixture in fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array")
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("token-frame"))
    {
        let name = fixture["name"]
            .as_str()
            .expect("fixture name must be a string");
        let error = wire_v1_token_frame_error(&fixture["frame"]);
        let expected_valid = fixture["expect"]["valid"]
            .as_bool()
            .expect("token-frame expect.valid must be a bool");
        assert_eq!(error.is_none(), expected_valid, "{name}");
        if let Some(error) = error {
            assert!(
                fixture["expect"]["errors"]
                    .as_array()
                    .is_some_and(|errors| errors.iter().any(|value| value.as_str() == Some(error))),
                "{name}: missing {error}"
            );
        }
    }
}

#[test]
fn shared_wire_v1_subprotocol_cases_match_spec() {
    let fixtures = wire_v1_fixtures();
    let fixtures = fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array");

    for fixture in fixtures
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("subprotocol"))
    {
        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("fixture name must be a string");
        let offers = fixture
            .get("offers")
            .and_then(Value::as_array)
            .expect("subprotocol offers must be an array")
            .iter()
            .map(|offer| {
                offer
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}: offer must be a string"))
            })
            .collect::<Vec<_>>();
        let actual = wire_v1_subprotocol_decision(&offers);
        let expect = fixture
            .get("expect")
            .and_then(Value::as_object)
            .expect("subprotocol expect must be an object");
        let accept = expect
            .get("accept")
            .and_then(Value::as_bool)
            .expect("subprotocol expect.accept must be a bool");
        assert_eq!(actual.is_some(), accept, "{name}");
        if let Some(token_hex) = actual {
            assert_eq!(
                expect.get("echo").and_then(Value::as_str),
                Some("agentloom-rc-v1"),
                "{name}"
            );
            assert_eq!(
                expect.get("token_hex").and_then(Value::as_str),
                Some(token_hex),
                "{name}"
            );
        }
    }
}

#[test]
fn shared_wire_v1_desktop_upgrade_cases_recompute_bearer_ascii_hex() {
    let fixtures = wire_v1_fixtures();
    for fixture in fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array")
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("desktop-upgrade"))
    {
        let name = fixture["name"]
            .as_str()
            .expect("fixture name must be a string");
        assert_eq!(
            wire_v1_desktop_upgrade_decision(fixture),
            fixture["expect"],
            "{name}"
        );
    }
}

#[test]
fn shared_wire_v1_inbound_matrix_matches_hard_coded_fail_closed_table() {
    let fixtures = wire_v1_fixtures();
    for fixture in fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array")
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("inbound-matrix"))
    {
        let name = fixture["name"]
            .as_str()
            .expect("fixture name must be a string");
        let scope = fixture["scope"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: scope must be a string"));
        let frame_type = fixture["frame_t"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: frame_t must be a string"));
        assert_eq!(
            wire_v1_inbound_matrix_decision(scope, frame_type),
            fixture["expect"],
            "{name}"
        );
    }
}

#[test]
fn shared_wire_v1_time_window_decisions_match_spec() {
    let fixtures = wire_v1_fixtures();
    let fixtures = fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array");

    for fixture in fixtures
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("time-window"))
    {
        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("fixture name must be a string");
        let now_ms = fixture
            .get("now_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{name}: now_ms must be u64"));
        let row = fixture
            .get("row")
            .unwrap_or_else(|| panic!("{name}: row must exist"));
        let expected = fixture
            .get("expect")
            .and_then(|expect| expect.get("decision"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: expect.decision must be a string"));
        assert_eq!(
            wire_v1_time_window_decision(now_ms, row),
            expected,
            "{name}"
        );
    }
}

#[test]
fn shared_wire_v1_ttl_clamp_cases_match_spec() {
    let fixtures = wire_v1_fixtures();
    let fixtures = fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array");

    for fixture in fixtures
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("ttl-clamp"))
    {
        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("fixture name must be a string");
        let cap = fixture
            .get("cap")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: cap must be a string"));
        let cap_ms = fixture
            .get("cap_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{name}: cap_ms must be u64"));
        assert_eq!(wire_v1_ttl_cap_ms(cap), Some(cap_ms), "{name}: cap table");
        let relay_now_ms = fixture
            .get("relay_now_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{name}: relay_now_ms must be u64"));
        let input_ms = fixture
            .get("input_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{name}: input_ms must be u64"));
        let stored_ms = fixture
            .get("expect")
            .and_then(|expect| expect.get("stored_ms"))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{name}: expect.stored_ms must be u64"));
        let expected = input_ms.min(relay_now_ms + cap_ms + 120_000);
        assert_eq!(stored_ms, expected, "{name}");
    }
}

#[test]
fn shared_wire_v1_chain_recomputes_pairing_and_device_paths() {
    let fixtures = wire_v1_fixtures();
    for fixture in fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array")
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("chain"))
    {
        let name = fixture["name"]
            .as_str()
            .expect("fixture name must be a string");
        let token_hex = fixture
            .get("connect_token_hex")
            .or_else(|| fixture.get("capability_token_hex"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: access token must be a string"));
        let token_hash_hex = fixture["token_hash_hex"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: token_hash_hex must be a string"));
        assert_eq!(
            wire_v1_sha256_ascii_hex(token_hex),
            token_hash_hex,
            "{name}: sha256(access token ASCII)"
        );

        let offers = fixture["subprotocol_offer"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: subprotocol_offer must be an array"))
            .iter()
            .map(|offer| {
                offer
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}: offer must be a string"))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            wire_v1_subprotocol_decision(&offers),
            Some(token_hex),
            "{name}: subprotocol offer"
        );

        let put_frame = &fixture["put_frame"];
        assert_eq!(
            wire_v1_token_frame_error(put_frame),
            None,
            "{name}: put frame"
        );
        assert_eq!(
            put_frame["current"]["token_hash"].as_str(),
            Some(token_hash_hex),
            "{name}: put token_hash"
        );
        assert_eq!(
            put_frame["scope"].as_str(),
            fixture["expect"]["scope"].as_str(),
            "{name}: put scope"
        );

        let window = &fixture["window"];
        assert_eq!(
            put_frame["current"]["access_expires"], window["access_expires"],
            "{name}: put/window access_expires"
        );
        let valid_until = if put_frame["scope"].as_str() == Some("remote") {
            &put_frame["current"]["refresh_until"]
        } else {
            &put_frame["current"]["access_expires"]
        };
        assert_eq!(
            valid_until, &window["valid_until"],
            "{name}: put/window valid_until"
        );
        let window_kind = window["kind"].as_str().unwrap_or("current");
        let window_scope = window["scope"]
            .as_str()
            .or_else(|| put_frame["scope"].as_str())
            .unwrap_or_else(|| panic!("{name}: window scope must be a string"));
        let subject_state = window["subject_state"].as_str().unwrap_or("active");
        let row = serde_json::json!({
            "kind": window_kind,
            "scope": window_scope,
            "subject_state": subject_state,
            "access_expires": window["access_expires"],
            "valid_until": window["valid_until"],
        });
        let now_ms = window["now_ms"]
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: window.now_ms must be u64"));
        let expected_scope = format!(
            "scope:{}",
            fixture["expect"]["scope"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: expect.scope must be a string"))
        );
        assert_eq!(
            wire_v1_time_window_decision(now_ms, &row),
            expected_scope,
            "{name}: access window"
        );
    }
}
