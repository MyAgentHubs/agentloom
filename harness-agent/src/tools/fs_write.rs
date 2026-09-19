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
mod tests;
