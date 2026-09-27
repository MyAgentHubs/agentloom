use super::*;

pub(crate) struct LeadStepArgs {
    pub(crate) session_id: String,
    pub(crate) lead_agent_id: String,
    pub(crate) last_event: String,
    pub(crate) event_cursor: String,
    pub(crate) user_msg: Option<String>,
    pub(crate) dispatchable_member_ids: Option<Vec<String>>,
    pub(crate) reasoning_tier: Option<String>,
}

pub(crate) fn lead_step_blocking(
    app: tauri::AppHandle,
    guard: ReservationGuard,
    args: LeadStepArgs,
    locale: Locale,
) -> Result<LeadStepOutcome, String> {
    let LeadStepArgs {
        session_id,
        lead_agent_id,
        last_event,
        event_cursor,
        user_msg,
        dispatchable_member_ids,
        reasoning_tier,
    } = args;
    let _guard = guard;
    let db = app.state::<db::Db>();

    let (driver, project) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let st = db::get_lead_loop_state(&conn, &session_id).map_err(|e| e.to_string())?;
        if st.last_event_cursor.as_deref() == Some(event_cursor.as_str()) {
            return Ok(LeadStepOutcome::Duplicate);
        }
        let lead_steps = db::list_decisions(&conn, &session_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|r| {
                matches!(
                    r.source_kind.as_deref(),
                    Some("reply" | "dispatch_worker" | "propose_verifier" | "ask_user" | "finish")
                )
            })
            .count();
        if lead_steps >= lead_step::MAX_LEAD_STEPS_PER_SESSION {
            let action = lead_step_budget_action(current_locale(&app));
            // Include a best-effort lead identity snapshot on legacy budget cards, using None when lookup cannot resolve it.
            // Do not block persistence of the budget card itself: it is a safety valve, and a failed name lookup must not prevent it.
            let lead_agent_name = db::get_agent(&conn, &lead_agent_id)
                .ok()
                .flatten()
                .map(|p| p.name);
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            db::insert_decision(
                &tx,
                &session_id,
                None,
                None,
                action.rationale(),
                "[]",
                "[]",
                "ask_user",
                None,
            )
            .map_err(|e| e.to_string())?;
            db::set_lead_event_cursor(&tx, &session_id, &event_cursor)
                .map_err(|e| e.to_string())?;
            let now = db::now_secs(); // Seconds, consistent with messages/DB created_at (Codex NIT).
            let decision_card =
                lead_step::build_decision_card_block(&new_run_id(), &new_run_id(), &action, now);
            let mut msg_completed_milestone = None;
            if let Some(b) = &decision_card {
                msg_completed_milestone = db::append_message_dedup(
                    &tx,
                    &session_id,
                    "assistant",
                    std::slice::from_ref(b),
                    Some("agent-team"),
                    Some(lead_agent_id.as_str()),
                    lead_agent_name.as_deref(),
                    &display_reduce::lead_decision_key(&event_cursor),
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            if let Some(milestone) = msg_completed_milestone {
                milestone.publish();
            }
            return Ok(LeadStepOutcome::Decided {
                action,
                decision_card,
            });
        }
        let driver = resolve_effective_team_config(&conn, &session_id, &lead_agent_id, None)?.lead;
        let _workspace = resolve_session_workspace(&conn, &session_id)?;
        let project = ensure_inplace_session_workdir(&conn, &session_id)?;
        (driver, project)
    };

    let wt = ensure_inplace_or_app_workspace(&session_id, project)?;

    let hook_run_id = new_run_id();
    let mut spawn = |prompt: &str, hint: Option<&str>| -> Result<String, String> {
        let prompt = match hint {
            Some(h) => format!("{prompt}\n\n【上次输出错误】{h}\n请修正后只输出一个 JSON。"),
            None => prompt.to_string(),
        };
        let search =
            resolve_harness_search_creds(db.inner(), &driver, &crate::keychain::KeyringStore)?;
        let key = resolve_member_key(&driver)?;
        let (mut cmd, parse_fn, stdin_prompt) = {
            // Narrow lock scope (H1/A1): build_lead_backend_command borrows conn only while
            // constructing the Command (reading profile/history and other DB data). The returned
            // Command does not retain that borrow; the guard is released as soon as this block
            // ends, so spawning the child and reading stdout to EOF (a full model round trip that
            // may take seconds or minutes) no longer holds the global DB lock.
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            build_lead_backend_command(
                &conn,
                &session_id,
                &hook_run_id,
                &driver,
                &prompt,
                &wt,
                agent::BuildMode::LeadAction,
                locale,
                reasoning_tier.as_deref(),
                key,
                search,
            )?
        };
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let spawn_err = |e: std::io::Error| {
            ui_msg::al_err("lead.spawnLeadFailed", &[("detail", e.to_string())])
        };
        let child = agent::spawn_with_stdin_prompt(&mut cmd, stdin_prompt.as_ref());
        let child = child.map_err(spawn_err)?;
        match lead_draft::read_draft_final_text(child, parser_for_parse_fn(parse_fn)) {
            (Some(text), _) => Ok(text),
            (None, stderr) if stderr.is_empty() => Err(ui_msg::al_err("lead.noFinalText", &[])),
            (None, stderr) => Err(ui_msg::al_err(
                "lead.noFinalTextStderr",
                &[("stderr", stderr)],
            )),
        }
    };

    let (action, decision_card) = lead_step::run_lead_step(
        db.inner(),
        &session_id,
        &last_event,
        &event_cursor,
        user_msg.as_deref(),
        dispatchable_member_ids.as_deref(),
        locale,
        &mut spawn,
    )?;
    Ok(LeadStepOutcome::Decided {
        action,
        decision_card,
    })
}
