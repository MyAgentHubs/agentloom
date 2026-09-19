#![cfg(test)]

use super::*;

#[test]
#[ignore = "requires authenticated codex CLI; PreToolUse end-to-end evidence"]
fn real_codex_pretooluse_e2e() {
    let temp = tempfile::TempDir::new().unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let target = temp.path().join("target.txt");
    fs::write(&target, "ORIGINAL\n").unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let server = start_server(Some(observed.clone())).unwrap();
    let token = random_token().unwrap();
    let session_id = format!("pre-codex-{}", std::process::id());
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path,
            session_id: session_id.clone(),
            run_id: "r1".into(),
            allowed_root: temp.path().to_path_buf(),
            ..Registration::default()
        },
    );
    let hook = HookConfig {
        endpoint: hook_endpoint(server.port),
        settings_path: temp.path().join("unused.json"),
        codex_config: codex_config(server.port),
        token: token.clone(),
    };
    let mut command = Command::new("codex");
    command.args(["-a", "never"]);
    configure_codex_command(&mut command, &hook);
    let output = command
            .args([
                "exec",
                "--json",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "Use apply_patch exactly once to change target.txt from ORIGINAL to UPDATED. Do not use shell or another tool.",
            ])
            .current_dir(temp.path())
            .output()
            .unwrap();
    println!(
        "CODEX_STATUS={}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
    let entries = store.list_entries(&session_id, "r1").unwrap();
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        archived_preimage(&entries[0], &session_id, "r1"),
        "ORIGINAL\n"
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "UPDATED\n");
    let _ = store.purge_run(&session_id, "r1");
}

#[test]
#[ignore = "requires authenticated claude CLI; PreToolUse end-to-end evidence"]
fn real_claude_pretooluse_e2e() {
    let temp = tempfile::TempDir::new().unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let target = temp.path().join("target.txt");
    fs::write(&target, "ORIGINAL\n").unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let server = start_server(Some(observed.clone())).unwrap();
    let token = random_token().unwrap();
    let session_id = format!("pre-claude-{}", std::process::id());
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path,
            session_id: session_id.clone(),
            run_id: "r1".into(),
            allowed_root: temp.path().to_path_buf(),
            ..Registration::default()
        },
    );
    let settings = write_settings(server.port).unwrap();
    let prompt = format!(
        "Use Edit exactly once to replace ORIGINAL with UPDATED in {}. Do not use Bash or Write.",
        target.display()
    );
    let output = Command::new("claude")
        .current_dir(temp.path())
        .env(TOKEN_ENV, &token)
        .args([
            "-p",
            &prompt,
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
            "--tools",
            "Edit,Read",
            "--settings",
            settings.to_str().unwrap(),
            "--setting-sources",
            "user,project,local",
        ])
        .output()
        .unwrap();
    println!(
        "CLAUDE_STATUS={}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
    let entries = store.list_entries(&session_id, "r1").unwrap();
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        archived_preimage(&entries[0], &session_id, "r1"),
        "ORIGINAL\n"
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "UPDATED\n");
    let _ = store.purge_run(&session_id, "r1");
    let _ = fs::remove_file(settings);
}

#[test]
#[cfg(unix)]
#[ignore = "requires authenticated claude CLI; Stop-block end-to-end evidence"]
fn real_claude_stop_blocks_until_background_task_done_e2e() {
    // Deliberately does NOT swap HOME (unlike the myagent PreToolUse test above): the claude
    // CLI reads its login credentials from the real HOME, and an isolated HOME here just
    // produces an unauthenticated "Not logged in" exit — confirmed by an actual failed run.
    // write_settings() still lands under the real ~/.agentloom/hooks, cleaned up below.
    let temp = tempfile::TempDir::new().unwrap();
    let db_path = temp.path().join("agentloom.db");
    let allowed_root = fs::canonicalize(temp.path()).unwrap();

    let observed = Arc::new(Mutex::new(Vec::new()));
    let server = start_server(Some(observed.clone())).unwrap();
    let token = random_token().unwrap();
    let session_id = format!("stop-e2e-{}", std::process::id());
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path,
            session_id: session_id.clone(),
            run_id: "r1".into(),
            allowed_root,
            // Starts unregistered; filled in with the real claude pid right after spawn below,
            // exactly like `register_agent_pid` does for a production run.
            agent_pid: None,
            ..Registration::default()
        },
    );
    let settings = write_settings(server.port).unwrap();
    let prompt = "Use the Bash tool with run_in_background set to true to start the command: sleep 45\nThen immediately reply 'started' without waiting for it.";

    let started_at = std::time::Instant::now();
    let mut command = Command::new("claude");
    command
        .current_dir(temp.path())
        .env(TOKEN_ENV, &token)
        .args([
            "-p",
            prompt,
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
            "--settings",
            settings.to_str().unwrap(),
            "--setting-sources",
            "user,project,local",
            "--model",
            "haiku",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().unwrap();
    let claude_pid = child.id();
    // claude's cold start takes well under a second; the Stop hook can't fire before this
    // registration lands, so there's no race with the first Stop event.
    server
        .registrations
        .lock()
        .unwrap()
        .get_mut(&token)
        .unwrap()
        .agent_pid = Some(claude_pid);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let output = match rx.recv_timeout(std::time::Duration::from_secs(180)) {
        Ok(result) => result.unwrap(),
        Err(_) => {
            unsafe {
                libc::killpg(claude_pid as libc::pid_t, libc::SIGKILL);
            }
            panic!("claude did not exit within 180s (still presumably stuck, blocked or hung)");
        }
    };
    let elapsed = started_at.elapsed();

    println!(
        "CLAUDE_STOP_E2E_STATUS={}\nELAPSED={elapsed:?}\nSTDOUT:\n{}\nSTDERR:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stop_events: Vec<serde_json::Value> = observed
        .lock()
        .unwrap()
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .filter(|value| value.get("hook_event_name").and_then(|v| v.as_str()) == Some("Stop"))
        .collect();
    println!("STOP_EVENTS={stop_events:#?}");

    // Best-effort cleanup before asserting, so a failed assertion doesn't leak the process
    // group (claude should already have exited by now; this is a no-op in the normal case).
    unsafe {
        libc::killpg(claude_pid as libc::pid_t, libc::SIGKILL);
    }

    assert!(
            stop_events.len() >= 2,
            "expected >= 2 Stop hook invocations (first one blocked, at least one retry after it), got {}",
            stop_events.len()
        );
    assert!(
        stop_events
            .iter()
            .any(|event| { event.get("stop_hook_active").and_then(|v| v.as_bool()) == Some(true) }),
        "expected at least one Stop event with stop_hook_active == true (claude retrying \
             after our earlier block), got: {stop_events:#?}"
    );
    assert!(
        output.status.success(),
        "claude exited non-zero: {}",
        output.status
    );
    // No hard floor on `elapsed`: the block reason text explicitly permits "kill them if no
    // longer needed", so a model retrying after a block may legitimately kill the background
    // sleep and exit well under 45s instead of waiting it out. `elapsed` is still printed
    // above as evidence, but asserting a minimum wall-clock time bakes in "waiting it out" as
    // the only compliant response, which it isn't — confirmed by a real run that killed the
    // job and exited cleanly at ~29s. The causal proof that the hook actually blocked is the
    // stop_hook_active: true retry asserted above, not elapsed time.

    let _ = fs::remove_file(settings);
}
