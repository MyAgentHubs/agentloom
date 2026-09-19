#![cfg(test)]

use super::*;

#[test]
fn first_event_watchdog_error_injection_requires_timeout() {
    assert!(!should_inject_first_event_watchdog_error(
        false, false, None
    ));
}

#[test]
fn first_event_watchdog_error_injection_is_suppressed_by_stop() {
    assert!(!should_inject_first_event_watchdog_error(
        true,
        false,
        Some("timeout")
    ));
}

#[test]
fn first_event_watchdog_error_injection_is_suppressed_by_completed() {
    assert!(!should_inject_first_event_watchdog_error(
        false,
        true,
        Some("timeout")
    ));
}

#[test]
fn first_event_watchdog_error_injection_accepts_unhandled_timeout() {
    assert!(should_inject_first_event_watchdog_error(
        false,
        false,
        Some("timeout")
    ));
}
