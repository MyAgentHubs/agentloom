//! Isolate completion-time acceptance passes so `finalize_completion_outcome` stays readable and
//! below the Clippy line-count threshold without changing its decision priority or event ordering.

use serde_json::{json, Value};

use crate::error::Result;
use crate::events::EventRecorder;
use crate::plan::contract::{
    AcceptanceKind, AcceptanceResult, CommandEvidence, CommandRole, TaskDecision, TaskReportStatus,
    TaskStatus,
};
use crate::plan::state::RunState;

use super::{
    criterion_command_result_readonly_checked, decision_reason, settle_report_decision,
    synthetic_report_for_task, PlanRunOptions,
};

#[derive(Default)]
pub(super) struct CompletionResults {
    pub(super) code_red: Vec<CommandEvidence>,
    pub(super) code_unmet: Vec<Value>,
    pub(super) infra: Vec<Value>,
    pub(super) stopped: Vec<Value>,
    pub(super) policy_unmet: Vec<Value>,
    pub(super) checked_ids: Vec<String>,
    pub(super) passed_ids: Vec<String>,
    pub(super) failed_ids: Vec<String>,
}

pub(super) async fn run_overall_checks(
    state: &RunState,
    opts: &PlanRunOptions,
    results: &mut CompletionResults,
) -> Result<()> {
    for criterion in &state.checks {
        results.checked_ids.push(criterion.id.clone());
        match criterion_command_result_readonly_checked(
            criterion,
            CommandRole::OverallCheck,
            &opts.workspace,
            opts.network,
            opts.fs_write_fence,
        )
        .await?
        {
            AcceptanceResult::Pass { .. } => results.passed_ids.push(criterion.id.clone()),
            AcceptanceResult::CodeRed { acceptance } => {
                results.failed_ids.push(criterion.id.clone());
                results.code_red.push(acceptance.clone());
                results.code_unmet.push(
                    json!({ "kind": "overall_check", "criterion": criterion.id, "evidence": acceptance }),
                );
            }
            AcceptanceResult::InfraRed {
                signature,
                acceptance,
            } => {
                results.infra.push(json!({ "kind": "overall_check", "criterion": criterion.id, "signature": signature, "evidence": acceptance }));
            }
            AcceptanceResult::NotRun { reason } => {
                results.stopped.push(json!({ "kind": "overall_check", "criterion": criterion.id, "stopped_unvalidated": reason }));
            }
            AcceptanceResult::PolicyFailure {
                reason,
                changed_files,
                acceptance,
            } => {
                results.policy_unmet.push(json!({
                    "kind": "overall_policy",
                    "criterion": criterion.id,
                    "reason": reason,
                    "changed_files": changed_files,
                    "evidence": acceptance,
                }));
            }
        }
    }
    Ok(())
}

pub(super) async fn run_done_task_acceptance(
    state: &RunState,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
    results: &mut CompletionResults,
) -> Result<()> {
    for task in &state.worklist {
        if !matches!(task.status, TaskStatus::Done) {
            continue;
        }
        results.checked_ids.push(task.acceptance.id.clone());
        let report = synthetic_report_for_task(task, TaskReportStatus::DoneCandidate);
        let acceptance = criterion_command_result_readonly_checked(
            &task.acceptance,
            CommandRole::AuthoritativeAcceptance,
            &opts.workspace,
            opts.network,
            opts.fs_write_fence,
        )
        .await?;
        let decision = settle_report_decision(&report, acceptance);
        let reason = decision_reason(&decision);
        recorder.emit(
            "plan.task.decision",
            json!({ "task": task.id, "decision": &decision, "reason": reason, "phase": "finalize" }),
        )?;
        match &decision {
            TaskDecision::PassedByAcceptance { .. } => {
                results.passed_ids.push(task.acceptance.id.clone());
            }
            TaskDecision::FailedByAcceptance { acceptance, .. } => {
                results.failed_ids.push(task.acceptance.id.clone());
                results.code_red.push(acceptance.clone());
                results.code_unmet.push(json!({ "kind": "task_acceptance", "task": task.id, "label": format!("task {} acceptance", task.id), "decision": &decision, "reason": decision_reason(&decision) }));
            }
            TaskDecision::UnvalidatedInfraError { signature, .. } => {
                results.infra.push(json!({ "kind": "task_acceptance", "task": task.id, "signature": signature, "decision": &decision }));
            }
            TaskDecision::StoppedUnvalidated { reason } => {
                results.stopped.push(json!({ "kind": "task_acceptance", "task": task.id, "stopped_unvalidated": reason }));
            }
            TaskDecision::FailedByPolicy { .. } => {
                results.policy_unmet.push(json!({
                    "kind": "task_policy",
                    "task": task.id,
                    "decision": &decision,
                    "reason": decision_reason(&decision),
                }));
            }
        }
    }
    Ok(())
}

pub(super) async fn run_artifact_advisories(
    state: &RunState,
    opts: &PlanRunOptions,
    results: &mut CompletionResults,
) -> Result<Vec<Value>> {
    let mut advisory_pending = Vec::new();
    for task in &state.worklist {
        if !matches!(task.status, TaskStatus::Done) {
            continue;
        }
        if task.acceptance_kind != AcceptanceKind::ChangeRequired {
            continue;
        }
        let Some(artifact) = &task.artifact_check else {
            continue;
        };
        match criterion_command_result_readonly_checked(
            artifact,
            CommandRole::AuthoritativeAcceptance,
            &opts.workspace,
            opts.network,
            opts.fs_write_fence,
        )
        .await?
        {
            AcceptanceResult::Pass { .. } => {}
            AcceptanceResult::PolicyFailure {
                reason,
                changed_files,
                acceptance,
            } => {
                results.policy_unmet.push(json!({
                    "kind": "finalize_artifact_policy",
                    "task": task.id,
                    "reason": reason,
                    "changed_files": changed_files,
                    "evidence": acceptance,
                }));
            }
            AcceptanceResult::CodeRed { acceptance } => {
                advisory_pending.push(json!({
                    "kind": "finalize_artifact_advisory",
                    "task": task.id,
                    "artifact": artifact.id,
                    "result": "code_red",
                    "evidence": acceptance,
                }));
            }
            AcceptanceResult::NotRun { reason } => {
                advisory_pending.push(json!({
                    "kind": "finalize_artifact_advisory",
                    "task": task.id,
                    "artifact": artifact.id,
                    "result": "not_run",
                    "detail": reason,
                }));
            }
            AcceptanceResult::InfraRed {
                signature,
                acceptance,
            } => {
                advisory_pending.push(json!({
                    "kind": "finalize_artifact_advisory",
                    "task": task.id,
                    "artifact": artifact.id,
                    "result": "infra_red",
                    "detail": signature,
                    "evidence": acceptance,
                }));
            }
        }
    }
    Ok(advisory_pending)
}
