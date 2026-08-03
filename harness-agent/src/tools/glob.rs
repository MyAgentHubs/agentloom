use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::Result;
use crate::provider::ToolCall;
use crate::tools::{
    emit_tool_completed, emit_tool_failed, emit_tool_started, Tool, ToolContext, ToolOutcome,
};

pub struct GlobTool;

const MIN_RESULTS: usize = 1;
const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_RESULTS_CAP: usize = 500;

#[derive(Debug, Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default = "default_max")]
    max_results: usize,
}

fn default_max() -> usize {
    DEFAULT_MAX_RESULTS
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files in the workspace matching a glob pattern (*, **, ?). Returns relative paths.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum matches to return (default: 100; hard limit: 500). Values above 500 are clamped. If results are truncated, narrow the pattern instead of increasing this value."
                        }
                    },
                    "required": ["pattern"]
                }
            }
        })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let args: GlobArgs = match serde_json::from_str(&call.function.arguments) {
            Ok(args) => args,
            Err(e) => {
                let msg = format!("bad arguments: {e}");
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
        };
        let max_results = args.max_results.clamp(MIN_RESULTS, MAX_RESULTS_CAP);
        emit_tool_started(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "pattern": args.pattern }),
        )?;
        let pattern = match normalize_path_filter(ctx.workspace, &args.pattern) {
            Ok(p) => p,
            Err(diag) => {
                emit_tool_completed(
                    ctx.recorder,
                    self.name(),
                    &call.id,
                    json!({ "count": 0, "truncated": false }),
                )?;
                return Ok(ToolOutcome::success(serde_json::to_string(
                    &json!({ "matches": [], "truncated": false, "note": diag }),
                )?));
            }
        };
        let mut matches = Vec::new();
        walk(ctx.workspace, ctx.workspace, &mut matches)?;
        let mut hits: Vec<String> = matches
            .into_iter()
            .filter(|rel| glob_match(&pattern, rel))
            .collect();
        hits.sort();
        let truncated = hits.len() > max_results;
        if truncated {
            hits.truncate(max_results);
        }
        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "count": hits.len(), "truncated": truncated }),
        )?;
        let note = if truncated {
            Some(format!(
                "[glob: too many matches; showing only the first {} and more exist. Narrow the pattern with a specific subdirectory or file extension instead of a broad **. Do not rerun the same query unchanged.]",
                hits.len()
            ))
        } else {
            None
        };
        let mut response = json!({ "matches": hits, "truncated": truncated });
        if let Some(note) = note {
            response["note"] = Value::String(note);
        }
        Ok(ToolOutcome::success(serde_json::to_string(&response)?))
    }
}

/// 目录名黑名单：vendor 依赖 / 构建产物几乎不可能是 glob·grep 要找的源码目标——
/// 真要精确定位这些目录下的东西，调用方可以传绝对/精确路径绕过 walk。排除它们
/// 是为了避免单次 walk 把海量无关文件（如 node_modules 里几千个包）灌进结果，
/// 挤爆结果上限或打爆下游 agent 的上下文预算。不在此处做 .gitignore 尊重——
/// 那是行为面更大的决策，留给 engine 线后续单独设计（backlog）。
const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".myagenthubs",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "__pycache__",
];

/// 收集 root 下所有文件的相对路径（POSIX `/`），跳过 EXCLUDED_DIRS。
pub(crate) fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if EXCLUDED_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// 经典 glob：`?`/`*` 不跨 `/`，`**` 跨 `/`。带回溯。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // 把 "**" 标记为可跨目录通配。实现：递归。
    glob_rec(pattern.as_bytes(), text.as_bytes())
}

/// 不依赖宿主 OS 的「字面绝对路径」判别（codex F5）：`Path::is_absolute` 在 Unix 把
/// `C:/ws/...` 当相对·会让 Windows 风格绝对过滤器漏过归一、又静默 0。这里同时认
/// POSIX 绝对（`/...`、`//...`）和盘符绝对（`^[A-Za-z]:/`）。输入已把 `\` 归一成 `/`。
fn is_absolute_lexical(p: &str) -> bool {
    let b = p.as_bytes();
    b.first() == Some(&b'/')
        || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/')
}

/// workspace 的可比对根（raw + canonical/lenient·都归一成以 '/' 结尾的字符串·去重）。
fn workspace_roots(workspace: &Path) -> Vec<String> {
    let raw = workspace.to_string_lossy().replace('\\', "/");
    let canon = crate::tools::fs_read::canonicalize_lenient(workspace)
        .to_string_lossy()
        .replace('\\', "/");
    let mut roots = Vec::new();
    for r in [raw, canon] {
        let with_slash = if r.ends_with('/') { r } else { format!("{r}/") };
        if !roots.contains(&with_slash) {
            roots.push(with_slash);
        }
    }
    roots
}

/// 把可能是绝对的 grep/glob 路径过滤器归一成 workspace 相对 glob（C1）。
/// 相对→原样；绝对在 workspace 下（比对 raw + canonical 两根）→去根转相对（glob 尾保留）；
/// 绝对在 workspace 外→Err(可恢复诊断)·调用方回 matches:[] + note·绝不静默 0。
pub(crate) fn normalize_path_filter(
    workspace: &Path,
    filter: &str,
) -> std::result::Result<String, String> {
    let normalized = filter.replace('\\', "/");
    if !is_absolute_lexical(&normalized) {
        return Ok(normalized);
    }
    for root in workspace_roots(workspace) {
        if let Some(rel) = normalized.strip_prefix(&root) {
            let rel = rel.trim_start_matches('/');
            return Ok(if rel.is_empty() {
                "**".to_string()
            } else {
                rel.to_string()
            });
        }
        if normalized == root.trim_end_matches('/') {
            return Ok("**".to_string());
        }
    }
    Err(format!(
        "path filter `{filter}` is an absolute path outside the workspace root `{}`. \
         Omit the filter to search the whole workspace, or pass a workspace-relative path.",
        workspace.display()
    ))
}

fn glob_rec(p: &[u8], t: &[u8]) -> bool {
    // 处理 ** （跨目录）
    if p.starts_with(b"**") {
        let mut rest = &p[2..];
        if rest.first() == Some(&b'/') {
            rest = &rest[1..];
        }
        // ** 匹配 0..=len 任意前缀（含 /）
        if glob_rec(rest, t) {
            return true;
        }
        for i in 0..t.len() {
            if glob_rec(rest, &t[i + 1..]) {
                return true;
            }
        }
        return rest.is_empty(); // "**" 末尾匹配所有
    }
    match (p.first(), t.first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(b'*'), _) => {
            // * 不跨 /
            if glob_rec(&p[1..], t) {
                return true;
            }
            let mut i = 0;
            while i < t.len() && t[i] != b'/' {
                i += 1;
                if glob_rec(&p[1..], &t[i..]) {
                    return true;
                }
            }
            false
        }
        (Some(b'?'), Some(&c)) if c != b'/' => glob_rec(&p[1..], &t[1..]),
        (Some(&pc), Some(&tc)) if pc == tc => glob_rec(&p[1..], &t[1..]),
        _ => false,
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
                name: "glob".into(),
                arguments: args.to_string(),
            },
        }
    }

    async fn run_glob(workspace: &std::path::Path, args: serde_json::Value) -> serde_json::Value {
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("e.jsonl");
        let mut rec =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace,
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };
        let out = GlobTool.execute(&mut ctx, &call(args)).await.unwrap();
        serde_json::from_str(&out.content).unwrap()
    }

    #[test]
    fn glob_match_basics() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs")); // * 不跨 /
        assert!(glob_match("**/*.rs", "src/a/main.rs")); // ** 跨 /
        assert!(glob_match("src/**", "src/a/b.txt"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn normalize_relative_filter_unchanged() {
        let ws = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize_path_filter(ws.path(), "src/**/*.rs").unwrap(),
            "src/**/*.rs"
        );
    }

    #[test]
    fn normalize_absolute_under_raw_root_strips_to_relative() {
        let ws = tempfile::tempdir().unwrap();
        let abs = format!("{}/harness-agent/src/*.rs", ws.path().to_string_lossy());
        assert_eq!(
            normalize_path_filter(ws.path(), &abs).unwrap(),
            "harness-agent/src/*.rs"
        );
    }

    #[test]
    fn normalize_absolute_under_canonical_root_strips_to_relative() {
        // 用 canonical 形态拼绝对路径（macOS /tmp↔/private/tmp 错位的等价模拟）
        let ws = tempfile::tempdir().unwrap();
        let canon = crate::tools::fs_read::canonicalize_lenient(ws.path());
        let abs = format!("{}/src/main.rs", canon.to_string_lossy());
        assert_eq!(
            normalize_path_filter(ws.path(), &abs).unwrap(),
            "src/main.rs"
        );
    }

    #[test]
    fn normalize_absolute_outside_workspace_returns_diagnostic() {
        let ws = tempfile::tempdir().unwrap();
        let err = normalize_path_filter(ws.path(), "/etc/passwd").unwrap_err();
        assert!(err.contains("outside the workspace"), "got: {err}");
        assert!(
            err.contains(&ws.path().to_string_lossy().to_string()),
            "got: {err}"
        );
    }

    #[test]
    fn normalize_windows_style_absolute_is_diagnostic_not_silent_relative() {
        // codex F5：Unix 上 Path::is_absolute("C:/...")=false·必须仍当绝对·回诊断、别静默放过。
        let ws = tempfile::tempdir().unwrap();
        let err = normalize_path_filter(ws.path(), r"C:\ws\src\*.rs").unwrap_err();
        assert!(err.contains("outside the workspace"), "got: {err}");
    }

    #[tokio::test]
    async fn tool_outcome_glob_success_is_non_mutating() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(workspace.path().join("b.txt"), "text\n").unwrap();

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

        let out = GlobTool
            .execute(&mut ctx, &call(json!({"pattern":"**/*.rs"})))
            .await
            .unwrap();
        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        assert!(!out.invalidates_verification);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["matches"], json!(["src/a.rs"]));
    }

    #[tokio::test]
    async fn tool_outcome_glob_bad_args_is_recoverable_and_emits_failed() {
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

        let out = GlobTool.execute(&mut ctx, &call(json!({}))).await.unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("bad arguments"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"glob\""));
        assert!(events.contains("\"tool_call_id\":\"c\""));
    }

    #[tokio::test]
    async fn tool_outcome_glob_default_truncates_at_one_hundred() {
        let workspace = tempfile::tempdir().unwrap();
        for i in 0..150 {
            std::fs::write(workspace.path().join(format!("f{i:04}.txt")), "x\n").unwrap();
        }

        let v = run_glob(workspace.path(), json!({"pattern":"*.txt"})).await;

        assert_eq!(v["matches"].as_array().unwrap().len(), 100);
        assert_eq!(v["truncated"], true);
        let note = v["note"].as_str().unwrap();
        assert!(note.contains("Narrow the pattern"));
        assert!(note.contains("same query"));
    }

    #[tokio::test]
    async fn tool_outcome_glob_max_results_is_capped_at_five_hundred() {
        let workspace = tempfile::tempdir().unwrap();
        for i in 0..600 {
            std::fs::write(workspace.path().join(format!("f{i:04}.txt")), "x\n").unwrap();
        }

        let v = run_glob(
            workspace.path(),
            json!({"pattern":"*.txt","max_results":100000}),
        )
        .await;

        assert_eq!(v["matches"].as_array().unwrap().len(), 500);
        assert_eq!(v["truncated"], true);
    }

    #[tokio::test]
    async fn tool_outcome_glob_excludes_vendor_and_build_dirs() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(workspace.path().join("target/debug")).unwrap();
        std::fs::write(workspace.path().join("src/a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            workspace.path().join("node_modules/pkg/index.rs"),
            "// vendor\n",
        )
        .unwrap();
        std::fs::write(workspace.path().join("target/debug/b.rs"), "// build\n").unwrap();

        let v = run_glob(workspace.path(), json!({"pattern":"**/*.rs"})).await;

        assert_eq!(v["matches"], json!(["src/a.rs"]));
        assert_eq!(v["truncated"], false);
    }

    #[tokio::test]
    async fn tool_outcome_glob_small_scope_behavior_unchanged() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src/a")).unwrap();
        std::fs::write(workspace.path().join("src/a/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), "// lib\n").unwrap();
        std::fs::write(workspace.path().join("b.txt"), "text\n").unwrap();

        let v = run_glob(workspace.path(), json!({"pattern":"**/*.rs"})).await;

        assert_eq!(v["matches"], json!(["src/a/main.rs", "src/lib.rs"]));
        assert_eq!(v["truncated"], false);
        assert!(v.get("note").is_none());
    }
}
