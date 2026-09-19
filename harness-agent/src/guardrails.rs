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
    /// 用声明 scope 校验器规范化（拒绝绝对/`..`/glob/保留段）·去重·返回（真正并入的规范化路径，
    /// 已在 scope 里的规范化路径，逐条被拒原因）——
    // Callers must relay this honestly: an empty `added` is not success on its own — a path
    // already in scope is not a rejection either (the agent CAN already write it), so it goes
    // into `already_in_scope`, not `rejected`. Only paths that truly cannot be merged (bad
    // shape, or no task scope at all) belong in `rejected` with a reason.
    pub(crate) fn extend_files_scope(
        &self,
        paths: &[String],
    ) -> crate::orchestrator::scope_change_result::ScopeExtension {
        use crate::orchestrator::scope_change_result::ScopeExtension;
        let mut guard = self.task_scope.borrow_mut();
        let Some(scope) = guard.as_mut() else {
            let rejected = paths
                .iter()
                .map(|p| {
                    (
                        p.clone(),
                        "no task scope in this run; scope extension only applies to planned tasks"
                            .to_string(),
                    )
                })
                .collect();
            return ScopeExtension {
                added: Vec::new(),
                already_in_scope: Vec::new(),
                rejected,
            };
        };
        let mut added = Vec::new();
        let mut already_in_scope = Vec::new();
        let mut rejected = Vec::new();
        for raw in paths {
            match crate::plan::paths::normalize_scope_path(raw) {
                Ok(norm) => {
                    if scope.files_scope.contains(&norm) {
                        already_in_scope.push(norm);
                    } else {
                        scope.files_scope.push(norm.clone());
                        added.push(norm);
                    }
                }
                Err(reason) => rejected.push((raw.clone(), reason)),
            }
        }
        ScopeExtension {
            added,
            already_in_scope,
            rejected,
        }
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
mod tests;
