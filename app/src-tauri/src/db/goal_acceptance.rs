use rusqlite::Connection;

#[allow(dead_code)] // Retained for fake_runner to represent persisted goal contracts.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GoalContract {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub goal: String,
    pub lead_participant_id: String,
    /// Contract state is 'draft' or 'frozen'; direct creation uses 'frozen', while editable drafts use 'draft'.
    pub status: String,
    /// Assignment draft (each unit's subtask+assignee+scope_files+acceptance); JSON array; DEFAULT '[]'.
    pub assignments_json: String,
    pub created_at: i64,
}

#[allow(dead_code)] // Retained for fake_runner to represent acceptance criteria.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub task_id: String,
    pub contract_id: Option<String>,
    /// 'run' | 'task'
    pub scope: String,
    pub claim: String,
    pub verifier: Option<String>,
    pub evidence: Option<String>,
    /// 'pending' | 'passed' | 'failed' | 'waived'
    pub status: String,
    pub waiver: Option<String>,
    pub created_at: i64,
}

#[allow(dead_code)] // Retained for fake_runner to persist goal contracts.
pub fn insert_goal_contract(conn: &Connection, g: &GoalContract) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO goal_contracts
            (id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            g.id,
            g.session_id,
            g.run_id,
            g.goal,
            g.lead_participant_id,
            g.status,
            g.assignments_json,
            g.created_at
        ],
    )?;
    Ok(())
}

/// Insert draft contracts idempotently for manual retries: unique/primary-key conflicts do nothing; NOT NULL/CHECK failures propagate.
/// The difference from insert_goal_contract is ON CONFLICT DO NOTHING (idempotent only for existing rows; schema-level real errors are not masked).
#[allow(dead_code)]
pub fn insert_goal_contract_if_absent(conn: &Connection, g: &GoalContract) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO goal_contracts
            (id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT DO NOTHING",
        rusqlite::params![
            g.id,
            g.session_id,
            g.run_id,
            g.goal,
            g.lead_participant_id,
            g.status,
            g.assignments_json,
            g.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)] // Retained for fake_runner to read the goal contract for a run.
pub fn get_goal_contract_by_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<GoalContract>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at
         FROM goal_contracts WHERE session_id = ?1 AND run_id = ?2",
    )?;
    let mut rows = stmt.query_map([session_id, run_id], |r| {
        Ok(GoalContract {
            id: r.get(0)?,
            session_id: r.get(1)?,
            run_id: r.get(2)?,
            goal: r.get(3)?,
            lead_participant_id: r.get(4)?,
            status: r.get(5)?,
            assignments_json: r.get(6)?,
            created_at: r.get(7)?,
        })
    })?;
    match rows.next() {
        Some(g) => Ok(Some(g?)),
        None => Ok(None),
    }
}

/// Set the lead-generated run-level goal summary for the top bar without changing the `GoalContract` struct.
#[allow(dead_code)]
pub fn set_goal_title_for_run(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    goal_title: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE goal_contracts SET goal_title = ?1 WHERE session_id = ?2 AND run_id = ?3",
        rusqlite::params![goal_title, session_id, run_id],
    )?;
    Ok(())
}

/// Read the run-level goal summary; both a missing row and a NULL `goal_title` return `Ok(None)`.
#[allow(dead_code)]
pub fn goal_title_for_run(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT goal_title FROM goal_contracts WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![session_id, run_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map(|opt| opt.flatten())
}

#[allow(dead_code)] // Retained for fake_runner to persist acceptance criteria.
pub fn insert_acceptance(conn: &Connection, c: &AcceptanceCriterion) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO acceptance_criteria
            (id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            c.id,
            c.session_id,
            c.run_id,
            c.task_id,
            c.contract_id,
            c.scope,
            c.claim,
            c.verifier,
            c.evidence,
            c.status,
            c.waiver,
            c.created_at
        ],
    )?;
    Ok(())
}

/// Insert acceptance idempotently (freeze→start reuses the same run): an id conflict does nothing (preserving the existing row, including status/waiver).
/// Other constraints (NOT NULL/CHECK) still report errors—this is idempotent only for existing rows and does not mask schema-level real errors.
#[allow(dead_code)]
pub fn insert_acceptance_if_absent(
    conn: &Connection,
    c: &AcceptanceCriterion,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO acceptance_criteria
            (id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO NOTHING",
        rusqlite::params![
            c.id,
            c.session_id,
            c.run_id,
            c.task_id,
            c.contract_id,
            c.scope,
            c.claim,
            c.verifier,
            c.evidence,
            c.status,
            c.waiver,
            c.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)] // Retained for fake_runner to read run acceptance criteria.
pub fn list_acceptance_by_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Vec<AcceptanceCriterion>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at
         FROM acceptance_criteria
         WHERE session_id = ?1 AND run_id = ?2
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([session_id, run_id], |r| {
        Ok(AcceptanceCriterion {
            id: r.get(0)?,
            session_id: r.get(1)?,
            run_id: r.get(2)?,
            task_id: r.get(3)?,
            contract_id: r.get(4)?,
            scope: r.get(5)?,
            claim: r.get(6)?,
            verifier: r.get(7)?,
            evidence: r.get(8)?,
            status: r.get(9)?,
            waiver: r.get(10)?,
            created_at: r.get(11)?,
        })
    })?;
    rows.collect()
}

/// At freeze time, persist the edited contract in one transaction: UPDATE goal_contracts(goal/assignments_json/status=frozen)
/// + replace all acceptance criteria for this run (DELETE old + INSERT edited). draft→frozen is one-way (enforcing the state machine).
#[allow(dead_code)] // Retained for the `freeze_team_plan` command in lib.rs to persist the frozen contract.
pub fn freeze_team_contract(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    goal: &str,
    assignments_json: &str,
    criteria: &[AcceptanceCriterion],
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let updated = tx.execute(
        "UPDATE goal_contracts
            SET goal = ?3, assignments_json = ?4, status = 'frozen'
            WHERE session_id = ?1 AND run_id = ?2 AND status = 'draft'",
        rusqlite::params![session_id, run_id, goal, assignments_json],
    )?;
    if updated != 1 {
        // The contract does not exist or is no longer a draft (already frozen) → do not silently replace criteria; return an error (enforcing one-way draft→frozen).
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    // Replace criteria only after UPDATE draft→frozen succeeds.
    tx.execute(
        "DELETE FROM acceptance_criteria WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![session_id, run_id],
    )?;
    for c in criteria {
        tx.execute(
            "INSERT INTO acceptance_criteria
                (id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                c.id,
                c.session_id,
                c.run_id,
                c.task_id,
                c.contract_id,
                c.scope,
                c.claim,
                c.verifier,
                c.evidence,
                c.status,
                c.waiver,
                c.created_at
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn update_acceptance_waiver(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    criterion_id: &str,
    reason: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE acceptance_criteria SET status='waived', waiver=?1
         WHERE id=?2 AND session_id=?3 AND run_id=?4",
        rusqlite::params![reason, criterion_id, session_id, run_id],
    )?;
    Ok(())
}
