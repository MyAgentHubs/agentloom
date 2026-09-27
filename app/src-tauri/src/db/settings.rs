use super::schema;
use rusqlite::{Connection, OptionalExtension};

pub(super) const ACTIVE_SEARCH_BACKEND_SETTING: &str = "search.active";

pub fn get_app_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
}

pub fn set_app_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO app_settings(key, value) VALUES(?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

pub fn is_commit_authorized(conn: &Connection, repo_key: &str) -> Result<bool, String> {
    let key = format!("commit.authorized.{repo_key}");
    let value = get_app_setting(conn, &key).map_err(|e| e.to_string())?;
    Ok(matches!(value.as_deref(), Some("1") | Some("true")))
}

pub fn set_commit_authorized(
    conn: &Connection,
    repo_key: &str,
    authorized: bool,
) -> Result<(), String> {
    let key = format!("commit.authorized.{repo_key}");
    let value = if authorized { "1" } else { "0" };
    set_app_setting(conn, &key, value).map_err(|e| e.to_string())
}

pub fn get_active_search_backend(conn: &Connection) -> rusqlite::Result<String> {
    Ok(
        match get_app_setting(conn, ACTIVE_SEARCH_BACKEND_SETTING)?.as_deref() {
            Some("exa") => "exa".to_string(),
            Some("duckduckgo") => "duckduckgo".to_string(),
            _ => "brave".to_string(),
        },
    )
}

pub fn set_active_search_backend(conn: &Connection, backend: &str) -> Result<(), String> {
    match backend {
        "duckduckgo" | "brave" | "exa" => {
            set_app_setting(conn, ACTIVE_SEARCH_BACKEND_SETTING, backend).map_err(|e| e.to_string())
        }
        other => Err(format!("invalid search backend: {other}")),
    }
}

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    schema::apply_all(conn)
}
