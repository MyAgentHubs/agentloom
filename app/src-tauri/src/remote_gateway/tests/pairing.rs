#![cfg(test)]

use super::*;
#[test]
fn routes_pair_frames_and_builds_provisional_accept_wire_shape() {
    let hello_calls = Arc::new(AtomicU64::new(0));
    let done_calls = Arc::new(AtomicU64::new(0));
    let hello_calls_for_handler = Arc::clone(&hello_calls);
    let done_calls_for_handler = Arc::clone(&done_calls);
    let inner = test_inner_with_pair_handlers(
        move |frame| {
            hello_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            assert_eq!(frame.room, "room-1");
            assert_eq!(frame.remote_pub, [7_u8; 32]);
            assert_eq!(frame.token_ct, "token-ciphertext");
            assert_eq!(frame.token_n, "token-nonce");
            Some(PairAcceptFrame {
                room: frame.room,
                device_id: "device-1".to_owned(),
                k_room_ct: "room-ciphertext".to_owned(),
                k_room_n: "room-nonce".to_owned(),
                tokens_ct: "tokens-ciphertext".to_owned(),
                tokens_n: "tokens-nonce".to_owned(),
                k_room: Zeroizing::new([3_u8; 32]),
            })
        },
        move |frame| {
            done_calls_for_handler.fetch_add(1, Ordering::Relaxed);
            if frame.room == "room-1" && frame.device_id == "device-1" {
                PairDoneAction::Accepted {
                    newly_paired_device_id: None,
                }
            } else {
                PairDoneAction::Rejected
            }
        },
    );
    let remote_pub = STANDARD.encode([7_u8; 32]);
    let response = handle_frame(
        &inner,
        &serde_json::json!({
            "t": "pair.hello",
            "room": "room-1",
            "remote_pub": remote_pub,
            "token_ct": "token-ciphertext",
            "token_n": "token-nonce",
            "origin_connection_id": "conn-pairing-1",
        })
        .to_string(),
        None,
    )
    .expect("accepted hello should produce pair.accept");
    assert_eq!(
        response,
        serde_json::json!({
            "t": "pair.accept",
            "room": "room-1",
            "device_id": "device-1",
            "k_room_ct": "room-ciphertext",
            "k_room_n": "room-nonce",
            "tokens_ct": "tokens-ciphertext",
            "tokens_n": "tokens-nonce",
        })
    );

    assert!(handle_frame(
        &inner,
        r#"{"t":"pair.done","room":"room-1","device_id":"device-1","origin_connection_id":"conn-pairing-1"}"#,
        None,
    )
    .is_none());
    assert_eq!(hello_calls.load(Ordering::Relaxed), 1);
    assert_eq!(done_calls.load(Ordering::Relaxed), 1);
    assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 0);
}

#[test]
fn pair_accept_serialization_never_exposes_plaintext_tokens() {
    let capability_token = "a1".repeat(32);
    let refresh_token = "b2".repeat(32);
    let room = "0123456789abcdef0123456789abcdef";
    let device_id = "11111111-1111-4111-8111-111111111111";
    let (tokens_ct, tokens_n) = crate::remote_pairing::seal_pair_accept_tokens(
        &[0x42_u8; 32],
        room,
        device_id,
        &capability_token,
        &refresh_token,
    );
    let serialized = pair_accept_json(PairAcceptFrame {
        room: room.to_owned(),
        device_id: device_id.to_owned(),
        k_room_ct: "room-ciphertext".to_owned(),
        k_room_n: "room-nonce".to_owned(),
        tokens_ct,
        tokens_n,
        k_room: Zeroizing::new([3_u8; 32]),
    })
    .to_string();

    assert!(!serialized.contains(&capability_token));
    assert!(!serialized.contains(&refresh_token));
    assert!(!serialized.contains("capability_token"));
    assert!(!serialized.contains("refresh_token"));
}

#[test]
fn wild_or_malformed_pair_frames_are_ignored_and_counted() {
    let inner = test_inner(|_| None, || None);
    let remote_pub = STANDARD.encode([7_u8; 32]);

    assert!(handle_frame(
        &inner,
        &serde_json::json!({
            "t": "pair.hello",
            "room": "room-1",
            "remote_pub": remote_pub,
            "token_ct": "ct",
            "token_n": "n",
        })
        .to_string(),
        None,
    )
    .is_none());
    assert!(handle_frame(
        &inner,
        r#"{"t":"pair.done","room":"room-1","device_id":"device-1"}"#,
        None,
    )
    .is_none());
    assert!(handle_frame(&inner, r#"{"t":"pair.hello","room":"room-1"}"#, None,).is_none());
    assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 3);
}

#[test]
fn validates_exactly_32_hexadecimal_room_id_characters() {
    assert!(is_valid_room_id("0123456789abcdef0123456789abcdef"));
    assert!(!is_valid_room_id("0123456789abcdef0123456789ABCDEF"));
    assert!(!is_valid_room_id("0123456789abcdef0123456789abcde"));
    assert!(!is_valid_room_id("0123456789abcdef0123456789abcdef0"));
    assert!(!is_valid_room_id("0123456789abcdef0123456789abcdeg"));
    assert!(!is_valid_room_id("0123456789abcdef0123456789ABCDE "));
}

#[test]
fn effective_relay_url_falls_back_to_default_when_none() {
    assert_eq!(
        effective_relay_url(None),
        Some(DEFAULT_PUBLIC_RELAY_URL.to_owned())
    );
}

#[test]
fn effective_relay_url_falls_back_to_default_when_blank() {
    assert_eq!(
        effective_relay_url(Some("   ".to_owned())),
        Some(DEFAULT_PUBLIC_RELAY_URL.to_owned())
    );
    assert_eq!(
        effective_relay_url(Some(String::new())),
        Some(DEFAULT_PUBLIC_RELAY_URL.to_owned())
    );
}

#[test]
fn effective_relay_url_passes_through_non_empty_value_unchanged() {
    assert_eq!(
        effective_relay_url(Some("wss://relay.example.com".to_owned())),
        Some("wss://relay.example.com".to_owned())
    );
    // 非空值不做额外 trim——只负责"空则兜底"，不越权改写用户已填的值。
    assert_eq!(
        effective_relay_url(Some("  wss://relay.example.com  ".to_owned())),
        Some("  wss://relay.example.com  ".to_owned())
    );
}
