#![cfg(test)]

use super::*;
fn wire_v1_decode_hex_32(value: &str, name: &str, field: &str) -> [u8; 32] {
    assert!(wire_v1_is_hex64(value), "{name}: {field} must be hex64");
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair =
            std::str::from_utf8(pair).unwrap_or_else(|_| panic!("{name}: {field} must be ASCII"));
        decoded[index] = u8::from_str_radix(pair, 16)
            .unwrap_or_else(|_| panic!("{name}: {field} invalid hex at byte {index}"));
    }
    decoded
}

#[test]
fn shared_connect_kdf_v1_vectors_match_ascii_hex_spec() {
    use sha2::Digest as _;

    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../../remote-relay/fixtures/connect-kdf-v1.json"
    ))
    .expect("connect-kdf-v1 fixture must be valid JSON");
    let fixtures = fixtures
        .as_array()
        .expect("connect-kdf-v1 fixture root must be an array");
    assert!(
        fixtures.len() >= 3,
        "at least three connect-KDF vectors are required"
    );
    let mut names = std::collections::HashSet::new();

    for fixture in fixtures {
        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("connect-KDF fixture name must be a string");
        assert!(names.insert(name), "duplicate fixture name: {name}");
        let pairing_token_hex = fixture
            .get("pairing_token_hex")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: pairing_token_hex must be a string"));
        assert!(
            wire_v1_is_hex64(pairing_token_hex),
            "{name}: pairing_token_hex must be lowercase hex64"
        );
        let expect = fixture
            .get("expect")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{name}: expect must be an object"));
        let expected_connect_token_hex = expect
            .get("connect_token_hex")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: expect.connect_token_hex must be a string"));
        let expected_token_hash_hex = expect
            .get("token_hash_hex")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: expect.token_hash_hex must be a string"));
        assert!(
            wire_v1_is_hex64(expected_connect_token_hex),
            "{name}: expect.connect_token_hex"
        );
        assert!(
            wire_v1_is_hex64(expected_token_hash_hex),
            "{name}: expect.token_hash_hex"
        );

        let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(None, pairing_token_hex.as_bytes());
        let mut connect_token = [0_u8; 32];
        hkdf.expand(b"agentloom-rc-connect-v1", &mut connect_token)
            .expect("32-byte HKDF-SHA256 output is valid");
        let connect_token_hex = wire_v1_lower_hex(&connect_token);
        let token_hash = sha2::Sha256::digest(connect_token_hex.as_bytes());
        let token_hash_hex = wire_v1_lower_hex(&token_hash);

        assert_eq!(
            connect_token_hex, expected_connect_token_hex,
            "{name}: connect_token"
        );
        assert_eq!(
            token_hash_hex, expected_token_hash_hex,
            "{name}: token_hash"
        );
    }
}

#[test]
fn shared_wire_v1_aad_kat_fixtures_build_and_decrypt() {
    let fixtures = wire_v1_fixtures();
    let fixtures = fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array");
    let mut aad_kat_count = 0_u32;

    for fixture in fixtures
        .iter()
        .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("aad-kat"))
    {
        aad_kat_count += 1;
        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("AAD KAT fixture name must be a string");
        let meta = fixture
            .get("meta")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{name}: meta must be an object"));
        let session = match meta.get("session") {
            Some(Value::String(value)) => Some(value.clone()),
            Some(Value::Null) => None,
            _ => panic!("{name}: meta.session must be string|null"),
        };
        let command_id = match meta.get("command_id") {
            Some(Value::String(value)) => Some(value.clone()),
            Some(Value::Null) => None,
            _ => panic!("{name}: meta.command_id must be string|null"),
        };
        let meta = EnvelopeMeta {
            v: meta
                .get("v")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_else(|| panic!("{name}: meta.v must fit u32")),
            room: meta
                .get("room")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: meta.room must be a string"))
                .to_owned(),
            epoch: meta
                .get("epoch")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{name}: meta.epoch must be u64")),
            kind: meta
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: meta.kind must be a string"))
                .to_owned(),
            session,
            command_id,
        };
        let expected_aad = fixture
            .get("expect")
            .and_then(Value::as_object)
            .and_then(|expect| expect.get("aad"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: expect.aad must be a string"));
        assert_eq!(
            crate::remote_crypto::build_aad(&meta),
            expected_aad,
            "{name}: AAD"
        );

        let kat = fixture
            .get("kat")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{name}: kat must be an object"));
        let key_hex = kat
            .get("key_hex")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: kat.key_hex must be a string"));
        let key = wire_v1_decode_hex_32(key_hex, name, "kat.key_hex");
        let ct_b64 = kat
            .get("ct_b64")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: kat.ct_b64 must be a string"));
        let n_b64 = kat
            .get("n_b64")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: kat.n_b64 must be a string"));
        let expected_plaintext = kat
            .get("plaintext")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: kat.plaintext must be a string"));
        let plaintext = crate::remote_crypto::open(&key, &meta, ct_b64, n_b64)
            .unwrap_or_else(|error| panic!("{name}: AAD KAT decryption failed: {error:?}"));
        assert_eq!(
            plaintext.as_slice(),
            expected_plaintext.as_bytes(),
            "{name}: plaintext"
        );
    }

    assert_eq!(
        aad_kat_count, 4,
        "exactly four AAD KAT fixtures are required"
    );
}

#[test]
fn shared_wire_v1_kat_fixtures_decrypt_and_authenticate() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .expect("wire-v1 fixture must be valid JSON");
    let fixtures = fixtures
        .as_array()
        .expect("wire-v1 fixture root must be an array");
    let mut kat_count = 0;
    let mut mutations_checked = false;

    for fixture in fixtures {
        if fixture.get("layer").and_then(Value::as_str) == Some("aad-kat") {
            continue;
        }
        let Some(kat) = fixture.get("kat").and_then(Value::as_object) else {
            continue;
        };
        kat_count += 1;

        let name = fixture
            .get("name")
            .and_then(Value::as_str)
            .expect("KAT fixture name must be a string");
        let key_hex = kat
            .get("k_room_hex")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: kat.k_room_hex must be a string"));
        assert_eq!(
            key_hex.len(),
            64,
            "{name}: kat.k_room_hex must encode exactly 32 bytes"
        );
        assert!(
            key_hex.is_ascii(),
            "{name}: kat.k_room_hex must contain only ASCII hex digits"
        );
        let mut key = [0_u8; 32];
        for (index, pair) in key_hex.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(pair)
                .unwrap_or_else(|_| panic!("{name}: kat.k_room_hex must be valid ASCII"));
            key[index] = u8::from_str_radix(pair, 16).unwrap_or_else(|_| {
                panic!("{name}: kat.k_room_hex contains invalid hex at byte {index}")
            });
        }

        let envelope = fixture
            .get("envelope")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{name}: envelope must be an object"));
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
        let meta = EnvelopeMeta {
            v: envelope
                .get("v")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_else(|| panic!("{name}: envelope.v must fit u32")),
            room: envelope
                .get("room")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: envelope.room must be a string"))
                .to_owned(),
            epoch: envelope
                .get("epoch")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{name}: envelope.epoch must be u64")),
            kind: envelope
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: envelope.kind must be a string"))
                .to_owned(),
            session,
            command_id,
        };
        let expected_aad = fixture
            .get("expect")
            .and_then(Value::as_object)
            .and_then(|expect| expect.get("aad"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: expect.aad must be a string"));
        assert_eq!(
            crate::remote_crypto::build_aad(&meta).as_bytes(),
            expected_aad.as_bytes(),
            "{name}: AAD mismatch"
        );

        let ct_b64 = envelope
            .get("ct")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: envelope.ct must be a base64 string"));
        let n_b64 = envelope
            .get("n")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: envelope.n must be a base64 string"));
        let expected_plaintext = kat
            .get("plaintext")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: kat.plaintext must be a string"));
        let plaintext = crate::remote_crypto::open(&key, &meta, ct_b64, n_b64)
            .unwrap_or_else(|error| panic!("{name}: KAT decryption failed: {error:?}"));
        assert_eq!(
            plaintext.as_slice(),
            expected_plaintext.as_bytes(),
            "{name}: plaintext mismatch"
        );

        if !mutations_checked {
            let mut tampered_ct = STANDARD
                .decode(ct_b64)
                .unwrap_or_else(|error| panic!("{name}: invalid ciphertext base64: {error}"));
            let first_byte = tampered_ct
                .first_mut()
                .unwrap_or_else(|| panic!("{name}: ciphertext must not be empty"));
            *first_byte ^= 1;
            let tampered_ct_b64 = STANDARD.encode(tampered_ct);

            // These mutations prove the test exercises AEAD authentication, not just wire shape.
            assert_eq!(
                crate::remote_crypto::open(&key, &meta, &tampered_ct_b64, n_b64),
                Err(crate::remote_crypto::CryptoError::DecryptFailed),
                "{name}: tampered ciphertext must fail authentication"
            );

            let tampered_meta = EnvelopeMeta {
                v: meta.v,
                room: meta.room.clone(),
                epoch: meta
                    .epoch
                    .checked_add(1)
                    .unwrap_or_else(|| panic!("{name}: envelope.epoch cannot be incremented")),
                kind: meta.kind.clone(),
                session: meta.session.clone(),
                command_id: meta.command_id.clone(),
            };
            assert_eq!(
                crate::remote_crypto::open(&key, &tampered_meta, ct_b64, n_b64),
                Err(crate::remote_crypto::CryptoError::DecryptFailed),
                "{name}: tampered AAD must fail authentication"
            );
            mutations_checked = true;
        }
    }

    assert!(kat_count >= 1, "at least one KAT fixture must exist");
}
