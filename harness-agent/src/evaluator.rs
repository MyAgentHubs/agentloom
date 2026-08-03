use std::collections::BTreeSet;
use std::path::Path;

use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::exec::controlled::{controlled_exec, ControlledExecOpts, ControlledExecOutcome};
use crate::goal::{Approval, AuthoredBy, CriterionStatus, GoalState, SuccessRule, Verifier};
use crate::guardrails::ContractPolicy;
use crate::run_progress::RippleCandidate;

/// 一次 check_cmd 的结构化证据（进 completion.evaluated.criteria[].evidence）。
pub struct CheckEvidence {
    pub authored_by: AuthoredBy,
    pub command: String,
    pub exit_code: Option<i32>,
    pub passed: bool,
    pub blocked_rule: Option<String>,
}

const CHECK_RUN_OUTPUT_CAP_BYTES: usize = 12 * 1024;

#[derive(Debug, Clone)]
struct CheckRun {
    exit_code: Option<i32>,
    passed: bool,
    stdout: String,
    stderr: String,
    truncated: bool,
    blocked_rule: Option<String>,
}

#[derive(Debug, Clone)]
struct ReflexFailure {
    command: String,
    run: CheckRun,
    diagnostics: Vec<crate::diagnostics::Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosticGroup {
    root_cause_key: String,
    representative_message: String,
    error_code: Option<String>,
    locations: Vec<(String, u32)>,
    multi_site: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RippleCandidateGroup {
    symbol: String,
    missing_field: Option<String>,
    compiler_reported: Vec<(String, u32)>,
    extra_candidates: Vec<(String, u32, String)>,
    truncated: bool,
}

pub(crate) struct ReflexFeedback {
    pub feedback: String,
    pub signature: String,
    #[allow(dead_code)]
    pub diagnostics: Vec<crate::diagnostics::Diagnostic>,
    pub candidates: Vec<RippleCandidate>,
}

pub(crate) struct ReflexValidation {
    pub checked: Vec<(String, bool)>,
    pub feedback: Option<ReflexFeedback>,
}

/// check_cmd 有效网络策略：global Off 或 verifier 显式 Off → Off（只收紧不放宽）；None 继承全局。
pub fn effective_network(
    global: crate::goal::NetworkPolicy,
    verifier: Option<crate::goal::NetworkPolicy>,
) -> crate::goal::NetworkPolicy {
    use crate::goal::NetworkPolicy::*;
    if global == Off || verifier == Some(Off) {
        Off
    } else {
        On
    }
}

/// 即时编译反馈用：找第一个支持 diagnostic prober 的 approved check_cmd，跑探针并返回诊断。
pub(crate) async fn probe_compile_diagnostics(
    goal: &GoalState,
    workspace: &Path,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    recorder: &mut EventRecorder,
    tool_call_tag: &str,
) -> Result<Vec<crate::diagnostics::Diagnostic>> {
    for c in &goal.contract.criteria {
        if c.approval != Approval::Approved {
            continue;
        }
        if let Verifier::Verifiable {
            check_cmd,
            success,
            timeout_s,
            network: v_net,
        } = &c.verifier
        {
            let Some(_prober) = crate::diagnostics::select_prober(check_cmd) else {
                continue;
            };
            let Some(probe_cmd) = crate::diagnostics::derive_probe_command(check_cmd) else {
                continue;
            };
            let eff = effective_network(network, *v_net);
            let (_run, diagnostics) = run_diagnostic_probe(
                &probe_cmd,
                success,
                *timeout_s,
                workspace,
                c.authored_by,
                recorder,
                eff,
                fs_write_fence,
                &c.id,
                tool_call_tag,
            )
            .await?;
            return Ok(diagnostics);
        }
    }
    Ok(Vec::new())
}

/// 对终态轮的完成判定：跑已批准 Verifiable check_cmd，更新 criteria.status，发 completion.evaluated。
#[allow(clippy::too_many_arguments)]
pub async fn evaluate_criteria(
    goal: &mut GoalState,
    contract_policy: ContractPolicy,
    workspace: &Path,
    final_text: &str,
    judge: &dyn crate::judge::Judge,
    recorder: &mut EventRecorder,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    round: usize,
) -> Result<()> {
    let evidence = format!(
        "objective: {}\n\nagent final reply:\n{}\n\ntool evidence:\n{}",
        goal.contract.objective,
        final_text,
        goal.evidence.join("\n")
    );
    let mut provenance: Vec<Option<CheckEvidence>> =
        Vec::with_capacity(goal.contract.criteria.len());
    for c in goal.contract.criteria.iter_mut() {
        match &c.verifier {
            Verifier::Verifiable {
                check_cmd,
                success,
                timeout_s,
                network: v_net,
                ..
            } => {
                if !is_contract_approved(c.authored_by, c.approval, contract_policy) {
                    c.status = CriterionStatus::Pending;
                    c.evidence_ref = Some("skipped: contract approval pending".into());
                    provenance.push(None);
                    continue;
                }
                let eff = effective_network(network, *v_net);
                let tool_call_id = terminal_check_tool_call_id(&c.id, round);
                let run = run_check_cmd(
                    check_cmd,
                    success,
                    *timeout_s,
                    workspace,
                    c.authored_by,
                    recorder,
                    eff,
                    fs_write_fence,
                    &c.id,
                    &tool_call_id,
                )
                .await?;
                let status = check_status(&run);
                let evidence = terminal_evidence_ref(check_cmd, *timeout_s, c.authored_by, &run);
                let ev = check_evidence(check_cmd, c.authored_by, &run);
                c.status = status;
                c.evidence_ref = Some(evidence);
                provenance.push(Some(ev));
            }
            Verifier::Judgmental { rubric } => {
                let verdict = judge.judge(&c.claim, rubric, &evidence, recorder).await?;
                let (status, label) = match verdict.decision {
                    crate::judge::JudgeDecision::Pass => (CriterionStatus::Passed, "passed"),
                    crate::judge::JudgeDecision::Fail => (CriterionStatus::Failed, "failed"),
                    crate::judge::JudgeDecision::Uncertain => {
                        (CriterionStatus::Uncertain, "uncertain")
                    }
                };
                recorder.emit(
                    "judge.evaluated",
                    serde_json::json!({
                        "criterion_id": c.id, "decision": label, "reason": verdict.reason
                    }),
                )?;
                c.status = status;
                c.evidence_ref = Some(format!("judge: {label} ({})", verdict.reason));
                provenance.push(None);
            }
        }
    }

    let per_criterion: Vec<_> = goal
        .contract
        .criteria
        .iter()
        .zip(provenance.iter())
        .map(|(c, prov)| {
            let mut obj = json!({
                "id": c.id,
                "status": status_str(c.status),
                "claim": c.claim,
                "evidence_ref": c.evidence_ref.clone(),
            });
            if let Some(ev) = prov {
                obj.as_object_mut().unwrap().insert(
                    "evidence".into(),
                    json!({
                        "authored_by": authored_by_str(ev.authored_by),
                        "command": ev.command,
                        "exit_code": ev.exit_code,
                        "passed": ev.passed,
                        "blocked_rule": ev.blocked_rule,
                    }),
                );
            }
            obj
        })
        .collect();
    recorder.emit("completion.evaluated", json!({ "criteria": per_criterion }))?;
    Ok(())
}

pub(crate) async fn reflex_validate(
    goal: &mut GoalState,
    workspace: &Path,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    reflex_round: u64,
    debt: usize,
    recorder: &mut EventRecorder,
) -> Result<ReflexValidation> {
    let mut failures = Vec::new();
    let mut checked = Vec::new();
    for c in &mut goal.contract.criteria {
        if c.approval != Approval::Approved {
            continue;
        }
        if let Verifier::Verifiable {
            check_cmd,
            success,
            timeout_s,
            network: v_net,
        } = &c.verifier
        {
            let eff = effective_network(network, *v_net);
            let tool_call_id = reflex_check_tool_call_id(reflex_round, &c.id);
            let (run, diagnostics) =
                if let Some(probe_cmd) = crate::diagnostics::derive_probe_command(check_cmd) {
                    run_diagnostic_probe(
                        &probe_cmd,
                        success,
                        *timeout_s,
                        workspace,
                        c.authored_by,
                        recorder,
                        eff,
                        fs_write_fence,
                        &c.id,
                        &tool_call_id,
                    )
                    .await?
                } else {
                    (
                        run_check_cmd(
                            check_cmd,
                            success,
                            *timeout_s,
                            workspace,
                            c.authored_by,
                            recorder,
                            eff,
                            fs_write_fence,
                            &c.id,
                            &tool_call_id,
                        )
                        .await?,
                        Vec::new(),
                    )
                };
            checked.push((c.id.clone(), run.passed));
            c.status = check_status(&run);
            if !run.passed {
                failures.push(ReflexFailure {
                    command: check_cmd.clone(),
                    run,
                    diagnostics,
                });
            }
        }
    }

    let failed: Vec<_> = failures
        .iter()
        .map(|failure| {
            json!({
                "cmd": failure.command,
                "exit_code": failure.run.exit_code,
            })
        })
        .collect();
    let passed = failures.is_empty();
    recorder.emit(
        "validation.checked",
        json!({
            "trigger": "reflex",
            "debt": debt,
            "reflex_round": reflex_round,
            "failed": failed,
            "passed": passed,
        }),
    )?;

    if passed {
        Ok(ReflexValidation {
            checked,
            feedback: None,
        })
    } else {
        let diagnostics = failures
            .iter()
            .flat_map(|failure| failure.diagnostics.clone())
            .collect();
        let candidates = collect_ripple_candidates(&failures, workspace);
        let projected_candidates = candidates.iter().map(project_ripple_candidate).collect();
        Ok(ReflexValidation {
            checked,
            feedback: Some(ReflexFeedback {
                feedback: format_reflex_feedback(&failures, &candidates),
                signature: reflex_failure_signature(&failures),
                diagnostics,
                candidates: projected_candidates,
            }),
        })
    }
}

pub struct AttemptTracker {
    max: usize,
    count: usize,
    last_fingerprint: Option<String>,
    last_passed: BTreeSet<String>,
}

impl AttemptTracker {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            count: 0,
            last_fingerprint: None,
            last_passed: BTreeSet::new(),
        }
    }

    /// 每次终态轮 eval 后调用，返回是否「已超限」（→ Blocked）。
    /// 规则：同一指纹下未出现「新 criterion 转 Passed」→ count+1；出现进展 → count=0。
    pub fn record(&mut self, goal: &GoalState) -> bool {
        let fp = fingerprint(goal);
        let passed: BTreeSet<String> = goal
            .contract
            .criteria
            .iter()
            .filter(|c| c.status == CriterionStatus::Passed)
            .map(|c| c.id.clone())
            .collect();
        let same_fp = self.last_fingerprint.as_deref() == Some(fp.as_str());
        let new_pass = passed.difference(&self.last_passed).next().is_some();
        if !new_pass && (same_fp || self.last_fingerprint.is_none()) {
            self.count += 1;
        } else {
            self.count = 0;
        }
        self.last_fingerprint = Some(fp);
        self.last_passed = passed;
        self.count >= self.max
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

/// 指纹 = 排序后的 (criterion id + verifier 内容) 串。
fn fingerprint(goal: &GoalState) -> String {
    let mut parts: Vec<String> = goal
        .contract
        .criteria
        .iter()
        .map(|c| {
            format!(
                "{}={}",
                c.id,
                serde_json::to_string(&c.verifier).unwrap_or_default()
            )
        })
        .collect();
    parts.sort();
    parts.join("|")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalOutcome {
    Complete,
    Continue,
    Blocked,
}

/// 综合判定 outcome（B9 的 evaluate_criteria 之后调用）。
/// 注意：空 criteria 在此处按 `all()` 真空为真；完成入口必须先通过 `may_finalize`。
pub fn decide_outcome(goal: &GoalState, exceeded: bool) -> EvalOutcome {
    let all_met = goal
        .contract
        .criteria
        .iter()
        .all(|c| matches!(c.status, CriterionStatus::Passed | CriterionStatus::Waived));
    if all_met {
        return EvalOutcome::Complete;
    }
    if exceeded {
        EvalOutcome::Blocked
    } else {
        EvalOutcome::Continue
    }
}

fn is_contract_approved(by: AuthoredBy, approval: Approval, policy: ContractPolicy) -> bool {
    match by {
        AuthoredBy::User => true,
        AuthoredBy::Agent => approval == Approval::Approved || policy == ContractPolicy::TrustAll,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_check_cmd(
    cmd: &str,
    success: &SuccessRule,
    timeout_s: u64,
    workspace: &Path,
    authored_by: AuthoredBy,
    recorder: &mut EventRecorder,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    criterion_id: &str,
    tool_call_id: &str,
) -> Result<CheckRun> {
    recorder.emit(
        "tool.started",
        json!({
            "tool": "check_cmd",
            "tool_call_id": tool_call_id,
            "criterion_id": criterion_id,
            "command": cmd,
            "cwd": workspace.to_string_lossy(),
            "authored_by": authored_by_str(authored_by),
        }),
    )?;
    let opts = ControlledExecOpts {
        command: cmd.to_string(),
        workspace: workspace.to_path_buf(),
        cwd: workspace.to_path_buf(),
        timeout_ms: timeout_s.saturating_mul(1000).max(1000),
        output_cap_bytes: 64 * 1024,
        network,
        fs_write_fence,
    };
    let outcome = controlled_exec(opts).await?;
    match outcome {
        ControlledExecOutcome::Blocked { rule } => {
            recorder.emit(
                "tool.failed",
                json!({
                    "tool": "check_cmd", "tool_call_id": tool_call_id,
                    "criterion_id": criterion_id, "rule": rule.clone(),
                    "error": format!("blocked: escape attempt ({rule})"),
                }),
            )?;
            Ok(CheckRun {
                exit_code: None,
                passed: false,
                stdout: String::new(),
                stderr: format!("blocked: escape attempt ({rule})"),
                truncated: false,
                blocked_rule: Some(rule),
            })
        }
        ControlledExecOutcome::NetworkUnenforceable { reason } => {
            recorder.emit(
                "tool.failed",
                json!({
                    "tool": "check_cmd",
                    "tool_call_id": tool_call_id,
                    "criterion_id": criterion_id,
                    "error": format!("network off unenforceable: {reason}"),
                }),
            )?;
            Ok(CheckRun {
                exit_code: None,
                passed: false,
                stdout: String::new(),
                stderr: format!("network off unenforceable: {reason}"),
                truncated: false,
                blocked_rule: None,
            })
        }
        ControlledExecOutcome::Ran {
            stdout,
            stderr,
            exit_code,
            timed_out,
            truncated,
        } => {
            if timed_out {
                recorder.emit(
                    "tool.failed",
                    json!({ "tool": "check_cmd", "tool_call_id": tool_call_id,
                            "criterion_id": criterion_id, "error": "timeout" }),
                )?;
                return Ok(CheckRun {
                    exit_code: None,
                    passed: false,
                    stdout: String::new(),
                    stderr: format!("timeout after {timeout_s}s"),
                    truncated: false,
                    blocked_rule: None,
                });
            }
            let passed = match success {
                SuccessRule::ExitZero => exit_code == Some(0),
                SuccessRule::StdoutContains(s) => stdout.contains(s.as_str()),
            };
            recorder.emit(
                "tool.completed",
                json!({
                    "tool": "check_cmd", "tool_call_id": tool_call_id,
                    "criterion_id": criterion_id, "exit_code": exit_code, "passed": passed,
                }),
            )?;
            let (stdout, stdout_truncated) = truncate_check_output(&stdout, false);
            let (stderr, stderr_truncated) = truncate_check_output(&stderr, false);
            Ok(CheckRun {
                exit_code,
                passed,
                stdout,
                stderr,
                truncated: truncated || stdout_truncated || stderr_truncated,
                blocked_rule: None,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_diagnostic_probe(
    cmd: &str,
    _success: &SuccessRule,
    timeout_s: u64,
    workspace: &Path,
    authored_by: AuthoredBy,
    recorder: &mut EventRecorder,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    source_criterion_id: &str,
    tool_call_id: &str,
) -> Result<(CheckRun, Vec<crate::diagnostics::Diagnostic>)> {
    recorder.emit(
        "tool.started",
        json!({
            "tool": "diagnostic_probe",
            "tool_call_id": tool_call_id,
            "probe_kind": "cargo_check",
            "source_criterion_id": source_criterion_id,
            "command": cmd,
            "cwd": workspace.to_string_lossy(),
            "authored_by": authored_by_str(authored_by),
        }),
    )?;
    let opts = ControlledExecOpts {
        command: cmd.to_string(),
        workspace: workspace.to_path_buf(),
        cwd: workspace.to_path_buf(),
        timeout_ms: timeout_s.saturating_mul(1000).max(1000),
        output_cap_bytes: 16 * 1024 * 1024,
        network,
        fs_write_fence,
    };
    let outcome = controlled_exec(opts).await?;
    match outcome {
        ControlledExecOutcome::Blocked { rule } => {
            recorder.emit(
                "tool.failed",
                json!({
                    "tool": "diagnostic_probe",
                    "tool_call_id": tool_call_id,
                    "source_criterion_id": source_criterion_id,
                    "rule": rule.clone(),
                    "error": format!("blocked: escape attempt ({rule})"),
                }),
            )?;
            Ok((
                CheckRun {
                    exit_code: None,
                    passed: false,
                    stdout: String::new(),
                    stderr: format!("blocked: escape attempt ({rule})"),
                    truncated: false,
                    blocked_rule: Some(rule),
                },
                Vec::new(),
            ))
        }
        ControlledExecOutcome::NetworkUnenforceable { reason } => {
            recorder.emit(
                "tool.failed",
                json!({
                    "tool": "diagnostic_probe",
                    "tool_call_id": tool_call_id,
                    "source_criterion_id": source_criterion_id,
                    "error": format!("network off unenforceable: {reason}"),
                }),
            )?;
            Ok((
                CheckRun {
                    exit_code: None,
                    passed: false,
                    stdout: String::new(),
                    stderr: format!("network off unenforceable: {reason}"),
                    truncated: false,
                    blocked_rule: None,
                },
                Vec::new(),
            ))
        }
        ControlledExecOutcome::Ran {
            stdout,
            stderr,
            exit_code,
            timed_out,
            truncated,
        } => {
            let diagnostics = crate::diagnostics::parse_cargo_diagnostics(&stdout);
            if timed_out {
                recorder.emit(
                    "tool.failed",
                    json!({
                        "tool": "diagnostic_probe",
                        "tool_call_id": tool_call_id,
                        "source_criterion_id": source_criterion_id,
                        "error": "timeout",
                    }),
                )?;
                return Ok((
                    CheckRun {
                        exit_code: None,
                        passed: false,
                        stdout: String::new(),
                        stderr: format!("timeout after {timeout_s}s"),
                        truncated: false,
                        blocked_rule: None,
                    },
                    diagnostics,
                ));
            }
            let passed = exit_code == Some(0);
            recorder.emit(
                "tool.completed",
                json!({
                    "tool": "diagnostic_probe",
                    "tool_call_id": tool_call_id,
                    "source_criterion_id": source_criterion_id,
                    "exit_code": exit_code,
                    "passed": passed,
                    "diagnostics_count": diagnostics.len(),
                }),
            )?;
            let (stdout, stdout_truncated) = truncate_check_output(&stdout, false);
            let (stderr, stderr_truncated) = truncate_check_output(&stderr, false);
            Ok((
                CheckRun {
                    exit_code,
                    passed,
                    stdout,
                    stderr,
                    truncated: truncated || stdout_truncated || stderr_truncated,
                    blocked_rule: None,
                },
                diagnostics,
            ))
        }
    }
}

fn terminal_check_tool_call_id(criterion_id: &str, round: usize) -> String {
    format!("check_{criterion_id}_{round}")
}

fn reflex_check_tool_call_id(reflex_round: u64, criterion_id: &str) -> String {
    format!("check_reflex_{reflex_round}_{criterion_id}")
}

fn check_status(run: &CheckRun) -> CriterionStatus {
    if run.passed {
        CriterionStatus::Passed
    } else {
        CriterionStatus::Failed
    }
}

fn check_evidence(cmd: &str, authored_by: AuthoredBy, run: &CheckRun) -> CheckEvidence {
    CheckEvidence {
        authored_by,
        command: cmd.to_string(),
        exit_code: run.exit_code,
        passed: run.passed,
        blocked_rule: run.blocked_rule.clone(),
    }
}

fn terminal_evidence_ref(
    cmd: &str,
    timeout_s: u64,
    authored_by: AuthoredBy,
    run: &CheckRun,
) -> String {
    if let Some(rule) = &run.blocked_rule {
        return format!(
            "check_cmd[{}] blocked: escape attempt ({rule}) cmd={cmd}",
            authored_by_str(authored_by)
        );
    }
    if run.exit_code.is_none() {
        if let Some(reason) = run.stderr.strip_prefix("network off unenforceable: ") {
            return format!(
                "check_cmd[{}] network off unenforceable: {reason} cmd={cmd}",
                authored_by_str(authored_by)
            );
        }
        return format!("check_cmd timed out after {timeout_s}s: {cmd}");
    }
    format!(
        "check_cmd[{}] exit={:?} passed={} cmd={} stdout={} stderr={}",
        authored_by_str(authored_by),
        run.exit_code,
        run.passed,
        cmd,
        first_line(&run.stdout),
        first_line(&run.stderr)
    )
}

fn format_reflex_feedback(
    failures: &[ReflexFailure],
    candidates: &[RippleCandidateGroup],
) -> String {
    let diagnostics = failures
        .iter()
        .flat_map(|failure| failure.diagnostics.iter().cloned())
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return format_raw_feedback(failures);
    }

    let mut feedback = "Validation found compile errors — fix all remaining sites:\n".to_string();
    for group in cluster_diagnostics(&diagnostics) {
        let code = group.error_code.as_deref().unwrap_or("unknown");
        feedback.push_str(&format!(
            "[{code}] {}\nstill {} places:\n",
            group.representative_message,
            group.locations.len()
        ));
        for (file, line) in &group.locations {
            feedback.push_str(&format!("  - {file}:{line}\n"));
        }
        if group.multi_site && group.error_code.is_some() {
            feedback.push_str(
                "(API/field shape changed — search this symbol's references and fix them all in one pass)\n",
            );
        }
    }
    for candidate in candidates {
        feedback.push_str(&format!(
            "Constructor sites to audit for `{}` (compiler is ground truth — fix all in one pass; some may already be fixed):\n",
            candidate.symbol
        ));
        if let Some(field) = &candidate.missing_field {
            feedback.push_str(&format!("  missing field: `{field}`\n"));
        }
        feedback.push_str("  compiler-reported:\n");
        for (file, line) in &candidate.compiler_reported {
            feedback.push_str(&format!("    - {file}:{line}\n"));
        }
        feedback
            .push_str("  additional candidates (text search, may include already-fixed sites):\n");
        for (file, line, snippet) in &candidate.extra_candidates {
            feedback.push_str(&format!("    - {file}:{line}   {snippet}\n"));
        }
        if candidate.truncated {
            feedback.push_str("  (candidate search truncated)\n");
        }
    }

    feedback.push_str("---\nRaw validation output:\n");
    for failure in failures {
        let exit = failure
            .run
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        feedback.push_str(&format!("$ {}  (exit {exit})\n", failure.command));
        let output = reflex_output(&failure.run);
        if !output.is_empty() {
            feedback.push_str(&output);
            if !feedback.ends_with('\n') {
                feedback.push('\n');
            }
        }
    }
    feedback
}

fn format_raw_feedback(failures: &[ReflexFailure]) -> String {
    let mut feedback =
        "Validation after your recent edits failed — fix these before continuing:\n".to_string();
    for failure in failures {
        let exit = failure
            .run
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        feedback.push_str(&format!("$ {}  (exit {exit})\n", failure.command));
        let output = reflex_output(&failure.run);
        if !output.is_empty() {
            feedback.push_str(&output);
            if !feedback.ends_with('\n') {
                feedback.push('\n');
            }
        }
    }
    feedback
}

fn cluster_diagnostics(diags: &[crate::diagnostics::Diagnostic]) -> Vec<DiagnosticGroup> {
    let mut groups: Vec<DiagnosticGroup> = Vec::new();

    for diag in diags {
        let location = (diag.file.clone(), diag.line);
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.root_cause_key == diag.root_cause_key)
        {
            if !group.locations.contains(&location) {
                group.locations.push(location);
                group.multi_site = group.locations.len() > 1;
            }
            continue;
        }

        groups.push(DiagnosticGroup {
            root_cause_key: diag.root_cause_key.clone(),
            representative_message: diag.message.clone(),
            error_code: diag.error_code.clone(),
            locations: vec![location],
            multi_site: false,
        });
    }

    groups
}

fn collect_ripple_candidates(
    failures: &[ReflexFailure],
    workspace: &Path,
) -> Vec<RippleCandidateGroup> {
    let diagnostics = failures
        .iter()
        .flat_map(|failure| failure.diagnostics.iter())
        .collect::<Vec<_>>();
    let groups = cluster_diagnostics(
        &diagnostics
            .iter()
            .map(|diag| (*diag).clone())
            .collect::<Vec<_>>(),
    );
    let mut candidates = Vec::new();

    for group in groups {
        if !group.multi_site || group.error_code.as_deref() != Some("E0063") {
            continue;
        }
        let group_diags = diagnostics
            .iter()
            .copied()
            .filter(|diag| diag.root_cause_key == group.root_cause_key)
            .collect::<Vec<_>>();
        let Some(symbol) = group_diags
            .iter()
            .find_map(|diag| crate::diagnostics::extract_ripple_symbol(diag))
        else {
            continue;
        };
        let pattern = format!("{symbol} {{");
        let (hits, truncated) = crate::tools::grep::grep_workspace(workspace, &pattern, true, 500);
        if hits.is_empty() {
            continue;
        }

        let reported = group.locations.iter().cloned().collect::<BTreeSet<_>>();
        let mut seen_extra = BTreeSet::new();
        let mut extra_candidates = Vec::new();
        for hit in hits {
            let location = (hit.path.clone(), hit.line);
            if reported.contains(&location) || !seen_extra.insert(location.clone()) {
                continue;
            }
            extra_candidates.push((hit.path, hit.line, hit.text));
        }

        candidates.push(RippleCandidateGroup {
            symbol,
            missing_field: group_diags.iter().find_map(|diag| diag.symbol.clone()),
            compiler_reported: group.locations,
            extra_candidates,
            truncated,
        });
    }

    candidates
}

fn project_ripple_candidate(candidate: &RippleCandidateGroup) -> RippleCandidate {
    RippleCandidate {
        symbol: candidate.symbol.clone(),
        missing_field: candidate.missing_field.clone(),
        compiler_reported_sites: candidate
            .compiler_reported
            .iter()
            .map(|(file, line)| format!("{file}:{line}"))
            .collect(),
        extra_candidate_sites: candidate
            .extra_candidates
            .iter()
            .map(|(file, line, snippet)| format!("{file}:{line}   {snippet}"))
            .collect(),
        truncated: candidate.truncated,
    }
}

fn reflex_failure_signature(failures: &[ReflexFailure]) -> String {
    // 用 JSON 编码 (command, exit) 列表当指纹：命令里含 `#`/`|` 也不会跨不同失败集串味
    // （指纹只用于「同一失败连续重复」相等比较·不回解）。
    let mut parts = failures
        .iter()
        .map(|failure| {
            let exit = failure
                .run
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string());
            (failure.command.clone(), exit)
        })
        .collect::<Vec<_>>();
    parts.sort();
    format!(
        "reflex:{}",
        serde_json::to_string(&parts).unwrap_or_default()
    )
}

fn reflex_output(run: &CheckRun) -> String {
    let mut chunks = Vec::new();
    if !run.stderr.is_empty() {
        chunks.push(run.stderr.as_str());
    }
    if !run.stdout.is_empty() {
        chunks.push(run.stdout.as_str());
    }
    let output = chunks.join("\n");
    if output.is_empty() {
        return String::new();
    }
    if run.truncated {
        format!("[output truncated]\n{output}")
    } else {
        output
    }
}

fn truncate_check_output(text: &str, upstream_truncated: bool) -> (String, bool) {
    if text.len() <= CHECK_RUN_OUTPUT_CAP_BYTES {
        return (text.to_string(), upstream_truncated);
    }

    let marker = "\n...[truncated]...\n";
    let first_budget = (CHECK_RUN_OUTPUT_CAP_BYTES / 4)
        .min(CHECK_RUN_OUTPUT_CAP_BYTES.saturating_sub(marker.len()));
    let first_line_end = text.find('\n').map(|idx| idx + 1).unwrap_or(first_budget);
    let first_end = previous_char_boundary(text, first_line_end.min(first_budget));
    let first = &text[..first_end];
    let tail_budget = CHECK_RUN_OUTPUT_CAP_BYTES.saturating_sub(first.len() + marker.len());
    let tail_start = next_char_boundary(text, text.len().saturating_sub(tail_budget));
    (format!("{first}{marker}{}", &text[tail_start..]), true)
}

fn previous_char_boundary(text: &str, mut idx: usize) -> usize {
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn next_char_boundary(text: &str, mut idx: usize) -> usize {
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

pub(crate) fn status_str(s: CriterionStatus) -> &'static str {
    match s {
        CriterionStatus::Pending => "pending",
        CriterionStatus::Passed => "passed",
        CriterionStatus::Failed => "failed",
        CriterionStatus::Waived => "waived",
        CriterionStatus::Uncertain => "uncertain",
    }
}

fn authored_by_str(b: AuthoredBy) -> &'static str {
    match b {
        AuthoredBy::User => "user",
        AuthoredBy::Agent => "agent",
    }
}

#[cfg(test)]
mod net_tests {
    use super::effective_network;
    use crate::goal::NetworkPolicy::*;

    #[test]
    fn global_off_forces_off() {
        assert_eq!(effective_network(Off, Some(On)), Off);
    }

    #[test]
    fn verifier_off_tightens() {
        assert_eq!(effective_network(On, Some(Off)), Off);
    }

    #[test]
    fn none_inherits() {
        assert_eq!(effective_network(On, None), On);
        assert_eq!(effective_network(Off, None), Off);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_events(path: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn failing_criterion(id: &str) -> crate::goal::Criterion {
        crate::goal::Criterion {
            id: id.into(),
            claim: "fails".into(),
            scope: None,
            authored_by: AuthoredBy::User,
            approval: Approval::Approved,
            verifier: Verifier::Verifiable {
                check_cmd: "false".into(),
                success: SuccessRule::ExitZero,
                timeout_s: 120,
                network: None,
            },
            status: CriterionStatus::Pending,
            evidence_ref: None,
        }
    }

    fn diagnostic(file: &str, line: u32, message: &str) -> crate::diagnostics::Diagnostic {
        crate::diagnostics::Diagnostic {
            file: file.into(),
            line,
            error_code: Some("E0063".into()),
            message: message.into(),
            root_cause_key: "E0063|missing field <id> in initializer of <id>".into(),
            symbol: Some("missing_field".into()),
        }
    }

    fn reflex_failure_with_diagnostics(
        diagnostics: Vec<crate::diagnostics::Diagnostic>,
    ) -> ReflexFailure {
        ReflexFailure {
            command: "cargo test --no-run".into(),
            run: CheckRun {
                exit_code: Some(101),
                passed: false,
                stdout: "raw cargo output".into(),
                stderr: String::new(),
                truncated: false,
                blocked_rule: None,
            },
            diagnostics,
        }
    }

    #[test]
    fn attempts_increment_then_block_without_progress() {
        let mut g = GoalState::new("obj", vec![failing_criterion("c1")]);
        g.contract.criteria[0].status = CriterionStatus::Failed;
        let mut t = AttemptTracker::new(3);
        assert!(!t.record(&g));
        assert!(!t.record(&g));
        assert!(t.record(&g));
        assert_eq!(decide_outcome(&g, true), EvalOutcome::Blocked);
    }

    #[test]
    fn pending_criterion_blocks_complete() {
        // 一条 passed + 一条 pending → 不应 Complete（现逻辑会错判 Complete）
        let mut g = GoalState::new(
            "obj",
            vec![failing_criterion("c1"), failing_criterion("c2")],
        );
        g.contract.criteria[0].status = CriterionStatus::Passed;
        // c2 维持 Pending
        assert_ne!(decide_outcome(&g, false), EvalOutcome::Complete);
        // 全部 passed/waived → Complete
        g.contract.criteria[1].status = CriterionStatus::Waived;
        assert_eq!(decide_outcome(&g, false), EvalOutcome::Complete);
    }

    #[test]
    fn progress_resets_attempts() {
        let mut g = GoalState::new(
            "obj",
            vec![failing_criterion("c1"), failing_criterion("c2")],
        );
        g.contract.criteria[0].status = CriterionStatus::Failed;
        g.contract.criteria[1].status = CriterionStatus::Failed;
        let mut t = AttemptTracker::new(3);
        t.record(&g);
        t.record(&g);
        g.contract.criteria[0].status = CriterionStatus::Passed;
        assert!(!t.record(&g));
        assert_eq!(t.count(), 0);
    }

    #[test]
    fn clusters_same_root_cause_with_all_locations() {
        let jsonl = include_str!("../tests/fixtures/cargo-diagnostics/missing_field.jsonl");
        let diags = crate::diagnostics::parse_cargo_diagnostics(jsonl);
        let groups = cluster_diagnostics(&diags);
        let e0063 = groups
            .iter()
            .find(|group| group.error_code.as_deref() == Some("E0063"))
            .expect("E0063 group");

        assert!(
            e0063.locations.len() >= 2,
            "expected multiple locations, got {:?}",
            e0063.locations
        );
        assert!(e0063.multi_site);

        let unique_locations: std::collections::BTreeSet<_> = e0063.locations.iter().collect();
        assert_eq!(unique_locations.len(), e0063.locations.len());
    }

    #[test]
    fn structured_feedback_lists_remaining_sites_and_hint() {
        let jsonl = include_str!("../tests/fixtures/cargo-diagnostics/missing_field.jsonl");
        let diags = crate::diagnostics::parse_cargo_diagnostics(jsonl);
        let feedback = format_reflex_feedback(
            &[ReflexFailure {
                command: "cargo test --no-run".into(),
                run: CheckRun {
                    exit_code: Some(101),
                    passed: false,
                    stdout: "raw cargo output".into(),
                    stderr: String::new(),
                    truncated: false,
                    blocked_rule: None,
                },
                diagnostics: diags,
            }],
            &[],
        );

        assert!(feedback.contains("Validation found compile errors"));
        assert!(feedback.contains("E0063"));
        assert!(feedback.contains("still"));
        assert!(feedback.matches(".rs:").count() >= 2);
        assert!(feedback.to_lowercase().contains("search"));
        assert!(feedback.contains("---"));
        assert!(feedback.contains("raw cargo output"));
    }

    #[test]
    fn non_cargo_feedback_unchanged_when_no_diagnostics() {
        let feedback = format_reflex_feedback(
            &[ReflexFailure {
                command: "check failing".into(),
                run: CheckRun {
                    exit_code: Some(2),
                    passed: false,
                    stdout: String::new(),
                    stderr: "boom".into(),
                    truncated: false,
                    blocked_rule: None,
                },
                diagnostics: vec![],
            }],
            &[],
        );

        assert_eq!(
            feedback,
            "Validation after your recent edits failed — fix these before continuing:\n$ check failing  (exit 2)\nboom\n"
        );
    }

    #[test]
    fn feedback_no_candidates_byte_identical_to_slice_b() {
        let message = "missing field `missing_field` in initializer of `RunOptions`";
        let failure = reflex_failure_with_diagnostics(vec![
            diagnostic("src/a.rs", 10, message),
            diagnostic("src/b.rs", 20, message),
        ]);

        let feedback = format_reflex_feedback(&[failure], &[]);

        assert_eq!(
            feedback,
            concat!(
                "Validation found compile errors — fix all remaining sites:\n",
                "[E0063] missing field `missing_field` in initializer of `RunOptions`\n",
                "still 2 places:\n",
                "  - src/a.rs:10\n",
                "  - src/b.rs:20\n",
                "(API/field shape changed — search this symbol's references and fix them all in one pass)\n",
                "---\n",
                "Raw validation output:\n",
                "$ cargo test --no-run  (exit 101)\n",
                "raw cargo output\n",
            )
        );
    }

    #[test]
    fn feedback_lists_audit_sites_separating_reported_and_extra() {
        let message = "missing field `missing_field` in initializer of `RunOptions`";
        let failure = reflex_failure_with_diagnostics(vec![
            diagnostic("src/a.rs", 10, message),
            diagnostic("src/b.rs", 20, message),
        ]);
        let candidates = vec![RippleCandidateGroup {
            symbol: "RunOptions".into(),
            missing_field: Some("missing_field".into()),
            compiler_reported: vec![("src/a.rs".into(), 10), ("src/b.rs".into(), 20)],
            extra_candidates: vec![(
                "tests/hidden.rs".into(),
                7,
                "let _ = RunOptions { field: 1 };".into(),
            )],
            truncated: true,
        }];

        let feedback = format_reflex_feedback(&[failure], &candidates);

        assert!(feedback
            .contains("Constructor sites to audit for `RunOptions` (compiler is ground truth"));
        assert!(feedback.contains("some may already be fixed"));
        assert!(feedback.contains("compiler-reported:"));
        assert!(feedback.contains("  - src/a.rs:10"));
        assert!(feedback
            .contains("additional candidates (text search, may include already-fixed sites):"));
        assert!(feedback.contains("  - tests/hidden.rs:7   let _ = RunOptions { field: 1 };"));
        assert!(feedback.contains("truncated"));
    }

    #[tokio::test]
    async fn judge_receives_agent_final_reply_in_evidence() {
        use std::sync::{Arc, Mutex};

        struct CapturingJudge(Arc<Mutex<String>>);

        #[async_trait::async_trait]
        impl crate::judge::Judge for CapturingJudge {
            async fn judge(
                &self,
                _claim: &str,
                _rubric: &str,
                evidence: &str,
                _ev: &mut EventRecorder,
            ) -> Result<crate::judge::JudgeVerdict> {
                *self.0.lock().unwrap() = evidence.to_string();
                Ok(crate::judge::JudgeVerdict {
                    decision: crate::judge::JudgeDecision::Pass,
                    reason: String::new(),
                })
            }
        }

        let mut goal = GoalState::new(
            "greet the user",
            crate::goal::parse_criteria(&["judge: friendly greeting".into()]).unwrap(),
        );
        let captured = Arc::new(Mutex::new(String::new()));
        let judge = CapturingJudge(captured.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &dir.path().join("e.jsonl"),
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        evaluate_criteria(
            &mut goal,
            ContractPolicy::Ask,
            dir.path(),
            "Hello there, friend!",
            &judge,
            &mut rec,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            0,
        )
        .await
        .unwrap();

        assert!(
            captured.lock().unwrap().contains("Hello there, friend!"),
            "judge evidence must include agent final reply"
        );
        assert_eq!(goal.contract.criteria[0].status, CriterionStatus::Passed);
    }

    #[tokio::test]
    async fn check_cmd_emits_deterministic_tool_call_id() {
        struct PassJudge;

        #[async_trait::async_trait]
        impl crate::judge::Judge for PassJudge {
            async fn judge(
                &self,
                _claim: &str,
                _rubric: &str,
                _evidence: &str,
                _ev: &mut EventRecorder,
            ) -> Result<crate::judge::JudgeVerdict> {
                Ok(crate::judge::JudgeVerdict {
                    decision: crate::judge::JudgeDecision::Pass,
                    reason: String::new(),
                })
            }
        }

        let mut goal = GoalState::new(
            "run a check",
            crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
        );
        let crit_id = goal.contract.criteria[0].id.clone();
        let judge = PassJudge;
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        evaluate_criteria(
            &mut goal,
            ContractPolicy::Ask,
            dir.path(),
            "",
            &judge,
            &mut rec,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            0,
        )
        .await
        .unwrap();

        let events = read_events(&events_path);
        let started = events
            .iter()
            .find(|event| event["type"] == "tool.started")
            .unwrap();
        assert_eq!(
            started["payload"]["tool_call_id"],
            format!("check_{crit_id}_0")
        );
        assert_eq!(started["payload"]["criterion_id"], crit_id);

        let evaluated = events
            .iter()
            .find(|event| event["type"] == "completion.evaluated")
            .unwrap();
        assert_eq!(
            evaluated["payload"]["criteria"][0]["evidence"]["command"],
            "true"
        );
        assert_eq!(
            evaluated["payload"]["criteria"][0]["evidence"]["authored_by"],
            "user"
        );
    }

    #[tokio::test]
    async fn check_cmd_escape_blocked_fails_criterion() {
        struct PassJudge;

        #[async_trait::async_trait]
        impl crate::judge::Judge for PassJudge {
            async fn judge(
                &self,
                _claim: &str,
                _rubric: &str,
                _evidence: &str,
                _ev: &mut EventRecorder,
            ) -> Result<crate::judge::JudgeVerdict> {
                Ok(crate::judge::JudgeVerdict {
                    decision: crate::judge::JudgeDecision::Pass,
                    reason: String::new(),
                })
            }
        }

        let mut goal = GoalState::new(
            "block escapes",
            crate::goal::parse_criteria(&["cmd: setsid true".into()]).unwrap(),
        );
        let judge = PassJudge;
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        evaluate_criteria(
            &mut goal,
            ContractPolicy::Ask,
            dir.path(),
            "",
            &judge,
            &mut rec,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            0,
        )
        .await
        .unwrap();

        assert_eq!(goal.contract.criteria[0].status, CriterionStatus::Failed);
        let events = read_events(&events_path);
        assert!(events.iter().any(|event| {
            event["type"] == "tool.failed"
                && event["payload"]["tool"] == "check_cmd"
                && event["payload"]["rule"] == "setsid"
        }));
    }

    #[tokio::test]
    async fn verify_reflex_feedback_includes_failed_cmd_output_and_reflex_id() {
        let mut goal = GoalState::new(
            "run reflex validation",
            crate::goal::parse_criteria(&[
                "cmd: printf verify-reflex-stdout; printf verify-reflex-stderr >&2; exit 7".into(),
            ])
            .unwrap(),
        );
        goal.contract.criteria[0].status = CriterionStatus::Passed;
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let feedback = reflex_validate(
            &mut goal,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            7,
            3,
            &mut rec,
        )
        .await
        .unwrap()
        .feedback
        .unwrap();

        assert!(feedback
            .feedback
            .contains("Validation after your recent edits failed"));
        assert!(feedback.diagnostics.is_empty());
        assert!(feedback.feedback.contains("$ printf verify-reflex-stdout"));
        assert!(feedback.feedback.contains("(exit 7)"));
        assert!(feedback.feedback.contains("verify-reflex-stderr"));
        assert_eq!(
            feedback.signature,
            "reflex:[[\"printf verify-reflex-stdout; printf verify-reflex-stderr >&2; exit 7\",\"7\"]]"
        );
        assert_eq!(goal.contract.criteria[0].status, CriterionStatus::Failed);

        let events = read_events(&events_path);
        assert!(events.iter().any(|event| {
            event["type"] == "tool.started"
                && event["payload"]["tool_call_id"] == "check_reflex_7_c1"
                && event["payload"]["tool"] == "check_cmd"
        }));
        assert!(!events.iter().any(|event| {
            event["type"] == "tool.started" && event["payload"]["tool"] == "diagnostic_probe"
        }));
        assert!(!events.iter().any(|event| {
            event["payload"]["tool_call_id"] == terminal_check_tool_call_id("c1", 7)
        }));
        assert_ne!(
            reflex_check_tool_call_id(7, "c1"),
            terminal_check_tool_call_id("c1", 7)
        );
    }

    #[tokio::test]
    async fn verify_reflex_updates_status_and_returns_checked_results() {
        let mut goal = GoalState::new(
            "run reflex validation",
            crate::goal::parse_criteria(&[
                "cmd: true".into(),
                "cmd: false".into(),
                "judge: inspect manually".into(),
            ])
            .unwrap(),
        );
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let outcome = reflex_validate(
            &mut goal,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            8,
            3,
            &mut rec,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.checked,
            vec![("c1".to_string(), true), ("c2".to_string(), false)]
        );
        assert!(outcome.feedback.is_some());
        assert_eq!(goal.contract.criteria[0].status, CriterionStatus::Passed);
        assert_eq!(goal.contract.criteria[1].status, CriterionStatus::Failed);
        assert_eq!(goal.contract.criteria[2].status, CriterionStatus::Pending);
    }

    #[tokio::test]
    async fn verify_reflex_returns_none_when_all_verifiable_checks_pass_or_absent() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let mut passing = GoalState::new(
            "all pass",
            crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
        );

        let passing_outcome = reflex_validate(
            &mut passing,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            1,
            2,
            &mut rec,
        )
        .await
        .unwrap();
        assert_eq!(passing_outcome.checked, vec![("c1".to_string(), true)]);
        assert!(passing_outcome.feedback.is_none());
        assert_eq!(passing.contract.criteria[0].status, CriterionStatus::Passed);

        let mut judgmental = GoalState::new(
            "no executable checks",
            crate::goal::parse_criteria(&["judge: inspect manually".into()]).unwrap(),
        );
        let judgmental_outcome = reflex_validate(
            &mut judgmental,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            2,
            2,
            &mut rec,
        )
        .await
        .unwrap();
        assert!(judgmental_outcome.checked.is_empty());
        assert!(judgmental_outcome.feedback.is_none());
    }

    #[tokio::test]
    async fn verify_reflex_uses_diagnostic_probe_for_derived_cargo_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "reflex_probe_fixture"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn broken() -> MissingType { MissingType }\n",
        )
        .unwrap();

        let mut goal = GoalState::new(
            "run reflex validation",
            crate::goal::parse_criteria(&[
                "cmd: cargo test --no-run --manifest-path Cargo.toml".into()
            ])
            .unwrap(),
        );
        let events_path = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let feedback = reflex_validate(
            &mut goal,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            11,
            2,
            &mut rec,
        )
        .await
        .unwrap()
        .feedback
        .unwrap();

        assert!(
            !feedback.diagnostics.is_empty(),
            "cargo probe should parse structured diagnostics"
        );
        assert_eq!(
            feedback.signature,
            "reflex:[[\"cargo test --no-run --manifest-path Cargo.toml\",\"101\"]]"
        );
        assert!(feedback
            .feedback
            .contains("$ cargo test --no-run --manifest-path Cargo.toml"));

        let events = read_events(&events_path);
        let started = events
            .iter()
            .find(|event| {
                event["type"] == "tool.started" && event["payload"]["tool"] == "diagnostic_probe"
            })
            .expect("diagnostic_probe tool.started must be emitted");
        assert_eq!(started["payload"]["tool_call_id"], "check_reflex_11_c1");
        assert_eq!(started["payload"]["probe_kind"], "cargo_check");
        assert_eq!(started["payload"]["source_criterion_id"], "c1");
        assert_eq!(
            started["payload"]["command"],
            "cargo check --manifest-path Cargo.toml --all-targets --keep-going --message-format=json"
        );
        assert_eq!(started["payload"]["authored_by"], "user");

        let completed = events
            .iter()
            .find(|event| {
                event["type"] == "tool.completed" && event["payload"]["tool"] == "diagnostic_probe"
            })
            .expect("diagnostic_probe tool.completed must be emitted");
        assert_eq!(completed["payload"]["source_criterion_id"], "c1");
        assert_eq!(completed["payload"]["passed"], false);
        assert!(
            completed["payload"]["diagnostics_count"].as_u64().unwrap() > 0,
            "completed event should report diagnostic count"
        );

        let checked = events
            .iter()
            .find(|event| event["type"] == "validation.checked")
            .unwrap();
        assert_eq!(
            checked["payload"]["failed"][0]["cmd"],
            "cargo test --no-run --manifest-path Cargo.toml"
        );
    }

    #[test]
    fn verify_reflex_feedback_truncates_failed_output() {
        let stderr = format!(
            "{}tail-marker",
            "x".repeat(CHECK_RUN_OUTPUT_CAP_BYTES + 256)
        );
        let feedback = format_reflex_feedback(
            &[ReflexFailure {
                command: "check failing".into(),
                run: CheckRun {
                    exit_code: Some(2),
                    passed: false,
                    stdout: String::new(),
                    stderr: truncate_check_output(&stderr, false).0,
                    truncated: true,
                    blocked_rule: None,
                },
                diagnostics: vec![],
            }],
            &[],
        );

        assert!(feedback.contains("$ check failing  (exit 2)"));
        assert!(feedback.contains("tail-marker"));
        assert!(feedback.contains("truncated"));
        assert!(feedback.len() < CHECK_RUN_OUTPUT_CAP_BYTES + 1024);
    }

    #[tokio::test]
    async fn reflex_validate_lists_extra_constructor_candidates_from_rs_search() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::create_dir(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "reflex_candidates_fixture"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            r#"
pub struct RunOptions
{
    pub field: u8,
    pub missing_field: u8,
}

pub fn one() -> RunOptions {
    RunOptions { field: 1 }
}

pub fn two() -> RunOptions {
    RunOptions { field: 2 }
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tests").join("hidden.rs"),
            r#"
use reflex_candidates_fixture::RunOptions;

#[test]
fn hidden_constructor() {
    let _ = RunOptions { field: 3 };
}
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.md"), "RunOptions { field: 4 }\n").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(
            dir.path().join("target").join("noise.rs"),
            "RunOptions { field: 5 }\n",
        )
        .unwrap();

        let mut goal = GoalState::new(
            "run reflex validation",
            crate::goal::parse_criteria(&[
                "cmd: cargo test --no-run --manifest-path Cargo.toml".into()
            ])
            .unwrap(),
        );
        let events_path = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &events_path,
            crate::events::OutputMode::Silent,
        )
        .unwrap();

        let feedback = reflex_validate(
            &mut goal,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            12,
            2,
            &mut rec,
        )
        .await
        .unwrap()
        .feedback
        .unwrap();

        assert!(feedback
            .feedback
            .contains("Constructor sites to audit for `RunOptions`"));
        assert!(feedback.feedback.contains("compiler-reported:"));
        assert!(feedback.feedback.contains("additional candidates"));
        assert!(feedback.feedback.contains("tests/hidden.rs"));
        assert!(!feedback.feedback.contains("notes.md"));
        assert!(!feedback.feedback.contains("target/noise.rs"));
        assert_eq!(feedback.candidates.len(), 1);
        assert_eq!(feedback.candidates[0].symbol, "RunOptions");
        assert_eq!(
            feedback.candidates[0].missing_field.as_deref(),
            Some("missing_field")
        );
        assert!(feedback.candidates[0]
            .compiler_reported_sites
            .iter()
            .any(|site| site.contains("src/lib.rs")));
        assert!(feedback.candidates[0]
            .extra_candidate_sites
            .iter()
            .any(|site| site.contains("tests/hidden.rs")));
    }
}
