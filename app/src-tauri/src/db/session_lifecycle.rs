use rusqlite::{Connection, OptionalExtension};

/// Sessions must bind repo_id and namespace_id (NOT NULL at the business layer).
pub fn create_session(
    conn: &Connection,
    id: &str,
    title: &str,
    repo_id: &str,
    namespace_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) VALUES (?1, ?2, ?3, ?4, strftime('%s','now'))",
        (id, title, repo_id, namespace_id),
    )?;
    // The remote-control mobile session-list subtitle needs a human-readable project name, so include one with the created
    // update instead of making the mobile client wait for the next full snapshot. If the repo is not found (theoretically unreachable because repo_id is always constrained by a foreign key),
    // silently use None so this lookup failure does not prevent session creation.
    let repo_name = crate::repos_repo::get_repo_by_id(conn, repo_id)
        .ok()
        .flatten()
        .map(|repo| repo.name);
    crate::remote_gateway::publish_session_index_created(
        id,
        title,
        repo_id,
        namespace_id,
        repo_name.as_deref(),
    );
    Ok(())
}

pub fn add_session_usage(
    conn: &Connection,
    session_id: &str,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET \
         total_input_tokens = total_input_tokens + ?1, \
         total_output_tokens = total_output_tokens + ?2 \
         WHERE id = ?3",
        (
            input_tokens.unwrap_or(0) as i64,
            output_tokens.unwrap_or(0) as i64,
            session_id,
        ),
    )?;
    Ok(())
}

pub fn set_session_parent(
    conn: &Connection,
    id: &str,
    parent_id: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET parent_session_id = ?2 WHERE id = ?1",
        (id, parent_id),
    )?;
    Ok(())
}

pub fn set_session_continued_to(
    conn: &Connection,
    id: &str,
    child_id: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET continued_to_session_id = ?2 WHERE id = ?1",
        (id, child_id),
    )?;
    Ok(())
}

fn continuation_lineage_error(message: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(message.to_string()),
    )
}

fn continuation_next_child(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let continued_to: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
            [session_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let live_children = conn
        .prepare(
            "SELECT id FROM sessions
             WHERE parent_session_id = ?1 AND deleted_at IS NULL
             ORDER BY id
             LIMIT 2",
        )?
        .query_map([session_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if live_children.len() > 1 {
        return Err(continuation_lineage_error(
            "continuation lineage has multiple live children",
        ));
    }

    match (continued_to, live_children.into_iter().next()) {
        (Some(pointer), Some(live_child)) if pointer != live_child => Err(
            continuation_lineage_error("continuation lineage child pointer mismatch"),
        ),
        (Some(pointer), _) => Ok(Some(pointer)),
        (None, live_child) => Ok(live_child),
    }
}

pub fn continuation_chain_ids(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<String>> {
    const MAX_CONTINUATION_CHAIN_LEN: usize = 128;
    let mut root = session_id.to_string();
    let mut upward_seen = vec![root.clone()];
    loop {
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_session_id FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
                [root.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(parent_id) = parent else { break };
        if upward_seen.len() >= MAX_CONTINUATION_CHAIN_LEN {
            return Err(continuation_lineage_error(
                "continuation lineage exceeds maximum depth",
            ));
        }
        if upward_seen.iter().any(|seen| seen == &parent_id) {
            return Err(continuation_lineage_error("continuation lineage cycle"));
        }
        root = parent_id;
        upward_seen.push(root.clone());
    }

    let mut ids = vec![root.clone()];
    let mut current = root;
    loop {
        let child = continuation_next_child(conn, &current)?;
        let Some(child_id) = child else { break };
        if ids.len() >= MAX_CONTINUATION_CHAIN_LEN {
            return Err(continuation_lineage_error(
                "continuation lineage exceeds maximum depth",
            ));
        }
        if ids.iter().any(|seen| seen == &child_id) {
            return Err(continuation_lineage_error("continuation lineage cycle"));
        }
        ids.push(child_id.clone());
        current = child_id;
    }
    Ok(ids)
}

pub fn rename_session(conn: &Connection, id: &str, title: &str) -> rusqlite::Result<()> {
    let changed = conn.execute("UPDATE sessions SET title = ?2 WHERE id = ?1", (id, title))?;
    if changed > 0 {
        crate::remote_gateway::publish_session_index_renamed(id, title);
    }
    Ok(())
}

/// Get the project id bound to the session (NULL means the default session with no project).
pub fn get_session_repo_id(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT repo_id FROM sessions WHERE id = ?1")?;
    let res = stmt.query_row([session_id], |r| r.get::<_, Option<String>>(0));
    match res {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read `workspace_scope`; `'root'` preserves the legacy project-root working directory for existing sessions.
/// Existing local-default sessions from before per-session subdirectory isolation are backfilled by a one-time migration;
/// NULL or any other value means the new behavior (a per-session subdirectory). Returns `Ok(None)` when the session does not exist,
/// matching the fault-tolerance behavior of other `get_session_*` queries, so callers use the existing “session not found” path.
pub fn get_session_workspace_scope(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT workspace_scope FROM sessions WHERE id = ?1")?;
    let res = stmt.query_row([session_id], |r| r.get::<_, Option<String>>(0));
    match res {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Continuation sessions inherit the parent's `workspace_scope` so legacy `'root'` sessions retain project-root access.
/// Continuations should keep working at the project root; otherwise the legacy exception protects only the parent and the continuation is sent back to a subdirectory.
/// If the parent is a new session (NULL), the continuation also remains NULL, matching newly created sessions.
pub fn set_session_workspace_scope(
    conn: &Connection,
    session_id: &str,
    scope: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET workspace_scope = ?2 WHERE id = ?1",
        (session_id, scope),
    )?;
    Ok(())
}

/// Get the namespace id for the session (NULL or missing means None).
pub fn get_session_namespace_id(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT namespace_id FROM sessions WHERE id = ?1")?;
    let res = stmt.query_row([session_id], |r| r.get::<_, Option<String>>(0));
    match res {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}
