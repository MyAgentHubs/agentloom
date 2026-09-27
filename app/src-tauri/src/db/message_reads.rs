use super::{blocks_to_text, Anchor, Block, Message};
use rusqlite::{Connection, OptionalExtension};

/// R1 (msgfix2 full review): content deserialization failures are no longer silently swallowed. The
/// private `activity_summary` JSON shape documented by `upsert_activity_summary_and_publish` does not
/// belong to any tagged variant of `Vec<Block>`. Historically, `unwrap_or_default()` silently turned
/// it into empty blocks, causing the desktop to render a blank assistant bubble. The actual opening
/// is now closed in SQL by excluding whole `activity_summary:*` messages in both `get_messages` and
/// `list_session_index_snapshot_rows`; see their documentation. This `unwrap_or_else` is only a final
/// defense: if another parse failure appears later because of dirty data or a missing variant for a
/// new block type, it leaves a searchable log instead of silently swallowing the error, clearing the
/// content, and telling nobody.
fn map_message_row(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    let id: i64 = r.get(0)?;
    let content_json: String = r.get(2)?;
    let content: Vec<Block> = serde_json::from_str(&content_json).unwrap_or_else(|error| {
        eprintln!("map_message_row: message {id} content 解析失败，回退为空 blocks: {error}");
        Vec::new()
    });
    Ok(Message {
        id,
        role: r.get(1)?,
        content,
        engine: r.get(3)?,
        agent_id: r.get(4)?,
        agent_name_snapshot: r.get(5)?,
        created_at: r.get(6)?,
        revision: r.get(7)?,
    })
}

/// R1 (msgfix2 full review): messages with the `activity_summary:*` dedup-key prefix documented by
/// `upsert_activity_summary_and_publish` do not enter this read path. Their content is an
/// aggregator-private JSON shape, not any tagged variant of `Vec<Block>`. The old implementation let
/// them reach `map_message_row`, where a parse failure was silently cleared to empty blocks by
/// `unwrap_or_default()`, contaminating three downstream paths: blank assistant bubbles in the desktop
/// conversation, empty assistant lines injected into the LLM prompt by `build_agent_prompt` in lib.rs
/// because its history comes directly from this function, and empty lines consuming slots intended
/// for real messages in the continuation.rs handoff window, which uses the same source. All three
/// downstream paths share this history source, so one SQL-level exclusion closes them together. There
/// is no need to add a `db::Block` variant and disturb exhaustive matches across the repository in
/// out-of-scope files such as member_runner.rs, lead_tools.rs, lead_step.rs, and memory_tools.rs.
pub fn get_messages(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, role, content, engine, agent_id, agent_name_snapshot, created_at, revision \
         FROM messages \
         WHERE session_id = ?1 AND (dedup_key IS NULL OR dedup_key NOT LIKE 'activity_summary:%') \
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([session_id], map_message_row)?;
    rows.collect()
}

pub fn get_message_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<Message>> {
    conn.query_row(
        "SELECT id, role, content, engine, agent_id, agent_name_snapshot, created_at, revision FROM messages WHERE id = ?1",
        [id],
        map_message_row,
    )
    .optional()
}

pub fn get_message_by_session_and_dedup_key(
    conn: &Connection,
    session_id: &str,
    dedup_key: &str,
) -> rusqlite::Result<Option<Message>> {
    conn.query_row(
        "SELECT id, role, content, engine, agent_id, agent_name_snapshot, created_at, revision \
         FROM messages WHERE session_id = ?1 AND dedup_key = ?2",
        (session_id, dedup_key),
        map_message_row,
    )
    .optional()
}

fn block_source_text(block: &Block) -> Option<String> {
    match block {
        Block::Text { text } | Block::Thinking { text } => Some(text.clone()),
        _ => None,
    }
}

#[allow(dead_code)]
fn char_range(text: &str, range: Option<[usize; 2]>) -> String {
    let Some([start, end]) = range else {
        return text.to_string();
    };
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

#[allow(dead_code)]
pub fn memory_read_source(conn: &Connection, anchor: &Anchor) -> rusqlite::Result<Option<String>> {
    if anchor.kind != "message" {
        return Ok(None);
    }
    let Ok(msg_id) = anchor.ref_id.parse::<i64>() else {
        return Ok(None);
    };
    let Some(message) = get_message_by_id(conn, msg_id)? else {
        return Ok(None);
    };
    let text = match anchor.block_index {
        Some(index) => {
            let Some(block) = message.content.get(index) else {
                return Ok(None);
            };
            match block_source_text(block) {
                Some(t) => t,
                None => return Ok(None),
            }
        }
        None => blocks_to_text(&message.content),
    };
    Ok(Some(char_range(&text, anchor.char_range)))
}

#[allow(dead_code)]
pub fn memory_read_source_json(
    conn: &Connection,
    anchor_json: &str,
) -> rusqlite::Result<Option<String>> {
    let value: serde_json::Value = match serde_json::from_str(anchor_json) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let anchor_value = value.get("anchor").cloned().unwrap_or(value);
    let raw_anchors: Vec<serde_json::Value> = if anchor_value.is_array() {
        serde_json::from_value(anchor_value).unwrap_or_default()
    } else {
        vec![anchor_value]
    };
    let mut parts = Vec::new();
    for raw in raw_anchors {
        let Ok(anchor) = serde_json::from_value::<Anchor>(raw) else {
            continue;
        };
        if let Some(text) = memory_read_source(conn, &anchor)? {
            parts.push(text);
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("\n\n")))
}

pub fn member_changed_paths_from_messages(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    assignment_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut paths = Vec::new();
    for msg in get_messages(conn, session_id)? {
        for block in msg.content {
            if let Block::TeamRun {
                run_id: rid,
                members,
                ..
            } = block
            {
                if rid != run_id {
                    continue;
                }
                for m in members {
                    if m.assignment_id != assignment_id {
                        continue;
                    }
                    if let Some(result) = m.result {
                        paths.extend(
                            result
                                .changed_files
                                .into_iter()
                                .map(|f| f.path.replace('\\', "/")),
                        );
                    }
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
