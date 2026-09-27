// This module contains settings-related Tauri commands and their helpers.

use crate::{
    conn_test, db, keychain, remote_gateway, repos_repo, ui_msg, ActiveRoomCredentialCache, Db,
    KeyringStore,
};
use rusqlite::Connection;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Clone, Serialize)]
pub(super) struct RemoteControlSettings {
    pub(super) enabled: bool,
    /// The original stored value, which may be empty. An empty value means it was not customized;
    /// the gateway and pairing code fall back to `remote_gateway::DEFAULT_PUBLIC_RELAY_URL`.
    pub(super) relay_url: String,
    /// The official public relay URL used when the relay address is empty. This is always the value
    /// of `remote_gateway::DEFAULT_PUBLIC_RELAY_URL`; the settings page uses it to display the
    /// default, and it is not another writable state value.
    pub(super) default_relay_url: String,
    /// The project bound to the gateway's active room. The same app-settings key is written by
    /// `remote_set_active_project_in_conn` and read by `remote_gateway.rs::current_config`; this
    /// field only exposes that value to the settings UI without changing its meaning. `None` means
    /// no active project is configured.
    pub(super) active_repo_id: Option<String>,
}

pub(super) fn remote_control_get_settings_in_conn(
    conn: &Connection,
) -> Result<RemoteControlSettings, String> {
    let enabled = db::get_app_setting(conn, "remote_control_enabled")
        .map_err(|e| e.to_string())?
        .map(|v| v == "true")
        .unwrap_or(false);
    let relay_url = db::get_app_setting(conn, "remote_relay_url")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    // Apply trim and filter on reads to match `remote_set_active_project_in_conn` and
    // `remote_gateway.rs::current_config`. Previously, only this path returned the raw value, so a
    // manually stored whitespace-only value appeared to the frontend as an active project and
    // bypassed the UI gate that prevents pairing without a selected project.
    let active_repo_id = db::get_app_setting(conn, REMOTE_ACTIVE_REPO_ID_SETTING)
        .map_err(|e| e.to_string())?
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    Ok(RemoteControlSettings {
        enabled,
        relay_url,
        default_relay_url: remote_gateway::DEFAULT_PUBLIC_RELAY_URL.to_owned(),
        active_repo_id,
    })
}

pub(super) fn remote_control_set_settings_in_conn(
    conn: &Connection,
    enabled: bool,
    relay_url: &str,
) -> Result<(), String> {
    let trimmed = relay_url.trim();
    if !trimmed.is_empty() {
        // A `starts_with("wss://")` check alone allows forms such as `wss://user:pass@host`,
        // `wss://host/room`, `wss://host?x=1`, and `wss://host#x`. The frontend rejects these too,
        // but the backend is the final boundary. This character-presence check avoids a URL parser
        // dependency: the suffix after `wss://` must be nonempty, contain no `@`, `?`, or `#`, and
        // may contain `/` only as one trailing character. Thus `wss://host` and `wss://host/` are
        // accepted while `wss://host/room` and `wss://host//` are rejected.
        let invalid_relay_url = match trimmed.strip_prefix("wss://") {
            None => true,
            Some(rest) => {
                rest.is_empty()
                    || rest.contains('@')
                    || rest.contains('?')
                    || rest.contains('#')
                    || rest.find('/').is_some_and(|idx| idx != rest.len() - 1)
            }
        };
        if invalid_relay_url {
            return Err(ui_msg::al_err(
                "remoteControl.invalidRelayUrl",
                &[("url", trimmed.to_string())],
            ));
        }
    }
    db::set_app_setting(
        conn,
        "remote_control_enabled",
        if enabled { "true" } else { "false" },
    )
    .map_err(|e| e.to_string())?;
    db::set_app_setting(conn, "remote_relay_url", trimmed).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn remote_control_get_settings(
    db: State<'_, Db>,
) -> Result<RemoteControlSettings, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    remote_control_get_settings_in_conn(&conn)
}

#[tauri::command]
pub(super) fn remote_control_set_settings(
    db: State<'_, Db>,
    enabled: bool,
    relay_url: String,
) -> Result<(), String> {
    let result = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        remote_control_set_settings_in_conn(&conn, enabled, &relay_url)
    };
    if result.is_ok() {
        remote_gateway::request_settings_reload();
    }
    result
}

/// Identifies which project is the source for the gateway's active room. This is the same
/// app-settings key read by `remote_gateway.rs::current_config`. That code intentionally uses a
/// literal instead of sharing this constant, consistent with the existing cross-file literal
/// convention for `REMOTE_ROOM_ID_SETTING`; the literals must remain identical.
pub(super) const REMOTE_ACTIVE_REPO_ID_SETTING: &str = "remote_active_repo_id";

/// `None` or whitespace clears the setting with `DELETE`, leaving no empty-string row. This matches
/// the convention used by `set_cli_path_in_conn` when its value is `None`.
///
/// Before writing, verify under the same connection lock that `repo_id` exists in the `repos`
/// table. Otherwise, a typo or stale ID could silently point the active project at a missing repo,
/// making every `current_config` parse fail. A missing repo returns the localized
/// `remoteControl.activeProjectMissing` error.
pub(super) fn remote_set_active_project_in_conn(
    conn: &Connection,
    repo_id: Option<&str>,
) -> Result<(), String> {
    match repo_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(repo_id) => {
            let exists = repos_repo::get_repo_by_id(conn, repo_id)
                .map_err(|error| error.to_string())?
                .is_some();
            if !exists {
                return Err(ui_msg::al_err(
                    "remoteControl.activeProjectMissing",
                    &[("repoId", repo_id.to_string())],
                ));
            }
            db::set_app_setting(conn, REMOTE_ACTIVE_REPO_ID_SETTING, repo_id)
                .map_err(|error| error.to_string())
        }
        None => conn
            .execute(
                "DELETE FROM app_settings WHERE key = ?1",
                [REMOTE_ACTIVE_REPO_ID_SETTING],
            )
            .map(|_rows| ())
            .map_err(|error| error.to_string()),
    }
}

/// `last_active_repo_id` is the per-namespace UI convenience that remembers which repo to select
/// when returning to a namespace. It is not the single global value identifying the project under
/// remote control, so this uses an independent app-settings key.
///
/// After a successful write, clear `ActiveRoomCredentialCache`. The gateway's
/// `active_room_resolver` uses the same `Arc`, so the next resolution checks the keychain again
/// instead of trusting a marker established for the previous active project or credential state.
#[tauri::command]
pub(super) fn remote_set_active_project(
    db: State<'_, Db>,
    cache: State<'_, ActiveRoomCredentialCache>,
    repo_id: Option<String>,
) -> Result<(), String> {
    let result = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        remote_set_active_project_in_conn(&conn, repo_id.as_deref())
    };
    if result.is_ok() {
        if let Ok(mut credential_ensured) = cache.0.lock() {
            credential_ensured.clear();
        }
        remote_gateway::request_settings_reload();
    }
    result
}

#[tauri::command]
pub(super) fn get_active_backend(db: State<'_, Db>) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::get_active_search_backend(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn get_search_key(backend: String) -> Result<bool, String> {
    keychain::search_key_configured_with_store(&KeyringStore, &backend)
}

/// Saving a key also makes its backend active. The UI has no separate active-backend control;
/// selecting a service, entering its key, and saving is the only way to choose the search service.
#[tauri::command]
pub(super) fn set_search_key(
    db: State<'_, Db>,
    backend: String,
    key: String,
) -> Result<(), String> {
    keychain::set_search_key_with_store(&KeyringStore, &backend, &key)?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_active_search_backend(&conn, &backend)
}

/// DuckDuckGo requires no key, so there is no save-key action that can also make it active. This
/// separate entry point changes only the active backend ID and does not modify any key entry.
#[tauri::command]
pub(super) fn set_active_search_backend(db: State<'_, Db>, backend: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_active_search_backend(&conn, &backend)
}

#[tauri::command]
pub(super) async fn test_search_service(
    backend: String,
    api_key: Option<String>,
) -> Result<conn_test::ConnectionTestResult, String> {
    let key = match api_key
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
    {
        Some(k) => k,
        None => return Ok(conn_test::resolve_missing()),
    };
    tauri::async_runtime::spawn_blocking(move || conn_test::probe_search(&backend, &key))
        .await
        .map_err(|e| e.to_string())
}
