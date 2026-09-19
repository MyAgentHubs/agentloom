#![cfg(test)]

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
    let extension = g.extend_files_scope(&[
        "./src//new.rs".into(),
        "../escape.rs".into(),
        "src/a.rs".into(),
    ]);
    assert_eq!(extension.added, vec!["src/new.rs".to_string()]);
    // Already-in-scope is not a rejection: the agent can already write that path.
    assert_eq!(extension.already_in_scope, vec!["src/a.rs".to_string()]);
    assert_eq!(
        extension.rejected.len(),
        1,
        "only the escape is truly rejected: {:?}",
        extension.rejected
    );
    assert!(extension
        .rejected
        .iter()
        .any(|(p, r)| p == "../escape.rs" && r.contains("..")));
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
