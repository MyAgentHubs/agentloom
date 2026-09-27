use super::*;

const INPUT_ACK_OUTBOX_CAPACITY: usize = 64;

pub(crate) fn enqueue_failed_input_ack(command_id: &str, reason: Option<&str>) {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    enqueue_failed_input_ack_into(inner, command_id, reason);
}

pub(super) fn enqueue_failed_input_ack_into(inner: &Inner, command_id: &str, reason: Option<&str>) {
    let frame = input_ack_json_with_reason(command_id, AckOutcome::Failed, reason);
    let queued = {
        let mut outbox = lock(&inner.state.input_ack_outbox);
        if outbox.len() >= INPUT_ACK_OUTBOX_CAPACITY {
            false
        } else {
            outbox.push_back(frame);
            true
        }
    };
    if !queued {
        eprintln!("input ack outbox full; dropping failed ack for command_id={command_id}");
    }
}

pub(super) fn drain_input_ack_outbox(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    inner: &Inner,
) -> Result<(), String> {
    drain_input_ack_outbox_with(inner, |message| {
        socket
            .send(message)
            .map_err(|error| format!("input ack outbox write failed: {error}"))
    })
}

pub(super) fn drain_input_ack_outbox_with<E>(
    inner: &Inner,
    mut send: impl FnMut(Message) -> Result<(), E>,
) -> Result<(), E> {
    loop {
        let Some(frame) = lock(&inner.state.input_ack_outbox).pop_front() else {
            return Ok(());
        };
        if let Err(error) = send(Message::Text(frame.to_string().into())) {
            let mut outbox = lock(&inner.state.input_ack_outbox);
            if outbox.len() >= INPUT_ACK_OUTBOX_CAPACITY {
                outbox.pop_back();
            }
            outbox.push_front(frame);
            return Err(error);
        }
        inner.state.frames_sent.fetch_add(1, Ordering::Relaxed);
    }
}
