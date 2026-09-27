use super::*;
/// Normalize stdin writer acknowledgement into `Result<(), String>` so delivery handling uses one outcome type.
/// Treat the absence of a receiver as `Ok(())` when the harness engine has no stdin prompt or the
/// round has no prompt to write. Harness prompts use an app-domain temporary file, and `write_all`
/// has already completed synchronously when `build_result` succeeds, so reaching this point is
/// equivalent to completed I/O acknowledgement. With `Some(rx)`, block on `recv`: the writer
/// thread always sends one result after a normal write, while thread-creation failure installs an
/// immediate `Err` as documented by `agent::spawn_with_stdin_prompt_ack`. A disconnected channel,
/// which should occur only if the writer panics unusually early, is also a failure; no receive
/// failure is accepted as success.
pub(super) fn resolve_stdin_ack(
    stdin_ack: Option<std::sync::mpsc::Receiver<std::io::Result<()>>>,
) -> Result<(), String> {
    match stdin_ack {
        Some(rx) => match rx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(io_err)) => Err(format!("stdin writer ack failed: {io_err}")),
            Err(_) => Err("stdin writer ack channel disconnected".to_string()),
        },
        None => Ok(()),
    }
}

/// Treat a missing database connection or failed writer acknowledgement as delivery failure to preserve pending reports.
/// Neither case writes to the database, so report ledger rows remain pending and the thin
/// AppHandle wrapper can route the outcome to `note_resume_failure`. The successful branch uses a
/// short transaction to set `delivered_at` for each `report_message_ids` entry.
/// `db::mark_member_reports_delivered` is transactional and treats an empty selection as a no-op to avoid false delivery marks.
/// Production calls may pass an empty collection. Extracting `_with_conn` makes this decision
/// independent of `AppHandle` and directly testable with a bare `Connection`, such as `mem_db()`,
/// following the `persist_lead_prespawn_failure` and `_with_conn` pairing.
pub(super) fn commit_lead_run_delivery_with_conn(
    conn: Option<&Connection>,
    session_id: &str,
    writer_ack: Result<(), String>,
    report_message_ids: &[i64],
) -> Result<(), String> {
    writer_ack?;
    match conn {
        Some(conn) => db::mark_member_reports_delivered(conn, session_id, report_message_ids)
            .map_err(|e| format!("delivery ack db failed: {e}")),
        None => Err("delivery ack db lock unavailable".to_string()),
    }
}

/// An empty delivery round with pending reports must retain backoff because running alone does not imply progress.
/// Pending report rows mean a run occurred without consuming anything, which must not reset
/// backoff as a success. Otherwise automatic feeding would assume delivery and stop retrying while
/// the truly stuck session appeared healthy. This pure function performs one read-only database
/// query and can be tested directly with `mem_db()` without `AppHandle`.
pub(super) fn is_delivery_round_empty_but_pending(
    conn: &Connection,
    session_id: &str,
    report_message_ids: &[i64],
    answer_ids: &[i64],
) -> rusqlite::Result<bool> {
    if !report_message_ids.is_empty() || !answer_ids.is_empty() {
        return Ok(false);
    }
    Ok(!db::pending_member_report_message_ids(conn, session_id)?.is_empty())
}

/// Represent delivery finalization outcomes explicitly so commit results and pending-report checks jointly determine success.
/// Combine the commit `Result` with the `is_delivery_round_empty_but_pending` query result when the
/// commit succeeds and a connection is available. Keeping this pure and independent of
/// `AppHandle` makes it directly testable. A pending-query error, such as database corruption or
/// a missing table, must never be treated as a successful query returning no pending work. Doing
/// so would confuse read failure with a genuinely empty result and incorrectly mark a conservatively
/// undelivered round as delivered, resetting its backoff.
pub(super) enum DeliveryOutcome {
    Success,
    UndeliveredEmptyButPending,
    UndeliveredPendingQueryError(String),
    UndeliveredCommitError(String),
}

pub(super) fn decide_delivery_outcome(
    commit_result: &Result<(), String>,
    empty_but_pending_query: Option<rusqlite::Result<bool>>,
) -> DeliveryOutcome {
    match commit_result {
        Err(error) => DeliveryOutcome::UndeliveredCommitError(error.clone()),
        Ok(()) => match empty_but_pending_query {
            Some(Ok(true)) => DeliveryOutcome::UndeliveredEmptyButPending,
            Some(Err(query_err)) => {
                DeliveryOutcome::UndeliveredPendingQueryError(query_err.to_string())
            }
            Some(Ok(false)) | None => DeliveryOutcome::Success,
        },
    }
}

/// Finalize delivery acknowledgement after lead run EOF while holding the slot, before another run can start.
/// This must run before `finish_run_without_git_writes` or
/// `emit_terminal_after_releasing_run_slot` releases the slot, preserving the ordering invariant:
/// acknowledgement commit, then slot release, then drain. On success, a short transaction commits
/// the report ledger and `ack_pending_answers` removes only the answer IDs included in this round,
/// leaving all other IDs for the next round.
/// Call `note_resume_success` to reset backoff only when delivery makes progress or no reports remain pending.
/// Empty rounds with pending reports use `note_resume_failure`; failed pending checks must also preserve backoff.
/// A pending-query error is conservatively undelivered; `.unwrap_or(false)` must not turn an
/// unreadable result into an empty one. On write failure, disconnected receive, or acknowledgement
/// database failure, `note_resume_failure` installs backoff. Reports remain pending and answers
/// remain unacknowledged, while the following `drain_after_run_release` arms a timer if needed.
/// As with the three prespawn failure sites, this avoids repeating `record_resume_failure`'s timer
/// and notification work here and thus avoids double arming with the subsequent drain.
pub(super) fn commit_lead_run_delivery(
    app: &AppHandle,
    session_id: &str,
    writer_ack: Result<(), String>,
    report_message_ids: &[i64],
    answer_ids: &[i64],
) {
    let db_state = app.state::<crate::db::Db>();
    let conn = db_state.0.lock().ok();
    let result = commit_lead_run_delivery_with_conn(
        conn.as_deref(),
        session_id,
        writer_ack,
        report_message_ids,
    );
    let empty_but_pending_query = match (&result, conn.as_deref()) {
        (Ok(()), Some(c)) => Some(is_delivery_round_empty_but_pending(
            c,
            session_id,
            report_message_ids,
            answer_ids,
        )),
        _ => None,
    };
    drop(conn);
    match decide_delivery_outcome(&result, empty_but_pending_query) {
        DeliveryOutcome::Success => {
            ack_pending_answers(session_id, answer_ids);
            note_resume_success(session_id);
        }
        DeliveryOutcome::UndeliveredEmptyButPending => {
            eprintln!(
                "lead run delivery for {session_id}: round delivered nothing (no report/answer \
                 included) but session still has pending reports — treating as undelivered"
            );
            note_resume_failure(session_id);
        }
        DeliveryOutcome::UndeliveredPendingQueryError(query_err) => {
            eprintln!(
                "lead run delivery for {session_id}: empty-but-pending query failed \
                 ({query_err}) — cannot confirm delivery, treating as undelivered (conservative)"
            );
            note_resume_failure(session_id);
        }
        DeliveryOutcome::UndeliveredCommitError(error) => {
            eprintln!("lead run delivery ack failed for {session_id}: {error}");
            note_resume_failure(session_id);
        }
    }
}

/// Automatic paths obey the shared `not_before` gate, while a fresh user click bypasses it for one immediate attempt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResumeGate {
    Normal,
    Bypass,
}

struct ResumeCandidate {
    lead_agent_id: String,
    member_agent_ids: Vec<String>,
    has_reports: bool,
}

/// Atomically snapshot both trigger classes: pending report ledger rows from
/// `autofeed_decision`, including its global-stop and team-configuration gates, plus in-memory
/// unacknowledged answer IDs read by the caller as described in `try_resume_pending_with_gate`.
/// If the door is closed because there is no `lead_agent_id` or global stop is active, neither
/// trigger applies and this returns `None`. Late answers remain in `pending_answer_ids` for the
/// next drain after the gate reopens.
fn snapshot_resume_candidate(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<ResumeCandidate>> {
    let has_reports = autofeed_decision(conn, session_id)?.is_some();
    let config = db::get_session_agent_config(conn, session_id)?;
    let Some((lead_agent_id, member_agent_ids)) = resume_after_answer_candidate(&config) else {
        return Ok(None);
    };
    if !autofeed_global_stop_allows_decision(conn, session_id)? {
        return Ok(None);
    }
    Ok(Some(ResumeCandidate {
        lead_agent_id,
        member_agent_ids,
        has_reports,
    }))
}

/// Pure function combining both trigger classes into one `StartOrigin`. With neither trigger it
/// returns `None` and does not start. Any answers, whether or not reports also exist, produce
/// `LateAnswer`; reports alone produce `Autofeed`. Coexisting triggers still produce one origin,
/// and `try_resume_pending_with_gate` calls `start_lead_session` only once, completing the guarantee
/// that one atomic snapshot starts only one round.
pub(super) fn resume_origin_for(has_reports: bool, answer_ids: &[i64]) -> Option<StartOrigin> {
    if !has_reports && answer_ids.is_empty() {
        return None;
    }
    Some(if answer_ids.is_empty() {
        StartOrigin::Autofeed
    } else {
        StartOrigin::LateAnswer
    })
}

/// Snapshot both resume triggers in one critical section so newly committed answers cannot fall between separate reads.
/// Read both while still holding the connection lock, leaving no gap for a newly committed answer.
/// `commit_late_answer` must acquire this lock before committing a new answer, so retaining it
/// removes any window between reading `has_reports` and `answer_ids`. An answer committed later is
/// naturally picked up by the next round and is not lost. If either trigger exists and the
/// global-stop, team-configuration, and gate-selected `not_before` checks pass, start one lead run
/// with the origin from `resume_origin_for`. When `not_before` blocks, ensure a timer is already
/// armed or arm one instead of merely returning, preventing a lost wakeup. The remote inbox path
/// does not pass through this gate and is unaffected.
///
/// Return `None` when no trigger exists, a gate blocks after arming a timer if needed, or a database
/// read fails. The two database failure sites intentionally make asymmetric tradeoffs:
/// - A snapshot failure means `snapshot_resume_candidate` did not finish, so team status is unknown.
///   Log with `eprintln!` and return `None` without calling `record_resume_failure`, emitting a
///   user-visible resume-failure message, setting `not_before`, or arming a timer. Installing those
///   mechanisms without knowing team status could attach irrelevant resume messaging and a timer
///   that can never be cleared to a solo session with no lead and no successful resume path to
///   call `note_resume_success`.
/// - A recheck failure from `autofeed_recheck_before_start` occurs after a `Some` candidate has
///   confirmed a team session and passed the gate. Call `record_resume_failure` to count the
///   failure, set `not_before`, and rearm the timer. Both callers otherwise see only a generic
///   `None` and cannot distinguish no trigger from a trigger followed by a database read failure.
///   Without accounting here and without another natural drain edge, pending reports or answers
///   could remain stranded forever.
/// Return `Some((lead_agent_id, result))` after actually attempting a round. `result` is the raw
/// `start_lead_session` result, whose failures are accounted for by `try_resume_pending` or
/// `try_resume_after_answer`; the former is fire-and-forget while the latter returns an outcome to
/// the frontend. Pass the atomic snapshot's `answer_ids` unchanged as `Some(answer_ids)` to
/// `start_lead_session`, which forces them into the prompt during assembly through
/// `build_lead_context_prompt_for_session`'s `forced_answer_ids`. The source of truth for answers
/// actually consumed in this round is `assembly.included_answer_ids`, captured in the runner
/// thread and consumed during final acknowledgement. There is no separate in-flight registration
/// side channel here.
/// Busy and failure paths cannot overwrite another run's answer set because snapshots travel as run-local arguments.
/// See the `resume_answer_ids` parameter documentation in `start_lead_session`.
pub(super) fn try_resume_pending_with_gate(
    app: &AppHandle,
    session_id: &str,
    gate: ResumeGate,
) -> Option<(String, Result<(), String>)> {
    let snapshot: Result<(Option<ResumeCandidate>, Vec<i64>), String> = {
        let db_state = app.state::<Db>();
        let lock_result = db_state.0.lock();
        match lock_result {
            Ok(conn) => match snapshot_resume_candidate(&conn, session_id) {
                Ok(candidate) => {
                    // The connection lock remains held while answer IDs are read in the same critical section, producing one atomic combined snapshot.
                    let answer_ids = snapshot_pending_answer_ids(session_id);
                    Ok((candidate, answer_ids))
                }
                Err(error) => Err(format!(
                    "resume_pending snapshot DB failed for {session_id}: {error}"
                )),
            },
            Err(error) => Err(format!(
                "resume_pending snapshot DB lock failed for {session_id}: {error}"
            )),
        }
    };
    let (candidate, answer_ids) = match snapshot {
        Ok(pair) => pair,
        Err(message) => {
            // Failure occurred before `snapshot_resume_candidate` could determine team status,
            // either while locking the database or running the combined query. Log only; do not
            // account, emit a user-visible message, or install a timer. See the snapshot-failure
            // section in this function's documentation.
            eprintln!("{message}");
            return None;
        }
    };
    let candidate = candidate?;

    let origin = resume_origin_for(candidate.has_reports, &answer_ids)?;

    if gate == ResumeGate::Normal && !resume_not_before_allows(session_id) {
        ensure_resume_timer_armed(app, session_id);
        return None;
    }

    // Keep this read-only precheck only to avoid reserving a slot that would certainly be released
    // silently. Correctness relies on the post-reservation gate in
    // `reserve_lead_start_after_globalstop`, as in the former `try_autofeed_lead` path.
    let recheck: Result<bool, String> = {
        let db_state = app.state::<Db>();
        let lock_result = db_state.0.lock();
        match lock_result {
            Ok(conn) => match autofeed_recheck_before_start(&conn, session_id) {
                Ok(allowed) => Ok(allowed),
                Err(error) => Err(format!(
                    "resume_pending recheck DB failed for {session_id}: {error}"
                )),
            },
            Err(error) => Err(format!(
                "resume_pending recheck DB lock failed for {session_id}: {error}"
            )),
        }
    };
    let start_allowed = match recheck {
        Ok(allowed) => allowed,
        Err(message) => {
            eprintln!("{message}");
            record_resume_failure(app, session_id, &message);
            return None;
        }
    };
    if !start_allowed {
        return None;
    }

    // The database lock was released at the end of the block above; never enter lead startup while holding it.
    // Pass the answer snapshot into the run before startup so a fast runner cannot finish before registration.
    // A caller-side registration window for the answer ID snapshot would itself be the race: the
    // runner could reach EOF first and take an empty set. Instead, pass the combined snapshot's
    // `answer_ids` unchanged to `start_lead_session` as `Some(answer_ids)`. They are only candidates
    // for `forced_answer_ids` during assembly; the runner thread decides which IDs are actually
    // included when it reaches assembly and records them in `assembly.included_answer_ids`.
    // Keep answer snapshots local to each run so early returns cannot overwrite shared registration state.
    // Thus busy and prespawn early returns cannot overwrite an existing set. See the
    // `resume_answer_ids` parameter documentation in `start_lead_session`.
    let result = start_lead_session(
        app.clone(),
        app.state::<Db>(),
        app.state::<Running>(),
        app.state::<member_runner::TeamRunning>(),
        session_id.to_string(),
        candidate.lead_agent_id.clone(),
        None,
        candidate.member_agent_ids,
        None,
        Some(origin),
        // Automatic resume for either autofeed reports or late answers uses message=None, so the deduplication key is unused.
        None,
        Some(answer_ids),
    );
    Some((candidate.lead_agent_id, result))
}

/// The drain-triggered automatic path obeys the shared `not_before` gate. Busy is not a failure;
/// non-busy failures install backoff through the shared failure wrapper.
/// Record failures with `record_resume_failure`; successful startup must not reset backoff before delivery is acknowledged.
/// Successful startup and run handoff do not imply delivery. Resetting early creates conflicting
/// sources of truth by erasing consecutive failures before the next actual failure. Reset only
/// after real I/O acknowledgement in the successful branch of `commit_lead_run_delivery`.
/// Only the confirmed delivery branch calls `note_resume_success`, keeping retry state tied to actual delivery.
pub(super) fn try_resume_pending(app: &AppHandle, session_id: &str) {
    match try_resume_pending_with_gate(app, session_id, ResumeGate::Normal) {
        None => {}
        Some((_, Ok(()))) => {}
        Some((_, Err(e))) if autofeed_busy_error(&e) => {}
        Some((_, Err(e))) => {
            eprintln!("resume_pending lead start failed (non-fatal): {e}");
            record_resume_failure(app, session_id, &e);
        }
    }
}

/// Route all post-release draining through one entry point so every trigger observes the same drain ordering.
/// The drain order has two fixed stages whose order is semantic and must not change, as asserted by
/// `drain_owned_runs_resume_pending_before_remote_inbox_and_inbox_not_gated`:
/// a) `try_resume_pending`, the single automatic-resume entry point, handles autofeed reports and
///    late answers in one atomic snapshot and accounts separately for success, failure, and busy.
///    This preserves prior semantics while replacing the former sequential autofeed then late-answer
///    accounting calls with one snapshot and one startup.
/// b) Drain `remote_inbox` in FIFO order. Stop on busy and leave work for the next release. This is
///    unaffected by `try_resume_pending`'s `not_before` gate, which blocks automatic resume only,
///    not the user-message channel.
/// If another release notification arrives while the same session is draining, merge it into a
/// dirty bit instead of entering both stages concurrently. At round completion, atomically consume
/// the bit and replay in place until a round ends without dirtiness, then remove the mutual-exclusion
/// registration. Each stage takes and releases only short-lived locks; no lock spans stages or loop
/// iterations, and the database lock is never held while calling `start_lead_session`.
/// Release short-lived locks before entering the send_message core to prevent cross-call deadlocks.
pub(super) fn drain_after_run_release(app: AppHandle, session_id: String) {
    let Some(guard) = try_begin_draining(&session_id) else {
        return;
    };
    drain_owned(app, session_id, guard);
}

pub(super) fn drain_owned(app: AppHandle, session_id: String, _guard: DrainingGuard) {
    drain_with_dirty_replay(&session_id, || {
        try_resume_pending(&app, &session_id);

        drain_remote_inbox(&app, &session_id);
    });
}

/// Pure loop core that atomically consumes the dirty bit after each drain round and replays in place when necessary; it has no AppHandle or database dependency and is directly testable.
pub(super) fn drain_with_dirty_replay<F: FnMut()>(session_id: &str, mut run_round: F) {
    loop {
        run_round();
        if !drain_round_dirty_and_continue(session_id) {
            break;
        }
    }
}

pub(super) struct DrainingGuard {
    session_id: String,
    generation: u64,
}

impl Drop for DrainingGuard {
    fn drop(&mut self) {
        // The normal path may have removed its registration before another thread installed a new
        // generation; the old guard must not remove that new registration. During panic unwinding,
        // its own registration still exists with a matching generation and is removed here as a fallback.
        if let Some(sessions) = DRAINING_SESSIONS.get() {
            let mut guard = sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard
                .get(&self.session_id)
                .is_some_and(|slot| slot.generation == self.generation)
            {
                guard.remove(&self.session_id);
            }
        }
    }
}

pub(super) fn try_begin_draining(session_id: &str) -> Option<DrainingGuard> {
    let sessions = DRAINING_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let session_id = session_id.to_string();
    let generation = {
        let mut guard = sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = guard.get_mut(&session_id) {
            slot.dirty = true;
            return None;
        }
        let generation = NEXT_DRAINING_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        guard.insert(
            session_id.clone(),
            DrainSlot {
                generation,
                dirty: false,
            },
        );
        generation
    };
    Some(DrainingGuard {
        session_id,
        generation,
    })
}

pub(super) fn drain_round_dirty_and_continue(session_id: &str) -> bool {
    let sessions = DRAINING_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.get_mut(session_id) {
        Some(slot) if slot.dirty => {
            slot.dirty = false;
            true
        }
        _ => {
            guard.remove(session_id);
            false
        }
    }
}

/// Real I/O wiring for draining `remote_inbox`: database reads and writes each use short-lived
/// locks, while the loop control flow lives in the pure, testable, AppHandle-independent
/// `drain_remote_inbox_loop` function.
pub(super) fn drain_remote_inbox(app: &AppHandle, session_id: &str) {
    drain_remote_inbox_loop(
        || {
            let db_state = app.state::<Db>();
            let conn = db_state.0.lock().ok()?;
            db::next_pending_remote_input(&conn, session_id)
                .ok()
                .flatten()
                .map(|entry| (entry.id, entry.command_id, entry.kind, entry.payload))
        },
        |kind, payload, command_id| {
            deliver_remote_inbox_entry(app, session_id, kind, payload, command_id)
        },
        |id, command_id| {
            let db_state = app.state::<Db>();
            let Ok(conn) = db_state.0.lock() else {
                eprintln!(
                    "remote_inbox mark_delivered skipped for command_id={command_id}: db lock poisoned"
                );
                return false;
            };
            match db::mark_remote_input_delivered(&conn, id) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!(
                        "remote_inbox mark_delivered failed for command_id={command_id} (non-fatal): {e}"
                    );
                    false
                }
            }
        },
        |id, command_id, error| {
            let db_state = app.state::<Db>();
            let Ok(conn) = db_state.0.lock() else {
                eprintln!(
                    "remote_inbox record_failure skipped for command_id={command_id}: db lock poisoned"
                );
                return None;
            };
            match db::record_remote_input_failure(&conn, id, error) {
                Ok(attempts) => Some(attempts),
                Err(e) => {
                    eprintln!(
                        "remote_inbox record_failure failed for command_id={command_id} (non-fatal): {e}"
                    );
                    None
                }
            }
        },
        |id, command_id, error| {
            let db_state = app.state::<Db>();
            let Ok(conn) = db_state.0.lock() else {
                eprintln!(
                    "remote_inbox mark_failed skipped for command_id={command_id}: db lock poisoned"
                );
                return false;
            };
            match db::mark_remote_input_failed(&conn, id, error) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!(
                        "remote_inbox mark_failed failed for command_id={command_id} (non-fatal): {e}"
                    );
                    false
                }
            }
        },
        |command_id, reason| {
            crate::remote_gateway::enqueue_failed_input_ack(command_id, reason);
        },
    );
}

/// Pure loop core that only controls fetching the next item, delivering it, and continuing or
/// stopping based on the result. It does not touch AppHandle or the database; real wiring lives in
/// `drain_remote_inbox`, while tests provide stub closures. Errors are divided into busy, immediate
/// terminal failures, and retryable delivery failures. Busy stops immediately with no side effects.
/// Immediate failures are marked terminally failed before continuing. Retryable delivery failures
/// increment attempts; fewer than three remain pending and stop this round, while the third is
/// marked terminally failed and processing continues. Every successful terminal mark notifies the
/// injected failure sink. Two existing safety valves remain: any failed mark write stops immediately,
/// and two consecutive returns of the same ID from `next_pending` stop before the second delivery.
/// Leaving an item pending and breaking does not conflict with the repeated-ID valve because this
/// round will not fetch the item again; the next release starts a new drain.
///
/// Two key invariants apply. First, busy can arise only from
/// `reserve_new_session_run`, and reservation precedes `append_message`, so busy has no side effects.
/// Second, delivery is intentionally at least once: a crash between successful delivery and
/// `mark_delivered` may deliver the item again. A claim-first design that marks before delivery
/// would degrade to at most once and lose the message on a crash, which is worse.
pub(super) fn drain_remote_inbox_loop(
    mut next_pending: impl FnMut() -> Option<(i64, String, String, String)>,
    mut deliver: impl FnMut(&str, &str, &str) -> Result<(), String>,
    mut mark_delivered: impl FnMut(i64, &str) -> bool,
    mut record_failure: impl FnMut(i64, &str, &str) -> Option<i64>,
    mut mark_failed: impl FnMut(i64, &str, &str) -> bool,
    mut notify_failed: impl FnMut(&str, Option<&str>),
) {
    let mut last_id: Option<i64> = None;
    loop {
        let Some((id, command_id, kind, payload)) = next_pending() else {
            break;
        };
        if last_id == Some(id) {
            // Safety valve: seeing the same item twice means the cursor did not advance; even if this is a real bug, never enter an infinite hot loop.
            break;
        }
        last_id = Some(id);
        match deliver(&kind, &payload, &command_id) {
            Ok(()) => {
                if !mark_delivered(id, &command_id) {
                    // Safety valve: stop when a mark write fails and retry on the next release instead of continuing through a queue that may already have been delivered.
                    break;
                }
            }
            Err(e) if autofeed_busy_error(&e) => break,
            Err(e) => match classify_remote_inbox_error(&e) {
                RemoteInboxErrorClass::Terminal { reason } => {
                    eprintln!("remote_inbox failed for command_id={command_id} (terminal): {e}");
                    if !mark_failed(id, &command_id, &e) {
                        break;
                    }
                    notify_failed(&command_id, reason);
                }
                RemoteInboxErrorClass::Delivery => {
                    eprintln!(
                        "remote_inbox delivery failed for command_id={command_id} (non-fatal): {e}"
                    );
                    let Some(attempts) = record_failure(id, &command_id, &e) else {
                        break;
                    };
                    if attempts < 3 {
                        // Preserve FIFO order: this item remains pending, so this round must not deliver later items for the same session ahead of it.
                        break;
                    }
                    if !mark_failed(id, &command_id, &e) {
                        break;
                    }
                    notify_failed(&command_id, None);
                }
            },
        }
    }
}
