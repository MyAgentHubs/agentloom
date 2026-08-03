use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::Result;
use crate::memory::{retrieve, MemoryStore};
use crate::provider::ToolCall;
use crate::tools::{
    emit_tool_completed, emit_tool_failed, emit_tool_started, Tool, ToolContext, ToolOutcome,
};

pub struct MemoryLookupTool;

#[derive(Debug, Deserialize)]
struct MemoryLookupArgs {
    query: String,
}

fn in_band_error(ctx: &mut ToolContext<'_>, call: &ToolCall, msg: &str) -> Result<ToolOutcome> {
    emit_tool_failed(ctx.recorder, "memory_lookup", &call.id, msg)?;
    Ok(ToolOutcome::recoverable(serde_json::to_string(
        &json!({ "error": msg }),
    )?))
}

#[async_trait]
impl Tool for MemoryLookupTool {
    fn name(&self) -> &str {
        "memory_lookup"
    }

    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "memory_lookup",
                "description": "Look up repo-local lessons previously saved by the user. Read-only. Results are repository memory and may be stale or incorrect.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Problem, command, file, or keyword to match against repository memory." }
                    },
                    "required": ["query"]
                }
            }
        })
    }

    fn mutates(&self) -> bool {
        false
    }

    fn requires_network(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let args: MemoryLookupArgs =
            match serde_json::from_str::<MemoryLookupArgs>(&call.function.arguments) {
                Ok(args) if !args.query.trim().is_empty() => args,
                Ok(_) => return in_band_error(ctx, call, "bad arguments: query is required"),
                Err(e) => return in_band_error(ctx, call, &format!("bad arguments: {e}")),
            };
        let query = args.query;

        emit_tool_started(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "query": query }),
        )?;

        let store = match MemoryStore::for_workspace(ctx.workspace) {
            Ok(store) => store,
            Err(e) => {
                emit_tool_completed(ctx.recorder, self.name(), &call.id, json!({ "count": 0 }))?;
                return Ok(ToolOutcome::success(serde_json::to_string(&json!({
                    "source": "repo_memory",
                    "notice": "Repository memory may be stale or incorrect.",
                    "query": query,
                    "count": 0,
                    "lessons": [],
                    "warning": e.to_string(),
                }))?));
            }
        };

        let lessons = store.list_active().unwrap_or_default();
        let matches = retrieve::match_lessons(&query, &lessons);
        let hits: Vec<(&str, &crate::memory::lesson::Lesson)> = matches
            .direct
            .into_iter()
            .map(|lesson| ("direct", lesson))
            .chain(matches.hint.into_iter().map(|lesson| ("hint", lesson)))
            .take(3)
            .collect();

        let mut out = Vec::new();
        for (mode, lesson) in hits {
            let _ = store.touch_last_used(&lesson.id, "unset");
            out.push(json!({
                "id": lesson.id,
                "match": mode,
                "content": lesson.to_markdown(),
            }));
        }

        let count = out.len();
        emit_tool_completed(
            ctx.recorder,
            self.name(),
            &call.id,
            json!({ "count": count }),
        )?;
        Ok(ToolOutcome::success(serde_json::to_string(&json!({
            "source": "repo_memory",
            "notice": "Repository memory may be stale or incorrect.",
            "query": query,
            "count": count,
            "lessons": out,
        }))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventRecorder, OutputMode};
    use crate::memory::lesson::{Lesson, LessonSource, LessonStatus};
    use crate::provider::{FunctionCall, ToolCall};
    use crate::tools::{Tool, ToolContext};
    use serde_json::json;
    use serial_test::serial;

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "memory_call".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "memory_lookup".into(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn tool_readonly_offline() {
        let tool = MemoryLookupTool;
        assert_eq!(tool.name(), "memory_lookup");
        assert!(!tool.mutates());
        assert!(!tool.requires_network());
    }

    #[tokio::test]
    async fn tool_outcome_memory_lookup_bad_args_is_recoverable_and_emits_failed() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: workspace.path(),
            recorder: &mut recorder,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };

        let out = MemoryLookupTool
            .execute(&mut ctx, &call(json!({})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        let returned: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert!(returned["error"]
            .as_str()
            .unwrap()
            .contains("bad arguments"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"memory_lookup\""));
        assert!(events.contains("\"tool_call_id\":\"memory_call\""));
    }

    #[tokio::test]
    #[serial]
    async fn lookup_returns_at_most_three_full_lessons_and_touches_them() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("MYAGENT_HOME", home.path());
        let workspace = tempfile::tempdir().unwrap();
        let store = MemoryStore::for_workspace(workspace.path()).unwrap();
        for i in 0..4 {
            store.write_lesson(&lesson(&format!("l{i}"))).unwrap();
        }

        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: workspace.path(),
            recorder: &mut recorder,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::Off,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };

        let out = MemoryLookupTool
            .execute(&mut ctx, &call(json!({"query":"cargo build E0463"})))
            .await
            .unwrap();

        let returned: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(returned["source"], "repo_memory");
        assert_eq!(returned["count"], 3);
        let lessons = returned["lessons"].as_array().unwrap();
        assert_eq!(lessons.len(), 3);
        assert!(lessons
            .iter()
            .all(|lesson| lesson["content"].as_str().unwrap().contains("## 修复/做法")));
        for lesson in lessons {
            let id = lesson["id"].as_str().unwrap();
            assert_eq!(
                store.read_lesson(id).unwrap().last_used.as_deref(),
                Some("unset")
            );
        }
        std::env::remove_var("MYAGENT_HOME");
    }

    fn lesson(id: &str) -> Lesson {
        Lesson {
            id: id.into(),
            status: LessonStatus::Active,
            source: LessonSource::UserTaught,
            created: "t".into(),
            last_confirmed: "t".into(),
            last_used: None,
            evidence_runs: vec![],
            tags: vec!["cargo".into()],
            observed_commands: vec![],
            episode_ref: None,
            body: "## 问题特征\ncargo build E0463\n## 修复/做法\nrustup update\n".into(),
        }
    }
}
