//! Planner↔Executor 接缝：PlanTask 翻成 run_solo 的任务级输入 + 读 child 完成判定（spec §4.2/§3.2）。

use std::path::Path;

use serde_json::Value;

use crate::error::Result;
use crate::goal::GoalContract;
use crate::orchestrator::{RunOutcome, RunResult};
use crate::plan::contract::{
    ChangeSet, ChildCriterionStatus, ChildEvaluation, ChildRunOutcome, PlanTask, ScopeViolation,
    TaskEvidence, TaskNarrative, TaskReport, TaskReportStatus, TASK_REPORT_SCHEMA_VERSION,
};
use crate::plan::write_audit::Violation;

/// 把一个原子任务翻成任务级 GoalContract（喂给 run_solo·spec §4.2/B6）。
/// objective=intent · criteria=[acceptance(行为道·硬验收) + artifact_check(按名鞭子·驱动 child·引擎侧 per-task 不否决·spec §2)] · scope=files_scope · constraints⊇forbidden+stop。
/// 纯函数（无 provider/IO）·新契约不带历史（version=1·update_log 空）。
/// B1：这张 contract 经 run_solo_task 用于初始化 child GoalState·render_state_frame 渲染 scope/constraints 给 executor。
pub fn task_to_goal_contract(task: &PlanTask) -> GoalContract {
    let mut constraints = Vec::new();
    for f in &task.forbidden_scope {
        constraints.push(format!("forbidden_scope（绝不能碰）: {f}"));
    }
    for s in &task.stop_conditions {
        constraints.push(format!("stop_condition（遇到就停下报告）: {s}"));
    }
    let scope = if task.files_scope.is_empty() {
        None
    } else {
        Some(task.files_scope.join(", "))
    };
    let mut criteria = vec![task.acceptance.clone()];
    if let Some(art) = &task.artifact_check {
        criteria.push(art.clone());
    }
    GoalContract {
        objective: task.intent.clone(),
        constraints,
        scope,
        criteria,
        version: 1,
        update_log: Vec::new(),
    }
}

/// 单个任务跑完后的地面真相判定（spec §3.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskVerdict {
    /// acceptance 全绿（passed/waived）。
    Done,
    /// 没全绿·附原因（哪条没过 / 没跑到验收）。
    Blocked { reason: String },
}

/// 读 child run 的 events.jsonl·取**末条** `completion.evaluated`·判 acceptance 是否全绿（F1）。
/// 不读 goal_contract.json sidecar status。没有该事件（max_turns/中断/失败）→ Blocked。
pub fn task_verdict_from_journal(events_path: &Path) -> Result<TaskVerdict> {
    let text = std::fs::read_to_string(events_path)?;
    let last_eval = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rfind(|event| event.get("type").and_then(Value::as_str) == Some("completion.evaluated"));

    let Some(eval) = last_eval else {
        return Ok(TaskVerdict::Blocked {
            reason: "child run 未产出 completion.evaluated（未跑到验收·max_turns/中断/失败）"
                .to_string(),
        });
    };

    let Some(criteria) = eval
        .get("payload")
        .and_then(|p| p.get("criteria"))
        .and_then(Value::as_array)
    else {
        return Ok(TaskVerdict::Blocked {
            reason: "completion.evaluated 无 criteria".to_string(),
        });
    };
    if criteria.is_empty() {
        return Ok(TaskVerdict::Blocked {
            reason: "completion.evaluated criteria 为空".to_string(),
        });
    }

    let mut unmet = Vec::new();
    for c in criteria {
        let status = c.get("status").and_then(Value::as_str).unwrap_or("unknown");
        if !matches!(status, "passed" | "waived") {
            let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
            unmet.push(format!("{id}={status}"));
        }
    }

    if unmet.is_empty() {
        Ok(TaskVerdict::Done)
    } else {
        Ok(TaskVerdict::Blocked {
            reason: format!("acceptance 未过：{}", unmet.join(", ")),
        })
    }
}

pub fn child_outcome_from_run(outcome: RunOutcome) -> ChildRunOutcome {
    match outcome {
        RunOutcome::Completed => ChildRunOutcome::Completed,
        RunOutcome::Blocked => ChildRunOutcome::Blocked,
        RunOutcome::NeedsDecision => ChildRunOutcome::NeedsDecision,
        RunOutcome::Interrupted => ChildRunOutcome::Interrupted,
        RunOutcome::Failed => ChildRunOutcome::Failed,
    }
}

pub fn child_evaluation_from_journal(events_path: &Path) -> Result<Option<ChildEvaluation>> {
    let text = std::fs::read_to_string(events_path)?;
    let last_eval = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rfind(|event| event.get("type").and_then(Value::as_str) == Some("completion.evaluated"));
    let Some(eval) = last_eval else {
        return Ok(None);
    };
    let criteria = eval
        .get("payload")
        .and_then(|p| p.get("criteria"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| ChildCriterionStatus {
            id: c
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            status: c
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        })
        .collect();
    Ok(Some(ChildEvaluation { criteria }))
}

pub fn task_report_from_child(
    task: &PlanTask,
    child: &RunResult,
    events_path: &Path,
    changed_files: Vec<String>,
    violations: Vec<Violation>,
) -> Result<TaskReport> {
    let child_evaluation = child_evaluation_from_journal(events_path)?;
    let legacy = task_verdict_from_journal(events_path).unwrap_or(TaskVerdict::Blocked {
        reason: "child journal 不可解析".to_string(),
    });
    let status = match (child.outcome, &legacy, child_evaluation.is_some()) {
        (RunOutcome::Completed, TaskVerdict::Done, _) => TaskReportStatus::DoneCandidate,
        (RunOutcome::NeedsDecision, _, _) => TaskReportStatus::NeedsDecisionCandidate,
        (RunOutcome::Completed, _, _) => TaskReportStatus::DoneCandidate,
        (_, _, false) => TaskReportStatus::StoppedUnvalidated,
        (_, TaskVerdict::Blocked { .. }, _) => TaskReportStatus::BlockedCandidate,
        _ => TaskReportStatus::Unknown,
    };
    let changes = ChangeSet {
        changed_files,
        scope_violations: violations
            .into_iter()
            .map(|v| ScopeViolation {
                path: v.path,
                reason: v.reason,
            })
            .collect(),
    };
    let mut evidence = Vec::new();
    if let Some(eval) = child_evaluation.clone() {
        evidence.push(TaskEvidence::ChildCompletion(eval));
    }
    evidence.push(TaskEvidence::WriteAudit(changes.clone()));
    Ok(TaskReport {
        schema_version: TASK_REPORT_SCHEMA_VERSION,
        task_id: task.id.clone(),
        child_run_id: child.run_id.clone(),
        status,
        acceptance: task.acceptance.clone(),
        child_outcome: child_outcome_from_run(child.outcome),
        child_evaluation,
        stop: None,
        changes,
        evidence,
        narrative: TaskNarrative::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::contract::parse_worklist;

    fn one_task(json: &str) -> crate::plan::contract::PlanTask {
        parse_worklist(json).unwrap().into_iter().next().unwrap()
    }

    #[test]
    fn maps_intent_acceptance_scope_and_constraints() {
        let task = one_task(
            r#"{ "tasks": [ {
              "id": "t1", "intent": "给 post 加重试",
              "files_scope": ["src/provider/retry.rs", "src/provider/mod.rs"],
              "forbidden_scope": ["src/provider/secret.rs"],
              "stop_conditions": ["要改公共接口就停下报告"],
              "acceptance_cmd": "cargo test --manifest-path harness-agent/Cargo.toml retry",
              "max_turns": 12 } ] }"#,
        );
        let gc = task_to_goal_contract(&task);

        assert_eq!(gc.objective, "给 post 加重试");
        assert_eq!(gc.criteria.len(), 1);
        assert!(gc.criteria[0].is_executable_verifiable());
        let scope = gc.scope.expect("scope set");
        assert!(scope.contains("src/provider/retry.rs"));
        assert!(scope.contains("src/provider/mod.rs"));
        assert!(gc
            .constraints
            .iter()
            .any(|c| c.contains("src/provider/secret.rs")));
        assert!(gc
            .constraints
            .iter()
            .any(|c| c.contains("要改公共接口就停下报告")));
        assert_eq!(gc.version, 1);
        assert!(gc.update_log.is_empty());
    }

    #[test]
    fn child_contract_includes_both_lanes_when_artifact_present() {
        let task = one_task(
            r#"{ "tasks": [ { "id": "t1", "intent": "add field", "files_scope": ["src"],
              "acceptance_cmd": "cargo test x", "artifact_check_cmd": "grep -rq foo src", "max_turns": 8 } ] }"#,
        );
        let gc = task_to_goal_contract(&task);
        assert_eq!(gc.criteria.len(), 2, "behavior + artifact 两条都给 child");
        let ids: Vec<&str> = gc.criteria.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"t1_acc") && ids.contains(&"t1_art"));
        for c in &gc.criteria {
            assert!(!c.claim.contains("grep"), "claim must not leak check_cmd");
            assert!(!c.claim.contains("cargo test"));
        }
    }

    #[test]
    fn child_contract_behavior_only_when_no_artifact() {
        let task = one_task(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src"],
              "acceptance_cmd": "cargo test x", "max_turns": 8 } ] }"#,
        );
        let gc = task_to_goal_contract(&task);
        assert_eq!(gc.criteria.len(), 1);
        assert_eq!(gc.criteria[0].id, "t1_acc");
    }

    #[test]
    fn empty_forbidden_and_stop_yield_no_extra_constraints() {
        let task = one_task(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "max_turns": 5 } ] }"#,
        );
        let gc = task_to_goal_contract(&task);
        assert!(gc.constraints.is_empty());
        assert_eq!(gc.scope.as_deref(), Some("a.rs"));
    }

    fn write_events(lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        (dir, path)
    }

    #[test]
    fn all_passed_is_done() {
        let (_d, path) = write_events(&[
            r#"{"type":"run.started","payload":{}}"#,
            r#"{"type":"completion.evaluated","payload":{"criteria":[{"id":"t1_acc","status":"passed"}]}}"#,
            r#"{"type":"run.completed","payload":{"turns":2}}"#,
        ]);
        assert_eq!(task_verdict_from_journal(&path).unwrap(), TaskVerdict::Done);
    }

    #[test]
    fn failed_criterion_is_blocked_with_id() {
        let (_d, path) = write_events(&[
            r#"{"type":"completion.evaluated","payload":{"criteria":[{"id":"t1_acc","status":"failed"}]}}"#,
        ]);
        match task_verdict_from_journal(&path).unwrap() {
            TaskVerdict::Blocked { reason } => assert!(reason.contains("t1_acc")),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn last_completion_evaluated_wins() {
        let (_d, path) = write_events(&[
            r#"{"type":"completion.evaluated","payload":{"criteria":[{"id":"t1_acc","status":"failed"}]}}"#,
            r#"{"type":"completion.evaluated","payload":{"criteria":[{"id":"t1_acc","status":"passed"}]}}"#,
        ]);
        assert_eq!(task_verdict_from_journal(&path).unwrap(), TaskVerdict::Done);
    }

    #[test]
    fn no_completion_event_is_blocked() {
        let (_d, path) = write_events(&[
            r#"{"type":"run.started","payload":{}}"#,
            r#"{"type":"run.blocked","payload":{"reason":"max_eval_attempts"}}"#,
        ]);
        assert!(matches!(
            task_verdict_from_journal(&path).unwrap(),
            TaskVerdict::Blocked { .. }
        ));
    }

    #[test]
    fn task_report_from_blocked_child_is_advisory_only() {
        let task = one_task(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src/a.rs"],
              "acceptance_cmd": "true", "max_turns": 5 } ] }"#,
        );
        let (_d, path) = write_events(&[
            r#"{"type":"completion.evaluated","payload":{"criteria":[{"id":"t1_acc","status":"failed"}]}}"#,
        ]);
        let child = crate::orchestrator::RunResult {
            run_id: "plan__t1".into(),
            outcome: crate::orchestrator::RunOutcome::Blocked,
            always_used: false,
        };
        let report =
            task_report_from_child(&task, &child, &path, vec!["src/a.rs".into()], vec![]).unwrap();
        assert_eq!(
            report.status,
            crate::plan::contract::TaskReportStatus::BlockedCandidate
        );
        assert_eq!(
            report.child_outcome,
            crate::plan::contract::ChildRunOutcome::Blocked
        );
        assert_eq!(report.acceptance.id, "t1_acc");
        assert!(report
            .child_evaluation
            .unwrap()
            .criteria
            .iter()
            .any(|c| c.status == "failed"));
    }

    #[test]
    fn task_report_records_scope_violations_as_policy_context() {
        let task = one_task(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src/a.rs"],
              "acceptance_cmd": "true", "max_turns": 5 } ] }"#,
        );
        let (_d, path) = write_events(&[]);
        let child = crate::orchestrator::RunResult {
            run_id: "plan__t1".into(),
            outcome: crate::orchestrator::RunOutcome::Completed,
            always_used: false,
        };
        let report = task_report_from_child(
            &task,
            &child,
            &path,
            vec!["src/secret.rs".into()],
            vec![crate::plan::write_audit::Violation {
                path: "src/secret.rs".into(),
                reason: "超出 files_scope 白名单：src/secret.rs".into(),
            }],
        )
        .unwrap();
        assert_eq!(report.changes.scope_violations.len(), 1);
        assert_eq!(
            report.status,
            crate::plan::contract::TaskReportStatus::DoneCandidate
        );
    }

    #[test]
    fn no_completion_event_becomes_stopped_unvalidated_candidate() {
        let (_d, path) = write_events(&[r#"{"type":"run.started","payload":{}}"#]);
        let eval = child_evaluation_from_journal(&path).unwrap();
        assert!(eval.is_none());
    }
}
