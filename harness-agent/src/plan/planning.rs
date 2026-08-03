//! 规划重试决策（纯函数）：给模型原始输出 → 接受/打回重试/升级。provider+recorder 壳在 1b。

use crate::plan::contract::{parse_worklist, PlanTask};
use crate::plan::review_gate::{review_worklist, ReviewVerdict};

#[derive(Debug)]
pub enum PlanStep {
    Accept {
        tasks: Vec<PlanTask>,
    },
    Retry {
        feedback: String,
        reasons: Vec<String>,
    },
    Escalate {
        reasons: Vec<String>,
    },
}

/// 折一次规划尝试。attempt_index 从 0 起；max_attempts = 打回上限 K（约定 ≥ 1）。
pub fn fold_plan_attempt(
    raw_model_json: &str,
    attempt_index: usize,
    max_attempts: usize,
) -> PlanStep {
    let last_attempt = attempt_index + 1 >= max_attempts.max(1);

    let reasons: Vec<String> = match parse_worklist(raw_model_json) {
        Ok(tasks) => match review_worklist(&tasks) {
            ReviewVerdict::Ok => return PlanStep::Accept { tasks },
            ReviewVerdict::Bounce { reasons } => reasons,
        },
        Err(e) => vec![format!(
            "worklist JSON 解析失败：{e}（要求严格 JSON·字段见 schema）"
        )],
    };

    if last_attempt {
        PlanStep::Escalate { reasons }
    } else {
        PlanStep::Retry {
            feedback: format!("计划被打回，请逐条修正后重出：\n- {}", reasons.join("\n- ")),
            reasons,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 5 } ] }"#;
    const BAD_JSON: &str = "this is not json";
    const BAD_GATE: &str = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": [], "acceptance_cmd": "cargo test", "max_turns": 5 } ] }"#;

    #[test]
    fn good_plan_accepts() {
        match fold_plan_attempt(GOOD, 0, 3) {
            PlanStep::Accept { tasks } => assert_eq!(tasks.len(), 1),
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn bad_json_retries() {
        assert!(matches!(
            fold_plan_attempt(BAD_JSON, 0, 3),
            PlanStep::Retry { .. }
        ));
    }

    #[test]
    fn bounce_reasons_bad_gate_retries_with_specific_feedback_and_reasons() {
        match fold_plan_attempt(BAD_GATE, 0, 3) {
            PlanStep::Retry { feedback, reasons } => {
                assert!(feedback.contains("files_scope"));
                assert!(reasons.iter().any(|r| r.contains("files_scope")));
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn exhausting_attempts_escalates() {
        match fold_plan_attempt(BAD_GATE, 2, 3) {
            PlanStep::Escalate { reasons } => assert!(!reasons.is_empty()),
            other => panic!("expected Escalate, got {other:?}"),
        }
    }
}
