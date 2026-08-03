use async_trait::async_trait;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

use crate::error::Result;
use crate::provider::ToolCall;
use crate::tools::glob::{glob_match, walk};
use crate::tools::{
    emit_tool_completed, emit_tool_failed, emit_tool_started, Tool, ToolContext, ToolOutcome,
};

pub struct GrepTool;

const MIN_RESULTS: usize = 1;
const DEFAULT_MAX_RESULTS: usize = 50;
const MAX_RESULTS_CAP: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrepHit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct GrepArgs {
    pattern: String, // 子串
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    path_glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default = "default_max")]
    max_results: usize,
}

fn default_max() -> usize {
    DEFAULT_MAX_RESULTS
}

pub(crate) fn grep_workspace(
    workspace: &Path,
    pattern: &str,
    rs_only: bool,
    max: usize,
) -> (Vec<GrepHit>, bool) {
    grep_workspace_inner(workspace, pattern, None, false, rs_only, max).unwrap_or_default()
}

fn grep_workspace_inner(
    workspace: &Path,
    pattern: &str,
    path_glob: Option<&str>,
    case_insensitive: bool,
    rs_only: bool,
    max: usize,
) -> Result<(Vec<GrepHit>, bool)> {
    let mut files = Vec::new();
    walk(workspace, workspace, &mut files)?;
    let needle = if case_insensitive {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let mut hits = Vec::new();
    let mut truncated = false;
    'outer: for rel in files {
        if rs_only && !is_rs_source_outside_target(&rel) {
            continue;
        }
        if let Some(g) = path_glob {
            if !glob_match(g, &rel) {
                continue;
            }
        }
        let abs = workspace.join(&rel);
        let Ok(content) = std::fs::read_to_string(&abs) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            let hay = if case_insensitive {
                line.to_lowercase()
            } else {
                line.to_string()
            };
            if hay.contains(&needle) {
                if hits.len() >= max {
                    truncated = true;
                    break 'outer;
                }
                hits.push(GrepHit {
                    path: rel.clone(),
                    line: (i + 1) as u32,
                    text: line.to_string(),
                });
            }
        }
    }

    Ok((hits, truncated))
}

fn grep_workspace_regex(
    workspace: &Path,
    regex: &regex::Regex,
    path_glob: Option<&str>,
    max: usize,
) -> Result<(Vec<GrepHit>, bool)> {
    let mut files = Vec::new();
    walk(workspace, workspace, &mut files)?;
    let mut hits = Vec::new();
    let mut truncated = false;
    'outer: for rel in files {
        if let Some(g) = path_glob {
            if !glob_match(g, &rel) {
                continue;
            }
        }
        let abs = workspace.join(&rel);
        let Ok(content) = std::fs::read_to_string(&abs) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                if hits.len() >= max {
                    truncated = true;
                    break 'outer;
                }
                hits.push(GrepHit {
                    path: rel.clone(),
                    line: (i + 1) as u32,
                    text: line.to_string(),
                });
            }
        }
    }

    Ok((hits, truncated))
}

fn is_rs_source_outside_target(path: &str) -> bool {
    let path = Path::new(path);
    path.extension().and_then(|ext| ext.to_str()) == Some("rs")
        && !path
            .components()
            .any(|component| component.as_os_str() == "target")
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search workspace text files. `regex:false`(default)=literal substring; `regex:true`=Rust regex. Prefer this tool when searching file contents because it is workspace-aware and safer; do not bypass it by running `grep` or `rg` in the shell.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "regex": { "type": "boolean" },
                        "path_glob": { "type": "string" },
                        "case_insensitive": { "type": "boolean" },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum matches to return (default: 50; hard limit: 200). Values above 200 are clamped. If results are truncated, narrow the query instead of increasing this value."
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
        let args: GrepArgs = match serde_json::from_str(&call.function.arguments) {
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

        // C1：归一 path_glob（绝对也认·别静默 0）。
        let path_glob_rel = match args.path_glob.as_deref() {
            None => None,
            Some(g) => match crate::tools::glob::normalize_path_filter(ctx.workspace, g) {
                Ok(rel) => Some(rel),
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
            },
        };

        let (hits, truncated) = if args.regex {
            if args.pattern.len() > 1024 {
                let msg = "grep pattern too long (max 1024 bytes)".to_string();
                emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                return Ok(ToolOutcome::recoverable(msg));
            }
            let regex = match RegexBuilder::new(&args.pattern)
                .case_insensitive(args.case_insensitive)
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    let msg = format!("invalid regex: {e}. For literal matching, omit the regex parameter or escape metacharacters.");
                    emit_tool_failed(ctx.recorder, self.name(), &call.id, &msg)?;
                    return Ok(ToolOutcome::recoverable(msg));
                }
            };
            grep_workspace_regex(ctx.workspace, &regex, path_glob_rel.as_deref(), max_results)?
        } else {
            grep_workspace_inner(
                ctx.workspace,
                &args.pattern,
                path_glob_rel.as_deref(),
                args.case_insensitive,
                false,
                max_results,
            )?
        };

        let hits: Vec<_> = hits
            .into_iter()
            .map(|hit| json!({ "path": hit.path, "line_number": hit.line, "line": hit.text }))
            .collect();
        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "count": hits.len(), "truncated": truncated }),
        )?;
        let note = if truncated {
            Some(format!(
                "[grep: too many matches; showing only the first {} and more exist. Narrow the query with path_glob (directory or file type), a more specific pattern, or a precise expression with regex:true. Do not rerun the same query unchanged.]",
                hits.len()
            ))
        } else if hits.is_empty() {
            Some("[grep: no matches. The query may be too narrow; try a shorter literal, remove regex anchors or unnecessary escaping, broaden or remove path_glob, or try case_insensitive:true.]".to_string())
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
                name: "grep".into(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn grep_description_is_nonempty_and_contains_no_cjk() {
        let definition = GrepTool.definition();
        let description = definition["function"]["description"].as_str().unwrap();

        assert!(!description.is_empty());
        assert!(description.contains("regex"));
        assert!(!description
            .chars()
            .any(|character| ('\u{4E00}'..='\u{9FFF}').contains(&character)));
    }

    async fn run_grep(workspace: &std::path::Path, args: serde_json::Value) -> serde_json::Value {
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
        let out = GrepTool.execute(&mut ctx, &call(args)).await.unwrap();
        serde_json::from_str(&out.content).unwrap()
    }

    #[tokio::test]
    async fn searches_substrings_with_line_numbers() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "alpha\nneedle here\n").unwrap();
        std::fs::write(workspace.path().join("two.md"), "nope\n").unwrap();

        let v = run_grep(workspace.path(), json!({"pattern":"needle"})).await;
        assert_eq!(v["matches"][0]["path"], "one.txt");
        assert_eq!(v["matches"][0]["line_number"], 2);
        assert_eq!(v["matches"][0]["line"], "needle here");
        assert_eq!(v["truncated"], false);
        assert!(v.get("note").is_none());
    }

    #[tokio::test]
    async fn truncated_results_include_narrowing_note() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "needle\nneedle\n").unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"needle","max_results":1}),
        )
        .await;

        assert_eq!(v["truncated"], true);
        let note = v["note"].as_str().unwrap();
        assert!(note.contains("path_glob"));
        assert!(note.contains("regex:true"));
        assert!(note.contains("same query"));
    }

    #[tokio::test]
    async fn default_max_results_is_fifty() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "needle\n".repeat(51)).unwrap();

        let v = run_grep(workspace.path(), json!({"pattern":"needle"})).await;

        assert_eq!(v["matches"].as_array().unwrap().len(), 50);
        assert_eq!(v["truncated"], true);
    }

    #[tokio::test]
    async fn literal_max_results_is_capped_at_two_hundred() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "needle\n".repeat(201)).unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"needle","regex":false,"max_results":500}),
        )
        .await;

        assert_eq!(v["matches"].as_array().unwrap().len(), 200);
        assert_eq!(v["truncated"], true);
    }

    #[tokio::test]
    async fn regex_max_results_is_capped_at_two_hundred() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "needle1\n".repeat(201)).unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"needle\\d","regex":true,"max_results":100000}),
        )
        .await;

        assert_eq!(v["matches"].as_array().unwrap().len(), 200);
        assert_eq!(v["truncated"], true);
    }

    #[tokio::test]
    async fn max_results_below_cap_is_respected() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "needle\n".repeat(11)).unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"needle","max_results":10}),
        )
        .await;

        assert_eq!(v["matches"].as_array().unwrap().len(), 10);
        assert_eq!(v["truncated"], true);
    }

    #[tokio::test]
    async fn zero_max_results_is_clamped_to_one() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "needle\nneedle\n").unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"needle","max_results":0}),
        )
        .await;

        assert_eq!(v["matches"].as_array().unwrap().len(), 1);
        assert_eq!(v["truncated"], true);
    }

    #[tokio::test]
    async fn zero_results_include_broadening_note() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "haystack\n").unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"missing","path_glob":"**/*.txt"}),
        )
        .await;

        assert!(v["matches"].as_array().unwrap().is_empty());
        let note = v["note"].as_str().unwrap();
        assert!(note.contains("shorter literal"));
        assert!(note.contains("path_glob"));
        assert!(note.contains("case_insensitive:true"));
    }

    #[tokio::test]
    async fn supports_case_insensitive_search() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("one.txt"), "Needle\n").unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"needle","case_insensitive":true}),
        )
        .await;
        assert_eq!(v["matches"][0]["line_number"], 1);
    }

    #[tokio::test]
    async fn filters_by_path_glob() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), "needle\n").unwrap();
        std::fs::write(workspace.path().join("notes.txt"), "needle\n").unwrap();

        let v = run_grep(
            workspace.path(),
            json!({"pattern":"needle","path_glob":"**/*.rs"}),
        )
        .await;
        assert_eq!(
            v["matches"],
            json!([{"path":"src/lib.rs","line_number":1,"line":"needle"}])
        );
    }

    #[tokio::test]
    async fn grep_absolute_path_glob_matches_like_relative() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws.path().join("harness-agent/src")).unwrap();
        std::fs::write(ws.path().join("harness-agent/src/lib.rs"), "needle\n").unwrap();
        std::fs::write(ws.path().join("notes.txt"), "needle\n").unwrap();

        let abs = format!("{}/harness-agent/src/*.rs", ws.path().to_string_lossy());
        let v = run_grep(ws.path(), json!({"pattern":"needle","path_glob": abs})).await;
        assert_eq!(
            v["matches"],
            json!([{"path":"harness-agent/src/lib.rs","line_number":1,"line":"needle"}])
        );
    }

    #[tokio::test]
    async fn grep_absolute_path_glob_outside_workspace_returns_note_not_silent_zero() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("a.rs"), "needle\n").unwrap();
        let v = run_grep(ws.path(), json!({"pattern":"needle","path_glob":"/etc/*"})).await;
        assert_eq!(v["matches"], json!([]));
        assert!(v["note"]
            .as_str()
            .unwrap()
            .contains("outside the workspace"));
    }

    #[tokio::test]
    async fn tool_outcome_grep_bad_args_is_recoverable_and_emits_failed() {
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

        let out = GrepTool.execute(&mut ctx, &call(json!({}))).await.unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("bad arguments"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"grep\""));
        assert!(events.contains("\"tool_call_id\":\"c\""));
    }

    #[test]
    fn grep_workspace_rs_only_excludes_md_and_target() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("target")).unwrap();
        std::fs::write(workspace.path().join("a.rs"), "let _ = Foo {\n").unwrap();
        std::fs::write(workspace.path().join("b.md"), "Foo {\n").unwrap();
        std::fs::write(workspace.path().join("target").join("c.rs"), "Foo {\n").unwrap();

        let (hits, truncated) = grep_workspace(workspace.path(), "Foo {", true, 500);

        assert!(!truncated);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a.rs");
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].text, "let _ = Foo {");
    }

    #[test]
    fn grep_workspace_reports_truncated() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a.rs"), "Foo {\nFoo {\n").unwrap();

        let (hits, truncated) = grep_workspace(workspace.path(), "Foo {", true, 1);

        assert_eq!(hits.len(), 1);
        assert!(truncated);
    }

    #[tokio::test]
    async fn grep_regex_literal_default_unchanged() {
        let workspace = tempfile::tempdir().unwrap();
        // pattern with regex metacharacters: "fn (" has a parenthesis
        std::fs::write(workspace.path().join("a.rs"), "fn foo(\nfn (\n").unwrap();

        // No regex param → literal substring match
        let v = run_grep(workspace.path(), json!({"pattern":"fn ("})).await;
        assert_eq!(v["matches"].as_array().unwrap().len(), 1);
        assert_eq!(v["matches"][0]["line"], "fn (");

        // regex:false explicitly → still literal
        let v2 = run_grep(workspace.path(), json!({"pattern":"fn (","regex":false})).await;
        assert_eq!(v2["matches"].as_array().unwrap().len(), 1);
        assert_eq!(v2["matches"][0]["line"], "fn (");

        // "a.b" should NOT match "aXb" (dot is literal, not wildcard)
        std::fs::write(workspace.path().join("b.rs"), "a.b\naXb\n").unwrap();
        let v3 = run_grep(workspace.path(), json!({"pattern":"a.b"})).await;
        assert_eq!(v3["matches"].as_array().unwrap().len(), 1);
        assert_eq!(v3["matches"][0]["line"], "a.b");
    }

    #[tokio::test]
    async fn grep_regex_true_matches_pattern() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a.rs"), "fn foo\nfn bar\nno match\n").unwrap();

        let v = run_grep(workspace.path(), json!({"pattern":"fn \\w+","regex":true})).await;
        assert_eq!(v["matches"].as_array().unwrap().len(), 2);
        assert_eq!(v["matches"][0]["line"], "fn foo");
        assert_eq!(v["matches"][1]["line"], "fn bar");
    }

    #[tokio::test]
    async fn grep_regex_invalid_says_human() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a.rs"), "content\n").unwrap();

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

        let out = GrepTool
            .execute(&mut ctx, &call(json!({"pattern":"(","regex":true})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("invalid regex"));
        assert!(out
            .content
            .contains("For literal matching, omit the regex parameter or escape metacharacters."));
    }

    #[tokio::test]
    async fn grep_regex_oversized_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a.rs"), "content\n").unwrap();

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

        let big_pattern = "a".repeat(1025);
        let out = GrepTool
            .execute(&mut ctx, &call(json!({"pattern":big_pattern,"regex":true})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(out.content.contains("too long"));
    }
}
