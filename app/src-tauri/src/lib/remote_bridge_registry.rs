use super::*;

/// Safely read app settings from the background remote-gateway thread.
/// Use `try_state` instead of `state` because this closure could theoretically run before the
/// `Db` has been managed. This is the same defensive pattern used by the existing
/// `MutationGuard` and `app.try_state::<crate::db::Db>()` call sites.
pub(super) fn remote_gateway_settings_reader(app: &AppHandle) -> remote_gateway::SettingsReader {
    let app = app.clone();
    Box::new(move |key: &str| {
        let db = app.try_state::<Db>()?;
        let conn = db.inner().0.lock().ok()?;
        db::get_app_setting(&conn, key).ok().flatten()
    })
}
/// The retired backdoor used a plaintext `remote_dev_token` app-setting key as a token source,
/// appending it to the desktop relay query as `?token=` for legacy admission. The real device
/// identity is now the `Authorization: Bearer` desktop credential, and the real token registry
/// never consumed this key. The corresponding legacy relay admission path has been removed, so
/// this setting is no longer read or used as a credential. The `TokenProvider` type remains
/// because `evaluate_connection_liveness` uses it for the general "credential material rotated,
/// force a reconnect" liveness behavior; it is not specific to the retired development backdoor.
pub(super) fn remote_gateway_token_provider(_app: &AppHandle) -> remote_gateway::TokenProvider {
    Box::new(|| None)
}
pub(super) fn remote_gateway_desktop_credential_provider(
) -> remote_gateway::DesktopCredentialProvider {
    Box::new(|room_id: &str| {
        remote_pairing::store::resolve_desktop_credential(&KeyringStore, room_id)
    })
}
pub(super) fn remote_gateway_claim_client() -> remote_gateway::ClaimClient {
    Box::new(remote_gateway::claim_room_blocking)
}
pub(super) fn remote_gateway_active_device_provider(
    app: &AppHandle,
) -> remote_gateway::ActiveDeviceProvider {
    let app = app.clone();
    Box::new(move |room_id: &str| {
        let db = app
            .try_state::<Db>()
            .ok_or_else(|| "Db state unavailable".to_owned())?;
        let conn = db.inner().0.lock().map_err(|error| error.to_string())?;
        let rows = db::list_remote_devices(&conn).map_err(|error| error.to_string())?;
        Ok(has_active_remote_device_in_room(&rows, room_id))
    })
}
pub(super) fn has_active_remote_device_in_room(
    rows: &[db::RemoteDeviceRow],
    room_id: &str,
) -> bool {
    rows.iter().any(|row| {
        row.revoked_at.is_none()
            && row
                .room_id
                .as_deref()
                .is_some_and(|stored_room| stored_room == room_id)
    })
}
pub(super) fn load_remote_registry_snapshot(
    conn: &Connection,
    room_id: &str,
    now_ms: u64,
) -> Result<remote_gateway::RegistrySnapshot, String> {
    let mut entries = Vec::new();
    for row in db::list_remote_devices(conn).map_err(|error| error.to_string())? {
        if row.revoked_at.is_some() || row.room_id.as_deref() != Some(room_id) {
            continue;
        }
        let generation = row.generation.ok_or_else(|| {
            remote_registry_snapshot_row_error(&row.device_id, "generation_missing")
        })?;
        let refresh_until_ms = row.refresh_until.ok_or_else(|| {
            remote_registry_snapshot_row_error(&row.device_id, "refresh_until_missing")
        })?;
        if generation <= 0 {
            return Err(remote_registry_snapshot_row_error(
                &row.device_id,
                "generation_invalid",
            ));
        }
        if refresh_until_ms < 100_000_000_000 {
            return Err(remote_registry_snapshot_row_error(
                &row.device_id,
                "refresh_until_invalid",
            ));
        }
        if row.token_hash.len() != 64
            || !row.token_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(remote_registry_snapshot_row_error(
                &row.device_id,
                "token_hash_invalid",
            ));
        }
        if row.access_expires_at <= 0 {
            return Err(remote_registry_snapshot_row_error(
                &row.device_id,
                "access_expires_invalid",
            ));
        }
        if row.access_expires_at > refresh_until_ms {
            return Err(remote_registry_snapshot_row_error(
                &row.device_id,
                "access_expires_after_refresh_until",
            ));
        }
        let prev = db::load_refresh_journal(conn, &row.device_id)
            .map_err(|error| error.to_string())?
            .filter(|journal| {
                journal.prev_generation > 0
                    && journal.prev_expires_at > 0
                    && u64::try_from(journal.prev_expires_at)
                        .is_ok_and(|expires_ms| now_ms < expires_ms)
            })
            .map(|journal| remote_gateway::TokenSyncPrev {
                token_hash: journal.prev_access_hash,
                generation: journal.prev_generation,
                prev_expires: journal.prev_expires_at,
            });
        entries.push(remote_gateway::TokenSyncEntry {
            subject: format!("device:{}", row.device_id),
            generation,
            scope: "remote".to_owned(),
            current: remote_gateway::TokenSyncCurrent {
                token_hash: row.token_hash,
                access_expires: row.access_expires_at,
                refresh_until: Some(refresh_until_ms),
            },
            prev,
        });
    }
    Ok(remote_gateway::RegistrySnapshot {
        revision: db::current_registry_revision(conn, room_id)
            .map_err(|error| error.to_string())?,
        entries,
    })
}
fn remote_registry_snapshot_row_error(device_id: &str, field: &str) -> String {
    eprintln!("remote registry snapshot rejected: device_id={device_id}, field={field}");
    format!("remote registry snapshot rejected for device {device_id}: {field}")
}
pub(super) fn remote_gateway_registry_snapshot_provider(
    app: &AppHandle,
) -> remote_gateway::RegistrySnapshotProvider {
    let app = app.clone();
    Box::new(move |room_id, now_ms| {
        let db = app
            .try_state::<Db>()
            .ok_or_else(|| "Db state unavailable".to_owned())?;
        let conn = db.inner().0.lock().map_err(|error| error.to_string())?;
        load_remote_registry_snapshot(&conn, room_id, now_ms)
    })
}

pub(super) fn remote_gateway_registry_rebase_provider(
    app: &AppHandle,
) -> remote_gateway::RegistryRebaseProvider {
    let app = app.clone();
    Box::new(
        move |room_id, relay_high_water, now_ms, include_pairing, revoke_subjects| {
            let db = app
                .try_state::<Db>()
                .ok_or_else(|| "Db state unavailable".to_owned())?;
            let conn = db.inner().0.lock().map_err(|error| error.to_string())?;
            rebase_remote_registry(
                &conn,
                room_id,
                relay_high_water,
                now_ms,
                include_pairing,
                revoke_subjects,
            )
        },
    )
}

/// Testable core for `remote_gateway_registry_high_water_provider`: after `sync.ack`, always
/// advance the desktop counter past `relay_high_water`, then allocate a new generation in the
/// same transaction for every `revoke_subjects` entry. Those entries are the `token.delete`
/// subjects still awaiting delivery in the outbox, including rejected entries. Because the new
/// generations come from the counter after it absorbs the relay high-water mark, they are strictly
/// greater than `relay_high_water` and therefore also strictly greater than this sync revision,
/// since a real relay always reports `relay_high_water >= revision`. Do not allocate delete
/// generations inside the `rebase_remote_registry` transaction before the snapshot revision is
/// fixed: that path runs only when the relay requests a rebase and misses the primary case where
/// the first sync is accepted directly without a rebase, which is why this bug went undetected.
pub(super) fn absorb_registry_high_water_and_reissue_revokes(
    conn: &Connection,
    room_id: &str,
    relay_high_water: i64,
    revoke_subjects: &[String],
) -> Result<Vec<(String, i64)>, String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    db::bump_registry_counter_to_in_transaction(&tx, room_id, relay_high_water)
        .map_err(|error| error.to_string())?;
    let mut revoke_generations = Vec::with_capacity(revoke_subjects.len());
    for subject in revoke_subjects {
        let generation = db::next_registry_generation_in_transaction(&tx, room_id)
            .map_err(|error| error.to_string())?;
        revoke_generations.push((subject.clone(), generation));
    }
    tx.commit().map_err(|error| error.to_string())?;
    Ok(revoke_generations)
}

pub(super) fn remote_gateway_registry_high_water_provider(
    app: &AppHandle,
) -> remote_gateway::RegistryHighWaterProvider {
    let app = app.clone();
    Box::new(move |room_id, relay_high_water, revoke_subjects| {
        let db = app
            .try_state::<Db>()
            .ok_or_else(|| "Db state unavailable".to_owned())?;
        let conn = db.inner().0.lock().map_err(|error| error.to_string())?;
        absorb_registry_high_water_and_reissue_revokes(
            &conn,
            room_id,
            relay_high_water,
            revoke_subjects,
        )
    })
}

/// `revoke_subjects` contains the `token.delete` subjects still awaiting delivery in the
/// outbox, including rejected entries. Allocate a new generation for each subject in the same
/// transaction, sharing `remote_registry_counter` with device and pairing allocations so
/// independently synthesized generations such as `high_water + 1` cannot collide.
pub(super) fn rebase_remote_registry(
    conn: &Connection,
    room_id: &str,
    relay_high_water: i64,
    now_ms: u64,
    include_pairing: bool,
    revoke_subjects: &[String],
) -> Result<
    (
        remote_gateway::RegistrySnapshot,
        Option<i64>,
        Vec<(String, i64)>,
    ),
    String,
> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    db::bump_registry_counter_to_in_transaction(&tx, room_id, relay_high_water)
        .map_err(|error| error.to_string())?;

    let rows = db::list_remote_devices(&tx).map_err(|error| error.to_string())?;
    for row in rows.into_iter().filter(|row| {
        row.revoked_at.is_none()
            && row.room_id.as_deref() == Some(room_id)
            && row.generation.is_some_and(|generation| generation > 0)
            && row
                .refresh_until
                .is_some_and(|refresh_until_ms| refresh_until_ms > 0)
            && row.access_expires_at > 0
    }) {
        let generation = db::next_registry_generation_in_transaction(&tx, room_id)
            .map_err(|error| error.to_string())?;
        let refresh_until_ms = row.refresh_until.expect("filtered above");
        if !db::set_remote_device_registry_in_transaction(
            &tx,
            &row.device_id,
            room_id,
            generation,
            refresh_until_ms,
        )
        .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "remote device {} disappeared during registry rebase",
                row.device_id
            ));
        }
    }
    let pairing_generation = if include_pairing {
        Some(
            db::next_registry_generation_in_transaction(&tx, room_id)
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let mut revoke_generations = Vec::with_capacity(revoke_subjects.len());
    for subject in revoke_subjects {
        let generation = db::next_registry_generation_in_transaction(&tx, room_id)
            .map_err(|error| error.to_string())?;
        revoke_generations.push((subject.clone(), generation));
    }
    let snapshot = load_remote_registry_snapshot(&tx, room_id, now_ms)?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok((snapshot, pairing_generation, revoke_generations))
}

/// Ensure credentials idempotently: do nothing if the keychain already has credentials for the
/// room, otherwise create them. Reuse the existing check-then-create behavior in
/// `remote_pairing::store::resolve_desktop_credential` and narrow the result to whether the
/// credential is ensured. Room resolution does not need the credential plaintext; the existing
/// `desktop_credential_provider` reads it again when actually connecting or claiming.
///
/// `remote_gateway_active_room_resolver` calls this after
/// `db::ensure_remote_room_for_project` persists the room row and before returning the `room_id`
/// used by `current_config` to build `GatewayConfig`. This ordering is crash-safe: if the process
/// crashes between persisting the room and creating its credential, the next active-project
/// resolution returns the same room idempotently and creates the credential idempotently. External
/// callers never observe a configuration that points to a room without a credential because
/// `current_config` cannot return `Some(GatewayConfig)` until both steps finish, so connection
/// and claim operations never run without credentials. The analogous crash-safe mechanism for the
/// retired legacy global room was removed with that fallback because it only served legacy room
/// replacement.
pub(super) fn ensure_desktop_credential_for_room(
    key_store: &dyn KeyStore,
    room_id: &str,
) -> Result<(), String> {
    remote_pairing::store::resolve_desktop_credential(key_store, room_id).map(|_credential| ())
}

/// Skip the keychain when `credential_ensured` contains the room; otherwise, including after the
/// cache was cleared, call `ensure_desktop_credential_for_room` and populate the cache. This
/// helper accepts only `&dyn KeyStore` and `&Mutex<HashSet<String>>`, with no `AppHandle`
/// dependency, so cache-hit and cache-miss behavior can be tested directly with `FakeKeyStore`
/// without constructing a real Tauri app.
pub(super) fn ensure_desktop_credential_for_room_cached(
    key_store: &dyn KeyStore,
    credential_ensured: &Mutex<HashSet<String>>,
    room_id: &str,
) -> Result<(), String> {
    let already_ensured = credential_ensured
        .lock()
        .map_err(|error| error.to_string())?
        .contains(room_id);
    if already_ensured {
        return Ok(());
    }
    ensure_desktop_credential_for_room(key_store, room_id)?;
    credential_ensured
        .lock()
        .map_err(|error| error.to_string())?
        .insert(room_id.to_owned());
    Ok(())
}

/// `remote_gateway_active_room_resolver` and the `remote_set_active_project` command share one
/// cache of rooms whose credentials have been confirmed. A successful project switch or active-
/// project clear must empty the cache so the next resolution checks the keychain again instead of
/// trusting a potentially stale marker, such as after external credential deletion or when an old
/// project's marker is irrelevant to the new project. This is Tauri-managed state, created once in
/// `run()`, with `Arc::clone` values passed to the resolver closure and command.
pub(super) struct ActiveRoomCredentialCache(pub(super) Arc<Mutex<HashSet<String>>>);

/// Resolve a project id to that project's room id. The ensure semantics reuse an existing room or
/// create one through `db::ensure_remote_room_for_project`, then immediately ensure its credential
/// through `ensure_desktop_credential_for_room_cached`. Call this only when remote access is
/// enabled and an active project is set; `current_config` owns that gate.
///
/// As an independent defensive check, verify that `project_id` actually exists in `repos` before
/// ensuring the room. This blocks manual database edits or stale settings from bypassing the
/// validation already performed when the command writes the setting. A missing project returns
/// `Err`, allowing `current_config` to follow its existing fail-closed path without falling back
/// to a legacy room.
pub(super) fn remote_gateway_active_room_resolver(
    app: &AppHandle,
    credential_ensured: Arc<Mutex<HashSet<String>>>,
) -> remote_gateway::ActiveRoomResolver {
    let app = app.clone();
    Box::new(move |project_id: &str| {
        let room_id = {
            let db = app
                .try_state::<Db>()
                .ok_or_else(|| "Db state unavailable".to_owned())?;
            let conn = db.inner().0.lock().map_err(|error| error.to_string())?;
            let exists = repos_repo::get_repo_by_id(&conn, project_id)
                .map_err(|error| error.to_string())?
                .is_some();
            if !exists {
                return Err(format!(
                    "remote active room resolution: repo not found: {project_id}"
                ));
            }
            db::ensure_remote_room_for_project(&conn, project_id)?
        };
        // The preceding block has already dropped `conn` and `db`, so no DB lock guard may
        // remain alive here or on the following line. The credential helper can block while
        // accessing the keychain. A future refactor that extends `conn` into this scope through a
        // `match`, `if let`, or similar construct would wait for keychain IPC while holding the
        // global DB lock and block every other caller that needs it, including each UI-thread IPC.
        // Before changing this section, confirm that the lock is already out of scope.
        ensure_desktop_credential_for_room_cached(&KeyringStore, &credential_ensured, &room_id)?;
        Ok(room_id)
    })
}

/// Read `K_room` each time the gateway actually attempts a connection. Once a long-lived
/// connection holds `K_room`, liveness polling no longer reads the keychain, avoiding keychain IPC
/// on the WebSocket reader thread. Until a key is held, polling continues so it can self-heal; a
/// newly observed key triggers `ConfigStale`, disconnecting and reconnecting so the next
/// connection carries the key. The key format intentionally duplicates the private
/// `remote_pairing::store::k_room_key_id` format because that function cannot leave its module.
/// Keep the two literals identical and check for semantic drift before changing either one.
/// A missing keychain entry means this desktop has not paired with any remote device, so upstream
/// traffic is disabled. Do not generate a new `K_room` here: only
/// `remote_pairing::store::resolve_k_room` may create it after pairing succeeds; the gateway only
/// reads it.
pub(super) fn remote_gateway_k_room_provider() -> remote_gateway::KRoomProvider {
    Box::new(|room_id: &str| {
        let key_id = format!("remote-kroom-{room_id}");
        let stored = zeroize::Zeroizing::new(KeyringStore.get(&key_id).ok().flatten()?);
        let bytes = zeroize::Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(stored.as_bytes())
                .ok()?,
        );
        if bytes.len() != 32 {
            return None;
        }
        let mut out = zeroize::Zeroizing::new([0u8; 32]);
        out.copy_from_slice(&bytes);
        Some(out)
    })
}

/// Provide the full post-connection snapshot on the dedicated `remote-index-snapshot` background
/// thread, so it does not block the WebSocket reader thread. Keep the provider bounded: briefly
/// lock the DB, avoid keychain, network, and child-process work, and silently return `None` on
/// failure without panicking. As in `remote_gateway_settings_reader`, use `try_state`
/// defensively in case the `Db` has not yet been managed.
pub(super) fn remote_gateway_session_index_snapshot_provider(
    app: &AppHandle,
) -> remote_gateway::SessionIndexSnapshotProvider {
    let app = app.clone();
    Box::new(move || {
        let db = app.try_state::<Db>()?;
        let conn = db.inner().0.lock().ok()?;
        let rows = db::list_session_index_snapshot_rows(&conn).ok()?;
        serde_json::to_value(rows).ok()
    })
}

/// Query a session's owning repository id through `db::get_session_repo_id`. Unlike the other
/// remote-gateway providers, this provider is called synchronously from the WebSocket reader thread
/// for the downward-command ownership gate and from the drain loops for upstream ownership
/// filtering through the connection-lifetime session-to-repository cache. It is not run on a
/// dedicated background thread, so it briefly locks the DB and never touches the keychain, network,
/// or child processes. The retired legacy global-room fallback and its conditional ownership-gate
/// switch are gone; under the single-active-room model the ownership gate is always enabled and
/// this provider is called whenever a connection exists.
pub(super) fn remote_gateway_session_repo_provider(
    app: &AppHandle,
) -> remote_gateway::SessionRepoProvider {
    let app = app.clone();
    Box::new(move |session_id: &str| {
        let db = app
            .try_state::<Db>()
            .ok_or_else(|| "Db state unavailable".to_owned())?;
        let lock_result = db.inner().0.lock();
        let conn = lock_result.map_err(|_| "Db lock poisoned".to_owned())?;
        db::get_session_repo_id(&conn, session_id).map_err(|error| error.to_string())
    })
}

/// Provide paginated `control.history` reads. Like the session-ownership provider, this holds only
/// a short DB lock on the WebSocket command thread and never touches the keychain, network, or child
/// processes. Query failures are returned explicitly so the command arm can fail closed.
pub(super) fn remote_gateway_session_history_provider(
    app: &AppHandle,
) -> remote_gateway::SessionHistoryProvider {
    let app = app.clone();
    Box::new(move |session_id, before_message_id, max_rows| {
        let rows = {
            let db = app
                .try_state::<Db>()
                .ok_or_else(|| "Db state unavailable".to_owned())?;
            let lock_result = db.inner().0.lock();
            let conn = lock_result.map_err(|_| "Db lock poisoned".to_owned())?;
            db::list_session_history_rows(&conn, session_id, before_message_id, max_rows)
                .map_err(|error| error.to_string())?
        };
        rows.into_iter()
            .map(|row| {
                let content_json =
                    serde_json::from_str(&row.content).map_err(|error| error.to_string())?;
                Ok(remote_gateway::SessionHistoryRow {
                    message_id: row.message_id,
                    role: row.role,
                    content_json,
                    // Hash and measure the original stored content bytes so content_ref matches the data being fetched.
                    // Measure the stored DB content bytes and preserve `row.content` instead of
                    // passing only a reserialized `Value`.
                    content_raw: row.content,
                    revision: row.revision,
                })
            })
            .collect()
    })
}

/// Validate this provider first in the `msg.fetch` authorization chain and fail closed on query
/// errors. Like `remote_gateway_session_repo_provider` and
/// `remote_gateway_session_history_provider`, it holds only a short DB lock on the command thread
/// and never touches the keychain, network, or child processes. Map `db::MessageForFetch` to the
/// deliberately separate `remote_gateway::MessageForFetchResult` type, following the existing
/// `SessionHistoryRow` convention, so remote-gateway tests can construct all three states without
/// depending on the database module.
pub(super) fn remote_gateway_message_fetch_provider(
    app: &AppHandle,
) -> remote_gateway::MessageFetchProvider {
    let app = app.clone();
    Box::new(move |session_id: &str, message_id: i64| {
        let db = app
            .try_state::<Db>()
            .ok_or_else(|| "Db state unavailable".to_owned())?;
        let lock_result = db.inner().0.lock();
        let conn = lock_result.map_err(|_| "Db lock poisoned".to_owned())?;
        let result = db::get_message_for_fetch(&conn, session_id, message_id)
            .map_err(|error| error.to_string())?;
        Ok(match result {
            db::MessageForFetch::Found {
                content,
                revision,
                session_deleted,
            } => remote_gateway::MessageForFetchResult::Found {
                content_raw: content,
                revision,
                session_deleted,
            },
            db::MessageForFetch::WrongSession => {
                remote_gateway::MessageForFetchResult::WrongSession
            }
            db::MessageForFetch::NotFound => remote_gateway::MessageForFetchResult::NotFound,
        })
    })
}

/// Production persistence provider for the activity-summary aggregator. Its signature mirrors
/// `db::upsert_activity_summary_and_publish`. Following the message-fetch provider convention, it
/// runs only on the dedicated activity-summary writer thread, not the Tauri main thread, briefly
/// locks the `Db` state, and never touches the keychain, network, or child processes.
pub(super) fn remote_gateway_activity_summary_writer(
    app: &AppHandle,
) -> remote_gateway::ActivitySummaryWriter {
    let app = app.clone();
    Box::new(
        move |session_id: &str,
              run_id: &str,
              tool_calls: i64,
              failed: i64,
              mcp_calls: i64,
              permission_prompts: i64,
              state: &str| {
            let db = app
                .try_state::<Db>()
                .ok_or_else(|| "Db state unavailable".to_owned())?;
            let lock_result = db.inner().0.lock();
            let conn = lock_result.map_err(|_| "Db lock poisoned".to_owned())?;
            db::upsert_activity_summary_and_publish(
                &conn,
                session_id,
                run_id,
                tool_calls,
                failed,
                mcp_calls,
                permission_prompts,
                state,
            )
            .map_err(|error| error.to_string())
        },
    )
}

/// Provide the post-connection replay batch on the dedicated `remote-index-snapshot` background
/// thread, like the session-index snapshot. Briefly lock the DB and silently return `None` on
/// failure without panicking.
pub(super) fn remote_gateway_milestone_replay_provider(
    app: &AppHandle,
) -> remote_gateway::MilestoneReplayProvider {
    let app = app.clone();
    Box::new(move || {
        let db = app.try_state::<Db>()?;
        let conn = db.inner().0.lock().ok()?;
        db::list_recent_milestone_replay_rows(&conn, db::RECENT_MILESTONE_REPLAY_LIMIT).ok()
    })
}

/// Replay current `run.status` rows after connection so active sessions are synchronized,
/// excluding soft-deleted sessions. Pass the rows to `publish_run_status_replay_rows` to rebuild
/// and resend frames for each session. As with the milestone replay provider, briefly lock the DB
/// and silently return `None` on failure without panicking.
pub(super) fn remote_gateway_session_runtime_replay_provider(
    app: &AppHandle,
) -> remote_gateway::SessionRuntimeReplayProvider {
    let app = app.clone();
    Box::new(move || {
        let db = app.try_state::<Db>()?;
        let conn = db.inner().0.lock().ok()?;
        db::list_session_runtime_replay_rows(&conn).ok()
    })
}

pub(super) fn remote_gateway_pair_hello_handler() -> remote_gateway::PairHelloHandler {
    Box::new(move |frame| {
        match process_pair_hello(pairing_slot(), &KeyringStore, frame, now_unix_secs()) {
            Ok(accept) => accept,
            Err(error) => {
                eprintln!("remote gateway pair.hello ignored: {error}");
                None
            }
        }
    })
}

pub(super) fn remote_gateway_pair_done_handler(app: &AppHandle) -> remote_gateway::PairDoneHandler {
    let app = app.clone();
    Box::new(move |frame| {
        let Ok(mut registry) = remote_registry().lock() else {
            eprintln!("remote gateway pair.done ignored: registry lock poisoned");
            return remote_gateway::PairDoneAction::Rejected;
        };
        let Some(db) = app.try_state::<Db>() else {
            eprintln!("remote gateway pair.done ignored: Db state unavailable");
            return remote_gateway::PairDoneAction::Rejected;
        };
        let Ok(conn) = db.inner().0.lock() else {
            eprintln!("remote gateway pair.done ignored: Db lock poisoned");
            return remote_gateway::PairDoneAction::Rejected;
        };
        let now_ms = now_unix_millis();
        let now_secs = now_ms / 1_000;
        match process_pair_done_with_registry(
            pairing_slot(),
            &mut registry,
            &conn,
            &KeyringStore,
            remote_token_book(),
            frame,
            now_secs,
            now_ms,
        ) {
            Ok(action) => {
                let newly_paired_device_id = match &action {
                    remote_gateway::PairDoneAction::Accepted {
                        newly_paired_device_id,
                    } => newly_paired_device_id.clone(),
                    remote_gateway::PairDoneAction::Rejected
                    | remote_gateway::PairDoneAction::Ready(_) => None,
                };
                drop(conn);
                drop(registry);
                if let Some(device_id) = newly_paired_device_id {
                    let _ = app.emit(
                        "remote-device-paired",
                        serde_json::json!({"device_id": device_id, "paired_at": now_secs}),
                    );
                }
                action
            }
            Err(error) => {
                eprintln!("remote gateway pair.done ignored: {error}");
                remote_gateway::PairDoneAction::Rejected
            }
        }
    })
}
