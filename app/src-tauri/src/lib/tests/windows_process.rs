#![cfg(test)]

use super::*;

#[test]
fn windows_kill_command_targets_process_tree_forcefully() {
    assert_eq!(
        windows_kill_command_args(1234),
        vec![
            "/PID".to_string(),
            "1234".to_string(),
            "/T".to_string(),
            "/F".to_string(),
        ]
    );
}

#[test]
fn windows_taskkill_program_uses_system_root_when_available() {
    assert_eq!(
        windows_taskkill_program(Some(r"C:\Windows")),
        r"C:\Windows\System32\taskkill.exe"
    );
}

#[test]
fn windows_taskkill_program_falls_back_when_system_root_is_missing() {
    assert_eq!(windows_taskkill_program(None), "taskkill");
}

#[test]
fn windows_taskkill_log_line_records_success() {
    assert_eq!(
        windows_taskkill_log_line(4242, 1_700_000_000, None),
        "[1700000000] taskkill pid=4242 spawn=ok\n"
    );
}

#[test]
fn windows_taskkill_log_line_records_failure_with_error_text() {
    assert_eq!(
        windows_taskkill_log_line(4242, 1_700_000_000, Some("program not found")),
        "[1700000000] taskkill pid=4242 spawn=failed error=program not found\n"
    );
}

#[test]
fn windows_taskkill_exit_log_line_records_exit_code_without_stderr() {
    assert_eq!(
        windows_taskkill_exit_log_line(4242, 1_700_000_000, "0", ""),
        "[1700000000] taskkill pid=4242 exit=0\n"
    );
}

#[test]
fn windows_taskkill_exit_log_line_appends_stderr_head_when_present() {
    assert_eq!(
        windows_taskkill_exit_log_line(4242, 1_700_000_000, "1", "ERROR: not found"),
        "[1700000000] taskkill pid=4242 exit=1 stderr=ERROR: not found\n"
    );
}

#[test]
fn windows_taskkill_exit_log_line_records_timeout() {
    assert_eq!(
        windows_taskkill_exit_log_line(4242, 1_700_000_000, "timeout", ""),
        "[1700000000] taskkill pid=4242 exit=timeout\n"
    );
}

// 下面这批用 TestHome（硬编码 /private/tmp 造临时 HOME）在 Windows 上必 panic，且最后一条
// 依赖「Mac 上没有 taskkill 可执行文件」这个前提在真 Windows 上语义相反——四条都只在 unix
// 跑（这台 CI 用 `cargo test --lib windows_` 按子串选测试，会选中它们）。
#[cfg(unix)]
#[test]
fn log_windows_taskkill_outcome_appends_ok_line_under_logs_dir() {
    let home = TestHome::new();

    log_windows_taskkill_outcome(4242, None);

    let logged =
        std::fs::read_to_string(home.path.join(".agentloom/logs/windows-taskkill.log")).unwrap();
    assert!(logged.contains("pid=4242 spawn=ok"), "{logged}");
}

#[cfg(unix)]
#[test]
fn log_windows_taskkill_outcome_appends_failure_line_with_error_text() {
    let home = TestHome::new();

    log_windows_taskkill_outcome(4242, Some("access denied".to_string()));

    let logged =
        std::fs::read_to_string(home.path.join(".agentloom/logs/windows-taskkill.log")).unwrap();
    assert!(
        logged.contains("pid=4242 spawn=failed error=access denied"),
        "{logged}"
    );
}

#[cfg(unix)]
#[test]
fn log_windows_taskkill_outcome_appends_multiple_calls_instead_of_overwriting() {
    let home = TestHome::new();

    log_windows_taskkill_outcome(1, None);
    log_windows_taskkill_outcome(2, Some("boom".to_string()));

    let logged =
        std::fs::read_to_string(home.path.join(".agentloom/logs/windows-taskkill.log")).unwrap();
    let lines: Vec<&str> = logged.lines().collect();
    assert_eq!(lines.len(), 2, "{logged}");
    assert!(lines[0].contains("pid=1 spawn=ok"), "{logged}");
    assert!(
        lines[1].contains("pid=2 spawn=failed error=boom"),
        "{logged}"
    );
}

/// Mac 测试机上没有 `taskkill` 这个可执行文件，spawn 必然走 Err 分支——这正好让我们不靠
/// 真 Windows 就能演练 `windows_taskkill_tree` 的失败路径（spawn 失败 + 落日志 + 不 panic）。
/// 成功路径（真 taskkill 树杀整棵进程 + detached 线程回填 exit 日志）测不到，留给 Windows
/// CI 编译门禁 + 真机验收。
#[cfg(unix)]
#[test]
fn windows_taskkill_tree_logs_failure_when_program_missing() {
    let home = TestHome::new();

    windows_taskkill_tree(999_999);

    let logged =
        std::fs::read_to_string(home.path.join(".agentloom/logs/windows-taskkill.log")).unwrap();
    assert!(logged.contains("pid=999999 spawn=failed"), "{logged}");
}
