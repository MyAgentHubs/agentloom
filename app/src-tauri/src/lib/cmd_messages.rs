// Message commands moved from lib.rs.

use super::*;

#[tauri::command]
pub(super) fn get_messages(db: State<Db>, session_id: String) -> Result<Vec<db::Message>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::get_messages(&conn, &session_id).map_err(|e| e.to_string())
}

/// The three non-reply frontend actions use this IPC for user-message
/// persistence paths that do not go through send_message/start_lead_session, such as manually
/// inserted bypass scenarios. They still call the non-deduplicating `db::append_message`.
/// This change leaves them untouched: user rows written through this bypass still have no
/// dedup_key and produce no msg.completed milestone, so remote replay batches cannot see them.
/// Leave this for a follow-up change. This scope covers only the four sources send_message,
/// start_lead_session, commit_late_answer, and continuation-session seeds, plus propagation of
/// the remote inbox command_id.
#[tauri::command]
pub(super) fn append_message(
    db: State<Db>,
    session_id: String,
    role: String,
    content: Vec<Block>,
    engine: Option<String>,
    agent_id: Option<String>,
    agent_name_snapshot: Option<String>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::append_message(
        &conn,
        &session_id,
        &role,
        &content,
        engine.as_deref(),
        agent_id.as_deref(),
        agent_name_snapshot.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn choose_decision_card(
    db: State<Db>,
    session_id: String,
    decision_id: String,
    expect_status: String,
    next_status: String,
    chosen_option: Option<String>,
) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    // Use `update_decision_card_status_message_id` and reread updated messages so status changes can be broadcast.
    // Resend this message through msg.completed with a new revision so remote clients know
    // the card changed status. The old API returns only bool and cannot provide message_id.
    // A resend failure does not roll back the CAS update committed above.
    let cas_message_id = db::update_decision_card_status_message_id(
        &conn,
        &session_id,
        &decision_id,
        &expect_status,
        &next_status,
        chosen_option.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    if let Some(message_id) = cas_message_id {
        if let Ok(Some(republish)) = db::get_message_for_republish(&conn, &session_id, message_id) {
            republish.publish();
        }
    }
    Ok(cas_message_id.is_some())
}

#[tauri::command]
pub(super) fn send_message(
    app: AppHandle,
    db: State<Db>,
    running: State<Running>,
    team_running: State<member_runner::TeamRunning>,
    session_id: String,
    agent_id: String,
    message: String,
    reasoning_tier: Option<String>,
    criteria: Option<Vec<String>>,
    // Deduplication key for persisted user messages. Manually entered frontend messages
    // omit it because Tauri parses a missing Option argument as None; for None, fall back to
    // `display_reduce::user_send_key(&run_id)`. The remote inbox delivery path
    // (`deliver_remote_inbox_entry`) passes `remote_input_key(command_id)` to deduplicate
    // at-least-once redelivery.
    user_dedup_key: Option<String>,
) -> Result<(), String> {
    let locale = current_locale(&app);
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        ensure_session_not_continued(&conn, &session_id, locale)?;
    }
    let id = require_agent_id(agent_id)?;
    let reasoning_tier = normalize_reasoning_tier(reasoning_tier)?;
    let criteria = validate_criteria(&criteria.unwrap_or_default())?;
    let running_inner = running.inner().clone();
    let team_running_inner = team_running.inner().clone();
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        reserve_new_session_run(
            &conn,
            &running_inner,
            &team_running_inner,
            &session_id,
            locale,
        )?;
    }
    let mut guard = ReservationGuard::new(running_inner.clone(), session_id.clone())
        .with_refresh(team_running_inner.clone(), app.clone());
    clear_session_stop_state(&team_running_inner, &session_id);

    // Resolve cwd first; in-place mode bypasses the gate/reconcile predicates to preserve existing working-tree state.
    // Otherwise, the user's existing staged / unstaged / untracked state would be
    // misclassified as diverged.
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let (workspace, wt) = ensure_session_workspace(&conn, &session_id)?;
        if workspace.requires_git_gate() {
            reconcile_session(&conn, &session_id, &wt)?;
            gate_git_state(&conn, &session_id)?;
        }
    }

    let key_store = KeyringStore;
    let run_id = new_run_id();
    let profile = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::get_agent(&conn, &id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| ui_msg::al_err("agent.notFound", &[]))?
    };
    let key = if profile.access == "borrow" || profile.access == "harness" {
        key_store.get(&profile.id)?
    } else {
        None
    };
    let search = resolve_harness_search_creds(&db, &profile, &key_store)?;
    let plan = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        build_send_plan_with(
            &conn,
            &session_id,
            &run_id,
            profile,
            key,
            search,
            &message,
            reasoning_tier.as_deref(),
            &criteria,
            locale,
        )?
    };
    let SendPlan {
        agent_id,
        name_snapshot,
        wt,
        command,
        parse_fn,
        stdin_prompt,
        profile: _profile,
        prompt: _prompt,
    } = plan;
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        // Persist with deduplication. When `user_dedup_key` is None for local manual
        // input, fall back to `user_send_key(&run_id)`. The connection remains in autocommit
        // throughout, with no explicit transaction, matching the call contract of
        // `append_message_dedup_and_publish`; successful persistence automatically publishes
        // the msg.completed milestone.
        let dedup_key = user_dedup_key.unwrap_or_else(|| display_reduce::user_send_key(&run_id));
        db::append_message_dedup_and_publish(
            &conn,
            &session_id,
            "user",
            &[Block::Text {
                text: message.clone(),
            }],
            None,
            Some(agent_id.as_str()),
            Some(name_snapshot.as_str()),
            &dedup_key,
        )
        .map_err(|e| e.to_string())?;
        // run_commits.engine is a legacy column name; this stores agent_id for compatibility
        // with the existing ledger schema.
        prepare_run_ledger(&conn, &session_id, &run_id, &agent_id, &wt)?;
        // Backfill run_id because `reserve_new_session_run` reserves the running slot before the run identifier exists.
        // The run_id does not exist yet when the slot is reserved. This upsert fills the actual
        // run_id for the same session_id; it is not an independent choke point, only a field
        // completion on the same running row. Failure is non-fatal but no longer swallowed
        // completely.
        if let Err(e) = db::set_session_runtime(
            &conn,
            &session_id,
            db::SESSION_RUNTIME_RUNNING,
            Some(&run_id),
        ) {
            eprintln!("session_runtime run_id backfill failed (non-fatal): {e}");
        }
    }
    let parser = parser_for_parse_fn(parse_fn);
    spawn_and_stream(
        app,
        running_inner.clone(),
        team_running_inner.clone(),
        session_id.clone(),
        run_id.clone(),
        wt,
        agent_id,
        Some(name_snapshot),
        command,
        stdin_prompt,
        parser,
        parse_fn,
        &mut guard,
    )
}
