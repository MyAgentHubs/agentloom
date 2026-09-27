use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteInboxErrorClass {
    Terminal { reason: Option<&'static str> },
    Delivery,
}

pub(super) fn classify_remote_inbox_error(error: &str) -> RemoteInboxErrorClass {
    if error == "AL_ERR:agent.sessionRunUnknown" {
        RemoteInboxErrorClass::Terminal {
            reason: Some("no_agent"),
        }
    } else if error == "REMOTE_INBOX_PAYLOAD_MALFORMED"
        || error == "REMOTE_INBOX_KIND_NOT_SUPPORTED_YET"
        || error.starts_with("UNKNOWN_REMOTE_INBOX_KIND:")
    {
        RemoteInboxErrorClass::Terminal { reason: None }
    } else {
        RemoteInboxErrorClass::Delivery
    }
}

pub(super) fn parse_remote_input(kind: &str, payload: &str) -> Result<String, String> {
    match kind {
        "input.send" => serde_json::from_str::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| {
                value
                    .get("text")
                    .and_then(|text| text.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| "REMOTE_INBOX_PAYLOAD_MALFORMED".to_string()),
        // Answer cards must be delivered immediately and never queued; this is only a theoretically unreachable defensive branch.
        "input.answer" => Err("REMOTE_INBOX_KIND_NOT_SUPPORTED_YET".to_string()),
        _ => Err(format!("UNKNOWN_REMOTE_INBOX_KIND:{kind}")),
    }
}

/// Read-back core for remote inbox echoing: the emit criterion is whether the message was
/// actually newly persisted, not whether the delivery succeeded as a whole. If it existed before
/// delivery, this attempt is only an at-least-once redelivery and must never emit again; otherwise,
/// read back the complete newly persisted message by `(session_id, dedup_key)`. Keep this as a pure
/// DB function so the thin AppHandle wrapper is responsible only for best-effort emission.
pub(super) fn remote_inbox_message_to_emit(
    conn: &rusqlite::Connection,
    session_id: &str,
    dedup_key: &str,
    existed_before: bool,
) -> Result<Option<db::Message>, String> {
    if existed_before {
        return Ok(None);
    }
    db::get_message_by_session_and_dedup_key(conn, session_id, dedup_key).map_err(|e| e.to_string())
}

fn remote_inbox_message_existed_before(app: &AppHandle, session_id: &str, dedup_key: &str) -> bool {
    let db_state = app.state::<Db>();
    let existed = match db_state.0.lock() {
        Ok(conn) => db::get_message_by_session_and_dedup_key(&conn, session_id, dedup_key)
            .map(|message| message.is_some())
            .unwrap_or(true),
        Err(_) => true,
    };
    existed
}

pub(super) fn session_run_slot_reserved(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
) -> bool {
    let solo_running = running
        .0
        .lock()
        .map(|slots| slots.contains_key(session_id))
        .unwrap_or(false);
    solo_running || team_running.is_session_running(session_id).unwrap_or(false)
}

pub(super) fn remote_inbox_agent_id_or_busy(
    agent_id: Result<String, String>,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
) -> Result<String, String> {
    match agent_id {
        Err(error)
            if error == "AL_ERR:agent.sessionRunUnknown"
                && session_run_slot_reserved(running, team_running, session_id) =>
        {
            Err(format!("SESSION_BUSY:{session_id}"))
        }
        result => result,
    }
}

pub(super) fn emit_remote_inbox_message_if_new(
    app: &AppHandle,
    session_id: &str,
    dedup_key: &str,
    existed_before: bool,
) {
    let message = {
        let db_state = app.state::<Db>();
        let Ok(conn) = db_state.0.lock() else {
            return;
        };
        remote_inbox_message_to_emit(&conn, session_id, dedup_key, existed_before)
            .ok()
            .flatten()
    };
    if let Some(message) = message {
        let _ = app.emit(
            "lead-message-appended",
            serde_json::json!({
                "session_id": session_id,
                "message": message,
            }),
        );
    }
}

/// Delivery core for a single `input.send`: team sessions (with a saved lead) use
/// `start_lead_session`, matching a message entered manually in the frontend; solo sessions still
/// use `resolve_session_run_agent` + `send_message`.
/// Reuse `resume_after_answer_candidate` for gating and release short-lived locks before cross-calls to avoid deadlocks.
/// This is a deadlock red line; kind/payload is first classified by the pure
/// `parse_remote_input` function, and parse failures pass through unchanged so the loop can mark
/// the terminal failure state. P0-c: `command_id` is threaded here entry by entry from
/// `drain_remote_inbox_loop`; `display_reduce::remote_input_key(command_id)` is derived and passed
/// to `start_lead_session`/`send_message` as `user_dedup_key`. For at-least-once redelivery (a crash
/// or disconnect before mark_delivered is persisted), this key deduplicates the same command_id at
/// the DB layer, so it never produces a second persisted message or a second msg.completed
/// milestone.
pub(super) fn deliver_remote_inbox_entry(
    app: &AppHandle,
    session_id: &str,
    kind: &str,
    payload: &str,
    command_id: &str,
) -> Result<(), String> {
    let text = parse_remote_input(kind, payload)?;
    let dedup_key = display_reduce::remote_input_key(command_id);
    // This is used only to determine whether this attempt actually persisted a new message. If the
    // notification-side query fails, conservatively treat it as already existing to avoid sending
    // a duplicate echo, without ever changing the actual input.send delivery result.
    let existed_before = remote_inbox_message_existed_before(app, session_id, &dedup_key);
    let team_candidate = {
        let config = {
            let db_state = app.state::<Db>();
            let conn = db_state.0.lock().map_err(|e| e.to_string())?;
            db::get_session_agent_config(&conn, session_id).map_err(|e| e.to_string())?
        };
        resume_after_answer_candidate(&config)
    };
    if let Some((lead_agent_id, member_agent_ids)) = team_candidate {
        let result = start_lead_session(
            app.clone(),
            app.state::<Db>(),
            app.state::<Running>(),
            app.state::<member_runner::TeamRunning>(),
            session_id.to_string(),
            lead_agent_id,
            Some(text),
            member_agent_ids,
            None,
            Some(StartOrigin::UserMessage),
            Some(display_reduce::remote_input_key(command_id)),
            // A brand-new user message delivery carries no snapshot of pending answer ids to resume.
            None,
        );
        emit_remote_inbox_message_if_new(app, session_id, &dedup_key, existed_before);
        return result;
    }
    let agent_id = {
        let db_state = app.state::<Db>();
        let conn = db_state.0.lock().map_err(|e| e.to_string())?;
        resolve_session_run_agent(&conn, session_id).map(|profile| profile.id)
    };
    let agent_id = remote_inbox_agent_id_or_busy(
        agent_id,
        app.state::<Running>().inner(),
        app.state::<member_runner::TeamRunning>().inner(),
        session_id,
    )?;
    let result = send_message(
        app.clone(),
        app.state::<Db>(),
        app.state::<Running>(),
        app.state::<member_runner::TeamRunning>(),
        session_id.to_string(),
        agent_id,
        text,
        None,
        None,
        Some(display_reduce::remote_input_key(command_id)),
    );
    emit_remote_inbox_message_if_new(app, session_id, &dedup_key, existed_before);
    result
}
