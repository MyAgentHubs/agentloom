use super::*;
pub(crate) type SettingsReader = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub(crate) type TokenProvider = Box<dyn Fn() -> Option<String> + Send + Sync>;
pub(crate) type DesktopCredentialProvider =
    Box<dyn Fn(&str) -> Result<Zeroizing<String>, String> + Send + Sync>;
pub(crate) type ClaimClient =
    Box<dyn Fn(&str, &str, &str) -> Result<ClaimResponse, String> + Send + Sync>;
pub(crate) type ActiveDeviceProvider = Box<dyn Fn(&str) -> Result<bool, String> + Send + Sync>;
/// project_id → this project's per-project room id. Ensure semantics (reuse an existing
/// room / create one when absent + credential idempotency first; see `remote_gateway_active_room_resolver`
/// in lib.rs for the production implementation). `current_config` should call this only when
/// remote is enabled && the active project is set—the caller owns the gate; this only resolves
/// a single project_id.
pub(crate) type ActiveRoomResolver = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;
pub(crate) type KRoomProvider = Box<dyn Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync>;
/// Provider contract: execution happens on the dedicated `remote-index-snapshot` thread, leaving
/// the WebSocket read thread with zero DB work. The provider must still hold the DB lock only for
/// the short read, never touch the keychain, network, or child processes, and return `None` on
/// failure without panicking.
pub(crate) type SessionIndexSnapshotProvider =
    Box<dyn Fn() -> Option<serde_json::Value> + Send + Sync>;
/// Provider contract mirrors `SessionIndexSnapshotProvider`: runs on the `remote-index-snapshot`
/// background thread, short DB read only, returns `None` on failure without panicking.
pub(crate) type MilestoneReplayProvider =
    Box<dyn Fn() -> Option<Vec<crate::db::MilestoneReplayRow>> + Send + Sync>;
/// Read current `session_runtime` rows for all sessions not soft-deleted under a short DB lock for replay after connection.
/// Current-state rows let `publish_milestone_replay_batch_on_connect` also place current-state
/// `run.status` frames into the replay batch (no new frame type; reuse the same frame construction
/// as `publish_run_status_milestone`). The provider contract is the same as above: run only on the
/// `remote-index-snapshot` background thread, return `None` on failure, and do not panic.
pub(crate) type SessionRuntimeReplayProvider =
    Box<dyn Fn() -> Option<Vec<crate::db::SessionRuntimeReplayRow>> + Send + Sync>;
/// session → owning repo id lookup (`Ok(None)` = the session does not exist / has no owner;
/// the ownership gate handles both identically—neither belongs to any active repo). `Err` = the
/// lookup itself failed (DB error); callers always fail closed rather than treating "unknown" as
/// allowed. See `remote_gateway_session_repo_provider` in lib.rs for the production implementation
/// (a wrapper around `db::get_session_repo_id`). The `RoomSource` enum was removed together with
/// legacy global-room fallback, and the ownership gate is always enabled under the
/// single-active-room model—as long as a connection exists (`active_repo_id_for_gating` is
/// always `Some`), it is always called; there is no longer a short-circuit switch that calls it
/// only for a particular room source.
pub(crate) type SessionRepoProvider =
    Box<dyn Fn(&str) -> Result<Option<String>, String> + Send + Sync>;
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionHistoryRow {
    pub message_id: i64,
    pub role: String,
    pub content_json: Value,
    /// Preserve the original DB `content` string alongside `content_json` so hashes and byte counts use stored bytes rather than reserialized JSON.
    /// The content_ref sha256/total_bytes must be calculated over these original bytes, not the
    /// reserialized `content_json` (`Value`'s internal `Map` sorts by key by default, so its bytes
    /// are not guaranteed to match the original text).
    pub content_raw: String,
    /// Current `messages.revision` for this message, preserving the stored revision in history responses.
    pub revision: i64,
}
/// Short-lock DB provider for `control.history`: results remain in `message_id DESC` order; the
/// paging layer handles budget convergence and the ascending-order reversal required on the wire.
/// Errors must be returned explicitly so the command reports failed instead of sending half a page.
pub(crate) type SessionHistoryProvider =
    Box<dyn Fn(&str, Option<i64>, i64) -> Result<Vec<SessionHistoryRow>, String> + Send + Sync>;

/// DB provider for the first `msg.fetch` authorization check: look up the exact `(session, message_id)` pair and fail closed on lookup errors.
/// The `(session, message_id)` lookup fetches one complete message. Its three states mirror
/// `db::MessageForFetch` (the production `remote_gateway_message_fetch_provider` in lib.rs wraps
/// `db::get_message_for_fetch`). This defines a separate type instead of directly referencing the
/// db.rs type, following the existing `SessionHistoryRow`/`SessionRepoProvider` convention, so
/// `remote_gateway.rs` tests can construct any of the three states without depending on db.rs.
/// `Err` = the lookup itself failed (DB error), which callers handle fail closed (as with the
/// existing `SessionRepoProvider`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MessageForFetchResult {
    /// The message belongs to this session; `content_raw` is the original DB `content` string
    /// (`content_ref.content_sha256` / `msg.chunk` slicing must use these original bytes, never a
    /// deserialized/reserialized value); `session_deleted` = whether its owning session is soft-deleted.
    Found {
        content_raw: String,
        revision: i64,
        session_deleted: bool,
    },
    /// The `message_id` exists but does not belong to the `session` claimed by the caller (unauthorized).
    WrongSession,
    /// The `message_id` does not exist at all.
    NotFound,
}
pub(crate) type MessageFetchProvider =
    Box<dyn Fn(&str, i64) -> Result<MessageForFetchResult, String> + Send + Sync>;
pub(crate) type PairHelloHandler =
    Box<dyn Fn(PairHelloFrame) -> Option<PairAcceptFrame> + Send + Sync>;
pub(crate) type PairDoneHandler = Box<dyn Fn(PairDoneFrame) -> PairDoneAction + Send + Sync>;

/// Relay-stamped `token.refresh.forward`—both `subject` and `request_generation` are
/// written by the relay, not claimed by the phone itself.
pub(crate) struct RefreshForwardFrame {
    pub request_id: String,
    pub subject: String,
    /// Used by the relay for its own `refresh_requests` delivery accounting; desktop-side decisions
    /// only recognize which hash matches the refresh_token decrypted from the ciphertext and do not
    /// use this field as a decision input (the relay already checked it in its own delivery predicate;
    /// repeating the check on the desktop adds no security benefit and only creates a new divergence
    /// point when the two sides use different criteria).
    pub request_generation: i64,
    pub ct: String,
    pub n: String,
}
/// Refresh receipt to replay after token.ack arrives—attached to the corresponding outbox
/// `token.put` item and emitted when the ack is consumed (the fixed
/// "put → ack → receipt" order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefreshOkFrame {
    pub request_id: String,
    pub subject: String,
    pub generation: i64,
    pub ct: String,
    pub n: String,
}

/// Immediate refresh-handler result: `Reply` returns a frame immediately (an idempotently replayed
/// ok / any fail); `Pending` means token.put has been attached to the outbox and the actual
/// token.refresh.ok must wait for the ack before it is sent.
pub(crate) enum RefreshOutcome {
    Reply(Value),
    Pending,
}

pub(crate) type RefreshHandler = Box<dyn Fn(RefreshForwardFrame) -> RefreshOutcome + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenSyncCurrent {
    pub token_hash: String,
    pub access_expires: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_until: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenSyncPrev {
    pub token_hash: String,
    pub generation: i64,
    pub prev_expires: i64,
}

/// Per-item wire shape of token.put after removing `t`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenSyncEntry {
    pub subject: String,
    pub generation: i64,
    pub scope: String,
    pub current: TokenSyncCurrent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<TokenSyncPrev>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistrySnapshot {
    pub revision: i64,
    pub entries: Vec<TokenSyncEntry>,
}

pub(crate) type RegistrySnapshotProvider =
    Box<dyn Fn(&str, u64) -> Result<RegistrySnapshot, String> + Send + Sync>;
/// The fifth parameter = the list of revoke (token.delete) subjects still awaiting delivery
/// (`pending_revoke_subjects`); the new third tuple item returned = the generations newly allocated
/// to those subjects in the same DB transaction (rebase resend).
pub(crate) type RegistryRebaseProvider = Box<
    dyn Fn(
            &str,
            i64,
            u64,
            bool,
            &[String],
        ) -> Result<(RegistrySnapshot, Option<i64>, Vec<(String, i64)>), String>
        + Send
        + Sync,
>;
/// The third parameter = the list of revoke (token.delete) subjects still awaiting delivery
/// (not acked, including rejected ones; `pending_revoke_subjects`). After callers
/// unconditionally absorb sync.ack's `relay_high_water` into the desktop counter (counter
/// absorption), the same implementation must immediately allocate a new generation to each of
/// these subjects and return it unchanged—the new generations are therefore guaranteed to be
/// strictly greater than the counter floor after this absorption, naturally satisfying "a delete
/// generation must be strictly greater than this sync's revision and also greater than the
/// relay_high_water reported by the relay."
pub(crate) type RegistryHighWaterProvider =
    Box<dyn Fn(&str, i64, &[String]) -> Result<Vec<(String, i64)>, String> + Send + Sync>;
