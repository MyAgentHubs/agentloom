use super::super::*;
use super::LeadRunnerCtx;
use std::sync::atomic::Ordering;

pub(crate) struct LeadStreamWitness {
    saw_completed: bool,
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    pending_terminals: Vec<agent_event::AgentEvent>,
    lead_completed_usage: Option<(Option<u64>, Option<u64>)>,
    latest_context_compacted: Option<(String, i64)>,
}

pub(crate) struct LeadExit {
    exit_status: Option<std::process::ExitStatus>,
    first_event_timeout_stderr: Option<String>,
    exit_success: bool,
    stderr_tail: String,
}

pub(crate) struct LeadCloseout {
    reducer: display_reduce::DisplayReducer,
    witness: LeadStreamWitness,
    stopped: bool,
    finish_called: bool,
    exit_success: bool,
}

pub(crate) struct LeadDelivery {
    pub(crate) stdin_ack: Option<std::sync::mpsc::Receiver<std::io::Result<()>>>,
    pub(crate) in_flight_report_ids_t: Vec<i64>,
    pub(crate) in_flight_answer_ids_t: Vec<i64>,
}

pub(crate) fn pump_lead_stdout(
    child: &mut std::process::Child,
    ctx: &LeadRunnerCtx,
    reducer: &mut display_reduce::DisplayReducer,
    first_event_watchdog: &FirstEventWatchdogSignal,
) -> LeadStreamWitness {
    let lead_run_id = std::borrow::Cow::Borrowed(ctx.run_id.as_str());
    let transport = &ctx.transport;
    // Read stdout, emit events in real time; track terminal events
    let mut saw_completed = false;
    let mut saw_error = false;
    // Lead must also record blocked/needs_decision terminal witnesses; otherwise lead_terminal_decision
    // would misclassify exit codes 3/4 (normal Blocked/NeedsDecision completion, not crashes)
    // as EmitError, contrary to the orchestrator's exit-code contract.
    let mut saw_blocked = false;
    let mut saw_needs_decision = false;
    let mut pending_terminals = Vec::new();
    // Use the last completion event from this lead run to account for its own usage without counting intermediate completions.
    // This follows solo's `pending_completed` semantics: only the last of multiple Completed events counts.
    // Parse Claude/borrowed lead output with `parse_claude_line_for_locale` so cache usage follows the same accounting semantics.
    // Harness uses parse_agent_line_for_locale(Harness) (`run.completed.payload.usage`, with its engine's own
    // accounting semantics, outside this change's scope). Persistence happens exactly once at closeout and does
    // not conflict with RunInfo.workingTokens (display-only state, never written to the DB by design);
    // see the persistence comment below.
    let mut lead_completed_usage: Option<(Option<u64>, Option<u64>)> = None;
    let mut latest_context_compacted: Option<(String, i64)> = None;
    if let Some(stdout) = child.stdout.take() {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            first_event_watchdog.first_line_seen();
            let locale = current_locale(&ctx.app);
            // NativeClaude/BorrowClaude keep their original path byte-for-byte (pinned by regression checks);
            // Harness uses myagent parsing (parse_agent_line_for_locale already dispatches by ParseFn).
            for event in match ctx.lead_engine {
                LeadEngine::NativeClaude | LeadEngine::BorrowClaude => {
                    if locale == Locale::Zh {
                        agent_event::parse_claude_line(&line)
                    } else {
                        agent_event::parse_claude_line_for_locale(&line, locale)
                    }
                }
                LeadEngine::Harness => parse_agent_line_for_locale(ParseFn::Harness, &line, locale),
            } {
                // Lead transcripts currently have no marker and do not trigger compaction; this wiring is reserved for lead compaction.
                remember_context_compacted(&mut latest_context_compacted, &event);
                reducer.feed(&event);
                // Borrow usage from the event without consuming it so lead accounting preserves subsequent event handling.
                // Only the last of multiple Completed events counts, matching solo's `pending_completed` overwrite semantics.
                if let agent_event::AgentEvent::Completed {
                    input_tokens,
                    output_tokens,
                    ..
                } = &event
                {
                    lead_completed_usage = Some((*input_tokens, *output_tokens));
                }
                match event {
                    agent_event::AgentEvent::Completed { .. } => {
                        saw_completed = true;
                        pending_terminals.push(event);
                    }
                    agent_event::AgentEvent::Error { .. } => {
                        saw_error = true;
                        pending_terminals.push(event);
                    }
                    agent_event::AgentEvent::NeedsDecision { .. } => {
                        saw_needs_decision = true;
                        pending_terminals.push(event);
                    }
                    agent_event::AgentEvent::Blocked { .. } => {
                        saw_blocked = true;
                        pending_terminals.push(event);
                    }
                    agent_event::AgentEvent::RunCloseout { .. } => {
                        pending_terminals.push(event);
                    }
                    event => {
                        transport.push(&lead_run_id, event);
                    }
                }
            }
        }
    }
    LeadStreamWitness {
        saw_completed,
        saw_error,
        saw_blocked,
        saw_needs_decision,
        pending_terminals,
        lead_completed_usage,
        latest_context_compacted,
    }
}

pub(crate) fn wait_lead_exit(
    child: &mut std::process::Child,
    pid: u32,
    first_line_seen: bool,
    first_event_deadline: Instant,
    stderr_live_tail: SharedStderrTail,
    first_event_watchdog: FirstEventWatchdogSignal,
    stderr_handle: Option<std::thread::JoinHandle<String>>,
) -> LeadExit {
    let (exit_status, owner_timed_out) = if first_line_seen {
        (child.wait().ok(), false)
    } else {
        match wait_for_first_event_owner(
            child,
            pid,
            first_event_deadline,
            Child::try_wait,
            Child::wait,
            kill_process_group,
            Instant::now,
            std::thread::sleep,
        ) {
            FirstEventOwnerWait::Exited(status) => (Some(status), false),
            FirstEventOwnerWait::TimedOut(status) => (status, true),
            FirstEventOwnerWait::WaitError => (None, false),
        }
    };
    let owner_timeout_stderr = owner_timed_out.then(|| stderr_tail_last_lines(&stderr_live_tail));
    let first_event_timeout_stderr = first_event_watchdog
        .timeout_stderr()
        .or(owner_timeout_stderr);
    let exit_success = exit_status.as_ref().is_some_and(|s| s.success());
    let stderr_tail = stderr_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    LeadExit {
        exit_status,
        first_event_timeout_stderr,
        exit_success,
        stderr_tail,
    }
}

pub(crate) fn decide_lead_terminals(
    ctx: &LeadRunnerCtx,
    mut reducer: display_reduce::DisplayReducer,
    mut witness: LeadStreamWitness,
    exit: LeadExit,
    stop_requested: bool,
    first_event_binary: &str,
) -> LeadCloseout {
    let session_id_t = &ctx.session_id;
    let lead_run_id = &ctx.run_id;
    let LeadExit {
        exit_status,
        first_event_timeout_stderr,
        exit_success,
        stderr_tail,
    } = exit;
    let saw_completed = witness.saw_completed;
    let mut saw_error = witness.saw_error;
    let saw_blocked = witness.saw_blocked;
    let saw_needs_decision = witness.saw_needs_decision;
    let mut pending_terminals = std::mem::take(&mut witness.pending_terminals);
    // Check if user stopped during finalizing
    let stopped = {
        let stop_now = matches!(
            ctx.running
                .0
                .lock()
                .ok()
                .and_then(|m| m.get(session_id_t).cloned()),
            Some(RunSlot::Finalizing {
                stop_requested: true
            })
        );
        stop_requested || stop_now
    };

    if should_inject_first_event_watchdog_error(
        stopped,
        saw_completed,
        first_event_timeout_stderr.as_deref(),
    ) {
        let stderr_summary = first_event_timeout_stderr
            .expect("watchdog injection predicate requires timeout stderr");
        let message = first_event_watchdog_error_message(
            current_locale(&ctx.app),
            "run.spawnFailed",
            "claude",
            first_event_binary,
            &stderr_summary,
        );
        let event = record_synthetic_cli_error(&mut reducer, message);
        pending_terminals.push(event);
        saw_error = true;
    }

    // finish not called: internal signal only, not user-facing
    let finish_called = ctx.done.load(Ordering::SeqCst);
    if !finish_called {
        eprintln!("[lead] finish not called for session {session_id_t}");
    }

    // Guarantee a release terminal after persistence and after the slot is removed.
    let terminal_decision = lead_terminal_decision(
        saw_completed,
        saw_error,
        saw_blocked,
        saw_needs_decision,
        exit_success,
        stopped,
    );
    // Synthetic errors must also feed the reducer (reducer.feed); otherwise persisted messages cannot see
    // the actual error and can only produce a generic fallback card. The real cause would remain only in
    // memory (the live channel) and disappear on restart, matching the need for record_synthetic_cli_error
    // in solo's sidecar_exit_error branch.
    // Push the event returned by record_synthetic_cli_error directly instead of reconstructing an
    // Error{message} from Option<String>, preventing silent divergence between the barrier payload and
    // the reducer payload (their previous equality was coincidental; separate construction sites eventually drift).
    if terminal_decision == LeadTerminal::EmitError {
        let locale = current_locale(&ctx.app);
        let message = cli_exit_failure_message(
            locale,
            match locale {
                Locale::Zh => "队长",
                Locale::En => "lead",
            },
            exit_status.as_ref(),
            &stderr_tail,
        );
        let event = record_synthetic_cli_error(&mut reducer, message);
        pending_terminals.push(event);
        saw_error = true;
    }
    pending_terminals = lead_terminal_events_for_barrier(
        lead_run_id,
        &terminal_decision,
        stopped,
        pending_terminals,
    );

    witness.saw_error = saw_error;
    witness.pending_terminals = pending_terminals;
    LeadCloseout {
        reducer,
        witness,
        stopped,
        finish_called,
        exit_success,
    }
}

pub(crate) fn persist_lead_closeout(
    ctx: LeadRunnerCtx,
    closeout: LeadCloseout,
    delivery: LeadDelivery,
    mcp_srv: mcp_server::McpServer,
) {
    let lead_run_id = ctx.run_id;
    let session_id_t = ctx.session_id;
    let transport = ctx.transport;
    let LeadCloseout {
        reducer,
        witness,
        stopped,
        finish_called,
        exit_success,
    } = closeout;
    let LeadStreamWitness {
        saw_error,
        saw_blocked,
        saw_needs_decision,
        pending_terminals,
        lead_completed_usage,
        latest_context_compacted,
        ..
    } = witness;
    let LeadDelivery {
        stdin_ack,
        in_flight_report_ids_t,
        in_flight_answer_ids_t,
    } = delivery;
    let db = ctx.app.state::<crate::db::Db>();
    persist_context_compacted(
        &db,
        &session_id_t,
        &lead_run_id,
        latest_context_compacted.as_ref(),
    );

    // The reducer decides closeout persistence: write to the DB when there is output (display_reduce.rs is
    // the sole place for that decision; this code only assembles facts and invokes persistence, making no decision).
    // saw_blocked/saw_needs_decision now carry actual witnesses recorded by the event loop (lead also sees
    // normal Blocked/NeedsDecision completion with myagent exit codes 3/4, so these are no longer hardcoded false).
    // Use "agent-team" as the engine, matching the session's existing decision-card writes.
    let outcome = display_reduce::RunOutcome {
        run_id: lead_run_id.clone(),
        exit_success,
        interrupted: stopped,
        saw_error,
        saw_blocked,
        saw_needs_decision,
        finish_called: Some(finish_called),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text: None,
    };
    let locale = current_locale(&ctx.app);
    if let Some(mut msg) = reducer.finish_for_locale(&outcome, locale) {
        localize_reduced_message(locale, &mut msg);
        let db = ctx.app.state::<crate::db::Db>();
        if let Ok(conn) = db.0.lock() {
            reconcile_running_dispatch_cards(&conn, &session_id_t, &mut msg.blocks);
            // Attach the lead identity snapshot to final reduction messages so they remain associated with the correct agent.
            // It is still borrowed here, not moved, as confirmed where the identity was declared above.
            let _ = db::append_message_dedup_and_publish(
                &conn,
                &session_id_t,
                "assistant",
                &msg.blocks,
                Some("agent-team"),
                Some(ctx.lead_agent_id.as_str()),
                Some(ctx.profile.name.as_str()),
                &msg.dedup_key,
            );
        };
    }

    // Persist lead usage only here and call `add_session_usage` once to prevent duplicate accounting.
    // Idempotency is guaranteed by calling it only once, as with solo's persist_normal_finalizer pair.
    // To avoid double accounting: RunInfo.workingTokens is frontend in-memory state for real-time runtime
    // display and is never written to the DB by design. What is persisted here is lead_completed_usage,
    // from the usage fields of actual Completed events in stdout. This and workingTokens are completely
    // separate data paths; neither overwrites the other or double-counts usage.
    if let Some((input_tokens, output_tokens)) = lead_completed_usage {
        let db = ctx.app.state::<crate::db::Db>();
        let lock_result = db.0.lock();
        match lock_result {
            Ok(conn) => {
                if let Err(e) =
                    db::add_session_usage(&conn, &session_id_t, input_tokens, output_tokens)
                {
                    eprintln!("lead run usage persist failed (non-fatal): {e}");
                }
            }
            Err(_) => eprintln!("lead run usage persist skipped: db lock poisoned"),
        }
    }

    // Resolve delivery acknowledgment after stdout reaches EOF while retaining the slot and holding no database lock.
    // Receive `stdin_ack` without a DB guard (harness has no stdin; `None` means I/O succeeded during the
    // synchronous file write, as documented by `resolve_stdin_ack`). This must precede
    // finish_run_without_git_writes / emit_terminal_after_releasing_run_slot (slot release), preserving
    // the ordering invariant: ack commit < slot release < drain. Both `in_flight_report_ids_t` and
    // `in_flight_answer_ids_t` were captured locally in the same thread during assembly from
    // `assembly.included_report_ids` / `assembly.included_answer_ids`.
    // Use `assembly.included_answer_ids` as the acknowledgment source so only answers actually assembled are committed.
    // No cross-thread registration/retrieval through a side channel is needed. Runs that return early on
    // assembly failure never reach here; both collections remain empty, making acknowledgment a no-op.
    let writer_ack = resolve_stdin_ack(stdin_ack);
    commit_lead_run_delivery(
        &ctx.app,
        &session_id_t,
        writer_ack,
        &in_flight_report_ids_t,
        &in_flight_answer_ids_t,
    );

    // At lead closeout, clean up the legacy pending ledger written by this run itself (set git_state=clean
    // when there is no commit intent, with the same semantics as solo). interrupted uses `stopped`
    // (true when stopped by the user), matching solo.
    // This must run before emit_terminal_after_releasing_run_slot releases the Running slot: while this run
    // still owns the slot, no new run can start, so setting clean cannot overwrite a subsequent run's running
    // state. Every run that prepared a ledger above must pass through this cleanup, leaving no running row
    // after normal, error, or stopped exits. Database write failures are only logged (non-fatal).
    {
        let db = ctx.app.state::<crate::db::Db>();
        let _ = if let Ok(conn) = db.0.lock() {
            if let Err(e) =
                finish_run_without_git_writes(&conn, &session_id_t, &lead_run_id, stopped)
            {
                eprintln!("lead run ledger cleanup failed (non-fatal): {e}");
            }
        } else {
            eprintln!("lead run ledger cleanup skipped: db lock poisoned");
        };
    }

    // Do not pre-lock the database here because pending-resume handling acquires it again and would deadlock.
    // Specifically, try_resume_pending would deadlock when locking again.
    let runtime_db = ctx.app.state::<crate::db::Db>();
    let _ = emit_terminal_after_releasing_run_slot(
        &ctx.running,
        &ctx.team_running,
        &session_id_t,
        &lead_run_id,
        pending_terminals,
        &transport,
        Some(runtime_db.inner()),
    );
    // Continuation feeding must run after the run slot is released; otherwise it hits its own SESSION_BUSY and loses this continuation.
    drain_after_run_release(ctx.app.clone(), session_id_t.clone());

    // drop McpServer last — stops accept loop (Drop impl calls server.unblock())
    drop(mcp_srv);
}
