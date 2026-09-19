#![cfg(test)]

use super::*;

#[test]
fn rename_session_updates_title() {
    let c = mem();
    create_session(&c, "s1", "新会话", "local-default", "local").unwrap();
    rename_session(&c, "s1", "修 typecheck 报错").unwrap();
    let mut stmt = c
        .prepare("SELECT title FROM sessions WHERE id='s1'")
        .unwrap();
    let t: String = stmt.query_row([], |r| r.get(0)).unwrap();
    assert_eq!(t, "修 typecheck 报错");
}

#[test]
fn delete_session_removes_it_and_its_messages() {
    let c = mem();
    create_session(&c, "s1", "A", "local-default", "local").unwrap();
    create_session(&c, "s2", "B", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text { text: "hi".into() }],
        None,
        None,
        None,
    )
    .unwrap();
    insert_run_pending(&c, "s1", "run-1", "codex", "base").unwrap();
    begin_run_commit_intent(&c, "s1", "run-1", "base", "running").unwrap();
    delete_session(&c, "s1").unwrap();
    assert_eq!(get_messages(&c, "s1").unwrap().len(), 0);
    assert!(list_run_commit_intents(&c).unwrap().is_empty());
    // s2 不受影响
    create_session(&c, "s3", "C", "local-default", "local").unwrap();
    assert!(get_messages(&c, "s2").is_ok());
}

#[test]
fn delete_session_removes_memory_blocks() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    conn.execute(
            "INSERT INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local','local','Local',1,0)",
            [],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default','local','local','Local 默认','/tmp/agentloom-memory-block-delete-session','active',0)",
            [],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) VALUES ('s1','T','local-default','local',0)",
            [],
        )
        .unwrap();
    upsert_memory_block(&conn, "s1", "goal", "g", None, Some("app")).unwrap();

    assert!(get_memory_block(&conn, "s1", "goal").unwrap().is_some());

    delete_session(&conn, "s1").unwrap();

    assert!(get_memory_block(&conn, "s1", "goal").unwrap().is_none());
}

#[test]
fn session_flag_columns_exist_and_default_false() {
    let c = mem();
    create_session(&c, "s-flags", "x", "local-default", "local").unwrap();
    // 新列默认值：pinned/unread/archived = 0、archived_at = NULL
    let (pinned, unread, archived, archived_at): (bool, bool, bool, Option<i64>) = c
        .query_row(
            "SELECT pinned, unread, archived, archived_at FROM sessions WHERE id = 's-flags'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!(!pinned);
    assert!(!unread);
    assert!(!archived);
    assert_eq!(archived_at, None);
}

#[test]
fn set_session_flags_update_columns() {
    let c = mem();
    create_session(&c, "s-set", "x", "local-default", "local").unwrap();

    set_session_pinned(&c, "s-set", true).unwrap();
    set_session_unread(&c, "s-set", true).unwrap();
    let (p, u): (bool, bool) = c
        .query_row(
            "SELECT pinned, unread FROM sessions WHERE id = 's-set'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(p);
    assert!(u);

    // archived=true 设 archived_at（非空）
    set_session_archived(&c, "s-set", true).unwrap();
    let (a, at): (bool, Option<i64>) = c
        .query_row(
            "SELECT archived, archived_at FROM sessions WHERE id = 's-set'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(a);
    assert!(at.is_some());

    // archived=false 清 archived_at（NULL）
    set_session_archived(&c, "s-set", false).unwrap();
    let (a2, at2): (bool, Option<i64>) = c
        .query_row(
            "SELECT archived, archived_at FROM sessions WHERE id = 's-set'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(!a2);
    assert_eq!(at2, None);
}

#[test]
fn set_sessions_archived_updates_chain_rows_together() {
    let c = mem();
    create_session(&c, "arch-root", "root", "local-default", "local").unwrap();
    create_session(&c, "arch-child", "child", "local-default", "local").unwrap();
    set_session_parent(&c, "arch-child", Some("arch-root")).unwrap();
    set_session_continued_to(&c, "arch-root", Some("arch-child")).unwrap();
    let ids = vec!["arch-root".to_string(), "arch-child".to_string()];

    set_sessions_archived(&c, &ids, true).unwrap();
    let archived_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sessions
                 WHERE id IN ('arch-root','arch-child')
                   AND archived = 1
                   AND archived_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived_count, 2);

    set_sessions_archived(&c, &ids, false).unwrap();
    let restored_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sessions
                 WHERE id IN ('arch-root','arch-child')
                   AND archived = 0
                   AND archived_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(restored_count, 2);
}

#[test]
fn create_session_publishes_index_only_after_success() {
    let c = mem();
    crate::remote_gateway::test_take_publish_log();

    create_session(&c, "index-create", "Title", "local-default", "local").unwrap();
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["session.index.created"]
    );

    assert!(create_session(&c, "index-create", "Duplicate", "local-default", "local").is_err());
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
}

#[test]
fn create_session_publishes_created_payload_with_repo_name() {
    // 远程控制手机端会话列表副标题要人类可读项目名——created 增量必须带上它，不是只有
    // 全量快照那一路才有（否则手机端在下一次全量快照到达前，新建会话的副标题会短暂空着）。
    // `mem()` 固定 seed 'local-default' repo 的 name 为 '我的项目'。
    let c = mem();
    crate::remote_gateway::test_take_session_index_created_payload_log();

    create_session(
        &c,
        "index-create-repo-name",
        "Title",
        "local-default",
        "local",
    )
    .unwrap();

    let payloads = crate::remote_gateway::test_take_session_index_created_payload_log();
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["session"]["repo_name"], "我的项目");
}

#[test]
fn rename_session_publishes_index_only_for_existing_row() {
    let c = mem();
    create_session(&c, "index-rename", "Before", "local-default", "local").unwrap();
    crate::remote_gateway::test_take_publish_log();

    rename_session(&c, "index-rename", "After").unwrap();
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["session.index.renamed"]
    );

    rename_session(&c, "missing-index-rename", "No row").unwrap();
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
}

#[test]
fn delete_session_publishes_index_only_for_existing_row() {
    let c = mem();
    create_session(&c, "index-delete", "Title", "local-default", "local").unwrap();
    crate::remote_gateway::test_take_publish_log();

    delete_session(&c, "index-delete").unwrap();
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["session.index.deleted"]
    );

    delete_session(&c, "index-delete").unwrap();
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
}

#[test]
fn set_sessions_archived_publishes_one_filtered_batch() {
    let c = mem();
    create_session(&c, "index-archive-1", "One", "local-default", "local").unwrap();
    create_session(&c, "index-archive-2", "Two", "local-default", "local").unwrap();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_session_index_archived_payload_log();

    let ids = vec![
        "index-archive-1".to_owned(),
        "missing-index-archive".to_owned(),
        "index-archive-2".to_owned(),
    ];
    set_sessions_archived(&c, &ids, true).unwrap();
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["session.index.archived"]
    );
    assert_eq!(
        crate::remote_gateway::test_take_session_index_archived_payload_log(),
        vec![serde_json::json!({
            "op": "archived",
            "full": false,
            "ids": ["index-archive-1", "index-archive-2"],
        })]
    );

    set_sessions_archived(&c, &ids, false).unwrap();
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["session.index.unarchived"]
    );
    assert_eq!(
        crate::remote_gateway::test_take_session_index_archived_payload_log(),
        vec![serde_json::json!({
            "op": "unarchived",
            "full": false,
            "ids": ["index-archive-1", "index-archive-2"],
        })]
    );

    set_sessions_archived(&c, &["still-missing".to_owned()], true).unwrap();
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
    assert!(crate::remote_gateway::test_take_session_index_archived_payload_log().is_empty());
}

#[test]
fn repo_archive_helpers_publish_one_batch_of_matching_ids() {
    let c = mem();
    create_session(&c, "repo-index-1", "One", "local-default", "local").unwrap();
    create_session(&c, "repo-index-2", "Two", "local-default", "local").unwrap();
    create_session(&c, "repo-index-3", "Three", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET archived = 1, archived_at = 1 WHERE id = 'repo-index-2'",
        [],
    )
    .unwrap();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_session_index_archived_payload_log();

    assert_eq!(archive_sessions_for_repo(&c, "local-default").unwrap(), 2);
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["session.index.archived"]
    );
    let payloads = crate::remote_gateway::test_take_session_index_archived_payload_log();
    assert_eq!(payloads.len(), 1);
    let mut archived_ids: Vec<&str> = payloads[0]["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    archived_ids.sort_unstable();
    assert_eq!(archived_ids, vec!["repo-index-1", "repo-index-3"]);

    c.execute(
        "UPDATE sessions SET archived = 0, archived_at = NULL WHERE id = 'repo-index-3'",
        [],
    )
    .unwrap();
    assert_eq!(unarchive_sessions_for_repo(&c, "local-default").unwrap(), 2);
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["session.index.unarchived"]
    );
    let payloads = crate::remote_gateway::test_take_session_index_archived_payload_log();
    assert_eq!(payloads.len(), 1);
    let mut unarchived_ids: Vec<&str> = payloads[0]["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    unarchived_ids.sort_unstable();
    assert_eq!(unarchived_ids, vec!["repo-index-1", "repo-index-2"]);

    assert_eq!(archive_sessions_for_repo(&c, "missing-repo").unwrap(), 0);
    assert_eq!(unarchive_sessions_for_repo(&c, "missing-repo").unwrap(), 0);
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
    assert!(crate::remote_gateway::test_take_session_index_archived_payload_log().is_empty());
}

#[test]
fn list_session_index_snapshot_rows_joins_runtime_and_excludes_soft_deleted() {
    let c = mem();
    create_session(&c, "snapshot-running", "Running", "local-default", "local").unwrap();
    create_session(
        &c,
        "snapshot-never-ran",
        "Never ran",
        "local-default",
        "local",
    )
    .unwrap();
    create_session(&c, "snapshot-deleted", "Deleted", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET created_at = CASE id \
                WHEN 'snapshot-running' THEN 101 \
                WHEN 'snapshot-never-ran' THEN 202 \
                ELSE 303 END",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE sessions SET archived = 1, archived_at = 1 WHERE id = 'snapshot-never-ran'",
        [],
    )
    .unwrap();
    set_session_runtime(&c, "snapshot-running", "running", Some("run-1")).unwrap();
    set_session_deleted(&c, "snapshot-deleted").unwrap();

    let rows = list_session_index_snapshot_rows(&c).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.id != "snapshot-deleted"));

    let running = rows
        .iter()
        .find(|row| row.id == "snapshot-running")
        .unwrap();
    assert_eq!(running.title, "Running");
    assert_eq!(running.repo_id.as_deref(), Some("local-default"));
    // `mem()` 固定 seed 'local-default' repo 的 name 为 '我的项目'——快照行必须真的 LEFT
    // JOIN repos 带出这份人类可读项目名，不是只有 repo_id。
    assert_eq!(running.repo_name.as_deref(), Some("我的项目"));
    assert!(!running.archived);
    assert_eq!(running.status.as_deref(), Some("running"));
    assert_eq!(running.run_id.as_deref(), Some("run-1"));
    assert!(running.updated_at >= 101);

    let never_ran = rows
        .iter()
        .find(|row| row.id == "snapshot-never-ran")
        .unwrap();
    assert_eq!(never_ran.title, "Never ran");
    assert_eq!(never_ran.repo_id.as_deref(), Some("local-default"));
    assert_eq!(never_ran.repo_name.as_deref(), Some("我的项目"));
    assert!(never_ran.archived);
    assert_eq!(never_ran.status, None);
    assert_eq!(never_ran.run_id, None);
    assert_eq!(never_ran.updated_at, 202);
}

#[test]
fn list_session_index_snapshot_rows_repo_name_is_none_when_repo_id_is_none() {
    // repo_id 为 None 的会话（理论上不该出现——sessions.repo_id 业务层必绑，见 create_session
    // 头注——但 LEFT JOIN 本身对 NULL repo_id 的行为必须验过，不能只测"有 repo"这一路）。
    let c = mem();
    create_session(&c, "snapshot-no-repo", "No repo", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET repo_id = NULL WHERE id = 'snapshot-no-repo'",
        [],
    )
    .unwrap();

    let rows = list_session_index_snapshot_rows(&c).unwrap();
    let row = rows
        .iter()
        .find(|row| row.id == "snapshot-no-repo")
        .unwrap();
    assert_eq!(row.repo_id, None);
    assert_eq!(row.repo_name, None);
}

#[test]
fn session_index_snapshot_rows_use_latest_user_or_assistant_text_and_activity() {
    let c = mem();
    create_session(&c, "snapshot-preview", "Preview", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            "snapshot-preview",
            "assistant",
            serde_json::json!([{ "type": "text", "text": "older" }]).to_string(),
            100,
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            "snapshot-preview",
            "user",
            serde_json::json!([
                { "type": "thinking", "text": "skip me" },
                { "type": "text", "text": "latest relevant text" },
            ])
            .to_string(),
            200,
        ],
    )
    .unwrap();
    c.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, 'system', ?2, ?3)",
            rusqlite::params![
                "snapshot-preview",
                serde_json::json!([{ "type": "text", "text": "newer system text" }]).to_string(),
                300,
            ],
        )
        .unwrap();

    let rows = list_session_index_snapshot_rows(&c).unwrap();
    let preview = rows
        .iter()
        .find(|row| row.id == "snapshot-preview")
        .unwrap();
    assert_eq!(
        preview.last_msg_preview.as_deref(),
        Some("latest relevant text")
    );
    assert_eq!(preview.last_activity_at, Some(200));
}

#[test]
fn session_index_preview_truncates_at_unicode_char_boundary_and_is_none_without_messages() {
    let c = mem();
    create_session(&c, "snapshot-unicode", "Unicode", "local-default", "local").unwrap();
    create_session(&c, "snapshot-empty", "Empty", "local-default", "local").unwrap();
    let long_text = format!("{}尾", "你".repeat(80));
    c.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, 'assistant', ?2, 400)",
            rusqlite::params![
                "snapshot-unicode",
                serde_json::json!([{ "type": "text", "text": long_text }]).to_string(),
            ],
        )
        .unwrap();

    let rows = list_session_index_snapshot_rows(&c).unwrap();
    let unicode = rows
        .iter()
        .find(|row| row.id == "snapshot-unicode")
        .unwrap();
    let preview = unicode.last_msg_preview.as_deref().unwrap();
    assert_eq!(preview.chars().count(), 80);
    assert_eq!(preview, "你".repeat(80));
    assert_eq!(unicode.last_activity_at, Some(400));

    let empty = rows.iter().find(|row| row.id == "snapshot-empty").unwrap();
    assert_eq!(empty.last_msg_preview, None);
    assert_eq!(empty.last_activity_at, None);
}

#[test]
fn recent_milestone_replay_includes_dedup_user_rows_filters_null_dedup_and_soft_deleted_session() {
    // 两向语料（P0-c 语义反转：user 行不再一律被滤）——
    // 正例：assistant/user 各一条带 dedup_key 都应纳入补发批；
    // 反例：assistant/user 各一条 dedup_key 为 NULL（走 `append_message`，非 dedup 版）
    // 仍应被滤，role 放宽不等于 dedup_key 校验被放松；deleted 会话即便带 dedup_key 也应被滤。
    let c = crate::test_support::mem_db();
    create_session(&c, "replay-live", "Live", "local-default", "local").unwrap();
    create_session(&c, "replay-deleted", "Deleted", "local-default", "local").unwrap();
    let blocks = [Block::Text {
        text: "milestone".into(),
    }];

    // 正例①：assistant + dedup_key 非空 —— 纳入。
    append_message_dedup(
        &c,
        "replay-live",
        "assistant",
        &blocks,
        None,
        None,
        None,
        "keep",
    )
    .unwrap();
    // 正例②：user + dedup_key 非空 —— 纳入（语义反转的核心断言）。
    append_message_dedup(
        &c,
        "replay-live",
        "user",
        &blocks,
        None,
        None,
        None,
        "user-with-dedup",
    )
    .unwrap();
    // 反例①：assistant + dedup_key NULL —— 仍被滤。
    append_message(&c, "replay-live", "assistant", &blocks, None, None, None).unwrap();
    // 反例②：user + dedup_key NULL —— 仍被滤（role 放宽≠dedup_key 校验放松）。
    append_message(&c, "replay-live", "user", &blocks, None, None, None).unwrap();
    // 反例③：assistant + dedup_key 非空但会话已软删 —— 仍被滤。
    append_message_dedup(
        &c,
        "replay-deleted",
        "assistant",
        &blocks,
        None,
        None,
        None,
        "deleted-session",
    )
    .unwrap();
    set_session_deleted(&c, "replay-deleted").unwrap();

    let rows = list_recent_milestone_replay_rows(&c, 20).unwrap();
    assert_eq!(rows.len(), 2, "两条正例都应纳入: {rows:?}");
    assert_eq!(rows[0].session_id, "replay-live");
    assert_eq!(rows[0].role, "assistant");
    assert_eq!(rows[0].dedup_key, "keep");
    assert_eq!(rows[0].content_json, serde_json::json!(blocks));
    assert_eq!(rows[1].session_id, "replay-live");
    assert_eq!(rows[1].role, "user", "user 行带 dedup_key 应被纳入补发批");
    assert_eq!(rows[1].dedup_key, "user-with-dedup");
    assert_eq!(rows[1].content_json, serde_json::json!(blocks));
}

#[test]
fn recent_milestone_replay_limit_returns_newest_rows_oldest_first() {
    let c = crate::test_support::mem_db();
    create_session(&c, "replay-limit", "Limit", "local-default", "local").unwrap();
    for index in 0..5 {
        append_message_dedup(
            &c,
            "replay-limit",
            "assistant",
            &[Block::Text {
                text: format!("message-{index}"),
            }],
            None,
            None,
            None,
            &format!("dedup-{index}"),
        )
        .unwrap();
    }

    let rows = list_recent_milestone_replay_rows(&c, 2).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].dedup_key, "dedup-3");
    assert_eq!(rows[1].dedup_key, "dedup-4");
    assert!(rows[0].message_id < rows[1].message_id);
}

/// idlefix-T1 补针 D：running 行必须排在 LIMIT 截断线之前——造出比
/// `RECENT_MILESTONE_REPLAY_LIMIT` 更多的 idle 会话（挤占空间的"陪跑"），再插入少量
/// running 会话，断言：① 总行数被封顶在同一个常量；② 全部 running 行都活下来、且排在
/// 结果最前面，不会被单纯"先来后到"顺序挤出这批补发帧。
#[test]
fn list_session_runtime_replay_rows_orders_running_first_and_caps_at_shared_limit() {
    let c = mem();
    let running_count = 3_i64;
    let idle_count = RECENT_MILESTONE_REPLAY_LIMIT + 5;
    for i in 0..idle_count {
        let id = format!("idle-{i}");
        create_session(&c, &id, &id, "local-default", "local").unwrap();
        set_session_runtime(&c, &id, "idle", None).unwrap();
    }
    for i in 0..running_count {
        let id = format!("running-{i}");
        create_session(&c, &id, &id, "local-default", "local").unwrap();
        set_session_runtime(&c, &id, "running", Some(&format!("run-{i}"))).unwrap();
    }

    let rows = list_session_runtime_replay_rows(&c).unwrap();

    assert_eq!(
        rows.len() as i64,
        RECENT_MILESTONE_REPLAY_LIMIT,
        "LIMIT 必须封顶在与 msg/card 补发同一个常量，不能让 session_runtime 全表无界涌入"
    );
    let running_rows: Vec<_> = rows.iter().filter(|r| r.status == "running").collect();
    assert_eq!(
        running_rows.len() as i64,
        running_count,
        "running 行不该被挤丢——ORDER BY 必须把它们排到截断线之前"
    );
    for row in rows.iter().take(running_count as usize) {
        assert_eq!(row.status, "running", "running 行必须排在结果最前面");
    }
}

#[test]
fn soft_delete_sets_tombstone_and_restore_clears_it() {
    let c = mem();
    c.execute(
            "INSERT INTO sessions (id,title,repo_id,namespace_id,created_at) VALUES ('s-del','t','local-default','local',100)",
            [],
        )
        .unwrap();

    set_session_deleted(&c, "s-del").unwrap();
    let dat: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id='s-del'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(dat.is_some(), "软删应设 deleted_at 时刻");

    restore_session(&c, "s-del").unwrap();
    let dat2: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id='s-del'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dat2, None, "恢复应清 deleted_at");
}

#[test]
fn list_expired_trashed_sessions_returns_only_past_grace() {
    let c = mem();
    for (id, dat) in [("s-old", "100"), ("s-new", "9000"), ("s-live", "NULL")] {
        c.execute(
                &format!(
                    "INSERT INTO sessions (id,title,repo_id,namespace_id,created_at,deleted_at) VALUES ('{id}','t','local-default','local',1,{dat})"
                ),
                [],
            )
            .unwrap();
    }
    // cutoff=1000:s-old(100<=1000) expired;s-new(9000) not expired;s-live(NULL) not soft-deleted
    let expired = list_expired_trashed_sessions(&c, 1000).unwrap();
    assert_eq!(
        expired,
        vec!["s-old".to_string()],
        "只返 deleted_at<=cutoff 的软删会话"
    );
}

#[test]
fn purge_session_cascades_all_session_scoped_rows() {
    // 🔴 I3 不变量锁(最高风险·漏表=永久孤儿行·codex+opus 双审 I2):delete_session 必须级联清掉
    //    该 session 的全部 18 张 session-scoped 表 + 3 张 artifact-scoped 表。每张各插一行·
    //    purge 后逐张断言归零——未来误删任一 DELETE 行→此测试立刻 FAIL(防回归命根)。
    //    （2026-08-11 M1 修复轮 P2-2：session_runtime 补第 15 张——M1-T1 新增独立运行态镜像表，
    //    同样按 session_id 键，此前漏了级联删。T-4b 再补 remote_inbox 第 16 张。）
    let c = mem();
    c.execute(
            "INSERT INTO sessions (id,title,repo_id,namespace_id,created_at) VALUES ('s-p','t','local-default','local',1)",
            [],
        )
        .unwrap();
    // --- 18 张 session-scoped(键 session_id) ---
    c.execute(
        "INSERT INTO messages (session_id,role,content,created_at) VALUES ('s-p','user','[]',1)",
        [],
    )
    .unwrap();
    let message_id = c.last_insert_rowid();
    c.execute(
            "INSERT INTO member_report_delivery (session_id,message_id,assignment_id) VALUES ('s-p',?1,'a1')",
            [message_id],
        )
        .unwrap();
    c.execute("INSERT INTO attachments (id,session_id,kind,sha256,rel_path,created_at) VALUES ('att-p','s-p','image','sha','p/a.png',1)", []).unwrap();
    c.execute("INSERT INTO memory_blocks (session_id,slot,text,updated_at) VALUES ('s-p','persona','t',1)", []).unwrap();
    c.execute("INSERT INTO memory_entries (session_id,category,text,created_at) VALUES ('s-p','decision','t',1)", []).unwrap();
    c.execute("INSERT INTO run_commits (session_id,run_id,engine,pre_head,state,created_at) VALUES ('s-p','r1','claude','abc123','running',1)", []).unwrap();
    c.execute("INSERT INTO run_commit_intents (session_id,run_id,expected_head,previous_state,created_at) VALUES ('s-p','r1','abc123','running',1)", []).unwrap();
    c.execute("INSERT INTO checkpoint_entries (session_id,run_id,file_path,existed,created_at) VALUES ('s-p','r1','/tmp/file.txt',0,1)", []).unwrap();
    c.execute("INSERT INTO team_run_pending (session_id,run_id,started_at,created_at) VALUES ('s-p','r1',1,1)", []).unwrap();
    c.execute(
        "INSERT INTO decision_ledger (session_id,text,created_at) VALUES ('s-p','d',1)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO lead_loop_state (session_id,updated_at) VALUES ('s-p',1)",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO landing_commits (id,session_id,run_id,pre_head,landed_head,created_at) VALUES ('lc-p','s-p','r1','pre','land',1)", []).unwrap();
    c.execute("INSERT INTO goal_contracts (id,session_id,run_id,goal,lead_participant_id,created_at) VALUES ('gc-p','s-p','r1','g','lead',1)", []).unwrap();
    c.execute("INSERT INTO acceptance_criteria (id,session_id,run_id,task_id,claim,created_at) VALUES ('ac-p','s-p','r1','task1','claim',1)", []).unwrap();
    c.execute("INSERT INTO artifacts (id,session_id,run_id,member_assignment_id,branch,base_sha,created_at) VALUES ('art-p','s-p','r1','a1','branch1','sha1',1)", []).unwrap();
    c.execute(
        "INSERT INTO session_agent_configs (session_id) VALUES ('s-p')",
        [],
    )
    .unwrap();
    c.execute(
            "INSERT INTO session_runtime (session_id,status,run_id,updated_at) VALUES ('s-p','running','r1',1)",
            [],
        )
        .unwrap();
    c.execute(
        "INSERT INTO remote_inbox (session_id,command_id,kind,payload,created_at) \
             VALUES ('s-p','cmd-p','input.send','{}',1)",
        [],
    )
    .unwrap();
    // --- 3 张 artifact-scoped(键 artifact_id='art-p'·delete_session 经收集 artifact id 级联删) ---
    c.execute("INSERT INTO verifications (id,artifact_id,cmd,artifact_sha,created_at) VALUES ('v-p','art-p','cargo test','sha',1)", []).unwrap();
    c.execute("INSERT INTO reviews (id,artifact_id,reviewer_agent,created_at) VALUES ('rv-p','art-p','codex',1)", []).unwrap();
    c.execute("INSERT INTO merge_candidates (id,artifact_id,staging_branch,created_at) VALUES ('mc-p','art-p','agentloom/staging',1)", []).unwrap();

    // m1(opus 复核加固):关 FK 再 purge——证明级联是显式逐表 DELETE 自洽·不靠 FK CASCADE
    // (I3 立论:生产不保证 PRAGMA foreign_keys=ON)。否则 session_agent_configs(唯一带
    // ON DELETE CASCADE 的表)的断言会被删 sessions 主行的 FK CASCADE 遮蔽·抓不到其显式 DELETE 回归。
    // PRAGMA 须在事务外设(SQLite 事务内改 foreign_keys 是 no-op)·delete_session 的 tx 继承此 OFF。
    c.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();

    delete_session(&c, "s-p").unwrap();

    // 逐张断言归零(session-scoped 按 session_id·artifact-scoped 按 artifact_id)。
    let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
    for (label, sql) in [
        ("sessions", "SELECT COUNT(*) FROM sessions WHERE id='s-p'"),
        (
            "messages",
            "SELECT COUNT(*) FROM messages WHERE session_id='s-p'",
        ),
        (
            "member_report_delivery",
            "SELECT COUNT(*) FROM member_report_delivery WHERE session_id='s-p'",
        ),
        (
            "attachments",
            "SELECT COUNT(*) FROM attachments WHERE session_id='s-p'",
        ),
        (
            "memory_blocks",
            "SELECT COUNT(*) FROM memory_blocks WHERE session_id='s-p'",
        ),
        (
            "memory_entries",
            "SELECT COUNT(*) FROM memory_entries WHERE session_id='s-p'",
        ),
        (
            "run_commits",
            "SELECT COUNT(*) FROM run_commits WHERE session_id='s-p'",
        ),
        (
            "run_commit_intents",
            "SELECT COUNT(*) FROM run_commit_intents WHERE session_id='s-p'",
        ),
        (
            "checkpoint_entries",
            "SELECT COUNT(*) FROM checkpoint_entries WHERE session_id='s-p'",
        ),
        (
            "team_run_pending",
            "SELECT COUNT(*) FROM team_run_pending WHERE session_id='s-p'",
        ),
        (
            "decision_ledger",
            "SELECT COUNT(*) FROM decision_ledger WHERE session_id='s-p'",
        ),
        (
            "lead_loop_state",
            "SELECT COUNT(*) FROM lead_loop_state WHERE session_id='s-p'",
        ),
        (
            "landing_commits",
            "SELECT COUNT(*) FROM landing_commits WHERE session_id='s-p'",
        ),
        (
            "goal_contracts",
            "SELECT COUNT(*) FROM goal_contracts WHERE session_id='s-p'",
        ),
        (
            "acceptance_criteria",
            "SELECT COUNT(*) FROM acceptance_criteria WHERE session_id='s-p'",
        ),
        (
            "artifacts",
            "SELECT COUNT(*) FROM artifacts WHERE session_id='s-p'",
        ),
        (
            "session_agent_configs",
            "SELECT COUNT(*) FROM session_agent_configs WHERE session_id='s-p'",
        ),
        (
            "session_runtime",
            "SELECT COUNT(*) FROM session_runtime WHERE session_id='s-p'",
        ),
        (
            "remote_inbox",
            "SELECT COUNT(*) FROM remote_inbox WHERE session_id='s-p'",
        ),
        (
            "verifications",
            "SELECT COUNT(*) FROM verifications WHERE artifact_id='art-p'",
        ),
        (
            "reviews",
            "SELECT COUNT(*) FROM reviews WHERE artifact_id='art-p'",
        ),
        (
            "merge_candidates",
            "SELECT COUNT(*) FROM merge_candidates WHERE artifact_id='art-p'",
        ),
    ] {
        assert_eq!(count(sql), 0, "purge 应级联清掉 {label} 行(漏表=永久孤儿)");
    }
}
