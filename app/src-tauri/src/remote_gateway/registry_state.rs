use super::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RegistryOutboxItem {
    pub(super) subject: String,
    pub(super) generation: i64,
    pub(super) frame: Value,
    /// Used only for statistics and diagnostics. `drain_registry_outbox` increments it before each
    /// actual send. The real send barriers are `last_sent_at`, which prevents resending before an
    /// ack, and `rejected`, which stops all sends except revocations. Only test assertions read it;
    /// it does not gate any sending logic.
    pub(super) attempts: u32,
    pub(super) last_sent_at: Option<u64>,
    pub(super) acked: bool,
    pub(super) rejected: bool,
    pub(super) pair_ready: Option<PairReadyFrame>,
    /// The refresh response attached to this `token.put` after a successful rotation. It is never
    /// sent before the ack arrives, preserving the fixed put-to-ack-to-response order, and
    /// `consume_token_ack` returns it to the caller. It is attached like `pair_ready`; the two are
    /// mutually exclusive because a subject's given put is either a pairing grant or a refresh
    /// rotation.
    pub(super) refresh_ok: Option<RefreshOkFrame>,
}

/// An element returned by `RegistryState::outbox_snapshot_for_test`: a read-only, test-only
/// snapshot. Each field is deliberately cloned instead of exposing all of `RegistryOutboxItem` as
/// `pub(crate)`, preventing external code from reaching into or modifying outbox state.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutboxItemSnapshot {
    pub(crate) frame: Value,
    pub(crate) generation: i64,
    pub(crate) attempts: u32,
    pub(crate) acked: bool,
    pub(crate) rejected: bool,
}

/// The serialized registry domain. The lock order is `registry -> db -> pairing slot`: all
/// grant, rotate, revoke, pairing begin/cancel/done, snapshot, and rebase paths must acquire this
/// `Arc<Mutex<_>>` first. Never acquire the registry from the DB or pairing slot in reverse order;
/// the DB, slot, or outbox may be changed only after this lock is held.
#[derive(Default)]
pub(crate) struct RegistryState {
    pub(super) pairing_entry: Option<TokenSyncEntry>,
    pub(super) outbox: VecDeque<RegistryOutboxItem>,
    acknowledged_pair_ready: HashMap<String, PairReadyFrame>,
    staged_pairing_k_room: Option<Zeroizing<[u8; 32]>>,
    /// Per-subject timestamps for successful rotations in a sliding one-hour window, plus a count
    /// of consecutive invalid attempts. This state deliberately resets on process restart to avoid
    /// a DB migration for one throttling counter. The first hour after a restart is therefore
    /// briefly more permissive, not stricter; this is acceptable because it is abuse throttling,
    /// not a security boundary.
    refresh_quota: HashMap<String, RefreshQuotaEntry>,
    /// When the relay rejects a put carrying a refresh response, the desktop DB has already rotated
    /// irreversibly to the new generation while the relay still has the old one. Returning
    /// `token.refresh.fail{reason:"put_rejected"}` tells the phone to retry but cannot advance the
    /// relay generation, so retrying the old refresh with the same request ID still fails the relay
    /// delivery predicate. This flag triggers active convergence: `consume_token_ack` sets it for
    /// `RefreshDropped`, then the connection loop takes and clears it after the current frame. Each
    /// rejected event can trigger convergence at most once. Convergence disconnects and lets the
    /// existing reconnect path perform its initial sync instead of resending `token.sync` on the
    /// live connection. The disconnect occurs on that read timeout or after the pending state
    /// exceeds the two-second hard deadline, avoiding a hot loop; registry synchronization also
    /// bounds its own rebase rounds.
    resync_required: bool,
}

#[derive(Default)]
struct RefreshQuotaEntry {
    successful_rotations_ms: VecDeque<u64>,
    consecutive_invalid: u32,
}

/// Six successful rotations per hour per subject.
const REFRESH_QUOTA_MAX_PER_HOUR: usize = 6;
const REFRESH_QUOTA_WINDOW_MS: u64 = 3_600_000;
/// Three or more consecutive invalid attempts produce `close:true`.
const REFRESH_INVALID_CLOSE_THRESHOLD: u32 = 3;

impl RegistryState {
    pub(crate) fn set_pairing_entry(&mut self, entry: TokenSyncEntry) {
        self.staged_pairing_k_room = None;
        self.pairing_entry = Some(entry);
        // `pairing` is the only reused subject: each pairing round gets a new UUID, but the registry
        // subject remains "pairing". An unsent token.delete queued when the previous round was
        // canceled must not remain in the outbox. The `enqueue_outbox` exception discards every
        // later put when a delete for the same subject is queued, so the new put would never enter
        // the outbox. On reconnect, the stale delete could instead receive a generation above the
        // current sync revision and immediately revoke the new pairing round. The relay checks the
        // subject's current generation on every inbound message and closes old connections whose
        // generation no longer matches. A new put also rebuilds all token aliases for the subject,
        // naturally invalidating the old pairing token. The authorization gate removes the old
        // socket's authority, so discarding the stale delete is safe.
        self.outbox
            .retain(|item| item.subject != "pairing" || !is_delete_frame(&item.frame));
    }

    pub(crate) fn clear_pairing_entry(&mut self) {
        self.pairing_entry = None;
        self.outbox.retain(|item| {
            item.subject != "pairing"
                || item.frame.get("t").and_then(Value::as_str) != Some("token.put")
        });
    }

    pub(crate) fn discard_staged_pairing_k_room(&mut self) {
        self.staged_pairing_k_room = None;
    }

    pub(super) fn active_pairing_entry(&self, now_ms: u64) -> Option<TokenSyncEntry> {
        self.pairing_entry
            .as_ref()
            .filter(|entry| {
                u64::try_from(entry.current.access_expires)
                    .is_ok_and(|expires_ms| now_ms < expires_ms)
            })
            .cloned()
    }

    pub(super) fn enqueue_outbox(&mut self, item: RegistryOutboxItem) {
        // Independent revoke retry exception: once a subject has a queued token.delete, discard all
        // later token.put entries for that subject. This neither replaces nor cancels the queued
        // revocation, and a new put cannot revive the device. A revoke item is itself a delete, so
        // this exception does not apply to it; it follows the normal generation-raising logic below.
        // Therefore queuing a revoke naturally cancels an unsent put for the same subject because
        // the revoke has the higher generation.
        if !is_delete_frame(&item.frame)
            && self
                .outbox
                .iter()
                .any(|queued| queued.subject == item.subject && is_delete_frame(&queued.frame))
        {
            return;
        }
        if self
            .outbox
            .iter()
            .any(|queued| queued.subject == item.subject && queued.generation >= item.generation)
        {
            return;
        }
        self.outbox
            .retain(|queued| queued.subject != item.subject || queued.generation > item.generation);
        self.acknowledged_pair_ready.remove(&item.subject);
        self.outbox.push_back(item);
    }

    pub(crate) fn enqueue_token_put(
        &mut self,
        entry: TokenSyncEntry,
        pair_ready: Option<PairReadyFrame>,
    ) {
        self.enqueue_outbox(RegistryOutboxItem {
            subject: entry.subject.clone(),
            generation: entry.generation,
            frame: token_put_frame(&entry),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready,
            refresh_ok: None,
        });
    }

    /// Attaches `refresh_ok` to the `token.put` after a successful rotation and sends the response
    /// only after the ack arrives, preserving the fixed ordering.
    pub(crate) fn enqueue_token_put_for_refresh(
        &mut self,
        entry: TokenSyncEntry,
        refresh_ok: RefreshOkFrame,
    ) {
        self.enqueue_outbox(RegistryOutboxItem {
            subject: entry.subject.clone(),
            generation: entry.generation,
            frame: token_put_frame(&entry),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: Some(refresh_ok),
        });
    }

    pub(crate) fn enqueue_token_delete(&mut self, subject: String, generation: i64, close: bool) {
        self.enqueue_outbox(RegistryOutboxItem {
            subject: subject.clone(),
            generation,
            frame: token_delete_frame(&subject, generation, close),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: None,
        });
    }

    /// A test-only read-only view of each outbox item's key dimensions, in queue order: frame,
    /// generation, attempts, acked, and rejected. Do not expose the `outbox` field as `pub(crate)`;
    /// that would let external code bypass methods such as `enqueue_outbox` and
    /// `consume_token_ack`, violating invariants protected by private fields in the serialized
    /// registry domain. This narrow cloning accessor exists only for integration-test assertions.
    /// The earlier `outbox_frames_for_test` exposed only the frame, so assertions could not inspect
    /// generation, attempts, acked, or rejected; this accessor includes them and has been renamed.
    #[cfg(test)]
    pub(crate) fn outbox_snapshot_for_test(&self) -> Vec<OutboxItemSnapshot> {
        self.outbox
            .iter()
            .map(|item| OutboxItemSnapshot {
                frame: item.frame.clone(),
                generation: item.generation,
                attempts: item.attempts,
                acked: item.acked,
                rejected: item.rejected,
            })
            .collect()
    }

    /// Subjects of revoke (`token.delete`) items still awaiting delivery, including rejected items
    /// that have not been acked. Rebase and reconnect use this list to obtain new generations from
    /// the DB, rebuild the delete frames, and send them. Revocations retry independently until ack,
    /// so a rejected revoke remains pending, unlike a rejected put, which stops sending. Excluding
    /// it would prevent a revocation rejected once by the relay from obtaining a new generation and
    /// retrying.
    pub(super) fn pending_revoke_subjects(&self) -> Vec<String> {
        self.outbox
            .iter()
            .filter(|item| !item.acked && is_delete_frame(&item.frame))
            .map(|item| item.subject.clone())
            .collect()
    }

    pub(super) fn prepare_outbox_for_reconnect(&mut self) {
        for item in &mut self.outbox {
            if !item.acked && !item.rejected {
                item.last_sent_at = None;
            }
        }
    }

    /// Called after the connection loop processes the current frame to take and clear the
    /// convergence gate. The read-and-clear semantics are the gate itself: a given `RefreshDropped`
    /// can produce at most one convergence intent and cannot retrigger without a new rejected event.
    /// When true, the loop records that the connection should close instead of resending
    /// `token.sync` in place. It disconnects only when the current read times out or the pending
    /// state exceeds the two-second hard deadline, allowing the existing reconnect path to sync on
    /// the next connection.
    pub(super) fn take_resync_required(&mut self) -> bool {
        std::mem::take(&mut self.resync_required)
    }

    pub(crate) fn consume_token_ack(
        &mut self,
        subject: &str,
        generation: i64,
        result: &str,
    ) -> TokenAckAction {
        let Some(index) = self
            .outbox
            .iter()
            .position(|item| item.subject == subject && item.generation == generation)
        else {
            return TokenAckAction::Ignored;
        };
        if result == "rejected" {
            self.outbox[index].rejected = true;
            // When the relay rejects a put, the desktop DB and TokenBook have already completed the
            // rotation. If the item carries a refresh response, it cannot be silently consumed like
            // other rejected items, which would stop forever without notifying the phone. Take the
            // response and let `handle_frame` immediately return
            // token.refresh.fail{reason:"put_rejected"}, so the phone can retry with the old refresh
            // instead of waiting for an ok/fail response the relay will never deliver. The item
            // remains marked rejected, preserving the drain barrier and rebase cleanup behavior.
            //
            // Returning only the fail frame still leaves the relay registry on the old generation,
            // because the relay rejected the rotation itself, so phone retries cannot advance it.
            // Also set the convergence gate so the connection loop can deliver the DB's new
            // generation to the relay through synchronization.
            if let Some(refresh_ok) = self.outbox[index].refresh_ok.take() {
                self.resync_required = true;
                return TokenAckAction::RefreshDropped {
                    request_id: refresh_ok.request_id,
                    subject: refresh_ok.subject,
                };
            }
            return TokenAckAction::Rejected;
        }
        let mut item = self
            .outbox
            .remove(index)
            .expect("outbox index was found immediately before removal");
        item.acked = true;
        if let Some(ready) = item.pair_ready {
            self.acknowledged_pair_ready
                .insert(subject.to_owned(), ready.clone());
            return TokenAckAction::PairReady(ready);
        }
        match item.refresh_ok {
            Some(refresh_ok) => TokenAckAction::RefreshOk(refresh_ok),
            None => TokenAckAction::Consumed,
        }
    }

    pub(crate) fn acknowledged_pair_ready(&self, subject: &str) -> Option<PairReadyFrame> {
        self.acknowledged_pair_ready.get(subject).cloned()
    }

    /// A valid pair.done replay is the explicit retry trigger for a device grant whose put has
    /// timed out or was rejected. Preserve the exact grant/ready payload and retry accounting;
    /// only reopen the send barrier for the next live-socket drain.
    pub(crate) fn replay_pair_ready(&mut self, subject: &str) -> Option<PairReadyFrame> {
        if let Some(ready) = self.acknowledged_pair_ready(subject) {
            return Some(ready);
        }
        if let Some(item) = self
            .outbox
            .iter_mut()
            .find(|item| item.subject == subject && item.pair_ready.is_some())
        {
            item.acked = false;
            item.rejected = false;
            item.last_sent_at = None;
        }
        None
    }

    pub(super) fn stage_pairing_k_room(&mut self, k_room: Zeroizing<[u8; 32]>) {
        self.staged_pairing_k_room = Some(k_room);
    }

    pub(super) fn pairing_k_room_is_staged(&self) -> bool {
        self.staged_pairing_k_room.is_some()
    }

    pub(super) fn take_staged_pairing_k_room(&mut self) -> Option<Zeroizing<[u8; 32]>> {
        self.staged_pairing_k_room.take()
    }

    /// `revoke_generations` contains `(subject, new generation)` pairs only for revoke items still
    /// awaiting delivery in the outbox, as produced by `pending_revoke_subjects`. Revoke items are
    /// absent from `entries` because the DB snapshot omits revoked devices, so they require a
    /// separate second matching pass. `close` is always true because rebase does not weaken the
    /// revocation intent.
    ///
    /// Rejected puts still stop and are discarded because `retain` removes them. A rejected revoke
    /// (`token.delete`) must not be removed by the same operation: revocations retry independently
    /// until ack, so they remain in the outbox for `rearm_revoke_entries` to rearm with a new
    /// generation and clear the rejected state.
    pub(super) fn rebase_outbox_entries(
        &mut self,
        entries: &[TokenSyncEntry],
        revoke_generations: &[(String, i64)],
    ) {
        self.outbox
            .retain(|item| !item.rejected || is_delete_frame(&item.frame));
        for entry in entries {
            let Some(item) = self.outbox.iter_mut().find(|item| {
                item.subject == entry.subject
                    && item.generation < entry.generation
                    && !is_delete_frame(&item.frame)
            }) else {
                continue;
            };
            item.generation = entry.generation;
            item.frame = token_put_frame(entry);
            item.last_sent_at = None;
            item.acked = false;
            self.acknowledged_pair_ready.remove(&entry.subject);
            // Rebase must update more than the generation and frame. The attached refresh response
            // (`refresh_ok`, attached by `enqueue_token_put_for_refresh`) still has the generation
            // from the original rotation. The relay delivers only when the response generation
            // equals the subject's current generation, so it drops the stale response after rebase.
            // Retrying with the old refresh would then return the same stale response and loop for
            // the entire 48-hour journal window. Generation is not part of the five-field AAD, so
            // changing it does not affect ciphertext authentication; `ct` and `n` remain unchanged.
            // Updating it is the response-side counterpart of changing the put frame's generation.
            if let Some(ok) = item.refresh_ok.as_mut() {
                ok.generation = entry.generation;
            }
        }
        self.rearm_revoke_entries(revoke_generations);
    }

    /// Applies `revoke_generations`, a list of `(subject, new generation)` pairs, to matching
    /// token.delete items in the outbox. It changes the generation, rebuilds the frame, and rearms
    /// the item for sending with `last_sent_at = None` and `acked = false`. It also clears
    /// `rejected`: a revocation rejected once by the relay must restart with a new generation rather
    /// than stop forever or be removed by the next rebase cleanup. Missing matches, whether already
    /// acked or never queued, are silently skipped and are not errors.
    pub(super) fn rearm_revoke_entries(&mut self, revoke_generations: &[(String, i64)]) {
        for (subject, generation) in revoke_generations {
            let Some(item) = self
                .outbox
                .iter_mut()
                .find(|item| &item.subject == subject && is_delete_frame(&item.frame))
            else {
                continue;
            };
            item.generation = *generation;
            item.frame = token_delete_frame(subject, *generation, true);
            item.last_sent_at = None;
            item.acked = false;
            item.rejected = false;
        }
    }

    pub(super) fn cancel_outbox_before_reset(&mut self) {
        self.outbox.clear();
    }

    /// Enforces six successful rotations per hour per subject using a sliding window. The check is
    /// read-only and does not consume quota; pruning occurs only when
    /// `record_refresh_rotation_success` records an actual success. Callers use this before
    /// refreshing device tokens to decide whether to proceed.
    pub(crate) fn refresh_quota_exceeded(&self, subject: &str, now_ms: u64) -> bool {
        self.refresh_quota.get(subject).is_some_and(|entry| {
            refresh_quota_window_count(entry, now_ms) >= REFRESH_QUOTA_MAX_PER_HOUR
        })
    }

    /// Records a successful rotation: removes timestamps older than one hour from the sliding
    /// window, appends the current timestamp, and clears the consecutive-invalid count. A successful
    /// rotation proves this was not an invalid or hostile attempt.
    pub(crate) fn record_refresh_rotation_success(&mut self, subject: &str, now_ms: u64) {
        let entry = self.refresh_quota.entry(subject.to_owned()).or_default();
        entry
            .successful_rotations_ms
            .retain(|&sent_at| now_ms.saturating_sub(sent_at) < REFRESH_QUOTA_WINDOW_MS);
        entry.successful_rotations_ms.push_back(now_ms);
        entry.consecutive_invalid = 0;
    }

    /// An idempotent replay that matches the previous value and the journal request ID does not
    /// consume quota, but still proves the request is valid and clears the consecutive-invalid
    /// count. This is a no-op when the subject has never had accounting state.
    pub(crate) fn record_refresh_replay(&mut self, subject: &str) {
        if let Some(entry) = self.refresh_quota.get_mut(subject) {
            entry.consecutive_invalid = 0;
        }
    }

    /// Records an invalid request: decryption failed, the hash matched neither the current value nor
    /// an unexpired previous value, or the device is unknown or revoked. Returns true when the
    /// consecutive-invalid count reaches `REFRESH_INVALID_CLOSE_THRESHOLD`, requiring the response
    /// to include `close:true`. Benign single-flight `in_flight` conflicts and quota excess do not
    /// use this path because neither is invalid.
    pub(crate) fn record_refresh_invalid(&mut self, subject: &str) -> bool {
        let entry = self.refresh_quota.entry(subject.to_owned()).or_default();
        entry.consecutive_invalid = entry.consecutive_invalid.saturating_add(1);
        entry.consecutive_invalid >= REFRESH_INVALID_CLOSE_THRESHOLD
    }

    /// Test-only count of subject entries currently in the `refresh_quota` map, used to verify that
    /// a missing subject does not create an entry. Production code gets no read-only length accessor
    /// for the actual in-memory map, preventing callers from using its length for product decisions.
    #[cfg(test)]
    pub(crate) fn refresh_quota_entry_count_for_test(&self) -> usize {
        self.refresh_quota.len()
    }
}

fn refresh_quota_window_count(entry: &RefreshQuotaEntry, now_ms: u64) -> usize {
    entry
        .successful_rotations_ms
        .iter()
        .filter(|&&sent_at| now_ms.saturating_sub(sent_at) < REFRESH_QUOTA_WINDOW_MS)
        .count()
}

pub(super) fn token_put_frame(entry: &TokenSyncEntry) -> Value {
    let mut frame = serde_json::to_value(entry)
        .expect("TokenSyncEntry serialization contains no fallible custom serializer");
    frame
        .as_object_mut()
        .expect("TokenSyncEntry serializes as an object")
        .insert("t".to_owned(), Value::String("token.put".to_owned()));
    frame
}

/// The field-by-field `token.delete` wire shape, aligned with
/// `fixtures/wire-v1.json`'s `token_delete_close_valid` case.
fn token_delete_frame(subject: &str, generation: i64, close: bool) -> Value {
    serde_json::json!({
        "t": "token.delete",
        "subject": subject,
        "generation": generation,
        "close": close,
    })
}

/// Distinguishes revocation intents (`token.delete`) in the outbox for exception and rebase paths.
fn is_delete_frame(frame: &Value) -> bool {
    frame.get("t").and_then(Value::as_str) == Some("token.delete")
}
