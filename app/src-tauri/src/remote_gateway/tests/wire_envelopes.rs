#![cfg(test)]

use super::*;
#[test]
fn builds_envelope_json_with_explicit_null_fields() {
    let meta = EnvelopeMeta {
        v: 1,
        room: "0123456789abcdef0123456789abcdef".to_owned(),
        epoch: 7,
        kind: "live".to_owned(),
        session: Some("sess-1".to_owned()),
        command_id: None,
    };
    let envelope = build_envelope_json(&meta, "fixed-ct", "fixed-n", 123_456, None);

    assert_eq!(envelope["v"], 1);
    assert_eq!(envelope["room"], meta.room);
    assert_eq!(envelope["epoch"], 7);
    assert_eq!(envelope["kind"], "live");
    assert_eq!(envelope["session"], "sess-1");
    assert_eq!(envelope["command_id"], serde_json::Value::Null);
    assert_eq!(envelope["seq"], serde_json::Value::Null);
    assert_eq!(envelope["ct"], "fixed-ct");
    assert_eq!(envelope["n"], "fixed-n");
    assert_eq!(envelope["ts"], 123_456);
}

#[test]
fn sealed_envelope_uses_lowercase_room_and_standard_twelve_byte_nonce() {
    let meta = EnvelopeMeta {
        v: 1,
        room: "0123456789abcdef0123456789abcdef".to_owned(),
        epoch: 7,
        kind: "event".to_owned(),
        session: Some("sess-1".to_owned()),
        command_id: None,
    };
    let (ct, nonce) = crate::remote_crypto::seal(&[9_u8; 32], &meta, br#"{"t":"probe"}"#);
    let envelope = build_envelope_json(&meta, &ct, &nonce, 42, Some("client-1"));

    assert_eq!(envelope["room"], meta.room.to_lowercase());
    assert!(!envelope["room"]
        .as_str()
        .unwrap()
        .chars()
        .any(|c| c.is_ascii_uppercase()));
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(envelope["n"].as_str().unwrap())
            .unwrap()
            .len(),
        12
    );
    assert!(base64::engine::general_purpose::STANDARD
        .decode(&ct)
        .is_ok());
}

#[test]
fn client_msg_id_derivation_matches_every_shared_uuid_v5_vector() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/client-msg-id-derivation-v1.json"
    ))
    .expect("client-msg-id fixture must be valid JSON");
    let vectors = fixture["vectors"]
        .as_array()
        .expect("client-msg-id fixture vectors must be an array");

    assert_eq!(vectors.len(), 6);
    for vector in vectors {
        let name = vector["name"]
            .as_str()
            .expect("vector name must be a string");
        let expected = vector["expect"]
            .as_str()
            .expect("vector expectation must be a string");
        assert_eq!(
            derive_client_msg_id(name),
            expected,
            "KAT failed for {name}"
        );
    }
}

#[test]
fn random_client_msg_id_is_valid_uuid_v4_text() {
    let client_msg_id = try_random_client_msg_id().expect("OS entropy should be available");

    assert_eq!(client_msg_id.len(), 36);
    assert!(is_valid_client_msg_id(&client_msg_id));
}

#[test]
fn random_client_msg_id_returns_none_when_entropy_fails() {
    let _guard = ForceClientMsgIdEntropyFailureGuard::new();

    assert_eq!(try_random_client_msg_id(), None);
}
