use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SessionIndexSnapshotRow {
    pub id: String,
    pub title: String,
    pub repo_id: Option<String>,
    pub archived: bool,
    pub status: Option<String>,
    pub run_id: Option<String>,
    pub updated_at: i64,
    pub last_msg_preview: Option<String>,
    pub last_activity_at: Option<i64>,
    /// Human-readable project name for mobile (used as the remote-control session-list subtitle),
    /// obtained via LEFT JOIN repos. It becomes None when the repo has been deleted or repo_id is
    /// None; following the existing convention (`repo_id` as the display fallback), the consumer
    /// falls back to the bare id.
    pub repo_name: Option<String>,
}

const SESSION_INDEX_PREVIEW_CHARS: usize = 80;

fn session_index_message_preview(content_json: &str) -> Option<String> {
    let content = serde_json::from_str::<serde_json::Value>(content_json).ok()?;
    let blocks = content.as_array()?;
    let text = blocks.iter().find_map(|block| {
        (block.get("type")?.as_str()? == "text")
            .then(|| block.get("text")?.as_str())
            .flatten()
    })?;
    Some(text.chars().take(SESSION_INDEX_PREVIEW_CHARS).collect())
}

/// Used by the post-connection full-snapshot provider in M0 §6 (remote_gateway
/// `session_index_snapshot_provider`).
/// LEFT JOIN all non-soft-deleted sessions with session_runtime to summarize runtime state; when
/// session_runtime has no corresponding row, status/run_id become None (the session has never run,
/// or the table has not yet been written), and updated_at falls back to sessions.created_at.
pub fn list_session_index_snapshot_rows(
    conn: &Connection,
) -> rusqlite::Result<Vec<SessionIndexSnapshotRow>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.title, s.repo_id, s.archived, sr.status, sr.run_id, \
                COALESCE(sr.updated_at, s.created_at) AS updated_at, \
                latest_message.content, latest_message.created_at, r.name \
         FROM sessions s \
         LEFT JOIN session_runtime sr ON sr.session_id = s.id \
         LEFT JOIN repos r ON r.id = s.repo_id \
         LEFT JOIN messages latest_message ON latest_message.id = ( \
             SELECT m.id FROM messages m \
             WHERE m.session_id = s.id AND m.role IN ('user', 'assistant') \
               AND (m.dedup_key IS NULL OR m.dedup_key NOT LIKE 'activity_summary:%') \
             ORDER BY m.id DESC LIMIT 1 \
         ) \
         WHERE s.deleted_at IS NULL \
         ORDER BY s.pinned DESC, s.created_at DESC, s.id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let latest_content: Option<String> = r.get(7)?;
        Ok(SessionIndexSnapshotRow {
            id: r.get(0)?,
            title: r.get(1)?,
            repo_id: r.get(2)?,
            archived: r.get(3)?,
            status: r.get(4)?,
            run_id: r.get(5)?,
            updated_at: r.get(6)?,
            last_msg_preview: latest_content
                .as_deref()
                .and_then(session_index_message_preview),
            last_activity_at: r.get(8)?,
            repo_name: r.get(9)?,
        })
    })?;
    rows.collect()
}

/// M0 v1.7.5 §4d: used by the post-connection replay-batch provider—the most recent N messages
/// where "role IN (assistant, user) and dedup_key is non-NULL" (across all sessions, excluding
/// soft-deleted sessions, using the same s.deleted_at IS NULL criterion as
/// list_session_index_snapshot_rows, without filtering archived). Select the most recent limit
/// rows by id DESC, then reverse them into ascending order (old → new) before returning them, so
/// the caller can reconstruct msg.completed in order and replay each one. Rows whose content fails
/// to deserialize (theoretically unreachable: content is always produced by
/// serde_json::to_string(&[Block])) are defensively skipped rather than failing the entire query.
/// P0-c: include user rows (assistant-only is a narrow criterion left over from Knife R—user
/// messages can now store a dedup_key and should be included in replay); the
/// `RECENT_MILESTONE_REPLAY_LIMIT` constant itself remains unchanged, and its 200-row budget is now
/// shared by the assistant and user roles rather than reserved for assistant.
#[derive(Clone, Debug, PartialEq)]
pub struct MilestoneReplayRow {
    pub session_id: String,
    pub message_id: i64,
    pub role: String,
    pub content_json: serde_json::Value,
    /// Preserve the original database `content` string alongside `content_json` so byte-based hashes and sizes remain exact.
    /// content_ref's sha256/total_bytes must be calculated from these original bytes, not from the
    /// reserialized `content_json` (`Value` → `Map` sorts by key by default, so its bytes are not
    /// guaranteed to match the original text).
    pub content: String,
    pub dedup_key: String,
    /// The message's current `messages.revision`, read from the database.
    pub revision: i64,
}

/// Result of the short-lock DB read for `control.history`. This only carries the raw SQLite
/// columns; the provider must parse `content` as JSON after releasing the global Db mutex, to keep
/// deserialization of large messages from expanding the lock scope.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionHistoryRow {
    pub message_id: i64,
    pub role: String,
    pub content: String,
    /// The message's current `messages.revision`, read from the database.
    pub revision: i64,
}

pub fn list_session_history_rows(
    conn: &Connection,
    session_id: &str,
    before_message_id: Option<i64>,
    max_rows: i64,
) -> rusqlite::Result<Vec<SessionHistoryRow>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.role, m.content, m.revision FROM messages m \
         JOIN sessions s ON s.id = m.session_id \
         WHERE m.session_id = ?1 AND m.role IN ('user','assistant') \
           AND (?2 IS NULL OR m.id < ?2) AND s.deleted_at IS NULL \
         ORDER BY m.id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map((session_id, before_message_id, max_rows), |r| {
        Ok(SessionHistoryRow {
            message_id: r.get(0)?,
            role: r.get(1)?,
            content: r.get(2)?,
            revision: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// Fetch by the exact `(session_id, message_id)` pair for `msg.fetch`, preserving ownership, deletion state, and raw content.
/// Full-content fetch—unlike `list_session_history_rows`, whose query puts `JOIN sessions ...
/// deleted_at IS NULL` directly in the WHERE clause, merging "the message does not exist" and "the
/// message exists but its session is soft-deleted" into the same empty result at the SQL level and
/// leaving the caller without the information needed to distinguish `not_found` from
/// `soft_deleted`. Meanwhile, `get_message_by_id` does not carry `session_id` (the `Message` struct
/// does not have this field in the first place), and its `content` has already been parsed into
/// `Vec<Block>`, losing the original string—neither can satisfy the dual requirements of the
/// three-state `msg.fetch` determination and calculating `content_ref` sha256 from the original
/// bytes (see the `MilestoneReplayRow`/`SessionHistoryRow` documentation for the same concern).
///
/// Two-stage query: first query the joined tables by `(session_id, message_id)`—a match means the
/// message does belong to this session, while also returning `sessions.deleted_at` to determine
/// whether the session is soft-deleted. A miss cannot be classified directly as `not_found` (the
/// message may simply belong to a different session), so a second global existence probe is needed
/// to distinguish "unauthorized" from "truly nonexistent."
#[derive(Clone, Debug, PartialEq)]
pub enum MessageForFetch {
    /// The message does belong to this session; `session_deleted` indicates whether its session has
    /// been soft-deleted (`sessions.deleted_at IS NOT NULL`). `content` is the original DB string,
    /// without any deserialization/reserialization—the caller must use this original text, not the
    /// parsed `Value`, to calculate `content_sha256` or create slices.
    Found {
        content: String,
        revision: i64,
        session_deleted: bool,
    },
    /// `message_id` exists in the `messages` table but does not belong to the `session_id` claimed
    /// by the caller (unauthorized).
    WrongSession,
    /// `message_id` does not exist anywhere in the `messages` table.
    NotFound,
}

pub fn get_message_for_fetch(
    conn: &Connection,
    session_id: &str,
    message_id: i64,
) -> rusqlite::Result<MessageForFetch> {
    let owned: Option<(String, i64, Option<i64>)> = conn
        .query_row(
            "SELECT m.content, m.revision, s.deleted_at \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE m.session_id = ?1 AND m.id = ?2",
            (session_id, message_id),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((content, revision, session_deleted_at)) = owned {
        return Ok(MessageForFetch::Found {
            content,
            revision,
            session_deleted: session_deleted_at.is_some(),
        });
    }
    // Keep fallback existence checks within the same repository; an unscoped message lookup would expose a global existence oracle.
    // Leaving ownership unrestricted would expose "whether this message_id exists under any repo
    // in the entire database" as a probeable global oracle (a valid id from another repo returns
    // `forbidden`, while a fabricated id returns `not_found`, and the differing responses make
    // enumeration possible). Tighten this to the **same-repo scope**: return `WrongSession` only
    // when `message_id` exists and its session and the `session_id` claimed by the caller belong to
    // **the same repo** (their `sessions.repo_id` values match). This is unauthorized but within the
    // same domain, so returning `forbidden` is reasonable—the attacker is already authorized to
    // access the list of other sessions in the same repo, and revealing "this id is in a project
    // you can see" discloses no new unauthorized information. Existence in another repo and total
    // nonexistence both produce the **same response**, `NotFound`/`not_found`, leaving the attacker
    // no binary criterion.
    //
    // `s.repo_id IS (SELECT repo_id FROM sessions WHERE id = ?2)` uses SQLite's `IS` (NULL-safe
    // equality) instead of `=`—`repo_id` is nullable (local default sessions have no project;
    // `ALTER TABLE sessions ADD COLUMN repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL`). With
    // `=`, `NULL = NULL` evaluates to NULL (false), misclassifying "both are default sessions with
    // no project" as "different repos" and incorrectly downgrading unauthorized probing between
    // such sessions to `not_found` (it should be `forbidden`). When the subquery matches a
    // nonexistent `session_id`, it returns NULL, and `s.repo_id IS NULL` is true only when this
    // session itself is also a default session—this does not accidentally loosen the determination
    // merely because the session claimed by the caller does not exist (`command_session_allowed`
    // already blocks nonexistent sessions at an earlier step; this is defense in depth, not the
    // only line of defense).
    let exists_in_same_repo: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM messages m \
             JOIN sessions s ON s.id = m.session_id \
             WHERE m.id = ?1 \
               AND s.repo_id IS (SELECT repo_id FROM sessions WHERE id = ?2)",
            (message_id, session_id),
            |r| r.get(0),
        )
        .optional()?;
    Ok(if exists_in_same_repo.is_some() {
        MessageForFetch::WrongSession
    } else {
        MessageForFetch::NotFound
    })
}

/// Bounded constant for the replay set, aligned with the relay retention window (7 days / 10,000
/// rows, M0 §6).
pub const RECENT_MILESTONE_REPLAY_LIMIT: i64 = 200;

pub fn list_recent_milestone_replay_rows(
    conn: &Connection,
    limit: i64,
) -> rusqlite::Result<Vec<MilestoneReplayRow>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.session_id, m.role, m.content, m.dedup_key, m.revision \
         FROM messages m \
         JOIN sessions s ON s.id = m.session_id \
         WHERE m.role IN ('assistant', 'user') AND m.dedup_key IS NOT NULL AND s.deleted_at IS NULL \
         ORDER BY m.id DESC \
         LIMIT ?1",
    )?;
    let raw_rows: Vec<(i64, String, String, String, String, i64)> = stmt
        .query_map([limit], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rows: Vec<MilestoneReplayRow> = raw_rows
        .into_iter()
        .filter_map(
            |(message_id, session_id, role, content_text, dedup_key, revision)| {
                let content_json = serde_json::from_str(&content_text).ok()?;
                Some(MilestoneReplayRow {
                    session_id,
                    message_id,
                    role,
                    content_json,
                    content: content_text,
                    dedup_key,
                    revision,
                })
            },
        )
        .collect();
    rows.reverse();
    Ok(rows)
}

/// Reconnect replay reads the current `run.status` snapshot here to restore the mobile top bar's authoritative state.
/// The `run.status` milestone (remote-web `streamSource.ts`) is published only once when the state
/// changes and is dropped without retry when the gate is closed. Previously, the post-connection
/// replay batch reconstructed only msg.completed/card.* and omitted it—joining midway or missing a
/// frame would leave the top bar stuck at the last observed state (out of sync with the green dot in
/// the session list, whose status comes from the `session.index` row). Here, the full current state
/// of `session_runtime` (excluding soft-deleted sessions, using the same criterion as
/// `list_session_index_snapshot_rows`) is given to the caller to reconstruct and replay a
/// `run.status` frame for each row; status is always non-NULL (CHECK constraint), while run_id may
/// be null.
///
/// Large session counts risk dropped replay frames because `enqueue_milestone_with_generation` uses a bounded channel.
/// Putting too many entries from the replay batch onto the channel at once can fill the channel and
/// even crowd out the current running-state frames that should be delivered first. Two safeguards
/// are added here, aligning the criteria with the msg/card half of replay
/// (`list_recent_milestone_replay_rows` + `RECENT_MILESTONE_REPLAY_LIMIT`, above): (1) `ORDER BY`
/// puts running rows first—if the channel really fills up, the sessions enqueued first and most
/// deserving of preservation are those that are "running," not arbitrary idle ones; (2) `LIMIT`
/// reuses the same upper-bound constant of 200, preventing this batch from occupying the entire
/// channel and crowding out other kinds of replay frames if the sessions table grows without bound.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRuntimeReplayRow {
    pub session_id: String,
    pub status: String,
    pub run_id: Option<String>,
}

pub fn list_session_runtime_replay_rows(
    conn: &Connection,
) -> rusqlite::Result<Vec<SessionRuntimeReplayRow>> {
    let mut stmt = conn.prepare(
        "SELECT sr.session_id, sr.status, sr.run_id \
         FROM session_runtime sr \
         JOIN sessions s ON s.id = sr.session_id \
         WHERE s.deleted_at IS NULL \
         ORDER BY CASE WHEN sr.status = 'running' THEN 0 ELSE 1 END \
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([RECENT_MILESTONE_REPLAY_LIMIT], |r| {
        Ok(SessionRuntimeReplayRow {
            session_id: r.get(0)?,
            status: r.get(1)?,
            run_id: r.get(2)?,
        })
    })?;
    rows.collect()
}
