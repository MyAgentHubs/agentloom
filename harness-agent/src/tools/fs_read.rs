use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::provider::ToolCall;
use crate::tools::{
    emit_tool_completed, emit_tool_failed, emit_tool_started, Tool, ToolContext, ToolOutcome,
};

pub struct FsReadTool;

#[derive(Debug, Deserialize)]
struct FsReadArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>, // 1-based, inclusive
    #[serde(default)]
    end_line: Option<usize>, // 1-based, inclusive
}

#[async_trait]
impl Tool for FsReadTool {
    fn name(&self) -> &str {
        "fs_read"
    }
    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "fs_read",
                "description": "Read a UTF-8 text file inside the workspace. Optional 1-based start_line/end_line slice.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to workspace (or absolute inside workspace)." },
                        "start_line": { "type": "integer" },
                        "end_line": { "type": "integer" }
                    },
                    "required": ["path"]
                }
            }
        })
    }
    fn mutates(&self) -> bool {
        false
    }
    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let args: FsReadArgs = match serde_json::from_str(&call.function.arguments) {
            Ok(args) => args,
            Err(e) => {
                let msg = format!("bad arguments: {e}");
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let path = match resolve_for_read(ctx.workspace, &args.path, ctx.fs_read_scope) {
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
                    "fs_read: cannot read {}: {e}. {}",
                    args.path,
                    suggest_for_missing(ctx.workspace, &args.path)
                );
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let sliced = slice_lines(&content, args.start_line, args.end_line);
        let (content_out, truncated, total_bytes) = cap_read_output(&sliced);
        let no_ranges = args.start_line.is_none() && args.end_line.is_none();
        ctx.file_ledger.record(
            &path.to_string_lossy(),
            &content,
            mtime_ms(&path),
            no_ranges && !truncated,
        );
        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "bytes": content_out.len(), "truncated": truncated }),
        )?;
        Ok(ToolOutcome::success(serde_json::to_string(&json!({
            "path": path.to_string_lossy(),
            "content": content_out,
            "truncated": truncated,
            "total_bytes": total_bytes,
        }))?))
    }
}

fn slice_lines(content: &str, start: Option<usize>, end: Option<usize>) -> String {
    if start.is_none() && end.is_none() {
        return content.to_string();
    }
    let lines: Vec<&str> = content.lines().collect();
    let s = start.unwrap_or(1).max(1);
    let e = end.unwrap_or(lines.len()).min(lines.len());
    if s > e {
        return String::new();
    }
    lines[(s - 1)..e].join("\n")
}

/// fs_read 单次输出上限（对齐 shell_exec 的 output_cap_bytes·堵「一次读爆」）。
const FS_READ_CAP_BYTES: usize = 64 * 1024;

/// 超上限就留头（UTF-8 char 边界安全）+ 追加可取回标记；返回 (内容, 是否截断, 原始字节数)。
fn cap_read_output(s: &str) -> (String, bool, usize) {
    let total = s.len();
    if total <= FS_READ_CAP_BYTES {
        return (s.to_string(), false, total);
    }
    let mut end = FS_READ_CAP_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str(&format!(
        "\n\n[fs_read: this file/slice is {total} bytes; showing the first {end} bytes. Read the rest with start_line/end_line, or grep for what you need.]"
    ));
    (out, true, total)
}

pub(crate) fn suggest_for_missing(workspace: &Path, requested: &str) -> String {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let cwd = workspace.to_string_lossy();
    let Some(basename) = Path::new(requested).file_name() else {
        return format!("Working directory: {cwd}.");
    };

    if let Some(found) = find_same_basename(&workspace, basename, 0) {
        let rel = found.strip_prefix(&workspace).unwrap_or(&found);
        let rel = rel.to_string_lossy().replace('\\', "/");
        return format!("Working directory: {cwd}. Did you mean {rel}?");
    }

    format!("Working directory: {cwd}.")
}

fn find_same_basename(dir: &Path, basename: &std::ffi::OsStr, depth: usize) -> Option<PathBuf> {
    if depth > 8 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file() && path.file_name() == Some(basename) {
            return Some(path);
        }
        if file_type.is_dir() && depth < 8 {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || name == "target" || name == "node_modules" || name.starts_with('.')
            {
                continue;
            }
            if let Some(found) = find_same_basename(&path, basename, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

/// 解析 path 到 workspace 内的绝对路径，越界报错。父目录可不存在（读会自然失败）。
pub fn resolve_in_workspace(workspace: &Path, path: &str) -> Result<PathBuf> {
    let workspace = workspace.canonicalize()?;
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    };
    let resolved = canonicalize_lenient(&candidate);
    if !resolved.starts_with(&workspace) {
        return Err(HarnessError::PermissionDenied(format!(
            "path is outside workspace: {}",
            resolved.to_string_lossy()
        )));
    }
    Ok(resolved)
}

/// Resolve a read path under the selected scope without changing the historical
/// resolver shared by fs_write/fs_edit.
pub fn resolve_for_read(
    workspace: &Path,
    path: &str,
    scope: crate::fs_scope::FsReadScope,
) -> Result<PathBuf> {
    let roots = match scope {
        crate::fs_scope::FsReadScope::ProjectDeps => crate::fs_scope::project_dependency_roots(),
        crate::fs_scope::FsReadScope::Workspace | crate::fs_scope::FsReadScope::Wide => &[],
    };
    resolve_for_read_with_roots(workspace, path, scope, roots)
}

pub(crate) fn resolve_for_read_with_roots(
    workspace: &Path,
    path: &str,
    scope: crate::fs_scope::FsReadScope,
    roots: &[PathBuf],
) -> Result<PathBuf> {
    if scope == crate::fs_scope::FsReadScope::Workspace {
        return resolve_in_workspace(workspace, path);
    }

    let workspace = workspace.canonicalize()?;
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    };
    let resolved = canonicalize_lenient(&candidate);
    if !crate::fs_scope::read_path_allowed_with_roots(&workspace, &candidate, scope, roots) {
        return Err(HarnessError::PermissionDenied(format!(
            "path is outside workspace: {}",
            resolved.to_string_lossy()
        )));
    }
    Ok(resolved)
}

fn mtime_ms(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Canonicalize the longest existing prefix of `path`, then re-append the
/// non-existent tail. Lets boundary checks resolve symlinked workspace roots
/// (macOS /var -> /private/var) even for files that don't exist yet.
pub(crate) fn canonicalize_lenient(path: &Path) -> std::path::PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonicalize_lenient(parent).join(name),
        _ => normalize_path(path),
    }
}

/// 纯词法规范化（解析 . 和 ..，不触盘），用于越界判断对不存在路径也成立。
/// `pub(crate)`：B4 Guardrails::ensure_in_workspace 与 B6 fs_write/fs_edit 复用。
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_read_resolution_is_byte_for_byte_compatible() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("inside.txt"), "ok").unwrap();
        std::fs::write(workspace.join(".env"), "still readable").unwrap();

        for input in [
            "inside.txt",
            ".env",
            "missing.txt",
            "../outside.txt",
            "/etc/passwd",
        ] {
            let old = resolve_in_workspace(&workspace, input);
            let new = resolve_for_read(&workspace, input, crate::fs_scope::FsReadScope::Workspace);
            match (old, new) {
                (Ok(a), Ok(b)) => assert_eq!(a, b),
                (Err(a), Err(b)) => assert_eq!(format!("{a:?}"), format!("{b:?}")),
                pair => panic!("resolution mismatch for {input}: {pair:?}"),
            }
        }
    }

    #[tokio::test]
    async fn expanded_reads_never_expand_fs_write_boundary() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let venv = root.path().join("venv");
        let python = venv.join("bin/python3");
        let dependency = venv.join("lib/site-packages/foo.py");
        let random_outside = root.path().join("random.txt");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::create_dir_all(dependency.parent().unwrap()).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr\n").unwrap();
        std::fs::write(&python, "").unwrap();
        std::fs::write(&dependency, "original\n").unwrap();
        std::fs::write(&random_outside, "random\n").unwrap();

        let test_path = std::env::join_paths([python.parent().unwrap()]).unwrap();
        let roots =
            crate::fs_scope::discover_project_dependency_roots(Some(&test_path), None, None, &[]);
        assert_eq!(
            resolve_for_read_with_roots(
                &workspace,
                dependency.to_str().unwrap(),
                crate::fs_scope::FsReadScope::ProjectDeps,
                &roots,
            )
            .unwrap(),
            dependency.canonicalize().unwrap()
        );
        assert!(resolve_for_read_with_roots(
            &workspace,
            random_outside.to_str().unwrap(),
            crate::fs_scope::FsReadScope::ProjectDeps,
            &roots,
        )
        .is_err());
        assert!(resolve_for_read_with_roots(
            &workspace,
            "/etc/passwd",
            crate::fs_scope::FsReadScope::Wide,
            &roots,
        )
        .is_ok());

        for scope in [
            crate::fs_scope::FsReadScope::Workspace,
            crate::fs_scope::FsReadScope::ProjectDeps,
            crate::fs_scope::FsReadScope::Wide,
        ] {
            let journal = root.path().join(format!("events-{scope:?}.jsonl"));
            let mut recorder =
                EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                    .unwrap();
            let mut ledger = crate::file_ledger::FileLedger::new();
            let mut ctx = ToolContext {
                workspace: &workspace,
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: crate::goal::NetworkPolicy::On,
                fs_read_scope: scope,
            };
            let outcome = crate::tools::fs_write::FsWriteTool
                .execute(
                    &mut ctx,
                    &call(json!({
                        "path": dependency.to_string_lossy(),
                        "content": "changed\n"
                    })),
                )
                .await
                .unwrap();
            assert_eq!(
                outcome.status,
                crate::tools::ToolStatus::FailedRecoverable,
                "fs_write escaped under {scope:?}"
            );
            assert_eq!(std::fs::read_to_string(&dependency).unwrap(), "original\n");
        }
    }
    use crate::events::EventRecorder;
    use crate::provider::{FunctionCall, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "fs_read".into(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn cap_read_output_passes_small_unchanged() {
        let (out, truncated, total) = cap_read_output("hello");
        assert_eq!(out, "hello");
        assert!(!truncated);
        assert_eq!(total, 5);
    }

    #[test]
    fn cap_read_output_truncates_large_with_marker_on_char_boundary() {
        let big = "a".repeat(FS_READ_CAP_BYTES + 100);
        let (out, truncated, total) = cap_read_output(&big);
        assert!(truncated);
        assert_eq!(total, FS_READ_CAP_BYTES + 100);
        // 留头 <= cap + 追加标记
        assert!(out.starts_with(&"a".repeat(100)));
        assert!(out.contains("start_line/end_line"));
        assert!(out.contains("showing the first"));
        // 截断点之前是合法 UTF-8（这里全 ascii·必然）
        assert!(out.is_char_boundary(0));
    }

    #[tokio::test]
    async fn fs_read_large_file_is_capped_but_ledger_keeps_full_hash() {
        let dir = tempfile::tempdir().unwrap();
        let big = "z".repeat(FS_READ_CAP_BYTES + 500);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        let journal = dir.path().join("e.jsonl");
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

        let out = FsReadTool
            .execute(&mut ctx, &call(json!({ "path": "big.txt" })))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["truncated"], true);
        assert_eq!(v["total_bytes"], (FS_READ_CAP_BYTES + 500) as i64);
        let content = v["content"].as_str().unwrap();
        assert!(content.len() < FS_READ_CAP_BYTES + 500); // 被截
        assert!(content.contains("start_line/end_line"));
        // ledger 仍记全文 hash；但截断读不再算全文已读，避免绕过先读后改。
        let key = resolve_in_workspace(dir.path(), "big.txt").unwrap();
        let entry = ledger.get(&key.to_string_lossy()).unwrap();
        assert_eq!(
            entry.content_hash,
            crate::file_ledger::fnv1a(big.as_bytes())
        );
        assert!(!entry.full_read);
    }

    #[tokio::test]
    async fn fs_read_truncated_marks_not_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let big = "z".repeat(FS_READ_CAP_BYTES + 500);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        std::fs::write(dir.path().join("small.txt"), "small\n").unwrap();
        let journal = dir.path().join("e.jsonl");
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

        let out = FsReadTool
            .execute(&mut ctx, &call(json!({ "path": "big.txt" })))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["truncated"], true);
        let key = resolve_in_workspace(dir.path(), "big.txt").unwrap();
        let entry = ctx.file_ledger.get(&key.to_string_lossy()).unwrap();
        assert!(!entry.full_read);

        let out = FsReadTool
            .execute(&mut ctx, &call(json!({ "path": "small.txt" })))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["truncated"], false);
        let key = resolve_in_workspace(dir.path(), "small.txt").unwrap();
        let entry = ctx.file_ledger.get(&key.to_string_lossy()).unwrap();
        assert!(entry.full_read);
    }

    #[tokio::test]
    async fn tool_outcome_fs_read_success_is_non_mutating() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "l1\nl2\nl3\nl4\n").unwrap();
        let journal = dir.path().join("e.jsonl");
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
        let out = FsReadTool
            .execute(
                &mut ctx,
                &call(json!({"path":"a.txt","start_line":2,"end_line":3})),
            )
            .await
            .unwrap();
        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        assert!(!out.invalidates_verification);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["content"], "l2\nl3");
    }

    #[tokio::test]
    async fn fs_read_records_into_ledger() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\n").unwrap();
        let journal = dir.path().join("e.jsonl");
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

        FsReadTool
            .execute(&mut ctx, &call(json!({"path":"a.txt"})))
            .await
            .unwrap();

        let key = resolve_in_workspace(dir.path(), "a.txt").unwrap();
        let entry = ledger.get(&key.to_string_lossy()).unwrap();
        assert!(entry.full_read);
    }

    #[tokio::test]
    async fn fs_read_partial_marks_not_full() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\n").unwrap();
        let journal = dir.path().join("e.jsonl");
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

        FsReadTool
            .execute(
                &mut ctx,
                &call(json!({"path":"a.txt","start_line":1,"end_line":1})),
            )
            .await
            .unwrap();

        let key = resolve_in_workspace(dir.path(), "a.txt").unwrap();
        let entry = ledger.get(&key.to_string_lossy()).unwrap();
        assert!(!entry.full_read);
    }

    #[tokio::test]
    async fn tool_outcome_fs_read_missing_file_is_recoverable_and_emits_failed() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
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

        let out = FsReadTool
            .execute(&mut ctx, &call(json!({"path":"missing.txt"})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("cannot read"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
    }

    #[tokio::test]
    async fn read_missing_suggests_same_name_under_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("harness-agent/src/tools/mod.rs");
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, "pub mod fs_read;\n").unwrap();
        let journal = dir.path().join("e.jsonl");
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

        let out = FsReadTool
            .execute(&mut ctx, &call(json!({"path":"src/tools/mod.rs"})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("Did you mean"));
        assert!(out.content.contains("harness-agent/src/tools/mod.rs"));
    }

    #[tokio::test]
    async fn read_missing_no_match_still_gives_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
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

        let out = FsReadTool
            .execute(&mut ctx, &call(json!({"path":"nope.xyz"})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("Working directory"));
        assert!(!out.content.contains("Did you mean"));
    }

    #[tokio::test]
    async fn tool_outcome_fs_read_bad_args_is_recoverable_and_emits_failed() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
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

        let out = FsReadTool
            .execute(&mut ctx, &call(json!({"start_line":1})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("bad arguments"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
    }

    #[tokio::test]
    async fn outside_path_is_recoverable_and_not_read() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("escape.txt");
        std::fs::write(&outside, "secret").unwrap();
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
        let out = FsReadTool
            .execute(&mut ctx, &call(json!({"path":"../escape.txt"})))
            .await
            .unwrap();
        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("outside the workspace"));
        assert!(out.content.contains("../escape.txt"));
        assert!(!out.content.contains("secret"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[cfg(unix)]
    #[test]
    fn resolves_through_symlinked_workspace() {
        let real = tempfile::tempdir().unwrap();
        let link_parent = tempfile::tempdir().unwrap();
        let link = link_parent.path().join("workspace-link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let resolved = resolve_in_workspace(&link, "demo.txt").unwrap();
        assert!(resolved.starts_with(real.path().canonicalize().unwrap()));

        let err = resolve_in_workspace(&link, "../escape.txt");
        assert!(matches!(
            err,
            Err(crate::error::HarnessError::PermissionDenied(_))
        ));
    }
}
