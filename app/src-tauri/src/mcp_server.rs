//! 进程内 MCP 服务（最小 Streamable HTTP · 同步 tiny_http）。
//! 块① 走通骨架：claude 队长经 --mcp-config http url 连上、调我们暴露的工具。
//! 传输：POST /mcp = JSON-RPC 2.0；notifications/* → HTTP 202 无 body；GET /mcp → 405。

use std::collections::HashMap;
use std::sync::Arc;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// 一个工具：名字 + 描述 + 入参 JSON Schema + 处理函数。
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub handler: Box<dyn Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>,
}

pub type ToolRegistry = HashMap<String, ToolDef>;

/// handle_jsonrpc 的结果：要么回 JSON-RPC（HTTP 200），要么是通知（HTTP 202 无 body）。
pub enum JsonRpcReply {
    Json(serde_json::Value),
    Accepted,
}

fn value_to_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn rpc_error(id: serde_json::Value, code: i64, msg: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":msg}})
}

/// 纯逻辑：把一条 JSON-RPC 请求翻成回复。可单测、不依赖传输。
pub fn handle_jsonrpc(req: &serde_json::Value, tools: &ToolRegistry) -> JsonRpcReply {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned();

    // 通知（无 id 或 notifications/* 前缀）→ 202、不回 body
    if id.is_none() || method.starts_with("notifications/") {
        return JsonRpcReply::Accepted;
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "agentloom", "version": "0.1"}
        }),
        "tools/list" => {
            let list: Vec<serde_json::Value> = tools
                .values()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
                .collect();
            serde_json::json!({ "tools": list })
        }
        "tools/call" => {
            let name = req
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            match tools.get(name) {
                Some(t) => match (t.handler)(args) {
                    Ok(v) => serde_json::json!({
                        "content": [{"type":"text","text": value_to_text(&v)}]
                    }),
                    Err(e) => serde_json::json!({
                        "content": [{"type":"text","text": e}],
                        "isError": true
                    }),
                },
                None => {
                    return JsonRpcReply::Json(rpc_error(
                        id,
                        -32602,
                        &format!("unknown tool: {name}"),
                    ))
                }
            }
        }
        other => {
            return JsonRpcReply::Json(rpc_error(id, -32601, &format!("method not found: {other}")))
        }
    };

    JsonRpcReply::Json(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}))
}

/// 运行中的 MCP 服务句柄；drop 时停掉 accept 循环。
pub struct McpServer {
    pub port: u16,
    server: Arc<tiny_http::Server>,
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

/// 在 127.0.0.1 随机端口起一个 MCP 服务；每个请求开独立线程处理
/// （避免某个慢工具阻塞后续 ping/cancel）。
pub fn start_mcp_server(tools: Arc<ToolRegistry>) -> Result<McpServer, String> {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?);
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .ok_or_else(|| crate::ui_msg::al_err("mcp.noPort", &[]))?;

    let srv = server.clone();
    std::thread::spawn(move || {
        for request in srv.incoming_requests() {
            let tools_for_req = tools.clone();
            std::thread::spawn(move || handle_request(request, &tools_for_req));
        }
    });

    Ok(McpServer { port, server })
}

fn respond_empty(request: tiny_http::Request, code: u16) {
    let _ = request.respond(tiny_http::Response::empty(code));
}

fn handle_request(mut request: tiny_http::Request, tools: &ToolRegistry) {
    if request.method() == &tiny_http::Method::Get {
        respond_empty(request, 405);
        return;
    }
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        respond_empty(request, 400);
        return;
    }
    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            respond_empty(request, 400);
            return;
        }
    };
    match handle_jsonrpc(&parsed, tools) {
        JsonRpcReply::Accepted => respond_empty(request, 202),
        JsonRpcReply::Json(v) => {
            let data = v.to_string();
            let resp = tiny_http::Response::from_string(data)
                .with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .unwrap(),
                )
                .with_header(
                    tiny_http::Header::from_bytes(
                        &b"MCP-Protocol-Version"[..],
                        PROTOCOL_VERSION.as_bytes(),
                    )
                    .unwrap(),
                );
            let _ = request.respond(resp);
        }
    }
}

/// Claude MCP 连接、工具调用统一放宽到 24h，供 config 与 lead 进程 env 共用。
pub(crate) const CLAUDE_MCP_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

/// 给 claude `--mcp-config` 用的 JSON：http transport + 大 timeout（长 worker 不被切）。
pub fn mcp_config_json(port: u16) -> String {
    format!(
        r#"{{"mcpServers":{{"agentloom":{{"type":"http","url":"http://127.0.0.1:{port}/mcp","timeout":{timeout}}}}}}}"#,
        timeout = CLAUDE_MCP_TIMEOUT_MS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_config_uses_shared_claude_timeout() {
        let config: serde_json::Value =
            serde_json::from_str(&mcp_config_json(4321)).expect("valid MCP config JSON");

        assert_eq!(
            config["mcpServers"]["agentloom"]["timeout"],
            CLAUDE_MCP_TIMEOUT_MS
        );
    }

    fn reg_with(
        name: &str,
        f: impl Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.insert(
            name.into(),
            ToolDef {
                name: name.into(),
                description: "test tool".into(),
                input_schema: serde_json::json!({"type":"object","properties":{}}),
                handler: Box::new(f),
            },
        );
        r
    }

    #[test]
    fn notifications_initialized_is_accepted() {
        let reg = ToolRegistry::new();
        let req = serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(matches!(handle_jsonrpc(&req, &reg), JsonRpcReply::Accepted));
    }

    #[test]
    fn tools_list_returns_names() {
        let reg = reg_with("ping", |_| Ok(serde_json::json!("pong")));
        let JsonRpcReply::Json(resp) = handle_jsonrpc(
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            &reg,
        ) else {
            panic!("expected Json")
        };
        assert_eq!(
            resp["result"]["tools"][0]["name"],
            serde_json::json!("ping")
        );
    }

    #[test]
    fn tools_call_wraps_text() {
        let reg = reg_with("ping", |_| Ok(serde_json::json!("pong")));
        let JsonRpcReply::Json(resp) = handle_jsonrpc(
            &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ping","arguments":{}}}),
            &reg,
        ) else {
            panic!("expected Json")
        };
        assert_eq!(
            resp["result"]["content"][0]["text"],
            serde_json::json!("pong")
        );
    }

    #[test]
    fn tools_call_error_sets_is_error() {
        let reg = reg_with("boom", |_| Err("nope".into()));
        let JsonRpcReply::Json(resp) = handle_jsonrpc(
            &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"boom","arguments":{}}}),
            &reg,
        ) else {
            panic!("expected Json")
        };
        assert_eq!(resp["result"]["isError"], serde_json::json!(true));
    }

    #[test]
    fn unknown_method_is_error() {
        let reg = ToolRegistry::new();
        let JsonRpcReply::Json(resp) = handle_jsonrpc(
            &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"nope"}),
            &reg,
        ) else {
            panic!("expected Json")
        };
        assert_eq!(resp["error"]["code"], serde_json::json!(-32601));
    }

    // 承重 spike：真跑 claude，验证它能连进程内 MCP 端点并调用工具（含一个慢工具不超时）。
    // 需本机登录的 claude。手动跑：
    //   cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored mcp_spike --nocapture
    #[test]
    #[ignore]
    fn mcp_spike_claude_calls_ping_and_slow() {
        let mut reg = ToolRegistry::new();
        reg.insert(
            "ping".into(),
            ToolDef {
                name: "ping".into(),
                description: "Return the string pong.".into(),
                input_schema: serde_json::json!({"type":"object","properties":{}}),
                handler: Box::new(|_| Ok(serde_json::json!("pong"))),
            },
        );
        reg.insert(
            "slow".into(),
            ToolDef {
                name: "slow".into(),
                description: "Sleep 90 seconds then return slow-ok.".into(),
                input_schema: serde_json::json!({"type":"object","properties":{}}),
                handler: Box::new(|_| {
                    std::thread::sleep(std::time::Duration::from_secs(90));
                    Ok(serde_json::json!("slow-ok"))
                }),
            },
        );
        let srv = start_mcp_server(Arc::new(reg)).expect("start mcp server");
        let cfg = mcp_config_json(srv.port);
        eprintln!("[spike] mcp config = {cfg}");

        let out = std::process::Command::new(crate::sandbox::resolve_claude_bin())
            .args([
                "-p",
                "First call the ping tool. Then call the slow tool. Then reply with exactly the two returned strings separated by a space.",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--mcp-config",
                &cfg,
                "--strict-mcp-config",
                "--allowedTools",
                "mcp__agentloom__ping,mcp__agentloom__slow",
            ])
            .output()
            .expect("run claude");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("[spike] claude stdout:\n{stdout}\n[spike] claude stderr:\n{stderr}");
        assert!(
            stdout.contains("pong") && stdout.contains("slow-ok"),
            "claude 没拿到两个工具返回 —— 传输或 MCP 协议有问题"
        );
    }

    // T6 校验：队长的 --tools 只读限制（禁 Write/Edit/Bash）下，claude 仍能调 MCP 工具。
    // cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored mcp_lead_tools_restriction --nocapture
    #[test]
    #[ignore]
    fn mcp_lead_tools_restriction_still_calls_mcp() {
        let mut reg = ToolRegistry::new();
        reg.insert(
            "ping".into(),
            ToolDef {
                name: "ping".into(),
                description: "Return the string pong.".into(),
                input_schema: serde_json::json!({"type":"object","properties":{}}),
                handler: Box::new(|_| Ok(serde_json::json!("pong"))),
            },
        );
        let srv = start_mcp_server(Arc::new(reg)).expect("start mcp server");
        let cfg = mcp_config_json(srv.port);
        let out = std::process::Command::new(crate::sandbox::resolve_claude_bin())
            .args([
                "-p",
                "Call the ping tool, then reply with exactly what it returned.",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--mcp-config",
                &cfg,
                "--strict-mcp-config",
                "--allowedTools",
                "mcp__agentloom__ping",
                "--disallowedTools",
                "Write,Edit,MultiEdit,NotebookEdit,Bash",
            ])
            .output()
            .expect("run claude");
        let stdout = String::from_utf8_lossy(&out.stdout);
        eprintln!("[lead-tools-spike] stdout:\n{stdout}");
        assert!(
            stdout.contains("pong"),
            "队长 --tools 只读限制下 claude 没调到 MCP ping —— --tools 可能把 MCP 工具也挡了"
        );
    }
}
