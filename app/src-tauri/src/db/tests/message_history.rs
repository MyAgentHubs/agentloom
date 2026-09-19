#![cfg(test)]

use super::*;

#[test]
fn history_db_before_null_returns_latest_rows_and_cursor_is_exclusive() {
    let c = mem();
    create_session(&c, "history-page", "History", "local-default", "local").unwrap();
    let mut ids = Vec::new();
    for index in 0..5 {
        append_message(
            &c,
            "history-page",
            if index % 2 == 0 { "user" } else { "assistant" },
            &[Block::Text {
                text: format!("message-{index}"),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        ids.push(c.last_insert_rowid());
    }

    let latest = list_session_history_rows(&c, "history-page", None, 2).unwrap();
    assert_eq!(
        latest.iter().map(|row| row.message_id).collect::<Vec<_>>(),
        vec![ids[4], ids[3]]
    );
    let earlier = list_session_history_rows(&c, "history-page", Some(ids[3]), 10).unwrap();
    assert_eq!(
        earlier.iter().map(|row| row.message_id).collect::<Vec<_>>(),
        vec![ids[2], ids[1], ids[0]],
        "before_message_id 必须是严格小于边界"
    );
}

#[test]
fn history_db_filters_non_conversation_roles_and_preserves_content_json() {
    let c = mem();
    create_session(&c, "history-role", "History", "local-default", "local").unwrap();
    for role in ["user", "system", "assistant", "tool"] {
        append_message(
            &c,
            "history-role",
            role,
            &[Block::Text {
                text: format!("{role}-text"),
            }],
            None,
            None,
            None,
        )
        .unwrap();
    }

    let rows = list_session_history_rows(&c, "history-role", None, 10).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.role.as_str()).collect::<Vec<_>>(),
        vec!["assistant", "user"]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0].content).unwrap(),
        serde_json::json!([{"type": "text", "text": "assistant-text"}])
    );
}

#[test]
fn history_db_soft_deleted_session_returns_empty_page() {
    let c = mem();
    create_session(
        &c,
        "history-soft-deleted",
        "History",
        "local-default",
        "local",
    )
    .unwrap();
    append_message(
        &c,
        "history-soft-deleted",
        "assistant",
        &[Block::Text {
            text: "must stay hidden".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    set_session_deleted(&c, "history-soft-deleted").unwrap();

    let rows = list_session_history_rows(&c, "history-soft-deleted", None, 10).unwrap();
    assert!(rows.is_empty());
}

/// msgfix1 T4：`get_message_for_fetch` 四态钉死——命中未删/命中已软删/存在但属他 session/
/// 完全不存在。这四态是 `msg.fetch` 校验链第①步（forbidden/soft_deleted/not_found 三 code）
/// 唯一的数据来源，任何一态判错都会导致 wire 层泄露越权/不存在的区分或漏挡越权读取。
#[test]
fn get_message_for_fetch_returns_found_with_original_bytes_when_session_is_live() {
    let c = mem();
    create_session(&c, "fetch-live", "Fetch", "local-default", "local").unwrap();
    append_message(
        &c,
        "fetch-live",
        "assistant",
        &[Block::Text {
            text: "hello fetch".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let message_id = c.last_insert_rowid();

    let raw_content: String = c
        .query_row(
            "SELECT content FROM messages WHERE id = ?1",
            [message_id],
            |r| r.get(0),
        )
        .unwrap();

    match get_message_for_fetch(&c, "fetch-live", message_id).unwrap() {
        MessageForFetch::Found {
            content,
            revision,
            session_deleted,
        } => {
            assert_eq!(
                content, raw_content,
                "必须原样返回 DB content 字符串，不能重新序列化——sha256/切片按原文字节计算"
            );
            assert_eq!(revision, 1, "新建消息默认 revision = 1");
            assert!(!session_deleted, "session 未软删");
        }
        other => panic!("expected Found, got {other:?}"),
    }
}

#[test]
fn get_message_for_fetch_flags_session_deleted_without_hiding_the_message() {
    let c = mem();
    create_session(&c, "fetch-soft-deleted", "Fetch", "local-default", "local").unwrap();
    append_message(
        &c,
        "fetch-soft-deleted",
        "assistant",
        &[Block::Text {
            text: "still readable content".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let message_id = c.last_insert_rowid();
    set_session_deleted(&c, "fetch-soft-deleted").unwrap();

    match get_message_for_fetch(&c, "fetch-soft-deleted", message_id).unwrap() {
        MessageForFetch::Found {
            session_deleted, ..
        } => {
            assert!(
                session_deleted,
                "会话软删后仍要能拿到内容——由调用方决定回 soft_deleted，而不是让查询本身消失"
            );
        }
        other => panic!("expected Found{{session_deleted:true}}, got {other:?}"),
    }
}

#[test]
fn get_message_for_fetch_reports_wrong_session_for_a_message_owned_elsewhere() {
    let c = mem();
    create_session(&c, "fetch-owner", "Fetch", "local-default", "local").unwrap();
    create_session(&c, "fetch-intruder", "Fetch", "local-default", "local").unwrap();
    append_message(
        &c,
        "fetch-owner",
        "assistant",
        &[Block::Text {
            text: "owned by fetch-owner".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let message_id = c.last_insert_rowid();

    assert_eq!(
        get_message_for_fetch(&c, "fetch-intruder", message_id).unwrap(),
        MessageForFetch::WrongSession,
        "message_id 存在但归属别的 session——不得当 not_found 处理（会跟真不存在混为一谈）"
    );
}

#[test]
fn get_message_for_fetch_reports_not_found_for_an_unknown_message_id() {
    let c = mem();
    create_session(&c, "fetch-empty", "Fetch", "local-default", "local").unwrap();

    assert_eq!(
        get_message_for_fetch(&c, "fetch-empty", 999_999).unwrap(),
        MessageForFetch::NotFound
    );
}

/// msgfix1 T4 返修①（存在性 oracle）：全局 `SELECT 1 FROM messages WHERE id=?1` 兜底会让
/// "他 repo 的合法 message_id"（回 forbidden）与"纯捏造的 id"（回 not_found）产生不同响应
/// ——攻击者据此可枚举全库 message_id 是否存在，与仓库归属无关。收紧后跨 repo 存在必须与
/// 完全不存在**同响应** `NotFound`；只有同 repo 内存在但属别的 session 才回 `WrongSession`
/// （见 `get_message_for_fetch_reports_wrong_session_for_a_message_owned_elsewhere` 那条同
/// repo 正例）。
#[test]
fn get_message_for_fetch_reports_not_found_not_wrong_session_for_a_message_in_another_repo() {
    let c = mem();
    c.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) \
             VALUES ('other-repo', 'local', 'local', '别的项目', '/tmp/agentloom-mem-other-repo', 'active', 0)",
            [],
        )
        .unwrap();
    create_session(&c, "fetch-owner-other-repo", "Fetch", "other-repo", "local").unwrap();
    create_session(
        &c,
        "fetch-intruder-cross-repo",
        "Fetch",
        "local-default",
        "local",
    )
    .unwrap();
    append_message(
        &c,
        "fetch-owner-other-repo",
        "assistant",
        &[Block::Text {
            text: "owned by a session in a different repo".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let message_id = c.last_insert_rowid();

    assert_eq!(
        get_message_for_fetch(&c, "fetch-intruder-cross-repo", message_id).unwrap(),
        MessageForFetch::NotFound,
        "跨 repo 存在必须与真不存在同响应，不能通过响应差异探测全局 message_id 是否存在"
    );
}

#[test]
fn history_init_schema_creates_role_filtered_pagination_index() {
    let c = mem();
    let sql: String = c
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_messages_history'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let normalized = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(normalized.contains("ON messages(session_id, id DESC)"));
    assert!(normalized.contains("WHERE role IN ('user','assistant')"));
}

#[test]
fn migrate_backfill_dedup_updates_only_null_user_and_assistant_rows_idempotently() {
    let c = crate::test_support::mem_db();
    create_session(&c, "backfill", "Backfill", "local-default", "local").unwrap();
    let blocks = [Block::Text {
        text: "history".into(),
    }];

    append_message(&c, "backfill", "user", &blocks, None, None, None).unwrap();
    let user_id = c.last_insert_rowid();
    append_message(&c, "backfill", "assistant", &blocks, None, None, None).unwrap();
    let assistant_id = c.last_insert_rowid();
    append_message(&c, "backfill", "system", &blocks, None, None, None).unwrap();
    let system_id = c.last_insert_rowid();
    append_message_dedup(
        &c,
        "backfill",
        "assistant",
        &blocks,
        None,
        None,
        None,
        "existing-key",
    )
    .unwrap();
    let existing_id = c.last_insert_rowid();

    assert_eq!(migrate_backfill_dedup_keys(&c).unwrap(), 2);
    let keys: Vec<(i64, Option<String>)> = c
        .prepare("SELECT id, dedup_key FROM messages ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        keys,
        vec![
            (user_id, Some(format!("backfill:{user_id}"))),
            (assistant_id, Some(format!("backfill:{assistant_id}"))),
            (system_id, None),
            (existing_id, Some("existing-key".into())),
        ]
    );
    assert_eq!(migrate_backfill_dedup_keys(&c).unwrap(), 0);
}

#[test]
fn migrate_backfill_dedup_makes_legacy_rows_visible_to_recent_milestone_replay() {
    let c = crate::test_support::mem_db();
    create_session(
        &c,
        "backfill-replay",
        "Backfill replay",
        "local-default",
        "local",
    )
    .unwrap();
    let blocks = [Block::Text {
        text: "legacy".into(),
    }];
    append_message(&c, "backfill-replay", "user", &blocks, None, None, None).unwrap();
    let user_id = c.last_insert_rowid();
    append_message(
        &c,
        "backfill-replay",
        "assistant",
        &blocks,
        None,
        None,
        None,
    )
    .unwrap();
    let assistant_id = c.last_insert_rowid();

    assert!(list_recent_milestone_replay_rows(&c, 20)
        .unwrap()
        .is_empty());
    assert_eq!(migrate_backfill_dedup_keys(&c).unwrap(), 2);

    let rows = list_recent_milestone_replay_rows(&c, 20).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].message_id, user_id);
    assert_eq!(rows[0].role, "user");
    assert_eq!(rows[0].dedup_key, format!("backfill:{user_id}"));
    assert_eq!(rows[1].message_id, assistant_id);
    assert_eq!(rows[1].role, "assistant");
    assert_eq!(rows[1].dedup_key, format!("backfill:{assistant_id}"));
}
