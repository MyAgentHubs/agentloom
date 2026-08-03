pub mod anthropic_compatible;
pub mod mock;
pub mod native_search;
pub mod openai_compatible;
pub(crate) mod pairing;
pub mod retry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::events::EventRecorder;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(content.into()),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(content.into()),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
            name: None,
        }
    }

    pub fn assistant(
        content: impl Into<String>,
        reasoning_content: Option<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(content.into()),
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            reasoning_content,
            name: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
            reasoning_content: None,
            name: None,
        }
    }

    /// 带 name 的 tool 消息（Kimi 回声往返要 tool_call_id + name）。
    pub fn tool_named(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
            reasoning_content: None,
            name: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    Other(String),
}

impl FinishReason {
    pub(crate) fn from_openai(value: &str) -> Self {
        match value {
            "stop" => Self::Stop,
            "length" => Self::Length,
            "tool_calls" => Self::ToolCalls,
            other => Self::Other(other.to_string()),
        }
    }

    pub(crate) fn from_anthropic(value: &str) -> Self {
        match value {
            "end_turn" => Self::Stop,
            "max_tokens" => Self::Length,
            "tool_use" => Self::ToolCalls,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub provider_id: String,
    pub model_id: String,
    pub supports_streaming: bool,
    pub supports_reasoning_deltas: bool,
    pub supports_tool_calling: bool,
    pub supports_images: bool,
    pub supports_computer_use: bool,
    pub supports_shell_tool: bool,
    pub max_context_tokens: Option<u32>,
    pub output_token_limit: Option<u32>,
    /// 这家 provider 的服务端是否有原生搜索（真·静态能力·按家族判·与网/开关无关）。
    #[serde(default)]
    pub server_side_search: bool,
}

#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        events: &mut EventRecorder,
    ) -> Result<ProviderResponse>;

    fn capabilities(&self) -> ProviderCapabilities;
}

pub fn shell_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "shell_exec",
            "description": "Run a shell command in the current workspace. Safety guard (best-effort, not a sandbox): the harness REFUSES common write/delete commands (rm/mv/cp/touch/mkdir/dd/redirects) that target paths outside the workspace, delete system paths or ~, write to .git or shell/tool startup configs (.bashrc/.zshrc/.gitconfig/.mcp.json/.claude.json/.claude/), use process substitution >(...)/<(...), or `cd` into a dir then write. Keep paths inside the workspace and use plain relative paths. If the guard refuses a command, do NOT try to work around it (e.g. via interpreter one-liners like python -c, eval, or xargs). Stop and report the refusal instead — if you believe it is wrong, say so in your report.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command line to execute. Prefer explicit commands with arguments."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory. Defaults to the runtime workspace."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Maximum runtime in milliseconds."
                    }
                },
                "required": ["command"]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assistant_carries_and_serializes_reasoning() {
        let m = ChatMessage::assistant("answer", Some("because X".into()), vec![]);
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"reasoning_content\":\"because X\""));
        let back: ChatMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(back.reasoning_content.as_deref(), Some("because X"));
    }
    #[test]
    fn user_message_omits_reasoning_field() {
        let s = serde_json::to_string(&ChatMessage::user("hi")).unwrap();
        assert!(!s.contains("reasoning_content"));
    }

    #[test]
    fn tool_named_serializes_name_and_plain_tool_omits_it() {
        let named = ChatMessage::tool_named("call_1", "$web_search", "{\"q\":\"x\"}");
        let s = serde_json::to_string(&named).unwrap();
        assert!(s.contains("\"name\":\"$web_search\""));
        assert!(s.contains("\"tool_call_id\":\"call_1\""));
        // 普通 tool 不带 name 字段（None 被 skip）
        let plain = ChatMessage::tool("call_2", "result");
        let s2 = serde_json::to_string(&plain).unwrap();
        assert!(!s2.contains("\"name\""));
    }
}
