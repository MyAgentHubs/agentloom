#![allow(dead_code)]

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

const HKDF_INFO: &[u8] = b"agentloom-rc-v1";
const CONNECT_HKDF_INFO: &[u8] = b"agentloom-rc-connect-v1";
const NONCE_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptoError {
    BadBase64,
    BadLength,
    DecryptFailed,
    NonContributory,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadBase64 => formatter.write_str("invalid base64"),
            Self::BadLength => formatter.write_str("invalid decoded length"),
            Self::DecryptFailed => formatter.write_str("decryption failed"),
            Self::NonContributory => formatter.write_str("non-contributory Diffie-Hellman result"),
        }
    }
}

impl std::error::Error for CryptoError {}

pub(crate) struct EnvelopeMeta {
    pub v: u32,
    pub room: String,
    pub epoch: u64,
    pub kind: String,
    pub session: Option<String>,
    pub command_id: Option<String>,
}

pub(crate) fn derive_k_pair(
    my_secret: &[u8; 32],
    their_public: &[u8; 32],
    pairing_code: &str,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let secret = StaticSecret::from(*my_secret);
    let public = PublicKey::from(*their_public);
    let shared_secret = secret.diffie_hellman(&public);
    if !shared_secret.was_contributory() {
        return Err(CryptoError::NonContributory);
    }
    let hkdf = Hkdf::<Sha256>::new(Some(pairing_code.as_bytes()), shared_secret.as_bytes());
    let mut key = [0_u8; 32];
    hkdf.expand(HKDF_INFO, &mut key)
        .expect("32-byte HKDF-SHA256 output is valid");
    Ok(Zeroizing::new(key))
}

/// §9.5 connect-token KDF. The IKM is the original 64-byte lowercase ASCII hex text;
/// the salt is explicitly empty and is intentionally unrelated to K_pair's pairing-code salt.
pub(crate) fn derive_connect_token(pairing_token_hex: &str) -> String {
    use std::fmt::Write as _;

    let hkdf = Hkdf::<Sha256>::new(Some(&[]), pairing_token_hex.as_bytes());
    let mut token = Zeroizing::new([0_u8; 32]);
    hkdf.expand(CONNECT_HKDF_INFO, token.as_mut())
        .expect("32-byte HKDF-SHA256 output is valid");
    let mut encoded = String::with_capacity(64);
    for byte in token.iter() {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

pub(crate) fn generate_x25519_keypair() -> ([u8; 32], [u8; 32]) {
    let secret_bytes = generate_key_32();
    let secret = StaticSecret::from(*secret_bytes);
    let public = PublicKey::from(&secret);
    (secret.to_bytes(), public.to_bytes())
}

pub(crate) fn generate_key_32() -> Zeroizing<[u8; 32]> {
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key).expect("operating system CSPRNG unavailable");
    Zeroizing::new(key)
}

pub(crate) fn wrap_key(kek: &[u8; 32], key: &[u8; 32]) -> (String, String) {
    let cipher = Aes256Gcm::new_from_slice(kek).expect("AES-256-GCM requires a 32-byte key");
    let nonce_bytes = generate_nonce();
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), key.as_slice())
        .expect("AES-256-GCM encryption failed");
    (STANDARD.encode(ciphertext), STANDARD.encode(nonce_bytes))
}

pub(crate) fn unwrap_key(
    kek: &[u8; 32],
    ct_b64: &str,
    n_b64: &str,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let (ciphertext, nonce_bytes) = decode_ciphertext_and_nonce(ct_b64, n_b64)?;
    let cipher = Aes256Gcm::new_from_slice(kek).expect("AES-256-GCM requires a 32-byte key");
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
            .map_err(|_| CryptoError::DecryptFailed)?,
    );
    let key = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::BadLength)?;
    Ok(Zeroizing::new(key))
}

pub(crate) fn build_aad(meta: &EnvelopeMeta) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        meta.v,
        meta.room,
        meta.epoch,
        meta.kind,
        meta.session.as_deref().unwrap_or_default(),
        meta.command_id.as_deref().unwrap_or_default()
    )
}

pub(crate) fn seal(key: &[u8; 32], meta: &EnvelopeMeta, plaintext: &[u8]) -> (String, String) {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256-GCM requires a 32-byte key");
    let nonce_bytes = generate_nonce();
    let aad = build_aad(meta);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .expect("AES-256-GCM encryption failed");
    (STANDARD.encode(ciphertext), STANDARD.encode(nonce_bytes))
}

pub(crate) fn open(
    key: &[u8; 32],
    meta: &EnvelopeMeta,
    ct_b64: &str,
    n_b64: &str,
) -> Result<Vec<u8>, CryptoError> {
    let (ciphertext, nonce_bytes) = decode_ciphertext_and_nonce(ct_b64, n_b64)?;
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256-GCM requires a 32-byte key");
    let aad = build_aad(meta);
    cipher
        .decrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| CryptoError::DecryptFailed)
}

fn generate_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).expect("operating system CSPRNG unavailable");
    nonce
}

fn decode_ciphertext_and_nonce(
    ct_b64: &str,
    n_b64: &str,
) -> Result<(Vec<u8>, [u8; NONCE_LEN]), CryptoError> {
    let ciphertext = STANDARD
        .decode(ct_b64)
        .map_err(|_| CryptoError::BadBase64)?;
    let nonce = STANDARD
        .decode(n_b64)
        .map_err(|_| CryptoError::BadBase64)?
        .try_into()
        .map_err(|_| CryptoError::BadLength)?;
    Ok((ciphertext, nonce))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    const ROOM: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn meta_with_optional_fields(session: Option<&str>, command_id: Option<&str>) -> EnvelopeMeta {
        EnvelopeMeta {
            v: 1,
            room: ROOM.to_owned(),
            epoch: 7,
            kind: "input".to_owned(),
            session: session.map(str::to_owned),
            command_id: command_id.map(str::to_owned),
        }
    }

    fn sealed_fixture() -> (Zeroizing<[u8; 32]>, EnvelopeMeta, String, String) {
        let key = generate_key_32();
        let meta = meta_with_optional_fields(Some("sess-1"), Some("cmd-42"));
        let (ct_b64, n_b64) = seal(&key, &meta, b"authenticated payload");
        (key, meta, ct_b64, n_b64)
    }

    fn mutate_standard_base64(encoded: &str) -> String {
        let mut bytes = STANDARD.decode(encoded).unwrap();
        bytes[0] ^= 1;
        STANDARD.encode(bytes)
    }

    #[test]
    fn build_aad_matches_relay_with_command_id() {
        let meta = meta_with_optional_fields(Some("sess-1"), Some("cmd-42"));

        assert_eq!(
            build_aad(&meta),
            "1|aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa|7|input|sess-1|cmd-42"
        );
    }

    #[test]
    fn build_aad_matches_relay_with_empty_optional_fields() {
        let meta = EnvelopeMeta {
            v: 1,
            room: ROOM.to_owned(),
            epoch: 3,
            kind: "presence".to_owned(),
            session: None,
            command_id: None,
        };

        assert_eq!(
            build_aad(&meta),
            "1|aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa|3|presence||"
        );
    }

    #[test]
    fn pairing_key_is_symmetric_and_pairing_code_bound() {
        let (alice_secret, alice_public) = generate_x25519_keypair();
        let (bob_secret, bob_public) = generate_x25519_keypair();

        let alice_key = derive_k_pair(&alice_secret, &bob_public, "123456").unwrap();
        let bob_key = derive_k_pair(&bob_secret, &alice_public, "123456").unwrap();
        let other_code_key = derive_k_pair(&alice_secret, &bob_public, "654321").unwrap();

        assert_eq!(alice_key, bob_key);
        assert_ne!(alice_key, other_code_key);
    }

    #[test]
    fn derive_k_pair_rejects_all_zero_peer_public_key() {
        let (first_secret, _) = generate_x25519_keypair();
        let (second_secret, _) = generate_x25519_keypair();
        let all_zero_public = [0_u8; 32];

        assert_ne!(first_secret, second_secret);
        assert_eq!(
            derive_k_pair(&first_secret, &all_zero_public, "123456"),
            Err(CryptoError::NonContributory)
        );
        assert_eq!(
            derive_k_pair(&second_secret, &all_zero_public, "123456"),
            Err(CryptoError::NonContributory)
        );
    }

    // T6A2：DH 部分是 RFC 7748 §5.2 X25519 测试向量，HKDF 部分曾用 Node.js 内置 crypto 与独立
    // Python HMAC-SHA256 HKDF 交叉核验过（核验方法与出处见共享 fixture 的 `source` 字段）。
    // 这组向量原是桌面测试与 Web 客户端测试各自维护的内联复制，现抽成
    // 跨端共享真相源，见 `remote-relay/fixtures/crypto-kat-v1.json`。
    #[test]
    fn derive_k_pair_matches_independent_kat_vector() {
        fn decode_hex_32(input: &str) -> [u8; 32] {
            let mut output = [0_u8; 32];
            for (index, byte) in output.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&input[index * 2..index * 2 + 2], 16).unwrap();
            }
            output
        }

        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../remote-relay/fixtures/crypto-kat-v1.json"
        ))
        .expect("crypto-kat-v1 fixture must be valid JSON");

        let dh = &fixture["x25519_dh"];
        let my_secret = decode_hex_32(
            dh["my_secret_hex"]
                .as_str()
                .expect("x25519_dh.my_secret_hex must be present"),
        );
        let their_public = decode_hex_32(
            dh["their_public_hex"]
                .as_str()
                .expect("x25519_dh.their_public_hex must be present"),
        );
        let dh_expected_shared_hex = dh["expected_shared_hex"]
            .as_str()
            .expect("x25519_dh.expected_shared_hex must be present");
        let expected_shared = decode_hex_32(dh_expected_shared_hex);

        let k_pair_hkdf = &fixture["k_pair_hkdf"];
        let k_pair_shared_secret_hex = k_pair_hkdf["shared_secret_hex"]
            .as_str()
            .expect("k_pair_hkdf.shared_secret_hex must be present");
        assert_eq!(
            k_pair_shared_secret_hex, dh_expected_shared_hex,
            "fixture internal consistency: k_pair_hkdf must chain from x25519_dh's shared secret"
        );
        let pairing_code = k_pair_hkdf["pairing_code"]
            .as_str()
            .expect("k_pair_hkdf.pairing_code must be present");
        let expected_k_pair = decode_hex_32(
            k_pair_hkdf["expected_k_pair_hex"]
                .as_str()
                .expect("k_pair_hkdf.expected_k_pair_hex must be present"),
        );

        let shared_secret =
            StaticSecret::from(my_secret).diffie_hellman(&PublicKey::from(their_public));
        assert_eq!(shared_secret.as_bytes(), &expected_shared);
        assert_eq!(
            *derive_k_pair(&my_secret, &their_public, pairing_code).unwrap(),
            expected_k_pair
        );
    }

    #[test]
    fn derive_connect_token_consumes_every_shared_kat_vector() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../../../remote-relay/fixtures/connect-kdf-v1.json"
        ))
        .expect("connect-kdf-v1 fixture must be valid JSON");
        let fixtures = fixtures
            .as_array()
            .expect("connect-kdf-v1 fixture root must be an array");
        assert_eq!(
            fixtures.len(),
            3,
            "all connect-token KAT cases are required"
        );

        for fixture in fixtures {
            let name = fixture["name"].as_str().expect("fixture name");
            let pairing_token = fixture["pairing_token_hex"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: pairing_token_hex"));
            let expected = fixture["expect"]["connect_token_hex"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: expect.connect_token_hex"));
            let derived = derive_connect_token(pairing_token);

            assert_eq!(derived, expected, "{name}");
            assert_ne!(
                derived, pairing_token,
                "{name}: KDF output must differ from input"
            );
            assert!(
                !derived.starts_with(&pairing_token[..16]),
                "{name}: KDF output must not preserve the input prefix"
            );
        }
    }

    #[test]
    fn wrap_key_round_trips() {
        let kek = generate_key_32();
        let key = generate_key_32();
        let (ct_b64, n_b64) = wrap_key(&kek, &key);

        assert_eq!(unwrap_key(&kek, &ct_b64, &n_b64).unwrap(), key);
    }

    #[test]
    fn unwrap_key_rejects_wrong_kek() {
        let kek = generate_key_32();
        let wrong_kek = generate_key_32();
        let key = generate_key_32();
        let (ct_b64, n_b64) = wrap_key(&kek, &key);

        assert_eq!(
            unwrap_key(&wrong_kek, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn unwrap_key_rejects_correctly_decrypted_but_wrong_length_plaintext() {
        let kek = generate_key_32();
        let cipher = Aes256Gcm::new_from_slice(&*kek).expect("AES-256-GCM requires a 32-byte key");
        let nonce_bytes = generate_nonce();
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), [0_u8; 16].as_slice())
            .expect("AES-256-GCM encryption failed");

        assert_eq!(
            unwrap_key(
                &kek,
                &STANDARD.encode(ciphertext),
                &STANDARD.encode(nonce_bytes)
            ),
            Err(CryptoError::BadLength)
        );
    }

    #[test]
    fn seal_open_round_trips_with_optional_fields_none() {
        let key = generate_key_32();
        let meta = meta_with_optional_fields(None, None);
        let plaintext = b"no optional fields";
        let (ct_b64, n_b64) = seal(&key, &meta, plaintext);

        assert_eq!(open(&key, &meta, &ct_b64, &n_b64).unwrap(), plaintext);
    }

    #[test]
    fn seal_open_round_trips_with_optional_fields_some() {
        let key = generate_key_32();
        let meta = meta_with_optional_fields(Some("sess-1"), Some("cmd-42"));
        let plaintext = b"all optional fields";
        let (ct_b64, n_b64) = seal(&key, &meta, plaintext);

        assert_eq!(open(&key, &meta, &ct_b64, &n_b64).unwrap(), plaintext);
    }

    #[test]
    fn open_rejects_wrong_key() {
        let key = generate_key_32();
        let wrong_key = generate_key_32();
        let meta = meta_with_optional_fields(Some("sess-1"), Some("cmd-42"));
        let (ct_b64, n_b64) = seal(&key, &meta, b"authenticated payload");

        assert_ne!(key, wrong_key);
        assert_eq!(
            open(&wrong_key, &meta, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn seal_uses_twelve_byte_standard_base64_nonce_and_standard_ciphertext() {
        let key = generate_key_32();
        let meta = meta_with_optional_fields(None, None);
        let (ct_b64, n_b64) = seal(&key, &meta, b"base64 shape");
        let standard_base64 = |value: &str| {
            value.len() % 4 == 0
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        };

        assert_eq!(STANDARD.decode(&n_b64).unwrap().len(), 12);
        assert!(standard_base64(&ct_b64));
        assert!(standard_base64(&n_b64));
    }

    #[test]
    fn open_rejects_changed_v() {
        let (key, mut meta, ct_b64, n_b64) = sealed_fixture();
        meta.v += 1;
        assert_eq!(
            open(&key, &meta, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn open_rejects_changed_room() {
        let (key, mut meta, ct_b64, n_b64) = sealed_fixture();
        meta.room = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned();
        assert_eq!(
            open(&key, &meta, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn open_rejects_changed_epoch() {
        let (key, mut meta, ct_b64, n_b64) = sealed_fixture();
        meta.epoch += 1;
        assert_eq!(
            open(&key, &meta, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn open_rejects_changed_kind() {
        let (key, mut meta, ct_b64, n_b64) = sealed_fixture();
        meta.kind = "control".to_owned();
        assert_eq!(
            open(&key, &meta, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn open_rejects_changed_session() {
        let (key, mut meta, ct_b64, n_b64) = sealed_fixture();
        meta.session = Some("sess-2".to_owned());
        assert_eq!(
            open(&key, &meta, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn open_rejects_changed_command_id() {
        let (key, mut meta, ct_b64, n_b64) = sealed_fixture();
        meta.command_id = Some("cmd-43".to_owned());
        assert_eq!(
            open(&key, &meta, &ct_b64, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn open_rejects_changed_ciphertext_byte() {
        let (key, meta, ct_b64, n_b64) = sealed_fixture();
        let changed_ct = mutate_standard_base64(&ct_b64);
        assert_eq!(
            open(&key, &meta, &changed_ct, &n_b64),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn open_rejects_changed_nonce_byte() {
        let (key, meta, ct_b64, n_b64) = sealed_fixture();
        let changed_nonce = mutate_standard_base64(&n_b64);
        assert_eq!(
            open(&key, &meta, &ct_b64, &changed_nonce),
            Err(CryptoError::DecryptFailed)
        );
    }

    #[test]
    fn repeated_seal_uses_unique_nonce_and_ciphertext() {
        let key = generate_key_32();
        let meta = meta_with_optional_fields(Some("sess-1"), Some("cmd-42"));
        let (first_ct, first_nonce) = seal(&key, &meta, b"same plaintext");
        let (second_ct, second_nonce) = seal(&key, &meta, b"same plaintext");

        assert_ne!(first_nonce, second_nonce);
        assert_ne!(first_ct, second_ct);
    }

    #[test]
    fn unwrap_key_rejects_bad_base64_without_panicking() {
        let kek = generate_key_32();
        assert_eq!(
            unwrap_key(&kek, "not valid!", "also invalid!"),
            Err(CryptoError::BadBase64)
        );
    }

    #[test]
    fn open_rejects_bad_base64_without_panicking() {
        let key = generate_key_32();
        let meta = meta_with_optional_fields(None, None);
        assert_eq!(
            open(&key, &meta, "not valid!", "also invalid!"),
            Err(CryptoError::BadBase64)
        );
    }

    #[test]
    fn valid_base64_nonce_with_wrong_length_is_rejected() {
        let key = generate_key_32();
        let meta = meta_with_optional_fields(None, None);
        let nonce = STANDARD.encode([0_u8; 11]);

        assert_eq!(
            open(&key, &meta, &STANDARD.encode([0_u8; 16]), &nonce),
            Err(CryptoError::BadLength)
        );
        assert_eq!(
            unwrap_key(&key, &STANDARD.encode([0_u8; 48]), &nonce),
            Err(CryptoError::BadLength)
        );
    }
}
