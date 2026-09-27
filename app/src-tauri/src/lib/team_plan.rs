/// Commands and helpers for drafting team plans.
use super::{
    agent, build_lead_backend_command, current_locale, db, ensure_inplace_or_app_workspace,
    ensure_inplace_session_workdir, lead_draft, new_run_id, parse_fn_for_profile,
    parser_for_parse_fn, resolve_harness_search_creds, resolve_member_key,
    resolve_session_workspace, ui_msg, Connection,
};
use tauri::Manager;

#[derive(Debug, Clone)]
pub(super) struct EffectiveTeamConfig {
    pub(super) lead: db::AgentProfile,
    pub(super) member_agent_ids: Option<Vec<String>>,
    pub(super) strict_member_pool: bool,
}

fn require_effective_lead_agent(
    conn: &Connection,
    lead_agent_id: &str,
) -> Result<db::AgentProfile, String> {
    let lead = db::get_agent(conn, lead_agent_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            ui_msg::al_err("run.unknownLeadAgent", &[("id", lead_agent_id.to_string())])
        })?;
    if !lead.enabled {
        return Err(format!("lead agent {lead_agent_id} disabled"));
    }
    Ok(lead)
}

pub(super) fn resolve_effective_team_config(
    conn: &Connection,
    session_id: &str,
    requested_lead_id: &str,
    legacy_roster_agent_ids: Option<Vec<String>>,
) -> Result<EffectiveTeamConfig, String> {
    let saved = db::get_session_agent_config(conn, session_id).map_err(|e| e.to_string())?;
    if let Some(saved_lead_id) = saved.lead_agent_id {
        let lead = require_effective_lead_agent(conn, &saved_lead_id)?;
        return Ok(EffectiveTeamConfig {
            lead,
            member_agent_ids: Some(saved.member_agent_ids),
            strict_member_pool: true,
        });
    }

    let lead = require_effective_lead_agent(conn, requested_lead_id)?;
    Ok(EffectiveTeamConfig {
        lead,
        member_agent_ids: legacy_roster_agent_ids,
        strict_member_pool: false,
    })
}

fn filter_agents_for_effective_member_pool(
    agents: &[db::AgentProfile],
    member_agent_ids: Option<&[String]>,
    strict_member_pool: bool,
) -> Vec<db::AgentProfile> {
    if strict_member_pool {
        lead_draft::filter_agents_by_roster_strict(agents, member_agent_ids)
    } else {
        lead_draft::filter_agents_by_roster(agents, member_agent_ids)
    }
}

/// Persist the lead draft plan as a draft contract so the gate card can render the proposed work.
/// Lock discipline: release the lock after resolving the driver agent and cwd; the slow driver call does not hold the lock (run_propose_team_plan briefly locks again when persisting).
/// Async + spawn_blocking fix: the sync command ran on the main thread, and real driver planning took seconds and once froze the entire UI.
/// The db State does not pass through the frontend; obtain it inside the closure via AppHandle.state::<Db>() (as in gh_repo_list/run threads; Db is Arc-shared).
#[tauri::command]
pub(super) async fn propose_team_plan(
    app: tauri::AppHandle,
    session_id: String,
    lead_id: String,
    goal: String,
    repo_context: Option<String>,
    roster_agent_ids: Option<Vec<String>>,
) -> Result<lead_draft::ProposeOutcome, String> {
    let locale = current_locale(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let db = app.state::<db::Db>();
        // Inside the lock, fetch only driver + cwd metadata (resolve is a pure DB lookup); create the app-domain fallback scaffold only after releasing the lock.
        // Also fetch the enabled agent pool inside the lock: feed it to the lead prompt so work is assigned by capability and dispatches are distributed.
        // run_propose_team_plan performs its own locked agent lookup for picking (semantics unchanged); this copy is only for building the prompt and is used after releasing the lock.
        let (driver, project, enabled_agents, member_agent_ids, strict_member_pool) = {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let effective = resolve_effective_team_config(
                &conn,
                &session_id,
                &lead_id,
                roster_agent_ids.clone(),
            )?;
            let _workspace = resolve_session_workspace(&conn, &session_id)?;
            let project = ensure_inplace_session_workdir(&conn, &session_id)?;
            let enabled_agents: Vec<db::AgentProfile> = db::list_agents(&conn)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|a| a.enabled)
                .collect();
            (
                effective.lead,
                project,
                enabled_agents,
                effective.member_agent_ids,
                effective.strict_member_pool,
            )
        };
        let wt = ensure_inplace_or_app_workspace(&session_id, project)?;
        let prompt = {
            let pool = filter_agents_for_effective_member_pool(
                &enabled_agents,
                member_agent_ids.as_deref(),
                strict_member_pool,
            );
            lead_draft::build_draft_prompt(&goal, repo_context.as_deref(), &pool, locale)
        };
        let effective_lead_id = driver.id.clone();
        let driver_parser = parser_for_parse_fn(parse_fn_for_profile(&driver));
        let hook_run_id = new_run_id();
        let spawn = || -> Result<std::process::Child, String> {
            let search =
                resolve_harness_search_creds(db.inner(), &driver, &crate::keychain::KeyringStore)?;
            let key = resolve_member_key(&driver)?;
            // Narrow the lock scope: release the guard immediately after build, then spawn.
            let (mut cmd, stdin_prompt) = {
                let conn = db.0.lock().map_err(|e| e.to_string())?;
                let (cmd, _, stdin_prompt) = build_lead_backend_command(
                    &conn,
                    &session_id,
                    &hook_run_id,
                    &driver,
                    &prompt,
                    &wt,
                    agent::BuildMode::LeadDraft,
                    locale,
                    None,
                    key,
                    search,
                )?;
                (cmd, stdin_prompt)
            };
            cmd.stdout(std::process::Stdio::piped());
            // Pipe stderr so planning failures include the tail in last_error for GUI diagnosis (it was previously lost to app stderr).
            cmd.stderr(std::process::Stdio::piped());
            agent::spawn_with_stdin_prompt(&mut cmd, stdin_prompt.as_ref())
                .map_err(|e| ui_msg::al_err("lead.spawnDriverFailed", &[("detail", e.to_string())]))
        };
        if strict_member_pool {
            lead_draft::run_propose_team_plan_with_roster_mode(
                db.inner(),
                &session_id,
                &effective_lead_id,
                lead_draft::DRAFT_MAX_ATTEMPTS,
                driver_parser,
                spawn,
                &wt,
                member_agent_ids.as_deref(),
                true,
            )
        } else {
            lead_draft::run_propose_team_plan(
                db.inner(),
                &session_id,
                &effective_lead_id,
                lead_draft::DRAFT_MAX_ATTEMPTS,
                driver_parser,
                spawn,
                &wt,
                member_agent_ids.as_deref(),
            )
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
