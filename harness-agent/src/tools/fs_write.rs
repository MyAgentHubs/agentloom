use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::model_registry::EditFormat;
use crate::provider::ToolCall;
use crate::tools::fs_edit::mtime_ms;
use crate::tools::fs_read::{normalize_path, resolve_in_workspace};
use crate::tools::{
    checkpoint_target_identity, checkpoint_target_state, emit_tool_completed, emit_tool_failed,
    emit_tool_started, revalidate_target_state_after_checkpoint, CheckpointTargetState, Tool,
    ToolContext, ToolOutcome,
};

pub struct FsWriteTool;

#[derive(Debug, Deserialize)]
struct FsWriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for FsWriteTool {
    fn name(&self) -> &str {
        "fs_write"
    }
    fn definition(&self) -> Value {
        json!({ "type": "function", "function": {
            "name": "fs_write",
            "description": "Create or overwrite a file with the given content. Prefer fs_edit when changing an existing file — it sends only the changed lines. Use fs_write only to create a new file or fully rewrite a small one; rewriting a large existing file in one call exceeds the model output limit and gets truncated.",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string" }, "content": { "type": "string" } },
                "required": ["path", "content"] } } })
    }
    fn mutates(&self) -> bool {
        true
    }
    fn write_targets(&self, args: &str, workspace: &Path) -> Result<Vec<PathBuf>> {
        let a: FsWriteArgs = serde_json::from_str(args)?;
        Ok(vec![target_path(workspace, &a.path)])
    }
    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let args: FsWriteArgs = match serde_json::from_str(&call.function.arguments) {
            Ok(args) => args,
            Err(e) => {
                let msg = crate::tools::humanize_args_error(
                    &call.function.arguments,
                    &e,
                    &["path", "content"],
                );
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let path = match resolve_in_workspace(ctx.workspace, &args.path) {
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
        let path_key = path.to_string_lossy().into_owned();
        let planned_state = match checkpoint_target_state(&path).map_err(|error| {
            HarnessError::Runtime(format!(
                "fs_write: cannot inspect {} before checkpoint: {error}",
                args.path
            ))
        })? {
            CheckpointTargetState::Existing(current_bytes) => {
                let (entry_mtime_ms, entry_content_hash, entry_full_read) =
                    match ctx.file_ledger.get(&path_key) {
                        Some(entry) => (entry.mtime_ms, entry.content_hash, entry.full_read),
                        None => {
                            let msg = format!(
                                "fs_write: file not read yet — read it first before editing: {}",
                                args.path
                            );
                            emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                            return Ok(ToolOutcome::recoverable(msg));
                        }
                    };
                if !entry_full_read {
                    let msg = format!(
                        "fs_write: file not read yet — read it first before editing: {}",
                        args.path
                    );
                    emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                    return Ok(ToolOutcome::recoverable(msg));
                }
                let cur_hash = crate::file_ledger::fnv1a(&current_bytes);
                let cur_mtime = mtime_ms(&path);
                if cur_mtime > entry_mtime_ms && cur_hash != entry_content_hash {
                    let msg = format!(
                        "fs_write: file changed since last read — read it again: {}",
                        args.path
                    );
                    emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                    return Ok(ToolOutcome::recoverable(msg));
                }
                CheckpointTargetState::Existing(current_bytes)
            }
            CheckpointTargetState::Missing => CheckpointTargetState::Missing,
        };
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
                    "fs_write: cannot inspect {} identity before checkpoint: {error}",
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
        // 真正的写盘 IO 失败保持致命：std::fs::write 可能已截断或部分写入。
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, args.content.as_bytes())?;
        ctx.file_ledger
            .record(&path_key, &args.content, mtime_ms(&path), true);
        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "bytes": args.content.len() }),
        )?;
        ctx.recorder.emit(
            "artifact.created",
            json!({
                "artifact_id": format!("art_{}", call.id),
                "kind": "file",
                "path": path.to_string_lossy(),
                "title": args.path,
                "mime_type": "text/plain",
            }),
        )?;
        Ok(ToolOutcome::success_mutating(serde_json::to_string(
            &json!({ "path": path.to_string_lossy(), "bytes": args.content.len() }),
        )?))
    }
}

fn target_path(workspace: &Path, path: &str) -> PathBuf {
    if Path::new(path).is_absolute() {
        normalize_path(Path::new(path))
    } else {
        normalize_path(&workspace.join(path))
    }
}

/// 大文件整写硬拦截阈值：对齐 fs_read 的 64KiB（比一页 fs_read 还大就别整写）。
pub const WHOLE_WRITE_MAX_BYTES: u64 = 64 * 1024;

/// 纯函数：该不该拦下这次 fs_write 整写。命中（已存在文件、磁盘 > 阈值、edit_format=Targeted）→ Some(友好引导)；否则 None。
pub fn oversized_whole_write_reason(target: &Path, edit_format: EditFormat) -> Option<String> {
    if edit_format == EditFormat::WholeFileOk {
        return None;
    }
    let size = std::fs::metadata(target).ok()?.len();
    if size > WHOLE_WRITE_MAX_BYTES {
        Some(format!(
            "fs_write refused: '{}' is {} bytes; rewriting a file this large in one call exceeds the model output limit and gets truncated. Use fs_edit to change only the specific lines you need.",
            target.display(),
            size
        ))
    } else {
        None
    }
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

    #[test]
    fn oversized_reason_triggers_only_for_large_targeted_existing() {
        use crate::model_registry::EditFormat;
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.rs");
        std::fs::write(
            &big,
            vec![b'a'; (super::WHOLE_WRITE_MAX_BYTES + 1) as usize],
        )
        .unwrap();
        let small = dir.path().join("small.rs");
        std::fs::write(&small, b"hi").unwrap();
        let missing = dir.path().join("nope.rs");
        assert!(
            super::oversized_whole_write_reason(&big, EditFormat::Targeted)
                .unwrap()
                .contains("fs_edit")
        );
        assert!(super::oversized_whole_write_reason(&big, EditFormat::WholeFileOk).is_none());
        assert!(super::oversized_whole_write_reason(&small, EditFormat::Targeted).is_none());
        assert!(super::oversized_whole_write_reason(&missing, EditFormat::Targeted).is_none());
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "fs_write".into(),
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

    async fn run_write(
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
        FsWriteTool.execute(&mut ctx, &call(args)).await
    }

    async fn run_write_after_read(
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
        FsWriteTool.execute(&mut ctx, &call(args)).await
    }

    #[tokio::test]
    async fn write_new_file_needs_no_read() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let out = crate::tools::with_checkpoint_env_override_for_test(None, None, async {
            run_write(
                workspace.path(),
                &journal,
                json!({"path":"new.txt","content":"hello"}),
            )
            .await
        })
        .await
        .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("new.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn write_existing_without_read_is_recoverable() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let file = workspace.path().join("a.txt");
        std::fs::write(&file, "old").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;

        let out = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"a.txt","content":"new"}),
                )
                .await
            },
        )
        .await
        .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("read it first"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        server.verify().await;
    }

    #[tokio::test]
    async fn write_checkpoint_posts_before_creating_parent_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("nested/out.txt");
        let expected_path = resolve_in_workspace(workspace.path(), "nested/out.txt").unwrap();
        let parent = target.parent().unwrap().to_path_buf();
        let seen_unwritten = Arc::new(AtomicBool::new(false));
        let seen_unwritten_for_mock = seen_unwritten.clone();
        let parent_for_mock = parent.clone();
        let target_for_mock = target.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(move |_req: &wiremock::Request| {
                seen_unwritten_for_mock.store(
                    !parent_for_mock.exists() && !target_for_mock.exists(),
                    Ordering::SeqCst,
                );
                ResponseTemplate::new(204)
            })
            .expect(1)
            .mount(&server)
            .await;

        let out = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"nested/out.txt","content":"hello"}),
                )
                .await
            },
        )
        .await
        .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        assert!(seen_unwritten.load(Ordering::SeqCst));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
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
        assert_eq!(
            body,
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "fs_write",
                "tool_input": { "path": expected_path.to_string_lossy() },
            })
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn write_checkpoint_create_during_callback_is_fatal_and_preserves_concurrent_file() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("nested/out.txt");
        let target_for_mock = target.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(move |_req: &wiremock::Request| {
                if let Some(parent) = target_for_mock.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&target_for_mock, "external create during checkpoint\n").unwrap();
                ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(50))
            })
            .expect(1)
            .mount(&server)
            .await;

        let err = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"nested/out.txt","content":"engine content\n"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("created during checkpoint wait"));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "external create during checkpoint\n"
        );
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.completed\""));
        assert!(!events.contains("\"type\":\"artifact.created\""));
        server.verify().await;
    }

    #[tokio::test]
    async fn write_checkpoint_existing_file_change_during_callback_is_fatal_and_preserves_concurrent_content(
    ) {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("a.txt");
        std::fs::write(&target, "before checkpoint\n").unwrap();
        let target_for_mock = target.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(move |_req: &wiremock::Request| {
                std::fs::write(&target_for_mock, "external change during checkpoint\n").unwrap();
                ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(50))
            })
            .expect(1)
            .mount(&server)
            .await;

        let err = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write_after_read(
                    workspace.path(),
                    &journal,
                    "a.txt",
                    json!({"path":"a.txt","content":"engine replacement\n"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("content changed during checkpoint wait"));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "external change during checkpoint\n"
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
    async fn write_checkpoint_same_byte_inode_swap_during_callback_is_fatal_and_preserves_replacement(
    ) {
        use std::os::unix::fs::MetadataExt;

        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("a.txt");
        std::fs::write(&target, "before checkpoint\n").unwrap();
        let original_ino = std::fs::metadata(&target).unwrap().ino();
        let target_for_mock = target.clone();
        let replacement = workspace.path().join("replacement.txt");
        let replacement_for_mock = replacement.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(move |_req: &wiremock::Request| {
                std::fs::write(&replacement_for_mock, "before checkpoint\n").unwrap();
                std::fs::rename(&replacement_for_mock, &target_for_mock).unwrap();
                ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(50))
            })
            .expect(1)
            .mount(&server)
            .await;

        let err = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write_after_read(
                    workspace.path(),
                    &journal,
                    "a.txt",
                    json!({"path":"a.txt","content":"engine replacement\n"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("identity changed"));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "before checkpoint\n"
        );
        assert_ne!(std::fs::metadata(&target).unwrap().ino(), original_ino);
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
    async fn write_checkpoint_parent_symlink_escape_during_callback_is_fatal_and_creates_no_outside_file(
    ) {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("nested/out.txt");
        let parent = target.parent().unwrap().to_path_buf();
        let outside_target = outside.path().join("out.txt");
        let parent_for_mock = parent.clone();
        let outside_for_mock = outside.path().to_path_buf();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(move |_req: &wiremock::Request| {
                std::os::unix::fs::symlink(&outside_for_mock, &parent_for_mock).unwrap();
                ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(50))
            })
            .expect(1)
            .mount(&server)
            .await;

        let err = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"nested/out.txt","content":"engine content\n"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("outside the workspace"));
        assert!(std::fs::symlink_metadata(&parent)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!outside_target.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.completed\""));
        assert!(!events.contains("\"type\":\"artifact.created\""));
        server.verify().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_checkpoint_broken_leaf_symlink_during_callback_is_fatal_and_preserves_symlink() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("nested/out.txt");
        let target_for_mock = target.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(move |_req: &wiremock::Request| {
                if let Some(parent) = target_for_mock.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::os::unix::fs::symlink("missing-target.txt", &target_for_mock).unwrap();
                ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(50))
            })
            .expect(1)
            .mount(&server)
            .await;

        let err = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"nested/out.txt","content":"engine content\n"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("broken symlink"));
        assert!(std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_link(&target).unwrap(),
            std::path::PathBuf::from("missing-target.txt")
        );
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.completed\""));
        assert!(!events.contains("\"type\":\"artifact.created\""));
        server.verify().await;
    }

    #[tokio::test]
    async fn write_checkpoint_missing_token_is_fatal_and_writes_nothing() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("nested/out.txt");
        let parent = target.parent().unwrap();
        let server = MockServer::start().await;

        let err = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            None,
            async {
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"nested/out.txt","content":"hello"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(!parent.exists());
        assert!(!target.exists());
        assert!(err.to_string().contains("checkpoint"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn write_checkpoint_http_500_is_fatal_and_does_not_create_parent() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("nested/out.txt");
        let parent = target.parent().unwrap().to_path_buf();
        let seen_unwritten = Arc::new(AtomicBool::new(false));
        let seen_unwritten_for_mock = seen_unwritten.clone();
        let parent_for_mock = parent.clone();
        let target_for_mock = target.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/checkpoint"))
            .respond_with(move |_req: &wiremock::Request| {
                seen_unwritten_for_mock.store(
                    !parent_for_mock.exists() && !target_for_mock.exists(),
                    Ordering::SeqCst,
                );
                ResponseTemplate::new(500)
            })
            .expect(1)
            .mount(&server)
            .await;

        let err = crate::tools::with_checkpoint_env_override_for_test(
            Some(format!("{}/checkpoint", server.uri())),
            Some("secret-token".into()),
            async {
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"nested/out.txt","content":"hello"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(seen_unwritten.load(Ordering::SeqCst));
        assert!(!parent.exists());
        assert!(!target.exists());
        assert!(err.to_string().contains("checkpoint"));
        server.verify().await;
    }

    #[tokio::test]
    async fn write_checkpoint_connection_failure_is_fatal_and_writes_nothing() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let target = workspace.path().join("nested/out.txt");
        let parent = target.parent().unwrap();
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
                run_write(
                    workspace.path(),
                    &journal,
                    json!({"path":"nested/out.txt","content":"hello"}),
                )
                .await
            },
        )
        .await
        .unwrap_err();

        assert!(!parent.exists());
        assert!(!target.exists());
        assert!(err.to_string().contains("checkpoint"));
    }

    #[tokio::test]
    async fn tool_outcome_fs_write_success_invalidates_verification() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
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

        let out = FsWriteTool
            .execute(
                &mut ctx,
                &call(json!({"path":"nested/a.txt","content":"hello"})),
            )
            .await
            .unwrap();
        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        assert!(out.invalidates_verification);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["bytes"], 5);
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("nested/a.txt")).unwrap(),
            "hello"
        );
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"artifact.created\""));
    }

    #[tokio::test]
    async fn write_wrong_type_hints_type() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");

        let out = run_write(
            workspace.path(),
            &journal,
            json!({"path":"a.txt","content":123}),
        )
        .await
        .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("type"));
    }

    #[tokio::test]
    async fn outside_path_is_recoverable_and_not_written() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("escape.txt");
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let mut rec =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: &workspace,
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };

        let out = FsWriteTool
            .execute(
                &mut ctx,
                &call(json!({"path":"../escape.txt","content":"nope"})),
            )
            .await
            .unwrap();
        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("outside the workspace"));
        assert!(out.content.contains("../escape.txt"));
        assert!(!outside.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
    }
}
