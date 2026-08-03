use serde_json::{json, Value};

use crate::error::Result;
use crate::events::EventRecorder;
use crate::goal::GoalState;
use crate::provider::{ChatMessage, ProviderCapabilities};
use crate::tools::shell_exec::ShellExecRequest;

pub(crate) fn push_tool_rejection(
    recorder: &mut EventRecorder,
    messages: &mut Vec<ChatMessage>,
    tool_name: &str,
    tool_call_id: &str,
    content: String,
    extra_event_fields: Option<Value>,
) -> Result<()> {
    emit_tool_rejection(
        recorder,
        tool_name,
        tool_call_id,
        &content,
        extra_event_fields,
    )?;
    messages.push(ChatMessage::tool(tool_call_id.to_string(), content));
    Ok(())
}

pub(crate) fn emit_tool_rejection(
    recorder: &mut EventRecorder,
    tool_name: &str,
    tool_call_id: &str,
    content: &str,
    extra_event_fields: Option<Value>,
) -> Result<()> {
    let error = serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| content.to_string());
    crate::tools::emit_tool_failed_with_extra(
        recorder,
        tool_name,
        tool_call_id,
        &error,
        extra_event_fields.unwrap_or(Value::Null),
    )?;
    Ok(())
}

pub(crate) fn guardrail_summary(name: &str, args: &str, _workspace: &std::path::Path) -> String {
    match name {
        "shell_exec" => serde_json::from_str::<ShellExecRequest>(args)
            .map(|request| request.command)
            .unwrap_or_else(|_| name.to_string()),
        "fs_write" => serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| {
                v.get("path")
                    .and_then(|p| p.as_str())
                    .map(|s| format!("write {s}"))
            })
            .unwrap_or_else(|| name.to_string()),
        "fs_edit" => serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| {
                v.get("path")
                    .and_then(|p| p.as_str())
                    .map(|s| format!("edit {s}"))
            })
            .unwrap_or_else(|| name.to_string()),
        _ => {
            if let Some(rest) = name.strip_prefix("mcp__") {
                let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
                let mut args_brief = args.replace('\n', " ");
                if args_brief.len() > 80 {
                    crate::text_util::truncate_at_char_boundary(&mut args_brief, 80);
                    args_brief.push('…');
                }
                format!("{server} · {tool} · {args_brief}")
            } else {
                name.to_string()
            }
        }
    }
}

pub(crate) fn emit_capabilities(
    recorder: &mut EventRecorder,
    capabilities: &ProviderCapabilities,
) -> Result<()> {
    recorder.emit("capabilities.declared", serde_json::to_value(capabilities)?)?;
    Ok(())
}

pub(crate) fn emit_no_progress_needs_decision(
    recorder: &mut EventRecorder,
    goal: &GoalState,
    progress: &crate::run_progress::RunProgress,
    attempts: &crate::evaluator::AttemptTracker,
) -> Result<()> {
    recorder.emit(
        "run.needs_decision",
        json!({
            "reason": "blocked_questions",
            "contract_version": goal.contract.version,
            "blocked_reason": "no_progress",
            "questions": [],
            "agent_diagnosis": null,
            "evidence_refs": [],
            "consecutive_stale_turns": progress.consecutive_stale_turns,
            "turns_since_last_real_edit": progress.turns_since_last_real_edit,
            "failed_criteria": goal.contract.criteria.iter()
                .filter(|c| !matches!(
                    c.status,
                    crate::goal::CriterionStatus::Passed
                        | crate::goal::CriterionStatus::Waived
                ))
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            "criteria": goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
            "attempts_summary": { "turns": progress.turns, "attempts": attempts.count() },
            "trigger": "harness",
        }),
    )?;
    Ok(())
}

/// 预算（轮数）耗尽时的处境标签：这趟有没有真编辑过至少一次。
/// 「还在干活」= 距上次真编辑 < 总轮数（即编辑过·那轮清零过）；从没编辑 → 恒相等 → no_progress。
/// 不复用 `decide().halt` 取反：预算耗尽这条路只在没触发停机时才走到·取反恒真会误标所有跑。
/// 不加 `consecutive_stale_turns`：预算耗尽时它恒「未停机」·且阈值=0 时会部分重启被禁用的安全行为。
///
/// P2（2026-07-26 与 P1 三概念分家同刀）：`turns_since_last_real_edit` 现在只被真编辑
/// （非 MCP 写工具）清零——修完 P1 后，全程没有 fs_write/fs_edit 可用的 run（例如全靠
/// mcp__agentloom__* 派单的 lead，`--disallow-tools fs_edit,fs_write,shell_exec`）无论干了
/// 多少活、`turns_since_last_real_edit` 恒等于 `turns`，会被这条判定误标成 "no_progress"。
/// `write_tools_offered`（run 全程恒定，见 run_loop.rs 顶部计算）为 false 时，「没编辑过」
/// 不能当作 no_progress 的依据——但也不能因此瞎猜「还在干活」，只能落回既有词表里最接近
/// 「说不清、留给下游/人看」的取值：`budget_exhausted_still_progressing`（不新增词表值）。
fn budget_exhausted_blocked_reason(
    progress: &crate::run_progress::RunProgress,
    write_tools_offered: bool,
) -> &'static str {
    if !write_tools_offered {
        return "budget_exhausted_still_progressing";
    }
    if progress.turns_since_last_real_edit < progress.turns {
        "budget_exhausted_still_progressing"
    } else {
        "no_progress"
    }
}

pub(crate) fn emit_budget_exhausted_needs_decision(
    recorder: &mut EventRecorder,
    goal: &GoalState,
    progress: &crate::run_progress::RunProgress,
    attempts: &crate::evaluator::AttemptTracker,
    write_tools_offered: bool,
) -> Result<()> {
    let blocked_reason = budget_exhausted_blocked_reason(progress, write_tools_offered);
    recorder.emit(
        "run.needs_decision",
        json!({
            "reason": "blocked_questions",
            "contract_version": goal.contract.version,
            "blocked_reason": blocked_reason,
            "questions": [],
            "agent_diagnosis": null,
            "evidence_refs": [],
            "consecutive_stale_turns": progress.consecutive_stale_turns,
            "turns_since_last_real_edit": progress.turns_since_last_real_edit,
            "failed_criteria": goal.contract.criteria.iter()
                .filter(|c| !matches!(
                    c.status,
                    crate::goal::CriterionStatus::Passed
                        | crate::goal::CriterionStatus::Waived
                ))
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            "criteria": goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
            "attempts_summary": { "turns": progress.turns, "attempts": attempts.count() },
            "trigger": "harness",
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{budget_exhausted_blocked_reason, guardrail_summary};
    use crate::run_progress::RunProgress;

    #[test]
    fn still_progressing_when_a_real_edit_happened() {
        // 编辑过 → 距上次真编辑会清零 → 小于总轮数 → 还在干活
        let progress = RunProgress {
            turns: 5,
            turns_since_last_real_edit: 0,
            ..Default::default()
        };
        assert_eq!(
            budget_exhausted_blocked_reason(&progress, true),
            "budget_exhausted_still_progressing"
        );
    }

    #[test]
    fn no_progress_when_checks_ran_but_never_edited() {
        // shell-proof：跑了检查但从没真编辑 → no_progress（旧计数器会把它误判 still_progressing）
        let progress = RunProgress {
            turns: 5,
            turns_since_last_real_edit: 5,
            checks_run: 3,
            ..Default::default()
        };
        assert_eq!(
            budget_exhausted_blocked_reason(&progress, true),
            "no_progress"
        );
    }

    #[test]
    fn no_progress_when_pure_reader_never_edited() {
        // 纯读新文件：stale 被清零但「距上次真编辑」照爬 → 从没编辑 → no_progress
        let progress = RunProgress {
            turns: 5,
            consecutive_stale_turns: 0,
            turns_since_last_real_edit: 5,
            ..Default::default()
        };
        assert_eq!(
            budget_exhausted_blocked_reason(&progress, true),
            "no_progress"
        );
    }

    #[test]
    fn no_progress_only_applies_when_write_tools_offered() {
        // P2：无写工具的 run（如全靠 mcp__agentloom__* 派单的 lead）从没「真编辑」过是常态，
        // 不能据此判 no_progress——否则修完 P1 后每个正常打满预算的 MCP-only lead 都会被
        // 误标 no_progress。
        let progress = RunProgress {
            turns: 5,
            turns_since_last_real_edit: 5,
            ..Default::default()
        };
        assert_eq!(
            budget_exhausted_blocked_reason(&progress, false),
            "budget_exhausted_still_progressing"
        );
    }

    #[test]
    fn guardrail_summary_does_not_panic_on_multibyte_args_straddling_byte_80() {
        // 真机现场复现：78 个 ASCII 字节后紧跟中文——第 80 字节正好切在
        // 汉字中间（该字符占字节 78..81），旧的 `args.truncate(80)` 会
        // panic：`assertion failed: self.is_char_boundary(new_len)`。
        let ascii_prefix = "a".repeat(78);
        let args = format!("{ascii_prefix}中文任务参数超长需要截断验证不panic");
        assert!(args.len() > 80, "fixture 前提：args 必须超过 80 字节");
        // 第 80 字节确实落在多字节字符中间，否则这个 fixture 就没测到要害。
        assert!(!args.is_char_boundary(80));

        let summary = guardrail_summary(
            "mcp__some_server__some_tool",
            &args,
            std::path::Path::new("."),
        );

        assert!(summary.ends_with('…'), "截断后应以 … 收尾: {summary}");
        // summary 形如 "server · tool · <args_brief>…"；提取 args_brief 部分校验字节数上限。
        let args_brief = summary
            .rsplit(" · ")
            .next()
            .expect("summary 必须含 ' · ' 分隔的 args_brief 段");
        let args_brief_without_ellipsis = args_brief.trim_end_matches('…');
        assert!(
            args_brief_without_ellipsis.len() <= 80,
            "截断后的 args_brief 不应超过 80 字节: {} bytes",
            args_brief_without_ellipsis.len()
        );
    }
}
