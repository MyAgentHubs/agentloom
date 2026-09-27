#![cfg(test)]

use super::*;
use std::cell::{Cell, RefCell};

#[test]
fn reserved_first_run_without_persisted_agent_is_reported_busy() {
    use crate::test_support::mem_db;

    let conn = mem_db();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    let session_id = "s-reserved-first-run";
    db::create_session(&conn, session_id, "x", "local-default", "local").unwrap();

    reserve_new_session_run(&conn, &running, &team_running, session_id, Locale::Zh).unwrap();
    assert_eq!(
        resolve_session_run_agent(&conn, session_id).unwrap_err(),
        "AL_ERR:agent.sessionRunUnknown"
    );
    assert!(session_run_slot_reserved(
        &running,
        &team_running,
        session_id
    ));
    assert_eq!(
        remote_inbox_agent_id_or_busy(
            resolve_session_run_agent(&conn, session_id).map(|profile| profile.id),
            &running,
            &team_running,
            session_id,
        )
        .unwrap_err(),
        format!("SESSION_BUSY:{session_id}")
    );
    assert!(autofeed_busy_error(&format!("SESSION_BUSY:{session_id}")));

    let idle_session_id = "s-idle-without-agent";
    db::create_session(&conn, idle_session_id, "x", "local-default", "local").unwrap();
    assert!(!session_run_slot_reserved(
        &running,
        &team_running,
        idle_session_id
    ));
    assert_eq!(
        remote_inbox_agent_id_or_busy(
            resolve_session_run_agent(&conn, idle_session_id).map(|profile| profile.id),
            &running,
            &team_running,
            idle_session_id,
        )
        .unwrap_err(),
        "AL_ERR:agent.sessionRunUnknown"
    );
}

#[test]
fn drain_remote_inbox_loop_leaves_busy_entry_pending_without_ack() {
    let mut pending = Some((
        1,
        "cmd-busy".to_string(),
        "input.send".to_string(),
        "{\"text\":\"hello\"}".to_string(),
    ));
    let marked_delivered = Cell::new(0);
    let recorded_failure = Cell::new(0);
    let marked_failed = Cell::new(0);
    let notifications = Cell::new(0);

    drain_remote_inbox_loop(
        || pending.take(),
        |_kind, _payload, _command_id| Err("SESSION_BUSY:s-reserved-first-run".to_string()),
        |_id, _command_id| {
            marked_delivered.set(marked_delivered.get() + 1);
            true
        },
        |_id, _command_id, _error| {
            recorded_failure.set(recorded_failure.get() + 1);
            Some(1)
        },
        |_id, _command_id, _error| {
            marked_failed.set(marked_failed.get() + 1);
            true
        },
        |_command_id, _reason| notifications.set(notifications.get() + 1),
    );

    assert_eq!(marked_delivered.get(), 0);
    assert_eq!(recorded_failure.get(), 0);
    assert_eq!(marked_failed.get(), 0);
    assert_eq!(notifications.get(), 0);
}

#[test]
fn classifies_session_run_unknown_as_terminal_no_agent() {
    assert_eq!(
        classify_remote_inbox_error("AL_ERR:agent.sessionRunUnknown"),
        RemoteInboxErrorClass::Terminal {
            reason: Some("no_agent")
        }
    );
    assert_eq!(
        classify_remote_inbox_error("REMOTE_INBOX_PAYLOAD_MALFORMED"),
        RemoteInboxErrorClass::Terminal { reason: None }
    );
    assert_eq!(
        classify_remote_inbox_error("UNKNOWN_REMOTE_INBOX_KIND:control.bogus"),
        RemoteInboxErrorClass::Terminal { reason: None }
    );
    assert_eq!(
        classify_remote_inbox_error("AGENT_NOT_FOUND"),
        RemoteInboxErrorClass::Delivery
    );
    assert_eq!(
        classify_remote_inbox_error("some other delivery failure"),
        RemoteInboxErrorClass::Delivery
    );
}

#[test]
fn drain_remote_inbox_loop_marks_no_agent_terminal_and_notifies_once() {
    let mut pending = Some((
        1,
        "cmd-no-agent".to_string(),
        "input.send".to_string(),
        "{\"text\":\"hello\"}".to_string(),
    ));
    let delivery_calls = Cell::new(0);
    let record_failure_calls = Cell::new(0);
    let marked_failed = RefCell::new(Vec::new());
    let notifications = RefCell::new(Vec::new());

    drain_remote_inbox_loop(
        || pending.take(),
        |_kind, _payload, _command_id| {
            delivery_calls.set(delivery_calls.get() + 1);
            Err("AL_ERR:agent.sessionRunUnknown".to_string())
        },
        |_id, _command_id| panic!("terminal failure must not mark delivered"),
        |_id, _command_id, _error| {
            record_failure_calls.set(record_failure_calls.get() + 1);
            None
        },
        |id, command_id, error| {
            marked_failed
                .borrow_mut()
                .push((id, command_id.to_string(), error.to_string()));
            true
        },
        |command_id, reason| {
            notifications
                .borrow_mut()
                .push((command_id.to_string(), reason.map(str::to_string)));
        },
    );

    assert_eq!(delivery_calls.get(), 1);
    assert_eq!(record_failure_calls.get(), 0);
    assert_eq!(marked_failed.borrow().len(), 1);
    assert_eq!(
        notifications.borrow().as_slice(),
        &[("cmd-no-agent".to_string(), Some("no_agent".to_string()))]
    );
}

#[test]
fn drain_remote_inbox_loop_notifies_only_after_third_delivery_failure() {
    let attempts = Cell::new(0_i64);
    let terminal = Cell::new(false);
    let delivery_calls = Cell::new(0);
    let marked_failed = Cell::new(0);
    let notifications = RefCell::new(Vec::new());

    for expected_attempts in 1..=3 {
        drain_remote_inbox_loop(
            || {
                (!terminal.get()).then(|| {
                    (
                        1,
                        "cmd-retry-ack".to_string(),
                        "input.send".to_string(),
                        "{\"text\":\"retry\"}".to_string(),
                    )
                })
            },
            |_kind, _payload, _command_id| {
                delivery_calls.set(delivery_calls.get() + 1);
                Err("AGENT_NOT_FOUND".to_string())
            },
            |_id, _command_id| panic!("delivery failure must not mark delivered"),
            |_id, _command_id, error| {
                assert_eq!(error, "AGENT_NOT_FOUND");
                attempts.set(attempts.get() + 1);
                Some(attempts.get())
            },
            |_id, _command_id, error| {
                assert_eq!(error, "AGENT_NOT_FOUND");
                marked_failed.set(marked_failed.get() + 1);
                terminal.set(true);
                true
            },
            |command_id, reason| {
                notifications
                    .borrow_mut()
                    .push((command_id.to_string(), reason.map(str::to_string)));
            },
        );

        assert_eq!(attempts.get(), expected_attempts);
        assert_eq!(
            notifications.borrow().len(),
            usize::from(expected_attempts == 3)
        );
    }

    assert_eq!(delivery_calls.get(), 3);
    assert_eq!(marked_failed.get(), 1);
    assert_eq!(
        notifications.borrow().as_slice(),
        &[("cmd-retry-ack".to_string(), None)]
    );
}
