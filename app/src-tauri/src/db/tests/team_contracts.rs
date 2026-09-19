#![cfg(test)]

use super::*;

#[test]
fn team_run_pending_insert_and_recover() {
    let c = crate::test_support::mem_db();
    // 建表已在 init_schema；插一条 running
    insert_team_run_pending(
        &c,
        "s1",
        "run-1",
        "目标X",
        "lead-1",
        r#"[{"assignment_id":"a1"}]"#,
    )
    .unwrap();
    // recover 扫 running → 标 interrupted·返回受影响行数 + assignments
    let recovered = recover_interrupted_team_runs(&c).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].session_id, "s1");
    assert_eq!(recovered[0].run_id, "run-1");
    assert_eq!(recovered[0].goal.as_deref(), Some("目标X"));
    assert_eq!(recovered[0].lead_participant_id.as_deref(), Some("lead-1"));
    assert_eq!(recovered[0].assignments_json, r#"[{"assignment_id":"a1"}]"#);
    let run_1_state: String = c
        .query_row(
            "SELECT state FROM team_run_pending WHERE run_id = ?1",
            ["run-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(run_1_state, "interrupted");
    // 幂等：再扫一次 0 条（已非 running）
    assert_eq!(recover_interrupted_team_runs(&c).unwrap().len(), 0);
    // mark done 不被 recover 碰
    insert_team_run_pending(&c, "s1", "run-2", "目标Y", "lead-1", "[]").unwrap();
    mark_team_run_done(&c, "s1", "run-2").unwrap();
    assert_eq!(recover_interrupted_team_runs(&c).unwrap().len(), 0);
    let run_2_state: String = c
        .query_row(
            "SELECT state FROM team_run_pending WHERE run_id = ?1",
            ["run-2"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(run_2_state, "done");
}

#[test]
fn list_interrupted_returns_session_interrupted_rows() {
    let c = crate::test_support::mem_db();
    insert_team_run_pending(&c, "s1", "run-1", "目标X", "lead-1", r#"["a1"]"#).unwrap();
    insert_team_run_pending(&c, "s2", "run-2", "目标Y", "lead-2", "[]").unwrap();
    // 崩溃恢复：running → interrupted（两行都标）
    recover_interrupted_team_runs(&c).unwrap();
    // s1 列出 1 行 interrupted·内容正确
    let s1 = list_interrupted_team_runs(&c, "s1").unwrap();
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].run_id, "run-1");
    assert_eq!(s1[0].goal.as_deref(), Some("目标X"));
    assert_eq!(s1[0].assignments_json, r#"["a1"]"#);
    // s2 也有 1 行
    assert_eq!(list_interrupted_team_runs(&c, "s2").unwrap().len(), 1);
    // 不存在 session → 空
    assert_eq!(list_interrupted_team_runs(&c, "sX").unwrap().len(), 0);
    // done 态不算 interrupted
    insert_team_run_pending(&c, "s3", "run-3", "目标Z", "lead-3", "[]").unwrap();
    mark_team_run_done(&c, "s3", "run-3").unwrap();
    assert_eq!(list_interrupted_team_runs(&c, "s3").unwrap().len(), 0);
}

#[test]
fn decision_ledger_append_and_list() {
    let c = crate::test_support::mem_db();
    insert_decision(
        &c,
        "s1",
        Some("run-1"),
        Some("a1"),
        "选用方案X",
        r#"[{"run_id":"run-1","assignment_id":"a1","block_index":3}]"#,
        "[]",
        "worker_tail",
        Some("high"),
    )
    .unwrap();
    insert_decision(
        &c,
        "s1",
        Some("run-1"),
        Some("a1"),
        "改用方案Y",
        "[]",
        "[1]",
        "lead_extract",
        None,
    )
    .unwrap();
    let rows = list_decisions(&c, "s1").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].text, "选用方案X");
    assert_eq!(
        rows[0].source_refs_json,
        r#"[{"run_id":"run-1","assignment_id":"a1","block_index":3}]"#
    );
    assert_eq!(rows[1].supersedes_json, "[1]");
    assert!(rows[0].id < rows[1].id);
    assert_eq!(rows[0].source_assignment_id.as_deref(), Some("a1"));
    assert_eq!(rows[0].source_kind.as_deref(), Some("worker_tail"));
    assert_eq!(rows[0].confidence.as_deref(), Some("high"));
    assert_eq!(rows[1].confidence, None);
}

#[test]
fn insert_decision_accepts_null_run_id() {
    let c = crate::test_support::mem_db();
    // 无 run 的 lead 决策（如 reply）：run_id = None
    insert_decision(
        &c,
        "s1",
        None,
        None,
        "直接回复用户",
        "[]",
        "[]",
        "lead_action",
        Some("high"),
    )
    .unwrap();
    let rows = list_decisions(&c, "s1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_id, None);
    assert_eq!(rows[0].text, "直接回复用户");
}

#[test]
fn record_dispatch_logs_dispatch_worker_kind() {
    let c = crate::test_support::mem_db();
    insert_decision(
        &c,
        "s1",
        None,
        None,
        "改 README｜task: 写新闻",
        "[\"README.md\"]",
        "[]",
        "dispatch_worker",
        None,
    )
    .unwrap();
    let rows = list_decisions(&c, "s1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source_kind.as_deref(), Some("dispatch_worker"));
    assert_eq!(rows[0].run_id, None, "lead 派单决策无 run·run_id 为空");
}

#[test]
fn goal_contract_and_acceptance_round_trip() {
    let c = Connection::open_in_memory().unwrap();
    init_schema(&c).unwrap();
    init_schema(&c).unwrap(); // 幂等

    let gc = GoalContract {
        id: "gc1".into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        goal: "实现 stage 2 心情记录".into(),
        lead_participant_id: "lead".into(),
        status: "frozen".into(),
        assignments_json: "[]".into(),
        created_at: 100,
    };
    insert_goal_contract(&c, &gc).unwrap();
    assert_eq!(get_goal_contract_by_run(&c, "s1", "r1").unwrap(), Some(gc));
    assert!(get_goal_contract_by_run(&c, "s1", "other")
        .unwrap()
        .is_none());

    let crit = AcceptanceCriterion {
        id: "ac1".into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        task_id: "t1".into(),
        contract_id: Some("gc1".into()),
        scope: "task".into(),
        claim: "mood-record 测试通过".into(),
        verifier: Some("npm test mood-record".into()),
        evidence: None,
        status: "pending".into(),
        waiver: None,
        created_at: 100,
    };
    insert_acceptance(&c, &crit).unwrap();
    assert_eq!(list_acceptance_by_run(&c, "s1", "r1").unwrap(), vec![crit]);
    assert!(list_acceptance_by_run(&c, "s1", "other")
        .unwrap()
        .is_empty());
}

#[test]
fn goal_contract_roundtrips_assignments_json() {
    let c = mem();
    let g = GoalContract {
        id: "r1-gc".into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        goal: "建登录".into(),
        lead_participant_id: "lead".into(),
        status: "draft".into(),
        assignments_json: r#"[{"subtask_id":"s1"}]"#.into(),
        created_at: 100,
    };
    insert_goal_contract(&c, &g).unwrap();
    let got = get_goal_contract_by_run(&c, "s1", "r1").unwrap().unwrap();
    assert_eq!(got.status, "draft");
    assert_eq!(got.assignments_json, r#"[{"subtask_id":"s1"}]"#);
    assert_eq!(got.created_at, 100);
}

#[test]
fn freeze_team_contract_flips_status_and_replaces_criteria() {
    let conn = mem();
    // 先落一个 draft 契约 + 2 条 draft criteria（模拟 B1 propose 的产物）
    insert_goal_contract(
        &conn,
        &GoalContract {
            id: "r1-gc".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            goal: "旧目标".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: 100,
        },
    )
    .unwrap();
    for (i, claim) in ["旧验收A", "旧验收B"].iter().enumerate() {
        insert_acceptance(
            &conn,
            &AcceptanceCriterion {
                id: format!("r1-c{i}"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                task_id: "t1".into(),
                contract_id: Some("r1-gc".into()),
                scope: "task".into(),
                claim: (*claim).into(),
                verifier: None,
                evidence: None,
                status: "pending".into(),
                waiver: None,
                created_at: 100 + i as i64,
            },
        )
        .unwrap();
    }

    // 冻结：改了 goal、assignments_json，criteria 换成编辑后的 1 条
    let edited = vec![AcceptanceCriterion {
        id: "r1-cNEW".into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        task_id: "t1".into(),
        contract_id: Some("r1-gc".into()),
        scope: "task".into(),
        claim: "新验收·用户改过".into(),
        verifier: Some("npm test".into()),
        evidence: None,
        status: "pending".into(),
        waiver: None,
        created_at: 200,
    }];
    freeze_team_contract(
        &conn,
        "s1",
        "r1",
        "新目标·用户改过",
        "[{\"subtask_id\":\"t1\"}]",
        &edited,
    )
    .unwrap();

    // 契约：status=frozen·goal/assignments 已更新
    let gc = get_goal_contract_by_run(&conn, "s1", "r1")
        .unwrap()
        .unwrap();
    assert_eq!(gc.status, "frozen");
    assert_eq!(gc.goal, "新目标·用户改过");
    assert_eq!(gc.assignments_json, "[{\"subtask_id\":\"t1\"}]");
    // criteria：旧 2 条被替换成新 1 条
    let cs = list_acceptance_by_run(&conn, "s1", "r1").unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].claim, "新验收·用户改过");
    assert_eq!(cs[0].verifier.as_deref(), Some("npm test"));
    assert_eq!(cs[0].status, "pending");
}

#[test]
fn freeze_team_contract_rejects_non_draft() {
    let conn = mem();
    insert_goal_contract(
        &conn,
        &GoalContract {
            id: "r2-gc".into(),
            session_id: "s2".into(),
            run_id: "r2".into(),
            goal: "g".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: 1,
        },
    )
    .unwrap();
    // 首次冻结 draft→frozen·ok
    freeze_team_contract(&conn, "s2", "r2", "g2", "[]", &[]).unwrap();
    // 再次冻结（已 frozen）→ 返错·不静默改
    assert!(freeze_team_contract(&conn, "s2", "r2", "g3", "[]", &[]).is_err());
    // 契约不存在 → 返错
    assert!(freeze_team_contract(&conn, "sX", "rX", "g", "[]", &[]).is_err());
    // 已 frozen 的 goal 没被第二次调用改成 g3
    let gc = get_goal_contract_by_run(&conn, "s2", "r2")
        .unwrap()
        .unwrap();
    assert_eq!(gc.goal, "g2");
}

#[test]
fn insert_goal_contract_if_absent_is_idempotent_on_conflict() {
    let conn = mem();
    let g = GoalContract {
        id: "x-gc".into(),
        session_id: "s1".into(),
        run_id: "x1".into(),
        goal: "g".into(),
        lead_participant_id: "lead".into(),
        status: "draft".into(),
        assignments_json: "[]".into(),
        created_at: 1,
    };
    insert_goal_contract_if_absent(&conn, &g).unwrap();
    // 再插同 run_id → 不报错（幂等）·仍只 1 行
    insert_goal_contract_if_absent(&conn, &g).unwrap();
    let gc = get_goal_contract_by_run(&conn, "s1", "x1")
        .unwrap()
        .unwrap();
    assert_eq!(gc.goal, "g");
}

#[test]
fn migration_adds_assignments_json_to_old_goal_contracts() {
    // codex P1-2：旧库（无 assignments_json 列）→ init_schema 迁移加列·默认 '[]'·幂等。
    let c = Connection::open_in_memory().unwrap();
    // 手建「旧 schema」goal_contracts（无 assignments_json 列）
    c.execute(
        "CREATE TABLE goal_contracts (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, run_id TEXT NOT NULL UNIQUE,
                goal TEXT NOT NULL, lead_participant_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'draft', created_at INTEGER NOT NULL )",
        [],
    )
    .unwrap();
    // 跑 init_schema（含迁移）→ 应探测到缺列并 ALTER 加上
    init_schema(&c).unwrap();
    // 列已存在
    let cols: Vec<String> = c
        .prepare("PRAGMA table_info(goal_contracts)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(cols.iter().any(|n| n == "assignments_json"));
    // 幂等：再跑一次不报错
    init_schema(&c).unwrap();
}

#[test]
fn migration_adds_goal_title_to_old_goal_contracts() {
    // B1（codex/opus 双审 P1）：旧库（有 assignments_json 但无 goal_title 列）→ init_schema 迁移加列·nullable·幂等。
    let c = Connection::open_in_memory().unwrap();
    // 手建「旧 schema」goal_contracts（无 goal_title 列）
    c.execute(
        "CREATE TABLE goal_contracts (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, run_id TEXT NOT NULL UNIQUE,
                goal TEXT NOT NULL, lead_participant_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'draft',
                assignments_json TEXT NOT NULL DEFAULT '[]', created_at INTEGER NOT NULL )",
        [],
    )
    .unwrap();
    // 插一条旧 row（迁移前就存在）
    c.execute(
            "INSERT INTO goal_contracts (id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at)
             VALUES ('gc-old', 's1', 'r1', 'old goal', 'lead', 'frozen', '[]', 1)",
            [],
        )
        .unwrap();
    // 跑 init_schema（含迁移）→ 探测缺列并真 ALTER 加上
    init_schema(&c).unwrap();
    let cols: Vec<String> = c
        .prepare("PRAGMA table_info(goal_contracts)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        cols.iter().any(|n| n == "goal_title"),
        "迁移后应有 goal_title 列"
    );
    // 旧 row 可读·goal_title 默认 None
    assert_eq!(goal_title_for_run(&c, "s1", "r1").unwrap(), None);
    // setter 后可读到值
    set_goal_title_for_run(&c, "s1", "r1", Some("迁移后短标题")).unwrap();
    assert_eq!(
        goal_title_for_run(&c, "s1", "r1").unwrap(),
        Some("迁移后短标题".to_string())
    );
    // 幂等：再跑一次 init_schema 不报错·值仍在
    init_schema(&c).unwrap();
    assert_eq!(
        goal_title_for_run(&c, "s1", "r1").unwrap(),
        Some("迁移后短标题".to_string())
    );
}

#[test]
fn goal_title_roundtrips_via_setter() {
    let c = mem();
    insert_goal_contract(
        &c,
        &GoalContract {
            id: "gc-gt1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            goal: "do something".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: 1,
        },
    )
    .unwrap();
    // just inserted, goal_title should be NULL -> None
    assert_eq!(goal_title_for_run(&c, "s1", "r1").unwrap(), None);
    // set it
    set_goal_title_for_run(&c, "s1", "r1", Some("create 10 cold joke files")).unwrap();
    assert_eq!(
        goal_title_for_run(&c, "s1", "r1").unwrap(),
        Some("create 10 cold joke files".to_string())
    );
}

#[test]
fn goal_title_for_run_absent_row_is_none() {
    let c = mem();
    assert_eq!(goal_title_for_run(&c, "s1", "no-such-run").unwrap(), None);
}

#[test]
fn goal_title_for_run_null_is_none() {
    let c = mem();
    insert_goal_contract(
        &c,
        &GoalContract {
            id: "gc-gt2".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            goal: "do something".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: 1,
        },
    )
    .unwrap();
    // never set goal_title, should be None
    assert_eq!(goal_title_for_run(&c, "s1", "r1").unwrap(), None);
}

#[test]
fn update_acceptance_waiver_sets_waived_and_reason() {
    let c = crate::test_support::mem_db();
    insert_acceptance(
        &c,
        &AcceptanceCriterion {
            id: "c1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            task_id: "t1".into(),
            contract_id: None,
            scope: "task".into(),
            claim: "e2e".into(),
            verifier: None,
            evidence: None,
            status: "pending".into(),
            waiver: None,
            created_at: 1,
        },
    )
    .unwrap();
    update_acceptance_waiver(&c, "s1", "r1", "c1", "本期不要了").unwrap();
    let rows = list_acceptance_by_run(&c, "s1", "r1").unwrap();
    assert_eq!(rows[0].status, "waived");
    assert_eq!(rows[0].waiver.as_deref(), Some("本期不要了"));
}

#[test]
fn block_team_run_serde_round_trip() {
    let block = Block::TeamRun {
        run_id: "r1".into(),
        goal: Some(TeamGoal {
            goal: "实现 stage 2".into(),
            status: "frozen".into(),
            criteria: vec![crate::agent_event::GoalCriterion {
                id: "ac1".into(),
                claim: "测试通过".into(),
                verifier: None,
                evidence: None,
                status: "pending".into(),
                scope: "task".into(),
            }],
        }),
        lead: Some("Claude".into()),
        members: vec![MemberSnapshot {
            participant_id: "worker-1".into(),
            assignment_id: "a1".into(),
            task_id: "t1".into(),
            name: "worker-1".into(),
            started_at: None,
            status: "done".into(),
            sub: "做 X".into(),
            steps_total: 2,
            steps_done: 2,
            cost_usd: Some(0.12),
            input_tokens: 1000,
            output_tokens: 200,
            failed: false,
            // 递归：队员细节块（drill-in 用）
            blocks: vec![Block::Text {
                text: "完成 X".into(),
            }],
            result: None,
        }],
    };
    // tag = "team_run"（与前端 Block 联合镜像）
    let v = serde_json::to_value(&block).unwrap();
    assert_eq!(v["type"], "team_run");
    assert_eq!(v["members"][0]["blocks"][0]["type"], "text");
    // round-trip：序列化→反序列化等价（证明 append_message 收得住、get_messages 丢不了）
    let back: Block = serde_json::from_value(v).unwrap();
    match &back {
        Block::TeamRun { lead, .. } => assert_eq!(lead.as_deref(), Some("Claude")),
        _ => panic!("expected team_run block"),
    }
    assert_eq!(back, block);
}

#[test]
fn block_lead_summary_round_trips() {
    let block = Block::LeadSummary {
        run_id: "r1".into(),
        summary_source: "single_passthrough".into(),
        status: SummaryStatus {
            kind: "partial".into(),
            succeeded_count: 1,
            total: 2,
        },
        sections: vec![
            SummarySection {
                heading: "结论".into(),
                body_richtext: Some("**bind 失败 = sandbox 权限**。".into()),
                findings: vec![],
                attribution: vec!["a1".into()],
                trace_ref: TraceRef {
                    run_id: "r1".into(),
                    assignment_ids: vec!["a1".into()],
                },
                source_spans: vec![],
            },
            // 第二个 section 走 skip_serializing_if 的「跳过」反面（None body + 非空 findings/source_spans）·
            // 连带 SourceSpan/SourceLoc/text_span 元组的全字段 round-trip（防 reload 丢字段·M1b T3 老坑钉）。
            SummarySection {
                heading: "证据".into(),
                body_richtext: None,
                findings: vec![Finding {
                    status: "done".into(),
                    text: "命令留痕".into(),
                    assignment_id: "a1".into(),
                }],
                attribution: vec!["a1".into(), "a2".into()],
                trace_ref: TraceRef {
                    run_id: "r1".into(),
                    assignment_ids: vec!["a1".into(), "a2".into()],
                },
                source_spans: vec![SourceSpan {
                    ref_no: 1,
                    text_span: (3, 17),
                    sources: vec![SourceLoc {
                        run_id: "r1".into(),
                        assignment_id: "a2".into(),
                        block_index: 2,
                    }],
                    conflict: true,
                }],
            },
        ],
        findings: vec![Finding {
            status: "miss".into(),
            text: "typecheck 红".into(),
            assignment_id: "a2".into(),
        }],
        artifact_refs: vec![ArtifactRef {
            kind: "code_diff".into(),
            label: "查看本轮改动".into(),
        }],
    };
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains("\"type\":\"lead_summary\""));
    let back: Block = serde_json::from_str(&json).unwrap();
    assert_eq!(block, back);
}

#[test]
fn acceptance_and_contract_check_reject_bad_values() {
    let c = Connection::open_in_memory().unwrap();
    init_schema(&c).unwrap();
    // 非法 criterion.status
    let bad_status = AcceptanceCriterion {
        id: "ac2".into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        task_id: "t1".into(),
        contract_id: None,
        scope: "task".into(),
        claim: "x".into(),
        verifier: None,
        evidence: None,
        status: "bogus".into(),
        waiver: None,
        created_at: 1,
    };
    assert!(insert_acceptance(&c, &bad_status).is_err());
    // 非法 criterion.scope
    let bad_scope = AcceptanceCriterion {
        id: "ac3".into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        task_id: "t1".into(),
        contract_id: None,
        scope: "galaxy".into(),
        claim: "x".into(),
        verifier: None,
        evidence: None,
        status: "pending".into(),
        waiver: None,
        created_at: 1,
    };
    assert!(insert_acceptance(&c, &bad_scope).is_err());
    // 非法 contract.status（只许 draft/frozen）
    let bad_contract = GoalContract {
        id: "gc2".into(),
        session_id: "s1".into(),
        run_id: "r2".into(),
        goal: "g".into(),
        lead_participant_id: "l".into(),
        status: "running".into(),
        assignments_json: "[]".into(),
        created_at: 1,
    };
    assert!(insert_goal_contract(&c, &bad_contract).is_err());
}
