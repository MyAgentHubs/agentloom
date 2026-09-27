// This module contains agent-related Tauri commands and their helpers.

use crate::{
    conn_test, db, namespaces_repo, repos_repo, resolve_active_repo_for_namespace, ui_msg,
    AgentProfile, Db, KeyStore, KeyringStore,
};
use rusqlite::Connection;
use tauri::{AppHandle, Manager, State};

pub(super) fn delete_agent_with_store(
    conn: &Connection,
    store: &dyn KeyStore,
    id: &str,
) -> Result<(), String> {
    db::delete_agent(conn, id).map_err(|e| e.to_string())?;
    if let Err(e) = store.delete(id) {
        eprintln!("delete_agent_with_store key delete failed for {id}: {e}");
    }
    Ok(())
}

pub(super) fn upsert_agent_guarded(
    conn: &Connection,
    profile: &AgentProfile,
) -> Result<(), String> {
    if let Some(existing) = db::get_agent(conn, &profile.id).map_err(|e| e.to_string())? {
        if existing.access == "native" && profile.access != "native" {
            return Err(ui_msg::al_err("agent.nativeAccessImmutable", &[]));
        }
    }
    db::upsert_agent(conn, profile).map_err(|e| e.to_string())
}

pub(super) fn set_agent_key_with_store(
    conn: &Connection,
    store: &dyn KeyStore,
    id: &str,
    key: &str,
) -> Result<(), String> {
    let mut profile = db::get_agent(conn, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("agent.notFound", &[]))?;
    if profile.access == "native" {
        return Err(ui_msg::al_err("agent.nativeKeyUnsupported", &[]));
    }
    store
        .set(id, key)
        .map_err(|detail| ui_msg::al_err("agent.keychainSaveFailed", &[("detail", detail)]))?;
    let saved = store
        .get(id)
        .map_err(|detail| ui_msg::al_err("agent.keychainSaveFailed", &[("detail", detail)]))?;
    if saved.as_deref() != Some(key) {
        return Err(ui_msg::al_err("agent.keychainSaveFailed", &[]));
    }
    profile.has_key = true;
    db::upsert_agent(conn, &profile).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub(super) struct AppContext {
    namespaces: Vec<namespaces_repo::NamespaceMeta>,
    active_namespace_id: String,
    active_repo_id: Option<String>,
    repos: Vec<repos_repo::RepoMeta>,
}

#[tauri::command]
pub(super) fn app_context(db: State<Db>) -> Result<AppContext, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let namespaces = namespaces_repo::list_active_namespaces(&conn).map_err(|e| e.to_string())?;
    let active_namespace_id = "local".to_string();
    let active_repo_id = resolve_active_repo_for_namespace(&conn, &active_namespace_id)?;
    let repos = repos_repo::list_active_by_namespace(&conn, &active_namespace_id)
        .map_err(|e| e.to_string())?;
    Ok(AppContext {
        namespaces,
        active_namespace_id,
        active_repo_id,
        repos,
    })
}

#[tauri::command]
pub(super) fn list_agents(db: State<Db>) -> Result<Vec<AgentProfile>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_agents(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn upsert_agent(db: State<Db>, profile: AgentProfile) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    upsert_agent_guarded(&conn, &profile)
}

pub(super) fn get_session_agent_config_impl(
    db: &Db,
    session_id: &str,
) -> Result<db::SessionAgentConfig, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::get_session_agent_config(&conn, session_id).map_err(|e| e.to_string())
}

pub(super) fn set_session_agent_config_impl(
    db: &Db,
    session_id: &str,
    lead_agent_id: Option<String>,
    member_agent_ids: Vec<String>,
) -> Result<db::SessionAgentConfig, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_session_agent_config(&conn, session_id, lead_agent_id, member_agent_ids)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn get_session_agent_config(
    app: AppHandle,
    session_id: String,
) -> Result<db::SessionAgentConfig, String> {
    let db = app.state::<Db>();
    get_session_agent_config_impl(db.inner(), &session_id)
}

#[tauri::command]
pub(super) fn set_session_agent_config(
    app: AppHandle,
    session_id: String,
    lead_agent_id: Option<String>,
    member_agent_ids: Vec<String>,
) -> Result<db::SessionAgentConfig, String> {
    let db = app.state::<Db>();
    set_session_agent_config_impl(db.inner(), &session_id, lead_agent_id, member_agent_ids)
}

#[tauri::command]
pub(super) fn delete_agent(db: State<Db>, id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    delete_agent_with_store(&conn, &KeyringStore, &id)
}

#[tauri::command]
pub(super) fn set_agent_key(db: State<Db>, id: String, key: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    set_agent_key_with_store(&conn, &KeyringStore, &id, &key)
}

#[tauri::command]
pub(super) async fn test_agent_connection(
    agent_id: Option<String>,
    endpoint: String,
    protocol: Option<String>,
    auth_mode: Option<String>,
    model: String,
    api_key: Option<String>,
) -> Result<conn_test::ConnectionTestResult, String> {
    let key = match conn_test::resolve_key(&KeyringStore, agent_id.as_deref(), api_key.as_deref())?
    {
        Some(k) => k,
        None => return Ok(conn_test::resolve_missing()),
    };
    tauri::async_runtime::spawn_blocking(move || {
        conn_test::probe(
            &endpoint,
            protocol.as_deref(),
            auth_mode.as_deref(),
            &model,
            &key,
        )
    })
    .await
    .map_err(|e| e.to_string())
}

/// `build_models_url` derives the actual request URL from `models_endpoint` and `protocol`.
/// When `protocol` is absent (as it is at every current frontend call site),
/// `build_models_url` returns `models_endpoint` unchanged, preserving the exact pre-wiring
/// behavior. The "base endpoint + protocol-specific /models" branch is used only if the
/// frontend explicitly supplies `protocol` in the future.
#[tauri::command]
pub(super) async fn fetch_agent_models(
    agent_id: Option<String>,
    models_endpoint: String,
    protocol: Option<String>,
    auth_mode: Option<String>,
    api_key: Option<String>,
) -> Result<Vec<String>, String> {
    let key = conn_test::resolve_key(&KeyringStore, agent_id.as_deref(), api_key.as_deref())?
        .ok_or_else(|| "missing_key".to_string())?;
    let url = conn_test::build_models_url(protocol.as_deref(), &models_endpoint);
    tauri::async_runtime::spawn_blocking(move || {
        conn_test::fetch_models_blocking(&url, auth_mode.as_deref(), &key)
    })
    .await
    .map_err(|e| e.to_string())?
}
