use std::path::{Path, PathBuf};
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::exec::controlled::{
    controlled_exec, resolved_shell_dialect, ControlledExecOpts, ControlledExecOutcome,
    ShellDialect,
};
use crate::provider::ToolCall;
use crate::tools::{
    emit_tool_completed, emit_tool_failed, emit_tool_started, Tool, ToolContext, ToolOutcome,
};

#[derive(Debug, Clone, Copy)]
pub struct ShellExecToolImpl {
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
}

#[allow(non_upper_case_globals)]
pub const ShellExecTool: ShellExecToolImpl = ShellExecToolImpl {
    fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
};

impl ShellExecToolImpl {
    pub const fn with_write_fence(
        self,
        fs_write_fence: crate::exec::sandbox::FsWriteFence,
    ) -> Self {
        Self { fs_write_fence }
    }
}

fn shell_tool_definition_for_dialect(dialect: ShellDialect) -> Value {
    let mut definition = crate::provider::shell_tool_definition();
    let dialect_description = match dialect {
        ShellDialect::Posix => "Runtime shell: commands run via a POSIX shell (sh); use POSIX syntax.",
        ShellDialect::Cmd => "Runtime shell: commands run via Windows cmd.exe; use Windows command syntax (dir, del, && is supported); cmd variable expansion (%VAR%, %X, %%X, !VAR!) is rejected by the safety scanner; do NOT use POSIX-only constructs (~, $VAR, single quotes).",
    };
    let description = definition["function"]["description"]
        .as_str()
        .expect("shell tool definition has a description");
    definition["function"]["description"] =
        Value::String(format!("{description} {dialect_description}"));
    definition
}

fn shell_unavailable_message(error: &HarnessError) -> Option<&str> {
    match error {
        HarnessError::ShellUnavailable(message) => Some(message),
        _ => None,
    }
}

fn recover_shell_unavailable(
    recorder: &mut crate::events::EventRecorder,
    tool: &str,
    call_id: &str,
    error: &HarnessError,
) -> Result<Option<ToolOutcome>> {
    let Some(message) = shell_unavailable_message(error) else {
        return Ok(None);
    };
    let message = message.to_string();
    emit_tool_failed(recorder, tool, call_id, &message)?;
    Ok(Some(ToolOutcome::recoverable(message)))
}

#[derive(Debug, Deserialize)]
pub struct ShellExecRequest {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[async_trait]
impl Tool for ShellExecToolImpl {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn definition(&self) -> Value {
        shell_tool_definition_for_dialect(resolved_shell_dialect())
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let request: ShellExecRequest = match serde_json::from_str(&call.function.arguments) {
            Ok(request) => request,
            Err(e) => {
                let msg = format!("bad arguments: {e}");
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        ctx.workspace.canonicalize()?;
        let cwd = match resolve_cwd(ctx.workspace, request.cwd.as_ref()) {
            Ok(cwd) => cwd,
            Err(e) => {
                let recoverable_explicit_cwd = request.cwd.is_some()
                    && match &e {
                        HarnessError::Io(source)
                            if source.kind() == std::io::ErrorKind::NotFound =>
                        {
                            ctx.workspace.canonicalize()?;
                            true
                        }
                        HarnessError::Io(_) => false,
                        HarnessError::PermissionDenied(_) => true,
                        _ => false,
                    };
                if !recoverable_explicit_cwd {
                    return Err(e);
                }
                let requested_cwd = request.cwd.as_ref().expect("explicit cwd is present");
                let msg = format!(
                    "shell_exec cwd does not exist or is outside the workspace: {}. Use an existing directory inside the workspace (e.g. a relative path like \".\").",
                    requested_cwd.to_string_lossy()
                );
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        // 防手滑网（设计 §二·非沙箱）：挡便宜能认出的真 footgun。用 canonical workspace
        // 与 canonical cwd 同前缀比对（macOS tempdir /var→/private/var·否则会把工作区内相对路径误判为越界）。
        let ws_canon = ctx
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| ctx.workspace.to_path_buf());
        if let Some(reason) = crate::safety::dangerous_paths::dangerous_command_scan(
            &request.command,
            &cwd,
            &ws_canon,
            ctx.fs_read_scope,
        ) {
            emit_tool_failed(
                ctx.recorder,
                self.name(),
                &call.id,
                &format!("blocked: {}", reason.rule),
            )?;
            return Ok(ToolOutcome::rejected(serde_json::to_string(&json!({
                "error": "blocked: dangerous command",
                "rule": reason.rule,
                "detail": reason.detail,
            }))?));
        }
        let timeout_ms = request.timeout_ms.unwrap_or(120_000);
        emit_tool_started(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({
                "command": request.command,
                "cwd": cwd.to_string_lossy(),
                "timeout_ms": timeout_ms,
            }),
        )?;

        let started = Instant::now();
        let opts = ControlledExecOpts {
            command: request.command.clone(),
            workspace: ctx.workspace.to_path_buf(),
            cwd: cwd.clone(),
            timeout_ms: timeout_ms.max(1),
            output_cap_bytes: 64 * 1024,
            network: ctx.network,
            fs_write_fence: self.fs_write_fence,
        };
        let outcome = match controlled_exec(opts).await {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(outcome) =
                    recover_shell_unavailable(ctx.recorder, self.name(), &call.id, &error)?
                {
                    return Ok(outcome);
                }
                return Err(error);
            }
        };
        let (stdout, stderr, exit_code, timed_out, truncated) = match outcome {
            ControlledExecOutcome::Blocked { rule } => {
                // 防御纵深：正常路径已被 orchestrator pre-gate 拦住、这里兜底回灌。
                emit_tool_failed(
                    ctx.recorder,
                    self.name(),
                    &call.id,
                    &format!("blocked: escape attempt ({rule})"),
                )?;
                return Ok(ToolOutcome::rejected(serde_json::to_string(&json!({
                    "error": "blocked: escape attempt", "rule": rule
                }))?));
            }
            ControlledExecOutcome::NetworkUnenforceable { reason } => {
                emit_tool_failed(
                    ctx.recorder,
                    self.name(),
                    &call.id,
                    &format!("network off unenforceable: {reason}"),
                )?;
                return Ok(ToolOutcome::rejected(serde_json::to_string(&json!({
                    "error": "network off unenforceable", "reason": reason
                }))?));
            }
            ControlledExecOutcome::Ran {
                stdout,
                stderr,
                exit_code,
                timed_out,
                truncated,
            } => (stdout, stderr, exit_code, timed_out, truncated),
        };
        if timed_out {
            let timeout_s = timeout_ms.div_ceil(1000);
            let msg = format!("command timed out after {timeout_s}s");
            emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
            return Ok(ToolOutcome::recoverable(msg));
        }
        let duration_ms = started.elapsed().as_millis();
        if !stdout.is_empty() {
            ctx.recorder.emit(
                "tool.stdout.delta",
                json!({
                    "tool_call_id": call.id,
                    "tool": self.name(),
                    "text": stdout,
                }),
            )?;
        }
        if !stderr.is_empty() {
            ctx.recorder.emit(
                "tool.stderr.delta",
                json!({
                    "tool_call_id": call.id,
                    "tool": self.name(),
                    "text": stderr,
                }),
            )?;
        }

        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "truncated": truncated,
            }),
        )?;
        let exit_note = exit_code
            .and_then(|code| crate::safety::exit_semantics::exit_note(&request.command, code));
        Ok(ToolOutcome::success(serde_json::to_string(&json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "truncated": truncated,
            "exit_note": exit_note,
        }))?))
    }
}

/// shell 专用 cwd 解析（要求 cwd 实际存在并在 workspace 内）。
pub fn resolve_cwd(workspace: &Path, requested: Option<&PathBuf>) -> Result<PathBuf> {
    let workspace = workspace.canonicalize()?;
    let candidate = match requested {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => workspace.join(path),
        None => workspace.clone(),
    };
    let cwd = candidate.canonicalize()?;
    if !cwd.starts_with(&workspace) {
        return Err(HarnessError::PermissionDenied(format!(
            "cwd is outside workspace: {}",
            cwd.to_string_lossy()
        )));
    }
    Ok(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventRecorder;
    use crate::provider::{FunctionCall, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "shell_call".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "shell_exec".into(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn shell_tool_description_explains_posix_dialect() {
        let definition =
            shell_tool_definition_for_dialect(crate::exec::controlled::ShellDialect::Posix);
        let description = definition["function"]["description"].as_str().unwrap();
        assert!(description.contains("commands run via a POSIX shell (sh); use POSIX syntax"));
    }

    #[test]
    fn shell_tool_description_explains_cmd_dialect() {
        let definition =
            shell_tool_definition_for_dialect(crate::exec::controlled::ShellDialect::Cmd);
        let description = definition["function"]["description"].as_str().unwrap();
        assert!(description.contains(
            "commands run via Windows cmd.exe; use Windows command syntax (dir, del, && is supported)"
        ));
        assert!(description.contains(
            "cmd variable expansion (%VAR%, %X, %%X, !VAR!) is rejected by the safety scanner"
        ));
        assert!(description.contains("do NOT use POSIX-only constructs (~, $VAR, single quotes)"));
    }

    fn context<'a>(
        workspace: &'a std::path::Path,
        recorder: &'a mut EventRecorder,
        file_ledger: &'a mut crate::file_ledger::FileLedger,
    ) -> ToolContext<'a> {
        ToolContext {
            workspace,
            recorder,
            file_ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        }
    }

    #[tokio::test]
    async fn explicit_missing_cwd_is_recoverable_and_not_spawned() {
        let workspace = tempfile::tempdir().unwrap();
        let missing_cwd = "missing-cwd";
        let spawned = workspace.path().join("spawned");
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "touch spawned", "cwd": missing_cwd})),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains(missing_cwd));
        assert!(!spawned.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn explicit_outside_cwd_is_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let spawned = outside.join("spawned");
        let outside_arg = outside.to_string_lossy().into_owned();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(&workspace, &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "touch spawned", "cwd": outside_arg})),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains(&outside_arg));
        assert!(!spawned.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn default_cwd_resolution_failure_stays_fatal() {
        let root = tempfile::tempdir().unwrap();
        let missing_workspace = root.path().join("missing-workspace");
        let journal = root.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(&missing_workspace, &mut recorder, &mut ledger);

        let err = ShellExecTool
            .execute(&mut ctx, &call(json!({"command": "true"})))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HarnessError::Io(ref source) if source.kind() == std::io::ErrorKind::NotFound
        ));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn explicit_cwd_with_missing_workspace_stays_fatal() {
        let root = tempfile::tempdir().unwrap();
        let missing_workspace = root.path().join("missing-workspace");
        let spawned = root.path().join("spawned");
        let journal = root.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(&missing_workspace, &mut recorder, &mut ledger);

        let err = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "touch ../spawned", "cwd": "."})),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HarnessError::Io(ref source) if source.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(!spawned.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_timeout_is_recoverable_and_emits_failed() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "sleep 1", "timeout_ms": 1})),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("timed out"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"shell_exec\""));
        assert!(events.contains("\"tool_call_id\":\"shell_call\""));
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_bad_args_is_recoverable_and_emits_failed() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(&mut ctx, &call(json!({})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("bad arguments"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"shell_exec\""));
        assert!(events.contains("\"tool_call_id\":\"shell_call\""));
    }

    #[test]
    fn only_shell_unavailable_errors_are_recoverable_spawn_failures() {
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let message = "POSIX shell not found".to_string();
        let outcome = recover_shell_unavailable(
            &mut recorder,
            "shell_exec",
            "shell_call",
            &HarnessError::ShellUnavailable(message.clone()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.status, crate::tools::ToolStatus::FailedRecoverable);
        assert_eq!(outcome.content, message);
        assert!(!outcome.invalidates_verification);

        assert_eq!(
            recover_shell_unavailable(
                &mut recorder,
                "shell_exec",
                "shell_call",
                &HarnessError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)),
            )
            .unwrap(),
            None
        );
        assert_eq!(
            recover_shell_unavailable(
                &mut recorder,
                "shell_exec",
                "shell_call",
                &HarnessError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            )
            .unwrap(),
            None
        );
        let events = std::fs::read_to_string(&journal).unwrap();
        assert_eq!(events.matches("\"type\":\"tool.failed\"").count(), 1);
        assert!(events.contains("POSIX shell not found"));
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_blocks_dangerous_command() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(&mut ctx, &call(json!({"command": "rm -rf /etc"})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Rejected);
        assert!(out.content.contains("blocked"));
        assert!(out.content.contains("rm_system_path"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_allows_in_workspace_relative_write() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        // 工作区内相对重定向目标必须放行（守住 canonical workspace 修复·否则会被误判越界）
        let out = ShellExecTool
            .execute(&mut ctx, &call(json!({"command": "echo hi > out.txt"})))
            .await
            .unwrap();
        assert_eq!(out.status, crate::tools::ToolStatus::Success);
    }
}
