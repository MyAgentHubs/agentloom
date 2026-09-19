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
mod tests;
