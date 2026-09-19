//! Regression for the resume-path MCP wiring fix: `resume_solo_with_judge_and_fs_scope`
//! used to hardcode `mcp_servers: Vec::new()`, so a resumed run could never see (or
//! call) an MCP server's tools even though a fresh `run` with the same config did.
//! This drives a real Streamable HTTP MCP mock server through `resume_solo_with_judge_and_fs_scope`
//! directly (no prior "run" needed — same trick `tests/autocompact_e2e.rs` uses to seed
//! a resumable conversation) and asserts the resumed run actually calls the server's
//! tool and sees its result.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use myagent::events::{EventRecorder, OutputMode};
use myagent::exec::sandbox::FsWriteFence;
use myagent::fs_scope::FsReadScope;
use myagent::goal::NetworkPolicy;
use myagent::journal::{save_conversation, RunPaths, SavedConversation};
use myagent::mcp::config::McpServerConfig;
use myagent::orchestrator::{resume_solo_with_judge_and_fs_scope, ControlInputKind, RunOutcome};
use myagent::provider::{
    ChatMessage, FinishReason, FunctionCall, ProviderCapabilities, ProviderClient,
    ProviderResponse, ToolCall,
};
use myagent::shell::PermissionPolicy;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Minimal Streamable HTTP MCP server: one tool `ping` that always returns "pong".
/// Trimmed down from `tests/mcp_streamable_http_contract.rs`'s `AppServerSubset`.
struct PingServer;

impl Respond for PingServer {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let req: Value = match serde_json::from_slice(&request.body) {
            Ok(v) => v,
            Err(_) => return ResponseTemplate::new(400),
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let id = req.get("id").cloned();
        if id.is_none() || method.starts_with("notifications/") {
            return ResponseTemplate::new(202);
        }
        let id = id.unwrap();

        let result = match method {
            "initialize" => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "srv", "version": "0.1"}
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "ping",
                    "description": "Return the string pong.",
                    "inputSchema": {"type": "object", "properties": {}}
                }]
            }),
            "tools/call" => json!({"content": [{"type": "text", "text": "pong"}]}),
            other => {
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("method not found: {other}")}
                });
                return ResponseTemplate::new(200).set_body_json(body);
            }
        };
        ResponseTemplate::new(200)
            .insert_header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
}

async fn start_ping_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(PingServer)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    server
}

/// Turn 1 (no tool-role message yet): call `mcp__srv__ping`. Turn 2 (tool result
/// present): record what the tool message said, then finish with no further calls.
struct ResumeMcpProvider {
    seen_tool_result: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ProviderClient for ResumeMcpProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse, myagent::error::HarnessError> {
        let tool_message = messages.iter().find(|m| m.role == "tool");
        match tool_message {
            None => Ok(ProviderResponse {
                text: String::new(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "mcp__srv__ping".to_string(),
                        arguments: "{}".to_string(),
                    },
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                interruption: None,
            }),
            Some(tool_message) => {
                *self.seen_tool_result.lock().unwrap() = tool_message.content.clone();
                Ok(ProviderResponse {
                    text: "done".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: Some(FinishReason::Stop),
                    interruption: None,
                })
            }
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "test-local".to_string(),
            model_id: "resume-mcp-test".to_string(),
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

fn http_cfg(name: &str, url: String) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        command: String::new(),
        url: Some(url),
        args: Vec::new(),
        env: Default::default(),
        trusted: true,
        headers: None,
    }
}

#[tokio::test]
async fn resume_wires_configured_mcp_servers_and_calls_the_tool() {
    let server = start_ping_server().await;
    let url = format!("{}/mcp", server.uri());

    let ws = tempfile::tempdir().unwrap();
    let run_id = "resume_mcp_wiring";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-mcp-test".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user("call the mcp ping tool"),
            ],
        },
    )
    .unwrap();

    let seen_tool_result = Arc::new(Mutex::new(None));
    let provider = ResumeMcpProvider {
        seen_tool_result: seen_tool_result.clone(),
    };

    let result = resume_solo_with_judge_and_fs_scope(
        provider,
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        3,
        ControlInputKind::Sentinel,
        false,
        false,
        myagent::config::SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        vec![http_cfg("srv", url)],
        Vec::new(),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        seen_tool_result.lock().unwrap().as_deref(),
        Some("pong"),
        "resumed run must have registered and called the mcp__srv__ping tool"
    );
}

/// Same wiring, but with no mcp_servers passed — the resumed run must not have the
/// mcp tool available at all (pins the pre-fix hardcoded-empty behavior as a
/// deliberate opt-out, not a silent bug, for a caller that really wants no MCP).
#[tokio::test]
async fn resume_without_mcp_servers_has_no_mcp_tools() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "resume_mcp_wiring_absent";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-mcp-test".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user("nothing to do"),
            ],
        },
    )
    .unwrap();

    struct FinishImmediatelyProvider;
    #[async_trait]
    impl ProviderClient for FinishImmediatelyProvider {
        async fn next_turn(
            &self,
            _messages: &[ChatMessage],
            tools: &[Value],
            _events: &mut EventRecorder,
        ) -> Result<ProviderResponse, myagent::error::HarnessError> {
            let has_mcp_tool = tools.iter().any(|t| {
                t["function"]["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("mcp__"))
            });
            assert!(
                !has_mcp_tool,
                "no mcp_servers were configured for this resume; no mcp__* tool should be offered"
            );
            Ok(ProviderResponse {
                text: "done".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                interruption: None,
            })
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                provider_id: "test-local".to_string(),
                model_id: "resume-mcp-test".to_string(),
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

    let result = resume_solo_with_judge_and_fs_scope(
        FinishImmediatelyProvider,
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        3,
        ControlInputKind::Sentinel,
        false,
        false,
        myagent::config::SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
}
