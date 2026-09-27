use rusqlite::{Connection, OptionalExtension};

/// Coding closed loop, Blade 1 (spec §1.7): a controlled commit artifact that records worker changes.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Artifact {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub member_assignment_id: String,
    pub branch: String,
    pub base_sha: String,
    pub commit_sha: Option<String>,
    pub files_changed: i64,
    pub state: String,
    pub created_at: i64,
}

const ARTIFACT_COLS: &str =
    "id, session_id, run_id, member_assignment_id, branch, base_sha, commit_sha, files_changed, state, created_at";

#[allow(dead_code)]
fn map_artifact_row(r: &rusqlite::Row) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        member_assignment_id: r.get(3)?,
        branch: r.get(4)?,
        base_sha: r.get(5)?,
        commit_sha: r.get(6)?,
        files_changed: r.get(7)?,
        state: r.get(8)?,
        created_at: r.get(9)?,
    })
}

#[allow(dead_code)]
pub fn insert_artifact(conn: &Connection, a: &Artifact) -> rusqlite::Result<()> {
    conn.execute(
        &format!("INSERT INTO artifacts ({ARTIFACT_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"),
        rusqlite::params![
            a.id,
            a.session_id,
            a.run_id,
            a.member_assignment_id,
            a.branch,
            a.base_sha,
            a.commit_sha,
            a.files_changed,
            a.state,
            a.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub fn get_artifact(conn: &Connection, id: &str) -> rusqlite::Result<Option<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts WHERE id = ?1"
    ))?;
    let mut rows = stmt.query_map([id], map_artifact_row)?;
    rows.next().transpose()
}

/// State transition: `state` must change; update `commit_sha`/`files_changed` only when `Some` (`None` does not overwrite existing values, which is idempotency-friendly).
#[allow(dead_code)]
pub fn set_artifact_state(
    conn: &Connection,
    id: &str,
    state: &str,
    commit_sha: Option<&str>,
    files_changed: Option<i64>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE artifacts SET state = ?2, \
         commit_sha = COALESCE(?3, commit_sha), \
         files_changed = COALESCE(?4, files_changed) \
         WHERE id = ?1",
        rusqlite::params![id, state, commit_sha, files_changed],
    )?;
    Ok(())
}

/// For recovery: find all artifacts stuck in `finalizing` (a crash occurred between the commit and its database write).
#[allow(dead_code)]
pub fn list_finalizing_artifacts(conn: &Connection) -> rusqlite::Result<Vec<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts WHERE state = 'finalizing' ORDER BY id"
    ))?;
    let rows = stmt.query_map([], map_artifact_row)?;
    rows.collect()
}

/// For recovery: return artifacts whose finalize operation crashed midway (`state=finalizing`),
/// so the caller can **retain** these members' worktrees (do not clean them, or the only source of the changes would be lost; spec §1.7 crash recovery).
/// Integration point: the recovery flow started by `lib.rs` (wire it in during Plan 6 integration; exclude these `member_assignment_id` values before cleaning worktrees).
#[allow(dead_code)]
pub fn recover_finalizing_artifacts(conn: &Connection) -> rusqlite::Result<Vec<Artifact>> {
    list_finalizing_artifacts(conn)
}

/// For idempotency (incorporated from review, codex#6): look up an existing artifact by (session, run, member), so repeated finalize operations find it.
#[allow(dead_code)]
pub fn get_artifact_by_member(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    member_assignment_id: &str,
) -> rusqlite::Result<Option<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts \
         WHERE session_id = ?1 AND run_id = ?2 AND member_assignment_id = ?3"
    ))?;
    let mut rows = stmt.query_map([session_id, run_id, member_assignment_id], map_artifact_row)?;
    rows.next().transpose()
}

pub fn merged_artifact_for_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts \
         WHERE session_id = ?1 AND run_id = ?2 AND state = 'merged' \
         ORDER BY created_at DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map([session_id, run_id], map_artifact_row)?;
    rows.next().transpose()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingCommit {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub artifact_id: Option<String>,
    pub pre_head: String,
    pub landed_head: String,
    pub commit_count: i64,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
    pub created_at: i64,
}

pub fn insert_landing_commit(conn: &Connection, l: &LandingCommit) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO landing_commits \
         (id, session_id, run_id, artifact_id, pre_head, landed_head, commit_count, files_changed, insertions, deletions, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![
            l.id,
            l.session_id,
            l.run_id,
            l.artifact_id,
            l.pre_head,
            l.landed_head,
            l.commit_count,
            l.files_changed,
            l.insertions,
            l.deletions,
            l.created_at
        ],
    )?;
    Ok(())
}

const LANDING_COMMIT_COLS: &str = "id, session_id, run_id, artifact_id, pre_head, landed_head, \
     commit_count, files_changed, insertions, deletions, created_at";

fn map_landing_commit_row(r: &rusqlite::Row) -> rusqlite::Result<LandingCommit> {
    Ok(LandingCommit {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        artifact_id: r.get(3)?,
        pre_head: r.get(4)?,
        landed_head: r.get(5)?,
        commit_count: r.get(6)?,
        files_changed: r.get(7)?,
        insertions: r.get(8)?,
        deletions: r.get(9)?,
        created_at: r.get(10)?,
    })
}

/// Read the latest landing record for a session/run to obtain the `pre_head` and `landed_head` undo anchors.
/// IDs increase monotonically (`idx_landing_commits_session` is therefore ordered by ID); take the latest with ID descending. Read-only.
pub fn latest_landing_commit(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<LandingCommit>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {LANDING_COMMIT_COLS} FROM landing_commits \
         WHERE session_id = ?1 AND run_id = ?2 ORDER BY id DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map([session_id, run_id], map_landing_commit_row)?;
    rows.next().transpose()
}

/// Review attribution: read the session starting point from the session's earliest landing record.
///
/// When `created_at` falls in the same second, use `rowid` to break ties by insertion order.
pub fn earliest_landing_pre_head_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT pre_head FROM landing_commits \
         WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC LIMIT 1",
        [session_id],
        |row| row.get(0),
    )
    .optional()
}

/// Review attribution: list every recorded landing commit range for the session in insertion order.
pub fn landing_commit_ranges_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT pre_head, landed_head FROM landing_commits \
         WHERE session_id = ?1 ORDER BY rowid ASC",
    )?;
    let rows = stmt
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect();
    rows
}

/// Incorporated from review (b2b): find the most recent run in the session that has been merged into staging but has not yet landed,
/// returning `(run_id, base_sha, merged_sha)`.
#[allow(dead_code)]
pub fn latest_staged_unlanded_run(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.run_id, a.base_sha, mc.merged_sha \
         FROM artifacts a \
         JOIN merge_candidates mc ON mc.artifact_id = a.id \
         LEFT JOIN landing_commits lc ON lc.session_id = a.session_id AND lc.run_id = a.run_id \
         WHERE a.session_id = ?1 \
           AND a.state = 'merged' \
           AND mc.state = 'merged' \
           AND mc.merged_sha IS NOT NULL \
           AND lc.id IS NULL \
         ORDER BY a.created_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map([session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    rows.next().transpose()
}

/// Coding closed loop, Blade 1 (spec §L1): a record of one verification command run by the harness in a temporary checkout at `artifact_sha`.
#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct Verification {
    pub id: String,
    pub artifact_id: String,
    pub cmd: String,
    pub artifact_sha: String,
    pub exit_code: Option<i64>,
    pub output_ref: Option<String>,
    pub verdict: String,
    pub created_at: i64,
}

const VERIFICATION_COLS: &str =
    "id, artifact_id, cmd, artifact_sha, exit_code, output_ref, verdict, created_at";

#[allow(dead_code)]
fn map_verification_row(r: &rusqlite::Row) -> rusqlite::Result<Verification> {
    Ok(Verification {
        id: r.get(0)?,
        artifact_id: r.get(1)?,
        cmd: r.get(2)?,
        artifact_sha: r.get(3)?,
        exit_code: r.get(4)?,
        output_ref: r.get(5)?,
        verdict: r.get(6)?,
        created_at: r.get(7)?,
    })
}

#[allow(dead_code)]
pub fn insert_verification(conn: &Connection, v: &Verification) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO verifications ({VERIFICATION_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"
        ),
        rusqlite::params![
            v.id,
            v.artifact_id,
            v.cmd,
            v.artifact_sha,
            v.exit_code,
            v.output_ref,
            v.verdict,
            v.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub fn get_verification(conn: &Connection, id: &str) -> rusqlite::Result<Option<Verification>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERIFICATION_COLS} FROM verifications WHERE id = ?1"
    ))?;
    let mut rows = stmt.query_map([id], map_verification_row)?;
    rows.next().transpose()
}

/// All verifications for an artifact, in ascending time order (earliest to latest).
#[allow(dead_code)]
pub fn list_verifications_for_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Vec<Verification>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERIFICATION_COLS} FROM verifications \
         WHERE artifact_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([artifact_id], map_verification_row)?;
    rows.collect()
}

/// The latest verdict for an artifact (for the Plan 3 merge gate, which permits merging only when L1 is green; prepared in this plan); return `None` when there is no record.
#[allow(dead_code)]
pub fn latest_verdict_for_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT verdict FROM verifications WHERE artifact_id = ?1 \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map([artifact_id], |r| r.get::<_, String>(0))?;
    rows.next().transpose()
}

/// Coding closed loop, Blade 1 (spec §L1): a candidate for merging an artifact into the run staging branch, plus the result record.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MergeCandidate {
    pub id: String,
    pub artifact_id: String,
    pub staging_branch: String,
    pub state: String,
    pub merged_sha: Option<String>,
    pub created_at: i64,
}

const MERGE_CANDIDATE_COLS: &str = "id, artifact_id, staging_branch, state, merged_sha, created_at";

#[allow(dead_code)]
fn map_merge_candidate_row(r: &rusqlite::Row) -> rusqlite::Result<MergeCandidate> {
    Ok(MergeCandidate {
        id: r.get(0)?,
        artifact_id: r.get(1)?,
        staging_branch: r.get(2)?,
        state: r.get(3)?,
        merged_sha: r.get(4)?,
        created_at: r.get(5)?,
    })
}

#[allow(dead_code)]
pub fn get_merge_candidate_by_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Option<MergeCandidate>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MERGE_CANDIDATE_COLS} FROM merge_candidates WHERE artifact_id = ?1"
    ))?;
    let mut rows = stmt.query_map([artifact_id], map_merge_candidate_row)?;
    rows.next().transpose()
}

/// Idempotent upsert: `artifact_id` is UNIQUE; on conflict, update `state`/`merged_sha` (retain the first row's `id`/`created_at` and do not insert a duplicate).
#[allow(dead_code)]
pub fn upsert_merge_candidate(conn: &Connection, m: &MergeCandidate) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO merge_candidates ({MERGE_CANDIDATE_COLS}) VALUES (?1,?2,?3,?4,?5,?6) \
             ON CONFLICT(artifact_id) DO UPDATE SET \
             state = excluded.state, merged_sha = excluded.merged_sha, \
             staging_branch = excluded.staging_branch"
        ),
        rusqlite::params![
            m.id,
            m.artifact_id,
            m.staging_branch,
            m.state,
            m.merged_sha,
            m.created_at
        ],
    )?;
    Ok(())
}

/// For the merge gate (incorporated from review, codex P1): the latest complete verification for an artifact (including `artifact_sha`).
/// The merge prerequisite is "the latest verification has `verdict=passed` **and** `artifact_sha` equals the `commit_sha` to be merged"—
/// checking only the verdict without binding the SHA would let a passed result for an old SHA admit a new commit. Return the complete row (not only the verdict).
#[allow(dead_code)]
pub fn latest_verification_for_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Option<Verification>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERIFICATION_COLS} FROM verifications WHERE artifact_id = ?1 \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map([artifact_id], map_verification_row)?;
    rows.next().transpose()
}
