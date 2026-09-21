use serde_json::Value;

use super::{
    claude_card, tool_result_text, tool_summary, truncate_output_for_locale, AgentEvent, ToolStatus,
};

pub(super) fn parse_claude_system_event(v: &Value) -> Vec<AgentEvent> {
    match v.get("subtype").and_then(Value::as_str) {
        Some("init") => match v.get("session_id").and_then(Value::as_str) {
            Some(id) => vec![AgentEvent::SessionStarted {
                conversation_id: id.to_string(),
            }],
            None => vec![],
        },
        _ => vec![],
    }
}

pub(super) fn parse_claude_stream_event(v: &Value) -> Vec<AgentEvent> {
    let Some(delta) = v
        .get("event")
        .filter(|e| e.get("type").and_then(Value::as_str) == Some("content_block_delta"))
        .and_then(|e| e.get("delta"))
    else {
        return vec![];
    };
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => match delta.get("text").and_then(Value::as_str) {
            Some(t) => vec![AgentEvent::TextDelta {
                text: t.to_string(),
            }],
            None => vec![],
        },
        _ => vec![],
    }
}

pub(super) fn parse_claude_assistant_event(v: &Value) -> Vec<AgentEvent> {
    let mut out = vec![];
    if let Some(content) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let summary = if name == "Bash" {
                    block
                        .get("input")
                        .and_then(|i| i.get("command"))
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| name.to_string())
                } else {
                    block
                        .get("input")
                        .map(|i| tool_summary(name, i))
                        .unwrap_or_else(|| name.to_string())
                };
                out.push(AgentEvent::ToolStarted {
                    id,
                    tool: name.to_string(),
                    summary,
                    card: claude_card(name),
                });
            }
            if block.get("type").and_then(Value::as_str) == Some("thinking") {
                let text = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                out.push(AgentEvent::ThinkingDelta { text });
            }
        }
    }
    let usage = v.get("message").and_then(|m| m.get("usage"));
    let input_tokens = combined_input_tokens(usage);
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(Value::as_u64);
    if input_tokens.is_some() || output_tokens.is_some() {
        out.push(AgentEvent::UsageDelta {
            input_tokens,
            output_tokens,
        });
    }
    out // 纯 text 且无 usage 的 assistant（无 tool_use）→ 空 vec
}

pub(super) fn parse_claude_user_event(v: &Value, locale: crate::Locale) -> Vec<AgentEvent> {
    let mut out = vec![];
    if let Some(content) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                let id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let output = block
                    .get("content")
                    .and_then(tool_result_text)
                    .map(|s| truncate_output_for_locale(&s, 32 * 1024, locale));
                out.push(AgentEvent::ToolCompleted {
                    id,
                    status: if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Ok
                    },
                    exit_code: None,
                    output,
                });
            }
        }
    }
    out
}

pub(super) fn parse_claude_result_event(v: &Value, locale: crate::Locale) -> Vec<AgentEvent> {
    let is_error = match v.get("is_error") {
        Some(serde_json::Value::Bool(b)) => *b,
        // is_error missing or non-bool: fail-closed--only an explicit subtype "success" counts as success
        _ => !matches!(
            v.get("subtype").and_then(serde_json::Value::as_str),
            Some("success")
        ),
    };
    if is_error {
        vec![AgentEvent::Error {
            message: v
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or(match locale {
                    crate::Locale::Zh => "未知错误",
                    crate::Locale::En => "Unknown error",
                })
                .to_string(),
        }]
    } else {
        let usage = v.get("usage");
        vec![AgentEvent::Completed {
            cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
            input_tokens: combined_input_tokens(usage),
            output_tokens: usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64),
            final_text: v
                .get("result")
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
    }
}

/// Correct the accounting for claude usage's real input-token count.
///
/// Self-check conclusion (Anthropic official documentation · platform.claude.com/docs/en/build-with-claude/prompt-caching):
/// `usage.input_tokens` counts only **uncached** input tokens--cache-hit input uses
/// `cache_read_input_tokens` (cache reads, approximately 0.1x price) and `cache_creation_input_tokens`
/// (cache writes, approximately 1.25x/2x price) as two separate fields, the three do not overlap. Documentation quote: "`input_tokens` is the
/// uncached remainder only. Total prompt size = input_tokens + cache_creation_input_tokens +
/// cache_read_input_tokens." Therefore, the true total input token count = the sum of the three, with no risk of double counting--previously
/// reading only `input_tokens` would severely underreport actual input consumption when cache-hit rates are high (for example, the clearly distorted `in=69 / out=27012`
/// figure: most input actually came from cache hits and simply was not counted).
///
/// Treat any missing field as 0 (serde tolerance, not an error); return `None` only when all three are missing (preserving
/// the existing semantic distinction between "the entire usage object is missing/has no input-side fields" and "input-side values are genuinely 0",
/// while the corresponding call site uses `is_some()` to determine whether to emit UsageDelta).
fn combined_input_tokens(usage: Option<&Value>) -> Option<u64> {
    let field = |key: &str| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
    let base = field("input_tokens");
    let cache_read = field("cache_read_input_tokens");
    let cache_creation = field("cache_creation_input_tokens");
    if base.is_none() && cache_read.is_none() && cache_creation.is_none() {
        None
    } else {
        Some(base.unwrap_or(0) + cache_read.unwrap_or(0) + cache_creation.unwrap_or(0))
    }
}
