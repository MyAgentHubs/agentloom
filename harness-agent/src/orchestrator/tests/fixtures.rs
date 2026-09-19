//! 测试夹具：`task_test_run_options` 从 `tests.rs` 拆出——避免该文件继续超出文件
//! 大小门禁的基线历史额度（旧棘轮已列入退役计划，权威门禁看仓根
//! `scripts/check_file_size.py`）。`use super::*` 沿用父模块（`tests`）已导入的名字。

use super::*;

pub(super) fn task_test_run_options(
    ws: &std::path::Path,
    jr: &std::path::Path,
    run_id: &str,
    criteria: Vec<crate::goal::Criterion>,
) -> RunOptions {
    RunOptions {
        prompt: "do the task".into(),
        workspace: ws.to_path_buf(),
        provider_id: "mock".into(),
        model: "mock".into(),
        client_session_id: None,
        output_mode: crate::events::OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        evidence_gate: EvidenceGate::Off,
        permission: crate::shell::PermissionPolicy::Allow,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        extra_read_roots: Vec::new(),
        fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
        native_search_enabled: false,
        disallowed_tools: std::collections::BTreeSet::new(),
        memory_enabled: false,
        search: crate::config::SearchChoice::Ddg,
        max_turns: 4,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria,
        contract_policy: crate::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 3,
        verify_reflex_debt: DEFAULT_VERIFY_EVERY,
        watchdog_repeat_threshold: DEFAULT_WATCHDOG_REPEAT,
        journal_root: jr.to_path_buf(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
        images: Vec::new(),
    }
}
