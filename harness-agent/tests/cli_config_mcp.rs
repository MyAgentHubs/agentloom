use assert_cmd::Command;
use tempfile::tempdir;

/// Helper: run `config mcp add ...` and assert success.
fn mcp_add(
    home: &tempfile::TempDir,
    name: &str,
    command: &str,
    args: &[&str],
    env: &[&str],
    trusted: bool,
) {
    let mut cmd = Command::cargo_bin("myagent").unwrap();
    cmd.env("MYAGENT_HOME", home.path());
    cmd.arg("config").arg("mcp").arg("add").arg(name);
    cmd.arg("--command").arg(command);
    if !args.is_empty() {
        cmd.arg("--args").arg(args.join(","));
    }
    if !env.is_empty() {
        cmd.arg("--env").arg(env.join(","));
    }
    if trusted {
        cmd.arg("--trusted");
    }
    cmd.assert().success();
}

/// Helper: run `config mcp remove <name>` and assert success.
fn mcp_remove(home: &tempfile::TempDir, name: &str) {
    Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args(["config", "mcp", "remove", name])
        .assert()
        .success();
}

/// Helper: run `config mcp list` and return stdout text.
fn mcp_list(home: &tempfile::TempDir) -> String {
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args(["config", "mcp", "list"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Helper: read the config file directly and return the parsed AppConfig.
fn read_config(home: &tempfile::TempDir) -> serde_json::Value {
    let path = home.path().join("config.json");
    let bytes = std::fs::read(path).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ─── add ────────────────────────────────────────────────────────────────────

#[test]
fn add_minimal_saves_server() {
    let home = tempdir().unwrap();
    mcp_add(&home, "my-server", "node", &[], &[], false);

    let cfg = read_config(&home);
    let servers = cfg["mcp_servers"].as_object().unwrap();
    assert_eq!(servers.len(), 1);
    let s = &servers["my-server"];
    assert_eq!(s["name"], "my-server");
    assert_eq!(s["command"], "node");
    assert!(s["args"].as_array().unwrap().is_empty());
    assert!(s["env"].as_object().unwrap().is_empty());
    assert_eq!(s["trusted"], false);
}

#[test]
fn add_with_args_writes_args_array() {
    let home = tempdir().unwrap();
    mcp_add(
        &home,
        "srv",
        "python",
        &["server.py", "--port", "8080"],
        &[],
        false,
    );

    let cfg = read_config(&home);
    let args = cfg["mcp_servers"]["srv"]["args"].as_array().unwrap();
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "server.py");
    assert_eq!(args[1], "--port");
    assert_eq!(args[2], "8080");
}

#[test]
fn add_with_env_writes_env_map() {
    let home = tempdir().unwrap();
    mcp_add(
        &home,
        "srv",
        "node",
        &[],
        &["NODE_ENV=production", "PORT=3000"],
        false,
    );

    let cfg = read_config(&home);
    let env = cfg["mcp_servers"]["srv"]["env"].as_object().unwrap();
    assert_eq!(env.len(), 2);
    assert_eq!(env["NODE_ENV"], "production");
    assert_eq!(env["PORT"], "3000");
}

#[test]
fn add_with_trusted_sets_trusted_true() {
    let home = tempdir().unwrap();
    mcp_add(&home, "srv", "cmd", &[], &[], true);

    let cfg = read_config(&home);
    assert_eq!(cfg["mcp_servers"]["srv"]["trusted"], true);
}

#[test]
fn add_leading_hyphen_arg_preserved() {
    let home = tempdir().unwrap();
    // Use -- so that clap does not try to interpret -y as a flag.
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args([
            "config",
            "mcp",
            "add",
            "srv",
            "--command",
            "myapp",
            "--args",
            "-y",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = read_config(&home);
    let args = cfg["mcp_servers"]["srv"]["args"].as_array().unwrap();
    assert_eq!(args.len(), 1);
    assert_eq!(args[0], "-y");
}

#[test]
fn add_with_url_saves_http_server() {
    let home = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args([
            "config",
            "mcp",
            "add",
            "http-srv",
            "--url",
            "http://127.0.0.1:9000/mcp",
        ])
        .assert()
        .success();

    let cfg = read_config(&home);
    let s = &cfg["mcp_servers"]["http-srv"];
    assert_eq!(s["url"], "http://127.0.0.1:9000/mcp");
    // stdio `command` stays empty for a url-type server.
    assert_eq!(s["command"], "");
}

#[test]
fn add_url_and_command_together_is_rejected() {
    let home = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args([
            "config",
            "mcp",
            "add",
            "bad",
            "--command",
            "node",
            "--url",
            "http://x/mcp",
        ])
        .assert()
        .failure();
}

#[test]
fn add_without_command_or_url_is_rejected() {
    let home = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args(["config", "mcp", "add", "bare"])
        .assert()
        .failure();
}

#[test]
fn list_and_remove_url_server() {
    let home = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args(["config", "mcp", "add", "u", "--url", "http://h/mcp"])
        .assert()
        .success();

    let out = mcp_list(&home);
    assert!(out.contains("mcp u:"), "got: {out}");
    assert!(out.contains("url=http://h/mcp"), "got: {out}");
    assert!(
        !out.contains("command="),
        "url server must not print command=: {out}"
    );

    mcp_remove(&home, "u");
    let cfg = read_config(&home);
    assert!(cfg["mcp_servers"].as_object().unwrap().is_empty());
}

#[test]
fn add_overwrite_replaces_existing() {
    let home = tempdir().unwrap();
    mcp_add(&home, "srv", "first", &[], &[], false);
    mcp_add(&home, "srv", "second", &[], &[], true);

    let cfg = read_config(&home);
    let servers = cfg["mcp_servers"].as_object().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(cfg["mcp_servers"]["srv"]["command"], "second");
    assert_eq!(cfg["mcp_servers"]["srv"]["trusted"], true);
}

#[test]
fn add_multiple_servers_coexist() {
    let home = tempdir().unwrap();
    mcp_add(&home, "a", "cmda", &[], &[], false);
    mcp_add(&home, "b", "cmdb", &[], &[], false);

    let cfg = read_config(&home);
    let servers = cfg["mcp_servers"].as_object().unwrap();
    assert_eq!(servers.len(), 2);
    assert!(servers.contains_key("a"));
    assert!(servers.contains_key("b"));
}

// ─── remove ─────────────────────────────────────────────────────────────────

#[test]
fn remove_existing_server() {
    let home = tempdir().unwrap();
    mcp_add(&home, "srv", "cmd", &[], &[], false);
    mcp_remove(&home, "srv");

    let cfg = read_config(&home);
    let servers = cfg["mcp_servers"].as_object().unwrap();
    assert!(servers.is_empty());
}

#[test]
fn remove_nonexistent_exits_nonzero() {
    let home = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .env("MYAGENT_HOME", home.path())
        .args(["config", "mcp", "remove", "no-such"])
        .assert()
        .failure();
}

#[test]
fn remove_only_target_remains() {
    let home = tempdir().unwrap();
    mcp_add(&home, "a", "cmda", &[], &[], false);
    mcp_add(&home, "b", "cmdb", &[], &[], false);
    mcp_remove(&home, "a");

    let cfg = read_config(&home);
    let servers = cfg["mcp_servers"].as_object().unwrap();
    assert_eq!(servers.len(), 1);
    assert!(servers.contains_key("b"));
    assert!(!servers.contains_key("a"));
}

// ─── list ───────────────────────────────────────────────────────────────────

#[test]
fn list_empty_shows_message() {
    let home = tempdir().unwrap();
    let out = mcp_list(&home);
    assert!(out.contains("no mcp servers configured"));
}

#[test]
fn list_single_shows_details() {
    let home = tempdir().unwrap();
    mcp_add(&home, "srv", "myapp", &["--verbose"], &["KEY=val"], true);

    let out = mcp_list(&home);
    assert!(out.contains("mcp srv:"));
    assert!(out.contains("command=myapp"));
    assert!(out.contains("args=[--verbose]"));
    assert!(out.contains("env=[KEY=val]"));
    assert!(out.contains("--trusted"));
}

#[test]
fn list_multiple_sorted() {
    let home = tempdir().unwrap();
    mcp_add(&home, "zzz", "last", &[], &[], false);
    mcp_add(&home, "aaa", "first", &[], &[], false);

    let out = mcp_list(&home);
    let a_pos = out.find("mcp aaa:").unwrap();
    let z_pos = out.find("mcp zzz:").unwrap();
    assert!(a_pos < z_pos, "list should be sorted by name");
}

#[test]
fn list_server_without_args_env_trusted() {
    let home = tempdir().unwrap();
    mcp_add(&home, "simple", "echo", &[], &[], false);

    let out = mcp_list(&home);
    assert!(out.contains("mcp simple:"));
    assert!(out.contains("command=echo"));
    // No args, env, or trusted marker
    assert!(!out.contains("args=["));
    assert!(!out.contains("env=["));
    assert!(!out.contains("--trusted"));
}
