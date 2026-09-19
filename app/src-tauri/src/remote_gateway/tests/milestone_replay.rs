#![cfg(test)]

use super::*;
#[test]
fn milestone_replay_uses_shared_msg_completed_client_msg_id_derivation() {
    let c = crate::test_support::mem_db();
    crate::db::create_session(&c, "replay-derive", "Replay", "local-default", "local").unwrap();
    crate::db::append_message_dedup(
        &c,
        "replay-derive",
        "assistant",
        &[crate::db::Block::Text {
            text: "complete".into(),
        }],
        None,
        None,
        None,
        "run_flush:derive",
    )
    .unwrap();
    let rows = crate::db::list_recent_milestone_replay_rows(&c, 10).unwrap();
    assert_eq!(rows.len(), 1);
    let expected = derive_msg_completed_client_msg_id("replay-derive", "run_flush:derive", 1);
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(rows.clone()),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    request_session_index_snapshot(&inner, generation);

    let (_, snapshot) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("session.index should precede replay");
    assert_eq!(snapshot.t, "session.index");
    let (item_generation, replay) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("msg.completed replay should be delivered");
    assert_eq!(item_generation, generation);
    assert_eq!(replay.t, "msg.completed");
    assert_eq!(replay.client_msg_id.as_bytes(), expected.as_bytes());
}

#[test]
fn replay_batch_oversized_message_and_live_publish_are_downgraded_to_preview_and_counted() {
    let oversized_text = "x".repeat(SNAPSHOT_SEND_BUDGET_BYTES + 1024);
    // msgfix1 T3 返修 P1-2：raw content 故意手写成非规范形态（多余空白 + 键顺序与
    // `serde_json::to_string` 默认输出相反）——钉死"sha256/total_bytes 必须对 DB
    // 原文字节计算"而非对 content_json 重序列化取哈希；下面额外断言 ref 对"重序列化
    // 后的规范形式"不成立，正反双向锁死这条契约（否则"误改成重序列化 Value 取哈希"
    // 这类回归所有语料都来自同一 Value 的序列化，会全绿放过）。
    let replay_content_raw = format!(
        "[ {{ \"text\":  {text_json} ,  \"type\":\"text\" }} ]",
        text_json = serde_json::to_string(&oversized_text).unwrap()
    );
    let oversized_blocks: Value =
        serde_json::from_str(&replay_content_raw).expect("hand-written raw content must parse");
    let canonical_reserialized = serde_json::to_string(&oversized_blocks).unwrap();
    assert_ne!(
        replay_content_raw, canonical_reserialized,
        "测试语料必须与规范重序列化不同，否则测不出「误用重序列化」这类回归"
    );
    let replay_row = crate::db::MilestoneReplayRow {
        session_id: "replay-oversized".into(),
        message_id: 41,
        role: "assistant".into(),
        content_json: oversized_blocks.clone(),
        content: replay_content_raw.clone(),
        dedup_key: "replay-oversized-dedup".into(),
        revision: 5,
    };
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(vec![replay_row.clone()]),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    // msgfix1 T3（设计稿 §A）：超预算消息不再静默丢弃——降级为 preview + content_ref
    // 后仍然入队送达，`replay_oversized_dropped` 计数器语义改为"降级为 preview 的次数"。
    publish_milestone_replay_batch_on_connect(&inner, generation);
    let (_, replay_item) = milestone_rx
        .try_recv()
        .expect("oversized replay message must be delivered as a preview, not dropped");
    assert_eq!(replay_item.t, "msg.completed");
    assert!(
        milestone_frame_bytes(&replay_item.t, &replay_item.payload) <= SNAPSHOT_SEND_BUDGET_BYTES
    );
    let replay_ref = &replay_item.payload["content_ref"];
    assert_eq!(replay_ref["message_id"], 41);
    assert_eq!(replay_ref["revision"], 5);
    assert_eq!(replay_ref["total_bytes"], replay_content_raw.len() as u64);
    assert_eq!(
        replay_ref["content_sha256"],
        sha256_hex_lower(replay_content_raw.as_bytes())
    );
    // msgfix1 T3 返修 P1-2（反向钉死）：ref 绝不能对"重序列化后的规范形式"成立——
    // 如果生产代码退化成对 content_json 重新序列化取哈希/长度，这两条会立刻转红。
    assert_ne!(
        replay_ref["total_bytes"],
        canonical_reserialized.len() as u64,
        "total_bytes 不能对重序列化后的规范形式成立"
    );
    assert_ne!(
        replay_ref["content_sha256"],
        sha256_hex_lower(canonical_reserialized.as_bytes()),
        "content_sha256 不能对重序列化后的规范形式成立"
    );
    assert!(
        replay_item.payload["blocks"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with(OVERSIZED_PREVIEW_TRUNCATION_NOTICE),
        "preview text must end with the truncation notice"
    );
    assert_eq!(
        inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
        1
    );

    let live_blocks = serde_json::json!([{
        "type": "text",
        "text": "y".repeat(SNAPSHOT_SEND_BUDGET_BYTES + 1024),
    }]);
    let live_content_raw = serde_json::to_string(&live_blocks).unwrap();
    enqueue_milestone_for_upstream(
        &inner.state,
        &inner.milestone_tx,
        MilestoneItem {
            session: Some("live-oversized".into()),
            t: "msg.completed".into(),
            payload: build_msg_completed_payload(
                42,
                "assistant",
                live_blocks,
                None,
                2,
                &live_content_raw,
            ),
            client_msg_id: derive_msg_completed_client_msg_id(
                "live-oversized",
                "live-oversized-dedup",
                2,
            ),
        },
    );
    let (_, live_item) = milestone_rx
        .try_recv()
        .expect("oversized live publish must be delivered as a preview, not dropped");
    assert_eq!(live_item.t, "msg.completed");
    assert!(milestone_frame_bytes(&live_item.t, &live_item.payload) <= SNAPSHOT_SEND_BUDGET_BYTES);
    let live_ref = &live_item.payload["content_ref"];
    assert_eq!(live_ref["message_id"], 42);
    assert_eq!(live_ref["revision"], 2);
    assert_eq!(live_ref["total_bytes"], live_content_raw.len() as u64);
    assert_eq!(
        live_ref["content_sha256"],
        sha256_hex_lower(live_content_raw.as_bytes())
    );
    assert_eq!(
        inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
        2
    );
}

#[test]
fn replay_batch_normal_message_is_enqueued_unchanged() {
    let blocks = serde_json::json!([{"type": "text", "text": "normal replay"}]);
    let replay_row = crate::db::MilestoneReplayRow {
        session_id: "replay-normal".into(),
        message_id: 43,
        role: "assistant".into(),
        content_json: blocks.clone(),
        content: serde_json::to_string(&blocks).unwrap(),
        dedup_key: "replay-normal-dedup".into(),
        revision: 1,
    };
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(vec![replay_row.clone()]),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    publish_milestone_replay_batch_on_connect(&inner, generation);

    let (item_generation, item) = milestone_rx.try_recv().unwrap();
    assert_eq!(item_generation, generation);
    assert_eq!(item.t, "msg.completed");
    assert_eq!(item.payload["blocks"], blocks);
    // msgfix1 T3（M0 §10.6「可选字段」）：非超预算消息顶层不带 content_ref，内部
    // ref-source 私有键也不得泄漏到 wire。
    assert!(item.payload.get("content_ref").is_none());
    assert!(item.payload.get(MSG_COMPLETED_REF_SOURCE_KEY).is_none());
    assert!(milestone_rx.try_recv().is_err());
    assert_eq!(
        inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
        0
    );
}

#[test]
fn replay_batch_tool_output_truncation_makes_message_sendable() {
    let content_json = serde_json::json!([{
        "type": "tool",
        "id": "tool-1",
        "tool": "shell",
        "summary": "ran",
        "card": "command",
        "status": "ok",
        "exit_code": 0,
        "output": "y".repeat(SNAPSHOT_SEND_BUDGET_BYTES + 1024),
    }]);
    let replay_row = crate::db::MilestoneReplayRow {
        session_id: "replay-truncated-tool".into(),
        message_id: 44,
        role: "assistant".into(),
        content_json: content_json.clone(),
        content: serde_json::to_string(&content_json).unwrap(),
        dedup_key: "replay-truncated-tool-dedup".into(),
        revision: 1,
    };
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(vec![replay_row.clone()]),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    publish_milestone_replay_batch_on_connect(&inner, generation);

    let (_, item) = milestone_rx.try_recv().unwrap();
    assert_eq!(item.t, "msg.completed");
    let truncated_output = item.payload["blocks"][0]["output"].as_str().unwrap();
    assert_eq!(truncated_output.len(), OUTPUT_TRUNCATE_BYTES);
    // msgfix1 T7 B2：msg.completed 口截断必须带可见化标记——不能让远端读者以为内容天然
    // 就在这里结束。
    assert!(
        truncated_output.ends_with(TOOL_OUTPUT_TRUNCATION_MARKER),
        "截断的工具输出必须带 {TOOL_OUTPUT_TRUNCATION_MARKER:?} 标记"
    );
    assert!(milestone_frame_bytes(&item.t, &item.payload) <= SNAPSHOT_SEND_BUDGET_BYTES);
    assert_eq!(
        inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
        0
    );
}

#[test]
fn milestone_replay_batch_includes_user_row_role_agnostic_publish() {
    // P0-c 真路径消费测试：`list_recent_milestone_replay_rows` 现在纳入带 dedup_key 的
    // user 行（db.rs 语义反转），这里验证消费方 `publish_milestone_replay_batch_on_connect`
    // 对 role 确实无感——user 行原样走到 msg.completed 补发帧，client_msg_id 推导与
    // assistant 行同一条公式（session_id + dedup_key），不因 role 分叉。
    let c = crate::test_support::mem_db();
    crate::db::create_session(&c, "replay-user", "Replay User", "local-default", "local").unwrap();
    crate::db::append_message_dedup(
        &c,
        "replay-user",
        "user",
        &[crate::db::Block::Text {
            text: "你好".into(),
        }],
        None,
        None,
        None,
        "remote_input:cmd-replay-user",
    )
    .unwrap();
    let rows = crate::db::list_recent_milestone_replay_rows(&c, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].role, "user");
    let expected =
        derive_msg_completed_client_msg_id("replay-user", "remote_input:cmd-replay-user", 1);
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(rows.clone()),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    request_session_index_snapshot(&inner, generation);

    let (_, snapshot) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("session.index should precede replay");
    assert_eq!(snapshot.t, "session.index");
    let (item_generation, replay) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("msg.completed replay should be delivered for a user row");
    assert_eq!(item_generation, generation);
    assert_eq!(replay.t, "msg.completed");
    assert_eq!(replay.payload["role"], "user");
    assert_eq!(replay.client_msg_id.as_bytes(), expected.as_bytes());
}

#[test]
fn connection_snapshot_is_followed_by_replay_rows_in_provider_order() {
    let replay_rows = vec![
        crate::db::MilestoneReplayRow {
            session_id: "s1".into(),
            message_id: 11,
            role: "assistant".into(),
            content_json: serde_json::json!([]),
            content: "[]".into(),
            dedup_key: "d1".into(),
            revision: 1,
        },
        crate::db::MilestoneReplayRow {
            session_id: "s2".into(),
            message_id: 12,
            role: "assistant".into(),
            content_json: serde_json::json!([]),
            content: "[]".into(),
            dedup_key: "d2".into(),
            revision: 1,
        },
    ];
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(replay_rows.clone()),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    request_session_index_snapshot(&inner, generation);

    let mut received = Vec::new();
    for _ in 0..3 {
        received.push(
            milestone_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("snapshot and ordered replay frames should be delivered"),
        );
    }
    assert!(received
        .iter()
        .all(|(item_generation, _)| *item_generation == generation));
    assert_eq!(received[0].1.t, "session.index");
    assert_eq!(received[1].1.t, "msg.completed");
    assert_eq!(received[1].1.payload["message_id"], 11);
    assert_eq!(received[2].1.t, "msg.completed");
    assert_eq!(received[2].1.payload["message_id"], 12);
}

#[test]
fn milestone_replay_rebuilds_resolved_and_pending_decision_cards() {
    let chosen_content_json = serde_json::json!([{
        "type": "decision_card",
        "decision_id": "dc-1",
        "status": "chosen",
        "chosen_option": "A",
        "unknown_future_field": { "preserved": true }
    }]);
    let chosen_row = crate::db::MilestoneReplayRow {
        session_id: "card-session".into(),
        message_id: 21,
        role: "assistant".into(),
        content_json: chosen_content_json.clone(),
        content: serde_json::to_string(&chosen_content_json).unwrap(),
        dedup_key: "card-chosen".into(),
        revision: 1,
    };
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(vec![chosen_row.clone()]),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);
    request_session_index_snapshot(&inner, generation);

    let (_, snapshot) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (_, completed) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (_, created) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (_, resolved) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(snapshot.t, "session.index");
    assert_eq!(completed.t, "msg.completed");
    assert_eq!(created.t, "card.created");
    assert_eq!(
        created.client_msg_id,
        derive_card_created_client_msg_id("dc-1")
    );
    assert_eq!(
        created.payload["block"]["unknown_future_field"]["preserved"],
        true
    );
    assert_eq!(resolved.t, "card.resolved");
    assert_eq!(
        resolved.client_msg_id,
        derive_card_resolved_client_msg_id("dc-1", "chosen")
    );
    assert_eq!(resolved.payload["chosen_option"], "A");

    let pending_content_json = serde_json::json!([{
        "type": "decision_card",
        "decision_id": "dc-2",
        "status": "pending",
        "chosen_option": null
    }]);
    let pending_row = crate::db::MilestoneReplayRow {
        session_id: "card-session".into(),
        message_id: 22,
        role: "assistant".into(),
        content_json: pending_content_json.clone(),
        content: serde_json::to_string(&pending_content_json).unwrap(),
        dedup_key: "card-pending".into(),
        revision: 1,
    };
    let (pending_inner, pending_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(vec![pending_row.clone()]),
        || None,
    );
    let pending_generation = pending_inner.state.advance_generation_and_set_gate(true);
    request_session_index_snapshot(&pending_inner, pending_generation);

    let (_, snapshot) = pending_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (_, completed) = pending_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (_, created) = pending_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(snapshot.t, "session.index");
    assert_eq!(completed.t, "msg.completed");
    assert_eq!(created.t, "card.created");
    assert_eq!(
        created.client_msg_id,
        derive_card_created_client_msg_id("dc-2")
    );
    assert!(
        pending_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "pending card must not emit card.resolved"
    );
}

/// idlefix-T1 缺口②：连接后补发批必须追加 `run.status` 现状帧——手机顶栏唯一数据源就是它，
/// 此前只在状态变化时 publish 一次、连接后补发批没有它，中途接入/错过一帧顶栏就永久卡在
/// Idle。这里断言补发批（session.index 之后）含一帧 `run.status`，值来自
/// `session_runtime_replay_provider`，且 client_msg_id 走确定性推导（不是每次重连都变）。
#[test]
fn milestone_replay_batch_includes_run_status_current_state_frame() {
    let runtime_row = crate::db::SessionRuntimeReplayRow {
        session_id: "runstatus-session".into(),
        status: "running".into(),
        run_id: Some("run-77".into()),
    };
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        || None,
        move || Some(vec![runtime_row.clone()]),
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    request_session_index_snapshot(&inner, generation);

    let (_, snapshot) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("session.index should precede replay");
    assert_eq!(snapshot.t, "session.index");
    let (item_generation, run_status) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("run.status replay frame should be delivered");
    assert_eq!(item_generation, generation);
    assert_eq!(run_status.t, "run.status");
    assert_eq!(run_status.payload["session_id"], "runstatus-session");
    assert_eq!(run_status.payload["status"], "running");
    assert_eq!(run_status.payload["run_id"], "run-77");
    assert_eq!(
        run_status.client_msg_id,
        derive_run_status_replay_client_msg_id("runstatus-session", "running", Some("run-77"))
    );
}

/// 缺口② round-trip：`session_runtime_replay_provider` 读失败（返回 None）不该连累
/// msg.completed/card.* 那半补发——两个 provider 各自 best-effort，互不拖累。
#[test]
fn milestone_replay_batch_msg_completed_survives_run_status_provider_failure() {
    let replay_row = crate::db::MilestoneReplayRow {
        session_id: "runstatus-fail-session".into(),
        message_id: 31,
        role: "assistant".into(),
        content_json: serde_json::json!([]),
        content: "[]".into(),
        dedup_key: "d-runstatus-fail".into(),
        revision: 1,
    };
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        move || Some(vec![replay_row.clone()]),
        || None,
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    request_session_index_snapshot(&inner, generation);

    let (_, snapshot) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (_, completed) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(snapshot.t, "session.index");
    assert_eq!(completed.t, "msg.completed");
    assert!(
        milestone_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "run_status provider 返回 None 时不该有 run.status 帧，但也不该吞掉上面已发的 msg.completed"
    );
}
