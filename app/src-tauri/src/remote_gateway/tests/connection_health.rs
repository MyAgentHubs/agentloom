#![cfg(test)]

use super::*;
#[test]
fn gateway_status_carries_all_internal_counters() {
    let state = GatewayInnerState::default();
    state.frames_seen.store(1, Ordering::Relaxed);
    state.frames_sent.store(2, Ordering::Relaxed);
    state.bad_frames.store(3, Ordering::Relaxed);
    state.upstream_dropped.store(4, Ordering::Relaxed);
    state
        .upstream_stale_generation_dropped
        .store(5, Ordering::Relaxed);
    state.upstream_budget_dropped.store(6, Ordering::Relaxed);
    state.milestone_dropped.store(7, Ordering::Relaxed);
    state
        .session_index_snapshot_unavailable
        .store(8, Ordering::Relaxed);
    state
        .snapshot_worker_spawn_count
        .store(9, Ordering::Relaxed);
    state.tool_correlation_dropped.store(10, Ordering::Relaxed);
    state.classify_skipped.store(11, Ordering::Relaxed);
    state.connection_failures.store(12, Ordering::Relaxed);
    state.panics.store(13, Ordering::Relaxed);
    state.upstream_repo_filtered.store(14, Ordering::Relaxed);
    state
        .partial_snapshot_capacity_dropped
        .store(15, Ordering::Relaxed);
    state
        .snapshot_oversized_dropped
        .store(16, Ordering::Relaxed);
    state.history_oversized_dropped.store(17, Ordering::Relaxed);
    state.replay_oversized_dropped.store(18, Ordering::Relaxed);
    state.keepalive_pings_sent.store(19, Ordering::Relaxed);
    state.disconnect_config_stale.store(20, Ordering::Relaxed);
    state.disconnect_closed_by_peer.store(21, Ordering::Relaxed);
    state.disconnect_error.store(22, Ordering::Relaxed);
    *lock(&state.last_disconnect_reason) = "read failed: redacted diagnostic".to_owned();

    assert_eq!(
        gateway_status_from_state(&state).counters,
        GatewayCounters {
            frames_seen: 1,
            frames_sent: 2,
            bad_frames: 3,
            upstream_dropped: 4,
            upstream_stale_generation_dropped: 5,
            upstream_budget_dropped: 6,
            milestone_dropped: 7,
            session_index_snapshot_unavailable: 8,
            snapshot_worker_spawn_count: 9,
            tool_correlation_dropped: 10,
            classify_skipped: 11,
            connection_failures: 12,
            panics: 13,
            upstream_repo_filtered: 14,
            partial_snapshot_capacity_dropped: 15,
            snapshot_oversized_dropped: 16,
            history_oversized_dropped: 17,
            replay_oversized_dropped: 18,
            keepalive_pings_sent: 19,
            disconnect_config_stale: 20,
            disconnect_closed_by_peer: 21,
            disconnect_error: 22,
            last_disconnect_reason: "read failed: redacted diagnostic".to_owned(),
        }
    );
}

#[test]
fn remote_config_stale_guard_backs_off_third_fast_exit_and_climbs_normally() {
    let started_at = Instant::now();
    let mut guard = ConfigStaleBackoffGuard::default();

    assert_eq!(
        guard.delay_attempt(started_at, Duration::from_secs(2)),
        None
    );
    assert_eq!(
        guard.delay_attempt(started_at + Duration::from_secs(5), Duration::from_secs(2)),
        None
    );
    assert_eq!(
        guard.delay_attempt(started_at + Duration::from_secs(10), Duration::from_secs(2)),
        Some(0)
    );
    assert_eq!(backoff_delay(0), Duration::from_secs(1));
    assert_eq!(
        guard.delay_attempt(started_at + Duration::from_secs(15), Duration::from_secs(2)),
        Some(1)
    );
    assert_eq!(backoff_delay(1), Duration::from_secs(2));
}

#[test]
fn remote_connect_loop_real_error_starts_at_first_backoff_after_config_stale_guard() {
    let inner = test_inner(|_| None, || None);
    lock(&inner.registry).enqueue_token_put(
        TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 8,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_430_700_000,
                refresh_until: None,
            },
            prev: None,
        },
        None,
    );
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let mut attempts = VecDeque::from([
        ConnectAttempt::Ran {
            token_for_redact: None,
            result: Ok(ConnectionExit::ConfigStale {
                connected_for: Duration::from_secs(1),
            }),
        },
        ConnectAttempt::Ran {
            token_for_redact: None,
            result: Ok(ConnectionExit::ConfigStale {
                connected_for: Duration::from_secs(1),
            }),
        },
        ConnectAttempt::Ran {
            token_for_redact: None,
            result: Ok(ConnectionExit::ConfigStale {
                connected_for: Duration::from_secs(1),
            }),
        },
        ConnectAttempt::Ran {
            token_for_redact: None,
            result: Err(ConnectionFailure::Other("real connection error".to_owned())),
        },
    ]);
    let mut completed_delays = Vec::new();

    connect_loop_with(
        Arc::downgrade(&inner),
        upstream_rx,
        milestone_rx,
        |_, _, _| {
            attempts
                .pop_front()
                .expect("unexpected extra connection attempt")
        },
        |inner, delay| {
            if inner.registry_publish_wake.load(Ordering::Acquire) {
                return false;
            }
            completed_delays.push(delay);
            completed_delays.len() == 2
        },
        |_| panic!("the test sequence must not enter terminal wait"),
    );

    assert_eq!(
        completed_delays,
        vec![Duration::from_secs(1), Duration::from_secs(1)],
        "an unacked outbox must not interrupt either the guard backoff or the following real-error backoff"
    );
    assert!(!inner.registry_publish_wake.load(Ordering::Acquire));
    assert_eq!(inner.state.connection_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn remote_disconnect_status_counts_categories_and_redacts_error_reason() {
    let inner = test_inner(|_| None, || None);
    let secret = "ab".repeat(32);
    let mut failed_attempts = 0;

    record_disconnect(&inner.state, DisconnectKind::ConfigStale, "config_stale");
    record_disconnect(&inner.state, DisconnectKind::ClosedByPeer, "closed_by_peer");
    record_failure(
        &inner,
        FailureKind::Connection,
        format!("read failed: token={secret}"),
        Some(&secret),
        &mut failed_attempts,
    );

    let counters = gateway_status_from_state(&inner.state).counters;
    assert_eq!(counters.disconnect_config_stale, 1);
    assert_eq!(counters.disconnect_closed_by_peer, 1);
    assert_eq!(counters.disconnect_error, 1);
    assert!(counters.last_disconnect_reason.starts_with("read failed:"));
    assert!(!counters.last_disconnect_reason.contains(&secret));
}

#[test]
fn remote_config_stale_guard_resets_after_stable_connection_or_other_exit() {
    let started_at = Instant::now();
    let mut guard = ConfigStaleBackoffGuard::default();

    for seconds in [0, 5, 10] {
        let _ = guard.delay_attempt(
            started_at + Duration::from_secs(seconds),
            Duration::from_secs(2),
        );
    }
    assert_eq!(guard.backoff_attempts, 1);
    assert_eq!(
        guard.delay_attempt(
            started_at + Duration::from_secs(15),
            CONFIG_STALE_GUARD_WINDOW,
        ),
        None
    );
    assert_eq!(guard.backoff_attempts, 0);

    assert_eq!(
        guard.delay_attempt(started_at + Duration::from_secs(20), Duration::from_secs(2)),
        None
    );
    guard.reset();
    assert_eq!(
        guard.delay_attempt(started_at + Duration::from_secs(21), Duration::from_secs(2)),
        None
    );
}

#[test]
fn remote_failed_registry_drain_keeps_publish_wake_set() {
    let inner = test_inner(|_| None, || None);
    lock(&inner.registry).enqueue_token_put(
        TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 8,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_430_700_000,
                refresh_until: None,
            },
            prev: None,
        },
        None,
    );
    inner.registry_publish_wake.store(true, Ordering::Release);

    let result = drain_registry_outbox_with(&inner, |_| Err::<(), _>("write failed"));
    assert_eq!(result, Err("write failed"));
    assert!(inner.registry_publish_wake.load(Ordering::Acquire));
}

#[test]
fn remote_retryable_exit_with_unacked_outbox_keeps_wake_cold_and_backs_off() {
    let inner = test_inner(|_| None, || None);
    lock(&inner.registry).enqueue_token_put(
        TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 8,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_430_700_000,
                refresh_until: None,
            },
            prev: None,
        },
        None,
    );
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let mut attempts = 0;
    let mut wake_during_backoff = Vec::new();
    let mut completed_delays = Vec::new();

    connect_loop_with(
        Arc::downgrade(&inner),
        upstream_rx,
        milestone_rx,
        |inner, _, _| {
            attempts += 1;
            if attempts == 1 {
                ConnectAttempt::Ran {
                    token_for_redact: None,
                    result: Ok(ConnectionExit::ClosedByPeer),
                }
            } else {
                inner.shutdown.store(true, Ordering::Release);
                ConnectAttempt::Waiting
            }
        },
        |inner, delay| {
            let wake = inner.registry_publish_wake.load(Ordering::Acquire);
            wake_during_backoff.push(wake);
            if wake {
                return false;
            }
            completed_delays.push(delay);
            false
        },
        |_| panic!("a retryable exit must not enter terminal wait"),
    );

    assert_eq!(
        attempts, 2,
        "the retry must happen after one completed backoff"
    );
    assert_eq!(wake_during_backoff, vec![false]);
    assert_eq!(completed_delays, vec![Duration::from_secs(1)]);
    assert!(!inner.registry_publish_wake.load(Ordering::Acquire));
}

#[test]
fn remote_registry_publish_wake_clears_after_complete_drain_without_empty_redrain() {
    let inner = test_inner(|_| None, || None);
    lock(&inner.registry).enqueue_token_put(
        TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 8,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_430_700_000,
                refresh_until: None,
            },
            prev: None,
        },
        None,
    );
    inner.registry_publish_wake.store(true, Ordering::Release);
    let mut sent = 0;

    drain_registry_outbox_with(&inner, |_| {
        sent += 1;
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(sent, 1);
    assert!(!inner.registry_publish_wake.load(Ordering::Acquire));

    drain_registry_outbox_with(&inner, |_| {
        sent += 1;
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(
        sent, 1,
        "a completed drain must not resend or keep the wake latch hot"
    );
    assert!(!inner.registry_publish_wake.load(Ordering::Acquire));
}

#[test]
fn remote_keepalive_sends_one_ping_after_quiet_timeout_rounds_reach_idle_threshold() {
    let state = GatewayInnerState::default();
    let started_at = Instant::now();
    let mut idle = KeepaliveIdle::new(started_at);
    let mut sent = Vec::new();

    for round in 1..=60 {
        let now = started_at + Duration::from_millis(500 * round);
        idle.send_ping_if_due(now, &state, |message| {
            sent.push(message);
            Ok::<_, ()>(())
        })
        .unwrap();
    }

    assert_eq!(sent, vec![Message::Ping(Vec::new().into())]);
    assert_eq!(state.keepalive_pings_sent.load(Ordering::Relaxed), 1);
}

#[test]
fn remote_keepalive_does_not_ping_while_business_frames_refresh_idle_time() {
    let state = GatewayInnerState::default();
    let started_at = Instant::now();
    let mut idle = KeepaliveIdle::new(started_at);
    let mut sent = Vec::new();

    for seconds in [10, 20, 29, 39, 49, 58] {
        idle.record_activity(started_at + Duration::from_secs(seconds));
        idle.send_ping_if_due(
            started_at + Duration::from_secs(seconds + 1),
            &state,
            |message| {
                sent.push(message);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    }

    assert!(sent.is_empty());
    assert_eq!(state.keepalive_pings_sent.load(Ordering::Relaxed), 0);
}

#[test]
fn remote_keepalive_sends_at_most_one_ping_per_consecutive_idle_window() {
    let state = GatewayInnerState::default();
    let started_at = Instant::now();
    let mut idle = KeepaliveIdle::new(started_at);
    let mut sent = Vec::new();

    for seconds in [30, 31, 59, 60, 61, 89, 90] {
        idle.send_ping_if_due(
            started_at + Duration::from_secs(seconds),
            &state,
            |message| {
                sent.push(message);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    }

    assert_eq!(
        sent,
        vec![
            Message::Ping(Vec::new().into()),
            Message::Ping(Vec::new().into()),
            Message::Ping(Vec::new().into()),
        ]
    );
    assert_eq!(state.keepalive_pings_sent.load(Ordering::Relaxed), 3);
}
