use crate::error::{HarnessError, Result};
use crate::journal::{save_conversation, RunPaths, SavedConversation};
use crate::provider::pairing::validate_tool_pairing;
use crate::provider::{ChatMessage, ToolCall};

pub(crate) fn save_conversation_snapshot(
    paths: &RunPaths,
    run_id: &str,
    provider: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<()> {
    if let Err(error) = validate_tool_pairing(messages) {
        return Err(HarnessError::Runtime(format!(
            "conversation pairing invalid at message {}: {}",
            error.index, error.message
        )));
    }
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            messages: messages.to_vec(),
        },
    )
}

pub(crate) fn save_working_ledger_if_dirty(
    paths: &RunPaths,
    ledger: &crate::working_ledger::WorkingLedger,
    ledger_dirty: &mut bool,
) -> Result<()> {
    if *ledger_dirty {
        crate::journal::save_working_ledger(&paths.working_ledger_path, ledger)?;
        *ledger_dirty = false;
    }
    Ok(())
}

pub(crate) fn append_unpaired_tool_results(
    messages: &mut Vec<ChatMessage>,
    tool_calls: &[ToolCall],
    content: &str,
) {
    for tool_call in tool_calls {
        let already_paired = messages.iter().any(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some(tool_call.id.as_str())
        });
        if !already_paired {
            messages.push(ChatMessage::tool(tool_call.id.clone(), content.to_string()));
        }
    }
}
