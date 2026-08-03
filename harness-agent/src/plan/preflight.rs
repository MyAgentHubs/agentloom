//! 开工前验收闸纯逻辑（spec §4·新建）：跑一遍任务验收 → 六态分类 → 裁决。
//! agent 自循环（run_solo/run_loop）一行不碰；只在编排层 run_plan_loop 之前用。

use crate::plan::contract::AcceptanceResult;

/// 开工前验收闸六态（spec §4.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightOutcome {
    /// 验收靠谱 + 确实有活（代码该改没改）→ 放行进 agent。
    ProceedCodeRed,
    /// 开工前就绿（验收太松/任务重复·确定事实）→ supersede + 追加更强替代。
    PreGreenAcceptance,
    /// 环境抽风（网/超时/锁/路径错）→ 挂起等环境。
    InfraRed { signature: String },
    /// 验收命令自己改了工作区（不只读）→ 挂起（脏了不在脏状态上推进·BLOCK-6）。
    AcceptanceReadOnlyViolation,
    /// 验收根本没跑成（非 approved / escape_scan blocked·无写入）→ 退回换合法验收。
    InvalidAcceptance,
    /// flaky（Pass 后复测翻）→ 不推断太松·挂起等稳定（不误毙）。
    UnstableAcceptance,
}

/// 纯分类：第一次验收结果 +（仅当第一次 Pass 时）一次复测结果 → 六态。
/// 复测只为确认 pre-green 不是 flaky：Pass+Pass=PreGreen；Pass+非Pass（含没复测）=Unstable（不误毙）。
pub fn classify_preflight(
    first: &AcceptanceResult,
    reconfirm: Option<&AcceptanceResult>,
) -> PreflightOutcome {
    match first {
        AcceptanceResult::CodeRed { .. } => PreflightOutcome::ProceedCodeRed,
        AcceptanceResult::InfraRed { signature, .. } => PreflightOutcome::InfraRed {
            signature: signature.clone(),
        },
        AcceptanceResult::PolicyFailure { .. } => PreflightOutcome::AcceptanceReadOnlyViolation,
        AcceptanceResult::NotRun { .. } => PreflightOutcome::InvalidAcceptance,
        AcceptanceResult::Pass { .. } => match reconfirm {
            Some(AcceptanceResult::Pass { .. }) => PreflightOutcome::PreGreenAcceptance,
            _ => PreflightOutcome::UnstableAcceptance,
        },
    }
}

/// refine 的措辞（成功追加后原任务一律 Superseded·reason 区分·BLOCK-5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefineKind {
    /// pre-green：要 Planner 产更强（fail-to-pass）的替代验收/任务。
    Strengthen,
    /// 验收非法（没跑成·无写入）：要 Planner 换合法（approved/可跑）验收。
    Legal,
}

/// 开工前闸裁决（纯·spec §4.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightStep {
    /// 放行进 agent 自循环。
    Proceed,
    /// 退回 Planner 追加替代任务（refine 路·只 pre-green / invalid 走·不弄脏工作区）。
    Refine { kind: RefineKind, reason: String },
    /// 挂起 exit4（infra / flaky / read-only 违规·修好后 resume·不当失败再规划·不在脏状态上推进）。
    Suspend { reason: String },
    /// refine 退回次数用尽 → 干净 exit4 带证据。
    Escalate { reason: String },
}

/// 开工前闸退回次数上限（spec §4.5「默认 ~2」·不蹭第二刀 replan_rounds）。
pub const DEFAULT_MAX_PREFLIGHT_REFINE: usize = 2;

/// 纯裁决：六态 + 已退回次数 + 上限 → 步骤。attempts_so_far = 该 refine 根任务已退回几次。
pub fn decide_preflight(
    outcome: &PreflightOutcome,
    attempts_so_far: usize,
    max_attempts: usize,
) -> PreflightStep {
    match outcome {
        PreflightOutcome::ProceedCodeRed => PreflightStep::Proceed,
        PreflightOutcome::InfraRed { signature } => PreflightStep::Suspend {
            reason: format!("infra_red: {signature}"),
        },
        PreflightOutcome::UnstableAcceptance => PreflightStep::Suspend {
            reason: "unstable_acceptance: pre-flight pass not reproducible".to_string(),
        },
        // read-only 违规：验收命令弄脏了工作区·本刀无 rollback 机器·不在脏状态上 refine/推进·挂起（BLOCK-6）。
        PreflightOutcome::AcceptanceReadOnlyViolation => PreflightStep::Suspend {
            reason: "acceptance_read_only_violation: command mutates workspace; make it a read-only check".to_string(),
        },
        PreflightOutcome::PreGreenAcceptance => refine_or_escalate(
            RefineKind::Strengthen,
            "acceptance_passed_before_execution",
            attempts_so_far,
            max_attempts,
        ),
        PreflightOutcome::InvalidAcceptance => refine_or_escalate(
            RefineKind::Legal,
            "invalid_acceptance",
            attempts_so_far,
            max_attempts,
        ),
    }
}

fn refine_or_escalate(
    kind: RefineKind,
    reason: &str,
    attempts_so_far: usize,
    max_attempts: usize,
) -> PreflightStep {
    if attempts_so_far < max_attempts.max(1) {
        PreflightStep::Refine {
            kind,
            reason: reason.to_string(),
        }
    } else {
        PreflightStep::Escalate {
            reason: format!("preflight_refine_exhausted: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::contract::{CommandEvidence, CommandRole};

    fn ev(success: bool) -> CommandEvidence {
        CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo test".into(),
            exit_code: if success { Some(0) } else { Some(1) },
            success,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: String::new(),
            truncated: false,
            environment_failure: None,
        }
    }

    fn pass() -> AcceptanceResult {
        AcceptanceResult::Pass {
            acceptance: ev(true),
        }
    }

    fn code_red() -> AcceptanceResult {
        AcceptanceResult::CodeRed {
            acceptance: ev(false),
        }
    }

    fn infra() -> AcceptanceResult {
        AcceptanceResult::InfraRed {
            signature: "connection refused".into(),
            acceptance: Some(ev(false)),
        }
    }

    fn not_run() -> AcceptanceResult {
        AcceptanceResult::NotRun {
            reason: "not approved".into(),
        }
    }

    fn policy() -> AcceptanceResult {
        AcceptanceResult::PolicyFailure {
            reason: "wrote files".into(),
            changed_files: vec!["a.rs".into()],
            acceptance: Some(ev(true)),
        }
    }

    #[test]
    fn code_red_is_proceed() {
        assert_eq!(
            classify_preflight(&code_red(), None),
            PreflightOutcome::ProceedCodeRed
        );
    }

    #[test]
    fn pass_then_pass_is_pre_green() {
        assert_eq!(
            classify_preflight(&pass(), Some(&pass())),
            PreflightOutcome::PreGreenAcceptance
        );
    }

    #[test]
    fn pass_then_red_is_unstable_not_pre_green() {
        assert_eq!(
            classify_preflight(&pass(), Some(&code_red())),
            PreflightOutcome::UnstableAcceptance
        );
    }

    #[test]
    fn pass_without_reconfirm_is_unstable_defensive() {
        assert_eq!(
            classify_preflight(&pass(), None),
            PreflightOutcome::UnstableAcceptance
        );
    }

    #[test]
    fn infra_maps_to_infra_red() {
        match classify_preflight(&infra(), None) {
            PreflightOutcome::InfraRed { signature } => {
                assert!(signature.contains("connection refused"));
            }
            other => panic!("expected infra red, got {other:?}"),
        }
    }

    #[test]
    fn not_run_is_invalid_acceptance() {
        assert_eq!(
            classify_preflight(&not_run(), None),
            PreflightOutcome::InvalidAcceptance
        );
    }

    #[test]
    fn policy_failure_is_read_only_violation() {
        assert_eq!(
            classify_preflight(&policy(), None),
            PreflightOutcome::AcceptanceReadOnlyViolation
        );
    }

    #[test]
    fn proceed_code_red_proceeds() {
        assert_eq!(
            decide_preflight(&PreflightOutcome::ProceedCodeRed, 0, 2),
            PreflightStep::Proceed
        );
    }

    #[test]
    fn pre_green_under_budget_refines_strengthen() {
        match decide_preflight(&PreflightOutcome::PreGreenAcceptance, 0, 2) {
            PreflightStep::Refine { kind, reason } => {
                assert_eq!(kind, RefineKind::Strengthen);
                assert!(reason.contains("passed_before_execution"));
            }
            other => panic!("expected refine, got {other:?}"),
        }
    }

    #[test]
    fn pre_green_at_budget_escalates() {
        match decide_preflight(&PreflightOutcome::PreGreenAcceptance, 2, 2) {
            PreflightStep::Escalate { reason } => {
                assert!(reason.contains("preflight_refine_exhausted"));
            }
            other => panic!("expected escalate, got {other:?}"),
        }
    }

    #[test]
    fn invalid_acceptance_refines_legal() {
        match decide_preflight(&PreflightOutcome::InvalidAcceptance, 0, 2) {
            PreflightStep::Refine { kind, .. } => assert_eq!(kind, RefineKind::Legal),
            other => panic!("expected refine, got {other:?}"),
        }
    }

    #[test]
    fn read_only_violation_suspends_not_refines() {
        match decide_preflight(&PreflightOutcome::AcceptanceReadOnlyViolation, 0, 2) {
            PreflightStep::Suspend { reason } => assert!(reason.contains("read_only")),
            other => panic!("expected suspend, got {other:?}"),
        }
    }

    #[test]
    fn infra_red_suspends() {
        match decide_preflight(
            &PreflightOutcome::InfraRed {
                signature: "connection refused".into(),
            },
            0,
            2,
        ) {
            PreflightStep::Suspend { reason } => assert!(reason.contains("connection refused")),
            other => panic!("expected suspend, got {other:?}"),
        }
    }

    #[test]
    fn unstable_suspends_does_not_supersede() {
        match decide_preflight(&PreflightOutcome::UnstableAcceptance, 0, 2) {
            PreflightStep::Suspend { reason } => assert!(reason.contains("unstable")),
            other => panic!("expected suspend, got {other:?}"),
        }
    }
}
