#![cfg(test)]

use super::*;
#[test]
fn build_oversized_preview_blocks_truncates_first_text_block_and_appends_notice() {
    let text = "x".repeat(OVERSIZED_PREVIEW_TEXT_HEAD_BYTES + 200);
    let blocks = serde_json::json!([{"type": "text", "text": text}]);
    let preview = build_oversized_preview_blocks(&blocks);
    let array = preview.as_array().unwrap();
    assert_eq!(array.len(), 1, "全 text 超限退化为单条合并文本块");
    let preview_text = array[0]["text"].as_str().unwrap();
    assert!(preview_text.ends_with(OVERSIZED_PREVIEW_TRUNCATION_NOTICE));
    let head = &preview_text[..preview_text.len() - OVERSIZED_PREVIEW_TRUNCATION_NOTICE.len()];
    assert_eq!(head.len(), OVERSIZED_PREVIEW_TEXT_HEAD_BYTES);
    assert_eq!(head, "x".repeat(OVERSIZED_PREVIEW_TEXT_HEAD_BYTES));
}

#[test]
fn build_oversized_preview_blocks_respects_utf8_char_boundary_at_cutoff() {
    // 边界恰好落在一个多字节字符中间：截断必须回退到上一个合法字符边界，不panic、不产出
    // 非法 UTF-8。
    let head = "a".repeat(OVERSIZED_PREVIEW_TEXT_HEAD_BYTES - 1);
    let text = format!("{head}界多字节收尾");
    let blocks = serde_json::json!([{"type": "text", "text": text}]);
    let preview = build_oversized_preview_blocks(&blocks);
    let preview_text = preview[0]["text"].as_str().unwrap();
    assert!(preview_text.is_char_boundary(preview_text.len()));
    assert!(preview_text.ends_with(OVERSIZED_PREVIEW_TRUNCATION_NOTICE));
}

#[test]
fn build_oversized_preview_blocks_never_splits_a_surrogate_pair_astral_char() {
    // "😀"（U+1F600）在 UTF-16 里是一对代理项；Rust `str` 是合法 UTF-8，`truncate_utf8`
    // 按 `is_char_boundary` 回退，天然不可能只保留半个标量值——这里验证端到端结果：
    // 截断点前恰好卡在这个 4 字节表情前时，表情要么完整保留、要么整体不出现，两种都合法，
    // 唯独不允许出现在 JSON 序列化/反序列化时产生非法字符串（若劈开会在这一步直接 panic
    // 或产出替换字符，测试改用「输出恒是合法 UTF-8 且不含 U+FFFD」来钉死）。
    let head = "a".repeat(OVERSIZED_PREVIEW_TEXT_HEAD_BYTES - 2);
    let text = format!("{head}😀tail-after-emoji");
    let blocks = serde_json::json!([{"type": "text", "text": text}]);
    let preview = build_oversized_preview_blocks(&blocks);
    let preview_text = preview[0]["text"].as_str().unwrap();
    assert!(
        !preview_text.contains('\u{FFFD}'),
        "不得产出 UTF-8 替换字符"
    );
    // 序列化/反序列化往返验证输出是合法 UTF-8 JSON 字符串。
    let round_tripped: String =
        serde_json::from_value(serde_json::Value::String(preview_text.to_owned())).unwrap();
    assert_eq!(round_tripped, preview_text);
}

#[test]
fn build_oversized_preview_blocks_keeps_actionable_blocks_verbatim() {
    let decision_card = serde_json::json!({
        "type": "decision_card",
        "decision_id": "dc-1",
        "kind": "ask",
        "question": "deploy?",
        "options": ["yes", "no"],
        "status": "pending",
    });
    let approval = serde_json::json!({
        "type": "approval",
        "approval_id": "ap-1",
        "status": "pending",
    });
    let huge_text = "z".repeat(OVERSIZED_PREVIEW_TEXT_HEAD_BYTES + 999);
    let blocks = serde_json::json!([
        decision_card.clone(),
        {"type": "text", "text": huge_text},
        approval.clone(),
        {"type": "tool", "id": "t1", "output": "dropped"},
    ]);
    let preview = build_oversized_preview_blocks(&blocks);
    let array = preview.as_array().unwrap();
    // actionable 两块原样保留 + 一条合并文本块，tool 块整体丢弃。
    assert_eq!(array.len(), 3);
    assert_eq!(array[0], decision_card);
    assert_eq!(array[1], approval);
    assert_eq!(array[2]["type"], "text");
    assert!(array[2]["text"]
        .as_str()
        .unwrap()
        .ends_with(OVERSIZED_PREVIEW_TRUNCATION_NOTICE));
    assert!(array.iter().all(|block| block["type"] != "tool"));
}

#[test]
fn build_oversized_preview_blocks_on_no_text_block_still_yields_notice_only() {
    let blocks = serde_json::json!([{"type": "tool", "id": "t1", "output": "dropped"}]);
    let preview = build_oversized_preview_blocks(&blocks);
    let array = preview.as_array().unwrap();
    assert_eq!(array.len(), 1);
    assert_eq!(array[0]["text"], OVERSIZED_PREVIEW_TRUNCATION_NOTICE);
}

#[test]
fn downgrade_to_preview_payload_falls_back_to_notice_only_when_actionable_blocks_still_overflow() {
    // 构造后整帧仍须过预算（设计稿 §A）：即便只剩 actionable 块，若它们本身巨大到超预算，
    // 必须进一步退化为"仅提示块 + content_ref"——ref 永不丢。
    let huge_question = "q".repeat(SNAPSHOT_SEND_BUDGET_BYTES + 1024);
    let payload = serde_json::json!({
        "message_id": 5,
        "role": "assistant",
        "blocks": [{
            "type": "decision_card",
            "decision_id": "dc-huge",
            "kind": "ask",
            "question": huge_question,
            "options": ["yes", "no"],
            "status": "pending",
        }],
    });
    let content_ref = build_content_ref(5, 1, "raw");
    let degraded = downgrade_to_preview_payload(&payload, content_ref.clone(), "msg.completed");
    assert!(milestone_frame_bytes("msg.completed", &degraded) <= SNAPSHOT_SEND_BUDGET_BYTES);
    assert_eq!(degraded["content_ref"], content_ref, "ref 永不丢");
    let blocks = degraded["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["text"], OVERSIZED_PREVIEW_TRUNCATION_NOTICE);
}

/// msgfix1 T3 返修 P1-1：history 口的同一变异（巨型 actionable 块本身就超预算）此前不
/// 转红——`build_history_page_with_limit` 的二次退化在返修前直接丢行（P0-1），只有
/// msg.completed 口的 `downgrade_to_preview_payload_falls_back_to_notice_only_when_
/// actionable_blocks_still_overflow` 能抓到这个变异。这里走真实 history 分页函数端到端
/// 验证：巨型 decision_card 单独就超 `HISTORY_SEND_BUDGET_BYTES`，preview（原样保留
/// actionable 块）仍超预算，必须退化到"仅提示块 + content_ref"而不是从页面里消失。
#[test]
fn history_page_downgrades_oversized_actionable_only_message_to_notice_only_with_ref() {
    let huge_question = "q".repeat(HISTORY_SEND_BUDGET_BYTES + 1024);
    let content_json = serde_json::json!([{
        "type": "decision_card",
        "decision_id": "dc-huge-history",
        "kind": "ask",
        "question": huge_question,
        "options": ["yes", "no"],
        "status": "pending",
    }]);
    let content_raw = serde_json::to_string(&content_json).unwrap();
    let row = SessionHistoryRow {
        message_id: 61,
        role: "assistant".to_owned(),
        content_json,
        content_raw: content_raw.clone(),
        revision: 2,
    };
    let page = build_history_page("history-huge-actionable", None, vec![row]);
    assert_eq!(
        page.oversized_dropped, 1,
        "巨型消息必须计入降级计数，不是被悄悄跳过"
    );
    let messages = page.payload["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "巨型 actionable 消息必须仍出现在页面里，不能因为 preview 仍超预算就消失"
    );
    assert_eq!(messages[0]["message_id"], 61);
    // preview（保留 decision_card 原样）本身就超预算，必须已经退化到仅提示块——
    // decision_card 不应该出现在最终 blocks 里。
    let blocks = messages[0]["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], OVERSIZED_PREVIEW_TRUNCATION_NOTICE);
    assert!(
        !blocks.iter().any(|block| block["type"] == "decision_card"),
        "二次退化后 actionable 块也不应再出现"
    );
    let content_ref = &messages[0]["content_ref"];
    assert_eq!(content_ref["message_id"], 61, "ref 永不丢");
    assert_eq!(content_ref["revision"], 2);
    assert_eq!(content_ref["total_bytes"], content_raw.len() as u64);
    assert_eq!(
        content_ref["content_sha256"],
        sha256_hex_lower(content_raw.as_bytes())
    );
    assert!(
        serde_json::to_vec(&page.payload).unwrap().len() <= HISTORY_SEND_BUDGET_BYTES,
        "退化后的页面必须自身也过预算"
    );
}
