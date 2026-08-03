use std::path::PathBuf;

use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::provider::ChatMessage;

/// 批跑无决策通道时回灌给模型的指引（治理提议被自动拒后·让它继续在现有契约内干）。
pub(crate) const GOVERNANCE_DENY_CONTINUE_GUIDANCE: &str =
    "In non-interactive mode the scope and acceptance criteria cannot be changed. Proceed under \
     the existing acceptance contract: implement the task and run the given verification. If it is \
     genuinely impossible within the contract, call block_with_questions with a concrete reason to \
     stop cleanly.";

pub(crate) const EXECUTOR_SYSTEM_PROMPT: &str = r#"You are MyAgentHubs harness-agent, an autonomous coding agent working in a real
repository. You are given ONE task and you keep working until its acceptance
criteria are green, or you are genuinely blocked and must escalate. Do not stop
early, and do not ask for confirmation to keep going — the harness tracks your
budget and will stop you when needed.

How to work effectively:
- Use tools aggressively. Reading, searching, editing and running commands are
  cheap; guessing is expensive. Prefer acting over deliberating.
- Enumerate before you edit a repeated change. If a change ripples (a field added
  to a struct, a changed function signature, a renamed symbol), its sites live in
  BOTH src AND tests — and there are usually MORE of them under tests. List them
  first, e.g. `grep -rn` for the struct literal or symbol you changed, to find
  ALL affected sites across src AND tests; then patch every site, the ones under
  tests included. Never fix such a change from memory — you will miss sites, and
  the sites you miss are almost always in tests.
- Issue several independent tool calls in one turn instead of one at a time; the
  harness runs them in order.
- Let the compiler/tests be your checklist. After a change that could ripple, run
  the full test build (it compiles the tests too), read EVERY error, and go edit
  the exact file each one names — including every error whose path is under tests.
  Keep repeating grep → patch → rebuild as long as the same kind of error (a
  missing field, an unresolved symbol, a wrong argument count) still appears in a
  file you are allowed to edit. Do not declare done while such an error remains in
  an editable, in-scope file: a red tests file you can edit is a site you have not
  patched yet, not a reason to stop. But if a site you must change is outside your
  Scope or forbidden, or a tool/permission/environment blocker prevents the edit,
  escalate with the exact paths and the error instead of fighting the boundary.
- A dashboard maintained by the harness is appended each turn (objective, fixed
  acceptance criteria, files you changed, ripple candidates, your budget). Use it;
  when present, the ripple-candidate list names sites to audit and patch if still
  stale, tests included. The acceptance criteria are fixed — to revise them,
  escalate; do not negotiate.
- Make each step count: re-running the same failing check or re-editing the same
  spot without new information is not progress and will trip the no-progress stop.
  Before re-running the same failing check, patch at least one listed site — that
  edit is the new information.
- Tool path arguments may be absolute (recommended) or workspace-relative; build absolute
  paths from the working directory shown in the `<env>` block. A path the task names relative
  to a crate lives under a project root, not necessarily the working directory. A zero-result
  grep/glob is not proof a file is absent — broaden or drop the path filter and look again.
- To change an existing file, prefer targeted fs_edit (replace the exact lines)
  over rewriting the whole file with fs_write. Rewriting a large file in one call
  exceeds the output limit and gets truncated — make several small fs_edit edits.

For shell commands call shell_exec and wait for harness permission. Stay
role-agnostic."#;

pub(crate) fn initial_messages(prompt: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::system(EXECUTOR_SYSTEM_PROMPT),
        ChatMessage::user(prompt.to_string()),
    ]
}

/// Append `extra` to the first system message's content (does not replace it).
/// No-op if `messages` has no system message. Used to wire
/// `RunOptions::append_system_prompt` (`myagent run --append-system-prompt`)
/// into the assembled conversation without touching every `initial_messages`
/// call site.
pub(crate) fn append_to_system_prompt(messages: &mut [ChatMessage], extra: &str) {
    if let Some(system_message) = messages.iter_mut().find(|m| m.role == "system") {
        match system_message.content.as_mut() {
            Some(content) => {
                content.push_str("\n\n");
                content.push_str(extra);
            }
            None => system_message.content = Some(extra.to_string()),
        }
    }
}

#[cfg(test)]
mod append_system_prompt_tests {
    use super::*;

    #[test]
    fn append_to_system_prompt_appends_after_two_newlines() {
        let mut messages = initial_messages("do the task");
        append_to_system_prompt(&mut messages, "TEAM LEAD MODE: use dispatch_worker.");
        let system_content = messages[0].content.as_deref().unwrap();
        assert!(system_content.starts_with(EXECUTOR_SYSTEM_PROMPT));
        assert!(system_content.ends_with("TEAM LEAD MODE: use dispatch_worker."));
        assert_eq!(
            system_content,
            format!(
                "{}\n\n{}",
                EXECUTOR_SYSTEM_PROMPT, "TEAM LEAD MODE: use dispatch_worker."
            )
        );
    }

    #[test]
    fn initial_messages_system_content_is_byte_identical_without_append() {
        // Regression pin: when no --append-system-prompt is given, the system
        // message must be exactly EXECUTOR_SYSTEM_PROMPT, unmodified.
        let messages = initial_messages("do the task");
        assert_eq!(messages[0].content.as_deref(), Some(EXECUTOR_SYSTEM_PROMPT));
    }
}

pub(crate) fn inject_context_pack(
    messages: &mut Vec<ChatMessage>,
    recorder: &mut EventRecorder,
    context_files: &[PathBuf],
) -> Result<()> {
    if context_files.is_empty() {
        return Ok(());
    }
    let mut injected = String::from("Context pack injected by harness-agent:\n");
    for path in context_files {
        let content = std::fs::read_to_string(path)?;
        injected.push_str(&format!(
            "\n--- file: {} ---\n{}\n",
            path.to_string_lossy(),
            content
        ));
    }
    recorder.emit(
        "context.pack.attached",
        json!({
            "files": context_files
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        }),
    )?;
    messages.push(ChatMessage::user(injected));
    Ok(())
}

/// 汇总「未达标」criterion（Failed + Uncertain）给下一轮回灌。
pub(crate) fn unmet_summary(goal: &crate::goal::GoalState) -> String {
    use crate::goal::CriterionStatus;

    goal.contract
        .criteria
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                CriterionStatus::Failed | CriterionStatus::Uncertain
            )
        })
        .map(|c| {
            let label = match c.status {
                CriterionStatus::Failed => "FAILED",
                CriterionStatus::Uncertain => "UNCERTAIN",
                _ => unreachable!(),
            };
            format!(
                "- {} {}: {}",
                c.id,
                label,
                crate::cockpit_render::render_criterion_for_model(c)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
