use crate::{
    agent_event, db, display_reduce, member_runner, note_resume_failure, refresh_session_runtime,
    ui_msg, worktree, LeadTerminal,
};
use crate::{RunSlot, Running};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn request_stop<K, F>(
    running: &Running,
    session_id: &str,
    kill: K,
    emit_terminal_release: F,
) -> Result<(), String>
where
    K: FnOnce(u32),
    F: FnOnce(&agent_event::AgentEvent),
{
    let mut m = running.0.lock().map_err(|e| e.to_string())?;
    match m.get_mut(session_id) {
        Some(RunSlot::Running(p)) => {
            // Observing Running(pid) under a healthy lock proves that the owner reader has not transitioned to Finalizing and has not yet reaped it with wait.
            // Kill the process group before hiding the pid in the same critical section, eliminating the accidental-kill window where the pid could be reaped and reused after unlocking.
            let pid = *p;
            kill(pid);
            m.insert(
                session_id.to_string(),
                RunSlot::Finalizing {
                    stop_requested: true,
                },
            );
            // Do not remove the slot; transition it to Finalizing{stop_requested:true}:
            // 1. When the finalizer transitions to Finalizing, carry=true makes interrupted=true, which is the primary path for marking interruption.
            // 2. The slot remains occupied throughout (Running -> Finalizing is never None), eliminating the None window so a new run cannot reserve it before closeout finishes.
            // 3. The pid is no longer exposed after the transition to Finalizing.
        }
        Some(RunSlot::Launching { stop_requested }) => {
            *stop_requested = true;
        }
        Some(RunSlot::Finalizing { stop_requested }) => {
            // The pid may already have been reused, so only set the flag and let the finalizer thread mark the interruption; never kill the process group here.
            *stop_requested = true;
        }
        Some(RunSlot::Mutating { op }) => {
            let _ = *op;
        }
        Some(RunSlot::TeamRun) => {
            // A single-session solo stop command matched a team-run marker by session_id. A team run has no single pid;
            // stop an individual member through `stop_team_member`, and leave the slot unchanged here without doing anything, matching the Mutating branch.
        }
        None => {
            // The process may have exited early and removed the slot while the frontend is still stuck in Working. When the real run_id is unavailable,
            // Use an empty string for unknown values; frontend run_id matching guards against duplicate closeout races.
            // Keep the None check and emit under the same lock so a new run cannot be inserted between them.
            let event = build_terminal_release_event(
                "",
                None,
                false,
                false,
                false,
                &db::RunCloseoutMetadata::default(),
                true,
            );
            emit_terminal_release(&event);
        }
    }
    Ok(())
}

/// Generate the run_id before spawning: time + pid + counter is sufficiently unique within a session and does not depend on a commit SHA.
pub(crate) fn new_run_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let p = std::process::id() as u64;
    format!("run-{t:016x}-{p:08x}-{c:08x}")
}

/// Before spawning, write the legacy ledger pending row and set git_state=running using the same connection while its lock is held.
/// Git projects record the current HEAD; non-Git projects record an empty baseline without affecting run startup.
pub(super) fn prepare_run_ledger(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    engine: &str,
    wt: &std::path::Path,
) -> Result<(), String> {
    let pre_head = worktree::rev_parse_head(wt).unwrap_or_default();
    // Make insert_run_pending + set_git_state atomic. unchecked_transaction works with &Connection.
    // Both steps are applied or neither is applied, avoiding the partial state where pending was written but running was not set. The logic and behavior are unchanged.
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    db::insert_run_pending(conn, session_id, run_id, engine, &pre_head).map_err(|e| {
        ui_msg::al_err("run.ledgerPendingWriteFailed", &[("detail", e.to_string())])
    })?;
    db::set_git_state(conn, session_id, "running").map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

/// Run closeout only clears the app's own legacy Git-ledger placeholder; it does not read, stage, or commit the worktree.
pub(super) fn finish_run_without_git_writes(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    interrupted: bool,
) -> Result<db::RunCloseoutMetadata, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let closeout =
        db::finalize_run_pending_without_git_writes(conn, session_id, run_id, interrupted)
            .map_err(|e| e.to_string())?;
    if !db::has_run_commit_intent(conn, session_id, run_id).map_err(|e| e.to_string())? {
        db::set_git_state(conn, session_id, "clean").map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(closeout)
}

pub(super) fn should_emit_metadata_bearing_completed(
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    interrupted: bool,
) -> bool {
    !interrupted && !(saw_error || saw_blocked || saw_needs_decision)
}

pub(super) fn should_emit_run_closeout(
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    interrupted: bool,
) -> bool {
    !should_emit_metadata_bearing_completed(saw_error, saw_blocked, saw_needs_decision, interrupted)
}

pub(super) fn record_synthetic_cli_error(
    reducer: &mut display_reduce::DisplayReducer,
    message: String,
) -> agent_event::AgentEvent {
    let event = agent_event::AgentEvent::Error { message };
    reducer.feed(&event);
    event
}

pub(super) fn build_terminal_release_event(
    run_id: &str,
    pending_completed: Option<&agent_event::AgentEvent>,
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    closeout: &db::RunCloseoutMetadata,
    interrupted: bool,
) -> agent_event::AgentEvent {
    if should_emit_run_closeout(saw_error, saw_blocked, saw_needs_decision, interrupted) {
        return agent_event::AgentEvent::RunCloseout {
            run_id: run_id.to_string(),
            commit_sha: closeout.commit_sha.clone(),
            files_changed: closeout.files_changed,
            insertions: closeout.insertions,
            deletions: closeout.deletions,
            interrupted: Some(interrupted),
        };
    }

    let (cost_usd, input_tokens, output_tokens, final_text) = match pending_completed {
        Some(agent_event::AgentEvent::Completed {
            cost_usd,
            input_tokens,
            output_tokens,
            final_text,
            ..
        }) => (*cost_usd, *input_tokens, *output_tokens, final_text.clone()),
        _ => (None, None, None, None),
    };
    agent_event::AgentEvent::Completed {
        cost_usd,
        input_tokens,
        output_tokens,
        final_text,
        result: None,
        run_id: Some(run_id.to_string()),
        commit_sha: closeout.commit_sha.clone(),
        files_changed: closeout.files_changed,
        insertions: closeout.insertions,
        deletions: closeout.deletions,
        interrupted: Some(interrupted),
    }
}

pub(super) fn build_lead_terminal_release_event(
    run_id: &str,
    decision: &LeadTerminal,
    interrupted: bool,
) -> Option<agent_event::AgentEvent> {
    match decision {
        LeadTerminal::EmitError | LeadTerminal::EmitRunCloseout => {
            Some(build_terminal_release_event(
                run_id,
                None,
                true,
                false,
                false,
                &db::RunCloseoutMetadata::default(),
                interrupted,
            ))
        }
        LeadTerminal::EmitCompleted => Some(agent_event::AgentEvent::Completed {
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            final_text: None,
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: Some(false),
        }),
        LeadTerminal::None => None,
    }
}

/// Any synthetic error must already have been pushed into `pending_terminals` by the caller before this function is called.
/// Obtain the sole event through `record_synthetic_cli_error`; do not reconstruct it here from `Option<String>`,
/// which would allow the two payloads to diverge silently. This function only appends a release terminal event to the end of the queue.
pub(super) fn lead_terminal_events_for_barrier(
    run_id: &str,
    decision: &LeadTerminal,
    interrupted: bool,
    mut pending_terminals: Vec<agent_event::AgentEvent>,
) -> Vec<agent_event::AgentEvent> {
    if let Some(event) = build_lead_terminal_release_event(run_id, decision, interrupted) {
        pending_terminals.push(event);
    }
    pending_terminals
}

pub(super) fn transition_lead_spawn_handoff<K, F>(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    db: Option<&crate::db::Db>,
    terminated: &AtomicBool,
    session_id: &str,
    pid: u32,
    run_id: &str,
    kill: K,
    emit_terminal_release: F,
) -> Result<bool, String>
where
    K: FnOnce(u32),
    F: FnOnce(&agent_event::AgentEvent),
{
    enum Action {
        Stream,
        Stopped,
        Abort,
        Poisoned(String),
    }

    let action = {
        match running.0.lock() {
            Ok(mut slots) => match slots.get(session_id).cloned() {
                Some(RunSlot::Launching {
                    stop_requested: true,
                }) => {
                    kill(pid);
                    terminated.store(true, Ordering::SeqCst);
                    // Stopped reflects an explicit global stop, so do not install retry backoff for an intentional cancellation.
                    // Intentionally do not call `note_resume_failure`; removing the slot is the terminal state here, so no additional step is needed.
                    slots.remove(session_id);
                    Action::Stopped
                }
                Some(RunSlot::Launching {
                    stop_requested: false,
                }) => {
                    slots.insert(session_id.to_string(), RunSlot::Running(pid));
                    Action::Stream
                }
                _ => {
                    kill(pid);
                    terminated.store(true, Ordering::SeqCst);
                    // Abort means the slot no longer belongs to this launch; install backoff before releasing it to avoid immediate retry races.
                    // Call `note_resume_failure` before removing the slot to preserve the state-before-slot-removal ordering protocol.
                    // note_resume_failure only touches the independent RESUME_STATE Mutex, so it neither conflicts with nor re-enters `slots`,
                    // which holds the running.0 lock. Call `slots.remove` only afterward, while still in the same critical section,
                    // closing the concurrency window where the slot already appears empty externally but the backoff state is not yet installed.
                    // If a concurrent drain acquires the running.0 lock immediately after `slots.remove` and sees the slot empty, the backoff is already installed.
                    note_resume_failure(session_id);
                    slots.remove(session_id);
                    Action::Abort
                }
            },
            Err(poisoned) => {
                // Handle poisoned `running.0` explicitly so an early error return cannot bypass slot cleanup.
                // No guard was acquired, so `slots.remove` was never called, but the outer caller's `Err(_)` branch in `handoff_lead_child` still calls
                // `drain_after_run_release`, violating the slot-release-before-drain ordering invariant because the draining code cannot know whether the slot was removed.
                // Poisoning a std::sync::Mutex does not discard its data: `PoisonError::into_inner` recovers the internal state from the last lock holder before poisoning.
                // Use it here to recover the guard, perform the same closeout as Abort by killing, setting terminated, recording `note_resume_failure`,
                // and actually calling `slots.remove`, then propagate the poisoned error to the caller. The caller still sees `Err`,
                // but the slot is now truly empty, so drain will not proceed while a slot remains occupied.
                let error = poisoned.to_string();
                terminated.store(true, Ordering::SeqCst);
                let mut slots = poisoned.into_inner();
                kill(pid);
                note_resume_failure(session_id);
                slots.remove(session_id);
                Action::Poisoned(error)
            }
        }
        // `slots`, which holds the running.0 lock, is dropped when each branch ends. The match action branches below call
        // refresh_session_runtime, which locks it again, so this lock must never be carried into those branches.
    };

    match action {
        Action::Stream => Ok(true),
        Action::Stopped => {
            // The Stopped branch actually removed the slot above with slots.remove, so refresh the runtime state.
            if let Some(db) = db {
                refresh_session_runtime(db, running, team_running, session_id);
            }
            let event =
                build_lead_terminal_release_event(run_id, &LeadTerminal::EmitRunCloseout, true)
                    .expect("stopped lead 必须生成终态释放事件");
            emit_terminal_release(&event);
            Ok(false)
        }
        Action::Abort => {
            // Likewise, the Abort branch actually removed the slot.
            if let Some(db) = db {
                refresh_session_runtime(db, running, team_running, session_id);
            }
            Ok(false)
        }
        Action::Poisoned(error) => {
            // Refresh runtime state after removing a slot on poison, as on Abort, before propagating the error.
            if let Some(db) = db {
                refresh_session_runtime(db, running, team_running, session_id);
            }
            Err(error)
        }
    }
}
