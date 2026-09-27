#[cfg(test)]
use super::ACCESS_LIFETIME_MS;
use super::{
    device_access_expires_at_ms, device_refresh_until_ms, refresh_prev_alias_expires_at_ms,
    seal_token_refresh_ok, AcceptOutcome, DeviceTokens, RotatedTokens, TokenBook,
};
use crate::db;
use crate::keychain::KeyStore;
use crate::remote_crypto::generate_key_32;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::Connection;
use std::collections::HashMap;
use zeroize::Zeroizing;

fn k_pair_key_id(device_id: &str) -> String {
    format!("remote-kpair-{device_id}")
}

fn k_room_key_id(room_id: &str) -> String {
    format!("remote-kroom-{room_id}")
}

fn desktop_credential_key_id(room_id: &str) -> String {
    format!("remote-desktop-credential-{room_id}")
}

fn valid_desktop_credential(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Resolve the room-scoped desktop owner credential. Existing entries are never overwritten:
/// malformed keychain state fails closed, while absence creates one 256-bit CSPRNG value.
pub(crate) fn resolve_desktop_credential(
    key_store: &dyn KeyStore,
    room_id: &str,
) -> Result<Zeroizing<String>, String> {
    let key_id = desktop_credential_key_id(room_id);
    if let Some(existing) = key_store.get(&key_id)? {
        if !valid_desktop_credential(&existing) {
            return Err(format!(
                "keychain entry {key_id} is not a lowercase hex64 desktop credential"
            ));
        }
        return Ok(Zeroizing::new(existing));
    }
    create_desktop_credential(key_store, room_id)
}

/// Create a credential only for a brand-new room. Refusing an existing entry makes it
/// impossible for a recovery path to rotate a live room's credential accidentally.
pub(crate) fn create_desktop_credential(
    key_store: &dyn KeyStore,
    room_id: &str,
) -> Result<Zeroizing<String>, String> {
    let key_id = desktop_credential_key_id(room_id);
    if key_store.get(&key_id)?.is_some() {
        return Err(format!(
            "desktop credential already exists for room {room_id}"
        ));
    }
    let credential = Zeroizing::new(super::encode_hex(generate_key_32().as_ref()));
    key_store.set(&key_id, credential.as_str())?;
    Ok(credential)
}

fn decode_32(value: &str) -> Option<[u8; 32]> {
    STANDARD.decode(value).ok()?.try_into().ok()
}
/// Refresh request/receipt ciphertext bodies use this device's own K_pair. Its lookup uses the same
/// key name as revoke_device removes (`remote-kpair-<device_id>`). Ok(None) means that this device
/// is absent from the keychain (unpaired or removed by revoke_device); callers treat it as invalid,
/// not as an internal error.
pub(crate) fn load_k_pair(
    key_store: &dyn KeyStore,
    device_id: &str,
) -> Result<Option<Zeroizing<[u8; 32]>>, String> {
    let key_id = k_pair_key_id(device_id);
    let Some(existing) = key_store.get(&key_id)? else {
        return Ok(None);
    };
    let bytes = decode_32(&existing)
        .ok_or_else(|| format!("keychain entry {key_id} 不是合法的 32 字节 K_pair"))?;
    Ok(Some(Zeroizing::new(bytes)))
}
/// Obtain the room's K_room: if the keychain already has `remote-kroom-<room_id>`, decrypt and
/// reuse it; otherwise generate a new 32-byte CSPRNG value, store it in the keychain immediately,
/// and return it. The gateway closed loop calls this after authenticated hello succeeds, then
/// finalizes with K_room. What remains here is only an idempotent room-level shared key; it contains
/// no pending device identity and therefore does not constitute a ghost device. The second and later
/// devices in the same room reuse it, each wrapped with its own K_pair.
pub(crate) fn resolve_k_room(key_store: &dyn KeyStore, room_id: &str) -> Result<[u8; 32], String> {
    let key_id = k_room_key_id(room_id);
    if let Some(existing) = key_store.get(&key_id)? {
        return decode_32(&existing)
            .ok_or_else(|| format!("keychain entry {key_id} 不是合法的 32 字节 K_room"));
    }
    let generated = generate_key_32();
    key_store.set(&key_id, &STANDARD.encode(generated.as_ref()))?;
    Ok(*generated)
}
/// Persist the successful handle_hello result: write DeviceRecord to the remote_devices table and
/// the device K_pair to the keychain (key name `remote-kpair-<device_id>`; `access_expires_at =
/// now_ms + 1h`, derived from the same ACCESS_LIFETIME_MS constant as TokenBook::insert, so the two
/// definitions cannot drift). Reconfirming K_room for room_id is solely a defensive idempotent
/// fallback (the gateway's hello path has already stored it in the keychain, so this does not
/// actually generate a new value).
pub(crate) fn persist_pairing_outcome(
    conn: &Connection,
    key_store: &dyn KeyStore,
    room_id: &str,
    outcome: &AcceptOutcome,
    created_at_secs: u64,
    now_ms: u64,
) -> Result<(), String> {
    resolve_k_room(key_store, room_id)?;

    let device = &outcome.device_record;
    let access_expires_at_ms = device_access_expires_at_ms(now_ms)?;
    db::insert_remote_device(
        conn,
        &device.device_id,
        Some(room_id),
        "",
        &device.token_hash,
        &device.refresh_hash,
        access_expires_at_ms,
        created_at_secs as i64,
    )
    .map_err(|e| e.to_string())?;

    // Writing the DB first avoids leaving an orphan K_pair when insert fails; a keychain failure in
    // the reverse order would leave an unusable device row.
    key_store.set(
        &k_pair_key_id(&device.device_id),
        &STANDARD.encode(device.k_pair.as_slice()),
    )
}
/// The sole composite write entry point for the desktop refresh rotation flow: first validate
/// read-only in memory and generate candidate tokens, allocate a generation
/// (next_registry_generation_in_transaction), then write new
/// access_hash/access_expires/refresh_hash/refresh_until plus all nine journal fields in the same DB
/// transaction, and commit the candidates to the in-memory TokenBook only after that succeeds. A
/// transaction failure has no side effects (TokenBook and keychain remain untouched; outbox enqueueing
/// belongs to the caller and this function does not touch it).
///
/// Callers must first use TokenBook::matches_current_refresh to confirm a "current" match before
/// calling this function. It runs prepare_refresh again with the same refresh_token (candidate tokens
/// are generated only here, so the probing stage does not calculate them needlessly).
/// prev_generation is provided by the caller (read from the pre-rotation device row's generation
/// column; TokenBook does not carry registry columns, so one read is sufficient and this function
/// need not query the DB again). The same applies to prev_access_hash: rather than reading
/// device.access_token_hash directly from the in-memory TokenBook, it is the authoritative value that
/// the caller read from the DB row. Those had been two independent sources of truth, even though they
/// are equal on every correct call path: TokenBook is always written immediately after this function's
/// DB transaction commits through book.commit_refresh, and no third write path exists. Passing the
/// authoritative DB value removes the shadow state of two independent values that ought to be equal.
/// room_id/k_pair/request_id seal the receipt ciphertext body within the same transaction (the journal
/// requires replaying the same receipt; sealing must happen after the new candidates are generated and
/// before the database write so response_ct/n can be stored together in the journal).
#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh_device_tokens(
    conn: &Connection,
    book: &mut TokenBook,
    device_id: &str,
    room_id: &str,
    prev_generation: i64,
    prev_access_hash: &str,
    k_pair: &[u8; 32],
    request_id: &str,
    refresh_token: &str,
    now_ms: u64,
) -> Result<RotatedTokens, String> {
    let candidate = book
        .prepare_refresh(device_id, refresh_token, now_ms)
        .map_err(|e| e.to_string())?;
    // Pre-rotation snapshot: prepare_refresh is read-only, and book is still in its "old" state here;
    // the journal's prev_refresh_hash field is taken directly from this snapshot, avoiding another DB
    // query for it (the caller has already supplied the access hash; see the function documentation).
    let device = book
        .devices
        .get(device_id)
        .ok_or_else(|| format!("remote device {device_id} disappeared mid refresh"))?;
    let prev_access_hash = prev_access_hash.to_owned();
    let prev_refresh_hash = device.refresh_token_hash.clone();

    let prev_expires_at_ms = refresh_prev_alias_expires_at_ms(now_ms)?;
    let (response_ct, response_n) = seal_token_refresh_ok(
        k_pair,
        room_id,
        device_id,
        request_id,
        &candidate.access_token,
        &candidate.refresh_token,
    );

    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let generation =
        db::next_registry_generation_in_transaction(&tx, room_id).map_err(|e| e.to_string())?;
    let refresh_until_ms = device_refresh_until_ms(now_ms)?;
    let access_expires_at_ms = i64::try_from(candidate.access_expires_at_ms)
        .map_err(|_| "access expiry exceeds SQLite INTEGER range".to_owned())?;

    let changed = db::update_remote_device_tokens(
        &tx,
        device_id,
        &candidate.access_token_hash,
        &candidate.refresh_token_hash,
        access_expires_at_ms,
    )
    .map_err(|e| e.to_string())?;
    if !changed {
        return Err(format!(
            "remote device {device_id} is missing or revoked; token refresh was not persisted"
        ));
    }
    if !db::set_remote_device_registry_in_transaction(
        &tx,
        device_id,
        room_id,
        generation,
        refresh_until_ms,
    )
    .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "remote device {device_id} disappeared during refresh registry write"
        ));
    }
    let journal = db::RemoteRefreshJournal {
        request_id: request_id.to_owned(),
        generation,
        prev_generation,
        prev_access_hash: prev_access_hash.clone(),
        prev_refresh_hash,
        response_ct: response_ct.clone(),
        response_n: response_n.clone(),
        prev_expires_at: prev_expires_at_ms,
        // No separate value is fixed for the receipt replay window; use the same source value as
        // refresh_prev_alias_expires_at_ms (see that function and the PREV_ALIAS_LIFETIME_MS comment for the
        // rationale), intentionally keeping the two windows synchronized rather than introducing a new
        // constant with a different value.
        response_expires: prev_expires_at_ms,
    };
    if !db::store_refresh_journal(&tx, device_id, &journal).map_err(|e| e.to_string())? {
        return Err(format!(
            "remote device {device_id} disappeared during refresh journal write"
        ));
    }
    tx.commit().map_err(|e| e.to_string())?;

    book.commit_refresh(device_id, &candidate);

    Ok(RotatedTokens {
        generation,
        access_token_hash: candidate.access_token_hash.clone(),
        access_expires_at_ms,
        refresh_until_ms,
        prev_generation,
        prev_access_hash,
        prev_expires_at_ms,
        response_ct,
        response_n,
    })
}
/// Rebuild TokenBook from non-revoked remote_devices rows (used to restore access/refresh validation
/// after a process restart). Construct TokenBook { devices } directly: store is a child module of
/// remote_pairing, so Rust privacy rules permit access to private fields defined by its ancestor
/// module. TokenBook does not need a public constructor solely for this module, which would touch the
/// pure-logic area.
pub(crate) fn load_token_book(conn: &Connection) -> Result<TokenBook, String> {
    let rows = db::list_remote_devices(conn).map_err(|e| e.to_string())?;
    let mut devices = HashMap::new();
    for row in rows {
        if row.revoked_at.is_some() {
            continue;
        }
        let Some(access_expires_at_ms) = u64::try_from(row.access_expires_at)
            .ok()
            .filter(|expires_at| *expires_at > 0)
        else {
            eprintln!(
                "remote_pairing::store::load_token_book: skipping device {}: non-positive access expiry",
                row.device_id
            );
            continue;
        };
        devices.insert(
            row.device_id.clone(),
            DeviceTokens {
                device_id: row.device_id,
                access_token_hash: row.token_hash,
                access_expires_at_ms,
                refresh_token_hash: row.refresh_hash,
                revoked: false,
            },
        );
    }
    Ok(TokenBook { devices })
}
/// Revoke a device: mark it in the DB and delete K_pair from the keychain. As established practice,
/// failure to delete K_pair is best-effort, so an intermittent keychain failure cannot prevent the
/// revocation itself from taking effect (the DB marker is the authoritative decision on whether this
/// device can still sign in). K_room rotation and rewrapping (the room key should change after a
/// revocation and be rewrapped for remaining devices) is intentionally not implemented here.
pub(crate) fn revoke_device(
    conn: &Connection,
    key_store: &dyn KeyStore,
    device_id: &str,
    now_secs: i64,
) -> Result<(), String> {
    db::revoke_remote_device(conn, device_id, now_secs).map_err(|e| e.to_string())?;
    if let Err(e) = key_store.delete(&k_pair_key_id(device_id)) {
        eprintln!(
            "remote_pairing::store::revoke_device: keychain delete failed for {device_id}: {e}"
        );
    }
    Ok(())
}

/// Immediately synchronize the current process's TokenBook after a successful DB revoke; an absent
/// in-memory device is handled as a no-op.
pub(crate) fn revoke_device_and_sync(
    conn: &Connection,
    key_store: &dyn KeyStore,
    book: &mut TokenBook,
    device_id: &str,
    now_secs: i64,
) -> Result<(), String> {
    revoke_device(conn, key_store, device_id, now_secs)?;
    book.revoke(device_id);
    Ok(())
}

#[cfg(test)]
mod tests;
