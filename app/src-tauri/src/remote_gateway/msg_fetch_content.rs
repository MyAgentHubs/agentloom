use super::*;

/// Computes the lowercase SHA-256 hex digest used by `content_ref.content_sha256`.
/// The test-only wire fixture helper is intentionally separate.
pub(super) fn sha256_hex_lower(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Builds a content reference from the caller's original JSON string. Byte length and digest use
/// the exact UTF-8 bytes so they match snapshot and history budget accounting.
pub(super) fn build_content_ref(message_id: i64, revision: i64, content_raw: &str) -> Value {
    serde_json::json!({
        "message_id": message_id,
        "revision": revision,
        "content_sha256": sha256_hex_lower(content_raw.as_bytes()),
        "total_bytes": content_raw.len(),
    })
}

/// Constructs a `msg.fetch.error` payload. `current_ref` is included only for
/// `stale_revision`; every other error omits the field entirely.
pub(super) fn msg_fetch_error_payload(code: &str, current_ref: Option<Value>) -> Value {
    let mut frame = serde_json::json!({"t": "msg.fetch.error", "code": code});
    if let Some(current_ref) = current_ref {
        frame["current_ref"] = current_ref;
    }
    frame
}

/// Splits raw message bytes into ordered `msg.chunk` payloads beginning at the resume offset.
/// Whole-message length and digest are retained for safe reassembly. Chunk boundaries operate on
/// opaque bytes and therefore need not align with UTF-8 character boundaries.
///
/// The result is always nonempty. An offset at or beyond the end, including empty content, yields
/// one terminal zero-length chunk at `total_bytes`. This gives callers a final frame that releases
/// single-flight ownership while preserving whole-message continuity and digest validation.
pub(super) fn build_msg_chunks(
    message_id: i64,
    revision: i64,
    content: &[u8],
    start_offset: usize,
) -> Vec<Value> {
    let total_bytes = content.len();
    let content_sha256 = sha256_hex_lower(content);
    let mut chunks = Vec::new();
    let mut offset = start_offset.min(total_bytes);
    while offset < total_bytes {
        let end = (offset + CHUNK_RAW_BYTES).min(total_bytes);
        let slice = &content[offset..end];
        chunks.push(serde_json::json!({
            "t": "msg.chunk",
            "message_id": message_id,
            "revision": revision,
            "content_sha256": content_sha256,
            "total_bytes": total_bytes,
            "offset": offset,
            "chunk_len": slice.len(),
            "bytes_b64": STANDARD.encode(slice),
        }));
        offset = end;
    }
    if chunks.is_empty() {
        chunks.push(serde_json::json!({
            "t": "msg.chunk",
            "message_id": message_id,
            "revision": revision,
            "content_sha256": content_sha256,
            "total_bytes": total_bytes,
            "offset": total_bytes,
            "chunk_len": 0,
            "bytes_b64": "",
        }));
    }
    chunks
}

/// Reports whether a session fetch still owns its single-flight slot. Expiry releases the slot,
/// and saturating subtraction safely handles clock rollback.
pub(super) fn msg_fetch_inflight_is_active(accepted_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(accepted_at_ms) < MSG_FETCH_INFLIGHT_TIMEOUT_MS
}

/// Applies the source-wide rolling byte budget. Expired entries are pruned before requested bytes
/// are checked and, when admitted, recorded. The returned window lets callers replace the ledger
/// within the same lock so pruning, admission, and accounting remain atomic.
pub(super) fn msg_fetch_budget_admit(
    mut window: VecDeque<(u64, usize)>,
    now_ms: u64,
    requested_bytes: usize,
) -> (bool, VecDeque<(u64, usize)>) {
    while let Some((at_ms, _)) = window.front() {
        if now_ms.saturating_sub(*at_ms) >= MSG_FETCH_BYTE_BUDGET_WINDOW_MS {
            window.pop_front();
        } else {
            break;
        }
    }
    let used: u64 = window.iter().map(|(_, bytes)| *bytes as u64).sum();
    let admit = used.saturating_add(requested_bytes as u64) <= MSG_FETCH_BYTE_BUDGET_PER_WINDOW;
    if admit {
        window.push_back((now_ms, requested_bytes));
    }
    (admit, window)
}

/// Checks and records `(session, command_id)` in the command ledger. Previously seen commands are
/// rejected; new commands are admitted, with the oldest entry evicted in FIFO order at capacity.
pub(super) fn msg_fetch_command_ledger_admit(
    state: &GatewayInnerState,
    session: &str,
    command_id: &str,
) -> bool {
    let mut ledger = lock(&state.msg_fetch_command_ledger);
    let key = (session.to_owned(), command_id.to_owned());
    if ledger.seen.contains(&key) {
        return false;
    }
    if ledger.order.len() >= MSG_FETCH_COMMAND_LEDGER_CAPACITY {
        if let Some(oldest) = ledger.order.pop_front() {
            ledger.seen.remove(&oldest);
        }
    }
    ledger.seen.insert(key.clone());
    ledger.order.push_back(key);
    true
}

/// Identifies blocks that require immediate user action. Approval, decision-card, and scope-change
/// blocks must survive preview degradation unchanged; display-only and historical blocks follow
/// the normal preview selection rules.
pub(super) fn is_actionable_block_type(block_type: &str) -> bool {
    matches!(block_type, "approval" | "decision_card" | "scope_change")
}

/// Determines at runtime whether an event's original content may enter activity aggregation.
/// Actionable blocks use restricted handling or are skipped, while other block types may join the
/// aggregate. Reusing `is_actionable_block_type` keeps routing tied to the single allowlist in
/// release as well as debug builds.
pub(super) fn event_joins_l1_aggregation(block_type: &str) -> bool {
    !is_actionable_block_type(block_type)
}

/// Builds an oversized-message preview. Actionable blocks are preserved, the first ordinary text
/// block is truncated on a UTF-8 boundary and receives the truncation notice, and other free-form
/// blocks are omitted. Rust strings contain Unicode scalar values, so boundary-aware truncation
/// cannot split a code point.
pub(super) fn build_oversized_preview_blocks(blocks: &Value) -> Value {
    let mut preview: Vec<Value> = Vec::new();
    let mut preview_text: Option<String> = None;
    if let Some(items) = blocks.as_array() {
        for block in items {
            let Some(block_type) = block.get("type").and_then(Value::as_str) else {
                continue;
            };
            if is_actionable_block_type(block_type) {
                preview.push(block.clone());
                continue;
            }
            if block_type == "text" && preview_text.is_none() {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    preview_text = Some(truncate_utf8(text, OVERSIZED_PREVIEW_TEXT_HEAD_BYTES));
                }
            }
        }
    }
    let mut combined = preview_text.unwrap_or_default();
    combined.push_str(OVERSIZED_PREVIEW_TRUNCATION_NOTICE);
    preview.push(serde_json::json!({"type": "text", "text": combined}));
    Value::Array(preview)
}

/// Downgrades an oversized completed-message or history-row payload to a preview plus content
/// reference. If that frame still exceeds the budget, it falls back to a notice-only block while
/// always retaining the content reference. Live and history paths share the same final shape.
pub(super) fn notice_only_preview_blocks() -> Value {
    serde_json::json!([{"type": "text", "text": OVERSIZED_PREVIEW_TRUNCATION_NOTICE}])
}

pub(super) fn downgrade_to_preview_payload(payload: &Value, content_ref: Value, t: &str) -> Value {
    let blocks = payload
        .get("blocks")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let mut degraded = payload.clone();
    degraded["blocks"] = build_oversized_preview_blocks(&blocks);
    degraded["content_ref"] = content_ref;
    if milestone_frame_bytes(t, &degraded) > SNAPSHOT_SEND_BUDGET_BYTES {
        degraded["blocks"] = notice_only_preview_blocks();
    }
    degraded
}
