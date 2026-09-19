#![cfg(test)]

use super::*;
#[test]
fn builds_msg_completed_payload() {
    let blocks = serde_json::json!([
        { "type": "text", "text": "done" },
        { "type": "code", "code": "ok" }
    ]);

    let payload = build_msg_completed_payload(42, "assistant", blocks.clone(), None, 1, "raw");
    assert_eq!(
        strip_ref_source(payload),
        serde_json::json!({
            "message_id": 42,
            "role": "assistant",
            "blocks": blocks,
            "revision": 1,
        })
    );
}

/// 显示当前 agent（MA1）：`agent` 为 `Some` 时 payload 插入 `"agent"` 键；`None` 时该键
/// 整个省略（不是 `null`）——保持老消费方（不认识 `agent` 键的旧解析逻辑）向后兼容。
#[test]
fn builds_msg_completed_payload_agent_field_optional() {
    let blocks = serde_json::json!([{ "type": "text", "text": "done" }]);

    let with_agent =
        build_msg_completed_payload(42, "assistant", blocks.clone(), Some("Claude"), 1, "raw");
    assert_eq!(with_agent["agent"], "Claude");
    assert_eq!(
        strip_ref_source(with_agent),
        serde_json::json!({
            "message_id": 42,
            "role": "assistant",
            "blocks": blocks,
            "agent": "Claude",
            "revision": 1,
        })
    );

    let without_agent =
        build_msg_completed_payload(42, "assistant", blocks.clone(), None, 1, "raw");
    assert!(
        without_agent.get("agent").is_none(),
        "agent key must be omitted (not null) when agent is None"
    );
    assert_eq!(
        strip_ref_source(without_agent),
        serde_json::json!({
            "message_id": 42,
            "role": "assistant",
            "blocks": blocks,
            "revision": 1,
        })
    );
}

/// msgfix1 T3（M0 §10.6）：`build_msg_completed_payload` 预算好的 content_ref 必须对
/// `content_raw` 原文字节计算——sha256/total_bytes 与直接对同一字符串计算的结果逐字节一致；
/// 非超预算路径顶层不带 `content_ref`（只在 `enqueue_milestone_item` 判定超预算时才附加）。
#[test]
fn msg_completed_payload_ref_source_matches_content_raw_bytes() {
    let content_raw = r#"[{"type":"text","text":"hello"}]"#;
    let payload = build_msg_completed_payload(
        7,
        "assistant",
        serde_json::json!([{"type": "text", "text": "hello"}]),
        None,
        3,
        content_raw,
    );
    let ref_source = payload
        .get(MSG_COMPLETED_REF_SOURCE_KEY)
        .expect("ref source must be present before stripping");
    assert_eq!(ref_source["message_id"], 7);
    assert_eq!(ref_source["revision"], 3);
    assert_eq!(ref_source["total_bytes"], content_raw.len() as u64);
    assert_eq!(
        ref_source["content_sha256"],
        sha256_hex_lower(content_raw.as_bytes())
    );
    assert!(strip_ref_source(payload).get("content_ref").is_none());
}

#[test]
fn builds_card_created_payload() {
    let block = serde_json::json!({
        "type": "decision_card",
        "decision_id": "decision-1",
        "status": "pending",
    });

    assert_eq!(
        build_card_created_payload(block.clone()),
        serde_json::json!({ "block": block })
    );
}

#[test]
fn builds_card_resolved_payload_including_null_chosen_option() {
    assert_eq!(
        build_card_resolved_payload("decision-1", "resolved", Some("A")),
        serde_json::json!({
            "decision_id": "decision-1",
            "status": "resolved",
            "chosen_option": "A",
        })
    );
    assert_eq!(
        build_card_resolved_payload("decision-1", "dismissed", None),
        serde_json::json!({
            "decision_id": "decision-1",
            "status": "dismissed",
            "chosen_option": null,
        })
    );
}

#[test]
fn builds_running_run_status_payload_with_run_id() {
    assert_eq!(
        build_run_status_payload("session-1", "running", Some("run-1")),
        serde_json::json!({
            "session_id": "session-1",
            "status": "running",
            "run_id": "run-1",
        })
    );
}

#[test]
fn builds_idle_run_status_payload_without_run_id() {
    assert_eq!(
        build_run_status_payload("session-1", "idle", None),
        serde_json::json!({
            "session_id": "session-1",
            "status": "idle",
            "run_id": null,
        })
    );
}

#[test]
fn builds_session_index_created_payload() {
    assert_eq!(
        build_session_index_created_payload("s1", "Title", "repo-1", "local", None),
        serde_json::json!({
            "op": "created",
            "full": false,
            "session": {
                "id": "s1",
                "title": "Title",
                "repo_id": "repo-1",
                "namespace_id": "local",
                "archived": false,
                "repo_name": null,
            },
        })
    );
}

#[test]
fn builds_session_index_created_payload_with_repo_name() {
    assert_eq!(
        build_session_index_created_payload("s1", "Title", "repo-1", "local", Some("Acme Corp")),
        serde_json::json!({
            "op": "created",
            "full": false,
            "session": {
                "id": "s1",
                "title": "Title",
                "repo_id": "repo-1",
                "namespace_id": "local",
                "archived": false,
                "repo_name": "Acme Corp",
            },
        })
    );
}

#[test]
fn builds_session_index_renamed_payload() {
    assert_eq!(
        build_session_index_renamed_payload("s1", "Renamed"),
        serde_json::json!({
            "op": "renamed",
            "full": false,
            "id": "s1",
            "title": "Renamed",
        })
    );
}

#[test]
fn builds_session_index_deleted_payload_without_session_fields() {
    let payload = build_session_index_deleted_payload("s1");
    assert_eq!(
        payload,
        serde_json::json!({ "op": "deleted", "full": false, "id": "s1" })
    );
    assert!(payload.get("title").is_none());
    assert!(payload.get("session").is_none());
}

#[test]
fn builds_session_index_archived_and_unarchived_payloads() {
    let ids = vec!["s1".to_owned(), "s2".to_owned()];
    assert_eq!(
        build_session_index_archived_payload(&ids, true),
        serde_json::json!({
            "op": "archived",
            "full": false,
            "ids": ["s1", "s2"],
        })
    );
    assert_eq!(
        build_session_index_archived_payload(&ids, false),
        serde_json::json!({
            "op": "unarchived",
            "full": false,
            "ids": ["s1", "s2"],
        })
    );
}

#[test]
fn builds_session_index_snapshot_payload_without_op() {
    let sessions = serde_json::json!([{"id": "s1"}, {"id": "s2"}]);
    let payload = build_session_index_snapshot_payload(sessions.clone(), Value::Null);
    assert_eq!(payload["full"], true);
    assert_eq!(payload["sessions"], sessions);
    assert_eq!(payload["repo"], Value::Null);
    assert!(payload.get("op").is_none());
}

#[test]
fn builds_session_index_snapshot_payload_with_repo_summary() {
    let sessions = serde_json::json!([{"id": "s1"}]);
    let repo = serde_json::json!({"id": "repo-1", "name": "Acme Corp"});
    let payload = build_session_index_snapshot_payload(sessions, repo.clone());
    assert_eq!(payload["repo"], repo);
}

// B2（backlog 跟进）：session.index 全量快照发送前尺寸闸。

#[test]
fn truncate_session_index_snapshot_rows_passes_through_unchanged_when_within_budget() {
    let sessions = serde_json::json!([{"id": "s1"}, {"id": "s2"}]);
    let (result, truncated) = truncate_session_index_snapshot_rows(sessions.clone(), 4096);
    assert_eq!(result, sessions);
    assert!(!truncated);
}

#[test]
fn truncate_session_index_snapshot_rows_empty_array_does_not_panic() {
    let (result, truncated) = truncate_session_index_snapshot_rows(serde_json::json!([]), 4096);
    assert_eq!(result, serde_json::json!([]));
    assert!(!truncated);
}

#[test]
fn truncate_session_index_snapshot_rows_drops_tail_rows_when_over_budget() {
    // 每行序列化后约 30 字节（`{"id":"row-N","pad":"..."}`），budget=100 只够放下前几行——
    // 断言：① 结果不超预算；② truncated=true；③ 保留的是排在前面的行（SQL 已按
    // pinned DESC, created_at DESC 排好序，重要行天然在前，这里只需验证"从尾部丢"这个
    // 截断策略本身，不需要真的模拟 pinned/created_at 排序）。
    let rows: Vec<Value> = (0..10)
        .map(|i| serde_json::json!({ "id": format!("row-{i}"), "pad": "xxxxxxxxxx" }))
        .collect();
    let sessions = Value::Array(rows.clone());
    let budget = 100;
    let full_bytes = serde_json::to_vec(&sessions).expect("must serialize").len();
    assert!(
        full_bytes > budget,
        "test fixture must actually exceed budget"
    );

    let (result, truncated) = truncate_session_index_snapshot_rows(sessions, budget);
    assert!(truncated);
    let result_bytes = serde_json::to_vec(&result).expect("must serialize").len();
    assert!(result_bytes <= budget);

    let kept = result.as_array().expect("result must be an array");
    assert!(!kept.is_empty(), "budget must fit at least the first row");
    assert!(
        kept.len() < rows.len(),
        "some tail rows must have been dropped"
    );
    // 保留的行必须是原数组的一个前缀（顺序不变、内容不变），不是任意子集。
    assert_eq!(kept.as_slice(), &rows[..kept.len()]);
}

#[test]
fn truncate_session_index_snapshot_rows_non_array_input_passes_through_unchanged() {
    // 理论不可达（调用方恒传 filter_session_index_snapshot_for_active_repo 的 fail-closed
    // 数组返回值）——防御性：不假设契约不会被破坏，但也不在这里重新发明一次 fail-closed。
    let (result, truncated) = truncate_session_index_snapshot_rows(Value::Null, 10);
    assert_eq!(result, Value::Null);
    assert!(!truncated);
}

#[test]
fn marks_session_index_snapshot_truncated_inserts_key_only_when_truncated() {
    let payload = serde_json::json!({ "full": true, "sessions": [], "repo": null });

    let untruncated = mark_session_index_snapshot_truncated(payload.clone(), false);
    assert!(untruncated.get("truncated").is_none());

    let truncated = mark_session_index_snapshot_truncated(payload, true);
    assert_eq!(truncated["truncated"], Value::Bool(true));
}

#[test]
fn derives_msg_completed_client_msg_id_from_kat() {
    assert_eq!(
        derive_msg_completed_client_msg_id("s-1", "dk-1", 1),
        "73996db9-9424-5e73-acb6-965bf87bfb80"
    );
}

#[test]
fn derives_msg_completed_client_msg_id_revision_matches_kat_vectors() {
    // msgfix1 T5（缺口④）：revision==1 逐字节沿用旧派生（存量零扰动，同上一条钉死的
    // 既有向量）；revision>1 在 name 末尾追加 `|<revision>`，与
    // client-msg-id-derivation-v1.json 里的两条 revision>1 KAT 向量互证——msgfix1 T7 B5：
    // 这两条向量原先在 pending 版样张里，T5 合入正式文件时已一并带过来并删除 pending 版，
    // 这里改指正式文件。
    assert_eq!(
        derive_msg_completed_client_msg_id("s-1", "dk-1", 1),
        "73996db9-9424-5e73-acb6-965bf87bfb80",
        "revision==1 必须与旧向量逐字节相同"
    );
    assert_eq!(
        derive_msg_completed_client_msg_id("s-1", "dk-1", 2),
        "d4d27c2b-e6dd-53b3-b789-e5e747ae9e15",
        "revision==2 必须匹配 name 追加 `|2` 后的 KAT"
    );
    assert_eq!(
        derive_msg_completed_client_msg_id("s-1", "dk-1", 3),
        "5585d9d6-5d34-5023-997c-86ea84dec1b9",
        "revision==3 必须匹配 name 追加 `|3` 后的 KAT"
    );
    assert_ne!(
        derive_msg_completed_client_msg_id("s-1", "dk-1", 2),
        derive_msg_completed_client_msg_id("s-1", "dk-1", 3),
        "不同 revision 必须派生出不同 client_msg_id（各自都是新事件）"
    );
}

#[test]
fn derives_card_created_client_msg_id_from_kat() {
    assert_eq!(
        derive_card_created_client_msg_id("decision-1"),
        "d158145f-8ee9-58aa-a45e-77c5f364a596"
    );
}

#[test]
fn derives_card_resolved_client_msg_id_from_kat() {
    assert_eq!(
        derive_card_resolved_client_msg_id("decision-1", "resolved"),
        "6ac580b3-1b88-5198-b7f5-27808dc027e3"
    );
}
