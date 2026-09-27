use crate::{
    agent, agent_event, build_commit_tool, build_create_pr_tool, build_publish_tool,
    build_push_tool, build_terminal_release_event, current_locale, db, display_reduce,
    drain_after_run_release, event_transport, lead_runtime_failure_message, mcp_server,
    member_runner, note_resume_failure, record_synthetic_cli_error, ui_msg, Block, Db,
    LeadRuntimeFailure, Locale,
};
use crate::{RunSlot, Running};
use rusqlite::Connection;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager};

pub(super) fn emit_lead_error_and_release(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    terminated: &AtomicBool,
    session_id: &str,
    run_id: &str,
    transport: &event_transport::EventTransport,
    message: String,
    runtime_db: Option<&crate::db::Db>,
) {
    terminated.store(true, Ordering::SeqCst);
    let terminal_release = build_terminal_release_event(
        run_id,
        None,
        true,
        false,
        false,
        &db::RunCloseoutMetadata::default(),
        false,
    );
    let _ = emit_terminal_after_releasing_run_slot(
        running,
        team_running,
        session_id,
        run_id,
        vec![agent_event::AgentEvent::Error { message }, terminal_release],
        transport,
        runtime_db,
    );
}

/// The three failure points before lead spawn (McpStart/CommandBuild/ProcessStart) previously sent
/// live events only through `emit_lead_error_and_release` (an in-memory channel), so these failures
/// disappeared after an app restart.
/// This function copies the persistence semantics of the normal closeout path (compare the end of
/// the `start_lead_session` thread:
/// `record_synthetic_cli_error` → `RunOutcome` → `finish_for_locale` + `localize_reduced_message`
/// → `db::append_message_dedup`): it feeds the same failure message to the reducer, constructs the
/// terminal state, and persists an assistant message so users can still see the failure after a
/// restart or while browsing history.
///
/// The order must be "feed first, then finish": `finish_for_locale` has a `seen_event` gate (it
/// returns `None` directly if it has not seen any event), and `record_synthetic_cli_error` calls
/// `reducer.feed(&event)` before returning. Reversing the order silently breaks this behavior; see
/// `persist_lead_prespawn_failure_requires_feed_before_finish`.
///
/// `reducer` is received by value and consumed: all three call sites are immediately before the
/// function returns, and this is already the only and final use of the run's `DisplayReducer`.
///
/// Returns the same `AgentEvent::Error` fed to the reducer so callers can reuse it if needed. The
/// current call sites instead clone the original message string for `emit_lead_error_and_release`,
/// leaving that function's signature unchanged.
///
/// This is split into a pure `_with_conn` core (taking `&Connection` + `Locale`, without Tauri) and
/// this function (a thin AppHandle shell responsible for obtaining locale/database state). It
/// mirrors the `persist_normal_finalizer`/`persist_normal_finalizer_if_needed` pair, allowing the
/// core logic to be unit-tested without Tauri.
pub(super) fn persist_lead_prespawn_failure(
    app: &AppHandle,
    reducer: display_reduce::DisplayReducer,
    session_id: &str,
    run_id: &str,
    lead_agent_id: &str,
    lead_agent_name: &str,
    message: String,
) -> agent_event::AgentEvent {
    let locale = current_locale(app);
    let db = app.state::<crate::db::Db>();
    let conn = db.0.lock().ok();
    persist_lead_prespawn_failure_with_conn(
        conn.as_deref(),
        locale,
        reducer,
        session_id,
        run_id,
        lead_agent_id,
        lead_agent_name,
        message,
    )
}

pub(super) fn persist_lead_prespawn_failure_with_conn(
    conn: Option<&Connection>,
    locale: Locale,
    reducer: display_reduce::DisplayReducer,
    session_id: &str,
    run_id: &str,
    lead_agent_id: &str,
    lead_agent_name: &str,
    message: String,
) -> agent_event::AgentEvent {
    let mut reducer = reducer;
    // Required order: feed first (inside record_synthetic_cli_error), then finish.
    // finish_for_locale has a seen_event gate; reversing the order silently returns None and
    // defeats this persistence behavior.
    let event = record_synthetic_cli_error(&mut reducer, message);
    let outcome = display_reduce::RunOutcome {
        run_id: run_id.to_string(),
        exit_success: false,
        interrupted: false,
        saw_error: true,
        saw_blocked: false,
        saw_needs_decision: false,
        finish_called: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text: None,
    };
    if let Some(mut msg) = reducer.finish_for_locale(&outcome, locale) {
        localize_reduced_message(locale, &mut msg);
        if let Some(conn) = conn {
            reconcile_running_dispatch_cards(conn, session_id, &mut msg.blocks);
            let _ = db::append_message_dedup_and_publish(
                conn,
                session_id,
                "assistant",
                &msg.blocks,
                Some("agent-team"),
                Some(lead_agent_id),
                Some(lead_agent_name),
                &msg.dedup_key,
            );
        }
    }
    event
}

/// Centralizes cleanup when spawning the runner thread returns `Err`. The closure never executes,
/// so neither the child nor the MCP server has started; only the Launching slot and the registered
/// EventTransport run (`register_run`) need cleanup. The order is: `note_resume_failure` (install
/// backoff before releasing the slot) → `persist_lead_prespawn_failure` (persist a visible error,
/// matching the other three prespawn failure points) → `emit_lead_error_and_release` (release the
/// slot and use `flush_barrier` to transition the registered EventTransport lane to Closed, which
/// inherently cleans up the registered run) → `drain_after_run_release` (drain only after the slot
/// is released). This is a separate function because triggering a real OS thread-creation failure
/// is nearly impossible; extraction permits direct unit testing without that condition. In the
/// `Err` branch of `std::thread::Builder::spawn` in `start_lead_session`, `guard.disarm()` follows
/// this call immediately, preventing `ReservationGuard::drop` from redundantly releasing the
/// already manually removed slot and refreshing it a second time.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_lead_runner_thread_spawn_failure(
    app: &AppHandle,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    db: &crate::db::Db,
    terminated: &AtomicBool,
    session_id: &str,
    run_id: &str,
    lead_agent_id: &str,
    lead_agent_name: &str,
    error: &str,
) {
    note_resume_failure(session_id);
    let message =
        lead_runtime_failure_message(current_locale(app), LeadRuntimeFailure::ThreadSpawn(error));
    persist_lead_prespawn_failure(
        app,
        display_reduce::DisplayReducer::new(run_id),
        session_id,
        run_id,
        lead_agent_id,
        lead_agent_name,
        message.clone(),
    );
    emit_lead_error_and_release(
        running,
        team_running,
        terminated,
        session_id,
        run_id,
        event_transport(),
        message,
        Some(db),
    );
    drain_after_run_release(app.clone(), session_id.to_string());
}

pub(super) fn reconcile_running_dispatch_cards(
    conn: &Connection,
    session_id: &str,
    blocks: &mut Vec<db::Block>,
) {
    let Ok(messages) = db::get_messages(conn, session_id) else {
        return;
    };
    let reports: Vec<String> = messages
        .into_iter()
        .filter(|message| {
            message.role == "assistant" && message.engine.as_deref() == Some("agent-team")
        })
        .filter_map(|message| {
            message.content.into_iter().find_map(|block| match block {
                db::Block::Text { text } => Some(text),
                _ => None,
            })
        })
        .filter(|text| text.starts_with("[Worker report]"))
        .collect();

    for block in blocks {
        let db::Block::DispatchCard { member, .. } = block else {
            continue;
        };
        let was_running = member.status == "running";
        if !was_running && !member.blocks.is_empty() {
            continue;
        }
        let assignment_line = format!("assignment_id: {}", member.assignment_id);
        let Some(report) = reports
            .iter()
            .find(|report| report.lines().any(|line| line == assignment_line))
        else {
            continue;
        };
        if was_running {
            let reported_status = report
                .lines()
                .find_map(|line| line.strip_prefix("status: "));
            member.status = match reported_status {
                Some("done") => "done",
                Some("failed") => "failed",
                Some("stopped") => "stopped",
                _ => "failed",
            }
            .to_string();
            member.failed = member.status != "done";
        }
        member.blocks = vec![db::Block::Text {
            text: report.clone(),
        }];
    }
}

pub(super) fn begin_lead_finalizing(
    running: &Running,
    terminated: &AtomicBool,
    session_id: &str,
) -> bool {
    // The termination flag must be set before the Running slot becomes reusable; even a poisoned
    // lock must not let an old handler continue dispatching work.
    terminated.store(true, Ordering::SeqCst);
    if let Ok(mut slots) = running.0.lock() {
        let carry = match slots.get(session_id) {
            Some(RunSlot::Running(_)) => false,
            Some(RunSlot::Finalizing { stop_requested }) => *stop_requested,
            _ => false,
        };
        slots.insert(
            session_id.to_string(),
            RunSlot::Finalizing {
                stop_requested: carry,
            },
        );
        carry
    } else {
        false
    }
}

pub(super) fn run_lead_worker_with_dispatch_intent<T>(
    team_running: &member_runner::TeamRunning,
    running: &Running,
    app: Option<&AppHandle>,
    session_id: &str,
    terminated: &AtomicBool,
    run: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    // This is one of the actual moments when the team transitions from busy to idle: this intent's
    // guard lives with the function stack frame until `run()` returns (when that member has truly
    // finished; see the `DispatchIntentGuard::drop` documentation). Production call sites attach
    // refresh when `app` is `Some`; test call sites pass `None` to skip it.
    let _intent = match app {
        Some(app) => team_running
            .begin_dispatch_intent(session_id)?
            .with_refresh(running.clone(), app.clone()),
        None => team_running.begin_dispatch_intent(session_id)?,
    };

    // Registering this intent and reserve_new_session_run's "check member ∪ intent + claim Running"
    // operation are totally ordered by the same TeamRunning lock, and terminated is always set
    // before the old lead releases the Running slot. If reserve wins first, the old handler that
    // registers an intent afterward must see terminated=true and stop. If the intent wins first,
    // reserve must see it and reject the new send. Neither lock order permits two concurrent runs,
    // so the timing of preflight no longer matters.
    if terminated.load(Ordering::SeqCst) {
        return Err("lead 已终结·派单中止".to_string());
    }

    run()
}

/// Use one pure function to classify session running state so all callers apply the same runtime rules.
/// `refresh_session_runtime` and every slot-release/closeout path below must call it to recompute
/// instead of hard-coding their own 'running'/'idle' literals. A session is running when a
/// solo/lead `Running` slot exists (any `RunSlot` variant, preserving the existing busy-gate
/// semantics) ∪ `team_running` considers that session to have an active member or a dispatch intent
/// that has not been cleared (`TeamRunning::is_session_running`). The latter closes the window in
/// which the lead has released the Running slot while a member-dispatch intent remains in flight:
/// the old write path would incorrectly write session_runtime as idle at that moment, whereas the
/// new path sees the intent and correctly keeps it running. If either lock is poisoned, conservatively
/// classify it as running. A temporary false busy result can only delay when the UI or remote side
/// sees the session become idle, while incorrectly writing idle during real work could cause other
/// code to assume that deleting, archiving, or claiming the slot again is safe.
pub(super) fn compute_session_runtime(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
) -> &'static str {
    let running_slot_present = running
        .0
        .lock()
        .map(|slots| slots.contains_key(session_id))
        .unwrap_or(true);
    let team_active = team_running.is_session_running(session_id).unwrap_or(true);
    if running_slot_present || team_active {
        db::SESSION_RUNTIME_RUNNING
    } else {
        db::SESSION_RUNTIME_IDLE
    }
}

/// All slot-release/closeout write paths converge here. It briefly locks the database connection,
/// recomputes the real state with `compute_session_runtime`, calls
/// `db::upsert_session_runtime_status`, and drops the connection immediately after writing. The
/// connection is not held across `flush_barrier`/`app.emit`, avoiding extension of the database
/// lock across event emission. The run_id column is unchanged: callers mostly release runs, whose
/// run_id was already written during reservation, and this path has no new value and must not
/// overwrite it with NULL. See the `db::upsert_session_runtime_status` documentation for details.
///
/// **Callers must ensure that they do not hold the `running.0` lock before calling this function.**
/// It reacquires that lock internally, and reentry on the same thread would deadlock because
/// `std::sync::Mutex` is not reentrant.
pub(crate) fn refresh_session_runtime(
    db: &crate::db::Db,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
) {
    let status = compute_session_runtime(running, team_running, session_id);
    match db.0.lock() {
        Ok(conn) => {
            if let Err(e) = db::upsert_session_runtime_status(&conn, session_id, status) {
                eprintln!("session_runtime refresh failed (non-fatal, session={session_id}): {e}");
            }
        }
        Err(_) => {
            eprintln!("session_runtime refresh skipped: db lock poisoned (session={session_id})");
        }
    }
}

/// Share the release path across normal completion and lead prespawn failure so slot removal precedes runtime refresh.
/// Remove the `Running` slot (dropping the lock immediately after removal, as shown below), then
/// recompute session_runtime through `refresh_session_runtime` (write only when `runtime_db` is
/// `Some`; test call sites pass `None` because they do not care about the runtime-state table), and
/// only then emit the terminal event.
/// Finish with `flush_barrier`; keep database locks short-lived so later recovery can reacquire the database without deadlocking.
/// Pre-locking the database outside and passing a bare `&Connection` would keep the lock alive
/// until `try_resume_pending` tries to acquire it again, immediately deadlocking on the second lock
/// from the same thread. The function now accepts `&crate::db::Db` (an unlocked handle), while lock
/// acquisition and release are fully contained in `refresh_session_runtime`. Callers therefore
/// never receive a live guard and cannot carry one into the next lock acquisition.
pub(super) fn emit_terminal_after_releasing_run_slot(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    run_id: &str,
    terminal_events: Vec<agent_event::AgentEvent>,
    transport: &event_transport::EventTransport,
    runtime_db: Option<&crate::db::Db>,
) -> bool {
    let mut slots = match running.0.lock() {
        Ok(slots) => slots,
        Err(poisoned) => poisoned.into_inner(),
    };
    slots.remove(session_id);
    drop(slots);
    if let Some(db) = runtime_db {
        refresh_session_runtime(db, running, team_running, session_id);
    }
    transport
        .flush_barrier(run_id, terminal_events)
        .unwrap_or(false)
}

/// Look up an agent-name snapshot before persisting a solo flush so MessageStream's fallback chain
/// does not end at the bare agent_id after reload (for example, live shows "DeepSeek" while reload
/// shows "deepseek"). Return None without failing if the profile cannot be found.
pub(super) fn resolve_agent_name_snapshot(conn: &Connection, agent_id: &str) -> Option<String> {
    db::get_agent(conn, agent_id).ok().flatten().map(|p| p.name)
}

pub(super) fn persist_normal_finalizer(
    conn: &Connection,
    session_id: &str,
    engine: &str,
    agent_name_snapshot: Option<&str>,
    msg: Option<&display_reduce::ReducedMessage>,
    completed_usage: Option<(Option<u64>, Option<u64>)>,
) {
    if let Some((input_tokens, output_tokens)) = completed_usage {
        if let Err(e) = db::add_session_usage(conn, session_id, input_tokens, output_tokens) {
            eprintln!("persist normal finalizer session usage failed (non-fatal): {e}");
        }
    }
    if let Some(msg) = msg {
        let _ = db::append_message_dedup_and_publish(
            conn,
            session_id,
            "assistant",
            &msg.blocks,
            Some(engine),
            Some(engine),
            agent_name_snapshot,
            &msg.dedup_key,
        );
    }
}

pub(super) fn persist_normal_finalizer_if_needed(
    db: &Db,
    session_id: &str,
    engine: &str,
    reduced_message: Option<&display_reduce::ReducedMessage>,
    completed_usage: Option<(Option<u64>, Option<u64>)>,
) {
    if reduced_message.is_some() || completed_usage.is_some() {
        if let Ok(conn) = db.0.lock() {
            // The engine variable is actually the agent_id, as established by the
            // spawn_and_stream signature and its call sites. Only message persistence needs a
            // profile-name snapshot; usage-only persistence avoids an extra database lookup.
            let agent_name_snapshot =
                reduced_message.and_then(|_| resolve_agent_name_snapshot(&conn, engine));
            persist_normal_finalizer(
                &conn,
                session_id,
                engine,
                agent_name_snapshot.as_deref(),
                reduced_message,
                completed_usage,
            );
        }
    }
}

pub(super) fn remember_context_compacted(
    pending: &mut Option<(String, i64)>,
    event: &agent_event::AgentEvent,
) {
    if let agent_event::AgentEvent::ContextCompacted {
        summary,
        through_message_id,
    } = event
    {
        *pending = Some((summary.clone(), *through_message_id));
    }
}

pub(super) fn persist_context_compacted(
    db: &Db,
    session_id: &str,
    run_id: &str,
    pending: Option<&(String, i64)>,
) {
    let Some((summary, through_message_id)) = pending else {
        return;
    };
    match db.0.lock() {
        Ok(conn) => {
            if let Err(error) = db::upsert_compact_state(
                &conn,
                session_id,
                summary,
                *through_message_id,
                Some(run_id),
            ) {
                eprintln!("compact state persist failed (non-fatal): {error}");
            }
        }
        Err(_) => eprintln!("compact state persist skipped: db lock poisoned"),
    }
}

pub(super) fn localize_truncation_marker(locale: Locale, value: &mut String) {
    if locale != Locale::En {
        return;
    }
    let Some(rest) = value.strip_prefix("…[已截断 ") else {
        return;
    };
    let Some((dropped, tail)) = rest.split_once(" 字节]\n") else {
        return;
    };
    if dropped.parse::<usize>().is_ok() {
        *value = format!("…[truncated {dropped} bytes]\n{tail}");
    }
}

pub(super) fn localize_reduced_message(
    locale: Locale,
    message: &mut display_reduce::ReducedMessage,
) {
    for block in &mut message.blocks {
        match block {
            Block::Text { text } | Block::Thinking { text } => {
                localize_truncation_marker(locale, text);
            }
            Block::Tool {
                summary, output, ..
            } => {
                localize_truncation_marker(locale, summary);
                if let Some(output) = output {
                    localize_truncation_marker(locale, output);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn attach_solo_commit_mcp(
    app: &AppHandle,
    session_id: &str,
    run_id: &str,
    worktree: &std::path::Path,
    agent_id: &str,
    command: &mut Command,
) -> Result<Option<mcp_server::McpServer>, String> {
    let profile = {
        let db_state = app.state::<Db>();
        let conn = db_state.0.lock().map_err(|e| e.to_string())?;
        db::get_agent(&conn, agent_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| ui_msg::al_err("agent.notFound", &[]))?
    };
    if !agent::supports_solo_commit_mcp(&profile) {
        return Ok(None);
    }

    let mut tools = mcp_server::ToolRegistry::new();
    tools.insert(
        "commit".to_string(),
        build_commit_tool(app, session_id, run_id, worktree),
    );
    tools.insert("push".to_string(), build_push_tool(app, session_id, run_id));
    tools.insert(
        "create_pr".to_string(),
        build_create_pr_tool(app, session_id, run_id),
    );
    tools.insert(
        "publish".to_string(),
        build_publish_tool(app, session_id, run_id),
    );
    let server = mcp_server::start_mcp_server(std::sync::Arc::new(tools))?;
    agent::attach_solo_commit_mcp_argv(command, &profile, server.port)?;
    Ok(Some(server))
}
