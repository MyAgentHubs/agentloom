use assert_cmd::Command;
use tempfile::tempdir;

fn run_mcp_list(home: &tempfile::TempDir, json: bool) -> String {
    let mut cmd = Command::cargo_bin("myagent").unwrap();
    cmd.env("MYAGENT_HOME", home.path()).args(["mcp", "list"]);
    if json {
        cmd.arg("--json");
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "mcp list should exit 0");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn add_server(home: &tempfile::TempDir, name: &str, command: &str) {
    Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args(["config", "mcp", "add", name, "--command", command])
        .assert()
        .success();
}

#[test]
fn mcp_resource_cli_list_no_servers_shows_message() {
    let home = tempdir().unwrap();
    let out = run_mcp_list(&home, false);
    assert!(out.contains("no mcp servers configured"), "got: {out}");
}

#[test]
fn mcp_resource_cli_list_no_servers_json_is_empty_array() {
    let home = tempdir().unwrap();
    let out = run_mcp_list(&home, true);
    assert_eq!(out.trim(), "[]");
}

#[test]
fn mcp_resource_cli_list_help_shows_usage() {
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args(["mcp", "list", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        stdout.contains("list") || stdout.contains("tools"),
        "got: {stdout}"
    );
}

#[test]
fn mcp_resource_cli_list_unreachable_server_marks_error_and_continues() {
    let home = tempdir().unwrap();
    add_server(&home, "badsrv", "/nonexistent/mcp-xyz");
    let out = run_mcp_list(&home, false);
    assert!(out.contains("badsrv"), "got: {out}");
    assert!(out.contains("error"), "got: {out}");
}
