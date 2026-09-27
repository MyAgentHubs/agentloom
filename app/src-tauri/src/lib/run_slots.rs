use crate::{db, kill_process_group, member_runner, refresh_session_runtime, ui_msg, Locale};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::process::Child;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};

/// Session run slot: Launching is a reservation, the pid in Running is the process-group leader,
/// and Finalizing means stdout is exhausted while the finalizer thread is still finishing up
/// (the pid is not exposed, preventing an accidentally reused pid from being killed).
#[derive(Clone, Debug)]
pub(super) enum RunSlot {
    Launching {
        stop_requested: bool,
    },
    Running(u32),
    // The finalizer thread constructs this variant after stdout is exhausted (the pid is not
    // exposed during wait + persistence, preventing an accidentally reused pid from being killed).
    Finalizing {
        stop_requested: bool,
    },
    Mutating {
        op: &'static str,
    },
    /// Reserves a slot while `start_team_run` dispatches work.
    /// A team run has no single pid (each member process is managed independently by
    /// `member_runner::TeamRunning`; stopping one member goes through `stop_team_member`, not here).
    /// This variant is used only as a busy-gate marker: while occupied, it blocks the delete/archive/
    /// clear operations in the `reserve_mutation` family, until all members reach a terminal state
    /// and `release_team_run_slot` releases it (see that function and the `TeamRunSlotGuard` docs).
    TeamRun,
}

/// Session -> run slot. Used for re-entry prevention, handoff, and stop-by-process-group.
#[derive(Clone, Default)]
pub(super) struct Running(pub(super) Arc<Mutex<HashMap<String, RunSlot>>>);

#[derive(Clone)]
pub(super) struct RegisteredHandoffProcess {
    pub(super) request_id: String,
    pub(super) child: Arc<Mutex<Child>>,
}

#[derive(Clone)]
pub(super) struct RegisteredHandoffRequest {
    pub(super) request_id: String,
    pub(super) cancel_requested: Arc<AtomicBool>,
}

#[derive(Default)]
pub(super) struct HandoffProcessRegistry {
    pub(super) requests: HashMap<String, RegisteredHandoffRequest>,
    pub(super) children: HashMap<String, RegisteredHandoffProcess>,
}

/// Running continuation-draft subprocesses, isolated by parent session.
#[derive(Clone, Default)]
pub(super) struct HandoffProcesses(pub(super) Arc<Mutex<HandoffProcessRegistry>>);

pub(super) struct HandoffRequestGuard {
    registry: HandoffProcesses,
    session_id: String,
    request_id: String,
    pub(super) cancel_requested: Arc<AtomicBool>,
}

impl HandoffRequestGuard {
    pub(super) fn register(
        registry: &HandoffProcesses,
        session_id: &str,
        request_id: &str,
    ) -> Result<Self, String> {
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let mut processes = registry
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if processes.requests.contains_key(session_id) {
            return Err(ui_msg::al_err(
                "team.oneshotFailed",
                &[("detail", "handoff request already registered".to_string())],
            ));
        }
        processes.requests.insert(
            session_id.to_string(),
            RegisteredHandoffRequest {
                request_id: request_id.to_string(),
                cancel_requested: cancel_requested.clone(),
            },
        );
        Ok(Self {
            registry: registry.clone(),
            session_id: session_id.to_string(),
            request_id: request_id.to_string(),
            cancel_requested,
        })
    }
}

impl Drop for HandoffRequestGuard {
    fn drop(&mut self) {
        let mut processes = self
            .registry
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let is_same_request = processes
            .requests
            .get(&self.session_id)
            .map(|registered| {
                registered.request_id == self.request_id
                    && Arc::ptr_eq(&registered.cancel_requested, &self.cancel_requested)
            })
            .unwrap_or(false);
        if is_same_request {
            processes.requests.remove(&self.session_id);
        }
    }
}

pub(super) fn try_reserve(running: &Running, session_id: &str) -> Result<(), String> {
    let mut m = running.0.lock().map_err(|e| e.to_string())?;
    if m.contains_key(session_id) {
        return Err(format!("SESSION_ALREADY_RUNNING:{session_id}"));
    }
    m.insert(
        session_id.to_string(),
        RunSlot::Launching {
            stop_requested: false,
        },
    );
    Ok(())
}

pub(super) fn reserve_new_session_run(
    conn: &Connection,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    locale: Locale,
) -> Result<(), String> {
    let team_in_db = conn
        .query_row(
            "SELECT 1 FROM team_run_pending WHERE session_id = ?1 AND state = 'running' LIMIT 1",
            [session_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .is_some();
    let reserved = if team_in_db {
        false
    } else {
        team_running.reserve_if_session_idle(session_id, || try_reserve(running, session_id))?
    };
    if !reserved {
        let detail = match locale {
            Locale::Zh => "队员仍在执行上一轮派单",
            Locale::En => "Team members are still executing assignments from the previous run",
        };
        return Err(ui_msg::al_err(
            "run.teamMembersActive",
            &[("detail", detail.to_string())],
        ));
    }
    // Record slot occupancy only after `reserved` is confirmed true, keeping persisted busy state tied to successful reservation.
    // Any earlier `?`/`return Err` has already exited. This is the only successful slot-reservation
    // exit shared by solo/lead `send_message` (`try_reserve` itself has no conn, and this is the
    // successful point closest to conn). The run_id has not been generated on site yet, so write
    // None. This table serves only the busy/idle bit, and the caller's later run_id is not backfilled.
    // This is a "reserve" write (not a "release/remove-slot" write), so it does not go through
    // refresh_session_runtime; see that function's documentation for the division of responsibility.
    // Failure is non-fatal but is no longer completely swallowed.
    if let Err(e) = db::set_session_runtime(conn, session_id, db::SESSION_RUNTIME_RUNNING, None) {
        eprintln!("session_runtime running write failed (non-fatal): {e}");
    }
    Ok(())
}

#[tauri::command]
pub(super) fn is_team_session_running(
    team_running: State<member_runner::TeamRunning>,
    session_id: String,
) -> Result<bool, String> {
    team_running.is_session_running(&session_id)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionRunStateView {
    pub status: String,
    pub updated_at: i64,
}

pub(super) fn query_session_run_state(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<SessionRunStateView>, String> {
    db::get_session_runtime(conn, session_id)
        .map(|opt| {
            opt.map(|rt| SessionRunStateView {
                status: rt.status,
                updated_at: rt.updated_at,
            })
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn get_session_run_state(
    db: State<crate::db::Db>,
    session_id: String,
) -> Result<Option<SessionRunStateView>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    query_session_run_state(&conn, &session_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SpawnHandoffAction {
    Stream,
    StopAndFinalize,
    Abort,
}

pub(super) fn transition_spawn_handoff(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    db: Option<&crate::db::Db>,
    session_id: &str,
    pid: u32,
) -> Result<SpawnHandoffAction, String> {
    transition_spawn_handoff_with_abort_kill(
        running,
        team_running,
        db,
        session_id,
        pid,
        kill_process_group,
    )
}

pub(super) fn transition_spawn_handoff_with_abort_kill<F>(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    db: Option<&crate::db::Db>,
    session_id: &str,
    pid: u32,
    kill_abort_process_group: F,
) -> Result<SpawnHandoffAction, String>
where
    F: FnOnce(u32),
{
    let mut slots = running.0.lock().map_err(|e| e.to_string())?;
    match slots.get(session_id).cloned() {
        Some(RunSlot::Launching {
            stop_requested: true,
        }) => {
            slots.insert(
                session_id.to_string(),
                RunSlot::Finalizing {
                    stop_requested: true,
                },
            );
            Ok(SpawnHandoffAction::StopAndFinalize)
        }
        Some(RunSlot::Launching {
            stop_requested: false,
        }) => {
            slots.insert(session_id.to_string(), RunSlot::Running(pid));
            Ok(SpawnHandoffAction::Stream)
        }
        other => {
            // Invariant: during handoff, this session's slot can only be the Launching slot that this
            // run just reserved. Finalizing must never appear here (a quick Stop enters it only from
            // Launching(true) above). If it appears, the lifecycle invariant has been broken. Call
            // killpg while still holding the lock, ensuring the same session cannot reserve again
            // before the child has received a termination signal. The unexpected existing slot may
            // belong to another run, so it must not be removed by mistake.
            eprintln!("handoff: slot 异常 {other:?} · killpg 防泄漏");
            kill_abort_process_group(pid);
            drop(slots);
            // This exceptional branch does not modify the Running slot (the
            // existing defensive design cannot establish ownership and must not remove another run's
            // slot). Because the "normal handoff" assumption has already been broken, recompute
            // session_runtime as a fallback to prevent table state from diverging from in-memory
            // truth on this extreme path.
            if let Some(db) = db {
                refresh_session_runtime(db, running, team_running, session_id);
            }
            Ok(SpawnHandoffAction::Abort)
        }
    }
}

pub(super) fn transition_auth_retry_handoff(
    running: &Running,
    session_id: &str,
    retry_pid: u32,
) -> Result<bool, String> {
    let mut slots = running.0.lock().map_err(|error| error.to_string())?;
    match slots.get(session_id) {
        Some(RunSlot::Finalizing {
            stop_requested: false,
        }) => {
            slots.insert(session_id.to_string(), RunSlot::Running(retry_pid));
            Ok(true)
        }
        Some(RunSlot::Finalizing {
            stop_requested: true,
        }) => Ok(false),
        _ => Ok(false),
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum AuthRetryHandoff {
    Continue,
    Interrupted,
    Failed { detail: String },
}

pub(super) fn classify_auth_retry_handoff(
    handoff: Result<bool, String>,
    stop_requested: bool,
) -> AuthRetryHandoff {
    match handoff {
        Ok(true) => AuthRetryHandoff::Continue,
        Ok(false) if stop_requested => AuthRetryHandoff::Interrupted,
        Ok(false) => AuthRetryHandoff::Failed {
            detail: "auth retry lost the run slot".to_string(),
        },
        Err(_) if stop_requested => AuthRetryHandoff::Interrupted,
        Err(error) => AuthRetryHandoff::Failed {
            detail: format!("auth retry handoff failed: {error}"),
        },
    }
}

pub(super) fn resolve_auth_retry_handoff<C, S>(
    handoff: Result<bool, String>,
    cleanup: C,
    read_stop: S,
) -> AuthRetryHandoff
where
    C: FnOnce(),
    S: FnOnce() -> bool,
{
    if matches!(&handoff, Ok(true)) {
        AuthRetryHandoff::Continue
    } else {
        cleanup();
        classify_auth_retry_handoff(handoff, read_stop())
    }
}

pub(super) struct ReservationGuard {
    running: Running,
    sid: String,
    armed: bool,
    // Refresh session_runtime on early failure unwind so failed starts do not leave stale runtime state.
    // Once `disarm()` is called (normal handoff succeeded), this coverage is no longer needed. It is
    // used only in the window where reservation succeeded but an early `?` failure occurred before
    // disarm. `None` means there is no refresh handle (the default state at test call sites, where Drop
    // performs only the original slot cleanup and does not touch db, requiring zero test changes).
    // After production call sites attach it with `with_refresh`, Drop recomputes session_runtime after
    // removing the slot.
    refresh: Option<(member_runner::TeamRunning, AppHandle)>,
    #[cfg(test)]
    pub(super) test_on_disarm: Option<Box<dyn FnMut() + Send>>,
}

impl ReservationGuard {
    pub(super) fn new(running: Running, sid: String) -> Self {
        Self {
            running,
            sid,
            armed: true,
            refresh: None,
            #[cfg(test)]
            test_on_disarm: None,
        }
    }

    /// Production call sites attach this immediately after obtaining the guard. If an early failure
    /// unwinds and Drop removes the slot, this recomputes and writes back session_runtime. Test call
    /// sites do not call this method, so `refresh` remains `None`.
    pub(super) fn with_refresh(
        mut self,
        team_running: member_runner::TeamRunning,
        app: AppHandle,
    ) -> Self {
        self.refresh = Some((team_running, app));
        self
    }

    pub(super) fn disarm(&mut self) {
        #[cfg(test)]
        if let Some(on_disarm) = self.test_on_disarm.as_mut() {
            on_disarm();
        }
        self.armed = false;
    }
}

pub(super) fn wait_for_aborted_child<W>(guard: &mut ReservationGuard, wait_for_child: W)
where
    W: FnOnce(),
{
    // A missing-slot Abort reopens reservation after kill. The old guard must be disarmed first;
    // otherwise, when the old call returns, its Drop would mistakenly remove a new request's
    // Launching slot inserted during the wait window.
    guard.disarm();
    wait_for_child();
}

pub(super) fn abort_spawn_after_register_failure<K, W>(
    running: &Running,
    team_running: &member_runner::TeamRunning,
    db: Option<&crate::db::Db>,
    session_id: &str,
    pid: u32,
    guard: &mut ReservationGuard,
    kill: K,
    wait_for_child: W,
) -> Result<(), String>
where
    K: FnOnce(u32),
    W: FnOnce(),
{
    match running.0.lock() {
        Ok(mut slots) => {
            // register_run occurs after a successful handoff: the slot is either this run's
            // Running(pid), or a concurrent Stop has changed it to this run's Finalizing. After
            // killing while holding the lock, disarm the old guard before clearing the slot, ensuring
            // that a new request can reserve safely during the wait window and that the old guard
            // cannot remove it by mistake.
            let owns_slot = matches!(
                slots.get(session_id),
                Some(RunSlot::Running(actual_pid)) if *actual_pid == pid
            ) || matches!(slots.get(session_id), Some(RunSlot::Finalizing { .. }));
            kill(pid);
            guard.disarm();
            if owns_slot {
                slots.remove(session_id);
            }
            drop(slots);
            // Recompute only when a slot was actually removed (owns_slot).
            // The `running.0` lock was released by `drop(slots)` above; refresh_session_runtime
            // acquires it again internally, so retaining it here would cause a same-thread reentrant
            // deadlock.
            if owns_slot {
                if let Some(db) = db {
                    refresh_session_runtime(db, running, team_running, session_id);
                }
            }
            wait_for_child();
            Ok(())
        }
        Err(error) => {
            // If the slot lock is poisoned, still terminate the spawned child on a best-effort basis,
            // but keep subsequent cleanup bounded: return on timeout even if the child is not reaped
            // (which may leave a zombie on Unix; on non-Unix, taskkill may also fail and the process
            // may even continue running). This runs on the synchronous Tauri command's calling thread;
            // an unbounded wait would freeze all of send_message and the UI. Leaking one handle is
            // preferable to freezing the UI. Slot ownership cannot be proven here, so do not perform
            // unlocked cleanup; report the error to the caller.
            kill(pid);
            wait_for_child();
            Err(error.to_string())
        }
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self.running.0.lock() {
            Ok(mut m) => {
                // Invariant: an accepted spawn handoff immediately disarms; an armed Drop path normally
                // sees only the Launching slot it just reserved. A quick Stop handoff transitions to
                // Finalizing before disarming, so even during an exceptional unwind this removes only
                // Launching and does not mistakenly release Finalizing/Running, which still require the
                // unified finalizer to finish cleanup.
                if matches!(m.get(&self.sid), Some(RunSlot::Launching { .. })) {
                    m.remove(&self.sid);
                }
            }
            Err(_) => return,
        }
        // `m` (the running.0 lock) was dropped when the match branch above ended.
        // refresh_session_runtime acquires the same lock again internally; retaining it here would
        // cause a same-thread reentrant deadlock.
        if let Some((team_running, app)) = &self.refresh {
            if let Some(db) = app.try_state::<crate::db::Db>() {
                refresh_session_runtime(db.inner(), &self.running, team_running, &self.sid);
            }
        }
    }
}

pub(super) struct MutationGuard {
    running: Running,
    sid: String,
}

impl Drop for MutationGuard {
    fn drop(&mut self) {
        let Ok(mut m) = self.running.0.lock() else {
            return;
        };
        let should_remove = match m.get(&self.sid) {
            Some(RunSlot::Mutating { op }) => {
                let _ = *op;
                true
            }
            _ => false,
        };
        if should_remove {
            m.remove(&self.sid);
        }
    }
}

#[allow(dead_code)]
pub(super) fn reserve_mutation(
    running: &Running,
    session_id: &str,
    op: &'static str,
) -> Result<MutationGuard, String> {
    let mut m = running.0.lock().map_err(|e| e.to_string())?;
    if m.contains_key(session_id) {
        return Err(format!("SESSION_BUSY:{op}"));
    }
    m.insert(session_id.to_string(), RunSlot::Mutating { op });
    Ok(MutationGuard {
        running: running.clone(),
        sid: session_id.to_string(),
    })
}

#[allow(dead_code)]
pub(super) fn reserve_thread_mutations(
    running: &Running,
    ids: &[String],
    op: &'static str,
) -> Result<Vec<MutationGuard>, String> {
    let mut guards = Vec::new();
    for id in ids {
        guards.push(reserve_mutation(running, id, op)?);
    }
    Ok(guards)
}

/// Occupy a Running slot when `start_team_run` starts, making the `reserve_mutation` gate
/// (used by delete/archive/purge/restore) effective for team runs. Before the fix, a team run never
/// occupied this slot, so mutating operations such as soft delete could proceed directly while a
/// member was still writing its worktree.
/// If the slot cannot be acquired (`contains_key` finds another slot), use the same rejection semantics
/// as solo `try_reserve`, allowing the caller to reuse the existing `SESSION_ALREADY_RUNNING:` error path.
pub(crate) fn reserve_team_run_slot(running: &Running, session_id: &str) -> Result<(), String> {
    let mut m = running.0.lock().map_err(|e| e.to_string())?;
    if m.contains_key(session_id) {
        return Err(format!("SESSION_ALREADY_RUNNING:{session_id}"));
    }
    m.insert(session_id.to_string(), RunSlot::TeamRun);
    Ok(())
}

/// Paired release for `reserve_team_run_slot`: remove the slot only if it is still the `TeamRun`
/// marker occupied by this run. This "check before remove" is purely defensive. A crate-wide inventory
/// currently finds no non-team-run path calling this function (`run_single_worker`, where the lead
/// synchronously dispatches one member through `dispatch_worker`, does not call `release_team_run_slot`
/// at all; its slot belongs to the lead's own `RunSlot::Running`/`Finalizing` throughout and is released
/// by the `reserve_new_session_run`/`ReservationGuard` lifecycle). The `matches!` protects against a
/// future call site accidentally passing a `session_id` it does not own, not a currently known collision.
pub(crate) fn release_team_run_slot(running: &Running, session_id: &str) {
    let Ok(mut m) = running.0.lock() else {
        return;
    };
    if matches!(m.get(session_id), Some(RunSlot::TeamRun)) {
        m.remove(session_id);
    }
}

/// Safety net for the preparation interval from when `start_team_run` starts until it actually spawns
/// members and hands release responsibility to `release_team_run_slot` (writing team_run_pending / goal
/// events / EventTransport registration, etc.). Any early `?` failure during this interval bypasses the
/// `spawn_member`/`run_member_finished` cleanup path, so this guard must release the slot through Drop;
/// otherwise, the occupied slot would permanently block delete/archive for this session (turning the
/// "lost work" protection into a new "cannot delete" failure). Once the spawn loop is actually entered
/// (even the "all synchronous failures" branch), the caller must call `disarm()`. Release is then handled
/// by `release_team_run_slot` at the terminal-state point determined by `run_member_finished` (possibly
/// the synchronous branch inside the loop, or the `spawn_member` background reader thread), avoiding
/// double release/double retention. This mirrors the solo-side `ReservationGuard` pattern (reserve ->
/// early failure falls back to Drop -> disarm once handed off to the actual executor).
pub(super) struct TeamRunSlotGuard {
    running: Running,
    session_id: String,
    armed: bool,
    // Like `ReservationGuard::refresh`, `None` is the default test state (Drop
    // clears only the Running slot and does not touch db). Production call sites attach it through
    // `with_refresh`, after which Drop recomputes session_runtime after removing the slot (covering the
    // window where `start_team_run` fails early with `?` during preparation and the guard was never disarmed).
    refresh: Option<(member_runner::TeamRunning, AppHandle)>,
}

impl TeamRunSlotGuard {
    pub(super) fn new(running: Running, session_id: String) -> Self {
        Self {
            running,
            session_id,
            armed: true,
            refresh: None,
        }
    }

    pub(super) fn with_refresh(
        mut self,
        team_running: member_runner::TeamRunning,
        app: AppHandle,
    ) -> Self {
        self.refresh = Some((team_running, app));
        self
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TeamRunSlotGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // release_team_run_slot briefly locks running.0 internally and drops the lock before returning.
        // No extra handling is needed before calling refresh here because the lock is no longer held
        // (`refresh_session_runtime` reacquires it).
        release_team_run_slot(&self.running, &self.session_id);
        if let Some((team_running, app)) = &self.refresh {
            if let Some(db) = app.try_state::<crate::db::Db>() {
                refresh_session_runtime(db.inner(), &self.running, team_running, &self.session_id);
            }
        }
    }
}
