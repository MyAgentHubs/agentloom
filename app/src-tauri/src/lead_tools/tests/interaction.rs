#![cfg(test)]

use super::*;

#[test]
fn ask_user_rejects_empty_question() {
    let args = AskUserArgs {
        question: "".into(),
        options: vec!["a".into(), "b".into()],
        recommended: None,
        rationale: None,
    };
    assert!(
        validate_ask_user_args(&args).is_err(),
        "empty question should be rejected"
    );
}

#[test]
fn ask_user_rejects_too_few_options() {
    // zero options
    let args0 = AskUserArgs {
        question: "选哪个？".into(),
        options: vec![],
        recommended: None,
        rationale: None,
    };
    assert_eq!(
        validate_ask_user_args(&args0),
        Err("AL_ERR:leadTools.askUserNeedsOptions".into()),
        "zero options should be rejected with a localizable envelope"
    );
    // one option
    let args1 = AskUserArgs {
        question: "选哪个？".into(),
        options: vec!["仅此一选".into()],
        recommended: None,
        rationale: None,
    };
    assert_eq!(
        validate_ask_user_args(&args1),
        Err("AL_ERR:leadTools.askUserNeedsOptions".into()),
        "one option should be rejected with a localizable envelope"
    );
}

#[test]
fn ask_user_accepts_valid_args() {
    let args = AskUserArgs {
        question: "你好吗？".into(),
        options: vec!["好".into(), "不好".into()],
        recommended: Some("好".into()),
        rationale: Some("测试".into()),
    };
    assert!(
        validate_ask_user_args(&args).is_ok(),
        "valid args should be accepted"
    );
}

#[test]
fn propose_verifier_rejects_empty_cmd() {
    let args = ProposeVerifierArgs {
        cmd: "".into(),
        rationale: None,
    };
    assert!(
        validate_propose_verifier_args(&args).is_err(),
        "empty cmd should be rejected"
    );
}

#[test]
fn propose_verifier_rejects_whitespace_only_cmd() {
    let args = ProposeVerifierArgs {
        cmd: "   ".into(),
        rationale: None,
    };
    assert!(
        validate_propose_verifier_args(&args).is_err(),
        "whitespace-only cmd should be rejected"
    );
}

#[test]
fn propose_verifier_accepts_valid_cmd() {
    let args = ProposeVerifierArgs {
        cmd: "cargo test".into(),
        rationale: Some("testing".into()),
    };
    assert!(
        validate_propose_verifier_args(&args).is_ok(),
        "valid cmd should be accepted"
    );
}

#[test]
fn verifier_result_summary_text_is_bilingual() {
    // fold-default 改款：Auto 直跑后的结果信息卡短摘要（替代旧版长文案 verifier_result_echo_text）。
    assert_eq!(
        verifier_result_summary_text(crate::Locale::Zh, "passed"),
        "自动验证 · 通过"
    );
    assert_eq!(
        verifier_result_summary_text(crate::Locale::En, "passed"),
        "Auto verification · passed"
    );
    assert_eq!(
        verifier_result_summary_text(crate::Locale::Zh, "failed"),
        "自动验证 · 未通过"
    );
    assert_eq!(
        verifier_result_summary_text(crate::Locale::En, "failed"),
        "Auto verification · failed"
    );
}

#[test]
fn verifier_result_block_is_folded_command_card_with_bilingual_summary() {
    // fold-default 核心断言：产出必须是 Block::Tool（折叠默认命令卡），工具名固定
    // "verifier"（跨刀协调已定），完整命令进 output（可展开区），verdict 正确映射
    // status/exit_code。
    match verifier_result_block(crate::Locale::Zh, "cargo test", "passed", Some(0)) {
        crate::db::Block::Tool {
            tool,
            summary,
            card,
            status,
            exit_code,
            output,
            ..
        } => {
            assert_eq!(tool, "verifier");
            assert_eq!(summary, "自动验证 · 通过");
            assert_eq!(card, crate::db::BlockCardKind::Command);
            assert_eq!(status, crate::db::BlockToolStatus::Ok);
            assert_eq!(exit_code, Some(0));
            assert_eq!(output.as_deref(), Some("cargo test"));
        }
        other => panic!("expected Block::Tool, got {other:?}"),
    }

    match verifier_result_block(crate::Locale::En, "npm test", "failed", Some(1)) {
        crate::db::Block::Tool {
            tool,
            summary,
            card,
            status,
            exit_code,
            output,
            ..
        } => {
            assert_eq!(tool, "verifier");
            assert_eq!(summary, "Auto verification · failed");
            assert_eq!(card, crate::db::BlockCardKind::Command);
            assert_eq!(status, crate::db::BlockToolStatus::Failed);
            assert_eq!(exit_code, Some(1));
            assert_eq!(output.as_deref(), Some("npm test"));
        }
        other => panic!("expected Block::Tool, got {other:?}"),
    }
}

#[test]
fn verifier_result_block_ids_are_unique_across_calls() {
    let a = verifier_result_block(crate::Locale::Zh, "cargo test", "passed", Some(0));
    let b = verifier_result_block(crate::Locale::Zh, "cargo test", "passed", Some(0));
    let id_of = |blk: crate::db::Block| match blk {
        crate::db::Block::Tool { id, .. } => id,
        other => panic!("expected Block::Tool, got {other:?}"),
    };
    assert_ne!(id_of(a), id_of(b), "每次落卡的块 id 必须互不相同");
}

// 决策打扰收敛刀 T4：prompt_user / append_decision_echo 都靠 db::append_message 把
// agent_id/agent_name 落进消息行（本仓无 tauri AppHandle 测试基础设施·无法直接调用
// 这两个私有函数本体，故在它们依赖的 db 层验证同一份写入形状能原样读回——这正是
// 两处改动唯一新增的行为：把 None,None 换成真实身份）。

#[test]
fn decision_card_message_round_trips_agent_identity() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    let block = crate::db::Block::DecisionCard {
        decision_id: "d1".into(),
        kind: "ask".into(),
        question: "跑验证命令「cargo test」？".into(),
        options: vec!["运行".into(), "跳过".into()],
        recommended: Some("运行".into()),
        rationale: None,
        payload: serde_json::Value::Null,
        source_run_id: format!("{}-r1", MCP_LEAD_DECISION_PREFIX),
        status: "pending".into(),
        chosen_option: None,
        created_at: 1,
    };
    crate::db::append_message(
        &conn,
        "s1",
        "assistant",
        std::slice::from_ref(&block),
        Some("agent-team"),
        Some("lead-claude"),
        Some("Claude 队长"),
    )
    .unwrap();

    let msgs = crate::db::get_messages(&conn, "s1").unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].agent_id.as_deref(), Some("lead-claude"));
    assert_eq!(msgs[0].agent_name_snapshot.as_deref(), Some("Claude 队长"));
    assert_eq!(msgs[0].engine.as_deref(), Some("agent-team"));
}

#[test]
fn decision_echo_message_round_trips_agent_identity_and_keeps_excluded_tag() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    crate::db::append_message(
        &conn,
        "s1",
        "assistant",
        &[crate::db::Block::Text {
            text: "已选择「运行」（跑验证命令「cargo test」？）".into(),
        }],
        Some(DECISION_ECHO_ENGINE_TAG),
        Some("lead-claude"),
        Some("Claude 队长"),
    )
    .unwrap();

    let msgs = crate::db::get_messages(&conn, "s1").unwrap();
    assert_eq!(msgs.len(), 1);
    // engine 标记不变——lead_step::build_recent_messages 认这个 tag 排除，改了会破坏 T1。
    assert_eq!(msgs[0].engine.as_deref(), Some(DECISION_ECHO_ENGINE_TAG));
    assert_eq!(msgs[0].agent_id.as_deref(), Some("lead-claude"));
    assert_eq!(msgs[0].agent_name_snapshot.as_deref(), Some("Claude 队长"));
}

// 决策打扰收敛刀 T2：propose_verifier 本体依赖 tauri::AppHandle（app.state::<Db>() /
// app.path() / current_locale(app)），本仓无 tauri AppHandle 测试基础设施（同 T4 一带
// 注释、也是 worktree.rs 里 cfg(not(target_os = "macos")) 分支只能标"no-op"的同一限制）。
// 这里在 append_verifier_result_echo 依赖的 db 层验证同一份写入形状能原样读回 +
// build_recent_messages 排除生效——这正是 T2 唯一新增的落库行为。

#[test]
fn verifier_result_echo_message_round_trips_agent_identity_and_keeps_excluded_tag() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    crate::db::append_message(
        &conn,
        "s1",
        "assistant",
        &[verifier_result_block(
            crate::Locale::Zh,
            "cargo test",
            "passed",
            Some(0),
        )],
        Some(VERIFIER_RESULT_ENGINE_TAG),
        Some("lead-claude"),
        Some("Claude 队长"),
    )
    .unwrap();

    let msgs = crate::db::get_messages(&conn, "s1").unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].engine.as_deref(), Some(VERIFIER_RESULT_ENGINE_TAG));
    assert_eq!(msgs[0].agent_id.as_deref(), Some("lead-claude"));
    assert_eq!(msgs[0].agent_name_snapshot.as_deref(), Some("Claude 队长"));
    assert!(msgs[0].content.iter().any(|b| matches!(
        b,
        crate::db::Block::Tool { tool, summary, .. }
            if tool == "verifier" && summary.contains("通过")
    )));
}

#[test]
fn legacy_text_shaped_verifier_echo_still_round_trips() {
    // 向后兼容：改造前库里已存的旧版纯文本回执（Block::Text）不迁移、不动数据，
    // 读回仍要正常反序列化——新旧两种块形状能在同一列共存。
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    crate::db::append_message(
        &conn,
        "s1",
        "assistant",
        &[crate::db::Block::Text {
            text: "已自动执行验证命令「cargo test」·结果：passed".into(),
        }],
        Some(VERIFIER_RESULT_ENGINE_TAG),
        None,
        None,
    )
    .unwrap();

    let msgs = crate::db::get_messages(&conn, "s1").unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].engine.as_deref(), Some(VERIFIER_RESULT_ENGINE_TAG));
    assert!(matches!(
        &msgs[0].content[0],
        crate::db::Block::Text { text } if text.contains("cargo test")
    ));
}

// ---- 决策打扰收敛刀 T1·症状 B：append_decision_echo_message 落库成功须回一条完整
// db::Message（供外层 emit "lead-message-appended" 用）----

fn seed_session_for_echo(conn: &rusqlite::Connection, session_id: &str) {
    crate::db::create_session(conn, session_id, "x", "local-default", "local").unwrap();
}

#[test]
fn append_decision_echo_message_returns_full_message_with_id() {
    let conn = crate::test_support::mem_db();
    seed_session_for_echo(&conn, "s-echo");

    let message = append_decision_echo_message(
        &conn,
        "s-echo",
        "d-echo",
        "要不要继续？",
        "继续",
        Some("lead-claude"),
        Some("Claude 队长"),
    )
    .expect("落库成功应回完整 Message");

    assert!(message.id > 0, "应带真实自增 id: {message:?}");
    assert_eq!(message.role, "assistant");
    assert_eq!(message.engine.as_deref(), Some(DECISION_ECHO_ENGINE_TAG));
    assert_eq!(message.agent_id.as_deref(), Some("lead-claude"));
    assert_eq!(message.agent_name_snapshot.as_deref(), Some("Claude 队长"));
    match &message.content[0] {
        crate::db::Block::Text { text } => {
            assert!(text.contains("继续"), "应带答案: {text}");
            assert!(text.contains("要不要继续？"), "应带问题原文: {text}");
        }
        other => panic!("期望 Text·得到 {other:?}"),
    }

    // 落库确实生效：get_messages 能读回同一条，且 id 与返回值一致。
    let msgs = crate::db::get_messages(&conn, "s-echo").unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].id, message.id);
}

#[test]
fn append_decision_echo_message_two_calls_produce_two_distinct_ids() {
    // 每次点击各自落一条、各自拿到自己的新 id——不会互相覆盖或复用同一行。
    let conn = crate::test_support::mem_db();
    seed_session_for_echo(&conn, "s-echo-2");

    let first =
        append_decision_echo_message(&conn, "s-echo-2", "d-echo-2-a", "Q1", "A1", None, None)
            .expect("第一条应落库成功");
    let second =
        append_decision_echo_message(&conn, "s-echo-2", "d-echo-2-b", "Q2", "A2", None, None)
            .expect("第二条应落库成功");

    assert_ne!(first.id, second.id);
    let msgs = crate::db::get_messages(&conn, "s-echo-2").unwrap();
    assert_eq!(msgs.len(), 2);
}

// ---- msgfix1 T5 缺口③：三处「落库+publish 统一链路」调用点各自的 dedup_key 稳定性 +
// 确实经 publish 链路发出 msg.completed ----

#[test]
fn append_decision_card_message_publishes_msg_completed_with_stable_dedup_key() {
    let conn = crate::test_support::mem_db();
    seed_session_for_echo(&conn, "s-card");
    crate::remote_gateway::test_take_publish_log(); // 清空可能的残留

    let block = crate::db::Block::Text {
        text: "决策卡".into(),
    };
    let milestone = append_decision_card_message(&conn, "s-card", "d-card-1", &block, None, None)
        .expect("落库不应报错")
        .expect("首次落库应产出可发布的 milestone");
    milestone.publish();

    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "决策卡承载消息必须经统一 publish 链路发出 msg.completed"
    );

    let dedup_key: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE session_id = 's-card'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dedup_key, "decision_card:d-card-1");

    // 稳定性：同一 decision_id 重放（如队长重试同一次 ask_user）必须被 dedup_key 挡下，
    // 不重复插入第二行。
    let retry = append_decision_card_message(&conn, "s-card", "d-card-1", &block, None, None)
        .expect("重放不应报错");
    assert!(
        retry.is_none(),
        "同一 decision_id 重放必须被 dedup_key 去重"
    );
}

#[test]
fn append_decision_echo_message_publishes_msg_completed_with_stable_dedup_key() {
    let conn = crate::test_support::mem_db();
    seed_session_for_echo(&conn, "s-echo-publish");
    crate::remote_gateway::test_take_publish_log();

    append_decision_echo_message(
        &conn,
        "s-echo-publish",
        "d-echo-publish",
        "要不要继续？",
        "继续",
        None,
        None,
    )
    .expect("落库成功应回完整 Message");

    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "决策回显必须经统一 publish 链路发出 msg.completed"
    );

    let dedup_key: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE session_id = 's-echo-publish'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dedup_key, "decision_echo:d-echo-publish");

    let retry = append_decision_echo_message(
        &conn,
        "s-echo-publish",
        "d-echo-publish",
        "要不要继续？",
        "继续",
        None,
        None,
    );
    assert!(
        retry.is_none(),
        "同一 decision_id 重放必须被 decision_echo:<decision_id> dedup_key 去重"
    );
}

#[test]
fn append_verifier_result_message_publishes_msg_completed_with_dedup_key_from_block_id() {
    let conn = crate::test_support::mem_db();
    seed_session_for_echo(&conn, "s-verifier");
    crate::remote_gateway::test_take_publish_log();

    let block = verifier_result_block(crate::Locale::Zh, "cargo test", "passed", Some(0));
    let crate::db::Block::Tool { id: block_id, .. } = &block else {
        panic!("verifier_result_block 应恒产出 Block::Tool");
    };
    let expected_dedup_key = format!("verifier_result:{block_id}");

    let milestone = append_verifier_result_message(&conn, "s-verifier", &block, None, None)
        .expect("落库不应报错")
        .expect("首次落库应产出可发布的 milestone");
    milestone.publish();

    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "验证回执必须经统一 publish 链路发出 msg.completed"
    );

    let dedup_key: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE session_id = 's-verifier'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dedup_key, expected_dedup_key, "dedup_key 必须复用块自身 id");

    // 稳定性：同一个块（同一 dedup_key）重放必须被去重。
    let retry = append_verifier_result_message(&conn, "s-verifier", &block, None, None)
        .expect("重放不应报错");
    assert!(retry.is_none(), "同一块 id 重放必须被 dedup_key 去重");
}
