use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::timeout;

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, PaginatedRequestParams,
    ReadResourceRequestParams,
};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::{IntoTransport, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{Peer, ServiceExt};

use crate::error::{HarnessError, Result};
use crate::exec::controlled::is_secret_env;
use crate::mcp::config::McpServerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
}

/// A single MCP client session, backed by the official rmcp Rust SDK.
///
/// The connection owns the running rmcp service (for shutdown) plus a cloned
/// `Peer` handle used to issue requests. Requests are dispatched through
/// [`McpConnection::request`], a thin JSON shim over rmcp's typed client API so
/// that the rest of the harness (host registration, tool proxies, `mcp list`)
/// keeps speaking the same `(method, params) -> Value` vocabulary it did with
/// the hand-written client.
pub struct McpConnection {
    peer: Peer<RoleClient>,
    service: Mutex<Option<RunningService<RoleClient, ClientInfo>>>,
    request_timeout: Duration,
}

impl McpConnection {
    /// Connect to an MCP server described by `cfg`, using `timeout` for both the
    /// initialize handshake and (via [`Self::connect_with_timeouts`]) every
    /// subsequent request. Callers that need the two to differ — e.g. a short
    /// connect timeout paired with a much longer per-request timeout for
    /// slow-running tools — should call [`Self::connect_with_timeouts`] directly.
    pub async fn connect(
        cfg: &McpServerConfig,
        timeout: Duration,
    ) -> Result<(Self, ServerCapabilities)> {
        Self::connect_with_timeouts(cfg, timeout, timeout).await
    }

    /// Connect to an MCP server described by `cfg`, running the initialize
    /// handshake within `connect_timeout`. A `url` config connects over
    /// Streamable HTTP; otherwise the `command` is spawned and connected over
    /// stdio. `request_timeout` bounds each subsequent request (see
    /// [`Self::request`]) and is intentionally a separate, typically much
    /// longer, budget: a request such as `tools/call` can legitimately take
    /// minutes (e.g. AgentLoom's `dispatch_worker` MCP tool waits on a worker
    /// run to finish), whereas the handshake should fail fast when the server is
    /// unreachable.
    pub async fn connect_with_timeouts(
        cfg: &McpServerConfig,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<(Self, ServerCapabilities)> {
        match cfg.url.as_deref() {
            Some(url) => {
                let transport = StreamableHttpClientTransport::from_uri(url.to_string());
                Self::serve(transport, connect_timeout, request_timeout).await
            }
            None => {
                // MCP server 命令来自用户配置/CLI，不是 agent 输入；不属于 agent 写入向量。
                let mut command = tokio::process::Command::new(&cfg.command);
                command.args(&cfg.args).env_clear();
                for (name, value) in child_env(cfg) {
                    command.env(name, value);
                }
                // stdin/stdout are wired to the transport by the builder; stderr is
                // discarded so a chatty server can never fill a pipe and stall us.
                let (transport, _stderr) = TokioChildProcess::builder(command)
                    .stderr(Stdio::null())
                    .spawn()?;
                Self::serve(transport, connect_timeout, request_timeout).await
            }
        }
    }

    /// Issue one MCP request and return its result as raw JSON in the same shape
    /// the wire protocol uses. Only the methods the harness actually calls are
    /// supported; each is routed to the matching typed rmcp client call and the
    /// typed result is serialized back to `Value`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        match timeout(self.request_timeout, self.dispatch(method, params)).await {
            Ok(result) => result,
            Err(_) => Err(HarnessError::Runtime(format!(
                "mcp request `{method}` timed out after {:?}. The request may still be \
                 running on the server — do not assume it failed or blindly retry/re-dispatch.",
                self.request_timeout
            ))),
        }
    }

    async fn dispatch(&self, method: &str, params: Value) -> Result<Value> {
        match method {
            "tools/list" => {
                let result = self
                    .peer
                    .list_tools(Some(paginated(&params)))
                    .await
                    .map_err(map_service_error)?;
                Ok(serde_json::to_value(result)?)
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = params.get("arguments").and_then(Value::as_object).cloned();
                let mut request = CallToolRequestParams::new(name);
                request.arguments = arguments;
                let result = self
                    .peer
                    .call_tool(request)
                    .await
                    .map_err(map_service_error)?;
                Ok(serde_json::to_value(result)?)
            }
            "resources/list" => {
                let result = self
                    .peer
                    .list_resources(Some(paginated(&params)))
                    .await
                    .map_err(map_service_error)?;
                Ok(serde_json::to_value(result)?)
            }
            "resources/read" => {
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let result = self
                    .peer
                    .read_resource(ReadResourceRequestParams::new(uri))
                    .await
                    .map_err(map_service_error)?;
                Ok(serde_json::to_value(result)?)
            }
            "prompts/list" => {
                let result = self
                    .peer
                    .list_prompts(Some(paginated(&params)))
                    .await
                    .map_err(map_service_error)?;
                Ok(serde_json::to_value(result)?)
            }
            other => Err(HarnessError::Runtime(format!(
                "unsupported mcp method `{other}`"
            ))),
        }
    }

    /// Gracefully shut the session down: cancel the rmcp service, which closes
    /// the transport (stdin EOF for stdio) and waits for the child to exit before
    /// killing it. Idempotent — a second call is a no-op.
    pub async fn shutdown(&self) {
        let running = self.service.lock().await.take();
        if let Some(running) = running {
            let _ = running.cancel().await;
        }
    }

    async fn serve<T, E, A>(
        transport: T,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<(Self, ServerCapabilities)>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let running = match timeout(connect_timeout, client_info().serve(transport)).await {
            Ok(Ok(running)) => running,
            Ok(Err(err)) => {
                return Err(HarnessError::Runtime(format!("mcp connect failed: {err}")))
            }
            Err(_) => {
                return Err(HarnessError::Runtime(format!(
                    "mcp initialize timed out after {connect_timeout:?}"
                )))
            }
        };

        let capabilities = server_capabilities(&running);
        let peer = running.peer().clone();
        Ok((
            Self {
                peer,
                service: Mutex::new(Some(running)),
                request_timeout,
            },
            capabilities,
        ))
    }

    /// Test-only: connect over an arbitrary in-memory transport (e.g. a duplex
    /// stream pair) so unit tests can drive a mock server without spawning a
    /// child process.
    #[cfg(test)]
    pub(crate) async fn connect_transport<T, E, A>(
        transport: T,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<(Self, ServerCapabilities)>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::serve(transport, connect_timeout, request_timeout).await
    }
}

fn client_info() -> ClientInfo {
    ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("myagent", env!("CARGO_PKG_VERSION")),
    )
}

fn paginated(params: &Value) -> PaginatedRequestParams {
    let cursor = params
        .get("cursor")
        .and_then(Value::as_str)
        .map(str::to_string);
    PaginatedRequestParams::default().with_cursor(cursor)
}

fn server_capabilities(running: &RunningService<RoleClient, ClientInfo>) -> ServerCapabilities {
    match running.peer_info() {
        Some(info) => ServerCapabilities {
            tools: info.capabilities.tools.is_some(),
            resources: info.capabilities.resources.is_some(),
            prompts: info.capabilities.prompts.is_some(),
        },
        None => ServerCapabilities {
            tools: false,
            resources: false,
            prompts: false,
        },
    }
}

/// Preserve the historical error text `mcp JSON-RPC error <code>: <message>` so
/// downstream callers and tests that key off the code / message keep working.
fn map_service_error(err: ServiceError) -> HarnessError {
    if let ServiceError::McpError(ref data) = err {
        return HarnessError::Runtime(format!(
            "mcp JSON-RPC error {}: {}",
            data.code.0, data.message
        ));
    }
    HarnessError::Runtime(format!("mcp request failed: {err}"))
}

fn child_env(cfg: &McpServerConfig) -> Vec<(String, String)> {
    let mut env = BTreeMap::new();
    for name in ["PATH", "HOME", "LANG"] {
        if is_secret_env(name) {
            continue;
        }
        if let Ok(value) = std::env::var(name) {
            env.insert(name.to_string(), value);
        }
    }

    for (name, value) in &cfg.env {
        env.insert(name.clone(), value.clone());
    }

    env.into_iter().collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared helpers for driving a mock MCP server over an in-memory duplex
    //! stream. Every rmcp connection runs the initialize handshake up front, so
    //! a mock server must answer it (via [`server_handshake`]) before handling
    //! any test-specific method.

    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{json, Value};
    use tokio::io::{
        AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream,
    };

    use super::{McpConnection, ServerCapabilities};

    pub const ALL_CAPS: ServerCapabilities = ServerCapabilities {
        tools: true,
        resources: true,
        prompts: true,
    };

    pub async fn read_json_line<R>(reader: &mut BufReader<R>) -> Value
    where
        R: AsyncRead + Unpin,
    {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line).await.unwrap();
        assert!(n > 0, "expected a JSON line");
        serde_json::from_slice(&line).unwrap()
    }

    pub async fn write_json_line<W>(writer: &mut W, value: Value)
    where
        W: AsyncWrite + Unpin,
    {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        writer.write_all(&bytes).await.unwrap();
        writer.flush().await.unwrap();
    }

    /// Answer the rmcp initialize handshake with the given capabilities, echoing
    /// `protocol_version` back to the client. Consumes the initialize request and
    /// the following `notifications/initialized` message.
    pub async fn server_handshake<R, W>(
        reader: &mut BufReader<R>,
        writer: &mut W,
        caps: ServerCapabilities,
        protocol_version: &str,
    ) where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let request = read_json_line(reader).await;
        assert_eq!(request["method"], "initialize");
        let mut capabilities = serde_json::Map::new();
        if caps.tools {
            capabilities.insert("tools".into(), json!({}));
        }
        if caps.resources {
            capabilities.insert("resources".into(), json!({}));
        }
        if caps.prompts {
            capabilities.insert("prompts".into(), json!({}));
        }
        write_json_line(
            writer,
            json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": {
                    "protocolVersion": protocol_version,
                    "capabilities": capabilities,
                    "serverInfo": {"name": "mock", "version": "0"}
                }
            }),
        )
        .await;
        let initialized = read_json_line(reader).await;
        assert_eq!(initialized["method"], "notifications/initialized");
    }

    /// Build a connected [`McpConnection`] over a fresh duplex pair whose server
    /// side has already completed the handshake advertising `caps`. Returns the
    /// connection plus the server-side reader/writer positioned right after the
    /// handshake, ready to handle a test-specific method. Uses `timeout` for
    /// both the connect handshake and every subsequent request; see
    /// [`connected_pair_with_timeouts`] when a test needs the two to differ.
    pub async fn connected_pair(
        caps: ServerCapabilities,
        timeout: Duration,
    ) -> (Arc<McpConnection>, BufReader<DuplexStream>, DuplexStream) {
        connected_pair_with_timeouts(caps, timeout, timeout).await
    }

    /// Like [`connected_pair`] but with independently controllable connect vs.
    /// request timeouts, so a test can e.g. give the handshake a generous
    /// budget while forcing a short request timeout (or vice versa).
    pub async fn connected_pair_with_timeouts(
        caps: ServerCapabilities,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> (Arc<McpConnection>, BufReader<DuplexStream>, DuplexStream) {
        let (client_read, server_write) = tokio::io::duplex(4096);
        let (server_read, client_write) = tokio::io::duplex(4096);
        let handshake = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            server_handshake(&mut reader, &mut writer, caps, "2025-11-25").await;
            (reader, writer)
        });
        let (conn, _caps) = McpConnection::connect_transport(
            (client_read, client_write),
            connect_timeout,
            request_timeout,
        )
        .await
        .unwrap();
        let (reader, writer) = handshake.await.unwrap();
        (Arc::new(conn), reader, writer)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write as _;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncWriteExt, BufReader};

    use super::test_support::{
        connected_pair, connected_pair_with_timeouts, read_json_line, server_handshake,
        write_json_line, ALL_CAPS,
    };
    use super::*;

    fn cfg(command: impl Into<String>) -> McpServerConfig {
        McpServerConfig {
            name: "test".into(),
            command: command.into(),
            url: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            trusted: false,
        }
    }

    #[tokio::test]
    async fn mcp_client_initialize_sends_client_info_and_parses_capabilities() {
        let (client_read, mut server_write) = tokio::io::duplex(4096);
        let (server_read, client_write) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let request = read_json_line(&mut reader).await;
            assert_eq!(request["jsonrpc"], "2.0");
            assert_eq!(request["method"], "initialize");
            assert_eq!(request["params"]["clientInfo"]["name"], "myagent");
            write_json_line(
                &mut server_write,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                        "serverInfo": {"name": "mock", "version": "0"}
                    }
                }),
            )
            .await;
            let initialized = read_json_line(&mut reader).await;
            assert_eq!(initialized["method"], "notifications/initialized");
        });

        let (_conn, caps) = McpConnection::connect_transport(
            (client_read, client_write),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(
            caps,
            ServerCapabilities {
                tools: true,
                resources: true,
                prompts: true
            }
        );
        server.await.unwrap();
    }

    // The rmcp client negotiates the protocol version instead of pinning one, so
    // a server that answers with an older (but valid) version now connects rather
    // than being rejected. This is the intentional behaviour that lets us talk to
    // the AgentLoom app server (which speaks 2025-06-18).
    #[tokio::test]
    async fn mcp_client_accepts_server_negotiated_protocol_version() {
        let (client_read, server_write) = tokio::io::duplex(4096);
        let (server_read, client_write) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            server_handshake(&mut reader, &mut writer, ALL_CAPS, "2025-06-18").await;
        });

        let (_conn, caps) = McpConnection::connect_transport(
            (client_read, client_write),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .expect("server advertising 2025-06-18 should connect");
        assert!(caps.tools);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_client_request_returns_result() {
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
                    "result": {"tools": []}
                }),
            )
            .await;
        });

        let result = conn.request("tools/list", json!({})).await.unwrap();
        assert_eq!(result["tools"], json!([]));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_client_concurrent_requests_each_get_own_response() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        // Server reads two tool calls and responds out of order; rmcp must route
        // each response back to the request that owns its id.
        let server = tokio::spawn(async move {
            let first = read_json_line(&mut reader).await;
            let second = read_json_line(&mut reader).await;
            for request in [second, first] {
                let marker = request["params"]["arguments"]["marker"].clone();
                write_json_line(
                    &mut writer,
                    json!({
                        "jsonrpc": "2.0",
                        "id": request["id"].clone(),
                        "result": {"content": [{"type": "text", "text": marker}]}
                    }),
                )
                .await;
            }
        });

        let first_conn = Arc::clone(&conn);
        let first = tokio::spawn(async move {
            first_conn
                .request(
                    "tools/call",
                    json!({"name": "t", "arguments": {"marker": "first"}}),
                )
                .await
        });
        let second_conn = Arc::clone(&conn);
        let second = tokio::spawn(async move {
            second_conn
                .request(
                    "tools/call",
                    json!({"name": "t", "arguments": {"marker": "second"}}),
                )
                .await
        });

        let (first, second) = tokio::join!(first, second);
        assert_eq!(
            first.unwrap().unwrap()["content"][0]["text"],
            json!("first")
        );
        assert_eq!(
            second.unwrap().unwrap()["content"][0]["text"],
            json!("second")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_client_request_times_out_when_server_is_silent() {
        let (conn, reader, writer) = connected_pair(ALL_CAPS, Duration::from_millis(20)).await;
        let server = tokio::spawn(async move {
            let _keep_open = (reader, writer);
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let err = conn.request("tools/list", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("timed out"));
        server.await.unwrap();
    }

    /// Pins the connect/request timeout split: a request must time out using
    /// `request_timeout`, never the (here, far longer) `connect_timeout` used
    /// only for the initial handshake. Before the split both were the same
    /// stored `timeout` field, so a slow-running request (e.g. AgentLoom's
    /// `dispatch_worker` MCP tool, which can legitimately run for minutes) would
    /// have inherited whatever short value the connect timeout happened to use.
    #[tokio::test]
    async fn mcp_client_request_timeout_is_independent_of_connect_timeout() {
        let (conn, reader, writer) = connected_pair_with_timeouts(
            ALL_CAPS,
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(conn.request_timeout, Duration::from_millis(20));

        let server = tokio::spawn(async move {
            // Never answers the request — the connection is otherwise healthy
            // (handshake already completed), only the request itself stalls.
            let _keep_open = (reader, writer);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let started = std::time::Instant::now();
        let err = conn.request("tools/list", json!({})).await.unwrap_err();
        let elapsed = started.elapsed();

        assert!(err.to_string().contains("timed out"));
        assert!(
            elapsed < Duration::from_secs(1),
            "request must time out using the 20ms request_timeout, not the 5s \
             connect_timeout (took {elapsed:?})"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_client_request_skips_bad_stdout_line_then_returns_result() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            writer.write_all(b"not json\n").await.unwrap();
            writer.flush().await.unwrap();
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {"tools": []}
                }),
            )
            .await;
        });

        let result = conn.request("tools/list", json!({})).await.unwrap();
        assert_eq!(result["tools"], json!([]));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_client_protocol_error_includes_code_and_message() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "error": {"code": -32001, "message": "bad tool"}
                }),
            )
            .await;
        });

        let err = conn
            .request("tools/call", json!({"name": "t"}))
            .await
            .unwrap_err();
        let err = err.to_string();
        assert!(err.contains("-32001"), "got: {err}");
        assert!(err.contains("bad tool"), "got: {err}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_client_ignores_interleaved_notification_and_matches_response() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            // A server-initiated notification arriving before the response must
            // not be mistaken for the response.
            write_json_line(
                &mut writer,
                json!({"jsonrpc": "2.0", "method": "notifications/progress"}),
            )
            .await;
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {"tools": [{"name": "matched", "inputSchema": {"type": "object"}}]}
                }),
            )
            .await;
        });

        let result = conn.request("tools/list", json!({})).await.unwrap();
        assert_eq!(result["tools"][0]["name"], json!("matched"));
        server.await.unwrap();
    }

    #[test]
    fn mcp_client_env_filters_parent_secrets_and_includes_baseline_and_cfg_env() {
        struct EnvGuard(&'static str);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var(self.0);
            }
        }

        std::env::set_var("MYAGENT_TASK2_API_KEY", "leak");
        let _guard = EnvGuard("MYAGENT_TASK2_API_KEY");

        let mut cfg = cfg("mcp-server");
        cfg.env.insert("EXPLICIT_ENV".into(), "present".into());
        let env: BTreeMap<_, _> = child_env(&cfg).into_iter().collect();

        assert!(env.contains_key("PATH"));
        assert_eq!(env.get("EXPLICIT_ENV"), Some(&"present".to_string()));
        assert!(!env.contains_key("MYAGENT_TASK2_API_KEY"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_client_spawn_smoke_connects_and_shutdown_returns() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mcp-smoke.sh");
        let mut file = fs::File::create(&script).unwrap();
        writeln!(
            file,
            "#!/bin/sh\nIFS= read -r request\nprintf 'stderr noise\\n' >&2\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{{}},\"serverInfo\":{{\"name\":\"smoke\",\"version\":\"0\"}}}}}}'\nIFS= read -r initialized\nexit 0"
        )
        .unwrap();
        drop(file);
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let cfg = cfg(script.to_string_lossy());
        // 5s (was 1s) so process spawn + handshake stays comfortably inside the
        // window even under heavily parallel test runs — the assertions below are
        // unchanged.
        let (conn, caps) = McpConnection::connect(&cfg, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(
            caps,
            ServerCapabilities {
                tools: false,
                resources: false,
                prompts: false
            }
        );
        conn.shutdown().await;
    }
}
