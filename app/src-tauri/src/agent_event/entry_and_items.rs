use super::*;

/// Extract the long-task trailer from worker final text. Tolerate Markdown fences, compact or
/// pretty JSON, and trailing prose by testing balanced objects from the last opening brace backward.
/// A worker that quotes this protocol as an example can produce a false positive. Preferring the
/// trailing object and requiring `status=="incomplete"` reduce that risk; the remaining outcome is
/// only a reversible soft marker, so worker prose is not constrained further.
pub fn parse_requires_long_task(final_text: &str) -> Option<RequiresLongTask> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        status: String,
        requires_long_task: RequiresLongTask,
    }
    let cleaned: String = final_text
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");
    let opens: Vec<usize> = cleaned.match_indices('{').map(|(i, _)| i).collect();
    for &start in opens.iter().rev() {
        if let Some(end) = balanced_brace_end(&cleaned, start) {
            if let Ok(env) = serde_json::from_str::<Envelope>(&cleaned[start..=end]) {
                if env.status == "incomplete" {
                    return Some(env.requires_long_task);
                }
            }
        }
    }
    None
}

/// Find the byte index of the balanced closing brace from `start`, ignoring braces and escapes inside strings.
fn balanced_brace_end(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Mark the soft outcome only when a successful worker includes the trailer in its final text.
/// Early returns keep the control flow flat and avoid nested conditional warnings.
pub fn maybe_mark_long_task(
    result: &mut MemberResult,
    status: StatusTransition,
    final_text: Option<&str>,
) {
    if !matches!(status, StatusTransition::Done) {
        return;
    }
    let Some(ft) = final_text else { return };
    if let Some(rlt) = parse_requires_long_task(ft) {
        result.requires_long_task = Some(rlt);
    }
}

/// One acceptance criterion shared by goal event snapshots and persisted goal blocks.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GoalCriterion {
    pub id: String,
    pub claim: String,
    pub verifier: Option<String>,
    pub evidence: Option<String>,
    /// 'pending' | 'passed' | 'failed' | 'waived'
    pub status: String,
    /// 'run' | 'task'
    pub scope: String,
}

/// One proposed scope, objective, or constraint change from `run.needs_decision`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScopeChange {
    pub proposal_id: String,
    pub kind: String,
    pub detail_text: String,
    pub detail_summary: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GoalCriterionUpdate {
    pub id: String,
    pub status: String,
    pub evidence: Option<String>,
}

pub(super) fn tool_summary(name: &str, input: &Value) -> String {
    for key in ["command", "file_path", "path", "pattern", "url", "query"] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            // The compact card already shows the tool name, so the summary omits it.
            return s.to_string();
        }
    }
    name.to_string()
}

pub fn relativize_summary(summary: &str, wt: &std::path::Path) -> String {
    let p = std::path::Path::new(summary);
    if !p.is_absolute() {
        return summary.to_string();
    }
    if let Ok(rel) = p.strip_prefix(wt) {
        return rel.to_string_lossy().into_owned();
    }
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| summary.to_string())
}

/// Truncate to approximately `max_bytes`, retaining the tail and respecting UTF-8 boundaries.
pub fn truncate_output(s: &str, max_bytes: usize) -> String {
    truncate_output_for_locale(s, max_bytes, crate::Locale::Zh)
}

pub(crate) fn truncate_output_for_locale(
    s: &str,
    max_bytes: usize,
    locale: crate::Locale,
) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let cut = s.len() - max_bytes;
    let mut start = cut;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    let dropped = start;
    match locale {
        crate::Locale::Zh => format!("…[已截断 {dropped} 字节]\n{}", &s[start..]),
        crate::Locale::En => format!("…[truncated {dropped} bytes]\n{}", &s[start..]),
    }
}

/// Remove a shell wrapper such as `/bin/zsh -lc "..."`; return the original command if unmatched.
pub fn unwrap_shell(cmd: &str) -> String {
    for prefix in ["/bin/zsh -lc ", "/bin/bash -lc ", "zsh -lc ", "bash -lc "] {
        if let Some(rest) = cmd.strip_prefix(prefix) {
            let trimmed = rest.trim();
            if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
                return trimmed[1..trimmed.len() - 1].to_string();
            }
            return trimmed.to_string();
        }
    }
    cmd.to_string()
}

/// Map Claude tool names to card styles; only Bash uses the full command card.
pub(super) fn claude_card(tool: &str) -> CardKind {
    if tool == "Bash" {
        CardKind::Command
    } else {
        CardKind::Compact
    }
}

/// Convert string or `[{type,text}]` tool-result content into text.
pub(super) fn tool_result_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let joined: String = arr
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        return Some(joined);
    }
    None
}

pub fn parse_claude_line(line: &str) -> Vec<AgentEvent> {
    parse_claude_line_for_locale(line, crate::Locale::Zh)
}

pub(crate) fn parse_claude_line_for_locale(line: &str, locale: crate::Locale) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    let Some(kind) = v.get("type").and_then(Value::as_str) else {
        return vec![];
    };
    match kind {
        "system" => parse_claude_system_event(&v),
        "stream_event" => parse_claude_stream_event(&v),
        "assistant" => parse_claude_assistant_event(&v),
        "user" => parse_claude_user_event(&v, locale),
        "result" => parse_claude_result_event(&v, locale),
        _ => vec![],
    }
}

pub fn parse_codex_line(line: &str) -> Vec<AgentEvent> {
    parse_codex_line_for_locale(line, crate::Locale::Zh)
}

fn is_codex_reconnect_notice(message: &str) -> bool {
    let Some(rest) = message.trim().strip_prefix("Reconnecting... ") else {
        return false;
    };
    let Some((attempt, rest)) = rest.split_once('/') else {
        return false;
    };
    let Some((limit, reason)) = rest.split_once(' ') else {
        return false;
    };
    let (Ok(attempt), Ok(limit)) = (attempt.parse::<u32>(), limit.parse::<u32>()) else {
        return false;
    };
    attempt > 0
        && attempt <= limit
        && reason.starts_with('(')
        && reason.ends_with(')')
        && reason.len() > 2
}

pub(crate) fn parse_codex_line_for_locale(line: &str, locale: crate::Locale) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    match v.get("type").and_then(Value::as_str) {
        Some("thread.started") => match v.get("thread_id").and_then(Value::as_str) {
            Some(id) => vec![AgentEvent::SessionStarted {
                conversation_id: id.to_string(),
            }],
            None => vec![],
        },
        Some("item.started") => parse_codex_item(v.get("item"), true, locale),
        Some("item.completed") => parse_codex_item(v.get("item"), false, locale),
        Some("error") => v
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !is_codex_reconnect_notice(message))
            .map(|message| {
                vec![AgentEvent::Error {
                    message: message.to_string(),
                }]
            })
            .unwrap_or_default(),
        Some("turn.failed") => v
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(|message| {
                vec![AgentEvent::Error {
                    message: message.to_string(),
                }]
            })
            .unwrap_or_default(),
        Some("turn.completed") => {
            let usage = v.get("usage");
            vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: usage
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(Value::as_u64),
                output_tokens: usage
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64),
                final_text: None,
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        }
        _ => vec![],
    }
}

/// A file-change summary keeps only basenames for readability, which loses image artifact paths.
/// Reuse the otherwise empty output field for newline-separated full image paths so the existing
/// summary-and-output scan can discover them without adding a field.
fn codex_file_change_image_output(item: &Value) -> Option<String> {
    const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "svg"];
    let paths: Vec<&str> = item
        .get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("path").and_then(Value::as_str))
        .filter(|path| {
            let lower = path.to_ascii_lowercase();
            IMAGE_EXTENSIONS
                .iter()
                .any(|ext| lower.ends_with(&format!(".{ext}")))
        })
        .collect();
    if paths.is_empty() {
        None
    } else {
        Some(paths.join("\n"))
    }
}

fn parse_codex_item(item: Option<&Value>, started: bool, locale: crate::Locale) -> Vec<AgentEvent> {
    let Some(item) = item else {
        return vec![];
    };
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            if started {
                return vec![];
            }
            match item.get("text").and_then(Value::as_str) {
                Some(t) => vec![AgentEvent::TextDelta {
                    text: t.to_string(),
                }],
                None => vec![],
            }
        }
        Some("command_execution") => {
            if started {
                let raw = item.get("command").and_then(Value::as_str).unwrap_or("");
                vec![AgentEvent::ToolStarted {
                    id,
                    tool: "command".into(),
                    summary: unwrap_shell(raw),
                    card: CardKind::Command,
                }]
            } else {
                let exit_code = item.get("exit_code").and_then(Value::as_i64);
                let status = if exit_code.unwrap_or(0) == 0 {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Failed
                };
                let output = item
                    .get("aggregated_output")
                    .and_then(Value::as_str)
                    .map(|s| truncate_output_for_locale(s, 32 * 1024, locale));
                vec![AgentEvent::ToolCompleted {
                    id,
                    status,
                    exit_code,
                    output,
                }]
            }
        }
        Some("file_change") => {
            if started {
                let summary = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .map(|c| {
                                let kind = c.get("kind").and_then(Value::as_str).unwrap_or("");
                                let path = c.get("path").and_then(Value::as_str).unwrap_or("");
                                let name = path.rsplit('/').next().unwrap_or(path);
                                format!("{kind} {name}")
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                vec![AgentEvent::ToolStarted {
                    id,
                    tool: "file".into(),
                    summary,
                    card: CardKind::Compact,
                }]
            } else {
                vec![AgentEvent::ToolCompleted {
                    id,
                    status: ToolStatus::Ok,
                    exit_code: None,
                    output: codex_file_change_image_output(item),
                }]
            }
        }
        _ => vec![],
    }
}
