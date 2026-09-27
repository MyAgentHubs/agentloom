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
        ..Default::default()
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
        let goal =
            crate::goal::GoalState::new("x", crate::goal::parse_criteria(&[spec.into()]).unwrap());
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
    assert!(!tool_args_complete("{\"a\":")); // truncated
    assert!(!tool_args_complete("null"));
    assert!(!tool_args_complete("[]"));
    assert!(!tool_args_complete("\"x\""));
    assert!(!tool_args_complete("5"));
    assert!(!tool_args_complete("")); // empty string, conservatively dropped on the interrupted path
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
    accs.insert(1usize, acc("b", "{\"y\":")); // insert key 1 first
    accs.insert(0usize, acc("a", "{\"x\":1}")); // insert key 0 second, verify ordering by key
    let (resp, dropped) = finalize_provider_response("text".into(), "r".into(), accs, false, None);
    assert_eq!(resp.text, "text");
    assert_eq!(resp.reasoning, "r");
    assert_eq!(resp.tool_calls.len(), 2);
    // BTreeMap key order: a(key0) first, then b(key1)
    assert_eq!(resp.tool_calls[0].id, "call_0");
    assert_eq!(resp.tool_calls[0].call_type, "function");
    assert_eq!(resp.tool_calls[0].function.name, "a");
    assert_eq!(resp.tool_calls[1].id, "call_1");
    assert_eq!(resp.tool_calls[1].function.name, "b");
    assert_eq!(resp.tool_calls[1].function.arguments, "{\"y\":"); // truncated, still kept on the normal (non-interrupted) path
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
    accs.insert(0usize, acc("name_only", "")); // non-empty name + empty args string
    accs.insert(1usize, acc("bad", "null")); // non-empty name + args=null
    let (resp, dropped) =
        finalize_provider_response("text".into(), String::new(), accs, true, None);
    assert!(resp.tool_calls.is_empty());
    assert_eq!(dropped, vec!["name_only".to_string(), "bad".to_string()]);
}

#[test]
fn finalize_interrupted_filtered_empty_response() {
    let mut accs = BTreeMap::new();
    accs.insert(0usize, acc("half", "{\"y\":")); // truncated -> dropped
    let (resp, dropped) =
        finalize_provider_response(String::new(), String::new(), accs, true, None);
    // empty after filtering (collect returns the original Err on this basis; assert the fields directly here)
    assert!(resp.text.is_empty());
    assert!(resp.reasoning.is_empty());
    assert!(resp.tool_calls.is_empty());
    assert_eq!(dropped, vec!["half".to_string()]);
}

#[test]
fn finalize_empty_shell_accumulator_not_partial() {
    let mut accs = BTreeMap::new();
    accs.insert(0usize, ToolCallAccumulator::default()); // index-only, name empty, args empty
    let (resp, dropped) =
        finalize_provider_response(String::new(), String::new(), accs, true, None);
    assert!(resp.tool_calls.is_empty());
    assert!(resp.text.is_empty() && resp.reasoning.is_empty());
    assert!(dropped.is_empty()); // name empty, never counts as a "dropped tool"
}

#[test]
fn response_is_empty_true_only_when_all_empty() {
    let empty = ProviderResponse {
        text: String::new(),
        reasoning: String::new(),
        tool_calls: vec![],
        finish_reason: None,
        interruption: None,
    };
    assert!(response_is_empty(&empty));
    let with_text = ProviderResponse {
        text: "hi".into(),
        reasoning: String::new(),
        tool_calls: vec![],
        finish_reason: None,
        interruption: None,
    };
    assert!(!response_is_empty(&with_text));
    let with_reasoning = ProviderResponse {
        text: String::new(),
        reasoning: "r".into(),
        tool_calls: vec![],
        finish_reason: None,
        interruption: None,
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
        interruption: None,
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
        context_tokens: Some(8192),
        output_tokens: Some(1024),
        ..Default::default()
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
        ..Default::default()
    })
    .unwrap();
    let caps = unspecified.capabilities();
    assert_eq!(caps.max_context_tokens, None);
    assert_eq!(caps.output_token_limit, None);
}

mod image_tests;
