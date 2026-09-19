#![cfg(test)]

use super::*;

#[test]
fn memory_entries_insert_and_list_active() {
    // 同会话插 3 条（2 decision + 1 pitfall·无 supersede）→ list(false) 返 3 条·id 升序。
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let id1 = insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "用 SQLite",
        "[]",
        "[]",
        None,
        None,
        false,
    )
    .unwrap();
    let id2 = insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "选 Rust",
        "[]",
        "[]",
        None,
        Some("high"),
        false,
    )
    .unwrap();
    let id3 = insert_memory_entry(
        &conn,
        "s1",
        "pitfall",
        "避免 unwrap 在生产",
        "[]",
        "[]",
        None,
        None,
        false,
    )
    .unwrap();
    let entries = list_memory_entries(&conn, "s1", false).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].id, id1);
    assert_eq!(entries[0].category, "decision");
    assert_eq!(entries[1].id, id2);
    assert_eq!(entries[2].id, id3);
    assert_eq!(entries[2].category, "pitfall");
}

#[test]
fn memory_entries_supersede_hides_old() {
    // 插 A → 插 B（supersedes=[id_A]）→ list(false)=只有 B；list(true)=A+B 都在。
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let id_a = insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "旧决策",
        "[]",
        "[]",
        None,
        None,
        false,
    )
    .unwrap();
    let supersedes = format!("[{id_a}]");
    let id_b = insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "新决策",
        "[]",
        &supersedes,
        None,
        None,
        false,
    )
    .unwrap();
    let active = list_memory_entries(&conn, "s1", false).unwrap();
    assert_eq!(active.len(), 1, "应只有活的行 B");
    assert_eq!(active[0].id, id_b);
    let all = list_memory_entries(&conn, "s1", true).unwrap();
    assert_eq!(all.len(), 2, "全量查询应含 A + B");
}

#[test]
fn memory_entries_invalid_json_rejected() {
    // source_refs_json 非法 → Err；合法 [] → Ok。
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let bad = insert_memory_entry(
        &conn, "s1", "risk", "测试", "not json", "[]", None, None, false,
    );
    let source_err = bad.unwrap_err().to_string();
    assert!(
        source_err.contains("AL_ERR:db.memory.badJson")
            && source_err.contains(r#""field":"source_refs_json""#),
        "source_refs_json 非法 JSON 应按 code 拒绝：{source_err}"
    );
    let supersedes_err = insert_memory_entry(
        &conn, "s1", "risk", "测试", "[]", "not json", None, None, false,
    )
    .unwrap_err()
    .to_string();
    assert!(
        supersedes_err.contains("AL_ERR:db.memory.badJson")
            && supersedes_err.contains(r#""field":"supersedes_json""#),
        "supersedes_json 非法 JSON 应按 code 拒绝：{supersedes_err}"
    );
    let good = insert_memory_entry(&conn, "s1", "risk", "测试", "[]", "[]", None, None, false);
    assert!(good.is_ok());
}

#[test]
fn memory_entries_pinned_roundtrip() {
    // pinned=true 插入 → list 读回 pinned==true。
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    insert_memory_entry(
        &conn,
        "s1",
        "watch",
        "关键观察",
        "[]",
        "[]",
        None,
        None,
        true,
    )
    .unwrap();
    let entries = list_memory_entries(&conn, "s1", false).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].pinned, "pinned 应读回 true");
}

#[test]
fn memory_entries_session_isolation() {
    // 两 session 各插条目·list(s1) 不含 s2 的行。
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "s1 决策",
        "[]",
        "[]",
        None,
        None,
        false,
    )
    .unwrap();
    insert_memory_entry(
        &conn,
        "s2",
        "decision",
        "s2 决策",
        "[]",
        "[]",
        None,
        None,
        false,
    )
    .unwrap();
    let s1 = list_memory_entries(&conn, "s1", false).unwrap();
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].session_id, "s1");
    let s2 = list_memory_entries(&conn, "s2", false).unwrap();
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].session_id, "s2");
}

#[test]
fn memory_entries_active_query_tolerates_null_in_supersedes() {
    // FIX 1a: null in supersedes_json must not zero-out the entire result.
    // A superseded by B (which has [id_a, null]); C has supersedes=[null] only -> C still active.
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let id_a = insert_memory_entry(
        &conn, "s1", "decision", "old A", "[]", "[]", None, None, false,
    )
    .unwrap();
    let supersedes_b = format!("[{id_a},null]");
    let id_b = insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "new B",
        "[]",
        &supersedes_b,
        None,
        None,
        false,
    )
    .unwrap();
    let id_c = insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "C with null supersedes",
        "[]",
        "[null]",
        None,
        None,
        false,
    )
    .unwrap();
    let active = list_memory_entries(&conn, "s1", false).unwrap();
    assert!(
        !active.is_empty(),
        "list(false) must not return 0 rows when supersedes_json contains null"
    );
    assert_eq!(active.len(), 2, "should have B and C active");
    let ids: Vec<i64> = active.iter().map(|e| e.id).collect();
    assert!(ids.contains(&id_b), "B must be active");
    assert!(
        ids.contains(&id_c),
        "C must be active (null-only supersedes hides nothing)"
    );
    assert!(!ids.contains(&id_a), "A must be superseded");
}

#[test]
fn memory_entries_active_query_ignores_backward_supersede() {
    // FIX 1b: an earlier row cannot supersede a later row.
    // X is inserted first with supersedes=[2] (id=2 does not exist yet -> forward reference mistake).
    // Y is inserted after X. list(false) must contain Y.
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    // Insert X with a supersedes that refers to a future id (backward kill attempt).
    let id_x = insert_memory_entry(
        &conn, "s1", "decision", "X early", "[]", "[2]", None, None, false,
    )
    .unwrap();
    let supersedes_y = "[]".to_string();
    let id_y = insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "Y later",
        "[]",
        &supersedes_y,
        None,
        None,
        false,
    )
    .unwrap();
    // id_y should be id_x + 1
    assert_eq!(id_y, id_x + 1);
    let active = list_memory_entries(&conn, "s1", false).unwrap();
    let ids: Vec<i64> = active.iter().map(|e| e.id).collect();
    assert!(
        ids.contains(&id_y),
        "Y must be in active list; earlier X cannot supersede later Y"
    );
}

#[test]
fn delete_session_removes_memory_entries() {
    // FIX 2: delete_session must also clear memory_entries rows.
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    conn.execute(
            "INSERT INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local','local','Local',1,0)",
            [],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default','local','local','Local 默认','/tmp/agentloom-delete-entries-test','active',0)",
            [],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) VALUES ('s1','T','local-default','local',0)",
            [],
        )
        .unwrap();
    insert_memory_entry(
        &conn,
        "s1",
        "decision",
        "test entry",
        "[]",
        "[]",
        None,
        None,
        false,
    )
    .unwrap();
    // Confirm entry exists before delete
    let before = list_memory_entries(&conn, "s1", true).unwrap();
    assert_eq!(before.len(), 1);
    delete_session(&conn, "s1").unwrap();
    // After delete_session, memory_entries for s1 must be empty
    let after = list_memory_entries(&conn, "s1", true).unwrap();
    assert!(
        after.is_empty(),
        "delete_session must remove memory_entries rows"
    );
}

#[test]
fn memory_read_source_thinking_block_returns_text() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[Block::Thinking {
            text: "想一想 abc".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let id = get_messages(&c, "s1").unwrap()[0].id;
    let anchor = Anchor {
        kind: "message".into(),
        ref_id: id.to_string(),
        block_index: Some(0),
        char_range: None,
        line: None,
        label: None,
    };
    let got = memory_read_source(&c, &anchor).unwrap().unwrap();
    assert_eq!(got, "想一想 abc");
}

#[test]
fn memory_read_source_non_text_block_returns_none() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[Block::Tool {
            id: "t1".into(),
            tool: "bash".into(),
            summary: "ran it".into(),
            card: BlockCardKind::Command,
            status: BlockToolStatus::Ok,
            exit_code: Some(0),
            output: None,
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let id = get_messages(&c, "s1").unwrap()[0].id;
    let anchor = Anchor {
        kind: "message".into(),
        ref_id: id.to_string(),
        block_index: Some(0),
        char_range: None,
        line: None,
        label: None,
    };
    let got = memory_read_source(&c, &anchor).unwrap();
    assert!(got.is_none());
}

#[test]
fn memory_read_source_non_numeric_ref_returns_none() {
    let c = mem();
    let anchor = Anchor {
        kind: "message".into(),
        ref_id: "abc".into(),
        block_index: None,
        char_range: None,
        line: None,
        label: None,
    };
    let got = memory_read_source(&c, &anchor).unwrap();
    assert!(got.is_none());
}

#[test]
fn memory_read_source_full_text_when_no_char_range() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "完整内容".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let id = get_messages(&c, "s1").unwrap()[0].id;
    let anchor = Anchor {
        kind: "message".into(),
        ref_id: id.to_string(),
        block_index: Some(0),
        char_range: None,
        line: None,
        label: None,
    };
    let got = memory_read_source(&c, &anchor).unwrap().unwrap();
    assert_eq!(got, "完整内容");
}

#[test]
fn memory_read_source_json_skips_malformed_anchor_in_array() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text {
            text: "好消息".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let id = get_messages(&c, "s1").unwrap()[0].id;
    // 数组：第一个合法锚，第二个 block_index 类型错（字符串）
    let json = format!(
        r#"[{{"kind":"message","ref":{id},"block_index":0}},{{"kind":"message","ref":"x","block_index":"bad"}}]"#
    );
    let got = memory_read_source_json(&c, &json).unwrap();
    assert!(got.is_some());
    assert!(got.unwrap().contains("好消息"));
}
