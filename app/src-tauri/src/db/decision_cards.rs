use rusqlite::Connection;

/// T-C3b b0: atomically compare and set the status of a `decision_card` block, preventing double-click
/// races. In one transaction, read-modify-write content as a `serde_json::Value` array rather than
/// through the `Block` enum, preserving unknown and sibling blocks and avoiding the same collateral
/// behavior as `unwrap_or_default()`. Find the object whose type is `decision_card` and whose
/// `decision_id` matches. Change its status to `next_status` only when its current status equals
/// `expect_status`, also writing `chosen_option` when it is `Some`, and return true when this call wins
/// the race. Otherwise make no change and return false, which is where the second double-click lands.
/// A decision ID is unique within a session, so stop at the first match. Atomicity relies on the app's
/// single `Db(Mutex<Connection>)`: the command layer already holds the lock, this read-modify-write is
/// serialized, and `unchecked_transaction` supplies database-level atomicity.
pub fn update_decision_card_status(
    conn: &Connection,
    session_id: &str,
    decision_id: &str,
    expect_status: &str,
    next_status: &str,
    chosen_option: Option<&str>,
) -> rusqlite::Result<bool> {
    Ok(update_decision_card_status_message_id(
        conn,
        session_id,
        decision_id,
        expect_status,
        next_status,
        chosen_option,
    )?
    .is_some())
}

/// Shared implementation of `update_decision_card_status`, preserving the wrapper's update behavior.
/// The only difference is replacing the `bool` that says whether a rewrite occurred with the
/// `message_id` that was rewritten. Because a decision ID is unique within a session and the scan
/// stops at the first match, there can be at most one. After the rewrite commits, callers such as
/// `prompt_user`, `commit_late_answer`, and `choose_decision_card` use this ID to reread the message
/// and republish `msg.completed` at the new revision (M0 §10.7). `update_decision_card_status` is a
/// thin wrapper over this function with byte-for-byte identical external behavior, including the
/// existing internal `publish_card_resolved_milestone` call in the same position and under the same
/// condition.
pub(crate) fn update_decision_card_status_message_id(
    conn: &Connection,
    session_id: &str,
    decision_id: &str,
    expect_status: &str,
    next_status: &str,
    chosen_option: Option<&str>,
) -> rusqlite::Result<Option<i64>> {
    let tx = conn.unchecked_transaction()?;
    let rows: Vec<(i64, String)> = {
        let mut stmt =
            tx.prepare("SELECT id, content FROM messages WHERE session_id = ?1 ORDER BY id ASC")?;
        let rows = stmt
            .query_map([session_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    for (msg_id, content_json) in rows {
        let mut content: serde_json::Value = match serde_json::from_str(&content_json) {
            Ok(v) => v,
            Err(_) => continue, // Skip a malformed row without changing it.
        };
        let Some(arr) = content.as_array_mut() else {
            continue;
        };
        let mut hit = false;
        let mut changed = false;
        for block in arr.iter_mut() {
            let Some(obj) = block.as_object_mut() else {
                continue;
            };
            if obj.get("type").and_then(|v| v.as_str()) != Some("decision_card") {
                continue;
            }
            if obj.get("decision_id").and_then(|v| v.as_str()) != Some(decision_id) {
                continue;
            }
            hit = true;
            if obj.get("status").and_then(|v| v.as_str()) == Some(expect_status) {
                obj.insert(
                    "status".into(),
                    serde_json::Value::String(next_status.to_string()),
                );
                if let Some(opt) = chosen_option {
                    obj.insert(
                        "chosen_option".into(),
                        serde_json::Value::String(opt.to_string()),
                    );
                }
                changed = true;
            }
            break; // A decision ID is unique, so stop at the first match.
        }
        if hit {
            if changed {
                let new_json =
                    serde_json::to_string(&content).expect("decision_card content 序列化失败");
                tx.execute(
                    "UPDATE messages SET content = ?1, revision = revision + 1 WHERE id = ?2",
                    rusqlite::params![new_json, msg_id],
                )?;
            }
            tx.commit()?;
            if changed {
                crate::remote_gateway::publish_card_resolved_milestone(
                    session_id,
                    decision_id,
                    next_status,
                    chosen_option,
                );
            }
            return Ok(if changed { Some(msg_id) } else { None });
        }
    }
    tx.commit()?;
    Ok(None) // The decision ID was not found.
}

/// Read a decision card's `(question, status)` by `decision_id` without changing any state.
/// When a late answer arrives, use this to recover the original question and include it in the user
/// message forwarded to the lead. It also detects a pending DB card after restart when memory has been
/// cleared; that case follows the late-answer path, while an already chosen card preserves
/// `NO_PENDING_QUESTION` semantics. The scan mirrors `update_decision_card_status`, sharing the same
/// understanding of how to find `decision_card` blocks in `messages.content`.
pub fn find_decision_card(
    conn: &Connection,
    session_id: &str,
    decision_id: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    let mut stmt =
        conn.prepare("SELECT content FROM messages WHERE session_id = ?1 ORDER BY id ASC")?;
    let rows: Vec<String> = stmt
        .query_map([session_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    for content_json in rows {
        let Ok(content) = serde_json::from_str::<serde_json::Value>(&content_json) else {
            continue;
        };
        let Some(arr) = content.as_array() else {
            continue;
        };
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) != Some("decision_card") {
                continue;
            }
            if block.get("decision_id").and_then(|v| v.as_str()) != Some(decision_id) {
                continue;
            }
            let question = block
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status = block
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Ok(Some((question, status)));
        }
    }
    Ok(None)
}
