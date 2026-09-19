#![cfg(test)]

use super::*;

#[test]
fn now_secs_is_positive() {
    assert!(now_secs() > 1_600_000_000); // 2020+ 的 epoch 秒
}

#[test]
fn member_changed_paths_from_messages_reads_declared_team_run_paths() {
    let c = mem();
    create_session(&c, "s1", "T", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[Block::TeamRun {
            run_id: "r1".into(),
            goal: None,
            lead: Some("Claude".into()),
            members: vec![MemberSnapshot {
                participant_id: "worker-1".into(),
                assignment_id: "a1".into(),
                task_id: "t1".into(),
                name: "worker".into(),
                started_at: None,
                status: "done".into(),
                sub: "改文件".into(),
                steps_total: 1,
                steps_done: 1,
                cost_usd: None,
                input_tokens: 0,
                output_tokens: 0,
                failed: false,
                blocks: vec![],
                result: Some(crate::agent_event::MemberResult {
                    schema_version: 1,
                    assignment_id: "a1".into(),
                    participant_id: "worker-1".into(),
                    status: "done".into(),
                    failure_reason: None,
                    changed_files: vec![
                        crate::agent_event::ChangedFile {
                            path: "src/lib.rs".into(),
                            insertions: 1,
                            deletions: 0,
                        },
                        crate::agent_event::ChangedFile {
                            path: "README.md".into(),
                            insertions: 1,
                            deletions: 0,
                        },
                    ],
                    anchor: crate::agent_event::ResultAnchor {
                        base_sha: "base".into(),
                        head_sha: Some("head".into()),
                        diff_ref: None,
                        generated_from: "test".into(),
                    },
                    command_evidence: vec![],
                    risk_inputs: crate::agent_event::RiskInputs {
                        files_changed: 2,
                        cmd_danger: "none".into(),
                        reversibility: "clean".into(),
                    },
                    decisions: vec![],
                    risks: vec![],
                    final_text_ref: None,
                    artifact_refs: vec![],
                    result_source: "deterministic".into(),
                    requires_long_task: None,
                    exit_code: None,
                    stderr_tail: None,
                    failure_kind: None,
                }),
            }],
        }],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();

    let paths = member_changed_paths_from_messages(&c, "s1", "r1", "a1").unwrap();
    assert_eq!(paths, vec!["README.md", "src/lib.rs"]);
    let missing = member_changed_paths_from_messages(&c, "s1", "r1", "missing").unwrap();
    assert!(missing.is_empty());
}

#[test]
fn lead_loop_state_table_exists_with_autonomy_check() {
    let c = crate::test_support::mem_db();
    c.execute(
        "INSERT INTO lead_loop_state (session_id, updated_at) VALUES ('s1', 0)",
        [],
    )
    .unwrap();
    let autonomy: String = c
        .query_row(
            "SELECT autonomy FROM lead_loop_state WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(autonomy, "cautious", "autonomy 默认应为 cautious");
    let bad = c.execute(
        "INSERT INTO lead_loop_state (session_id, autonomy, updated_at) VALUES ('s2','bogus',0)",
        [],
    );
    assert!(bad.is_err(), "非法 autonomy 应被 CHECK 挡");
}

#[test]
fn lead_loop_state_crud_roundtrip() {
    let c = crate::test_support::mem_db();
    // 无行时 get 返回 cautious 默认（不写库）
    let st = get_lead_loop_state(&c, "s1").unwrap();
    assert_eq!(st.autonomy, "cautious");
    assert_eq!(st.active_run_id, None);

    // set_autonomy upsert（首次创建行）
    set_lead_autonomy(&c, "s1", "handsfree").unwrap();
    assert_eq!(get_lead_loop_state(&c, "s1").unwrap().autonomy, "handsfree");

    // set_active 更新 active 指针·不动 autonomy
    set_lead_active(&c, "s1", Some("run-9"), Some("task-2")).unwrap();
    let st = get_lead_loop_state(&c, "s1").unwrap();
    assert_eq!(st.active_run_id.as_deref(), Some("run-9"));
    assert_eq!(st.active_task_id.as_deref(), Some("task-2"));
    assert_eq!(st.autonomy, "handsfree", "set_active 不应重置 autonomy");

    // set_cursor 更新游标·不动 autonomy/active
    set_lead_event_cursor(&c, "s1", "evt-42").unwrap();
    let st = get_lead_loop_state(&c, "s1").unwrap();
    assert_eq!(st.last_event_cursor.as_deref(), Some("evt-42"));
    assert_eq!(st.autonomy, "handsfree", "set_cursor 不应重置 autonomy");
    assert_eq!(
        st.active_run_id.as_deref(),
        Some("run-9"),
        "set_cursor 不应重置 active"
    );
}

#[test]
fn set_and_get_lead_autonomy_roundtrip_all_three() {
    let c = crate::test_support::mem_db();
    for a in ["cautious", "handsfree", "auto"] {
        set_lead_autonomy(&c, "s1", a).unwrap();
        assert_eq!(get_lead_loop_state(&c, "s1").unwrap().autonomy, a);
    }
}

#[test]
fn set_lead_autonomy_rejects_invalid_value() {
    let c = crate::test_support::mem_db();
    assert!(set_lead_autonomy(&c, "s1", "yolo").is_err());
}

#[test]
fn decision_ledger_run_id_is_nullable() {
    let c = crate::test_support::mem_db();
    c.execute(
            "INSERT INTO decision_ledger (session_id, run_id, text, created_at) VALUES ('s1', NULL, '直接回复', 0)",
            [],
        )
        .unwrap();
    let cnt: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM decision_ledger WHERE session_id='s1' AND run_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cnt, 1);
}

#[test]
fn decision_ledger_old_notnull_table_migrated_to_nullable() {
    // 手造 NOT NULL 旧表 + 插数据 → init_schema 触发重建 → 断言放宽+不丢数据+index 重建+id 延续。
    // 用裸 open_in_memory（不走 mem_db·因后者跑 init_schema 会把基表建成 nullable·触发不了重建分支）。
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE decision_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                source_assignment_id TEXT,
                text TEXT NOT NULL,
                source_refs_json TEXT NOT NULL DEFAULT '[]',
                supersedes_json TEXT NOT NULL DEFAULT '[]',
                source_kind TEXT,
                confidence TEXT,
                created_at INTEGER NOT NULL
            );
            INSERT INTO decision_ledger (id, session_id, run_id, text, created_at)
                VALUES (7, 's1', 'run-1', '旧决策', 100);",
    )
    .unwrap();
    init_schema(&c).unwrap();
    let run_id_notnull: i64 = c
        .query_row(
            "SELECT \"notnull\" FROM pragma_table_info('decision_ledger') WHERE name='run_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(run_id_notnull, 0, "run_id 应已放宽为 nullable");
    let (text, created): (String, i64) = c
        .query_row(
            "SELECT text, created_at FROM decision_ledger WHERE id=7",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(text, "旧决策");
    assert_eq!(created, 100);
    c.execute(
            "INSERT INTO decision_ledger (session_id, run_id, text, created_at) VALUES ('s2', NULL, '新', 0)",
            [],
        )
        .unwrap();
    let idx_cnt: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_decision_ledger_session'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(idx_cnt, 1, "迁移后 idx_decision_ledger_session 应重建");
    let new_id: i64 = c
        .query_row(
            "SELECT id FROM decision_ledger WHERE session_id='s2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        new_id > 7,
        "AUTOINCREMENT 应延续·新 id 应 > 7·实际 {new_id}"
    );

    // 幂等：再跑一次 init_schema·不重建·数据不丢（刀2.1 终审 NIT）
    init_schema(&c).unwrap();
    let still: (String, i64) = c
        .query_row(
            "SELECT text, created_at FROM decision_ledger WHERE id=7",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(still.0, "旧决策");
    let nn2: i64 = c
        .query_row(
            "SELECT \"notnull\" FROM pragma_table_info('decision_ledger') WHERE name='run_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nn2, 0, "二次 init_schema 后 run_id 仍 nullable·未重建坏");
}

#[test]
fn state_machine_tables_created_by_init_schema() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    for tbl in ["artifacts", "verifications", "reviews", "merge_candidates"] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [tbl],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "表 {tbl} 应被 init_schema 建出");
    }
    // artifacts 关键列存在
    let cols: Vec<String> = {
        let mut s = conn.prepare("PRAGMA table_info(artifacts)").unwrap();
        let r = s.query_map([], |row| row.get::<_, String>(1)).unwrap();
        r.map(|c| c.unwrap()).collect()
    };
    for c in [
        "id",
        "session_id",
        "run_id",
        "member_assignment_id",
        "branch",
        "base_sha",
        "commit_sha",
        "files_changed",
        "state",
        "created_at",
    ] {
        assert!(cols.iter().any(|x| x == c), "artifacts 缺列 {c}");
    }
}

#[test]
fn verification_crud_and_latest_verdict() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let mk = |id: &str, verdict: &str, exit: Option<i64>, at: i64| Verification {
        id: id.into(),
        artifact_id: "art-1".into(),
        cmd: "cargo test".into(),
        artifact_sha: "sha-abc".into(),
        exit_code: exit,
        output_ref: Some("ok".into()),
        verdict: verdict.into(),
        created_at: at,
    };
    insert_verification(&conn, &mk("v-1", "failed", Some(1), 100)).unwrap();
    insert_verification(&conn, &mk("v-2", "passed", Some(0), 200)).unwrap();
    // 另一 artifact 的不串
    insert_verification(
        &conn,
        &Verification {
            id: "v-x".into(),
            artifact_id: "art-2".into(),
            ..mk("v-x", "passed", Some(0), 300)
        },
    )
    .unwrap();

    let got = get_verification(&conn, "v-2").unwrap().unwrap();
    assert_eq!(got.verdict, "passed");
    assert_eq!(got.exit_code, Some(0));
    assert_eq!(got.artifact_sha, "sha-abc");

    // list：按 created_at 升序·只 art-1 的两条
    let list = list_verifications_for_artifact(&conn, "art-1").unwrap();
    let ids: Vec<&str> = list.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, vec!["v-1", "v-2"]);

    // latest_verdict：art-1 最新（created_at 最大）= v-2 passed
    assert_eq!(
        latest_verdict_for_artifact(&conn, "art-1")
            .unwrap()
            .as_deref(),
        Some("passed")
    );
    // 没 verification 的 artifact → None
    assert_eq!(latest_verdict_for_artifact(&conn, "art-zzz").unwrap(), None);
}
