#![cfg(test)]

use super::*;

#[test]
fn input_ack_outbox_drains_failed_acks_in_fifo_order() {
    let inner = test_inner(|_| None, || None);
    for (command_id, reason) in [
        ("cmd-1", Some("no_agent")),
        ("cmd-2", None),
        ("cmd-3", Some("delivery_failed")),
    ] {
        enqueue_failed_input_ack_into(&inner, command_id, reason);
    }

    let mut sent = Vec::new();
    drain_input_ack_outbox_with(&inner, |message| {
        sent.push(message);
        Ok::<_, ()>(())
    })
    .unwrap();

    let expected = [
        ("cmd-1", Some("no_agent")),
        ("cmd-2", None),
        ("cmd-3", Some("delivery_failed")),
    ]
    .map(|(command_id, reason)| {
        Message::Text(
            input_ack_json_with_reason(command_id, AckOutcome::Failed, reason)
                .to_string()
                .into(),
        )
    });
    assert_eq!(sent, expected);
    assert!(lock(&inner.state.input_ack_outbox).is_empty());
}

#[test]
fn input_ack_outbox_requeues_failed_send_at_front_and_preserves_order() {
    let inner = test_inner(|_| None, || None);
    enqueue_failed_input_ack_into(&inner, "cmd-1", Some("no_agent"));
    enqueue_failed_input_ack_into(&inner, "cmd-2", None);

    let error = drain_input_ack_outbox_with(&inner, |_message| Err("socket closed")).unwrap_err();
    assert_eq!(error, "socket closed");

    let expected_first = input_ack_json_with_reason("cmd-1", AckOutcome::Failed, Some("no_agent"));
    assert_eq!(
        lock(&inner.state.input_ack_outbox).front(),
        Some(&expected_first)
    );

    let mut retried = Vec::new();
    drain_input_ack_outbox_with(&inner, |message| {
        retried.push(message);
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(
        retried,
        vec![
            Message::Text(expected_first.to_string().into()),
            Message::Text(
                input_ack_json_with_reason("cmd-2", AckOutcome::Failed, None)
                    .to_string()
                    .into()
            ),
        ]
    );
}

#[test]
fn input_ack_outbox_drops_new_frame_when_capacity_is_reached() {
    let inner = test_inner(|_| None, || None);
    for index in 0..64 {
        enqueue_failed_input_ack_into(&inner, &format!("cmd-{index}"), None);
    }
    enqueue_failed_input_ack_into(&inner, "cmd-64", Some("must_drop"));

    let outbox = lock(&inner.state.input_ack_outbox);
    assert_eq!(outbox.len(), 64);
    assert!(outbox.iter().all(|frame| frame["command_id"] != "cmd-64"));
}
