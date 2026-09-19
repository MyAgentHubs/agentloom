#![cfg(test)]

use super::*;

#[test]
fn run_card_block_round_trips_through_json() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![
        Block::Text {
            text: "改完了".into(),
        },
        Block::RunCard {
            run_id: "run-1".into(),
            commit_sha: Some("deadbeef".into()),
            files_changed: 3,
            insertions: 10,
            deletions: 2,
            interrupted: false,
        },
    ];
    append_message(&c, "s1", "assistant", &blocks, Some("claude"), None, None).unwrap();
    assert_eq!(get_messages(&c, "s1").unwrap()[0].content, blocks);
}

#[test]
fn decision_card_block_round_trips_through_json() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![Block::DecisionCard {
        decision_id: "dc-1".into(),
        kind: "dispatch_confirm".into(),
        question: "开干还是只读探？".into(),
        options: vec!["开干".into(), "只读探".into(), "我来调整".into()],
        recommended: Some("开干".into()),
        rationale: Some("低风险·单文件".into()),
        payload: serde_json::json!({"run_id": "r-pre-1", "files": 1}),
        source_run_id: "r-pre-1".into(),
        status: "pending".into(),
        chosen_option: None,
        created_at: 1_700_000_000,
    }];
    append_message(
        &c,
        "s1",
        "assistant",
        &blocks,
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    let got = get_messages(&c, "s1").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].content, blocks); // 含 status/payload/created_at 全字段往返不变形
}

#[test]
fn coding_task_block_round_trips_through_json() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![Block::CodingTask {
        run_id: "r-1".into(),
        assignment_id: "a-1".into(),
        worker_name: "codex".into(),
        phase: "verify_failed".into(),
        step_done: Some(3),
        step_total: Some(5),
        artifact_id: Some("art-1".into()),
        verify_cmd: Some("cargo test".into()),
        detail: Some("L1 没过".into()),
        lead_rationale: None,
    }];
    append_message(
        &c,
        "s1",
        "assistant",
        &blocks,
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();
    let got = get_messages(&c, "s1").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].content, blocks);
}

#[test]
fn context_truncated_block_uses_its_own_serde_tag() {
    // T7a：截断块与压实块必须是两个独立 tag——前端按 tag 分流两种文案/视觉。
    assert_eq!(
        serde_json::to_string(&Block::ContextTruncated {}).unwrap(),
        r#"{"type":"context_truncated"}"#
    );
    assert_eq!(
        serde_json::from_str::<Block>(r#"{"type":"context_truncated"}"#).unwrap(),
        Block::ContextTruncated {}
    );
    assert_ne!(Block::ContextTruncated {}, Block::ContextCompacted {});
}

#[test]
fn context_truncated_block_round_trips_through_message_storage() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![
        Block::Text {
            text: "截断前".into(),
        },
        Block::ContextTruncated {},
    ];
    append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();

    assert_eq!(get_messages(&c, "s1").unwrap()[0].content, blocks);
}

#[test]
fn coding_task_block_with_omitted_optionals_round_trips() {
    // 前端可选字段缺省（undefined）时·后端 serde(default) 应能反序列化·不清消息。
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let json = r#"[{"type":"coding_task","run_id":"r-2","assignment_id":"a-2","worker_name":"codex","phase":"finalizing"}]"#;
    let blocks: Vec<Block> = serde_json::from_str(json).unwrap();
    append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();
    let got = get_messages(&c, "s1").unwrap();
    assert_eq!(got.len(), 1);
    match &got[0].content[0] {
        Block::CodingTask {
            run_id,
            step_done,
            artifact_id,
            ..
        } => {
            assert_eq!(run_id, "r-2");
            assert_eq!(*step_done, None);
            assert_eq!(*artifact_id, None);
        }
        other => panic!("期望 CodingTask·得到 {other:?}"),
    }
}

#[test]
fn coding_task_block_with_explicit_null_optionals_round_trips() {
    // 真实线格式：前端 blockFromCodingState（App.tsx:808）会发 artifact_id:null / detail:null（key 存在但值为 JSON null）·
    // 守「reload 不清消息」契约——serde 把 JSON null 反序列化进 Option<String> 得 None·不崩。
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let json = r#"[{"type":"coding_task","run_id":"r-3","assignment_id":"a-3","worker_name":"codex","phase":"applied","artifact_id":null,"verify_cmd":"cargo test","detail":null}]"#;
    let blocks: Vec<Block> = serde_json::from_str(json).unwrap();
    append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();
    let got = get_messages(&c, "s1").unwrap();
    assert_eq!(got.len(), 1); // 没被 unwrap_or_default 清空
    match &got[0].content[0] {
        Block::CodingTask {
            run_id,
            artifact_id,
            verify_cmd,
            detail,
            ..
        } => {
            assert_eq!(run_id, "r-3");
            assert_eq!(*artifact_id, None); // 显式 null → None
            assert_eq!(verify_cmd.as_deref(), Some("cargo test"));
            assert_eq!(*detail, None);
        }
        other => panic!("期望 CodingTask·得到 {other:?}"),
    }
}

#[test]
fn choose_decision_card_compare_and_set() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![Block::DecisionCard {
        decision_id: "dc-1".into(),
        kind: "ask".into(),
        question: "?".into(),
        options: vec!["A".into(), "B".into()],
        recommended: None,
        rationale: None,
        payload: serde_json::Value::Null,
        source_run_id: "r-1".into(),
        status: "pending".into(),
        chosen_option: None,
        created_at: 1,
    }];
    append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();

    // 第一次抢锁 pending→submitting 成功
    assert!(update_decision_card_status(&c, "s1", "dc-1", "pending", "submitting", None).unwrap());
    // 第二次同 expect=pending（双击/race）→ 已是 submitting → 失败·不改
    assert!(!update_decision_card_status(&c, "s1", "dc-1", "pending", "submitting", None).unwrap());
    // submitting→chosen + 写 chosen_option
    assert!(
        update_decision_card_status(&c, "s1", "dc-1", "submitting", "chosen", Some("A")).unwrap()
    );

    let got = get_messages(&c, "s1").unwrap();
    match &got[0].content[0] {
        Block::DecisionCard {
            status,
            chosen_option,
            ..
        } => {
            assert_eq!(status, "chosen");
            assert_eq!(chosen_option.as_deref(), Some("A"));
        }
        other => panic!("期望 DecisionCard·得到 {other:?}"),
    }
    // 不存在的 decision_id → false（不 panic）
    assert!(!update_decision_card_status(&c, "s1", "nope", "pending", "submitting", None).unwrap());
}

#[test]
fn update_decision_card_status_publishes_only_for_cas_winner() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![Block::DecisionCard {
        decision_id: "dc-publish-1".into(),
        kind: "ask".into(),
        question: "?".into(),
        options: vec!["A".into(), "B".into()],
        recommended: None,
        rationale: None,
        payload: serde_json::Value::Null,
        source_run_id: "r-1".into(),
        status: "pending".into(),
        chosen_option: None,
        created_at: 1,
    }];
    append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();

    crate::remote_gateway::test_take_publish_log();
    assert!(update_decision_card_status(
        &c,
        "s1",
        "dc-publish-1",
        "pending",
        "resolved",
        Some("A"),
    )
    .unwrap());
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["card.resolved"]
    );

    assert!(!update_decision_card_status(
        &c,
        "s1",
        "dc-publish-1",
        "pending",
        "resolved",
        Some("B"),
    )
    .unwrap());
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
}

#[test]
fn update_decision_card_status_bumps_revision_and_noop_cas_loss_does_not() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![Block::DecisionCard {
        decision_id: "dc-rev-1".into(),
        kind: "ask".into(),
        question: "?".into(),
        options: vec!["A".into(), "B".into()],
        recommended: None,
        rationale: None,
        payload: serde_json::Value::Null,
        source_run_id: "r-1".into(),
        status: "pending".into(),
        chosen_option: None,
        created_at: 1,
    }];
    append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();
    assert_eq!(get_messages(&c, "s1").unwrap()[0].revision, 1);

    // 第一次成功 CAS：pending → submitting，原子 +1。
    assert!(
        update_decision_card_status(&c, "s1", "dc-rev-1", "pending", "submitting", None).unwrap()
    );
    assert_eq!(get_messages(&c, "s1").unwrap()[0].revision, 2);

    // CAS 落败（expect 已不是 pending）：不改内容 → revision 不动。
    assert!(
        !update_decision_card_status(&c, "s1", "dc-rev-1", "pending", "submitting", None).unwrap()
    );
    assert_eq!(
        get_messages(&c, "s1").unwrap()[0].revision,
        2,
        "CAS 落败不应递增 revision"
    );

    // 第二次成功 CAS：submitting → chosen，连续两次成功更新 revision 应为 3。
    assert!(
        update_decision_card_status(&c, "s1", "dc-rev-1", "submitting", "chosen", Some("A"),)
            .unwrap()
    );
    assert_eq!(
        get_messages(&c, "s1").unwrap()[0].revision,
        3,
        "连续两次原子更新后 revision 应为 3"
    );
}

#[test]
fn find_decision_card_returns_question_and_status() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![Block::DecisionCard {
        decision_id: "dc-1".into(),
        kind: "ask".into(),
        question: "改哪个方案？".into(),
        options: vec!["A".into(), "B".into()],
        recommended: None,
        rationale: None,
        payload: serde_json::Value::Null,
        source_run_id: "r-1".into(),
        status: "pending".into(),
        chosen_option: None,
        created_at: 1,
    }];
    append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();

    let found = find_decision_card(&c, "s1", "dc-1").unwrap();
    assert_eq!(
        found,
        Some(("改哪个方案？".to_string(), "pending".to_string()))
    );

    // 卡状态翻了之后再查·要看到最新状态（迟到答案落地判定依赖这点）。
    assert!(update_decision_card_status(&c, "s1", "dc-1", "pending", "chosen", Some("A")).unwrap());
    let found_after = find_decision_card(&c, "s1", "dc-1").unwrap();
    assert_eq!(
        found_after,
        Some(("改哪个方案？".to_string(), "chosen".to_string()))
    );

    // 不存在的 decision_id → None
    assert_eq!(find_decision_card(&c, "s1", "nope").unwrap(), None);
}

#[test]
fn choose_decision_card_does_not_touch_siblings_or_other_messages() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // 消息①：text 兄弟块 + 目标 decision_card 同条
    let m1 = vec![
        Block::Text {
            text: "保留我".into(),
        },
        Block::DecisionCard {
            decision_id: "dc-1".into(),
            kind: "ask".into(),
            question: "?".into(),
            options: vec!["A".into()],
            recommended: None,
            rationale: None,
            payload: serde_json::Value::Null,
            source_run_id: "r-1".into(),
            status: "pending".into(),
            chosen_option: None,
            created_at: 1,
        },
    ];
    append_message(&c, "s1", "assistant", &m1, None, None, None).unwrap();
    // 消息②：另一条 text·不该被动
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text {
            text: "别动我".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();

    assert!(update_decision_card_status(&c, "s1", "dc-1", "pending", "chosen", Some("A")).unwrap());
    let got = get_messages(&c, "s1").unwrap();
    // 兄弟 text 块原样保留
    assert_eq!(
        got[0].content[0],
        Block::Text {
            text: "保留我".into()
        }
    );
    match &got[0].content[1] {
        Block::DecisionCard { status, .. } => assert_eq!(status, "chosen"),
        other => panic!("期望 DecisionCard·得到 {other:?}"),
    }
    // 另一条消息不受影响
    assert_eq!(
        got[1].content[0],
        Block::Text {
            text: "别动我".into()
        }
    );
}

#[test]
fn blocks_to_text_summarizes_run_card() {
    let blocks = vec![
        Block::Text {
            text: "答案".into(),
        },
        Block::RunCard {
            run_id: "run-1".into(),
            commit_sha: None,
            files_changed: 2,
            insertions: 5,
            deletions: 1,
            interrupted: false,
        },
    ];
    // run_card 给个简短文本（不 panic、不进 prompt 主体噪声）
    assert_eq!(blocks_to_text(&blocks), "答案\n[This run changed 2 files]");
}

#[test]
fn blocks_to_text_run_card_summary_uses_english_and_keeps_file_count() {
    let blocks = vec![Block::RunCard {
        run_id: "run-1".into(),
        commit_sha: None,
        files_changed: 7,
        insertions: 5,
        deletions: 1,
        interrupted: false,
    }];

    let summary = blocks_to_text(&blocks);
    assert!(!summary
        .chars()
        .any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)));
    assert!(summary.contains('7'));
}
