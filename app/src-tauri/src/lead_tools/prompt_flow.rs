use super::*;

/// MCP tool for asking the user a question.
/// Validate, insert a decision card into the database, emit the frontend event, wait for an answer
/// according to `wait`, persist the card state, and return the answer.
///
/// `wait: None` preserves the original behavior and waits until the session stops or an answer arrives.
/// Internal ask_user callers retain this unbounded wait so confirmation remains blocking.
/// `wait: Some(d)` bounds the wait and returns a pending outcome when no answer arrives by the deadline.
/// `PromptOutcome::Pending` lets the handler return instead of remaining blocked.
#[allow(clippy::too_many_arguments)]
fn prompt_user(
    app: &tauri::AppHandle,
    session_id: &str,
    question: &str,
    options: Vec<String>,
    recommended: Option<String>,
    rationale: Option<String>,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
    wait: Option<std::time::Duration>,
) -> Result<PromptOutcome, String> {
    use tauri::Manager;

    let decision_id = crate::new_run_id();
    let action = crate::lead_action::LeadAction::AskUser {
        question: question.to_string(),
        options: options.clone(),
        recommended: recommended.clone(),
        rationale: rationale.clone().unwrap_or_default(),
    };

    let (card, card_milestone) = {
        let db_state = app.state::<crate::db::Db>();
        let conn = db_state.0.lock().map_err(|e| e.to_string())?;
        let now = crate::db::now_secs();
        // The sentinel prefix lets the frontend route by identity without falling back to the legacy lead path.
        let source_run_id = format!("{}-{}", MCP_LEAD_DECISION_PREFIX, crate::new_run_id());

        crate::db::insert_decision(
            &conn,
            session_id,
            None,
            None,
            action.rationale(),
            "[]",
            "[]",
            "mcp_ask",
            None,
        )
        .map_err(|e| e.to_string())?;

        let card =
            crate::lead_step::build_decision_card_block(&decision_id, &source_run_id, &action, now);

        let mut card_milestone = None;
        if let Some(b) = &card {
            // Snapshot the lead identity on the decision card so live and restored author labels remain meaningful.
            // Without the snapshot, the live author label is duplicated and restored messages fall back to an internal tag.
            card_milestone = append_decision_card_message(
                &conn,
                session_id,
                &decision_id,
                b,
                agent_id,
                agent_name,
            )
            .map_err(|e| e.to_string())?;
        }
        (card, card_milestone)
    }; // DB lock released here

    if let Some(milestone) = card_milestone {
        milestone.publish();
    }

    if let Some(b) = &card {
        use tauri::Emitter;
        let _ = app.emit(
            "lead-decision-card",
            serde_json::json!({
                "session_id": session_id,
                "block": b,
                "agent_id": agent_id,
                "agent_name_snapshot": agent_name,
            }),
        );
        if let Ok(block_value) = serde_json::to_value(b) {
            crate::remote_gateway::publish_card_created_milestone(
                session_id,
                &decision_id,
                block_value,
            );
        }
    }

    let questions = app.state::<crate::LeadQuestions>();
    let running = app.state::<crate::Running>();
    match crate::wait_for_answer(
        questions.inner(),
        running.inner(),
        session_id,
        &decision_id,
        wait,
    )? {
        crate::WaitOutcome::Answered(opt) => {
            // The winning compare-and-swap must obtain the updated message ID so its new revision can be republished.
            // In addition to the changed flag, obtain the updated message ID so the message can be reread and
            // republished with its new revision; the old API returned only a boolean.
            let (changed, republish) = {
                let db_state = app.state::<crate::db::Db>();
                let outcome = match db_state.0.lock() {
                    Ok(conn) => {
                        let cas_message_id = crate::db::update_decision_card_status_message_id(
                            &conn,
                            session_id,
                            &decision_id,
                            "pending",
                            "chosen",
                            Some(&opt),
                        )
                        .unwrap_or(None);
                        let republish = cas_message_id.and_then(|message_id| {
                            crate::db::get_message_for_republish(&conn, session_id, message_id)
                                .ok()
                                .flatten()
                        });
                        (cas_message_id.is_some(), republish)
                    }
                    Err(_) => (false, None),
                };
                outcome
            };
            // Silently skip republish failure or a missing deduplication key without rolling back the committed CAS update.
            if let Some(milestone) = republish {
                milestone.publish();
            }
            if changed {
                use tauri::Emitter;
                let _ = app.emit(
                    "decision-card-resolved",
                    serde_json::json!({
                        "session_id": session_id,
                        "decision_id": decision_id,
                        "status": "chosen",
                        "chosen_option": opt,
                    }),
                );
            }
            Ok(PromptOutcome::Answered(opt, decision_id.clone()))
        }
        crate::WaitOutcome::TimedOut => Ok(PromptOutcome::Pending),
    }
}

/// An unbounded `prompt_user` call must never return Pending because its answer wait cannot time out.
/// Seeing it means an internal invariant failed, so return an explicit error rather than swallowing it or panicking.
fn unbounded_prompt_never_pending() -> String {
    "prompt_user: unexpected Pending outcome for an unbounded (wait=None) call".to_string()
}

/// Preserves the original unbounded wait for internal reuse and always returns an answer.
/// Return {"answer": ...}; solo delivery confirmation uses `ask_user_bounded` to avoid an unbounded wait.
/// The remaining preview confirmation continues to block without producing `pending_user`.
/// Accept `agent_id`/`agent_name` when the caller knows the active identity so the card can retain a snapshot.
/// Pass known identity values into the decision-card snapshot. Preview confirmation has no natural identity
/// source and therefore passes `None`, preserving the fallback behavior.
pub fn ask_user(
    app: &tauri::AppHandle,
    session_id: &str,
    args: AskUserArgs,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    validate_ask_user_args(&args)?;
    match prompt_user(
        app,
        session_id,
        &args.question,
        args.options,
        args.recommended,
        args.rationale,
        agent_id,
        agent_name,
        None,
    )? {
        PromptOutcome::Answered(opt, _decision_id) => Ok(serde_json::json!({ "answer": opt })),
        PromptOutcome::Pending => Err(unbounded_prompt_never_pending()),
    }
}

/// Bound the lead-facing ask_user tool to a 240-second wait so an unanswered decision does not stall the handler.
/// It mirrors the bounded-wait pattern used by `dispatch_worker` through `DISPATCH_WORKER_WAIT`.
/// An answer within the window returns `{"answer": <option>}` and persists a visible chat echo that is excluded
/// from lead context because the tool result already delivered the answer. A timeout returns
/// `{"status": "pending_user", "note": ...}`, lets the handler exit, and leaves the database card pending and
/// clickable. A late click becomes a real user message that the next lead-context build can see.
pub fn ask_user_bounded(
    app: &tauri::AppHandle,
    session_id: &str,
    args: AskUserArgs,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    validate_ask_user_args(&args)?;
    let question = args.question.clone();
    match prompt_user(
        app,
        session_id,
        &args.question,
        args.options,
        args.recommended,
        args.rationale,
        agent_id,
        agent_name,
        Some(DISPATCH_WORKER_WAIT),
    )? {
        PromptOutcome::Answered(opt, decision_id) => {
            append_decision_echo(
                app,
                session_id,
                &decision_id,
                &question,
                &opt,
                agent_id,
                agent_name,
            );
            Ok(serde_json::json!({ "answer": opt }))
        }
        PromptOutcome::Pending => Ok(serde_json::json!({
            "status": "pending_user",
            "note": "The user hasn't answered yet within the wait window. Their answer will show up as a user message in your conversation context on a later turn — don't ask again, and don't treat this as a failure. Keep going with other work in the meantime."
        })),
    }
}

/// Persist a visible click echo so choosing an answer still leaves feedback after the decision card disappears.
/// Previously, a chosen decision card disappeared from the interface and its empty turn was not rendered.
/// `engine=DECISION_ECHO_ENGINE_TAG` is the sole exclusion marker used when assembling recent lead messages.
/// The echo is user-visible only and must not feed the answer to the lead twice. Persistence is best effort;
/// failure does not affect the already successful `ask_user` call or its stored and delivered answer.
///
/// Emit `"lead-message-appended"` after persistence so the frontend shows the echo without reopening the session.
/// This inserts the echo into the current message stream immediately instead of waiting for a later full reload.
/// The payload intentionally matches one complete `db::Message`, including its ID, so the frontend can append by
/// `(session_id, message)` and deduplicate by `message.id`. Persistence lives in the pure connection-based
/// `append_decision_echo_message` helper because an application handle cannot be constructed in an ordinary unit
/// test. The emit shell only forwards a validated payload, and the helper return value is the tested boundary.
fn append_decision_echo(
    app: &tauri::AppHandle,
    session_id: &str,
    decision_id: &str,
    question: &str,
    answer: &str,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) {
    use tauri::{Emitter, Manager};
    let db_state = app.state::<crate::db::Db>();
    let Ok(conn) = db_state.0.lock() else {
        return;
    };
    let message = append_decision_echo_message(
        &conn,
        session_id,
        decision_id,
        question,
        answer,
        agent_id,
        agent_name,
    );
    drop(conn);
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

/// Pure database core of `append_decision_echo`: persist an echo and read back the complete inserted
/// `db::Message` for the caller to emit. A write or readback failure returns `None`, preserving best-effort
/// behavior without affecting the already successful `ask_user` call.
/// Use append_message_dedup and the shared publish path so decision echoes also reach mobile clients.
/// The previous append path did not publish message completion, so mobile clients could not see this echo.
/// Use `decision_echo:<decision_id>` because the decision ID remains stable throughout one `prompt_user` call.
/// Generate the key once per decision: replays share it, while distinct decisions remain independent.
/// Use `:` rather than `|` for the reason documented by `append_decision_card_message`.
pub(super) fn append_decision_echo_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    decision_id: &str,
    question: &str,
    answer: &str,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Option<crate::db::Message> {
    let text = format!("已选择「{answer}」（{}）", clip_chars(question, 160));
    let dedup_key = format!("decision_echo:{decision_id}");
    let milestone = crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        &[crate::db::Block::Text { text }],
        Some(DECISION_ECHO_ENGINE_TAG),
        agent_id,
        agent_name,
        &dedup_key,
    )
    .ok()??;
    let id = conn.last_insert_rowid();
    milestone.publish();
    crate::db::get_message_by_id(conn, id).ok().flatten()
}

/// Truncates by character for multibyte safety when summarizing the original question in a decision echo.
fn clip_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// Persist a visible verifier result using the same echo pattern so the result remains visible in chat.
/// Persist a user-visible-only message marked with `VERIFIER_RESULT_ENGINE_TAG`, which context assembly excludes
/// because the tool result already delivered the verdict and output. Store a collapsed-by-default command card
/// using `Block::Tool` from `verifier_result_block` instead of laying the full command out as text. Persistence
/// is best effort and cannot invalidate a verifier result that has already completed and reached the lead.
/// Use append_message_dedup and the shared publish path so verifier result cards also reach mobile clients.
/// The previous append path did not publish message completion, leaving mobile clients unable to see the card.
/// Reuse the block ID generated by `verifier_result_block` as the deduplication key. One `propose_verifier` call
/// creates exactly one block and message, so their identifiers correspond one-to-one. There is no longer-lived
/// business identifier in the inputs, making the block's unique ID the minimal collision-free choice.
fn append_verifier_result_echo(
    app: &tauri::AppHandle,
    session_id: &str,
    locale: crate::Locale,
    cmd: &str,
    verdict: &str,
    exit_code: Option<i64>,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) {
    use tauri::Manager;
    let db_state = app.state::<crate::db::Db>();
    let Ok(conn) = db_state.0.lock() else {
        return;
    };
    let block = verifier_result_block(locale, cmd, verdict, exit_code);
    if let Ok(Some(milestone)) =
        append_verifier_result_message(&conn, session_id, &block, agent_id, agent_name)
    {
        milestone.publish();
    }
}

/// Pure connection-based database core for `append_verifier_result_echo`, matching the other persistence helpers
/// and remaining directly unit-testable without an application handle. The deduplication key reuses the block ID
/// generated by `verifier_result_block`. A verifier call constructs one block and one message, making their IDs
/// one-to-one; no longer-lived business identifier is available in the inputs, so the unique block ID is minimal
/// and cannot collide with another verifier call.
/// Use `:` rather than `|` in the key to avoid collisions in the enclosing client-message identifier fields.
pub(super) fn append_verifier_result_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    block: &crate::db::Block,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> rusqlite::Result<Option<crate::db::MsgCompletedMilestone>> {
    let crate::db::Block::Tool { id: block_id, .. } = block else {
        return Ok(None); // Theoretically unreachable because `verifier_result_block` always constructs `Block::Tool`.
    };
    let dedup_key = format!("verifier_result:{block_id}");
    crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        std::slice::from_ref(block),
        Some(VERIFIER_RESULT_ENGINE_TAG),
        agent_id,
        agent_name,
        &dedup_key,
    )
}

/// Run propose_verifier directly under the automatic permission policy while retaining verifier safety boundaries.
/// This is the default represented by the static automatic-permission indicator: execute without adding a setting,
/// storage, or user prompt. `run_verifier_in_place` retains the unchanged safety boundaries: an offline sandbox,
/// content accounting before and after execution, honest failure when the tree changes, session integration locking,
/// and fail-closed behavior on unsupported platforms. This function only omits the former confirmation step.
pub fn propose_verifier(
    app: &tauri::AppHandle,
    session_id: &str,
    args: ProposeVerifierArgs,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    validate_propose_verifier_args(&args)?;

    // Lock DB briefly to get workspace, then RELEASE before running verifier (slow)
    let (workspace, wt) = {
        use tauri::Manager;
        let db_state = app.state::<crate::db::Db>();
        let conn = db_state.0.lock().map_err(|e| e.to_string())?;
        crate::ensure_session_workspace(&conn, session_id)?
    }; // DB lock released here

    match workspace {
        crate::SessionWorkspace::Repo(_base_repo) => {
            // Run the verification command directly in the session worktree without a temporary empty worktree.
            // `app_data_dir` lets the sandbox deny the application's data domain when available; otherwise it
            // still denies the local application directory. Unsupported platforms fail closed and return the
            // error directly to the lead without showing a user prompt.
            use tauri::Manager;
            let app_data_dir = app.path().app_data_dir().ok();
            let res =
                crate::worktree::run_verifier_in_place(&wt, &args.cmd, app_data_dir.as_deref())?;
            append_verifier_result_echo(
                app,
                session_id,
                crate::current_locale(app),
                &args.cmd,
                &res.verdict,
                res.exit_code,
                agent_id,
                agent_name,
            );
            Ok(serde_json::json!({
                "ran": true,
                "verdict": res.verdict,
                "exit_code": res.exit_code,
                "output": res.output,
            }))
        }
        crate::SessionWorkspace::Local => Err(crate::ui_msg::al_err(
            "leadTools.verifierLocalUnsupported",
            &[],
        )),
    }
}
