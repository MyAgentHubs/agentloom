#![cfg(test)]

use super::*;

#[test]
fn host_os_reports_rust_target_os() {
    assert_eq!(host_os(), std::env::consts::OS);
}

#[test]
fn boot_trace_line_format_matches_stderr_shape() {
    let line = boot_trace_line_format("react mount", 12.5, 34.75);
    assert_eq!(
        line,
        "[boot]                  react mount   js=   12.5ms   proc=   34.8ms\n"
    );
}

#[test]
fn boot_trace_header_format_includes_version_os_and_timestamp() {
    let header = boot_trace_header_format("0.1.0", "macos/aarch64", 1_700_000_000);
    assert_eq!(
        header,
        "==== AgentLoom boot v0.1.0 · macos/aarch64 · t=1700000000 ====\n"
    );
}

#[test]
fn write_boot_trace_line_appends_to_new_file_under_logs_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    write_boot_trace_line(tmp.path(), "line one\n");
    write_boot_trace_line(tmp.path(), "line two\n");
    let contents = std::fs::read_to_string(tmp.path().join("logs").join("boot-trace.log")).unwrap();
    assert_eq!(contents, "line one\nline two\n");
}

#[test]
fn write_boot_trace_line_creates_missing_logs_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let app_data_dir = tmp.path().join("does-not-exist-yet");
    write_boot_trace_line(&app_data_dir, "hello\n");
    assert!(app_data_dir.join("logs").join("boot-trace.log").exists());
}

#[test]
fn write_boot_trace_line_truncates_when_over_size_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    let logs_dir = tmp.path().join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();
    let path = logs_dir.join("boot-trace.log");
    let oversized = "x".repeat((BOOT_TRACE_LOG_MAX_BYTES + 1) as usize);
    std::fs::write(&path, &oversized).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > BOOT_TRACE_LOG_MAX_BYTES);

    write_boot_trace_line(tmp.path(), "fresh line\n");

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents, "fresh line\n");
}
