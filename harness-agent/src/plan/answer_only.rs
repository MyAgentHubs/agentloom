use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::orchestrator::{RunOutcome, RunResult};
use crate::plan::run_plan::PlanRunOptions;

pub(super) fn maybe_complete(
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
) -> Result<Option<RunResult>> {
    if !is_answer_only_objective(&opts.objective) {
        return Ok(None);
    }

    recorder.emit(
        "agent.note.delta",
        json!({
            "text": "已进入 plan 模式；这条请求明确要求不改文件、只回复，所以不会生成任务清单，也不会改动工作区。下一步会等待需要拆解执行的编码任务，再进入规划和执行流程。"
        }),
    )?;
    recorder.emit(
        "run.completed",
        json!({ "tasks": 0, "mode": "answer_only" }),
    )?;

    Ok(Some(RunResult {
        run_id: opts.plan_run_id.clone(),
        outcome: RunOutcome::Completed,
        always_used: false,
    }))
}

fn is_answer_only_objective(objective: &str) -> bool {
    let s = objective.to_lowercase();
    contains_any(
        &s,
        &[
            "不要改文件",
            "不要修改文件",
            "不改文件",
            "不修改文件",
            "无需改文件",
            "不需要改文件",
            "不要写文件",
            "do not edit",
            "don't edit",
            "don’t edit",
            "do not modify",
            "don't modify",
            "don’t modify",
            "no file changes",
            "without changing files",
        ],
    ) && contains_any(
        &s,
        &[
            "只回复",
            "只回答",
            "仅回复",
            "仅回答",
            "只需要回复",
            "只需回复",
            "回复你是否",
            "是否进入",
            "smoke test",
            "smoke check",
            "状态确认",
            "answer only",
            "just reply",
            "reply only",
        ],
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::is_answer_only_objective;

    #[test]
    fn detects_chinese_smoke_answer_only_request() {
        assert!(is_answer_only_objective(
            "做一个 smoke test：不要改文件，只回复你是否进入了 plan 模式，以及下一步会怎么处理。"
        ));
    }

    #[test]
    fn normal_coding_task_still_requires_planning() {
        assert!(!is_answer_only_objective(
            "给 src/lib.rs 增加 is_warm 函数，并补一条测试。"
        ));
    }

    #[test]
    fn scoped_engine_edit_is_not_confused_with_answer_only() {
        assert!(!is_answer_only_objective(
            "不要改 app 代码，请修改 harness-agent 的 plan 入口。"
        ));
    }
}
