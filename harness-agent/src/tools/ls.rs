use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::provider::ToolCall;
use crate::tools::fs_read::resolve_for_read;
use crate::tools::{
    emit_tool_completed, emit_tool_failed, emit_tool_started, Tool, ToolContext, ToolOutcome,
};

pub struct LsTool;

#[derive(Debug, Deserialize)]
struct LsArgs {
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "ls",
                "description": "List entries of a directory inside the workspace. Defaults to workspace root.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }
            }
        })
    }
    fn mutates(&self) -> bool {
        false
    }
    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let args: LsArgs = match serde_json::from_str(&call.function.arguments) {
            Ok(args) => args,
            Err(e) => {
                let msg = format!("bad arguments: {e}");
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let requested_path = args.path.as_deref().unwrap_or(".");
        let dir = match resolve_for_read(ctx.workspace, requested_path, ctx.fs_read_scope) {
            Ok(dir) => dir,
            Err(HarnessError::PermissionDenied(_)) => {
                let msg = format!(
                    "path is outside the workspace and was not accessed: {requested_path}. Use a relative path inside the workspace (e.g. \"src/foo.rs\"), not an absolute path."
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
            json!({ "path": dir.to_string_lossy() }),
        )?;
        let mut entries = Vec::new();
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(e) => {
                let msg = format!("ls: cannot read dir {}: {e}", dir.to_string_lossy());
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        for entry in read_dir {
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_dir = file_type.is_dir();
            entries.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "is_dir": is_dir,
            }));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "count": entries.len() }),
        )?;
        Ok(ToolOutcome::success(serde_json::to_string(
            &json!({ "path": dir.to_string_lossy(), "entries": entries }),
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventRecorder;
    use crate::provider::{FunctionCall, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "ls".into(),
                arguments: args.to_string(),
            },
        }
    }

    #[tokio::test]
    async fn lists_directory_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let mut rec =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: dir.path(),
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };
        let out = LsTool
            .execute(&mut ctx, &call(json!({"path":"."})))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["entries"][0]["name"], "a.txt");
        assert_eq!(v["entries"][0]["is_dir"], false);
        assert_eq!(v["entries"][1]["name"], "b.txt");
        assert_eq!(v["entries"][1]["is_dir"], false);
        assert_eq!(v["entries"][2]["name"], "sub");
        assert_eq!(v["entries"][2]["is_dir"], true);
    }

    #[tokio::test]
    async fn outside_path_is_recoverable_and_not_listed() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
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

        let out = LsTool
            .execute(&mut ctx, &call(json!({"path":"../outside"})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("outside the workspace"));
        assert!(out.content.contains("../outside"));
        assert!(!out.content.contains("secret.txt"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn tool_outcome_ls_missing_dir_is_recoverable_and_emits_failed() {
        let dir = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let mut rec =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: dir.path(),
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };

        let out = LsTool
            .execute(&mut ctx, &call(json!({"path":"missing"})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("ls: cannot read dir"));
        assert!(out.content.contains("missing"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"ls\""));
        assert!(events.contains("\"tool_call_id\":\"c\""));
    }

    #[tokio::test]
    async fn tool_outcome_ls_bad_args_is_recoverable_and_emits_failed() {
        let dir = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let mut rec =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: dir.path(),
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };

        let out = LsTool
            .execute(&mut ctx, &call(json!({"path": 42})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("bad arguments"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"ls\""));
        assert!(events.contains("\"tool_call_id\":\"c\""));
    }
}
