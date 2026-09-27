#![cfg(test)]

use super::*;

#[test]
fn session_run_state_none_when_no_row() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();

    let state = run_slots::query_session_run_state(&conn, "s1").unwrap();
    assert!(state.is_none());
}

#[test]
fn session_run_state_returns_running_row() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    db::set_session_runtime(&conn, "s1", db::SESSION_RUNTIME_RUNNING, Some("r1")).unwrap();

    let state = run_slots::query_session_run_state(&conn, "s1")
        .unwrap()
        .unwrap();
    assert_eq!(state.status, db::SESSION_RUNTIME_RUNNING);
    assert!(state.updated_at > 0);
}

#[test]
fn session_run_state_returns_idle_row() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    db::set_session_runtime(&conn, "s1", db::SESSION_RUNTIME_IDLE, Some("r1")).unwrap();

    let state = run_slots::query_session_run_state(&conn, "s1")
        .unwrap()
        .unwrap();
    assert_eq!(state.status, db::SESSION_RUNTIME_IDLE);
    assert!(state.updated_at > 0);
}
