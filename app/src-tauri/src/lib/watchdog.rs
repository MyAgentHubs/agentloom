use crate::{agent_event, kill_process_group, sandbox, ui_msg, Locale, ParseFn, RunSlot, Running};
use std::io::{Read, Write};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

pub(crate) const STDERR_TAIL_LIMIT: usize = 4096;
pub(super) const FIRST_EVENT_TIMEOUT_SECS: u64 = 60;
pub(super) const FIRST_EVENT_STDERR_LINES: usize = 3;
pub(super) const FIRST_EVENT_WAIT_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(200);
// Once stdout has closed, two seconds per cleanup owner is enough grace for a normal exit while
// keeping process/pipe cleanup from delaying durable messages and the terminal event for minutes.
pub(super) const FINALIZER_OWNER_WAIT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);
pub(super) const FINALIZER_OWNER_WAIT_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(50);

/// Contract anchor: failing to call finish is an internal signal and must not be exposed to users.
#[allow(dead_code)]
pub(crate) const LEAD_FINISH_WARNING_USER_FACING: bool = false;

pub(super) fn append_stderr_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    tail.extend_from_slice(chunk);
    if tail.len() > STDERR_TAIL_LIMIT {
        let drop_len = tail.len() - STDERR_TAIL_LIMIT;
        tail.drain(0..drop_len);
    }
}

#[cfg(test)]
pub(crate) fn spawn_stderr_tail_thread<R>(
    stderr: R,
    log: Option<std::fs::File>,
) -> std::thread::JoinHandle<String>
where
    R: Read + Send + 'static,
{
    spawn_stderr_tail_thread_shared(stderr, log).0
}

pub(super) type SharedStderrTail = Arc<Mutex<Vec<u8>>>;

pub(super) fn spawn_stderr_tail_thread_shared<R>(
    mut stderr: R,
    mut log: Option<std::fs::File>,
) -> (std::thread::JoinHandle<String>, SharedStderrTail)
where
    R: Read + Send + 'static,
{
    let shared_tail = Arc::new(Mutex::new(Vec::new()));
    let shared_tail_t = shared_tail.clone();
    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match stderr.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if let Some(file) = log.as_mut() {
                        let _ = file.write_all(chunk);
                    }
                    if let Ok(mut tail) = shared_tail_t.lock() {
                        append_stderr_tail(&mut tail, chunk);
                    }
                }
                Err(_) => break,
            }
        }
        if let Some(file) = log.as_mut() {
            let _ = file.flush();
        }
        shared_tail_t
            .lock()
            .map(|tail| String::from_utf8_lossy(&tail).trim().to_string())
            .unwrap_or_default()
    });
    (handle, shared_tail)
}

pub(super) fn stderr_tail_last_lines(tail: &SharedStderrTail) -> String {
    let Ok(tail) = tail.lock() else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&tail);
    let mut lines = text
        .lines()
        .rev()
        .take(FIRST_EVENT_STDERR_LINES)
        .collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n").trim().to_string()
}

pub(super) fn stderr_tail_snapshot(tail: &SharedStderrTail) -> String {
    tail.lock()
        .map(|tail| String::from_utf8_lossy(&tail).trim().to_string())
        .unwrap_or_default()
}

pub(super) fn finalizer_stderr_tail_after_owner_wait(
    outcome: FinalizerOwnerWait<String>,
    live_tail: &SharedStderrTail,
) -> String {
    match outcome {
        FinalizerOwnerWait::Finished(tail) => tail,
        FinalizerOwnerWait::TimedOut | FinalizerOwnerWait::WaitError => {
            // Match the completed join path: the shared buffer is already bounded to
            // STDERR_TAIL_LIMIT (4096 bytes), so do not further truncate a cleanup-timeout
            // diagnostic to the first-event watchdog's three-line summary.
            stderr_tail_snapshot(live_tail)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FirstEventWatchdogState {
    Armed,
    FirstLineSeen,
    StopRequested,
    TimedOut,
    StdoutClosed,
}

pub(super) fn first_event_watchdog_should_trigger(state: FirstEventWatchdogState) -> bool {
    state == FirstEventWatchdogState::Armed
}

pub(super) fn claim_first_event_watchdog_timeout<K, R>(
    running: &Running,
    session_id: &str,
    pid: u32,
    kill: K,
    report: R,
) -> Result<bool, String>
where
    K: FnOnce(u32),
    R: FnOnce(),
{
    // Safety invariant: successfully observing Running(pid) through this healthy slot lock means
    // the owner reader has not yet transitioned to Finalizing and therefore has not called wait.
    // Kill, hide the pid, and report the timeout while retaining that same lock, so Stop cannot
    // interleave. If the mutex is poisoned, lock() fails permanently here just as it does for any
    // later claim; the watchdog degrades to no claim/no kill instead of acting on an unproven pid.
    let mut slots = running.0.lock().map_err(|error| error.to_string())?;
    match slots.get(session_id) {
        Some(RunSlot::Running(actual_pid)) if *actual_pid == pid => {
            kill(pid);
            slots.insert(
                session_id.to_string(),
                RunSlot::Finalizing {
                    stop_requested: false,
                },
            );
            report();
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Registry adapter used by the shared first-event watchdog. Implementations must only claim
/// while their slot lock proves the pid has not been reaped, and must kill/report under that lock.
pub(super) trait FirstEventWatchdogRegistry: Clone + Send + 'static {
    type Key: Send + 'static;

    fn claim_first_event_watchdog_timeout<R>(
        &self,
        key: &Self::Key,
        pid: u32,
        report: R,
    ) -> Result<bool, String>
    where
        R: FnOnce();
}

impl FirstEventWatchdogRegistry for Running {
    type Key = String;

    fn claim_first_event_watchdog_timeout<R>(
        &self,
        session_id: &Self::Key,
        pid: u32,
        report: R,
    ) -> Result<bool, String>
    where
        R: FnOnce(),
    {
        claim_first_event_watchdog_timeout(self, session_id, pid, kill_process_group, report)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FirstEventOwnerWait<T> {
    Exited(T),
    TimedOut(Option<T>),
    WaitError,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn wait_for_first_event_owner<C, T, E, TryWait, Wait, Kill, Now, Sleep>(
    child: &mut C,
    pid: u32,
    deadline: Instant,
    mut try_wait: TryWait,
    wait: Wait,
    kill: Kill,
    mut now: Now,
    mut sleep: Sleep,
) -> FirstEventOwnerWait<T>
where
    TryWait: FnMut(&mut C) -> Result<Option<T>, E>,
    Wait: FnOnce(&mut C) -> Result<T, E>,
    Kill: FnOnce(u32),
    Now: FnMut() -> Instant,
    Sleep: FnMut(std::time::Duration),
{
    loop {
        match try_wait(child) {
            Ok(Some(status)) => return FirstEventOwnerWait::Exited(status),
            Ok(None) => {}
            Err(_) => return FirstEventOwnerWait::WaitError,
        }

        let current = now();
        if current >= deadline {
            // This thread exclusively owns child and no wait has succeeded, so pid cannot have
            // been reaped or reused before this kill. Reap synchronously after terminating it.
            kill(pid);
            return FirstEventOwnerWait::TimedOut(wait(child).ok());
        }
        sleep(
            deadline
                .saturating_duration_since(current)
                .min(FIRST_EVENT_WAIT_POLL_INTERVAL),
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FinalizerOwnerWait<T> {
    Finished(T),
    TimedOut,
    WaitError,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalizer_owner_wait<
    Owner,
    T,
    PollError,
    WaitError,
    Poll,
    Wait,
    Kill,
    Now,
    Sleep,
    Continue,
    R,
>(
    mut owner: Owner,
    deadline: Instant,
    mut poll: Poll,
    wait: Wait,
    kill: Kill,
    mut now: Now,
    mut sleep: Sleep,
    continue_finalizer: Continue,
) -> R
where
    Poll: FnMut(&mut Owner) -> Result<bool, PollError>,
    Wait: FnOnce(Owner) -> Result<T, WaitError>,
    Kill: FnOnce(),
    Now: FnMut() -> Instant,
    Sleep: FnMut(std::time::Duration),
    Continue: FnOnce(FinalizerOwnerWait<T>) -> R,
{
    let outcome = loop {
        match poll(&mut owner) {
            Ok(true) => {
                break match wait(owner) {
                    Ok(value) => FinalizerOwnerWait::Finished(value),
                    Err(_) => FinalizerOwnerWait::WaitError,
                };
            }
            Ok(false) => {}
            Err(_) => break FinalizerOwnerWait::WaitError,
        }

        let current = now();
        if current >= deadline {
            // Cleanup is best-effort. Never synchronously reap after this kill: inherited pipe
            // handles can keep that wait blocked, and finalization must still persist and emit.
            kill();
            break FinalizerOwnerWait::TimedOut;
        }
        sleep(
            deadline
                .saturating_duration_since(current)
                .min(FINALIZER_OWNER_WAIT_POLL_INTERVAL),
        );
    };

    // Keep continuation inside the bounded-wait contract so every outcome, including timeout,
    // proceeds into the caller's remaining finalizer work.
    continue_finalizer(outcome)
}

pub(super) fn wait_for_child_cleanup_bounded(child: &mut Child, pid: u32) {
    finalizer_owner_wait(
        child,
        Instant::now() + FINALIZER_OWNER_WAIT_TIMEOUT,
        |child| Child::try_wait(child).map(|status| status.is_some()),
        Child::wait,
        || kill_process_group(pid),
        Instant::now,
        std::thread::sleep,
        |_| (),
    );
}

pub(super) fn transition_stdout_closed_to_finalizing(running: &Running, session_id: &str) -> bool {
    let mut slots = match running.0.lock() {
        Ok(slots) => slots,
        Err(poisoned) => poisoned.into_inner(),
    };
    let carry = match slots.get(session_id) {
        Some(RunSlot::Launching { stop_requested }) => *stop_requested,
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
}

pub(super) fn finalizer_stop_requested(running: &Running, session_id: &str) -> bool {
    let slots = match running.0.lock() {
        Ok(slots) => slots,
        Err(poisoned) => poisoned.into_inner(),
    };
    matches!(
        slots.get(session_id),
        Some(RunSlot::Finalizing {
            stop_requested: true
        })
    )
}

pub(super) fn finalizer_exit_success_after_owner_wait<T, IsSuccess>(
    exit_status: Option<&T>,
    owner_timed_out: bool,
    completed_seen: bool,
    is_success: IsSuccess,
) -> bool
where
    IsSuccess: FnOnce(&T) -> bool,
{
    exit_status.is_some_and(is_success) || (owner_timed_out && completed_seen)
}

pub(super) enum FinalizerCloseoutContinuation {
    Normal { exit_success: bool },
    CleanupTimedOut { exit_success: bool },
}

impl FinalizerCloseoutContinuation {
    pub(super) fn exit_success(&self) -> bool {
        match self {
            Self::Normal { exit_success } | Self::CleanupTimedOut { exit_success } => *exit_success,
        }
    }

    pub(super) fn persist_then_emit<Persist, Emit>(self, persist: Persist, emit_terminal: Emit)
    where
        Persist: FnOnce(),
        Emit: FnOnce(),
    {
        // A cleanup timeout is deliberately not a control-flow exit. Durable assistant output
        // must be written before the terminal release even when the process could not be reaped.
        match self {
            Self::Normal { .. } | Self::CleanupTimedOut { .. } => {}
        }
        persist();
        emit_terminal();
    }
}

pub(super) fn prepare_finalizer_closeout<T, IsSuccess>(
    outcome: FinalizerOwnerWait<T>,
    first_line_seen: bool,
    pending_completed: Option<&agent_event::AgentEvent>,
    is_success: IsSuccess,
) -> (Option<T>, bool, FinalizerCloseoutContinuation)
where
    IsSuccess: FnOnce(&T) -> bool,
{
    let (exit_status, cleanup_timed_out, owner_timed_out) = match outcome {
        FinalizerOwnerWait::Finished(status) => (Some(status), false, false),
        FinalizerOwnerWait::TimedOut => (None, true, !first_line_seen),
        FinalizerOwnerWait::WaitError => (None, false, false),
    };
    let completed_seen = matches!(
        pending_completed,
        Some(agent_event::AgentEvent::Completed { .. })
    );
    let exit_success = finalizer_exit_success_after_owner_wait(
        exit_status.as_ref(),
        cleanup_timed_out,
        completed_seen,
        is_success,
    );
    let continuation = if cleanup_timed_out {
        FinalizerCloseoutContinuation::CleanupTimedOut { exit_success }
    } else {
        FinalizerCloseoutContinuation::Normal { exit_success }
    };
    (exit_status, owner_timed_out, continuation)
}

#[derive(Clone)]
pub(super) struct FirstEventWatchdogSignal {
    pub(super) state: Arc<(Mutex<FirstEventWatchdogState>, Condvar)>,
    pub(super) timeout_stderr: Arc<Mutex<Option<String>>>,
}

impl FirstEventWatchdogSignal {
    pub(super) fn first_line_seen(&self) {
        self.cancel(FirstEventWatchdogState::FirstLineSeen);
    }

    /// Cancels the watchdog because EOF transfers timeout ownership to the child-owning reader.
    /// Returns whether a first line was observed (or the watchdog already fired/stopped).
    pub(super) fn stdout_closed(&self) -> bool {
        let (state, wake) = &*self.state;
        let Ok(mut state) = state.lock() else {
            return true;
        };
        if *state == FirstEventWatchdogState::Armed {
            *state = FirstEventWatchdogState::StdoutClosed;
            wake.notify_one();
            return false;
        }
        if *state == FirstEventWatchdogState::FirstLineSeen {
            *state = FirstEventWatchdogState::StdoutClosed;
            wake.notify_one();
        }
        true
    }

    fn cancel(&self, reason: FirstEventWatchdogState) {
        let (state, wake) = &*self.state;
        if let Ok(mut state) = state.lock() {
            if *state == FirstEventWatchdogState::Armed {
                *state = reason;
                wake.notify_one();
            }
        }
    }

    pub(super) fn timeout_stderr(&self) -> Option<String> {
        self.timeout_stderr
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }
}

pub(super) fn spawn_first_event_watchdog<R>(
    running: R,
    key: R::Key,
    pid: u32,
    stderr_tail: SharedStderrTail,
    timeout: std::time::Duration,
) -> (FirstEventWatchdogSignal, std::thread::JoinHandle<()>)
where
    R: FirstEventWatchdogRegistry,
{
    let signal = FirstEventWatchdogSignal {
        state: Arc::new((Mutex::new(FirstEventWatchdogState::Armed), Condvar::new())),
        timeout_stderr: Arc::new(Mutex::new(None)),
    };
    let signal_t = signal.clone();
    let handle = std::thread::spawn(move || {
        let (state_lock, wake) = &*signal_t.state;
        let Ok(state) = state_lock.lock() else {
            return;
        };
        let Ok((mut state, wait)) = wake.wait_timeout_while(state, timeout, |state| {
            *state == FirstEventWatchdogState::Armed
        }) else {
            return;
        };
        if !wait.timed_out() || !first_event_watchdog_should_trigger(*state) {
            return;
        }

        let summary = stderr_tail_last_lines(&stderr_tail);
        let claimed = running
            .claim_first_event_watchdog_timeout(&key, pid, || {
                *state = FirstEventWatchdogState::TimedOut;
                if let Ok(mut timeout_stderr) = signal_t.timeout_stderr.lock() {
                    *timeout_stderr = Some(summary);
                }
            })
            .unwrap_or(false);
        if !claimed {
            *state = FirstEventWatchdogState::StopRequested;
        }
    });
    (signal, handle)
}

pub(super) fn first_event_watchdog_error_message(
    locale: Locale,
    code: &str,
    engine: &str,
    binary: &str,
    stderr_summary: &str,
) -> String {
    let stderr_summary = stderr_summary.trim();
    let detail = match (locale, stderr_summary.is_empty()) {
        (Locale::Zh, true) => format!(
            "{engine} 引擎在 {FIRST_EVENT_TIMEOUT_SECS} 秒内没有输出首行 stdout 事件，已终止进程组。程序：{binary}。没有 stderr 输出。"
        ),
        (Locale::Zh, false) => format!(
            "{engine} 引擎在 {FIRST_EVENT_TIMEOUT_SECS} 秒内没有输出首行 stdout 事件，已终止进程组。程序：{binary}。stderr 尾部（最多 {FIRST_EVENT_STDERR_LINES} 行）：{stderr_summary}"
        ),
        (Locale::En, true) => format!(
            "The {engine} engine produced no first stdout event within {FIRST_EVENT_TIMEOUT_SECS} seconds, so its process group was terminated. Program: {binary}. No stderr output was captured."
        ),
        (Locale::En, false) => format!(
            "The {engine} engine produced no first stdout event within {FIRST_EVENT_TIMEOUT_SECS} seconds, so its process group was terminated. Program: {binary}. stderr tail (up to {FIRST_EVENT_STDERR_LINES} lines): {stderr_summary}"
        ),
    };
    ui_msg::al_err(code, &[("detail", detail)])
}

pub(super) fn first_event_watchdog_engine(parse_fn: ParseFn) -> &'static str {
    match parse_fn {
        ParseFn::Claude => "claude",
        ParseFn::Codex => "codex",
        ParseFn::Harness | ParseFn::HarnessPlan => "myagent harness",
    }
}

pub(super) fn first_event_watchdog_binary(parse_fn: ParseFn, command: &Command) -> String {
    if matches!(parse_fn, ParseFn::Claude) {
        sandbox::resolve_claude_bin()
    } else {
        command.get_program().to_string_lossy().into_owned()
    }
}

pub(super) fn should_inject_first_event_watchdog_error(
    stop_requested: bool,
    saw_completed: bool,
    timeout_stderr: Option<&str>,
) -> bool {
    !stop_requested && !saw_completed && timeout_stderr.is_some()
}
