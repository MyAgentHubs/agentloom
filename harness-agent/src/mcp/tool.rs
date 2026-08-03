use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::Result;
use crate::mcp::client::McpConnection;
use crate::provider::ToolCall;
use crate::tools::{emit_tool_failed, Tool, ToolContext, ToolOutcome};

pub struct McpToolProxy {
    conn: Arc<McpConnection>,
    server: String,
    tool_name: String,
    full_name: String,
    description: Option<String>,
    input_schema: Value,
    trusted: bool,
}

impl McpToolProxy {
    pub fn new(
        conn: Arc<McpConnection>,
        server: impl Into<String>,
        tool_name: impl Into<String>,
        description: Option<String>,
        input_schema: Value,
        trusted: bool,
    ) -> Self {
        let server = server.into();
        let tool_name = tool_name.into();
        let full_name = sanitize_full_name(&server, &tool_name);
        Self {
            conn,
            server,
            tool_name,
            full_name,
            description,
            input_schema,
            trusted,
        }
    }
}

#[async_trait]
impl Tool for McpToolProxy {
    fn name(&self) -> &str {
        debug_assert_eq!(
            self.full_name,
            sanitize_full_name(&self.server, &self.tool_name)
        );
        &self.full_name
    }

    fn definition(&self) -> Value {
        let parameters = if self.input_schema.is_object() {
            self.input_schema.clone()
        } else {
            json!({"type": "object", "properties": {}})
        };
        json!({
            "type": "function",
            "function": {
                "name": self.full_name,
                "description": self.description.clone().unwrap_or_default(),
                "parameters": parameters
            }
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    fn guardrail_trusted(&self) -> bool {
        self.trusted
    }

    fn is_mcp(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let arguments: Value =
            serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
        match self
            .conn
            .request(
                "tools/call",
                json!({"name": self.tool_name, "arguments": arguments}),
            )
            .await
        {
            Err(err) => {
                let msg = err.to_string();
                emit_tool_failed(ctx.recorder, &self.full_name, &call.id, &msg)?;
                Ok(ToolOutcome::recoverable(json!({"error": msg}).to_string()))
            }
            Ok(result) => {
                if matches!(result.get("isError"), Some(Value::Bool(true))) {
                    let msg = convert_tool_result(&result);
                    emit_tool_failed(ctx.recorder, &self.full_name, &call.id, &msg)?;
                    Ok(ToolOutcome::recoverable(msg))
                } else {
                    Ok(ToolOutcome::success_mutating(convert_tool_result(&result)))
                }
            }
        }
    }
}

fn sanitize_full_name(server: &str, tool: &str) -> String {
    let raw = format!("mcp__{server}__{tool}");
    let mut sanitized: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.len() > 64 {
        sanitized.truncate(64);
    }
    sanitized
}

fn convert_tool_result(result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                Some("resource_link") => {
                    if let Some(uri) = item.get("uri").and_then(Value::as_str) {
                        parts.push(uri.to_string());
                    }
                }
                Some("image") => parts.push("[image content omitted]".to_string()),
                Some("audio") => parts.push("[audio content omitted]".to_string()),
                Some("resource") => parts.push("[resource content omitted]".to_string()),
                Some(kind) => parts.push(format!("[{kind} content omitted]")),
                None => {}
            }
        }
    }
    if let Some(structured) = result.get("structuredContent") {
        parts.push(structured.to_string());
    }
    parts.join("\n")
}

/// 资源「列」工具 `mcp__<server>__list_resources`（只读·不过审批门）。
/// 返回的 uri/name 是 server 给的数据·标不可信·只显示不升格为指令。
pub struct McpResourceListTool {
    conn: Arc<McpConnection>,
    server: String,
    full_name: String,
}

impl McpResourceListTool {
    pub fn new(conn: Arc<McpConnection>, server: impl Into<String>) -> Self {
        let server = server.into();
        let full_name = sanitize_full_name(&server, "list_resources");
        Self {
            conn,
            server,
            full_name,
        }
    }
}

#[async_trait]
impl Tool for McpResourceListTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.full_name,
                "description": format!("List readable resources exposed by MCP server `{}`.", self.server),
                "parameters": {"type": "object", "properties": {}}
            }
        })
    }

    fn mutates(&self) -> bool {
        false
    }

    fn is_mcp(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        match list_all_resources(&self.conn).await {
            Ok(text) => Ok(ToolOutcome::success(text)),
            Err(err) => {
                let msg = err.to_string();
                emit_tool_failed(ctx.recorder, &self.full_name, &call.id, &msg)?;
                Ok(ToolOutcome::recoverable(
                    json!({ "error": msg }).to_string(),
                ))
            }
        }
    }
}

/// 资源「读」工具 `mcp__<server>__read_resource(uri)`（只读·不过审批门）。
pub struct McpResourceReadTool {
    conn: Arc<McpConnection>,
    server: String,
    full_name: String,
}

impl McpResourceReadTool {
    pub fn new(conn: Arc<McpConnection>, server: impl Into<String>) -> Self {
        let server = server.into();
        let full_name = sanitize_full_name(&server, "read_resource");
        Self {
            conn,
            server,
            full_name,
        }
    }
}

#[async_trait]
impl Tool for McpResourceReadTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.full_name,
                "description": format!("Read one resource from MCP server `{}` by uri.", self.server),
                "parameters": {
                    "type": "object",
                    "properties": {"uri": {"type": "string"}},
                    "required": ["uri"]
                }
            }
        })
    }

    fn mutates(&self) -> bool {
        false
    }

    fn is_mcp(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
        let arguments: Value =
            serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
        let uri = arguments.get("uri").and_then(Value::as_str).unwrap_or("");
        match self
            .conn
            .request("resources/read", json!({ "uri": uri }))
            .await
        {
            Ok(result) => Ok(ToolOutcome::success(convert_resource_contents(&result))),
            Err(err) => {
                let msg = err.to_string();
                emit_tool_failed(ctx.recorder, &self.full_name, &call.id, &msg)?;
                Ok(ToolOutcome::recoverable(
                    json!({ "error": msg }).to_string(),
                ))
            }
        }
    }
}

/// 拉 `resources/list`（nextCursor 续拉）·每条转成 `uri (name)` 行。
async fn list_all_resources(conn: &Arc<McpConnection>) -> Result<String> {
    let mut lines = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let params = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        };
        let result = conn.request("resources/list", params).await?;
        if let Some(resources) = result.get("resources").and_then(Value::as_array) {
            for resource in resources {
                let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
                if uri.is_empty() {
                    continue;
                }
                match resource.get("name").and_then(Value::as_str) {
                    Some(name) => lines.push(format!("{uri} ({name})")),
                    None => lines.push(uri.to_string()),
                }
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
    Ok(lines.join("\n"))
}

/// `resources/read` 的 contents[] → 字符串：text 拼接·blob 占位。
fn convert_resource_contents(result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(contents) = result.get("contents").and_then(Value::as_array) {
        for item in contents {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
            } else if item.get("blob").is_some() {
                parts.push("[binary content omitted]".to_string());
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{json, Value};

    use super::*;
    use crate::events::{EventRecorder, OutputMode};
    use crate::goal::NetworkPolicy;
    use crate::mcp::client::test_support::{
        connected_pair, read_json_line, write_json_line, ALL_CAPS,
    };
    use crate::mcp::client::McpConnection;
    use crate::provider::{FunctionCall, ToolCall};
    use crate::tools::{Tool, ToolContext, ToolStatus};

    async fn proxy_with_schema(input_schema: Value, trusted: bool) -> McpToolProxy {
        McpToolProxy::new(
            dummy_conn().await,
            "srv.name",
            "tool.name",
            Some("tool description".into()),
            input_schema,
            trusted,
        )
    }

    /// A connection whose mock server only completes the handshake, for tests
    /// that never issue a request (definition / name / mutates checks).
    async fn dummy_conn() -> Arc<McpConnection> {
        let (conn, _reader, _writer) = connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        conn
    }

    fn tool_call(args: Value) -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "mcp__srv__tool".into(),
                arguments: serde_json::to_string(&args).unwrap(),
            },
        }
    }

    fn journal_contains(journal: &std::path::Path, needle: &str) -> bool {
        std::fs::read_to_string(journal).unwrap().contains(needle)
    }

    #[test]
    fn mcp_proxy_name_sanitize_replaces_dot_and_illegal_chars() {
        assert_eq!(
            sanitize_full_name("server.name", "tool/name + x"),
            "mcp__server_name__tool_name___x"
        );
    }

    #[test]
    fn mcp_proxy_name_sanitize_truncates_to_64_chars() {
        let full_name = sanitize_full_name(
            "server_with_a_very_long_name_that_would_overflow",
            "tool_with_a_very_long_name_that_would_overflow",
        );

        assert!(full_name.len() <= 64);
        assert!(full_name.starts_with("mcp__server_with"));
    }

    #[tokio::test]
    async fn mcp_proxy_definition_wraps_schema_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "q": { "type": "string" }
            },
            "required": ["q"]
        });
        let proxy = proxy_with_schema(schema.clone(), false).await;

        let definition = proxy.definition();

        assert_eq!(definition["type"], "function");
        assert_eq!(definition["function"]["name"], "mcp__srv_name__tool_name");
        assert_eq!(definition["function"]["description"], "tool description");
        assert_eq!(definition["function"]["parameters"], schema);
    }

    #[tokio::test]
    async fn mcp_proxy_definition_wraps_schema_defaults_non_object() {
        let proxy = McpToolProxy::new(dummy_conn().await, "srv", "tool", None, Value::Null, false);

        let definition = proxy.definition();

        assert_eq!(definition["type"], "function");
        assert_eq!(definition["function"]["name"], "mcp__srv__tool");
        assert_eq!(definition["function"]["description"], "");
        assert_eq!(
            definition["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[tokio::test]
    async fn mcp_proxy_mutates_and_trusted_reflects_constructor() {
        let trusted = proxy_with_schema(json!({"type": "object"}), true).await;
        let untrusted = proxy_with_schema(json!({"type": "object"}), false).await;

        assert!(trusted.mutates());
        assert!(trusted.guardrail_trusted());
        assert!(untrusted.mutates());
        assert!(!untrusted.guardrail_trusted());
    }

    #[tokio::test]
    async fn mcp_proxy_and_resource_tools_report_is_mcp_true() {
        // K1：run_loop 靠 is_mcp() 识别「这是 MCP 工具」，不靠名字前缀猜。
        let proxy = proxy_with_schema(json!({"type": "object"}), false).await;
        assert!(proxy.is_mcp());

        let list_tool = McpResourceListTool::new(dummy_conn().await, "srv");
        assert!(list_tool.is_mcp());

        let read_tool = McpResourceReadTool::new(dummy_conn().await, "srv");
        assert!(read_tool.is_mcp());
    }

    #[test]
    fn mcp_proxy_convert_text_blocks_and_resource_links() {
        let result = json!({
            "content": [
                { "type": "text", "text": "one" },
                { "type": "resource_link", "uri": "file:///tmp/a.txt" },
                { "type": "text", "text": "two" }
            ]
        });

        assert_eq!(convert_tool_result(&result), "one\nfile:///tmp/a.txt\ntwo");
    }

    #[test]
    fn mcp_proxy_convert_media_and_embedded_resource_placeholders() {
        let result = json!({
            "content": [
                { "type": "image", "data": "..." },
                { "type": "audio", "data": "..." },
                { "type": "resource", "resource": {} },
                { "type": "custom", "value": true }
            ]
        });

        assert_eq!(
            convert_tool_result(&result),
            "[image content omitted]\n[audio content omitted]\n[resource content omitted]\n[custom content omitted]"
        );
    }

    #[test]
    fn mcp_proxy_convert_structured_content_appends_json() {
        let result = json!({
            "content": [
                { "type": "text", "text": "visible" }
            ],
            "structuredContent": {
                "answer": 42
            }
        });

        assert_eq!(convert_tool_result(&result), "visible\n{\"answer\":42}");
    }

    #[test]
    fn mcp_proxy_convert_structured_content_without_content_returns_json() {
        let result = json!({
            "structuredContent": {
                "answer": 42
            }
        });

        assert_eq!(convert_tool_result(&result), "{\"answer\":42}");
    }

    #[tokio::test]
    async fn mcp_proxy_execute_success_calls_tools_call_and_returns_content() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            assert_eq!(request["jsonrpc"], "2.0");
            assert_eq!(request["method"], "tools/call");
            assert_eq!(request["params"]["name"], "tool.raw");
            assert_eq!(request["params"]["arguments"], json!({"q": "needle"}));

            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {
                        "content": [
                            { "type": "text", "text": "hi" }
                        ]
                    }
                }),
            )
            .await;
        });
        let proxy = McpToolProxy::new(
            conn,
            "srv",
            "tool.raw",
            None,
            json!({"type": "object"}),
            false,
        );
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("e.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let call = tool_call(json!({"q": "needle"}));
        let mut ledger = crate::file_ledger::FileLedger::new();

        let outcome = {
            let mut ctx = ToolContext {
                workspace: workspace.path(),
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: NetworkPolicy::On,
                fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            };
            proxy.execute(&mut ctx, &call).await.unwrap()
        };

        assert_eq!(outcome.status, ToolStatus::Success);
        assert!(outcome.invalidates_verification);
        assert_eq!(outcome.content, "hi");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_proxy_execute_request_err_inband_emits_tool_failed() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "error": {
                        "code": -32000,
                        "message": "transport exploded"
                    }
                }),
            )
            .await;
        });
        let proxy = McpToolProxy::new(conn, "srv", "tool", None, json!({"type": "object"}), false);
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("e.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let call = tool_call(json!({"q": "needle"}));
        let mut ledger = crate::file_ledger::FileLedger::new();

        let outcome = {
            let mut ctx = ToolContext {
                workspace: workspace.path(),
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: NetworkPolicy::On,
                fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            };
            proxy.execute(&mut ctx, &call).await.unwrap()
        };

        assert_eq!(outcome.status, ToolStatus::FailedRecoverable);
        assert!(outcome.content.contains("transport exploded"));
        assert!(journal_contains(&journal, "\"type\":\"tool.failed\""));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_proxy_execute_iserror_inband_emits_tool_failed() {
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
                        "content": [
                            { "type": "text", "text": "boom" }
                        ],
                        "isError": true
                    }
                }),
            )
            .await;
        });
        let proxy = McpToolProxy::new(conn, "srv", "tool", None, json!({"type": "object"}), false);
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("e.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let call = tool_call(json!({"q": "needle"}));
        let mut ledger = crate::file_ledger::FileLedger::new();

        let outcome = {
            let mut ctx = ToolContext {
                workspace: workspace.path(),
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: NetworkPolicy::On,
                fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            };
            proxy.execute(&mut ctx, &call).await.unwrap()
        };

        assert_eq!(outcome.status, ToolStatus::FailedRecoverable);
        assert!(outcome.content.contains("boom"));
        assert!(journal_contains(&journal, "\"type\":\"tool.failed\""));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_resource_list_returns_resources_and_not_mutating() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            assert_eq!(request["method"], "resources/list");
            write_json_line(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": { "resources": [
                        { "uri": "file:///a.txt", "name": "A" },
                        { "uri": "file:///b.txt", "name": "B" }
                    ] }
                }),
            )
            .await;
        });
        let tool = McpResourceListTool::new(conn, "srv");
        assert!(!tool.mutates());
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("e.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let call = tool_call(json!({}));
        let mut ledger = crate::file_ledger::FileLedger::new();
        let outcome = {
            let mut ctx = ToolContext {
                workspace: workspace.path(),
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: NetworkPolicy::On,
                fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            };
            tool.execute(&mut ctx, &call).await.unwrap()
        };
        assert_eq!(outcome.status, ToolStatus::Success);
        assert!(outcome.content.contains("file:///a.txt (A)"));
        assert!(outcome.content.contains("file:///b.txt"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_resource_list_pagination_follows_cursor() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let first = read_json_line(&mut reader).await;
            write_json_line(
                &mut writer,
                json!({"jsonrpc":"2.0","id":first["id"].clone(),"result":{"resources":[{"uri":"file:///a","name":"a"}],"nextCursor":"c1"}}),
            )
            .await;
            let second = read_json_line(&mut reader).await;
            assert_eq!(second["params"]["cursor"], "c1");
            write_json_line(
                &mut writer,
                json!({"jsonrpc":"2.0","id":second["id"].clone(),"result":{"resources":[{"uri":"file:///b","name":"b"}]}}),
            )
            .await;
        });
        let tool = McpResourceListTool::new(conn, "srv");
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("e.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let call = tool_call(json!({}));
        let mut ledger = crate::file_ledger::FileLedger::new();
        let outcome = {
            let mut ctx = ToolContext {
                workspace: workspace.path(),
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: NetworkPolicy::On,
                fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            };
            tool.execute(&mut ctx, &call).await.unwrap()
        };
        assert!(outcome.content.contains("file:///a"));
        assert!(outcome.content.contains("file:///b"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_resource_read_returns_text_contents() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            assert_eq!(request["method"], "resources/read");
            assert_eq!(request["params"]["uri"], "file:///a.txt");
            write_json_line(
                &mut writer,
                json!({"jsonrpc":"2.0","id":request["id"].clone(),"result":{"contents":[{"uri":"file:///a.txt","text":"hello world"}]}}),
            )
            .await;
        });
        let tool = McpResourceReadTool::new(conn, "srv");
        assert!(!tool.mutates());
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("e.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let call = tool_call(json!({"uri":"file:///a.txt"}));
        let mut ledger = crate::file_ledger::FileLedger::new();
        let outcome = {
            let mut ctx = ToolContext {
                workspace: workspace.path(),
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: NetworkPolicy::On,
                fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            };
            tool.execute(&mut ctx, &call).await.unwrap()
        };
        assert_eq!(outcome.status, ToolStatus::Success);
        assert_eq!(outcome.content, "hello world");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mcp_resource_read_blob_uses_placeholder() {
        let (conn, mut reader, mut writer) =
            connected_pair(ALL_CAPS, Duration::from_millis(500)).await;
        let server = tokio::spawn(async move {
            let request = read_json_line(&mut reader).await;
            write_json_line(
                &mut writer,
                json!({"jsonrpc":"2.0","id":request["id"].clone(),"result":{"contents":[{"uri":"file:///img.png","blob":"AAAA"}]}}),
            )
            .await;
        });
        let tool = McpResourceReadTool::new(conn, "srv");
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("e.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, OutputMode::Silent).unwrap();
        let call = tool_call(json!({"uri":"file:///img.png"}));
        let mut ledger = crate::file_ledger::FileLedger::new();
        let outcome = {
            let mut ctx = ToolContext {
                workspace: workspace.path(),
                recorder: &mut recorder,
                file_ledger: &mut ledger,
                network: NetworkPolicy::On,
                fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
            };
            tool.execute(&mut ctx, &call).await.unwrap()
        };
        assert!(outcome.content.contains("[binary content omitted]"));
        server.await.unwrap();
    }
}
