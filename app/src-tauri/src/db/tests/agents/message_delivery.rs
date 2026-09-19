#![cfg(test)]

use super::*;

// 刀 R P0-2：messages.dedup_key 迁移 + 部分唯一索引。

#[test]
fn init_schema_new_db_has_dedup_key_column_and_index() {
    let c = mem();
    let mut stmt = c.prepare("PRAGMA table_info(messages)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"dedup_key".to_string()),
        "messages 应含 dedup_key 列：实际 {cols:?}"
    );
    let idx_exists: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_messages_dedup'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_exists, 1, "idx_messages_dedup 索引应已建");
    // 幂等：跑两遍不炸
    init_schema(&c).unwrap();
}

#[test]
fn init_schema_adds_dedup_key_to_messages_idempotent() {
    // 模拟「旧库无 dedup_key 列」→ 调 init_schema 应补列 + 建索引，且可重复调不报错。
    let c = Connection::open_in_memory().unwrap();
    c.execute(
        "CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL CHECK (json_valid(content)),
                engine TEXT,
                agent_id TEXT,
                agent_name_snapshot TEXT,
                created_at INTEGER NOT NULL
            )",
        [],
    )
    .unwrap();
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(messages)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        cols.contains(&"dedup_key".to_string()),
        "旧库 messages 应补上 dedup_key 列：实际 {cols:?}"
    );
    let idx_exists: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_messages_dedup'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_exists, 1, "idx_messages_dedup 索引应已建");
    // 再跑一次（幂等性 · 旧库二次启动）应不报错
    init_schema(&c).unwrap();
}

#[test]
fn append_message_dedup_same_key_twice_writes_once() {
    let c = crate::test_support::mem_db();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let ok1 = append_message_dedup(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "first".into(),
        }],
        Some("claude"),
        None,
        None,
        "run_flush:r1",
    )
    .unwrap();
    assert!(ok1.is_some(), "第一次写应成功并返回 Some(milestone)");
    let ok2 = append_message_dedup(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "second write attempt".into(),
        }],
        Some("claude"),
        None,
        None,
        "run_flush:r1",
    )
    .unwrap();
    assert!(ok2.is_none(), "同键第二次写应被挡、返回 None");
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM messages WHERE session_id = 's1' AND dedup_key = 'run_flush:r1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "同键只应落 1 行");
}

#[test]
fn append_message_dedup_and_publish_publishes_only_for_new_key() {
    let c = crate::test_support::mem_db();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();

    crate::remote_gateway::test_take_publish_log();
    assert!(append_message_dedup_and_publish(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "first".into(),
        }],
        Some("claude"),
        None,
        None,
        "run_flush:publish-r1",
    )
    .unwrap());
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"]
    );

    assert!(!append_message_dedup_and_publish(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "duplicate".into(),
        }],
        Some("claude"),
        None,
        None,
        "run_flush:publish-r1",
    )
    .unwrap());
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
}

#[test]
fn append_message_dedup_defers_publish_until_caller_commits() {
    let c = crate::test_support::mem_db();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();

    crate::remote_gateway::test_take_publish_log();
    let tx = c.unchecked_transaction().unwrap();
    let milestone = append_message_dedup(
        &tx,
        "s1",
        "assistant",
        &[Block::Text {
            text: "in-tx".into(),
        }],
        Some("claude"),
        None,
        None,
        "run_flush:tx-defer",
    )
    .unwrap();
    assert!(milestone.is_some(), "真插入应返回 Some(milestone)");
    assert!(
        crate::remote_gateway::test_take_publish_log().is_empty(),
        "append_message_dedup 不应在事务提交前发布 msg.completed"
    );

    tx.commit().unwrap();
    milestone.unwrap().publish();
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "commit 成功之后调用 publish() 才应该真正发布"
    );
}

#[test]
fn member_report_delivery_atomic_helper_commits_message_pending_then_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("member-report-publish-order.db");
    let c = Connection::open(&db_path).unwrap();
    init_schema(&c).unwrap();
    c.execute(
        "INSERT OR IGNORE INTO namespaces
                (id, kind, name, is_builtin, added_at)
             VALUES ('local', 'local', 'Local', 1, 0)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT OR IGNORE INTO repos
                (id, namespace_id, source, name, path, status, added_at)
             VALUES ('local-default', 'local', 'local', 'Local', '/tmp', 'active', 0)",
        [],
    )
    .unwrap();
    create_session(&c, "delivery-success", "x", "local-default", "local").unwrap();
    let observer = Connection::open(&db_path).unwrap();
    let rows_visible_at_publish = std::cell::Cell::new(false);

    crate::remote_gateway::test_take_publish_log();
    assert!(persist_member_report_atomic_with_publish(
        &c,
        "delivery-success",
        &[Block::Text {
            text: "[Worker report]\nstatus: done".into(),
        }],
        Some("worker-agent"),
        Some("Worker"),
        "member_result:run-1:assignment-1",
        Some("assignment-1"),
        None,
        |milestone| {
            let visible: (i64, i64) = observer
                .query_row(
                    "SELECT
                            (SELECT count(*) FROM messages
                              WHERE session_id = 'delivery-success'
                                AND dedup_key = 'member_result:run-1:assignment-1'),
                            (SELECT count(*) FROM member_report_delivery
                              WHERE session_id = 'delivery-success'
                                AND delivered_at IS NULL)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            rows_visible_at_publish.set(visible == (1, 1));
            milestone.publish();
        },
    )
    .unwrap());
    assert!(
            rows_visible_at_publish.get(),
            "publish 回调触发时，独立连接必须已能看见 message 与 pending 台账；否则 publish 早于 commit"
        );

    let message_id: i64 = c
        .query_row(
            "SELECT id FROM messages WHERE session_id = 'delivery-success'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let pending: (i64, String, Option<i64>) = c
        .query_row(
            "SELECT message_id, assignment_id, delivered_at
                   FROM member_report_delivery
                  WHERE session_id = 'delivery-success'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(pending, (message_id, "assignment-1".into(), None));
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "helper 只能在消息与 pending 台账同时 commit 后 publish"
    );
}

#[test]
fn member_report_delivery_atomic_helper_rolls_back_without_ghost_publish() {
    let c = crate::test_support::mem_db();
    create_session(&c, "delivery-rollback", "x", "local-default", "local").unwrap();
    for _ in 0..2 {
        append_message(
            &c,
            "delivery-rollback",
            "assistant",
            &[member_report_delivery_running_card("assignment-1")],
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();
    }
    let before = get_messages(&c, "delivery-rollback").unwrap();
    let second_card_id = before[1].id;
    c.execute_batch(&format!(
        "CREATE TRIGGER fail_second_dispatch_card_update
             BEFORE UPDATE OF content ON messages
             WHEN OLD.id = {second_card_id}
             BEGIN
               SELECT RAISE(FAIL, 'forced second DispatchCard update failure');
             END;"
    ))
    .unwrap();

    crate::remote_gateway::test_take_publish_log();
    let error = persist_member_report_atomic(
        &c,
        "delivery-rollback",
        &[Block::Text {
            text: "[Worker report]\nstatus: failed".into(),
        }],
        Some("worker-agent"),
        Some("Worker"),
        "member_result:run-rollback:assignment-1",
        Some("assignment-1"),
        Some(("failed", "[Worker report]\nstatus: failed")),
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("forced second DispatchCard update failure"));

    assert_eq!(
        get_messages(&c, "delivery-rollback").unwrap(),
        before,
        "第二张卡更新失败后，第一张 DispatchCard 也必须回滚到 running，且不得留下 report 消息"
    );
    let delivery_count: i64 = c
        .query_row(
            "SELECT count(*) FROM member_report_delivery
                  WHERE session_id = 'delivery-rollback'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(delivery_count, 0, "rollback 后不得留 pending 台账幽灵行");
    assert!(
        crate::remote_gateway::test_take_publish_log().is_empty(),
        "rollback 路径不得发布 msg.completed 幽灵事件"
    );
}

#[test]
fn persist_member_report_atomic_republishes_dispatch_card_terminal_rewrite_with_new_revision() {
    // msgfix1 T5（缺口④）：running dispatch_card 终态改写提交后，必须重读该消息、以新
    // revision 重发 msg.completed——revision 从 1（running）bump 到 2（done），
    // derive_msg_completed_client_msg_id 随 revision 变化，relay 才会把这次改写当"新事件"
    // 广播，不会被幂等去重吞掉。
    let c = crate::test_support::mem_db();
    create_session(&c, "s-dispatch-republish", "x", "local-default", "local").unwrap();

    // 先落一条带 running dispatch_card 的 lead 消息（真实生产路径：lead 自己 flush 走
    // append_message_dedup*，dedup_key 用 run_flush 风格）。
    let dispatch_card = member_report_delivery_running_card("assignment-republish");
    let inserted = append_message_dedup(
        &c,
        "s-dispatch-republish",
        "assistant",
        &[dispatch_card],
        Some("agent-team"),
        None,
        None,
        "run_flush:lead-run-republish",
    )
    .unwrap()
    .expect("首次落库应产出 milestone");
    let dispatch_message_id = inserted.message_id;
    assert_eq!(
        c.query_row(
            "SELECT revision FROM messages WHERE id = ?1",
            [dispatch_message_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        1,
        "新插入行 revision 应为 schema 默认值 1"
    );

    crate::remote_gateway::test_take_publish_log();

    let inserted_report = persist_member_report_atomic(
        &c,
        "s-dispatch-republish",
        &[Block::Text {
            text: "[Worker report]\nstatus: done".into(),
        }],
        Some("worker-agent"),
        Some("Worker"),
        "member_result:run-republish:assignment-republish",
        Some("assignment-republish"),
        Some(("done", "[Worker report]\nstatus: done")),
    )
    .unwrap();
    assert!(inserted_report, "新报告消息应真插入");

    // DB 侧：dispatch_card 消息 revision 必须 +1（原有不变量，未受影响）。
    let bumped_revision: i64 = c
        .query_row(
            "SELECT revision FROM messages WHERE id = ?1",
            [dispatch_message_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        bumped_revision, 2,
        "终态改写必须把 dispatch_card 消息 revision 从 1 bump 到 2"
    );

    // publish 侧：一次是新报告消息本身的 msg.completed，一次是 dispatch_card 终态改写的
    // 重发——不是只发了新报告那一条就完事。
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed", "msg.completed"],
        "缺口④：终态改写必须额外重发一次 msg.completed"
    );

    // 重读侧：get_message_for_republish 必须能读到 bump 后的 revision 与改写后的内容，
    // dedup_key 与首发时一致（否则 client_msg_id 对不上同一条消息）。
    let republish = get_message_for_republish(&c, "s-dispatch-republish", dispatch_message_id)
        .unwrap()
        .expect("dispatch_card 消息应可重读用于重发");
    assert_eq!(republish.dedup_key, "run_flush:lead-run-republish");
    assert_eq!(republish.revision, 2);
    assert!(
        republish.content_raw.contains("\"status\":\"done\""),
        "重读内容必须是终态改写后的内容: {}",
        republish.content_raw
    );

    // client_msg_id 必须随 revision 变化——旧 revision(1) 与新 revision(2) 派生出不同
    // id，relay 才会把这次改写当"新事件"广播、不被幂等去重吞掉。
    assert_ne!(
        crate::remote_gateway::derive_msg_completed_client_msg_id(
            "s-dispatch-republish",
            &republish.dedup_key,
            1,
        ),
        crate::remote_gateway::derive_msg_completed_client_msg_id(
            "s-dispatch-republish",
            &republish.dedup_key,
            republish.revision,
        ),
        "revision 改变后 client_msg_id 必须跟着变"
    );
}

#[test]
fn member_report_delivery_grandfather_message_without_row_is_not_pending() {
    let c = crate::test_support::mem_db();
    create_session(&c, "delivery-grandfather", "x", "local-default", "local").unwrap();
    append_message(
        &c,
        "delivery-grandfather",
        "assistant",
        &[Block::Text {
            text: "[Worker report]\nlegacy".into(),
        }],
        Some("agent-team"),
        Some("worker-agent"),
        Some("Worker"),
    )
    .unwrap();

    assert!(
        pending_member_report_message_ids(&c, "delivery-grandfather")
            .unwrap()
            .is_empty(),
        "无台账行的存量消息必须视为已交付"
    );
}

#[test]
fn member_report_delivery_delivered_row_is_not_pending() {
    let c = crate::test_support::mem_db();
    create_session(&c, "delivery-complete", "x", "local-default", "local").unwrap();
    persist_member_report_atomic(
        &c,
        "delivery-complete",
        &[Block::Text {
            text: "[Worker report]\nstatus: done".into(),
        }],
        Some("worker-agent"),
        Some("Worker"),
        "member_result:run-complete:assignment-1",
        Some("assignment-1"),
        None,
    )
    .unwrap();
    c.execute(
        "UPDATE member_report_delivery
                SET delivered_at = 1
              WHERE session_id = 'delivery-complete'",
        [],
    )
    .unwrap();

    assert!(
        pending_member_report_message_ids(&c, "delivery-complete")
            .unwrap()
            .is_empty(),
        "delivered_at 非 NULL 的台账行不得再算 pending"
    );
}

#[test]
fn member_report_delivery_delete_session_purges_rows() {
    let c = crate::test_support::mem_db();
    create_session(&c, "delivery-purge", "x", "local-default", "local").unwrap();
    persist_member_report_atomic(
        &c,
        "delivery-purge",
        &[Block::Text {
            text: "[Worker report]\nstatus: done".into(),
        }],
        Some("worker-agent"),
        Some("Worker"),
        "member_result:run-purge:assignment-1",
        Some("assignment-1"),
        None,
    )
    .unwrap();

    delete_session(&c, "delivery-purge").unwrap();
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM member_report_delivery WHERE session_id = 'delivery-purge'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn append_message_dedup_different_keys_each_write() {
    let c = crate::test_support::mem_db();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    append_message_dedup(
        &c,
        "s1",
        "assistant",
        &[Block::Text { text: "a".into() }],
        Some("claude"),
        None,
        None,
        "run_flush:r1",
    )
    .unwrap();
    append_message_dedup(
        &c,
        "s1",
        "assistant",
        &[Block::Text { text: "b".into() }],
        Some("claude"),
        None,
        None,
        "run_flush:r2",
    )
    .unwrap();
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM messages WHERE session_id = 's1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2, "不同键各应落 1 行");
}

#[test]
fn append_message_null_dedup_key_path_unaffected_by_index() {
    // 既有 append_message 不传键（NULL）——多次写不受部分唯一索引影响。
    let c = crate::test_support::mem_db();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    for _ in 0..3 {
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::Text { text: "hi".into() }],
            Some("claude"),
            None,
            None,
        )
        .unwrap();
    }
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM messages WHERE session_id = 's1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 3, "NULL dedup_key 不参与唯一约束，3 次写应各自成功");
}
