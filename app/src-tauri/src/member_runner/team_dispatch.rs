use super::*;

/// Member specification supplied by the frontend: it provides the assignment, participant, and task,
/// while `agent_id` selects a configured agent.
/// Tauri only converts top-level command argument names; nested structs must explicitly use camelCase.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInput {
    pub participant_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub subtask: String,
    #[serde(default)]
    pub goal_title: Option<String>,
}

pub(super) fn validate_members_against_saved_session_config(
    conn: &rusqlite::Connection,
    session_id: &str,
    members: &[MemberInput],
) -> Result<(), String> {
    let config =
        crate::db::get_session_agent_config(conn, session_id).map_err(|e| e.to_string())?;
    if config.lead_agent_id.is_none() {
        return Ok(());
    }

    let allowed: HashSet<&str> = config.member_agent_ids.iter().map(String::as_str).collect();
    for member in members {
        if !allowed.contains(member.agent_id.as_str()) {
            return Err(crate::ui_msg::al_err(
                "member.notInSessionPool",
                &[("id", member.agent_id.clone())],
            ));
        }
        let agent = crate::db::get_agent(conn, &member.agent_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                crate::ui_msg::al_err(
                    "member.unavailableMissing",
                    &[("id", member.agent_id.clone())],
                )
            })?;
        if !agent.enabled {
            return Err(crate::ui_msg::al_err(
                "member.unavailableDisabled",
                &[("id", member.agent_id.clone())],
            ));
        }
    }
    Ok(())
}

/// Narrows the lock scope by extracting the part of `start_team_run` that prepares commands for N members
/// into a standalone function without `tauri::AppHandle`/`State` dependencies or spawn/emit side effects.
/// This keeps `start_team_run` smaller and lets the error-prone multi-phase locking logic be unit-tested without the Tauri runtime; see the `prepare_team_members_*` tests in this file.
///
/// Previously, validation, N profile reads, Keychain IPC, Git worktree creation, and command assembly
/// ran serially per member under one global DB lock. Keychain IPC and spawning Git can each take seconds,
/// so N sequential members could block every other app session's DB work for seconds or tens of seconds. It is now split into three phases:
/// 1. Under the lock, quickly read all DB data: validation, acceptance criteria, and every agent profile.
/// 2. Outside the outer lock, perform slow Keychain IPC for agent and harness search keys, and create a
///    Git worktree per non-in-place member. Search credential resolution briefly takes the `db.0` lock
///    itself to read the backend name, once per member; sharing one resolution per team run remains an
///    optimization opportunity.
/// 3. Under the lock, assemble final commands from resolved profile/key/search/wt data. `make_backend`
///    no longer accepts `conn`, so this phase performs no Keychain IPC; `backend.build_command` retains
///    the necessary fast DB reads and writes.
///
/// **Behavioral qualification:** do not call the implementations identical in every respect. On the
/// normal path, where all members prepare successfully, every step has the same inputs and outputs.
/// Failure-path error precedence and side-effect order did change, but harmlessly. First, the old code
/// ran each member through profile -> key -> make_backend -> wt -> command, reporting the first member
/// to fail. Phase 1 now strictly validates profiles in a batch, so a later member's `agent.notFound`
/// can precede an earlier member's Keychain or workspace error. Second, `session_inplace_wt` is now
/// computed once outside the loop, so an unavailable session project path yields
/// `run.projectPathUnavailable` before any member's `agent.notFound`. In both cases the whole batch
/// still fails with no member dispatched. Only the first displayed error may change; this is a
/// different reporting order among the same errors, not a new failure mode.
pub(super) fn prepare_team_members(
    db: &crate::db::Db,
    session_id: &str,
    run_id: &str,
    goal: &str,
    members: Vec<MemberInput>,
    criteria: &[GoalCriterion],
    locale: crate::Locale,
) -> Result<Vec<PreparedMember>, String> {
    let mut member_preps: Vec<(MemberSpec, crate::db::AgentProfile)> =
        Vec::with_capacity(members.len());
    let inplace_wt: Option<std::path::PathBuf> = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        validate_members_against_saved_session_config(&conn, session_id, &members)?;
        // When criteria are supplied as arguments, use their claims directly because this tier does
        // not persist them to the DB. Otherwise, retain the DB read as a forward-compatible path.
        let acceptance: Vec<String> = if criteria.is_empty() {
            crate::db::list_acceptance_by_run(&conn, session_id, run_id)
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.claim)
                .collect()
        } else {
            criteria.iter().map(|c| c.claim.clone()).collect()
        };
        // The session-level in-place project path depends only on `session_id`, not `assignment_id`, so
        // compute it once under the lock for all members, reducing N+1 lookups to one; see `crate::session_inplace_wt`.
        let inplace_wt = crate::session_inplace_wt(&conn, session_id)?;
        for mi in members {
            // The old code queried the profile here and inside `build_member_command`: twice per member.
            // The tolerant `.ok().flatten()` result was used only on success; if the agent was absent,
            // the strict inner query immediately returned `agent.notFound` and its fallback went unused.
            // One strict query preserves both outcomes and observable behavior while reducing 2N queries to N.
            let profile = crate::get_member_agent_profile(&conn, &mi.agent_id)?;
            // Stub assignments currently provide only `assignment_id`, so `scope_files` stays empty until the assignment path produces scope.
            let scope_files: Vec<String> = Vec::new();
            let task_pack = build_task_pack(goal, &mi.subtask, &scope_files, &acceptance, locale);
            let spec = MemberSpec {
                participant_id: mi.participant_id,
                assignment_id: mi.assignment_id,
                task_id: mi.task_id,
                agent_name: profile.name.clone(),
                agent_id: mi.agent_id,
                provider: profile.provider.clone(),
                subtask: mi.subtask,
                prompt: task_pack,
            };
            member_preps.push((spec, profile));
        }
        inplace_wt
    };

    // Outside the lock, perform slow Keychain IPC and create a Git worktree per non-in-place member; neither needs `conn`.
    let mut member_ready: Vec<(
        MemberSpec,
        crate::db::AgentProfile,
        Option<String>,
        crate::HarnessSearchCreds,
        std::path::PathBuf,
    )> = Vec::with_capacity(member_preps.len());
    for (spec, profile) in member_preps {
        let key = crate::resolve_member_key(&profile)?;
        let search =
            crate::resolve_harness_search_creds(db, &profile, &crate::keychain::KeyringStore)?;
        let wt = match &inplace_wt {
            Some(p) => p.clone(),
            None => crate::worktree::ensure_member_workspace(
                session_id,
                &spec.assignment_id,
                None,
                true,
            )?,
        };
        member_ready.push((spec, profile, key, search, wt));
    }

    // Under the lock, quickly assemble final commands, taking one batch lock rather than one per member.
    // This reduces contention and moves the old lock span over slow work until after all workspaces are ready.
    //
    // TOCTOU safety check: deliberately do not query the profile again in Phase 3. A prior version
    // re-queried it to close the deletion window, but that was untested dead code: removing it left all
    // tests green, so its protection was never verified. Worse, it introduced an inconsistency absent
    // before: `MemberSpec.agent_name`/`.provider` came from Phase 1, while only the fresh profile reached
    // `build_member_command_with`; since `derive_command_evidence` parses with `spec.provider`, this could record usage against engine A while executing with engine B.
    // The current design queries once across Phases 1 and 3. `member_preps`/`member_ready` carry the same
    // `AgentProfile` throughout, ensuring the spec and final command share one snapshot instead of mixing
    // old spec data with new command data. This better matches the original semantics: although the old
    // code queried twice--tolerantly for the spec and strictly inside `build_member_command`--both queries
    // held the same lock, preventing intervening DB changes and making them effectively one query. Making
    // it literally one query is simpler and safer. The tradeoff is that deletion or modification between
    // Phases 1 and 3 is not detected, so Phase 1 data still builds the command. This extremely narrow single-machine window requires another thread to change the agent within milliseconds and matches
    // the old risk exposure. Its impact is bounded: stale provider/access data makes authentication or
    // execution fail explicitly rather than causing a silent mismatch.
    let mut prepared: Vec<PreparedMember> = Vec::with_capacity(member_ready.len());
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        for (spec, profile, key, search, wt) in member_ready {
            let (command, parser, parse_fn, granularity, stdin_prompt) =
                crate::build_member_command_with(
                    &conn, session_id, run_id, &spec, &profile, key, search, &wt, locale,
                )?;
            prepared.push((
                spec,
                command,
                parser,
                parse_fn,
                wt,
                granularity,
                stdin_prompt,
            ));
        }
    }
    Ok(prepared)
}

/// Runs an actual team: writes the goal, emits `GoalDeclared`, then resolves the backend, prepares a worktree, and spawns each member.
/// All members must pass preflight before spawning begins; a per-member spawn failure emits terminal `Failed`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_team_run(
    app: tauri::AppHandle,
    db: tauri::State<'_, crate::db::Db>,
    team_running: tauri::State<'_, TeamRunning>,
    running: tauri::State<'_, crate::Running>,
    session_id: String,
    goal: String,
    lead: String,
    members: Vec<MemberInput>,
    run_id: Option<String>,
    criteria: Option<Vec<GoalCriterion>>,
    goal_title: Option<String>,
) -> Result<String, String> {
    let locale = crate::current_locale(&app);
    if members.is_empty() {
        return Err(crate::ui_msg::al_err("member.emptyTeam", &[]));
    }
    // Reserve the `Running` slot first, matching solo `reserve_new_session_run`/`try_reserve`, so the
    // delete/archive/purge/restore `reserve_mutation` gate applies to team runs. Failure means another
    // run with this `session_id` is active, solo or team, so reject it with the existing
    // `SESSION_ALREADY_RUNNING` semantics used for solo collisions. `slot_guard` covers early failures
    // between here and actual spawning. On entering the spawn loop, disarm it and transfer release to
    // `release_team_run_slot`, called when `run_member_finished` identifies the last member, either in
    // the synchronous all-failed branch below or a `spawn_member` background reader thread.
    crate::reserve_team_run_slot(running.inner(), &session_id)?;
    // Attach refresh handles to the guard. Any early `?` while writing `team_run_pending`, emitting the
    // goal, registering `EventTransport`, etc. makes `Drop` release the slot and must synchronously
    // recompute `session_runtime`; previously this entire window never updated that table.
    let mut slot_guard = crate::TeamRunSlotGuard::new(running.inner().clone(), session_id.clone())
        .with_refresh(team_running.inner().clone(), app.clone());
    // Reuse a frontend-proposed `run_id`; otherwise generate one locally for backward compatibility.
    let run_id = run_id.unwrap_or_else(crate::new_run_id);
    let criteria = criteria.unwrap_or_default();
    // Team run slot reservation is the team registration choke point: `reserve_team_run_slot` above
    // acquired the slot (otherwise `?` already returned); run_id is now determined and written alongside.
    // This is a "reserve" write: run_id is known here, so call set_session_runtime directly, not
    // refresh_session_runtime for "release/free slot" writes; see both functions' docs for responsibilities.
    // Failure here is non-fatal but no longer silently swallowed, following set_goal_title's pattern.
    if let Ok(conn) = db.0.lock() {
        if let Err(e) = crate::db::set_session_runtime(
            &conn,
            &session_id,
            crate::db::SESSION_RUNTIME_RUNNING,
            Some(&run_id),
        ) {
            eprintln!("session_runtime running write failed (non-fatal): {e}");
        }
    }

    let assignments_json = serde_json::to_string(
        &members
            .iter()
            .map(|m| serde_json::json!({ "assignment_id": &m.assignment_id }))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".into());
    let prepared: Vec<PreparedMember> = prepare_team_members(
        db.inner(),
        &session_id,
        &run_id,
        &goal,
        members,
        &criteria,
        locale,
    )?;

    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::db::insert_team_run_pending(
            &conn,
            &session_id,
            &run_id,
            &goal,
            &lead,
            &assignments_json,
        )
        .map_err(|e| e.to_string())?;
    }
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        write_team_goal(&conn, &session_id, &run_id, &goal, &criteria)?;
        // The top-bar short title is a display nicety: write only a lead-produced `goal_title`, and let
        // `None` preserve the old title. Setting failure does not fail the run, but is logged to expose real DB/schema bugs.
        if let Some(title) = goal_title.as_deref() {
            if let Err(e) = set_goal_title_after_contract(&conn, &session_id, &run_id, Some(title))
            {
                eprintln!("set goal_title failed (non-fatal): {e}");
            }
        }
    }
    let (gmeta, gev) = team_goal_event(&run_id, &goal, &lead, &criteria);
    let goal_lane_id = format!("team-goal:{run_id}");
    crate::event_transport()
        .register_run(
            &goal_lane_id,
            &session_id,
            Some(gmeta.clone()),
            TextGranularity::Token,
            crate::event_transport::RunIdentity {
                agent_id: Some(lead.clone()),
                agent_name_snapshot: None,
            },
        )
        .map_err(|e| format!("EventTransport register_run failed: {e:?}"))?;
    crate::event_transport().push_with_dispatch(&goal_lane_id, gmeta, gev);
    crate::event_transport()
        .flush_barrier(&goal_lane_id, Vec::new())
        .map_err(|e| format!("EventTransport goal flush failed: {e:?}"))?;

    // The confirmed dispatch finished synchronous preparation and recorded `GoalDeclared`; before any
    // member registration/startup check, clear the prior global stop state or `spawn_member` stops new workers immediately.
    crate::clear_session_stop_state(team_running.inner(), &session_id);
    team_running.init_run(&run_id, prepared.len());
    // Actual spawning starts here. Transfer release to the terminal point found by `run_member_finished`,
    // in the synchronous all-failed branch or `spawn_member` reader thread. Disarm the now-unneeded guard
    // so its `Drop` does not prematurely release an active slot when this function returns.
    slot_guard.disarm();
    for (spec, command, parser, parse_fn, wt, granularity, stdin_prompt) in prepared {
        if let Err(e) = spawn_member(
            app.clone(),
            team_running.inner().clone(),
            running.inner().clone(),
            session_id.clone(),
            run_id.clone(),
            spec.clone(),
            wt,
            command,
            stdin_prompt,
            parser,
            parse_fn,
            granularity,
        ) {
            eprintln!("spawn_member 失败 {}: {e}", spec.assignment_id);
            if team_running.run_member_finished(&run_id) {
                crate::release_team_run_slot(running.inner(), &session_id);
                if let Ok(conn) = db.0.lock() {
                    if let Err(e) = finalize_team_run(&conn, &session_id, &run_id) {
                        eprintln!("finalize_team_run failed (non-fatal): {e}");
                    }
                }
                // Team-drain choke point for synchronous failure to spawn every member, matching the
                // asynchronous `spawn_member` reader path. Recompute instead of hard-coding `idle`.
                // The prior `db.0.lock()` ended with its `if let`; use a new short lock here to avoid
                // carrying the DB lock into `refresh_session_runtime` and its nested lock acquisition.
                crate::refresh_session_runtime(
                    db.inner(),
                    running.inner(),
                    team_running.inner(),
                    &session_id,
                );
            }
        }
    }
    Ok(run_id)
}

/// Stops one member with `killpg` on its process group; the reader derives terminal `Stopped` from the stop flag.
#[tauri::command]
pub fn stop_team_member(
    team_running: tauri::State<'_, TeamRunning>,
    session_id: String,
    run_id: String,
    assignment_id: String,
) -> Result<(), String> {
    let key = MemberKey::new(&session_id, &run_id, &assignment_id);
    team_running.request_stop_member(&key, crate::kill_process_group);
    Ok(())
}
