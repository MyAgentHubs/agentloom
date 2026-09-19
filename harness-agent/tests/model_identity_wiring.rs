//! 续 53.17.x 返工刀·真机证伪回归钉。
//!
//! 上一刀（commit 937f7520）在 entry.rs 里挂了
//! `inject_model_identity(&mut messages, &options.model)`（实现见
//! src/orchestrator/prompt.rs），让借壳跑的 GLM/DeepSeek 等模型能报真实身份、不
//! 冒认底层驱动方。真机测试时怀疑 `RunOptions.model` 在 `myagent run` 路径上一路
//! 空到 entry.rs，导致注入变空转。
//!
//! 本刀实勘结论（详见 commit message / 交接报告）：cli.rs::run_with_provider
//! （`run` 子命令 cli.rs:637 与 `shell` 交互命令 cli.rs:1052 共用同一个函数）在把
//! `RunOptions` 交给 entry.rs 之前，早就会用 `config::provider_config_with_model`
//! 解析出的真实模型名回填 `options.model`（mock 分支 cli.rs:767、真实 provider 分支
//! cli.rs:775）——这行回填自 2026-06-07 起就存在，比这次的身份注入功能早了两个多
//! 月，不是本刀引入也不需要本刀再修。`config::provider_config_with_model` 自身的
//! override/env/默认解析已有 config.rs 的单元测试钉住（见
//! `model_override_takes_highest_priority` 等）。
//!
//! 这里补的是中间缺的那一段回归覆盖：entry.rs 实际调用的同一条链路
//! （`myagent::orchestrator::run_solo`）在拿到 cli.rs 回填后的 `RunOptions.model`
//! 时，是否真的把身份行注入进了发给 provider 的 system 消息——覆盖三种下游语义：
//! 1) 显式 `--model` 覆盖解析出的真名；
//! 2) 没传 `--model`、由 provider 默认解析出的真名（`config::default_model`）；
//! 3) 真无模型可解析（commit 937f7520 原话：部分 resume 构造期 model 尚未知）时，
//!    不留空尾巴。

use std::sync::{Arc, Mutex};

use myagent::provider::{ChatMessage, ProviderCapabilities, ProviderClient, ProviderResponse};

struct MessageCaptor {
    seen: Arc<Mutex<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl ProviderClient for MessageCaptor {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        *self.seen.lock().unwrap() = messages.to_vec();
        Ok(ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "mock".into(),
            model_id: "mock-model".into(),
            supports_streaming: false,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: false,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }
}

fn opts_for(ws: &std::path::Path, model: &str) -> myagent::orchestrator::RunOptions {
    myagent::orchestrator::RunOptions {
        prompt: "who are you".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "mock".into(),
        model: model.into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
        control_input: myagent::orchestrator::ControlInputKind::Sentinel,
        permission: myagent::shell::PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        extra_read_roots: Vec::new(),
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: false,
        disallowed_tools: Default::default(),
        memory_enabled: false,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 1,
        run_id: None,
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 1,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
        images: Vec::new(),
    }
}

#[tokio::test]
async fn run_solo_injects_real_model_identity_from_explicit_override() {
    let ws = tempfile::tempdir().unwrap();
    // 模拟 cli.rs:775 用 `--model deepseek-v4-pro` 解析出的 config.model 回填后的
    // RunOptions.model。
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = MessageCaptor { seen: seen.clone() };

    myagent::orchestrator::run_solo(provider, opts_for(ws.path(), "deepseek-v4-pro"))
        .await
        .unwrap();

    let messages = seen.lock().unwrap().clone();
    let system = messages[0].content.as_deref().unwrap();
    assert!(system.contains("Underlying model: deepseek-v4-pro"));
}

#[tokio::test]
async fn run_solo_injects_real_model_identity_from_provider_default() {
    let ws = tempfile::tempdir().unwrap();
    // 没传 --model 时 cli.rs:775 用的是 config::provider_config_with_model 兜底到的
    // default_model(provider)——不是空字符串，是一个真实、非空的模型名。直接调用
    // 同一个纯函数，而不是手写字面量，这样 default_model 的实现一变这个测试就跟着
    // 转，不会锈成假绿。
    let resolved_default = myagent::config::default_model("deepseek");
    assert!(!resolved_default.trim().is_empty());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = MessageCaptor { seen: seen.clone() };

    myagent::orchestrator::run_solo(provider, opts_for(ws.path(), &resolved_default))
        .await
        .unwrap();

    let messages = seen.lock().unwrap().clone();
    let system = messages[0].content.as_deref().unwrap();
    assert!(system.contains(&format!("Underlying model: {resolved_default}")));
}

#[tokio::test]
async fn run_solo_omits_model_identity_when_model_unresolved() {
    let ws = tempfile::tempdir().unwrap();
    // 真无模型可解析的路径（commit 937f7520 原话：「部分 resume 路径构造期 model 尚
    // 未知」）——不注入，不留空尾巴。
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = MessageCaptor { seen: seen.clone() };

    myagent::orchestrator::run_solo(provider, opts_for(ws.path(), ""))
        .await
        .unwrap();

    let messages = seen.lock().unwrap().clone();
    let system = messages[0].content.as_deref().unwrap();
    assert!(!system.contains("Underlying model"));
}
