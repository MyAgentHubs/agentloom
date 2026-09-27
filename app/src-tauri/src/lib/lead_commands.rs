use super::*;

/// Testable core for the goal seed decision: seed only when the session has no existing goal and
/// this run actually carries a user message. The resume path (`try_resume_pending`,
/// message=None) must never seed, even when the session has no existing goal. Otherwise it would
/// permanently write the `RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT` placeholder into the goal
/// memory block and emit `session-goal-updated` to the top bar, contaminating sessions where the
/// first seed hit the lock's Err branch or that predate the goal feature; both cases are reachable.
pub(super) fn should_seed_goal(has_existing_goal: bool, has_message: bool) -> bool {
    !has_existing_goal && has_message
}

/// Testable core for step 4 of `start_lead_session`, which persists the user message.
/// message=None, used by the `try_resume_pending` resume path, must never write to the database:
/// `commit_late_answer` has already stored a formatted user-answer message, so another write
/// here would duplicate the answer in the transcript. Keeping this as a pure `&Connection`
/// function lets `test_db()` assert the `db::get_messages` row count before and after without
/// starting and stopping the full Tauri `State`/`AppHandle` command environment.
/// The caller computes and passes `dedup_key` from the `user_dedup_key` IPC argument or the
/// `user_send_key(&run_id)` fallback. The connection uses autocommit with no explicit transaction,
/// satisfying the `append_message_dedup_and_publish` contract, so a successful write publishes
/// the msg.completed milestone automatically. The message=None branch returns before using it.
pub(super) fn persist_lead_start_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    lead_agent_id: &str,
    lead_agent_name: &str,
    message: Option<&str>,
    dedup_key: &str,
) -> Result<(), String> {
    let Some(text) = message else {
        return Ok(());
    };
    db::append_message_dedup_and_publish(
        conn,
        session_id,
        "user",
        &[db::Block::Text {
            text: text.to_string(),
        }],
        None,
        Some(lead_agent_id),
        Some(lead_agent_name),
        dedup_key,
    )
    .map(|_inserted| ())
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(super) fn start_lead_session(
    app: AppHandle,
    db: State<Db>,
    running: State<Running>,
    team_running: State<member_runner::TeamRunning>,
    session_id: String,
    lead_agent_id: String,
    // `try_resume_pending` passes `None` to reuse persisted late answers without inserting a duplicate message.
    // `commit_late_answer` has already persisted them, so start the new run from existing history.
    message: Option<String>,
    member_ids: Vec<String>,
    reasoning_tier: Option<String>,
    // The frontend composer calls this Tauri command directly; the default means a user-message origin.
    start_origin: Option<StartOrigin>,
    // Deduplication key for persisting user messages. The frontend omits it because Tauri parses a
    // missing Option argument as None; None falls back to `display_reduce::user_send_key(&run_id)`.
    // The remote inbox delivery path (`deliver_remote_inbox_entry`) passes
    // `remote_input_key(command_id)` to deduplicate at-least-once redelivery. It is unused when
    // message=None because `persist_lead_start_message` returns early.
    user_dedup_key: Option<String>,
    // Carry the atomic answer snapshot from `try_resume_pending_with_gate` into assembly to preserve this run's delivery scope.
    // Feed the answer IDs snapshotted in the same critical section to assembly through
    // `forced_answer_ids` in `build_lead_context_prompt_for_session` so they are included in the
    // prompt. The source of truth for answer acknowledgement is now
    // `assembly.included_answer_ids`, captured directly by the runner thread with same-thread
    // happens-before ordering, so no cross-thread global side channel is needed. The frontend
    // invoke omits this field, which Tauri parses as None. Other internal callers,
    // `deliver_remote_inbox_entry` and `start_continuation_session`, also pass `None` because they
    // carry no pending-answer IDs to resume.
    resume_answer_ids: Option<Vec<i64>>,
) -> Result<(), String> {
    let start_origin = start_origin.unwrap_or(StartOrigin::UserMessage);

    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        ensure_session_not_continued(&conn, &session_id, current_locale(&app))?;
    }

    let creds = lead_session::resolve_lead_credentials(&app, db.inner(), &lead_agent_id)?;

    // 2. Prevent re-entry into the same session through the lead slot and prior member activity.
    let running_inner = running.inner().clone();
    let team_running_inner = team_running.inner().clone();
    // `reserve_lead_start_after_globalstop(...)` no longer attaches refresh itself. Let `conn`
    // release naturally when the block below ends, then attach refresh explicitly. The
    // globally-stopped and busy early-return branches refresh before returning Err so
    // session_runtime observes the brief reservation and release. The continuing branch attaches
    // the refresh handle to the guard only after the connection block ends, ensuring every later
    // early-return drop occurs after the database lock is released and avoiding same-thread
    // re-entrant deadlock.
    let Some(mut guard) = lead_session::reserve_lead_slot(
        &app,
        db.inner(),
        &running_inner,
        &team_running_inner,
        &session_id,
        message.is_some(),
    )?
    else {
        return Ok(());
    };

    // 3. Get the user's project working directory.
    let wt = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let (_workspace, wt) = ensure_session_workspace(&conn, &session_id)?;
        wt
    };

    // 4a. Generate the lead run_id here, before step 4 persists the user message, rather than at
    // the former step 6. `new_run_id()` is a pure in-memory operation using a timestamp, pid, and
    // process-local counter with no I/O or database side effects, so moving it has no observable
    // effect. The intervening member_pool construction neither reads nor writes run_id, and no
    // consumer depends on it remaining ungenerated. Step 4 needs a dedup_key, and local or resume
    // paths with `user_dedup_key` set to None require the `user_send_key(run_id)` fallback.
    let run_id = new_run_id();

    // Persist incoming messages only when present; resumed late answers are already stored and must not be duplicated.
    // Writing another one here would duplicate the answer in the transcript. The decision lives
    // in the testable `persist_lead_start_message` core. The remote inbox delivery path passes
    // `user_dedup_key`, equal to `remote_input_key(command_id)`; local and resume paths fall back to
    // `user_send_key(run_id)`.
    let dedup_key = user_dedup_key.unwrap_or_else(|| display_reduce::user_send_key(&run_id));
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        persist_lead_start_message(
            &conn,
            &session_id,
            &lead_agent_id,
            &creds.profile.name,
            message.as_deref(),
            &dedup_key,
        )?;
        // Reservation through `reserve_lead_start_after_globalstop` precedes run identifier creation, so the runtime row needs backfilling.
        // The earlier reservation wrote this session_runtime row as running(run_id=None) because
        // run_id did not exist when the slot was reserved. The solo path backfills the same case,
        // but the lead path previously omitted it. The UPSERT's `run_id = excluded.run_id` would
        // otherwise leave this NULL permanently, and the mobile runtime's `runId===null` guard
        // would discard every subsequent live delta for the session as liveDroppedNoRun. Backfill
        // as early as possible after generating run_id; this only completes a field on the same
        // running row, is not an independent choke point, and reports non-fatal failures.
        if let Err(e) = db::set_session_runtime(
            &conn,
            &session_id,
            db::SESSION_RUNTIME_RUNNING,
            Some(&run_id),
        ) {
            eprintln!("session_runtime run_id backfill (lead) failed (non-fatal): {e}");
        }
    }

    let member_pool = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        lead_session::load_member_pool(&conn, &member_ids)
    };

    let flags = lead_session::LeadRunFlags::new();

    let lead_ctx = lead_session::build_lead_ctx(
        &app,
        &running_inner,
        &team_running_inner,
        &session_id,
        &run_id,
        member_pool,
        &flags,
    );

    let tools_arc = lead_session::build_lead_tool_registry(
        &app,
        &session_id,
        &run_id,
        &wt,
        &lead_agent_id,
        &creds.profile.name,
        &lead_ctx,
    );

    // All three lead engines use Claude parsing for NativeClaude and BorrowClaude and myagent parsing for Harness.
    // Use Token granularity for sub-line fragments so incremental output is emitted without waiting for complete lines.
    // This matches the Token branch of TextGranularity::for_parse_fn(Claude|Harness|HarnessPlan).
    event_transport()
        .register_run(
            &run_id,
            &session_id,
            None,
            member_runner::TextGranularity::Token,
            crate::event_transport::RunIdentity {
                agent_id: Some(lead_agent_id.clone()),
                agent_name_snapshot: Some(creds.profile.name.clone()),
            },
        )
        .map_err(|e| format!("EventTransport register_run failed: {e:?}"))?;

    // Keep the guard armed until runner thread creation succeeds so a spawn failure still releases the reserved slot.
    // `std::thread::Builder::spawn` returns Err, unlike bare `std::thread::spawn`, whose failure is
    // a panic rather than a testable Result. Keep the guard armed temporarily: if Builder::spawn
    // fails, the Launching slot remains untouched and the guard's Drop can release it. That path
    // must first persist a visible error, install backoff, and drain, so use an explicit `match`
    // and disarm only after the thread has actually taken ownership in the Ok branch.

    // Retain pending answers across spawning and remove them only after actual I/O acknowledgement to prevent answer loss.
    // `pending_answer_ids` tracks unacknowledged answers precisely by message ID and removes them
    // through `ack_pending_answers` only after real I/O acknowledgement, so no best-effort early
    // cleanup is needed here. Any run, regardless of origin, precisely removes delivered IDs on
    // acknowledgement; undelivered IDs remain for the next `try_resume_pending` triggered by
    // `drain_after_run_release`.

    // Preserve failure-handler values before the runner closure takes ownership of `creds.profile` and `run_id`.
    // The runner thread creation failure branch, `handle_lead_runner_thread_spawn_failure`, runs
    // outside the closure and needs a clone of each value; after closure capture, these names
    // would otherwise no longer be available.
    let profile_name_for_thread_spawn_failure = creds.profile.name.clone();
    let run_id_for_thread_spawn_failure = run_id.clone();

    let ctx = lead_session::LeadRunnerCtx {
        app: app.clone(),
        session_id: session_id.clone(),
        run_id,
        lead_agent_id: lead_agent_id.clone(),
        profile: creds.profile,
        borrow_api_key: creds.borrow_api_key,
        harness_creds: creds.harness_creds,
        lead_engine: creds.lead_engine,
        running: running_inner.clone(),
        team_running: team_running.inner().clone(),
        terminated: lead_ctx.terminated.clone(),
        done: flags.done,
        transport: event_transport().clone(),
        member_pool: lead_ctx.member_pool.clone(),
        resume_answer_ids,
        start_origin,
        message,
        reasoning_tier,
        wt,
        tools: tools_arc,
    };

    let spawn_result = std::thread::Builder::new()
        .name(format!("lead-runner-{session_id}"))
        .spawn(move || lead_session::run_lead_runner(ctx));

    match spawn_result {
        Ok(_join_handle) => {
            // 9. disarm guard — thread owns the Running slot from here.
            guard.disarm();
            Ok(())
        }
        Err(e) => {
            // Thread creation failure leaves the runner closure unexecuted, so cleanup must handle the untouched launching slot.
            // The Launching slot remains exactly as it was before the guard could release it. Run
            // unified cleanup through `handle_lead_runner_thread_spawn_failure`: install backoff,
            // persist a visible error, release the slot and emit terminal state, then drain. Only
            // afterward disarm the guard so its Drop does not redundantly release or refresh a slot
            // that cleanup already handled manually.
            handle_lead_runner_thread_spawn_failure(
                &app,
                &running_inner,
                &team_running_inner,
                db.inner(),
                &flags.terminated,
                &session_id,
                &run_id_for_thread_spawn_failure,
                &lead_agent_id,
                &profile_name_for_thread_spawn_failure,
                &e.to_string(),
            );
            guard.disarm();
            // Return the failure even after cleanup so the caller cannot record a failed resume as successful.
            // The caller, `try_resume_pending_with_gate`, must still see the failure after cleanup
            // and drain so it does not mistake it for success and clear backoff. An Ok branch would
            // imply that the slot was acquired and ownership transferred to the runner thread.
            // Without a runner thread, assembly never executes, so no included-answer snapshot exists to acknowledge.
            // The answers remain in `pending_answer_ids`; there is no risk of registering them
            // incorrectly and no global side channel to clean up.
            Err(format!("lead runner thread spawn failed: {e}"))
        }
    }
}
