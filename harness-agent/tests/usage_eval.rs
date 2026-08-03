//! usage 透传考卷（冻结阅卷器·协议由配套内部评测程序文档规定）。
//!
//! 一题一个 #[tokio::test]，共 12 题（U1–U12·codex xhigh 独立审后按 REVISE 意见
//! 扩题：U10 第二发射点 engine_finalize、U11 blocked 负范围、U12 fallback）。
//! 全程离线：wiremock 供 SSE / JSON 夹具（形状锚定 2026-07-10 真 API 录音：
//! DeepSeek 流式 / zai anthropic 非流式，见配套的内部评测录音夹具），
//! 真 run_solo 端到端跑完，对 events.jsonl 里终态事件的 `usage` 荷载做硬断言。
//!
//! **冻结（never-edit）**：本文件与配套的内部评测程序文档同批封存，loop 内不许改。
//! 数字设计：题题互异且 input≠output（防 input/output 写反漏检·防「恰好对称碰过」）。

use std::time::Duration;

use std::collections::BTreeSet;

use serde_json::{json, Value};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use myagent::control::{ControlCommand, ControlRecv, ControlSource};
use myagent::events::OutputMode;
use myagent::orchestrator::{run_solo, run_solo_with_control, ControlInputKind, RunOptions};
use myagent::provider::anthropic_compatible::AnthropicProvider;
use myagent::provider::openai_compatible::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use myagent::shell::PermissionPolicy;

// ---------- 共用夹具与断言工具（冻结·与题目同权） ----------

/// OpenAI 兼容 SSE 体：每 chunk 一段 `data: {json}` + 末尾 `data: [DONE]`。
fn sse(chunks: &[Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn sse_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

/// 角色前导 chunk（DeepSeek 真流第一段的形状）。
fn role_chunk() -> Value {
    json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]})
}

fn content_chunk(text: &str) -> Value {
    json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]})
}

/// DeepSeek 默认流真形状：usage 挂在 finish_reason:"stop" 的收尾 chunk 上
/// （录音 fixtures/recordings/deepseek-stream-default.sse.txt）。
fn finish_chunk_with_usage(prompt: u64, completion: u64) -> Value {
    json!({"choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop"}],
           "usage":{"prompt_tokens":prompt,"completion_tokens":completion,
                    "total_tokens":prompt+completion,
                    "prompt_tokens_details":{"cached_tokens":0},
                    "prompt_cache_hit_tokens":0,"prompt_cache_miss_tokens":prompt}})
}

fn finish_chunk_no_usage() -> Value {
    json!({"choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop"}]})
}

/// 单 chunk 完整 tool_call（任意工具·让 run 进下一轮）。
fn named_tool_call_chunk(call_id: &str, tool: &str, args: &Value) -> Value {
    json!({"choices":[{"index":0,"delta":{"tool_calls":[{
        "index":0,"id":call_id,"type":"function",
        "function":{"name":tool,"arguments":args.to_string()}
    }]},"finish_reason":null}]})
}

/// 单 chunk 完整 tool_call（shell_exec true·让 run 进下一轮）。
fn tool_call_chunk(call_id: &str) -> Value {
    named_tool_call_chunk(call_id, "shell_exec", &json!({"command":"true"}))
}

/// 带 usage 的最终文本响应（一次 LLM 调用）。
fn final_text_sse(text: &str, prompt: u64, completion: u64) -> String {
    sse(&[
        role_chunk(),
        content_chunk(text),
        finish_chunk_with_usage(prompt, completion),
    ])
}

/// 带 usage 的 tool_call 响应（一次 LLM 调用·run 会继续下一轮）。
fn tool_call_sse(call_id: &str, text: &str, prompt: u64, completion: u64) -> String {
    sse(&[
        role_chunk(),
        content_chunk(text),
        tool_call_chunk(call_id),
        finish_chunk_with_usage(prompt, completion),
    ])
}

fn openai_provider_with_fallback(
    base_url: &str,
    fallback_model: Option<&str>,
) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "deepseek".into(),
        api_key: "sk-test".into(),
        base_url: base_url.to_string(),
        model: "deepseek-v4-flash".into(),
        timeout_secs: 10,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: false,
        fallback_model: fallback_model.map(String::from),
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap()
}

fn openai_provider(base_url: &str) -> OpenAiCompatibleProvider {
    openai_provider_with_fallback(base_url, None)
}

fn anthropic_provider(base_url: &str) -> AnthropicProvider {
    AnthropicProvider::new(OpenAiCompatibleConfig {
        provider_id: "zai".into(),
        api_key: "sk-test".into(),
        base_url: base_url.to_string(),
        model: "glm-4.6".into(),
        timeout_secs: 10,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: false,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap()
}

fn run_opts(ws: &std::path::Path, run_id: &str) -> RunOptions {
    RunOptions {
        prompt: "please do the task".into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "deepseek".into(),
        model: "deepseek-v4-flash".into(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        permission: PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
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
    }
}

/// 顶层 JSON 字段 `stream == true` 匹配器（解析后核·防「子串包含」被嵌套同名字段绕过）。
struct StreamTrueTopLevel;

impl wiremock::Match for StreamTrueTopLevel {
    fn matches(&self, request: &wiremock::Request) -> bool {
        serde_json::from_slice::<Value>(&request.body)
            .map(|v| v["stream"] == json!(true))
            .unwrap_or(false)
    }
}

/// 终态事件冻结形状断言：payload 键集恰等于 expected（与顺序无关）。
fn assert_exact_keys(payload: &serde_json::Map<String, Value>, expected: &[&str], what: &str) {
    let got: BTreeSet<&str> = payload.keys().map(String::as_str).collect();
    let want: BTreeSet<&str> = expected.iter().copied().collect();
    assert_eq!(
        got, want,
        "{what} must keep its exact frozen shape: {payload:?}"
    );
}

fn journal_events(ws: &std::path::Path, run_id: &str) -> Vec<Value> {
    let path = ws
        .join(".myagenthubs/runs")
        .join(run_id)
        .join("events.jsonl");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read journal {}: {e}", path.display()));
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("journal line is JSON"))
        .collect()
}

fn events_of<'a>(events: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e["type"].as_str() == Some(event_type))
        .collect()
}

/// 恰一条 run.completed，且它是 journal 最后一条事件（CONTRACT 终态不变量），返回其 payload。
fn sole_completed_payload(events: &[Value]) -> Value {
    let completed = events_of(events, "run.completed");
    assert_eq!(completed.len(), 1, "expected exactly one run.completed");
    let last = events.last().expect("journal not empty");
    assert_eq!(
        last["type"].as_str(),
        Some("run.completed"),
        "terminal event must be the last journal line"
    );
    completed[0]["payload"].clone()
}

/// usage 荷载硬断言：存在、恰两键、u64 精确值。
fn assert_usage_exact(payload: &Value, input: u64, output: u64) {
    let usage = payload
        .get("usage")
        .unwrap_or_else(|| panic!("run.completed payload missing `usage`: {payload}"));
    let obj = usage.as_object().expect("usage must be a JSON object");
    assert_eq!(
        obj.keys().collect::<Vec<_>>(),
        vec!["input_tokens", "output_tokens"],
        "usage must contain exactly input_tokens/output_tokens (canonical shape): {usage}"
    );
    assert_eq!(
        usage["input_tokens"].as_u64(),
        Some(input),
        "input_tokens mismatch: {usage}"
    );
    assert_eq!(
        usage["output_tokens"].as_u64(),
        Some(output),
        "output_tokens mismatch: {usage}"
    );
}

// ---------- U1 单次调用·DeepSeek 默认流真形状（usage 挂 finish chunk） ----------

#[tokio::test]
async fn u1_single_call_stream_usage_on_finish_chunk() {
    let server = MockServer::start().await;
    // body 匹配器锁请求形状：生产路径只有流式（build_body 恒 "stream": true）——
    // 若实现把顶层 stream 改掉，本 mock 不再命中（wiremock 404）→ 本题必红。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(StreamTrueTopLevel)
        .respond_with(sse_response(final_text_sse("OK done.", 17, 5)))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(openai_provider(&server.uri()), run_opts(ws.path(), "u1"))
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u1");
    let payload = sole_completed_payload(&events);
    assert_eq!(payload["turns"].as_u64(), Some(1));
    assert_usage_exact(&payload, 17, 5);
}

// ---------- U2 多轮工具循环·累计而非覆盖 ----------

#[tokio::test]
async fn u2_multi_turn_usage_accumulates_across_calls() {
    let server = MockServer::start().await;
    // 三次 LLM 调用：11/3 + 23/7 + 41/13 = 75/23。末次(41,13)≠和：覆盖式实现必挂。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(tool_call_sse("call_u2_a", "step one", 11, 3)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(tool_call_sse("call_u2_b", "step two", 23, 7)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(final_text_sse("all done", 41, 13)))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(openai_provider(&server.uri()), run_opts(ws.path(), "u2"))
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u2");
    let payload = sole_completed_payload(&events);
    assert_eq!(payload["turns"].as_u64(), Some(3), "scripted 3 LLM turns");
    assert_usage_exact(&payload, 75, 23);
}

// ---------- U3 include_usage 规范流形状：中间 chunk usage:null + 末尾空 choices usage chunk ----------

#[tokio::test]
async fn u3_stream_null_usage_chunks_and_trailing_empty_choices_usage() {
    let server = MockServer::start().await;
    // OpenAI stream_options.include_usage 规范形状（DeepSeek include_usage 录音印证
    // 中间 chunk 带 "usage":null）：收尾后再来一个 choices 为空数组、只带 usage 的 chunk。
    let body = sse(&[
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}],"usage":null}),
        json!({"choices":[{"index":0,"delta":{"content":"OK"},"finish_reason":null}],"usage":null}),
        json!({"choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop"}],"usage":null}),
        json!({"choices":[],"usage":{"prompt_tokens":29,"completion_tokens":9,"total_tokens":38}}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(body))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(openai_provider(&server.uri()), run_opts(ws.path(), "u3"))
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u3");
    let payload = sole_completed_payload(&events);
    assert_usage_exact(&payload, 29, 9);
}

// ---------- U4 anthropic 非流路径（zai/GLM 真形状·多余字段须被忽略） ----------

#[tokio::test]
async fn u4_anthropic_nonstream_usage_captured() {
    let server = MockServer::start().await;
    // 锚定录音 fixtures/recordings/zai-anthropic-nonstream.json 的真形状
    // （含 cache_read_input_tokens / server_tool_use / service_tier 等多余字段）。
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_eval_u4","type":"message","role":"assistant","model":"glm-4.6",
            "content":[{"type":"text","text":"OK"}],
            "stop_reason":"end_turn","stop_sequence":null,
            "usage":{"input_tokens":31,"output_tokens":8,
                     "cache_read_input_tokens":0,
                     "server_tool_use":{"web_search_requests":0},
                     "service_tier":"standard"}
        })))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    let mut opts = run_opts(ws.path(), "u4");
    opts.provider_id = "zai".into();
    opts.model = "glm-4.6".into();
    run_solo(anthropic_provider(&server.uri()), opts)
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u4");
    let payload = sole_completed_payload(&events);
    assert_usage_exact(&payload, 31, 8);
}

// ---------- U5 provider 全程不报 usage → 诚实缺席（无 usage 键·不编 0） ----------

#[tokio::test]
async fn u5_no_usage_anywhere_means_field_absent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(sse(&[
            role_chunk(),
            content_chunk("step"),
            tool_call_chunk("call_u5"),
            finish_chunk_no_usage(),
        ])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(sse(&[
            role_chunk(),
            content_chunk("done without usage"),
            finish_chunk_no_usage(),
        ])))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(openai_provider(&server.uri()), run_opts(ws.path(), "u5"))
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u5");
    let payload = sole_completed_payload(&events);
    assert_eq!(
        payload["turns"].as_u64(),
        Some(2),
        "run itself must be intact"
    );
    assert!(
        !payload.as_object().unwrap().contains_key("usage"),
        "no call reported usage → `usage` must be entirely absent (no fabricated zeros): {payload}"
    );
}

// ---------- U6 混合：一次带 usage、一次不带 → 只求和已报的（不编造） ----------

#[tokio::test]
async fn u6_mixed_reporting_sums_only_reported_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(tool_call_sse("call_u6", "with usage", 19, 6)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(sse(&[
            role_chunk(),
            content_chunk("final, no usage"),
            finish_chunk_no_usage(),
        ])))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(openai_provider(&server.uri()), run_opts(ws.path(), "u6"))
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u6");
    let payload = sole_completed_payload(&events);
    assert_usage_exact(&payload, 19, 6);
}

// ---------- U7 中断锁死：已消耗不上报·run.interrupted 形状不变 ----------

struct StopAtSecondPoll {
    run_id: String,
    polls: usize,
}

impl ControlSource for StopAtSecondPoll {
    fn poll(&mut self) -> Option<ControlCommand> {
        self.polls += 1;
        if self.polls >= 2 {
            Some(ControlCommand::Stop {
                run_id: self.run_id.clone(),
            })
        } else {
            None
        }
    }
    fn recv_approval(&mut self, _timeout: Duration) -> ControlRecv {
        ControlRecv::Closed
    }
}

#[tokio::test]
async fn u7_interrupted_run_reports_no_usage_and_no_completed() {
    let server = MockServer::start().await;
    // 第 1 轮 LLM 调用真实消耗 (37,11) 并返回 tool_call；
    // 工具执行前的 poll 触发 stop → run.interrupted。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(tool_call_sse(
            "call_u7",
            "about to run tool",
            37,
            11,
        )))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo_with_control(
        openai_provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        run_opts(ws.path(), "u7"),
        Box::new(StopAtSecondPoll {
            run_id: "u7".into(),
            polls: 0,
        }),
    )
    .await
    .unwrap();

    let events = journal_events(ws.path(), "u7");
    // 前提自证：LLM 调用确实发生过（不然「没上报」是空转真空绿）。
    assert!(
        !events_of(&events, "agent.note.delta").is_empty(),
        "the LLM call must actually have happened before the interrupt"
    );
    assert!(
        events_of(&events, "run.completed").is_empty(),
        "interrupted run must not emit run.completed"
    );
    let interrupted = events_of(&events, "run.interrupted");
    assert_eq!(interrupted.len(), 1, "expected exactly one run.interrupted");
    let payload = interrupted[0]["payload"].as_object().unwrap();
    // CONTRACT: run.interrupted 是 Tier 1 stable（无 additive 豁免）——键集一个都不许多
    //（不只禁叫 "usage" 的键·换个名夹带 token 数一样违规）。
    assert_exact_keys(payload, &["resume_command", "step_id"], "run.interrupted");
}

// ---------- U8 形状与向后兼容：canonical 两键 + turns 保留 + 老消费者可解析 ----------

#[tokio::test]
async fn u8_payload_shape_canonical_and_backward_compatible() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(final_text_sse("shape check", 43, 15)))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(openai_provider(&server.uri()), run_opts(ws.path(), "u8"))
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u8");
    let payload = sole_completed_payload(&events);

    // canonical 形状：恰两键 + u64（assert_usage_exact 内含）；数值为 JSON 整数非字符串。
    assert_usage_exact(&payload, 43, 15);
    assert!(payload["usage"]["input_tokens"].is_u64());
    assert!(payload["usage"]["output_tokens"].is_u64());
    // 既有字段不动：turns 仍在。
    assert_eq!(payload["turns"].as_u64(), Some(1));
    // 不许夹带发明字段（用户拍过：只存 token 不存钱；total 冗余不进事件）。
    for forbidden in [
        "cost_usd",
        "total_tokens",
        "prompt_tokens",
        "completion_tokens",
    ] {
        assert!(
            payload.get(forbidden).is_none() && payload["usage"].get(forbidden).is_none(),
            "forbidden field `{forbidden}` leaked into run.completed: {payload}"
        );
    }
    // 老消费者（只认 turns 的 serde 结构）读新荷载不炸——可选字段向后兼容。
    #[derive(serde::Deserialize)]
    struct LegacyCompleted {
        turns: u64,
    }
    let legacy: LegacyCompleted =
        serde_json::from_value(payload.clone()).expect("legacy consumer must still parse");
    assert_eq!(legacy.turns, 1);
}

// ---------- U9 重试路径不重复计：500 → 退避重试成功·恰计一次 ----------

#[tokio::test]
async fn u9_retry_counts_usage_exactly_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream hiccup"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(final_text_sse("recovered", 53, 21)))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(openai_provider(&server.uri()), run_opts(ws.path(), "u9"))
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u9");
    // 前提自证：确实走过一次 provider_retry（wiremock 顺序真的先给了 500）。
    let retried = events_of(&events, "provider.warning")
        .iter()
        .any(|e| e["payload"]["warning"].as_str() == Some("provider_retry"));
    assert!(retried, "expected a provider_retry warning in the journal");
    let payload = sole_completed_payload(&events);
    // 双计（106,42）或漏计（缺席）都算错：恰好一次。
    assert_usage_exact(&payload, 53, 21);
}

// ---------- U10 第二发射点：engine_finalize（completion.rs try_finalize）也要带 usage ----------

#[tokio::test]
async fn u10_engine_finalize_path_carries_usage() {
    // 只在 run_loop.rs 主发射点组装 usage 的实现会漏掉这条路（codex 独立审 BLOCKER 2）。
    // 确定性触发链：已批准 cmd:true 标准 + verify_reflex_debt=1 →
    // 轮 1 fs_write 真编辑 → 反射验证全过 → completion gate 武装（收尾提示）→
    // 轮 2 非编辑工具（fs_read）→ ready_to_finalize → try_finalize →
    // run.completed{via:"engine_finalize"}。
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(sse(&[
            role_chunk(),
            content_chunk("write the note"),
            named_tool_call_chunk(
                "call_u10_write",
                "fs_write",
                &json!({"path":"note.txt","content":"hello\n"}),
            ),
            finish_chunk_with_usage(47, 19),
        ])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(sse(&[
            role_chunk(),
            content_chunk("read it back"),
            named_tool_call_chunk("call_u10_read", "fs_read", &json!({"path":"note.txt"})),
            finish_chunk_with_usage(61, 29),
        ])))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    let mut opts = run_opts(ws.path(), "u10");
    opts.criteria = myagent::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    opts.verify_reflex_debt = 1;
    run_solo(openai_provider(&server.uri()), opts)
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u10");
    let payload = sole_completed_payload(&events);
    // 自证走的确实是第二发射点（模型从没交最终文本·是引擎主动收尾）。
    assert_eq!(
        payload["via"].as_str(),
        Some("engine_finalize"),
        "this question must exercise the try_finalize emit site: {payload}"
    );
    assert_usage_exact(&payload, 108, 48); // 47+61 / 19+29
}

// ---------- U11 blocked 负范围：run.blocked 不带 usage·冻结形状键集不变 ----------

#[tokio::test]
async fn u11_blocked_run_reports_no_usage_and_keeps_frozen_shape() {
    // run.blocked 是 CONTRACT Tier 1 stable（无 additive 豁免）。
    // 「所有终态一律注入 usage」的实现必挂在这（codex 独立审 BLOCKER 3）。
    let server = MockServer::start().await;
    // 模型每轮都交带 usage 的最终文本，但标准 cmd:false 永不过 →
    // max_eval_attempts=1 耗尽 → run.blocked。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(sse_response(final_text_sse("done i think", 67, 31)))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    let mut opts = run_opts(ws.path(), "u11");
    opts.criteria = myagent::goal::parse_criteria(&["cmd: false".into()]).unwrap();
    opts.max_eval_attempts = 1;
    run_solo(openai_provider(&server.uri()), opts)
        .await
        .unwrap();

    let events = journal_events(ws.path(), "u11");
    // 前提自证：LLM 调用真的发生并报了 usage（不然「没上报」是真空绿）。
    assert!(
        !events_of(&events, "agent.note.delta").is_empty(),
        "LLM calls must actually have happened before blocking"
    );
    assert!(
        events_of(&events, "run.completed").is_empty(),
        "blocked run must not emit run.completed"
    );
    let blocked = events_of(&events, "run.blocked");
    assert_eq!(blocked.len(), 1, "expected exactly one run.blocked");
    let payload = blocked[0]["payload"].as_object().unwrap();
    assert_exact_keys(
        payload,
        &["attempts", "criteria", "reason", "turns"],
        "run.blocked",
    );
}

// ---------- U12 fallback 换模型：最终被采用那次响应的 usage 计入·恰一次 ----------

#[tokio::test]
async fn u12_fallback_model_usage_counted_once() {
    let server = MockServer::start().await;
    // 主模型恒 500（重试耗尽·退避 ~3.5s）→ 引擎换 fallback-model 重发 → 成功带 usage。
    // 用 body 里的 model 名区分两段 mock。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("deepseek-v4-flash"))
        .respond_with(ResponseTemplate::new(500).set_body_string("primary down"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("fallback-model"))
        .respond_with(sse_response(final_text_sse(
            "recovered on fallback",
            71,
            37,
        )))
        .mount(&server)
        .await;

    let ws = tempfile::tempdir().unwrap();
    run_solo(
        openai_provider_with_fallback(&server.uri(), Some("fallback-model")),
        run_opts(ws.path(), "u12"),
    )
    .await
    .unwrap();

    let events = journal_events(ws.path(), "u12");
    // 前提自证：确实发生了 fallback 换模型。
    let fell_back = events_of(&events, "provider.warning")
        .iter()
        .any(|e| e["payload"]["warning"].as_str() == Some("fallback_model"));
    assert!(
        fell_back,
        "expected a fallback_model warning in the journal"
    );
    let payload = sole_completed_payload(&events);
    assert_usage_exact(&payload, 71, 37);
}
