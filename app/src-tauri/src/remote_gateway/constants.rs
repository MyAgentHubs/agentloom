use std::time::Duration;

/// Official public relay used when the relay address is empty. A user-provided `wss://`
/// address overrides it.
pub(crate) const DEFAULT_PUBLIC_RELAY_URL: &str = "wss://agentloom.myagenthubs.com";

/// Falls back to the official public relay when the address is `None` or blank after
/// trimming. A non-empty address is returned unchanged; callers own their trimming policy.
pub(crate) fn effective_relay_url(raw: Option<String>) -> Option<String> {
    match raw {
        Some(value) if !value.trim().is_empty() => Some(value),
        _ => Some(DEFAULT_PUBLIC_RELAY_URL.to_owned()),
    }
}
pub(super) const SETTINGS_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub(super) const READ_TIMEOUT: Duration = Duration::from_millis(500);
pub(super) const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

// Hard deadline for disconnecting after a rejected refresh put. It is guaranteed only when
// each `socket.read()` returns a complete frame; incomplete fragments can also block later
// shutdown, liveness, and `drain_upstream` checks inside tungstenite.
pub(super) const REGISTRY_RESYNC_DRAIN_DEADLINE: Duration = Duration::from_secs(2);
pub(super) const BACKOFF_POLL_INTERVAL: Duration = Duration::from_millis(200);
pub(super) const WS_MAX_REDIRECTS: u8 = 0;
pub(super) const ROOM_CLAIM_CONFLICT_STOP_ERROR: &str =
    "房间归属被占；为保已配对设备不自动换房，远程控制已停机";
pub(super) const ROOM_CLAIM_CONFLICT_STOP_REASON: &str = "room_claim_conflict";

/// Stops a per-project room claim conflict when no known devices are found. Automatic room
/// replacement cannot work in the single-active-room model because `current_config` would resolve
/// the same room again, so this branch stops without a retry or replacement loop.
pub(super) const ROOM_CLAIM_CONFLICT_PROJECT_STOP_ERROR: &str =
    "房间归属被占；per-project 房间不支持自动换房，远程控制已停机——请到 Settings 重新配对该项目";
pub(super) const ROOM_CLAIM_CONFLICT_PROJECT_STOP_REASON: &str = "room_claim_conflict_project";
pub(super) const ROOM_TOMBSTONED_STOP_ERROR: &str = "房间已在服务端终结（410），远程控制已停机";
pub(super) const ROOM_TOMBSTONED_STOP_REASON: &str = "room_tombstoned";
pub(super) const ROOM_DEVICE_STATUS_UNAVAILABLE_STOP_REASON: &str =
    "room_device_status_unavailable";
pub(super) const REGISTRY_REBASE_LIMIT_STOP_REASON: &str = "registry_rebase_limit";
pub(super) const REGISTRY_REBASE_LIMIT_STOP_ERROR: &str =
    "relay 注册表高水位连续抬升，远程控制已停机";
pub(super) const MAX_REGISTRY_REBASES: u8 = 3;

/// Maximum span by which `relay_high_water` may exceed local `snapshot.revision`. A million
/// generations leaves room for honest rebases while bounding faulty-relay damage far below
/// `i64::MAX` and preventing `next_generation` from approaching overflow.
pub(super) const REGISTRY_HIGH_WATER_MAX_SPAN: i64 = 1_000_000;
pub(super) const REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON: &str =
    "registry_high_water_out_of_range";
pub(super) const REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_ERROR: &str =
    "relay 报回的注册表高水位远超本地计数器，远程控制已停机";
pub(super) const MAX_REGISTRY_SYNC_ENTRIES: usize = 256;
pub(super) const MAX_REGISTRY_FRAME_BYTES: usize = 64 * 1024;
pub(super) const MAX_INCOMING_BYTES: usize = 1024 * 1024;
pub(super) const DEFAULT_LIVENESS_INTERVAL: Duration = Duration::from_secs(5);
/// Protocol Ping cadence for defeating middlebox idle timeouts. Production links were observed
/// being cut after roughly 390 seconds of silence; 30 seconds leaves ample safety margin.
pub(super) const KEEPALIVE_IDLE_INTERVAL: Duration = Duration::from_secs(30);
/// Preserve immediate reconnects for ordinary config changes, but stop transient resolver/DB
/// failures from creating an unbounded ConfigStale reconnect storm.
pub(super) const CONFIG_STALE_GUARD_WINDOW: Duration = Duration::from_secs(30);
pub(super) const CONFIG_STALE_GUARD_THRESHOLD: u32 = 3;
pub(super) const CLOSE_DRAIN_ATTEMPTS: u8 = 8;
pub(super) const UPSTREAM_CAPACITY: usize = 1024;
pub(super) const TOOL_CORRELATION_CAPACITY: usize = 1024;

/// Capacity limit for `partial_snapshots`. Reaching it rejects only new session entries to
/// prevent leaks and does not stop updates to sessions already being tracked.
pub(super) const PARTIAL_SNAPSHOT_CAPACITY: usize = 128;
pub(super) const OUTPUT_TRUNCATE_BYTES: usize = 2048;

/// Serialized-frame budget after `build_snapshot_payload` converges. It leaves room for base64,
/// the AEAD tag, and JSON escaping below the relay limit. Measurement includes the complete
/// plaintext payload and its `t` field, so overflow removes the oldest blocks first.
pub(super) const SNAPSHOT_PAYLOAD_BUDGET_BYTES: usize = 32 * 1024;

/// Send-side fallback expressed as a plaintext budget after envelope expansion. A wire frame is
/// empirically about 1.37 times the plaintext payload plus a fixed header, so this leaves margin
/// below `MAX_REGISTRY_FRAME_BYTES` as a last-resort fuse.
pub(super) const SNAPSHOT_SEND_BUDGET_BYTES: usize = 44 * 1024;

/// Plaintext safety margin for `control.history` responses and snapshots below the relay limit,
/// accounting for base64, the AEAD tag, and JSON envelope expansion.
pub(super) const HISTORY_SEND_BUDGET_BYTES: usize = 44 * 1024;
pub(super) const HISTORY_PAGE_MAX_ROWS: usize = 50;

/// Truncation notice inserted at the front of `blocks` after convergence drops blocks. It reuses
/// `db::Block::Text`; `snapshot_truncated_notice_text` adds the removed-block count.
pub(super) const SNAPSHOT_TRUNCATED_NOTICE: &str = "（快照已截断，仅含最近内容）";

/// Retained text prefix in oversized `msg.completed` and history previews, measured in UTF-8
/// bytes to match the content reference's `total_bytes`, rather than in characters.
pub(super) const OVERSIZED_PREVIEW_TEXT_HEAD_BYTES: usize = 512;

/// Appended after the retained preview prefix. It must match `blocks[0].text` in the
/// `msg_completed_with_content_ref` canonical fixture byte for byte.
pub(super) const OVERSIZED_PREVIEW_TRUNCATION_NOTICE: &str =
    "内容较长，已截断——点击加载全文查看完整报告。";

/// Private staging key for the content reference built by `build_msg_completed_payload`.
/// `enqueue_milestone_item` must remove it before measurement or transmission. It carries data
/// until `content_ref` is attached only when content is actually reduced to a preview.
pub(super) const MSG_COMPLETED_REF_SOURCE_KEY: &str = "__msgfix1_content_ref_source";
pub(super) const MAX_DRAIN_ITEMS_PER_ROUND: usize = 64;
pub(super) const DRAIN_ROUND_BUDGET: Duration = Duration::from_millis(250);

// Maximum JSON safe integer (2^53 - 1).
pub(super) const JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
pub(super) const COMMAND_ID_MAX_LEN: usize = 128;

/// Desktop-side defense-in-depth guard matching the relay's 128-byte `session` limit. It uses
/// UTF-8 bytes from `str::len()` and fails before `command_session_allowed`, because malformed
/// identifiers do not require an ownership lookup.
pub(super) const SESSION_ID_MAX_BYTES: usize = 128;

// Hard upper limit for the `control.stop` lifetime: 30 seconds.
pub(super) const CONTROL_STOP_MAX_LIFETIME_MS: u64 = 30_000;

// Clock-skew tolerance window: two minutes.
pub(super) const CONTROL_STOP_SKEW_MS: u64 = 120_000;
pub(super) const CLIENT_MSG_ID_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0xfe4e51ad_468c_4c11_85c2_f15f0c22f030);

/// Maximum `total_bytes` for a `msg.fetch` target. Larger messages return `too_large` without
/// entering the chunking path.
pub(super) const MSG_FETCH_TOTAL_BYTES_LIMIT: usize = 4 * 1024 * 1024;

/// Raw content bytes per `msg.chunk`, selected from measured worst-case encrypted wire frames.
/// At 24 KiB, a frame retains more than 10% margin below the 64 KiB relay limit after double
/// base64 expansion, AEAD overhead, maximum protocol fields, and JSON serialization.
pub(super) const CHUNK_RAW_BYTES: usize = 24 * 1024;

/// Reply queue capacity. A full 4 MiB fetch produces 171 chunks at `CHUNK_RAW_BYTES`; 256 slots
/// cover that fetch with margin for a prior timed-out fetch whose chunks have not fully drained.
pub(super) const REPLY_QUEUE_CAPACITY: usize = 256;

/// Maximum in-flight `msg.fetch` lifetime. A full fetch needs at least three drain rounds, or
/// about 1.5 seconds when driven only by read timeouts; 30 seconds leaves network and relay margin
/// while ensuring a stalled fetch eventually releases its session slot.
pub(super) const MSG_FETCH_INFLIGHT_TIMEOUT_MS: u64 = 30_000;

/// Gateway-wide 60-second fetch-byte budget. Global accounting prevents concurrent sessions from
/// multiplying the connection allowance. Eight MiB of plaintext stays below the relay's 16 MiB
/// wire budget after expansion and permits two full-size fetches before returning `busy`.
pub(super) const MSG_FETCH_BYTE_BUDGET_PER_WINDOW: u64 = 8 * 1024 * 1024;
pub(super) const MSG_FETCH_BYTE_BUDGET_WINDOW_MS: u64 = 60_000;

/// Bounds `msg_fetch_command_ledger` while retaining recent command identities. The 512-entry
/// FIFO comfortably covers normal paging and retries; eviction only permits reuse of an old
/// command ID and is not a security boundary.
pub(super) const MSG_FETCH_COMMAND_LEDGER_CAPACITY: usize = 512;
