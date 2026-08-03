use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::error::{HarnessError, Result};
use crate::events::EventRecorder;
use crate::provider::openai_compatible::OpenAiCompatibleConfig;
use crate::provider::retry::{backoff_delay, is_retryable_status, RetryPolicy};
use crate::provider::{
    ChatMessage, FinishReason, FunctionCall, ProviderCapabilities, ProviderClient,
    ProviderResponse, ToolCall,
};

/// canonical 历史 → Anthropic /v1/messages 请求体（纯函数·归一化严格交替 + 工具结果合并）。
/// 返回 Result：首条非-system 消息必须是 user（Anthropic 硬要求）·否则报错（codex BLOCKER1）。
pub(crate) fn build_anthropic_request(
    messages: &[ChatMessage],
    tools: &[Value],
    model: &str,
    max_tokens: u32,
    temperature: Option<f64>,
    top_p: Option<f64>,
) -> Result<Value> {
    let mut system = String::new();
    let mut out: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => {
                if let Some(c) = &msg.content {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(c);
                }
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = &msg.content {
                    if !text.is_empty() {
                        blocks.push(json!({"type":"text","text":text}));
                    }
                }
                if let Some(calls) = &msg.tool_calls {
                    for call in calls {
                        let input: Value = serde_json::from_str(&call.function.arguments)
                            .unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type":"tool_use","id":call.id,"name":call.function.name,"input":input
                        }));
                    }
                }
                // 空 assistant（无 text 无 tool_call）→ 跳过·绝不发空 text 块（codex BLOCKER2·Anthropic 拒空块）。
                if blocks.is_empty() {
                    continue;
                }
                // 合并相邻 assistant（防连续 assistant 破坏交替·codex HIGH3）。
                push_role_blocks(&mut out, "assistant", blocks);
            }
            "tool" => {
                let block = json!({
                    "type":"tool_result",
                    "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                    "content": msg.content.clone().unwrap_or_default(),
                });
                push_role_blocks(&mut out, "user", vec![block]);
            }
            _ => {
                // user（含未知 role 当 user）·空内容也建一个 text 块（user 块允许·只 assistant 空块被拒）
                let block = json!({"type":"text","text": msg.content.clone().unwrap_or_default()});
                push_role_blocks(&mut out, "user", vec![block]);
            }
        }
    }

    // 首条非-system 必须 user（codex BLOCKER1·严格交替前置）。
    if let Some(first) = out.first() {
        if first["role"] != json!("user") {
            return Err(HarnessError::InvalidConfig(
                "anthropic transcript must start with a user message (got assistant/tool first)"
                    .into(),
            ));
        }
    }

    let anthropic_tools: Vec<Value> = tools
        .iter()
        .filter_map(|t| {
            let f = t.get("function").unwrap_or(t);
            Some(json!({
                "name": f.get("name")?.clone(),
                "description": f.get("description").cloned().unwrap_or(Value::Null),
                "input_schema": f.get("parameters").cloned().unwrap_or(json!({"type":"object"})),
            }))
        })
        .collect();

    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("max_tokens".into(), json!(max_tokens));
    if let Some(temperature) = temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if !system.is_empty() {
        body.insert("system".into(), json!(system));
    }
    body.insert("messages".into(), json!(out));
    if !anthropic_tools.is_empty() {
        body.insert("tools".into(), json!(anthropic_tools));
    }
    Ok(Value::Object(body))
}

/// 把 blocks 追加到「当前同 role 消息」——若 out 末尾已是该 role 就并进其 content 数组·否则新开一条。
/// 同时保证 user/assistant 严格交替（连续同 role 自动合并·codex HIGH3）。
fn push_role_blocks(out: &mut Vec<Value>, role: &str, mut blocks: Vec<Value>) {
    if let Some(last) = out.last_mut() {
        if last["role"] == json!(role) {
            if let Some(arr) = last["content"].as_array_mut() {
                arr.append(&mut blocks);
                return;
            }
        }
    }
    out.push(json!({"role": role, "content": blocks}));
}

/// Anthropic 响应 → canonical ProviderResponse。content 缺失/非数组 → 报 provider 错（codex MEDIUM6·非静默空）。
pub(crate) fn parse_anthropic_response(body: &Value) -> Result<ProviderResponse> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            HarnessError::Provider(format!(
                "anthropic response missing `content` array: {body}"
            ))
        })?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            Some("thinking") | Some("redacted_thinking") => {
                if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                    reasoning.push_str(t);
                }
            }
            Some("tool_use") => {
                let id = b
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = b
                    .get("input")
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "{}".into());
                tool_calls.push(ToolCall {
                    id,
                    call_type: "function".into(),
                    function: FunctionCall { name, arguments },
                });
            }
            _ => {}
        }
    }
    Ok(ProviderResponse {
        text,
        reasoning,
        tool_calls,
        finish_reason: body
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(FinishReason::from_anthropic),
    })
}

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    config: OpenAiCompatibleConfig,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()?;
        Ok(Self { config, client })
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }

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
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(body)
                .send()
                .await;

            match result {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() || !is_retryable_status(status.as_u16()) {
                        return Ok(response);
                    }
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
}

#[async_trait]
impl ProviderClient for AnthropicProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let max_tokens = self.config.output_tokens.unwrap_or(4096);
        let body = build_anthropic_request(
            messages,
            tools,
            &self.config.model,
            max_tokens,
            self.config.temperature,
            self.config.sampling.top_p,
        )?;
        let response = self.post_once(&body, events).await?;
        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            return Err(HarnessError::Provider(format!(
                "provider returned HTTP {}: {}",
                status, body_text
            )));
        }
        let v: Value = response.json().await?;
        let input = v["usage"]["input_tokens"].as_u64();
        let output = v["usage"]["output_tokens"].as_u64();
        if input.is_some() || output.is_some() {
            events.record_llm_usage(input.unwrap_or(0), output.unwrap_or(0));
        }
        let resp = parse_anthropic_response(&v)?;
        if !resp.reasoning.is_empty() {
            events.emit_reasoning_delta(&resp.reasoning)?;
        }
        if !resp.text.is_empty() {
            events.emit_text_delta(&resp.text)?;
        }
        Ok(resp)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: self.config.provider_id.clone(),
            model_id: self.config.model.clone(),
            supports_streaming: false,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: self.config.context_tokens,
            output_token_limit: self.config.output_tokens,
            server_side_search: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, FunctionCall, ToolCall};
    use serde_json::{json, Value};

    #[tokio::test]
    async fn anthropic_capabilities_reports_shell_and_no_streaming() {
        use crate::provider::ProviderClient;

        let cfg = crate::provider::openai_compatible::OpenAiCompatibleConfig {
            provider_id: "zai".into(),
            api_key: "k".into(),
            base_url: "https://api.z.ai/api/anthropic".into(),
            model: "glm-4.6".into(),
            timeout_secs: 30,
            temperature: None,
            sampling: Default::default(),
            network: crate::goal::NetworkPolicy::On,
            native_search_enabled: false,
            fallback_model: None,
            context_tokens: Some(128_000),
            output_tokens: None,
        };
        let p = super::AnthropicProvider::new(cfg).unwrap();
        let caps = p.capabilities();
        assert!(!caps.supports_streaming);
        assert!(caps.supports_tool_calling);
        assert!(caps.supports_shell_tool, "本地工具应报 true");
        assert!(!caps.server_side_search);
        assert_eq!(caps.provider_id, "zai");
    }

    fn tc(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    #[test]
    fn system_lifted_and_tools_mapped() {
        let msgs = vec![ChatMessage::system("SYS"), ChatMessage::user("hi")];
        let tools = vec![
            json!({"type":"function","function":{"name":"fs_read","description":"d","parameters":{"type":"object"}}}),
        ];
        let body = build_anthropic_request(&msgs, &tools, "glm-4.6", 4096, None, None).unwrap();
        assert_eq!(body["system"], json!("SYS"));
        assert_eq!(body["max_tokens"], json!(4096));
        assert_eq!(body["model"], json!("glm-4.6"));
        assert_eq!(body["tools"][0]["name"], json!("fs_read"));
        assert_eq!(body["tools"][0]["input_schema"], json!({"type":"object"}));
        assert!(body["tools"][0].get("function").is_none());
        // messages 不含 system
        let roles: Vec<&str> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["user"]);
    }

    #[test]
    fn max_tokens_stop_reason_maps_to_length() {
        let response = parse_anthropic_response(&json!({
            "content": [{"type": "thinking", "thinking": "truncated"}],
            "stop_reason": "max_tokens"
        }))
        .unwrap();

        assert_eq!(
            response.finish_reason,
            Some(crate::provider::FinishReason::Length)
        );
    }

    fn roles(body: &Value) -> Vec<String> {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn assistant_text_plus_tool_calls_one_content_array() {
        let msgs = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant(
                "thinking out loud",
                None,
                vec![
                    tc("call_1", "fs_read", "{\"path\":\"a\"}"),
                    tc("call_2", "grep", "{\"q\":\"x\"}"),
                ],
            ),
        ];
        let body = build_anthropic_request(&msgs, &[], "m", 100, None, None).unwrap();
        let asst = &body["messages"][1];
        assert_eq!(asst["role"], json!("assistant"));
        let blocks = asst["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], json!("text"));
        assert_eq!(blocks[1]["type"], json!("tool_use"));
        assert_eq!(blocks[1]["id"], json!("call_1"));
        assert_eq!(blocks[1]["name"], json!("fs_read"));
        assert_eq!(blocks[1]["input"], json!({"path":"a"}));
        assert_eq!(blocks[2]["type"], json!("tool_use"));
        assert_eq!(blocks[2]["id"], json!("call_2"));
    }

    #[test]
    fn adjacent_tool_results_merge_into_one_user_message() {
        let msgs = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant("", None, vec![tc("c1", "t", "{}"), tc("c2", "t", "{}")]),
            ChatMessage::tool("c1", "result one"),
            ChatMessage::tool("c2", "result two"),
        ];
        let body = build_anthropic_request(&msgs, &[], "m", 100, None, None).unwrap();
        // user, assistant, user(合并的 tool_result) —— 不是两条 user
        assert_eq!(roles(&body), vec!["user", "assistant", "user"]);
        let results = body["messages"][2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["type"], json!("tool_result"));
        assert_eq!(results[0]["tool_use_id"], json!("c1"));
        assert_eq!(results[0]["content"], json!("result one"));
        assert_eq!(results[1]["tool_use_id"], json!("c2"));
    }

    #[test]
    fn consecutive_plain_user_messages_merge() {
        let msgs = vec![
            ChatMessage::user("a"),
            ChatMessage::user("b"),
            ChatMessage::assistant("ok", None, vec![]),
        ];
        let body = build_anthropic_request(&msgs, &[], "m", 100, None, None).unwrap();
        assert_eq!(roles(&body), vec!["user", "assistant"]);
    }

    #[test]
    fn malformed_tool_args_fall_back_to_empty_object() {
        // 前导 user 保证首条合法（codex BLOCKER1）
        let msgs = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant("", None, vec![tc("c1", "t", "NOT JSON")]),
        ];
        let body = build_anthropic_request(&msgs, &[], "m", 100, None, None).unwrap();
        assert_eq!(body["messages"][1]["content"][0]["input"], json!({}));
    }

    #[test]
    fn first_message_must_be_user_else_err() {
        // 首条非-system 是 assistant → 报错（Anthropic 硬要求·codex BLOCKER1）
        let msgs = vec![ChatMessage::assistant("hi", None, vec![])];
        assert!(build_anthropic_request(&msgs, &[], "m", 100, None, None).is_err());
    }

    #[test]
    fn empty_assistant_message_skipped_no_empty_text_block() {
        // 空 assistant(无 text 无 tool)→ 不发空 text 块（codex BLOCKER2）
        let msgs = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant("", None, vec![]),
        ];
        let body = build_anthropic_request(&msgs, &[], "m", 100, None, None).unwrap();
        assert_eq!(roles(&body), vec!["user"]);
    }

    #[test]
    fn tool_result_then_user_feedback_one_user_results_first() {
        // assistant(tool_use) → tool → user("feedback")：合并成一条 user·tool_result 在前·text 在后（codex MEDIUM7）
        let msgs = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant("", None, vec![tc("c1", "t", "{}")]),
            ChatMessage::tool("c1", "res"),
            ChatMessage::user("feedback"),
        ];
        let body = build_anthropic_request(&msgs, &[], "m", 100, None, None).unwrap();
        assert_eq!(roles(&body), vec!["user", "assistant", "user"]);
        let blocks = body["messages"][2]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], json!("tool_result"));
        assert_eq!(blocks[1]["type"], json!("text"));
        assert_eq!(blocks[1]["text"], json!("feedback"));
    }

    #[test]
    fn strict_alternation_invariant() {
        // 真实形态：system, user, assistant(tool), tool, assistant(text)
        let msgs = vec![
            ChatMessage::system("S"),
            ChatMessage::user("go"),
            ChatMessage::assistant("", None, vec![tc("c1", "t", "{}")]),
            ChatMessage::tool("c1", "r"),
            ChatMessage::assistant("done", None, vec![]),
        ];
        let body = build_anthropic_request(&msgs, &[], "m", 100, None, None).unwrap();
        let rs = roles(&body);
        assert_eq!(rs.first().map(String::as_str), Some("user"));
        for pair in rs.windows(2) {
            assert_ne!(pair[0], pair[1], "相邻 role 必须交替: {rs:?}");
        }
    }

    #[test]
    fn parse_text_tool_use_thinking() {
        let body = json!({"content":[
            {"type":"thinking","thinking":"hmm"},
            {"type":"text","text":"hello"},
            {"type":"tool_use","id":"tu_1","name":"fs_read","input":{"path":"a"}}
        ]});
        let resp = parse_anthropic_response(&body).unwrap();
        assert_eq!(resp.text, "hello");
        assert_eq!(resp.reasoning, "hmm");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "tu_1");
        assert_eq!(resp.tool_calls[0].function.name, "fs_read");
        assert_eq!(resp.tool_calls[0].function.arguments, "{\"path\":\"a\"}");
    }

    #[test]
    fn parse_missing_content_errors() {
        // content 缺失/非数组 → 报 provider 错·不静默空（codex MEDIUM6）
        assert!(parse_anthropic_response(&json!({"id":"x"})).is_err());
    }
}
