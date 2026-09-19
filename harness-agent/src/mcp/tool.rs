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
mod tests;
