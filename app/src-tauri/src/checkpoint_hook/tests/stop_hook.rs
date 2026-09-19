#![cfg(test)]

use super::*;

// PsRow/parse_ps_row/live_background_processes are the unix-only ps-descendant fallback path
// (used only when background_tasks is None); gate the helper and every test that touches them
// the same way.
#[cfg(unix)]
fn ps_row(pid: u32, pgid: u32, ppid: u32, stat: &str, command: &str) -> PsRow {
    PsRow {
        pid,
        pgid,
        ppid,
        stat: stat.to_string(),
        command: command.to_string(),
    }
}

#[test]
#[cfg(unix)]
fn parse_ps_row_parses_typical_line_with_multi_word_command() {
    let row = parse_ps_row("  123   456     1 Ss+  /bin/sh -c 'sleep 30 && echo done'").unwrap();
    assert_eq!(row.pid, 123);
    assert_eq!(row.pgid, 456);
    assert_eq!(row.ppid, 1);
    assert_eq!(row.stat, "Ss+");
    assert_eq!(row.command, "/bin/sh -c 'sleep 30 && echo done'");
}

#[test]
#[cfg(unix)]
fn parse_ps_row_rejects_malformed_or_empty_lines() {
    assert!(parse_ps_row("").is_none());
    assert!(parse_ps_row("not-enough-numeric-fields").is_none());
    assert!(parse_ps_row("abc 1 2 S command").is_none());
}

#[test]
#[cfg(unix)]
fn live_background_processes_finds_multi_level_ppid_descendants() {
    let rows = vec![
        ps_row(100, 100, 1, "Ss", "claude"),
        ps_row(200, 100, 100, "S", "sleep 30"),
        ps_row(201, 300, 200, "S", "python worker.py"),
    ];
    let live = live_background_processes(&rows, 100, "127.0.0.1:9/checkpoint");
    let pids: Vec<u32> = live.iter().map(|row| row.pid).collect();
    assert_eq!(pids.len(), 2);
    assert!(pids.contains(&200));
    assert!(pids.contains(&201));
}

#[test]
#[cfg(unix)]
fn live_background_processes_pgid_fallback_catches_reparented_child() {
    // pid 555 was reparented to init (ppid=1) but still carries the agent's original pgid.
    let rows = vec![
        ps_row(100, 100, 1, "Ss", "claude"),
        ps_row(555, 100, 1, "S", "codex exec"),
    ];
    let live = live_background_processes(&rows, 100, "marker-not-present");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].pid, 555);
}

#[test]
#[cfg(unix)]
fn live_background_processes_excludes_agent_row_itself() {
    let rows = vec![ps_row(100, 100, 1, "Ss", "claude")];
    assert!(live_background_processes(&rows, 100, "marker-not-present").is_empty());
}

#[test]
#[cfg(unix)]
fn live_background_processes_excludes_zombies() {
    let rows = vec![
        ps_row(100, 100, 1, "Ss", "claude"),
        ps_row(200, 100, 100, "Z", "sleep 30 <defunct>"),
    ];
    assert!(live_background_processes(&rows, 100, "marker-not-present").is_empty());
}

#[test]
#[cfg(unix)]
fn live_background_processes_excludes_hook_curl_command() {
    let marker = "127.0.0.1:54321/checkpoint";
    let rows = vec![
        ps_row(100, 100, 1, "Ss", "claude"),
        ps_row(
            200,
            100,
            100,
            "S",
            &format!("/bin/sh -c curl ... {marker} || true"),
        ),
    ];
    assert!(live_background_processes(&rows, 100, marker).is_empty());
}

#[test]
#[cfg(unix)]
fn live_background_processes_on_empty_table_is_empty() {
    assert!(live_background_processes(&[], 100, "marker-not-present").is_empty());
}

#[test]
#[cfg(unix)]
fn live_background_processes_agent_row_absent_still_finds_children_by_ppid() {
    let rows = vec![ps_row(200, 100, 100, "S", "sleep 30")];
    let live = live_background_processes(&rows, 100, "marker-not-present");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].pid, 200);
}

#[test]
fn stop_block_reason_lists_up_to_five_and_counts_remainder() {
    // stop_block_reason is generic over already-labeled items now (no PsRow/task distinction
    // at this layer — handle_stop picks exactly one source per call), so this test only needs
    // plain strings.
    let items: Vec<String> = (0..7).map(|i| format!("worker-{i}")).collect();
    let reason = stop_block_reason(&items, 2);
    assert!(reason.contains("and 2 more"));
    assert!(reason.contains(&format!("stop-block 2/{STOP_BLOCK_MAX_COUNT}")));
    assert!(reason.contains("worker-0"));
    assert!(reason.contains("worker-4"));
    assert!(!reason.contains("worker-5"));
}

#[test]
fn stop_block_reason_preserves_item_order() {
    let items = vec!["first item".to_string(), "second item".to_string()];
    let reason = stop_block_reason(&items, 1);
    assert!(reason.contains("first item"));
    assert!(reason.contains("second item"));
    let first_pos = reason.find("first item").unwrap();
    let second_pos = reason.find("second item").unwrap();
    assert!(first_pos < second_pos);
}

#[test]
fn background_task_label_prefers_description_and_command_falls_back_when_one_missing() {
    let both = BackgroundTaskInput {
        description: "Start a 60-second background sleep".into(),
        command: "sleep 60".into(),
        ..BackgroundTaskInput::default()
    };
    let label = background_task_label(&both);
    assert!(label.contains("Start a 60-second background sleep"));
    assert!(label.contains("sleep 60"));

    let command_only = BackgroundTaskInput {
        command: "sleep 60".into(),
        ..BackgroundTaskInput::default()
    };
    assert_eq!(background_task_label(&command_only), "sleep 60");

    let neither = BackgroundTaskInput::default();
    assert_eq!(background_task_label(&neither), "background task");
}

#[cfg(unix)]
fn spawn_agent_with_background_sleep() -> (std::process::Child, u32) {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("sleep 30 & sleep 31");
    command.process_group(0);
    let child = command.spawn().unwrap();
    let pid = child.id();
    (child, pid)
}

#[cfg(unix)]
fn kill_and_reap(mut child: std::process::Child, pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.wait();
}

/// Under heavy parallel test load, `ps` can be asked for a snapshot before the shell has
/// actually forked its background job. Poll (bounded) until it shows up, so the test asserts
/// on the hook's actual blocking behavior rather than on scheduler timing.
#[cfg(unix)]
fn wait_until_background_child_visible(agent_pid: u32, marker: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let rows = ps_snapshot().unwrap();
        if !live_background_processes(&rows, agent_pid, marker).is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background child never became visible in ps within 5s"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
#[cfg(unix)]
fn stop_hook_blocks_while_background_children_alive_then_releases() {
    let (child, agent_pid) = spawn_agent_with_background_sleep();
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            agent_pid: Some(agent_pid),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let client = reqwest::blocking::Client::new();
    let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
    let marker = format!("127.0.0.1:{}{HOOK_PATH}", server.port);
    wait_until_background_child_visible(agent_pid, &marker);

    let response = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().unwrap();
    assert!(text.contains("\"decision\":\"block\""));

    kill_and_reap(child, agent_pid);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let rows = ps_snapshot().unwrap();
        if live_background_processes(&rows, agent_pid, &marker).is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background children did not exit within 5s"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let response = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
}

#[test]
#[cfg(unix)]
fn stop_hook_stops_blocking_after_max_count_even_if_processes_still_alive() {
    let (child, agent_pid) = spawn_agent_with_background_sleep();
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            agent_pid: Some(agent_pid),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let client = reqwest::blocking::Client::new();
    let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
    let marker = format!("127.0.0.1:{}{HOOK_PATH}", server.port);
    wait_until_background_child_visible(agent_pid, &marker);

    for attempt in 1..=STOP_BLOCK_MAX_COUNT {
        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "attempt {attempt} of {STOP_BLOCK_MAX_COUNT} should still be blocked"
        );
    }
    let response = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

    kill_and_reap(child, agent_pid);
}

#[test]
fn stop_hook_with_no_agent_pid_registered_passes_through() {
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
}

#[test]
fn stop_hook_with_unknown_token_is_rejected_like_any_other_event() {
    // Unregistered tokens are rejected before the Stop/PreToolUse branch is even reached
    // (same 403 as a forged PreToolUse request) — this is existing behavior, unchanged here.
    let server = start_server(None).unwrap();
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", "not-a-registered-token")
        .body(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

// Verbatim (field-for-field) payload shapes captured from a real `claude -p` Stop hook
// invocation, per the 2026-07-24 packet capture. Kept as fixtures so parsing/behavior is
// pinned to what Claude Code actually sends, not to our guess at its shape.
const REAL_STOP_PAYLOAD_NO_TASKS: &str = r#"{
        "session_id": "sess-1",
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": "/tmp/project",
        "prompt_id": "prompt-1",
        "permission_mode": "acceptEdits",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "Done.",
        "session_crons": [],
        "background_tasks": []
    }"#;

const REAL_STOP_PAYLOAD_WITH_RUNNING_TASK: &str = r#"{
        "session_id": "sess-1",
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": "/tmp/project",
        "prompt_id": "prompt-1",
        "permission_mode": "acceptEdits",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "Done.",
        "session_crons": [],
        "background_tasks": [{"id":"bjsxof5lo","type":"shell","status":"running","description":"Start a 60-second background sleep","command":"sleep 60"}]
    }"#;

#[test]
fn real_stop_payload_with_no_tasks_parses_and_has_no_running_tasks() {
    let input: HookInput = serde_json::from_str(REAL_STOP_PAYLOAD_NO_TASKS).unwrap();
    assert_eq!(input.hook_event_name, "Stop");
    // The real payload sends "background_tasks": [] explicitly — that's Some(empty), not
    // None. Some(_) is authoritative regardless of emptiness (see M1: this is the whole point
    // of the fix, an explicit empty list must never fall back to the ps scan).
    assert_eq!(input.background_tasks, Some(Vec::new()));
}

#[test]
fn stop_payload_missing_background_tasks_key_parses_as_none() {
    let input: HookInput =
        serde_json::from_str(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#).unwrap();
    assert_eq!(input.background_tasks, None);
}

#[test]
fn real_stop_payload_with_running_task_parses_fields_verbatim() {
    let input: HookInput = serde_json::from_str(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK).unwrap();
    let tasks = input.background_tasks.unwrap();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(task.id, "bjsxof5lo");
    assert_eq!(task.r#type, "shell");
    assert_eq!(task.status, "running");
    assert_eq!(task.description, "Start a 60-second background sleep");
    assert_eq!(task.command, "sleep 60");
}

#[test]
fn real_stop_payload_with_no_tasks_and_no_agent_pid_passes_through() {
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(REAL_STOP_PAYLOAD_NO_TASKS)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
}

#[test]
fn running_background_task_blocks_even_without_registered_agent_pid() {
    // background_tasks is the primary signal and does not depend on agent_pid at all: a
    // registration that never got a pid (e.g. register_agent_pid raced or was never called)
    // must still block on a real "running" task.
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().unwrap();
    assert!(text.contains("\"decision\":\"block\""));
    assert!(text.contains("Start a 60-second background sleep"));
    assert!(text.contains("sleep 60"));
}

/// D2 (2026-07-29 delta review) — regression pin for P1: before all four bare
/// `registrations.lock()` call sites (`install`, `register_agent_pid`, and `handle_stop`'s
/// two) were converted to `lock_registrations`, a single poisoning panic (poisoning is
/// sticky — every later `.lock()` on the same mutex keeps failing forever, `into_inner()`
/// doesn't clear it) left the Stop-block anti-thrash guard permanently fail-open: every
/// subsequent Stop request got 204 with no error and nothing logged, instead of correctly
/// blocking on a still-running background task. This proves Stop-block survives a poisoning
/// panic caused by a totally unrelated request.
#[test]
fn stop_hook_still_blocks_after_a_poisoning_panic() {
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let client = reqwest::blocking::Client::new();

    // Poison the registrations mutex via the shared test-only panic injection point. Any
    // token works — the panic fires before token lookup — so this doesn't need its own
    // registration.
    let panicking = client
        .post(&endpoint)
        .header(TEST_PANIC_HEADER, "1")
        .body(REAL_STOP_PAYLOAD_NO_TASKS)
        .send()
        .unwrap();
    assert!(panicking.status().is_server_error());
    std::thread::sleep(Duration::from_millis(100));

    let response = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().unwrap();
    assert!(
        text.contains("\"decision\":\"block\""),
        "expected Stop-block to still correctly fire after a poisoning panic elsewhere, got: \
             {text}"
    );
}

#[test]
fn completed_status_background_task_is_not_treated_as_running() {
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let body = r#"{"hook_event_name":"Stop","stop_hook_active":false,"background_tasks":[{"id":"x","type":"shell","status":"completed","description":"d","command":"sleep 1"}]}"#;
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
}

// M1 regression test: this is the exact false-positive the authoritative-background_tasks
// fix addresses. Before M1, background_tasks and the ps scan were unioned, so a real live
// ps-descendant (standing in for e.g. a long-lived stdio MCP server child of claude) kept
// triggering a block on every Stop even once Claude Code itself reported nothing running.
#[test]
#[cfg(unix)]
fn background_tasks_present_but_empty_is_authoritative_and_ignores_live_ps_descendant() {
    let (child, agent_pid) = spawn_agent_with_background_sleep();
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            agent_pid: Some(agent_pid),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let marker = format!("127.0.0.1:{}{HOOK_PATH}", server.port);
    // Prove the ps-descendant is really there (and would have tripped the old union logic)
    // before asserting the new behavior ignores it.
    wait_until_background_child_visible(agent_pid, &marker);

    let body = REAL_STOP_PAYLOAD_NO_TASKS;
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

    kill_and_reap(child, agent_pid);
}

#[test]
fn stop_hook_window_expired_passes_through_even_with_a_running_task() {
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            stop_blocks: 1,
            first_stop_block_at: Some(
                std::time::Instant::now()
                    - std::time::Duration::from_secs(STOP_BLOCK_MAX_WINDOW_SECS + 1),
            ),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
}

#[test]
fn stop_hook_within_window_still_blocks_with_a_running_task() {
    let server = start_server(None).unwrap();
    let token = random_token().unwrap();
    server.registrations.lock().unwrap().insert(
        token.clone(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            stop_blocks: 1,
            first_stop_block_at: Some(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            ),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", &token)
        .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.text().unwrap().contains("\"decision\":\"block\""));
}
