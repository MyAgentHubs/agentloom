use super::*;

pub(super) fn build_member_result(
    spec: &MemberSpec,
    status: StatusTransition,
    changed_files: Vec<ChangedFile>,
    anchor: ResultAnchor,
    command_evidence: Vec<CommandEvidence>,
    final_text: Option<&str>,
) -> MemberResult {
    let status = match status {
        StatusTransition::Dispatched => "dispatched",
        StatusTransition::NeedsInput => "needs_input",
        StatusTransition::Done => "done",
        StatusTransition::Failed => "failed",
        StatusTransition::Stopped => "stopped",
        StatusTransition::Reassigned => "reassigned",
    };
    let risk_inputs = derive_risk_inputs(&changed_files, &command_evidence);
    MemberResult {
        schema_version: 1,
        assignment_id: spec.assignment_id.clone(),
        participant_id: spec.participant_id.clone(),
        status: status.into(),
        failure_reason: None,
        changed_files,
        anchor,
        command_evidence,
        risk_inputs,
        decisions: vec![],
        risks: vec![],
        final_text_ref: final_text.map(|s| s.to_string()),
        artifact_refs: vec![],
        result_source: "raw".into(),
        requires_long_task: None,
        exit_code: None,
        stderr_tail: None,
        failure_kind: None,
    }
}

// Leaves room for a second concatenation within `build_recent_messages`' 2,000-character per-message budget.
pub(super) const MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS: usize = 1_900;
pub(super) const MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS: usize = 1_500;
pub(super) const MEMBER_RESULT_LEDGER_FAILURE_REASON_MAX_CHARS: usize = 300;
pub(super) const MEMBER_RESULT_TRANSIENT_ERROR_MAX_CHARS: usize = 200;
pub(super) const MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX: usize = 50;
pub(super) const MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX_CHARS: usize = 600;
pub(super) const MEMBER_RESULT_LEDGER_IDENTITY_MAX_CHARS: usize = 100;
pub(super) const MEMBER_RESULT_TRANSIENT_ERROR_RISK_ID: &str = "transient_error";
pub(super) const GIT_WALL_BLOCKED_RISK_ID: &str = "git_write_blocked";
/// `Done && (saw_blocked || saw_needs_decision)` is contractually unusual but not `Failed`: the harness
/// emitted a narrative `Blocked`/`NeedsDecision` event, yet the process exited cleanly. Keeping
/// `terminal_status` independent of these flags preserves baseline behavior and avoids downgrading
/// the result to `Failed`, but the combination should not pass silently; record a risk to leave a
/// user-visible trace.
pub(super) const STALLED_ON_DONE_RISK_ID: &str = "stalled_narrative_on_clean_exit";

pub(super) fn clip_member_result_field(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let kept = max_chars.saturating_sub(1);
    format!("{}…", text.chars().take(kept).collect::<String>())
}

pub(super) fn clip_member_result_final_text(text: &str, available_chars: usize) -> String {
    let total = text.chars().count();
    let initial_kept = MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS.min(available_chars);
    if total <= initial_kept {
        return text.to_string();
    }

    // The notice length depends on the kept count. Walk downward to avoid a digit-boundary
    // oscillation (for example 999/1000) while preserving the existing 1,500-char cap.
    let mut kept = initial_kept;
    loop {
        let notice = format!("\n[truncated: kept {kept} of {total} characters]");
        if kept + notice.chars().count() <= available_chars {
            let content: String = text.chars().take(kept).collect();
            return format!("{content}{notice}");
        }
        if kept == 0 {
            return clip_member_result_field(&notice, available_chars);
        }
        kept = kept.saturating_sub(1);
    }
}

pub(super) fn transient_error_note(message: &str) -> String {
    let message = clip_member_result_field(
        &message.replace(['\r', '\n'], " "),
        MEMBER_RESULT_TRANSIENT_ERROR_MAX_CHARS,
    );
    format!("transient_errors: {message}")
}

pub(super) fn render_member_changed_files(changed_files: &[ChangedFile]) -> String {
    let mut section = String::from("changed_files:\n");
    if changed_files.is_empty() {
        section.push_str("- (none)\n");
        return section;
    }

    let candidate_count = changed_files
        .len()
        .min(MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX);
    let mut rendered_count = 0;
    for file in changed_files.iter().take(candidate_count) {
        let path = clip_member_result_field(&file.path.replace(['\r', '\n'], " "), 100);
        let line = format!("- {path} (+{}/-{})\n", file.insertions, file.deletions);
        let omitted_after = changed_files.len() - rendered_count - 1;
        let summary_len = if omitted_after > 0 {
            format!("- (+{omitted_after} more)\n").chars().count()
        } else {
            0
        };
        if section.chars().count() + line.chars().count() + summary_len
            > MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX_CHARS
        {
            break;
        }
        section.push_str(&line);
        rendered_count += 1;
    }
    let omitted = changed_files.len() - rendered_count;
    if omitted > 0 {
        section.push_str(&format!("- (+{omitted} more)\n"));
    }
    section
}

pub(super) fn render_member_terminal_report(
    agent_name: &str,
    assignment_id: &str,
    status: &str,
    failure_reason: Option<&str>,
    transient_error: Option<&str>,
    final_text: Option<&str>,
    changed_files: &[ChangedFile],
) -> String {
    let agent_name = clip_member_result_field(
        &agent_name.replace(['\r', '\n'], " "),
        MEMBER_RESULT_LEDGER_IDENTITY_MAX_CHARS,
    );
    let assignment_id = clip_member_result_field(
        &assignment_id.replace(['\r', '\n'], " "),
        MEMBER_RESULT_LEDGER_IDENTITY_MAX_CHARS,
    );
    let mut report = format!(
        "[Worker report]\nagent: {agent_name}\nassignment_id: {assignment_id}\nstatus: {status}\n"
    );
    if let Some(reason) = failure_reason.filter(|reason| !reason.trim().is_empty()) {
        let reason = clip_member_result_field(
            &reason.replace(['\r', '\n'], " "),
            MEMBER_RESULT_LEDGER_FAILURE_REASON_MAX_CHARS,
        );
        report.push_str(&format!("failure_reason: {reason}\n"));
    }
    if let Some(note) = transient_error.filter(|note| !note.trim().is_empty()) {
        let note = clip_member_result_field(
            &note.replace(['\r', '\n'], " "),
            "transient_errors: ".chars().count() + MEMBER_RESULT_TRANSIENT_ERROR_MAX_CHARS,
        );
        report.push_str(&format!("{note}\n"));
    }
    report.push_str(&render_member_changed_files(changed_files));
    report.push_str("final_text:\n");
    let available = MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS.saturating_sub(report.chars().count());
    match final_text.filter(|text| !text.trim().is_empty()) {
        Some(text) => report.push_str(&clip_member_result_final_text(text, available)),
        None => report.push_str("(none)"),
    }
    debug_assert!(report.chars().count() <= MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS);
    report
}

pub(super) fn render_member_result_report(agent_name: &str, result: &MemberResult) -> String {
    let transient_error = result
        .risks
        .iter()
        .find(|risk| risk.id == MEMBER_RESULT_TRANSIENT_ERROR_RISK_ID)
        .map(|risk| risk.text.as_str());
    let mut report = render_member_terminal_report(
        agent_name,
        &result.assignment_id,
        &result.status,
        result.failure_reason.as_deref(),
        transient_error,
        result.final_text_ref.as_deref(),
        &result.changed_files,
    );
    if let Some(risk) = result
        .risks
        .iter()
        .find(|risk| risk.id == GIT_WALL_BLOCKED_RISK_ID)
    {
        report.push_str(&format!("\n⚠ {}\n", risk.text));
    }
    report
}

pub(super) fn member_result_dedup_key(run_id: &str, assignment_id: &str) -> String {
    format!("member_result:{run_id}:{assignment_id}")
}

pub(super) fn member_result_setup_failed_dedup_key(run_id: &str, assignment_id: &str) -> String {
    format!("member_result_setup_failed:{run_id}:{assignment_id}")
}

pub(crate) fn persist_member_result_message(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    agent_id: &str,
    agent_name: &str,
    result: &MemberResult,
) -> rusqlite::Result<bool> {
    let report = render_member_result_report(agent_name, result);
    crate::db::persist_member_report_atomic(
        conn,
        session_id,
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some(agent_id),
        Some(agent_name),
        &member_result_dedup_key(run_id, &result.assignment_id),
        Some(&result.assignment_id),
        Some((&result.status, &report)),
    )
}

pub(super) fn persist_member_failure_message(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    failure_reason: &str,
) -> rusqlite::Result<bool> {
    let report = render_member_terminal_report(
        &spec.agent_name,
        &spec.assignment_id,
        "failed",
        Some(failure_reason),
        None,
        None,
        &[],
    );
    crate::db::persist_member_report_atomic(
        conn,
        session_id,
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some(&spec.agent_id),
        Some(&spec.agent_name),
        &member_result_dedup_key(run_id, &spec.assignment_id),
        Some(&spec.assignment_id),
        Some(("failed", &report)),
    )
}

pub(super) fn persist_member_setup_failure_message(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    failure_reason: &str,
) -> rusqlite::Result<bool> {
    let report = render_member_terminal_report(
        &spec.agent_name,
        &spec.assignment_id,
        "failed",
        Some(failure_reason),
        None,
        None,
        &[],
    );
    crate::db::persist_member_report_atomic(
        conn,
        session_id,
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some(&spec.agent_id),
        Some(&spec.agent_name),
        &member_result_setup_failed_dedup_key(run_id, &spec.assignment_id),
        Some(&spec.assignment_id),
        Some(("failed", &report)),
    )
}

pub(super) fn log_member_run_side_effect_failure(
    operation: &str,
    session_id: &str,
    run_id: &str,
    assignment_id: &str,
    error: &str,
) {
    eprintln!(
        "member run {operation} failed (best-effort) \
         session_id={session_id} run_id={run_id} assignment_id={assignment_id}: {error}"
    );
}

pub(super) fn run_member_side_effect_best_effort<F>(
    operation: &str,
    session_id: &str,
    run_id: &str,
    assignment_id: &str,
    action: F,
) where
    F: FnOnce() -> Result<(), String>,
{
    if let Err(error) = action() {
        log_member_run_side_effect_failure(operation, session_id, run_id, assignment_id, &error);
    }
}
