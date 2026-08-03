//! run_plan：两层编排外层·包 run_solo（不重写）·串行领原子任务（spec §4·第一刀脊柱）。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::Result;
use crate::events::{EventRecorder, OutputMode};
use crate::goal::{Criterion, GoalState, NetworkPolicy};
use crate::guardrails::ContractPolicy;
use crate::journal::RunPaths;
use crate::orchestrator::{
    run_solo_task, ControlInputKind, RunOptions, RunOutcome, RunResult, DEFAULT_VERIFY_EVERY,
    DEFAULT_WATCHDOG_REPEAT, MIN_TASK_TURN_BUDGET,
};
use crate::plan::contract::{
    decide_task, merge_task_acceptance, AcceptanceKind, AcceptanceResult, ChangeSet,
    ChildRunOutcome, CommandEvidence, CommandRole, PlanTask, RemediationMeta, TaskDecision,
    TaskNarrative, TaskReport, TaskReportStatus, TaskStatus,
};
use crate::plan::executor_bridge::{task_report_from_child, task_to_goal_contract};
use crate::plan::false_red::criterion_command_result_with_fence;
use crate::plan::planning::{fold_plan_attempt, PlanStep};
use crate::plan::preflight::{
    classify_preflight, decide_preflight, PreflightStep, RefineKind, DEFAULT_MAX_PREFLIGHT_REFINE,
};
use crate::plan::probe::{detect_invariants, stale_scope_paths};
use crate::plan::replan::{
    decide_replan, failure_fingerprint, fingerprint_hard_dedup_safe, gen_remediation_id,
    is_duplicate_task, validate_remediation_append, ReplanStep,
};
use crate::plan::state::{PlanTerminal, RunState, Trigger, UnmetSnapshot};
use crate::plan::write_audit::{
    capture_baseline, changed_paths_since, classify_violations, partition_formatting_violations,
    TaskScope, WriteBaseline,
};
use crate::provider::{ChatMessage, ProviderClient};
use crate::shell::PermissionPolicy;

/// Planner system 提示：含 "WORKLIST" 标记（mock 测试据此辨 planner 调用 vs child 调用）。
const PLANNER_SYSTEM: &str = "You are the harness PLANNER. Decompose the goal into a WORKLIST of \
atomic tasks and reply with ONLY a JSON object of this exact shape: \
{\"tasks\":[{\"id\":\"t1\",\"intent\":\"single-concern sentence\",\"files_scope\":[\"concrete/path\"],\
\"acceptance_cmd\":\"a runnable read-only TEST command; behavior/regression lane; run it always as its own command\",\
\"artifact_check_cmd\":\"a read-only fail-to-pass STRUCTURAL check; advisory, not a hard gate; grep -rq the whole files_scope directory by stable symbol name, not a single file\",\
\"max_turns\":8,\
\"forbidden_scope\":[],\"expected_diff_shape\":\"which kinds of symbols/sites this change is expected to \
touch; the executor will grep and compile to enumerate the actual sites within scope\",\
\"stop_conditions\":[\"stop and report ONLY for a genuine boundary change: a required site falls outside \
files_scope, or you must add a new dependency, change the goal, or touch a different public contract or \
concern\"],\
\"depends_on\":[]}]}. \
RIPPLE CHANGES: editing one definition — a public type, field, function or method signature, enum \
variant, trait/interface, constructor, serialization format, or config contract — forces matching edits \
at every use site across the repo. You cannot see the repo, so you cannot list those sites. Treat a \
ripple as ONE atomic task even when it spans many files; never split the definition edit from the \
call-site / construction-site / test / fixture updates it forces, and never give such a task a \
single-file files_scope. Set files_scope to the smallest directory set (e.g. [\"src\",\"tests\"]) that \
plausibly contains the definition AND its use sites — broad enough for the executor to chase every site, \
no broader. The executor can grep and compile; let it enumerate the sites. Finding another use site of \
THIS same change INSIDE the declared files_scope is not a scope change — it is in scope. Split only \
independent concerns; do NOT split by file when files must change together for the repo to compile or \
tests to pass, and do NOT give broad overlapping directory scopes to multiple independent tasks. Use a \
single-file files_scope only for a truly local edit that changes no shared contract. \
Rules: id must match [A-Za-z0-9_-]+ (no slashes/dots); files_scope must be a non-empty small set of \
concrete named files and/or directories (no globs, no empty, no absolute, no '..'); acceptance_cmd must \
be runnable and idempotent (no writes); declare depends_on for ordering; two tasks without a dependency \
path must not write overlapping paths. Two separate lanes: artifact_check_cmd is the fail-to-pass \
driver and advisory, not a hard gate (red before the change, green only after; read-only/idempotent). \
It MUST match by stable symbol name and grep -rq the whole files_scope directory, not a single file; \
never use type spelling, import path, or whitespace. If it stays red, the task is not failed mid-run; \
the engine re-checks it at finalize and only then asks a human. The real gate is the behavior test plus \
human overall criteria. Keep the two lanes as two separate commands; never chain them into one shell \
command. Change-required tasks MUST produce artifact_check_cmd; omit it ONLY for a pure-refactor or \
invariant task with no new symbol, and mark acceptance_kind invariant. Examples by change kind — add \
field/symbol: grep the new name in the directory; rename: grep the new name appears; delete: grep the \
old name is gone from the directory; change a config value: grep the new value present. Output JSON \
only, no prose.";

const REMEDIATION_SYSTEM: &str = "You are the harness REMEDIATION PLANNER. Produce a narrow WORKLIST \
that fixes only the provided failing evidence. Reply with ONLY the same JSON object schema as the \
normal planner: {\"tasks\":[{\"id\":\"local_hint\",\"intent\":\"single concrete repair\",\
\"files_scope\":[\"concrete/path\"],\"acceptance_cmd\":\"read-only shell check\",\
\"artifact_check_cmd\":\"read-only fail-to-pass structural check; advisory, not a hard gate; grep -rq the whole files_scope directory by stable symbol name, not a single file\",\
\"max_turns\":4,\"forbidden_scope\":[],\"expected_diff_shape\":\"specific repair\",\
\"stop_conditions\":[\"stop only for a genuine boundary change outside this repair, not for another \
in-scope use site of the same repair\"],\"depends_on\":[]}]}. Rules: files_scope must be \
concrete and non-empty; prefer one task; do not depend on the blocked parent task. If the repair itself \
ripples across many sites (e.g. a missing field at every construction site), keep it ONE task with a \
directory-level files_scope (e.g. [\"src\",\"tests\"]) so the executor can fix every site; do not scope \
it to a single file. Two separate lanes: artifact_check_cmd is the fail-to-pass driver and advisory, \
not a hard gate (red before the repair, green only after; read-only/idempotent). It MUST match by stable \
symbol name and grep -rq the whole files_scope directory, not a single file; never use type spelling, \
import path, or whitespace. If it stays red, the task is not failed mid-run; the engine re-checks it at \
finalize and only then asks a human. The real gate is the behavior test plus human overall criteria. \
Keep the two lanes as two separate commands; never chain them into one shell command. Change-required \
tasks MUST produce artifact_check_cmd; omit it ONLY for a pure-refactor or invariant task with no new \
symbol, and mark acceptance_kind invariant. Examples by change kind — add symbol: grep the new name in \
the directory; rename: grep the new name appears; delete: grep the old name is gone from the directory; \
config value: grep the new value present. Output JSON only.";

const PREFLIGHT_REFINE_SYSTEM: &str = "You are the harness PRE-FLIGHT acceptance refiner. A task's \
acceptance command passed BEFORE any work was done, or could not be run, so the task cannot be trusted \
to detect whether the work actually happened. Produce a STRONGER artifact_check_cmd only (leave the \
behavior test alone): it MUST be red now (fail before the work), green only after, read-only/idempotent, \
within the original files_scope, advisory, not a hard gate, and match by stable symbol name with grep -rq \
over the whole files_scope directory, not a single file; never use type spelling/import path/whitespace. \
Keep the two lanes as two separate commands; never chain them into one shell command. Reply with ONLY the same JSON worklist schema as the planner: {\"tasks\":[{\"id\":\"local_hint\",\
\"intent\":\"...\",\"files_scope\":[\"concrete/path\"],\"acceptance_cmd\":\"original behavior test unchanged\",\
\"artifact_check_cmd\":\"read-only fail-to-pass structural check; advisory, not a hard gate; grep -rq the whole files_scope directory by stable symbol name, not a single file\",\"max_turns\":4,\
\"forbidden_scope\":[],\"expected_diff_shape\":\"...\",\"stop_conditions\":[],\"depends_on\":[]}]}. \
Output JSON only, no prose.";

pub const DEFAULT_MAX_REPLAN_ROUNDS: usize = 3;

/// run_plan 的输入旋钮（plan 级·与 child 的 RunOptions 分开）。
pub struct PlanRunOptions {
    pub objective: String,
    /// 总验收细项（1b 只存不跑·桩·1c 接真总验收）。
    pub checks: Vec<Criterion>,
    pub workspace: PathBuf,
    pub journal_root: PathBuf,
    pub plan_run_id: String,
    pub permission: PermissionPolicy,
    pub network: NetworkPolicy,
    pub fs_read_scope: crate::fs_scope::FsReadScope,
    pub fs_write_fence: crate::exec::sandbox::FsWriteFence,
    pub output_mode: OutputMode,
    /// 评审闸打回上限 K。
    pub max_review_attempts: usize,
    /// 计划级总预算（任务执行次数顶·spec §4.8）。
    pub max_plan_steps: usize,
    /// 自救补救轮数顶（补救任务执行仍照常消耗 max_plan_steps）。
    pub max_replan_rounds: usize,
    pub contract_policy: ContractPolicy,
    pub max_eval_attempts: usize,
    /// child run 回合数封顶（F2·CLI --max-turns·>0 时给 task.max_turns 封顶）。
    pub default_task_max_turns: usize,
    /// 开工前验收闸开关（CLI --preflight-gate·默认 on·落进 RunState·resume 用落盘值·FIX 7）。
    pub preflight_gate: bool,
}

#[derive(Debug)]
enum PlanOutcome {
    Tasks(Vec<PlanTask>),
    Escalated,
}
/// 外层入口：拆计划 → 落盘 → 串行跑（spec §4.1）。
pub async fn run_plan<P: ProviderClient + Clone>(
    provider: P,
    opts: PlanRunOptions,
) -> Result<RunResult> {
    crate::exec::sandbox::validate_write_fence(opts.fs_write_fence)?;
    let paths = RunPaths::new(&opts.journal_root, &opts.plan_run_id);
    paths.create_dirs()?;
    let mut recorder = EventRecorder::new(
        opts.plan_run_id.clone(),
        None,
        Some(opts.workspace.to_string_lossy().into_owned()),
        &paths.events_path,
        opts.output_mode,
    )?;
    recorder.emit(
        "run.started",
        json!({ "mode": "plan", "objective": opts.objective, "workspace": opts.workspace }),
    )?;
    if let Some(result) = super::answer_only::maybe_complete(&opts, &mut recorder)? {
        return Ok(result);
    }
    let worklist = match plan_worklist(&provider, &opts, &mut recorder).await? {
        PlanOutcome::Tasks(t) => t,
        PlanOutcome::Escalated => return Ok(needs_decision_result(&opts)),
    };

    let mut checks = opts.checks.clone();
    checks.extend(detect_invariants(&opts.workspace)); // per-language 全局不变量（§3.4·进总验收·落盘）
    let goal_contract = GoalState::new(opts.objective.clone(), opts.checks.clone()).contract;
    let mut state = RunState::new(goal_contract, worklist, checks);
    state.preflight_gate = opts.preflight_gate;
    let state_path = paths.run_dir.join("plan_state.json");

    run_plan_loop(provider, &opts, state, &state_path, &mut recorder).await
}
/// 崩溃重启：载落盘 RunState（含 steps_used）·把「进行中」任务重跑 acceptance 当真相·再接着跑（spec §4.7·B5）。
/// 没落盘 → 当全新 run_plan（含 Planner）。1b 单次重跑·1c 加假红防护。
pub async fn resume_plan<P: ProviderClient + Clone>(
    provider: P,
    opts: PlanRunOptions,
) -> Result<RunResult> {
    crate::exec::sandbox::validate_write_fence(opts.fs_write_fence)?;
    let paths = RunPaths::new(&opts.journal_root, &opts.plan_run_id);
    let state_path = paths.run_dir.join("plan_state.json");
    let mut state: RunState = match std::fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(_) => return run_plan(provider, opts).await,
    };

    paths.create_dirs()?;
    let mut recorder = EventRecorder::new(
        opts.plan_run_id.clone(),
        None,
        Some(opts.workspace.to_string_lossy().into_owned()),
        &paths.events_path,
        opts.output_mode,
    )?;
    recorder.emit(
        "run.resumed",
        json!({ "mode": "plan", "steps_used": state.steps_used }),
    )?;

    if let Some(task) = state.worklist.iter().find(|task| {
        task.acceptance_kind == AcceptanceKind::ChangeRequired
            && task.artifact_check.is_none()
            && (matches!(&task.status, TaskStatus::Pending | TaskStatus::InProgress)
                || matches!(
                    &task.status,
                    TaskStatus::Blocked { reason } if reason.starts_with("failed_by_acceptance:")
                ))
    }) {
        recorder.emit(
            "run.needs_decision",
            json!({
                "reason": "resume_missing_driver",
                "task": task.id,
                "next_step": "legacy 任务缺结构检查 driver·无法自动判「是否真干完」·重新规划/拆解该任务后再 resume",
            }),
        )?;
        save_state(&state_path, &state)?;
        return Ok(needs_decision_result(&opts));
    }

    let in_progress: Vec<PlanTask> = state
        .worklist
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::InProgress))
        .cloned()
        .collect();
    for task in in_progress {
        let report = synthetic_report_for_task(&task, TaskReportStatus::StoppedUnvalidated);
        let decision = run_task_acceptance(
            &task,
            &report,
            &opts.workspace,
            opts.network,
            opts.fs_write_fence,
        )
        .await?;
        let reason = decision_reason(&decision);
        recorder.emit(
            "plan.task.decision",
            json!({ "task": task.id, "decision": &decision, "reason": reason, "phase": "resume" }),
        )?;
        match &decision {
            TaskDecision::PassedByAcceptance { advisory, .. } => {
                recorder.emit(
                    "plan.task.reverified",
                    json!({ "task": task.id, "verdict": "pass" }),
                )?;
                emit_advisory_if_any(&mut recorder, &task.id, advisory.as_ref())?;
                state.mark_status(&task.id, TaskStatus::Done);
            }
            TaskDecision::FailedByAcceptance { .. } => {
                recorder.emit(
                    "plan.task.reverified",
                    json!({ "task": task.id, "verdict": "code_red" }),
                )?;
                state.mark_status(&task.id, TaskStatus::Pending); // 当没做·重领
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "resume_code_red", "task": task.id,
                            "next_step": "resume 重验 code red·任务已退回 Pending，下一轮重新执行" }),
                )?;
                save_state(&state_path, &state)?;
                return Ok(needs_decision_result(&opts));
            }
            TaskDecision::UnvalidatedInfraError { signature, .. } => {
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "infra_red", "task": task.id, "signature": signature,
                            "next_step": "resume 重验遇环境抽风·修环境后再 resume" }),
                )?;
                save_state(&state_path, &state)?;
                return Ok(needs_decision_result(&opts));
            }
            TaskDecision::StoppedUnvalidated { reason } => {
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "stopped_unvalidated", "task": task.id, "detail": reason,
                            "next_step": "resume 重验没跑成·修验收命令/环境后再 resume" }),
                )?;
                save_state(&state_path, &state)?;
                return Ok(needs_decision_result(&opts));
            }
            TaskDecision::FailedByPolicy { .. } => {
                let reason = decision_reason(&decision);
                state.mark_status(
                    &task.id,
                    TaskStatus::Blocked {
                        reason: reason.clone(),
                    },
                );
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "failed_by_policy", "task": task.id, "detail": reason }),
                )?;
                save_state(&state_path, &state)?;
                return Ok(needs_decision_result(&opts));
            }
        }
    }
    save_state(&state_path, &state)?;

    run_plan_loop(provider, &opts, state, &state_path, &mut recorder).await
}

/// Planner 调用 + 评审闸折账循环（spec §3.1/§4.1 第 1–2 步）。
async fn plan_worklist<P: ProviderClient>(
    provider: &P,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
) -> Result<PlanOutcome> {
    let k = opts.max_review_attempts.max(1);
    let mut messages = vec![
        ChatMessage::system(PLANNER_SYSTEM),
        ChatMessage::user(format!(
            "Goal:\n{}\n\nProduce the JSON worklist now.",
            opts.objective
        )),
    ];
    for attempt in 0..k {
        let resp = provider.next_turn(&messages, &[], recorder).await?;
        match fold_plan_attempt(&resp.text, attempt, k) {
            PlanStep::Accept { tasks } => {
                recorder.emit(
                    "plan.worklist.accepted",
                    json!({ "tasks": tasks.len(), "attempt": attempt }),
                )?;
                return Ok(PlanOutcome::Tasks(tasks));
            }
            PlanStep::Retry { feedback, reasons } => {
                recorder.emit(
                    "plan.worklist.bounced",
                    json!({ "attempt": attempt, "reasons": reasons }),
                )?;
                messages.push(ChatMessage::assistant(resp.text, None, vec![]));
                messages.push(ChatMessage::user(feedback));
            }
            PlanStep::Escalate { reasons } => {
                recorder.emit(
                    "run.needs_decision",
                    json!({
                        "reason": "plan_unreviewable", "attempts": attempt + 1, "unmet": reasons,
                        "next_step": "用户改目标 / 人工拆解 / 进第二刀（再规划）",
                    }),
                )?;
                return Ok(PlanOutcome::Escalated);
            }
        }
    }
    recorder.emit(
        "run.needs_decision",
        json!({ "reason": "plan_unreviewable", "next_step": "planner 未收敛" }),
    )?;
    Ok(PlanOutcome::Escalated)
}

#[allow(clippy::too_many_arguments)]
async fn plan_remediation<P: ProviderClient>(
    provider: &P,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
    parent: &str,
    evidence: &[CommandEvidence],
    completed_ids: &[String],
    existing_worklist: &[PlanTask],
    round: usize,
    short_memory: Option<String>,
) -> Result<PlanOutcome> {
    let k = opts.max_review_attempts.max(1);
    let evidence_json = serde_json::to_string_pretty(evidence)?;
    let fingerprint = evidence
        .iter()
        .map(failure_fingerprint)
        .collect::<Vec<_>>()
        .join(" | ");
    let memory = short_memory.unwrap_or_else(|| "none".to_string());

    let mut messages = vec![
        ChatMessage::system(REMEDIATION_SYSTEM),
        ChatMessage::user(format!(
            "Original objective:\n{}\n\nParent:\n{}\n\nCompleted task ids:\n{}\n\nFailure evidence:\n{}\n\nPrevious remediation memory:\n{}\n\nProduce the remediation WORKLIST JSON now.",
            opts.objective,
            parent,
            completed_ids.join(", "),
            evidence_json,
            memory
        )),
    ];

    for attempt in 0..k {
        let resp = provider.next_turn(&messages, &[], recorder).await?;
        match fold_plan_attempt(&resp.text, attempt, k) {
            PlanStep::Accept { tasks } => {
                let stamped =
                    stamp_remediation_tasks(tasks, parent, &fingerprint, round, existing_worklist);
                recorder.emit(
                    "plan.replan.planned",
                    json!({ "parent": parent, "round": round, "tasks": stamped.len(), "attempt": attempt }),
                )?;
                return Ok(PlanOutcome::Tasks(stamped));
            }
            PlanStep::Retry { feedback, reasons } => {
                recorder.emit(
                    "plan.replan.bounced",
                    json!({ "parent": parent, "round": round, "attempt": attempt, "reasons": reasons }),
                )?;
                messages.push(ChatMessage::assistant(resp.text, None, vec![]));
                messages.push(ChatMessage::user(feedback));
            }
            PlanStep::Escalate { reasons } => {
                recorder.emit(
                    "plan.replan.escalated",
                    json!({ "parent": parent, "round": round, "reason": "plan_unreviewable", "reasons": reasons }),
                )?;
                return Ok(PlanOutcome::Escalated);
            }
        }
    }

    Ok(PlanOutcome::Escalated)
}

fn stamp_remediation_tasks(
    mut tasks: Vec<PlanTask>,
    parent: &str,
    evidence_fingerprint: &str,
    round: usize,
    existing_worklist: &[PlanTask],
) -> Vec<PlanTask> {
    let attempt_no = existing_worklist
        .iter()
        .filter_map(|t| t.remediation.as_ref())
        .filter(|m| m.parent == parent && m.evidence_fingerprint == evidence_fingerprint)
        .count()
        + 1;

    let mut id_map = std::collections::HashMap::new();
    for (idx, task) in tasks.iter().enumerate() {
        id_map.insert(task.id.clone(), gen_remediation_id(parent, round, idx + 1));
    }

    for task in &mut tasks {
        let old_id = task.id.clone();
        let new_id = id_map.get(&old_id).expect("id map created").clone();
        task.id = new_id.clone();
        task.acceptance.id = format!("{new_id}_acc");
        task.acceptance.claim = format!("acceptance for task {new_id}");
        if let Some(art) = task.artifact_check.as_mut() {
            art.id = format!("{new_id}_art");
            art.claim = format!("artifact check for task {new_id}");
        }
        for dep in &mut task.depends_on {
            if let Some(mapped) = id_map.get(dep) {
                *dep = mapped.clone();
            }
        }
        task.remediation = Some(RemediationMeta {
            parent: parent.to_string(),
            evidence_fingerprint: evidence_fingerprint.to_string(),
            attempt_no,
            round,
        });
        task.status = TaskStatus::Pending;
    }

    tasks
}

/// 开工前闸退回规划（独立路·不蹭第二刀 remediation·spec §4.3 步 2/§4.6）。
async fn plan_preflight_refine<P: ProviderClient>(
    provider: &P,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
    original: &PlanTask,
    kind: RefineKind,
    root: &str,
    round: usize,
) -> Result<PlanOutcome> {
    let k = opts.max_review_attempts.max(1);
    let situation = match kind {
        RefineKind::Strengthen => "acceptance passed before execution (too weak / redundant)",
        RefineKind::Legal => "acceptance could not be run (not an approved, runnable check)",
    };
    let original_cmd = match original.artifact_check.as_ref().map(|c| &c.verifier) {
        Some(crate::goal::Verifier::Verifiable { check_cmd, .. }) => check_cmd.clone(),
        Some(crate::goal::Verifier::Judgmental { rubric }) => format!("judgmental: {rubric}"),
        None => "(no artifact_check)".to_string(),
    };
    let mut messages = vec![
        ChatMessage::system(PREFLIGHT_REFINE_SYSTEM),
        ChatMessage::user(format!(
            "Original objective:\n{}\n\nOriginal task intent:\n{}\n\nOriginal files_scope:\n{}\n\nOriginal artifact_check_cmd:\n{}\n\nProblem:\n{}\n\nProduce ONE stronger replacement task as JSON now.",
            opts.objective,
            original.intent,
            original.files_scope.join(", "),
            original_cmd,
            situation,
        )),
    ];

    for attempt in 0..k {
        let resp = provider.next_turn(&messages, &[], recorder).await?;
        match fold_plan_attempt(&resp.text, attempt, k) {
            PlanStep::Accept { tasks } => {
                // 必须恰好一个替代任务（R2-B2）：评审闸跑在 stamp 前的原始 scope 上，stamp 后多任务会被
                // 统一覆写成原任务 scope → 互相重叠、绕过评审闸的「无依赖任务不得写重叠路径」检查。限定一个即无此问题。
                if tasks.len() != 1 {
                    recorder.emit(
                        "plan.preflight.refine_bounced",
                        json!({ "root": root, "round": round, "attempt": attempt,
                                "reasons": ["preflight refine must produce exactly one replacement task"] }),
                    )?;
                    if attempt + 1 >= k {
                        recorder.emit(
                            "plan.preflight.refine_escalated",
                            json!({ "root": root, "round": round, "reason": "not_exactly_one_replacement" }),
                        )?;
                        return Ok(PlanOutcome::Escalated);
                    }
                    messages.push(ChatMessage::assistant(resp.text, None, vec![]));
                    messages.push(ChatMessage::user(
                        "Produce EXACTLY ONE replacement task in the worklist.".to_string(),
                    ));
                    continue;
                }
                let stamped = stamp_preflight_refine_tasks(tasks, original, root, round);
                recorder.emit(
                    "plan.preflight.refine_planned",
                    json!({ "root": root, "round": round, "tasks": stamped.len(), "attempt": attempt }),
                )?;
                return Ok(PlanOutcome::Tasks(stamped));
            }
            PlanStep::Retry { feedback, reasons } => {
                recorder.emit(
                    "plan.preflight.refine_bounced",
                    json!({ "root": root, "round": round, "attempt": attempt, "reasons": reasons }),
                )?;
                messages.push(ChatMessage::assistant(resp.text, None, vec![]));
                messages.push(ChatMessage::user(feedback));
            }
            PlanStep::Escalate { reasons } => {
                recorder.emit(
                    "plan.preflight.refine_escalated",
                    json!({ "root": root, "round": round, "reason": "plan_unreviewable", "reasons": reasons }),
                )?;
                return Ok(PlanOutcome::Escalated);
            }
        }
    }
    Ok(PlanOutcome::Escalated)
}

/// stamp 替代任务：harness 盖 id·acceptance.id 跟着改·确定性继承原任务 scope/forbidden/depends_on·
/// 强制 acceptance_kind=ChangeRequired（不信模型给的 scope·BLOCK-7）·Pending。
fn stamp_preflight_refine_tasks(
    mut tasks: Vec<PlanTask>,
    original: &PlanTask,
    root: &str,
    round: usize,
) -> Vec<PlanTask> {
    let mut id_map = std::collections::HashMap::new();
    for (idx, task) in tasks.iter().enumerate() {
        id_map.insert(task.id.clone(), gen_remediation_id(root, round, idx + 1));
    }
    for task in &mut tasks {
        let old_id = task.id.clone();
        let new_id = id_map.get(&old_id).expect("id map created").clone();
        task.id = new_id.clone();
        task.acceptance = original.acceptance.clone();
        task.acceptance.id = format!("{new_id}_acc");
        task.acceptance.claim = format!("acceptance for task {new_id}");
        if let Some(art) = task.artifact_check.as_mut() {
            art.id = format!("{new_id}_art");
            art.claim = format!("artifact check for task {new_id}");
        }
        task.files_scope = original.files_scope.clone();
        task.forbidden_scope = original.forbidden_scope.clone();
        task.acceptance_kind = crate::plan::contract::AcceptanceKind::ChangeRequired;
        task.depends_on = original.depends_on.clone();
        task.status = TaskStatus::Pending;
    }
    tasks
}

fn replan_memory(state: &RunState) -> Option<String> {
    state.last_snapshot.as_ref().map(|s| {
        format!(
            "last trigger={:?}; checked={:?}; passed={:?}; failed={:?}; rounds={}",
            s.trigger, s.checked_ids, s.passed_ids, s.failed_ids, state.replan_rounds
        )
    })
}

/// 主循环：取下一个可跑 pending → 跑 → 审计 → 判完成 → 折账 → 三支出口（spec §4.1 第 4–7 步）。
/// 抽成独立函数·T8 的 resume_plan 复用。预算计数走 state.steps_used（持久化·B5）。
async fn run_plan_loop<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    mut state: RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
) -> Result<RunResult> {
    let caps = provider.capabilities();
    save_state(state_path, &state)?;

    // C1：crate-root 前缀（terrain 探一次·全 task 复用）——让 Planner 的 crate-相对短 scope
    // 对得上模型写的 worktree-相对全路径。实时闸 + 事后审计共用同一份带 crate 根的 scope。
    let crate_roots: Vec<String> = crate::terrain::detect(&opts.workspace)
        .project_roots
        .iter()
        .map(|r| r.rel.to_string_lossy().into_owned())
        .collect();

    loop {
        match state.terminal() {
            // 全 done → 总验收【桩】（1b 信 per-task done·1c 接真总验收 + 整盘重验 + per-language）
            PlanTerminal::AllTasksDone => {
                let outcome = finalize_completion_outcome(&state, opts, recorder).await?;
                match outcome {
                    FinalizeOutcome::NeedsReplan {
                        code_red, snapshot, ..
                    } => {
                        match handle_overall_replan(
                            provider.clone(),
                            opts,
                            &mut state,
                            state_path,
                            recorder,
                            snapshot,
                            code_red,
                        )
                        .await?
                        {
                            ReplanLoopAction::Continue => continue,
                            ReplanLoopAction::Return(result) => return Ok(result),
                        }
                    }
                    other => return finish_finalize_outcome(other, &state, opts, recorder),
                }
            }
            // 没可跑 pending、又没全 done → 全卡住（F7）→ 干净 exit4（F6·不临时半建再规划）
            PlanTerminal::AllBlocked => {
                match handle_all_blocked_replan(
                    provider.clone(),
                    opts,
                    &mut state,
                    state_path,
                    recorder,
                )
                .await?
                {
                    ReplanLoopAction::Continue => continue,
                    ReplanLoopAction::Return(result) => return Ok(result),
                }
            }
            PlanTerminal::Running => {}
        }

        // 计划级总预算（B5·走持久化的 steps_used·resume 后不归零）
        if state.steps_used >= opts.max_plan_steps {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "plan_budget_exhausted", "steps_used": state.steps_used,
                    "max_plan_steps": opts.max_plan_steps, "completed_tasks": completed_ids(&state),
                    "next_step": "提高预算 / 人工介入",
                }),
            )?;
            return Ok(needs_decision_result(opts));
        }

        let task = state
            .runnable_next()
            .expect("PlanTerminal::Running → runnable_next is Some")
            .clone();
        let child_id = format!("{}__{}", opts.plan_run_id, task.id);

        // 开跑前 scope 复核（§4.1·提到 mark InProgress/计步之前·过期则 Blocked·不耗 step）
        let stale = stale_scope_paths(&opts.workspace, &task.files_scope);
        if !stale.is_empty() {
            let reason = format!(
                "files_scope 落空（计划过期·所在目录已不存在）：{}",
                stale.join(", ")
            );
            recorder.emit(
                "plan.task.blocked",
                json!({ "task": task.id, "reason": "scope_stale", "stale_paths": stale }),
            )?;
            state.mark_status(&task.id, TaskStatus::Blocked { reason });
            save_state(state_path, &state)?;
            continue;
        }

        // 拍 baseline（开工前闸 + 执行后审计复用同一份·BLOCK 1）
        let baseline = capture_baseline(&opts.workspace)?;

        // 开工前验收闸（§4·gate 开 且 change_required 才跑；invariant 走全局 health-check·不进闸）
        if state.preflight_gate && task.acceptance_kind == AcceptanceKind::ChangeRequired {
            match run_preflight_gate(
                provider.clone(),
                opts,
                &mut state,
                state_path,
                recorder,
                &task,
                &baseline,
            )
            .await?
            {
                PreflightAction::Proceed => {}
                PreflightAction::Continue => continue,
                PreflightAction::Return(result) => return Ok(result),
            }
        }

        // 只有放行（ProceedCodeRed）或闸关/invariant 才到这：现在才 mark InProgress + 计步
        state.mark_status(&task.id, TaskStatus::InProgress);
        state.steps_used += 1;
        save_state(state_path, &state)?;

        let scope = TaskScope::from_task(&task).with_crate_roots(crate_roots.clone());
        let task_contract = task_to_goal_contract(&task);
        let child_opts = child_run_options(opts, &caps, &task, &child_id);

        let child = run_solo_task(
            provider.clone(),
            Box::new(crate::judge::NoopJudge),
            child_opts,
            Some(task_contract),
            Some(scope.clone()),
        )
        .await?;

        let changed_files = changed_paths_since(&opts.workspace, &baseline)?;
        let raw_violations = classify_violations(&changed_files, &scope);
        // fmt-scope：把「名单外纯排版」从违规里分出来降级 advisory（红线/真内容/拿不准维持违规）。
        let (violations, fmt_advisories) =
            partition_formatting_violations(&opts.workspace, &baseline, &scope, raw_violations)
                .await;
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
        let report =
            task_report_from_child(&task, &child, &child_events, changed_files, violations)?;

        recorder.emit("plan.task.report", serde_json::to_value(&report)?)?;

        // 权威验收必走（两道）：child outcome / journal verdict 只进 report，不再当「是否跑验收」的闸。
        let decision = run_task_acceptance(
            &task,
            &report,
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
                save_state(state_path, &state)?;
            }
            TaskDecision::FailedByAcceptance { .. } => {
                let reason = decision_reason(&decision);
                recorder.emit(
                    "plan.task.blocked",
                    json!({ "task": task.id, "reason": reason }),
                )?;
                state.mark_status(&task.id, TaskStatus::Blocked { reason });
                save_state(state_path, &state)?;
            }
            TaskDecision::FailedByPolicy { .. } => {
                let reason = decision_reason(&decision);
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "failed_by_policy", "task": task.id, "detail": reason }),
                )?;
                state.mark_status(&task.id, TaskStatus::Blocked { reason });
                save_state(state_path, &state)?;
                return Ok(needs_decision_result(opts));
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
                save_state(state_path, &state)?; // 保持 InProgress，resume 先补验收
                return Ok(needs_decision_result(opts));
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
                save_state(state_path, &state)?;
                return Ok(needs_decision_result(opts));
            }
        }
    }
}

enum ReplanLoopAction {
    Continue,
    Return(RunResult),
}

enum PreflightAction {
    Proceed,
    Continue,
    Return(RunResult),
}

fn preflight_lane(task: &PlanTask) -> Option<&Criterion> {
    task.artifact_check.as_ref()
}

/// 开工前验收闸（§4）：复用已拍 baseline 跑一遍任务验收 → 六态 → 裁决 → 处置。
/// Proceed = 放行进 agent；Continue = 已 supersede/退回·回主循环；Return = exit4。
#[allow(clippy::too_many_arguments)]
async fn run_preflight_gate<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
    task: &PlanTask,
    baseline: &WriteBaseline,
) -> Result<PreflightAction> {
    let Some(artifact) = preflight_lane(task) else {
        return Ok(PreflightAction::Proceed);
    };

    recorder.emit(
        "plan.preflight.considered",
        json!({ "task": task.id, "artifact": artifact.id }),
    )?;

    let first = criterion_command_result_readonly_checked_with_baseline(
        artifact,
        CommandRole::AuthoritativeAcceptance,
        &opts.workspace,
        opts.network,
        opts.fs_write_fence,
        baseline,
    )
    .await?;
    let reconfirm = if matches!(first, AcceptanceResult::Pass { .. }) {
        Some(
            criterion_command_result_readonly_checked_with_baseline(
                artifact,
                CommandRole::AuthoritativeAcceptance,
                &opts.workspace,
                opts.network,
                opts.fs_write_fence,
                baseline,
            )
            .await?,
        )
    } else {
        None
    };
    let outcome = classify_preflight(&first, reconfirm.as_ref());

    let root = state
        .preflight_refine_lineage
        .get(&task.id)
        .cloned()
        .unwrap_or_else(|| task.id.clone());
    let attempts = state
        .preflight_refine_attempts
        .get(&root)
        .copied()
        .unwrap_or(0);
    let step = decide_preflight(&outcome, attempts, DEFAULT_MAX_PREFLIGHT_REFINE);

    match step {
        PreflightStep::Proceed => {
            recorder.emit("plan.preflight.proceed", json!({ "task": task.id }))?;
            Ok(PreflightAction::Proceed)
        }
        PreflightStep::Suspend { reason } => {
            recorder.emit(
                "plan.preflight.suspended",
                json!({ "task": task.id, "detail": reason }),
            )?;
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "preflight_suspended",
                    "task": task.id,
                    "detail": reason,
                    "next_step": "环境抽风/验收不稳定/验收会改工作区·非代码红·修好后 resume·别当失败再规划",
                }),
            )?;
            save_state(state_path, state)?;
            Ok(PreflightAction::Return(needs_decision_result(opts)))
        }
        PreflightStep::Escalate { reason } => {
            state.mark_status(
                &task.id,
                TaskStatus::RejectedAcceptance {
                    reason: reason.clone(),
                },
            );
            recorder.emit(
                "plan.preflight.escalated",
                json!({ "task": task.id, "root": root, "detail": reason }),
            )?;
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "preflight_refine_exhausted",
                    "task": task.id, "root": root, "detail": reason,
                    "next_step": "开工前验收反复太松/非法·退回次数用尽·人工改目标/拆解",
                }),
            )?;
            save_state(state_path, state)?;
            Ok(PreflightAction::Return(needs_decision_result(opts)))
        }
        PreflightStep::Refine { kind, reason } => {
            handle_preflight_refine(
                provider, opts, state, state_path, recorder, task, kind, reason, &root, attempts,
            )
            .await
        }
    }
}

/// pre-green/invalid → 退回 Planner 追加更强替代 + 原任务 Superseded（§4.3·成功一律 Superseded·BLOCK-5）。
#[allow(clippy::too_many_arguments)]
async fn handle_preflight_refine<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
    task: &PlanTask,
    kind: RefineKind,
    reason: String,
    root: &str,
    attempts: usize,
) -> Result<PreflightAction> {
    let req_event = match kind {
        RefineKind::Strengthen => "plan.preflight.pre_green",
        RefineKind::Legal => "plan.preflight.refine_requested",
    };
    recorder.emit(
        req_event,
        json!({ "task": task.id, "root": root, "kind": format!("{kind:?}"), "reason": reason.clone() }),
    )?;

    let round = attempts + 1;
    let planned = plan_preflight_refine(&provider, opts, recorder, task, kind, root, round).await?;
    let candidates = match planned {
        PlanOutcome::Tasks(t) => t,
        PlanOutcome::Escalated => {
            return preflight_give_up(
                state,
                state_path,
                recorder,
                task,
                root,
                "preflight_refine_unreviewable",
                opts,
            );
        }
    };

    let fresh: Vec<PlanTask> = candidates
        .into_iter()
        .filter(|c| !is_duplicate_task(c, &state.worklist))
        .collect();
    if fresh.is_empty() {
        return preflight_give_up(
            state,
            state_path,
            recorder,
            task,
            root,
            "preflight_refine_duplicate",
            opts,
        );
    }
    let satisfied_ids: std::collections::HashSet<String> = state
        .worklist
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Superseded { .. }))
        .map(|t| t.id.clone())
        .collect();
    if let Err(reasons) = validate_remediation_append(&fresh, &state.worklist, &satisfied_ids) {
        recorder.emit(
            "run.needs_decision",
            json!({ "reason": "preflight_refine_append_rejected", "task": task.id, "root": root, "reasons": reasons }),
        )?;
        return preflight_give_up(
            state,
            state_path,
            recorder,
            task,
            root,
            "preflight_refine_append_rejected",
            opts,
        );
    }

    let replacement_ids: Vec<String> = fresh.iter().map(|t| t.id.clone()).collect();
    for id in &replacement_ids {
        state
            .preflight_refine_lineage
            .insert(id.clone(), root.to_string());
    }
    state.add_tasks(fresh);
    *state
        .preflight_refine_attempts
        .entry(root.to_string())
        .or_insert(0) += 1;
    state.rewrite_dependents(&task.id, &replacement_ids);
    state.mark_status(
        &task.id,
        TaskStatus::Superseded {
            by: replacement_ids.clone(),
            reason,
        },
    );

    recorder.emit(
        "plan.preflight.superseded",
        json!({ "task": task.id, "by": replacement_ids }),
    )?;
    recorder.emit(
        "plan.preflight.refine_appended",
        json!({ "task": task.id, "root": root, "round": round, "replacements": replacement_ids }),
    )?;
    save_state(state_path, state)?;
    Ok(PreflightAction::Continue)
}

/// refine 放弃（规划不收敛/重复/校验不过）：原任务记 RejectedAcceptance 审计终态·随即 exit4。
fn preflight_give_up(
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
    task: &PlanTask,
    root: &str,
    reason: &str,
    opts: &PlanRunOptions,
) -> Result<PreflightAction> {
    state.mark_status(
        &task.id,
        TaskStatus::RejectedAcceptance {
            reason: reason.to_string(),
        },
    );
    recorder.emit(
        "plan.preflight.escalated",
        json!({ "task": task.id, "root": root, "reason": reason }),
    )?;
    recorder.emit(
        "run.needs_decision",
        json!({ "reason": reason, "task": task.id, "root": root,
                "next_step": "开工前替代任务规划失败·人工改目标/拆解" }),
    )?;
    save_state(state_path, state)?;
    Ok(PreflightAction::Return(needs_decision_result(opts)))
}

async fn handle_all_blocked_replan<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
) -> Result<ReplanLoopAction> {
    let blocked_acceptance_tasks: Vec<PlanTask> = state
        .worklist
        .iter()
        .filter(|t| {
            matches!(
                &t.status,
                TaskStatus::Blocked { reason } if reason.starts_with("failed_by_acceptance:")
            )
        })
        .cloned()
        .collect();

    if blocked_acceptance_tasks.is_empty() {
        return Ok(ReplanLoopAction::Return(escalate_blocked(
            recorder, opts, state,
        )?));
    }

    let mut checked_ids = Vec::new();
    let mut passed_ids = Vec::new();
    let mut failed_ids = Vec::new();
    let mut evidence = Vec::new();

    for task in blocked_acceptance_tasks {
        checked_ids.push(task.acceptance.id.clone());
        let report = synthetic_report_for_task(&task, TaskReportStatus::BlockedCandidate);
        let decision = run_task_acceptance(
            &task,
            &report,
            &opts.workspace,
            opts.network,
            opts.fs_write_fence,
        )
        .await?;
        recorder.emit(
            "plan.replan.reverified",
            json!({ "task": task.id, "decision": &decision, "reason": decision_reason(&decision) }),
        )?;

        match &decision {
            TaskDecision::PassedByAcceptance { advisory, .. } => {
                passed_ids.push(task.acceptance.id.clone());
                emit_advisory_if_any(recorder, &task.id, advisory.as_ref())?;
                state.mark_status(&task.id, TaskStatus::Done);
            }
            TaskDecision::FailedByAcceptance { acceptance, .. } => {
                failed_ids.push(task.acceptance.id.clone());
                evidence.push(acceptance.clone());
                state.mark_status(
                    &task.id,
                    TaskStatus::Blocked {
                        reason: decision_reason(&decision),
                    },
                );
            }
            TaskDecision::UnvalidatedInfraError { signature, .. } => {
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "infra_red", "task": task.id, "signature": signature,
                            "next_step": "blocked 任务重验遇环境抽风·修环境后 resume" }),
                )?;
                save_state(state_path, state)?;
                return Ok(ReplanLoopAction::Return(needs_decision_result(opts)));
            }
            TaskDecision::StoppedUnvalidated { reason } => {
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "stopped_unvalidated", "task": task.id, "detail": reason,
                            "next_step": "blocked 任务重验没跑成·先修验收命令/环境后 resume" }),
                )?;
                save_state(state_path, state)?;
                return Ok(ReplanLoopAction::Return(needs_decision_result(opts)));
            }
            TaskDecision::FailedByPolicy { .. } => {
                let reason = decision_reason(&decision);
                state.mark_status(
                    &task.id,
                    TaskStatus::Blocked {
                        reason: reason.clone(),
                    },
                );
                recorder.emit(
                    "run.needs_decision",
                    json!({ "reason": "failed_by_policy", "task": task.id, "detail": reason }),
                )?;
                save_state(state_path, state)?;
                return Ok(ReplanLoopAction::Return(needs_decision_result(opts)));
            }
        }
    }

    let snapshot = UnmetSnapshot {
        trigger: Trigger::TaskLevel,
        checked_ids,
        passed_ids,
        failed_ids,
    };

    if evidence.is_empty() {
        state.last_snapshot = Some(snapshot);
        save_state(state_path, state)?;
        return Ok(ReplanLoopAction::Continue);
    }

    recorder.emit(
        "plan.replan.considered",
        json!({
            "trigger": "task_level",
            "round": state.replan_rounds + 1,
            "evidence_fingerprints": evidence.iter().map(failure_fingerprint).collect::<Vec<_>>(),
        }),
    )?;

    if state.replan_rounds >= opts.max_replan_rounds {
        return append_or_escalate_replan(
            PlanOutcome::Tasks(Vec::new()),
            state,
            state_path,
            opts,
            recorder,
            Trigger::TaskLevel,
            snapshot,
            evidence,
        );
    }

    let parent = evidence
        .first()
        .and_then(|ev| ev.criterion_id.strip_suffix("_acc"))
        .unwrap_or("task");

    let planned = plan_remediation(
        &provider,
        opts,
        recorder,
        parent,
        &evidence,
        &completed_ids(state),
        &state.worklist,
        state.replan_rounds + 1,
        replan_memory(state),
    )
    .await?;

    append_or_escalate_replan(
        planned,
        state,
        state_path,
        opts,
        recorder,
        Trigger::TaskLevel,
        snapshot,
        evidence,
    )
}

async fn handle_overall_replan<P: ProviderClient + Clone>(
    provider: P,
    opts: &PlanRunOptions,
    state: &mut RunState,
    state_path: &Path,
    recorder: &mut EventRecorder,
    snapshot: UnmetSnapshot,
    evidence: Vec<CommandEvidence>,
) -> Result<ReplanLoopAction> {
    recorder.emit(
        "plan.replan.considered",
        json!({
            "trigger": "overall_level",
            "round": state.replan_rounds + 1,
            "evidence_fingerprints": evidence.iter().map(failure_fingerprint).collect::<Vec<_>>(),
        }),
    )?;

    if state.replan_rounds >= opts.max_replan_rounds {
        return append_or_escalate_replan(
            PlanOutcome::Tasks(Vec::new()),
            state,
            state_path,
            opts,
            recorder,
            Trigger::OverallLevel,
            snapshot,
            evidence,
        );
    }

    let planned = plan_remediation(
        &provider,
        opts,
        recorder,
        "overall",
        &evidence,
        &completed_ids(state),
        &state.worklist,
        state.replan_rounds + 1,
        replan_memory(state),
    )
    .await?;

    append_or_escalate_replan(
        planned,
        state,
        state_path,
        opts,
        recorder,
        Trigger::OverallLevel,
        snapshot,
        evidence,
    )
}

#[allow(clippy::too_many_arguments)]
fn append_or_escalate_replan(
    planned: PlanOutcome,
    state: &mut RunState,
    state_path: &Path,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
    trigger: Trigger,
    snapshot: UnmetSnapshot,
    evidence: Vec<CommandEvidence>,
) -> Result<ReplanLoopAction> {
    let candidates = match planned {
        PlanOutcome::Tasks(tasks) => tasks,
        PlanOutcome::Escalated => {
            recorder.emit(
                "plan.replan.escalated",
                json!({ "round": state.replan_rounds + 1, "reason": "unreviewable" }),
            )?;
            return Ok(ReplanLoopAction::Return(escalate_blocked(
                recorder, opts, state,
            )?));
        }
    };

    match decide_replan(
        state,
        trigger,
        snapshot.clone(),
        evidence.clone(),
        candidates,
        opts.max_replan_rounds,
    ) {
        ReplanStep::Append { tasks } => {
            let appended_ids = tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>();
            for ev in &evidence {
                if fingerprint_hard_dedup_safe(ev) {
                    let fp = failure_fingerprint(ev);
                    if !state.remediated_fingerprints.contains(&fp) {
                        state.remediated_fingerprints.push(fp);
                    }
                }
            }
            state.replan_rounds += 1;
            state.last_snapshot = Some(snapshot);
            state.add_tasks(tasks);
            recorder.emit(
                "plan.replan.appended",
                json!({ "round": state.replan_rounds, "appended_task_ids": appended_ids }),
            )?;
            save_state(state_path, state)?;
            Ok(ReplanLoopAction::Continue)
        }
        ReplanStep::Escalate { reason, evidence } => {
            recorder.emit(
                "plan.replan.escalated",
                json!({
                    "round": state.replan_rounds + 1,
                    "reason": reason,
                    "unmet_snapshot": snapshot,
                    "evidence_chain": evidence,
                }),
            )?;
            save_state(state_path, state)?;
            Ok(ReplanLoopAction::Return(escalate_blocked(
                recorder, opts, state,
            )?))
        }
    }
}

/// child run 回合预算：planner 给的 task.max_turns·被 operator 的 default 封顶（F2·CLI 旋钮真生效）。
fn child_max_turns(task_max_turns: usize, default_cap: usize) -> usize {
    let floor = MIN_TASK_TURN_BUDGET;
    let ceiling = if default_cap > 0 {
        default_cap.max(floor)
    } else {
        usize::MAX
    };
    task_max_turns.max(floor).min(ceiling)
}

/// 把 PlanTask 翻成 child run 的 RunOptions（新构造点·设全 22 字段·不碰已有构造点）。
fn child_run_options(
    opts: &PlanRunOptions,
    caps: &crate::provider::ProviderCapabilities,
    task: &PlanTask,
    child_id: &str,
) -> RunOptions {
    let goal_contract = task_to_goal_contract(task);
    RunOptions {
        prompt: goal_contract.objective.clone(),
        workspace: opts.workspace.clone(),
        provider_id: caps.provider_id.clone(),
        model: caps.model_id.clone(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        permission: opts.permission,
        network: opts.network,
        fs_read_scope: opts.fs_read_scope,
        fs_write_fence: opts.fs_write_fence,
        evidence_gate: crate::orchestrator::EvidenceGate::Off,
        native_search_enabled: false,
        disallowed_tools: std::collections::BTreeSet::new(),
        memory_enabled: false,
        search: crate::config::SearchChoice::Ddg,
        max_turns: child_max_turns(task.max_turns, opts.default_task_max_turns),
        run_id: Some(child_id.to_string()),
        context_files: Vec::new(),
        criteria: goal_contract.criteria.clone(),
        contract_policy: opts.contract_policy,
        max_eval_attempts: opts.max_eval_attempts,
        // Keep the default cadence; orchestrator.rs bypasses this threshold when
        // a mutating edit leaves open ripple candidates.
        verify_reflex_debt: DEFAULT_VERIFY_EVERY,
        watchdog_repeat_threshold: DEFAULT_WATCHDOG_REPEAT,
        journal_root: opts.journal_root.clone(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn settle_report_decision(report: &TaskReport, acceptance: AcceptanceResult) -> TaskDecision {
    decide_task(report, acceptance)
}

async fn run_task_acceptance(
    task: &PlanTask,
    report: &TaskReport,
    workspace: &Path,
    network: NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> Result<TaskDecision> {
    let behavior = criterion_command_result_readonly_checked(
        &task.acceptance,
        CommandRole::AuthoritativeAcceptance,
        workspace,
        network,
        fs_write_fence,
    )
    .await?;
    let artifact = match &task.artifact_check {
        Some(c) => Some(
            criterion_command_result_readonly_checked(
                c,
                CommandRole::AuthoritativeAcceptance,
                workspace,
                network,
                fs_write_fence,
            )
            .await?,
        ),
        None => None,
    };
    Ok(merge_task_acceptance(report, artifact, behavior))
}

async fn criterion_command_result_readonly_checked(
    criterion: &Criterion,
    role: CommandRole,
    workspace: &Path,
    network: NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> Result<AcceptanceResult> {
    let baseline = capture_baseline(workspace)?;
    criterion_command_result_readonly_checked_with_baseline(
        criterion,
        role,
        workspace,
        network,
        fs_write_fence,
        &baseline,
    )
    .await
}

/// BLOCK 1：吃外部已拍 baseline 的只读验收（开工前闸复用主循环刚拍的同一份·不重复拍）。
async fn criterion_command_result_readonly_checked_with_baseline(
    criterion: &Criterion,
    role: CommandRole,
    workspace: &Path,
    network: NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    baseline: &WriteBaseline,
) -> Result<AcceptanceResult> {
    let result =
        criterion_command_result_with_fence(criterion, role, workspace, network, fs_write_fence)
            .await?;
    // 已执行的 check 一律比 baseline；写了工作区是观察到的确定事实，即使结果是 InfraRed。
    // 未执行（NotRun / NetworkUnenforceable 这类无 evidence 的 InfraRed）才跳 delta。
    if result.acceptance_evidence().is_none() {
        return Ok(result);
    }
    let changed_files = changed_paths_since(workspace, baseline)?;
    if changed_files.is_empty() {
        return Ok(result);
    }
    Ok(AcceptanceResult::PolicyFailure {
        reason: format!("acceptance_read_only_violation: {role:?} wrote workspace files"),
        changed_files,
        acceptance: result.acceptance_evidence().cloned(),
    })
}

fn synthetic_report_for_task(task: &PlanTask, status: TaskReportStatus) -> TaskReport {
    TaskReport {
        schema_version: crate::plan::contract::TASK_REPORT_SCHEMA_VERSION,
        task_id: task.id.clone(),
        child_run_id: String::new(),
        status,
        acceptance: task.acceptance.clone(),
        child_outcome: ChildRunOutcome::Unknown,
        child_evaluation: None,
        stop: None,
        changes: ChangeSet::default(),
        evidence: vec![],
        narrative: TaskNarrative::default(),
    }
}

fn decision_reason(decision: &TaskDecision) -> String {
    match decision {
        TaskDecision::PassedByAcceptance { .. } => "passed_by_acceptance".to_string(),
        TaskDecision::FailedByAcceptance { acceptance, .. } => {
            format!("failed_by_acceptance: {}", acceptance.criterion_id)
        }
        TaskDecision::FailedByPolicy { violations, .. } => format!(
            "failed_by_policy: {}",
            violations
                .iter()
                .map(|v| v.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TaskDecision::UnvalidatedInfraError { signature, .. } => {
            format!("unvalidated_infra_error: {signature}")
        }
        TaskDecision::StoppedUnvalidated { reason } => {
            format!("stopped_unvalidated: {reason}")
        }
    }
}

fn emit_advisory_if_any(
    recorder: &mut EventRecorder,
    task_id: &str,
    advisory: Option<&crate::plan::contract::AdvisoryNote>,
) -> Result<()> {
    if let Some(note) = advisory {
        recorder.emit(
            "plan.task.advisory",
            json!({
                "task": task_id,
                "lane": &note.lane,
                "result": &note.result,
                "detail": &note.detail,
                "evidence": &note.evidence,
                "note": "结构检查红但行为道绿·已判 Done·结构检查仅供参考·留 finalize 现验复核",
            }),
        )?;
    }
    Ok(())
}

pub(crate) fn save_state(path: &Path, state: &RunState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn completed_ids(state: &RunState) -> Vec<String> {
    state
        .worklist
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Done))
        .map(|t| t.id.clone())
        .collect()
}

fn blocked_reason(task: &PlanTask) -> String {
    match &task.status {
        TaskStatus::Blocked { reason } => reason.clone(),
        other => format!("{other:?}"),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum FinalizeOutcome {
    Completed,
    NeedsReplan {
        code_red: Vec<CommandEvidence>,
        unmet: Vec<Value>,
        snapshot: UnmetSnapshot,
    },
    Infra {
        infra: Vec<Value>,
        code_red_observed: Vec<Value>,
    },
    Stopped {
        stopped: Vec<Value>,
        code_red_observed: Vec<Value>,
    },
    Policy {
        policy_failures: Vec<Value>,
    },
    AdvisoryPending {
        tasks: Vec<Value>,
    },
}

/// 全任务 done 后的真总验收（spec §3.2/§3.4·替 1b 桩）：跑总验收细项 ∪ per-language 不变量（state.checks）
/// + 整盘重验所有 done 任务 acceptance（B2/B3），全走假红防护。infra/停止/策略先分桶，不喂再规划。
#[cfg(test)]
async fn finalize_completion(
    state: &RunState,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
) -> Result<RunResult> {
    let outcome = finalize_completion_outcome(state, opts, recorder).await?;
    finish_finalize_outcome(outcome, state, opts, recorder)
}

async fn finalize_completion_outcome(
    state: &RunState,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
) -> Result<FinalizeOutcome> {
    let mut code_red: Vec<CommandEvidence> = Vec::new();
    let mut code_unmet: Vec<Value> = Vec::new();
    let mut infra: Vec<Value> = Vec::new();
    let mut stopped: Vec<Value> = Vec::new();
    let mut policy_unmet: Vec<Value> = Vec::new();
    let mut checked_ids = Vec::new();
    let mut passed_ids = Vec::new();
    let mut failed_ids = Vec::new();

    for c in &state.checks {
        checked_ids.push(c.id.clone());
        match criterion_command_result_readonly_checked(
            c,
            CommandRole::OverallCheck,
            &opts.workspace,
            opts.network,
            opts.fs_write_fence,
        )
        .await?
        {
            AcceptanceResult::Pass { .. } => passed_ids.push(c.id.clone()),
            AcceptanceResult::CodeRed { acceptance } => {
                failed_ids.push(c.id.clone());
                code_red.push(acceptance.clone());
                code_unmet.push(
                    json!({ "kind": "overall_check", "criterion": c.id, "evidence": acceptance }),
                );
            }
            AcceptanceResult::InfraRed {
                signature,
                acceptance,
            } => {
                infra.push(json!({ "kind": "overall_check", "criterion": c.id, "signature": signature, "evidence": acceptance }));
            }
            AcceptanceResult::NotRun { reason } => {
                stopped.push(json!({ "kind": "overall_check", "criterion": c.id, "stopped_unvalidated": reason }));
            }
            AcceptanceResult::PolicyFailure {
                reason,
                changed_files,
                acceptance,
            } => {
                policy_unmet.push(json!({
                    "kind": "overall_policy",
                    "criterion": c.id,
                    "reason": reason,
                    "changed_files": changed_files,
                    "evidence": acceptance,
                }));
            }
        }
    }
    for t in &state.worklist {
        if matches!(t.status, TaskStatus::Done) {
            checked_ids.push(t.acceptance.id.clone());
            let report = synthetic_report_for_task(t, TaskReportStatus::DoneCandidate);
            let acceptance = criterion_command_result_readonly_checked(
                &t.acceptance,
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
                json!({ "task": t.id, "decision": &decision, "reason": reason, "phase": "finalize" }),
            )?;
            match &decision {
                TaskDecision::PassedByAcceptance { .. } => passed_ids.push(t.acceptance.id.clone()),
                TaskDecision::FailedByAcceptance { acceptance, .. } => {
                    failed_ids.push(t.acceptance.id.clone());
                    code_red.push(acceptance.clone());
                    code_unmet.push(json!({ "kind": "task_acceptance", "task": t.id, "label": format!("task {} acceptance", t.id), "decision": &decision, "reason": decision_reason(&decision) }));
                }
                TaskDecision::UnvalidatedInfraError { signature, .. } => {
                    infra.push(json!({ "kind": "task_acceptance", "task": t.id, "signature": signature, "decision": &decision }));
                }
                TaskDecision::StoppedUnvalidated { reason } => {
                    stopped.push(json!({ "kind": "task_acceptance", "task": t.id, "stopped_unvalidated": reason }));
                }
                TaskDecision::FailedByPolicy { .. } => {
                    policy_unmet.push(json!({
                        "kind": "task_policy",
                        "task": t.id,
                        "decision": &decision,
                        "reason": decision_reason(&decision),
                    }));
                }
            }
        }
    }

    let mut advisory_pending: Vec<Value> = Vec::new();
    for t in &state.worklist {
        if !matches!(t.status, TaskStatus::Done) {
            continue;
        }
        if t.acceptance_kind != AcceptanceKind::ChangeRequired {
            continue;
        }
        let Some(artifact) = &t.artifact_check else {
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
                policy_unmet.push(json!({
                    "kind": "finalize_artifact_policy",
                    "task": t.id,
                    "reason": reason,
                    "changed_files": changed_files,
                    "evidence": acceptance,
                }));
            }
            AcceptanceResult::CodeRed { acceptance } => {
                advisory_pending.push(json!({
                    "kind": "finalize_artifact_advisory",
                    "task": t.id,
                    "artifact": artifact.id,
                    "result": "code_red",
                    "evidence": acceptance,
                }));
            }
            AcceptanceResult::NotRun { reason } => {
                advisory_pending.push(json!({
                    "kind": "finalize_artifact_advisory",
                    "task": t.id,
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
                    "task": t.id,
                    "artifact": artifact.id,
                    "result": "infra_red",
                    "detail": signature,
                    "evidence": acceptance,
                }));
            }
        }
    }

    if !policy_unmet.is_empty() {
        return Ok(FinalizeOutcome::Policy {
            policy_failures: policy_unmet,
        });
    }
    if !infra.is_empty() {
        return Ok(FinalizeOutcome::Infra {
            infra,
            code_red_observed: code_unmet,
        });
    }
    if !stopped.is_empty() {
        return Ok(FinalizeOutcome::Stopped {
            stopped,
            code_red_observed: code_unmet,
        });
    }
    if !code_unmet.is_empty() {
        return Ok(FinalizeOutcome::NeedsReplan {
            code_red,
            unmet: code_unmet,
            snapshot: UnmetSnapshot {
                trigger: Trigger::OverallLevel,
                checked_ids,
                passed_ids,
                failed_ids,
            },
        });
    }
    if !advisory_pending.is_empty() {
        return Ok(FinalizeOutcome::AdvisoryPending {
            tasks: advisory_pending,
        });
    }

    Ok(FinalizeOutcome::Completed)
}

fn finish_finalize_outcome(
    outcome: FinalizeOutcome,
    state: &RunState,
    opts: &PlanRunOptions,
    recorder: &mut EventRecorder,
) -> Result<RunResult> {
    match outcome {
        FinalizeOutcome::Completed => {
            recorder.emit("run.completed", json!({ "tasks": state.worklist_len() }))?;
            Ok(completed_result(opts))
        }
        FinalizeOutcome::Policy { policy_failures } => {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "failed_by_policy",
                    "policy_failures": policy_failures,
                    "completed_tasks": completed_ids(state),
                    "next_step": "验收命令实际改动了工作区·先改成只读/幂等检查，再 resume",
                }),
            )?;
            Ok(needs_decision_result(opts))
        }
        FinalizeOutcome::Infra {
            infra,
            code_red_observed,
        } => {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "infra_red",
                    "infra_signatures": infra,
                    "code_unmet_observed": code_red_observed,
                    "next_step": "环境抽风(网络/超时/锁)·非代码红·修环境后重跑确认·别当失败再规划",
                }),
            )?;
            Ok(needs_decision_result(opts))
        }
        FinalizeOutcome::Stopped {
            stopped,
            code_red_observed,
        } => {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "stopped_unvalidated",
                    "stopped": stopped,
                    "code_unmet_observed": code_red_observed,
                    "completed_tasks": completed_ids(state),
                    "next_step": "验收未跑成·先修验收命令/环境后 resume",
                }),
            )?;
            Ok(needs_decision_result(opts))
        }
        FinalizeOutcome::NeedsReplan { unmet, .. } => {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "overall_red",
                    "unmet": unmet,
                    "completed_tasks": completed_ids(state),
                    "next_step": "总验收/整盘重验红·进第二刀（回 Planner 追加任务·净进展集合判）",
                }),
            )?;
            Ok(needs_decision_result(opts))
        }
        FinalizeOutcome::AdvisoryPending { tasks } => {
            recorder.emit(
                "run.needs_decision",
                json!({
                    "reason": "advisory_pending",
                    "tasks": tasks,
                    "completed_tasks": completed_ids(state),
                    "next_step": "这些任务结构检查没确认真干完(行为道绿但按名检查现验仍红)·人工核对·改对后 resume 会当场复验放行",
                }),
            )?;
            Ok(needs_decision_result(opts))
        }
    }
}

/// 全卡住 → 可行动求助（spec §4.8·F6·不甩锅）。
fn escalate_blocked(
    recorder: &mut EventRecorder,
    opts: &PlanRunOptions,
    state: &RunState,
) -> Result<RunResult> {
    let blocked: Vec<_> = state
        .worklist
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                TaskStatus::Blocked { .. } | TaskStatus::RejectedAcceptance { .. }
            )
        })
        .map(|t| json!({ "id": t.id, "intent": t.intent, "reason": blocked_reason(t) }))
        .collect();
    recorder.emit(
        "run.needs_decision",
        json!({
            "reason": "all_blocked", "blocked_tasks": blocked, "completed_tasks": completed_ids(state),
            "replan_rounds": state.replan_rounds,
            "remediated_fingerprints": state.remediated_fingerprints,
            "last_snapshot": state.last_snapshot,
            "next_step": "第一刀不含卡住再拆/再规划——进第二刀（出路阶梯 + 再规划）",
        }),
    )?;
    Ok(needs_decision_result(opts))
}

fn completed_result(opts: &PlanRunOptions) -> RunResult {
    RunResult {
        run_id: opts.plan_run_id.clone(),
        outcome: RunOutcome::Completed,
        always_used: false,
    }
}
fn needs_decision_result(opts: &PlanRunOptions) -> RunResult {
    RunResult {
        run_id: opts.plan_run_id.clone(),
        outcome: RunOutcome::NeedsDecision,
        always_used: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::contract::TaskStatus;
    use std::ops::Not;

    #[derive(Clone)]
    struct ScriptedPlanner {
        worklist: String,
    }
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ScriptedPlanner {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            let is_planner = messages
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|c| c.contains("WORKLIST")));
            let text = if is_planner {
                self.worklist.clone()
            } else {
                "done".to_string()
            };
            Ok(crate::provider::ProviderResponse {
                text,
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            crate::provider::ProviderCapabilities {
                provider_id: "mock".into(),
                model_id: "mock".into(),
                supports_streaming: false,
                supports_reasoning_deltas: false,
                supports_tool_calling: true,
                supports_images: false,
                supports_computer_use: false,
                supports_shell_tool: true,
                max_context_tokens: Some(128_000),
                output_token_limit: Some(8_192),
                server_side_search: false,
            }
        }
    }

    #[derive(Clone)]
    struct FmtScopeE2eProvider {
        worklist: String,
        child_shell: String,
    }
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for FmtScopeE2eProvider {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            let is_planner = messages
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|c| c.contains("WORKLIST")));
            if is_planner {
                return Ok(crate::provider::ProviderResponse {
                    text: self.worklist.clone(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            if tool_msgs == 0 {
                return Ok(crate::provider::ProviderResponse {
                    text: "running out-of-scope shell".into(),
                    reasoning: String::new(),
                    tool_calls: vec![crate::provider::ToolCall {
                        id: "call_shell".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "shell_exec".into(),
                            arguments: serde_json::json!({ "command": self.child_shell })
                                .to_string(),
                        },
                    }],
                    finish_reason: None,
                });
            }
            Ok(crate::provider::ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            ScriptedPlanner {
                worklist: String::new(),
            }
            .capabilities()
        }
    }

    #[derive(Clone)]
    struct ScriptedRemediationPlanner {
        response: String,
    }

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ScriptedRemediationPlanner {
        async fn next_turn(
            &self,
            _messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            Ok(crate::provider::ProviderResponse {
                text: self.response.clone(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            ScriptedPlanner {
                worklist: String::new(),
            }
            .capabilities()
        }
    }

    #[derive(Clone)]
    struct ScriptedRefiner {
        refine_worklist: String,
    }

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ScriptedRefiner {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            assert!(
                messages.iter().any(|m| m
                    .content
                    .as_deref()
                    .is_some_and(|c| c.contains("PRE-FLIGHT"))),
                "refiner should get preflight system prompt"
            );
            Ok(crate::provider::ProviderResponse {
                text: self.refine_worklist.clone(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            ScriptedPlanner {
                worklist: String::new(),
            }
            .capabilities()
        }
    }

    #[derive(Clone)]
    struct PlannerThenRefiner {
        worklist: String,
        refine_worklist: String,
    }

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for PlannerThenRefiner {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            let has = |needle: &str| {
                messages
                    .iter()
                    .any(|m| m.content.as_deref().is_some_and(|c| c.contains(needle)))
            };
            let text = if has("PRE-FLIGHT") {
                self.refine_worklist.clone()
            } else if has("WORKLIST") {
                self.worklist.clone()
            } else {
                "done".to_string()
            };
            Ok(crate::provider::ProviderResponse {
                text,
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            ScriptedPlanner {
                worklist: String::new(),
            }
            .capabilities()
        }
    }

    #[derive(Clone)]
    struct PanicOnChild {
        worklist: String,
        refine_worklist: String,
    }

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for PanicOnChild {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            let has = |needle: &str| {
                messages
                    .iter()
                    .any(|m| m.content.as_deref().is_some_and(|c| c.contains(needle)))
            };
            let text = if has("PRE-FLIGHT") {
                self.refine_worklist.clone()
            } else if has("WORKLIST") {
                self.worklist.clone()
            } else {
                panic!("agent self-loop must NOT run for a pre-green task");
            };
            Ok(crate::provider::ProviderResponse {
                text,
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            ScriptedPlanner {
                worklist: String::new(),
            }
            .capabilities()
        }
    }

    #[derive(Clone)]
    struct ReplanProvider {
        initial_worklist: String,
        remediation_worklist: String,
    }

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ReplanProvider {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _tools: &[serde_json::Value],
            _events: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            let is_remediation = messages.iter().any(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("REMEDIATION PLANNER"))
            });
            let is_planner = messages
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|c| c.contains("WORKLIST")));
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            let prompt = messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .and_then(|m| m.content.as_deref())
                .unwrap_or("");

            if is_remediation {
                return Ok(crate::provider::ProviderResponse {
                    text: self.remediation_worklist.clone(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            if is_planner {
                return Ok(crate::provider::ProviderResponse {
                    text: self.initial_worklist.clone(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            if prompt.contains("create fixed marker") && tool_msgs == 0 {
                return Ok(crate::provider::ProviderResponse {
                    text: "writing fixed marker".into(),
                    reasoning: String::new(),
                    tool_calls: vec![crate::provider::ToolCall {
                        id: "call_write_fixed".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "fs_write".into(),
                            arguments: serde_json::json!({"path":"fixed","content":"ok\n"})
                                .to_string(),
                        },
                    }],
                    finish_reason: None,
                });
            }

            Ok(crate::provider::ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            ScriptedPlanner {
                worklist: String::new(),
            }
            .capabilities()
        }
    }

    #[derive(Clone)]
    struct OverallReplanProvider {
        initial_worklist: String,
        remediation_worklist: String,
    }

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for OverallReplanProvider {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _tools: &[serde_json::Value],
            _events: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<crate::provider::ProviderResponse> {
            let is_remediation = messages.iter().any(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("REMEDIATION PLANNER"))
            });
            let is_planner = messages
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|c| c.contains("WORKLIST")));
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            let prompt = messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .and_then(|m| m.content.as_deref())
                .unwrap_or("");

            if is_remediation {
                return Ok(crate::provider::ProviderResponse {
                    text: self.remediation_worklist.clone(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            if is_planner {
                return Ok(crate::provider::ProviderResponse {
                    text: self.initial_worklist.clone(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            if prompt.contains("create overall marker") && tool_msgs == 0 {
                return Ok(crate::provider::ProviderResponse {
                    text: "writing overall marker".into(),
                    reasoning: String::new(),
                    tool_calls: vec![crate::provider::ToolCall {
                        id: "call_write_overall".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "fs_write".into(),
                            arguments: serde_json::json!({"path":"overall_fixed","content":"ok\n"})
                                .to_string(),
                        },
                    }],
                    finish_reason: None,
                });
            }

            Ok(crate::provider::ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            ScriptedPlanner {
                worklist: String::new(),
            }
            .capabilities()
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    fn init_git(dir: &std::path::Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@local"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("seed"), "x").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
    }

    #[tokio::test]
    async fn with_baseline_variant_detects_read_only_violation_against_passed_baseline() {
        let ws = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let baseline = capture_baseline(ws.path()).unwrap();

        let c = crate::goal::parse_criteria(&["cmd: printf x > touched.txt".to_string()])
            .unwrap()
            .remove(0);
        let result = criterion_command_result_readonly_checked_with_baseline(
            &c,
            CommandRole::AuthoritativeAcceptance,
            ws.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &baseline,
        )
        .await
        .unwrap();

        assert!(matches!(result, AcceptanceResult::PolicyFailure { .. }));
    }

    #[tokio::test]
    async fn with_baseline_variant_passes_clean_read_only_command() {
        let ws = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let baseline = capture_baseline(ws.path()).unwrap();
        let c = crate::goal::parse_criteria(&["cmd: true".to_string()])
            .unwrap()
            .remove(0);
        let result = criterion_command_result_readonly_checked_with_baseline(
            &c,
            CommandRole::AuthoritativeAcceptance,
            ws.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &baseline,
        )
        .await
        .unwrap();
        assert!(matches!(result, AcceptanceResult::Pass { .. }));
    }

    fn opts(ws: &std::path::Path, jr: &std::path::Path, id: &str) -> PlanRunOptions {
        PlanRunOptions {
            objective: "big goal".into(),
            checks: vec![],
            workspace: ws.to_path_buf(),
            journal_root: jr.to_path_buf(),
            plan_run_id: id.into(),
            permission: crate::shell::PermissionPolicy::Allow,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
            output_mode: crate::events::OutputMode::Silent,
            max_review_attempts: 3,
            max_plan_steps: 50,
            max_replan_rounds: DEFAULT_MAX_REPLAN_ROUNDS,
            contract_policy: crate::guardrails::ContractPolicy::Ask,
            max_eval_attempts: 3,
            default_task_max_turns: 5,
            preflight_gate: false,
        }
    }

    #[tokio::test]
    async fn run_plan_persists_preflight_gate_flag_into_state() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let mut o = opts(ws.path(), jr.path(), "plan_gate_persist");
        o.preflight_gate = true;
        let provider = ScriptedPlanner {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#.to_string(),
        };
        let _ = run_plan(provider, o).await.unwrap();
        let state_path = RunPaths::new(jr.path(), "plan_gate_persist")
            .run_dir
            .join("plan_state.json");
        let state: RunState = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert!(state.preflight_gate);
    }

    #[test]
    fn child_max_turns_floors_small_budgets_and_keeps_larger_values() {
        assert_eq!(child_max_turns(3, 0), MIN_TASK_TURN_BUDGET);
        assert_eq!(child_max_turns(80, 0), 80);
        assert_eq!(
            child_max_turns(8, MIN_TASK_TURN_BUDGET),
            MIN_TASK_TURN_BUDGET
        );
        assert_eq!(
            child_max_turns(80, MIN_TASK_TURN_BUDGET),
            MIN_TASK_TURN_BUDGET
        );
        assert_eq!(child_max_turns(80, 100), 80);
        assert_eq!(child_max_turns(200, 100), 100);
        assert_eq!(child_max_turns(10, 5), MIN_TASK_TURN_BUDGET);
        assert_eq!(child_max_turns(0, 0), MIN_TASK_TURN_BUDGET);
    }

    #[test]
    fn child_run_options_keep_default_reflex_threshold() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        let mut opts = opts(ws.path(), jr.path(), "plan_child_reflex_default");
        opts.fs_read_scope = crate::fs_scope::FsReadScope::Wide;
        let task = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        let provider = crate::provider::mock::MockProvider::default();
        let caps = crate::provider::ProviderClient::capabilities(&provider);

        let run_opts = child_run_options(&opts, &caps, &task, "child_reflex_default");

        assert_eq!(run_opts.verify_reflex_debt, DEFAULT_VERIFY_EVERY);
        assert_eq!(run_opts.fs_read_scope, crate::fs_scope::FsReadScope::Wide);
    }

    #[test]
    fn preflight_lane_uses_artifact_only_and_skips_when_absent() {
        let with_artifact = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        assert_eq!(preflight_lane(&with_artifact).unwrap().id, "t1_art");

        let legacy = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        assert!(preflight_lane(&legacy).is_none());
    }

    fn fake_report(
        status: crate::plan::contract::TaskReportStatus,
    ) -> crate::plan::contract::TaskReport {
        let task = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        crate::plan::contract::TaskReport {
            schema_version: crate::plan::contract::TASK_REPORT_SCHEMA_VERSION,
            task_id: "t1".into(),
            child_run_id: "plan__t1".into(),
            status,
            acceptance: task.acceptance,
            child_outcome: crate::plan::contract::ChildRunOutcome::Blocked,
            child_evaluation: None,
            stop: None,
            changes: crate::plan::contract::ChangeSet::default(),
            evidence: vec![],
            narrative: crate::plan::contract::TaskNarrative::default(),
        }
    }

    fn parse_one(json: &str) -> PlanTask {
        crate::plan::contract::parse_worklist(json)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    #[tokio::test]
    async fn run_task_acceptance_runs_both_lanes_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let task = parse_one(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src"],
              "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 5 } ] }"#,
        );
        let report = synthetic_report_for_task(&task, TaskReportStatus::DoneCandidate);
        let decision = run_task_acceptance(
            &task,
            &report,
            dir.path(),
            NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
        )
        .await
        .unwrap();
        match decision {
            TaskDecision::PassedByAcceptance {
                advisory: Some(note),
                ..
            } => {
                assert_eq!(note.result, crate::plan::contract::AdvisoryResult::CodeRed);
                assert!(note.evidence.is_some());
            }
            other => panic!("expected PassedByAcceptance advisory, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_task_acceptance_both_green_is_done() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let task = parse_one(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src"],
              "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 5 } ] }"#,
        );
        let report = synthetic_report_for_task(&task, TaskReportStatus::DoneCandidate);
        let decision = run_task_acceptance(
            &task,
            &report,
            dir.path(),
            NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
        )
        .await
        .unwrap();
        assert!(
            matches!(decision, TaskDecision::PassedByAcceptance { .. }),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn main_loop_emits_advisory_for_artifact_code_red_without_per_task_stop() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#;

        let _ = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_advisory_main"),
        )
        .await
        .unwrap();

        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_advisory_main/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("\"type\":\"plan.task.advisory\""));
        assert!(events.contains("\"result\":\"code_red\""));
        assert!(events.contains("\"type\":\"plan.task.done\""));
        assert!(!events.contains(concat!("guard", "_suspect")));
    }

    #[tokio::test]
    async fn e2e_fmt_scope_pure_reformat_advised_not_policy_failed() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        // 预先 commit 一个【任务 scope 外】的 .rs + Cargo.toml(edition) → a.rs tracked·resolve_fmt_context 能拿到 edition
        std::fs::write(
            ws.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(ws.path().join("a.rs"), "fn  a( ) {let   x=1;}\n").unwrap();
        git(ws.path(), &["add", "-A"]);
        git(ws.path(), &["commit", "-q", "-m", "base a.rs"]);

        // 任务 scope = b.rs；child 经 shell 对 scope 外 a.rs 跑 rustfmt（纯排版副作用·模拟 cargo fmt 误伤邻文件）
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "reformat neighbor", "files_scope": ["b.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let provider = FmtScopeE2eProvider {
            worklist: worklist.into(),
            child_shell: "rustfmt --edition 2021 a.rs".into(),
        };
        let _ = run_plan(provider, opts(ws.path(), jr.path(), "plan_fmt_e2e_pos"))
            .await
            .unwrap();

        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_fmt_e2e_pos/events.jsonl"),
        )
        .unwrap();
        assert!(
            events.contains("\"type\":\"plan.task.scope_formatting_advisory\""),
            "scope 外纯排版应 emit advisory: {events}"
        );
        assert!(
            !events.contains("failed_by_policy"),
            "纯排版 scope 外不该 failed_by_policy: {events}"
        );
        assert!(
            events.contains("\"type\":\"plan.task.done\""),
            "任务应 done: {events}"
        );
    }

    #[tokio::test]
    async fn e2e_fmt_scope_real_content_still_policy_failed() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        std::fs::write(
            ws.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(ws.path().join("a.rs"), "fn a() {\n    let x = 1;\n}\n").unwrap();
        git(ws.path(), &["add", "-A"]);
        git(ws.path(), &["commit", "-q", "-m", "base a.rs"]);

        // child 经 shell 往 scope 外 a.rs 追加【真内容】（新函数·非纯排版）
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "inject", "files_scope": ["b.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let provider = FmtScopeE2eProvider {
            worklist: worklist.into(),
            child_shell: "printf '\\npub fn injected() { let y = 41; }\\n' >> a.rs".into(),
        };
        let _ = run_plan(provider, opts(ws.path(), jr.path(), "plan_fmt_e2e_neg"))
            .await
            .unwrap();

        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_fmt_e2e_neg/events.jsonl"),
        )
        .unwrap();
        assert!(
            events.contains("failed_by_policy"),
            "scope 外真内容应仍 failed_by_policy: {events}"
        );
        assert!(
            !events.contains("\"type\":\"plan.task.scope_formatting_advisory\""),
            "真内容不该被当纯排版豁免: {events}"
        );
    }

    #[tokio::test]
    async fn resume_emits_advisory_for_artifact_code_red_without_per_task_stop() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
        let paths = RunPaths::new(jr.path(), "plan_advisory_resume");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

        let _ = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            opts(ws.path(), jr.path(), "plan_advisory_resume"),
        )
        .await
        .unwrap();

        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("\"type\":\"plan.task.advisory\""));
        assert!(events.contains("\"result\":\"code_red\""));
        assert!(!events.contains(concat!("guard", "_suspect")));
        let after: RunState =
            serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
                .unwrap();
        assert!(after
            .worklist
            .iter()
            .all(|t| matches!(t.status, crate::plan::contract::TaskStatus::Done)));
    }

    #[tokio::test]
    async fn all_blocked_reverify_emits_advisory_for_artifact_code_red() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "blocked",
              "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status(
            "t1",
            TaskStatus::Blocked {
                reason: "failed_by_acceptance: t1_acc".into(),
            },
        );

        let paths = RunPaths::new(jr.path(), "plan_advisory_all_blocked");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_advisory_all_blocked".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let _ = run_plan_loop(
            ScriptedPlanner {
                worklist: String::new(),
            },
            &opts(ws.path(), jr.path(), "plan_advisory_all_blocked"),
            state,
            &paths.run_dir.join("plan_state.json"),
            &mut recorder,
        )
        .await
        .unwrap();

        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("\"type\":\"plan.task.advisory\""));
        assert!(events.contains("\"result\":\"code_red\""));
        assert!(!events.contains(concat!("guard", "_suspect")));
        let after: RunState =
            serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
                .unwrap();
        assert!(after
            .worklist
            .iter()
            .all(|t| matches!(t.status, TaskStatus::Done)));
    }

    #[tokio::test]
    async fn finalize_reverifies_behavior_only_keeps_done() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let task = parse_one(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src"],
              "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 5 } ] }"#,
        );
        let report = synthetic_report_for_task(&task, TaskReportStatus::DoneCandidate);
        let behavior = criterion_command_result_readonly_checked(
            &task.acceptance,
            CommandRole::AuthoritativeAcceptance,
            dir.path(),
            NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
        )
        .await
        .unwrap();
        let decision = settle_report_decision(&report, behavior);
        assert!(
            matches!(decision, TaskDecision::PassedByAcceptance { .. }),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn finalize_advisory_pending_when_artifact_still_red() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", TaskStatus::Done);
        let o = opts(ws.path(), jr.path(), "plan_finalize_advisory_red");
        let paths = RunPaths::new(jr.path(), "plan_finalize_advisory_red");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_advisory_red".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let res = finalize_completion(&state, &o, &mut recorder)
            .await
            .unwrap();

        assert_eq!(res.outcome, RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("\"reason\":\"advisory_pending\""));
        assert!(events.contains("\"task\":\"t1\""));
        assert!(!events.contains("\"type\":\"run.completed\""));
    }

    #[tokio::test]
    async fn finalize_completes_when_artifact_now_green() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", TaskStatus::Done);
        let o = opts(ws.path(), jr.path(), "plan_finalize_advisory_green");
        let paths = RunPaths::new(jr.path(), "plan_finalize_advisory_green");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_advisory_green".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let res = finalize_completion(&state, &o, &mut recorder)
            .await
            .unwrap();

        assert_eq!(res.outcome, RunOutcome::Completed);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("\"type\":\"run.completed\""));
        assert!(!events.contains("advisory_pending"));
    }

    #[tokio::test]
    async fn finalize_overall_red_takes_priority_over_advisory() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap(),
        );
        state.mark_status("t1", TaskStatus::Done);
        let o = opts(
            ws.path(),
            jr.path(),
            "plan_finalize_overall_before_advisory",
        );
        let paths = RunPaths::new(jr.path(), "plan_finalize_overall_before_advisory");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_overall_before_advisory".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let res = finalize_completion(&state, &o, &mut recorder)
            .await
            .unwrap();

        assert_eq!(res.outcome, RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("\"reason\":\"overall_red\""));
        assert!(!events.contains("advisory_pending"));
    }

    fn acceptance(success: bool) -> crate::plan::contract::AcceptanceResult {
        let ev = crate::plan::contract::CommandEvidence {
            role: crate::plan::contract::CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: if success { "true" } else { "false" }.into(),
            exit_code: if success { Some(0) } else { Some(1) },
            success,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: String::new(),
            truncated: false,
            environment_failure: None,
        };
        if success {
            crate::plan::contract::AcceptanceResult::Pass { acceptance: ev }
        } else {
            crate::plan::contract::AcceptanceResult::CodeRed { acceptance: ev }
        }
    }

    fn code_red_evidence(id: &str) -> crate::plan::contract::CommandEvidence {
        crate::plan::contract::CommandEvidence {
            role: crate::plan::contract::CommandRole::AuthoritativeAcceptance,
            criterion_id: id.into(),
            command: "cargo test".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: format!("error[E0063]\n --> src/lib.rs:{id}:9"),
            truncated: false,
            environment_failure: None,
        }
    }

    #[tokio::test]
    async fn plan_remediation_parses_stamps_and_reviews_candidates() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = ScriptedRemediationPlanner {
            response: r#"{ "tasks": [ { "id": "model_id", "intent": "fix missing c field",
              "files_scope": ["src/lib.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#
                .into(),
        };
        let o = opts(ws.path(), jr.path(), "plan_remediation_ok");
        let paths = RunPaths::new(jr.path(), "plan_remediation_ok");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_remediation_ok".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let evidence = vec![code_red_evidence("t1_acc")];
        let outcome = plan_remediation(
            &provider,
            &o,
            &mut recorder,
            "t1",
            &evidence,
            &[],
            &[],
            1,
            None,
        )
        .await
        .unwrap();

        match outcome {
            PlanOutcome::Tasks(tasks) => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, "t1_r1_fix1");
                assert_eq!(tasks[0].acceptance.id, "t1_r1_fix1_acc");
                let meta = tasks[0].remediation.as_ref().expect("remediation meta");
                assert_eq!(meta.parent, "t1");
                assert_eq!(meta.round, 1);
                assert_eq!(meta.attempt_no, 1);
                assert!(meta.evidence_fingerprint.contains("t1_acc").not());
            }
            PlanOutcome::Escalated => panic!("expected remediation tasks"),
        }
    }

    #[test]
    fn remediation_restamps_both_lane_ids() {
        let task = parse_one(
            r#"{ "tasks": [ { "id": "orig", "intent": "x", "files_scope": ["src"],
              "acceptance_cmd": "cargo test x", "artifact_check_cmd": "grep -rq foo src", "max_turns": 4 } ] }"#,
        );
        let stamped = stamp_remediation_tasks(vec![task], "t3", "fp1", 1, &[]);
        let t = &stamped[0];
        assert_eq!(t.acceptance.id, format!("{}_acc", t.id));
        let art = t.artifact_check.as_ref().expect("artifact lane kept");
        assert_eq!(
            art.id,
            format!("{}_art", t.id),
            "remediation 必须重盖 artifact_check.id"
        );
        assert!(art.claim.contains(&t.id), "artifact claim 跟新 id");
    }

    #[tokio::test]
    async fn plan_remediation_escalates_when_review_gate_rejects_empty_scope() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = ScriptedRemediationPlanner {
            response: r#"{ "tasks": [ { "id": "bad", "intent": "fix vaguely",
              "files_scope": [], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#
                .into(),
        };
        let o = opts(ws.path(), jr.path(), "plan_remediation_bad");
        let paths = RunPaths::new(jr.path(), "plan_remediation_bad");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_remediation_bad".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let evidence = vec![code_red_evidence("t1_acc")];
        let outcome = plan_remediation(
            &provider,
            &o,
            &mut recorder,
            "t1",
            &evidence,
            &[],
            &[],
            1,
            None,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, PlanOutcome::Escalated));
    }

    #[tokio::test]
    async fn plan_preflight_refine_stamps_ids_and_inherits_scope_and_deps() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let o = opts(ws.path(), jr.path(), "plan_refine_unit");
        let paths = RunPaths::new(jr.path(), "plan_refine_unit");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_refine_unit".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let original = {
            let mut t = crate::plan::contract::parse_worklist(
                r#"{ "tasks": [ { "id": "t1", "intent": "add field c", "files_scope": ["src/lib.rs"],
                  "acceptance_cmd": "cargo test --lib", "artifact_check_cmd": "true", "max_turns": 4, "depends_on": ["t0"] } ] }"#,
            )
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
            t.status = crate::plan::contract::TaskStatus::InProgress;
            t
        };

        let refiner = ScriptedRefiner {
            refine_worklist:
                r#"{ "tasks": [ { "id": "stronger", "intent": "add field c and grep it",
              "files_scope": ["src/"],
              "acceptance_cmd": "grep -q 'pub c: u32' src/lib.rs && cargo test --lib",
              "artifact_check_cmd": "grep -q 'pub c: u32' src/lib.rs", "max_turns": 4 } ] }"#
                    .to_string(),
        };

        let planned = plan_preflight_refine(
            &refiner,
            &o,
            &mut recorder,
            &original,
            crate::plan::preflight::RefineKind::Strengthen,
            "t1",
            1,
        )
        .await
        .unwrap();

        match planned {
            PlanOutcome::Tasks(tasks) => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, "t1_r1_fix1");
                assert_eq!(tasks[0].acceptance.id, "t1_r1_fix1_acc");
                match &tasks[0].acceptance.verifier {
                    crate::goal::Verifier::Verifiable { check_cmd, .. } => {
                        assert_eq!(check_cmd, "cargo test --lib");
                    }
                    _ => panic!("acceptance must remain behavior verifiable"),
                }
                let art = tasks[0]
                    .artifact_check
                    .as_ref()
                    .expect("refine must keep artifact lane");
                assert_eq!(art.id, "t1_r1_fix1_art");
                assert_eq!(tasks[0].depends_on, vec!["t0".to_string()]);
                assert_eq!(tasks[0].files_scope, vec!["src/lib.rs".to_string()]);
                assert_eq!(
                    tasks[0].acceptance_kind,
                    crate::plan::contract::AcceptanceKind::ChangeRequired
                );
                assert_eq!(tasks[0].status, crate::plan::contract::TaskStatus::Pending);
            }
            other => panic!("expected tasks, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn plan_preflight_refine_rejects_multi_task_output() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let o = opts(ws.path(), jr.path(), "plan_refine_multi");
        let paths = RunPaths::new(jr.path(), "plan_refine_multi");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_refine_multi".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let original = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src/lib.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 4 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        let refiner = ScriptedRefiner {
            refine_worklist: r#"{ "tasks": [
              { "id": "a", "intent": "x", "files_scope": ["src/a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 3 },
              { "id": "b", "intent": "y", "files_scope": ["src/b.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#
                .to_string(),
        };
        let planned = plan_preflight_refine(
            &refiner,
            &o,
            &mut recorder,
            &original,
            crate::plan::preflight::RefineKind::Strengthen,
            "t1",
            1,
        )
        .await
        .unwrap();
        assert!(matches!(planned, PlanOutcome::Escalated));
    }

    #[test]
    fn planner_prompt_teaches_ripple_as_one_atomic_task() {
        let p = PLANNER_SYSTEM;
        assert!(p.contains("RIPPLE CHANGES"), "缺连锁教学小节");
        assert!(p.contains("ONE atomic task"), "连锁须当一个原子任务");
        assert!(p.contains("smallest directory set"), "须教最小目录集 scope");
        assert!(
            p.contains("INSIDE the declared files_scope is not a scope change"),
            "「不算 scope change」必须收紧到 files_scope 之内"
        );
        assert!(
            p.contains("which kinds of symbols/sites"),
            "expected_diff_shape 须改成「预期哪类点·执行端枚举」(NIT)"
        );
    }

    #[test]
    fn planner_prompt_drops_one_directory_and_widen_scope_wording() {
        // BLOCK 1：files_scope 规则从「or one directory」松成小撮文件/目录。
        assert!(
            !PLANNER_SYSTEM.contains("or one directory"),
            "旧的单目录规则文字还在·与多目录 scope 自相矛盾"
        );
        assert!(
            PLANNER_SYSTEM.contains("files and/or directories"),
            "松过的 files_scope 规则文字缺失"
        );
        // 自我逼停的旧 stop 措辞（卡 scope_change 的真因）必须去掉。
        assert!(
            !PLANNER_SYSTEM.contains("if you must widen scope"),
            "自我逼停的 widen-scope stop 还在"
        );
        assert!(
            PLANNER_SYSTEM.contains("genuine boundary change"),
            "掰正后的 stop 措辞缺失"
        );
        assert!(
            PLANNER_SYSTEM.contains("outside files_scope"),
            "sites 落在 files_scope 之外时才该停——措辞缺失"
        );
    }

    #[test]
    fn planner_prompt_teaches_split_only_independent_concerns() {
        let p = PLANNER_SYSTEM;
        assert!(p.contains("Split only independent concerns"), "缺拆分纪律");
        assert!(
            p.contains("do NOT split by file"),
            "缺「需一起改的别按文件拆」纪律"
        );
        assert!(
            p.contains("do NOT give broad overlapping directory scopes"),
            "缺「别给多个独立任务套宽重叠 scope」纪律(FIX1)"
        );
        assert!(
            p.contains("multiple independent tasks"),
            "缺「多个独立任务」措辞"
        );
    }

    #[test]
    fn remediation_prompt_aligns_ripple_scope_and_stop() {
        let p = REMEDIATION_SYSTEM;
        assert!(p.contains("If the repair itself ripples"), "补救缺连锁教学");
        assert!(
            p.contains("directory-level files_scope"),
            "补救连锁须允许目录级 scope"
        );
        assert!(
            !p.contains("stop if scope must widen"),
            "补救里自我逼停的旧 stop 还在"
        );
        assert!(
            p.contains("not for another in-scope use site of the same repair"),
            "掰正后的补救 stop 措辞缺失"
        );
        assert!(p.contains("keep it ONE task"), "补救连锁须当一个任务(FIX1)");
        assert!(
            p.contains("fix every site"),
            "补救须让执行端修全所有点(FIX1)"
        );
    }

    #[test]
    fn both_planner_prompts_require_fail_to_pass_acceptance() {
        for p in [PLANNER_SYSTEM, REMEDIATION_SYSTEM] {
            let low = p.to_ascii_lowercase();
            assert!(
                low.contains("fail-to-pass") || low.contains("fail before"),
                "prompt missing fail-to-pass guidance"
            );
            assert!(
                low.contains("grep") || low.contains("artifact"),
                "prompt missing 'point at the real artifact' guidance"
            );
        }
    }

    #[test]
    fn prompts_emit_two_lanes_by_name_not_combined() {
        for p in [PLANNER_SYSTEM, REMEDIATION_SYSTEM, PREFLIGHT_REFINE_SYSTEM] {
            assert!(
                p.contains("artifact_check_cmd"),
                "prompt must ask for separate artifact lane"
            );
            assert!(
                !p.contains("AND run the tests"),
                "毒句『AND run the tests』必须消失"
            );
            assert!(!p.contains("grep for that field AND"), "毒句必须消失");
            assert!(
                p.contains("by name") || p.contains("symbol name") || p.contains("符号名"),
                "结构检查必须按名·不抠类型拼写"
            );
        }
    }

    #[test]
    fn prompts_use_by_name_search_dir_and_advisory() {
        for p in [PLANNER_SYSTEM, REMEDIATION_SYSTEM, PREFLIGHT_REFINE_SYSTEM] {
            assert!(p.contains("stable symbol name"), "按名");
            assert!(
                p.contains("grep -rq") || p.contains("search the whole directory"),
                "搜目录·{:.40}",
                p
            );
            assert!(p.contains("not a single file"), "不指定单文件");
            assert!(
                p.contains("advisory") && p.contains("not a hard gate"),
                "advisory 非硬过关"
            );
            assert!(!p.contains("&&"), "别用 && 捏一条");
            assert!(!p.to_lowercase().contains("and run the tests"), "删毒句");
        }
    }

    #[test]
    fn planner_prompts_do_not_tell_planner_to_keep_max_turns_small() {
        assert!(!PLANNER_SYSTEM.contains("Keep max_turns small"));
        assert!(!REMEDIATION_SYSTEM.contains("Keep max_turns small"));
        assert!(!PREFLIGHT_REFINE_SYSTEM.contains("Keep max_turns small"));
    }

    #[tokio::test]
    async fn preflight_pre_green_supersedes_original_and_appends_replacement() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let provider = PlannerThenRefiner {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "add field", "files_scope": ["src/lib.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#.to_string(),
            refine_worklist: r#"{ "tasks": [ { "id": "stronger", "intent": "add field and grep", "files_scope": ["src/lib.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#.to_string(),
        };
        let mut o = opts(ws.path(), jr.path(), "plan_pre_green");
        o.preflight_gate = true;

        let _ = run_plan(provider, o).await.unwrap();

        let state_path = RunPaths::new(jr.path(), "plan_pre_green")
            .run_dir
            .join("plan_state.json");
        let state: RunState = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();

        let t1 = state.worklist.iter().find(|t| t.id == "t1").unwrap();
        assert!(matches!(t1.status, TaskStatus::Superseded { .. }));
        assert!(state.worklist.iter().any(|t| t.id == "t1_r1_fix1"));
        assert_eq!(state.preflight_refine_attempts.get("t1"), Some(&1));
        assert_eq!(
            state.preflight_refine_lineage.get("t1_r1_fix1"),
            Some(&"t1".to_string())
        );
    }

    #[tokio::test]
    async fn preflight_code_red_proceeds_into_agent_and_consumes_step() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = ScriptedPlanner {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#.to_string(),
        };
        let mut o = opts(ws.path(), jr.path(), "plan_code_red_proceeds");
        o.preflight_gate = true;
        let res = run_plan(provider, o).await.unwrap();

        let state_path = RunPaths::new(jr.path(), "plan_code_red_proceeds")
            .run_dir
            .join("plan_state.json");
        let state: RunState = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert!(state.steps_used >= 1);
        assert!(matches!(res.outcome, RunOutcome::NeedsDecision));
    }

    #[tokio::test]
    async fn preflight_infra_red_suspends_without_consuming_step() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = ScriptedPlanner {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "echo connection refused; exit 1", "max_turns": 3 } ] }"#.to_string(),
        };
        let mut o = opts(ws.path(), jr.path(), "plan_infra_suspend");
        o.preflight_gate = true;
        let res = run_plan(provider, o).await.unwrap();

        let state_path = RunPaths::new(jr.path(), "plan_infra_suspend")
            .run_dir
            .join("plan_state.json");
        let state: RunState = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(state.steps_used, 0);
        let t1 = state.worklist.iter().find(|t| t.id == "t1").unwrap();
        assert!(matches!(t1.status, TaskStatus::Pending));
        assert!(matches!(res.outcome, RunOutcome::NeedsDecision));
    }

    #[tokio::test]
    async fn preflight_read_only_violation_suspends() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = ScriptedPlanner {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "printf x > sneaky.txt", "max_turns": 3 } ] }"#.to_string(),
        };
        let mut o = opts(ws.path(), jr.path(), "plan_readonly_suspend");
        o.preflight_gate = true;
        let res = run_plan(provider, o).await.unwrap();
        let state_path = RunPaths::new(jr.path(), "plan_readonly_suspend")
            .run_dir
            .join("plan_state.json");
        let state: RunState = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(state.steps_used, 0);
        assert!(matches!(res.outcome, RunOutcome::NeedsDecision));
    }

    #[tokio::test]
    async fn preflight_gate_off_bypasses_gate() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = ScriptedPlanner {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#.to_string(),
        };
        let mut o = opts(ws.path(), jr.path(), "plan_gate_off");
        o.preflight_gate = false;
        let res = run_plan(provider, o).await.unwrap();
        assert!(matches!(res.outcome, RunOutcome::Completed));
    }

    #[tokio::test]
    async fn preflight_invariant_task_skips_gate() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = ScriptedPlanner {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "keep green", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3, "acceptance_kind": "invariant" } ] }"#.to_string(),
        };
        let mut o = opts(ws.path(), jr.path(), "plan_invariant_skip");
        o.preflight_gate = true;
        let res = run_plan(provider, o).await.unwrap();
        assert!(matches!(res.outcome, RunOutcome::Completed));
    }

    #[tokio::test]
    async fn preflight_pre_green_never_runs_agent_and_keeps_budgets_independent() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let provider = PanicOnChild {
            worklist: r#"{ "tasks": [ { "id": "t1", "intent": "add field", "files_scope": ["src/lib.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#.to_string(),
            refine_worklist: r#"{ "tasks": [ { "id": "stronger", "intent": "add field stronger", "files_scope": ["src/lib.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#.to_string(),
        };
        let mut o = opts(ws.path(), jr.path(), "plan_pre_green_no_agent");
        o.preflight_gate = true;
        let res = run_plan(provider, o).await.unwrap();

        let state_path = RunPaths::new(jr.path(), "plan_pre_green_no_agent")
            .run_dir
            .join("plan_state.json");
        let state: RunState = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(state.steps_used, 0);
        assert_eq!(state.replan_rounds, 0);
        assert!(state
            .worklist
            .iter()
            .any(|t| matches!(t.status, TaskStatus::RejectedAcceptance { .. })));
        assert!(matches!(res.outcome, RunOutcome::NeedsDecision));
    }

    #[tokio::test]
    async fn resume_uses_persisted_preflight_gate_not_cli() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.preflight_gate = false;
        let paths = RunPaths::new(jr.path(), "plan_resume_gate");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

        let mut o = opts(ws.path(), jr.path(), "plan_resume_gate");
        o.preflight_gate = true;
        let provider = ScriptedPlanner {
            worklist: String::new(),
        };
        let res = resume_plan(provider, o).await.unwrap();
        assert!(matches!(res.outcome, RunOutcome::Completed));
    }

    #[tokio::test]
    async fn finalize_completes_with_done_and_superseded_mix() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let mut tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [
              { "id": "t1", "intent": "done one", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 },
              { "id": "t2", "intent": "superseded one", "files_scope": ["b.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        tasks[0].status = TaskStatus::Done;
        tasks[1].status = TaskStatus::Superseded {
            by: vec!["t2_r1_fix1".into()],
            reason: "acceptance_passed_before_execution".into(),
        };
        let state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        let o = opts(ws.path(), jr.path(), "plan_finalize_mix");
        let paths = RunPaths::new(jr.path(), "plan_finalize_mix");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_mix".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let res = finalize_completion(&state, &o, &mut recorder)
            .await
            .unwrap();
        assert!(matches!(res.outcome, RunOutcome::Completed));
    }

    #[tokio::test]
    async fn task_code_red_appends_remediation_reverifies_green_and_completes() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let provider = ReplanProvider {
            initial_worklist:
                r#"{ "tasks": [ { "id": "t1", "intent": "original fails until fixed exists",
              "files_scope": ["fixed"], "acceptance_cmd": "test -f fixed", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#
                    .into(),
            remediation_worklist:
                r#"{ "tasks": [ { "id": "model_fix", "intent": "create fixed marker",
              "files_scope": ["fixed"], "acceptance_cmd": "test -f fixed", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#
                    .into(),
        };

        let res = run_plan(
            provider,
            opts(ws.path(), jr.path(), "plan_task_replan_success"),
        )
        .await
        .unwrap();

        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
        let state: RunState = serde_json::from_slice(
            &std::fs::read(
                jr.path()
                    .join(".myagenthubs/runs/plan_task_replan_success/plan_state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state.replan_rounds, 1);
        assert!(state.worklist.iter().any(|t| t.remediation.is_some()));
        assert!(state
            .worklist
            .iter()
            .all(|t| matches!(t.status, TaskStatus::Done)));
    }

    #[tokio::test]
    async fn all_blocked_reverify_policy_exits_without_replan() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "blocked",
              "files_scope": ["a.rs"], "acceptance_cmd": "printf x > a.rs", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status(
            "t1",
            TaskStatus::Blocked {
                reason: "failed_by_acceptance: t1_acc".into(),
            },
        );

        let paths = RunPaths::new(jr.path(), "plan_task_replan_policy");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_task_replan_policy".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let res = run_plan_loop(
            ScriptedPlanner {
                worklist: String::new(),
            },
            &opts(ws.path(), jr.path(), "plan_task_replan_policy"),
            state,
            &paths.run_dir.join("plan_state.json"),
            &mut recorder,
        )
        .await
        .unwrap();

        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("failed_by_policy"));
        assert!(!events.contains("plan.replan.appended"));
    }

    #[tokio::test]
    async fn all_blocked_round_budget_full_escalates() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "blocked",
              "files_scope": ["a.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.replan_rounds = DEFAULT_MAX_REPLAN_ROUNDS;
        state.mark_status(
            "t1",
            TaskStatus::Blocked {
                reason: "failed_by_acceptance: t1_acc".into(),
            },
        );

        let paths = RunPaths::new(jr.path(), "plan_task_replan_budget");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_task_replan_budget".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let res = run_plan_loop(
            ScriptedPlanner {
                worklist: String::new(),
            },
            &opts(ws.path(), jr.path(), "plan_task_replan_budget"),
            state,
            &paths.run_dir.join("plan_state.json"),
            &mut recorder,
        )
        .await
        .unwrap();

        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("plan.replan.escalated"));
        assert!(events.contains("budget"));
    }

    #[tokio::test]
    async fn overall_code_red_appends_remediation_then_completes() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let mut o = opts(ws.path(), jr.path(), "plan_overall_replan_success");
        o.checks =
            crate::goal::parse_criteria(&["cmd: test -f overall_fixed".to_string()]).unwrap();

        let provider = OverallReplanProvider {
            initial_worklist: r#"{ "tasks": [ { "id": "t1", "intent": "base task",
              "files_scope": ["seed"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#
                .into(),
            remediation_worklist:
                r#"{ "tasks": [ { "id": "overall_fix", "intent": "create overall marker",
              "files_scope": ["overall_fixed"], "acceptance_cmd": "test -f overall_fixed", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#
                    .into(),
        };

        let res = run_plan(provider, o).await.unwrap();

        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
        let state: RunState = serde_json::from_slice(
            &std::fs::read(
                jr.path()
                    .join(".myagenthubs/runs/plan_overall_replan_success/plan_state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state.replan_rounds, 1);
        assert!(state.worklist.iter().any(|t| t
            .remediation
            .as_ref()
            .is_some_and(|m| m.parent == "overall")));
    }

    #[tokio::test]
    async fn overall_not_run_does_not_enter_replan() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let mut c = crate::goal::parse_criteria(&["cmd: true".to_string()])
            .unwrap()
            .remove(0);
        c.authored_by = crate::goal::AuthoredBy::Agent;
        c.approval = crate::goal::Approval::Pending;

        let mut o = opts(ws.path(), jr.path(), "plan_overall_not_run_no_replan");
        o.checks = vec![c];

        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "base",
          "files_scope": ["seed"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;

        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            o,
        )
        .await
        .unwrap();

        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_overall_not_run_no_replan/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("stopped_unvalidated"));
        assert!(!events.contains("plan.replan.appended"));
    }

    #[tokio::test]
    async fn resume_keeps_replan_rounds() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.replan_rounds = 2;
        state.mark_status("t1", TaskStatus::InProgress);

        let paths = RunPaths::new(jr.path(), "plan_resume_replan_rounds");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

        let res = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            opts(ws.path(), jr.path(), "plan_resume_replan_rounds"),
        )
        .await
        .unwrap();

        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
        let after: RunState =
            serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
                .unwrap();
        assert_eq!(after.replan_rounds, 2);
    }

    #[test]
    fn blocked_child_candidate_does_not_prevent_acceptance_pass() {
        let decision = settle_report_decision(
            &fake_report(crate::plan::contract::TaskReportStatus::BlockedCandidate),
            acceptance(true),
        );
        assert!(matches!(
            decision,
            crate::plan::contract::TaskDecision::PassedByAcceptance { advisory: None, .. }
        ));
    }

    #[tokio::test]
    async fn child_blocked_outcome_does_not_prevent_acceptance_pass() {
        let dir = tempfile::tempdir().unwrap();
        init_git(dir.path());
        let task = parse_one(
            r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src"],
              "acceptance_cmd": "true", "max_turns": 5 } ] }"#,
        );
        let mut report = synthetic_report_for_task(&task, TaskReportStatus::BlockedCandidate);
        report.child_outcome = crate::plan::contract::ChildRunOutcome::Blocked;

        let decision = run_task_acceptance(
            &task,
            &report,
            dir.path(),
            NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
        )
        .await
        .unwrap();

        assert!(matches!(
            decision,
            crate::plan::contract::TaskDecision::PassedByAcceptance { advisory: None, .. }
        ));
    }

    #[test]
    fn settlement_distinguishes_policy_from_acceptance_failure() {
        let mut report = fake_report(crate::plan::contract::TaskReportStatus::DoneCandidate);
        report
            .changes
            .scope_violations
            .push(crate::plan::contract::ScopeViolation {
                path: "outside.rs".into(),
                reason: "超出 files_scope 白名单：outside.rs".into(),
            });
        let decision = settle_report_decision(&report, acceptance(false));
        assert!(matches!(
            decision,
            crate::plan::contract::TaskDecision::FailedByPolicy { .. }
        ));
    }

    #[test]
    fn settlement_code_red_is_failed_by_acceptance() {
        let decision = settle_report_decision(
            &fake_report(crate::plan::contract::TaskReportStatus::DoneCandidate),
            acceptance(false),
        );
        assert!(matches!(
            decision,
            crate::plan::contract::TaskDecision::FailedByAcceptance { .. }
        ));
    }

    #[tokio::test]
    async fn two_tasks_all_done_completes() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 },
          { "id": "t2", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3, "depends_on": ["t1"] }
        ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_done"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
        let state: RunState = serde_json::from_slice(
            &std::fs::read(
                jr.path()
                    .join(".myagenthubs/runs/plan_done/plan_state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(state
            .worklist
            .iter()
            .all(|t| matches!(t.status, TaskStatus::Done)));
    }

    #[tokio::test]
    async fn blocked_task_escalates_exit4() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_blocked"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_blocked/events.jsonl"),
        )
        .unwrap();
        assert!(
            events.contains("\"type\":\"run.needs_decision\"") && events.contains("blocked_tasks")
        );
    }

    // 任务级：acceptance 真红（"false"）→ 权威重跑 CodeRed → blocked → AllBlocked → exit4
    #[tokio::test]
    async fn task_acceptance_code_red_blocks() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        // child 自报 done（ScriptedPlanner 返回 "done"·journal 会绿）·但权威重跑 acceptance "false" → CodeRed
        // 注：1b 里 "false" 让 child 自身 evaluator 也红→journal 红·这里走预筛 blocked 亦可；
        // 为真测「权威重跑」路径·acceptance 用「child 自验易过、权威重跑必红」难构造·故此测覆盖预筛+权威红合一的 blocked 终态。
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_task_red"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
    }

    // 任务级：acceptance 命中 infra 签名 → 挂起 infra_red（不当代码红·B1/B2）
    #[tokio::test]
    async fn task_acceptance_infra_red_suspends() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "echo connection refused; exit 1", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_task_infra"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_task_infra/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("infra_red"));
    }

    // 任务级：acceptance "true" → 权威重跑 Pass → Done → 完成
    #[tokio::test]
    async fn task_acceptance_pass_completes() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_task_ok"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
    }

    #[tokio::test]
    async fn stale_scope_task_blocked_then_exit4() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        std::fs::write(ws.path().join("locked"), "x").unwrap();
        // files_scope = locked/x.rs·locked 是文件 → 开跑前复核挡下 → blocked → AllBlocked → exit4
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["locked/x.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_stale"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events =
            std::fs::read_to_string(jr.path().join(".myagenthubs/runs/plan_stale/events.jsonl"))
                .unwrap();
        assert!(events.contains("scope_stale"));
        let state: RunState = serde_json::from_slice(
            &std::fs::read(
                jr.path()
                    .join(".myagenthubs/runs/plan_stale/plan_state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(state.worklist.iter().any(|t| t.id == "t1"
            && matches!(t.status, crate::plan::contract::TaskStatus::Blocked { .. })));
    }

    #[tokio::test]
    async fn unreviewable_plan_escalates_exit4() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": [], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_unrev"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events =
            std::fs::read_to_string(jr.path().join(".myagenthubs/runs/plan_unrev/events.jsonl"))
                .unwrap();
        assert!(events.contains("plan_unreviewable"));
    }

    #[tokio::test]
    async fn plan_budget_exhausted_escalates_exit4() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 },
          { "id": "t2", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3, "depends_on": ["t1"] }
        ] }"#;
        let mut o = opts(ws.path(), jr.path(), "plan_budget");
        o.max_plan_steps = 1; // 只给 1 步 → 跑完 t1 后预算耗尽·不跑 t2
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            o,
        )
        .await
        .unwrap();

        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events =
            std::fs::read_to_string(jr.path().join(".myagenthubs/runs/plan_budget/events.jsonl"))
                .unwrap();
        assert!(events.contains("plan_budget_exhausted"));
        let state: RunState = serde_json::from_slice(
            &std::fs::read(
                jr.path()
                    .join(".myagenthubs/runs/plan_budget/plan_state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state.steps_used, 1);
        assert!(state
            .worklist
            .iter()
            .any(|t| t.id == "t1" && matches!(t.status, crate::plan::contract::TaskStatus::Done)));
    }

    #[tokio::test]
    async fn resume_reverifies_in_progress_task_green_marks_done() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        ).unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
        let paths = RunPaths::new(jr.path(), "plan_resume");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

        let res = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            opts(ws.path(), jr.path(), "plan_resume"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
        let after: RunState =
            serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
                .unwrap();
        assert!(after
            .worklist
            .iter()
            .all(|t| matches!(t.status, crate::plan::contract::TaskStatus::Done)));
    }

    #[tokio::test]
    async fn resume_reverify_runs_both_lanes_marks_done_on_artifact_red() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "artifact_check_cmd": "false", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
        let paths = RunPaths::new(jr.path(), "plan_resume_guard");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

        let _res = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            opts(ws.path(), jr.path(), "plan_resume_guard"),
        )
        .await
        .unwrap();
        let after: RunState =
            serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
                .unwrap();
        let t1 = after.worklist.iter().find(|t| t.id == "t1").unwrap();
        assert!(matches!(
            &t1.status,
            crate::plan::contract::TaskStatus::Done
        ));
    }

    async fn resume_legacy_change_task_without_driver(
        run_id: &str,
        status: TaskStatus,
        acceptance_kind: crate::plan::contract::AcceptanceKind,
    ) -> (RunOutcome, String) {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let mut task = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "legacy", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .remove(0);
        task.acceptance_kind = acceptance_kind;
        task.status = status;
        let state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            vec![task],
            vec![],
        );
        let paths = RunPaths::new(jr.path(), run_id);
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

        let result = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            opts(ws.path(), jr.path(), run_id),
        )
        .await
        .unwrap();
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        (result.outcome, events)
    }

    #[tokio::test]
    async fn resume_blocks_change_required_without_driver_pending() {
        let (outcome, events) = resume_legacy_change_task_without_driver(
            "plan_resume_missing_driver_pending",
            TaskStatus::Pending,
            crate::plan::contract::AcceptanceKind::ChangeRequired,
        )
        .await;
        assert_eq!(outcome, RunOutcome::NeedsDecision);
        assert!(events.contains("\"reason\":\"resume_missing_driver\""));
    }

    #[tokio::test]
    async fn resume_blocks_change_required_without_driver_in_progress() {
        let (outcome, events) = resume_legacy_change_task_without_driver(
            "plan_resume_missing_driver_in_progress",
            TaskStatus::InProgress,
            crate::plan::contract::AcceptanceKind::ChangeRequired,
        )
        .await;
        assert_eq!(outcome, RunOutcome::NeedsDecision);
        assert!(events.contains("\"reason\":\"resume_missing_driver\""));
    }

    #[tokio::test]
    async fn resume_blocks_change_required_without_driver_blocked_failed_acceptance() {
        let (outcome, events) = resume_legacy_change_task_without_driver(
            "plan_resume_missing_driver_blocked",
            TaskStatus::Blocked {
                reason: "failed_by_acceptance: t1_acc".into(),
            },
            crate::plan::contract::AcceptanceKind::ChangeRequired,
        )
        .await;
        assert_eq!(outcome, RunOutcome::NeedsDecision);
        assert!(events.contains("\"reason\":\"resume_missing_driver\""));
    }

    #[tokio::test]
    async fn resume_allows_pass_through_states_without_driver() {
        let cases = [
            (
                "plan_resume_missing_driver_allow_done",
                TaskStatus::Done,
                crate::plan::contract::AcceptanceKind::ChangeRequired,
            ),
            (
                "plan_resume_missing_driver_allow_invariant",
                TaskStatus::Pending,
                crate::plan::contract::AcceptanceKind::Invariant,
            ),
            (
                "plan_resume_missing_driver_allow_policy_blocked",
                TaskStatus::Blocked {
                    reason: "failed_by_policy: a.rs".into(),
                },
                crate::plan::contract::AcceptanceKind::ChangeRequired,
            ),
            (
                "plan_resume_missing_driver_allow_rejected",
                TaskStatus::RejectedAcceptance {
                    reason: "preflight_refine_exhausted".into(),
                },
                crate::plan::contract::AcceptanceKind::ChangeRequired,
            ),
            (
                "plan_resume_missing_driver_allow_children",
                TaskStatus::BlockedByChildren,
                crate::plan::contract::AcceptanceKind::ChangeRequired,
            ),
        ];

        for (run_id, status, acceptance_kind) in cases {
            let (_outcome, events) =
                resume_legacy_change_task_without_driver(run_id, status, acceptance_kind).await;
            assert!(
                !events.contains("\"reason\":\"resume_missing_driver\""),
                "{run_id} was incorrectly blocked: {events}"
            );
        }
    }

    #[tokio::test]
    async fn overall_acceptance_red_escalates_exit4() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let mut o = opts(ws.path(), jr.path(), "plan_overall_red");
        o.checks = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap(); // 目标总验收注定红
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            o,
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_overall_red/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("overall_red"));
    }

    #[tokio::test]
    async fn finalize_not_run_is_stopped_not_needs_replan() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let mut c = crate::goal::parse_criteria(&["cmd: true".to_string()])
            .unwrap()
            .remove(0);
        c.authored_by = crate::goal::AuthoredBy::Agent;
        c.approval = crate::goal::Approval::Pending;

        let state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            vec![],
            vec![c],
        );
        let o = opts(ws.path(), jr.path(), "plan_finalize_not_run");
        let paths = RunPaths::new(jr.path(), "plan_finalize_not_run");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_not_run".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
            .await
            .unwrap();

        assert!(matches!(outcome, FinalizeOutcome::Stopped { .. }));
    }

    #[tokio::test]
    async fn finalize_pure_code_red_is_needs_replan() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let checks = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap();
        let state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            vec![],
            checks,
        );
        let o = opts(ws.path(), jr.path(), "plan_finalize_code_red");
        let paths = RunPaths::new(jr.path(), "plan_finalize_code_red");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_code_red".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
            .await
            .unwrap();

        assert!(matches!(outcome, FinalizeOutcome::NeedsReplan { .. }));
    }

    #[tokio::test]
    async fn finalize_infra_is_typed_infra() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let checks =
            crate::goal::parse_criteria(&["cmd: echo connection refused; exit 1".to_string()])
                .unwrap();
        let state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            vec![],
            checks,
        );
        let o = opts(ws.path(), jr.path(), "plan_finalize_infra_typed");
        let paths = RunPaths::new(jr.path(), "plan_finalize_infra_typed");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_infra_typed".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
            .await
            .unwrap();

        assert!(matches!(outcome, FinalizeOutcome::Infra { .. }));
    }

    #[tokio::test]
    async fn finalize_read_only_delta_is_typed_policy() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());

        let checks =
            crate::goal::parse_criteria(&["cmd: printf touched > overall.txt".to_string()])
                .unwrap();
        let state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            vec![],
            checks,
        );
        let o = opts(ws.path(), jr.path(), "plan_finalize_policy_typed");
        let paths = RunPaths::new(jr.path(), "plan_finalize_policy_typed");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_finalize_policy_typed".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let outcome = finalize_completion_outcome(&state, &o, &mut recorder)
            .await
            .unwrap();

        assert!(matches!(outcome, FinalizeOutcome::Policy { .. }));
    }

    #[tokio::test]
    async fn finalize_done_task_reverify_uses_failed_by_acceptance_decision() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::Done);
        let o = opts(ws.path(), jr.path(), "plan_reverify_decision");
        let paths = RunPaths::new(jr.path(), "plan_reverify_decision");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_reverify_decision".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let res = finalize_completion(&state, &o, &mut recorder)
            .await
            .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("overall_red"));
        assert!(events.contains("failed_by_acceptance"));
        assert!(!events.contains("failed_by_policy"));
    }

    #[tokio::test]
    async fn resume_in_progress_code_red_goes_pending_not_done_or_policy() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
        let paths = RunPaths::new(jr.path(), "plan_resume_code_red");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();
        let mut o = opts(ws.path(), jr.path(), "plan_resume_code_red");
        o.max_plan_steps = 1;
        let res = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            o,
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let after: RunState =
            serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
                .unwrap();
        assert!(after.worklist.iter().any(|t| {
            t.id == "t1" && matches!(t.status, crate::plan::contract::TaskStatus::Pending)
        }));
    }

    #[tokio::test]
    async fn overall_check_records_command_role_overall_check() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let mut o = opts(ws.path(), jr.path(), "plan_overall_role");
        o.checks = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap();
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            o,
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_overall_role/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("overall_check"));
    }

    #[tokio::test]
    async fn finalize_infra_red_reported_as_infra_with_observed() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let mut o = opts(ws.path(), jr.path(), "plan_infra");
        o.checks =
            crate::goal::parse_criteria(&["cmd: echo connection refused; exit 1".to_string()])
                .unwrap();
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            o,
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events =
            std::fs::read_to_string(jr.path().join(".myagenthubs/runs/plan_infra/events.jsonl"))
                .unwrap();
        assert!(events.contains("infra_red"));
    }

    // B3：整盘重验逮「done 任务的 acceptance 后来红了」——直接构造全 Done + 一条 acceptance "false" 测 finalize
    #[tokio::test]
    async fn finalize_reverify_catches_broken_done_task_acceptance() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "false", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        ).unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::Done); // 假装已 done
        let o = opts(ws.path(), jr.path(), "plan_reverify");
        let paths = RunPaths::new(jr.path(), "plan_reverify");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_reverify".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let res = finalize_completion(&state, &o, &mut recorder)
            .await
            .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("overall_red") && events.contains("t1 acceptance"));
    }

    // B1：resume 时 in-progress 任务 acceptance 命中 infra → 挂起（不当没做完重跑）
    #[tokio::test]
    async fn resume_in_progress_infra_suspends() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "echo connection refused; exit 1", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        ).unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
        let paths = RunPaths::new(jr.path(), "plan_resume_infra");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();
        let res = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            opts(ws.path(), jr.path(), "plan_resume_infra"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.run_dir.join("events.jsonl")).unwrap();
        assert!(events.contains("infra_red"));
    }

    #[tokio::test]
    async fn resume_without_state_falls_back_to_fresh_run() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = resume_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_resume_fresh"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::Completed);
    }

    #[tokio::test]
    async fn resume_continues_budget_does_not_reset() {
        // B5：崩溃前已用掉预算·resume 不归零
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [
              { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 },
              { "id": "t2", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 }
            ] }"#,
        ).unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::Done);
        state.steps_used = 1; // 崩溃前已用 1 步
        let paths = RunPaths::new(jr.path(), "plan_resume_budget");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();

        let mut o = opts(ws.path(), jr.path(), "plan_resume_budget");
        o.max_plan_steps = 1; // 预算只 1 步·已用 1 → resume 不该再跑 t2
        let res = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            o,
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let after: RunState =
            serde_json::from_slice(&std::fs::read(paths.run_dir.join("plan_state.json")).unwrap())
                .unwrap();
        assert!(after.worklist.iter().any(
            |t| t.id == "t2" && matches!(t.status, crate::plan::contract::TaskStatus::Pending)
        ));
    }

    #[tokio::test]
    async fn task_acceptance_read_only_delta_is_failed_by_policy() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "printf $$ > a.rs", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_readonly_task"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_readonly_task/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("failed_by_policy"));
        assert!(events.contains("acceptance_read_only_violation"));
    }

    #[tokio::test]
    async fn overall_check_read_only_delta_is_failed_by_policy() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
          "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let mut o = opts(ws.path(), jr.path(), "plan_readonly_overall");
        o.checks =
            crate::goal::parse_criteria(&["cmd: printf $$ > overall.txt".to_string()]).unwrap();
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            o,
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_readonly_overall/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("failed_by_policy"));
        assert!(events.contains("overall.txt"));
    }

    #[tokio::test]
    async fn finalize_done_task_reverify_read_only_delta_is_failed_by_policy() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "printf $$ > a.rs", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::Done);
        let o = opts(ws.path(), jr.path(), "plan_readonly_finalize");
        let paths = RunPaths::new(jr.path(), "plan_readonly_finalize");
        paths.create_dirs().unwrap();
        let mut recorder = crate::events::EventRecorder::new(
            "plan_readonly_finalize".to_string(),
            None,
            Some(ws.path().to_string_lossy().into_owned()),
            &paths.events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let res = finalize_completion(&state, &o, &mut recorder)
            .await
            .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.events_path).unwrap();
        assert!(events.contains("failed_by_policy"));
        assert!(events.contains("acceptance_read_only_violation"));
    }

    #[tokio::test]
    async fn resume_reverify_read_only_delta_is_failed_by_policy() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let tasks = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "printf $$ > a.rs", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#,
        )
        .unwrap();
        let mut state = RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        );
        state.mark_status("t1", crate::plan::contract::TaskStatus::InProgress);
        let paths = RunPaths::new(jr.path(), "plan_readonly_resume");
        paths.create_dirs().unwrap();
        save_state(&paths.run_dir.join("plan_state.json"), &state).unwrap();
        let res = resume_plan(
            ScriptedPlanner {
                worklist: String::new(),
            },
            opts(ws.path(), jr.path(), "plan_readonly_resume"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(paths.run_dir.join("events.jsonl")).unwrap();
        assert!(events.contains("failed_by_policy"));
        assert!(events.contains("acceptance_read_only_violation"));
    }

    #[tokio::test]
    async fn infra_red_read_only_delta_becomes_policy_failure() {
        let ws = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let criterion = crate::goal::parse_criteria(&[
            "cmd: printf $$ > infra.txt; echo connection refused; exit 1".to_string(),
        ])
        .unwrap()
        .remove(0);
        let baseline = capture_baseline(ws.path()).unwrap();
        let result = criterion_command_result_readonly_checked_with_baseline(
            &criterion,
            CommandRole::OverallCheck,
            ws.path(),
            NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &baseline,
        )
        .await
        .unwrap();
        assert!(matches!(result, AcceptanceResult::PolicyFailure { .. }));
    }

    #[tokio::test]
    async fn infra_red_without_delta_stays_infra_red() {
        let ws = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let criterion =
            crate::goal::parse_criteria(&["cmd: echo connection refused; exit 1".to_string()])
                .unwrap()
                .remove(0);
        let baseline = capture_baseline(ws.path()).unwrap();
        let result = criterion_command_result_readonly_checked_with_baseline(
            &criterion,
            CommandRole::OverallCheck,
            ws.path(),
            NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &baseline,
        )
        .await
        .unwrap();
        assert!(matches!(result, AcceptanceResult::InfraRed { .. }));
    }

    #[tokio::test]
    async fn bounce_reasons_worklist_bounce_event_records_reasons() {
        let ws = tempfile::tempdir().unwrap();
        let jr = tempfile::tempdir().unwrap();
        init_git(ws.path());
        let worklist = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": [], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 3 } ] }"#;
        let res = run_plan(
            ScriptedPlanner {
                worklist: worklist.into(),
            },
            opts(ws.path(), jr.path(), "plan_bounce_reasons"),
        )
        .await
        .unwrap();
        assert_eq!(res.outcome, crate::orchestrator::RunOutcome::NeedsDecision);
        let events = std::fs::read_to_string(
            jr.path()
                .join(".myagenthubs/runs/plan_bounce_reasons/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("\"type\":\"plan.worklist.bounced\""));
        assert!(events.contains("\"reasons\""));
        assert!(events.contains("files_scope"));
    }
}
