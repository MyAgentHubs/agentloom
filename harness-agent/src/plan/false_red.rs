//! 假红防护（spec §3.3·新建）：跑 check 的结果别被环境抽风（缓存/网络/代理/锁）骗。
//! 凡「跑个 check 信结果」的地方（任务级 acceptance / 总验收 / 整盘重验 / per-language 不变量 / 崩溃重跑）共用。

use std::path::Path;

use crate::error::Result;
use crate::exec::controlled::{controlled_exec, ControlledExecOpts, ControlledExecOutcome};
use crate::goal::{Criterion, NetworkPolicy, SuccessRule, Verifier};
use crate::plan::contract::{AcceptanceResult, CommandEvidence, CommandRole, EnvironmentFailure};

/// 一次 check 的判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckVerdict {
    /// 绿。
    Pass,
    /// 代码红（真失败·该 exit4 / blocked）。
    CodeRed { detail: String },
    /// 基础设施红（环境抽风·不当代码错·不喂再规划·挂起等环境）。
    InfraRed { signature: String },
}

/// best-effort 识别「环境抽风」签名（spec §3.3·B5 收窄）：只认**具体短语**，
/// 不用裸 "timeout"/"proxy"/"another process"（会把代码红如 "cannot find function `timeout`" 误判 infra）。
/// 真超时由 T2 的结构化 `timed_out` 字段单独处理。完整失败分类法是 P2。
pub fn infra_signature(stderr: &str, stdout: &str) -> Option<String> {
    let hay = format!("{stderr}\n{stdout}").to_ascii_lowercase();
    const SIGNS: &[&str] = &[
        "connection refused",
        "connection reset by peer",
        "could not connect to",
        "could not resolve host",
        "could not resolve proxy",
        "temporary failure in name resolution",
        "network is unreachable",
        "no route to host",
        "operation timed out",
        "connection timed out",
        "blocking waiting for file lock",
        "resource temporarily unavailable",
    ];
    SIGNS
        .iter()
        .find(|s| hay.contains(**s))
        .map(|s| (*s).to_string())
}

/// 跑一条 check·红了在干净进程重试一次·仍红则分类（spec §3.3）。eff_network = 已算好的有效网络策略。
pub async fn run_check_guarded(
    check_cmd: &str,
    success: &SuccessRule,
    timeout_s: u64,
    workspace: &Path,
    eff_network: NetworkPolicy,
) -> Result<CheckVerdict> {
    let first = run_once(check_cmd, success, timeout_s, workspace, eff_network).await?;
    if first == CheckVerdict::Pass {
        return Ok(CheckVerdict::Pass);
    }
    // 红了·干净进程重试一次（瞬时抖动重试即过）·以第二次结果为准
    run_once(check_cmd, success, timeout_s, workspace, eff_network).await
}

async fn run_once(
    check_cmd: &str,
    success: &SuccessRule,
    timeout_s: u64,
    workspace: &Path,
    eff_network: NetworkPolicy,
) -> Result<CheckVerdict> {
    let outcome = controlled_exec(ControlledExecOpts {
        command: check_cmd.to_string(),
        workspace: workspace.to_path_buf(),
        cwd: workspace.to_path_buf(),
        timeout_ms: timeout_s.saturating_mul(1000).max(1000),
        output_cap_bytes: 64 * 1024,
        network: eff_network,
        fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
    })
    .await?;
    Ok(match outcome {
        ControlledExecOutcome::Ran {
            stdout,
            stderr,
            exit_code,
            timed_out,
            ..
        } => {
            if timed_out {
                CheckVerdict::InfraRed {
                    signature: "operation timed out".to_string(),
                }
            } else {
                let passed = match success {
                    SuccessRule::ExitZero => exit_code == Some(0),
                    SuccessRule::StdoutContains(s) => stdout.contains(s.as_str()),
                };
                if passed {
                    CheckVerdict::Pass
                } else if let Some(sig) = infra_signature(&stderr, &stdout) {
                    CheckVerdict::InfraRed { signature: sig }
                } else {
                    CheckVerdict::CodeRed {
                        detail: format!("exit={exit_code:?}"),
                    }
                }
            }
        }
        ControlledExecOutcome::NetworkUnenforceable { reason } => CheckVerdict::InfraRed {
            signature: format!("network unenforceable: {reason}"),
        },
        ControlledExecOutcome::Blocked { rule } => CheckVerdict::CodeRed {
            detail: format!("blocked: escape attempt ({rule})"),
        },
    })
}

/// 一条 Criterion 的守护判定。F3：先查已批准可执行 Verifiable（不绕过审批·镜像 evaluator 跳过逻辑）。
pub async fn criterion_command_result(
    criterion: &Criterion,
    role: CommandRole,
    workspace: &Path,
    network: NetworkPolicy,
) -> Result<AcceptanceResult> {
    criterion_command_result_with_fence(
        criterion,
        role,
        workspace,
        network,
        crate::exec::sandbox::FsWriteFence::Off,
    )
    .await
}

pub async fn criterion_command_result_with_fence(
    criterion: &Criterion,
    role: CommandRole,
    workspace: &Path,
    network: NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> Result<AcceptanceResult> {
    if !criterion.is_executable_verifiable() {
        return Ok(AcceptanceResult::NotRun {
            reason: format!(
                "criterion {} 非已批准可跑 Verifiable·无法自动验收",
                criterion.id
            ),
        });
    }
    let Verifier::Verifiable {
        check_cmd,
        success,
        timeout_s,
        network: v_net,
    } = &criterion.verifier
    else {
        // is_executable_verifiable() 已保证是 Verifiable·此分支不可达·防御
        return Ok(AcceptanceResult::NotRun {
            reason: format!("criterion {} 非 Verifiable", criterion.id),
        });
    };
    let eff = crate::evaluator::effective_network(network, *v_net);
    run_check_guarded_with_evidence(
        criterion,
        role,
        check_cmd,
        success,
        *timeout_s,
        workspace,
        eff,
        fs_write_fence,
    )
    .await
}

async fn run_check_guarded_with_evidence(
    criterion: &Criterion,
    role: CommandRole,
    check_cmd: &str,
    success: &SuccessRule,
    timeout_s: u64,
    workspace: &Path,
    eff_network: NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> Result<AcceptanceResult> {
    let first = run_once_with_evidence(
        criterion,
        role,
        check_cmd,
        success,
        timeout_s,
        workspace,
        eff_network,
        fs_write_fence,
    )
    .await?;
    if matches!(first, AcceptanceResult::Pass { .. }) {
        return Ok(first);
    }
    // 红了·干净进程重试一次·以第二次 evidence 为准。
    run_once_with_evidence(
        criterion,
        role,
        check_cmd,
        success,
        timeout_s,
        workspace,
        eff_network,
        fs_write_fence,
    )
    .await
}

async fn run_once_with_evidence(
    criterion: &Criterion,
    role: CommandRole,
    check_cmd: &str,
    success: &SuccessRule,
    timeout_s: u64,
    workspace: &Path,
    eff_network: NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> Result<AcceptanceResult> {
    let outcome = controlled_exec(ControlledExecOpts {
        command: check_cmd.to_string(),
        workspace: workspace.to_path_buf(),
        cwd: workspace.to_path_buf(),
        timeout_ms: timeout_s.saturating_mul(1000).max(1000),
        output_cap_bytes: 64 * 1024,
        network: eff_network,
        fs_write_fence,
    })
    .await?;
    Ok(match outcome {
        ControlledExecOutcome::Ran {
            stdout,
            stderr,
            exit_code,
            timed_out,
            truncated,
        } => {
            let passed = !timed_out
                && match success {
                    SuccessRule::ExitZero => exit_code == Some(0),
                    SuccessRule::StdoutContains(s) => stdout.contains(s.as_str()),
                };
            let env = if timed_out {
                Some(EnvironmentFailure {
                    signature: "operation timed out".to_string(),
                })
            } else {
                infra_signature(&stderr, &stdout).map(|signature| EnvironmentFailure { signature })
            };
            let ev = CommandEvidence {
                role,
                criterion_id: criterion.id.clone(),
                command: check_cmd.to_string(),
                exit_code,
                success: passed,
                timed_out,
                stdout_summary: stdout,
                stderr_summary: stderr,
                truncated,
                environment_failure: env.clone(),
            };
            if passed {
                AcceptanceResult::Pass { acceptance: ev }
            } else if let Some(env) = env {
                AcceptanceResult::InfraRed {
                    signature: env.signature,
                    acceptance: Some(ev),
                }
            } else {
                AcceptanceResult::CodeRed { acceptance: ev }
            }
        }
        ControlledExecOutcome::NetworkUnenforceable { reason } => AcceptanceResult::InfraRed {
            signature: format!("network unenforceable: {reason}"),
            acceptance: None,
        },
        ControlledExecOutcome::Blocked { rule } => AcceptanceResult::NotRun {
            reason: format!("acceptance command blocked by escape_scan: {rule}"),
        },
    })
}

/// 一条 Criterion 的守护判定。F3：先查已批准可执行 Verifiable（不绕过审批·镜像 evaluator 跳过逻辑）。
pub async fn criterion_verdict(
    criterion: &Criterion,
    workspace: &Path,
    network: NetworkPolicy,
) -> Result<CheckVerdict> {
    Ok(
        match criterion_command_result(
            criterion,
            CommandRole::AuthoritativeAcceptance,
            workspace,
            network,
        )
        .await?
        {
            AcceptanceResult::Pass { .. } => CheckVerdict::Pass,
            AcceptanceResult::CodeRed { acceptance } => CheckVerdict::CodeRed {
                detail: format!("exit={:?}", acceptance.exit_code),
            },
            AcceptanceResult::InfraRed { signature, .. } => CheckVerdict::InfraRed { signature },
            AcceptanceResult::NotRun { reason } => CheckVerdict::CodeRed { detail: reason },
            AcceptanceResult::PolicyFailure { reason, .. } => {
                CheckVerdict::CodeRed { detail: reason }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::{NetworkPolicy, SuccessRule};

    #[test]
    fn recognizes_specific_infra_phrases() {
        assert!(infra_signature("error: connection refused", "").is_some());
        assert!(infra_signature("", "could not resolve host: api.example.com").is_some());
        assert!(infra_signature("Blocking waiting for file lock on package cache", "").is_some());
        assert!(infra_signature("error: operation timed out", "").is_some());
        assert!(infra_signature("network is unreachable", "").is_some());
        assert!(infra_signature("Connection Refused", "").is_some()); // 大小写不敏感
    }

    #[test]
    fn does_not_misclassify_code_failures() {
        // 裸词不再命中（B5）：这些是代码红·不能当 infra
        assert!(infra_signature("error[E0425]: cannot find function `timeout`", "").is_none());
        assert!(infra_signature("proxy module: assertion failed", "").is_none());
        assert!(infra_signature("another process spawned successfully", "").is_none());
        assert!(infra_signature("assertion `left == right` failed", "").is_none());
        assert!(infra_signature("error[E0308]: mismatched types", "").is_none());
        assert!(infra_signature("", "").is_none());
    }

    #[tokio::test]
    async fn passing_check_is_pass() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run_check_guarded(
                "true",
                &SuccessRule::ExitZero,
                30,
                dir.path(),
                NetworkPolicy::On
            )
            .await
            .unwrap(),
            CheckVerdict::Pass
        );
    }

    #[tokio::test]
    async fn plain_failure_is_code_red() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            run_check_guarded(
                "false",
                &SuccessRule::ExitZero,
                30,
                dir.path(),
                NetworkPolicy::On
            )
            .await
            .unwrap(),
            CheckVerdict::CodeRed { .. }
        ));
    }

    #[tokio::test]
    async fn failure_with_infra_signature_is_infra_red() {
        let dir = tempfile::tempdir().unwrap();
        let v = run_check_guarded(
            "echo connection refused; exit 1",
            &SuccessRule::ExitZero,
            30,
            dir.path(),
            NetworkPolicy::On,
        )
        .await
        .unwrap();
        assert!(matches!(v, CheckVerdict::InfraRed { .. }));
    }

    #[tokio::test]
    async fn retry_once_lets_transient_red_pass() {
        let dir = tempfile::tempdir().unwrap();
        // 第一次红、第二次绿（marker 文件）→ 守护跑重试一次 → Pass
        let v = run_check_guarded(
            "if [ -f m ]; then exit 0; else touch m; exit 1; fi",
            &SuccessRule::ExitZero,
            30,
            dir.path(),
            NetworkPolicy::On,
        )
        .await
        .unwrap();
        assert_eq!(v, CheckVerdict::Pass);
    }

    #[tokio::test]
    async fn criterion_verdict_runs_approved_verifiable() {
        let dir = tempfile::tempdir().unwrap();
        let c = approved("true");
        assert_eq!(
            criterion_verdict(&c, dir.path(), NetworkPolicy::On)
                .await
                .unwrap(),
            CheckVerdict::Pass
        );
    }

    #[tokio::test]
    async fn criterion_verdict_rejects_unapproved_or_judgmental() {
        let dir = tempfile::tempdir().unwrap();
        // 未批准的 agent criterion → CodeRed（不绕过审批·F3）
        let mut pending = approved("true");
        pending.authored_by = crate::goal::AuthoredBy::Agent;
        pending.approval = crate::goal::Approval::Pending;
        assert!(matches!(
            criterion_verdict(&pending, dir.path(), NetworkPolicy::On)
                .await
                .unwrap(),
            CheckVerdict::CodeRed { .. }
        ));
        // Judgmental → CodeRed（无法自动重跑）
        let judg = crate::goal::Criterion {
            verifier: crate::goal::Verifier::Judgmental {
                rubric: "looks good".into(),
            },
            ..approved("true")
        };
        assert!(matches!(
            criterion_verdict(&judg, dir.path(), NetworkPolicy::On)
                .await
                .unwrap(),
            CheckVerdict::CodeRed { .. }
        ));
    }

    #[tokio::test]
    async fn criterion_command_result_pass_has_authoritative_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let c = approved("printf ok");
        let result = criterion_command_result(
            &c,
            crate::plan::contract::CommandRole::AuthoritativeAcceptance,
            dir.path(),
            NetworkPolicy::On,
        )
        .await
        .unwrap();
        match result {
            crate::plan::contract::AcceptanceResult::Pass { acceptance } => {
                assert_eq!(
                    acceptance.role,
                    crate::plan::contract::CommandRole::AuthoritativeAcceptance
                );
                assert_eq!(acceptance.criterion_id, "c1");
                assert!(acceptance.success);
                assert!(acceptance.stdout_summary.contains("ok"));
            }
            other => panic!("expected pass, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn criterion_command_result_code_red_keeps_command_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let c = approved("printf nope >&2; exit 7");
        let result = criterion_command_result(
            &c,
            crate::plan::contract::CommandRole::AuthoritativeAcceptance,
            dir.path(),
            NetworkPolicy::On,
        )
        .await
        .unwrap();
        match result {
            crate::plan::contract::AcceptanceResult::CodeRed { acceptance } => {
                assert!(!acceptance.success);
                assert_eq!(acceptance.exit_code, Some(7));
                assert!(acceptance.stderr_summary.contains("nope"));
                assert!(acceptance.environment_failure.is_none());
            }
            other => panic!("expected code red, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn criterion_command_result_infra_red_marks_environment_failure() {
        let dir = tempfile::tempdir().unwrap();
        let c = approved("echo connection refused; exit 1");
        let result = criterion_command_result(
            &c,
            crate::plan::contract::CommandRole::AuthoritativeAcceptance,
            dir.path(),
            NetworkPolicy::On,
        )
        .await
        .unwrap();
        match result {
            crate::plan::contract::AcceptanceResult::InfraRed {
                signature,
                acceptance,
            } => {
                assert!(signature.contains("connection refused"));
                let ev = acceptance.expect("infra red still carries command evidence");
                assert!(ev.environment_failure.is_some());
            }
            other => panic!("expected infra red, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn criterion_command_result_unapproved_is_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = approved("true");
        c.authored_by = crate::goal::AuthoredBy::Agent;
        c.approval = crate::goal::Approval::Pending;
        let result = criterion_command_result(
            &c,
            crate::plan::contract::CommandRole::AuthoritativeAcceptance,
            dir.path(),
            NetworkPolicy::On,
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            crate::plan::contract::AcceptanceResult::NotRun { .. }
        ));
    }

    fn approved(cmd: &str) -> crate::goal::Criterion {
        crate::goal::Criterion {
            id: "c1".into(),
            claim: "x".into(),
            scope: None,
            authored_by: crate::goal::AuthoredBy::User,
            approval: crate::goal::Approval::Approved,
            verifier: crate::goal::Verifier::Verifiable {
                check_cmd: cmd.into(),
                success: SuccessRule::ExitZero,
                timeout_s: 30,
                network: None,
            },
            status: crate::goal::CriterionStatus::Pending,
            evidence_ref: None,
        }
    }
}
