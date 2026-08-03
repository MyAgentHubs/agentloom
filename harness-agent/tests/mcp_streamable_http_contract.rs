//! L3 contract test: lock the rmcp-based MCP client against the exact Streamable
//! HTTP subset the AgentLoom app server speaks (`app/src-tauri/src/mcp_server.rs`).
//!
//! The app server is deliberately minimal: POST /mcp answers a single JSON-RPC
//! message as plain `application/json`; a notification (no `id`) gets HTTP 202
//! with no body; GET /mcp is 405 (no SSE); there is no `Mcp-Session-Id`
//! (stateless); `initialize` pins protocolVersion `2025-06-18`; only
//! `initialize` / `tools/list` / `tools/call` are understood.
//!
//! This test replicates that surface with wiremock and drives a real
//! `McpConnection` end to end: initialize handshake, tools/list, tools/call, and
//! (implicitly, via the handshake's `notifications/initialized`) the 202
//! notification path.

use std::time::Duration;

use myagent::mcp::client::McpConnection;
use myagent::mcp::config::McpServerConfig;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Replicates `handle_jsonrpc` from the app server: one request in, one reply out.
struct AppServerSubset;

impl Respond for AppServerSubset {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let req: Value = match serde_json::from_slice(&request.body) {
            Ok(v) => v,
            Err(_) => return ResponseTemplate::new(400),
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let id = req.get("id").cloned();

        // Notifications (no id, or notifications/* prefix) → 202, no body.
        if id.is_none() || method.starts_with("notifications/") {
            return ResponseTemplate::new(202);
        }
        let id = id.unwrap();

        let result = match method {
            "initialize" => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "agentloom", "version": "0.1"}
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "ping",
                    "description": "Return the string pong.",
                    "inputSchema": {"type": "object", "properties": {}}
                }]
            }),
            "tools/call" => {
                let name = req
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if name == "ping" {
                    json!({"content": [{"type": "text", "text": "pong"}]})
                } else {
                    let body = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32602, "message": format!("unknown tool: {name}")}
                    });
                    return ResponseTemplate::new(200).set_body_json(body);
                }
            }
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

async fn start_app_server() -> MockServer {
    let server = MockServer::start().await;
    // POST /mcp is the whole JSON-RPC surface.
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(AppServerSubset)
        .mount(&server)
        .await;
    // GET /mcp is 405 (no SSE); the client must tolerate this.
    Mock::given(method("GET"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    server
}

fn http_cfg(url: String) -> McpServerConfig {
    McpServerConfig {
        name: "agentloom".into(),
        command: String::new(),
        url: Some(url),
        args: Vec::new(),
        env: Default::default(),
        trusted: false,
    }
}

#[tokio::test]
async fn streamable_http_initialize_list_and_call_round_trip() {
    let server = start_app_server().await;
    let url = format!("{}/mcp", server.uri());
    let cfg = http_cfg(url);

    // initialize handshake succeeds against a server advertising 2025-06-18,
    // and the `notifications/initialized` (202) path did not break it.
    let (conn, caps) = McpConnection::connect(&cfg, Duration::from_secs(5))
        .await
        .expect("connect to app-server subset over Streamable HTTP");
    assert!(caps.tools, "server advertised tools capability");
    assert!(!caps.resources);
    assert!(!caps.prompts);

    // tools/list returns the advertised tool.
    let listed = conn
        .request("tools/list", json!({}))
        .await
        .expect("tools/list");
    assert_eq!(listed["tools"][0]["name"], "ping");

    // tools/call is a one-shot request/response.
    let called = conn
        .request("tools/call", json!({"name": "ping", "arguments": {}}))
        .await
        .expect("tools/call");
    assert_eq!(called["content"][0]["text"], "pong");

    conn.shutdown().await;
}

#[tokio::test]
async fn streamable_http_tool_error_surfaces_iserror() {
    let server = start_app_server().await;
    let url = format!("{}/mcp", server.uri());
    let cfg = http_cfg(url);

    let (conn, _caps) = McpConnection::connect(&cfg, Duration::from_secs(5))
        .await
        .expect("connect");

    // Calling an unknown tool yields a JSON-RPC error, surfaced as an Err.
    let err = conn
        .request("tools/call", json!({"name": "nope", "arguments": {}}))
        .await
        .expect_err("unknown tool should error");
    assert!(err.to_string().contains("-32602"), "got: {err}");

    conn.shutdown().await;
}
