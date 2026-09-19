#![cfg(test)]

use super::*;
fn wire_v1_fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .expect("wire-v1 fixture must be valid JSON")
}

fn wire_v1_is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn wire_v1_lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

mod crypto_vectors;
mod token_plane;
