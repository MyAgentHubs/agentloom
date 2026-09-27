use super::*;

/// Member opening event (mirrors the first event in fake_runner build_fake_run):
/// Dispatched + TextDelta(subtask). The frontend teamReducer uses it to create the member card and populate m.sub (teamReducer.ts:137).
/// spawn_member emits this before reading stdout, so the card appears immediately, the stop button is enabled, and the subtask is displayed correctly.
pub fn member_open_event(run_id: &str, spec: &MemberSpec) -> (DispatchMeta, AgentEvent) {
    let mut meta = member_dispatch_meta(run_id, spec, Some(StatusTransition::Dispatched));
    meta.task_pack = Some(spec.prompt.clone());
    (
        meta,
        AgentEvent::TextDelta {
            text: spec.subtask.clone(),
        },
    )
}

/// Terminal-state mapping (including exit codes): stop takes priority → Completed+exit0 = Done →
/// Error or nonzero exit = Failed → otherwise Done.
///
/// By design, saw_blocked/saw_needs_decision **intentionally do not
/// participate in status determination here**. Mutation testing proved that adding them to
/// `saw_error || saw_blocked || saw_needs_decision || !exit_success` would silently downgrade Done
/// to Failed when `saw_blocked=true, exit_success=true, and Completed was not observed`, breaking
/// the in-place handoff in `run_stage1_for_locale`.
///
/// Delta review clarified the rationale: **the narrowing is valid, but "clean exit means truly
/// finished" is not a defensible reason**. `exit_success=true && saw_completed=false` actually hits
/// the final `else` fallback branch and never observes a real `run.completed`/`Completed` event,
/// so it is not positive evidence that a cleanly exited process definitely finished. The valid reason
/// is **established baseline behavior**: the `saw_error || !exit_success` rule predates 108f81f0.
/// This change exposes member failure reasons and should not casually alter unrelated, unverified
/// behavior (whether a clean exit after Blocked should count as completed is a separate product
/// decision that needs its own validation). Therefore, these two flags are currently used **only**
/// by the caller to choose honest stalled wording versus generic environment-failure wording (see
/// the read_member_attempt call site); they do not change the state machine. This preserves the
/// status quo rather than making a new affirmative decision. The contractually odd combination
/// `Done && (saw_blocked || saw_needs_decision)`, which is not marked Failed, records a risk in
/// member_result below so it becomes visible instead of remaining silent (see the
/// STALLED_ON_DONE_RISK_ID comment near member_result.risks.push).
pub fn terminal_status(
    saw_error: bool,
    saw_completed: bool,
    exit_success: bool,
    stopped: bool,
) -> StatusTransition {
    if stopped {
        StatusTransition::Stopped
    } else if saw_completed && exit_success {
        StatusTransition::Done
    } else if saw_error || !exit_success {
        StatusTransition::Failed
    } else {
        StatusTransition::Done
    }
}

pub(super) fn detect_blocking_write_failure(text: &str) -> Option<String> {
    let normalized = text.to_ascii_lowercase();
    for marker in [
        "operation not permitted",
        "permission denied",
        "read-only file system",
    ] {
        if normalized.contains(marker) {
            return Some(marker.to_string());
        }
    }

    if normalized.contains("apply_patch") {
        for marker in ["rejected", "reject", "failed"] {
            if normalized.contains(marker) {
                return Some(format!("apply_patch {marker}"));
            }
        }
    }

    None
}

/// Detect whether the sandbox blocked a git command that writes to .git in this attempt. Returns the blocked command string for display in the report.
pub(super) fn detect_git_wall_block(tool_events: &[AgentEvent]) -> Option<String> {
    let mut started: HashMap<String, String> = HashMap::new();
    for event in tool_events {
        if let AgentEvent::ToolStarted { id, summary, .. } = event {
            started.entry(id.clone()).or_insert_with(|| summary.clone());
        }
    }
    for event in tool_events {
        if let AgentEvent::ToolCompleted {
            id,
            status: ToolStatus::Failed,
            output: Some(output),
            ..
        } = event
        {
            let command = started.get(id).map(String::as_str).unwrap_or("");
            let normalized_output = output.to_ascii_lowercase();
            let signature = normalized_output.contains("operation not permitted")
                || normalized_output.contains("read-only file system");
            let is_git_write =
                command.to_ascii_lowercase().contains("git") || normalized_output.contains(".git");
            if signature && is_git_write {
                return Some(command.to_string());
            }
        }
    }
    None
}

pub fn derive_command_evidence(tool_events: &[AgentEvent], provider: &str) -> Vec<CommandEvidence> {
    let mut started_order = Vec::new();
    let mut started_cmds: HashMap<String, String> = HashMap::new();
    let mut completed: HashMap<String, (String, Option<i64>)> = HashMap::new();

    for event in tool_events {
        match event {
            AgentEvent::ToolStarted {
                id, tool, summary, ..
            } => {
                if let Entry::Vacant(entry) = started_cmds.entry(id.clone()) {
                    started_order.push(id.clone());
                    let cmd = match tool.as_str() {
                        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                            format!("{tool} {summary}")
                        }
                        _ => summary.clone(),
                    };
                    entry.insert(cmd);
                }
            }
            AgentEvent::ToolCompleted {
                id,
                status,
                exit_code,
                ..
            } => {
                let status = match status {
                    ToolStatus::Ok => "ok",
                    ToolStatus::Failed => "failed",
                };
                completed.insert(id.clone(), (status.to_string(), *exit_code));
            }
            _ => {}
        }
    }

    started_order
        .into_iter()
        .filter_map(|id| {
            let cmd = started_cmds.remove(&id)?;
            let (status, exit_code) = completed.remove(&id)?;
            Some(CommandEvidence {
                cmd,
                exit_code,
                status,
                source_provider: provider.to_string(),
                output_ref: None,
            })
        })
        .collect()
}

pub fn derive_risk_inputs(
    changed_files: &[ChangedFile],
    command_evidence: &[CommandEvidence],
) -> RiskInputs {
    // Deterministic risk table:
    // files_changed = raw file count; any write/change-like command => med; otherwise low.
    // This table does not emit high, and no auto-commit/push means every result is reversible.
    let cmd_danger = if command_evidence
        .iter()
        .any(|evidence| command_is_write_like(&evidence.cmd))
    {
        "med"
    } else {
        "low"
    };
    RiskInputs {
        files_changed: changed_files.len() as u64,
        cmd_danger: cmd_danger.into(),
        reversibility: "reversible".into(),
    }
}

pub(crate) fn command_is_write_like(cmd: &str) -> bool {
    let normalized = cmd.to_ascii_lowercase().replace('\n', " ");
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return false;
    };

    matches!(*first, "write" | "edit" | "multiedit" | "notebookedit")
        || tokens.iter().enumerate().any(|(idx, token)| {
            matches!(
                *token,
                "mkdir" | "mv" | "cp" | "touch" | "rm" | "rmdir" | "rustfmt"
            ) && is_command_position(&tokens, idx)
        })
        || tokens.iter().any(|token| {
            matches!(*token, "--write" | "--fix")
                || token.starts_with("--write=")
                || token.starts_with("--fix=")
        })
        || command_pair_exists(&tokens, "sed", "-i")
        || tee_writes_non_dev_null(&tokens)
        || command_pair_exists(&tokens, "git", "apply")
        || command_pair_exists(&tokens, "cargo", "fmt")
        || tokens
            .windows(2)
            .any(|pair| matches!(pair[0], ">" | ">>") && pair[1] != "/dev/null")
}

fn command_pair_exists(tokens: &[&str], command: &str, arg: &str) -> bool {
    tokens
        .windows(2)
        .enumerate()
        .any(|(idx, pair)| pair[0] == command && pair[1] == arg && is_command_position(tokens, idx))
}

fn tee_writes_non_dev_null(tokens: &[&str]) -> bool {
    for (idx, token) in tokens.iter().enumerate() {
        if *token != "tee" || !is_command_position(tokens, idx) {
            continue;
        }
        for target in &tokens[idx + 1..] {
            if is_shell_separator(target) {
                break;
            }
            if target.starts_with('-') || *target == "/dev/null" {
                continue;
            }
            return true;
        }
    }
    false
}

fn is_command_position(tokens: &[&str], idx: usize) -> bool {
    idx == 0 || is_shell_separator(tokens[idx - 1])
}

fn is_shell_separator(token: &str) -> bool {
    matches!(token, "|" | "&&" | "||" | ";")
}
