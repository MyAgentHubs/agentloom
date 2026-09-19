#![cfg(test)]

use super::*;

#[test]
fn random_tokens_are_unique_64_character_hex_strings() {
    let first = random_token().unwrap();
    let second = random_token().unwrap();

    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(first, second);
}

#[test]
fn skip_marker_is_stable() {
    assert_eq!(CH_SKIP_MARKER, "[CH-SKIP]");
}

#[test]
fn parses_pretooluse_paths_and_rejects_posttooluse() {
    let edit = r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/tmp/edit.txt"}}"#;
    let notebook = r#"{"hook_event_name":"PreToolUse","tool_name":"NotebookEdit","tool_input":{"notebook_path":"/tmp/notebook.ipynb"}}"#;
    let fs_edit = r#"{"hook_event_name":"PreToolUse","tool_name":"fs_edit","tool_input":{"path":"/tmp/fs-edit.txt"}}"#;
    let fs_write = r#"{"hook_event_name":"PreToolUse","tool_name":"fs_write","tool_input":{"path":"/tmp/fs-write.txt"}}"#;
    let post = r#"{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"/tmp/write.txt"}}"#;
    let bash =
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"true"}}"#;

    assert_eq!(
        hook_paths(edit).unwrap(),
        vec![PathBuf::from("/tmp/edit.txt")]
    );
    assert_eq!(
        hook_paths(notebook).unwrap(),
        vec![PathBuf::from("/tmp/notebook.ipynb")]
    );
    assert_eq!(
        hook_paths(fs_edit).unwrap(),
        vec![PathBuf::from("/tmp/fs-edit.txt")]
    );
    assert_eq!(
        hook_paths(fs_write).unwrap(),
        vec![PathBuf::from("/tmp/fs-write.txt")]
    );
    assert!(hook_paths(post)
        .unwrap_err()
        .contains("unsupported checkpoint hook event"));
    assert!(hook_paths(bash).unwrap().is_empty());
}

#[test]
fn rejects_myagent_missing_or_relative_paths() {
    let missing_path = r#"{"hook_event_name":"PreToolUse","tool_name":"fs_edit","tool_input":{}}"#;
    let relative_path = r#"{"hook_event_name":"PreToolUse","tool_name":"fs_write","tool_input":{"path":"relative.txt"}}"#;

    assert_eq!(
        hook_paths(missing_path).unwrap_err(),
        "fs_edit hook requires tool_input.path"
    );
    assert_eq!(
        hook_paths(relative_path).unwrap_err(),
        "hook path must be absolute"
    );
}

#[test]
fn parses_multi_file_patch_and_rejects_unsafe_paths() {
    let command = concat!(
        "*** Begin Patch\n",
        "*** Update File: one.txt\n@@\n-one\n+ONE\n",
        "*** Move to: moved.txt\n",
        "*** Add File: nested/two.txt\n+TWO\n",
        "*** End Patch",
    );
    let body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": "/tmp/codex-work",
        "tool_name": "apply_patch",
        "tool_input": { "command": command }
    })
    .to_string();
    assert_eq!(
        hook_paths(&body).unwrap(),
        vec![
            PathBuf::from("/tmp/codex-work/one.txt"),
            PathBuf::from("/tmp/codex-work/moved.txt"),
            PathBuf::from("/tmp/codex-work/nested/two.txt"),
        ]
    );
    for path in ["../outside.txt", "/absolute.txt"] {
        let patch = format!("*** Begin Patch\n*** Update File: {path}\n@@\n-a\n+b\n*** End Patch");
        assert!(parse_patch_paths(&patch).is_err());
    }
}

#[test]
fn wrong_and_stale_tokens_are_rejected() {
    let server = start_server(None).unwrap();
    server.registrations.lock().unwrap().insert(
        "right".into(),
        Registration {
            db_path: PathBuf::from("/tmp/db"),
            session_id: "s1".into(),
            run_id: "r1".into(),
            allowed_root: PathBuf::from("/tmp"),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let body = r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/tmp/x"}}"#;
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", "wrong")
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.registrations.lock().unwrap().remove("right");
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header("X-AgentLoom-Token", "right")
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[test]
fn existing_internal_error_exits_have_distinct_diagnostic_markers() {
    let server = start_server(None).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let allowed_root = fs::canonicalize(temp.path()).unwrap();
    server.registrations.lock().unwrap().extend([
        (
            "path-error".into(),
            Registration {
                db_path: temp.path().join("unused.db"),
                session_id: "s-path".into(),
                run_id: "r-path".into(),
                allowed_root: allowed_root.clone(),
                ..Registration::default()
            },
        ),
        (
            "checkpoint-error".into(),
            Registration {
                db_path: temp.path().join("missing-parent").join("checkpoint.db"),
                session_id: "s-checkpoint".into(),
                run_id: "r-checkpoint".into(),
                allowed_root: allowed_root.clone(),
                ..Registration::default()
            },
        ),
    ]);
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let client = reqwest::blocking::Client::new();

    let path_error = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", "path-error")
        .body(r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{}}"#)
        .send()
        .unwrap();
    assert_eq!(
        path_error.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(path_error.text().unwrap().starts_with(CH_883_MARKER));

    let target = allowed_root.join("target.txt");
    fs::write(&target, "contents").unwrap();
    let checkpoint_error_body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": target }
    })
    .to_string();
    let checkpoint_error = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", "checkpoint-error")
        .body(checkpoint_error_body)
        .send()
        .unwrap();
    assert_eq!(
        checkpoint_error.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(checkpoint_error.text().unwrap().starts_with(CH_977_MARKER));
}

#[test]
fn write_outside_project_root_is_skipped_but_git_path_still_fails_closed() {
    let (_home_root, home) = crate::test_support::tmp_root();
    let _home = HomeGuard::set(&home);
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    let allowed_root = fs::canonicalize(&project).unwrap();
    let outside = temp.path().join("handoff.md");
    fs::write(&outside, "outside before").unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let server = start_server(None).unwrap();
    let token = "outside-root";
    let session_id = "s-outside-root";
    let run_id = "r-outside-root";
    server.registrations.lock().unwrap().insert(
        token.into(),
        Registration {
            db_path,
            session_id: session_id.into(),
            run_id: run_id.into(),
            allowed_root: allowed_root.clone(),
            ..Registration::default()
        },
    );
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
    let client = reqwest::blocking::Client::new();
    let outside_body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": outside }
    })
    .to_string();

    let response = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", token)
        .body(outside_body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let response_body = response.text().unwrap();
    assert!(!response_body.contains(CH_977_MARKER));
    assert!(!response_body.contains(CH_883_MARKER));
    let entries = crate::checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .list_entries(session_id, run_id)
        .unwrap();
    assert!(entries.is_empty());

    let git_path = allowed_root.join(".git/config");
    fs::create_dir_all(git_path.parent().unwrap()).unwrap();
    fs::write(&git_path, "config").unwrap();
    let git_body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": git_path }
    })
    .to_string();
    let response = client
        .post(&endpoint)
        .header("X-AgentLoom-Token", token)
        .body(git_body)
        .send()
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(response.text().unwrap().contains(CH_977_MARKER));
}

#[test]
fn apply_patch_records_inside_path_and_skips_outside_path() {
    let (_home_root, home) = crate::test_support::tmp_root();
    let _home = HomeGuard::set(&home);
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    let allowed_root = fs::canonicalize(&project).unwrap();
    let inside = allowed_root.join("inside.txt");
    let outside = temp.path().join("outside.txt");
    fs::write(&inside, "inside before").unwrap();
    fs::write(&outside, "outside before").unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let server = start_server(None).unwrap();
    let token = "mixed-paths";
    let session_id = "s-mixed-paths";
    let run_id = "r-mixed-paths";
    server.registrations.lock().unwrap().insert(
        token.into(),
        Registration {
            db_path,
            session_id: session_id.into(),
            run_id: run_id.into(),
            allowed_root,
            ..Registration::default()
        },
    );
    let command = concat!(
        "*** Begin Patch\n",
        "*** Update File: project/inside.txt\n@@\n-before\n+after\n",
        "*** Update File: outside.txt\n@@\n-before\n+after\n",
        "*** End Patch",
    );
    let body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": temp.path(),
        "tool_name": "apply_patch",
        "tool_input": { "command": command }
    })
    .to_string();
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);

    let response = reqwest::blocking::Client::new()
        .post(endpoint)
        .header("X-AgentLoom-Token", token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let response_body = response.text().unwrap();
    assert!(!response_body.contains(CH_977_MARKER));
    assert!(!response_body.contains(CH_883_MARKER));
    let entries = crate::checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .list_entries(session_id, run_id)
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].file_path, inside);
}

#[test]
fn apply_patch_skips_outside_path_before_recording_inside_path() {
    let (_home_root, home) = crate::test_support::tmp_root();
    let _home = HomeGuard::set(&home);
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    let allowed_root = fs::canonicalize(&project).unwrap();
    let inside = allowed_root.join("inside.txt");
    let outside = temp.path().join("outside.txt");
    fs::write(&inside, "inside before").unwrap();
    fs::write(&outside, "outside before").unwrap();
    let db_path = temp.path().join("agentloom.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::init_schema(&conn).unwrap();
    let server = start_server(None).unwrap();
    let token = "outside-before-inside";
    let session_id = "s-outside-before-inside";
    let run_id = "r-outside-before-inside";
    server.registrations.lock().unwrap().insert(
        token.into(),
        Registration {
            db_path,
            session_id: session_id.into(),
            run_id: run_id.into(),
            allowed_root,
            ..Registration::default()
        },
    );
    let command = concat!(
        "*** Begin Patch\n",
        "*** Update File: outside.txt\n@@\n-before\n+after\n",
        "*** Update File: project/inside.txt\n@@\n-before\n+after\n",
        "*** End Patch",
    );
    let body = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": temp.path(),
        "tool_name": "apply_patch",
        "tool_input": { "command": command }
    })
    .to_string();
    let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);

    let response = reqwest::blocking::Client::new()
        .post(endpoint)
        .header("X-AgentLoom-Token", token)
        .body(body)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let entries = crate::checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .list_entries(session_id, run_id)
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries.iter().any(|entry| entry.file_path == inside));
    assert!(!entries.iter().any(|entry| entry.file_path == outside));
}
