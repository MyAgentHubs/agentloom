//! t12-img 第三轮 opus 审 P2-1 回归（阻断级）：`--image` 挂在首条 user goal（head）
//! 上，`build_candidate` 对 head 不剥图；openai/gpt、anthropic/claude 家族此前在
//! `default_context_tokens` 里没登记窗口 → 落 16384 猜测预算 → 3 张图（4800 token）
//! 加系统/任务/地形头必爆 → `context_budget_exhausted`，0 次 HTTP 请求，`run_solo`
//! 直接 `NeedsDecision`——把本 feature 在最常见 vision provider 上彻底打死。
//! 修法：(a) openai/gpt=128k、anthropic/claude=200k 窗口登记，与 `default_supports_images`
//! 判 true 的家族表对齐；(c) 落默认猜测预算时图片不计入预算（不能拿猜的数毙掉用户
//! 明确要发的附件）。真 `OpenAiCompatibleProvider` + wiremock 抓真实请求数，不是纸上推理。

use std::sync::{Arc, Mutex};

use myagent::events::OutputMode;
use myagent::image::ImageBlock;
use myagent::orchestrator::{run_solo, ControlInputKind, RunOptions, RunOutcome};
use myagent::provider::openai_compatible::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use myagent::shell::PermissionPolicy;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn png_bytes(payload: &[u8]) -> Vec<u8> {
    let mut b = PNG_MAGIC.to_vec();
    b.extend_from_slice(payload);
    b
}

fn image_block(i: usize) -> ImageBlock {
    let bytes = png_bytes(format!("img-{i}").as_bytes());
    use base64::Engine;
    ImageBlock {
        media_type: "image/png".into(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        source_path: None,
        bytes: bytes.len(),
        sha256: None,
    }
}

/// 每次收到请求就计数 + 回一个立即完成的最终文本响应（一轮跑完，不需要工具循环）。
struct CountingResponder(Arc<Mutex<usize>>);

impl Respond for CountingResponder {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        *self.0.lock().unwrap() += 1;
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(
                "data: {\"choices\":[{\"delta\":{\"content\":\"OK done.\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
    }
}

fn run_opts(ws: &std::path::Path, run_id: &str, images: Vec<ImageBlock>) -> RunOptions {
    RunOptions {
        prompt: "look at these screenshots".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "openai".into(),
        model: "gpt-4o".into(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        permission: PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        extra_read_roots: Vec::new(),
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: false,
        disallowed_tools: Default::default(),
        memory_enabled: false,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 6,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
        images,
    }
}

/// 复刻 opus 审报告 P2-1 的最小差分探针：真 provider config 里的 `context_tokens`
/// 与生产路径一致——来自 `config::default_context_tokens(provider_id, model)`（CLI 在
/// `provider_config_with_model` 里正是这样算的），不是随手写的常量。
async fn run_with_n_images(n: usize, provider_id: &str, model: &str) -> (RunOutcome, usize) {
    let server = MockServer::start().await;
    let count = Arc::new(Mutex::new(0usize));
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(CountingResponder(count.clone()))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    let images: Vec<ImageBlock> = (0..n).map(image_block).collect();
    let ctx = myagent::config::default_context_tokens(provider_id, model);
    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: provider_id.into(),
        api_key: "sk-test".into(),
        base_url: format!("{}/v1", server.uri()),
        model: model.into(),
        timeout_secs: 5,
        native_search_enabled: false,
        context_tokens: ctx,
        ..Default::default()
    })
    .unwrap();

    let run_id = format!("img_budget_{provider_id}_{n}");
    let result = run_solo(provider, run_opts(ws.path(), &run_id, images))
        .await
        .unwrap();
    let requests = *count.lock().unwrap();
    (result.outcome, requests)
}

#[tokio::test]
async fn openai_gpt4o_three_images_does_not_exhaust_budget_up_front() {
    let (outcome, requests) = run_with_n_images(3, "openai", "gpt-4o").await;
    assert!(
        requests >= 1,
        "3 张图不该在发第一个请求前就被预算闸拦死（outcome={outcome:?}）"
    );
    assert_eq!(outcome, RunOutcome::Completed);
}

#[tokio::test]
async fn openai_gpt4o_eight_images_does_not_exhaust_budget_up_front() {
    let (outcome, requests) = run_with_n_images(8, "openai", "gpt-4o").await;
    assert!(
        requests >= 1,
        "8 张图不该在发第一个请求前就被预算闸拦死（outcome={outcome:?}）"
    );
    assert_eq!(outcome, RunOutcome::Completed);
}

#[tokio::test]
async fn glm_5_2_eight_images_still_completes() {
    // glm-5.2 在 model_registry 里本就有登记窗口（1_000_000）——回归对照组：这条本该
    // 修前修后都绿，用来证明本次修复没有反过来伤到已经登记窗口的家族。
    let (outcome, requests) = run_with_n_images(8, "glm", "glm-5.2").await;
    assert!(requests >= 1);
    assert_eq!(outcome, RunOutcome::Completed);
}
