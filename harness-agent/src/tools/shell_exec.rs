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

/// 单流输出的 canonical/model 结果上限。捕获上限（output_cap_bytes = 64KB）不受影响：
/// tool.stdout.delta / tool.stderr.delta 事件、run journal、UI 终端显示仍拿全量；返回给模型
/// 的工具结果以及 canonical messages / conversation.json 拿剪过版本，resume 后仍是剪过版本且无回灌。
/// 这是为减轻 canonical 与 resume 负担的刻意取舍，代价是 canonical 历史不再全量；若将来增加
/// 读取 messages 做审计的路径，须知那里只有 16KB 视图。
/// 参照同仓 search 工具的 MAX_OUTPUT_BYTES（8KB），shell 放宽到 16KB。
const WIRE_OUTPUT_CAP_BYTES: usize = 16 * 1024;

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

/// 中段被省略时插的标记。
fn wire_elided_marker(bytes: usize) -> String {
    format!("\n[… {bytes} bytes elided from the middle to keep this result inside the model's context window; re-run the command with a narrower filter (for example, grep/head/tail) to retrieve the needed section …]\n")
}

/// 把超长输出剪成「头 + 省略标记 + 尾」。UTF-8 边界安全·确定性。
/// 保证：返回串字节数 <= max_bytes。返回 (剪后串, 是否真的剪了)。
fn cap_for_wire(s: &str, max_bytes: usize) -> (String, bool) {
    if s.len() <= max_bytes {
        return (s.to_string(), false);
    }

    let marker_len = wire_elided_marker(s.len()).len();
    let body_budget = max_bytes.saturating_sub(marker_len);
    if body_budget == 0 {
        let marker = wire_elided_marker(s.len());
        let mut marker_end = max_bytes.min(marker.len());
        while !marker.is_char_boundary(marker_end) {
            marker_end -= 1;
        }
        return (marker[..marker_end].to_string(), true);
    }

    let head_bytes = body_budget * 3 / 5;
    let tail_bytes = body_budget - head_bytes;
    let mut head_end = head_bytes;
    while !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - tail_bytes;
    while !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if head_end >= tail_start {
        return (s.to_string(), false);
    }

    let elided = tail_start - head_end;
    let marker = wire_elided_marker(elided);
    (
        format!("{}{}{}", &s[..head_end], marker, &s[tail_start..]),
        true,
    )
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
        let (wire_stdout, stdout_elided) = cap_for_wire(&stdout, WIRE_OUTPUT_CAP_BYTES);
        let (wire_stderr, stderr_elided) = cap_for_wire(&stderr, WIRE_OUTPUT_CAP_BYTES);
        let exit_note = exit_code
            .and_then(|code| crate::safety::exit_semantics::exit_note(&request.command, code));
        Ok(ToolOutcome::success(serde_json::to_string(&json!({
            "stdout": wire_stdout,
            "stderr": wire_stderr,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "truncated": truncated,
            "wire_truncated": stdout_elided || stderr_elided,
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
    fn cap_for_wire_leaves_small_output_untouched() {
        let input = "short output\n";
        let (out, truncated) = cap_for_wire(input, WIRE_OUTPUT_CAP_BYTES);

        assert_eq!(out.as_bytes(), input.as_bytes());
        assert!(!truncated);
    }

    #[test]
    fn cap_for_wire_keeps_head_and_tail_and_marks_middle() {
        let input = "A".repeat(8_000) + &"B".repeat(8_000) + &"C".repeat(8_000);
        let (out, truncated) = cap_for_wire(&input, WIRE_OUTPUT_CAP_BYTES);

        assert!(out.len() <= WIRE_OUTPUT_CAP_BYTES);
        assert!(truncated);
        assert!(out.starts_with('A'));
        assert!(out.ends_with('C'));
        assert!(out.contains("bytes elided from the middle"));
    }

    #[test]
    fn cap_for_wire_is_utf8_safe() {
        let input = "中文测试abc".repeat(3_000);

        for max_bytes in [WIRE_OUTPUT_CAP_BYTES, 512] {
            let (out, truncated) = cap_for_wire(&input, max_bytes);
            assert!(truncated);
            assert!(out.len() <= max_bytes);
            assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        }
    }

    #[test]
    fn cap_for_wire_is_deterministic() {
        let input = "deterministic-output-中文".repeat(2_000);

        assert_eq!(
            cap_for_wire(&input, WIRE_OUTPUT_CAP_BYTES),
            cap_for_wire(&input, WIRE_OUTPUT_CAP_BYTES)
        );
    }

    #[test]
    fn cap_for_wire_边界() {
        let input = "中文测试abc".repeat(100);
        let (out, truncated) = cap_for_wire(&input, 32);

        assert!(truncated);
        assert!(out.len() <= 32);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn shell_exec_emits_full_stdout_but_caps_wire_output() {
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
                &call(json!({
                    "command": "awk 'BEGIN { for (i = 0; i < 20000; i++) printf \"A\" }'"
                })),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        let wire: Value = serde_json::from_str(&out.content).unwrap();
        assert!(wire["stdout"].as_str().unwrap().len() <= WIRE_OUTPUT_CAP_BYTES);
        assert_eq!(wire["wire_truncated"], true);

        let events: Vec<Value> = std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let stdout_event = events
            .iter()
            .find(|event| event["type"] == "tool.stdout.delta")
            .expect("tool.stdout.delta event");
        assert!(stdout_event["payload"]["text"].as_str().unwrap().len() > WIRE_OUTPUT_CAP_BYTES);
    }

    #[tokio::test]
    async fn shell_exec_caps_stderr_into_the_wire_too() {
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
                &call(json!({
                    "command": "awk 'BEGIN { for (i = 0; i < 20000; i++) printf \"E\" }' >&2"
                })),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        let wire: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(wire["stdout"], "");
        let wire_stderr = wire["stderr"].as_str().unwrap();
        assert!(wire_stderr.len() <= WIRE_OUTPUT_CAP_BYTES);
        assert!(wire_stderr.contains("bytes elided from the middle"));
        assert_eq!(wire["wire_truncated"], true);

        let events: Vec<Value> = std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let stderr_event = events
            .iter()
            .find(|event| event["type"] == "tool.stderr.delta")
            .expect("tool.stderr.delta event");
        assert!(stderr_event["payload"]["text"].as_str().unwrap().len() > WIRE_OUTPUT_CAP_BYTES);
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
