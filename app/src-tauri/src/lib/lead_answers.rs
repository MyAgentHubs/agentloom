use crate::{
    autofeed_busy_error, current_locale, db, display_reduce, record_resume_failure,
    register_pending_answer_id_if_team, try_resume_pending_with_gate, Locale, ResumeGate, Running,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

pub enum LeadAnswer {
    Choice(String),
    Cancel,
}

/// Downgrade Live slots to TimedOut when bounded `prompt_user` waits expire so late answers remain distinguishable.
/// The handler thread has already exited cleanly (it no longer holds or waits on this Sender), but
/// the slot itself remains so `answer_question_inner` can recognize a later user click as a late
/// answer rather than as a question that was never asked.
pub enum LeadQuestionSlot {
    /// The handler is still blocked waiting; sending through the Sender unblocks it (the original
    /// path persists the card through prompt_user itself).
    Live(std::sync::mpsc::Sender<LeadAnswer>),
    /// The handler returned after its bounded wait timed out; the answer must be persisted through
    /// the late path (`commit_late_answer`).
    TimedOut,
}

#[derive(Clone, Default)]
pub struct LeadQuestions(pub Arc<Mutex<HashMap<String, LeadQuestionSlot>>>);

/// The result of `wait_for_answer`: an answer arrived on time, or the bounded waiting window
/// expired without one.
pub(crate) enum WaitOutcome {
    Answered(String),
    TimedOut,
}

/// Preserve unbounded waiting for `wait: None`, using `running` to detect cancellation.
/// The propose_verifier / legacy ask_user reuse point is preserved unchanged and never produces
/// TimedOut); `wait: Some(d)` means a bounded wait (used by the real ask_user MCP tool): if no
/// answer arrives by total duration `d`, downgrade the slot from Live to TimedOut and return
/// WaitOutcome::TimedOut so the handler exits cleanly instead of remaining blocked.
///
/// There is no race window between the deadline transition and an answer arriving at exactly the
/// same time: the transition check and the "answer obtained" check share the same `questions.0`
/// lock. The transition happens only while the slot is still Live at that moment. If the answer
/// was already sent first through this path (`answer_question_inner` removed the Live slot and
/// sent it), the transition branch sees that the slot is no longer Live (usually absent entirely)
/// and performs one final receive on the channel to collect the in-flight answer. No answer is
/// lost without cause, and both sides cannot each decide they won.
pub(crate) fn wait_for_answer(
    questions: &LeadQuestions,
    running: &Running,
    session_id: &str,
    decision_id: &str,
    wait: Option<std::time::Duration>,
) -> Result<WaitOutcome, String> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel::<LeadAnswer>();
    {
        let mut m = questions.0.lock().map_err(|e| e.to_string())?;
        m.insert(decision_id.to_string(), LeadQuestionSlot::Live(tx));
    }
    let deadline = wait.map(|d| std::time::Instant::now() + d);
    loop {
        if let Some(dl) = deadline {
            let remaining = dl.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                let mut m = questions.0.lock().map_err(|e| e.to_string())?;
                if matches!(m.get(decision_id), Some(LeadQuestionSlot::Live(_))) {
                    m.insert(decision_id.to_string(), LeadQuestionSlot::TimedOut);
                    drop(m);
                    return Ok(WaitOutcome::TimedOut);
                }
                drop(m);
                // The slot is no longer Live, which means the answer has been or is being sent
                // through the map path (remove has happened and send was already called), but we
                // have not polled it yet. The channel message is in flight, so wait briefly once
                // to finish receiving it.
                return match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                    Ok(LeadAnswer::Choice(opt)) => Ok(WaitOutcome::Answered(opt)),
                    Ok(LeadAnswer::Cancel) | Err(_) => Err("ASK_CANCELLED".to_string()),
                };
            }
        }
        let poll = match deadline {
            Some(dl) => dl
                .saturating_duration_since(std::time::Instant::now())
                .min(std::time::Duration::from_millis(500)),
            None => std::time::Duration::from_millis(500),
        };
        match rx.recv_timeout(poll) {
            Ok(LeadAnswer::Choice(opt)) => {
                // Defensive remove: answer_question_inner removed this decision_id before send,
                // so this is a no-op in the normal flow. Keep it in case a future sender does not
                // remove first, ensuring no dangling entry remains after receiving an answer.
                questions
                    .0
                    .lock()
                    .map_err(|e| e.to_string())?
                    .remove(decision_id);
                return Ok(WaitOutcome::Answered(opt));
            }
            Ok(LeadAnswer::Cancel) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                questions
                    .0
                    .lock()
                    .map_err(|e| e.to_string())?
                    .remove(decision_id);
                return Err("ASK_CANCELLED".to_string());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let in_running = running
                    .0
                    .lock()
                    .map_err(|e| e.to_string())?
                    .contains_key(session_id);
                if !in_running {
                    questions
                        .0
                        .lock()
                        .map_err(|e| e.to_string())?
                        .remove(decision_id);
                    return Err("ASK_CANCELLED:lead stopped".to_string());
                }
            }
        }
    }
}

/// Route `answer_lead_question` by the current slot state so each answer follows the appropriate delivery path.
/// Which route the answer takes. This is a pure in-memory decision (it does not touch the DB);
/// downstream, `answer_question_inner` uses it to decide whether persistence is needed.
enum AnswerRoute {
    /// The slot is still Live: the answer has been sent to the handler that is still blocked. The
    /// handler's original path (after prompt_user receives the answer) persists the card status;
    /// this path does not touch the DB or emit again. The handler triggers the card-flip broadcast
    /// itself after the CAS persistence succeeds.
    Delivered,
    /// The slot is TimedOut: the handler exited cleanly earlier, so the answer must be persisted by
    /// the late path (mark the card chosen and turn it into a real user message for the lead's next
    /// round).
    Late,
    /// The map has no slot for this decision_id at all. This process may never have run the bounded
    /// wait (for example, the in-memory state was cleared after an app restart while a pending card
    /// remains in the DB), or this may be a double-click on an already answered card. Let the
    /// caller inspect the DB card status and decide.
    Missing,
}

/// Keep slot lookup and state dispatch in a pure in-memory decision step without database side effects.
/// A single critical section ensures the answer can be delivered only once (after the Live branch
/// removes it, no later call can obtain the same Sender).
fn take_question_route(
    questions: &LeadQuestions,
    decision_id: &str,
    answer: &str,
) -> Result<AnswerRoute, String> {
    let mut m = questions.0.lock().map_err(|e| e.to_string())?;
    match m.remove(decision_id) {
        Some(LeadQuestionSlot::Live(tx)) => {
            let _ = tx.send(LeadAnswer::Choice(answer.to_string()));
            Ok(AnswerRoute::Delivered)
        }
        Some(LeadQuestionSlot::TimedOut) => Ok(AnswerRoute::Late),
        None => Ok(AnswerRoute::Missing),
    }
}

/// Commit late answers only when the card CAS changes pending to chosen, preventing duplicate processing.
/// Also append a real user message (`[User's answer to ‘question’] option`) for the lead to see
/// naturally in the next build_lead_context_prompt; this is the only channel that conveys an
/// answer to the lead in the late-answer scenario.
/// Losing the CAS (`changed=false`) means another path already finalized this card and the answer
/// has already been recorded. Do not append a duplicate second message; return `Ok(None)` (to the
/// caller this still means the answer was delivered successfully, not an error, but there is no
/// new message to emit).
///
/// Return `Result<Option<db::Message>, String>` so callers can emit the persisted message without coupling storage to UI events.
/// After persistence succeeds, read back the complete newly inserted `db::Message` so the outer
/// shell can emit `"lead-message-appended"`. This uses the same "pure core + outer emitting shell"
/// split as `lead_tools::append_decision_echo`/`append_decision_echo_message`: this function still
/// takes only `&Connection`, does not touch `AppHandle`, and leaves emission to the caller.
pub(super) fn commit_late_answer(
    conn: &rusqlite::Connection,
    session_id: &str,
    decision_id: &str,
    answer: &str,
    locale: Locale,
) -> Result<Option<db::Message>, String> {
    let question = db::find_decision_card(conn, session_id, decision_id)
        .map_err(|e| e.to_string())?
        .map(|(q, _)| q)
        .unwrap_or_default();
    // Use `update_decision_card_status_message_id` to identify and republish the changed message with its new revision.
    // Besides the changed boolean, obtain the rewritten message_id, reread that message, and
    // republish msg.completed with the new revision (the client_msg_id includes the revision, so
    // the relay treats it as a new event and must broadcast it). A republish failure does not roll
    // back the CAS rewrite already committed above; skip it silently (best effort, matching the
    // other similar best-effort handling points elsewhere in this flow).
    let cas_message_id = db::update_decision_card_status_message_id(
        conn,
        session_id,
        decision_id,
        "pending",
        "chosen",
        Some(answer),
    )
    .map_err(|e| e.to_string())?;
    if let Some(message_id) = cas_message_id {
        if let Ok(Some(republish)) = db::get_message_for_republish(conn, session_id, message_id) {
            republish.publish();
        }
    }
    let changed = cas_message_id.is_some();
    if !changed {
        return Ok(None);
    }
    let question = clip_chars_for_echo(&question, 200);
    let text = match locale {
        Locale::Zh => format!("[用户对『{question}』的回答] {answer}"),
        Locale::En => format!("[User's answer to ‘{question}’] {answer}"),
    };
    // Deduplicated persistence is keyed by decision_id. The CAS already guarantees this branch
    // runs only once per decision_id, so late_answer_key itself does not need run_id/command_id
    // reinforcement. In both callers of this function (the Late/Missing branches of
    // answer_question_inner), conn comes directly from `db.0.lock()` with no explicit transaction,
    // using autocommit. This satisfies the sibling contract of append_message_dedup_and_publish,
    // so publish() may run immediately after the insert succeeds.
    let dedup_key = display_reduce::late_answer_key(decision_id);
    let milestone = db::append_message_dedup(
        conn,
        session_id,
        "user",
        &[db::Block::Text { text }],
        None,
        None,
        None,
        &dedup_key,
    )
    .map_err(|e| e.to_string())?;
    // Read the row back only when it was inserted. Defensively treat the theoretically unreachable
    // dedup collision (the same decision_id reaches here twice despite the CAS) as "no new message
    // to emit" rather than panicking.
    let Some(milestone) = milestone else {
        return Ok(None);
    };
    let id = conn.last_insert_rowid();
    milestone.publish();
    db::get_message_by_id(conn, id).map_err(|e| e.to_string())
}

/// Truncate by char (multibyte-safe) for commit_late_answer's summary of the original question.
fn clip_chars_for_echo(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// The testable core of answer_lead_question, decoupled from Tauri State. Its three routes are
/// defined by `AnswerRoute`:
/// Delivered: the answer was sent to a live handler, whose original path handles persistence
/// (including its own live echo emit; see `lead_tools::append_decision_echo`), so return `Ok(None)`
/// directly here.
/// Late: the handler is already gone (it exited cleanly when the bounded wait expired), so persist
/// the answer here (mark the card chosen and turn it into a user message).
/// Missing: the map has no slot for this decision_id, so query the DB. If the card is still pending
/// (for example, in-memory state was cleared after a process restart), persist it as a late answer;
/// if the card is already chosen (a real double-click / already answered), preserve
/// NO_PENDING_QUESTION. The duplicate-delivery guarantee is not relaxed: answering the same
/// question a second time must never produce a second persisted message.
///
/// The return value carries both an optional late-answer message and whether this call definitively
/// won the decision card. Delivered does not persist in this call and cannot synchronously confirm
/// the handler's later CAS result, so resolved is always false; card-flip broadcasting is delegated
/// to `lead_tools::prompt_user`, which emits it after receiving the answer and getting `Ok(true)`
/// from CAS persistence. Late/Missing sets resolved=true only when the CAS actually changes pending
/// to chosen and returns the newly appended message.
/// This core does not trigger resumption. Any new caller that reuses it, especially a future remote
/// entry point such as remote_gateway, must connect `try_resume_after_answer` itself or call the
/// `answer_lead_question` shell that already wires emission and resumption. Otherwise a remote card
/// answer will silently leave the run stalled after the answer is persisted.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AnswerQuestionResult {
    pub(super) appended: Option<db::Message>,
    pub(super) resolved: bool,
}

pub(crate) fn answer_question_inner(
    questions: &LeadQuestions,
    db: &db::Db,
    session_id: &str,
    decision_id: &str,
    answer: String,
    locale: Locale,
) -> Result<AnswerQuestionResult, String> {
    match take_question_route(questions, decision_id, &answer)? {
        AnswerRoute::Delivered => Ok(AnswerQuestionResult {
            appended: None,
            resolved: false,
        }),
        AnswerRoute::Late => {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let appended = commit_late_answer(&conn, session_id, decision_id, &answer, locale)?;
            // Register unacknowledged answer IDs after commit_late_answer succeeds for Team sessions so delivery can be retried.
            // See register_pending_answer_id_if_team: do not consume them here or before spawn;
            // Retain them until confirmed stdin I/O allows ack_pending_answers to acknowledge actual delivery.
            if let Some(message) = &appended {
                register_pending_answer_id_if_team(&conn, session_id, message.id);
            }
            Ok(AnswerQuestionResult {
                resolved: appended.is_some(),
                appended,
            })
        }
        AnswerRoute::Missing => {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            match db::find_decision_card(&conn, session_id, decision_id)
                .map_err(|e| e.to_string())?
            {
                Some((_, status)) if status == "pending" => {
                    let appended =
                        commit_late_answer(&conn, session_id, decision_id, &answer, locale)?;
                    if let Some(message) = &appended {
                        register_pending_answer_id_if_team(&conn, session_id, message.id);
                    }
                    Ok(AnswerQuestionResult {
                        resolved: appended.is_some(),
                        appended,
                    })
                }
                _ => Err("NO_PENDING_QUESTION".to_string()),
            }
        }
    }
}

/// Expose backend resume results so the frontend can reflect the outcome without launching a duplicate run.
/// The return value of `answer_lead_question` (do not confuse it with AnswerQuestionResult):
/// `resumed` tells the frontend whether the backend already triggered resumption itself. On
/// success, `lead_agent_id` is the saved lead actually used to start; only a non-busy startup
/// failure returns its original error through `resume_error`. Based on this, the frontend performs
/// optimistic rendering only and never invokes any resumption command itself.
/// A second frontend resume would collide with the backend reservation and report a misleading busy error.
/// The frontend no longer has any IPC entry point that triggers resumption itself.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub(super) struct AnswerLeadQuestionOutcome {
    pub(super) resumed: bool,
    pub(super) lead_agent_id: Option<String>,
    pub(super) resume_error: Option<String>,
}

impl AnswerLeadQuestionOutcome {
    pub(super) fn quietly_not_resumed() -> Self {
        Self {
            resumed: false,
            lead_agent_id: None,
            resume_error: None,
        }
    }
}

#[tauri::command]
pub(super) fn answer_lead_question(
    app: AppHandle,
    questions: State<LeadQuestions>,
    db: State<db::Db>,
    session_id: String,
    decision_id: String,
    answer: String,
) -> Result<AnswerLeadQuestionOutcome, String> {
    let chosen_answer = answer.clone();
    let AnswerQuestionResult { appended, resolved } = answer_question_inner(
        questions.inner(),
        db.inner(),
        &session_id,
        &decision_id,
        answer,
        current_locale(&app),
    )?;
    use tauri::Emitter;
    if resolved {
        let _ = app.emit(
            "decision-card-resolved",
            serde_json::json!({
                "session_id": session_id,
                "decision_id": decision_id,
                "status": "chosen",
                "chosen_option": chosen_answer,
            }),
        );
    }
    // Emit only the message returned by a successful commit_late_answer; event delivery cannot undo the database commit.
    // This affects only immediate visibility (the next full get_messages fetch still includes it);
    // best effort, with no retry.
    let outcome = if let Some(message) = appended {
        let _ = app.emit(
            "lead-message-appended",
            serde_json::json!({
                "session_id": session_id,
                "message": message,
            }),
        );
        // Emit the answer before resuming so the frontend receives the answer message before the next run starts.
        // Then receive the run-start event; appended=None (Delivered or a double-click that lost
        // the CAS) must never trigger this.
        try_resume_after_answer(&app, &session_id)
    } else {
        AnswerLeadQuestionOutcome::quietly_not_resumed()
    };
    Ok(outcome)
}

/// Pure function that converts `(lead_agent_id, start result)` from
/// `try_resume_pending_with_gate` into `AnswerLeadQuestionOutcome`. Busy (losing the slot because
/// the session is already running) quietly converges to `quietly_not_resumed()`; non-busy errors
/// preserve their original text, and success carries the actual saved lead. It performs no
/// bookkeeping (the caller handles bookkeeping side effects separately for busy, non-busy, and
/// success), so it can be unit-tested without AppHandle.
pub(super) fn classify_resume_attempt_outcome(
    lead_agent_id: String,
    result: Result<(), String>,
) -> AnswerLeadQuestionOutcome {
    match result {
        Ok(()) => AnswerLeadQuestionOutcome {
            resumed: true,
            lead_agent_id: Some(lead_agent_id),
            resume_error: None,
        },
        Err(e) if autofeed_busy_error(&e) => AnswerLeadQuestionOutcome::quietly_not_resumed(),
        Err(e) => AnswerLeadQuestionOutcome {
            resumed: false,
            lead_agent_id: None,
            resume_error: Some(e),
        },
    }
}

/// Treat a freshly submitted late answer, including a remote card answer, as an immediate recovery attempt after persistence.
/// Immediately make one attempt while bypassing the shared `not_before` backoff. Delegate to the
/// unified entry point `try_resume_pending_with_gate(..., ResumeGate::Bypass)`, which atomically
/// snapshots both trigger sources (ledger pending reports and unacknowledged late-answer IDs,
/// including the one just persisted) plus the global-stop/team gates, while skipping the
/// `not_before` gate.
/// One successful delivery consumes both causes; release the short database gate lock before calling `start_lead_session`.
/// This is the same hard boundary established by the previous deadlock incident.
///
/// Deduplication adds no new lock: the CAS in `commit_late_answer` guarantees that only one caller
/// can receive `Some(message)`. `start_lead_session` has its own `reserve_new_session_run` slot gate
/// as a fallback against concurrent starts. Losing that slot (`autofeed_busy_error` matches)
/// quietly converges to "the session is already running". Busy does not count toward the shared
/// backoff failure count; the registered `pending_answer_ids` wait for the drain after the next run
/// slot release to retry naturally. No separate "immediate second probe" patch is needed: the
/// answer ID is registered before the start attempt, so every interleaved release drain naturally
/// sees it. A non-busy failure writes one non-fatal log line and enters shared backoff through
/// `record_resume_failure`. Do not panic or bubble `Err` into a command failure. The return value
/// carries whether resumption occurred, the actual saved lead, and the original non-busy error.
fn try_resume_after_answer(app: &AppHandle, session_id: &str) -> AnswerLeadQuestionOutcome {
    let Some((lead_agent_id, result)) =
        try_resume_pending_with_gate(app, session_id, ResumeGate::Bypass)
    else {
        return AnswerLeadQuestionOutcome::quietly_not_resumed();
    };
    let was_busy = matches!(&result, Err(e) if autofeed_busy_error(e));
    let outcome = classify_resume_attempt_outcome(lead_agent_id, result);
    if !was_busy {
        if let Some(error) = &outcome.resume_error {
            eprintln!("resume after late answer failed (non-fatal): {error}");
            record_resume_failure(app, session_id, error);
        }
        // Do not clear backoff when `resume_error` is `None`: successful runner creation does not establish actual delivery.
        // "Success, run handed off" means only that this attempt started, not that delivery actually
        // occurred. Clearing too early would erase the queued consecutive-failure state before the
        // next MCP/build/spawn failure and keep backoff stuck at its shortest tier. The real reset
        // happens only after an actual I/O acknowledgment, in the `Ok` branch of
        // `commit_lead_run_delivery`.
        // Call `note_resume_success` only after delivery is acknowledged so failed attempts retain their backoff history.
    }
    outcome
}

/// Testable pure gating core for `try_resume_pending_with_gate`: for a team session
/// (`session_agent_configs` has `lead_agent_id`), open the resumption gate and return
/// `Some((lead_agent_id, member_agent_ids))`. For a solo session (the row is absent or
/// `lead_agent_id` is `NULL`), do not resume and return `None`: `commit_late_answer` already
/// persisted the late answer as a real user message for the next normal run to consume naturally,
/// and it must not be mistaken for a team resumption trigger.
pub(super) fn resume_after_answer_candidate(
    config: &db::SessionAgentConfig,
) -> Option<(String, Vec<String>)> {
    let lead_agent_id = config.lead_agent_id.clone()?;
    Some((lead_agent_id, config.member_agent_ids.clone()))
}
