//! One-shot LLM, handoff cancellation, and lead synthesis commands.

use super::{
    agent, build_synthesis_prompt, checkpoint_hook, collect_assistant_text, current_locale, db,
    ensure_session_workspace, make_backend, new_run_id, resolve_harness_search_creds, ui_msg, Arc,
    AtomicBool, BuildContext, Child, Command, Db, ExitStatus, HandoffProcesses, Instant, KeyStore,
    KeyringStore, Mutex, Ordering, ParseFn, Read, RegisteredHandoffProcess, State, Stdio,
};
#[cfg(not(unix))]
use super::{log_windows_taskkill_reap_timeout, windows_taskkill_tree};

/// Generic one-shot (non-streaming) LLM call for an already-built agent command.
///
/// Spawns via `agent::spawn_with_stdin_prompt` and collects output with `wait_with_output()`
/// (non-streaming, synchronous), checks the exit status, and extracts the assistant text via
/// `collect_assistant_text`.
///
/// Does NOT require a lead agent ID, a workers list, or any Team-synthesis assumptions —
/// the caller is responsible for building the command and choosing the prompt.
/// `parse_fn` determines how to parse the agent's stdout (Claude vs. Codex).
///
/// Returns `Err` if the process fails to start, exits non-zero, or produces no
/// assistant text.
pub(super) fn run_oneshot_llm(
    mut command: Command,
    parse_fn: ParseFn,
    stdin_prompt: Option<agent::StdinPrompt>,
) -> Result<String, String> {
    let _hook_guard = checkpoint_hook::guard_for_command(&command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    // When the prompt uses stdin, `Command::output()` cannot be used: it handles spawn and wait
    // internally, leaving no chance to write the body to child.stdin. Spawn manually (the helper
    // handles piping, the writer thread, and EOF), then collect stdout/stderr with `wait_with_output()`.
    let child = agent::spawn_with_stdin_prompt(&mut command, stdin_prompt.as_ref())
        .map_err(|e| ui_msg::al_err("team.oneshotSpawnFailed", &[("detail", e.to_string())]))?;
    let out = child
        .wait_with_output()
        .map_err(|e| ui_msg::al_err("team.oneshotSpawnFailed", &[("detail", e.to_string())]))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            ui_msg::al_err("team.oneshotFailed", &[("detail", out.status.to_string())])
        } else {
            ui_msg::al_err("team.oneshotFailed", &[("detail", stderr)])
        });
    }
    let text = collect_assistant_text(&out.stdout, parse_fn);
    if text.trim().is_empty() {
        return Err(ui_msg::al_err("team.oneshotNoText", &[]));
    }
    Ok(text)
}

pub(super) const HANDOFF_GENERATION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(180);
const HANDOFF_PROCESS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const HANDOFF_PIPE_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

fn unregister_handoff_process(
    registry: &HandoffProcesses,
    session_id: &str,
    child: &Arc<Mutex<Child>>,
) {
    let mut processes = registry
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let still_registered = processes
        .children
        .get(session_id)
        .map(|registered| Arc::ptr_eq(&registered.child, child))
        .unwrap_or(false);
    if still_registered {
        processes.children.remove(session_id);
    }
}

/// Polling interval and deadline for bounded root reaping on non-Unix platforms. taskkill is
/// an asynchronous child: when spawn returns, it has not yet enumerated the process table.
/// Killing the root with `child.kill()` first can remove its pid before taskkill arrives,
/// causing "no running instance" and leaving descendants alive, as observed in GitLab runner.
/// Therefore `kill_handoff_child_with` polls for taskkill to reap the root without racing it;
/// only fall back to `child.kill()` if the root is still alive at the deadline.
#[cfg_attr(unix, allow(dead_code))]
const WINDOWS_TASKKILL_REAP_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(10);
#[cfg_attr(unix, allow(dead_code))]
const WINDOWS_TASKKILL_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Tree termination and bounded root reaping, extracted from the non-Unix branch of
/// `kill_handoff_child` for independent testing. `tree_kill` is an injection point like
/// `kill_child` in `run_oneshot_llm_with_timeout_and_kill`: production passes
/// `windows_taskkill_tree`, while tests pass probe closures to verify timing and pid.
/// Only cross-platform std APIs are used (`Child::try_wait`/`kill`, `Instant`, `thread::sleep`),
/// so no platform cfg is needed here. The `kill_handoff_child` wrapper selects unchanged
/// native killpg on Unix and this implementation elsewhere.
///
/// Pid safety requires the caller to retain the Child handle throughout; Windows cannot reuse
/// its pid while the handle remains alive. It does not depend on a particular lock or caller.
/// Both production paths satisfy this: `run_oneshot_llm_with_timeout_and_kill` holds the child
/// lock inline, and `cancel_handoff_generation_inner` clones the same `Arc<Mutex<Child>>`
/// and locks it separately. This function does not recheck that precondition.
#[cfg_attr(unix, allow(dead_code))]
pub(super) fn kill_handoff_child_with(
    child: &mut Child,
    tree_kill: impl FnOnce(u32),
) -> std::io::Result<()> {
    let pid = child.id();
    tree_kill(pid);
    // Code order is not execution order: tree termination starts asynchronously. Poll for the
    // root to exit within the deadline without killing it ahead of taskkill.
    let deadline = Instant::now() + WINDOWS_TASKKILL_REAP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            // Log the one-second fallback: if child.kill() reaps the root before taskkill arrives,
            // descendants may survive without their parent. Unix has no such polling branch,
            // so only non-Unix platforms emit this log.
            #[cfg(not(unix))]
            log_windows_taskkill_reap_timeout(pid);
            break;
        }
        std::thread::sleep(WINDOWS_TASKKILL_REAP_POLL_INTERVAL);
    }
    // The deadline expired with the root still alive: kill it as a fallback, matching Unix cleanup.
    match child.kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn kill_handoff_child(child: &mut Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let group_result = unsafe {
            if libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) == 0 {
                Ok(())
            } else {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        };
        let child_result = match child.kill() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        };
        group_result.and(child_result)
    }
    #[cfg(not(unix))]
    {
        kill_handoff_child_with(child, windows_taskkill_tree)
    }
}

struct HandoffPipeReader {
    bytes: Arc<Mutex<Vec<u8>>>,
    completed: std::sync::mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HandoffPipeReader {
    fn collect_before(mut self, deadline: Instant) -> Vec<u8> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if self.completed.recv_timeout(remaining).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
        // A reader that is still blocked on a descendant-owned pipe is detached;
        // the shared buffer still preserves every byte it managed to read.
        self.bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

fn read_handoff_pipe<R: Read + Send + 'static>(mut pipe: R) -> HandoffPipeReader {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let bytes_for_thread = bytes.clone();
    let (completed_sender, completed) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => bytes_for_thread
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = completed_sender.send(());
    });
    HandoffPipeReader {
        bytes,
        completed,
        thread: Some(thread),
    }
}

fn collect_handoff_output(
    stdout: HandoffPipeReader,
    stderr: HandoffPipeReader,
) -> (Vec<u8>, Vec<u8>) {
    let deadline = Instant::now() + HANDOFF_PIPE_DRAIN_TIMEOUT;
    (
        stdout.collect_before(deadline),
        stderr.collect_before(deadline),
    )
}

fn finish_handoff_oneshot(
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    parse_fn: ParseFn,
) -> Result<String, String> {
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            ui_msg::al_err("team.oneshotFailed", &[("detail", status.to_string())])
        } else {
            ui_msg::al_err("team.oneshotFailed", &[("detail", stderr)])
        });
    }
    let text = collect_assistant_text(&stdout, parse_fn);
    if text.trim().is_empty() {
        return Err(ui_msg::al_err("team.oneshotNoText", &[]));
    }
    Ok(text)
}

/// Handoff-only one-shot runner. Unlike `run_oneshot_llm`, this owns a killable,
/// session-scoped child and enforces a deadline.
pub(super) fn run_oneshot_llm_with_timeout(
    command: Command,
    parse_fn: ParseFn,
    stdin_prompt: Option<agent::StdinPrompt>,
    timeout: std::time::Duration,
    registry: &HandoffProcesses,
    session_id: &str,
    request_id: &str,
    cancel_requested: Arc<AtomicBool>,
) -> Result<String, String> {
    run_oneshot_llm_with_timeout_and_kill(
        command,
        parse_fn,
        stdin_prompt,
        timeout,
        registry,
        session_id,
        request_id,
        cancel_requested,
        kill_handoff_child,
    )
}

pub(super) fn run_oneshot_llm_with_timeout_and_kill<K>(
    mut command: Command,
    parse_fn: ParseFn,
    stdin_prompt: Option<agent::StdinPrompt>,
    timeout: std::time::Duration,
    registry: &HandoffProcesses,
    session_id: &str,
    request_id: &str,
    cancel_requested: Arc<AtomicBool>,
    kill_child: K,
) -> Result<String, String>
where
    K: Fn(&mut Child) -> std::io::Result<()>,
{
    if cancel_requested.load(Ordering::Acquire) {
        return Err("AL_ERR:continuation.handoffCancelled".to_string());
    }
    let _hook_guard = checkpoint_hook::guard_for_command(&command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = agent::spawn_with_stdin_prompt(&mut command, stdin_prompt.as_ref())
        .map_err(|e| ui_msg::al_err("team.oneshotSpawnFailed", &[("detail", e.to_string())]))?;
    let stdout = child.stdout.take().expect("piped handoff stdout");
    let stderr = child.stderr.take().expect("piped handoff stderr");
    let stdout_reader = read_handoff_pipe(stdout);
    let stderr_reader = read_handoff_pipe(stderr);
    let child = Arc::new(Mutex::new(child));
    let cancel_at_registration = {
        let mut processes = registry
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if processes.children.contains_key(session_id) {
            drop(processes);
            let mut child_guard = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match kill_child(&mut child_guard) {
                Ok(()) => {
                    if let Err(error) = child_guard.wait() {
                        eprintln!("handoff: failed to reap duplicate child: {error}");
                    }
                }
                Err(error) => {
                    eprintln!("handoff: failed to terminate duplicate child: {error}");
                }
            }
            drop(child_guard);
            let _ = collect_handoff_output(stdout_reader, stderr_reader);
            return Err(ui_msg::al_err(
                "team.oneshotFailed",
                &[("detail", "handoff process already registered".to_string())],
            ));
        }
        processes.children.insert(
            session_id.to_string(),
            RegisteredHandoffProcess {
                request_id: request_id.to_string(),
                child: child.clone(),
            },
        );
        cancel_requested.load(Ordering::Acquire)
    };

    let started = Instant::now();
    let mut timed_out = false;
    let outcome: Result<ExitStatus, String> = loop {
        let status = child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_wait();
        match status {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => {
                let error = ui_msg::al_err("team.oneshotFailed", &[("detail", error.to_string())]);
                let mut child_guard = child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match kill_child(&mut child_guard) {
                    Ok(()) => {
                        if let Err(wait_error) = child_guard.wait() {
                            eprintln!(
                                "handoff: failed to reap child after wait error: {wait_error}"
                            );
                        }
                    }
                    Err(kill_error) => {
                        eprintln!(
                            "handoff: failed to terminate child after wait error: {kill_error}"
                        );
                    }
                }
                break Err(error);
            }
        }
        if cancel_at_registration || cancel_requested.load(Ordering::Acquire) {
            let mut child_guard = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match kill_child(&mut child_guard) {
                Ok(()) => {
                    if let Err(wait_error) = child_guard.wait() {
                        eprintln!("handoff: failed to reap cancelled child: {wait_error}");
                    }
                }
                Err(kill_error) => {
                    eprintln!("handoff: failed to terminate cancelled child: {kill_error}");
                }
            }
            break Err("AL_ERR:continuation.handoffCancelled".to_string());
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let mut child_guard = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            break match kill_child(&mut child_guard) {
                Err(error) => Err(ui_msg::al_err(
                    "team.oneshotFailed",
                    &[(
                        "detail",
                        format!("failed to terminate timed-out handoff: {error}"),
                    )],
                )),
                Ok(()) => child_guard.wait().map_err(|error| {
                    ui_msg::al_err("team.oneshotFailed", &[("detail", error.to_string())])
                }),
            };
        }
        std::thread::sleep(HANDOFF_PROCESS_POLL_INTERVAL);
    };

    unregister_handoff_process(registry, session_id, &child);
    let (stdout, stderr) = collect_handoff_output(stdout_reader, stderr_reader);

    if cancel_requested.load(Ordering::Acquire) {
        return Err("AL_ERR:continuation.handoffCancelled".to_string());
    }
    let status = outcome?;
    if timed_out {
        return Err(ui_msg::al_err("continuation.handoffTimedOut", &[]));
    }
    finish_handoff_oneshot(status, stdout, stderr, parse_fn)
}

pub(super) fn cancel_handoff_generation_inner(
    registry: &HandoffProcesses,
    session_id: &str,
    request_id: &str,
) -> Result<bool, String> {
    // Hold the global registry lock only in this block, dropping it immediately after cloning
    // the target child's Arc. The non-Unix `kill_handoff_child` path can wait up to one second;
    // holding the global lock during that wait would block other sessions' registration/cancellation.
    // Pid safety relies on the `Arc<Mutex<Child>>` locked separately below, not the registry lock:
    // Windows cannot reuse a pid while its Child handle remains alive.
    let child_handle = {
        let processes = registry
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(request) = processes.requests.get(session_id) else {
            return Ok(false);
        };
        if request.request_id != request_id {
            return Ok(false);
        }
        request.cancel_requested.store(true, Ordering::Release);
        let Some(registered) = processes.children.get(session_id) else {
            return Ok(true);
        };
        if registered.request_id != request_id {
            return Ok(false);
        }
        registered.child.clone()
    };
    let mut child = child_handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if child.try_wait().map_err(|e| e.to_string())?.is_some() {
        return Ok(false);
    }
    kill_handoff_child(&mut child).map_err(|e| e.to_string())?;
    Ok(true)
}

// Use an async command: non-Unix `kill_handoff_child_with` can wait up to one second for
// root exit. Synchronous Tauri v2 commands run on the main thread, so Stop could freeze the
// Windows UI for that long. Async dispatches this to the thread pool without blocking the UI.
// This is transparent to `invoke("cancel_handoff_generation", ...)`: both forms return a
// Promise to JavaScript and require no caller changes.
#[tauri::command(async)]
pub(super) fn cancel_handoff_generation(
    processes: State<'_, HandoffProcesses>,
    session_id: String,
    request_id: String,
) -> Result<(), String> {
    cancel_handoff_generation_inner(processes.inner(), &session_id, &request_id).map(|_| ())
}

pub(super) fn remap_oneshot_error(err: String) -> String {
    for (source, target) in [
        ("team.oneshotSpawnFailed", "team.summarizeSpawnFailed"),
        ("team.oneshotFailed", "team.summarizeFailed"),
        ("team.oneshotNoText", "team.summarizeNoText"),
    ] {
        let prefix = format!("AL_ERR:{source}");
        if let Some(suffix) = err.strip_prefix(&prefix) {
            if suffix.is_empty() || suffix.starts_with(':') {
                return format!("AL_ERR:{target}{suffix}");
            }
        }
    }
    err
}

#[tauri::command]
pub(super) async fn lead_summarize(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    session_id: String,
    lead_agent_id: String,
    goal: String,
    workers: Vec<(String, String)>,
) -> Result<String, String> {
    // If no member produced text, skip synthesis and let the frontend use fallback_raw,
    // preventing the lead from inventing output without sources.
    if workers.iter().all(|(_, out)| out.trim().is_empty()) {
        return Err(ui_msg::al_err("team.noMemberOutput", &[]));
    }
    let prompt = build_synthesis_prompt(&goal, &workers);
    let locale = current_locale(&app);

    let profile = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_agent(&conn, &lead_agent_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| ui_msg::al_err("run.unknownLeadAgentGeneric", &[]))?
    };
    let key = if profile.access == "borrow" {
        KeyringStore.get(&profile.id)?
    } else {
        None
    };
    let search = resolve_harness_search_creds(&db, &profile, &KeyringStore)?;
    let hook_run_id = new_run_id();
    let (command, parse_fn, stdin_prompt) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let (_, wt) = ensure_session_workspace(&conn, &session_id)?;
        let backend = make_backend(&profile, key, search, locale)?;
        let parse_fn = backend.parse_fn();
        let ctx = BuildContext {
            prompt: &prompt,
            session_id: &session_id,
            run_id: &hook_run_id,
            wt: &wt,
            conn: &conn,
            mode: agent::BuildMode::Normal,
            locale,
            reasoning_tier: None,
            criteria: &[],
        };
        let command = backend.build_command(&ctx)?;
        let stdin_prompt = backend.stdin_prompt(&ctx);
        (command, parse_fn, stdin_prompt)
    };

    let text = tauri::async_runtime::spawn_blocking(move || {
        run_oneshot_llm(command, parse_fn, stdin_prompt)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(remap_oneshot_error)?;
    Ok(text)
}
