use super::{process_elapsed_ms, APP_DATA_DIR, BOOT_TRACE_HEADER_WRITTEN};
use std::io::Write;

/// Growth limit for boot-trace.log: if the file already exceeds this size before a write,
/// truncate and rewrite it first (this is a diagnostic log, not an audit log, so a simple approach is sufficient).
pub(super) const BOOT_TRACE_LOG_MAX_BYTES: u64 = 256 * 1024;

/// Format for a single boot_trace line (excluding the header line), extracted as a pure function
/// for unit testing; its behavior must match the existing stderr output.
pub(super) fn boot_trace_line_format(label: &str, ms: f64, proc_ms: f64) -> String {
    format!(
        "[boot] {:>28}   js={:>7.1}ms   proc={:>7.1}ms\n",
        label, ms, proc_ms
    )
}

/// Header line written before the first boot_trace entry for each process: app version + OS information + timestamp,
/// so a remotely submitted boot-trace.log immediately identifies the version, platform, and startup that produced it.
pub(super) fn boot_trace_header_format(app_version: &str, os_info: &str, unix_secs: u64) -> String {
    format!("==== AgentLoom boot v{app_version} · {os_info} · t={unix_secs} ====\n")
}

/// Append one boot trace line to `<app_data_dir>/logs/boot-trace.log`.
///
/// The diagnostic facility itself must never crash the startup path: failure to create the directory,
/// open the file, or write to it is always silently ignored, without panicking or propagating errors.
/// This is an auxiliary log for diagnosing issues such as a blank screen for remote users, not a critical path.
pub(super) fn write_boot_trace_line(app_data_dir: &std::path::Path, line: &str) {
    let dir = app_data_dir.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("boot-trace.log");
    // Prevent unbounded growth: if the file already exceeds the threshold before a write, truncate and rewrite it first.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > BOOT_TRACE_LOG_MAX_BYTES {
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

#[tauri::command]
pub(super) fn boot_trace(label: String, ms: f64) {
    let proc_ms = process_elapsed_ms();
    if std::env::var("AGENTLOOM_BOOT_TRACE").is_ok() {
        eprintln!(
            "[boot] {:>28}   js={:>7.1}ms   proc={:>7.1}ms",
            label, ms, proc_ms
        );
    }
    // Always write to disk (regardless of the AGENTLOOM_BOOT_TRACE gate): a .app launched by double-clicking
    // has no visible stderr. When a remote user reports a blank screen, this file is the only diagnostic data they can send back.
    if let Some(dir) = APP_DATA_DIR.get() {
        if BOOT_TRACE_HEADER_WRITTEN.set(()).is_ok() {
            let unix_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let os_info = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);
            let header = boot_trace_header_format(env!("CARGO_PKG_VERSION"), &os_info, unix_secs);
            write_boot_trace_line(dir, &header);
        }
        let line = boot_trace_line_format(&label, ms, proc_ms);
        write_boot_trace_line(dir, &line);
    }
}
