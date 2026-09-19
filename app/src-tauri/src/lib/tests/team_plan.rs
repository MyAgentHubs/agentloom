#![cfg(test)]

use super::*;

#[test]
fn list_acceptance_command_logic_reads_rows() {
    let c = crate::test_support::mem_db();
    db::insert_acceptance(
        &c,
        &db::AcceptanceCriterion {
            id: "c1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            task_id: "t1".into(),
            contract_id: None,
            scope: "task".into(),
            claim: "测试绿".into(),
            verifier: Some("npm test".into()),
            evidence: None,
            status: "pending".into(),
            waiver: None,
            created_at: 1,
        },
    )
    .unwrap();
    let rows = db::list_acceptance_by_run(&c, "s1", "r1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].claim, "测试绿");
}

#[test]
fn freeze_team_plan_command_freezes_persisted_draft() {
    let conn = crate::test_support::mem_db();
    db::insert_goal_contract(
        &conn,
        &db::GoalContract {
            id: "r9-gc".into(),
            session_id: "s9".into(),
            run_id: "r9".into(),
            goal: "g".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: 1,
        },
    )
    .unwrap();
    // 经 db 层冻结（command 仅在其上加锁 + serde·已上一 task 覆盖事务语义）
    db::freeze_team_contract(&conn, "s9", "r9", "g2", "[]", &[]).unwrap();
    let gc = db::get_goal_contract_by_run(&conn, "s9", "r9")
        .unwrap()
        .unwrap();
    assert_eq!(gc.status, "frozen");
    assert_eq!(gc.goal, "g2");
}

#[test]
fn insert_goal_contract_row_persists_draft() {
    let conn = crate::test_support::mem_db();
    // 直接验底层（command 薄壳·真核 db::insert_goal_contract）
    db::insert_goal_contract(
        &conn,
        &db::GoalContract {
            id: "m1-gc".into(),
            session_id: "s1".into(),
            run_id: "m1".into(),
            goal: "手动填的目标".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: db::now_secs(),
        },
    )
    .unwrap();
    let gc = db::get_goal_contract_by_run(&conn, "s1", "m1")
        .unwrap()
        .unwrap();
    assert_eq!(gc.status, "draft");
    assert_eq!(gc.goal, "手动填的目标");
}
