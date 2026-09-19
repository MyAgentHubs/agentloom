#![cfg(test)]

use super::*;

/// 造一条 pending 的 DecisionCard 消息（迟到路径测试的前置：commit_late_answer 要能
/// 从 DB 找回 question + CAS 卡状态）。
fn seed_pending_decision_card(
    conn: &rusqlite::Connection,
    session_id: &str,
    decision_id: &str,
    question: &str,
) {
    db::create_session(conn, session_id, "x", "local-default", "local").unwrap();
    db::append_message(
        conn,
        session_id,
        "assistant",
        &[db::Block::DecisionCard {
            decision_id: decision_id.to_string(),
            kind: "ask".into(),
            question: question.to_string(),
            options: vec!["继续".into(), "算了".into()],
            recommended: None,
            rationale: None,
            payload: serde_json::Value::Null,
            source_run_id: format!("{}-run", decision_id),
            status: "pending".into(),
            chosen_option: None,
            created_at: 1,
        }],
        None,
        None,
        None,
    )
    .unwrap();
}

#[test]
fn wait_for_answer_returns_choice_when_answered() {
    let q = LeadQuestions::default();
    let running = Running::default();
    running
        .0
        .lock()
        .unwrap()
        .insert("s1".into(), RunSlot::Mutating { op: "lead" });
    let (qc, rc) = (q.clone(), running.clone());
    let h = std::thread::spawn(move || wait_for_answer(&qc, &rc, "s1", "d1", None));
    loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut m = q.0.lock().unwrap();
        if let Some(LeadQuestionSlot::Live(tx)) = m.remove("d1") {
            let _ = tx.send(LeadAnswer::Choice("继续".into()));
            break;
        }
        drop(m);
    }
    match h.join().unwrap().unwrap() {
        WaitOutcome::Answered(opt) => assert_eq!(opt, "继续"),
        WaitOutcome::TimedOut => panic!("unbounded wait must never time out"),
    }
}

#[test]
fn wait_for_answer_cancels_when_session_not_running() {
    let q = LeadQuestions::default();
    let running = Running::default();
    let r = wait_for_answer(&q, &running, "s-absent", "d2", None);
    assert!(
        r.is_err(),
        "session not in Running should cancel and return Err"
    );
    assert!(
        q.0.lock().unwrap().get("d2").is_none(),
        "must remove decision_id on cancel (no leak)"
    );
}

#[test]
fn wait_for_answer_bounded_times_out_and_downgrades_slot_to_timed_out() {
    // 决策打扰收敛刀 T1：有界等待窗口耗尽·没人来答 → 返回 TimedOut，handler 体面退出；
    // 槽位不是被 remove 掉，而是降级成 TimedOut——留给随后姗姗来迟的答案认出「这是迟到答案」。
    let q = LeadQuestions::default();
    let running = Running::default();
    running
        .0
        .lock()
        .unwrap()
        .insert("s1".into(), RunSlot::Mutating { op: "lead" });
    let r = wait_for_answer(
        &q,
        &running,
        "s1",
        "d-timeout",
        Some(std::time::Duration::from_millis(20)),
    );
    match r.unwrap() {
        WaitOutcome::TimedOut => {}
        WaitOutcome::Answered(_) => panic!("no one answered; must time out"),
    }
    assert!(
        matches!(
            q.0.lock().unwrap().get("d-timeout"),
            Some(LeadQuestionSlot::TimedOut)
        ),
        "slot must be downgraded to TimedOut, not removed"
    );
}

#[test]
fn wait_for_answer_bounded_prefers_answer_that_arrives_before_deadline() {
    // 有界等待窗口内答了 → 走原路 Answered，不受「有界」影响（240s 只是上限，不是恒等超时）。
    let q = LeadQuestions::default();
    let running = Running::default();
    running
        .0
        .lock()
        .unwrap()
        .insert("s1".into(), RunSlot::Mutating { op: "lead" });
    let (qc, rc) = (q.clone(), running.clone());
    let h = std::thread::spawn(move || {
        wait_for_answer(
            &qc,
            &rc,
            "s1",
            "d-fast",
            Some(std::time::Duration::from_secs(30)),
        )
    });
    loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut m = q.0.lock().unwrap();
        if let Some(LeadQuestionSlot::Live(tx)) = m.remove("d-fast") {
            let _ = tx.send(LeadAnswer::Choice("继续".into()));
            break;
        }
        drop(m);
    }
    match h.join().unwrap().unwrap() {
        WaitOutcome::Answered(opt) => assert_eq!(opt, "继续"),
        WaitOutcome::TimedOut => panic!("answer arrived well before the 30s deadline"),
    }
}

#[test]
fn answer_question_inner_delivered_path_does_not_touch_db() {
    // Live 槽位：答案直接 send 给还活着的 handler，answer_question_inner 本身不落库
    // 也不再由本次调用报 resolved；翻卡广播转交 prompt_user，在 CAS 落库成功后触发。
    let q = LeadQuestions::default();
    let (tx, rx) = std::sync::mpsc::channel::<LeadAnswer>();
    q.0.lock()
        .unwrap()
        .insert("d3".into(), LeadQuestionSlot::Live(tx));
    let db = test_db();
    // Delivered 路径没有落库消息，也无法同步确认 handler 随后的 CAS 是否成功。
    let result = answer_question_inner(&q, &db, "s-unused", "d3", "继续".into(), Locale::Zh)
        .expect("delivered path should succeed");
    assert_eq!(result.appended, None);
    assert!(
        !result.resolved,
        "Delivered 不落库，不能在 answer_question_inner 提前报告 resolved"
    );
    match rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap() {
        LeadAnswer::Choice(s) => assert_eq!(s, "继续"),
        LeadAnswer::Cancel => panic!("expected Choice, got Cancel"),
    }
    assert!(
        q.0.lock().unwrap().get("d3").is_none(),
        "answer 后必须移除 decision_id·不泄漏"
    );
}

#[test]
fn answer_question_inner_delivered_send_failure_does_not_report_resolved() {
    let q = LeadQuestions::default();
    let (tx, rx) = std::sync::mpsc::channel::<LeadAnswer>();
    drop(rx);
    q.0.lock()
        .unwrap()
        .insert("d-send-failed".into(), LeadQuestionSlot::Live(tx));

    let result = answer_question_inner(
        &q,
        &test_db(),
        "s-unused",
        "d-send-failed",
        "继续".into(),
        Locale::Zh,
    )
    .expect("send 失败仍应维持 Delivered 的 best-effort 返回，不 panic/不报 Err");

    assert_eq!(result.appended, None);
    assert!(
        !result.resolved,
        "tx.send 失败绝不能被后续代码误报成 resolved"
    );
    assert!(
        q.0.lock().unwrap().get("d-send-failed").is_none(),
        "send 失败后槽位也已消费，不应泄漏"
    );
}

#[test]
fn answer_question_inner_late_path_commits_chosen_status_and_user_message() {
    // 迟到路径（TimedOut 槽位）：落卡 chosen + append 一条真实 user 消息喂给 lead 下一轮。
    let db = test_db();
    {
        let conn = db.0.lock().unwrap();
        seed_pending_decision_card(&conn, "s-late", "d-late", "要不要重试？");
    }
    let q = LeadQuestions::default();
    q.0.lock()
        .unwrap()
        .insert("d-late".into(), LeadQuestionSlot::TimedOut);

    let result = answer_question_inner(&q, &db, "s-late", "d-late", "继续".into(), Locale::Zh)
        .expect("late path should succeed");
    assert!(result.resolved);

    let conn = db.0.lock().unwrap();
    let msgs = db::get_messages(&conn, "s-late").unwrap();
    // 消息①=DecisionCard（已翻 chosen），消息②=迟到答案的真实 user 消息。
    assert_eq!(msgs.len(), 2, "应新增一条落库消息: {msgs:?}");
    // T3：返回值必须是刚落库那条完整消息（供外层 emit "lead-message-appended"），
    // 不是 None（emit 就无从谈起）。
    assert_eq!(
        result.appended.as_ref().map(|m| m.id),
        Some(msgs[1].id),
        "应返回刚插入的迟到回答消息本身: {result:?}"
    );
    match &msgs[0].content[0] {
        db::Block::DecisionCard {
            status,
            chosen_option,
            ..
        } => {
            assert_eq!(status, "chosen");
            assert_eq!(chosen_option.as_deref(), Some("继续"));
        }
        other => panic!("期望 DecisionCard·得到 {other:?}"),
    }
    assert_eq!(msgs[1].role, "user");
    match &msgs[1].content[0] {
        db::Block::Text { text } => {
            assert!(text.contains("要不要重试？"), "应带问题原文: {text}");
            assert!(text.contains("继续"), "应带答案: {text}");
        }
        other => panic!("期望 Text·得到 {other:?}"),
    }
    // 槽位用完即走，不留悬空条目。
    assert!(q.0.lock().unwrap().get("d-late").is_none());
}

#[test]
fn answer_question_inner_late_path_registers_resume_pending_answer_id_for_team_session_only() {
    // S-2：Late 路径落库成功后经 register_pending_answer_id_if_team 登记——Team 会话
    // （session_agent_configs 有 lead_agent_id）必须登记进 RESUME_STATE，供
    // try_resume_pending_with_gate 的快照纳入「含答案」触发原因；Solo 会话（无该行）绝不能
    // 登记，否则 RESUME_STATE 里这个 session 的集合只会随每次补答只增不减——Solo 永远不会
    // 触发续跑、也就永远不会调用 ack_pending_answers 摘除，是进程内内存泄漏。
    let db = test_db();
    {
        let conn = db.0.lock().unwrap();
        seed_pending_decision_card(
            &conn,
            "s-resume-pending-late-team",
            "d-resume-pending-late-team",
            "要不要重试？",
        );
        db::seed_builtin_agents(&conn).unwrap();
        db::set_session_agent_config(
            &conn,
            "s-resume-pending-late-team",
            Some("claude".to_string()),
            vec![],
        )
        .unwrap();

        seed_pending_decision_card(
            &conn,
            "s-resume-pending-late-solo",
            "d-resume-pending-late-solo",
            "要不要重试？",
        );
        // Solo：故意不写 session_agent_configs 行。
    }
    let q = LeadQuestions::default();
    q.0.lock().unwrap().insert(
        "d-resume-pending-late-team".into(),
        LeadQuestionSlot::TimedOut,
    );
    q.0.lock().unwrap().insert(
        "d-resume-pending-late-solo".into(),
        LeadQuestionSlot::TimedOut,
    );

    let team_result = answer_question_inner(
        &q,
        &db,
        "s-resume-pending-late-team",
        "d-resume-pending-late-team",
        "继续".into(),
        Locale::Zh,
    )
    .expect("team late path should succeed");
    let solo_result = answer_question_inner(
        &q,
        &db,
        "s-resume-pending-late-solo",
        "d-resume-pending-late-solo",
        "继续".into(),
        Locale::Zh,
    )
    .expect("solo late path should succeed");

    let team_message_id = team_result
        .appended
        .expect("team late answer should append a message")
        .id;
    assert!(
        solo_result.appended.is_some(),
        "solo 落库仍应成功——只是不该登记进 RESUME_STATE"
    );

    assert_eq!(
        snapshot_pending_answer_ids("s-resume-pending-late-team"),
        vec![team_message_id],
        "team 会话的迟到答案必须被登记，供续跑快照纳入"
    );
    assert!(
        snapshot_pending_answer_ids("s-resume-pending-late-solo").is_empty(),
        "solo 会话不该登记进 RESUME_STATE，否则永不会被 ack、进程内只增不减"
    );
}

#[test]
fn answer_question_inner_missing_slot_pending_card_registers_resume_pending_answer_id_for_team_session(
) {
    // 覆盖 Missing 路由（内存 map 里没有这个槽位、DB 卡仍 pending，等同迟到答案）——落库
    // 与登记复用的是同一段代码路径（同一个 register_pending_answer_id_if_team 助手），Solo
    // 侧「不登记」的判断已经在上面 late 路径的测试里覆盖过，这里只需确认 Team 会话在
    // Missing 路由下也确实登记成功。
    let db = test_db();
    {
        let conn = db.0.lock().unwrap();
        seed_pending_decision_card(
            &conn,
            "s-resume-pending-missing-team",
            "d-resume-pending-missing-team",
            "还继续吗？",
        );
        db::seed_builtin_agents(&conn).unwrap();
        db::set_session_agent_config(
            &conn,
            "s-resume-pending-missing-team",
            Some("claude".to_string()),
            vec![],
        )
        .unwrap();
    }
    let q = LeadQuestions::default(); // 槽位压根没插入 => Missing 路由

    let result = answer_question_inner(
        &q,
        &db,
        "s-resume-pending-missing-team",
        "d-resume-pending-missing-team",
        "继续".into(),
        Locale::Zh,
    )
    .expect("missing route with pending card should succeed");
    let message_id = result
        .appended
        .expect("missing+pending path should append a late answer message")
        .id;

    assert_eq!(
        snapshot_pending_answer_ids("s-resume-pending-missing-team"),
        vec![message_id],
        "Missing 路由（卡仍 pending）也要走同一个 register_pending_answer_id_if_team 助手登记"
    );
}

#[test]
fn commit_late_answer_uses_zh_shell_copy() {
    let db = test_db();
    let conn = db.0.lock().unwrap();
    seed_pending_decision_card(&conn, "s-late-zh", "d-late-zh", "Retry now?");

    let appended = commit_late_answer(&conn, "s-late-zh", "d-late-zh", "Continue", Locale::Zh)
        .expect("late answer should be committed")
        .expect("late answer should append a user message");

    match &appended.content[0] {
        db::Block::Text { text } => {
            assert_eq!(text, "[用户对『Retry now?』的回答] Continue");
        }
        other => panic!("expected Text, got {other:?}"),
    }
    // P0-c 返工（测试硬度钉②-c）：迟到答案落库行 dedup_key 字面断言——硬编码
    // `late_answer:{decision_id}`，不复算 `display_reduce::late_answer_key`，防「键工厂
    // 改常量」类变异。
    let dedup_key: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE id = ?1",
            [appended.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        dedup_key, "late_answer:d-late-zh",
        "迟到答案落库行 dedup_key 必须字面等于 late_answer:{{decision_id}}"
    );
}

#[test]
fn commit_late_answer_uses_en_shell_copy_without_cjk() {
    let db = test_db();
    let conn = db.0.lock().unwrap();
    seed_pending_decision_card(&conn, "s-late-en", "d-late-en", "Retry now?");

    let appended = commit_late_answer(&conn, "s-late-en", "d-late-en", "Continue", Locale::En)
        .expect("late answer should be committed")
        .expect("late answer should append a user message");

    match &appended.content[0] {
        db::Block::Text { text } => {
            assert_eq!(text, "[User's answer to ‘Retry now?’] Continue");
            assert!(
                !text
                    .chars()
                    .any(|ch| ('\u{4E00}'..='\u{9FFF}').contains(&ch)),
                "English shell copy must not contain CJK unified ideographs: {text}"
            );
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn answer_question_inner_double_answer_is_idempotent_no_duplicate_message() {
    // 双击幂等：迟到答案落地一次之后，第二次答同一 decision_id 必须 NO_PENDING_QUESTION，
    // 且 DB 里不出现第二条落库消息（防双发语义不放松·迟到路径同样受保护）。
    let db = test_db();
    {
        let conn = db.0.lock().unwrap();
        seed_pending_decision_card(&conn, "s-dbl", "d-dbl", "还要继续吗？");
    }
    let q = LeadQuestions::default();
    q.0.lock()
        .unwrap()
        .insert("d-dbl".into(), LeadQuestionSlot::TimedOut);
    let first = answer_question_inner(&q, &db, "s-dbl", "d-dbl", "继续".into(), Locale::Zh)
        .expect("first answer should succeed");
    assert!(first.appended.is_some());
    assert!(first.resolved);

    // 第二次：map 已经没有槽位（第一次已 remove）→ Missing 分支查 DB → 卡已 chosen → 报错。
    let err =
        answer_question_inner(&q, &db, "s-dbl", "d-dbl", "算了".into(), Locale::Zh).unwrap_err();
    assert_eq!(err, "NO_PENDING_QUESTION");

    let conn = db.0.lock().unwrap();
    let msgs = db::get_messages(&conn, "s-dbl").unwrap();
    assert_eq!(
        msgs.len(),
        2,
        "第二次双击不应新增消息（1 张卡 + 1 条迟到回答）: {msgs:?}"
    );
}

#[test]
fn answer_question_inner_missing_slot_falls_back_to_pending_db_card() {
    // 模拟「进程重启·内存 map 全空」但 DB 里还留着一张 pending 卡：也要走通迟到路径。
    let db = test_db();
    {
        let conn = db.0.lock().unwrap();
        seed_pending_decision_card(&conn, "s-restart", "d-restart", "重启后还问吗？");
    }
    let q = LeadQuestions::default(); // 空 map，模拟重启

    let result =
        answer_question_inner(&q, &db, "s-restart", "d-restart", "继续".into(), Locale::Zh)
            .expect("missing-slot fallback should succeed");
    assert!(result.resolved);

    let conn = db.0.lock().unwrap();
    let msgs = db::get_messages(&conn, "s-restart").unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].role, "user");
    assert_eq!(result.appended.map(|m| m.id), Some(msgs[1].id));
}

#[test]
fn answer_question_inner_late_path_cas_loss_is_not_resolved() {
    // 模拟另一条路径已抢先把卡翻成 chosen，但本次调用仍拿到 TimedOut 槽位：Late 分支
    // 的 pending→chosen CAS 必须失败，既不追加消息，也不能把本次答案广播成赢家。
    let db = test_db();
    {
        let conn = db.0.lock().unwrap();
        seed_pending_decision_card(&conn, "s-cas-loss", "d-cas-loss", "已经决定了吗？");
        let changed = db::update_decision_card_status(
            &conn,
            "s-cas-loss",
            "d-cas-loss",
            "pending",
            "chosen",
            Some("先到答案"),
        )
        .unwrap();
        assert!(changed, "test setup should choose the pending card");
    }
    let q = LeadQuestions::default();
    q.0.lock()
        .unwrap()
        .insert("d-cas-loss".into(), LeadQuestionSlot::TimedOut);

    let result = answer_question_inner(
        &q,
        &db,
        "s-cas-loss",
        "d-cas-loss",
        "后到答案".into(),
        Locale::Zh,
    )
    .expect("CAS loss is an idempotent success");
    assert_eq!(result.appended, None);
    assert!(!result.resolved);

    let conn = db.0.lock().unwrap();
    let msgs = db::get_messages(&conn, "s-cas-loss").unwrap();
    assert_eq!(msgs.len(), 1, "CAS loser must not append a user message");
    match &msgs[0].content[0] {
        db::Block::DecisionCard { chosen_option, .. } => {
            assert_eq!(chosen_option.as_deref(), Some("先到答案"));
        }
        other => panic!("期望 DecisionCard·得到 {other:?}"),
    }
}

#[test]
fn answer_question_inner_missing_slot_and_already_chosen_card_errors() {
    // 空 map + DB 卡已经是 chosen（真双击/已回答）→ 维持 NO_PENDING_QUESTION，不产生任何新消息。
    let db = test_db();
    {
        let conn = db.0.lock().unwrap();
        seed_pending_decision_card(&conn, "s-chosen", "d-chosen", "已经问过了");
        db::update_decision_card_status(
            &conn,
            "s-chosen",
            "d-chosen",
            "pending",
            "chosen",
            Some("继续"),
        )
        .unwrap();
    }
    let q = LeadQuestions::default();

    let err = answer_question_inner(&q, &db, "s-chosen", "d-chosen", "算了".into(), Locale::Zh)
        .unwrap_err();
    assert_eq!(err, "NO_PENDING_QUESTION");

    let conn = db.0.lock().unwrap();
    let msgs = db::get_messages(&conn, "s-chosen").unwrap();
    assert_eq!(msgs.len(), 1, "不应新增任何消息: {msgs:?}");
}
