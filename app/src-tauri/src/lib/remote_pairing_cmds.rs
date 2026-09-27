use super::*;

// ---------------------------------------------------------------------------------------
// T5d-a/T5e2 (remote control): pairing state machine, storage, and command wiring. All real K_room/K_pair key access goes through
// `remote_pairing::store` (the keychain), while the device list/token hashes go through `db::remote_devices` (the app data DB). The gateway callback
// only stages `AcceptOutcome` on hello; it persists the device and updates TokenBook only on done.
// ---------------------------------------------------------------------------------------

pub(super) fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// M2-4d: `current_config` in `remote_gateway.rs` no longer reads this app_settings key (the legacy global-room fallback was removed, and the
/// gateway accepts only the per-project room resolved from `remote_active_repo_id`). M24DR rework closeout: `remote_pairing_cancel_inner` and
/// `remote_device_revoke_inner` also now use `resolve_active_pairing_room_id_readonly` to obtain the current active room's generation (see that
/// function's documentation), rather than reading this legacy global room. Therefore, this constant and `resolve_remote_room_id` no longer have
/// callers on production paths and are covered only by their own test below (`resolve_remote_room_id_generates_once_and_reuses`). They are
/// deliberately retained: existing data in this app_settings row is not migrated or modified at all (a deliberate design decision), and deleting the
/// function that reads it would leave no code path explaining the origin of that existing data.
#[allow(dead_code)] // There are no callers on production paths (the M24DR rework removed the
                    // cancel/revoke dependency). This repository has precedents (`run_single_worker`, etc.); it is retained only for the
                    // `resolve_remote_room_id` test below and the historical readability of the existing app_settings row; see the documentation
                    // above.
pub(super) const REMOTE_ROOM_ID_SETTING: &str = "remote_room_id";

/// Obtain the current room ID (the legacy global room, covered only by its own test after the M24DR rework; see the `REMOTE_ROOM_ID_SETTING`
/// documentation above). Reuse the app_settings value if present; otherwise, generate a new value, write it back to app_settings, and return it.
/// Once generated, room_id is this desktop's permanent room number; the pairing flow does not change rooms each time, only the one-time
/// `pairing_token` changes.
///
/// M24DR rework closeout (the former M2-4d gap, now fixed): `remote_pairing_cancel_inner`/`remote_device_revoke_inner` used to call this function
/// for the legacy global room's generation counter, while `remote_pairing_begin` had already switched to the active project's per-project room. The
/// two sides obtained numbers for different rooms, so the identifier issued by `next_registry_generation` (persistently counted per room_id) did not
/// match the room to which the phone was actually connected. Relay CAS always rejected it and cancel/revoke silently failed (Blocker; see the
/// documentation for `resolve_active_pairing_room_id_readonly`, which both callers now use). Review determined that cancel/revoke must both obtain a
/// number for the current active room. It specifically rejected the originally proposed path where cancel uses the room_id carried by
/// `PairingSession` and revoke uses the device's own `remote_devices.room_id` column: that path would require a room-scoped outbox, and both review
/// tracks judged it over-engineered.
#[allow(dead_code)] // See the REMOTE_ROOM_ID_SETTING documentation: there are no production-path
                    // callers, and it is covered only by its own test.
pub(super) fn resolve_remote_room_id(conn: &Connection) -> Result<String, String> {
    if let Some(existing) =
        db::get_app_setting(conn, REMOTE_ROOM_ID_SETTING).map_err(|e| e.to_string())?
    {
        return Ok(existing);
    }
    let generated = remote_pairing::generate_room_id();
    db::set_app_setting(conn, REMOTE_ROOM_ID_SETTING, &generated).map_err(|e| e.to_string())?;
    Ok(generated)
}

/// Desktop-side in-process pairing-state slot. Only one Waiting or SentAccept value is allowed at a time. begin/cancel directly overwrites the
/// current value, thereby discarding any in-memory outcome that has not yet received done and never creating a device record for it.
pub(super) enum PairingSlot {
    Idle,
    Waiting(remote_pairing::PairingSession),
    SentAccept {
        outcome: remote_pairing::AcceptOutcome,
        room_id: String,
        sent_at_secs: u64,
        k_room: zeroize::Zeroizing<[u8; 32]>,
        origin_connection_id: String,
    },
    Done {
        room_id: String,
        device_id: String,
        completed_at_secs: u64,
    },
}

static PAIRING_SLOT: OnceLock<Mutex<PairingSlot>> = OnceLock::new();
static REMOTE_TOKEN_BOOK: OnceLock<Mutex<remote_pairing::TokenBook>> = OnceLock::new();
static REMOTE_REGISTRY: OnceLock<Arc<Mutex<remote_gateway::RegistryState>>> = OnceLock::new();
pub(super) const PAIR_ACCEPT_LIFETIME_SECS: u64 = remote_pairing::PAIRING_LIFETIME_SECS;

pub(super) fn pairing_slot() -> &'static Mutex<PairingSlot> {
    PAIRING_SLOT.get_or_init(|| Mutex::new(PairingSlot::Idle))
}

pub(super) fn remote_token_book() -> &'static Mutex<remote_pairing::TokenBook> {
    REMOTE_TOKEN_BOOK.get_or_init(|| Mutex::new(remote_pairing::TokenBook::new()))
}

pub(super) fn remote_registry() -> &'static Arc<Mutex<remote_gateway::RegistryState>> {
    REMOTE_REGISTRY.get_or_init(|| Arc::new(Mutex::new(remote_gateway::RegistryState::default())))
}

pub(super) fn initialize_remote_token_book(conn: &Connection) {
    let book = match remote_pairing::store::load_token_book(conn) {
        Ok(book) => book,
        Err(error) => {
            eprintln!(
                "load remote TokenBook failed due to database error; starting empty: {error}"
            );
            remote_pairing::TokenBook::new()
        }
    };
    let _ = REMOTE_TOKEN_BOOK.set(Mutex::new(book));
}

/// `PAIRING_SLOT` is held from state validation through the state write. In particular, the done
/// path also holds it across persistence and TokenBook insertion, so cancel/re-begin cannot slip
/// between a stale state check and device creation.
pub(super) fn process_pair_hello(
    slot: &Mutex<PairingSlot>,
    key_store: &dyn KeyStore,
    frame: remote_gateway::PairHelloFrame,
    now_secs: u64,
) -> Result<Option<remote_gateway::PairAcceptFrame>, String> {
    let mut slot = slot.lock().map_err(|e| e.to_string())?;
    let PairingSlot::Waiting(session) = &mut *slot else {
        return Ok(None);
    };
    if session.room_id != frame.room {
        return Ok(None);
    }

    let room_id = session.room_id.clone();
    let hello = remote_pairing::HelloFrame {
        remote_pub: frame.remote_pub,
        token_ct_b64: frame.token_ct,
        token_n_b64: frame.token_n,
    };
    let (outcome, k_room) =
        remote_pairing::remote_pairing_authenticate_hello(key_store, session, &hello, now_secs)?;
    let k_room = zeroize::Zeroizing::new(k_room);
    let (tokens_ct, tokens_n) = remote_pairing::seal_pair_accept_tokens(
        &outcome.device_record.k_pair,
        &room_id,
        &outcome.device_record.device_id,
        &outcome.capability_token,
        &outcome.refresh_token,
    );
    let accept = remote_gateway::PairAcceptFrame {
        room: room_id.clone(),
        device_id: outcome.device_record.device_id.clone(),
        k_room_ct: outcome.k_room_wrapped_ct.clone(),
        k_room_n: outcome.k_room_wrapped_n.clone(),
        tokens_ct,
        tokens_n,
        k_room: k_room.clone(),
    };
    *slot = PairingSlot::SentAccept {
        outcome,
        room_id,
        sent_at_secs: now_secs,
        k_room,
        origin_connection_id: frame.origin_connection_id,
    };
    Ok(Some(accept))
}

pub(super) fn process_pair_done_with_registry(
    slot: &Mutex<PairingSlot>,
    registry: &mut remote_gateway::RegistryState,
    conn: &Connection,
    key_store: &dyn KeyStore,
    token_book: &Mutex<remote_pairing::TokenBook>,
    frame: remote_gateway::PairDoneFrame,
    now_secs: u64,
    now_ms: u64,
) -> Result<remote_gateway::PairDoneAction, String> {
    let mut slot = slot.lock().map_err(|e| e.to_string())?;
    if let PairingSlot::Done {
        room_id,
        device_id,
        completed_at_secs,
    } = &*slot
    {
        if now_secs >= completed_at_secs.saturating_add(remote_pairing::PAIRING_LIFETIME_SECS) {
            *slot = PairingSlot::Idle;
            return Ok(remote_gateway::PairDoneAction::Rejected);
        }
        if frame.room != *room_id || frame.device_id != *device_id {
            return Ok(remote_gateway::PairDoneAction::Rejected);
        }
        let subject = format!("device:{device_id}");
        return Ok(match registry.replay_pair_ready(&subject) {
            Some(ready) => remote_gateway::PairDoneAction::Ready(ready),
            None => remote_gateway::PairDoneAction::Accepted {
                newly_paired_device_id: None,
            },
        });
    }
    let PairingSlot::SentAccept {
        outcome,
        room_id,
        sent_at_secs,
        k_room,
        origin_connection_id,
    } = &*slot
    else {
        return Ok(remote_gateway::PairDoneAction::Rejected);
    };
    if now_secs >= sent_at_secs.saturating_add(PAIR_ACCEPT_LIFETIME_SECS) {
        *slot = PairingSlot::Idle;
        return Ok(remote_gateway::PairDoneAction::Rejected);
    }
    if frame.room != *room_id
        || frame.device_id != outcome.device_record.device_id
        || frame.origin_connection_id != *origin_connection_id
    {
        return Ok(remote_gateway::PairDoneAction::Rejected);
    }
    let (Some(confirm_ct), Some(confirm_n)) =
        (frame.confirm_ct.as_deref(), frame.confirm_n.as_deref())
    else {
        return Ok(remote_gateway::PairDoneAction::Rejected);
    };
    if !remote_pairing::verify_pair_done_confirm(
        k_room,
        room_id,
        &outcome.device_record.device_id,
        confirm_ct,
        confirm_n,
    ) {
        return Ok(remote_gateway::PairDoneAction::Rejected);
    }

    let mut token_book = token_book.lock().map_err(|e| e.to_string())?;
    let generation = db::next_registry_generation(conn, room_id).map_err(|e| e.to_string())?;
    remote_pairing::store::persist_pairing_outcome(
        conn, key_store, room_id, outcome, now_secs, now_ms,
    )?;
    let device_id = outcome.device_record.device_id.clone();
    let access_expires_ms = remote_pairing::device_access_expires_at_ms(now_ms)?;
    let refresh_until_ms = remote_pairing::device_refresh_until_ms(now_ms)?;
    if !db::set_remote_device_registry(conn, &device_id, room_id, generation, refresh_until_ms)
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "paired remote device {device_id} disappeared before registry assignment"
        ));
    }
    token_book.insert(
        device_id.clone(),
        &outcome.capability_token,
        &outcome.refresh_token,
        now_ms,
    );
    let (ready_ct, ready_n) =
        remote_pairing::seal_pair_ready(&*outcome.device_record.k_pair, room_id, &device_id);
    let ready = remote_gateway::PairReadyFrame {
        room: room_id.clone(),
        device_id: device_id.clone(),
        ct: ready_ct,
        n: ready_n,
    };
    registry.enqueue_token_put(
        remote_gateway::TokenSyncEntry {
            subject: format!("device:{device_id}"),
            generation,
            scope: "remote".to_owned(),
            current: remote_gateway::TokenSyncCurrent {
                token_hash: outcome.device_record.token_hash.clone(),
                access_expires: access_expires_ms,
                refresh_until: Some(refresh_until_ms),
            },
            prev: None,
        },
        Some(ready),
    );
    *slot = PairingSlot::Done {
        room_id: room_id.clone(),
        device_id: device_id.clone(),
        completed_at_secs: now_secs,
    };
    Ok(remote_gateway::PairDoneAction::Accepted {
        newly_paired_device_id: Some(device_id),
    })
}

#[cfg(test)]
pub(super) fn process_pair_done(
    slot: &Mutex<PairingSlot>,
    conn: &Connection,
    key_store: &dyn KeyStore,
    token_book: &Mutex<remote_pairing::TokenBook>,
    frame: remote_gateway::PairDoneFrame,
    now_secs: u64,
    now_ms: u64,
) -> Result<Option<String>, String> {
    let mut registry = remote_gateway::RegistryState::default();
    match process_pair_done_with_registry(
        slot,
        &mut registry,
        conn,
        key_store,
        token_book,
        frame,
        now_secs,
        now_ms,
    )? {
        remote_gateway::PairDoneAction::Accepted {
            newly_paired_device_id,
        } => Ok(newly_paired_device_id),
        remote_gateway::PairDoneAction::Rejected | remote_gateway::PairDoneAction::Ready(_) => {
            Ok(None)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state")]
pub(super) enum RemotePairingStatus {
    Idle,
    WaitingForHello { expires_at: u64 },
    WaitingForDone { expires_at: u64 },
    Done { device_id: String },
}

/// Pure-function core (testable and independent of the global slot): Waiting expires according to the QR clock; SentAccept and Done are each
/// retained for five minutes from their respective accept/done times. Any expiration automatically degrades to Idle before mapping to IPC state.
pub(super) fn compute_pairing_status(slot: &mut PairingSlot, now_secs: u64) -> RemotePairingStatus {
    let expired = match slot {
        PairingSlot::Waiting(session) => now_secs >= session.expires_at_secs,
        PairingSlot::SentAccept { sent_at_secs, .. } => {
            now_secs >= sent_at_secs.saturating_add(PAIR_ACCEPT_LIFETIME_SECS)
        }
        PairingSlot::Done {
            completed_at_secs, ..
        } => now_secs >= completed_at_secs.saturating_add(remote_pairing::PAIRING_LIFETIME_SECS),
        PairingSlot::Idle => false,
    };
    if expired {
        *slot = PairingSlot::Idle;
    }
    match slot {
        PairingSlot::Idle => RemotePairingStatus::Idle,
        PairingSlot::Waiting(session) => RemotePairingStatus::WaitingForHello {
            expires_at: session.expires_at_secs,
        },
        PairingSlot::SentAccept { sent_at_secs, .. } => RemotePairingStatus::WaitingForDone {
            expires_at: sent_at_secs.saturating_add(PAIR_ACCEPT_LIFETIME_SECS),
        },
        PairingSlot::Done { device_id, .. } => RemotePairingStatus::Done {
            device_id: device_id.clone(),
        },
    }
}

/// M2-4d (seam P0, caught by both review tracks): pairing must use the current active project's per-project room and must no longer use the legacy
/// global `remote_room_id`. When an active project is set, the gateway's `current_config` connects to its room in `project_remote_rooms`. If the QR
/// code continued to contain the legacy room, the desktop gateway would never claim it, so pairing would always fail. This reuses M2-4a's
/// `db::ensure_remote_room_for_project` (reuse an existing room or create one when absent). Its decision semantics share the same source as the
/// gateway's `current_config`: first check `remote_control_enabled` (M24DR rework, aligning with the rule that `current_config` calls
/// the resolver only when an active project is set and remote control is enabled; when remote control is disabled, it must not incidentally create a
/// room and consume a generation), then trim and filter `remote_active_repo_id` (matching `remote_set_active_project_in_conn`/`current_config`;
/// whitespace alone does not count as set), and finally verify that the repo actually exists in the `repos` table (guarding against a manually
/// edited DB or a stale setting). Disabled remote control and an unset/blank active project share the same rejection path: neither is ready for
/// pairing, there is no dedicated "remote disabled" pairing error code, and no new copy is introduced. Since the user explicitly clicked Start
/// Pairing, they deserve an understandable error rather than a disappearing QR code. If the repo referenced by active does not exist, reuse the
/// `remoteControl.activeProjectMissing` error code already established by `remote_set_active_project_in_conn` for the same scenario, rather than
/// introducing new frontend copy; frontend i18n is outside this task's scope.
pub(super) fn resolve_active_pairing_room_id(conn: &Connection) -> Result<String, String> {
    let enabled = db::get_app_setting(conn, "remote_control_enabled")
        .map_err(|error| error.to_string())?
        .is_some_and(|value| value == "true");
    let active_repo_id_raw = db::get_app_setting(conn, REMOTE_ACTIVE_REPO_ID_SETTING)
        .map_err(|error| error.to_string())?;
    let active_repo_id = active_repo_id_raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let repo_id = match (enabled, active_repo_id) {
        (true, Some(repo_id)) => repo_id,
        _ => {
            return Err(ui_msg::al_err(
                "remoteControl.pairingNeedsActiveProject",
                &[],
            ))
        }
    };
    let exists = repos_repo::get_repo_by_id(conn, repo_id)
        .map_err(|error| error.to_string())?
        .is_some();
    if !exists {
        return Err(ui_msg::al_err(
            "remoteControl.activeProjectMissing",
            &[("repoId", repo_id.to_string())],
        ));
    }
    db::ensure_remote_room_for_project(conn, repo_id)
}

/// M24DR rework, item 1: read-only variant used by cancel/revoke to obtain the current active room's generation number. It shares semantics with
/// `resolve_active_pairing_room_id` (used by begin) but deliberately does not ensure a room. Blocker background:
/// `remote_pairing_cancel_inner`/`remote_device_revoke_inner` previously always called the legacy `resolve_remote_room_id` to obtain a number, while
/// `remote_pairing_begin` had already switched to the active project's per-project room. The two sides therefore used separate generation counters
/// (`db::next_registry_generation` persists counters by room_id). The `token.delete` enqueued by cancel/revoke carried the legacy room's necessarily
/// stale identifier into the active room to which the phone was actually connected. Relay CAS always rejected it (the `generation_too_low` family),
/// so revocation silently failed. Both review tracks determined that cancel/revoke must obtain a number for the current active room. They
/// specifically rejected the previously proposed path where cancel uses the room_id carried by `PairingSession` and revoke uses the device's own
/// `remote_devices.room_id` column: that path would require a room-scoped outbox, and both review tracks judged it over-engineered.
///
/// It does not ensure because cancel/revoke only want to know which room is currently connected and must not have the side effect of incidentally
/// creating a room and consuming a generation. In particular, if active points to a project that was just cleared or switched away, ensure would
/// create an unused orphan room from nothing. Semantics: first check `remote_control_enabled`, aligning with the decision rules of
/// `resolve_active_pairing_room_id` and the gateway's `current_config`. Disabled means unconfigured and follows the same unresolved path without a
/// new branch; return `Ok(None)` immediately without reading the active repo. Then read `remote_active_repo_id`, trim and filter it, returning
/// `Ok(None)` immediately if unset. Verify that the repo still exists in the `repos` table, returning `Ok(None)` if absent. Finally, read only an
/// existing `project_remote_rooms` row via `db::remote_room_for_project`; absence remains absence and does not create a room, so return `Ok(None)`.
/// The four empty-result scenarios (disabled, unset, deleted repo, or no room row) all mean the same thing to callers: the active room cannot be
/// resolved. Each caller decides how to fall back; see the documentation for `remote_pairing_cancel_inner`/`remote_device_revoke_inner`.
///
/// M24DR rework, DEVLIST rework item 1: after adding the `remote_control_enabled` check, all four empty-result scenarios follow the same path for
/// every consumer (the device list `remote_devices_list_in_conn`, `remote_pairing_cancel_inner`, and `remote_device_revoke_inner`). Semantic change:
/// when remote control is disabled, cancel/revoke do not enqueue the relay's `token.delete`, matching the behavior when the active project is unset;
/// see their respective documentation. Local state clearing/revocation still occurs as usual (DB revoke plus TokenBook invalidation, or
/// pairing-state cleanup); only the outbox delete is omitted. On the relay side, the existing omission mechanism in the `synchronize_registry`
/// reconciliation supplies the revocation the next time the connection is enabled. Both review tracks verified that fallback path; see the
/// reconciliation explanation in the `remote_device_revoke_inner` documentation.
pub(super) fn resolve_active_pairing_room_id_readonly(
    conn: &Connection,
) -> Result<Option<String>, String> {
    let enabled = db::get_app_setting(conn, "remote_control_enabled")
        .map_err(|error| error.to_string())?
        .is_some_and(|value| value == "true");
    if !enabled {
        return Ok(None);
    }
    let active_repo_id_raw = db::get_app_setting(conn, REMOTE_ACTIVE_REPO_ID_SETTING)
        .map_err(|error| error.to_string())?;
    let active_repo_id = active_repo_id_raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(repo_id) = active_repo_id else {
        return Ok(None);
    };
    let exists = repos_repo::get_repo_by_id(conn, repo_id)
        .map_err(|error| error.to_string())?
        .is_some();
    if !exists {
        return Ok(None);
    }
    db::remote_room_for_project(conn, repo_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub(super) fn remote_pairing_begin(
    db: State<Db>,
    relay_url: String,
) -> Result<remote_pairing::QrPayload, String> {
    // When the relay address is empty (unset or whitespace-only in the frontend), fall back to the official public relay, using the same defaulting
    // logic as the gateway connection side (`remote_gateway::current_config`); see the `effective_relay_url` documentation.
    let relay_url = remote_gateway::effective_relay_url(Some(relay_url))
        .expect("effective_relay_url always returns Some");
    let mut registry = remote_registry().lock().map_err(|e| e.to_string())?;
    let (room_id, generation) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let room_id = resolve_active_pairing_room_id(&conn)?;
        let generation =
            db::next_registry_generation(&conn, &room_id).map_err(|e| e.to_string())?;
        (room_id, generation)
    };
    let now_ms = now_unix_millis();
    let now_secs = now_ms / 1_000;
    let (session, qr_payload) =
        remote_pairing::PairingSession::begin(&relay_url, &room_id, now_secs);
    let token_hash = remote_pairing::pairing_connect_token_hash(&session.pairing_token)?;
    let access_expires_ms = remote_pairing::pairing_access_expires_at_ms(now_ms)?;

    let mut slot = pairing_slot().lock().map_err(|e| e.to_string())?;
    *slot = PairingSlot::Waiting(session);
    let entry = remote_gateway::TokenSyncEntry {
        subject: "pairing".to_owned(),
        generation,
        scope: "pairing".to_owned(),
        current: remote_gateway::TokenSyncCurrent {
            token_hash,
            access_expires: access_expires_ms,
            refresh_until: None,
        },
        prev: None,
    };
    registry.set_pairing_entry(entry.clone());
    registry.enqueue_token_put(entry, None);
    drop(slot);
    drop(registry);
    remote_gateway::request_registry_publish();
    Ok(qr_payload)
}

/// The `remote_pairing_cancel` command itself accepts Tauri `State<Db>`, making it awkward to call directly in unit tests. Following the
/// approach used by `remote_device_revoke_inner` in the S1h R5 rework, extract the pure logic that must occur atomically (clear pairing state,
/// obtain a number and enter the revoke channel, and return the slot to Idle) into a core function that accepts only already-unlocked references.
/// The command layer becomes a thin lock-taking wrapper that delegates to it.
///
/// M24DR rework, item 1: obtain the number via `resolve_active_pairing_room_id_readonly` for the current active room rather than the legacy
/// `resolve_remote_room_id`; see the former function's documentation for the Blocker. If the active room cannot be resolved (remote control
/// disabled, active project unset, repo deleted, or the project has no room row), local pairing state is still cleared normally:
/// `clear_pairing_entry`, `discard_staged_pairing_k_room`, and returning the slot to Idle are unaffected. However, do not enqueue the relay's
/// `token.delete`: there is no room to send it to, and that is no reason to reject local cleanup. The in-progress pairing token has a natural
/// 300-second QR-clock expiration fallback (`remote_pairing::PairingSession`), so the relay will not indefinitely consider the canceled pairing
/// valid.
pub(super) fn remote_pairing_cancel_inner(
    conn: &Connection,
    registry: &mut remote_gateway::RegistryState,
    slot: &mut PairingSlot,
) -> Result<(), String> {
    let generation = match resolve_active_pairing_room_id_readonly(conn)? {
        Some(room_id) => {
            Some(db::next_registry_generation(conn, &room_id).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    registry.clear_pairing_entry();
    registry.discard_staged_pairing_k_room();
    if let Some(generation) = generation {
        registry.enqueue_token_delete("pairing".to_owned(), generation, true);
    }
    *slot = PairingSlot::Idle;
    Ok(())
}

/// `remote_pairing_begin`/`remote_device_revoke` both wake the connection main loop
/// (`request_registry_publish()`) after releasing locks so newly enqueued outbox entries can be sent promptly even while stopped or backing off.
/// `remote_pairing_cancel` previously omitted this step. With a live connection, existing drain polling still covers the outbox within 500 ms, but
/// while stopped or backing off the `token.delete` enqueued by canceling pairing would remain blocked and unsent until the connection main loop woke
/// naturally. Add the wake-up here, keeping the same lock, mutate state, unlock, wake structure as the other two commands.
#[tauri::command]
pub(super) fn remote_pairing_cancel(db: State<Db>) -> Result<(), String> {
    let mut registry = remote_registry().lock().map_err(|e| e.to_string())?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut slot = pairing_slot().lock().map_err(|e| e.to_string())?;
    remote_pairing_cancel_inner(&conn, &mut registry, &mut slot)?;
    drop(slot);
    drop(conn);
    drop(registry);
    remote_gateway::request_registry_publish();
    Ok(())
}

#[tauri::command]
pub(super) fn remote_pairing_status() -> Result<RemotePairingStatus, String> {
    let mut registry = remote_registry().lock().map_err(|e| e.to_string())?;
    let mut slot = pairing_slot().lock().map_err(|e| e.to_string())?;
    let status = compute_pairing_status(&mut slot, now_unix_secs());
    if matches!(status, RemotePairingStatus::Idle) {
        registry.clear_pairing_entry();
        registry.discard_staged_pairing_k_room();
    }
    Ok(status)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct RemoteGatewayStatus {
    pub(super) running: bool,
    pub(super) stopped_reason: Option<String>,
    pub(super) last_error: Option<String>,
    pub(super) counters: remote_gateway::GatewayCounters,
}

pub(super) fn remote_gateway_status_view(
    status: remote_gateway::GatewayStatus,
) -> RemoteGatewayStatus {
    RemoteGatewayStatus {
        running: !matches!(status.state, remote_gateway::GatewayState::Disabled),
        stopped_reason: status.stopped_reason,
        last_error: status.last_error,
        counters: status.counters,
    }
}

#[tauri::command]
pub(super) fn remote_gateway_status() -> RemoteGatewayStatus {
    remote_gateway_status_view(remote_gateway::status())
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct RemoteDeviceView {
    pub(super) device_id: String,
    name: String,
    created_at: i64,
    /// Pass the DB's Unix millisecond value through IPC unchanged.
    access_expires_at: i64,
    revoked_at: Option<i64>,
}

impl From<db::RemoteDeviceRow> for RemoteDeviceView {
    fn from(row: db::RemoteDeviceRow) -> Self {
        // token_hash/refresh_hash are deliberately excluded from the IPC view. Even as hashes, there is no reason to pass them to the frontend (see
        // the same restraint in keychain.rs: "IPC must expose configured state only").
        Self {
            device_id: row.device_id,
            name: row.name,
            created_at: row.created_at,
            access_expires_at: row.access_expires_at,
            revoked_at: row.revoked_at,
        }
    }
}

/// M24D-DEVLIST: device room ownership. The list shows only devices in the current active project's room; devices in other rooms must be managed by
/// switching to the corresponding project. Resolve the room with `resolve_active_pairing_room_id_readonly` (do not ensure or consume a generation;
/// its semantics match cancel/revoke, as documented on that function). Return an empty list if it cannot be resolved (remote control disabled,
/// active project unset, repo deleted, or the project has no room row). With no active room to display, the UI must not list devices from other
/// rooms. Those devices could previously be revoked from the UI, but when revoke could not resolve the active room it did not enqueue the relay's
/// `token.delete`; revocation took effect only locally, and the UI falsely reported complete cleanup. See the `remote_device_revoke_inner`
/// documentation; closing exactly that gap is the purpose of this closeout. Filtering is performed at the lib.rs layer, leaving
/// `db::list_remote_devices`/db.rs unchanged. `db::list_remote_devices` remains an unfiltered query and is narrowed by `room_id` only here.
///
/// DEVLIST rework, item 1: after adding the `remote_control_enabled` check to `resolve_active_pairing_room_id_readonly`, this also produces an empty
/// list when remote control is disabled, aligning with the gateway's disabled-means-unconfigured semantics without a separate check.
pub(super) fn remote_devices_list_in_conn(
    conn: &Connection,
) -> Result<Vec<RemoteDeviceView>, String> {
    let Some(active_room_id) = resolve_active_pairing_room_id_readonly(conn)? else {
        return Ok(Vec::new());
    };
    let rows = db::list_remote_devices(conn).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter(|row| row.room_id.as_deref() == Some(active_room_id.as_str()))
        .map(RemoteDeviceView::from)
        .collect())
}

#[tauri::command]
pub(super) fn remote_devices_list(db: State<Db>) -> Result<Vec<RemoteDeviceView>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    remote_devices_list_in_conn(&conn)
}

/// S1h 2a: when revoking a device, in addition to the existing DB revoke and TokenBook invalidation, obtain a number for `token.delete`
/// (`close:true`) and place it in the outbox revoke channel. Connections that are stopped or backing off must also be able to deliver the revocation
/// intent by reusing the S1g1 mechanism. Lock order is registry, then db, then token_book, matching the existing order.
///
/// S1h R5 rework: the `remote_device_revoke` command itself accepts Tauri `State<Db>`, making it awkward to call directly in unit tests. Extract the
/// two operations that must happen atomically (the omission-means-revocation fallback of DB revoke plus TokenBook invalidation, and explicitly
/// entering token.delete into the revoke channel) into a core function that accepts only already-unlocked references. The command layer becomes a
/// thin lock-taking wrapper that delegates to it. The defense-in-depth test exercises this core so it genuinely verifies the seam where both
/// operations occur in the same call, rather than manually reproducing the action sequence.
///
/// M24DR rework, item 1: obtain the number via `resolve_active_pairing_room_id_readonly` for the current active room rather than the legacy
/// `resolve_remote_room_id`; see the former function's documentation for the Blocker. If the active room cannot be resolved (remote control
/// disabled, active project unset, repo deleted, or the project has no room row), DB revoke plus TokenBook invalidation still occur normally as the
/// omission-means-revocation fallback. Do not enqueue an explicit `token.delete`, because there is no room to send it to. The fallback is justified
/// because, on the next connection, the reconciliation omission mechanism in `synchronize_registry` marks a subject omitted from the local snapshot
/// as revoked on the relay side (the non-reset branch in `room-store.js:463-478`). Explicit delete only accelerates the process; it is not the sole
/// path by which revocation takes effect.
pub(super) fn remote_device_revoke_inner(
    conn: &Connection,
    registry: &mut remote_gateway::RegistryState,
    token_book: &mut remote_pairing::TokenBook,
    key_store: &dyn KeyStore,
    device_id: &str,
    now_secs: i64,
) -> Result<(), String> {
    let generation = match resolve_active_pairing_room_id_readonly(conn)? {
        Some(room_id) => {
            Some(db::next_registry_generation(conn, &room_id).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    remote_pairing::store::revoke_device_and_sync(
        conn, key_store, token_book, device_id, now_secs,
    )?;
    if let Some(generation) = generation {
        registry.enqueue_token_delete(format!("device:{device_id}"), generation, true);
    }
    Ok(())
}

#[tauri::command]
pub(super) fn remote_device_revoke(db: State<Db>, device_id: String) -> Result<(), String> {
    let mut registry = remote_registry().lock().map_err(|e| e.to_string())?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut token_book = remote_token_book().lock().map_err(|e| e.to_string())?;
    remote_device_revoke_inner(
        &conn,
        &mut registry,
        &mut token_book,
        &KeyringStore,
        &device_id,
        now_unix_secs() as i64,
    )?;
    drop(token_book);
    drop(conn);
    drop(registry);
    remote_gateway::request_registry_publish();
    Ok(())
}
