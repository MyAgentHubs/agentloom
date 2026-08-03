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
/// is_executable_verifiable() 判「Verifiable 且 approval==Approved」，故 evaluator 不跳过（B3）。
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
mod tests {
    use super::*;
    use crate::goal::Approval;

    const VALID: &str = r#"{ "tasks": [
      { "id": "t1", "intent": "add retry helper", "files_scope": ["src/provider/retry.rs"],
        "acceptance_cmd": "cargo test --manifest-path harness-agent/Cargo.toml retry", "max_turns": 12 }
    ] }"#;
    const TWO_LANE: &str = r#"{ "tasks": [
      { "id": "t1", "intent": "add mcp_servers field", "files_scope": ["src", "tests"],
        "acceptance_cmd": "cargo test --manifest-path harness-agent/Cargo.toml mcp",
        "artifact_check_cmd": "grep -rq 'mcp_servers' harness-agent/src/orchestrator.rs",
        "max_turns": 8 }
    ] }"#;

    #[test]
    fn parses_minimal_task_and_builds_approved_acceptance() {
        let tasks = parse_worklist(VALID).expect("valid worklist parses");
        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!(t.id, "t1");
        assert_eq!(t.files_scope, vec!["src/provider/retry.rs".to_string()]);
        assert_eq!(t.max_turns, 12);
        assert_eq!(t.status, TaskStatus::Pending);
        assert!(t.forbidden_scope.is_empty());
        assert!(t.depends_on.is_empty());
        assert_eq!(t.acceptance.approval, Approval::Approved);
        assert!(t.acceptance.is_executable_verifiable());
    }

    #[test]
    fn parses_two_lanes_behavior_and_artifact() {
        let tasks = parse_worklist(TWO_LANE).expect("two-lane worklist parses");
        let t = &tasks[0];
        assert_eq!(t.acceptance.id, "t1_acc");
        assert!(t.acceptance.is_executable_verifiable());
        let art = t.artifact_check.as_ref().expect("artifact lane present");
        assert_eq!(art.id, "t1_art");
        assert!(art.is_executable_verifiable());
        match &art.verifier {
            crate::goal::Verifier::Verifiable { check_cmd, .. } => {
                assert!(check_cmd.contains("mcp_servers"));
            }
            _ => panic!("artifact must be Verifiable"),
        }
    }

    #[test]
    fn legacy_single_acceptance_lands_behavior_artifact_none() {
        let tasks = parse_worklist(VALID).expect("legacy worklist parses");
        let t = &tasks[0];
        assert_eq!(t.acceptance.id, "t1_acc");
        assert!(t.artifact_check.is_none());
    }

    #[test]
    fn plantask_serde_omits_artifact_when_none() {
        let tasks = parse_worklist(VALID).unwrap();
        let json = serde_json::to_string(&tasks[0]).unwrap();
        assert!(
            json.contains("\"acceptance\""),
            "behavior lane key stays 'acceptance'"
        );
        assert!(
            !json.contains("artifact_check"),
            "None artifact lane not serialized"
        );
    }

    #[test]
    fn plantask_deserializes_old_state_without_artifact_key() {
        let tasks = parse_worklist(VALID).unwrap();
        let mut v = serde_json::to_value(&tasks[0]).unwrap();
        assert!(v.get("artifact_check").is_none());
        let back: PlanTask = serde_json::from_value(v.take()).unwrap();
        assert!(back.artifact_check.is_none());
        assert_eq!(back.acceptance.id, "t1_acc");
    }

    #[test]
    fn parse_worklist_sets_remediation_none_for_normal_tasks() {
        let tasks = parse_worklist(VALID).expect("valid worklist parses");
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].remediation.is_none());
    }

    #[test]
    fn parse_worklist_defaults_acceptance_kind_to_change_required() {
        let tasks = parse_worklist(VALID).expect("valid worklist parses");
        assert_eq!(tasks[0].acceptance_kind, AcceptanceKind::ChangeRequired);
    }

    #[test]
    fn parse_worklist_reads_explicit_invariant_acceptance_kind() {
        let json = r#"{ "tasks": [
          { "id": "t1", "intent": "keep build green", "files_scope": ["src/lib.rs"],
            "acceptance_cmd": "cargo build", "max_turns": 4, "acceptance_kind": "invariant" }
        ] }"#;
        let tasks = parse_worklist(json).expect("valid worklist parses");
        assert_eq!(tasks[0].acceptance_kind, AcceptanceKind::Invariant);
    }

    #[test]
    fn acceptance_kind_round_trips_on_plan_task() {
        let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
        task.acceptance_kind = AcceptanceKind::Invariant;
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"acceptance_kind\""));
        let back: PlanTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.acceptance_kind, AcceptanceKind::Invariant);
    }

    #[test]
    fn legacy_plan_task_without_acceptance_kind_defaults_to_change_required() {
        let task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
        let mut value = serde_json::to_value(task).unwrap();
        value.as_object_mut().unwrap().remove("acceptance_kind");
        let back: PlanTask = serde_json::from_value(value).unwrap();
        assert_eq!(back.acceptance_kind, AcceptanceKind::ChangeRequired);
    }

    #[test]
    fn superseded_status_round_trips() {
        let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
        task.status = TaskStatus::Superseded {
            by: vec!["t1_r1_fix1".into()],
            reason: "acceptance_passed_before_execution".into(),
        };
        let json = serde_json::to_string(&task).unwrap();
        let back: PlanTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, task.status);
    }

    #[test]
    fn rejected_acceptance_status_round_trips() {
        let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
        task.status = TaskStatus::RejectedAcceptance {
            reason: "preflight_refine_exhausted".into(),
        };
        let json = serde_json::to_string(&task).unwrap();
        let back: PlanTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, task.status);
    }

    #[test]
    fn advisory_note_serde_round_trips() {
        let note = AdvisoryNote {
            lane: "artifact".into(),
            result: AdvisoryResult::CodeRed,
            detail: Some("grep 未匹配".into()),
            evidence: None,
        };
        let json = serde_json::to_string(&note).unwrap();
        let back: AdvisoryNote = serde_json::from_str(&json).unwrap();
        assert_eq!(note, back);
        assert!(
            json.contains("\"result\":\"code_red\""),
            "result 应是 snake_case enum"
        );
    }

    #[test]
    fn passed_by_acceptance_without_advisory_omits_key_zero_drift() {
        let ev = CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo test".into(),
            exit_code: Some(0),
            success: true,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: String::new(),
            truncated: false,
            environment_failure: None,
        };
        let d = TaskDecision::PassedByAcceptance {
            acceptance: ev,
            advisory: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(
            v.get("kind").and_then(|k| k.as_str()),
            Some("passed_by_acceptance")
        );
        assert!(
            v.get("advisory").is_none(),
            "advisory=None 必须不写出·保 golden 零漂移"
        );
    }

    #[test]
    fn remediation_meta_round_trips_on_plan_task() {
        let mut task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
        task.remediation = Some(RemediationMeta {
            parent: "t1".into(),
            evidence_fingerprint: "cargo test:E0063:src/lib.rs:37".into(),
            attempt_no: 1,
            round: 2,
        });

        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"remediation\""));
        let back: PlanTask = serde_json::from_str(&json).unwrap();

        assert_eq!(back.remediation, task.remediation);
    }

    #[test]
    fn legacy_plan_task_without_remediation_defaults_to_none() {
        let task = parse_worklist(VALID).unwrap().into_iter().next().unwrap();
        let mut value = serde_json::to_value(task).unwrap();
        value.as_object_mut().unwrap().remove("remediation");

        let back: PlanTask = serde_json::from_value(value).unwrap();
        assert!(back.remediation.is_none());
    }

    #[test]
    fn missing_required_field_errors() {
        let bad = r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["a"] } ] }"#;
        assert!(parse_worklist(bad).is_err());
    }

    #[test]
    fn malformed_json_errors() {
        assert!(parse_worklist("not json").is_err());
    }

    fn command_ev(success: bool) -> CommandEvidence {
        CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "true".into(),
            exit_code: if success { Some(0) } else { Some(1) },
            success,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: String::new(),
            truncated: false,
            environment_failure: None,
        }
    }

    fn pass_ev(id: &str) -> CommandEvidence {
        let mut e = command_ev(true);
        e.criterion_id = id.into();
        e
    }

    fn red_ev(id: &str) -> CommandEvidence {
        let mut e = command_ev(false);
        e.criterion_id = id.into();
        e
    }

    fn report_with_changes(changes: ChangeSet) -> TaskReport {
        TaskReport {
            schema_version: TASK_REPORT_SCHEMA_VERSION,
            task_id: "t1".into(),
            child_run_id: "plan__t1".into(),
            status: TaskReportStatus::BlockedCandidate,
            acceptance: harness_approved_criterion("t1", "true"),
            child_outcome: ChildRunOutcome::Blocked,
            child_evaluation: None,
            stop: None,
            changes,
            evidence: vec![],
            narrative: TaskNarrative::default(),
        }
    }

    #[test]
    fn decide_ignores_report_status_when_acceptance_passes() {
        let report = report_with_changes(ChangeSet::default());
        let d = decide_task(
            &report,
            AcceptanceResult::Pass {
                acceptance: command_ev(true),
            },
        );
        assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
        assert_eq!(d.task_status(), Some(TaskStatus::Done));
    }

    #[test]
    fn behavior_pass_artifact_code_red_is_done_with_advisory() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::CodeRed {
                acceptance: red_ev("t1_art"),
            }),
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        match d {
            TaskDecision::PassedByAcceptance {
                advisory: Some(n), ..
            } => {
                assert_eq!(n.result, AdvisoryResult::CodeRed);
                assert_eq!(n.lane, "artifact");
                assert!(n.evidence.is_some());
            }
            other => panic!("expected Done+advisory, got {other:?}"),
        }
    }

    #[test]
    fn behavior_pass_artifact_none_is_done() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            None,
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
        assert_eq!(d.task_status(), Some(TaskStatus::Done));
    }

    #[test]
    fn behavior_pass_artifact_pass_is_done() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::Pass {
                acceptance: pass_ev("t1_art"),
            }),
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
    }

    #[test]
    fn behavior_code_red_wins_over_any_artifact() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::InfraRed {
                signature: "net".into(),
                acceptance: None,
            }),
            AcceptanceResult::CodeRed {
                acceptance: red_ev("t1_acc"),
            },
        );
        assert!(matches!(d, TaskDecision::FailedByAcceptance { .. }));
    }

    #[test]
    fn behavior_pass_artifact_not_run_is_done_with_advisory_not_run() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::NotRun {
                reason: "未批".into(),
            }),
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        match d {
            TaskDecision::PassedByAcceptance {
                advisory: Some(n), ..
            } => {
                assert_eq!(n.result, AdvisoryResult::NotRun);
                assert_eq!(n.detail.as_deref(), Some("未批"));
                assert!(n.evidence.is_none());
            }
            other => panic!("expected Done+advisory(not_run), got {other:?}"),
        }
    }

    #[test]
    fn behavior_pass_artifact_infra_red_is_done_with_advisory_infra() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::InfraRed {
                signature: "operation timed out".into(),
                acceptance: Some(red_ev("t1_art")),
            }),
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        match d {
            TaskDecision::PassedByAcceptance {
                advisory: Some(n), ..
            } => {
                assert_eq!(n.result, AdvisoryResult::InfraRed);
                assert_eq!(n.detail.as_deref(), Some("operation timed out"));
                assert!(n.evidence.is_some());
            }
            other => panic!("expected Done+advisory(infra_red), got {other:?}"),
        }
    }

    #[test]
    fn behavior_pass_artifact_pass_or_none_is_done_no_advisory() {
        let report = report_with_changes(ChangeSet::default());
        for artifact in [
            None,
            Some(AcceptanceResult::Pass {
                acceptance: pass_ev("t1_art"),
            }),
        ] {
            let d = merge_task_acceptance(
                &report,
                artifact,
                AcceptanceResult::Pass {
                    acceptance: pass_ev("t1_acc"),
                },
            );
            assert!(
                matches!(d, TaskDecision::PassedByAcceptance { advisory: None, .. }),
                "{d:?}"
            );
        }
    }

    #[test]
    fn behavior_code_red_still_fails_regardless_of_artifact() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::Pass {
                acceptance: pass_ev("t1_art"),
            }),
            AcceptanceResult::CodeRed {
                acceptance: red_ev("t1_acc"),
            },
        );
        assert!(
            matches!(d, TaskDecision::FailedByAcceptance { .. }),
            "{d:?}"
        );
    }

    #[test]
    fn scope_violation_is_policy_even_if_both_pass() {
        let report = report_with_changes(ChangeSet {
            changed_files: vec!["src/a.rs".into(), "src/secret.rs".into()],
            scope_violations: vec![ScopeViolation {
                path: "src/secret.rs".into(),
                reason: "超出 files_scope".into(),
            }],
        });
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::Pass {
                acceptance: pass_ev("t1_art"),
            }),
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        assert!(matches!(d, TaskDecision::FailedByPolicy { .. }));
    }

    #[test]
    fn artifact_policy_failure_is_policy() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::PolicyFailure {
                reason: "wrote".into(),
                changed_files: vec!["x".into()],
                acceptance: None,
            }),
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        assert!(matches!(d, TaskDecision::FailedByPolicy { .. }));
    }

    #[test]
    fn artifact_policy_with_behavior_code_red_keeps_behavior_evidence() {
        let report = report_with_changes(ChangeSet::default());
        let d = merge_task_acceptance(
            &report,
            Some(AcceptanceResult::PolicyFailure {
                reason: "wrote".into(),
                changed_files: vec!["x".into()],
                acceptance: None,
            }),
            AcceptanceResult::CodeRed {
                acceptance: red_ev("t1_acc"),
            },
        );
        match d {
            TaskDecision::FailedByPolicy {
                acceptance: Some(ev),
                ..
            } => assert_eq!(ev.criterion_id, "t1_acc"),
            other => panic!("expected FailedByPolicy with behavior evidence, got {other:?}"),
        }
    }

    #[test]
    fn decide_task_is_behavior_only_merge() {
        let report = report_with_changes(ChangeSet::default());
        let d = decide_task(
            &report,
            AcceptanceResult::Pass {
                acceptance: pass_ev("t1_acc"),
            },
        );
        assert!(matches!(d, TaskDecision::PassedByAcceptance { .. }));
    }

    #[test]
    fn decide_code_red_is_failed_by_acceptance() {
        let report = report_with_changes(ChangeSet::default());
        let d = decide_task(
            &report,
            AcceptanceResult::CodeRed {
                acceptance: command_ev(false),
            },
        );
        assert!(matches!(d, TaskDecision::FailedByAcceptance { .. }));
        assert!(matches!(d.task_status(), Some(TaskStatus::Blocked { .. })));
    }

    #[test]
    fn decide_infra_and_not_run_are_unvalidated_not_blocked() {
        let report = report_with_changes(ChangeSet::default());
        let infra = decide_task(
            &report,
            AcceptanceResult::InfraRed {
                signature: "connection refused".into(),
                acceptance: Some(command_ev(false)),
            },
        );
        assert!(matches!(infra, TaskDecision::UnvalidatedInfraError { .. }));
        assert_eq!(infra.task_status(), None);

        let stopped = decide_task(
            &report,
            AcceptanceResult::NotRun {
                reason: "blocked by escape_scan".into(),
            },
        );
        assert!(matches!(stopped, TaskDecision::StoppedUnvalidated { .. }));
        assert_eq!(stopped.task_status(), None);
    }

    #[test]
    fn write_audit_violation_is_failed_by_policy_even_if_acceptance_passes() {
        let report = report_with_changes(ChangeSet {
            changed_files: vec!["src/a.rs".into(), "src/secret.rs".into()],
            scope_violations: vec![ScopeViolation {
                path: "src/secret.rs".into(),
                reason: "超出 files_scope 白名单：src/secret.rs".into(),
            }],
        });
        let d = decide_task(
            &report,
            AcceptanceResult::Pass {
                acceptance: command_ev(true),
            },
        );
        assert!(matches!(d, TaskDecision::FailedByPolicy { .. }));
        match d.task_status() {
            Some(TaskStatus::Blocked { reason }) => assert!(reason.contains("failed_by_policy")),
            other => panic!("expected policy blocked status, got {other:?}"),
        }
    }

    #[test]
    fn read_only_delta_acceptance_result_maps_to_failed_by_policy() {
        let report = report_with_changes(ChangeSet::default());
        let d = decide_task(
            &report,
            AcceptanceResult::PolicyFailure {
                reason: "acceptance_read_only_violation".into(),
                changed_files: vec!["a.rs".into()],
                acceptance: Some(command_ev(true)),
            },
        );
        match d {
            TaskDecision::FailedByPolicy {
                violations,
                acceptance,
            } => {
                assert_eq!(violations[0].path, "a.rs");
                assert!(violations[0]
                    .reason
                    .contains("acceptance_read_only_violation"));
                assert!(acceptance.is_some());
            }
            other => panic!("expected FailedByPolicy, got {other:?}"),
        }
    }
}
