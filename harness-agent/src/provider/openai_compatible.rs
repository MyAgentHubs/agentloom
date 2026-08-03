use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::events::EventRecorder;
use crate::provider::retry::{backoff_delay, is_retryable_status, RetryPolicy};
use crate::provider::{
    ChatMessage, FinishReason, FunctionCall, ProviderCapabilities, ProviderClient,
    ProviderResponse, ToolCall,
};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SamplingParams {
    pub top_p: Option<f64>,
    pub do_sample: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    pub provider_id: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub timeout_secs: u64,
    pub temperature: Option<f64>,
    pub sampling: SamplingParams,
    /// 本次 run 的联网策略（原始值·next_turn 据此判原生搜索开不开）。
    pub network: crate::goal::NetworkPolicy,
    /// 用户是否允许原生服务端搜索（原始值·默认 true·--native-search off 置 false）。
    pub native_search_enabled: bool,
    /// 主模型重试耗尽后若配了 fallback model，自动换模型再发一轮。
    pub fallback_model: Option<String>,
    pub context_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleConfig,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()?;
        Ok(Self { config, client })
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// 表内模型按 registry；未知模型回落旧 model 名启发式。
    fn supports_reasoning(&self) -> bool {
        crate::model_registry::lookup(&self.config.provider_id, &self.config.model)
            .map(|spec| spec.supports_reasoning)
            .unwrap_or_else(|| {
                let m = self.config.model.to_ascii_lowercase();
                m.contains("reasoner") || m.contains("thinking") || m.contains("deepseek")
            })
    }

    /// 拼请求体（含 reasoning 过滤）。with_native=false 时绝不注入原生搜索。
    fn build_body(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        with_native: bool,
    ) -> Result<Value> {
        let mut body = json!({ "model": self.config.model, "stream": true });
        if let Some(t) = self.config.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(max_tokens) = self.config.output_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(top_p) = self.config.sampling.top_p {
            body["top_p"] = json!(top_p);
        }
        if let Some(do_sample) = self.config.sampling.do_sample {
            body["do_sample"] = json!(do_sample);
        }
        let wire_messages = if self.supports_reasoning() {
            serde_json::to_value(messages)?
        } else {
            let stripped: Vec<ChatMessage> = messages
                .iter()
                .map(|m| {
                    let mut c = m.clone();
                    c.reasoning_content = None;
                    c
                })
                .collect();
            serde_json::to_value(stripped)?
        };
        body["messages"] = wire_messages;
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
            body["tool_choice"] = json!("auto");
        }
        // 原生服务端搜索：Active 时按家族注入（Kimi 在 Task 8 单独处理回声）。
        use crate::provider::native_search::{
            apply_glm, apply_qwen, disable_thinking, kimi_tool_def, native_search_state,
            provider_family, NativeSearchState, ProviderFamily,
        };
        let family = provider_family(&self.config.provider_id);
        let state = native_search_state(
            family.has_native_search(),
            self.config.network,
            self.config.native_search_enabled,
        );
        if with_native && state == NativeSearchState::Active {
            match family {
                ProviderFamily::Glm => apply_glm(&mut body, &self.config.base_url),
                ProviderFamily::Qwen => apply_qwen(&mut body),
                ProviderFamily::Kimi => {
                    match body.get_mut("tools").and_then(|t| t.as_array_mut()) {
                        Some(arr) => arr.push(kimi_tool_def()),
                        None => body["tools"] = json!([kimi_tool_def()]),
                    }
                    disable_thinking(&mut body);
                }
                ProviderFamily::Generic => {}
            }
        }
        Ok(body)
    }

    /// 发请求（不检查状态码·让调用方按 status 决定降级）。
    /// 内置重试：对传输错误和可重试的 HTTP 状态（429 / 5xx）自动退避重试。
    /// 外层 post() 会在主模型重试耗尽后自动换 fallback_model 再发一轮。
    async fn post_once(
        &self,
        body: &Value,
        events: &mut EventRecorder,
    ) -> Result<reqwest::Response> {
        let policy = RetryPolicy::default();
        for attempt in 0..=policy.max_retries {
            let result = self
                .client
                .post(self.endpoint())
                .bearer_auth(&self.config.api_key)
                .json(body)
                .send()
                .await;
            match result {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() || !is_retryable_status(status.as_u16()) {
                        return Ok(response);
                    }
                    // Retryable status (429 / 5xx): sleep and retry if attempts remain.
                    if attempt < policy.max_retries {
                        events.emit(
                            "provider.warning",
                            json!({
                                "warning": "provider_retry",
                                "attempt": attempt,
                                "status": status.as_u16(),
                            }),
                        )?;
                        tokio::time::sleep(backoff_delay(&policy, attempt)).await;
                        continue;
                    }
                    // Exhausted retries; return the last response.
                    return Ok(response);
                }
                Err(e) => {
                    if attempt < policy.max_retries {
                        events.emit(
                            "provider.warning",
                            json!({
                                "warning": "provider_retry",
                                "attempt": attempt,
                                "status": Value::Null,
                            }),
                        )?;
                        tokio::time::sleep(backoff_delay(&policy, attempt)).await;
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        unreachable!("loop always returns");
    }

    /// 外层 post：先发主模型，若满足 fallback 条件且配了 fallback_model，换模型再发一轮。
    async fn post(&self, body: &Value, events: &mut EventRecorder) -> Result<reqwest::Response> {
        let primary = self.post_once(body, events).await;
        let (is_err, status) = match &primary {
            Err(_) => (true, None),
            Ok(r) => (false, Some(r.status().as_u16())),
        };
        if crate::provider::retry::warrants_fallback(is_err, status) {
            if let Some(fb) = self.config.fallback_model.clone() {
                events.emit(
                    "provider.warning",
                    serde_json::json!({
                        "warning": "fallback_model",
                        "from": self.config.model,
                        "to": fb,
                    }),
                )?;
                let mut fb_body = body.clone();
                fb_body["model"] = serde_json::json!(fb);
                return self.post_once(&fb_body, events).await;
            }
        }
        primary
    }

    /// 发请求；若这轮原生搜索 Active 且 provider 返 4xx，则 warning + 不带原生重发一次。
    /// 返回最终要 collect 的 Response。Kimi 每轮 post 也走这个（T8）。
    async fn post_native_or_degrade(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        events: &mut EventRecorder,
    ) -> Result<reqwest::Response> {
        use crate::provider::native_search::{
            native_search_state, provider_family, NativeSearchState,
        };
        let family = provider_family(&self.config.provider_id);
        let active = native_search_state(
            family.has_native_search(),
            self.config.network,
            self.config.native_search_enabled,
        ) == NativeSearchState::Active;
        let response = self
            .post(&self.build_body(messages, tools, true)?, events)
            .await?;
        if active && response.status().is_client_error() {
            events.emit(
                "provider.warning",
                json!({
                    "warning": "native_search_degraded",
                    "status": response.status().as_u16(),
                }),
            )?;
            return self
                .post(&self.build_body(messages, tools, false)?, events)
                .await;
        }
        Ok(response)
    }

    /// 检查状态 + 流式收集成 ProviderResponse。emit=false 时不发 text/reasoning delta（Kimi 内层用）。
    async fn collect(
        &self,
        response: reqwest::Response,
        emit: bool,
        events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        if !response.status().is_success() {
            return provider_status_error(response).await;
        }
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_accumulators: BTreeMap<usize, ToolCallAccumulator> = BTreeMap::new();
        let mut buffer = Vec::new();
        let mut stream = response.bytes_stream();
        let mut done = false;
        let mut interrupted = false;
        let mut interrupt_err: Option<HarnessError> = None;
        let mut pending_usage = None;
        let mut finish_reason = None;
        while !done {
            let Some(chunk) = stream.next().await else {
                break;
            };
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    interrupt_err = Some(e.into()); // 存下传输错·待收尾判空
                    interrupted = true;
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some((pos, separator_len)) = find_sse_separator(&buffer) {
                let event_bytes = buffer[..pos].to_vec();
                buffer.drain(..pos + separator_len);
                let event = match String::from_utf8(event_bytes) {
                    Ok(event) => event,
                    Err(err) => {
                        events.emit(
                            "provider.warning",
                            json!({
                                "warning": "invalid_utf8_sse_event",
                                "error": err.to_string(),
                            }),
                        )?;
                        continue;
                    }
                };
                for line in event.lines() {
                    let line = line.trim_end_matches('\r');
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        done = true;
                        break;
                    }
                    let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
                        Ok(chunk) => chunk,
                        Err(err) => {
                            events.emit(
                                "provider.warning",
                                json!({
                                    "warning": "invalid_sse_json",
                                    "error": err.to_string(),
                                }),
                            )?;
                            continue;
                        }
                    };
                    apply_chunk(
                        chunk,
                        &mut content,
                        &mut reasoning,
                        &mut tool_accumulators,
                        emit,
                        events,
                        &mut pending_usage,
                        &mut finish_reason,
                    )?;
                    if done {
                        break;
                    }
                }
            }
        }

        let (response, dropped) = finalize_provider_response(
            content,
            reasoning,
            tool_accumulators,
            interrupted,
            finish_reason,
        );
        if interrupted {
            // 过滤完整个空 → 维持现状报错·绝不返回空成功轮(BLOCK 修法)
            if response_is_empty(&response) {
                return Err(interrupt_err.expect("interrupted implies stored error"));
            }
            // 确有可留内容 → 此刻才 emit：先 stream_interrupted，再逐个被丢 tool
            events.emit(
                "provider.warning",
                json!({
                    "warning": "stream_interrupted",
                    "error": interrupt_err.map(|e| e.to_string()).unwrap_or_default(),
                }),
            )?;
            for name in dropped {
                events.emit(
                    "provider.warning",
                    json!({
                        "warning": "dropped_incomplete_tool_call",
                        "name": name,
                    }),
                )?;
            }
        }
        if let Some((input, output)) = pending_usage {
            events.record_llm_usage(input, output);
        }
        Ok(response)
    }

    async fn kimi_turn_with_echo(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        use crate::provider::native_search::is_kimi_web_search;
        const MAX_ECHO: usize = 3;
        let mut history: Vec<ChatMessage> = messages.to_vec();
        for _ in 0..=MAX_ECHO {
            // 经共用 helper：注入 Kimi $web_search·若 4xx 则 warning+不带原生重发(降级=这轮不回声)。
            let resp = self.post_native_or_degrade(&history, tools, events).await?;
            let mut collected = self.collect(resp, false, events).await?; // 内层不 emit
            let has_web = collected
                .tool_calls
                .iter()
                .any(|c| is_kimi_web_search(&c.function.name));
            let all_web = !collected.tool_calls.is_empty()
                && collected
                    .tool_calls
                    .iter()
                    .all(|c| is_kimi_web_search(&c.function.name));
            if all_web {
                // 纯 $web_search：回声往返。assistant(带【全部】tool_calls) 进史一次，
                // 然后【每个】$web_search 都 push 一条 tool_named（原样回传 arguments）——
                // 否则同轮多个 $web_search 时 tool 消息少于 tool_calls，Kimi 第二轮会 400。
                history.push(ChatMessage::assistant(
                    collected.text.clone(),
                    None,
                    collected.tool_calls.clone(),
                ));
                for call in collected
                    .tool_calls
                    .iter()
                    .filter(|c| is_kimi_web_search(&c.function.name))
                {
                    history.push(ChatMessage::tool_named(
                        call.id.clone(),
                        "$web_search",
                        call.function.arguments.clone(),
                    ));
                }
                continue;
            }
            if has_web {
                // 混了普通工具 + $web_search：必须先把 $web_search 从 tool_calls 滤掉再交回
                // orchestrator——否则 orchestrator 按 name 查 registry 找不到 $web_search →
                // 抛 "unsupported tool call: $web_search" → 整轮 Failed（spec §3.5 第 7 点 + 验收 3）。
                collected
                    .tool_calls
                    .retain(|c| !is_kimi_web_search(&c.function.name));
            }
            // 出最终文本 / 只剩普通工具调用：emit 一次（内层一直 emit=false）后返回交回 orchestrator。
            return self.emit_and_return(collected, events);
        }
        // 超上限仍在 $web_search → 不假完成，返回 Err（见下「Err 语义」说明）。
        Err(crate::error::HarnessError::Provider(
            "kimi web_search did not converge within echo limit".into(),
        ))
    }

    /// 内层一直 emit=false·最终一次性把内容/推理发出去（保证 UI/JSONL 拿到答案）。
    fn emit_and_return(
        &self,
        r: ProviderResponse,
        events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        if !r.reasoning.is_empty() {
            events.emit_reasoning_delta(&r.reasoning)?;
        }
        if !r.text.is_empty() {
            events.emit_text_delta(&r.text)?;
        }
        Ok(r)
    }
}

#[async_trait]
impl ProviderClient for OpenAiCompatibleProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        use crate::provider::native_search::{
            native_search_state, provider_family, NativeSearchState, ProviderFamily,
        };
        let family = provider_family(&self.config.provider_id);
        let active = native_search_state(
            family.has_native_search(),
            self.config.network,
            self.config.native_search_enabled,
        ) == NativeSearchState::Active;
        if family == ProviderFamily::Kimi && active {
            return self.kimi_turn_with_echo(messages, tools, events).await;
        }
        let response = self.post_native_or_degrade(messages, tools, events).await?;
        self.collect(response, true, events).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let spec = crate::model_registry::lookup(&self.config.provider_id, &self.config.model);
        ProviderCapabilities {
            provider_id: self.config.provider_id.clone(),
            model_id: self.config.model.clone(),
            supports_streaming: spec.map(|spec| spec.supports_streaming).unwrap_or(true),
            supports_reasoning_deltas: spec
                .map(|spec| spec.supports_reasoning_deltas)
                .unwrap_or_else(|| self.supports_reasoning()),
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: self.config.context_tokens,
            output_token_limit: self.config.output_tokens,
            server_side_search: crate::provider::native_search::provider_family(
                &self.config.provider_id,
            )
            .has_native_search(),
        }
    }
}

async fn provider_status_error<T>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = if body.trim().is_empty() {
        status_message(status)
    } else {
        format!("{}: {}", status_message(status), body)
    };
    Err(HarnessError::Provider(message))
}

fn status_message(status: StatusCode) -> String {
    format!("provider returned HTTP {status}")
}

fn find_sse_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    for index in 0..buffer.len().saturating_sub(1) {
        if buffer[index] == b'\n' && buffer[index + 1] == b'\n' {
            return Some((index, 2));
        }
        if index + 3 < buffer.len()
            && buffer[index] == b'\r'
            && buffer[index + 1] == b'\n'
            && buffer[index + 2] == b'\r'
            && buffer[index + 3] == b'\n'
        {
            return Some((index, 4));
        }
    }
    None
}

fn apply_chunk(
    chunk: ChatCompletionChunk,
    content: &mut String,
    reasoning: &mut String,
    tool_accumulators: &mut BTreeMap<usize, ToolCallAccumulator>,
    emit: bool,
    events: &mut EventRecorder,
    latest_usage: &mut Option<(u64, u64)>,
    finish_reason: &mut Option<FinishReason>,
) -> Result<()> {
    if let Some(usage) = chunk.usage {
        if usage.prompt_tokens.is_some() || usage.completion_tokens.is_some() {
            *latest_usage = Some((
                usage.prompt_tokens.unwrap_or(0),
                usage.completion_tokens.unwrap_or(0),
            ));
        }
    }
    for choice in chunk.choices {
        if let Some(reason) = choice.finish_reason {
            *finish_reason = Some(FinishReason::from_openai(&reason));
        }
        if let Some(delta_content) = choice.delta.content {
            content.push_str(&delta_content);
            if emit {
                events.emit_text_delta(&delta_content)?;
            }
        }
        if let Some(delta_reasoning) = choice.delta.reasoning_content.or(choice.delta.reasoning) {
            reasoning.push_str(&delta_reasoning);
            if emit {
                events.emit_reasoning_delta(&delta_reasoning)?;
            }
        }
        for tool_delta in choice.delta.tool_calls.unwrap_or_default() {
            let accumulator = tool_accumulators.entry(tool_delta.index).or_default();
            if let Some(id) = tool_delta.id {
                accumulator.id = id;
            }
            if let Some(call_type) = tool_delta.call_type {
                accumulator.call_type = call_type;
            }
            if let Some(function) = tool_delta.function {
                if let Some(name) = function.name {
                    accumulator.name = name;
                }
                if let Some(arguments) = function.arguments {
                    accumulator.arguments.push_str(&arguments);
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
}

#[derive(Debug, Deserialize)]
struct ChunkUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct ToolCallAccumulator {
    id: String,
    call_type: String,
    name: String,
    arguments: String,
}

/// 中断收尾时判一个 tool 的 arguments 是否可安全保留：非空且是合法 JSON 对象。
/// 挡掉半截(`{"a":`) / scalar/数组(null/[]/"x"/5) / 空串——这些丢掉、靠模型下一轮重发。
/// `{}` 算完整(用户拍定·风险低·危险工具缺必填字段会被工具层自己挡)。
fn tool_args_complete(args: &str) -> bool {
    !args.is_empty() && serde_json::from_str::<serde_json::Map<String, Value>>(args).is_ok()
}

/// 把累积的 content/reasoning/tool 收尾成 ProviderResponse。纯函数·不碰 events。
/// interrupted=false：与改造前收尾逐字节等价(只过滤 name 空)。
/// interrupted=true：在 name 非空之外多套 tool_args_complete 过滤，被丢且 name 非空的收进返回的 Vec(供 collect emit warning)。
fn finalize_provider_response(
    content: String,
    reasoning: String,
    accs: BTreeMap<usize, ToolCallAccumulator>,
    interrupted: bool,
    finish_reason: Option<FinishReason>,
) -> (ProviderResponse, Vec<String>) {
    let mut dropped = Vec::new();
    let tool_calls = accs
        .into_values()
        .filter(|acc| !acc.name.is_empty())
        .filter(|acc| {
            if interrupted && !tool_args_complete(&acc.arguments) {
                dropped.push(acc.name.clone());
                false
            } else {
                true
            }
        })
        .enumerate()
        .map(|(index, acc)| ToolCall {
            id: if acc.id.is_empty() {
                format!("call_{index}")
            } else {
                acc.id
            },
            call_type: if acc.call_type.is_empty() {
                "function".to_string()
            } else {
                acc.call_type
            },
            function: FunctionCall {
                name: acc.name,
                arguments: acc.arguments,
            },
        })
        .collect();
    (
        ProviderResponse {
            text: content,
            reasoning,
            tool_calls,
            finish_reason,
        },
        dropped,
    )
}

/// 收尾后响应是否整个空(无 text、无 reasoning、无幸存 tool)。
/// 中断路径据此决定：空 → 返回原始传输错误(不产空成功轮)；非空 → 优雅收尾。
fn response_is_empty(resp: &ProviderResponse) -> bool {
    resp.text.is_empty() && resp.reasoning.is_empty() && resp.tool_calls.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_for_base_url(
        provider_id: &str,
        model: &str,
        base_url: &str,
    ) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
            provider_id: provider_id.into(),
            api_key: "sk-test".into(),
            base_url: base_url.into(),
            model: model.into(),
            timeout_secs: 5,
            temperature: None,
            sampling: Default::default(),
            network: crate::goal::NetworkPolicy::On,
            native_search_enabled: true,
            fallback_model: None,
            context_tokens: None,
            output_tokens: None,
        })
        .unwrap()
    }

    fn provider_for(provider_id: &str, model: &str) -> OpenAiCompatibleProvider {
        provider_for_base_url(provider_id, model, "https://example.test/v1")
    }

    fn acc(name: &str, args: &str) -> ToolCallAccumulator {
        ToolCallAccumulator {
            name: name.into(),
            arguments: args.into(),
            ..Default::default()
        }
    }

    fn representative_tools() -> Vec<Value> {
        vec![
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "fs_read",
                    "description": "Read a UTF-8 file from the workspace.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }
                }
            }),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "apply_patch",
                    "description": "Apply a unified patch to workspace files.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "patch": { "type": "string" }
                        },
                        "required": ["patch"]
                    }
                }
            }),
        ]
    }

    fn assert_no_internal_markers(blob: &str) {
        for marker in ["cmd:", "contains:", "judge:", "check_cmd[", "cmd="] {
            assert!(
                !blob.contains(marker),
                "assembled request leaked internal marker: {marker}"
            );
        }
    }

    #[test]
    fn assembled_request_no_markers_across_criterion_syntaxes() {
        for spec in [
            "cmd: cargo test",
            "contains:OK: cargo run",
            "judge: looks correct",
        ] {
            let goal = crate::goal::GoalState::new(
                "x",
                crate::goal::parse_criteria(&[spec.into()]).unwrap(),
            );
            let frame = crate::context_builder::render_state_frame(
                &goal,
                &crate::run_progress::RunProgress::default(),
                1,
                40,
                crate::adaptive_safety_net::SafetyLevel::Free,
                &crate::working_ledger::WorkingLedger::default(),
                true,
            );
            let wire = crate::context_builder::build_wire_messages(
                &[ChatMessage::user("please work")],
                &frame,
            );
            let provider = provider_for("openai-compatible", "test-model");
            let body = provider
                .build_body(&wire, &representative_tools(), false)
                .unwrap();
            let blob = serde_json::to_string(&body).unwrap();
            assert_no_internal_markers(&blob);
            if spec == "cmd: cargo test" {
                assert!(blob.contains("验收检查"));
                assert!(blob.contains("cargo test"));
            }
        }
    }

    #[test]
    fn build_body_uses_config_base_url_for_glm_native_search_shape() {
        let provider = provider_for_base_url("glm", "glm-4.5", "https://api.z.ai/api/paas/v4");
        let body = provider
            .build_body(&[ChatMessage::user("search")], &[], true)
            .unwrap();
        let web_search = &body["tools"][0]["web_search"];
        assert_eq!(web_search["enable"], json!(true));
        assert_eq!(web_search["search_engine"], json!("search_pro_jina"));
        assert_eq!(web_search["search_result"], json!(true));
    }

    #[test]
    fn tool_args_complete_accepts_json_objects_only() {
        assert!(tool_args_complete("{}"));
        assert!(tool_args_complete("{\"a\":1}"));
        assert!(!tool_args_complete("{\"a\":")); // 半截
        assert!(!tool_args_complete("null"));
        assert!(!tool_args_complete("[]"));
        assert!(!tool_args_complete("\"x\""));
        assert!(!tool_args_complete("5"));
        assert!(!tool_args_complete("")); // 空串·中断路径保守丢
    }

    #[test]
    fn apply_chunk_keeps_latest_non_null_usage() {
        let mut usage = None;
        let mut events = EventRecorder::with_sinks("run_test", None, None, vec![]);
        for data in [
            r#"{"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2}}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4}}"#,
        ] {
            apply_chunk(
                serde_json::from_str(data).unwrap(),
                &mut String::new(),
                &mut String::new(),
                &mut BTreeMap::new(),
                false,
                &mut events,
                &mut usage,
                &mut None,
            )
            .unwrap();
        }
        assert_eq!(usage, Some((3, 4)));
    }

    #[test]
    fn apply_chunk_preserves_length_finish_reason() {
        let mut finish_reason = None;
        let mut events = EventRecorder::with_sinks("run_test", None, None, vec![]);
        let chunk =
            serde_json::from_str(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#).unwrap();

        apply_chunk(
            chunk,
            &mut String::new(),
            &mut String::new(),
            &mut BTreeMap::new(),
            false,
            &mut events,
            &mut None,
            &mut finish_reason,
        )
        .unwrap();

        assert_eq!(finish_reason, Some(crate::provider::FinishReason::Length));
        let (response, _) = finalize_provider_response(
            String::new(),
            "truncated".into(),
            BTreeMap::new(),
            false,
            finish_reason,
        );
        assert_eq!(
            response.finish_reason,
            Some(crate::provider::FinishReason::Length)
        );
    }
    #[test]
    fn finalize_normal_keeps_all_named_tools_byte_for_byte() {
        let mut accs = BTreeMap::new();
        accs.insert(1usize, acc("b", "{\"y\":")); // 先插 key 1
        accs.insert(0usize, acc("a", "{\"x\":1}")); // 后插 key 0·验证按 key 排序
        let (resp, dropped) =
            finalize_provider_response("text".into(), "r".into(), accs, false, None);
        assert_eq!(resp.text, "text");
        assert_eq!(resp.reasoning, "r");
        assert_eq!(resp.tool_calls.len(), 2);
        // 按 BTreeMap key 顺序：先 a(key0) 后 b(key1)
        assert_eq!(resp.tool_calls[0].id, "call_0");
        assert_eq!(resp.tool_calls[0].call_type, "function");
        assert_eq!(resp.tool_calls[0].function.name, "a");
        assert_eq!(resp.tool_calls[1].id, "call_1");
        assert_eq!(resp.tool_calls[1].function.name, "b");
        assert_eq!(resp.tool_calls[1].function.arguments, "{\"y\":"); // 半截·正常路径也保留
        assert!(dropped.is_empty());
    }

    #[test]
    fn finalize_interrupted_drops_incomplete_args_and_reports_names() {
        let mut accs = BTreeMap::new();
        accs.insert(0usize, acc("good", "{\"x\":1}"));
        accs.insert(1usize, acc("half", "{\"y\":"));
        let (resp, dropped) =
            finalize_provider_response("text".into(), String::new(), accs, true, None);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].function.name, "good");
        assert_eq!(dropped, vec!["half".to_string()]);
    }

    #[test]
    fn finalize_interrupted_drops_empty_string_and_null_args() {
        let mut accs = BTreeMap::new();
        accs.insert(0usize, acc("name_only", "")); // name 非空 + args 空串
        accs.insert(1usize, acc("bad", "null")); // name 非空 + args=null
        let (resp, dropped) =
            finalize_provider_response("text".into(), String::new(), accs, true, None);
        assert!(resp.tool_calls.is_empty());
        assert_eq!(dropped, vec!["name_only".to_string(), "bad".to_string()]);
    }

    #[test]
    fn finalize_interrupted_filtered_empty_response() {
        let mut accs = BTreeMap::new();
        accs.insert(0usize, acc("half", "{\"y\":")); // 半截 → 丢
        let (resp, dropped) =
            finalize_provider_response(String::new(), String::new(), accs, true, None);
        // 过滤后整个空（collect 据此 return 原始 Err·此处直接断字段）
        assert!(resp.text.is_empty());
        assert!(resp.reasoning.is_empty());
        assert!(resp.tool_calls.is_empty());
        assert_eq!(dropped, vec!["half".to_string()]);
    }

    #[test]
    fn finalize_empty_shell_accumulator_not_partial() {
        let mut accs = BTreeMap::new();
        accs.insert(0usize, ToolCallAccumulator::default()); // index-only·name 空·args 空
        let (resp, dropped) =
            finalize_provider_response(String::new(), String::new(), accs, true, None);
        assert!(resp.tool_calls.is_empty());
        assert!(resp.text.is_empty() && resp.reasoning.is_empty());
        assert!(dropped.is_empty()); // name 空·从不算"被丢的 tool"
    }

    #[test]
    fn response_is_empty_true_only_when_all_empty() {
        let empty = ProviderResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: None,
        };
        assert!(response_is_empty(&empty));
        let with_text = ProviderResponse {
            text: "hi".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: None,
        };
        assert!(!response_is_empty(&with_text));
        let with_reasoning = ProviderResponse {
            text: String::new(),
            reasoning: "r".into(),
            tool_calls: vec![],
            finish_reason: None,
        };
        assert!(!response_is_empty(&with_reasoning));
        let with_tool = ProviderResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_0".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "x".into(),
                    arguments: "{}".into(),
                },
            }],
            finish_reason: None,
        };
        assert!(!response_is_empty(&with_tool));
    }

    #[test]
    fn reasoning_table_driven_deepseek_unchanged() {
        let p = provider_for("deepseek", "deepseek-reasoner");
        assert!(p.supports_reasoning());
        let p2 = provider_for("kimi", "moonshot-v1-128k");
        assert!(!p2.supports_reasoning());
        let p3 = provider_for("kimi", "moonshot-v1-8k-thinking");
        assert!(!p3.supports_reasoning());
    }

    #[test]
    fn capabilities_reasoning_deltas_and_streaming_from_table() {
        let caps = provider_for("deepseek", "deepseek-reasoner").capabilities();
        assert!(caps.supports_reasoning_deltas);
        assert!(caps.supports_streaming);
    }

    #[test]
    fn capabilities_reports_configured_context_limits() {
        let configured = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
            provider_id: "openai-compatible".into(),
            api_key: "sk-test".into(),
            base_url: "https://example.test/v1".into(),
            model: "test-model".into(),
            timeout_secs: 5,
            temperature: None,
            sampling: Default::default(),
            network: crate::goal::NetworkPolicy::On,
            native_search_enabled: true,
            fallback_model: None,
            context_tokens: Some(8192),
            output_tokens: Some(1024),
        })
        .unwrap();
        let caps = configured.capabilities();
        assert_eq!(caps.max_context_tokens, Some(8192));
        assert_eq!(caps.output_token_limit, Some(1024));

        let unspecified = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
            provider_id: "openai-compatible".into(),
            api_key: "sk-test".into(),
            base_url: "https://example.test/v1".into(),
            model: "test-model".into(),
            timeout_secs: 5,
            temperature: None,
            sampling: Default::default(),
            network: crate::goal::NetworkPolicy::On,
            native_search_enabled: true,
            fallback_model: None,
            context_tokens: None,
            output_tokens: None,
        })
        .unwrap();
        let caps = unspecified.capabilities();
        assert_eq!(caps.max_context_tokens, None);
        assert_eq!(caps.output_token_limit, None);
    }
}
