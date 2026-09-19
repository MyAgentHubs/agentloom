#![cfg(test)]

use super::*;

#[test]
fn upsert_activity_summary_and_publish_first_call_inserts_revision_one_and_publishes() {
    // msgfix2 U1（设计稿 v4.1 §4.1）：首次出现某 run 的 activity_summary——新 INSERT，
    // revision 走 schema DEFAULT 1，content 是单元素 blocks 数组、type="activity_summary"，
    // 六个计数/状态字段齐全，且首发就走 publish（不是只落库不发）。
    let c = crate::test_support::mem_db();
    create_session(&c, "s-activity-summary", "x", "local-default", "local").unwrap();
    crate::remote_gateway::test_take_publish_log();

    upsert_activity_summary_and_publish(&c, "s-activity-summary", "run-1", 3, 0, 1, 0, "running")
        .unwrap();

    let (dedup_key, content_raw, revision): (String, String, i64) = c
        .query_row(
            "SELECT dedup_key, content, revision FROM messages WHERE session_id = ?1",
            ["s-activity-summary"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(dedup_key, "activity_summary:run-1");
    assert_eq!(revision, 1, "首次插入 revision 必须走 schema DEFAULT 1");

    let content: serde_json::Value = serde_json::from_str(&content_raw).unwrap();
    let block = &content[0];
    assert_eq!(block["type"], "activity_summary");
    assert_eq!(block["run_id"], "run-1");
    assert_eq!(block["tool_calls"], 3);
    assert_eq!(block["failed"], 0);
    assert_eq!(block["mcp_calls"], 1);
    assert_eq!(block["permission_prompts"], 0);
    assert_eq!(block["state"], "running");

    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "首次 upsert 必须发布一次 msg.completed"
    );
}

#[test]
fn get_messages_excludes_activity_summary_rows() {
    // R1（msgfix2 整盘审）：`activity_summary:*` 消息落共享 messages 表，但它不是一条
    // 真实的助手消息——`get_messages` 是桌面消息流 + `build_agent_prompt`（lib.rs）+
    // `continuation.rs` 交接窗口共用的唯一 history 来源，三条下游都不该看到它。
    let c = crate::test_support::mem_db();
    create_session(&c, "s-r1-get-messages", "x", "local-default", "local").unwrap();
    append_message(
        &c,
        "s-r1-get-messages",
        "user",
        &[Block::Text {
            text: "第一条真实消息".to_owned(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    upsert_activity_summary_and_publish(&c, "s-r1-get-messages", "run-1", 2, 0, 0, 0, "running")
        .unwrap();
    append_message(
        &c,
        "s-r1-get-messages",
        "assistant",
        &[Block::Text {
            text: "第二条真实消息".to_owned(),
        }],
        None,
        None,
        None,
    )
    .unwrap();

    let messages = get_messages(&c, "s-r1-get-messages").unwrap();
    assert_eq!(
        messages.len(),
        2,
        "activity_summary 消息必须被排除在外，只剩两条真实消息"
    );
    for message in &messages {
        assert_ne!(
            blocks_to_text(&message.content),
            "",
            "真实消息不该有空 content（activity_summary 混进来才会解析成空 blocks）"
        );
    }
}

#[test]
fn session_index_preview_skips_activity_summary_falls_back_to_real_message() {
    // R1（msgfix2 整盘审）：db.rs:6559 附近——latest_message 子查询如果选中了
    // `activity_summary:*` 那条（它没有 type=="text" 的块），`session_index_message_
    // preview` 找不到文本块只能返回 None，会话预览被顶成空白。修复后子查询必须跳过
    // activity_summary、回退到更早的那条真实文本消息。
    let c = crate::test_support::mem_db();
    create_session(&c, "s-r1-preview", "预览会话", "local-default", "local").unwrap();
    append_message(
        &c,
        "s-r1-preview",
        "assistant",
        &[Block::Text {
            text: "这是真正的最后一条消息内容".to_owned(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    // activity_summary 在真实消息之后写入——成为该 session 在 messages 表里 id 最大的行。
    upsert_activity_summary_and_publish(&c, "s-r1-preview", "run-1", 1, 0, 0, 0, "running")
        .unwrap();

    let rows = list_session_index_snapshot_rows(&c).unwrap();
    let row = rows
        .into_iter()
        .find(|r| r.id == "s-r1-preview")
        .expect("会话必须出现在快照行里");
    assert_eq!(
        row.last_msg_preview.as_deref(),
        Some("这是真正的最后一条消息内容"),
        "预览必须回退到真实文本消息，不能因为最新一行是 activity_summary 就顶成 None"
    );
}

#[test]
fn upsert_activity_summary_content_shape_is_exactly_seven_keys_no_actionable_leak() {
    // L0 保护反向测试（设计稿 v4.1 §4.1）：activity_summary 块必须**只**含七个键（type +
    // 六个计数/状态字段）——不管调用方传了什么，这条写入路径在结构上就没有任何字段能装下
    // approval/decision_card/scope_change 之类需要用户行动的块内容，锁死"L1 摘要永不携带
    // actionable 块"这条不变量。
    let c = crate::test_support::mem_db();
    create_session(
        &c,
        "s-activity-summary-shape",
        "x",
        "local-default",
        "local",
    )
    .unwrap();
    upsert_activity_summary_and_publish(
        &c,
        "s-activity-summary-shape",
        "run-shape",
        1,
        0,
        0,
        0,
        "running",
    )
    .unwrap();
    let content_raw: String = c
        .query_row(
            "SELECT content FROM messages WHERE session_id = ?1",
            ["s-activity-summary-shape"],
            |r| r.get(0),
        )
        .unwrap();
    let content: serde_json::Value = serde_json::from_str(&content_raw).unwrap();
    let blocks = content.as_array().unwrap();
    assert_eq!(blocks.len(), 1, "activity_summary 恒为单元素 blocks 数组");
    let mut keys: Vec<&str> = blocks[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "failed",
            "mcp_calls",
            "permission_prompts",
            "run_id",
            "state",
            "tool_calls",
            "type",
        ],
        "只许这七个键——多一个键就是某种内容泄漏进了 L1 摘要"
    );
}

#[test]
fn upsert_activity_summary_and_publish_second_call_updates_in_place_bumps_revision_and_republishes()
{
    // msgfix2 U1：同一 run 第二次调用（计数变化/终态翻转）——同一 message_id 原地
    // UPDATE，revision 从 1 bump 到 2，不产生第二行；重发走缺口④同一姿势。
    let c = crate::test_support::mem_db();
    create_session(&c, "s-activity-summary-2", "x", "local-default", "local").unwrap();

    upsert_activity_summary_and_publish(&c, "s-activity-summary-2", "run-2", 1, 0, 0, 0, "running")
        .unwrap();
    let first_id: i64 = c
        .query_row(
            "SELECT id FROM messages WHERE session_id = ?1",
            ["s-activity-summary-2"],
            |r| r.get(0),
        )
        .unwrap();

    crate::remote_gateway::test_take_publish_log();
    upsert_activity_summary_and_publish(&c, "s-activity-summary-2", "run-2", 5, 1, 1, 1, "done")
        .unwrap();

    let row_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            ["s-activity-summary-2"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(row_count, 1, "第二次 upsert 不得插出第二行");

    let (id, content_raw, revision): (i64, String, i64) = c
        .query_row(
            "SELECT id, content, revision FROM messages WHERE session_id = ?1",
            ["s-activity-summary-2"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(id, first_id, "必须原地改写同一 message_id");
    assert_eq!(revision, 2, "第二次 upsert 必须把 revision 从 1 bump 到 2");
    let content: serde_json::Value = serde_json::from_str(&content_raw).unwrap();
    assert_eq!(content[0]["tool_calls"], 5);
    assert_eq!(content[0]["failed"], 1);
    assert_eq!(content[0]["state"], "done");

    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "第二次 upsert 必须重发一次 msg.completed（revision=2）"
    );
}

#[test]
fn upsert_activity_summary_and_publish_retry_with_unchanged_content_does_not_bump_revision() {
    // msgfix2 U1b（第三轮审查 B1/G1-b）：模拟写线程重试——同一份内容（同计数/同状态）被
    // 第二次调用（`flush_activity_summary` 失败重试语义，见该函数文档），必须原地不动
    // revision，不能把"内容相同的重放"误判成一次真正的内容推进。
    let c = crate::test_support::mem_db();
    create_session(
        &c,
        "s-activity-summary-retry",
        "x",
        "local-default",
        "local",
    )
    .unwrap();

    upsert_activity_summary_and_publish(
        &c,
        "s-activity-summary-retry",
        "run-retry",
        3,
        0,
        1,
        0,
        "running",
    )
    .unwrap();
    let revision_after_first: i64 = c
        .query_row(
            "SELECT revision FROM messages WHERE session_id = ?1",
            ["s-activity-summary-retry"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revision_after_first, 1);

    // 完全相同的六个字段再调一次——同 dedup_key 命中 UPDATE 分支，但 content 序列化结果
    // 逐字节相同。
    upsert_activity_summary_and_publish(
        &c,
        "s-activity-summary-retry",
        "run-retry",
        3,
        0,
        1,
        0,
        "running",
    )
    .unwrap();

    let (row_count, revision_after_retry): (i64, i64) = c
        .query_row(
            "SELECT COUNT(*), MAX(revision) FROM messages WHERE session_id = ?1",
            ["s-activity-summary-retry"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row_count, 1, "重试不得插出第二行");
    assert_eq!(
        revision_after_retry, 1,
        "内容未变的重试必须保持 revision=1，不能因为一次 best-effort 重试就把 revision 推到 2"
    );

    // 紧接着一次真正的内容变化（计数推进）——revision 必须照常 +1，证明幂等判断没有
    // 误伤真实的内容推进路径。
    upsert_activity_summary_and_publish(
        &c,
        "s-activity-summary-retry",
        "run-retry",
        4,
        0,
        1,
        0,
        "running",
    )
    .unwrap();
    let revision_after_real_change: i64 = c
        .query_row(
            "SELECT revision FROM messages WHERE session_id = ?1",
            ["s-activity-summary-retry"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        revision_after_real_change, 2,
        "真正的内容变化必须照常把 revision 从 1 bump 到 2"
    );
}

#[test]
fn reconcile_stale_running_activity_summaries_seals_orphans_and_skips_active_and_non_running() {
    // msgfix2 U1（设计稿 v4.1 §4.1「revision 保留」重启恢复规则）：state=running 且
    // run_id 不在 active_run_ids 里的一律封口为 failed 并 republish；仍在 active 集合里的
    // 不动；本就已经是终态（done）的也不动（不能把已经正常完结的 run 篡改成 failed）。
    let c = crate::test_support::mem_db();
    create_session(&c, "s-reconcile", "x", "local-default", "local").unwrap();

    upsert_activity_summary_and_publish(&c, "s-reconcile", "orphan-run", 2, 0, 0, 0, "running")
        .unwrap();
    upsert_activity_summary_and_publish(&c, "s-reconcile", "active-run", 4, 0, 0, 0, "running")
        .unwrap();
    upsert_activity_summary_and_publish(&c, "s-reconcile", "finished-run", 1, 0, 0, 0, "done")
        .unwrap();
    crate::remote_gateway::test_take_publish_log();

    let mut active = std::collections::HashSet::new();
    active.insert("active-run".to_string());
    let sealed = reconcile_stale_running_activity_summaries(&c, &active).unwrap();
    assert_eq!(sealed, 1, "只有 orphan-run 应被封口");

    let state_of = |run_id: &str| -> String {
        let content_raw: String = c
            .query_row(
                "SELECT content FROM messages WHERE session_id = ?1 AND dedup_key = ?2",
                ("s-reconcile", format!("activity_summary:{run_id}")),
                |r| r.get(0),
            )
            .unwrap();
        let content: serde_json::Value = serde_json::from_str(&content_raw).unwrap();
        content[0]["state"].as_str().unwrap().to_string()
    };
    assert_eq!(state_of("orphan-run"), "failed");
    assert_eq!(state_of("active-run"), "running");
    assert_eq!(state_of("finished-run"), "done");

    let orphan_revision: i64 = c
            .query_row(
                "SELECT revision FROM messages WHERE session_id = ?1 AND dedup_key = 'activity_summary:orphan-run'",
                ["s-reconcile"],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(orphan_revision, 2, "封口必须 revision+1");

    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "只应重发被封口的那一条"
    );
}
