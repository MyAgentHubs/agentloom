use super::Block;
use rusqlite::{Connection, OptionalExtension};

pub fn append_message(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &[Block],
    engine: Option<&str>,
    agent_id: Option<&str>,
    agent_name_snapshot: Option<&str>,
) -> rusqlite::Result<()> {
    let json = serde_json::to_string(content).expect("content 序列化失败");
    conn.execute(
        "INSERT INTO messages (session_id, role, content, engine, agent_id, agent_name_snapshot, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%s','now'))",
        (session_id, role, json, engine, agent_id, agent_name_snapshot),
    )?;
    Ok(())
}

/// Cut R P0-2: deduplicating write entry point. If the same `(session_id, dedup_key)` already exists,
/// skip the entire row (`INSERT OR IGNORE`, backed by the partial unique index `idx_messages_dedup`);
/// otherwise insert it. All other column semantics match `append_message` exactly. Returns
/// `Ok(Some(milestone))` when a row was inserted, or `Ok(None)` for a duplicate with no write.
/// Note (Opus review P0-2 Low): `OR IGNORE` suppresses **any** constraint violation, including
/// `json_valid` CHECK and NOT NULL violations, not only unique conflicts. This function expects only
/// `idx_messages_dedup` conflicts; `serde_json::to_string(&[Block])` always produces valid JSON,
/// every NOT NULL column always has a value, and other violations are unreachable in practice.
/// If `Ok(None)` ever occurs when the key definitely is not duplicated, first check whether another
/// constraint violation was suppressed.
pub struct MsgCompletedMilestone {
    pub(super) session_id: String,
    pub(super) dedup_key: String,
    pub(super) message_id: i64,
    pub(super) role: String,
    pub(super) blocks_value: serde_json::Value,
    pub(super) agent_name_snapshot: Option<String>,
    /// Keep the original JSON string written to `messages.content` so hashes and byte counts use the persisted bytes.
    /// The `content_sha256` and `total_bytes` fields of `content_ref` must be calculated from these
    /// original bytes, not from `blocks_value` reserialized through `serde_json::to_value` (`Value`
    /// to `Map` sorts by key by default, so its bytes are not guaranteed to match the original).
    pub(super) content_raw: String,
    /// `append_message_dedup` is the sole constructor of this struct and constructs it only after a successful insert.
    /// Construction follows a successful INSERT, so the new row's revision is always the schema `DEFAULT 1` (also used by the
    /// migration); no path inserts and then performs another UPDATE in the same call, so this is a
    /// structural fact rather than a guessed value.
    pub(super) revision: i64,
}

impl MsgCompletedMilestone {
    /// Callers must invoke this only after the corresponding write has actually been persisted: an
    /// insert in an explicit transaction must wait for a successful `commit()`; an insert on an
    /// autocommit connection may invoke it as soon as the insert succeeds.
    pub fn publish(self) {
        crate::remote_gateway::publish_msg_completed_milestone(
            &self.session_id,
            &self.dedup_key,
            self.message_id,
            &self.role,
            self.blocks_value,
            self.agent_name_snapshot.as_deref(),
            self.revision,
            &self.content_raw,
        );
    }
}

pub fn append_message_dedup(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &[Block],
    engine: Option<&str>,
    agent_id: Option<&str>,
    agent_name_snapshot: Option<&str>,
    dedup_key: &str,
) -> rusqlite::Result<Option<MsgCompletedMilestone>> {
    let json = serde_json::to_string(content).expect("content 序列化失败");
    conn.execute(
        "INSERT OR IGNORE INTO messages (session_id, role, content, engine, agent_id, agent_name_snapshot, dedup_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%s','now'))",
        (
            session_id,
            role,
            json.as_str(),
            engine,
            agent_id,
            agent_name_snapshot,
            dedup_key,
        ),
    )?;
    if conn.changes() > 0 {
        let message_id = conn.last_insert_rowid();
        let blocks_value = serde_json::to_value(content).unwrap_or(serde_json::Value::Null);
        Ok(Some(MsgCompletedMilestone {
            session_id: session_id.to_string(),
            dedup_key: dedup_key.to_string(),
            message_id,
            role: role.to_string(),
            blocks_value,
            agent_name_snapshot: agent_name_snapshot.map(str::to_string),
            content_raw: json,
            revision: 1,
        }))
    } else {
        Ok(None)
    }
}

/// For autocommit connections only: a successful insert is already persisted, so the milestone may
/// be published immediately.
pub fn append_message_dedup_and_publish(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &[Block],
    engine: Option<&str>,
    agent_id: Option<&str>,
    agent_name_snapshot: Option<&str>,
    dedup_key: &str,
) -> rusqlite::Result<bool> {
    let milestone = append_message_dedup(
        conn,
        session_id,
        role,
        content,
        engine,
        agent_id,
        agent_name_snapshot,
        dedup_key,
    )?;
    let inserted = milestone.is_some();
    if let Some(milestone) = milestone {
        milestone.publish();
    }
    Ok(inserted)
}

/// Atomically persist the worker report and pending-delivery ledger; publish the message milestone
/// only after the transaction commits successfully.
/// The grandfathered meaning of no ledger row is "delivered", so a dedup hit does not backfill a row
/// for an old message.
#[allow(clippy::too_many_arguments)]
pub fn persist_member_report_atomic(
    conn: &Connection,
    session_id: &str,
    content: &[Block],
    agent_id: Option<&str>,
    agent_name_snapshot: Option<&str>,
    dedup_key: &str,
    assignment_id: Option<&str>,
    dispatch_terminal: Option<(&str, &str)>,
) -> rusqlite::Result<bool> {
    persist_member_report_atomic_with_publish(
        conn,
        session_id,
        content,
        agent_id,
        agent_name_snapshot,
        dedup_key,
        assignment_id,
        dispatch_terminal,
        MsgCompletedMilestone::publish,
    )
}

/// Shares the complete transaction path with the public helper; the extra parameter only lets tests
/// observe committed state from an independent connection at the exact publish point. Production
/// callers always pass `MsgCompletedMilestone::publish`.
#[allow(clippy::too_many_arguments)]
pub(super) fn persist_member_report_atomic_with_publish<F>(
    conn: &Connection,
    session_id: &str,
    content: &[Block],
    agent_id: Option<&str>,
    agent_name_snapshot: Option<&str>,
    dedup_key: &str,
    assignment_id: Option<&str>,
    dispatch_terminal: Option<(&str, &str)>,
    publish: F,
) -> rusqlite::Result<bool>
where
    F: FnOnce(MsgCompletedMilestone),
{
    let tx = conn.unchecked_transaction()?;
    let milestone = append_message_dedup(
        &tx,
        session_id,
        "assistant",
        content,
        Some("agent-team"),
        agent_id,
        agent_name_snapshot,
        dedup_key,
    )?;
    if let Some(milestone) = milestone.as_ref() {
        tx.execute(
            "INSERT INTO member_report_delivery
                (session_id, message_id, assignment_id, delivered_at)
             VALUES (?1, ?2, ?3, NULL)",
            (session_id, milestone.message_id, assignment_id),
        )?;
    }
    let dispatch_card_changed_ids = if let (Some(assignment_id), Some((status, report_text))) =
        (assignment_id, dispatch_terminal)
    {
        update_dispatch_card_terminal_ids(&tx, session_id, assignment_id, status, report_text)?
    } else {
        Vec::new()
    };
    let inserted = milestone.is_some();
    tx.commit()?;
    if let Some(milestone) = milestone {
        publish(milestone);
    }
    // The dispatch card's terminal rewrite is committed; reread the message and republish with its new revision.
    // Republish `msg.completed` (its `client_msg_id` carries the revision, so the relay treats it as
    // a new event and broadcasts it) to tell remote clients that this card reached a terminal state.
    // A republish failure does not roll back the successful DB rewrite above; this is best effort,
    // with missed events recovered through replay batches/history, and the failure is only logged.
    for message_id in dispatch_card_changed_ids {
        match get_message_for_republish(conn, session_id, message_id) {
            Ok(Some(republish)) => republish.publish(),
            Ok(None) => {
                eprintln!(
                    "persist_member_report_atomic: dispatch_card terminal rewrite republish skipped — message {message_id} has no dedup_key or was not found"
                );
            }
            Err(error) => {
                eprintln!(
                    "persist_member_report_atomic: dispatch_card terminal rewrite republish read failed for message {message_id}: {error}"
                );
            }
        }
    }
    Ok(inserted)
}

/// Only an explicit ledger row with `delivered_at IS NULL` is pending; legacy messages without a row
/// are considered delivered.
pub fn pending_member_report_message_ids(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT message_id
           FROM member_report_delivery
          WHERE session_id = ?1 AND delivered_at IS NULL
          ORDER BY message_id ASC",
    )?;
    let rows = stmt.query_map([session_id], |row| row.get(0))?;
    rows.collect()
}

/// Only after actual I/O acknowledgement, mark each report included in this prompt's ledger section as delivered via
/// `delivered_at` in a short transaction; if any `UPDATE` fails, roll everything back so no phantom
/// "partially delivered" state remains.
/// An empty `message_ids` collection is a no-op, allowing callers without prompt report selection to pass an empty set.
/// Update only rows where `delivered_at IS NULL`; do not timestamp already delivered rows again.
pub fn mark_member_reports_delivered(
    conn: &Connection,
    session_id: &str,
    message_ids: &[i64],
) -> rusqlite::Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for message_id in message_ids {
        tx.execute(
            "UPDATE member_report_delivery
                SET delivered_at = strftime('%s','now')
              WHERE session_id = ?1 AND message_id = ?2 AND delivered_at IS NULL",
            (session_id, message_id),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Rewrite a running `DispatchCard` in a persisted lead message in place to the worker's terminal state.
/// SQL LIKE only prefilters candidates; `assignment_id` and block type are matched exactly as JSON fields.
pub fn update_dispatch_card_terminal(
    conn: &Connection,
    session_id: &str,
    assignment_id: &str,
    status: &str,
    report_text: &str,
) -> rusqlite::Result<bool> {
    Ok(
        !update_dispatch_card_terminal_ids(conn, session_id, assignment_id, status, report_text)?
            .is_empty(),
    )
}

/// Shared implementation of `update_dispatch_card_terminal`, preserving the wrapper's update behavior.
/// The only difference is replacing the `bool` that says whether anything was rewritten with the
/// `message_id` values that were rewritten. After commit, the caller
/// (`persist_member_report_atomic_with_publish`) uses these IDs to reread messages and republish
/// `msg.completed` at the new revision (M0 §10.7). `update_dispatch_card_terminal` is a thin wrapper
/// over this function, with byte-for-byte identical external behavior.
fn update_dispatch_card_terminal_ids(
    conn: &Connection,
    session_id: &str,
    assignment_id: &str,
    status: &str,
    report_text: &str,
) -> rusqlite::Result<Vec<i64>> {
    if status == "running" {
        return Ok(Vec::new());
    }

    let escaped_assignment_id = assignment_id
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let like_pattern = format!("%{escaped_assignment_id}%");
    let candidates = {
        let mut stmt = conn.prepare(
            "SELECT id, content
               FROM messages
              WHERE session_id = ?1
                AND role = 'assistant'
                AND content LIKE ?2 ESCAPE '\\'
              ORDER BY id ASC",
        )?;
        let rows = stmt.query_map((session_id, like_pattern.as_str()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut changed_ids = Vec::new();
    for (message_id, content_json) in candidates {
        let Ok(mut content) = serde_json::from_str::<serde_json::Value>(&content_json) else {
            continue;
        };
        let Some(blocks) = content.as_array_mut() else {
            continue;
        };
        let mut message_changed = false;
        for block in blocks {
            let Some(block) = block.as_object_mut() else {
                continue;
            };
            if block.get("type").and_then(serde_json::Value::as_str) != Some("dispatch_card") {
                continue;
            }
            let Some(member) = block
                .get_mut("member")
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            if member
                .get("assignment_id")
                .and_then(serde_json::Value::as_str)
                != Some(assignment_id)
                || member.get("status").and_then(serde_json::Value::as_str) != Some("running")
            {
                continue;
            }
            member.insert("status".into(), serde_json::Value::String(status.into()));
            member.insert("failed".into(), serde_json::Value::Bool(status != "done"));
            member.insert(
                "blocks".into(),
                serde_json::json!([{ "type": "text", "text": report_text }]),
            );
            message_changed = true;
        }
        if !message_changed {
            continue;
        }
        let Ok(json) = serde_json::to_string(&content) else {
            continue;
        };
        conn.execute(
            "UPDATE messages SET content = ?2, revision = revision + 1 WHERE id = ?1",
            (message_id, json),
        )?;
        changed_ids.push(message_id);
    }
    Ok(changed_ids)
}

/// Reread a message by `(session_id, message_id)` so committed terminal-state rewrites can be republished with the new revision.
/// After an `update_dispatch_card_terminal` or `update_decision_card_status` commit, republish
/// `msg.completed` at the new revision. The row must have a non-null `dedup_key`: messages that either
/// function can rewrite were necessarily persisted through the `append_message_dedup*` family with
/// a dedup key. A rewritten match with a null `dedup_key` is theoretically unreachable; defensively
/// return `Ok(None)` so the caller silently skips an unrepublishable row instead of panicking.
pub(crate) fn get_message_for_republish(
    conn: &Connection,
    session_id: &str,
    message_id: i64,
) -> rusqlite::Result<Option<MsgCompletedMilestone>> {
    let row: Option<(String, String, Option<String>, Option<String>, i64)> = conn
        .query_row(
            "SELECT role, content, dedup_key, agent_name_snapshot, revision \
             FROM messages WHERE session_id = ?1 AND id = ?2",
            (session_id, message_id),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((role, content_raw, dedup_key, agent_name_snapshot, revision)) = row else {
        return Ok(None);
    };
    let Some(dedup_key) = dedup_key else {
        return Ok(None);
    };
    let blocks_value = serde_json::from_str(&content_raw).unwrap_or(serde_json::Value::Null);
    Ok(Some(MsgCompletedMilestone {
        session_id: session_id.to_string(),
        dedup_key,
        message_id,
        role,
        blocks_value,
        agent_name_snapshot,
        content_raw,
        revision,
    }))
}

/// msgfix2 U1 (design v4.1 §4.1, M0 §10.11): an atomic upsert plus republish dedicated to the L1
/// activity-summary aggregator. One SQL statement covers both states: the first miss in
/// `idx_messages_dedup` inserts with the schema's `DEFAULT 1` revision; an existing
/// `(session_id, dedup_key)` updates content in place and increments the revision. This follows the
/// same `ON CONFLICT ... DO UPDATE` convention as the existing `upsert_memory_block` in this file.
/// After writing, reread the row and reuse gap 4's approach (`get_message_for_republish` plus
/// `MsgCompletedMilestone::publish`) to republish `msg.completed` at the new revision. Both states
/// appear as the same function to clients: `derive_msg_completed_client_msg_id` already selects the
/// derivation rule for `revision==1` versus `revision>1`, so this code need not distinguish whether
/// the operation inserted or updated.
///
/// `content` is not a typed `db::Block` card. `activity_summary` uses an aggregator-private JSON
/// shape: a single-element blocks array with `type: "activity_summary"`. It follows the tagged-union
/// convention of the eight existing card types without entering the `Block` enum itself, avoiding
/// changes to exhaustive `Block` matches throughout the repository, including member_runner.rs,
/// lead_tools.rs, display_reduce.rs, continuation.rs, memory_tools.rs, and lib.rs, which are outside
/// the scope of this remote_gateway.rs plus db.rs cut.
///
/// `last_insert_rowid()` is not updated when the upsert takes the UPDATE branch; SQLite advances it
/// only for a real INSERT. It therefore cannot identify the message ID here, and the row must be
/// explicitly reread by `(session_id, dedup_key)` after the write.
///
/// The UPDATE branch advances `revision` only when `content` changes, keeping identical retries version-stable.
/// The write-thread retry described in `flush_activity_summary` does not guarantee that the prior
/// attempt truly failed to persist. If failure occurs after this SQL statement but before
/// `get_message_for_republish_by_dedup_key` rereads the row, for example because that SELECT fails,
/// the content was actually committed. Retrying the same counts on the next tick would otherwise
/// bump the revision again for identical content, creating pure noise where clients see an increased
/// revision with no content change. The `CASE` comparison of `messages.content = excluded.content`
/// leaves the revision unchanged for identical replays, while real count or state changes still add
/// one and preserve the existing two-state semantics.
pub fn upsert_activity_summary_and_publish(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    tool_calls: i64,
    failed: i64,
    mcp_calls: i64,
    permission_prompts: i64,
    state: &str,
) -> rusqlite::Result<()> {
    let dedup_key = format!("activity_summary:{run_id}");
    let content_json = serde_json::to_string(&serde_json::json!([{
        "type": "activity_summary",
        "run_id": run_id,
        "tool_calls": tool_calls,
        "failed": failed,
        "mcp_calls": mcp_calls,
        "permission_prompts": permission_prompts,
        "state": state,
    }]))
    .expect("activity_summary content must serialize");

    conn.execute(
        "INSERT INTO messages (session_id, role, content, dedup_key, created_at) \
         VALUES (?1, 'assistant', ?2, ?3, strftime('%s','now')) \
         ON CONFLICT(session_id, dedup_key) WHERE dedup_key IS NOT NULL DO UPDATE SET \
           content = excluded.content, \
           revision = CASE WHEN messages.content = excluded.content \
                            THEN messages.revision ELSE messages.revision + 1 END",
        rusqlite::params![session_id, content_json, dedup_key],
    )?;

    let Some(republish) = get_message_for_republish_by_dedup_key(conn, session_id, &dedup_key)?
    else {
        // Theoretically unreachable: the statement above either inserts or hits the conflict key it
        // just created, so an immediate reread by `(session_id, dedup_key)` must find the row.
        // Defensively skip silently instead of panicking, consistent with the existing
        // `get_message_for_republish` behavior of not publishing when no row is found.
        return Ok(());
    };
    republish.publish();
    Ok(())
}

/// `get_message_for_republish` looks up by message ID; this function looks up by dedup key. After an
/// upsert, the caller knows only the `activity_summary:<run_id>` dedup key, not which message ID holds
/// the result: the insert branch has a new ID and the update branch an existing one. First recover the
/// message ID by dedup key, then reuse the existing lookup-by-ID path.
fn get_message_for_republish_by_dedup_key(
    conn: &Connection,
    session_id: &str,
    dedup_key: &str,
) -> rusqlite::Result<Option<MsgCompletedMilestone>> {
    let message_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM messages WHERE session_id = ?1 AND dedup_key = ?2",
            (session_id, dedup_key),
            |r| r.get(0),
        )
        .optional()?;
    let Some(message_id) = message_id else {
        return Ok(None);
    };
    get_message_for_republish(conn, session_id, message_id)
}

/// msgfix2 U1 (design v4.1 §4.1, "revision preservation", restart recovery rule): called at desktop
/// startup to scan all `activity_summary:*` messages. Any message with `state=="running"` whose
/// `run_id` is absent from the caller-provided `active_run_ids`, the set of logical runs still active,
/// is rewritten in place to `state=="failed"` and republished using gap 4's approach. Otherwise, an
/// activity summary for a run left running by an abnormal desktop restart or crash would remain stuck
/// as running forever on mobile, with no later event able to change it.
///
/// The caller supplies `active_run_ids`; db.rs does not know whether a run is still alive because that
/// is runtime state, not storage state. This function solely reconciles against a supplied set of live
/// runs. Wiring it into the desktop startup sequence with real `active_run_ids` is deferred to a later
/// cut on the lib.rs side and lies outside this cut's scope.
///
/// The return value is the number of messages actually sealed, for caller logging or assertions, not
/// an error signal.
pub fn reconcile_stale_running_activity_summaries(
    conn: &Connection,
    active_run_ids: &std::collections::HashSet<String>,
) -> rusqlite::Result<u64> {
    let candidates: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, session_id, dedup_key, content FROM messages \
             WHERE dedup_key LIKE 'activity_summary:%'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let mut sealed = 0_u64;
    for (message_id, session_id, dedup_key, content_json) in candidates {
        let Some(run_id) = dedup_key.strip_prefix("activity_summary:") else {
            continue;
        };
        if active_run_ids.contains(run_id) {
            continue;
        }
        let Ok(mut content) = serde_json::from_str::<serde_json::Value>(&content_json) else {
            continue;
        };
        let Some(block) = content.as_array_mut().and_then(|arr| arr.get_mut(0)) else {
            continue;
        };
        if block.get("state").and_then(serde_json::Value::as_str) != Some("running") {
            continue;
        }
        block["state"] = serde_json::Value::String("failed".to_owned());
        let Ok(json) = serde_json::to_string(&content) else {
            continue;
        };
        conn.execute(
            "UPDATE messages SET content = ?2, revision = revision + 1 WHERE id = ?1",
            (message_id, json),
        )?;
        if let Some(republish) = get_message_for_republish(conn, &session_id, message_id)? {
            republish.publish();
            sealed += 1;
        }
    }
    Ok(sealed)
}
