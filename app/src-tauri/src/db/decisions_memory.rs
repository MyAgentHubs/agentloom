use rusqlite::{Connection, OptionalExtension};

/// A medical-record-style append-grid entry (decision / pitfall / risk / pending item). Writes always append a new row; supersede pointers replace old entries.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEntry {
    pub id: i64,
    pub session_id: String,
    pub category: String,
    pub text: String,
    pub source_refs_json: String,
    pub supersedes_json: String,
    pub source: Option<String>,
    pub confidence: Option<String>,
    pub pinned: bool,
    pub created_at: i64,
}

/// M2 §5.2: an append-only `decision_ledger` row; wired into `lead_step` in Blade 2.1 Plan 2.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionRow {
    pub id: i64,
    pub session_id: String,
    pub run_id: Option<String>,
    pub source_assignment_id: Option<String>,
    pub text: String,
    pub source_refs_json: String,
    pub supersedes_json: String,
    pub source_kind: Option<String>,
    pub confidence: Option<String>,
    pub created_at: i64,
}

const DECISION_LEDGER_COLS: &str = "id, session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at";
const MEMORY_ENTRY_COLS: &str =
    "id, session_id, category, text, source_refs_json, supersedes_json, source, confidence, pinned, created_at";

fn map_decision_row(r: &rusqlite::Row) -> rusqlite::Result<DecisionRow> {
    Ok(DecisionRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get::<_, Option<String>>(2)?,
        source_assignment_id: r.get(3)?,
        text: r.get(4)?,
        source_refs_json: r.get(5)?,
        supersedes_json: r.get(6)?,
        source_kind: r.get(7)?,
        confidence: r.get(8)?,
        created_at: r.get(9)?,
    })
}

fn map_memory_entry_row(r: &rusqlite::Row) -> rusqlite::Result<MemoryEntry> {
    Ok(MemoryEntry {
        id: r.get(0)?,
        session_id: r.get(1)?,
        category: r.get(2)?,
        text: r.get(3)?,
        source_refs_json: r.get(4)?,
        supersedes_json: r.get(5)?,
        source: r.get(6)?,
        confidence: r.get(7)?,
        pinned: r.get::<_, i64>(8)? != 0,
        created_at: r.get(9)?,
    })
}

/// M2 §5.2: only append to `decision_ledger`, preserving the `source_refs`/`supersedes` provenance chain; wired into `lead_step` in Blade 2.1 Plan 2.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn insert_decision(
    conn: &Connection,
    session_id: &str,
    run_id: Option<&str>,
    source_assignment_id: Option<&str>,
    text: &str,
    source_refs_json: &str,
    supersedes_json: &str,
    source_kind: &str,
    confidence: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO decision_ledger \
         (session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%s','now'))",
        (
            session_id,
            run_id,
            source_assignment_id,
            text,
            source_refs_json,
            supersedes_json,
            source_kind,
            confidence,
        ),
    )?;
    Ok(())
}

/// M2 §5.2: read `decision_ledger` in append order; wired into `lead_step` in Blade 2.1 Plan 2.
#[allow(dead_code)]
pub fn list_decisions(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<DecisionRow>> {
    let sql = format!(
        "SELECT {DECISION_LEDGER_COLS} FROM decision_ledger WHERE session_id = ?1 ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([session_id], map_decision_row)?;
    rows.collect()
}

/// Append one medical-record entry (append-only). `source_refs_json` / `supersedes_json` must be valid JSON,
/// otherwise return `Err` (with an error message). Returns the new row ID.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn insert_memory_entry(
    conn: &Connection,
    session_id: &str,
    category: &str,
    text: &str,
    source_refs_json: &str,
    supersedes_json: &str,
    source: Option<&str>,
    confidence: Option<&str>,
    pinned: bool,
) -> rusqlite::Result<i64> {
    // Validate JSON validity at the Rust layer (bypassing differences in SQLite CHECK error modes).
    serde_json::from_str::<serde_json::Value>(source_refs_json).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            crate::ui_msg::al_err(
                "db.memory.badJson",
                &[
                    ("field", "source_refs_json".to_string()),
                    ("detail", e.to_string()),
                ],
            ),
        )))
    })?;
    serde_json::from_str::<serde_json::Value>(supersedes_json).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            crate::ui_msg::al_err(
                "db.memory.badJson",
                &[
                    ("field", "supersedes_json".to_string()),
                    ("detail", e.to_string()),
                ],
            ),
        )))
    })?;
    conn.execute(
        "INSERT INTO memory_entries \
         (session_id, category, text, source_refs_json, supersedes_json, source, confidence, pinned, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%s','now'))",
        rusqlite::params![
            session_id,
            category,
            text,
            source_refs_json,
            supersedes_json,
            source,
            confidence,
            pinned as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List a session's medical-record entries. `include_superseded=false` returns only "live rows" (IDs not superseded by other rows in this session);
/// `true` returns all rows (audit / recall). Both are ordered by ID ascending.
// NOTE: supersedes_json/source_refs_json are validated as legal JSON only in phase 1 (not strict integer arrays).
// The active-row query is robust to null/non-integer elements via NOT EXISTS + je.type='integer'.
// Strict integer-array validation deferred to phase 1d / phase 3.
#[allow(dead_code)]
pub fn list_memory_entries(
    conn: &Connection,
    session_id: &str,
    include_superseded: bool,
) -> rusqlite::Result<Vec<MemoryEntry>> {
    let sql = if include_superseded {
        format!(
            "SELECT {MEMORY_ENTRY_COLS} FROM memory_entries \
             WHERE session_id = ?1 ORDER BY id ASC"
        )
    } else {
        // NOT EXISTS correlated subquery:
        //  - Filters only "more recent" rows (e.id > m.id) to guard against backward supersede.
        //  - je.type = 'integer' skips null/non-integer elements, preventing the SQL three-valued
        //    logic trap where NULL inside NOT IN causes every row to evaluate to NULL (non-TRUE).
        //  - Phase 1 only validates JSON validity; strict integer-array enforcement deferred to 1d/phase 3.
        format!(
            "SELECT {MEMORY_ENTRY_COLS} FROM memory_entries m \
             WHERE m.session_id = ?1 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM memory_entries e, json_each(e.supersedes_json) je \
                   WHERE e.session_id = ?1 \
                     AND e.id > m.id \
                     AND je.type = 'integer' \
                     AND CAST(je.value AS INTEGER) = m.id \
               ) \
             ORDER BY m.id ASC"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([session_id], map_memory_entry_row)?;
    rows.collect()
}

/// Blade 2.1 (spec §6.1): session-level persistent state for the Lead Decision Loop.
#[allow(dead_code)] // Wired into `lead_step` in Plan 2.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeadLoopState {
    pub session_id: String,
    pub autonomy: String, // cautious | handsfree | auto
    pub active_run_id: Option<String>,
    pub active_task_id: Option<String>,
    pub last_event_cursor: Option<String>,
}

impl LeadLoopState {
    fn default_for(session_id: &str) -> Self {
        LeadLoopState {
            session_id: session_id.to_string(),
            autonomy: "cautious".to_string(),
            active_run_id: None,
            active_task_id: None,
            last_event_cursor: None,
        }
    }
}

/// Read the session decision-loop state; if no row exists, return the cautious default (without writing to the database).
#[allow(dead_code)] // Wired into `lead_step` in Plan 2.
pub fn get_lead_loop_state(conn: &Connection, session_id: &str) -> rusqlite::Result<LeadLoopState> {
    let mut stmt = conn.prepare(
        "SELECT session_id, autonomy, active_run_id, active_task_id, last_event_cursor \
         FROM lead_loop_state WHERE session_id = ?1",
    )?;
    let row = stmt
        .query_row([session_id], |r| {
            Ok(LeadLoopState {
                session_id: r.get(0)?,
                autonomy: r.get(1)?,
                active_run_id: r.get(2)?,
                active_task_id: r.get(3)?,
                last_event_cursor: r.get(4)?,
            })
        })
        .optional()?;
    Ok(row.unwrap_or_else(|| LeadLoopState::default_for(session_id)))
}

/// Set the autonomy level (upsert; safe level; readable by the backend).
#[allow(dead_code)] // Wired into `lead_step` in Plan 2.
pub fn set_lead_autonomy(
    conn: &Connection,
    session_id: &str,
    autonomy: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO lead_loop_state (session_id, autonomy, updated_at) \
         VALUES (?1, ?2, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET autonomy = excluded.autonomy, updated_at = excluded.updated_at",
        (session_id, autonomy),
    )?;
    Ok(())
}

/// Set the current active run/task pointers (upsert; leave autonomy unchanged).
#[allow(dead_code)] // Wired into `lead_step` in Plan 2.
pub fn set_lead_active(
    conn: &Connection,
    session_id: &str,
    active_run_id: Option<&str>,
    active_task_id: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO lead_loop_state (session_id, active_run_id, active_task_id, updated_at) \
         VALUES (?1, ?2, ?3, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET \
            active_run_id = excluded.active_run_id, \
            active_task_id = excluded.active_task_id, \
            updated_at = excluded.updated_at",
        (session_id, active_run_id, active_task_id),
    )?;
    Ok(())
}

/// Set `last_event_cursor` (upsert; used for idempotent deduplication).
#[allow(dead_code)] // Wired into `lead_step` in Plan 2.
pub fn set_lead_event_cursor(
    conn: &Connection,
    session_id: &str,
    cursor: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO lead_loop_state (session_id, last_event_cursor, updated_at) \
         VALUES (?1, ?2, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET \
            last_event_cursor = excluded.last_event_cursor, updated_at = excluded.updated_at",
        (session_id, cursor),
    )?;
    Ok(())
}
