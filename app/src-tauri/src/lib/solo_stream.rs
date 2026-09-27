use super::*;
use agent_event::AgentEvent;

pub(crate) struct SoloStreamCtx {
    pub(crate) app: AppHandle,
    pub(crate) running: Running,
    pub(crate) team_running: member_runner::TeamRunning,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) wt: std::path::PathBuf,
    pub(crate) engine: String,
    pub(crate) parser: fn(&str) -> Vec<AgentEvent>,
    pub(crate) parse_fn: ParseFn,
    pub(crate) first_event_engine: String,
    pub(crate) first_event_binary: String,
    pub(crate) transport: event_transport::EventTransport,
}

pub(crate) struct SoloStreamGuards {
    pub(crate) hook_guard: Option<checkpoint_hook::HookRunGuard>,
    pub(crate) solo_mcp_server: Option<mcp_server::McpServer>,
}

pub(crate) struct SoloStreamTiming {
    pub(crate) pid: u32,
    pub(crate) first_event_deadline: Instant,
    pub(crate) run_started_at: std::time::SystemTime,
}

struct AttemptEvents {
    reducer: display_reduce::DisplayReducer,
    pending_completed: Option<AgentEvent>,
    pending_terminals: Vec<AgentEvent>,
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    last_error_message: Option<String>,
    codex_thread_id: Option<String>,
}

struct AttemptIo {
    stderr_tail: Option<std::thread::JoinHandle<String>>,
    stderr_live_tail: SharedStderrTail,
    first_event_watchdog: FirstEventWatchdogSignal,
    first_event_watchdog_handle: std::thread::JoinHandle<()>,
}

struct AttemptCloseout {
    exit_status: Option<ExitStatus>,
    exit_success: bool,
    interrupted: bool,
    first_event_timeout_stderr: Option<String>,
    stderr_tail: String,
    closeout_continuation: FinalizerCloseoutContinuation,
}

pub(crate) fn run_solo_stream(
    mut ctx: SoloStreamCtx,
    mut child: Child,
    mut command: Command,
    stdin_prompt: Option<agent::StdinPrompt>,
    guards: SoloStreamGuards,
    timing: SoloStreamTiming,
) {
    let SoloStreamGuards {
        hook_guard,
        solo_mcp_server,
    } = guards;
    let SoloStreamTiming {
        pid,
        first_event_deadline,
        run_started_at,
    } = timing;
    // Keep the in-process MCP server alive for the entire solo run, including auth retries.
    let _solo_mcp_server = solo_mcp_server;
    let mut retry_count = 0;
    let mut latest_context_compacted: Option<(String, i64)> = None;
    let mut current_pid = pid;
    let mut current_first_event_deadline = first_event_deadline;
    let (events, closeout) = loop {
        let io = start_attempt_io(&ctx, &mut child, current_pid, current_first_event_deadline);
        let (next_ctx, mut events) = pump_stdout(
            ctx,
            &mut child,
            &io.first_event_watchdog,
            &mut latest_context_compacted,
        );
        let (next_ctx, mut closeout) = wait_for_attempt(
            next_ctx,
            &mut child,
            current_pid,
            current_first_event_deadline,
            io,
            &events,
        );
        ctx = next_ctx;
        record_attempt_errors(&ctx, &mut events, &closeout);
        if let Some((retry_child, retry_pid, retry_deadline)) = retry_auth(
            &ctx,
            &mut command,
            stdin_prompt.as_ref(),
            &mut retry_count,
            &mut events,
            &mut closeout,
        ) {
            current_pid = retry_pid;
            current_first_event_deadline = retry_deadline;
            child = retry_child;
            continue;
        }
        break (events, closeout);
    };

    // Revoke before the Running slot can be released: inherited background tokens must not
    // be able to mutate a completed run's checkpoint ledger.
    drop(hook_guard);

    finish_solo_stream(
        ctx,
        events,
        closeout,
        latest_context_compacted,
        run_started_at,
    );
}

fn start_attempt_io(
    ctx: &SoloStreamCtx,
    child: &mut Child,
    current_pid: u32,
    current_first_event_deadline: Instant,
) -> AttemptIo {
    let (stderr_tail, stderr_live_tail) = match child.stderr.take() {
        Some(stderr) => {
            let (handle, tail) =
                spawn_stderr_tail_thread_shared(stderr, log_file_for(&ctx.session_id));
            (Some(handle), tail)
        }
        None => (None, Arc::new(Mutex::new(Vec::new()))),
    };
    let (first_event_watchdog, first_event_watchdog_handle) = spawn_first_event_watchdog(
        ctx.running.clone(),
        ctx.session_id.clone(),
        current_pid,
        stderr_live_tail.clone(),
        current_first_event_deadline.saturating_duration_since(Instant::now()),
    );
    AttemptIo {
        stderr_tail,
        stderr_live_tail,
        first_event_watchdog,
        first_event_watchdog_handle,
    }
}

fn pump_stdout(
    ctx: SoloStreamCtx,
    child: &mut Child,
    first_event_watchdog: &FirstEventWatchdogSignal,
    latest_context_compacted: &mut Option<(String, i64)>,
) -> (SoloStreamCtx, AttemptEvents) {
    let SoloStreamCtx {
        run_id, transport, ..
    } = ctx;
    let mut reducer = display_reduce::DisplayReducer::new(&run_id);
    let mut pending_completed: Option<AgentEvent> = None;
    let mut pending_terminals: Vec<AgentEvent> = Vec::new();
    let mut saw_error = false;
    let mut saw_blocked = false;
    let mut saw_needs_decision = false;
    let mut last_error_message: Option<String> = None;
    let mut codex_thread_id: Option<String> = None;
    let mut harness_plan_filter = if matches!(ctx.parse_fn, ParseFn::HarnessPlan) {
        Some(agent_event::HarnessPlanDisplayFilter::default())
    } else {
        None
    };
    let reader = child.stdout.take().map(BufReader::new);
    for line in reader.into_iter().flat_map(BufRead::lines) {
        let Ok(line) = line else { break };
        first_event_watchdog.first_line_seen();
        let locale = current_locale(&ctx.app);
        let parsed_events = if locale == Locale::Zh {
            (ctx.parser)(&line)
        } else {
            parse_agent_line_for_locale(ctx.parse_fn, &line, locale)
        };
        let events = match harness_plan_filter.as_mut() {
            Some(filter) => filter.apply(&line, parsed_events),
            None => parsed_events,
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
                    summary: agent_event::relativize_summary(&summary, &ctx.wt),
                    card,
                },
                event => event,
            };
            remember_context_compacted(latest_context_compacted, &event);
            if codex_thread_id.is_none() {
                if let Some(thread_id) = codex_thread_id_from_event(ctx.parse_fn, &event) {
                    codex_thread_id = Some(thread_id.to_string());
                }
            }
            reducer.feed(&event);
            match &event {
                AgentEvent::Completed { .. } => {
                    pending_completed = Some(event.clone());
                    continue; // Stash without emitting; wait for the finalizer to produce a single terminal state.
                }
                AgentEvent::Error { message } => {
                    saw_error = true;
                    last_error_message = Some(message.clone());
                    pending_terminals.push(event);
                    continue;
                }
                AgentEvent::Blocked { .. } => {
                    saw_blocked = true;
                    pending_terminals.push(event);
                    continue;
                }
                AgentEvent::NeedsDecision { .. } => {
                    saw_needs_decision = true;
                    pending_terminals.push(event);
                    continue;
                }
                _ => {}
            }
            let _ = transport.push(&run_id, event);
        }
    }
    (
        SoloStreamCtx {
            run_id,
            transport,
            ..ctx
        },
        AttemptEvents {
            reducer,
            pending_completed,
            pending_terminals,
            saw_error,
            saw_blocked,
            saw_needs_decision,
            last_error_message,
            codex_thread_id,
        },
    )
}

fn wait_for_attempt(
    ctx: SoloStreamCtx,
    child: &mut Child,
    current_pid: u32,
    current_first_event_deadline: Instant,
    io: AttemptIo,
    events: &AttemptEvents,
) -> (SoloStreamCtx, AttemptCloseout) {
    let SoloStreamCtx {
        running: running_t,
        session_id,
        ..
    } = ctx;
    let AttemptIo {
        stderr_tail,
        stderr_live_tail,
        first_event_watchdog,
        first_event_watchdog_handle,
    } = io;
    let pending_completed = &events.pending_completed;
    let first_line_seen = first_event_watchdog.stdout_closed();
    let _ = first_event_watchdog_handle.join();
    // Once stdout is exhausted, transition to Finalizing (without exposing the pid; stop in
    // this state only sets the flag, without killpg), carrying any existing stop_requested.
    let stop_requested = transition_stdout_closed_to_finalizing(&running_t, &session_id);
    // After the first line arrives normally, allow only a short grace period for process exit;
    // without a first line, retain the watchdog deadline measured from spawn. At either
    // deadline, only best-effort kill; do not reap synchronously and block subsequent persistence.
    let child_wait_deadline = if first_line_seen {
        Instant::now() + FINALIZER_OWNER_WAIT_TIMEOUT
    } else {
        current_first_event_deadline
    };
    let (exit_status, owner_timed_out, closeout_continuation) = finalizer_owner_wait(
        child,
        child_wait_deadline,
        |child| Child::try_wait(child).map(|status| status.is_some()),
        Child::wait,
        || {
            // On Windows, taskkill /T /F best-effort terminates the process tree;
            // Job Object ownership remains an intentionally separate v2 change.
            kill_process_group(current_pid)
        },
        Instant::now,
        std::thread::sleep,
        |outcome| {
            prepare_finalizer_closeout(
                outcome,
                first_line_seen,
                pending_completed.as_ref(),
                ExitStatus::success,
            )
        },
    );
    let owner_timeout_stderr = owner_timed_out.then(|| stderr_tail_last_lines(&stderr_live_tail));
    let first_event_timeout_stderr = first_event_watchdog
        .timeout_stderr()
        .or(owner_timeout_stderr);
    // A parsed Completed event remains authoritative when cleanup alone timed out.
    let exit_success = closeout_continuation.exit_success();
    let stderr_tail = stderr_tail
        .map(|handle| {
            finalizer_owner_wait(
                handle,
                Instant::now() + FINALIZER_OWNER_WAIT_TIMEOUT,
                |handle| Ok::<_, std::convert::Infallible>(handle.is_finished()),
                |handle| handle.join().map_err(|_| ()),
                || {
                    // On Windows, taskkill /T /F best-effort terminates the process tree;
                    // Job Object ownership remains an intentionally separate v2 change.
                    kill_process_group(current_pid)
                },
                Instant::now,
                std::thread::sleep,
                |outcome| finalizer_stderr_tail_after_owner_wait(outcome, &stderr_live_tail),
            )
        })
        .unwrap_or_default();
    // If the stop flag is set after waiting (the user clicked stop during finalizing), treat the run as interrupted.
    let interrupted = stop_requested || finalizer_stop_requested(&running_t, &session_id);
    (
        SoloStreamCtx {
            running: running_t,
            session_id,
            ..ctx
        },
        AttemptCloseout {
            exit_status,
            exit_success,
            interrupted,
            first_event_timeout_stderr,
            stderr_tail,
            closeout_continuation,
        },
    )
}

fn record_attempt_errors(
    ctx: &SoloStreamCtx,
    events: &mut AttemptEvents,
    closeout: &AttemptCloseout,
) {
    if should_inject_first_event_watchdog_error(
        closeout.interrupted,
        events.pending_completed.is_some(),
        closeout.first_event_timeout_stderr.as_deref(),
    ) {
        let stderr_summary = closeout
            .first_event_timeout_stderr
            .as_ref()
            .expect("watchdog injection predicate requires timeout stderr");
        let message = first_event_watchdog_error_message(
            current_locale(&ctx.app),
            "run.spawnFailed",
            &ctx.first_event_engine,
            &ctx.first_event_binary,
            stderr_summary,
        );
        events.last_error_message = Some(message.clone());
        let event = record_synthetic_cli_error(&mut events.reducer, message);
        events.pending_terminals.push(event);
        events.saw_error = true;
    }

    if agent::sidecar_exit_error(
        events.saw_error,
        events.saw_blocked,
        events.saw_needs_decision,
        closeout.exit_success,
        closeout.interrupted,
    ) {
        let message = cli_exit_failure_message(
            current_locale(&ctx.app),
            &ctx.engine,
            closeout.exit_status.as_ref(),
            &closeout.stderr_tail,
        );
        events.last_error_message = Some(message.clone());
        let event = record_synthetic_cli_error(&mut events.reducer, message);
        events.pending_terminals.push(event);
        events.saw_error = true;
    }
}

fn retry_auth(
    ctx: &SoloStreamCtx,
    command: &mut Command,
    stdin_prompt: Option<&agent::StdinPrompt>,
    retry_count: &mut u32,
    events: &mut AttemptEvents,
    closeout: &mut AttemptCloseout,
) -> Option<(Child, u32, Instant)> {
    let should_retry_auth = events.saw_error
        && !closeout.interrupted
        && events
            .last_error_message
            .as_deref()
            .is_some_and(agent_event::is_auth_error)
        && *retry_count < agent_event::AUTH_RETRY_MAX;
    if should_retry_auth {
        *retry_count += 1;
        std::thread::sleep(std::time::Duration::from_millis(
            350 * u64::from(*retry_count),
        ));
        let retry_started_at = Instant::now();
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        match agent::spawn_with_stdin_prompt(command, stdin_prompt) {
            Ok(mut retry_child) => {
                let retry_pid = retry_child.id();
                let handoff =
                    transition_auth_retry_handoff(&ctx.running, &ctx.session_id, retry_pid);
                let handoff = resolve_auth_retry_handoff(
                    handoff,
                    || {
                        kill_process_group(retry_pid);
                        wait_for_child_cleanup_bounded(&mut retry_child, retry_pid);
                    },
                    || finalizer_stop_requested(&ctx.running, &ctx.session_id),
                );
                match handoff {
                    AuthRetryHandoff::Continue => {
                        checkpoint_hook::register_agent_pid(command, retry_pid);
                        return Some((
                            retry_child,
                            retry_pid,
                            retry_started_at
                                + std::time::Duration::from_secs(FIRST_EVENT_TIMEOUT_SECS),
                        ));
                    }
                    AuthRetryHandoff::Interrupted => {
                        closeout.interrupted = true;
                    }
                    AuthRetryHandoff::Failed { detail } => {
                        let message = ui_msg::al_err("run.spawnFailed", &[("detail", detail)]);
                        let event = record_synthetic_cli_error(&mut events.reducer, message);
                        events.pending_terminals.push(event);
                        events.saw_error = true;
                    }
                }
            }
            Err(error) => {
                let message = ui_msg::al_err("run.spawnFailed", &[("detail", error.to_string())]);
                let event = record_synthetic_cli_error(&mut events.reducer, message);
                events.pending_terminals.push(event);
                events.saw_error = true;
            }
        }
    }
    None
}

fn emit_codex_images(
    ctx: SoloStreamCtx,
    events: &mut AttemptEvents,
    closeout: &AttemptCloseout,
    run_started_at: std::time::SystemTime,
) -> SoloStreamCtx {
    let SoloStreamCtx {
        run_id, transport, ..
    } = ctx;
    if matches!(ctx.parse_fn, ParseFn::Codex)
        && events.pending_completed.is_some()
        && closeout.exit_success
        && !closeout.interrupted
        && !events.saw_error
        && !events.saw_blocked
        && !events.saw_needs_decision
    {
        if let Some(images_dir) = events
            .codex_thread_id
            .as_deref()
            .and_then(codex_generated_images_dir)
        {
            let since = run_started_at
                .checked_sub(std::time::Duration::from_secs(2))
                .unwrap_or(std::time::UNIX_EPOCH);
            let images = scan_new_images(&images_dir, since);
            if !images.is_empty() {
                for event in codex_image_tool_events(&run_id, &images) {
                    events.reducer.feed(&event);
                    let _ = transport.push(&run_id, event);
                }
            }
        }
    }
    SoloStreamCtx {
        run_id,
        transport,
        ..ctx
    }
}

fn finish_solo_stream(
    ctx: SoloStreamCtx,
    mut events: AttemptEvents,
    closeout: AttemptCloseout,
    latest_context_compacted: Option<(String, i64)>,
    run_started_at: std::time::SystemTime,
) {
    let ctx = emit_codex_images(ctx, &mut events, &closeout, run_started_at);
    let SoloStreamCtx {
        app: app_t,
        running: running_t,
        team_running: team_running_t,
        session_id,
        run_id,
        engine,
        transport,
        ..
    } = ctx;
    let AttemptEvents {
        reducer,
        pending_completed,
        mut pending_terminals,
        saw_error,
        saw_blocked,
        saw_needs_decision,
        ..
    } = events;
    let AttemptCloseout {
        exit_success,
        interrupted,
        closeout_continuation,
        ..
    } = closeout;
    // The app no longer performs any git closeout on the worktree; it only clears its own old pending ledger.
    let db = app_t.state::<Db>();
    persist_context_compacted(&db, &session_id, &run_id, latest_context_compacted.as_ref());
    let closeout = if let Ok(conn) = db.0.lock() {
        match finish_run_without_git_writes(&conn, &session_id, &run_id, interrupted) {
            Ok(closeout) => closeout,
            Err(error) => {
                eprintln!("finish run ledger cleanup failed (non-fatal): {error}");
                db::RunCloseoutMetadata::default()
            }
        }
    } else {
        db::RunCloseoutMetadata::default()
    };
    let final_text_for_outcome = match &pending_completed {
        Some(AgentEvent::Completed { final_text, .. }) => final_text.clone(),
        _ => None,
    };
    let completed_usage = match &pending_completed {
        Some(AgentEvent::Completed {
            input_tokens,
            output_tokens,
            ..
        }) => Some((*input_tokens, *output_tokens)),
        _ => None,
    };
    let terminal_release_event = build_terminal_release_event(
        &run_id,
        pending_completed.as_ref(),
        saw_error,
        saw_blocked,
        saw_needs_decision,
        &closeout,
        interrupted,
    );

    // The reducer decides closeout: persist whenever there is output (display_reduce.rs is
    // the only place for that decision; this code only assembles facts and calls persistence,
    // without making any decisions).
    let outcome = display_reduce::RunOutcome {
        run_id: run_id.clone(),
        exit_success,
        interrupted,
        saw_error,
        saw_blocked,
        saw_needs_decision,
        finish_called: None,
        commit_sha: closeout.commit_sha.clone(),
        files_changed: closeout.files_changed,
        insertions: closeout.insertions,
        deletions: closeout.deletions,
        final_text: final_text_for_outcome,
    };
    let mut reduced_message = reducer.finish(&outcome);
    if let Some(message) = reduced_message.as_mut() {
        localize_reduced_message(current_locale(&app_t), message);
    }
    closeout_continuation.persist_then_emit(
        || {
            persist_normal_finalizer_if_needed(
                &db,
                &session_id,
                &engine,
                reduced_message.as_ref(),
                completed_usage,
            );
        },
        || {
            // RunCloseout / metadata-bearing Completed is the only release signal: emit only after
            // both ledger and reducer are persisted and the slot is actually released, preventing
            // the composer from racing ahead and interleaving the old and new runs.
            pending_terminals.push(terminal_release_event);
            // Pass an unlocked `&Db` so the release helper controls lock lifetime and avoids holding it across recovery.
            // Release is fully encapsulated in emit_terminal_after_releasing_run_slot; do not lock in advance here.
            let _ = emit_terminal_after_releasing_run_slot(
                &running_t,
                &team_running_t,
                &session_id,
                &run_id,
                pending_terminals,
                &transport,
                Some(db.inner()),
            );
        },
    );
    // Solo run closeout also releases a run slot. drain_after_run_release previously omitted
    // this path, leaving pending remote input in solo sessions waiting forever to be drained.
    // The lock scope above ends entirely inside emit_terminal_after_releasing_run_slot, so
    // this call does not span a lock.
    drain_after_run_release(app_t.clone(), session_id.clone());
}
