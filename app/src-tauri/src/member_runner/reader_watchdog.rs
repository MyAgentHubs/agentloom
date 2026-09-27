use super::*;

pub(super) struct MemberFinalizationState {
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    blocked_message: Option<String>,
    blocked_reason: Option<String>,
    failure_reason: Option<String>,
    buffered: Option<AgentEvent>,
    tool_events: Vec<AgentEvent>,
    assistant_text: String,
    assistant_text_only: String,
    exit_status: Option<ExitStatus>,
    stderr_tail: String,
    status: StatusTransition,
    stopped: bool,
    run_done: bool,
    failure_kind: Option<&'static str>,
}

pub(super) struct MemberResultArtifacts {
    changed_files: Vec<ChangedFile>,
    anchor: ResultAnchor,
    command_evidence: Vec<CommandEvidence>,
    git_wall: Option<String>,
    session_head_sha: Option<String>,
    final_text: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_settled_member_attempt(
    mut child: Child,
    mut retry_command: Option<&mut Command>,
    retry_stdin_prompt: Option<&crate::agent::StdinPrompt>,
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
) -> (MemberReadAttempt, MemberFirstEventWatchdog) {
    let mut retry_count = 0;
    let mut current_pid = child.id();
    let mut attempt_watchdog = first_event;
    let attempt = loop {
        let mut attempt = read_member_attempt(
            child,
            tr,
            key,
            run_id,
            spec,
            wt,
            parser,
            parse_fn,
            locale,
            granularity,
            attempt_watchdog.clone(),
            emit,
        );
        let exit_success = attempt.exit_status.as_ref().is_some_and(|s| s.success());
        let auth_failed = matches!(
            terminal_status(
                attempt.saw_error,
                attempt.buffered.is_some(),
                exit_success,
                false,
            ),
            StatusTransition::Failed
        ) && attempt
            .failure_reason
            .as_deref()
            .is_some_and(crate::agent_event::is_auth_error);
        if !auth_failed || retry_count >= crate::agent_event::AUTH_RETRY_MAX {
            break attempt;
        }
        let Some(command) = retry_command.as_deref_mut() else {
            break attempt;
        };

        retry_count += 1;
        std::thread::sleep(std::time::Duration::from_millis(
            350 * u64::from(retry_count),
        ));
        attempt_watchdog = MemberFirstEventWatchdog::for_command(parse_fn, command, spec);
        match crate::agent::spawn_with_stdin_prompt(command, retry_stdin_prompt) {
            Ok(mut retry_child) => {
                let retry_pid = retry_child.id();
                if tr.register_auth_retry(key, current_pid, retry_pid) {
                    current_pid = retry_pid;
                    child = retry_child;
                    continue;
                }
                crate::kill_process_group(retry_pid);
                let _ = retry_child.wait();
                break attempt;
            }
            Err(error) => {
                let message =
                    crate::ui_msg::al_err("member.spawnFailed", &[("detail", error.to_string())]);
                attempt.saw_error = true;
                attempt.failure_reason = Some(message.clone());
                attempt.terminal_events.push(AgentEvent::Error { message });
                break attempt;
            }
        }
    };
    (attempt, attempt_watchdog)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin_member_finalization(
    attempt: MemberReadAttempt,
    attempt_watchdog: &MemberFirstEventWatchdog,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    locale: crate::Locale,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
) -> MemberFinalizationState {
    let MemberReadAttempt {
        mut saw_error,
        saw_blocked,
        saw_needs_decision,
        blocked_message,
        blocked_reason,
        mut failure_reason,
        buffered,
        mut terminal_events,
        tool_events,
        assistant_text,
        assistant_text_only,
        exit_status,
        stderr_tail,
        first_event_timeout_stderr,
    } = attempt;
    let exit_success = exit_status.as_ref().is_some_and(|s| s.success());
    // Remove the pid while holding the lock (avoids killing a reused pid), read the stop flag,
    // and decide run_done from the remaining count.
    let (stopped, run_done) = tr.finish_member_and_run_done(key);
    if crate::should_inject_first_event_watchdog_error(
        stopped,
        buffered.is_some(),
        first_event_timeout_stderr.as_deref(),
    ) {
        let stderr_summary = first_event_timeout_stderr
            .expect("watchdog injection predicate requires timeout stderr");
        let message = crate::first_event_watchdog_error_message(
            locale,
            "member.spawnFailed",
            &attempt_watchdog.engine,
            &attempt_watchdog.binary,
            &stderr_summary,
        );
        terminal_events.push(AgentEvent::Error {
            message: message.clone(),
        });
        saw_error = true;
        failure_reason = Some(message);
    }
    for event in terminal_events {
        emit(member_dispatch_meta(run_id, spec, None), event);
    }
    let status = terminal_status(saw_error, buffered.is_some(), exit_success, stopped);
    MemberFinalizationState {
        saw_error,
        saw_blocked,
        saw_needs_decision,
        blocked_message,
        blocked_reason,
        failure_reason,
        buffered,
        tool_events,
        assistant_text,
        assistant_text_only,
        exit_status,
        stderr_tail,
        status,
        stopped,
        run_done,
        failure_kind: None,
    }
}

pub(super) fn classify_and_synthesize_failure(
    state: &mut MemberFinalizationState,
    run_id: &str,
    spec: &MemberSpec,
    locale: crate::Locale,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
) {
    // P1-2 (adversarial review, structured criterion): failure_kind is a trustworthy hard
    // criterion sent to the frontend. "stalled" / "env" are written only by the backend here,
    // according to the real saw_blocked/saw_needs_decision flags, and are never inferred back
    // from prose. The frontend must not use a regex to sniff failure_reason for a code-phrase
    // (that phrase is itself in failure_reason, so agent output/stderr could copy it verbatim and
    // impersonate an "honest stall"; the structured field has no such reverse-controllable path).
    //
    // D6 (delta review, demonstrated counterexample): this assignment used to be embedded in the
    // `if failure_reason.is_none()` branch below that decides whether to synthesize fallback prose.
    // But an agent reporting Error first (run.failed / native Claude error / auth-retry injection)
    // is the most common failure shape. Once failure_reason is nonempty, that whole branch is
    // skipped and the "stalled" criterion never gets written. The canonical casualty is a harness
    // emitting run.blocked (saw_blocked=true), then run.failed (saw_error=true and failure_reason
    // nonempty): an honest stall was classified by the frontend as an "env" failure because the
    // later agent Error won the race, contradicting the goal of honest completion. Decide it
    // independently instead: any real Blocked/NeedsDecision event marks stalled, regardless of
    // whether message synthesis runs. This opens no spoofing path: saw_blocked/saw_needs_decision
    // can only be set by real events from the harness parser. An agent-originated Error could at
    // worst previously make a run that should be stalled lose the marker (fixed here); it has no
    // reverse path that can promote itself to stalled from nothing.
    // budget_exhausted / context_exhausted structured routing trusts only
    // AgentEvent::Blocked.reason. agent_event.rs fills that field only when (1) trigger=="harness"
    // and a whitelist entry such as budget_exhausted_still_progressing matches, or (2) top-level
    // reason literally equals "context_budget_exhausted" (a per-turn context token budget overflow;
    // see the agent_event.rs::harness_context_budget_exhausted_reason documentation: it does not
    // share (1)'s emit point, has no blocked_reason/trigger fields, and the agent has no input path
    // to it). Mimicking the text cannot bypass this. Nonmatches retain the old "stalled" path
    // (no_progress / stuck_repeating / agent-initiated block_with_questions all stay there; this
    // change adds only the fourth "context_budget_exhausted" category and does not alter the
    // existing budget_exhausted_still_progressing behavior).
    let is_budget_exhausted =
        state.blocked_reason.as_deref() == Some("budget_exhausted_still_progressing");
    let is_context_exhausted = state.blocked_reason.as_deref() == Some("context_budget_exhausted");
    if matches!(state.status, StatusTransition::Failed)
        && !state.stopped
        && (state.saw_blocked || state.saw_needs_decision)
    {
        state.failure_kind = Some(if is_budget_exhausted {
            "budget_exhausted"
        } else if is_context_exhausted {
            "context_exhausted"
        } else {
            "stalled"
        });
    }
    synthesize_failure_message(
        state,
        run_id,
        spec,
        locale,
        emit,
        is_budget_exhausted,
        is_context_exhausted,
    );
}

pub(super) fn synthesize_failure_message(
    state: &mut MemberFinalizationState,
    run_id: &str,
    spec: &MemberSpec,
    locale: crate::Locale,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    is_budget_exhausted: bool,
    is_context_exhausted: bool,
) {
    // P2 (this change: honest prose is no longer displaced by raw engine errors): failure_reason
    // is assigned unconditionally in the Error branch above. If an attempt contains any nonempty
    // Error event, `failure_reason.is_none()` is always false, short-circuiting the entire gate
    // that decides whether to synthesize honest prose. Thus the actionable budget_exhausted /
    // context_exhausted explanation was never written and users saw only the raw engine error.
    // This is an existence short circuit, independent of event arrival order.
    //
    // Fix: open the gate only for budget_exhausted / context_exhausted. Even if an Error already
    // occupies failure_reason, synthesize the honest prose and append the displaced raw Error
    // afterward (preserving diagnostics without letting them replace the explanation). Do not
    // open the stalled branch: `run_member_reader_harness_blocked_then_agent_reported_error_still_stalled`
    // locks in the existing behavior that preserves the agent's earlier raw Error verbatim as
    // failure_reason without replacing it with honest prose.
    let overridden_error_text = if is_budget_exhausted || is_context_exhausted {
        state.failure_reason.clone()
    } else {
        None
    };
    let should_synthesize_message = matches!(state.status, StatusTransition::Failed)
        && !state.stopped
        && (state.failure_reason.is_none() || is_budget_exhausted || is_context_exhausted);
    if !should_synthesize_message {
        return;
    }
    // P1: a Blocked/NeedsDecision event (harness contract exit code 3/4) means the member stopped
    // normally because it stalled or needs a decision, not because the environment failed. Use
    // honest wording instead of misleading users to check CLI login/quota/network. This applies
    // only to harness-parser members: only parse_harness_line_for_locale can set these flags, so
    // Claude/Codex exit code 3 cannot trigger it. P2-3 (adversarial review): these flags choose the
    // message only; terminal_status itself ignores them (clean exit plus Blocked stays Done and
    // relay continues, as documented above the original function).
    //
    // is_budget_exhausted/is_context_exhausted necessarily imply saw_blocked because both can only
    // come from a structured reason on AgentEvent::Blocked (see blocked_reason's "nonempty wins"
    // comment). Therefore the newly opened budget/context branches always take this path and can
    // never fall into the generic process-failure synthesis below. That branch remains restricted
    // to the old zero-signal case, preserving its behavior.
    let message = if state.saw_blocked || state.saw_needs_decision {
        honest_failure_message(
            state,
            locale,
            is_budget_exhausted,
            is_context_exhausted,
            overridden_error_text.as_deref(),
        )
    } else {
        // Only this branch synthesizes a generic process-failure message from scratch: there is no
        // more specific signal (not stalled, and no readable Error text from the agent), so only it
        // may set "env". Other Failed sources (saw_error with real auth/quota text, blocking-write,
        // or stage1 relay failure) remain for the frontend's existing regex classification chain;
        // do not blanket them with "env" here.
        state.failure_kind = Some("env");
        crate::cli_exit_failure_message(
            locale,
            &spec.agent_name,
            state.exit_status.as_ref(),
            &state.stderr_tail,
        )
    };
    emit(
        member_dispatch_meta(run_id, spec, None),
        AgentEvent::Error {
            message: message.clone(),
        },
    );
    state.failure_reason = Some(message);
}

pub(super) fn honest_failure_message(
    state: &MemberFinalizationState,
    locale: crate::Locale,
    is_budget_exhausted: bool,
    is_context_exhausted: bool,
    overridden_error_text: Option<&str>,
) -> String {
    let mut message = if is_budget_exhausted {
        crate::member_budget_exhausted_failure_message(locale)
    } else if is_context_exhausted {
        crate::member_context_exhausted_failure_message(locale)
    } else {
        crate::member_stall_failure_message(
            locale,
            state.saw_blocked,
            state.saw_needs_decision,
            state.exit_status.as_ref(),
        )
        .expect("saw_blocked || saw_needs_decision guarantees Some")
    };
    // P2-6: on the same protocol path, the harness parser has already rendered the real reason for
    // a stall/interruption as human-readable text (harness_blocked_message /
    // harness_interrupted_message; see read_member_attempt's Blocked match arm). Append it so users
    // need not inspect the trace. run.interrupted also arrives as a Blocked event, and its message
    // explicitly says the run was interrupted, distinguishing a real interruption from the generic
    // "waiting for an answer / blocked" frame.
    if let Some(detail) = state.blocked_message.as_deref().map(str::trim) {
        if !detail.is_empty() {
            message.push('\n');
            message.push_str(detail);
        }
    }
    // Only budget/context populate overridden_error_text (see the opened condition in
    // should_synthesize_message). It is the original engine Error that previously occupied
    // failure_reason. The concatenation order is "honest prose -> blocked_message detail -> raw
    // Error": the actionable honest explanation is most important; blocked_message is the real
    // human-readable reason from the same harness protocol and is more relevant than a separate
    // agent/engine Error, so it is second; the raw Error is retained only as trailing diagnostics.
    // This matches the existing blocked_message append style and introduces no new format.
    //
    // Adversarial-review patch: this was previously appended bare, making it look like part of the
    // honest prose (for example, "you can dispatch another task" followed by an auth error). Add the
    // bilingual overridden_error_lead_in to mark it as a separate engine error. Add it only here:
    // the blocked_message above must remain bare because frontend humanizeFailureDetail anchors a
    // regex on a known raw code immediately after its delimiter; extra prose would break the anchor.
    if let Some(raw) = overridden_error_text.map(str::trim) {
        if !raw.is_empty() {
            message.push('\n');
            message.push_str(overridden_error_lead_in(locale));
            message.push_str(raw);
        }
    }
    message
}

pub(super) fn apply_post_run_outcomes(
    state: &mut MemberFinalizationState,
    wt: &std::path::Path,
    base_sha: &str,
    run_id: &str,
    spec: &MemberSpec,
    locale: crate::Locale,
    stage1: Option<&Stage1Ctx>,
) -> MemberResultArtifacts {
    let (changed_files, anchor) = crate::worktree::synthesize_hard_fields(wt, base_sha);
    let command_evidence = derive_command_evidence(&state.tool_events, &spec.provider);
    let git_wall = detect_git_wall_block(&state.tool_events);
    // Member return text: prefer Completed.final_text when the parser supplies it; otherwise use the
    // provider-neutral accumulated TextDelta body (for example Codex final_text is always None and
    // finishes via streaming), so the lead can see worker output without dispatching another reader.
    let completed_final = match &state.buffered {
        Some(AgentEvent::Completed { final_text, .. }) => {
            final_text.as_deref().filter(|s| !s.trim().is_empty())
        }
        _ => None,
    };
    let final_text = completed_final
        .or_else(|| {
            let text = state.assistant_text_only.trim();
            (!text.is_empty()).then_some(text)
        })
        .map(str::to_string);
    let mut scan_text = std::mem::take(&mut state.assistant_text);
    if let Some(text) = final_text.as_deref() {
        scan_text.push_str(text);
    }
    if matches!(state.status, StatusTransition::Done) && changed_files.is_empty() {
        if let Some(marker) = detect_blocking_write_failure(&scan_text) {
            state.status = StatusTransition::Failed;
            if state.failure_reason.is_none() {
                state.failure_reason = Some(blocking_write_failure_message(locale, &marker));
            }
        }
    }
    // Stage 1 (final-review fix): calculate this before build_member_result so a relay failure can
    // downgrade the terminal status.
    let changed = !changed_files.is_empty();
    let session_head_sha = match stage1 {
        Some(ctx) if matches!(state.status, StatusTransition::Done) => {
            match run_stage1_for_locale(locale, ctx, run_id, base_sha, changed) {
                Stage1Result::Relayed { session_head } => Some(session_head),
                Stage1Result::NoChanges => None,
                Stage1Result::Failed { reason } => {
                    // The worker completed but its changes did not reach the session: downgrade to
                    // Failed with the reason instead of reporting Done to the lead (which would make
                    // the lead believe relay succeeded while the next worker cannot see the changes,
                    // violating honest reporting / G1).
                    state.status = StatusTransition::Failed;
                    if state.failure_reason.is_none() {
                        state.failure_reason = Some(reason);
                    }
                    None
                }
            }
        }
        _ => None,
    };
    MemberResultArtifacts {
        changed_files,
        anchor,
        command_evidence,
        git_wall,
        session_head_sha,
        final_text,
    }
}

pub(super) fn build_and_emit_member_result(
    mut state: MemberFinalizationState,
    artifacts: MemberResultArtifacts,
    run_id: &str,
    spec: &MemberSpec,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
) -> bool {
    let transient_error = if matches!(state.status, StatusTransition::Done) && state.saw_error {
        state.failure_reason.take().map(|message| Risk {
            id: MEMBER_RESULT_TRANSIENT_ERROR_RISK_ID.into(),
            text: transient_error_note(&message),
            source_refs: vec![],
            confidence: None,
            source_kind: Some("member_runner".into()),
        })
    } else {
        None
    };
    // D7: Done after a Blocked/NeedsDecision event is an odd contract combination. Record a risk
    // (as transient_error does) instead of letting it pass silently.
    let stalled_on_done = if matches!(state.status, StatusTransition::Done)
        && (state.saw_blocked || state.saw_needs_decision)
    {
        Some(Risk {
            id: STALLED_ON_DONE_RISK_ID.into(),
            text: "队员进程干净退出（exit 0），但过程里见过 Blocked/NeedsDecision 叙事事件\
（harness 契约退出码 3/4 语义）——终态仍按 Done 处理（维持既有基线行为），这里留痕供排查。"
                .into(),
            source_refs: vec![],
            confidence: None,
            source_kind: Some("member_runner".into()),
        })
    } else {
        None
    };
    let final_text_ref = artifacts.final_text.as_deref();
    let mut member_result = build_member_result(
        spec,
        state.status,
        artifacts.changed_files,
        artifacts.anchor,
        artifacts.command_evidence,
        final_text_ref,
    );
    // P2-8: normalize once more at the end. If a future upstream failure path supplies an empty
    // string, it must not bypass the invariant that terminal Failed has a nonempty failure_reason.
    member_result.failure_reason = state.failure_reason.filter(|r| !r.trim().is_empty());
    // P1-2: failure_kind is written only after seeing Blocked/NeedsDecision or taking the generic
    // process-failure synthesis path (see classify_and_synthesize_failure). Other terminal sources,
    // such as blocking-write and stage1 relay failure, leave it unset for the frontend's existing
    // text-heuristic fallback instead of impersonating a structured category that did not cover them.
    member_result.failure_kind = state.failure_kind.map(str::to_string);
    // P2-7: persist exit_code/stderr_tail only for a real failure or stop. A clean Done run need not
    // store up to 4 KB of stderr (a common carrier of tokens/credentials) in the DB blocks JSON.
    if matches!(
        state.status,
        StatusTransition::Failed | StatusTransition::Stopped
    ) {
        member_result.exit_code = state
            .exit_status
            .as_ref()
            .and_then(std::process::ExitStatus::code);
        member_result.stderr_tail = {
            let trimmed = state.stderr_tail.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
    }
    if let Some(risk) = transient_error {
        member_result.risks.push(risk);
    }
    if let Some(risk) = stalled_on_done {
        member_result.risks.push(risk);
    }
    if let Some(command) = artifacts.git_wall {
        let clipped = clip_member_result_field(&command, 120);
        member_result.risks.push(Risk {
            id: GIT_WALL_BLOCKED_RISK_ID.into(),
            text: format!(
                "agent 试图 git 写（{clipped}）但被沙箱挡下、未执行（.git 只读）。如需回滚请用替代法或手动处理。"
            ),
            source_refs: vec![],
            confidence: None,
            source_kind: Some("member_runner".into()),
        });
    }
    crate::agent_event::maybe_mark_long_task(&mut member_result, state.status, final_text_ref);
    let (meta, event) = member_terminal_event(
        run_id,
        spec,
        state.buffered,
        state.status,
        Some(member_result),
        artifacts.session_head_sha,
    );
    emit(meta, event);
    state.run_done
}
