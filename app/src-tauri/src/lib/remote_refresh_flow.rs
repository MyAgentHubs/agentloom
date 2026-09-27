use super::*;

struct ResolvedRefreshDevice<'a> {
    device_id: &'a str,
    row: db::RemoteDeviceRow,
    room_id: String,
    current_generation: i64,
    k_pair: zeroize::Zeroizing<[u8; 32]>,
    refresh_token: String,
}

fn resolve_refresh_device<'a>(
    registry: &mut remote_gateway::RegistryState,
    conn: &Connection,
    key_store: &dyn KeyStore,
    frame: &'a remote_gateway::RefreshForwardFrame,
) -> Result<ResolvedRefreshDevice<'a>, remote_gateway::RefreshOutcome> {
    // The subject is relay-stamped and forwarded, not an identity authenticated by the desktop.
    // Until it has been confirmed to correspond to a real device in the DB, always use
    // count_invalid=false. `refresh_fail_reply` calls `record_refresh_invalid` only when
    // count_invalid=true (which creates an entry through `.entry(subject).or_default()`). If the
    // early returns below still counted invalid requests, a rogue or malicious relay could vary
    // fake subjects and make the in-memory `refresh_quota` map grow without bound (a memory DoS).
    // The number of real devices is bounded and controlled by the desktop's own pairing flow.
    // Once `row` has been confirmed to exist (after the `Ok(Some(row))` branch below), subsequent
    // failure paths resume count_invalid=true: the subject is then a real device and cannot inflate
    // the count through forgery.
    let Some(device_id) = frame.subject.strip_prefix("device:") else {
        return Err(refresh_fail_reply(
            registry,
            &frame.request_id,
            &frame.subject,
            "invalid",
            false,
        ));
    };

    let row = match db::get_remote_device(conn, device_id) {
        Ok(Some(row)) if row.revoked_at.is_none() && row.room_id.is_some() => row,
        Ok(_) => {
            return Err(refresh_fail_reply(
                registry,
                &frame.request_id,
                &frame.subject,
                "invalid",
                false,
            ))
        }
        Err(error) => {
            eprintln!("remote refresh: device lookup failed for {device_id}: {error}");
            return Err(refresh_fail_reply(
                registry,
                &frame.request_id,
                &frame.subject,
                "invalid",
                false,
            ));
        }
    };
    let room_id = row.room_id.clone().expect("checked Some above");
    let Some(current_generation) = row.generation else {
        return Err(refresh_fail_reply(
            registry,
            &frame.request_id,
            &frame.subject,
            "invalid",
            true,
        ));
    };

    let k_pair = match remote_pairing::store::load_k_pair(key_store, device_id) {
        Ok(Some(k_pair)) => k_pair,
        Ok(None) => {
            return Err(refresh_fail_reply(
                registry,
                &frame.request_id,
                &frame.subject,
                "invalid",
                true,
            ))
        }
        Err(error) => {
            eprintln!("remote refresh: k_pair load failed for {device_id}: {error}");
            return Err(refresh_fail_reply(
                registry,
                &frame.request_id,
                &frame.subject,
                "invalid",
                true,
            ));
        }
    };

    let refresh_token = match remote_pairing::open_token_refresh_request(
        &k_pair,
        &room_id,
        device_id,
        &frame.request_id,
        &frame.ct,
        &frame.n,
    ) {
        Ok(token) => token,
        Err(_) => {
            return Err(refresh_fail_reply(
                registry,
                &frame.request_id,
                &frame.subject,
                "invalid",
                true,
            ))
        }
    };

    Ok(ResolvedRefreshDevice {
        device_id,
        row,
        room_id,
        current_generation,
        k_pair,
        refresh_token,
    })
}

fn rotate_current_refresh(
    registry: &mut remote_gateway::RegistryState,
    conn: &Connection,
    token_book: &mut remote_pairing::TokenBook,
    frame: &remote_gateway::RefreshForwardFrame,
    now_ms: u64,
    resolved: &ResolvedRefreshDevice<'_>,
) -> remote_gateway::RefreshOutcome {
    let device_id = resolved.device_id;
    // §2d: Check quota only when a rotation will actually be attempted: invalid requests,
    // idempotent replays, and in-flight conflicts do not consume it.
    //
    // This branch has no in_flight check and needs none: every match of the "current" hash rotates
    // unconditionally and overwrites the journal. That is not an omission. The journal's
    // prev_generation, prev_access_hash, and prev_expires_at fields are also the sole source for
    // the §9.4 registry snapshot's previous alias (`TokenSyncPrev`); see
    // `remote_registry_snapshot_entries`, in its
    // `db::load_refresh_journal(...).map(|journal| TokenSyncPrev { ... })` section. Every
    // successful rotation must overwrite them. Otherwise, the next registry snapshot's previous
    // alias would remain at the old value from the preceding rotation, distorting the relay/device
    // side's "previous-match window." The Mismatch branch's rule that in_flight must never
    // overwrite the journal belongs exclusively to recognizing repeated requests that match the
    // previous alias there; it cannot be moved here and applied wholesale.
    if registry.refresh_quota_exceeded(&frame.subject, now_ms) {
        return remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_fail_json(
            &frame.request_id,
            &frame.subject,
            "rate_limited",
            false,
        ));
    }
    match remote_pairing::store::refresh_device_tokens(
        conn,
        token_book,
        device_id,
        &resolved.room_id,
        resolved.current_generation,
        &resolved.row.token_hash,
        &resolved.k_pair,
        &frame.request_id,
        &resolved.refresh_token,
        now_ms,
    ) {
        Ok(rotated) => {
            registry.record_refresh_rotation_success(&frame.subject, now_ms);
            let entry = remote_gateway::TokenSyncEntry {
                subject: frame.subject.clone(),
                generation: rotated.generation,
                scope: "remote".to_owned(),
                current: remote_gateway::TokenSyncCurrent {
                    token_hash: rotated.access_token_hash,
                    access_expires: rotated.access_expires_at_ms,
                    refresh_until: Some(rotated.refresh_until_ms),
                },
                prev: Some(remote_gateway::TokenSyncPrev {
                    token_hash: rotated.prev_access_hash,
                    generation: rotated.prev_generation,
                    prev_expires: rotated.prev_expires_at_ms,
                }),
            };
            let refresh_ok = remote_gateway::RefreshOkFrame {
                request_id: frame.request_id.clone(),
                subject: frame.subject.clone(),
                generation: rotated.generation,
                ct: rotated.response_ct,
                n: rotated.response_n,
            };
            registry.enqueue_token_put_for_refresh(entry, refresh_ok);
            remote_gateway::RefreshOutcome::Pending
        }
        Err(error) => {
            eprintln!("remote refresh: rotation commit failed for {device_id}: {error}");
            // A failed rotation transaction itself (a DB error or persistence failure) is a
            // desktop fault, not a problem with the request sent by the device. It must not consume
            // the phone's consecutive-invalid allowance; otherwise, three consecutive desktop
            // failures could close the phone's legitimate connection. This matches the early
            // returns for poisoned locks or an unavailable DB in `remote_gateway_refresh_handler`,
            // which hard-code `close:false`: they too are desktop-side problems and never count
            // toward consecutive invalid requests.
            refresh_fail_reply(
                registry,
                &frame.request_id,
                &frame.subject,
                "invalid",
                false,
            )
        }
    }
}

fn replay_or_in_flight(
    registry: &mut remote_gateway::RegistryState,
    conn: &Connection,
    frame: &remote_gateway::RefreshForwardFrame,
    now_ms: u64,
    resolved: &ResolvedRefreshDevice<'_>,
) -> remote_gateway::RefreshOutcome {
    let device_id = resolved.device_id;
    match db::load_refresh_journal(conn, device_id) {
        Ok(Some(journal))
            if u64::try_from(journal.response_expires).is_ok_and(|expires| now_ms < expires) =>
        {
            if !remote_pairing::refresh_token_hash_matches(
                &resolved.refresh_token,
                &journal.prev_refresh_hash,
            ) {
                return refresh_fail_reply(
                    registry,
                    &frame.request_id,
                    &frame.subject,
                    "invalid",
                    true,
                );
            }
            if journal.request_id == frame.request_id {
                // §2c: Idempotent replay returns the same receipt unchanged: it does not rotate,
                // allocate a generation, write the DB, or enqueue an outbox item.
                //
                // Use the current generation of the device row read for this request
                // (`current_generation`, parsed earlier in this function), not
                // `journal.generation`: the latter is the old value frozen when the preceding
                // rotation succeeded. If a rebase (§9.3 reassigns a generation for each device)
                // occurred between rotation and this replay, `journal.generation` is stale. The
                // relay-side §9.6 delivery predicate requires receipt.generation to equal the
                // subject's current generation, so a receipt sent with the old generation is
                // necessarily dropped. The AAD five-tuple excludes generation, so changing this
                // field does not affect ciphertext authentication: `ct`/`n` are still replayed
                // unchanged from `journal.response_ct`/`journal.response_n`, without sealing again.
                registry.record_refresh_replay(&frame.subject);
                remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_ok_json(
                    &frame.request_id,
                    &frame.subject,
                    resolved.current_generation,
                    &journal.response_ct,
                    &journal.response_n,
                ))
            } else {
                // §2c/§9.6: in_flight is a benign single-flight conflict: do not close, consume
                // quota, or overwrite the journal (overwriting it would invalidate the first
                // request's replay guarantee).
                //
                // The in_flight decision is triggered only in this previous-alias branch
                // (Mismatch), and must not be moved to the Current branch. Here the match is the
                // previous alias produced by the preceding rotation. This request is either a
                // legitimate replay with the same request_id (overwriting the journal would break
                // its replay guarantee) or another concurrent in-flight attempt (overwriting it
                // would lose the first request's replay capability); neither case should overwrite
                // the journal. A Current-branch match uses a fresh current token and has entirely
                // different semantics; see the corresponding comment before its
                // `refresh_device_tokens` call.
                remote_gateway::RefreshOutcome::Reply(remote_gateway::refresh_fail_json(
                    &frame.request_id,
                    &frame.subject,
                    "in_flight",
                    false,
                ))
            }
        }
        Ok(_) => refresh_fail_reply(registry, &frame.request_id, &frame.subject, "invalid", true),
        Err(error) => {
            eprintln!("remote refresh: journal load failed for {device_id}: {error}");
            refresh_fail_reply(registry, &frame.request_id, &frame.subject, "invalid", true)
        }
    }
}

/// The `token.refresh.forward` orchestration core preserves the registry→db→token_book lock
/// order used by `remote_device_revoke_inner`. Failures use `refresh_fail_reply` (accumulating
/// consecutive invalid counts and closing when needed); rotation and outbox enqueue occur only
/// when §2a matches the current refresh hash and the quota limit has not been reached.
pub(crate) fn process_token_refresh_with_registry(
    registry: &mut remote_gateway::RegistryState,
    conn: &Connection,
    key_store: &dyn KeyStore,
    token_book: &mut remote_pairing::TokenBook,
    frame: &remote_gateway::RefreshForwardFrame,
    now_ms: u64,
) -> remote_gateway::RefreshOutcome {
    let resolved = match resolve_refresh_device(registry, conn, key_store, frame) {
        Ok(resolved) => resolved,
        Err(outcome) => return outcome,
    };

    match token_book.matches_current_refresh(resolved.device_id, &resolved.refresh_token) {
        remote_pairing::RefreshTokenMatch::Unavailable => {
            refresh_fail_reply(registry, &frame.request_id, &frame.subject, "invalid", true)
        }
        remote_pairing::RefreshTokenMatch::Current => {
            rotate_current_refresh(registry, conn, token_book, frame, now_ms, &resolved)
        }
        remote_pairing::RefreshTokenMatch::Mismatch => {
            replay_or_in_flight(registry, conn, frame, now_ms, &resolved)
        }
    }
}
