#![cfg(test)]

use super::*;

#[test]
fn forged_post_request_cannot_bless_or_wrongly_restore_user_work() {
    // CheckpointStore::new() 的 blob 根目录经 crate::worktree::logs_dir() 派生自进程级 HOME
    // env（取值发生在调用瞬间，非测试开始时锁定）。本测试先经 PreToolUse 钩子（后台线程里
    // 建一个 CheckpointStore 写 preimage blob），再在测试主线程另建一个 CheckpointStore 读
    // 同一个 blob——如果这两次 HOME 读到的值不一样（被其它测试并发改了），两次算出的根目录
    // 就不同，读的时候会 ENOENT。用与本文件同级测试
    // myagent_hook_server_keeps_first_preimage_and_undo_restores_original_bytes 相同的
    // HomeGuard（内部持 test_home_lock）把 HOME 锁定在测试专属目录，堵住这条窗口。
    let (_home_root, home) = crate::test_support::tmp_root();
    let _home = HomeGuard::set(&home);
    let temp = tempfile::TempDir::new().unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let target = temp.path().join("main.py");
    fs::write(&target, "U0").unwrap();
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    let session_id = format!("forged-post-{}", std::process::id());
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
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let client = reqwest::blocking::Client::new();
    let body = |event: &str, content: &str| {
        serde_json::json!({
            "hook_event_name": event,
            "tool_name": "Write",
            "tool_input": { "file_path": target, "content": content }
        })
        .to_string()
    };
    assert!(client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body("PreToolUse", "A1"))
        .send()
        .unwrap()
        .status()
        .is_success());
    fs::write(&target, "U1").unwrap();

    let forged = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body("PostToolUse", "U1"))
        .send()
        .unwrap();
    assert!(forged.status().is_server_error());
    let forged_repeat_pre = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body("PreToolUse", "attacker-controlled-but-ignored"))
        .send()
        .unwrap();
    assert!(forged_repeat_pre.status().is_success());

    let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
    let entry = store
        .list_undo_entries(&session_id, "r1")
        .unwrap()
        .remove(0);
    assert_eq!(
        entry.preimage_preview,
        crate::checkpoint::UndoPreview::Text {
            content: "U0".into()
        }
    );
    assert_eq!(
        entry.current_preview,
        crate::checkpoint::UndoPreview::Text {
            content: "U1".into()
        }
    );
    fs::write(&target, "U2-after-list").unwrap();
    let report = store
        .undo_run(
            &session_id,
            "r1",
            std::slice::from_ref(&entry.file_path),
            std::slice::from_ref(&entry.current_digest),
        )
        .unwrap();
    assert!(report.restored.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(fs::read_to_string(&target).unwrap(), "U2-after-list");
    let _ = store.purge_run(&session_id, "r1");
}

#[test]
fn settings_contain_pretooluse_and_stop_codex_config_stays_pretooluse_only() {
    let settings = settings_json(4321);
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["matcher"],
        CLAUDE_EDIT_TOOLS.join("|")
    );
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"],
        HOOK_TIMEOUT_SECS
    );
    let pretooluse_command = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(pretooluse_command.contains("|| exit 2"));
    assert!(!pretooluse_command.contains("|| true"));

    // Stop hook: no matcher (fires unconditionally), same timeout, but must fail open
    // (`|| true`) instead of blocking the tool call (`|| exit 2`) when curl itself fails.
    assert!(settings["hooks"]["Stop"][0].get("matcher").is_none());
    assert_eq!(
        settings["hooks"]["Stop"][0]["hooks"][0]["timeout"],
        HOOK_TIMEOUT_SECS
    );
    let stop_command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(stop_command.contains("|| true"));
    assert!(!stop_command.contains("|| exit 2"));
    assert!(stop_command.contains("X-AgentLoom-Token"));

    let config = codex_config(4321);
    assert_eq!(config.len(), 1);
    assert!(config[0].starts_with("hooks.PreToolUse=["));
    assert!(!config[0].contains("PostToolUse"));
    assert!(!config[0].contains("\"Stop\""));
    assert!(config[0].contains("matcher = \"^apply_patch$\""));
    assert!(config[0].contains(&format!("timeout = {HOOK_TIMEOUT_SECS}")));
    assert!(config[0].contains(&format!(
        "--connect-timeout 5 --max-time {CURL_MAX_TIME_SECS}"
    )));
    assert!(config[0].contains("|| exit 2"));
}

#[test]
fn myagent_hook_server_keeps_first_preimage_and_undo_restores_original_bytes() {
    let (_home_root, home) = crate::test_support::tmp_root();
    let _home = HomeGuard::set(&home);
    let temp = tempfile::TempDir::new().unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let allowed_root = fs::canonicalize(temp.path()).unwrap();
    let target = allowed_root.join("main.rs");
    fs::write(&target, "ORIGINAL\n").unwrap();
    let target = fs::canonicalize(&target).unwrap();
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    let session_id = format!("myagent-undo-{}", std::process::id());
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path,
            session_id: session_id.clone(),
            run_id: "r1".into(),
            allowed_root: allowed_root.clone(),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "fs_edit",
        "tool_input": { "path": target.to_string_lossy() },
    })
    .to_string();
    let client = reqwest::blocking::Client::new();

    let first = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body.clone())
        .send()
        .unwrap();
    assert!(first.status().is_success());
    fs::write(&target, "FIRST MUTATION\n").unwrap();

    let second = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body)
        .send()
        .unwrap();
    assert!(second.status().is_success());
    fs::write(&target, "SECOND MUTATION\n").unwrap();

    let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
    let entries = store.list_entries(&session_id, "r1").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].file_path, target);
    assert_eq!(
        archived_preimage(&entries[0], &session_id, "r1"),
        "ORIGINAL\n"
    );
    assert_eq!(
        crate::db::list_checkpoint_file_paths_for_session(&conn, &session_id).unwrap(),
        vec![target.clone()]
    );

    let undo_entries = store.list_undo_entries(&session_id, "r1").unwrap();
    assert_eq!(undo_entries.len(), 1);
    assert_eq!(undo_entries[0].file_path, target);
    assert_eq!(
        undo_entries[0].change_kind,
        crate::checkpoint::ChangeKind::Modified
    );
    assert_eq!(
        undo_entries[0].preimage_preview,
        crate::checkpoint::UndoPreview::Text {
            content: "ORIGINAL\n".into()
        }
    );
    assert_eq!(
        undo_entries[0].current_preview,
        crate::checkpoint::UndoPreview::Text {
            content: "SECOND MUTATION\n".into()
        }
    );
    assert!(!undo_entries[0].already_undone);

    let report = store
        .undo_run(
            &session_id,
            "r1",
            std::slice::from_ref(&undo_entries[0].file_path),
            std::slice::from_ref(&undo_entries[0].current_digest),
        )
        .unwrap();
    assert_eq!(report.restored, vec![target.clone()]);
    assert!(report.failed.is_empty());
    assert!(report.skipped.is_empty());
    assert_eq!(fs::read_to_string(&target).unwrap(), "ORIGINAL\n");
    assert!(
        crate::db::list_checkpoint_file_paths_for_session(&conn, &session_id)
            .unwrap()
            .is_empty()
    );
}

/// D1 (2026-07-29 delta review) — regression pin for the revocation barrier itself, exercised
/// through the real production path (`install()` + `guard_for_command()` +
/// `HookRunGuard::drop`, not an ephemeral test-only `HookServer`) against a genuinely slow
/// (test-injected) DB write. Proves three things about dropping the run guard while that write
/// is still in flight:
///   1. the drop returns at all — watched with a timeout so a regression here fails this test
///      instead of hanging the whole suite;
///   2. it actually waited for the write, rather than returning immediately;
///   3. after the drop returns, a new write against the same (now-revoked) token is rejected.
///
/// Kills the "delete `InFlightWriteGuard`'s decrement" mutation: under that mutation,
/// `in_flight_writes` never returns to zero, so `HookRunGuard::drop`'s poll loop spins
/// forever and assertion 1 times out — with no other test catching it, since nothing else
/// exercises revocation racing a genuinely in-flight write.
#[test]
fn hook_run_guard_drop_waits_for_in_flight_write_then_revokes() {
    let (_home_root, home) = crate::test_support::tmp_root();
    let _home = HomeGuard::set(&home);
    let temp = tempfile::TempDir::new().unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let allowed_root = fs::canonicalize(temp.path()).unwrap();
    let target = allowed_root.join("main.rs");
    fs::write(&target, "ORIGINAL\n").unwrap();
    let target = fs::canonicalize(&target).unwrap();

    // Real `install()` against a file-backed DB (not the `#[cfg(test)]` in-memory shortcut at
    // the top of `install` — that bypasses the process-wide `SERVER`/registrations entirely),
    // and a real `HookRunGuard` built the same way production spawn code builds one.
    let hook = install(&conn, "d1-hookrunguard-session", "r1", &allowed_root).unwrap();
    let mut command = Command::new("true"); // never spawned — guard_for_command only reads its env.
    command.env(TOKEN_ENV, &hook.token);
    let guard = guard_for_command(&command).expect("guard_for_command must find the TOKEN_ENV var");

    let body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "fs_edit",
        "tool_input": { "path": target.to_string_lossy() },
    })
    .to_string();

    let write_endpoint = hook.endpoint.clone();
    let write_token = hook.token.clone();
    let write_body = body.clone();
    let write_thread = std::thread::spawn(move || {
        reqwest::blocking::Client::new()
            .post(&write_endpoint)
            .header("X-AgentLoom-Token", &write_token)
            .header(TEST_SLEEP_HEADER, "1500")
            .body(write_body)
            .send()
            .unwrap()
            .status()
    });

    // Give the write time to be accepted and pass its `still_active` check (and therefore
    // construct its `InFlightWriteGuard`) before dropping the run guard, so the drop
    // genuinely races an in-flight write instead of running before it even starts.
    std::thread::sleep(Duration::from_millis(300));

    let drop_started = std::time::Instant::now();
    let (drop_tx, drop_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(guard);
        let _ = drop_tx.send(drop_started.elapsed());
    });
    // Assertion 1: bounded return.
    let drop_elapsed = drop_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("HookRunGuard::drop did not return within 10s — revocation is hanging");

    let write_status = write_thread.join().unwrap();
    assert_eq!(write_status, reqwest::StatusCode::NO_CONTENT);

    // Assertion 2: revocation actually waited for the in-flight write rather than returning
    // immediately. The write's simulated slow DB write started shortly before this drop was
    // attempted (~300ms in) and runs 1500ms total, so a correctly-waiting drop should block
    // for roughly the remaining ~1200ms; 900ms is a comfortable floor that still clearly
    // distinguishes "waited" from "returned instantly" (a broken/no-op wait returns in low
    // single-digit milliseconds).
    assert!(
        drop_elapsed >= Duration::from_millis(900),
        "HookRunGuard::drop returned after only {drop_elapsed:?} — expected it to block for \
             roughly the remainder of the in-flight write's simulated 1.5s DB write"
    );

    // Assertion 3: a new write against the now-revoked token is rejected.
    let after_revocation = reqwest::blocking::Client::new()
        .post(&hook.endpoint)
        .header("X-AgentLoom-Token", &hook.token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(after_revocation.status(), reqwest::StatusCode::FORBIDDEN);
}
