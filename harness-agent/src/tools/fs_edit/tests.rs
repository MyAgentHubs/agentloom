#![cfg(test)]

use super::*;
use crate::events::EventRecorder;
use crate::provider::{FunctionCall, ToolCall};
use crate::tools::fs_read::FsReadTool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn call(args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "c".into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "fs_edit".into(),
            arguments: args.to_string(),
        },
    }
}

fn read_call(args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "r".into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "fs_read".into(),
            arguments: args.to_string(),
        },
    }
}

async fn run_edit(
    workspace: &Path,
    journal: &Path,
    args: serde_json::Value,
) -> Result<crate::tools::ToolOutcome> {
    let mut rec =
        EventRecorder::new("r", None, None, journal, crate::events::OutputMode::Silent).unwrap();
    let mut ledger = crate::file_ledger::FileLedger::new();
    let mut ctx = ToolContext {
        workspace,
        recorder: &mut rec,
        file_ledger: &mut ledger,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        extra_read_roots: &[],
    };
    FsEditTool.execute(&mut ctx, &call(args)).await
}

async fn run_edit_after_read(
    workspace: &Path,
    journal: &Path,
    read_path: &str,
    args: serde_json::Value,
) -> Result<crate::tools::ToolOutcome> {
    let mut rec =
        EventRecorder::new("r", None, None, journal, crate::events::OutputMode::Silent).unwrap();
    let mut ledger = crate::file_ledger::FileLedger::new();
    let mut ctx = ToolContext {
        workspace,
        recorder: &mut rec,
        file_ledger: &mut ledger,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        extra_read_roots: &[],
    };
    let read_out = FsReadTool
        .execute(&mut ctx, &read_call(json!({ "path": read_path })))
        .await?;
    assert_eq!(read_out.status, crate::tools::ToolStatus::Success);
    FsEditTool.execute(&mut ctx, &call(args)).await
}

async fn run_edit_after_partial_read(
    workspace: &Path,
    journal: &Path,
    read_args: serde_json::Value,
    args: serde_json::Value,
) -> Result<crate::tools::ToolOutcome> {
    let mut rec =
        EventRecorder::new("r", None, None, journal, crate::events::OutputMode::Silent).unwrap();
    let mut ledger = crate::file_ledger::FileLedger::new();
    let mut ctx = ToolContext {
        workspace,
        recorder: &mut rec,
        file_ledger: &mut ledger,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        extra_read_roots: &[],
    };
    let read_out = FsReadTool.execute(&mut ctx, &read_call(read_args)).await?;
    assert_eq!(read_out.status, crate::tools::ToolStatus::Success);
    FsEditTool.execute(&mut ctx, &call(args)).await
}

#[tokio::test]
async fn edit_existing_without_read_is_recoverable() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();

    let out = run_edit(
        workspace.path(),
        &journal,
        json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(out.content.contains("read it first"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one foo two");
    let events = std::fs::read_to_string(&journal).unwrap();
    assert!(events.contains("\"type\":\"tool.failed\""));
}

#[tokio::test]
async fn edit_after_read_succeeds() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();

    let out = run_edit_after_read(
        workspace.path(),
        &journal,
        "a.txt",
        json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::Success);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one bar two");
}

#[tokio::test]
async fn edit_checkpoint_posts_before_write() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();
    let expected_path = resolve_in_workspace(workspace.path(), "a.txt").unwrap();
    let saw_original = Arc::new(AtomicBool::new(false));
    let saw_original_for_mock = saw_original.clone();
    let file_for_mock = file.clone();
    let expected_body = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "fs_edit",
        "tool_input": { "path": expected_path.to_string_lossy() },
    });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/checkpoint"))
        .respond_with(move |_req: &wiremock::Request| {
            let current = std::fs::read_to_string(&file_for_mock).unwrap();
            saw_original_for_mock.store(current == "one foo two", Ordering::SeqCst);
            ResponseTemplate::new(204)
        })
        .expect(1)
        .mount(&server)
        .await;

    let out = crate::tools::with_checkpoint_env_override_for_test(
        Some(format!("{}/checkpoint", server.uri())),
        Some("secret-token".into()),
        async {
            run_edit_after_read(
                workspace.path(),
                &journal,
                "a.txt",
                json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
            )
            .await
        },
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::Success);
    assert!(saw_original.load(Ordering::SeqCst));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one bar two");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/checkpoint");
    assert_eq!(
        request
            .headers
            .get("x-agentloom-token")
            .and_then(|value| value.to_str().ok()),
        Some("secret-token")
    );
    let body: serde_json::Value = request.body_json().unwrap();
    assert_eq!(body, expected_body);
    server.verify().await;
}

#[tokio::test]
async fn edit_checkpoint_target_change_during_callback_is_fatal_and_preserves_concurrent_content() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();
    let file_for_mock = file.clone();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/checkpoint"))
        .respond_with(move |_req: &wiremock::Request| {
            std::fs::write(&file_for_mock, "one foo from concurrent writer").unwrap();
            ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(50))
        })
        .expect(1)
        .mount(&server)
        .await;

    let err = crate::tools::with_checkpoint_env_override_for_test(
        Some(format!("{}/checkpoint", server.uri())),
        Some("secret-token".into()),
        async {
            run_edit_after_read(
                workspace.path(),
                &journal,
                "a.txt",
                json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
            )
            .await
        },
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("changed during checkpoint wait"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one foo from concurrent writer"
    );
    let events: Vec<serde_json::Value> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "tool.failed" && event["payload"]["tool_call_id"] == "c"
    }));
    assert!(!events.iter().any(|event| {
        event["type"] == "tool.completed" && event["payload"]["tool_call_id"] == "c"
    }));
    assert!(!events.iter().any(|event| {
        event["type"] == "artifact.created" && event["payload"]["artifact_id"] == "art_c"
    }));
    server.verify().await;
}

#[cfg(unix)]
#[tokio::test]
async fn edit_checkpoint_same_byte_symlink_swap_during_callback_is_fatal_and_preserves_replacement()
{
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    let replacement = workspace.path().join("replacement.txt");
    std::fs::write(&file, "one foo two").unwrap();
    std::fs::write(&replacement, "one foo two").unwrap();
    let file_for_mock = file.clone();
    let replacement_for_mock = replacement.clone();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/checkpoint"))
        .respond_with(move |_req: &wiremock::Request| {
            std::fs::remove_file(&file_for_mock).unwrap();
            std::os::unix::fs::symlink(&replacement_for_mock, &file_for_mock).unwrap();
            ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(50))
        })
        .expect(1)
        .mount(&server)
        .await;

    let err = crate::tools::with_checkpoint_env_override_for_test(
        Some(format!("{}/checkpoint", server.uri())),
        Some("secret-token".into()),
        async {
            run_edit_after_read(
                workspace.path(),
                &journal,
                "a.txt",
                json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
            )
            .await
        },
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("different target"));
    assert!(std::fs::symlink_metadata(&file)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_to_string(&replacement).unwrap(),
        "one foo two"
    );
    let events: Vec<serde_json::Value> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "tool.failed" && event["payload"]["tool_call_id"] == "c"
    }));
    assert!(!events.iter().any(|event| {
        event["type"] == "tool.completed" && event["payload"]["tool_call_id"] == "c"
    }));
    assert!(!events.iter().any(|event| {
        event["type"] == "artifact.created" && event["payload"]["artifact_id"] == "art_c"
    }));
    server.verify().await;
}

#[tokio::test]
async fn edit_after_partial_read_succeeds() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two\nthree baz four\n").unwrap();

    let out = run_edit_after_partial_read(
        workspace.path(),
        &journal,
        json!({"path":"a.txt","start_line":1,"end_line":1}),
        json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::Success);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one bar two\nthree baz four\n"
    );
}

#[tokio::test]
async fn edit_after_external_change_asks_reread() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();
    let mut rec =
        EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent).unwrap();
    let mut ledger = crate::file_ledger::FileLedger::new();
    let mut ctx = ToolContext {
        workspace: workspace.path(),
        recorder: &mut rec,
        file_ledger: &mut ledger,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        extra_read_roots: &[],
    };

    let read_out = FsReadTool
        .execute(&mut ctx, &read_call(json!({"path":"a.txt"})))
        .await
        .unwrap();
    assert_eq!(read_out.status, crate::tools::ToolStatus::Success);
    std::fs::write(&file, "one foo externally changed").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))
        .unwrap();

    let out = FsEditTool
        .execute(
            &mut ctx,
            &call(json!({"path":"a.txt","old_string":"foo","new_string":"bar"})),
        )
        .await
        .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(out.content.contains("read it again"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one foo externally changed"
    );
}

#[tokio::test]
async fn tool_outcome_fs_edit_success_invalidates_verification() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    std::fs::write(workspace.path().join("a.txt"), "one foo two").unwrap();

    let out = run_edit_after_read(
        workspace.path(),
        &journal,
        "a.txt",
        json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::Success);
    assert!(out.invalidates_verification);
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(v["replaced"], 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
        "one bar two"
    );
    let events = std::fs::read_to_string(&journal).unwrap();
    assert!(events.contains("\"type\":\"artifact.created\""));
}

#[tokio::test]
async fn tool_outcome_fs_edit_no_match_is_recoverable_and_emits_failed() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/checkpoint"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let err = crate::tools::with_checkpoint_env_override_for_test(
        Some(format!("{}/checkpoint", server.uri())),
        Some("secret-token".into()),
        async {
            run_edit_after_read(
                workspace.path(),
                &journal,
                "a.txt",
                json!({"path":"a.txt","old_string":"missing","new_string":"bar"}),
            )
            .await
        },
    )
    .await;

    let out = err.unwrap();
    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(!out.invalidates_verification);
    assert!(out.content.contains("no match"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one foo two");
    let events = std::fs::read_to_string(&journal).unwrap();
    assert!(!events.contains("\"type\":\"artifact.created\""));
    assert!(events.contains("\"type\":\"tool.failed\""));
    server.verify().await;
}

#[tokio::test]
async fn edit_checkpoint_http_500_is_fatal_and_keeps_original_content() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();
    let file_for_mock = file.clone();
    let saw_original = Arc::new(AtomicBool::new(false));
    let saw_original_for_mock = saw_original.clone();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/checkpoint"))
        .respond_with(move |_req: &wiremock::Request| {
            let current = std::fs::read_to_string(&file_for_mock).unwrap();
            saw_original_for_mock.store(current == "one foo two", Ordering::SeqCst);
            ResponseTemplate::new(500)
        })
        .expect(1)
        .mount(&server)
        .await;

    let err = crate::tools::with_checkpoint_env_override_for_test(
        Some(format!("{}/checkpoint", server.uri())),
        Some("secret-token".into()),
        async {
            run_edit_after_read(
                workspace.path(),
                &journal,
                "a.txt",
                json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
            )
            .await
        },
    )
    .await
    .unwrap_err();

    assert!(saw_original.load(Ordering::SeqCst));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one foo two");
    assert!(err.to_string().contains("checkpoint"));
    server.verify().await;
}

#[tokio::test]
async fn edit_checkpoint_connection_failure_is_fatal_and_keeps_original_content() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "one foo two").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "http://127.0.0.1:{}/checkpoint",
        listener.local_addr().unwrap().port()
    );
    drop(listener);

    let err = crate::tools::with_checkpoint_env_override_for_test(
        Some(endpoint),
        Some("secret-token".into()),
        async {
            run_edit_after_read(
                workspace.path(),
                &journal,
                "a.txt",
                json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
            )
            .await
        },
    )
    .await
    .unwrap_err();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one foo two");
    assert!(err.to_string().contains("checkpoint"));
}

#[tokio::test]
async fn tool_outcome_fs_edit_non_unique_is_recoverable_and_emits_failed() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "x and x").unwrap();

    let err = run_edit_after_read(
        workspace.path(),
        &journal,
        "a.txt",
        json!({"path":"a.txt","old_string":"x","new_string":"y"}),
    )
    .await;

    let out = err.unwrap();
    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(!out.invalidates_verification);
    assert!(out.content.contains("not unique"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x and x");
    let events = std::fs::read_to_string(&journal).unwrap();
    assert!(!events.contains("\"type\":\"artifact.created\""));
    assert!(events.contains("\"type\":\"tool.failed\""));
}

#[tokio::test]
async fn replace_all_true_replaces_every_match_and_reports_count() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "x and x and x").unwrap();

    let out = run_edit_after_read(
        workspace.path(),
        &journal,
        "a.txt",
        json!({
            "path": "a.txt",
            "old_string": "x",
            "new_string": "y",
            "replace_all": true
        }),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::Success);
    assert!(out.invalidates_verification);
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(v["replaced"], 3);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "y and y and y");

    let events = std::fs::read_to_string(&journal).unwrap();
    assert!(events.contains("\"replaced\":3"));
    assert!(events.contains("\"type\":\"artifact.created\""));
}

#[tokio::test]
async fn replace_all_false_keeps_non_unique_recoverable_error() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, "x and x").unwrap();

    let out = run_edit_after_read(
        workspace.path(),
        &journal,
        "a.txt",
        json!({
            "path": "a.txt",
            "old_string": "x",
            "new_string": "y",
            "replace_all": false
        }),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(!out.invalidates_verification);
    assert!(out.content.contains("not unique"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x and x");
}

#[test]
fn fs_edit_schema_exposes_replace_all() {
    let def = FsEditTool.definition();
    let replace_all = &def["function"]["parameters"]["properties"]["replace_all"];
    assert_eq!(replace_all["type"], "boolean");
    assert!(replace_all["description"]
        .as_str()
        .unwrap()
        .contains("all occurrences"));
}

#[tokio::test]
async fn tool_outcome_fs_edit_bad_args_is_recoverable_and_emits_failed() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");

    let out = run_edit(workspace.path(), &journal, json!({"path":"a.txt"}))
        .await
        .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(out.content.contains("missing required"));
    let events = std::fs::read_to_string(&journal).unwrap();
    assert!(events.contains("\"type\":\"tool.failed\""));
}

#[tokio::test]
async fn edit_missing_required_param_says_which() {
    let workspace = tempfile::tempdir().unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");

    let out = run_edit(
        workspace.path(),
        &journal,
        json!({"old_string":"x","new_string":"y"}),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(out.content.contains("missing required"));
    assert!(out.content.contains("path"));
}

#[tokio::test]
async fn outside_path_is_recoverable_and_not_edited() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = root.path().join("escape.txt");
    std::fs::write(&outside, "one foo two").unwrap();
    let journal_dir = tempfile::tempdir().unwrap();
    let journal = journal_dir.path().join("e.jsonl");

    let out = run_edit(
        &workspace,
        &journal,
        json!({"path":"../escape.txt","old_string":"foo","new_string":"bar"}),
    )
    .await
    .unwrap();

    assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
    assert!(!out.invalidates_verification);
    assert!(out.content.contains("outside the workspace"));
    assert!(out.content.contains("../escape.txt"));
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "one foo two");
    let events = std::fs::read_to_string(&journal).unwrap();
    assert!(events.contains("\"type\":\"tool.failed\""));
}
