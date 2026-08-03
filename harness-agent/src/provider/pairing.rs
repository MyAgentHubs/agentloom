use crate::provider::ChatMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairingError {
    pub index: usize,
    pub message: String,
}

pub(crate) fn validate_tool_pairing(messages: &[ChatMessage]) -> Result<(), PairingError> {
    validate_prefix_len(messages).map(|_| ())
}

fn validate_prefix_len(messages: &[ChatMessage]) -> Result<usize, PairingError> {
    let mut index = 0usize;
    while index < messages.len() {
        let message = &messages[index];
        if message.role == "tool" {
            return Err(PairingError {
                index,
                message: "tool message without preceding assistant tool_calls".to_string(),
            });
        }

        let Some(tool_calls) = message
            .tool_calls
            .as_ref()
            .filter(|calls| !calls.is_empty())
        else {
            index += 1;
            continue;
        };

        if message.role != "assistant" {
            return Err(PairingError {
                index,
                message: "tool_calls are only valid on assistant messages".to_string(),
            });
        }

        for (offset, tool_call) in tool_calls.iter().enumerate() {
            let tool_index = index + 1 + offset;
            let Some(tool_message) = messages.get(tool_index) else {
                return Err(PairingError {
                    index,
                    message: format!("missing tool result for {}", tool_call.id),
                });
            };
            if tool_message.role != "tool" {
                return Err(PairingError {
                    index,
                    message: format!("non-tool message before result for {}", tool_call.id),
                });
            }
            if tool_message.tool_call_id.as_deref() != Some(tool_call.id.as_str()) {
                return Err(PairingError {
                    index,
                    message: format!("tool result does not match {}", tool_call.id),
                });
            }
        }

        index += 1 + tool_calls.len();
    }
    Ok(index)
}

pub(crate) struct ConversationRepair {
    pub messages: Vec<ChatMessage>,
    pub dropped: usize,
}

pub(crate) fn repair_tool_pairing(messages: Vec<ChatMessage>) -> ConversationRepair {
    match validate_prefix_len(&messages) {
        Ok(_) => ConversationRepair {
            messages,
            dropped: 0,
        },
        Err(error) => {
            let dropped = messages.len().saturating_sub(error.index);
            ConversationRepair {
                messages: messages.into_iter().take(error.index).collect(),
                dropped,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{FunctionCall, ToolCall};

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "test_tool".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    fn assistant(ids: &[&str]) -> ChatMessage {
        ChatMessage::assistant(
            "tools",
            None,
            ids.iter().map(|id| call(id)).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn conversation_pairing_validate_accepts_legal_adjacent_tool_results() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant(&["call_a", "call_b"]),
            ChatMessage::tool("call_a", "a"),
            ChatMessage::tool("call_b", "b"),
            ChatMessage::assistant("done", None, Vec::new()),
        ];

        assert!(validate_tool_pairing(&messages).is_ok());
    }

    #[test]
    fn conversation_pairing_repair_drops_tail_unpaired_assistant() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant(&["call_a"]),
            ChatMessage::user("after illegal tail"),
        ];

        let repaired = repair_tool_pairing(messages);

        assert_eq!(repaired.dropped, 2);
        assert_eq!(repaired.messages.len(), 1);
        assert!(validate_tool_pairing(&repaired.messages).is_ok());
    }

    #[test]
    fn conversation_pairing_validate_rejects_partial_missing_tool_result() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant(&["call_a", "call_b"]),
            ChatMessage::tool("call_a", "a"),
        ];

        assert!(validate_tool_pairing(&messages).is_err());
    }

    #[test]
    fn conversation_pairing_repair_drops_partial_missing_tool_result_tail() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant(&["call_a", "call_b"]),
            ChatMessage::tool("call_a", "a"),
        ];

        let repaired = repair_tool_pairing(messages);

        assert_eq!(repaired.dropped, 2);
        assert_eq!(repaired.messages.len(), 1);
        assert!(validate_tool_pairing(&repaired.messages).is_ok());
    }

    #[test]
    fn conversation_pairing_validate_rejects_non_contiguous_tool_result() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant(&["call_a"]),
            ChatMessage::user("breaks adjacency"),
            ChatMessage::tool("call_a", "a"),
        ];

        assert!(validate_tool_pairing(&messages).is_err());
    }

    #[test]
    fn conversation_pairing_repair_drops_non_contiguous_tool_result_tail() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant(&["call_a"]),
            ChatMessage::user("breaks adjacency"),
            ChatMessage::tool("call_a", "a"),
        ];

        let repaired = repair_tool_pairing(messages);

        assert_eq!(repaired.dropped, 3);
        assert_eq!(repaired.messages.len(), 1);
        assert!(validate_tool_pairing(&repaired.messages).is_ok());
    }

    #[test]
    fn conversation_pairing_validate_accepts_messages_without_tool_calls() {
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("go"),
            ChatMessage::assistant("done", None, Vec::new()),
        ];

        assert!(validate_tool_pairing(&messages).is_ok());
    }
}
