// This file contains Windows taskkill helpers moved from lib.rs.

use crate::{worktree, BOOT_TRACE_LOG_MAX_BYTES};
use std::io::{Read, Write};
use std::process::{Child, Stdio};
use std::time::Instant;

#[cfg_attr(unix, allow(dead_code))]
pub(super) fn windows_taskkill_program(system_root: Option<&str>) -> String {
    match system_root {
        Some(system_root) => format!(
            r"{}\System32\taskkill.exe",
            system_root.trim_end_matches(['\\', '/'])
        ),
        None => "taskkill".to_string(),
    }
}

#[cfg_attr(unix, allow(dead_code))]
pub(super) fn windows_kill_command_args(pid: u32) -> Vec<String> {
    vec![
        "/PID".to_string(),
        pid.to_string(),
        "/T".to_string(),
        "/F".to_string(),
    ]
}

#[cfg_attr(unix, allow(dead_code))]
fn unix_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Formats one log line for a taskkill **spawn** result. This is a pure function for unit testing
/// with no cfg dependency. It only says whether the taskkill command itself started; see
/// `windows_taskkill_exit_log_line` to determine whether it actually killed the target tree.
#[cfg_attr(unix, allow(dead_code))]
pub(super) fn windows_taskkill_log_line(pid: u32, unix_secs: u64, error: Option<&str>) -> String {
    match error {
        Some(err) => format!("[{unix_secs}] taskkill pid={pid} spawn=failed error={err}\n"),
        // Constructed only in tests and unreachable in production: the sole production call site of
        // `log_windows_taskkill_outcome`, the spawn `Err` branch of `windows_taskkill_tree`, always
        // passes `Some(..)`. A successful spawn takes the `Ok(child)` branch and starts the watcher
        // thread, so it never writes this "spawn=ok" line. Keep this branch because removing it would
        // also require rewriting assertions in `log_windows_taskkill_outcome_appends_ok_line_under_logs_dir`
        // and `_appends_multiple_calls_instead_of_overwriting`, which deliberately pass `None` to
        // distinguish the line from the failure line produced by `Some`.
        None => format!("[{unix_secs}] taskkill pid={pid} spawn=ok\n"),
    }
}

/// Formats one log line for the **actual exit outcome** of taskkill as a pure function for unit
/// testing. `exit` is a decimal exit code, "unknown" when no exit code is available,
/// "wait-error:<err>" when polling itself fails, or "timeout" when bounded polling times out and
/// the taskkill command itself is killed as a fallback.
#[cfg_attr(unix, allow(dead_code))]
pub(super) fn windows_taskkill_exit_log_line(
    pid: u32,
    unix_secs: u64,
    exit: &str,
    stderr_head: &str,
) -> String {
    if stderr_head.is_empty() {
        format!("[{unix_secs}] taskkill pid={pid} exit={exit}\n")
    } else {
        format!("[{unix_secs}] taskkill pid={pid} exit={exit} stderr={stderr_head}\n")
    }
}

/// Growth limit for windows-taskkill.log. It matches `BOOT_TRACE_LOG_MAX_BYTES`; this diagnostic log
/// is not an audit log either, so a simple approach is sufficient.
#[cfg_attr(unix, allow(dead_code))]
const WINDOWS_TASKKILL_LOG_MAX_BYTES: u64 = BOOT_TRACE_LOG_MAX_BYTES;
/// Truncation length for the first stderr line from the taskkill child. Diagnostic context is enough;
/// do not pour the entire error stack into the file.
#[cfg_attr(unix, allow(dead_code))]
const WINDOWS_TASKKILL_STDERR_HEAD_BYTES: usize = 200;
/// Poll interval and limit for the detached thread started by `windows_taskkill_tree` while it waits
/// for the taskkill child itself to exit. This wait asks whether the taskkill command has finished;
/// it is independent from the `WINDOWS_TASKKILL_REAP_*` wait in `kill_handoff_child_with`, which asks
/// whether root was killed, and neither bounded wait blocks the other.
#[cfg_attr(unix, allow(dead_code))]
const WINDOWS_TASKKILL_WATCH_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(10);
#[cfg_attr(unix, allow(dead_code))]
const WINDOWS_TASKKILL_WATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Appends one line to `~/.agentloom/logs/windows-taskkill.log`. As with `log_file_for` and
/// `write_boot_trace_line`, failures to create the directory or open the file are silently ignored:
/// diagnostics must never break the main process-killing flow. The growth guard also follows
/// `write_boot_trace_line`: if the file is already over the limit before writing, truncate it first.
#[cfg_attr(unix, allow(dead_code))]
fn append_windows_taskkill_log_line(line: &str) {
    let dir = worktree::logs_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("windows-taskkill.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > WINDOWS_TASKKILL_LOG_MAX_BYTES {
            let _ = std::fs::write(&path, "");
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Windows release builds use the GUI subsystem, where stderr is completely invisible. This records
/// whether the taskkill **command itself** can spawn and is the first clue for reports that Stop did
/// nothing.
#[cfg_attr(unix, allow(dead_code))]
pub(super) fn log_windows_taskkill_outcome(pid: u32, error: Option<String>) {
    let line = windows_taskkill_log_line(pid, unix_secs_now(), error.as_deref());
    append_windows_taskkill_log_line(&line);
}

/// The actual outcome after the taskkill child itself finishes. This differs from spawn success:
/// spawning only proves that the command started, not that it killed the target tree.
#[cfg_attr(unix, allow(dead_code))]
fn log_windows_taskkill_exit(pid: u32, exit: &str, stderr_head: &str) {
    let line = windows_taskkill_exit_log_line(pid, unix_secs_now(), exit, stderr_head);
    append_windows_taskkill_log_line(&line);
}

/// Formats one log line for `kill_handoff_child_with` timing out while boundedly waiting for root to
/// exit and racing to the fallback `child.kill()`. This pure function exists for unit testing. The
/// line preserves evidence for cases where a grandchild survives again: compare its timestamp with
/// the actual taskkill outcome filled in by the watcher via `windows_taskkill_exit_log_line` to
/// determine whether this one-second timeout fallback ran before taskkill actually removed the tree.
#[cfg_attr(unix, allow(dead_code))]
fn windows_taskkill_reap_timeout_log_line(pid: u32, unix_secs: u64) -> String {
    format!("[{unix_secs}] taskkill pid={pid} reap=timeout-fallback-kill\n")
}

/// Writes `windows_taskkill_reap_timeout_log_line` to disk.
#[cfg_attr(unix, allow(dead_code))]
pub(super) fn log_windows_taskkill_reap_timeout(pid: u32) {
    let line = windows_taskkill_reap_timeout_log_line(pid, unix_secs_now());
    append_windows_taskkill_log_line(&line);
}

/// Reads the beginning of the taskkill child's stderr, truncating it to
/// `WINDOWS_TASKKILL_STDERR_HEAD_BYTES` bytes and taking only the first line. Call this only after
/// the child exits and closes the write end; the pipe then contains finitely many bytes and reading
/// cannot block.
#[cfg_attr(unix, allow(dead_code))]
fn windows_taskkill_stderr_head(taskkill_child: &mut Child) -> String {
    let Some(mut stderr) = taskkill_child.stderr.take() else {
        return String::new();
    };
    let mut buf = [0_u8; WINDOWS_TASKKILL_STDERR_HEAD_BYTES];
    let read = stderr.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..read])
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Body of the detached thread started by `windows_taskkill_tree`: boundedly poll for the taskkill
/// child itself to exit, then log `exit=<code>` plus the first stderr line. If it has not exited by
/// `WINDOWS_TASKKILL_WATCH_TIMEOUT`, kill taskkill as a fallback and log `exit=timeout`. This never
/// affects the caller, which waits for the target root child to die in `kill_handoff_child_with`, not
/// for this taskkill command itself to finish.
#[cfg_attr(unix, allow(dead_code))]
fn watch_windows_taskkill_exit(pid: u32, mut taskkill_child: Child) {
    let deadline = Instant::now() + WINDOWS_TASKKILL_WATCH_TIMEOUT;
    loop {
        match taskkill_child.try_wait() {
            Ok(Some(status)) => {
                let exit = status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let stderr_head = windows_taskkill_stderr_head(&mut taskkill_child);
                log_windows_taskkill_exit(pid, &exit, &stderr_head);
                return;
            }
            Ok(None) => {}
            Err(error) => {
                log_windows_taskkill_exit(pid, &format!("wait-error:{error}"), "");
                return;
            }
        }
        if Instant::now() >= deadline {
            let _ = taskkill_child.kill();
            log_windows_taskkill_exit(pid, "timeout", "");
            return;
        }
        std::thread::sleep(WINDOWS_TASKKILL_WATCH_POLL_INTERVAL);
    }
}

/// Shared Windows tree-kill path for `kill_process_group` and `kill_handoff_child_with`. The caller
/// must pin pid with its Child handle before it is reaped and reused; on Windows, the system does not
/// reuse a pid while its handle remains alive. Each caller guarantees that precondition, so this
/// function does not validate it again. It is fire-and-forget and nonblocking: the detached thread
/// above boundedly polls whether taskkill finishes and records its actual exit code, while this
/// function returns immediately.
#[cfg_attr(unix, allow(dead_code))]
pub(super) fn windows_taskkill_tree(pid: u32) {
    let system_root = std::env::var("SystemRoot").ok();
    let program = windows_taskkill_program(system_root.as_deref());
    let args = windows_kill_command_args(pid);
    match crate::proc::command(program)
        .args(args)
        // No one reads stdout. Normal taskkill output goes there, while only failure diagnostics on
        // stderr and the actual exit outcome from the watcher's try_wait matter. Piping without
        // reading would only occupy the pipe buffer, so harden this to null.
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            std::thread::spawn(move || watch_windows_taskkill_exit(pid, child));
        }
        Err(error) => {
            eprintln!("taskkill failed for pid {pid}: {error}");
            log_windows_taskkill_outcome(pid, Some(error.to_string()));
        }
    }
}

pub(crate) fn kill_process_group(pid: u32) {
    // A negative pid denotes a process group: killpg removes the agent and the entire tree of
    // bash/git/... processes it spawned.
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        // On Windows, taskkill /T /F forcibly terminates the whole process tree; leave Job Objects
        // for v2.
        windows_taskkill_tree(pid);
    }
}
