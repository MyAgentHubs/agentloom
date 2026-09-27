use super::*;

/// `run_single_worker`, the hot path used by the lead MCP `dispatch_worker`, uses this independent
/// preparation function. Like `start_team_run`, it must not hold the same lock during keychain
/// IPC, `ensure_member_workspace` git worktree creation, or the stage-one snapshot's
/// `ensure_session_workspace` git operation. Matching `prepare_team_members` by avoiding
/// `tauri::AppHandle` lets it be unit-tested directly outside the Tauri runtime (see the
/// `prepare_single_worker_*` cases in `member_runner/tests/preparation.rs`). The three phases
/// narrow the lock scope:
/// 1. Inside the lock and fast: read the profile (preserving the original
///    `member.unavailableMissing` error envelope instead of using
///    `get_member_agent_profile`/`agent.notFound`, which belongs to another error family and would
///    change the user-visible error text), read the session-level in-place path, and determine the
///    stage-one action.
/// 2. Outside the lock and slow: perform keychain IPC, create the member git worktree when not
///    in-place, and create the session git worktree when the stage-one action is NeedsWorkspace.
/// 3. Inside the lock and fast: assemble the final Command.
///
/// **Execution order**: the original `stage1_snapshot_for_session` ran after
/// `build_member_command` (a build failure returned early via `?`, so stage one was never computed
/// and no session worktree was created). Here phase two (creating the session worktree) runs before
/// phase three (assembling the Command). The phases were separated to move all slow operations to
/// phase two; Command construction itself is fast, while creating the worktree after stage one
/// decides that a session worktree is needed is naturally a slow phase-two operation. As a side
/// effect, if Command construction in phase three fails, a session git worktree can now remain in
/// addition to the member git worktree. This only occurs when `stage1_phase1 == NeedsWorkspace` for
/// a non-in-place Repo session; in-place sessions always return Skip and are unaffected. This is
/// harmless: the next use of the same session reuses or idempotently rebuilds it, so no data is
/// corrupted, but the behavior is documented here explicitly.
pub(super) fn prepare_single_worker(
    db: &crate::db::Db,
    session_id: &str,
    run_id: &str,
    member: &MemberInput,
    fallback_spec: &MemberSpec,
    locale: crate::Locale,
) -> Result<PreparedSingleMember, String> {
    let (profile, inplace_wt, stage1_phase1) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let profile = crate::db::get_agent(&conn, &member.agent_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                crate::ui_msg::al_err(
                    "member.unavailableMissing",
                    &[("id", member.agent_id.clone())],
                )
            })?;
        let inplace_wt = crate::session_inplace_wt(&conn, session_id)?;
        let stage1_phase1 = stage1_snapshot_phase1(&conn, session_id);
        (profile, inplace_wt, stage1_phase1)
    };
    let spec = MemberSpec {
        provider: profile.provider.clone(),
        agent_name: profile.name.clone(),
        ..fallback_spec.clone()
    };

    let key = crate::resolve_member_key(&profile)?;
    let search = crate::resolve_harness_search_creds(db, &profile, &crate::keychain::KeyringStore)?;
    let wt = match &inplace_wt {
        Some(p) => p.clone(),
        None => {
            crate::worktree::ensure_member_workspace(session_id, &spec.assignment_id, None, true)?
        }
    };
    let stage1_snapshot =
        stage1_phase1.and_then(|phase1| stage1_snapshot_phase2(phase1, session_id));

    let (command, parser, parse_fn, granularity, stdin_prompt) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::build_member_command_with(
            &conn, session_id, run_id, &spec, &profile, key, search, &wt, locale,
        )?
    };
    Ok((
        spec,
        command,
        parser,
        parse_fn,
        wt,
        granularity,
        stage1_snapshot,
        stdin_prompt,
    ))
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // Public single-worker entry point for later wiring; current library tests only exercise inner functions.
pub fn run_single_worker(
    app: &tauri::AppHandle,
    db: &crate::db::Db,
    team_running: &TeamRunning,
    session_id: &str,
    run_id: &str,
    member: &MemberInput,
    emit_events: bool,
) -> Result<crate::agent_event::MemberResult, String> {
    let locale = crate::current_locale(app);
    let scope_files: Vec<String> = Vec::new();
    let acceptance: Vec<String> = Vec::new();
    let task_pack = build_task_pack(
        member.goal_title.as_deref().unwrap_or(""),
        &member.subtask,
        &scope_files,
        &acceptance,
        locale,
    );
    let fallback_spec = MemberSpec {
        participant_id: member.participant_id.clone(),
        assignment_id: member.assignment_id.clone(),
        task_id: member.task_id.clone(),
        agent_id: member.agent_id.clone(),
        provider: member.agent_id.clone(),
        agent_name: member.agent_id.clone(),
        subtask: member.subtask.clone(),
        prompt: task_pack,
    };
    let prepared = prepare_single_worker(db, session_id, run_id, member, &fallback_spec, locale);
    let (spec, command, parser, parse_fn, wt, granularity, stage1_snapshot, stdin_prompt) =
        match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                if emit_events {
                    emit_single_worker_setup_failure_best_effort(
                        session_id,
                        run_id,
                        &fallback_spec,
                        TextGranularity::Token,
                        &error,
                    );
                }
                return Err(finish_single_worker_setup_failure(
                    session_id,
                    run_id,
                    &fallback_spec,
                    error,
                    |reason| {
                        let conn = db.0.lock().map_err(|e| e.to_string())?;
                        persist_member_failure_message(
                            &conn,
                            session_id,
                            run_id,
                            &fallback_spec,
                            reason,
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    },
                    || {
                        let conn = db.0.lock().map_err(|e| e.to_string())?;
                        finalize_team_run(&conn, session_id, run_id).map_err(|e| e.to_string())
                    },
                ));
            }
        };

    let stage1_snapshot = match stage1_snapshot {
        Ok(stage1_snapshot) => stage1_snapshot,
        Err(error) => {
            if emit_events {
                emit_single_worker_setup_failure_best_effort(
                    session_id,
                    run_id,
                    &spec,
                    granularity,
                    &error,
                );
            }
            return Err(finish_single_worker_setup_failure(
                session_id,
                run_id,
                &spec,
                error,
                |reason| {
                    let conn = db.0.lock().map_err(|e| e.to_string())?;
                    persist_member_failure_message(&conn, session_id, run_id, &spec, reason)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                },
                || {
                    let conn = db.0.lock().map_err(|e| e.to_string())?;
                    finalize_team_run(&conn, session_id, run_id).map_err(|e| e.to_string())
                },
            ));
        }
    };
    let stage1 = stage1_ctx_from_snapshot(stage1_snapshot, session_id, &member.assignment_id, &wt);

    dispatch_single_worker_run(
        app,
        db,
        team_running,
        session_id,
        run_id,
        spec,
        command,
        parser,
        parse_fn,
        granularity,
        wt,
        stage1,
        emit_events,
        stdin_prompt,
    )
}

/// Thin wrapper around the actual spawn: emit the opening event, spawn the process group with
/// stderr written to the member log, register it, then run `run_member_reader` in a thread with an
/// emit closure backed by EventTransport push/barrier. Returns the pid.
/// There is no auto-commit or ledger.
#[allow(clippy::too_many_arguments)]
pub fn spawn_member(
    app: tauri::AppHandle,
    tr: TeamRunning,
    running: crate::Running,
    session_id: String,
    run_id: String,
    spec: MemberSpec,
    wt: std::path::PathBuf,
    mut command: Command,
    stdin_prompt: Option<crate::agent::StdinPrompt>,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: crate::agent::ParseFn,
    granularity: TextGranularity,
) -> Result<u32, String> {
    let hook_guard = crate::checkpoint_hook::guard_for_command(&command);
    command.stderr(Stdio::piped());
    let base_sha = crate::worktree::rev_parse_head(&wt).unwrap_or_default();
    let transport = crate::event_transport().clone();
    let transport_lane_id =
        register_member_transport(&transport, &session_id, &run_id, &spec, granularity, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let first_event_watchdog =
        MemberFirstEventWatchdog::for_command(Some(parse_fn), &command, &spec);
    command.stdout(Stdio::piped());
    let child = match crate::agent::spawn_with_stdin_prompt(&mut command, stdin_prompt.as_ref()) {
        Ok(child) => child,
        Err(error) => {
            let (open_meta, open_event) = member_open_event(&run_id, &spec);
            transport.push_with_dispatch(&transport_lane_id, open_meta, open_event);
            // The old Team path (`start_team_run` to `spawn_member`) once persisted `result=None`
            // on spawn failure, just like the three `run_single_worker` paths. Apply the same fix
            // so this path cannot regress to the misleading "worker did not return a result" text.
            let reason =
                crate::ui_msg::al_err("member.spawnFailed", &[("detail", error.to_string())]);
            let result = build_failure_only_member_result(&spec, &reason);
            let (terminal_meta, terminal_event) = member_terminal_event(
                &run_id,
                &spec,
                None,
                StatusTransition::Failed,
                Some(result),
                None,
            );
            let _ = transport.flush_barrier_with_dispatch(
                &transport_lane_id,
                vec![(terminal_meta, terminal_event)],
            );
            return Err(reason);
        }
    };
    let pid = child.id();
    let key = MemberKey::new(&session_id, &run_id, &spec.assignment_id);
    tr.register(&key, pid);
    request_stop_new_member_if_session_stopped(&tr, &key, crate::kill_process_group);
    // Emit the opening event with Dispatched and subtask so the card appears immediately.
    let (ometa, oev) = member_open_event(&run_id, &spec);
    transport.push_with_dispatch(&transport_lane_id, ometa, oev);

    std::thread::spawn(move || {
        let locale = crate::current_locale(&app);
        let mut pending_terminals = Vec::new();
        let run_done = run_member_reader_for_locale_with_watchdog(
            child,
            Some(&mut command),
            stdin_prompt.as_ref(),
            hook_guard,
            &tr,
            &key,
            &run_id,
            &spec,
            &wt,
            &base_sha,
            parser,
            Some(parse_fn),
            locale,
            granularity,
            first_event_watchdog,
            &mut |d, e| {
                emit_member_transport_event(
                    &transport,
                    &transport_lane_id,
                    &mut pending_terminals,
                    d,
                    e,
                )
            },
            None,
        );
        if run_done {
            // `run_done=true` signals that `run_member_finished` found the final member to reach a
            // terminal state. Whether that member completed, failed, or was stopped, all three
            // states are folded into the normal return path by `terminal_status` and reach this
            // branch. Release the slot here without depending on DB state; release first, then
            // persist to the DB on a best-effort basis, with neither operation blocking the other.
            crate::release_team_run_slot(&running, &session_id);
            if let Some(db) = app.try_state::<crate::db::Db>() {
                if let Ok(conn) = db.0.lock() {
                    if let Err(e) = finalize_team_run(&conn, &session_id, &run_id) {
                        eprintln!("finalize_team_run failed (non-fatal): {e}");
                    }
                }
                // Normal completion, failure, and stop all converge on this `run_done` branch when
                // the team becomes empty. Recompute runtime state instead of hard-coding the idle
                // literal. The `db.0.lock()` above is released when its `if let` block ends, so use
                // a separate short lock to avoid recursively locking while already holding it.
                crate::refresh_session_runtime(db.inner(), &running, &tr, &session_id);
            }
        }
    });
    Ok(pid)
}
