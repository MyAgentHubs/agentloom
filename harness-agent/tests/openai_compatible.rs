use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;
use serial_test::serial;
use tempfile::tempdir;
use wiremock::matchers::{body_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use myagent::events::{EventRecorder, OutputMode};
use myagent::goal::NetworkPolicy;
use myagent::provider::anthropic_compatible::AnthropicProvider;
use myagent::provider::openai_compatible::{
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, SamplingParams,
};
use myagent::provider::{shell_tool_definition, ChatMessage, ProviderClient};

#[tokio::test]
async fn streams_content_and_reasoning_from_base_url_with_v1() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    json!({"choices":[{"delta":{"content":"Hel"}}]}),
                    json!({"choices":[{"delta":{"reasoning_content":"thinking"}}]}),
                    json!({"choices":[{"delta":{"content":"lo"}}]}),
                ])),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "deepseek".to_string(),
        api_key: "sk-test".to_string(),
        base_url: format!("{}/v1", server.uri()),
        model: "deepseek-v4-flash".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap();

    let response = provider
        .next_turn(
            &[ChatMessage::user("hello")],
            &[shell_tool_definition()],
            &mut events,
        )
        .await
        .unwrap();

    assert_eq!(response.text, "Hello");
    assert_eq!(response.reasoning, "thinking");
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("agent.note.delta"));
    assert!(journal.contains("agent.reasoning.delta"));
}

#[tokio::test]
async fn accumulates_streamed_tool_call_arguments() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    json!({
                        "choices": [{
                            "delta": {
                                "tool_calls": [{
                                    "index": 0,
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "shell_exec",
                                        "arguments": "{\"command\":\"echo"
                                    }
                                }]
                            }
                        }]
                    }),
                    json!({
                        "choices": [{
                            "delta": {
                                "tool_calls": [{
                                    "index": 0,
                                    "function": {
                                        "arguments": " hi\"}"
                                    }
                                }]
                            }
                        }]
                    }),
                ])),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "openai-compatible".to_string(),
        api_key: "sk-test".to_string(),
        base_url: server.uri(),
        model: "test-model".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap();

    let response = provider
        .next_turn(
            &[ChatMessage::user("run shell")],
            &[shell_tool_definition()],
            &mut events,
        )
        .await
        .unwrap();

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].function.name, "shell_exec");
    assert_eq!(
        response.tool_calls[0].function.arguments,
        "{\"command\":\"echo hi\"}"
    );
}

#[tokio::test]
async fn tolerates_crlf_sse_and_provider_keepalive_json() {
    let server = MockServer::start().await;
    let body = [
        format!("data: {}\r\n\r\n", json!({"type":"ping"})),
        format!(
            "data: {}\r\n\r\n",
            json!({"choices":[{"delta":{"content":"ok"}}]})
        ),
        "data: [DONE]\r\n\r\n".to_string(),
    ]
    .join("");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "openai-compatible".to_string(),
        api_key: "sk-test".to_string(),
        base_url: server.uri(),
        model: "test-model".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap();

    let response = provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();

    assert_eq!(response.text, "ok");
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("provider.warning"));
}

#[tokio::test]
async fn replays_reasoning_content_to_reasoning_provider() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
                ),
        )
        .mount(&server)
        .await;
    let cfg = myagent::provider::openai_compatible::OpenAiCompatibleConfig {
        provider_id: "deepseek".into(),
        api_key: "k".into(),
        base_url: server.uri(),
        model: "deepseek-v4-flash".into(),
        timeout_secs: 30,
        temperature: Some(0.0),
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    };
    let provider =
        myagent::provider::openai_compatible::OpenAiCompatibleProvider::new(cfg).unwrap();
    let messages = vec![
        myagent::provider::ChatMessage::user("do it"),
        myagent::provider::ChatMessage::assistant("step", Some("my-reasoning".into()), vec![]),
        myagent::provider::ChatMessage::user("continue"),
    ];
    let dir = tempfile::tempdir().unwrap();
    let mut rec = myagent::events::EventRecorder::new(
        "r",
        None,
        None,
        &dir.path().join("e.jsonl"),
        myagent::events::OutputMode::Silent,
    )
    .unwrap();
    let _ = provider.next_turn(&messages, &[], &mut rec).await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let dumped = serde_json::to_string(&body["messages"]).unwrap();
    assert!(
        dumped.contains("my-reasoning"),
        "reasoning must be replayed; got {dumped}"
    );
    assert_eq!(body["temperature"], serde_json::json!(0.0));
}

#[tokio::test]
async fn strips_reasoning_for_non_reasoning_provider() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
                ),
        )
        .mount(&server)
        .await;
    let cfg = myagent::provider::openai_compatible::OpenAiCompatibleConfig {
        provider_id: "openai".into(),
        api_key: "k".into(),
        base_url: server.uri(),
        model: "gpt-4.1-mini".into(),
        timeout_secs: 30,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    };
    let provider =
        myagent::provider::openai_compatible::OpenAiCompatibleProvider::new(cfg).unwrap();
    let messages = vec![
        myagent::provider::ChatMessage::assistant("step", Some("my-reasoning".into()), vec![]),
        myagent::provider::ChatMessage::user("continue"),
    ];
    let dir = tempfile::tempdir().unwrap();
    let mut rec = myagent::events::EventRecorder::new(
        "r",
        None,
        None,
        &dir.path().join("e.jsonl"),
        myagent::events::OutputMode::Silent,
    )
    .unwrap();
    let _ = provider.next_turn(&messages, &[], &mut rec).await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let dumped = serde_json::to_string(&body["messages"]).unwrap();
    assert!(
        !dumped.contains("my-reasoning"),
        "reasoning must be stripped for non-reasoning provider; got {dumped}"
    );
    assert!(
        body.get("temperature").is_none(),
        "no temperature when config None"
    );
}

fn sse(chunks: Vec<serde_json::Value>) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str("data: ");
        body.push_str(&chunk.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

#[tokio::test]
async fn normal_stream_with_content_reasoning_and_complete_tool_finalizes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    json!({"choices": [{"delta": {"content": "hello "}}]}),
                    json!({"choices": [{"delta": {"reasoning_content": "let me think"}}]}),
                    json!({"choices": [{"delta": {"content": "world"}}]}),
                    json!({"choices": [{"delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "shell_exec", "arguments": "{\"command\":\"ls\"}"}
                        }]
                    }}]}),
                ])),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "openai-compatible".to_string(),
        api_key: "sk-test".to_string(),
        base_url: server.uri(),
        model: "test-model".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap();

    let response = provider
        .next_turn(
            &[ChatMessage::user("run ls")],
            &[shell_tool_definition()],
            &mut events,
        )
        .await
        .unwrap();

    assert_eq!(response.text, "hello world");
    assert_eq!(response.reasoning, "let me think");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call_1");
    assert_eq!(response.tool_calls[0].call_type, "function");
    assert_eq!(response.tool_calls[0].function.name, "shell_exec");
    assert_eq!(
        response.tool_calls[0].function.arguments,
        "{\"command\":\"ls\"}"
    );
}

#[tokio::test]
async fn glm_active_injects_web_search_without_overwriting_tools() {
    let body =
        captured_request_body("glm", NetworkPolicy::On, true, &[shell_tool_definition()]).await;
    let tools = body["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["type"] == "function"));
    assert!(tools.iter().any(|t| {
        t["type"] == "web_search"
            && t["web_search"]["enable"] == "True"
            && t["web_search"]["search_engine"] == "search_pro"
            && t["web_search"]["search_result"] == "True"
    }));
}

#[tokio::test]
async fn glm_disabled_by_user_does_not_inject_web_search() {
    let body =
        captured_request_body("glm", NetworkPolicy::On, false, &[shell_tool_definition()]).await;
    assert!(!has_web_search_tool(&body));
}

#[tokio::test]
async fn glm_disabled_by_network_does_not_inject_web_search() {
    let body =
        captured_request_body("glm", NetworkPolicy::Off, true, &[shell_tool_definition()]).await;
    assert!(!has_web_search_tool(&body));
}

#[tokio::test]
async fn generic_without_native_capability_does_not_inject_web_search() {
    let body = captured_request_body(
        "deepseek",
        NetworkPolicy::On,
        true,
        &[shell_tool_definition()],
    )
    .await;
    assert!(!has_web_search_tool(&body));
}

#[tokio::test]
async fn qwen_active_sets_enable_search_and_search_options() {
    let body = captured_request_body("qwen", NetworkPolicy::On, true, &[]).await;
    assert_eq!(body["enable_search"], true);
    assert_eq!(body["search_options"]["search_strategy"], "agent");
}

#[tokio::test]
async fn native_search_4xx_degrades_and_retries_without_native() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("\"web_search\""))
        .respond_with(ResponseTemplate::new(400).set_body_string("native search rejected"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"ok"}}]})])),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "glm".to_string(),
        api_key: "sk-test".to_string(),
        base_url: server.uri(),
        model: "test-model".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap();

    let response = provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();

    assert_eq!(response.text, "ok");
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let first: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(has_web_search_tool(&first));
    assert!(!has_web_search_tool(&second));
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("provider.warning"));
    assert!(journal.contains("native_search_degraded"));
    assert!(journal.contains("\"status\":400"));
}

async fn captured_request_body(
    provider_id: &str,
    network: NetworkPolicy,
    native_search_enabled: bool,
    tools: &[serde_json::Value],
) -> serde_json::Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"ok"}}]})])),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: provider_id.to_string(),
        api_key: "sk-test".to_string(),
        base_url: server.uri(),
        model: "test-model".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network,
        native_search_enabled,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap();

    provider
        .next_turn(&[ChatMessage::user("hello")], tools, &mut events)
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    serde_json::from_slice(&reqs[0].body).unwrap()
}

fn has_web_search_tool(body: &serde_json::Value) -> bool {
    body["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["type"] == "web_search"))
}

#[tokio::test]
async fn kimi_echo_round_trip_appends_history_and_hides_interim() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            move |_req: &Request| match responder_calls.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![json!({
                        "choices": [{
                            "delta": {
                                "content": "interim",
                                "reasoning_content": "hidden-thinking",
                                "tool_calls": [{
                                    "index": 0,
                                    "id": "t1",
                                    "type": "builtin_function",
                                    "function": {
                                        "name": "$web_search",
                                        "arguments": "{\"query\":\"rust\"}"
                                    }
                                }]
                            }
                        }]
                    })])),
                _ => ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![
                        json!({"choices":[{"delta":{"content":"final"}}]}),
                    ])),
            },
        )
        .mount(&server)
        .await;

    let provider = kimi_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let response = provider
        .next_turn(&[ChatMessage::user("search")], &[], &mut events)
        .await
        .unwrap();

    assert_eq!(response.text, "final");
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let first: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(has_kimi_web_search_tool(&first));
    assert_eq!(first["thinking"]["type"], "disabled");
    assert!(first.get("enable_thinking").is_none());
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let messages = second["messages"].as_array().unwrap();
    let assistant = &messages[messages.len() - 2];
    let tool = &messages[messages.len() - 1];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"], "interim");
    assert_eq!(assistant["tool_calls"][0]["id"], "t1");
    assert_eq!(assistant["tool_calls"][0]["type"], "builtin_function");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        "$web_search"
    );
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        "{\"query\":\"rust\"}"
    );
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "t1");
    assert_eq!(tool["name"], "$web_search");
    assert_eq!(tool["content"], "{\"query\":\"rust\"}");

    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("final"));
    assert!(!journal.contains("interim"));
    assert!(!journal.contains("hidden-thinking"));
    assert!(!journal.contains("\"type\":\"tool."));
}

#[tokio::test]
async fn kimi_multiple_web_search_echoes_all() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            move |_req: &Request| match responder_calls.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![json!({
                        "choices": [{
                            "delta": {
                                "tool_calls": [
                                    {
                                        "index": 0,
                                        "id": "t1",
                                        "type": "builtin_function",
                                        "function": {
                                            "name": "$web_search",
                                            "arguments": "{\"query\":\"rust\"}"
                                        }
                                    },
                                    {
                                        "index": 1,
                                        "id": "t2",
                                        "type": "builtin_function",
                                        "function": {
                                            "name": "$web_search",
                                            "arguments": "{\"query\":\"moonshot\"}"
                                        }
                                    }
                                ]
                            }
                        }]
                    })])),
                _ => ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"done"}}]})])),
            },
        )
        .mount(&server)
        .await;

    let provider = kimi_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let response = provider
        .next_turn(&[ChatMessage::user("search twice")], &[], &mut events)
        .await
        .unwrap();

    assert_eq!(response.text, "done");
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let messages = second["messages"].as_array().unwrap();
    let assistant_index = messages
        .iter()
        .position(|message| message["role"] == "assistant" && message["tool_calls"].is_array())
        .unwrap();
    let assistant = &messages[assistant_index];
    let tool_one = &messages[assistant_index + 1];
    let tool_two = &messages[assistant_index + 2];

    let tool_calls = assistant["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0]["id"], "t1");
    assert_eq!(tool_calls[0]["type"], "builtin_function");
    assert_eq!(tool_calls[0]["function"]["name"], "$web_search");
    assert_eq!(
        tool_calls[0]["function"]["arguments"],
        "{\"query\":\"rust\"}"
    );
    assert_eq!(tool_calls[1]["id"], "t2");
    assert_eq!(tool_calls[1]["type"], "builtin_function");
    assert_eq!(tool_calls[1]["function"]["name"], "$web_search");
    assert_eq!(
        tool_calls[1]["function"]["arguments"],
        "{\"query\":\"moonshot\"}"
    );

    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        2
    );
    assert_eq!(tool_one["role"], "tool");
    assert_eq!(tool_one["tool_call_id"], "t1");
    assert_eq!(tool_one["name"], "$web_search");
    assert_eq!(tool_one["content"], "{\"query\":\"rust\"}");
    assert_eq!(tool_two["role"], "tool");
    assert_eq!(tool_two["tool_call_id"], "t2");
    assert_eq!(tool_two["name"], "$web_search");
    assert_eq!(tool_two["content"], "{\"query\":\"moonshot\"}");
}

#[tokio::test]
async fn kimi_loop_cap_does_not_fake_completion() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(kimi_web_search_sse("t1", "{\"query\":\"again\"}")),
        )
        .mount(&server)
        .await;

    let provider = kimi_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let err = provider
        .next_turn(&[ChatMessage::user("search")], &[], &mut events)
        .await
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("kimi web_search did not converge within echo limit"));
}

#[tokio::test]
async fn kimi_mixed_tool_calls_filter_out_web_search() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": "t1",
                                    "type": "builtin_function",
                                    "function": {
                                        "name": "$web_search",
                                        "arguments": "{\"query\":\"rust\"}"
                                    }
                                },
                                {
                                    "index": 1,
                                    "id": "call_shell",
                                    "type": "function",
                                    "function": {
                                        "name": "shell_exec",
                                        "arguments": "{\"command\":\"echo ok\"}"
                                    }
                                }
                            ]
                        }
                    }]
                })])),
        )
        .mount(&server)
        .await;

    let provider = kimi_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let response = provider
        .next_turn(
            &[ChatMessage::user("search and run")],
            &[shell_tool_definition()],
            &mut events,
        )
        .await
        .unwrap();

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].function.name, "shell_exec");
    assert_eq!(
        response.tool_calls[0].function.arguments,
        "{\"command\":\"echo ok\"}"
    );
}

#[tokio::test]
async fn kimi_4xx_degrades_not_fail() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("$web_search"))
        .respond_with(ResponseTemplate::new(400).set_body_string("native rejected"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"ok"}}]})])),
        )
        .mount(&server)
        .await;

    let provider = kimi_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let response = provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();

    assert_eq!(response.text, "ok");
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let first: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(has_kimi_web_search_tool(&first));
    assert_eq!(first["thinking"]["type"], "disabled");
    assert!(first.get("enable_thinking").is_none());
    assert!(!has_kimi_web_search_tool(&second));
    assert!(second.get("enable_thinking").is_none());
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("provider.warning"));
    assert!(journal.contains("native_search_degraded"));
    assert!(journal.contains("\"status\":400"));
}

fn kimi_provider(base_url: String) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "kimi".to_string(),
        api_key: "sk-test".to_string(),
        base_url,
        model: "moonshot-v1-8k".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap()
}

fn test_events(temp: &tempfile::TempDir) -> EventRecorder {
    EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap()
}

fn has_kimi_web_search_tool(body: &serde_json::Value) -> bool {
    body["tools"].as_array().is_some_and(|tools| {
        tools.iter().any(|tool| {
            tool["type"] == "builtin_function" && tool["function"]["name"] == "$web_search"
        })
    })
}

fn kimi_web_search_sse(id: &str, arguments: &str) -> String {
    sse(vec![json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": id,
                    "type": "builtin_function",
                    "function": {
                        "name": "$web_search",
                        "arguments": arguments
                    }
                }]
            }
        }]
    })])
}

#[test]
fn capabilities_declare_server_side_search_statically() {
    use myagent::provider::openai_compatible::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
    use myagent::provider::ProviderClient;
    let mk = |id: &str| {
        OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
            provider_id: id.into(),
            api_key: "k".into(),
            base_url: "http://localhost:1".into(),
            model: "m".into(),
            timeout_secs: 5,
            temperature: None,
            sampling: Default::default(),
            network: myagent::goal::NetworkPolicy::On,
            native_search_enabled: true,
            fallback_model: None,
            context_tokens: None,
            output_tokens: None,
        })
        .unwrap()
    };
    assert!(mk("glm").capabilities().server_side_search);
    assert!(mk("qwen").capabilities().server_side_search);
    assert!(mk("kimi").capabilities().server_side_search);
    assert!(!mk("deepseek").capabilities().server_side_search);
}

#[test]
fn capabilities_old_json_without_new_field_deserializes() {
    // 旧 journal/JSON 没有 server_side_search → #[serde(default)] 反序列化为 false·不炸
    let old = r#"{"provider_id":"x","model_id":"m","supports_streaming":true,"supports_reasoning_deltas":false,"supports_tool_calling":true,"supports_images":false,"supports_computer_use":false,"supports_shell_tool":true,"max_context_tokens":null,"output_token_limit":null}"#;
    let caps: myagent::provider::ProviderCapabilities = serde_json::from_str(old).unwrap();
    assert!(!caps.server_side_search);
}

// ── retry tests ────────────────────────────────────────────────────

/// Helper: a generic provider that does NOT have native search, so
/// post_native_or_degrade won't attempt a 4xx degrade. Good for retry tests.
fn generic_provider(base_url: String) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "openai-compatible".to_string(),
        api_key: "sk-test".to_string(),
        base_url,
        model: "test-model".to_string(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap()
}

#[tokio::test]
async fn retry_503_twice_then_200_succeeds() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            move |_req: &Request| match responder_calls.fetch_add(1, Ordering::SeqCst) {
                0 | 1 => ResponseTemplate::new(503),
                _ => ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"ok"}}]})])),
            },
        )
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let response = provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();

    assert_eq!(response.text, "ok");
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3);
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("provider_retry"));
    assert!(journal.contains("\"status\":503"));
}

#[tokio::test]
async fn retry_always_500_gives_up_after_max_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let err = provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("HTTP 500"));
    // Default RetryPolicy has max_retries=3 → 4 total sends.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 4);
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("provider_retry"));
    // All warnings should have status=500.
    let retry_warnings: Vec<_> = journal
        .lines()
        .filter(|l| l.contains("provider_retry"))
        .collect();
    assert_eq!(retry_warnings.len(), 3); // 3 retries after the initial attempt.
}

#[tokio::test]
async fn retry_400_non_retryable_returns_immediately() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let err = provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("HTTP 400"));
    // 400 is not retryable → exactly 1 request, no retries.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(!journal.contains("provider_retry"));
}

#[tokio::test]
async fn sampling_openai_configured_fields_match_wire_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "stream": true,
            "temperature": 0.0,
            "max_tokens": 1234,
            "top_p": 0.9,
            "do_sample": false,
            "messages": [{"role": "user", "content": "hello"}],
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"ok"}}]})])),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "openai-compatible".into(),
        api_key: "sk-test".into(),
        base_url: server.uri(),
        model: "test-model".into(),
        timeout_secs: 5,
        temperature: Some(0.0),
        sampling: SamplingParams {
            top_p: Some(0.9),
            do_sample: Some(false),
        },
        network: NetworkPolicy::On,
        native_search_enabled: false,
        fallback_model: None,
        context_tokens: None,
        output_tokens: Some(1234),
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();
}

#[tokio::test]
async fn sampling_openai_unconfigured_fields_are_absent_from_wire_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_json(json!({
            "model": "test-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}],
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"ok"}}]})])),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();
}

#[tokio::test]
async fn sampling_anthropic_configured_fields_match_wire_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_json(json!({
            "model": "glm-4.6",
            "max_tokens": 2345,
            "temperature": 0.25,
            "top_p": 0.8,
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello"}],
            }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 11, "output_tokens": 7},
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(OpenAiCompatibleConfig {
        provider_id: "zai".into(),
        api_key: "sk-test".into(),
        base_url: server.uri(),
        model: "glm-4.6".into(),
        timeout_secs: 5,
        temperature: Some(0.25),
        sampling: SamplingParams {
            top_p: Some(0.8),
            do_sample: Some(false),
        },
        network: NetworkPolicy::On,
        native_search_enabled: false,
        fallback_model: None,
        context_tokens: None,
        output_tokens: Some(2345),
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();
}

#[tokio::test]
async fn sampling_anthropic_never_sends_do_sample_even_when_configured() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 11, "output_tokens": 7},
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(OpenAiCompatibleConfig {
        provider_id: "zai".into(),
        api_key: "sk-test".into(),
        base_url: server.uri(),
        model: "glm-4.6".into(),
        timeout_secs: 5,
        temperature: Some(0.25),
        sampling: SamplingParams {
            top_p: Some(0.8),
            do_sample: Some(false),
        },
        network: NetworkPolicy::On,
        native_search_enabled: false,
        fallback_model: None,
        context_tokens: None,
        output_tokens: Some(2345),
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["temperature"], json!(0.25));
    assert_eq!(body["top_p"], json!(0.8));
    assert!(
        body.get("do_sample").is_none(),
        "Anthropic wire body must omit do_sample: {body}"
    );
}

#[tokio::test]
async fn sampling_anthropic_unconfigured_fields_are_absent_from_wire_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_json(json!({
            "model": "glm-4.6",
            "max_tokens": 4096,
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello"}],
            }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 13, "output_tokens": 5},
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(OpenAiCompatibleConfig {
        provider_id: "zai".into(),
        api_key: "sk-test".into(),
        base_url: server.uri(),
        model: "glm-4.6".into(),
        timeout_secs: 5,
        temperature: None,
        sampling: Default::default(),
        network: NetworkPolicy::On,
        native_search_enabled: false,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    provider
        .next_turn(&[ChatMessage::user("hello")], &[], &mut events)
        .await
        .unwrap();
}

struct SamplingEnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl SamplingEnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for SamplingEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
#[serial]
fn sampling_provider_env_values_load_into_runtime_config() {
    let home = tempdir().unwrap();
    let _home = SamplingEnvGuard::set("MYAGENT_HOME", home.path().to_str().unwrap());
    let _api_key = SamplingEnvGuard::set("SAMPLING_TEST_API_KEY", "sk-test");
    let _temperature = SamplingEnvGuard::set("SAMPLING_TEST_TEMPERATURE", "0.125");
    let _top_p = SamplingEnvGuard::set("SAMPLING_TEST_TOP_P", "0.75");
    let _do_sample = SamplingEnvGuard::set("SAMPLING_TEST_DO_SAMPLE", "false");
    let _output_tokens = SamplingEnvGuard::set("SAMPLING_TEST_OUTPUT_TOKENS", "3456");

    let config = myagent::config::provider_config("sampling-test").unwrap();

    assert_eq!(config.temperature, Some(0.125));
    assert_eq!(config.sampling.top_p, Some(0.75));
    assert_eq!(config.sampling.do_sample, Some(false));
    assert_eq!(config.output_tokens, Some(3456));
}
