use async_trait::async_trait;
use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::provider::{
    ChatMessage, FinishReason, FunctionCall, ProviderCapabilities, ProviderClient,
    ProviderResponse, ToolCall,
};

#[derive(Debug, Clone)]
pub struct MockProvider {
    model: String,
    finish_reason: Option<FinishReason>,
}

impl Default for MockProvider {
    fn default() -> Self {
        Self {
            model: "mock-model".to_string(),
            finish_reason: None,
        }
    }
}

impl MockProvider {
    pub fn with_finish_reason(mut self, finish_reason: FinishReason) -> Self {
        self.finish_reason = Some(finish_reason);
        self
    }
}

#[async_trait]
impl ProviderClient for MockProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        // Agentic loop scripted scenario (read->edit->verify >=3 turns). 由 tool 消息计数驱动，resume-safe。
        let is_agentic = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("agentic loop"))
        });
        if is_agentic {
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            return scripted_step(tool_msgs, events, self.finish_reason.clone());
        }

        let is_dual_gate = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("criterion then tool"))
        });
        if is_dual_gate {
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            return dual_gate_step(tool_msgs, events, self.finish_reason.clone());
        }

        let is_two_step_egress = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("two step egress"))
        });
        if is_two_step_egress {
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            return two_step_egress_step(tool_msgs, events, self.finish_reason.clone());
        }

        let is_egress_curl = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("egress curl"))
        });
        if is_egress_curl {
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            return egress_curl_step(tool_msgs, events, self.finish_reason.clone());
        }

        let is_web_search_demo = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("web_search demo"))
        });
        if is_web_search_demo {
            let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
            return web_search_step(tool_msgs, events, self.finish_reason.clone());
        }

        if messages.last().map(|message| message.role.as_str()) == Some("tool") {
            let text = "Tool finished. I wrote the dispatch handoff report.";
            for chunk in ["Tool finished. ", "I wrote the dispatch handoff report."] {
                events.emit_text_delta(chunk)?;
            }
            return Ok(provider_response(
                text,
                String::new(),
                Vec::new(),
                self.finish_reason.clone(),
            ));
        }

        let prompt = messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .and_then(|message| message.content.as_deref())
            .unwrap_or_default();

        if prompt.contains("show reasoning") {
            for chunk in ["Thinking: ", "weigh options, ", "then answer."] {
                events.emit_reasoning_delta(chunk)?;
            }
            let text = "Here is my reasoned answer.";
            events.emit_text_delta(text)?;
            return Ok(provider_response(
                text,
                "weigh options then answer",
                Vec::new(),
                self.finish_reason.clone(),
            ));
        }

        if prompt.contains("propose scope") {
            let text = "I need to widen the scope; proposing a scope change.";
            events.emit_text_delta(text)?;
            return Ok(provider_response(
                text,
                String::new(),
                vec![ToolCall {
                    id: "call_scope_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "propose_scope_change".to_string(),
                        arguments: json!({
                            "kind": "scope",
                            "detail": "refactor the whole module instead of the one bug"
                        })
                        .to_string(),
                    },
                }],
                self.finish_reason.clone(),
            ));
        }

        if prompt.contains("propose criterion") {
            let text = "Proposing a verifiable acceptance criterion.";
            events.emit_text_delta(text)?;
            return Ok(provider_response(
                text,
                String::new(),
                vec![ToolCall {
                    id: "call_crit_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "propose_criterion".to_string(),
                        arguments: json!({
                            "claim": "marker file exists",
                            "check_cmd": "true",
                            "success": "exit_zero"
                        })
                        .to_string(),
                    },
                }],
                self.finish_reason.clone(),
            ));
        }

        if prompt.contains("fail shell") {
            let text = "I'll run a failing shell command and inspect the result.";
            events.emit_text_delta(text)?;
            return Ok(provider_response(
                text,
                "mock provider selected failing shell tool",
                vec![ToolCall {
                    id: "call_mock_shell_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "shell_exec".to_string(),
                        arguments: json!({
                            "command": "printf 'failing\\n'; exit 7",
                            "timeout_ms": 5000
                        })
                        .to_string(),
                    },
                }],
                self.finish_reason.clone(),
            ));
        }

        if prompt.contains("escape shell") {
            let text = "I'll try to detach a background process.";
            events.emit_text_delta(text)?;
            return Ok(provider_response(
                text,
                "mock selected an escaping shell command",
                vec![ToolCall {
                    id: "call_mock_escape_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "shell_exec".to_string(),
                        arguments: json!({ "command": "setsid sleep 60", "timeout_ms": 5000 })
                            .to_string(),
                    },
                }],
                self.finish_reason.clone(),
            ));
        }

        if prompt.contains("dispatch") || prompt.contains("shell") {
            let text = "I'll call the shell dispatch command after approval.";
            events.emit_text_delta(text)?;
            return Ok(provider_response(
                text,
                "mock provider selected shell tool",
                vec![ToolCall {
                    id: "call_mock_shell_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "shell_exec".to_string(),
                        arguments: json!({
                            "command": "printf 'dispatch accepted\\nreport saved: dispatch-handoff-report.md\\n'",
                            "timeout_ms": 5000
                        })
                        .to_string(),
                    },
                }],
                self.finish_reason.clone(),
            ));
        }

        let text = format!("Mock response for: {prompt}");
        for chunk in text.as_bytes().chunks(12) {
            events.emit_text_delta(std::str::from_utf8(chunk).unwrap_or_default())?;
        }
        Ok(provider_response(
            text,
            String::new(),
            Vec::new(),
            self.finish_reason.clone(),
        ))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "mock".to_string(),
            model_id: self.model.clone(),
            supports_streaming: true,
            supports_reasoning_deltas: true,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: Some(128_000),
            output_token_limit: Some(8_192),
            server_side_search: false,
        }
    }
}

fn provider_response(
    text: impl Into<String>,
    reasoning: impl Into<String>,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<FinishReason>,
) -> ProviderResponse {
    let finish_reason =
        finish_reason.or_else(|| tool_calls.is_empty().then_some(FinishReason::Stop));
    ProviderResponse {
        text: text.into(),
        reasoning: reasoning.into(),
        tool_calls,
        finish_reason,
    }
}

fn scripted_step(
    tool_msgs: usize,
    events: &mut crate::events::EventRecorder,
    finish_reason: Option<FinishReason>,
) -> crate::error::Result<crate::provider::ProviderResponse> {
    use crate::provider::{FunctionCall, ToolCall};
    use serde_json::json;

    let (text, tool_calls) = match tool_msgs {
        0 => (
            "Step 1: write demo.txt with the initial value.",
            vec![ToolCall {
                id: "call_agentic_write".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "fs_write".to_string(),
                    arguments: json!({ "path": "demo.txt", "content": "VALUE=1\n" }).to_string(),
                },
            }],
        ),
        1 => (
            "Step 2: read demo.txt back to inspect it.",
            vec![ToolCall {
                id: "call_agentic_read".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "fs_read".to_string(),
                    arguments: json!({ "path": "demo.txt" }).to_string(),
                },
            }],
        ),
        2 => (
            "Step 3: fix the value via an exact edit.",
            vec![ToolCall {
                id: "call_agentic_edit".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "fs_edit".to_string(),
                    arguments: json!({ "path": "demo.txt", "old_string": "VALUE=1", "new_string": "VALUE=2" }).to_string(),
                },
            }],
        ),
        _ => ("All steps done; demo.txt now holds VALUE=2.", Vec::new()),
    };

    events.emit_text_delta(text)?;
    Ok(provider_response(
        text,
        String::new(),
        tool_calls,
        finish_reason,
    ))
}

fn dual_gate_step(
    tool_msgs: usize,
    events: &mut crate::events::EventRecorder,
    finish_reason: Option<FinishReason>,
) -> crate::error::Result<crate::provider::ProviderResponse> {
    use crate::provider::{FunctionCall, ToolCall};
    use serde_json::json;

    let (text, tool_calls) = match tool_msgs {
        0 => (
            "Proposing a verifiable acceptance criterion before running a tool.",
            vec![ToolCall {
                id: "call_dual_crit_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "propose_criterion".to_string(),
                    arguments: json!({
                        "claim": "dual gate criterion passes",
                        "check_cmd": "true",
                        "success": "exit_zero"
                    })
                    .to_string(),
                },
            }],
        ),
        1 => (
            "Now running a mutating shell command after the criterion approval.",
            vec![ToolCall {
                id: "call_dual_shell_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "shell_exec".to_string(),
                    arguments: json!({
                        "command": "printf 'dual done\\n'",
                        "timeout_ms": 5000
                    })
                    .to_string(),
                },
            }],
        ),
        _ => ("Dual gate scenario complete.", Vec::new()),
    };

    events.emit_text_delta(text)?;
    Ok(provider_response(
        text,
        String::new(),
        tool_calls,
        finish_reason,
    ))
}

fn egress_curl_step(
    tool_msgs: usize,
    events: &mut crate::events::EventRecorder,
    finish_reason: Option<FinishReason>,
) -> crate::error::Result<crate::provider::ProviderResponse> {
    use crate::provider::{FunctionCall, ToolCall};
    use serde_json::json;

    let (text, tool_calls) = match tool_msgs {
        0 => (
            "I'll test public egress with curl.",
            vec![ToolCall {
                id: "call_mock_curl_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "shell_exec".to_string(),
                    arguments: json!({
                        "command": "curl -sS --max-time 5 https://example.com"
                    })
                    .to_string(),
                },
            }],
        ),
        _ => ("Curl egress scenario complete.", Vec::new()),
    };

    events.emit_text_delta(text)?;
    Ok(provider_response(
        text,
        String::new(),
        tool_calls,
        finish_reason,
    ))
}

fn web_search_step(
    tool_msgs: usize,
    events: &mut crate::events::EventRecorder,
    finish_reason: Option<FinishReason>,
) -> crate::error::Result<crate::provider::ProviderResponse> {
    use crate::provider::{FunctionCall, ToolCall};
    use serde_json::json;

    let (text, tool_calls) = match tool_msgs {
        0 => (
            "I'll search for rust.",
            vec![ToolCall {
                id: "call_mock_web_search_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "web_search".to_string(),
                    arguments: json!({ "query": "rust" }).to_string(),
                },
            }],
        ),
        _ => ("search demo done", Vec::new()),
    };

    events.emit_text_delta(text)?;
    Ok(provider_response(
        text,
        String::new(),
        tool_calls,
        finish_reason,
    ))
}

// 二段式剧本：第一轮工具读到本地内容（其中可含注入指令），第二轮模型据此发起出网调用。
// 用于确定性验证：--network off 下第二轮出网被 harness 关卡卡死（验关卡、不验模型行为）。
fn two_step_egress_step(
    tool_msgs: usize,
    events: &mut crate::events::EventRecorder,
    finish_reason: Option<FinishReason>,
) -> crate::error::Result<crate::provider::ProviderResponse> {
    use crate::provider::{FunctionCall, ToolCall};
    use serde_json::json;

    let (text, tool_calls) = match tool_msgs {
        0 => (
            "Reading local content first.",
            vec![ToolCall {
                id: "call_mock_twostep_read".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "shell_exec".to_string(),
                    arguments: json!({
                        "command": "printf 'local content read'"
                    })
                    .to_string(),
                },
            }],
        ),
        1 => (
            "Now making an outbound network call.",
            vec![ToolCall {
                id: "call_mock_twostep_egress".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "shell_exec".to_string(),
                    arguments: json!({
                        "command": "curl -sS --max-time 5 https://example.org/collect"
                    })
                    .to_string(),
                },
            }],
        ),
        _ => ("Two-step egress scenario complete.", Vec::new()),
    };

    events.emit_text_delta(text)?;
    Ok(provider_response(
        text,
        String::new(),
        tool_calls,
        finish_reason,
    ))
}
