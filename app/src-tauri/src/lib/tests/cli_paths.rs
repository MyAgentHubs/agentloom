#![cfg(test)]

use super::*;

fn cached_cli_path(cli: &str) -> Option<String> {
    detect::cli_path_overrides()
        .read()
        .ok()
        .and_then(|cache| cache.paths.get(cli).cloned())
}

#[test]
fn cli_path_rejects_unknown_cli_with_ui_error() {
    let conn = cli_path_test_db();

    let error = set_cli_path_in_conn(&conn, "gemini", None, false).unwrap_err();

    assert_eq!(error, r#"AL_ERR:cliPath.invalidCli:{"cli":"gemini"}"#);
}

#[test]
fn cli_path_none_and_blank_both_clear_the_setting() {
    let _guard = detect::CliPathOverrideTestGuard::new();
    let conn = cli_path_test_db();

    db::set_app_setting(&conn, CLAUDE_CLI_PATH_SETTING, "/old/claude").unwrap();
    set_cli_path_in_conn(&conn, "claude", None, false).unwrap();
    assert_eq!(
        db::get_app_setting(&conn, CLAUDE_CLI_PATH_SETTING).unwrap(),
        None
    );

    db::set_app_setting(&conn, CLAUDE_CLI_PATH_SETTING, "/old/claude").unwrap();
    set_cli_path_in_conn(&conn, "claude", Some("  "), false).unwrap();
    assert_eq!(
        db::get_app_setting(&conn, CLAUDE_CLI_PATH_SETTING).unwrap(),
        None
    );
}

#[test]
fn cli_path_invalid_path_returns_ui_error_without_writing() {
    let conn = cli_path_test_db();
    db::set_app_setting(&conn, CODEX_CLI_PATH_SETTING, "/old/codex").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing-codex");

    let error = set_cli_path_in_conn(&conn, "codex", missing.to_str(), false).unwrap_err();

    assert!(error.starts_with("AL_ERR:cliPath.invalidPath:"));
    assert_eq!(
        db::get_app_setting(&conn, CODEX_CLI_PATH_SETTING)
            .unwrap()
            .as_deref(),
        Some("/old/codex")
    );
}

#[test]
fn cli_path_write_and_clear_keep_spawn_override_cache_in_sync() {
    let _guard = detect::CliPathOverrideTestGuard::new();
    let conn = cli_path_test_db();
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("claude");
    std::fs::write(&cli, "test cli").unwrap();

    set_cli_path_in_conn(&conn, "claude", cli.to_str(), false).unwrap();
    assert_eq!(cached_cli_path("claude").as_deref(), cli.to_str());

    set_cli_path_in_conn(&conn, "claude", None, false).unwrap();
    assert_eq!(cached_cli_path("claude"), None);
}

#[test]
fn failed_startup_cache_load_falls_back_to_the_database_instead_of_meaning_no_override() {
    let _guard = detect::CliPathOverrideTestGuard::new();
    let broken_startup_db = Connection::open_in_memory().unwrap();
    assert!(load_cli_path_override_cache(&broken_startup_db).is_err());

    let fallback_calls = std::cell::Cell::new(0);
    let resolved = cli_path_override_for_spawn_from(
        "codex",
        detect::cached_cli_path_for_spawn("codex"),
        |cli| {
            fallback_calls.set(fallback_calls.get() + 1);
            assert_eq!(cli, "codex");
            Ok(Some("/database/codex".to_string()))
        },
    )
    .unwrap();

    assert_eq!(resolved.as_deref(), Some("/database/codex"));
    assert_eq!(fallback_calls.get(), 1);
    assert_eq!(
        detect::cached_cli_path_for_spawn("codex"),
        detect::CachedCliPath::Ready(Some("/database/codex".to_string()))
    );
}

#[test]
fn startup_cache_load_reaches_the_real_codex_and_claude_resolvers() {
    let _guard = detect::CliPathOverrideTestGuard::new();
    let conn = cli_path_test_db();
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("codex");
    let claude = dir.path().join("claude");
    std::fs::write(&codex, "test codex").unwrap();
    std::fs::write(&claude, "test claude").unwrap();
    db::set_app_setting(&conn, CODEX_CLI_PATH_SETTING, codex.to_str().unwrap()).unwrap();
    db::set_app_setting(&conn, CLAUDE_CLI_PATH_SETTING, claude.to_str().unwrap()).unwrap();

    load_cli_path_override_cache(&conn).unwrap();

    assert_eq!(agent::resolve_codex_bin().unwrap(), codex.into_os_string());
    assert_eq!(
        sandbox::resolve_claude_bin_for_spawn().unwrap(),
        claude.to_string_lossy()
    );
}
