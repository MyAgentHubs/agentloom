use super::*;

/// Builds the bare payload for a `control.snapshot` response. It omits `t`, following the same
/// convention as builders such as `build_run_status_payload`; `milestone_payload` merges `t` while
/// draining. The v1.8.12 shape invariant is centralized here: an active run means `run_id` is not
/// null and `through_run_seq` is not null and at least 1. A zero sequence is invalid because the
/// production sequencer increments before returning, making the first event sequence 1. Before
/// that first event, the desktop has no reduced entry and uses the idle branch. Empty `blocks`
/// produce a null `partial_msg`; callers pass `(None, &[])` for idle state, naturally making all
/// three fields null.
///
/// **Builder type convergence**: `run` combines the former independent `(Option<String>,
/// Option<u64>)` values into one `Option<(String, u64)>`. The run pair is wholly present or absent,
/// making an invalid combination such as `run_id=Some/through=None` unrepresentable.
///
/// **Size budget**: `blocks` contains the raw accumulated reduction for the active run, before
/// `DisplayReducer::finish` applies final truncation. A large tool output or narrative can hit the
/// relay's 64 KB frame limit, disconnecting the desktop with code 1009 and causing a reconnect
/// loop. `shrink_snapshot_blocks_to_budget` converges in two steps: truncate each tool-card output
/// to `OUTPUT_TRUNCATE_BYTES`, matching the remote `tool.completed` and live paths; then, if the
/// serialized frame including `t` still exceeds `SNAPSHOT_PAYLOAD_BUDGET_BYTES`, account for the
/// truncation notice first and choose which business blocks fit. Business blocks may all be
/// removed, leaving only the notice and watermark fields. Oversized frames must not be left for
/// the relay to reject; the snapshot arm also enforces `SNAPSHOT_SEND_BUDGET_BYTES` when sending.
pub(super) fn build_snapshot_payload(
    session: &str,
    run: Option<(&str, u64)>,
    blocks: &[crate::db::Block],
) -> Value {
    let (run_id, through_run_seq) = match run {
        Some((run_id, through_run_seq)) => (Some(run_id), Some(through_run_seq)),
        None => (None, None),
    };
    let partial_msg = if blocks.is_empty() {
        Value::Null
    } else {
        let bounded = shrink_snapshot_blocks_to_budget(session, run_id, through_run_seq, blocks);
        serde_json::json!({ "role": "assistant", "blocks": bounded })
    };
    serde_json::json!({
        "session": session,
        "run_id": run_id,
        "through_run_seq": through_run_seq,
        "partial_msg": partial_msg,
    })
}

/// Provides the single implementation for measuring a complete snapshot frame. `payload` is the
/// bare four-field snapshot object without `t`. This helper measures a clone after adding the
/// `"t":"snapshot"` field that `milestone_payload` later merges, without modifying the caller's
/// value. Both `shrink_snapshot_blocks_to_budget` and the `control.snapshot` send-side
/// `SNAPSHOT_SEND_BUDGET_BYTES` fallback therefore use the same measurement. Measuring the bare
/// payload at send time would underestimate a boundary frame that already converged with `t`.
pub(super) fn snapshot_frame_bytes(payload: &Value) -> usize {
    milestone_frame_bytes("snapshot", payload)
}

/// Appends the evicted block count to the truncation notice. `folded_count` is the number of
/// business blocks removed during convergence and excludes the notice block itself.
fn snapshot_truncated_notice_text(folded_count: usize) -> String {
    format!("{SNAPSHOT_TRUNCATED_NOTICE}({folded_count} 块折叠)")
}

/// Classifies eviction priority for `shrink_snapshot_blocks_to_budget`. Only `Block::Text`, the
/// user-facing narrative body, counts as narrative and is evicted second. Other non-actionable
/// block types such as `Tool`, `Thinking`, and `DispatchCard` are evicted first.
fn is_snapshot_narrative_text_block(block: &crate::db::Block) -> bool {
    matches!(block, crate::db::Block::Text { .. })
}

/// Applies the third eviction criterion for `shrink_snapshot_blocks_to_budget`: actionable blocks
/// (`approval`, `decision_card`, and `scope_change`; see `is_actionable_block_type`) never enter the
/// eviction candidates. Explicit exclusion is more robust than relying on their currently not
/// being `Block::Text`. This reuses the single block-type string allowlist instead of duplicating a
/// `matches!` list. Because `Block` is tagged with `#[serde(tag = "type")]`, its serialized `type`
/// is the real wire label, keeping this predicate aligned with `build_oversized_preview_blocks`.
fn is_snapshot_actionable_block(block: &crate::db::Block) -> bool {
    serde_json::to_value(block)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|block_type| is_actionable_block_type(block_type))
        })
        .unwrap_or(false)
}

/// Performs the convergence used by `build_snapshot_payload`; see that function's size-budget
/// ordering. `session`, `run_id`, and `through_run_seq` are used only to recompute the serialized
/// size of trial frames matching the actual envelope, rather than measuring the `blocks` array
/// alone. They do not otherwise affect truncation decisions.
///
/// **Measurements include `t`**: `frame_bytes` accounts for the `"t":"snapshot"` field later
/// merged by `milestone_payload`. Otherwise, measuring the bare payload would classify boundary
/// cases such as a single 32 KiB text block as within budget even when the final frame is not.
///
/// **Type-based eviction with original order preserved**: candidates have a fixed order by type
/// priority and then oldest first within each priority. Non-actionable, non-`Block::Text` tool-like
/// blocks (`Tool`, `Thinking`, `DispatchCard`, and others) are evicted first. Narrative
/// `Block::Text` blocks follow. Actionable blocks (`approval`, `decision_card`, and `scope_change`)
/// never enter the candidate sequence, regardless of budget pressure. This avoids treating an
/// approval card like disposable tool output merely because it is not `Block::Text`, which could
/// otherwise hide a pending approval from the user. Candidates are removed until the remaining
/// marginal block bytes fit or every non-actionable candidate is gone. The returned collection is
/// filtered strictly in the original `blocks` order, so retained blocks keep their rendering
/// order. `snapshot_truncated_notice_text` reports the number removed. There is no persisted-prefix
/// tier because it was never implemented and no metadata supports it. Discarded blocks are not
/// replaced with fetchable references.
///
/// **All business blocks may be removed**: in the extreme case where the budget cannot fit even
/// the notice baseline, which is unreachable under the current input contract, the result keeps
/// only the notice and watermark fields.
///
/// Full-frame measurement is centralized in `snapshot_frame_bytes`, which is also used by the
/// `control.snapshot` send-side fallback so both paths share the same accounting.
fn shrink_snapshot_blocks_to_budget(
    session: &str,
    run_id: Option<&str>,
    through_run_seq: Option<u64>,
    blocks: &[crate::db::Block],
) -> Vec<crate::db::Block> {
    let bounded: Vec<crate::db::Block> = blocks
        .iter()
        .cloned()
        .map(truncate_snapshot_tool_output)
        .collect();

    let frame_bytes = |bounded: &[crate::db::Block]| -> usize {
        let payload = serde_json::json!({
            "session": session,
            "run_id": run_id,
            "through_run_seq": through_run_seq,
            "partial_msg": { "role": "assistant", "blocks": bounded },
        });
        snapshot_frame_bytes(&payload)
    };

    if frame_bytes(&bounded) <= SNAPSHOT_PAYLOAD_BUDGET_BYTES {
        return bounded;
    }

    // Evict tool-like blocks from oldest to newest, followed by narrative text from oldest to
    // newest. Actionable blocks never enter this list; see the type-based eviction documentation
    // and `is_snapshot_narrative_text_block` / `is_snapshot_actionable_block`.
    let eviction_order: Vec<usize> = bounded
        .iter()
        .enumerate()
        .filter(|(_, block)| {
            !is_snapshot_narrative_text_block(block) && !is_snapshot_actionable_block(block)
        })
        .map(|(idx, _)| idx)
        .chain(
            bounded
                .iter()
                .enumerate()
                .filter(|(_, block)| {
                    is_snapshot_narrative_text_block(block) && !is_snapshot_actionable_block(block)
                })
                .map(|(idx, _)| idx),
        )
        .collect();

    // Account for the notice first, estimating its count width with the worst case where every
    // block is evicted. Selection therefore never underestimates notice overhead: the actual count
    // is no larger and its text can only be shorter.
    //
    // Under the current input contract, `base_bytes` cannot exceed the 32,768-byte
    // `SNAPSHOT_PAYLOAD_BUDGET_BYTES`. The guarded branch remains for defense in depth, not because
    // it is expected to run. `session` is at most 128 bytes due to `SESSION_ID_MAX_BYTES`, `run_id`
    // has a fixed 38-byte format, `through_run_seq` is at most 20 decimal digits, and the notice is
    // a short fixed string plus a small eviction count. Together with JSON overhead this is only a
    // few hundred bytes, nearly two orders of magnitude below the budget.
    let worst_case_notice = crate::db::Block::Text {
        text: snapshot_truncated_notice_text(bounded.len()),
    };
    let base_bytes = frame_bytes(std::slice::from_ref(&worst_case_notice));

    let mut kept = vec![true; bounded.len()];
    let mut kept_bytes: usize = bounded
        .iter()
        .map(|block| {
            serde_json::to_string(block)
                .map(|json| json.len() + 1) // +1 for the array-separating comma.
                .unwrap_or(usize::MAX)
        })
        .sum();
    let mut evicted_count = 0usize;

    if base_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES {
        let budget_left_for_blocks = SNAPSHOT_PAYLOAD_BUDGET_BYTES - base_bytes;
        for idx in eviction_order {
            if kept_bytes <= budget_left_for_blocks {
                break;
            }
            let marginal = serde_json::to_string(&bounded[idx])
                .map(|json| json.len() + 1)
                .unwrap_or(usize::MAX);
            kept[idx] = false;
            kept_bytes = kept_bytes.saturating_sub(marginal);
            evicted_count += 1;
        }
    } else {
        // Extreme fallback described above: remove every business block and retain only the notice.
        kept = vec![false; bounded.len()];
        evicted_count = bounded.len();
    }

    let notice = crate::db::Block::Text {
        text: snapshot_truncated_notice_text(evicted_count),
    };
    let mut result = Vec::with_capacity(1 + kept.iter().filter(|keep| **keep).count());
    result.push(notice);
    // Filter in the original `bounded` order; retained blocks are not reordered by eviction order.
    result.extend(
        bounded
            .into_iter()
            .zip(kept)
            .filter(|(_, keep)| *keep)
            .map(|(block, _)| block),
    );
    result
}

/// Truncates each tool-card output to `OUTPUT_TRUNCATE_BYTES`, matching the remote
/// `tool.completed` and live paths where `extract_tool_milestones` and `classify` use the same
/// limit. Other block types have no free-length text field to converge here; `Text` and `Thinking`
/// are handled by whole-frame block eviction.
fn truncate_snapshot_tool_output(mut block: crate::db::Block) -> crate::db::Block {
    if let crate::db::Block::Tool {
        output: Some(output),
        ..
    } = &mut block
    {
        *output = truncate_utf8(output, OUTPUT_TRUNCATE_BYTES);
    }
    block
}

#[derive(Debug, PartialEq)]
pub(super) struct HistoryPage {
    pub(super) payload: Value,
    pub(super) oversized_dropped: u64,
    pub(super) next_scan_before: Option<i64>,
}

pub(super) fn truncate_history_tool_outputs(mut blocks: Value) -> Value {
    let Some(items) = blocks.as_array_mut() else {
        return blocks;
    };
    for block in items {
        let Some(fields) = block.as_object_mut() else {
            continue;
        };
        if fields.get("type").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(output) = fields.get_mut("output") else {
            continue;
        };
        if let Some(text) = output.as_str() {
            // `msg.completed` via `enqueue_milestone_item` and history queries share this helper so both paths visibly mark truncated output.
            // Both paths must add a visible marker whenever truncation occurs.
            *output = Value::String(truncate_utf8_with_marker(text, OUTPUT_TRUNCATE_BYTES));
        }
    }
    blocks
}

pub(super) fn history_payload(
    session: &str,
    before_message_id: Option<i64>,
    messages: &[Value],
    next_before: Option<i64>,
) -> Value {
    serde_json::json!({
        "t": "history",
        "session": session,
        "before_message_id": before_message_id,
        "messages": messages,
        "next_before": next_before,
    })
}

/// Constructs a general history wire message. When `content_ref` is `None`, the key is omitted
/// entirely rather than set to null. Normally sized messages do not carry this optional field; it
/// is attached only when a message is downgraded to a preview.
///
/// `revision` is always present at the top level. History rows are one of the three top-level
/// revision producers; initial live publication and reconnect replay both use
/// `build_msg_completed_payload`.
fn history_message_from_parts(
    message_id: i64,
    role: &str,
    blocks: Value,
    revision: i64,
    content_ref: Option<Value>,
) -> Value {
    let mut message = serde_json::json!({
        "message_id": message_id,
        "role": role,
        "blocks": blocks,
        "revision": revision,
    });
    if let Some(content_ref) = content_ref {
        message["content_ref"] = content_ref;
    }
    message
}

fn history_message(row: &SessionHistoryRow) -> Value {
    history_message_from_parts(
        row.message_id,
        &row.role,
        truncate_history_tool_outputs(row.content_json.clone()),
        row.revision,
        None,
    )
}

/// Accepts database rows from newest to oldest. Each tool output is first converged to the existing
/// limit, then the page is filled from newest backward. Wire output is finally reversed into
/// ascending `message_id` order. One extra fetched row is used only to determine precisely whether
/// the earliest page has `next_before=null`.
pub(super) fn build_history_page(
    session: &str,
    before_message_id: Option<i64>,
    rows: Vec<SessionHistoryRow>,
) -> HistoryPage {
    build_history_page_with_limit(session, before_message_id, rows, HISTORY_PAGE_MAX_ROWS)
}

pub(super) fn build_history_page_with_limit(
    session: &str,
    before_message_id: Option<i64>,
    mut rows: Vec<SessionHistoryRow>,
    page_max_rows: usize,
) -> HistoryPage {
    let database_has_more = rows.len() > page_max_rows;
    rows.truncate(page_max_rows);

    let mut kept_desc: Vec<Value> = Vec::new();
    let mut oversized_dropped = 0_u64;
    let mut budget_truncated = false;
    let mut oldest_scanned = None;

    for (index, row) in rows.iter().enumerate() {
        let row_has_older = database_has_more || index + 1 < rows.len();
        let mut message = history_message(row);
        let single = history_payload(
            session,
            before_message_id,
            std::slice::from_ref(&message),
            row_has_older.then_some(row.message_id),
        );
        if serde_json::to_vec(&single)
            .map(|json| json.len())
            .unwrap_or(usize::MAX)
            > HISTORY_SEND_BUDGET_BYTES
        {
            // Downgrade oversized messages to a block-level preview plus content_ref so they remain visible and their full content can still be fetched.
            // `oversized_dropped` therefore counts messages downgraded to previews, preserving
            // observability while the remote can still see each message and fetch its full text.
            let content_ref = build_content_ref(row.message_id, row.revision, &row.content_raw);
            let preview_message = history_message_from_parts(
                row.message_id,
                &row.role,
                build_oversized_preview_blocks(&row.content_json),
                row.revision,
                Some(content_ref.clone()),
            );
            let preview_single = history_payload(
                session,
                before_message_id,
                std::slice::from_ref(&preview_message),
                row_has_older.then_some(row.message_id),
            );
            oversized_dropped += 1;
            // If a block-level preview still exceeds the budget, including oversized actionable blocks, fall back to a notice plus content_ref instead of dropping the row.
            // Use the same notice-only blocks plus `content_ref` terminal form as the
            // `msg.completed` path through `downgrade_to_preview_payload`. It is designed to fit:
            // the four fixed `content_ref` fields, fixed-length notice, and bounded session/cursor
            // overhead are far below the 44 KiB `HISTORY_SEND_BUDGET_BYTES`. Keeping the reference
            // is a hard invariant, so there is no fallback that drops the row.
            if serde_json::to_vec(&preview_single)
                .map(|json| json.len())
                .unwrap_or(usize::MAX)
                > HISTORY_SEND_BUDGET_BYTES
            {
                message = history_message_from_parts(
                    row.message_id,
                    &row.role,
                    notice_only_preview_blocks(),
                    row.revision,
                    Some(content_ref),
                );
            } else {
                message = preview_message;
            }
        }

        kept_desc.push(message);
        let mut candidate_ascending = kept_desc.clone();
        candidate_ascending.reverse();
        let min_id = kept_desc
            .last()
            .and_then(|message| message.get("message_id"))
            .and_then(Value::as_i64);
        let candidate = history_payload(
            session,
            before_message_id,
            &candidate_ascending,
            row_has_older.then_some(min_id).flatten(),
        );
        if serde_json::to_vec(&candidate)
            .map(|json| json.len())
            .unwrap_or(usize::MAX)
            > HISTORY_SEND_BUDGET_BYTES
        {
            kept_desc.pop();
            budget_truncated = true;
            break;
        }
        oldest_scanned = Some(row.message_id);
    }

    let mut messages = kept_desc;
    messages.reverse();
    // Emit a cursor only when unscanned rows remain. An oversized row is deliberately consumed, so
    // the cursor advances past it. A normal row that does not fit the page budget is not consumed;
    // the cursor stays on the preceding scanned row so the next page can still retrieve it.
    let next_scan_before = (budget_truncated || database_has_more)
        .then_some(oldest_scanned)
        .flatten();

    HistoryPage {
        payload: history_payload(session, before_message_id, &messages, next_scan_before),
        oversized_dropped,
        next_scan_before,
    }
}
