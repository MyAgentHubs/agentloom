use super::super::*;

pub(crate) struct LeadCredentials {
    pub(crate) profile: crate::db::AgentProfile,
    pub(crate) lead_engine: LeadEngine,
    pub(crate) borrow_api_key: Option<String>,
    pub(crate) harness_creds: Option<(Option<String>, Option<String>, Option<String>)>,
}

pub(crate) fn resolve_lead_credentials(
    app: &AppHandle,
    db: &crate::db::Db,
    lead_agent_id: &str,
) -> Result<LeadCredentials, String> {
    // Gate: determine whether the agent can act as lead and which spawn branch to use based on provider and access.
    let profile = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::db::get_agent(&conn, lead_agent_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("lead agent {lead_agent_id} \u{4e0d}\u{5b58}\u{5728}"))?
    };
    let lead_engine = lead_engine_for_profile(&profile)?;
    // A borrow lead must retrieve its API key from the keychain in advance, from the same source as the borrow branch of make_backend;
    // native Claude and harness do not need it and use None (the harness key uses the separate harness_creds below).
    let borrow_api_key: Option<String> = match lead_engine {
        LeadEngine::BorrowClaude => {
            let key = KeyringStore.get(&profile.id)?;
            Some(key.ok_or_else(|| ui_msg::al_err("agent.missingApiKey", &[]))?)
        }
        LeadEngine::NativeClaude | LeadEngine::Harness => None,
    };
    // A harness lead must retrieve the provider key plus the optional search key and backend from the keychain in advance
    // (from the same source as the "harness" branch of make_backend: validate_harness_agent_key + resolve_harness_search).
    let harness_creds: Option<(Option<String>, Option<String>, Option<String>)> = match lead_engine
    {
        LeadEngine::Harness => {
            let key = KeyringStore.get(&profile.id)?;
            validate_harness_agent_key(&profile, key.as_deref(), current_locale(app))?;
            let (search_api_key, search_backend) = {
                let conn = db.0.lock().map_err(|e| e.to_string())?;
                resolve_harness_search(&conn, &KeyringStore)
            };
            Some((key, search_api_key, search_backend))
        }
        LeadEngine::NativeClaude | LeadEngine::BorrowClaude => None,
    };

    Ok(LeadCredentials {
        profile,
        lead_engine,
        borrow_api_key,
        harness_creds,
    })
}

pub(crate) fn reserve_lead_slot(
    app: &AppHandle,
    db: &crate::db::Db,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    has_message: bool,
) -> Result<Option<ReservationGuard>, String> {
    let reserved = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        reserve_lead_start_after_globalstop(
            &conn,
            running,
            team_running,
            session_id,
            current_locale(app),
            has_message,
        )
    };
    let guard = match reserved {
        Ok(g) => g,
        Err(e) => {
            refresh_session_runtime(db, running, team_running, session_id);
            return Err(e);
        }
    };
    let Some(mut guard) = guard else {
        return Ok(None);
    };
    guard = guard.with_refresh(team_running.clone(), app.clone());
    Ok(Some(guard))
}
