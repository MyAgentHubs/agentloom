#![cfg(test)]

use super::*;

#[test]
fn active_backend_defaults_to_brave_when_unset() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
}

#[test]
fn active_backend_roundtrip_and_dirty_value_defaults_brave() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
    set_active_search_backend(&conn, "exa").unwrap();
    assert_eq!(get_active_search_backend(&conn).unwrap(), "exa");
    set_app_setting(&conn, ACTIVE_SEARCH_BACKEND_SETTING, "searxng").unwrap();
    assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
    set_app_setting(&conn, ACTIVE_SEARCH_BACKEND_SETTING, "EXA").unwrap();
    assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
}

#[test]
fn active_backend_recognizes_duckduckgo() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    set_app_setting(&conn, ACTIVE_SEARCH_BACKEND_SETTING, "duckduckgo").unwrap();
    assert_eq!(get_active_search_backend(&conn).unwrap(), "duckduckgo");
}

#[test]
fn set_active_backend_accepts_known_values_and_roundtrips() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    set_active_search_backend(&conn, "duckduckgo").unwrap();
    assert_eq!(get_active_search_backend(&conn).unwrap(), "duckduckgo");

    set_active_search_backend(&conn, "brave").unwrap();
    assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");

    set_active_search_backend(&conn, "exa").unwrap();
    assert_eq!(get_active_search_backend(&conn).unwrap(), "exa");
}

#[test]
fn set_active_backend_rejects_unknown_value() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();

    let err = set_active_search_backend(&conn, "searxng").unwrap_err();
    assert_eq!(err, "invalid search backend: searxng");
}

#[test]
fn commit_authorized_defaults_to_false_when_unset() {
    let conn = mem();

    assert!(!is_commit_authorized(&conn, "repo-a").unwrap());
}

#[test]
fn commit_authorized_roundtrips_true_then_false() {
    let conn = mem();

    set_commit_authorized(&conn, "repo-a", true).unwrap();
    assert!(is_commit_authorized(&conn, "repo-a").unwrap());

    set_commit_authorized(&conn, "repo-a", false).unwrap();
    assert!(!is_commit_authorized(&conn, "repo-a").unwrap());
}

#[test]
fn commit_authorized_recognizes_true_string() {
    let conn = mem();

    set_app_setting(&conn, "commit.authorized.repo-a", "true").unwrap();

    assert!(is_commit_authorized(&conn, "repo-a").unwrap());
}

#[test]
fn commit_authorized_is_isolated_by_repo_key() {
    let conn = mem();

    set_commit_authorized(&conn, "repo-a", true).unwrap();

    assert!(is_commit_authorized(&conn, "repo-a").unwrap());
    assert!(!is_commit_authorized(&conn, "repo-b").unwrap());
}
