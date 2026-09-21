//! Main-loop steps extracted from `run_plan_loop` without changing event or state-transition order.

use std::path::Path;

use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::journal::RunPaths;
use crate::orchestrator::run_solo_task;
use crate::plan::contract::{AcceptanceKind, PlanTask, TaskDecision, TaskReport, TaskStatus};
use crate::plan::executor_bridge::{task_report_from_child, task_to_goal_contract};
use crate::plan::probe::stale_scope_paths;
use crate::plan::state::RunState;
use crate::plan::write_audit::{
    capture_baseline, changed_paths_since, classify_violations, partition_formatting_violations,
    TaskScope, WriteBaseline,
};
use crate::provider::{ProviderCapabilities, ProviderClient};

use super::{
    child_run_options, decision_reason, emit_advisory_if_any, finalize_completion_outcome,
    finish_finalize_outcome, handle_overall_replan, needs_decision_result, run_preflight_gate,
    run_task_acceptance, save_state, FinalizeOutcome, PlanRunOptions, PreflightAction,
    ReplanLoopAction, TaskPreflightAction,
};

pub(super) async fn handle_all_tasks_done<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
) -> Result<ReplanLoopAction> {
    let outcome = finalize_completion_outcome(state, opts, recorder).await?;
    match outcome {
        FinalizeOutcome::NeedsReplan {
            code_red, snapshot, ..
        } => {
            handle_overall_replan(
                provider, opts, state, state_path, recorder, snapshot, code_red,
            )
            .await
        }
        other => Ok(ReplanLoopAction::Return(finish_finalize_outcome(
            other, state, opts, recorder,
        )?)),
    }
}

pub(super) fn pick_next_task(
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
) -> Result<Option<PlanTask>> {
    let task = state
        .runnable_next()
        .expect("PlanTerminal::Running → runnable_next is Some")
        .clone();

    // Recheck scope before running (§4.1: before marking InProgress/counting steps; expired -> Blocked; no step consumed)
    let stale = stale_scope_paths(&opts.workspace, &task.files_scope);
    if stale.is_empty() {
        return Ok(Some(task));
    }

    let reason = format!(
        "files_scope 落空（计划过期·所在目录已不存在）：{}",
        stale.join(", ")
    );
    recorder.emit(
        "plan.task.blocked",
        json!({ "task": task.id, "reason": "scope_stale", "stale_paths": stale }),
    )?;
    state.mark_status(&task.id, TaskStatus::Blocked { reason });
    save_state(state_path, state)?;
    Ok(None)
}

pub(super) async fn prepare_task<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
    task: &PlanTask,
) -> Result<TaskPreflightAction> {
    // Capture baseline (pre-work gate; reuse for post-execution audit · BLOCK 1)
    let baseline = capture_baseline(&opts.workspace)?;

    // Pre-work acceptance gate (§4: run only when gate is open and change_required; invariant uses global health-check and bypasses it)
    if state.preflight_gate && task.acceptance_kind == AcceptanceKind::ChangeRequired {
        return match run_preflight_gate(
            provider, opts, state, state_path, recorder, task, &baseline,
        )
        .await?
        {
            PreflightAction::Proceed => Ok(TaskPreflightAction::Proceed(baseline)),
            PreflightAction::Continue => Ok(TaskPreflightAction::Continue),
            PreflightAction::Return(result) => Ok(TaskPreflightAction::Return(result)),
        };
    }

    Ok(TaskPreflightAction::Proceed(baseline))
}

pub(super) async fn run_child_task_and_report<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    caps: &ProviderCapabilities,
    crate_roots: &[String],
    task: &PlanTask,
    baseline: &WriteBaseline,
    recorder: &mut EventRecorder,
) -> Result<TaskReport> {
    let child_id = format!("{}__{}", opts.plan_run_id, task.id);
    let scope = TaskScope::from_task(task).with_crate_roots(crate_roots.to_vec());
    let task_contract = task_to_goal_contract(task);
    let child_opts = child_run_options(opts, caps, task, &child_id);

    let child = run_solo_task(
        provider,
        Box::new(crate::judge::NoopJudge),
        child_opts,
        Some(task_contract),
        Some(scope.clone()),
    )
    .await?;

    let changed_files = changed_paths_since(&opts.workspace, baseline)?;
    let raw_violations = classify_violations(&changed_files, &scope);
    // fmt-scope: downgrade out-of-list formatting-only changes to advisory (red lines/real content/uncertainty remain violations).
    let (violations, fmt_advisories) =
        partition_formatting_violations(&opts.workspace, baseline, &scope, raw_violations).await;
    if !fmt_advisories.is_empty() {
        recorder.emit(
            "plan.task.scope_formatting_advisory",
            json!({
                "task": task.id,
                "files": fmt_advisories.iter().map(|v| v.path.clone()).collect::<Vec<_>>(),
                "note": "顺手排版了名单外文件·已放行·纯排版（formatter 副作用）",
            }),
        )?;
    }
    let child_events = RunPaths::new(&opts.journal_root, &child_id).events_path;
    let report = task_report_from_child(task, &child, &child_events, changed_files, violations)?;

    recorder.emit("plan.task.report", serde_json::to_value(&report)?)?;
    Ok(report)
}

pub(super) async fn settle_task_decision(
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
    task: &PlanTask,
    report: &TaskReport,
) -> Result<ReplanLoopAction> {
    // Authoritative acceptance always runs (two stages): child outcome / journal verdict only enter the report, not the acceptance-run gate.
    let decision = run_task_acceptance(
        task,
        report,
        &opts.workspace,
        opts.network,
        opts.fs_write_fence,
    )
    .await?;
    recorder.emit(
        "plan.task.decision",
        json!({ "task": task.id, "decision": &decision, "reason": decision_reason(&decision) }),
    )?;

    match decision {
        TaskDecision::PassedByAcceptance { ref advisory, .. } => {
            recorder.emit("plan.task.done", json!({ "task": task.id }))?;
            emit_advisory_if_any(recorder, &task.id, advisory.as_ref())?;
            state.mark_status(&task.id, TaskStatus::Done);
            save_state(state_path, state)?;
            Ok(ReplanLoopAction::Continue)
        }
        TaskDecision::FailedByAcceptance { .. } => {
            let reason = decision_reason(&decision);
            recorder.emit(
                "plan.task.blocked",
                json!({ "task": task.id, "reason": reason }),
            )?;
            state.mark_status(&task.id, TaskStatus::Blocked { reason });
            save_state(state_path, state)?;
            Ok(ReplanLoopAction::Continue)
        }
        TaskDecision::FailedByPolicy { .. } => {
            let reason = decision_reason(&decision);
            recorder.emit(
                "run.needs_decision",
                json!({ "reason": "failed_by_policy", "task": task.id, "detail": reason }),
            )?;
            state.mark_status(&task.id, TaskStatus::Blocked { reason });
            save_state(state_path, state)?;
            Ok(ReplanLoopAction::Return(needs_decision_result(opts)))
        }
        TaskDecision::UnvalidatedInfraError { signature, .. } => {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "infra_red",
                    "task": task.id,
                    "signature": signature,
                    "next_step": "环境抽风(网络/超时/锁)·非代码红·修环境后 resume·别当失败再规划",
                }),
            )?;
            save_state(state_path, state)?; // Keep InProgress; resume by completing acceptance first
            Ok(ReplanLoopAction::Return(needs_decision_result(opts)))
        }
        TaskDecision::StoppedUnvalidated { reason } => {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "stopped_unvalidated",
                    "task": task.id,
                    "detail": reason,
                    "next_step": "验收未跑成·先修验收环境/命令后 resume",
                }),
            )?;
            save_state(state_path, state)?;
            Ok(ReplanLoopAction::Return(needs_decision_result(opts)))
        }
    }
}
