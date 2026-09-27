use rusqlite::Connection;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryBlock {
    pub session_id: String,
    pub slot: String,
    pub text: String,
    pub title: Option<String>,
    pub anchor_refs_json: String,
    pub updated_by: Option<String>,
    pub updated_at: i64,
    pub revision: i64,
    pub updated_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactState {
    pub summary: String,
    pub through_message_id: i64,
    pub revision: i64,
}

/// Overwrite-slot upsert (goal/state/next): the same (session_id, slot) is unique; a write overwrites the old value (medical-record "current only").
pub fn upsert_memory_block(
    conn: &Connection,
    session_id: &str,
    slot: &str,
    text: &str,
    title: Option<&str>,
    updated_by: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO memory_blocks (session_id, slot, text, title, updated_by, updated_at, revision) \
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), 1) \
         ON CONFLICT(session_id, slot) DO UPDATE SET \
           text = excluded.text, title = excluded.title, \
           updated_by = excluded.updated_by, updated_at = excluded.updated_at, \
           revision = revision + 1",
        rusqlite::params![session_id, slot, text, title, updated_by],
    )?;
    Ok(())
}

pub fn get_memory_block(
    conn: &Connection,
    session_id: &str,
    slot: &str,
) -> rusqlite::Result<Option<MemoryBlock>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT session_id, slot, text, title, anchor_refs_json, updated_by, updated_at, revision, updated_run_id \
         FROM memory_blocks WHERE session_id = ?1 AND slot = ?2",
        rusqlite::params![session_id, slot],
        |r| {
            Ok(MemoryBlock {
                session_id: r.get(0)?,
                slot: r.get(1)?,
                text: r.get(2)?,
                title: r.get(3)?,
                anchor_refs_json: r.get(4)?,
                updated_by: r.get(5)?,
                updated_at: r.get(6)?,
                revision: r.get(7)?,
                updated_run_id: r.get(8)?,
            })
        },
    )
    .optional()
}

pub fn get_compact_state(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<CompactState>> {
    let Some(block) = get_memory_block(conn, session_id, "compact")? else {
        return Ok(None);
    };
    let anchors: serde_json::Value = match serde_json::from_str(&block.anchor_refs_json) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(anchors) = anchors.as_array() else {
        return Ok(None);
    };
    if anchors.len() != 1 || anchors[0].get("kind").and_then(|v| v.as_str()) != Some("message") {
        return Ok(None);
    }
    let Some(through_message_id) = anchors[0].get("ref").and_then(|v| v.as_i64()) else {
        return Ok(None);
    };
    Ok(Some(CompactState {
        summary: block.text,
        through_message_id,
        revision: block.revision,
    }))
}

pub fn upsert_compact_state(
    conn: &Connection,
    session_id: &str,
    summary: &str,
    through_message_id: i64,
    run_id: Option<&str>,
) -> rusqlite::Result<()> {
    let anchor_refs_json = serde_json::json!([{
        "kind": "message",
        "ref": through_message_id,
    }])
    .to_string();
    conn.execute(
        "INSERT INTO memory_blocks \
         (session_id, slot, text, title, anchor_refs_json, updated_by, updated_at, revision, updated_run_id) \
         VALUES (?1, 'compact', ?2, NULL, ?3, 'autocompact', strftime('%s','now'), 1, ?5) \
         ON CONFLICT(session_id, slot) DO UPDATE SET \
           text = excluded.text, title = NULL, anchor_refs_json = excluded.anchor_refs_json, \
           updated_by = excluded.updated_by, updated_at = excluded.updated_at, \
           revision = memory_blocks.revision + 1, updated_run_id = excluded.updated_run_id \
         WHERE CASE \
           WHEN json_valid(memory_blocks.anchor_refs_json) = 0 THEN 1 \
           WHEN json_type(memory_blocks.anchor_refs_json) IS NOT 'array' THEN 1 \
           WHEN json_array_length(memory_blocks.anchor_refs_json) != 1 THEN 1 \
           WHEN json_extract(memory_blocks.anchor_refs_json, '$[0].kind') IS NOT 'message' THEN 1 \
           WHEN json_type(memory_blocks.anchor_refs_json, '$[0].ref') IS NOT 'integer' THEN 1 \
           ELSE json_extract(memory_blocks.anchor_refs_json, '$[0].ref') <= ?4 \
         END",
        rusqlite::params![
            session_id,
            summary,
            anchor_refs_json,
            through_message_id,
            run_id
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum MemorySetOutcome {
    Applied { revision: i64 },
    Conflict { current_revision: i64 },
}

/// Optimistic-lock write to an overwrite slot (goal/state/next).
/// base_revision = the slot revision read by the caller; a nonexistent slot is treated as revision 0.
/// Write only on a match (revision+1); on a mismatch, return Conflict without writing—preventing lost updates.
///
/// NOTE: Atomicity depends on the outer Mutex<Connection> serializing access (the command holds the lock throughout), not on a DB transaction.
/// For hardening across concurrent connections, do not directly call this concurrently with &Connection.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn memory_set(
    conn: &Connection,
    session_id: &str,
    slot: &str,
    text: &str,
    title: Option<&str>,
    updated_by: Option<&str>,
    updated_run_id: Option<&str>,
    base_revision: i64,
) -> rusqlite::Result<MemorySetOutcome> {
    use rusqlite::OptionalExtension;
    let current_rev: Option<i64> = conn
        .query_row(
            "SELECT revision FROM memory_blocks WHERE session_id = ?1 AND slot = ?2",
            rusqlite::params![session_id, slot],
            |r| r.get(0),
        )
        .optional()?;

    match current_rev {
        None => {
            if base_revision == 0 {
                conn.execute(
                    "INSERT INTO memory_blocks (session_id, slot, text, title, updated_by, updated_at, revision, updated_run_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), 1, ?6)",
                    rusqlite::params![session_id, slot, text, title, updated_by, updated_run_id],
                )?;
                Ok(MemorySetOutcome::Applied { revision: 1 })
            } else {
                Ok(MemorySetOutcome::Conflict {
                    current_revision: 0,
                })
            }
        }
        Some(rev) => {
            if rev == base_revision {
                let new_rev = rev + 1;
                conn.execute(
                    "UPDATE memory_blocks SET text = ?1, title = ?2, updated_by = ?3, \
                     updated_at = strftime('%s','now'), revision = ?4, updated_run_id = ?5 \
                     WHERE session_id = ?6 AND slot = ?7",
                    rusqlite::params![
                        text,
                        title,
                        updated_by,
                        new_rev,
                        updated_run_id,
                        session_id,
                        slot
                    ],
                )?;
                Ok(MemorySetOutcome::Applied { revision: new_rev })
            } else {
                Ok(MemorySetOutcome::Conflict {
                    current_revision: rev,
                })
            }
        }
    }
}
