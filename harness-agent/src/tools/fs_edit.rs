use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::provider::ToolCall;
use crate::tools::fs_read::{normalize_path, resolve_in_workspace, suggest_for_missing};
use crate::tools::{
    checkpoint_target_identity, emit_tool_completed, emit_tool_failed, emit_tool_started,
    revalidate_target_state_after_checkpoint, CheckpointTargetState, Tool, ToolContext,
    ToolOutcome,
};

pub struct FsEditTool;

#[derive(Debug, Deserialize)]
struct FsEditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for FsEditTool {
    fn name(&self) -> &str {
        "fs_edit"
    }
    fn definition(&self) -> Value {
        json!({ "type": "function", "function": {
            "name": "fs_edit",
            "description": "Replace an exact, unique occurrence of old_string with new_string in a workspace file. Fails if old_string is absent or appears more than once.",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences of old_string instead of requiring exactly one match."
                } },
                "required": ["path", "old_string", "new_string"] } } })
    }
    fn mutates(&self) -> bool {
        true
    }
    fn write_targets(&self, args: &str, workspace: &Path) -> Result<Vec<PathBuf>> {
        let a: FsEditArgs = serde_json::from_str(args)?;
        let p = if Path::new(&a.path).is_absolute() {
            normalize_path(Path::new(&a.path))
        } else {
            normalize_path(&workspace.join(&a.path))
        };
        Ok(vec![p])
    }
    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let args: FsEditArgs = match serde_json::from_str(&call.function.arguments) {
            Ok(args) => args,
            Err(e) => {
                let msg = crate::tools::humanize_args_error(
                    &call.function.arguments,
                    &e,
                    &["path", "old_string", "new_string"],
                );
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let path: PathBuf = match resolve_in_workspace(ctx.workspace, &args.path) {
            Ok(path) => path,
            Err(HarnessError::PermissionDenied(_)) => {
                let msg = format!(
                    "path is outside the workspace and was not accessed: {}. Use a relative path inside the workspace (e.g. \"src/foo.rs\"), not an absolute path.",
                    args.path
                );
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
            Err(e) => return Err(e),
        };
        emit_tool_started(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "path": path.to_string_lossy() }),
        )?;
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                let msg = format!(
                    "fs_edit: cannot read {}: {e}. {}",
                    args.path,
                    suggest_for_missing(ctx.workspace, &args.path)
                );
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let path_key = path.to_string_lossy().into_owned();
        let (entry_mtime_ms, entry_content_hash) = match ctx.file_ledger.get(&path_key) {
            Some(entry) => (entry.mtime_ms, entry.content_hash),
            None => {
                let msg = format!(
                    "fs_edit: file not read yet — read it first before editing: {}",
                    args.path
                );
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let cur_hash = crate::file_ledger::fnv1a(content.as_bytes());
        let cur_mtime = mtime_ms(&path);
        if cur_mtime > entry_mtime_ms && cur_hash != entry_content_hash {
            let msg = format!(
                "fs_edit: file changed since last read — read it again: {}",
                args.path
            );
            emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
            return Ok(ToolOutcome::recoverable(msg));
        }
        let count = content.matches(&args.old_string).count();
        if count == 0 {
            let msg = format!("fs_edit: no match for old_string in {}", args.path);
            emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
            return Ok(ToolOutcome::recoverable(msg));
        }
        if count > 1 && !args.replace_all {
            let msg = format!(
                "fs_edit: old_string not unique ({count} matches) in {}",
                args.path
            );
            emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
            return Ok(ToolOutcome::recoverable(msg));
        }
        let updated = if args.replace_all {
            content.replace(&args.old_string, &args.new_string)
        } else {
            content.replacen(&args.old_string, &args.new_string, 1)
        };
        let planned_state = CheckpointTargetState::Existing(content.as_bytes().to_vec());
        let planned_identity = checkpoint_target_identity(&path);
        let checkpointed = match crate::tools::checkpoint_pre_write(self.name(), &path).await {
            Ok(checkpointed) => checkpointed,
            Err(error) => {
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &error.to_string())?;
                return Err(error);
            }
        };
        let path = if checkpointed {
            let planned_identity = planned_identity.map_err(|error| {
                HarnessError::Runtime(format!(
                    "fs_edit: cannot inspect {} identity before checkpoint: {error}",
                    args.path
                ))
            })?;
            match revalidate_target_state_after_checkpoint(
                self.name(),
                ctx.workspace,
                &args.path,
                &path,
                &planned_state,
                &planned_identity,
            ) {
                Ok(path) => path,
                Err(error) => {
                    emit_tool_failed(ctx.recorder, self.name(), &call.id, &error.to_string())?;
                    return Err(error);
                }
            }
        } else {
            path
        };
        std::fs::write(&path, updated.as_bytes())?;
        ctx.file_ledger
            .record(&path_key, &updated, mtime_ms(&path), true);
        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "replaced": count }),
        )?;
        ctx.recorder.emit(
            "artifact.created",
            json!({
                "artifact_id": format!("art_{}", call.id), "kind": "file",
                "path": path.to_string_lossy(), "title": args.path, "mime_type": "text/plain"
            }),
        )?;
        Ok(ToolOutcome::success_mutating(serde_json::to_string(
            &json!({ "path": path.to_string_lossy(), "replaced": count }),
        )?))
    }
}

pub(crate) fn mtime_ms(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
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
            EventRecorder::new("r", None, None, journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace,
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
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
            EventRecorder::new("r", None, None, journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace,
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
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
            EventRecorder::new("r", None, None, journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace,
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
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
    async fn edit_checkpoint_target_change_during_callback_is_fatal_and_preserves_concurrent_content(
    ) {
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
    async fn edit_checkpoint_same_byte_symlink_swap_during_callback_is_fatal_and_preserves_replacement(
    ) {
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
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: workspace.path(),
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
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
}
