use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::Result;
use crate::events::EventRecorder;
use crate::goal::NetworkPolicy;
use crate::mcp::client::McpConnection;
use crate::mcp::config::McpServerConfig;
use crate::mcp::tool::{McpResourceListTool, McpResourceReadTool};
use crate::tools::Tool;
use crate::tools::ToolRegistry;

/// Timeout for the MCP initialize handshake (connect + capability negotiation)
/// only. Kept short: an unreachable/hung server should fail fast so the run can
/// keep going without that server's tools registered — this is not the timeout
/// applied to requests issued after the connection is up (see
/// `MCP_REQUEST_TIMEOUT`).
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for each individual request (`tools/call` and friends) issued once a
/// connection is established. Deliberately much longer than
/// `MCP_CONNECT_TIMEOUT`: AgentLoom's `dispatch_worker` MCP tool can legitimately
/// block for minutes while a worker runs (and `ask_user` similarly waits on the
/// user), so this must stay ≥ AgentLoom's synchronous wait ceiling
/// (app/src-tauri/src/lead_tools.rs:77 `DISPATCH_WORKER_WAIT` = 240s) —
/// otherwise myagent times out on a request that is still in flight
/// server-side, wrongly treats it as a failure, and may re-dispatch/duplicate
/// work that was already queued for delivery.
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

pub struct McpHost {
    conns: Vec<Arc<McpConnection>>,
}

impl McpHost {
    pub async fn shutdown(&self) {
        for conn in &self.conns {
            conn.shutdown().await;
        }
    }
}

pub async fn connect(
    servers: &[McpServerConfig],
    network: NetworkPolicy,
    registry: &mut ToolRegistry,
    recorder: &mut EventRecorder,
) -> Result<McpHost> {
    let mut conns = Vec::new();
    if matches!(network, NetworkPolicy::Off) {
        return Ok(McpHost { conns });
    }

    for cfg in servers {
        match McpConnection::connect_with_timeouts(cfg, MCP_CONNECT_TIMEOUT, MCP_REQUEST_TIMEOUT)
            .await
        {
            Ok((conn, caps)) => {
                let conn = Arc::new(conn);
                if caps.tools {
                    if let Err(err) =
                        register_connection_tools(&conn, &cfg.name, cfg.trusted, registry, recorder)
                            .await
                    {
                        recorder.emit(
                            "mcp.server.failed",
                            json!({
                                "server": cfg.name,
                                "phase": "list",
                                "error": err.to_string()
                            }),
                        )?;
                    }
                }
                if caps.resources {
                    register_resource_tools(&conn, &cfg.name, registry, recorder)?;
                }
                conns.push(conn);
            }
            Err(err) => {
                recorder.emit(
                    "mcp.server.failed",
                    json!({
                        "server": cfg.name,
                        "phase": "connect",
                        "error": err.to_string()
                    }),
                )?;
            }
        }
    }

    Ok(McpHost { conns })
}

/// server 声明 resources 能力时·注册「列/读」两个只读工具（注册前查重·撞名跳+warning）。
fn register_resource_tools(
    conn: &Arc<McpConnection>,
    server: &str,
    registry: &mut ToolRegistry,
    recorder: &mut EventRecorder,
) -> Result<()> {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(McpResourceListTool::new(Arc::clone(conn), server)),
        Box::new(McpResourceReadTool::new(Arc::clone(conn), server)),
    ];
    for tool in tools {
        let full = tool.name().to_string();
        if registry.get(&full).is_some() {
            recorder.emit(
                "provider.warning",
                json!({
                    "warning": "mcp resource tool name collision",
                    "tool": full,
                    "server": server
                }),
            )?;
            continue;
        }
        registry.register(tool);
    }
    Ok(())
}

async fn register_connection_tools(
    conn: &Arc<McpConnection>,
    server: &str,
    trusted: bool,
    registry: &mut ToolRegistry,
    recorder: &mut EventRecorder,
) -> Result<()> {
    let mut cursor: Option<String> = None;
    let mut pages = 0;

    loop {
        let params = match &cursor {
            Some(cursor) => json!({"cursor": cursor}),
            None => json!({}),
        };
        let result = conn.request("tools/list", params).await?;

        if let Some(tools) = result.get("tools").and_then(Value::as_array) {
            for tool in tools {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let input_schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                let proxy = crate::mcp::tool::McpToolProxy::new(
                    Arc::clone(conn),
                    server,
                    name,
                    description,
                    input_schema,
                    trusted,
                );
                let full = proxy.name().to_string();
                if registry.get(&full).is_some() {
                    recorder.emit(
                        "provider.warning",
                        json!({
                            "warning": "mcp tool name collision",
                            "tool": full,
                            "server": server
                        }),
                    )?;
                    continue;
                }
                registry.register(Box::new(proxy));
            }
        }

        match result.get("nextCursor").and_then(Value::as_str) {
            Some(next) => {
                cursor = Some(next.to_string());
                pages += 1;
                if pages > 100 {
                    break;
                }
            }
            None => break,
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::{json, Value};

    use super::*;
    use crate::events::OutputMode;
    use crate::mcp::client::test_support::{
        connected_pair, read_json_line, write_json_line, ALL_CAPS,
    };
    use crate::provider::ToolCall;
    use crate::tools::{Tool, ToolContext, ToolOutcome};

    fn cfg(command: impl Into<String>) -> McpServerConfig {
        McpServerConfig {
            name: "srv".into(),
            command: command.into(),
            url: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            trusted: false,
        }
    }

    fn recorder(journal: &Path) -> EventRecorder {
        EventRecorder::new("r", None, None, journal, OutputMode::Silent).unwrap()
    }

    fn event_payloads(journal: &Path, event_type: &str) -> Vec<Value> {
        std::fs::read_to_string(journal)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["type"] == event_type)
            .map(|event| event["payload"].clone())
            .collect()
    }

    #[tokio::test]
    async fn mcp_host_network_off_connects_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut recorder = recorder(&journal);
        let mut registry = ToolRegistry::new();
        let servers = vec![cfg("/nonexistent/mcp-xyz")];

        let host = connect(&servers, NetworkPolicy::Off, &mut registry, &mut recorder)
            .await
            .unwrap();

        assert!(host.conns.is_empty());
        assert!(registry.get("mcp__srv__a").is_none());
        assert!(std::fs::read_to_string(&journal).unwrap().is_empty());
    }

    #[tokio::test]
    async fn mcp_host_connect_failure_emits_event() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut recorder = recorder(&journal);
        let mut registry = ToolRegistry::new();
        let servers = vec![cfg("/nonexistent/mcp-xyz")];

        let host = connect(&servers, NetworkPolicy::On, &mut registry, &mut recorder)
            .await
            .unwrap();

        assert!(host.conns.is_empty());
        assert!(registry.get("mcp__srv__a").is_none());
        let failures = event_payloads(&journal, "mcp.server.failed");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0]["server"], "srv");
        assert_eq!(failures[0]["phase"], "connect");
        assert!(failures[0]["error"].as_str().unwrap().contains("No such"));
    }

    /// A url-type (Streamable HTTP) server that cannot be reached must degrade
    /// exactly like an unreachable stdio server: emit `mcp.server.failed`
    /// (phase=connect) and let the run continue rather than aborting.
    #[tokio::test]
    async fn mcp_host_http_connect_failure_emits_event() {
        // Bind then immediately drop the listener to obtain a 127.0.0.1 port that
        // is (essentially certainly) refusing connections.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let mut server = cfg("");
        server.url = Some(format!("http://127.0.0.1:{port}/mcp"));

        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut recorder = recorder(&journal);
        let mut registry = ToolRegistry::new();

        let host = connect(&[server], NetworkPolicy::On, &mut registry, &mut recorder)
            .await
            .unwrap();

        assert!(host.conns.is_empty());
        let failures = event_payloads(&journal, "mcp.server.failed");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0]["server"], "srv");
        assert_eq!(failures[0]["phase"], "connect");
    }

    #[tokio::test]
    async fn mcp_host_register_tools_from_connection() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            assert_eq!(request["method"], "tools/list");
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {
                        "tools": [
                            {"name": "a", "inputSchema": {"type": "object"}},
                            {
                                "name": "b",
                                "description": "d",
                                "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}
                            }
                        ]
                    }
                }),
            )
            .await;
        });
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut recorder = recorder(&journal);
        let mut registry = ToolRegistry::new();

        register_connection_tools(&conn, "srv", false, &mut registry, &mut recorder)
            .await
            .unwrap();

        assert!(registry.get("mcp__srv__a").is_some());
        assert!(registry.get("mcp__srv__b").is_some());
        assert_eq!(
            registry.get("mcp__srv__b").unwrap().definition()["function"]["description"],
            "d"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_resource_register_when_capability_adds_list_and_read_tools() {
        let (conn, _reader, _writer) = connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut recorder = recorder(&journal);
        let mut registry = ToolRegistry::new();

        register_resource_tools(&conn, "srv", &mut registry, &mut recorder).unwrap();

        assert!(registry.get("mcp__srv__list_resources").is_some());
        assert!(registry.get("mcp__srv__read_resource").is_some());
        assert!(!registry.get("mcp__srv__list_resources").unwrap().mutates());
        assert!(!registry.get("mcp__srv__read_resource").unwrap().mutates());
    }

    #[tokio::test]
    async fn mcp_host_tools_list_pagination() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let first = read_json_line(&mut reader).await;
            assert_eq!(first["method"], "tools/list");
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": first["id"].clone(),
                    "result": {
                        "tools": [{"name": "a", "inputSchema": {"type": "object"}}],
                        "nextCursor": "c1"
                    }
                }),
            )
            .await;

            let second = read_json_line(&mut reader).await;
            assert_eq!(second["method"], "tools/list");
            assert_eq!(second["params"]["cursor"], "c1");
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": second["id"].clone(),
                    "result": {
                        "tools": [{"name": "b", "inputSchema": {"type": "object"}}]
                    }
                }),
            )
            .await;
        });
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut recorder = recorder(&journal);
        let mut registry = ToolRegistry::new();

        register_connection_tools(&conn, "srv", false, &mut registry, &mut recorder)
            .await
            .unwrap();

        assert!(registry.get("mcp__srv__a").is_some());
        assert!(registry.get("mcp__srv__b").is_some());
        server.await.unwrap();
    }

    struct MockTool;

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            "mcp__srv__a"
        }

        fn definition(&self) -> Value {
            json!({"type": "function", "function": {"name": "mcp__srv__a", "description": "original"}})
        }

        fn mutates(&self) -> bool {
            false
        }

        async fn execute(
            &self,
            _ctx: &mut ToolContext<'_>,
            _call: &ToolCall,
        ) -> Result<ToolOutcome> {
            Ok(ToolOutcome::success("ok".into()))
        }
    }

    #[tokio::test]
    async fn mcp_host_name_collision_skips_and_warns() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {
                        "tools": [{"name": "a", "inputSchema": {"type": "object"}}]
                    }
                }),
            )
            .await;
        });
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut recorder = recorder(&journal);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool));

        register_connection_tools(&conn, "srv", false, &mut registry, &mut recorder)
            .await
            .unwrap();

        assert_eq!(
            registry.get("mcp__srv__a").unwrap().definition()["function"]["description"],
            "original"
        );
        let warnings = event_payloads(&journal, "provider.warning");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0]["warning"], "mcp tool name collision");
        assert_eq!(warnings[0]["tool"], "mcp__srv__a");
        assert_eq!(warnings[0]["server"], "srv");
        server.await.unwrap();
    }
}
