use super::*;

pub(super) fn run_stage1_for_locale(
    locale: crate::Locale,
    ctx: &Stage1Ctx,
    _run_id: &str,
    base_sha: &str,
    changed: bool,
) -> Stage1Result {
    if !changed {
        return Stage1Result::NoChanges;
    }
    let head = match crate::worktree::rev_parse_head(&ctx.member_wt) {
        Ok(head) => head,
        Err(error) => {
            let reason = stage1_failure_message(locale, Stage1Failure::Finalize(&error));
            eprintln!("{reason}");
            return Stage1Result::Failed { reason };
        }
    };
    if crate::worktree::worktree_is_dirty(&ctx.member_wt) {
        let failure = if head == base_sha {
            Stage1Failure::Uncommitted
        } else {
            Stage1Failure::DirtyTail(&ctx.member_branch)
        };
        let reason = stage1_failure_message(locale, failure);
        eprintln!("{reason}");
        return Stage1Result::Failed { reason };
    }
    if head == base_sha {
        return Stage1Result::NoChanges;
    }
    match crate::worktree::merge_artifact_to_session_head(&ctx.session_wt, &ctx.member_branch) {
        Ok(crate::worktree::SessionMergeOutcome::Merged { session_head })
        | Ok(crate::worktree::SessionMergeOutcome::AlreadyMerged { session_head }) => {
            Stage1Result::Relayed { session_head }
        }
        Ok(crate::worktree::SessionMergeOutcome::NotFastForward) => {
            let reason =
                stage1_failure_message(locale, Stage1Failure::NotFastForward(&ctx.member_branch));
            eprintln!("{reason}");
            Stage1Result::Failed { reason }
        }
        Err(e) => {
            let reason =
                stage1_failure_message(locale, Stage1Failure::SessionMerge(&e.to_string()));
            eprintln!("{reason}");
            Stage1Result::Failed { reason }
        }
    }
}

/// Core (testable and independent of AppHandle/Tauri): reads child.stdout and emits intermediate
/// events **line by line in real time**. Buffers Completed; after stdout is exhausted, calls
/// begin_finalize_member, then removes the slot and obtains the stop flag/run_done after child.wait().
/// It then emits exactly one terminal event (true streaming with exit-code handling). The caller
/// injects emit (production = EventTransport push/barrier closure; tests = collect into Vec).
#[derive(Clone)]
pub(super) struct MemberFirstEventWatchdog {
    pub(super) deadline: std::time::Instant,
    pub(super) engine: String,
    pub(super) binary: String,
}

pub(super) struct MemberReadAttempt {
    pub(super) saw_error: bool,
    /// Whether a Blocked event from the harness parser was observed (myagent exit-code 3 contract: normal termination, not a crash).
    pub(super) saw_blocked: bool,
    /// Whether a NeedsDecision event from the harness parser was observed (myagent exit-code 4 contract: normal termination, not a crash).
    /// These two flags can only be set by events produced by `parse_harness_line_for_locale`.
    /// The claude/codex parsers never construct Blocked/NeedsDecision, so exit codes 3/4 have no
    /// contractual meaning for those providers and will not be misclassified.
    pub(super) saw_needs_decision: bool,
    /// The harness parser has already rendered the real reason for Blocked/run.interrupted in
    /// human-readable form (harness_blocked_message / harness_interrupted_message). Keep a copy
    /// for composing terminal wording so users do not have to inspect the trace to learn what blocked.
    pub(super) blocked_message: Option<String>,
    /// Structured routing for budget_exhausted / context_exhausted: `AgentEvent::Blocked.reason`
    /// has a value only when the harness triggers it and matches the allowlist, or when the top-level
    /// reason is literally "context_budget_exhausted" (see agent_event.rs documentation).
    /// **Nonempty wins** (adversarial-review fix): do not update it under the same trim guard as
    /// `blocked_message`. A run may first receive a Blocked event with a structured reason (such as
    /// a budget_exhausted/context_exhausted NeedsDecision), then receive run.blocked/run.interrupted
    /// with a nonempty message but always a None reason. Copying the "overwrite on nonempty message"
    /// behavior of `blocked_message` would let the later None erase the structured reason and
    /// incorrectly downgrade it to "stalled". Overwrite only when the new event actually has Some;
    /// None must not erase a recorded value. Terminal failure_kind uses this to distinguish
    /// "still progressing when the turn budget was exhausted" (budget_exhausted) and "single-turn
    /// context token budget overflow" (context_exhausted) from other stalled cases
    /// (no_progress/stuck_repeating/agent-triggered block_with_questions). Do not sniff the message
    /// text: the agent may simply repeat similar wording in its output.
    pub(super) blocked_reason: Option<String>,
    pub(super) failure_reason: Option<String>,
    pub(super) buffered: Option<AgentEvent>,
    pub(super) terminal_events: Vec<AgentEvent>,
    pub(super) tool_events: Vec<AgentEvent>,
    pub(super) assistant_text: String,
    pub(super) assistant_text_only: String,
    pub(super) exit_status: Option<ExitStatus>,
    pub(super) stderr_tail: String,
    pub(super) first_event_timeout_stderr: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_member_attempt(
    mut child: Child,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    first_event: MemberFirstEventWatchdog,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
) -> MemberReadAttempt {
    let context = attempt_reader::AttemptReadContext {
        tr,
        key,
        run_id,
        spec,
        wt,
        parser,
        parse_fn,
        locale,
        granularity,
        first_event_deadline: first_event.deadline,
    };
    let mut reader = attempt_reader::start_attempt_reader(&mut child, &context);
    attempt_reader::read_stdout_events(&mut child, &context, &mut reader, emit);
    attempt_reader::finish_attempt(child, &context, reader)
}

impl MemberFirstEventWatchdog {
    pub(super) fn for_command(
        parse_fn: Option<crate::agent::ParseFn>,
        command: &Command,
        spec: &MemberSpec,
    ) -> Self {
        let engine = parse_fn
            .map(crate::first_event_watchdog_engine)
            .unwrap_or(&spec.agent_name)
            .to_string();
        let binary = parse_fn
            .map(|parse_fn| crate::first_event_watchdog_binary(parse_fn, command))
            .unwrap_or_else(|| command.get_program().to_string_lossy().into_owned());
        Self {
            deadline: std::time::Instant::now()
                + std::time::Duration::from_secs(crate::FIRST_EVENT_TIMEOUT_SECS),
            engine,
            binary,
        }
    }

    #[cfg(test)]
    fn fallback(parse_fn: Option<crate::agent::ParseFn>, spec: &MemberSpec) -> Self {
        let engine = parse_fn
            .map(crate::first_event_watchdog_engine)
            .unwrap_or(&spec.agent_name)
            .to_string();
        Self {
            deadline: std::time::Instant::now()
                + std::time::Duration::from_secs(crate::FIRST_EVENT_TIMEOUT_SECS),
            binary: spec.agent_name.clone(),
            engine,
        }
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn run_member_reader(
    child: Child,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    base_sha: &str,
    parser: fn(&str) -> Vec<AgentEvent>,
    granularity: TextGranularity,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> bool {
    run_member_reader_for_locale(
        child,
        None,
        tr,
        key,
        run_id,
        spec,
        wt,
        base_sha,
        parser,
        None,
        crate::Locale::Zh,
        granularity,
        emit,
        stage1,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn run_member_reader_for_locale(
    child: Child,
    hook_guard: Option<crate::checkpoint_hook::HookRunGuard>,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    base_sha: &str,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> bool {
    let first_event = MemberFirstEventWatchdog::fallback(parse_fn, spec);
    run_member_reader_for_locale_with_watchdog(
        child,
        None,
        None,
        hook_guard,
        tr,
        key,
        run_id,
        spec,
        wt,
        base_sha,
        parser,
        parse_fn,
        locale,
        granularity,
        first_event,
        emit,
        stage1,
    )
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::cognitive_complexity)]
pub(super) fn run_member_reader_for_locale_with_watchdog(
    child: Child,
    retry_command: Option<&mut Command>,
    retry_stdin_prompt: Option<&crate::agent::StdinPrompt>,
    hook_guard: Option<crate::checkpoint_hook::HookRunGuard>,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    base_sha: &str,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    first_event: MemberFirstEventWatchdog,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> bool {
    let (attempt, attempt_watchdog) = reader_watchdog::read_settled_member_attempt(
        child,
        retry_command,
        retry_stdin_prompt,
        tr,
        key,
        run_id,
        spec,
        wt,
        parser,
        parse_fn,
        locale,
        granularity,
        first_event,
        emit,
    );
    // Revoke the run-bound hook token before TeamRunning exposes this run as finished.
    drop(hook_guard);
    let mut state = reader_watchdog::begin_member_finalization(
        attempt,
        &attempt_watchdog,
        tr,
        key,
        run_id,
        spec,
        locale,
        emit,
    );
    reader_watchdog::classify_and_synthesize_failure(&mut state, run_id, spec, locale, emit);
    let artifacts = reader_watchdog::apply_post_run_outcomes(
        &mut state, wt, base_sha, run_id, spec, locale, stage1,
    );
    reader_watchdog::build_and_emit_member_result(state, artifacts, run_id, spec, emit)
}
