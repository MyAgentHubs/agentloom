use super::super::*;
use super::{
    decide_lead_terminals, persist_lead_closeout, pump_lead_stdout, wait_lead_exit, LeadDelivery,
};

pub(crate) struct LeadRunnerCtx {
    pub(crate) app: AppHandle,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    /// creds.profile / creds.borrow_api_key are used only once after the gate check (a name
    /// snapshot for append_message), and nothing uses them afterward, so they are moved into
    /// the thread rather than cloned. The run_commits.engine column stores the agent_id (the
    /// old column name), so a clone of the lead's agent_id is carried into the thread to write
    /// the run ledger.
    pub(crate) lead_agent_id: String,
    pub(crate) profile: crate::db::AgentProfile,
    pub(crate) borrow_api_key: Option<String>,
    pub(crate) harness_creds: Option<(Option<String>, Option<String>, Option<String>)>,
    /// creds.lead_engine is Copy.
    pub(crate) lead_engine: LeadEngine,
    pub(crate) running: Running,
    /// session_runtime recomputation needs team_running (compute_session_runtime's "Running
    /// slot ∪ team active" predicate) — needed at all four release chokepoints (the three
    /// pre-spawn failure paths below plus the normal closeout).
    pub(crate) team_running: member_runner::TeamRunning,
    pub(crate) terminated: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) done: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) transport: event_transport::EventTransport,
    /// Pass the roster through the prompt builder's pool parameter to keep roster data inside
    /// its designated data section, the AGENTLOOM-DATA fence data section (not appended at the
    /// end of the prompt — preserving the leverage of the final position for the language
    /// reminder / upkeep nudge). A clone of member_pool is carried into the thread here.
    pub(crate) member_pool: Vec<lead_tools::PoolMember>,
    /// Carry late-answer identifiers into the runner so resumed answers can be included in its
    /// context; the identifiers are passed to `build_lead_context_prompt_for_session` so the
    /// prompt includes the resumed answers. The source of truth for answer acknowledgment has
    /// moved to `assembly.included_answer_ids`, captured directly in-thread during assembly, so
    /// this "requested for inclusion" snapshot no longer needs to be registered through a
    /// global side channel.
    pub(crate) resume_answer_ids: Option<Vec<i64>>,
    /// Preserve the start origin so assembly failures can either abort silently or use a logged
    /// fallback prompt.
    pub(crate) start_origin: StartOrigin,
    pub(crate) message: Option<String>,
    pub(crate) reasoning_tier: Option<String>,
    pub(crate) wt: std::path::PathBuf,
    pub(crate) tools: Arc<mcp_server::ToolRegistry>,
}

pub(crate) fn run_lead_runner(mut ctx: LeadRunnerCtx) {
    let Some(SpawnedLeadRun {
        mut child,
        mut reducer,
        mcp_srv,
        first_event_binary,
        first_event_deadline,
        delivery,
    }) = spawn_lead_runner(&mut ctx)
    else {
        return;
    };
    if !handoff_lead_child(&ctx, &mut child) {
        drop(mcp_srv);
        return;
    }
    prepare_lead_run_ledger(&ctx);
    let session_id_t = &ctx.session_id;
    let pid = child.id();
    // Spawn stderr tail (with log file for debugging)
    let (stderr_handle, stderr_live_tail) = match child.stderr.take() {
        Some(stderr) => {
            let (handle, tail) =
                spawn_stderr_tail_thread_shared(stderr, log_file_for(session_id_t));
            (Some(handle), tail)
        }
        None => (None, Arc::new(Mutex::new(Vec::new()))),
    };
    let (first_event_watchdog, first_event_watchdog_handle) = spawn_first_event_watchdog(
        ctx.running.clone(),
        session_id_t.clone(),
        pid,
        stderr_live_tail.clone(),
        first_event_deadline.saturating_duration_since(Instant::now()),
    );

    let witness = pump_lead_stdout(&mut child, &ctx, &mut reducer, &first_event_watchdog);
    let first_line_seen = first_event_watchdog.stdout_closed();
    let _ = first_event_watchdog_handle.join();

    // Transition to Finalizing: the shared closeout point first shuts down the old handler, then moves the lead slot toward release.
    let stop_requested = begin_lead_finalizing(&ctx.running, &ctx.terminated, session_id_t);

    let exit = wait_lead_exit(
        &mut child,
        pid,
        first_line_seen,
        first_event_deadline,
        stderr_live_tail,
        first_event_watchdog,
        stderr_handle,
    );
    let closeout = decide_lead_terminals(
        &ctx,
        reducer,
        witness,
        exit,
        stop_requested,
        &first_event_binary,
    );
    persist_lead_closeout(ctx, closeout, delivery, mcp_srv);
}

struct SpawnedLeadRun {
    child: std::process::Child,
    reducer: display_reduce::DisplayReducer,
    mcp_srv: mcp_server::McpServer,
    first_event_binary: String,
    first_event_deadline: Instant,
    delivery: LeadDelivery,
}

fn spawn_lead_runner(ctx: &mut LeadRunnerCtx) -> Option<SpawnedLeadRun> {
    let session_id_t = &ctx.session_id;
    let reducer = display_reduce::DisplayReducer::new(&ctx.run_id);
    // start MCP server — held on thread stack
    let mcp_srv = match mcp_server::start_mcp_server(std::mem::take(&mut ctx.tools)) {
        Ok(s) => s,
        Err(e) => {
            abort_lead_prespawn(
                ctx,
                reducer,
                LeadRuntimeFailure::McpStart(&e.to_string()),
                None,
            );
            return None;
        }
    };
    let mcp_cfg = mcp_server::mcp_config_json(mcp_srv.port);
    // myagent `--mcp-server <name>=<url>` takes a bare URL, not Claude's JSON config.
    let mcp_url = format!("http://127.0.0.1:{}/mcp", mcp_srv.port);

    // Fix forgotten context: seed the session-level goal and assemble the context prompt.
    // Use three bounded retries with staggered delays to tolerate transient lock contention without waiting indefinitely.
    // This handles transient contention from brief lock holders such as drain_remote_inbox. The runner thread
    // holds no other locks here, so a brief sleep is safe and cannot deadlock; only continued failure is treated
    // as an actual assembly failure (see the branching below).
    // Use RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT for both missing-message fallbacks so resumed runs never receive an empty prompt.
    // Do not feed an empty string to the engine; normal first-turn and continuation-with-message behavior is unchanged.
    let message_or_fallback: String = ctx
        .message
        .clone()
        .unwrap_or_else(|| RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT.to_string());
    let assembly = assemble_lead_prompt(ctx, &message_or_fallback);
    let session_has_goal = assembly.session_has_goal;
    let in_flight_report_ids_t = assembly.included_report_ids;
    let in_flight_answer_ids_t = assembly.included_answer_ids;
    let assembly_outcome = assembly.outcome;
    // Abort automatic starts on assembly failure so a fallback-only run cannot be mistaken for delivery of pending content.
    // That would present "a run happened" as "a run delivered content." Do not start this run: install backoff,
    // then preserve the ordering: emit_lead_error_and_release releases the slot/emits the terminal,
    // then drain_after_run_release, then release MCP.
    // UserMessage retains the old behavior: use a fallback prompt, log the failure, and start normally
    // (the user's actively submitted message must not be swallowed).
    let assembled_prompt: String = match assembly_outcome {
        Ok(prompt) => prompt,
        Err(detail) => match ctx.start_origin {
            StartOrigin::Autofeed | StartOrigin::LateAnswer => {
                abort_lead_prespawn(
                    ctx,
                    reducer,
                    LeadRuntimeFailure::ContextAssembly(&detail),
                    Some(mcp_srv),
                );
                return None;
            }
            StartOrigin::UserMessage => {
                eprintln!(
                    "lead context assembly failed for {session_id_t} (UserMessage origin): \
             {detail}; falling back to raw message"
                );
                message_or_fallback.clone()
            }
        },
    };
    // Emit only when a session goal actually exists, avoiding a spurious refresh once the frontend is connected.
    if session_has_goal {
        let _ = ctx.app.emit(
            "session-goal-updated",
            serde_json::json!({ "session_id": session_id_t, "title": serde_json::Value::Null }),
        );
    }
    // Build the command per lead engine: native layers the profile model / effort onto the shared lead argv.
    // Borrow reuses the same claude_sandboxed_cmd_in + lead_claude_argv_extra; only system_prompt
    // (the identity prompt combined with LEAD_SYS_V2) and environment assembly differ.
    // Harness (myagent) uses the separate harness_lead_cmd_in (bypassing the Claude sandbox base;
    // MCP uses a bare URL).
    let build_result = build_lead_command(ctx, &assembled_prompt, &mcp_cfg, &mcp_url);
    let (mut cmd, claude_bin) = match build_result {
        Ok(built) => built,
        Err(e) => {
            abort_lead_prespawn(
                ctx,
                reducer,
                LeadRuntimeFailure::CommandBuild(&e.to_string()),
                Some(mcp_srv),
            );
            return None;
        }
    };
    log_claude_bin(session_id_t, &claude_bin);
    let first_event_binary = claude_bin.clone();
    let stdin_prompt = finish_lead_command(&mut cmd, ctx.lead_engine, &assembled_prompt);
    let first_event_started_at = Instant::now();
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Use the acknowledged spawn path to verify stdin delivery after EOF while the slot is still held.
    // Check actual I/O success with `resolve_stdin_ack`, rather than immediately dropping the ack receiver
    // as the old `spawn_with_stdin_prompt` path did.
    let (child, stdin_ack) =
        match agent::spawn_with_stdin_prompt_ack(&mut cmd, stdin_prompt.as_ref()) {
            Ok(agent::SpawnedWithStdinPrompt { child, stdin_ack }) => (child, stdin_ack),
            Err(e) => {
                abort_lead_prespawn(
                    ctx,
                    reducer,
                    LeadRuntimeFailure::ProcessStart(&e.to_string()),
                    Some(mcp_srv),
                );
                return None;
            }
        };
    let first_event_deadline =
        first_event_started_at + std::time::Duration::from_secs(FIRST_EVENT_TIMEOUT_SECS);

    Some(SpawnedLeadRun {
        child,
        reducer,
        mcp_srv,
        first_event_binary,
        first_event_deadline,
        delivery: LeadDelivery {
            stdin_ack,
            in_flight_report_ids_t,
            in_flight_answer_ids_t,
        },
    })
}

fn handoff_lead_child(ctx: &LeadRunnerCtx, child: &mut std::process::Child) -> bool {
    let session_id_t = &ctx.session_id;
    let lead_run_id = &ctx.run_id;
    let transport = &ctx.transport;
    let pid = child.id();
    // Transition Launching -> Running (or abort if stop was requested)
    let handoff_runtime_db = ctx.app.state::<crate::db::Db>();
    let proceed = match transition_lead_spawn_handoff(
        &ctx.running,
        &ctx.team_running,
        Some(handoff_runtime_db.inner()),
        &ctx.terminated,
        session_id_t,
        pid,
        lead_run_id,
        kill_process_group,
        |event| {
            let _ = transport.flush_barrier(lead_run_id, vec![event.clone()]);
        },
    ) {
        Ok(proceed) => proceed,
        Err(_) => {
            let _ = child.wait();
            // A poisoned running-state lock leaves slot release uncertain, so defer draining until release can be established.
            // terminated.store(true); it is uncertain whether the slot was fully removed, but "drain only after
            // slot release" is a lower bound rather than an upper bound. Add a drain here so other drain sources
            // (remote inbox, etc.) do not wait forever for another drain opportunity because of this extreme early return.
            drain_after_run_release(ctx.app.clone(), session_id_t.clone());
            return false;
        }
    };
    if !proceed {
        let _ = child.wait();
        // Stopped and aborted handoffs have already released the slot, so draining pending work is now safe.
        // Slot release precedes this line; add a drain as explained above.
        drain_after_run_release(ctx.app.clone(), session_id_t.clone());
        return false;
    }

    true
}

fn prepare_lead_run_ledger(ctx: &LeadRunnerCtx) {
    let session_id_t = &ctx.session_id;
    let lead_run_id = &ctx.run_id;
    // Immediately after the lead run starts (the Running slot is established), write the legacy run ledger
    // (pending + running), so the commit tool's run_commit lookup in build_commit_tool can find the current run
    // instead of reporting "current run ledger is missing". This deliberately follows a successful transition:
    // MCP/build/spawn failures and stops during launching (the early returns above) have not written a ledger
    // and need no cleanup, eliminating the "written but not cleaned up" closeout race at its source.
    // From here, the only exit is the normal closeout below. Its paired cleanup runs before
    // emit_terminal_after_releasing_run_slot, while this run still owns the Running slot, to avoid interfering
    // with the git_state of a subsequent run. engine stores the lead's agent_id, as on the solo path.
    // Database write failures are only logged and do not kill the child that has already started.
    {
        let db = ctx.app.state::<crate::db::Db>();
        let _ = if let Ok(conn) = db.0.lock() {
            if let Err(e) = prepare_run_ledger(
                &conn,
                session_id_t,
                lead_run_id,
                &ctx.lead_agent_id,
                &ctx.wt,
            ) {
                eprintln!("lead run ledger prepare failed (non-fatal): {e}");
            }
        } else {
            eprintln!("lead run ledger prepare skipped: db lock poisoned");
        };
    }
}

fn abort_lead_prespawn(
    ctx: &LeadRunnerCtx,
    reducer: display_reduce::DisplayReducer,
    failure: LeadRuntimeFailure<'_>,
    mcp_srv: Option<mcp_server::McpServer>,
) {
    let message = lead_runtime_failure_message(current_locale(&ctx.app), failure);
    persist_lead_prespawn_failure(
        &ctx.app,
        reducer,
        &ctx.session_id,
        &ctx.run_id,
        &ctx.lead_agent_id,
        &ctx.profile.name,
        message.clone(),
    );
    // Set backoff before slot release so resumes cannot bypass the delay.
    // Do not trigger the repeated-notification rules here (those visibility notifications go through
    // record_resume_failure; persist_lead_prespawn_failure has already persisted this error message,
    // so no second message is needed). The immediately following drain_after_run_release will arm
    // its own timer when it encounters the not_before gate.
    note_resume_failure(&ctx.session_id);
    // Skip pre-locking: `emit_lead_error_and_release` takes its own short-lived locks.
    // Its signature takes `&crate::db::Db` (an unlocked handle) and acquires and releases short locks internally.
    // Previously, the guard from pre-locking here survived until `try_resume_pending` tried to lock again below;
    // a second lock on the same thread deadlocked immediately because TimedMutex/std::sync::Mutex is not reentrant.
    // That is the deadlock being fixed here.
    let runtime_db = ctx.app.state::<crate::db::Db>();
    emit_lead_error_and_release(
        &ctx.running,
        &ctx.team_running,
        &ctx.terminated,
        &ctx.session_id,
        &ctx.run_id,
        &ctx.transport,
        message,
        Some(runtime_db.inner()),
    );
    drain_after_run_release(ctx.app.clone(), ctx.session_id.clone());
    drop(mcp_srv);
}

struct LeadAssembly {
    outcome: Result<String, String>,
    session_has_goal: bool,
    included_report_ids: Vec<i64>,
    included_answer_ids: Vec<i64>,
}

fn assemble_lead_prompt(ctx: &LeadRunnerCtx, message_or_fallback: &str) -> LeadAssembly {
    let mut session_has_goal = false;
    // Return the pending report identifiers actually included in the prompt so delivery acknowledges only included reports.
    // Pass the selection result unchanged into the existing closeout acknowledgment pipeline
    // (`commit_lead_run_delivery`), preserving the closeout order.
    let mut in_flight_report_ids_t: Vec<i64> = Vec::new();
    // Acknowledge answers from the assembly result captured in this thread so acknowledgments reflect actual prompt inclusion.
    // Do not use the global `record_in_flight_answer_ids`/`take_in_flight_answer_ids` side channel.
    let mut in_flight_answer_ids_t: Vec<i64> = Vec::new();
    let db_state = ctx.app.state::<crate::db::Db>();
    let mut lock_attempt = db_state.0.try_lock();
    if lock_attempt.is_err() {
        for delay_ms in [50u64, 100, 200] {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            lock_attempt = db_state.0.try_lock();
            if lock_attempt.is_ok() {
                break;
            }
        }
    }
    let assembly_outcome: Result<String, String> = match lock_attempt {
        Ok(conn) => {
            // Branch explicitly: only Ok(None) seeds the first turn; Ok(Some) is not clobbered; Err does not seed
            // (do not treat a read failure as absence).
            // The seed decision lives in the testable pure function `should_seed_goal`.
            // A resumed run (message=None) must never seed the goal memory block with
            // RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT even when no goal exists. Doing so would permanently
            // write "Please continue the task based on the latest session records." into the goal and emit
            // session-goal-updated to the top bar, polluting sessions whose first seed hit lock contention
            // and took the Err branch, or old sessions created before the goal feature existed.
            // The actual first-message path (message=Some) is unchanged.
            match crate::db::get_memory_block(&conn, &ctx.session_id, "goal") {
                Ok(existing_goal) => {
                    let has_existing = existing_goal.is_some();
                    if has_existing {
                        session_has_goal = true;
                    } else if should_seed_goal(has_existing, ctx.message.is_some()) {
                        match crate::db::upsert_memory_block(
                            &conn,
                            &ctx.session_id,
                            "goal",
                            message_or_fallback,
                            None,
                            Some("app"),
                        ) {
                            Ok(()) => session_has_goal = true,
                            Err(e) => {
                                eprintln!("seed session goal failed (non-fatal): {e}")
                            }
                        }
                    }
                    // Otherwise: a resumed run (message=None) without an existing goal neither seeds nor emits.
                }
                Err(e) => eprintln!("read session goal failed (non-fatal): {e}"),
            }
            match build_lead_context_prompt_for_session(
                &conn,
                &ctx.session_id,
                &ctx.member_pool,
                current_locale(&ctx.app),
                ctx.lead_engine,
                ctx.resume_answer_ids.as_deref().unwrap_or(&[]),
            ) {
                Ok(assembly) => {
                    in_flight_report_ids_t = assembly.included_report_ids;
                    in_flight_answer_ids_t = assembly.included_answer_ids;
                    Ok(assembly.prompt)
                }
                Err(e) => Err(e),
            }
        }
        Err(_) => Err("db lock unavailable for lead context assembly after retries".to_string()),
    };
    LeadAssembly {
        outcome: assembly_outcome,
        session_has_goal,
        included_report_ids: in_flight_report_ids_t,
        included_answer_ids: in_flight_answer_ids_t,
    }
}

fn build_lead_command(
    ctx: &LeadRunnerCtx,
    assembled_prompt: &str,
    mcp_cfg: &str,
    mcp_url: &str,
) -> Result<(std::process::Command, String), String> {
    match ctx.lead_engine {
        LeadEngine::NativeClaude => {
            let extra_strings = native_lead_argv_extra_for_profile(
                &ctx.profile,
                ctx.reasoning_tier.as_deref(),
                mcp_cfg,
            );
            let extra_refs: Vec<&str> = extra_strings.iter().map(|s| s.as_str()).collect();
            claude_lead_cmd_in(&ctx.wt, assembled_prompt, &extra_refs)
        }
        LeadEngine::BorrowClaude => {
            let identity = agent::borrow_claude_identity_prompt(&ctx.profile);
            let system_prompt = format!("{identity}\n\n{LEAD_SYS_V2}");
            let api_key = ctx.borrow_api_key.as_deref().unwrap_or_default();
            borrow_lead_cmd_in(
                &ctx.profile,
                api_key,
                &ctx.wt,
                assembled_prompt,
                mcp_cfg,
                &system_prompt,
            )
        }
        LeadEngine::Harness => {
            let (api_key, search_api_key, search_backend) = ctx
                .harness_creds
                .as_ref()
                .map(|(k, sk, sb)| (k.as_deref(), sk.as_deref(), sb.as_deref()))
                .unwrap_or((None, None, None));
            harness_lead_cmd_in(
                &ctx.profile,
                api_key,
                search_api_key,
                search_backend,
                &ctx.wt,
                &ctx.session_id,
                assembled_prompt,
                mcp_url,
            )
        }
    }
}

fn finish_lead_command(
    cmd: &mut std::process::Command,
    engine: LeadEngine,
    prompt: &str,
) -> Option<agent::StdinPrompt> {
    // borrow_lead_cmd_in already calls apply_clean_env and layers on the borrow environment.
    // Calling apply_clean_env again here would wipe the newly set ANTHROPIC_* variables, so only
    // native calls it here (preserving the prior behavior: native already called apply_clean_env here).
    if matches!(engine, LeadEngine::NativeClaude) {
        apply_clean_env(cmd);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // Claude/borrow argv no longer contains the prompt body (claude_agent_argv in claude_sandboxed_cmd_in
    // no longer appends -p <prompt>); the body now goes through stdin. Harness still uses
    // write_harness_prompt_file to write an app-scoped temporary file and passes its path, so it needs no stdin.
    match engine {
        LeadEngine::NativeClaude | LeadEngine::BorrowClaude => {
            Some(agent::StdinPrompt::from(prompt))
        }
        LeadEngine::Harness => None,
    }
}
