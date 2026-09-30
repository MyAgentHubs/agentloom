// Repo listing and runtime detection IPC.

use crate::{db, detect, github, repos_repo, ui_msg, Db, APP_DATA_DIR};
use rusqlite::Connection;
use tauri::State;

pub(super) const CLAUDE_CLI_PATH_SETTING: &str = "cli_path.claude";
pub(super) const CODEX_CLI_PATH_SETTING: &str = "cli_path.codex";

#[tauri::command]
pub(super) fn list_repos(db: State<Db>) -> Result<Vec<repos_repo::RepoMeta>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    repos_repo::list_active(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn list_repos_by_status(
    db: State<Db>,
    status: String,
) -> Result<Vec<repos_repo::RepoMeta>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    repos_repo::list_by_status(&conn, &status).map_err(|e| e.to_string())
}

fn cli_path_setting_key(cli: &str) -> Result<&'static str, String> {
    match cli {
        "claude" => Ok(CLAUDE_CLI_PATH_SETTING),
        "codex" => Ok(CODEX_CLI_PATH_SETTING),
        _ => Err(ui_msg::al_err(
            "cliPath.invalidCli",
            &[("cli", cli.to_string())],
        )),
    }
}

pub(super) fn set_cli_path_in_conn(
    conn: &Connection,
    cli: &str,
    path: Option<&str>,
    windows: bool,
) -> Result<(), String> {
    let key = cli_path_setting_key(cli)?;
    let path = path.map(str::trim).filter(|path| !path.is_empty());
    match path {
        Some(path) => {
            if !detect::override_path_allowed(std::path::Path::new(path), windows) {
                return Err(ui_msg::al_err(
                    "cliPath.invalidPath",
                    &[("path", path.to_string())],
                ));
            }
            db::set_app_setting(conn, key, path).map_err(|error| {
                ui_msg::al_err(
                    "cliPath.databaseUnavailable",
                    &[("detail", error.to_string())],
                )
            })?;
        }
        None => {
            conn.execute("DELETE FROM app_settings WHERE key = ?1", [key])
                .map_err(|error| {
                    ui_msg::al_err(
                        "cliPath.databaseUnavailable",
                        &[("detail", error.to_string())],
                    )
                })?;
        }
    }
    // The database is authoritative. Update it first so a cache failure is surfaced while the
    // persisted choice remains available to detection and will repopulate the cache on restart.
    detect::set_cached_cli_path(cli, path)
        .map_err(|detail| ui_msg::al_err("cliPath.databaseUnavailable", &[("detail", detail)]))
}

pub(super) fn load_cli_path_override_cache(conn: &Connection) -> Result<(), String> {
    let claude = db::get_app_setting(conn, CLAUDE_CLI_PATH_SETTING).map_err(|e| e.to_string())?;
    let codex = db::get_app_setting(conn, CODEX_CLI_PATH_SETTING).map_err(|e| e.to_string())?;
    detect::replace_cached_cli_paths([("claude", claude), ("codex", codex)])
}

pub(super) fn cli_path_override_for_spawn_from(
    cli: &str,
    cached: detect::CachedCliPath,
    read_database: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    match cached {
        detect::CachedCliPath::Ready(path) => Ok(path),
        detect::CachedCliPath::Uninitialized => {
            let path = read_database(cli)?;
            detect::set_cached_cli_path(cli, path.as_deref())?;
            Ok(path)
        }
    }
}

pub(crate) fn cli_path_override_for_spawn(cli: &str) -> Option<String> {
    let cached = detect::cached_cli_path_for_spawn(cli);
    cli_path_override_for_spawn_from(cli, cached, |cli| {
        let key = cli_path_setting_key(cli)?;
        let dir = APP_DATA_DIR
            .get()
            .ok_or_else(|| "application data directory is unavailable".to_string())?;
        let conn = Connection::open(dir.join("agentloom.db")).map_err(|error| error.to_string())?;
        db::get_app_setting(&conn, key).map_err(|error| error.to_string())
    })
    .map_err(|error| {
        eprintln!(
            "CLI path override cache is uninitialized and the database fallback failed: {error}"
        );
        error
    })
    .ok()
    .flatten()
}

fn detect_runtime_value(db: &Db) -> serde_json::Value {
    let (claude_override, codex_override) = match db.0.lock() {
        Ok(conn) => (
            db::get_app_setting(&conn, CLAUDE_CLI_PATH_SETTING)
                .ok()
                .flatten(),
            db::get_app_setting(&conn, CODEX_CLI_PATH_SETTING)
                .ok()
                .flatten(),
        ),
        Err(_) => (None, None),
    };

    // Return both the Claude and Codex runtimes for the first onboarding step.
    serde_json::json!({
        "claude": detect::detect_claude_with_override(claude_override.as_deref()),
        "codex": detect::detect_codex_with_override(codex_override.as_deref()),
    })
}

#[tauri::command]
pub(super) fn detect_runtime(db: State<Db>) -> serde_json::Value {
    detect_runtime_value(&db)
}

#[tauri::command]
pub(super) fn set_cli_path(
    db: State<Db>,
    cli: String,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    {
        let conn = db.0.lock().map_err(|error| {
            ui_msg::al_err(
                "cliPath.databaseUnavailable",
                &[("detail", error.to_string())],
            )
        })?;
        set_cli_path_in_conn(&conn, &cli, path.as_deref(), cfg!(target_os = "windows"))?;
    }
    Ok(detect_runtime_value(&db))
}

#[tauri::command]
pub(super) fn detect_git() -> detect::DetectResult {
    detect::detect_git()
}

#[tauri::command]
pub(super) fn detect_gh() -> detect::DetectResult {
    detect::detect_gh()
}

#[tauri::command]
pub(super) async fn install_gh() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(github::run_install_gh)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(super) fn detect_brew() -> bool {
    github::detect_brew_available()
}

// Errors from this command are never shown to the user: the frontend silently falls back
// to its static model table, so the strings here stay plain English for logs only.
#[tauri::command]
pub(super) async fn list_codex_models(
    db: State<'_, Db>,
) -> Result<Vec<crate::codex_models::CodexModelInfo>, String> {
    let codex_override = match db.0.lock() {
        Ok(conn) => db::get_app_setting(&conn, CODEX_CLI_PATH_SETTING)
            .ok()
            .flatten(),
        Err(_) => None,
    };
    tauri::async_runtime::spawn_blocking(move || {
        let path = detect::detect_codex_with_override(codex_override.as_deref())
            .path
            .ok_or_else(|| "codex CLI not found".to_string())?;
        crate::codex_models::list_codex_models_with_bin(
            std::path::Path::new(&path),
            std::time::Duration::from_secs(15),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}
