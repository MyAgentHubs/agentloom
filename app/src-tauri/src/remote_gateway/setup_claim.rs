use super::*;
pub(crate) fn setup(
    settings: SettingsReader,
    token_provider: TokenProvider,
    desktop_credential_provider: DesktopCredentialProvider,
    claim_client: ClaimClient,
    active_device_provider: ActiveDeviceProvider,
    active_room_resolver: ActiveRoomResolver,
    k_room_provider: KRoomProvider,
    session_index_snapshot_provider: SessionIndexSnapshotProvider,
    milestone_replay_provider: MilestoneReplayProvider,
    session_runtime_replay_provider: SessionRuntimeReplayProvider,
    pair_hello_handler: PairHelloHandler,
    pair_done_handler: PairDoneHandler,
    registry: Arc<Mutex<RegistryState>>,
    registry_snapshot_provider: RegistrySnapshotProvider,
    registry_rebase_provider: RegistryRebaseProvider,
    registry_high_water_provider: RegistryHighWaterProvider,
    refresh_handler: RefreshHandler,
    input_send_handler: InputSendHandler,
    input_answer_handler: InputAnswerHandler,
    control_replay_handler: ControlReplayHandler,
    control_stop_handler: ControlStopHandler,
    session_repo_provider: SessionRepoProvider,
    session_history_provider: SessionHistoryProvider,
    message_fetch_provider: MessageFetchProvider,
) {
    if GATEWAY.get().is_some() {
        return;
    }

    let (upstream_tx, upstream_rx) = mpsc::sync_channel::<(u64, LiveQueueItem)>(UPSTREAM_CAPACITY);
    let (milestone_tx, milestone_rx) =
        mpsc::sync_channel::<(u64, MilestoneItem)>(UPSTREAM_CAPACITY);
    let inner = Arc::new(Inner {
        settings,
        token_provider,
        desktop_credential_provider,
        claim_client,
        active_device_provider,
        active_room_resolver,
        k_room_provider,
        session_index_snapshot_provider,
        milestone_replay_provider,
        session_runtime_replay_provider,
        pair_hello_handler,
        pair_done_handler,
        registry,
        registry_snapshot_provider,
        registry_rebase_provider,
        registry_high_water_provider,
        refresh_handler,
        input_send_handler,
        input_answer_handler,
        control_replay_handler,
        control_stop_handler,
        session_repo_provider,
        session_history_provider,
        message_fetch_provider,
        upstream_tx,
        milestone_tx,
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    });
    if GATEWAY.set(Arc::clone(&inner)).is_err() {
        return;
    }
    install_panic_hook();

    let weak_inner = Arc::downgrade(&inner);
    thread::Builder::new()
        .name("remote-gateway".into())
        .spawn(move || connect_loop(weak_inner, upstream_rx, milestone_rx))
        .expect("failed to start remote gateway thread");
}

pub(crate) fn parse_config(
    enabled_raw: Option<&str>,
    relay_url_raw: Option<&str>,
    room_id_raw: Option<&str>,
    active_repo_id: Option<String>,
) -> (bool, Option<GatewayConfig>) {
    let enabled = enabled_raw == Some("true");
    let normalized_room_id = room_id_raw.map(str::to_lowercase);
    let config = match (relay_url_raw, normalized_room_id) {
        (Some(relay_url), Some(room_id)) if !relay_url.is_empty() && is_valid_room_id(&room_id) => {
            Some(GatewayConfig {
                relay_url: relay_url.to_owned(),
                room_id,
                active_repo_id,
            })
        }
        _ => None,
    };
    (enabled, config)
}

pub(super) fn is_valid_room_id(room_id: &str) -> bool {
    room_id.len() == 32
        && room_id
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, 'a'..='f'))
}

// Backdoor retirement: desktop identifies itself exclusively via the `Authorization: Bearer`
// header and never follows redirects, so the legacy `?role=desktop`/`&token=` query
// params are pure dead weight now — worse, `&token=` used to carry the plaintext dev token
// straight into CF edge logs. The relay no longer honors either param either
// (the relay's room admission handler dropped the legacy admission path in this same batch).
pub(crate) fn build_ws_url(base: &str, room: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/room/{room}")
}

pub(super) fn build_ws_request(
    url: &str,
    credential: &DesktopCredential,
) -> Result<tungstenite::handshake::client::Request, String> {
    let mut request = url
        .into_client_request()
        .map_err(|error| format!("invalid relay URL: {error}"))?;
    let authorization =
        HeaderValue::from_str(&format!("Bearer {}", credential.expose())).map_err(|_| {
            "desktop credential cannot be encoded as an Authorization header".to_owned()
        })?;
    request.headers_mut().insert(AUTHORIZATION, authorization);
    Ok(request)
}

pub(crate) fn claim_room_blocking(
    relay_url: &str,
    room_id: &str,
    credential_hash: &str,
) -> Result<ClaimResponse, String> {
    let http_base = relay_url
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            relay_url
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .ok_or_else(|| "relay URL must use ws:// or wss://".to_owned())?;
    let url = format!("{}/room/{room_id}/claim", http_base.trim_end_matches('/'));
    let body = serde_json::json!({"v": 1, "credential_hash": credential_hash}).to_string();
    if body.len() > 1024 {
        return Err("room claim request body exceeds 1KB".to_owned());
    }
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("claim client setup failed: {error}"))?;
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .map_err(|error| format!("claim request failed: {error}"))?;
    match response.status().as_u16() {
        200 => Ok(ClaimResponse::Claimed),
        409 => Ok(ClaimResponse::Conflict),
        410 => Ok(ClaimResponse::Tombstoned),
        429 => Ok(ClaimResponse::RateLimited),
        status @ 300..=399 => Err(format!("claim redirect refused (HTTP {status})")),
        status => Err(format!("claim returned unexpected HTTP {status}")),
    }
}

pub(super) fn redact(input: &str, token: Option<&str>) -> String {
    let mut redacted = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(offset) = input[cursor..].find("token=") {
        let value_start = cursor + offset + "token=".len();
        redacted.push_str(&input[cursor..value_start]);
        redacted.push_str("***");

        match input[value_start..].find('&') {
            Some(value_len) => cursor = value_start + value_len,
            None => {
                cursor = input.len();
                break;
            }
        }
    }
    redacted.push_str(&input[cursor..]);

    // The query `token=` pass above only covers the retired `?token=` leak vector.
    // Now that backdoor is gone, `Authorization: Bearer <credential>` is the desktop's only
    // credential channel, and `Sec-WebSocket-Protocol: agentloom-rc-v1, token.<hex>` is the
    // remote scope's — scrub both value shapes wherever they appear in a text blob (e.g. a
    // Debug-formatted request/headers dump ending up in a panic or connection-failure
    // message), not just in URL query strings. No known leak path emits either header into
    // such a string today (a log-hygiene assertion covers that), but once query tokens
    // are gone these two are the only remaining credential shapes worth a standing defense.
    let redacted = scrub_after_marker(&redacted, "Bearer ");
    let redacted = scrub_after_marker(&redacted, "token.");

    match token {
        Some(token) if !token.is_empty() && redacted.contains(token) => {
            redacted.replace(token, "***")
        }
        _ => redacted,
    }
}

/// Both real credential shapes this guards are pure hex64 (256-bit CSPRNG desktop credential
/// / remote capability token) — `min_hex_len` should stay well under that, not at it, so
/// legitimate hex64 material is never missed by an off-by-a-little threshold.
const MIN_SCRUBBED_HEX_LEN: usize = 32;

/// Strips the opaque value following `marker` up to the first non-hex-digit character, but
/// only when that run is at least `MIN_SCRUBBED_HEX_LEN` characters long. Without this floor,
/// `scrub_after_marker` clobbers unrelated short hex-*looking* runs that happen to follow a
/// marker string — `t=token.ack` becomes `t=token.***k` (this function's own diagnostic
/// string, `read_registry_sync_ack`'s frame-type detail, self-inflicted damage),
/// and English prose after `Bearer ` (e.g. `Bearer bad`, all-hex-alphabet words) gets
/// needlessly mangled too. Both real credential shapes this guards are pure hex64, so a
/// 32-character floor can't miss genuine credential material while it stops these short,
/// non-credential false positives.
fn scrub_after_marker(input: &str, marker: &str) -> String {
    let mut redacted = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(offset) = input[cursor..].find(marker) {
        let value_start = cursor + offset + marker.len();
        let value_len = input[value_start..]
            .find(|character: char| !character.is_ascii_hexdigit())
            .unwrap_or(input.len() - value_start);
        redacted.push_str(&input[cursor..value_start]);
        if value_len >= MIN_SCRUBBED_HEX_LEN {
            redacted.push_str("***");
        } else {
            // Below the floor: not credential-shaped enough to risk clobbering — leave the
            // run as-is (e.g. `token.ack`'s `ack`, `token.refresh_failed`'s `refresh_failed`).
            redacted.push_str(&input[value_start..value_start + value_len]);
        }
        cursor = value_start + value_len;
    }
    redacted.push_str(&input[cursor..]);
    redacted
}

pub(super) fn registry_ack_reason_for_log(reason: Option<&str>) -> String {
    let reason = reason.unwrap_or("unspecified");
    let safe_shape = reason.len() <= 96
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    let contains_hash_like_run = reason
        .as_bytes()
        .windows(64)
        .any(|window| window.iter().all(u8::is_ascii_hexdigit));
    if safe_shape && !contains_hash_like_run {
        reason.to_owned()
    } else {
        "redacted".to_owned()
    }
}

pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let seconds = (1_u64 << attempt.min(6)).min(60);
    Duration::from_secs(seconds)
}

#[derive(Default)]
pub(super) struct ConfigStaleBackoffGuard {
    consecutive_fast_exits: u32,
    pub(super) backoff_attempts: u32,
    last_config_stale_at: Option<Instant>,
}

impl ConfigStaleBackoffGuard {
    pub(super) fn delay_attempt(&mut self, now: Instant, connected_for: Duration) -> Option<u32> {
        let previous_was_recent = self.last_config_stale_at.is_some_and(|previous| {
            now.saturating_duration_since(previous) < CONFIG_STALE_GUARD_WINDOW
        });
        if connected_for >= CONFIG_STALE_GUARD_WINDOW || !previous_was_recent {
            self.consecutive_fast_exits = 0;
            self.backoff_attempts = 0;
        }

        self.consecutive_fast_exits = self.consecutive_fast_exits.saturating_add(1);
        self.last_config_stale_at = Some(now);
        if self.consecutive_fast_exits < CONFIG_STALE_GUARD_THRESHOLD {
            return None;
        }

        let attempt = self.backoff_attempts;
        self.backoff_attempts = self.backoff_attempts.saturating_add(1);
        Some(attempt)
    }

    pub(super) fn reset(&mut self) {
        self.consecutive_fast_exits = 0;
        self.backoff_attempts = 0;
        self.last_config_stale_at = None;
    }
}
