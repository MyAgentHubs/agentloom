//! 计划契约：Planner 产的原子任务结构 + 严格解析。

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::goal::{Approval, AuthoredBy, Criterion, CriterionStatus, SuccessRule, Verifier};

/// 验收性质：change_required = 干完才该绿（进开工前闸）；invariant = 全程该绿（走全局 health-check·不进闸）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceKind {
    #[default]
    ChangeRequired,
    Invariant,
}

fn default_acceptance_kind() -> AcceptanceKind {
    AcceptanceKind::ChangeRequired
}

/// 任务在总账里的状态（只加不删·status 流转）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Blocked {
        reason: String,
    },
    /// 被自身的再拆子任务取代（第二刀用）。
    BlockedByChildren,
    /// 开工前验收就绿/非法（疑似太松/重复/没跑成）→ 被更强替代任务取代（开工前闸·非 Done 终态）。
    Superseded {
        by: Vec<String>,
        reason: String,
    },
    /// 开工前闸退回次数用尽/规划不收敛·放弃该任务的审计终态（随即 exit4）。
    RejectedAcceptance {
        reason: String,
    },
}

fn default_status() -> TaskStatus {
    TaskStatus::Pending
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationMeta {
    pub parent: String,
    pub evidence_fingerprint: String,
    pub attempt_no: usize,
    pub round: usize,
}

/// 一个原子任务的计划契约（spec §2.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTask {
    pub id: String,
    pub intent: String,
    pub files_scope: Vec<String>,
    #[serde(default)]
    pub forbidden_scope: Vec<String>,
    /// 行为道（spec 的 behavior_check）：测试/回归·高信任·永远单跑。
    pub acceptance: Criterion,
    /// 结构道（spec 的 artifact_check）：按符号名 grep·低信任·fail-to-pass·None=无（仅 invariant/legacy）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_check: Option<Criterion>,
    #[serde(default)]
    pub expected_diff_shape: String,
    #[serde(default)]
    pub stop_conditions: Vec<String>,
    pub depends_on: Vec<String>,
    pub max_turns: usize,
    #[serde(default = "default_acceptance_kind")]
    pub acceptance_kind: AcceptanceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<RemediationMeta>,
    #[serde(default = "default_status")]
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanTaskSpec {
    id: String,
    intent: String,
    files_scope: Vec<String>,
    #[serde(default)]
    forbidden_scope: Vec<String>,
    acceptance_cmd: String,
    #[serde(default)]
    artifact_check_cmd: Option<String>,
    #[serde(default)]
    expected_diff_shape: String,
    #[serde(default)]
    stop_conditions: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    max_turns: usize,
    #[serde(default = "default_acceptance_kind")]
    acceptance_kind: AcceptanceKind,
}

#[derive(Debug, Clone, Deserialize)]
struct WorklistSpec {
    tasks: Vec<PlanTaskSpec>,
}

/// acceptance shell 命令 → harness-approved 可执行 Criterion。
/// authored_by=User + approval=Approved 镜像 goal::parse_criteria：
/// Verifiable with approval == Approved satisfies is_executable_verifiable(), ensuring the evaluator runs the check.
fn harness_approved_criterion(task_id: &str, cmd: &str) -> Criterion {
    Criterion {
        id: format!("{task_id}_acc"),
        claim: format!("acceptance for task {task_id}"),
        scope: None,
        authored_by: AuthoredBy::User,
        approval: Approval::Approved,
        verifier: Verifier::Verifiable {
            check_cmd: cmd.to_string(),
            success: SuccessRule::ExitZero,
            timeout_s: 120,
            network: None,
        },
        status: CriterionStatus::Pending,
        evidence_ref: None,
    }
}

/// 结构道（artifact）verifiable·id 用 `_art` 后缀（与行为道 `_acc` 区分）。
fn harness_approved_artifact(task_id: &str, cmd: &str) -> Criterion {
    Criterion {
        id: format!("{task_id}_art"),
        claim: format!("artifact check for task {task_id}"),
        scope: None,
        authored_by: AuthoredBy::User,
        approval: Approval::Approved,
        verifier: Verifier::Verifiable {
            check_cmd: cmd.to_string(),
            success: SuccessRule::ExitZero,
            timeout_s: 120,
            network: None,
        },
        status: CriterionStatus::Pending,
        evidence_ref: None,
    }
}

/// 严格解析 worklist JSON。缺必填字段报错；容忍多余字段。
pub fn parse_worklist(json: &str) -> Result<Vec<PlanTask>> {
    let spec: WorklistSpec = serde_json::from_str(crate::plan::json::extract_json_object(json))?;
    let tasks = spec
        .tasks
        .into_iter()
        .map(|t| PlanTask {
            acceptance: harness_approved_criterion(&t.id, &t.acceptance_cmd),
            artifact_check: t
                .artifact_check_cmd
                .as_ref()
                .map(|cmd| harness_approved_artifact(&t.id, cmd)),
            id: t.id,
            intent: t.intent,
            files_scope: t.files_scope,
            forbidden_scope: t.forbidden_scope,
            expected_diff_shape: t.expected_diff_shape,
            stop_conditions: t.stop_conditions,
            depends_on: t.depends_on,
            max_turns: t.max_turns,
            acceptance_kind: t.acceptance_kind,
            remediation: None,
            status: TaskStatus::Pending,
        })
        .collect();
    Ok(tasks)
}

pub const TASK_REPORT_SCHEMA_VERSION: u32 = 1;

fn task_report_schema_v1() -> u32 {
    TASK_REPORT_SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReport {
    #[serde(default = "task_report_schema_v1")]
    pub schema_version: u32,
    pub task_id: String,
    pub child_run_id: String,
    #[serde(default)]
    pub status: TaskReportStatus,
    pub acceptance: Criterion,
    #[serde(default)]
    pub child_outcome: ChildRunOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_evaluation: Option<ChildEvaluation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopSummary>,
    #[serde(default)]
    pub changes: ChangeSet,
    #[serde(default)]
    pub evidence: Vec<TaskEvidence>,
    #[serde(default)]
    pub narrative: TaskNarrative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskReportStatus {
    DoneCandidate,
    BlockedCandidate,
    NeedsDecisionCandidate,
    StoppedUnvalidated,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChildRunOutcome {
    Completed,
    Blocked,
    NeedsDecision,
    Interrupted,
    Failed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChildEvaluation {
    #[serde(default)]
    pub criteria: Vec<ChildCriterionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildCriterionStatus {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopSummary {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChangeSet {
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub scope_violations: Vec<ScopeViolation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeViolation {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskNarrative {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_request: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRole {
    ChildCompletionCheck,
    AuthoritativeAcceptance,
    OverallCheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentFailure {
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEvidence {
    pub role: CommandRole,
    pub criterion_id: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub stdout_summary: String,
    #[serde(default)]
    pub stderr_summary: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_failure: Option<EnvironmentFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvidence {
    Command(CommandEvidence),
    ChildCompletion(ChildEvaluation),
    WriteAudit(ChangeSet),
    HarnessStop(StopSummary),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptanceResult {
    Pass {
        acceptance: CommandEvidence,
    },
    CodeRed {
        acceptance: CommandEvidence,
    },
    InfraRed {
        signature: String,
        acceptance: Option<CommandEvidence>,
    },
    NotRun {
        reason: String,
    },
    PolicyFailure {
        reason: String,
        changed_files: Vec<String>,
        acceptance: Option<CommandEvidence>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryResult {
    CodeRed,
    NotRun,
    InfraRed,
}

/// 行为道绿、但结构检查（artifact 道）红/没跑成时记一条 advisory。
/// 瞬时态：随 `PassedByAcceptance.advisory` 走，驱动 `plan.task.advisory` 事件，不进持久 RunState。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryNote {
    pub lane: String,
    pub result: AdvisoryResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CommandEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskDecision {
    PassedByAcceptance {
        acceptance: CommandEvidence,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        advisory: Option<AdvisoryNote>,
    },
    FailedByAcceptance {
        acceptance: CommandEvidence,
        evidence_refs: Vec<String>,
    },
    UnvalidatedInfraError {
        signature: String,
        acceptance: Option<CommandEvidence>,
    },
    StoppedUnvalidated {
        reason: String,
    },
    FailedByPolicy {
        violations: Vec<ScopeViolation>,
        acceptance: Option<CommandEvidence>,
    },
}

pub fn decide_task(report: &TaskReport, acceptance: AcceptanceResult) -> TaskDecision {
    merge_task_acceptance(report, None, acceptance)
}

/// 全合并裁决（spec §2·v4）：严格优先级覆盖所有格子。
/// `report` 仅为读 `scope_violations`（child 写出界·保最高优先级）。artifact=None 表无结构道。
pub fn merge_task_acceptance(
    report: &TaskReport,
    artifact: Option<AcceptanceResult>,
    behavior: AcceptanceResult,
) -> TaskDecision {
    if !report.changes.scope_violations.is_empty() {
        return TaskDecision::FailedByPolicy {
            violations: report.changes.scope_violations.clone(),
            acceptance: behavior.acceptance_evidence().cloned().or_else(|| {
                artifact
                    .as_ref()
                    .and_then(|a| a.acceptance_evidence().cloned())
            }),
        };
    }

    if let AcceptanceResult::PolicyFailure {
        reason,
        changed_files,
        acceptance,
    } = &behavior
    {
        return policy_failure(reason, changed_files, acceptance.clone());
    }
    if let Some(AcceptanceResult::PolicyFailure {
        reason,
        changed_files,
        acceptance,
    }) = &artifact
    {
        let ev = behavior
            .acceptance_evidence()
            .cloned()
            .or_else(|| acceptance.clone());
        return policy_failure(reason, changed_files, ev);
    }

    let behavior_pass = match behavior {
        AcceptanceResult::CodeRed { acceptance } => {
            return TaskDecision::FailedByAcceptance {
                evidence_refs: vec![acceptance.criterion_id.clone()],
                acceptance,
            };
        }
        AcceptanceResult::InfraRed {
            signature,
            acceptance,
        } => {
            return TaskDecision::UnvalidatedInfraError {
                signature,
                acceptance,
            }
        }
        AcceptanceResult::NotRun { reason } => {
            return TaskDecision::StoppedUnvalidated { reason };
        }
        AcceptanceResult::Pass { acceptance } => acceptance,
        AcceptanceResult::PolicyFailure { .. } => unreachable!("handled above"),
    };

    match artifact {
        None | Some(AcceptanceResult::Pass { .. }) => TaskDecision::PassedByAcceptance {
            acceptance: behavior_pass,
            advisory: None,
        },
        Some(AcceptanceResult::CodeRed { acceptance }) => TaskDecision::PassedByAcceptance {
            acceptance: behavior_pass,
            advisory: Some(AdvisoryNote {
                lane: "artifact".into(),
                result: AdvisoryResult::CodeRed,
                detail: None,
                evidence: Some(acceptance),
            }),
        },
        Some(AcceptanceResult::NotRun { reason }) => TaskDecision::PassedByAcceptance {
            acceptance: behavior_pass,
            advisory: Some(AdvisoryNote {
                lane: "artifact".into(),
                result: AdvisoryResult::NotRun,
                detail: Some(reason),
                evidence: None,
            }),
        },
        Some(AcceptanceResult::InfraRed {
            signature,
            acceptance,
        }) => TaskDecision::PassedByAcceptance {
            acceptance: behavior_pass,
            advisory: Some(AdvisoryNote {
                lane: "artifact".into(),
                result: AdvisoryResult::InfraRed,
                detail: Some(signature),
                evidence: acceptance,
            }),
        },
        Some(AcceptanceResult::PolicyFailure { .. }) => unreachable!("handled above"),
    }
}

fn policy_failure(
    reason: &str,
    changed_files: &[String],
    acceptance: Option<CommandEvidence>,
) -> TaskDecision {
    TaskDecision::FailedByPolicy {
        violations: changed_files
            .iter()
            .map(|path| ScopeViolation {
                path: path.clone(),
                reason: reason.to_string(),
            })
            .collect(),
        acceptance,
    }
}

impl AcceptanceResult {
    pub fn acceptance_evidence(&self) -> Option<&CommandEvidence> {
        match self {
            AcceptanceResult::Pass { acceptance } | AcceptanceResult::CodeRed { acceptance } => {
                Some(acceptance)
            }
            AcceptanceResult::InfraRed { acceptance, .. }
            | AcceptanceResult::PolicyFailure { acceptance, .. } => acceptance.as_ref(),
            AcceptanceResult::NotRun { .. } => None,
        }
    }
}

impl TaskDecision {
    pub fn task_status(&self) -> Option<TaskStatus> {
        match self {
            TaskDecision::PassedByAcceptance { .. } => Some(TaskStatus::Done),
            TaskDecision::FailedByAcceptance { acceptance, .. } => Some(TaskStatus::Blocked {
                reason: format!("failed_by_acceptance: {}", acceptance.criterion_id),
            }),
            TaskDecision::FailedByPolicy { violations, .. } => Some(TaskStatus::Blocked {
                reason: format!(
                    "failed_by_policy: {}",
                    violations
                        .iter()
                        .map(|v| v.path.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
            TaskDecision::UnvalidatedInfraError { .. }
            | TaskDecision::StoppedUnvalidated { .. } => None,
        }
    }
}
#[cfg(test)]
mod tests;
