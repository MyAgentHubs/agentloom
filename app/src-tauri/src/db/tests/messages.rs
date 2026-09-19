#![cfg(test)]

use super::*;

#[test]
fn append_then_get_blocks_in_order() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text {
            text: "你好".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "你好呀".into(),
        }],
        Some("claude"),
        None,
        None,
    )
    .unwrap();
    let msgs = get_messages(&c, "s1").unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(
        msgs[0].content,
        vec![Block::Text {
            text: "你好".into()
        }]
    );
    assert_eq!(msgs[1].engine, Some("claude".into()));
}

#[test]
fn get_messages_returns_id_and_created_at() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text {
            text: "第一条".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[Block::Text {
            text: "第二条".into(),
        }],
        Some("claude"),
        Some("agent-1"),
        Some("Claude"),
    )
    .unwrap();

    let msgs = get_messages(&c, "s1").unwrap();

    assert_eq!(msgs.len(), 2);
    assert!(msgs[0].id > 0);
    assert_eq!(msgs[1].id, msgs[0].id + 1);
    assert!(msgs[0].created_at > 0);
    assert!(msgs[1].created_at >= msgs[0].created_at);
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].engine.as_deref(), Some("claude"));
    assert_eq!(msgs[1].agent_id.as_deref(), Some("agent-1"));
    assert_eq!(msgs[1].agent_name_snapshot.as_deref(), Some("Claude"));
}

#[test]
fn get_message_by_id_returns_one_message() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text {
            text: "可定位".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let id = get_messages(&c, "s1").unwrap()[0].id;

    let got = get_message_by_id(&c, id).unwrap().unwrap();

    assert_eq!(got.id, id);
    assert_eq!(got.role, "user");
    assert_eq!(
        got.content,
        vec![Block::Text {
            text: "可定位".into()
        }]
    );
    assert!(got.created_at > 0);
}

#[test]
fn get_message_by_id_returns_none_for_missing_id() {
    let c = mem();

    assert!(get_message_by_id(&c, 42).unwrap().is_none());
}

#[test]
fn memory_read_source_reads_message_anchor_text_range() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "assistant",
        &[
            Block::Text {
                text: "第一块".into(),
            },
            Block::Text {
                text: "abcdef".into(),
            },
        ],
        None,
        None,
        None,
    )
    .unwrap();
    let id = get_messages(&c, "s1").unwrap()[0].id;
    let anchor = Anchor {
        kind: "message".into(),
        ref_id: id.to_string(),
        block_index: Some(1),
        char_range: Some([1, 4]),
        line: None,
        label: None,
    };

    let got = memory_read_source(&c, &anchor).unwrap().unwrap();

    assert_eq!(got, "bcd");
}

#[test]
fn memory_read_source_json_accepts_anchor_object() {
    let c = mem();
    create_session(&c, "s1", "测试", "local-default", "local").unwrap();
    append_message(
        &c,
        "s1",
        "user",
        &[Block::Text {
            text: "json 来源".into(),
        }],
        None,
        None,
        None,
    )
    .unwrap();
    let id = get_messages(&c, "s1").unwrap()[0].id;
    let json = format!(r#"{{"kind":"message","ref":{id},"block_index":0}}"#);

    let got = memory_read_source_json(&c, &json).unwrap().unwrap();

    assert_eq!(got, "json 来源");
}

#[test]
fn memory_read_source_json_tolerates_bad_input() {
    let c = mem();

    let got = memory_read_source_json(&c, "not json");

    assert!(got.unwrap().is_none());
}

#[test]
fn memory_read_source_non_message_kind_returns_none() {
    let c = mem();
    let anchor = Anchor {
        kind: "file".into(),
        ref_id: "a.rs".into(),
        block_index: None,
        char_range: None,
        line: None,
        label: None,
    };

    let got = memory_read_source(&c, &anchor).unwrap();

    assert!(got.is_none());
}

#[test]
fn blocks_to_text_joins_text_blocks() {
    let blocks = vec![
        Block::Text {
            text: "第一段".into(),
        },
        Block::Text {
            text: "第二段".into(),
        },
    ];
    assert_eq!(blocks_to_text(&blocks), "第一段\n第二段");
}

#[test]
fn image_block_roundtrips_through_json() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    let blocks = vec![
        Block::Text {
            text: "看图".into(),
        },
        Block::Image {
            attachment_id: "a1".into(),
            media_type: "image/png".into(),
        },
    ];
    append_message(&c, "s1", "user", &blocks, None, None, None).unwrap();
    assert_eq!(get_messages(&c, "s1").unwrap()[0].content, blocks);
}

#[test]
fn tool_and_thinking_blocks_round_trip() {
    let c = mem();
    let blocks = vec![
        Block::Text {
            text: "我来跑命令".into(),
        },
        Block::Thinking {
            text: "先想想步骤".into(),
        },
        Block::Tool {
            id: "t1".into(),
            tool: "Bash".into(),
            summary: "ls".into(),
            card: BlockCardKind::Command,
            status: BlockToolStatus::Failed,
            exit_code: Some(1),
            output: Some("boom".into()),
        },
        Block::Tool {
            id: "t2".into(),
            tool: "Read".into(),
            summary: "a.rs".into(),
            card: BlockCardKind::Compact,
            status: BlockToolStatus::Interrupted,
            exit_code: None,
            output: None,
        },
    ];
    append_message(&c, "s1", "assistant", &blocks, Some("claude"), None, None).unwrap();
    assert_eq!(get_messages(&c, "s1").unwrap()[0].content, blocks);
}

#[test]
fn blocks_to_text_ignores_tool_and_thinking() {
    let blocks = vec![
        Block::Text {
            text: "答案".into(),
        },
        Block::Thinking {
            text: "推理".into(),
        },
        Block::Tool {
            id: "t1".into(),
            tool: "Bash".into(),
            summary: "ls".into(),
            card: BlockCardKind::Command,
            status: BlockToolStatus::Ok,
            exit_code: Some(0),
            output: Some("x".into()),
        },
    ];
    assert_eq!(blocks_to_text(&blocks), "答案");
}
