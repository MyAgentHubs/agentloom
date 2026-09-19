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
        headers: None,
    }
}

// ─── mcp_http_headers ───

#[test]
fn mcp_http_headers_none_when_config_has_no_headers() {
    let cfg = cfg("");
    assert!(mcp_http_headers(&cfg).unwrap().is_none());
}

#[test]
fn mcp_http_headers_none_when_headers_map_is_empty() {
    let mut cfg = cfg("");
    cfg.headers = Some(BTreeMap::new());
    assert!(mcp_http_headers(&cfg).unwrap().is_none());
}

#[test]
fn mcp_http_headers_builds_map_and_expands_env_placeholder() {
    std::env::set_var("MYAGENT_TEST_CLIENT_TOKEN", "sekrit");
    let mut cfg = cfg("");
    cfg.headers = Some(BTreeMap::from([
        (
            "Authorization".to_string(),
            "Bearer ${MYAGENT_TEST_CLIENT_TOKEN}".to_string(),
        ),
        ("X-Static".to_string(), "value".to_string()),
    ]));
    let headers = mcp_http_headers(&cfg).unwrap().unwrap();
    std::env::remove_var("MYAGENT_TEST_CLIENT_TOKEN");

    assert_eq!(
        headers
            .get(&HeaderName::from_static("authorization"))
            .map(|v| v.to_str().unwrap()),
        Some("Bearer sekrit")
    );
    assert_eq!(
        headers
            .get(&HeaderName::try_from("X-Static").unwrap())
            .map(|v| v.to_str().unwrap()),
        Some("value")
    );
}

#[test]
fn mcp_http_headers_errors_when_env_placeholder_unset() {
    std::env::remove_var("MYAGENT_TEST_CLIENT_MISSING");
    let mut cfg = cfg("");
    cfg.headers = Some(BTreeMap::from([(
        "Authorization".to_string(),
        "Bearer ${MYAGENT_TEST_CLIENT_MISSING}".to_string(),
    )]));
    let err = mcp_http_headers(&cfg).unwrap_err();
    assert!(err.to_string().contains("MYAGENT_TEST_CLIENT_MISSING"));
}

#[test]
fn mcp_http_headers_rejects_invalid_header_name() {
    let mut cfg = cfg("");
    cfg.headers = Some(BTreeMap::from([(
        "bad header name".to_string(),
        "v".to_string(),
    )]));
    let err = mcp_http_headers(&cfg).unwrap_err();
    assert!(err.to_string().contains("bad header name"));
}

/// End-to-end wiring check: a url-type config with headers set must reach
/// transport construction (i.e. `mcp_http_headers` + `custom_headers` +
/// `from_config` all type-check and run) and fail with the expected
/// connect error, not a header-building panic or type error, when the
/// server is unreachable.
#[tokio::test]
async fn mcp_client_connect_with_headers_reaches_transport_and_fails_to_connect() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mut config = cfg("");
    config.url = Some(format!("http://127.0.0.1:{port}/mcp"));
    config.headers = Some(BTreeMap::from([(
        "X-Static".to_string(),
        "value".to_string(),
    )]));

    let result = McpConnection::connect(&config, Duration::from_secs(5)).await;
    let err = match result {
        Err(err) => err,
        Ok(_) => panic!("connecting to a closed port must fail"),
    };
    assert!(err.to_string().contains("mcp connect failed"), "got: {err}");
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
    let (conn, mut reader, mut writer) = connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
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
    let (conn, mut reader, mut writer) = connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
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
    let (conn, reader, writer) =
        connected_pair_with_timeouts(ALL_CAPS, Duration::from_secs(5), Duration::from_millis(20))
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
    let (conn, mut reader, mut writer) = connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
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
    let (conn, mut reader, mut writer) = connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
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
    let (conn, mut reader, mut writer) = connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
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
