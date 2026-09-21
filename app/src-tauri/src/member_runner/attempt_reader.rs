use super::*;
use std::io::{BufRead, BufReader};

pub(super) struct AttemptReadContext<'a> {
    pub(super) tr: &'a TeamRunning,
    pub(super) key: &'a MemberKey,
    pub(super) run_id: &'a str,
    pub(super) spec: &'a MemberSpec,
    pub(super) wt: &'a std::path::Path,
    pub(super) parser: fn(&str) -> Vec<AgentEvent>,
    pub(super) parse_fn: Option<crate::agent::ParseFn>,
    pub(super) locale: crate::Locale,
    pub(super) granularity: TextGranularity,
    pub(super) first_event_deadline: std::time::Instant,
}

pub(super) struct AttemptReader {
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    blocked_message: Option<String>,
    blocked_reason: Option<String>,
    failure_reason: Option<String>,
    buffered: Option<AgentEvent>,
    terminal_events: Vec<AgentEvent>,
    tool_events: Vec<AgentEvent>,
    assistant_text: String,
    assistant_text_only: String,
    pid: u32,
    stderr_handle: Option<std::thread::JoinHandle<String>>,
    stderr_live_tail: Arc<Mutex<Vec<u8>>>,
    first_event_watchdog: crate::FirstEventWatchdogSignal,
    first_event_watchdog_handle: std::thread::JoinHandle<()>,
}

pub(super) fn start_attempt_reader(
    child: &mut Child,
    context: &AttemptReadContext<'_>,
) -> AttemptReader {
    let pid = child.id();
    let (stderr_handle, stderr_live_tail) = match child.stderr.take() {
        Some(stderr) => {
            let (handle, tail) = crate::spawn_stderr_tail_thread_shared(
                stderr,
                crate::member_log_file(&context.key.session_id, &context.spec.assignment_id),
            );
            (Some(handle), tail)
        }
        None => (None, Arc::new(Mutex::new(Vec::new()))),
    };
    let (first_event_watchdog, first_event_watchdog_handle) = crate::spawn_first_event_watchdog(
        context.tr.clone(),
        context.key.clone(),
        pid,
        stderr_live_tail.clone(),
        context
            .first_event_deadline
            .saturating_duration_since(std::time::Instant::now()),
    );
    AttemptReader {
        saw_error: false,
        saw_blocked: false,
        saw_needs_decision: false,
        blocked_message: None,
        blocked_reason: None,
        failure_reason: None,
        buffered: None,
        terminal_events: Vec::new(),
        tool_events: Vec::new(),
        assistant_text: String::new(),
        assistant_text_only: String::new(),
        pid,
        stderr_handle,
        stderr_live_tail,
        first_event_watchdog,
        first_event_watchdog_handle,
    }
}

pub(super) fn read_stdout_events(
    child: &mut Child,
    context: &AttemptReadContext<'_>,
    reader: &mut AttemptReader,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
) {
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        reader.first_event_watchdog.first_line_seen();
        let events = if context.locale == crate::Locale::Zh {
            (context.parser)(&line)
        } else {
            crate::parse_agent_line_for_locale(
                context
                    .parse_fn
                    .expect("localized member parser requires ParseFn"),
                &line,
                context.locale,
            )
        };
        for event in events {
            let event = match event {
                AgentEvent::ToolStarted {
                    id,
                    tool,
                    summary,
                    card,
                } => AgentEvent::ToolStarted {
                    id,
                    tool,
                    summary: crate::agent_event::relativize_summary(&summary, context.wt),
                    card,
                },
                event => event,
            };
            handle_event(reader, context, event, emit);
        }
    }
}

fn handle_event(
    reader: &mut AttemptReader,
    context: &AttemptReadContext<'_>,
    event: AgentEvent,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
) {
    match &event {
        AgentEvent::Completed { .. } => reader.buffered = Some(event),
        AgentEvent::ToolStarted { .. } | AgentEvent::ToolCompleted { .. } => {
            reader.tool_events.push(event.clone());
            emit(
                member_dispatch_meta(context.run_id, context.spec, None),
                event,
            );
        }
        AgentEvent::Error { message } => {
            // P2-8 (Opus adversarial review): an empty string such as `"error": ""` is treated
            // by the harness parser as a valid message (not None, but Some("")); normalize it to
            // None so an empty string cannot bypass the `failure_reason.is_none() -> synthesize
            // one` decision below.
            //
            // Adversarial-review fix (this change): `failure_reason` must use "non-empty wins"
            // rather than being overwritten unconditionally. With the old behavior, if one
            // attempt first received an Error with real text and later an empty/whitespace Error
            // (the real sequence demonstrated by probe F: budget Blocked + real Error + empty
            // Error), the later empty string erased the real error already recorded by replacing
            // it with None, losing the diagnostic completely. Use the same "only overwrite when
            // the new event has non-empty content" rule as blocked_reason (see the
            // AgentEvent::Blocked branch below). Multiple non-empty Errors still use the later
            // value, preserving the original behavior; only an empty string no longer erases it.
            if !message.trim().is_empty() {
                reader.failure_reason = Some(message.clone());
            }
            reader.saw_error = true;
            reader.terminal_events.push(event);
        }
        // P1 (surface member failure reasons): Blocked/NeedsDecision are normal completion
        // narratives for the myagent engine's exit-code 3/4 contract, not crashes. Record flags
        // so finalization can choose honest wording. The events themselves are still emitted to
        // the frontend in real time, as in the old `_` fallback branch, preserving event flow.
        AgentEvent::Blocked { message, reason } => {
            reader.saw_blocked = true;
            // P2-6: harness_blocked_message / harness_interrupted_message already rendered the
            // real cause in plain language. Keep it for final status wording. run.interrupted
            // also follows this branch, whose real message says that the run was interrupted and
            // distinguishes it from the generic "blocked" framing.
            if !message.trim().is_empty() {
                reader.blocked_message = Some(message.clone());
            }
            // Adversarial-review fix: blocked_reason must use "non-empty wins" and must not be
            // written under the message trim guard above. Probe-backed counterexample: within one
            // run, a budget_exhausted NeedsDecision with reason=Some arrives first, followed by
            // run.blocked/run.interrupted with a non-empty message but always reason=None on those
            // two protocol paths (see their construction sites in agent_event.rs). The old logic
            // overwrote the structured reason with None, incorrectly downgrading final
            // classification to "stalled". Only overwrite when the new event has a structured
            // reason. Once a Some value has been captured, later None values do not erase it.
            if reason.is_some() {
                reader.blocked_reason = reason.clone();
            }
            emit(
                member_dispatch_meta(context.run_id, context.spec, None),
                event,
            );
        }
        AgentEvent::NeedsDecision { .. } => {
            reader.saw_needs_decision = true;
            emit(
                member_dispatch_meta(context.run_id, context.spec, None),
                event,
            );
        }
        AgentEvent::TextDelta { text } => {
            reader.assistant_text.push_str(text);
            reader.assistant_text_only.push_str(text);
            if context.granularity == TextGranularity::Line {
                reader.assistant_text.push('\n');
                reader.assistant_text_only.push('\n');
            }
            emit(
                member_dispatch_meta(context.run_id, context.spec, None),
                event,
            );
        }
        AgentEvent::ThinkingDelta { text } => {
            reader.assistant_text.push_str(text);
            if context.granularity == TextGranularity::Line {
                reader.assistant_text.push('\n');
            }
            emit(
                member_dispatch_meta(context.run_id, context.spec, None),
                event,
            );
        }
        _ => emit(
            member_dispatch_meta(context.run_id, context.spec, None),
            event,
        ),
    }
}

pub(super) fn finish_attempt(
    mut child: Child,
    context: &AttemptReadContext<'_>,
    reader: AttemptReader,
) -> MemberReadAttempt {
    let first_line_seen = reader.first_event_watchdog.stdout_closed();
    let _ = reader.first_event_watchdog_handle.join();
    context.tr.begin_finalize_member(context.key);
    let (exit_status, owner_timed_out) = if first_line_seen {
        (child.wait().ok(), false)
    } else {
        match crate::wait_for_first_event_owner(
            &mut child,
            reader.pid,
            context.first_event_deadline,
            Child::try_wait,
            Child::wait,
            crate::kill_process_group,
            std::time::Instant::now,
            std::thread::sleep,
        ) {
            crate::FirstEventOwnerWait::Exited(status) => (Some(status), false),
            crate::FirstEventOwnerWait::TimedOut(status) => (status, true),
            crate::FirstEventOwnerWait::WaitError => (None, false),
        }
    };
    let owner_timeout_stderr =
        owner_timed_out.then(|| crate::stderr_tail_last_lines(&reader.stderr_live_tail));
    let first_event_timeout_stderr = reader
        .first_event_watchdog
        .timeout_stderr()
        .or(owner_timeout_stderr);
    let stderr_tail = reader
        .stderr_handle
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();

    MemberReadAttempt {
        saw_error: reader.saw_error,
        saw_blocked: reader.saw_blocked,
        saw_needs_decision: reader.saw_needs_decision,
        blocked_message: reader.blocked_message,
        blocked_reason: reader.blocked_reason,
        failure_reason: reader.failure_reason,
        buffered: reader.buffered,
        terminal_events: reader.terminal_events,
        tool_events: reader.tool_events,
        assistant_text: reader.assistant_text,
        assistant_text_only: reader.assistant_text_only,
        exit_status,
        stderr_tail,
        first_event_timeout_stderr,
    }
}
