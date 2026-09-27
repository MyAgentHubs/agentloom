use super::*;

/// Unified failure exit. Use `count_invalid=false` for benign rejections or failures that should
/// not be blamed on the device, such as an in-flight request, quota overflow, a desktop failure, or
/// a subject not yet confirmed as a real device. These do not increment the consecutive-invalid
/// counter and never include `close`. Use `count_invalid=true` to record an invalid request and
/// include `close:true` when the limit of three consecutive invalid requests is reached.
///
/// On the frame that reaches the limit, change `reason` to `invalid_repeated` so it differs from
/// the first two non-closing `invalid` failures. Callers currently pass `"invalid"` uniformly;
/// this function performs the shared rewrite based on `close`, so each call site need not decide
/// independently.
pub(super) fn refresh_fail_reply(
    registry: &mut remote_gateway::RegistryState,
    request_id: &str,
    subject: &str,
    reason: &str,
    count_invalid: bool,
) -> remote_gateway::RefreshOutcome {
    let close = if count_invalid {
        registry.record_refresh_invalid(subject)
    } else {
        false
    };
    let reason = if close { "invalid_repeated" } else { reason };
    remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_fail_json(
        request_id, subject, reason, close,
    ))
}

pub(super) fn remote_gateway_refresh_handler(app: &AppHandle) -> remote_gateway::RefreshHandler {
    let app = app.clone();
    Box::new(move |frame| {
        let Ok(mut registry) = remote_registry().lock() else {
            eprintln!("remote gateway token.refresh ignored: registry lock poisoned");
            return remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_fail_json(
                &frame.request_id,
                &frame.subject,
                "invalid",
                false,
            ));
        };
        let Some(db) = app.try_state::<Db>() else {
            eprintln!("remote gateway token.refresh ignored: Db state unavailable");
            return remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_fail_json(
                &frame.request_id,
                &frame.subject,
                "invalid",
                false,
            ));
        };
        let Ok(conn) = db.inner().0.lock() else {
            eprintln!("remote gateway token.refresh ignored: Db lock poisoned");
            return remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_fail_json(
                &frame.request_id,
                &frame.subject,
                "invalid",
                false,
            ));
        };
        let Ok(mut token_book) = remote_token_book().lock() else {
            eprintln!("remote gateway token.refresh ignored: token book lock poisoned");
            return remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_fail_json(
                &frame.request_id,
                &frame.subject,
                "invalid",
                false,
            ));
        };
        let now_ms = now_unix_millis();
        process_token_refresh_with_registry(
            &mut registry,
            &conn,
            &KeyringStore,
            &mut token_book,
            &frame,
            now_ms,
        )
    })
}

/// Core decision logic for remote-input enqueueing, optional immediate processing, and reading the
/// terminal state of duplicate ledger rows. All three dependencies are injected by the caller, so
/// tests do not need an `AppHandle`. Each DB lock held by enqueue or lookup has been released when
/// it returns. Drain only newly inserted rows, guaranteeing zero processing side effects for a
/// duplicate `command_id`.
pub(super) fn remote_input_send_ack(
    enqueue: impl FnOnce() -> Result<bool, String>,
    drain: impl FnOnce(),
    lookup_terminal_state: impl FnOnce() -> Result<Option<db::RemoteInboxTerminalState>, String>,
) -> Option<remote_gateway::AckOutcome> {
    let inserted = match enqueue() {
        Ok(inserted) => inserted,
        Err(e) => {
            eprintln!("remote input enqueue failed (non-fatal): {e}");
            return None;
        }
    };

    if inserted {
        drain();
        // Once draining moved to a dedicated thread, the acknowledgment no longer waited for the
        // delivery result, so every new row is queued.
        return Some(remote_gateway::AckOutcome::Queued);
    }

    match lookup_terminal_state() {
        Ok(Some(db::RemoteInboxTerminalState::Delivered)) => Some(remote_gateway::AckOutcome::Ok),
        Ok(Some(db::RemoteInboxTerminalState::Pending)) => Some(remote_gateway::AckOutcome::Queued),
        Ok(Some(db::RemoteInboxTerminalState::Failed)) | Ok(None) => {
            Some(remote_gateway::AckOutcome::Failed)
        }
        Err(e) => {
            eprintln!("remote input terminal-state lookup failed (non-fatal): {e}");
            None
        }
    }
}

pub(super) fn spawn_remote_input_drain(drain: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("remote-input-drain".into())
        .spawn(drain);
    if let Err(e) = spawned {
        eprintln!("remote input drain thread spawn failed (non-fatal): {e}");
    }
}

pub(super) const REMOTE_ANSWER_SPAWN_FAILED: &str = "REMOTE_ANSWER_SPAWN_FAILED";

pub(super) fn spawn_remote_answer_processing(work: impl FnOnce() + Send + 'static) -> bool {
    let spawned = std::thread::Builder::new()
        .name("remote-answer".into())
        .spawn(work);
    match spawned {
        Ok(_) => true,
        Err(e) => {
            eprintln!("remote answer thread spawn failed (non-fatal): {e}");
            false
        }
    }
}

pub(super) fn mark_remote_answer_spawn_failed(app: &AppHandle, command_id: &str) {
    let db_state = app.state::<Db>();
    match db_state.0.lock() {
        Ok(conn) => {
            if let Err(e) = db::mark_remote_input_failed_by_command_id(
                &conn,
                command_id,
                REMOTE_ANSWER_SPAWN_FAILED,
            ) {
                eprintln!(
                    "remote input.answer spawn-failure mark_failed 写入失败（non-fatal）：command_id={command_id} err={e}"
                );
            }
        }
        Err(_) => eprintln!(
            "remote input.answer spawn-failure mark_failed 跳过：db lock poisoned (command_id={command_id})"
        ),
    };
}

pub(super) fn parse_remote_answer_payload(payload: &str) -> Result<(String, String), String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            let decision_id = value.get("decision_id")?.as_str()?.to_string();
            let option = value.get("option")?.as_str()?.to_string();
            Some((decision_id, option))
        })
        .ok_or_else(|| "REMOTE_INBOX_PAYLOAD_MALFORMED".to_string())
}

/// Pure loop core for startup recovery of pending `input.answer` entries. Parsing and spawn
/// decisions do not touch the `AppHandle` or DB. Thin injected closures perform real thread
/// creation and terminal-state writes, allowing direct coverage of spawn-failure and malformed-
/// payload terminal paths.
pub(super) fn recover_pending_remote_answers_loop(
    entries: Vec<db::RemoteInboxEntry>,
    mut spawn_answer: impl FnMut(&db::RemoteInboxEntry, String, String) -> bool,
    mut mark_failed: impl FnMut(&str, &str),
) {
    for entry in &entries {
        match parse_remote_answer_payload(&entry.payload) {
            Ok((decision_id, option)) => {
                if !spawn_answer(entry, decision_id, option) {
                    mark_failed(&entry.command_id, REMOTE_ANSWER_SPAWN_FAILED);
                }
            }
            Err(error) => mark_failed(&entry.command_id, &error),
        }
    }
}

/// I/O shell for startup recovery of pending `input.answer` entries. Query under one short DB
/// lock, then start a separate thread for each answer through the existing processing chain.
/// Synchronously write terminal states for malformed ledger entries and thread-creation failures
/// so they cannot remain pending forever.
pub(super) fn startup_recover_pending_remote_answers(app: &AppHandle, session_id: &str) {
    let entries = {
        let db_state = app.state::<Db>();
        let Ok(conn) = db_state.0.lock() else {
            eprintln!(
                "pending input.answer 启动恢复跳过：db lock poisoned (session_id={session_id})"
            );
            return;
        };
        match db::pending_remote_answers(&conn, session_id) {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("pending input.answer 启动恢复查询失败（忽略·不阻塞启动）：{e}");
                return;
            }
        }
    };
    if entries.is_empty() {
        return;
    }

    let app = app.clone();
    let session_id = session_id.to_string();
    recover_pending_remote_answers_loop(
        entries,
        |entry, decision_id, option| {
            let processing_app = app.clone();
            let processing_session = session_id.clone();
            let processing_command_id = entry.command_id.clone();
            spawn_remote_answer_processing(move || {
                process_remote_answer(
                    processing_app,
                    processing_session,
                    processing_command_id,
                    decision_id,
                    option,
                )
            })
        },
        |command_id, error| {
            if error == REMOTE_ANSWER_SPAWN_FAILED {
                mark_remote_answer_spawn_failed(&app, command_id);
                return;
            }
            let db_state = app.state::<Db>();
            match db_state.0.lock() {
                Ok(conn) => {
                    if let Err(e) =
                        db::mark_remote_input_failed_by_command_id(&conn, command_id, error)
                    {
                        eprintln!(
                            "pending input.answer mark_failed 写入失败（non-fatal）：command_id={command_id} err={e}"
                        );
                    }
                }
                Err(_) => eprintln!(
                    "pending input.answer mark_failed 跳过：db lock poisoned (command_id={command_id})"
                ),
            };
        },
    );
}

pub(super) fn remote_answer_terminal(
    answer: impl FnOnce() -> Result<AnswerLeadQuestionOutcome, String>,
    mark_delivered: impl FnOnce(),
    mark_failed: impl FnOnce(&str),
) {
    match answer() {
        Ok(_outcome) => mark_delivered(),
        Err(error) => mark_failed(&error),
    }
}

pub(super) fn process_remote_answer(
    app: AppHandle,
    session_id: String,
    command_id: String,
    decision_id: String,
    option: String,
) {
    let delivered_command_id = command_id.clone();
    let failed_command_id = command_id;
    remote_answer_terminal(
        || {
            answer_lead_question(
                app.clone(),
                app.state::<LeadQuestions>(),
                app.state::<Db>(),
                session_id,
                decision_id,
                option,
            )
        },
        || {
            let db_state = app.state::<Db>();
            let Ok(conn) = db_state.0.lock() else {
                eprintln!(
                    "remote input.answer mark_delivered skipped for command_id={delivered_command_id}: db lock poisoned"
                );
                return;
            };
            if let Err(e) =
                db::mark_remote_input_delivered_by_command_id(&conn, &delivered_command_id)
            {
                eprintln!(
                    "remote input.answer mark_delivered failed for command_id={delivered_command_id} (non-fatal): {e}"
                );
            }
        },
        |error| {
            let db_state = app.state::<Db>();
            let Ok(conn) = db_state.0.lock() else {
                eprintln!(
                    "remote input.answer mark_failed skipped for command_id={failed_command_id}: db lock poisoned"
                );
                return;
            };
            if let Err(e) =
                db::mark_remote_input_failed_by_command_id(&conn, &failed_command_id, error)
            {
                eprintln!(
                    "remote input.answer mark_failed failed for command_id={failed_command_id} (non-fatal): {e}"
                );
            }
        },
    );
}

pub(super) fn remote_gateway_input_send_handler(
    app: &AppHandle,
) -> remote_gateway::InputSendHandler {
    let app = app.clone();
    Box::new(move |frame| {
        let remote_gateway::InputSendFrame {
            session,
            command_id,
            text,
        } = frame;
        let payload = serde_json::json!({ "text": text }).to_string();
        remote_input_send_ack(
            || {
                let db_state = app.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                db::enqueue_remote_input(&conn, &session, &command_id, "input.send", &payload)
                    .map_err(|e| e.to_string())
            },
            || {
                let Some(guard) = try_begin_draining(&session) else {
                    return;
                };
                let app = app.clone();
                let session = session.clone();
                spawn_remote_input_drain(move || drain_owned(app, session, guard));
            },
            || {
                let db_state = app.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                db::remote_inbox_terminal_state_by_command_id(&conn, &command_id)
                    .map_err(|e| e.to_string())
            },
        )
    })
}

pub(super) fn remote_gateway_input_answer_handler(
    app: &AppHandle,
) -> remote_gateway::InputAnswerHandler {
    let app = app.clone();
    Box::new(move |frame| {
        let remote_gateway::InputAnswerFrame {
            session,
            command_id,
            decision_id,
            option,
        } = frame;
        let payload = serde_json::json!({
            "decision_id": &decision_id,
            "option": &option,
        })
        .to_string();
        let processing_app = app.clone();
        let processing_session = session.clone();
        let processing_command_id = command_id.clone();
        let processing_decision_id = decision_id.clone();
        let processing_option = option.clone();
        let spawn_failure_app = app.clone();
        let spawn_failure_command_id = command_id.clone();
        remote_input_send_ack(
            || {
                let db_state = app.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                db::enqueue_remote_input(&conn, &session, &command_id, "input.answer", &payload)
                    .map_err(|e| e.to_string())
            },
            || {
                if !spawn_remote_answer_processing(move || {
                    process_remote_answer(
                        processing_app,
                        processing_session,
                        processing_command_id,
                        processing_decision_id,
                        processing_option,
                    )
                }) {
                    mark_remote_answer_spawn_failed(&spawn_failure_app, &spawn_failure_command_id);
                }
            },
            || {
                let db_state = app.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                db::remote_inbox_terminal_state_by_command_id(&conn, &command_id)
                    .map_err(|e| e.to_string())
            },
        )
    })
}

pub(super) fn remote_gateway_control_stop_handler(
    app: &AppHandle,
) -> remote_gateway::ControlStopHandler {
    let app = app.clone();
    Box::new(move |frame| {
        let db = app.state::<Db>();
        let running = app.state::<Running>();
        let team_running = app.state::<member_runner::TeamRunning>();
        let session_id = frame.session.clone();
        let result = stop_session_with_background_inspection(
            &db,
            &running,
            &team_running,
            &session_id,
            current_locale(&app),
            kill_process_group,
            inspect_background_processes_for_stop,
            |notice| emit_background_stop_notice(&app, &session_id, notice),
            |event| emit_agent_event(&app, &session_id, None, event),
        );
        match result {
            Ok(()) => remote_gateway::AckOutcome::Ok,
            Err(e) => {
                eprintln!("remote control.stop failed (non-fatal): {e}");
                remote_gateway::AckOutcome::Failed
            }
        }
    })
}

pub(super) fn remote_gateway_control_replay_handler(
    app: &AppHandle,
) -> remote_gateway::ControlReplayHandler {
    let app = app.clone();
    Box::new(move |session_id, command_id| {
        let db = app.state::<Db>();
        let Ok(conn) = db.0.lock() else {
            eprintln!("remote control replay ledger rejected command: db lock poisoned");
            return false;
        };
        match db::record_control_command_seen(&conn, session_id, command_id, "{}") {
            Ok(is_new) => is_new,
            Err(e) => {
                eprintln!("remote control replay ledger failed closed (non-fatal): {e}");
                false
            }
        }
    })
}
