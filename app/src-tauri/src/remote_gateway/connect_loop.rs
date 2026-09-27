use super::*;

pub(super) fn connect_loop(
    inner: Weak<Inner>,
    upstream_rx: Receiver<(u64, LiveQueueItem)>,
    milestone_rx: Receiver<(u64, MilestoneItem)>,
) {
    connect_loop_with(
        inner,
        upstream_rx,
        milestone_rx,
        attempt_once,
        interruptible_sleep,
        wait_for_reload,
    );
}

pub(super) fn connect_loop_with(
    inner: Weak<Inner>,
    upstream_rx: Receiver<(u64, LiveQueueItem)>,
    milestone_rx: Receiver<(u64, MilestoneItem)>,
    mut attempt_connection: impl FnMut(
        &Arc<Inner>,
        &Receiver<(u64, LiveQueueItem)>,
        &Receiver<(u64, MilestoneItem)>,
    ) -> ConnectAttempt,
    mut sleep: impl FnMut(&Inner, Duration) -> bool,
    mut wait: impl FnMut(&Inner) -> bool,
) {
    let mut failed_attempts = 0_u32;
    let mut config_stale_guard = ConfigStaleBackoffGuard::default();

    loop {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        if inner.shutdown.load(Ordering::Acquire) {
            return;
        }
        if inner.reload_requested.swap(false, Ordering::AcqRel) {
            failed_attempts = 0;
            config_stale_guard.reset();
            set_status(&inner.state, GatewayState::Waiting, None);
        } else if inner.registry_publish_wake.swap(false, Ordering::AcqRel) {
            failed_attempts = 0;
            set_status(&inner.state, GatewayState::Waiting, None);
        }

        let attempt = catch_unwind(AssertUnwindSafe(|| {
            attempt_connection(&inner, &upstream_rx, &milestone_rx)
        }));
        if inner.shutdown.load(Ordering::Acquire) {
            return;
        }

        let delay_attempt = match attempt {
            Ok(ConnectAttempt::Disabled) => {
                config_stale_guard.reset();
                set_status(&inner.state, GatewayState::Disabled, None);
                if sleep(&inner, SETTINGS_POLL_INTERVAL) {
                    return;
                }
                continue;
            }
            Ok(ConnectAttempt::Waiting) => {
                config_stale_guard.reset();
                set_status(&inner.state, GatewayState::Waiting, None);
                if sleep(&inner, SETTINGS_POLL_INTERVAL) {
                    return;
                }
                continue;
            }
            Ok(ConnectAttempt::Ran {
                result: Ok(ConnectionExit::ClosedByPeer),
                ..
            }) => {
                config_stale_guard.reset();
                record_disconnect(&inner.state, DisconnectKind::ClosedByPeer, "closed_by_peer");
                failed_attempts = 0;
                set_status(&inner.state, GatewayState::Backoff, None);
                0
            }
            Ok(ConnectAttempt::Ran {
                result: Ok(ConnectionExit::ConfigStale { connected_for }),
                ..
            }) => {
                record_disconnect(&inner.state, DisconnectKind::ConfigStale, "config_stale");
                let Some(delay_attempt) =
                    config_stale_guard.delay_attempt(Instant::now(), connected_for)
                else {
                    continue;
                };
                set_status(&inner.state, GatewayState::Backoff, None);
                delay_attempt
            }
            Ok(ConnectAttempt::Ran {
                result: Ok(ConnectionExit::PairingReloadRequested),
                ..
            }) => {
                config_stale_guard.reset();
                failed_attempts = 0;
                continue;
            }
            Ok(ConnectAttempt::Stopped(reason)) => {
                config_stale_guard.reset();
                let redacted = redact(&reason.message, None);
                eprintln!("remote gateway stopped: {redacted}");
                set_stopped_status(&inner.state, redacted, Some(reason.code.to_owned()));
                if wait(&inner) {
                    return;
                }
                continue;
            }
            Ok(ConnectAttempt::Ran {
                token_for_redact,
                result: Err(error),
            }) => {
                config_stale_guard.reset();
                record_failure(
                    &inner,
                    FailureKind::Connection,
                    error.to_string(),
                    token_for_redact.as_ref().map(SecretToken::expose),
                    &mut failed_attempts,
                )
            }
            Err(payload) => {
                config_stale_guard.reset();
                let token_literal = take_active_token(&inner);
                record_failure(
                    &inner,
                    FailureKind::Panic,
                    panic_message(payload),
                    token_literal.as_deref(),
                    &mut failed_attempts,
                )
            }
        };

        if sleep(&inner, backoff_delay(delay_attempt)) {
            return;
        }
    }
}

#[derive(Debug)]
pub(super) enum ConnectAttempt {
    Disabled,
    Waiting,
    Stopped(StopReason),
    Ran {
        token_for_redact: Option<SecretToken>,
        result: Result<ConnectionExit, ConnectionFailure>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ConnectionFailure {
    Unauthorized,
    Tombstoned,
    Stopped(StopReason),
    Other(String),
}

impl std::fmt::Display for ConnectionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("relay rejected desktop authorization (401)"),
            Self::Tombstoned => formatter.write_str("relay room is tombstoned (410)"),
            Self::Stopped(reason) => formatter.write_str(&reason.message),
            Self::Other(error) => formatter.write_str(error),
        }
    }
}

impl From<String> for ConnectionFailure {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnectionExit {
    ClosedByPeer,
    ConfigStale { connected_for: Duration },
    PairingReloadRequested,
}

#[derive(Clone, Copy)]
pub(super) enum DisconnectKind {
    ConfigStale,
    ClosedByPeer,
    Error,
}

#[derive(Clone, Copy)]
pub(super) enum FailureKind {
    Connection,
    Panic,
}

/// Under the single-active-room design, configuration resolution has two states. The legacy
/// global `remote_room_id` fallback has been removed: it never existed on mobile, there are no
/// real pairings to preserve, and the migration policy explicitly leaves the stored data intact.
/// 1. When `remote_active_repo_id` is set and remote control is enabled, use
///    `active_room_resolver` to ensure the project's room (reuse an existing room or create one),
///    ensure credentials idempotently first, and use that room.
/// 2. Otherwise, when active is unset or blank, remote control is disabled, or resolution fails,
///    `room_id_raw` remains `None` and `parse_config` follows the existing unconfigured branch,
///    leaving the gateway Waiting without connecting. The gateway no longer reads the
///    `remote_room_id` app setting; its stored data is not migrated or modified.
///
/// Active resolution failures, including DB errors and exhausted
/// `ensure_remote_room_for_project` retries, use state 2: no configuration is currently
/// available, the next resolution attempt may recover, only a log is emitted, and no global room
/// fallback is used.
///
/// `attempt_once`, before a real connection attempt, and the connected-state liveness poll, once
/// per `liveness_interval`, share this logic. The production `active_room_resolver` caches rooms
/// whose credentials have already been confirmed, so repeated steady-state calls do not access
/// the keychain repeatedly; see the `remote_gateway_active_room_resolver` documentation.
///
/// Once `GatewayConfig` is constructed, `active_repo_id` is always `Some`, because only a
/// successful active branch reaches `parse_config`'s configured branch. The `RoomSource` enum and
/// its branch deciding whether a claim conflict may automatically change rooms were removed with
/// the legacy fallback. For a conflict where no devices are found, `ensure_claim` now always
/// stops directly and never enters a room-change path.
pub(super) fn current_config(inner: &Inner) -> (bool, Option<GatewayConfig>) {
    let enabled_raw = (inner.settings)("remote_control_enabled");
    // Fall back to the official public relay when the relay address is absent or blank; see the
    // `effective_relay_url` documentation.
    let relay_url_raw = effective_relay_url((inner.settings)("remote_relay_url"));
    let enabled = enabled_raw.as_deref() == Some("true");
    let active_repo_id_raw = (inner.settings)("remote_active_repo_id");
    // Trim on read, symmetrically with the writer's trim-and-filter behavior in
    // `remote_set_active_project_in_conn`; neither side treats whitespace-only values as set. The
    // trimmed value, rather than the original untrimmed string, is also passed to
    // `active_room_resolver`.
    let active_repo_id = active_repo_id_raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let (room_id_raw, config_active_repo_id): (Option<String>, Option<String>) =
        match active_repo_id {
            Some(project_id) if enabled => match (inner.active_room_resolver)(project_id) {
                Ok(room_id) => (Some(room_id), Some(project_id.to_owned())),
                Err(error) => {
                    eprintln!(
                        "remote gateway: active room resolution failed for project \
                         {project_id}: {error}"
                    );
                    // Resolution failed and there is no longer a legacy fallback. With a `None`
                    // room ID, `parse_config` follows the existing unconfigured branch and never
                    // constructs a `GatewayConfig`.
                    (None, None)
                }
            },
            // An unset or blank active project, or disabled remote control, is directly treated
            // as unconfigured without reading any global room setting.
            _ => (None, None),
        };

    parse_config(
        enabled_raw.as_deref(),
        relay_url_raw.as_deref(),
        room_id_raw.as_deref(),
        config_active_repo_id,
    )
}

pub(super) fn attempt_once(
    inner: &Arc<Inner>,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
) -> ConnectAttempt {
    let (enabled, config) = current_config(inner);
    if !enabled {
        inner.state.disable_upstream_gate();
        return ConnectAttempt::Disabled;
    }
    let Some(config) = config else {
        inner.state.disable_upstream_gate();
        return ConnectAttempt::Waiting;
    };

    set_status(&inner.state, GatewayState::Connecting, None);
    // Retired backdoor: `token_provider` used to read the dev-only `remote_dev_token`
    // app-setting used as an interop placeholder — that read is gone (lib.rs wires a
    // `|| None` stub here now), so this always evaluates to `None`. The plumbing itself
    // (SecretToken/active_token/evaluate_connection_liveness's token comparison) stays:
    // it is the generic "credential material rotated mid-connection → force reconnect"
    // path, not specific to the retired dev token.
    let token = (inner.token_provider)().map(SecretToken::new);
    set_active_token(inner, token.as_ref());
    let credential = match (inner.desktop_credential_provider)(&config.room_id) {
        Ok(credential) => DesktopCredential::new(credential),
        Err(error) => {
            clear_active_token(inner);
            return ConnectAttempt::Ran {
                token_for_redact: token,
                result: Err(ConnectionFailure::Other(format!(
                    "desktop credential unavailable: {error}"
                ))),
            };
        }
    };
    let k_room = (inner.k_room_provider)(&config.room_id);
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let mut retried_after_claim = false;
    let result = loop {
        let result = run_authenticated_connection(
            inner,
            &url,
            &credential,
            &config,
            token.as_ref(),
            upstream_rx,
            milestone_rx,
            k_room.as_ref(),
        );
        match result {
            Err(ConnectionFailure::Tombstoned) => {
                clear_active_token(inner);
                return ConnectAttempt::Stopped(StopReason::new(
                    ROOM_TOMBSTONED_STOP_REASON,
                    ROOM_TOMBSTONED_STOP_ERROR,
                ));
            }
            Err(ConnectionFailure::Unauthorized) if !retried_after_claim => {}
            Err(ConnectionFailure::Stopped(reason)) => {
                clear_active_token(inner);
                return ConnectAttempt::Stopped(reason);
            }
            result => break result,
        }

        retried_after_claim = true;
        match ensure_claim(inner, &config, &credential) {
            ClaimAction::Reconnect => continue,
            ClaimAction::Backoff(error) => break Err(ConnectionFailure::Other(error)),
            ClaimAction::Stop(error) => {
                clear_active_token(inner);
                return ConnectAttempt::Stopped(error);
            }
        }
    };
    clear_active_token(inner);
    ConnectAttempt::Ran {
        token_for_redact: token,
        result,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ClaimAction {
    Reconnect,
    Backoff(String),
    Stop(StopReason),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct StopReason {
    pub(super) code: &'static str,
    message: String,
}

impl StopReason {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub(super) fn ensure_claim(
    inner: &Inner,
    config: &GatewayConfig,
    credential: &DesktopCredential,
) -> ClaimAction {
    let credential_hash = crate::remote_pairing::desktop_credential_hash(credential.expose());
    let response = match (inner.claim_client)(&config.relay_url, &config.room_id, &credential_hash)
    {
        Ok(response) => response,
        Err(error) => return ClaimAction::Backoff(format!("room claim failed: {error}")),
    };

    match response {
        ClaimResponse::Claimed => ClaimAction::Reconnect,
        ClaimResponse::RateLimited => {
            ClaimAction::Backoff("room claim rate-limited (429)".to_owned())
        }
        ClaimResponse::Tombstoned => ClaimAction::Stop(StopReason::new(
            ROOM_TOMBSTONED_STOP_REASON,
            ROOM_TOMBSTONED_STOP_ERROR,
        )),
        // In the Conflict branch, both `Ok(true)`, meaning the room has known devices, and
        // `Ok(false)`, meaning no devices were found, now converge on an explicit fail-closed
        // Stop. The former preserves the existing behavior that protects paired devices from an
        // automatic room change. The latter follows the single-active-room model: a per-project
        // room cannot actually change because resolving it again returns the room bound to the
        // same project, so stopping and directing the user to Settings is preferable to an idle
        // retry loop. The legacy global-room recovery path for automatically changing rooms,
        // `room_regenerator`/`ClaimAction::RoomRegenerated`/`MAX_ROOM_REGENERATIONS`, was removed
        // with the legacy fallback. It served only this `Ok(false)` branch and had no other
        // trigger; Tombstoned and `Ok(true)` have always stopped directly.
        ClaimResponse::Conflict => match (inner.active_device_provider)(&config.room_id) {
            Err(error) => ClaimAction::Stop(StopReason::new(
                ROOM_DEVICE_STATUS_UNAVAILABLE_STOP_REASON,
                format!("无法确认当前房间的配对设备状态；为保护既有设备，远程控制已停机: {error}"),
            )),
            Ok(true) => ClaimAction::Stop(StopReason::new(
                ROOM_CLAIM_CONFLICT_STOP_REASON,
                ROOM_CLAIM_CONFLICT_STOP_ERROR,
            )),
            Ok(false) => ClaimAction::Stop(StopReason::new(
                ROOM_CLAIM_CONFLICT_PROJECT_STOP_REASON,
                ROOM_CLAIM_CONFLICT_PROJECT_STOP_ERROR,
            )),
        },
    }
}

pub(super) fn record_failure(
    inner: &Inner,
    kind: FailureKind,
    error: String,
    token: Option<&str>,
    failed_attempts: &mut u32,
) -> u32 {
    if matches!(kind, FailureKind::Panic) {
        inner.state.panics.fetch_add(1, Ordering::Relaxed);
    }
    inner
        .state
        .connection_failures
        .fetch_add(1, Ordering::Relaxed);

    let redacted = redact(&error, token);
    if matches!(kind, FailureKind::Connection) {
        record_disconnect(&inner.state, DisconnectKind::Error, &redacted);
    }
    match kind {
        FailureKind::Connection => eprintln!("remote gateway connection failed: {redacted}"),
        FailureKind::Panic => eprintln!("remote gateway connection panicked: {redacted}"),
    }
    set_status(&inner.state, GatewayState::Backoff, Some(redacted));

    let attempt = *failed_attempts;
    *failed_attempts = failed_attempts.saturating_add(1);
    attempt
}

pub(super) fn record_disconnect(state: &GatewayInnerState, kind: DisconnectKind, reason: &str) {
    let counter = match kind {
        DisconnectKind::ConfigStale => &state.disconnect_config_stale,
        DisconnectKind::ClosedByPeer => &state.disconnect_closed_by_peer,
        DisconnectKind::Error => &state.disconnect_error,
    };
    counter.fetch_add(1, Ordering::Relaxed);
    *lock(&state.last_disconnect_reason) = reason.to_owned();
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ConnectionDecision {
    Continue,
    Disconnect,
}

pub(super) fn evaluate_connection_liveness(
    current_enabled: bool,
    current_config: Option<&GatewayConfig>,
    connected_config: &GatewayConfig,
    current_token: Option<&SecretToken>,
    connected_token: Option<&SecretToken>,
    connected_k_room_available: bool,
    current_k_room_available: bool,
) -> ConnectionDecision {
    if !current_enabled || (!connected_k_room_available && current_k_room_available) {
        return ConnectionDecision::Disconnect;
    }
    match current_config {
        Some(config) if config == connected_config && current_token == connected_token => {
            ConnectionDecision::Continue
        }
        _ => ConnectionDecision::Disconnect,
    }
}
