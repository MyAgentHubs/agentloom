use std::cell::Cell;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ValueEnum;
use serde_json::{json, Value};

use crate::control::{ControlCommand, ControlRecv, ControlSource};
use crate::error::{HarnessError, Result};
use crate::events::EventRecorder;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PermissionPolicy {
    Ask,
    Allow,
    Deny,
}

/// Contract policy for whether agent-authored verifiable check_cmd needs approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ContractPolicy {
    Ask,
    TrustUser,
    TrustAll,
}

/// 工具门对一次工具调用的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Approved,
    Rejected { reason: RejectReason },
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    UserRejected,
    ApprovalUnavailable,
}

pub struct GuardrailRequest<'a> {
    pub tool: &'a str,
    pub summary: String,
    pub cwd: &'a Path,
    pub write_paths: &'a [PathBuf],
    pub trusted: bool,
}

pub struct Guardrails {
    workspace: PathBuf,
    policy: PermissionPolicy,
    interactive: bool,
    always: Cell<bool>,
    contract_always: Cell<bool>,
    task_scope: std::cell::RefCell<Option<crate::plan::write_audit::TaskScope>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskAction {
    Approve,
    Reject,
    Always,
    Stop,
    /// 空输入（直接回车）或看不懂的键——不当任何决定，调用方应重新提示（不误拒手滑）。
    Unknown,
}

pub fn parse_ask_answer(input: &str) -> AskAction {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => AskAction::Approve,
        "n" | "no" => AskAction::Reject,
        "a" | "always" => AskAction::Always,
        "s" | "stop" => AskAction::Stop,
        _ => AskAction::Unknown,
    }
}

enum AskOutcome {
    Approved,
    Rejected { reason: Option<&'static str> },
    Interrupted,
}

/// 实时闸对一次写入的范围裁决（C2）。HardDeny=真安全/红线（硬挡）·Advisory=白名单外（软放行）。
enum ScopeGateVerdict {
    Allowed,
    /// 红线 / 逃工作区 / 路径无法规范化（fail-closed）——硬挡。
    HardDeny(String),
    /// 仅「超出 files_scope 白名单」——放行 + 由编排层注入软提示·携规范化路径。
    Advisory(String),
}

impl Guardrails {
    pub fn new(workspace: impl Into<PathBuf>, policy: PermissionPolicy, interactive: bool) -> Self {
        Self {
            workspace: workspace.into(),
            policy,
            interactive,
            always: Cell::new(false),
            contract_always: Cell::new(false),
            task_scope: std::cell::RefCell::new(None),
        }
    }

    /// 接上这一趟任务的写入边界（run_plan 透传·spec §4.5 实时硬挡）。
    pub fn with_task_scope(self, scope: crate::plan::write_audit::TaskScope) -> Self {
        *self.task_scope.borrow_mut() = Some(scope);
        self
    }

    /// C3：把模型经 propose_scope_change(kind=scope, paths=[..]) 申报的文件并进实时白名单·后续越界提示对它们消音。
    /// 只动实时闸的活 scope；**跑完审计仍用任务原始 scope**（人按完整 diff 复核扩张·模型不能自我授权绕过审计）。
    /// 用声明 scope 校验器规范化（拒绝绝对/`..`/glob/保留段）·去重·返回真正并入的规范化路径。
    pub fn extend_files_scope(&self, paths: &[String]) -> Vec<String> {
        let mut guard = self.task_scope.borrow_mut();
        let Some(scope) = guard.as_mut() else {
            return Vec::new();
        };
        let mut added = Vec::new();
        for raw in paths {
            if let Ok(norm) = crate::plan::paths::normalize_scope_path(raw) {
                if !scope.files_scope.contains(&norm) {
                    scope.files_scope.push(norm.clone());
                    added.push(norm);
                }
            }
        }
        added
    }

    /// 写入目标对任务边界的分型裁决（实时·C2）。
    /// 安全相关环节（canonicalize/逃逸/无法规范化）一律保守 HardDeny（绝不 fail-open）；
    /// 只有「白名单外」一种判 Advisory（软放行）。
    fn check_task_scope(
        &self,
        path: &Path,
        scope: &crate::plan::write_audit::TaskScope,
    ) -> ScopeGateVerdict {
        let Ok(workspace) = self.workspace.canonicalize() else {
            return ScopeGateVerdict::HardDeny("workspace canonicalize 失败·保守拒".to_string());
        };
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        };
        let resolved = crate::tools::fs_read::canonicalize_lenient(&candidate);
        // 危险配置文件硬拒：字面 + symlink 解析两形态都查（防 workspace 内软链偷写 .git）
        if crate::safety::dangerous_paths::path_hits_dangerous_config(&candidate)
            || crate::safety::dangerous_paths::path_hits_dangerous_config(&resolved)
        {
            return ScopeGateVerdict::HardDeny(format!(
                "写入危险配置/启动文件·内置默认拒：{}",
                resolved.to_string_lossy()
            ));
        }
        let Ok(rel) = resolved.strip_prefix(&workspace) else {
            return ScopeGateVerdict::HardDeny(format!(
                "写入目标在 workspace 外·保守拒：{}",
                resolved.to_string_lossy()
            ));
        };
        let Some(norm) = crate::plan::paths::normalize_observed_path(&rel.to_string_lossy()) else {
            return ScopeGateVerdict::HardDeny(format!(
                "写入路径无法规范化·保守拒：{}",
                rel.to_string_lossy()
            ));
        };
        match crate::plan::write_audit::scope_violation_kind(&norm, scope) {
            crate::plan::write_audit::ScopeOutcome::InScope => ScopeGateVerdict::Allowed,
            crate::plan::write_audit::ScopeOutcome::Forbidden(reason) => {
                ScopeGateVerdict::HardDeny(reason)
            }
            crate::plan::write_audit::ScopeOutcome::OutOfAllowlist(_) => {
                ScopeGateVerdict::Advisory(norm)
            }
        }
    }

    /// 「白名单外·已放行」的规范化写入路径（供编排层注入软提示）。
    /// **不含** forbidden/逃逸——那些在 gate 里直接 HardDeny。无 task_scope → 空。
    pub fn scope_advisory_paths(&self, write_paths: &[PathBuf]) -> Vec<String> {
        let guard = self.task_scope.borrow();
        let Some(scope) = guard.as_ref() else {
            return Vec::new();
        };
        write_paths
            .iter()
            .filter_map(|p| match self.check_task_scope(p, scope) {
                ScopeGateVerdict::Advisory(path) => Some(path),
                _ => None,
            })
            .collect()
    }

    pub fn policy(&self) -> PermissionPolicy {
        self.policy
    }

    /// 决策通道是否可用（非消费·C2）：**与 gate 的弹问条件 `interactive && is_terminal` 一致**
    /// （codex F3）OR sidecar approval 通道仍连着。都不在（批跑 Sentinel / sidecar 已 EOF /
    /// Human-但-管道 stdin）→ 不可用 → 治理提议改 deny-and-continue。
    pub fn decision_channel_available(&self, control: &dyn crate::control::ControlSource) -> bool {
        (self.interactive && io::stdin().is_terminal()) || control.approval_channel_available()
    }

    pub fn always_used(&self) -> bool {
        self.always.get()
    }

    pub fn gate(
        &self,
        recorder: &mut EventRecorder,
        control: &mut dyn ControlSource,
        approval_id: &str,
        req: &GuardrailRequest,
    ) -> Result<GateDecision> {
        for p in req.write_paths {
            self.ensure_in_workspace(p)?;
        }

        if let Some(scope) = self.task_scope.borrow().as_ref() {
            for p in req.write_paths {
                match self.check_task_scope(p, scope) {
                    // 白名单外 → 软放行（提示由编排层注入）；范围内 → 放行
                    ScopeGateVerdict::Allowed | ScopeGateVerdict::Advisory(_) => {}
                    // 红线 / 逃逸 / 无法规范化 → 硬挡（真安全·不软化）
                    ScopeGateVerdict::HardDeny(reason) => {
                        return Err(HarnessError::PermissionDenied(format!(
                            "out of task scope: {reason}"
                        )));
                    }
                }
            }
        }

        recorder.emit(
            "approval.requested",
            json!({
                "approval_id": approval_id,
                "tool": req.tool,
                "summary": req.summary,
                "command": req.summary,
                "cwd": req.cwd.to_string_lossy(),
                "policy": format!("{:?}", self.policy).to_ascii_lowercase(),
                "write_paths": req.write_paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            }),
        )?;

        let outcome = match self.policy {
            PermissionPolicy::Deny => AskOutcome::Rejected { reason: None },
            _ if req.trusted => AskOutcome::Approved,
            PermissionPolicy::Allow => AskOutcome::Approved,
            PermissionPolicy::Ask => self.resolve_ask(control, approval_id, req)?,
        };

        match outcome {
            AskOutcome::Approved => {
                recorder.emit(
                    "approval.resolved",
                    json!({
                        "approval_id": approval_id,
                        "decision": "approved",
                    }),
                )?;
                Ok(GateDecision::Approved)
            }
            AskOutcome::Rejected { reason } => {
                let mut payload = json!({
                    "approval_id": approval_id,
                    "decision": "rejected",
                });
                if let Some(reason) = reason {
                    if let Value::Object(ref mut object) = payload {
                        object.insert("reason".to_string(), json!(reason));
                    }
                }
                recorder.emit("approval.resolved", payload)?;
                let reason = match reason {
                    Some("channel_closed") => RejectReason::ApprovalUnavailable,
                    _ => RejectReason::UserRejected,
                };
                Ok(GateDecision::Rejected { reason })
            }
            AskOutcome::Interrupted => Ok(GateDecision::Interrupted),
        }
    }

    pub fn gate_contract(
        &self,
        recorder: &mut EventRecorder,
        control: &mut dyn ControlSource,
        contract_policy: ContractPolicy,
        approval_id: &str,
        proposal_id: &str,
        req: &GuardrailRequest,
    ) -> Result<GateDecision> {
        recorder.emit(
            "approval.requested",
            json!({
                "approval_id": approval_id,
                "proposal_id": proposal_id,
                "request_kind": "criterion",
                "tool": req.tool,
                "summary": req.summary,
                "command": req.summary,
                "cwd": req.cwd.to_string_lossy(),
                "policy": format!("{:?}", contract_policy).to_ascii_lowercase(),
                "write_paths": [],
            }),
        )?;

        let outcome = match contract_policy {
            ContractPolicy::TrustAll => AskOutcome::Approved,
            ContractPolicy::Ask | ContractPolicy::TrustUser => {
                self.resolve_ask_with(&self.contract_always, control, approval_id, req)?
            }
        };

        match outcome {
            AskOutcome::Approved => {
                recorder.emit(
                    "approval.resolved",
                    json!({
                        "approval_id": approval_id,
                        "proposal_id": proposal_id,
                        "request_kind": "criterion",
                        "decision": "approved",
                    }),
                )?;
                Ok(GateDecision::Approved)
            }
            AskOutcome::Rejected { reason } => {
                let mut payload = json!({
                    "approval_id": approval_id,
                    "proposal_id": proposal_id,
                    "request_kind": "criterion",
                    "decision": "rejected",
                });
                if let Some(reason) = reason {
                    if let Value::Object(ref mut object) = payload {
                        object.insert("reason".to_string(), json!(reason));
                    }
                }
                recorder.emit("approval.resolved", payload)?;
                let reason = match reason {
                    Some("channel_closed") => RejectReason::ApprovalUnavailable,
                    _ => RejectReason::UserRejected,
                };
                Ok(GateDecision::Rejected { reason })
            }
            AskOutcome::Interrupted => Ok(GateDecision::Interrupted),
        }
    }

    fn resolve_ask(
        &self,
        control: &mut dyn ControlSource,
        approval_id: &str,
        req: &GuardrailRequest,
    ) -> Result<AskOutcome> {
        self.resolve_ask_with(&self.always, control, approval_id, req)
    }

    fn resolve_ask_with(
        &self,
        always: &Cell<bool>,
        control: &mut dyn ControlSource,
        approval_id: &str,
        req: &GuardrailRequest,
    ) -> Result<AskOutcome> {
        if always.get() {
            return Ok(AskOutcome::Approved);
        }

        if self.interactive && io::stdin().is_terminal() {
            eprintln!();
            eprintln!("Allow {} ?", req.tool);
            eprintln!("  {}", req.summary);
            eprintln!("  cwd: {}", req.cwd.to_string_lossy());
            loop {
                eprint!("[y]es / [n]o / [a]lways / [s]top: ");
                io::stderr().flush()?;
                let mut answer = String::new();
                let bytes = io::stdin().read_line(&mut answer)?;
                if bytes == 0 {
                    // EOF：没法再问，保守拒（fail-closed），别陷死循环。
                    return Ok(AskOutcome::Rejected { reason: None });
                }
                match parse_ask_answer(&answer) {
                    AskAction::Approve => return Ok(AskOutcome::Approved),
                    AskAction::Always => {
                        always.set(true);
                        return Ok(AskOutcome::Approved);
                    }
                    AskAction::Stop => return Ok(AskOutcome::Interrupted),
                    AskAction::Reject => return Ok(AskOutcome::Rejected { reason: None }),
                    // 空输入/未知键：不当决定，重新提示（手滑不误拒）。
                    AskAction::Unknown => {
                        eprintln!("  请输入 y / n / a / s");
                        continue;
                    }
                }
            }
        }

        loop {
            match control.recv_approval(Duration::from_millis(200)) {
                ControlRecv::Command(ControlCommand::Approve {
                    approval_id: id, ..
                }) if id == approval_id => {
                    return Ok(AskOutcome::Approved);
                }
                ControlRecv::Command(ControlCommand::Reject {
                    approval_id: id, ..
                }) if id == approval_id => {
                    return Ok(AskOutcome::Rejected { reason: None });
                }
                ControlRecv::Command(_) => continue,
                ControlRecv::Timeout => {
                    if matches!(
                        control.poll(),
                        Some(ControlCommand::Stop { .. } | ControlCommand::Pause { .. })
                    ) {
                        return Ok(AskOutcome::Interrupted);
                    }
                    continue;
                }
                ControlRecv::Closed => {
                    return Ok(AskOutcome::Rejected {
                        reason: Some("channel_closed"),
                    });
                }
            }
        }
    }

    pub fn ensure_in_workspace(&self, path: &Path) -> Result<()> {
        let workspace = self.workspace.canonicalize()?;
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        };
        let resolved = crate::tools::fs_read::canonicalize_lenient(&candidate);
        if !resolved.starts_with(&workspace) {
            return Err(HarnessError::PermissionDenied(format!(
                "write target outside workspace: {}",
                resolved.to_string_lossy()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ControlCommand, QueueControlSource};
    use crate::events::{EventRecorder, OutputMode};

    fn rec(dir: &std::path::Path) -> EventRecorder {
        EventRecorder::new("r", None, None, &dir.join("e.jsonl"), OutputMode::Silent).unwrap()
    }

    struct AvailableSource;
    impl crate::control::ControlSource for AvailableSource {
        fn poll(&mut self) -> Option<ControlCommand> {
            None
        }
        fn approval_channel_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn decision_channel_available_tracks_live_sidecar() {
        // codex F3：predicate 的交互分支是 `interactive && is_terminal()`·is_terminal 在测试环境
        // 不确定（cargo test 下 stdin 通常非 TTY）→ 只确定性测「sidecar 通道」这一侧·交互+真终端
        // 这侧靠真实 TTY / 集成验。
        let dir = tempfile::tempdir().unwrap();
        // 非交互 + 无通道 → 不可用
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        assert!(!g.decision_channel_available(&QueueControlSource::new(vec![])));
        // 非交互 + 活 sidecar → 可用
        assert!(g.decision_channel_available(&AvailableSource));
        // 交互也不破坏 sidecar 分支（OR 短路）
        let g_i = Guardrails::new(dir.path(), PermissionPolicy::Ask, true);
        assert!(g_i.decision_channel_available(&AvailableSource));
    }

    #[test]
    fn parse_ask_answer_maps_all_options() {
        assert_eq!(parse_ask_answer("y"), AskAction::Approve);
        assert_eq!(parse_ask_answer("yes"), AskAction::Approve);
        assert_eq!(parse_ask_answer("Y"), AskAction::Approve);
        assert_eq!(parse_ask_answer("n"), AskAction::Reject);
        assert_eq!(parse_ask_answer("no"), AskAction::Reject);
        assert_eq!(parse_ask_answer("a"), AskAction::Always);
        assert_eq!(parse_ask_answer("always"), AskAction::Always);
        assert_eq!(parse_ask_answer("s"), AskAction::Stop);
        assert_eq!(parse_ask_answer("stop"), AskAction::Stop);
        assert_eq!(parse_ask_answer(""), AskAction::Unknown);
        assert_eq!(parse_ask_answer("xyz"), AskAction::Unknown);
    }

    #[test]
    fn always_flag_short_circuits_subsequent_gates_to_approved() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        g.always.set(true);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn contract_always_does_not_leak_to_tool_gate() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        g.contract_always.set(true);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };

        assert!(matches!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Rejected { .. }
        ));
        assert!(!g.always_used());
    }

    #[test]
    fn tool_always_does_not_leak_to_contract_gate() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        g.always.set(true);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "propose_criterion",
            summary: "tests pass".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };

        assert!(matches!(
            g.gate_contract(&mut r, &mut c, ContractPolicy::Ask, "ap_1", "prop_1", &req)
                .unwrap(),
            GateDecision::Rejected { .. }
        ));
    }

    #[test]
    fn allow_policy_returns_approved() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "shell_exec",
            summary: "ls".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn ask_consumes_control_approve_returns_approved() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![ControlCommand::Approve {
            run_id: "r".into(),
            approval_id: "ap_1".into(),
        }]);
        let req = GuardrailRequest {
            tool: "shell_exec",
            summary: "ls".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn ask_reject_returns_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![ControlCommand::Reject {
            run_id: "r".into(),
            approval_id: "ap_1".into(),
        }]);
        let req = GuardrailRequest {
            tool: "shell_exec",
            summary: "ls".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Rejected {
                reason: RejectReason::UserRejected
            }
        );
    }

    #[test]
    fn ask_without_decision_fails_closed_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "shell_exec",
            summary: "ls".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Rejected {
                reason: RejectReason::ApprovalUnavailable
            }
        );

        let events = std::fs::read_to_string(journal).unwrap();
        assert!(events.contains("\"type\":\"approval.resolved\""));
        assert!(events.contains("\"decision\":\"rejected\""));
        assert!(events.contains("\"reason\":\"channel_closed\""));
    }

    #[test]
    fn deny_policy_returns_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Deny, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "shell_exec",
            summary: "ls".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Rejected {
                reason: RejectReason::UserRejected
            }
        );
    }

    #[test]
    fn gate_mcp_gate_deny_rejects_even_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Deny, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "mcp__github__create_issue",
            summary: "github · create_issue · {}".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: true,
        };

        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Rejected {
                reason: RejectReason::UserRejected
            }
        );
    }

    #[test]
    fn gate_mcp_gate_deny_rejects_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Deny, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "mcp__github__create_issue",
            summary: "github · create_issue · {}".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };

        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Rejected {
                reason: RejectReason::UserRejected
            }
        );
    }

    #[test]
    fn gate_mcp_gate_allow_trusted_approved() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "mcp__github__create_issue",
            summary: "github · create_issue · {}".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: true,
        };

        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn gate_mcp_gate_ask_trusted_auto_approved() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "mcp__github__create_issue",
            summary: "github · create_issue · {}".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: true,
        };

        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn gate_mcp_gate_ask_untrusted_uses_approval() {
        let dir = tempfile::tempdir().unwrap();
        let g = Guardrails::new(dir.path(), PermissionPolicy::Ask, false);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![ControlCommand::Approve {
            run_id: "r".into(),
            approval_id: "ap_1".into(),
        }]);
        let req = GuardrailRequest {
            tool: "mcp__github__create_issue",
            summary: "github · create_issue · {}".into(),
            cwd: dir.path(),
            write_paths: &[],
            trusted: false,
        };

        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn task_scope_out_of_allowlist_is_now_allowed_not_denied() {
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let out = dir.path().join("src/b.rs");
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: std::slice::from_ref(&out),
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
        assert_eq!(
            g.scope_advisory_paths(std::slice::from_ref(&out)),
            vec!["src/b.rs".to_string()]
        );
    }

    #[test]
    fn extend_files_scope_adds_normalizes_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let added = g.extend_files_scope(&[
            "./src//new.rs".into(),
            "../escape.rs".into(),
            "src/a.rs".into(),
        ]);
        assert_eq!(added, vec!["src/new.rs".to_string()]);
        let out = dir.path().join("src/new.rs");
        assert!(g
            .scope_advisory_paths(std::slice::from_ref(&out))
            .is_empty());
    }

    #[test]
    fn task_scope_allows_in_scope_write() {
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: Vec::new(),
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: &[dir.path().join("src/a.rs")],
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn task_scope_forbidden_still_hard_denied() {
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec!["src".into()],
            forbidden_scope: vec!["src/secret.rs".into()],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: &[dir.path().join("src/secret.rs")],
            trusted: false,
        };
        assert!(matches!(
            g.gate(&mut r, &mut c, "ap_1", &req),
            Err(crate::error::HarnessError::PermissionDenied(_))
        ));
    }

    #[test]
    fn task_scope_glob_named_out_of_allowlist_is_advisory_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let out = dir.path().join("evil[1].rs");
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: std::slice::from_ref(&out),
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
        assert_eq!(
            g.scope_advisory_paths(std::slice::from_ref(&out)),
            vec!["evil[1].rs".to_string()]
        );
    }

    #[test]
    fn task_scope_allows_in_scope_write_no_advisory() {
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let out = dir.path().join("src/a.rs");
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: std::slice::from_ref(&out),
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
        assert!(g
            .scope_advisory_paths(std::slice::from_ref(&out))
            .is_empty());
    }

    #[test]
    fn dangerous_config_write_to_dotgit_is_hard_denied() {
        let dir = tempfile::tempdir().unwrap();
        // files_scope 显式把 .git/config 放进白名单（真 in-scope）——危险配置即使在 scope 内也 HardDeny。
        // 用具体路径而非 "."（codex skeptic 挑出："." 不经 scope 归一化、并不真表示整 workspace、证不到 in-scope）。
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec![".git/config".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let out = dir.path().join(".git/config");
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: std::slice::from_ref(&out),
            trusted: false,
        };
        assert!(matches!(
            g.gate(&mut r, &mut c, "ap_1", &req),
            Err(crate::error::HarnessError::PermissionDenied(_))
        ));
    }

    #[test]
    fn dangerous_config_write_to_bashrc_is_hard_denied() {
        let dir = tempfile::tempdir().unwrap();
        // .bashrc 显式 in-scope，仍 HardDeny（同上·具体路径才真证 in-scope）。
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec![".bashrc".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let out = dir.path().join(".bashrc");
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: std::slice::from_ref(&out),
            trusted: false,
        };
        assert!(matches!(
            g.gate(&mut r, &mut c, "ap_1", &req),
            Err(crate::error::HarnessError::PermissionDenied(_))
        ));
    }

    #[test]
    fn dangerous_config_normal_src_file_not_denied_by_this_rule() {
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec!["src/foo.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        let out = dir.path().join("src/foo.rs");
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: std::slice::from_ref(&out),
            trusted: false,
        };
        assert_eq!(
            g.gate(&mut r, &mut c, "ap_1", &req).unwrap(),
            GateDecision::Approved
        );
    }

    #[test]
    fn dangerous_config_symlink_to_dotgit_is_hard_denied() {
        let dir = tempfile::tempdir().unwrap();
        // 在 workspace 内建 .git 目录和 config 文件，然后建软链 evil → .git
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), b"dummy").unwrap();
        std::os::unix::fs::symlink(dir.path().join(".git"), dir.path().join("evil")).unwrap();

        let scope = crate::plan::write_audit::TaskScope {
            files_scope: vec![".".into()],
            forbidden_scope: vec![],
            crate_roots: vec![],
        };
        let g = Guardrails::new(dir.path(), PermissionPolicy::Allow, false).with_task_scope(scope);
        let mut r = rec(dir.path());
        let mut c = QueueControlSource::new(vec![]);
        // 写 evil/config：字面路径不含 .git，但 symlink 解析后命中 .git
        let out = dir.path().join("evil/config");
        let req = GuardrailRequest {
            tool: "fs_write",
            summary: "w".into(),
            cwd: dir.path(),
            write_paths: std::slice::from_ref(&out),
            trusted: false,
        };
        assert!(matches!(
            g.gate(&mut r, &mut c, "ap_1", &req),
            Err(crate::error::HarnessError::PermissionDenied(_))
        ));
    }
}
