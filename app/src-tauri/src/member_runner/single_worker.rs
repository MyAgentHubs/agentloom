use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_single_worker_run(
    app: &tauri::AppHandle,
    db: &crate::db::Db,
    team_running: &TeamRunning,
    session_id: &str,
    run_id: &str,
    spec: MemberSpec,
    command: std::process::Command,
    parser: MemberParser,
    parse_fn: crate::agent::ParseFn,
    granularity: TextGranularity,
    wt: std::path::PathBuf,
    stage1: Option<Stage1Ctx>,
    emit_events: bool,
    stdin_prompt: Option<crate::agent::StdinPrompt>,
) -> Result<crate::agent_event::MemberResult, String> {
    let transport = crate::event_transport().clone();
    // Member tokens belong in the session total: users care how much the session cost, and members
    // are dispatched by it, so count their consumption too, not just the lead's own share.
    // The sole data source is each event flowing through `emit_fn`: the terminal `Completed` event,
    // built by `member_terminal_event` (see its docs on forwarding buffered Completed's real tokens),
    // carries actual input_tokens/output_tokens; `MemberResult` itself has no usage field.
    // Use `Cell` instead of directly mutably borrowing closure captures: `emit_fn` needs `&mut` borrows
    // from both `run_single_worker_inner_for_locale` and `emit_terminal_failed_orchestrated`.
    // `Cell` avoids extra borrow-checker wrangling over two mutable borrows of outside variables in one
    // `emit_fn`; `Cell<Option<(Option<u64>, Option<u64>)>>` is all-Copy, with zero-cost `get`/`set`.
    let member_usage: std::cell::Cell<Option<(Option<u64>, Option<u64>)>> =
        std::cell::Cell::new(None);
    let result = run_single_worker_lifecycle(
        team_running,
        session_id,
        run_id,
        &spec,
        || {
            if !emit_events {
                return Ok(None);
            }
            register_member_transport(&transport, session_id, run_id, &spec, granularity, true)
                .map(Some)
        },
        |transport_lane_id| {
            let (open_meta, open_event) = member_open_event(run_id, &spec);
            if let Some(lane_id) = transport_lane_id.as_deref() {
                transport.push_with_dispatch(lane_id, stamp_orchestrated(open_meta), open_event);
            }
            let base_sha = crate::worktree::rev_parse_head(&wt).unwrap_or_default();
            let mut pending_terminals = Vec::new();
            let mut emit_fn = |d: DispatchMeta, e: AgentEvent| {
                if let AgentEvent::Completed {
                    input_tokens,
                    output_tokens,
                    ..
                } = &e
                {
                    member_usage.set(Some((*input_tokens, *output_tokens)));
                }
                if let Some(lane_id) = transport_lane_id.as_deref() {
                    emit_member_transport_event(
                        &transport,
                        lane_id,
                        &mut pending_terminals,
                        stamp_orchestrated(d),
                        e,
                    );
                }
            };
            let result = run_single_worker_inner_for_locale(
                team_running,
                session_id,
                run_id,
                spec.clone(),
                command,
                stdin_prompt,
                parser,
                Some(parse_fn),
                crate::current_locale(app),
                granularity,
                wt,
                base_sha,
                &mut emit_fn,
                stage1.as_ref(),
            );
            if let Err(reason) = &result {
                emit_terminal_failed_orchestrated(run_id, &spec, reason, &mut emit_fn);
            }
            result
        },
        |result| {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            persist_member_result_message(
                &conn,
                session_id,
                run_id,
                &spec.agent_id,
                &spec.agent_name,
                result,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |reason| {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            persist_member_setup_failure_message(&conn, session_id, run_id, &spec, reason)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
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
    );
    // Account exactly once after the lifecycle returns. A member ending in either Ok(Done) or
    // Ok(Failed) may have run a process and consumed tokens (failed worker calls are still billable),
    // so gate on whether real usage was captured, not on whether `result` is Ok. This prevents double
    // accounting: this is the only add_session_usage call in this function, with no second write path.
    if let Some((input_tokens, output_tokens)) = member_usage.get() {
        if input_tokens.is_some() || output_tokens.is_some() {
            let lock_result = db.0.lock();
            match lock_result {
                Ok(conn) => {
                    if let Err(e) =
                        crate::db::add_session_usage(&conn, session_id, input_tokens, output_tokens)
                    {
                        eprintln!("member usage persist failed (non-fatal): {e}");
                    }
                }
                Err(_) => eprintln!("member usage persist skipped: db lock poisoned"),
            }
        }
    }
    result
}
