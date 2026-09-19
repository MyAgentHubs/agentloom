#![cfg(test)]

use super::*;

#[test]
fn remote_done_replay_for_different_device_is_rejected_without_persistence() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = Mutex::new(PairingSlot::Done {
        room_id: PAIR_TEST_ROOM.to_owned(),
        device_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        completed_at_secs: PAIR_TEST_NOW,
    });
    let mut registry = remote_gateway::RegistryState::default();

    let action = process_pair_done_with_registry(
        &slot,
        &mut registry,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: PAIR_TEST_ROOM.to_owned(),
            device_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            ..Default::default()
        },
        PAIR_TEST_NOW + 1,
        PAIR_TEST_NOW_MS + 1_000,
    )
    .unwrap();

    assert!(matches!(action, remote_gateway::PairDoneAction::Rejected));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Done { .. }));
}

#[test]
fn remote_done_replay_after_retention_expiry_is_rejected_and_returns_idle() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let device_id = "11111111-1111-4111-8111-111111111111";
    let slot = Mutex::new(PairingSlot::Done {
        room_id: PAIR_TEST_ROOM.to_owned(),
        device_id: device_id.to_owned(),
        completed_at_secs: PAIR_TEST_NOW,
    });
    let mut registry = remote_gateway::RegistryState::default();

    let action = process_pair_done_with_registry(
        &slot,
        &mut registry,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: PAIR_TEST_ROOM.to_owned(),
            device_id: device_id.to_owned(),
            ..Default::default()
        },
        PAIR_TEST_NOW + remote_pairing::PAIRING_LIFETIME_SECS + 1,
        PAIR_TEST_NOW_MS + (remote_pairing::PAIRING_LIFETIME_SECS + 1) * 1_000,
    )
    .unwrap();

    assert!(matches!(action, remote_gateway::PairDoneAction::Rejected));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Idle));
}

#[test]
fn pair_done_without_confirm_is_rejected_and_slot_stays_sent_accept() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    let accept = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("valid hello should produce pair.accept");
    let (capability_token, _) = decrypt_pair_accept_tokens(&slot, &accept);

    let completed = process_pair_done(
        &slot,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: accept.room.clone(),
            device_id: accept.device_id.clone(),
            ..Default::default()
        },
        PAIR_TEST_NOW + 1,
        PAIR_TEST_NOW_MS + 1_000,
    )
    .unwrap();

    assert!(completed.is_none());
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert_eq!(
        token_book.lock().unwrap().verify_access(
            &accept.device_id,
            &capability_token,
            PAIR_TEST_NOW_MS + 1_000,
        ),
        Err(remote_pairing::PairingError::NotFound)
    );
    assert!(matches!(
        *slot.lock().unwrap(),
        PairingSlot::SentAccept { .. }
    ));
}

#[test]
fn pair_done_with_forged_confirm_from_wrong_key_is_rejected_and_slot_stays_sent_accept() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let token_book = Mutex::new(remote_pairing::TokenBook::new());
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    let accept = process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("valid hello should produce pair.accept");
    let (capability_token, _) = decrypt_pair_accept_tokens(&slot, &accept);
    let (confirm_ct, confirm_n) =
        remote_pairing::seal_pair_done_confirm(&[9_u8; 32], &accept.room, &accept.device_id);

    let completed = process_pair_done(
        &slot,
        &conn,
        &key_store,
        &token_book,
        remote_gateway::PairDoneFrame {
            room: accept.room.clone(),
            device_id: accept.device_id.clone(),
            confirm_ct: Some(confirm_ct),
            confirm_n: Some(confirm_n),
            origin_connection_id: "conn-pairing-test".to_owned(),
        },
        PAIR_TEST_NOW + 1,
        PAIR_TEST_NOW_MS + 1_000,
    )
    .unwrap();

    assert!(completed.is_none());
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
    assert_eq!(
        token_book.lock().unwrap().verify_access(
            &accept.device_id,
            &capability_token,
            PAIR_TEST_NOW_MS + 1_000,
        ),
        Err(remote_pairing::PairingError::NotFound)
    );
    assert!(matches!(
        *slot.lock().unwrap(),
        PairingSlot::SentAccept { .. }
    ));
}

#[test]
fn pairing_accept_without_done_expires_without_ghost_device() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("valid hello should produce pair.accept");

    let status = {
        let mut guard = slot.lock().unwrap();
        compute_pairing_status(&mut guard, PAIR_TEST_NOW + PAIR_ACCEPT_LIFETIME_SECS)
    };

    assert_eq!(status, RemotePairingStatus::Idle);
    assert!(matches!(*slot.lock().unwrap(), PairingSlot::Idle));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
}

#[test]
fn cancel_or_rebegin_discards_pending_outcome_and_new_token_can_pair() {
    let conn = pairing_test_db();
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let (first_token, first_hello) = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        (
            session.pairing_token.clone(),
            pairing_gateway_hello(session, session.pairing_token.as_bytes()),
        )
    };
    process_pair_hello(&slot, &key_store, first_hello, PAIR_TEST_NOW)
        .unwrap()
        .expect("first hello should reach SentAccept");

    *slot.lock().unwrap() = PairingSlot::Idle;
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());

    let (new_session, _) = remote_pairing::PairingSession::begin(
        "wss://relay.example.test",
        PAIR_TEST_ROOM,
        PAIR_TEST_NOW + 1,
    );
    assert_ne!(new_session.pairing_token, first_token);
    let second_hello = pairing_gateway_hello(&new_session, new_session.pairing_token.as_bytes());
    *slot.lock().unwrap() = PairingSlot::Waiting(new_session);

    assert!(
        process_pair_hello(&slot, &key_store, second_hello, PAIR_TEST_NOW + 1)
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        *slot.lock().unwrap(),
        PairingSlot::SentAccept { .. }
    ));
    assert!(db::list_remote_devices(&conn).unwrap().is_empty());
}

#[test]
fn compute_pairing_status_reports_and_expires_sent_accept() {
    let key_store = FakeKeyStore::default();
    let slot = fresh_pairing_slot();
    let hello = {
        let guard = slot.lock().unwrap();
        let PairingSlot::Waiting(session) = &*guard else {
            unreachable!()
        };
        pairing_gateway_hello(session, session.pairing_token.as_bytes())
    };
    process_pair_hello(&slot, &key_store, hello, PAIR_TEST_NOW)
        .unwrap()
        .unwrap();
    let mut guard = slot.lock().unwrap();

    assert_eq!(
        compute_pairing_status(&mut guard, PAIR_TEST_NOW + 1),
        RemotePairingStatus::WaitingForDone {
            expires_at: PAIR_TEST_NOW + PAIR_ACCEPT_LIFETIME_SECS,
        }
    );
}
