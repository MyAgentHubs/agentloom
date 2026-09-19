#![cfg(test)]

use super::*;
#[test]
fn classifies_supported_live_variants() {
    use crate::agent_event::AgentEvent;

    let cases = [
        (
            AgentEvent::TextDelta {
                text: "hello".to_owned(),
            },
            11,
            (
                "live",
                serde_json::json!({"t": "text_delta", "seq": 11, "text": "hello"}),
            ),
        ),
        (
            AgentEvent::ThinkingDelta {
                text: "hmm".to_owned(),
            },
            12,
            (
                "live",
                serde_json::json!({"t": "thinking_delta", "seq": 12, "text": "hmm"}),
            ),
        ),
        (
            AgentEvent::ToolOutputDelta {
                id: "tool-1".to_owned(),
                text: "chunk".to_owned(),
            },
            13,
            (
                "live",
                serde_json::json!({
                    "t": "tool_output_delta",
                    "seq": 13,
                    "id": "tool-1",
                    "text": "chunk",
                }),
            ),
        ),
        (
            AgentEvent::UsageDelta {
                input_tokens: Some(21),
                output_tokens: None,
            },
            14,
            (
                "live",
                serde_json::json!({
                    "t": "usage_delta",
                    "seq": 14,
                    "input_tokens": 21,
                    "output_tokens": null,
                }),
            ),
        ),
    ];

    for (event, seq, expected) in cases {
        assert_eq!(classify(&event, seq), Some(expected));
    }
}

#[test]
fn classify_truncates_oversized_live_text_to_output_cap() {
    use crate::agent_event::AgentEvent;

    let oversized = "x".repeat(OUTPUT_TRUNCATE_BYTES + 17);
    let events = [
        AgentEvent::TextDelta {
            text: oversized.clone(),
        },
        AgentEvent::ThinkingDelta {
            text: oversized.clone(),
        },
        AgentEvent::ToolOutputDelta {
            id: "tool-1".to_owned(),
            text: oversized,
        },
    ];

    for event in events {
        let (_, value) = classify(&event, 1).expect("live delta should be classified");
        assert_eq!(
            value["text"]
                .as_str()
                .expect("classified live delta should have text")
                .len(),
            OUTPUT_TRUNCATE_BYTES
        );
    }
}

#[test]
fn truncate_utf8_respects_byte_limit_and_character_boundary() {
    let oversized = "x".repeat(OUTPUT_TRUNCATE_BYTES + 17);
    assert_eq!(
        truncate_utf8(&oversized, OUTPUT_TRUNCATE_BYTES).len(),
        OUTPUT_TRUNCATE_BYTES
    );

    let boundary = format!("{}界", "a".repeat(OUTPUT_TRUNCATE_BYTES - 1));
    let truncated = truncate_utf8(&boundary, OUTPUT_TRUNCATE_BYTES);
    assert_eq!(truncated.len(), OUTPUT_TRUNCATE_BYTES - 1);
    assert_eq!(truncated, "a".repeat(OUTPUT_TRUNCATE_BYTES - 1));
}

#[test]
fn truncate_utf8_with_marker_appends_marker_only_when_truncation_actually_happens() {
    // msgfix1 T7 B2：真正发生截断时，总字节数恒 ≤ max_bytes（标记计入预算之内，不会把
    // 消息顶超），且结果以固定标记收尾。
    let oversized = "x".repeat(OUTPUT_TRUNCATE_BYTES + 17);
    let truncated = truncate_utf8_with_marker(&oversized, OUTPUT_TRUNCATE_BYTES);
    assert!(truncated.len() <= OUTPUT_TRUNCATE_BYTES);
    assert!(truncated.ends_with(TOOL_OUTPUT_TRUNCATION_MARKER));
    assert!(
        truncated
            .starts_with(&"x".repeat(OUTPUT_TRUNCATE_BYTES - TOOL_OUTPUT_TRUNCATION_MARKER.len())),
        "正文应保留腾出标记空间后的最大前缀"
    );

    // 未截断（原文本本就不超预算）时原样返回，不附加标记。
    let short = "well under the cap";
    assert_eq!(
        truncate_utf8_with_marker(short, OUTPUT_TRUNCATE_BYTES),
        short
    );
    assert!(!truncate_utf8_with_marker(short, OUTPUT_TRUNCATE_BYTES)
        .contains(TOOL_OUTPUT_TRUNCATION_MARKER));
}

// ========================================================================================
// msgfix1 T3（设计稿 §A）：超限消息块级 preview + content_ref 纯函数覆盖。
// ========================================================================================

#[test]
fn is_actionable_block_type_matches_approval_decision_card_and_scope_change_only() {
    assert!(is_actionable_block_type("approval"));
    assert!(is_actionable_block_type("decision_card"));
    // msgfix1 T3 返修 P0-1：scope_change 来自 NeedsDecision、UI 有「接受并继续」用户
    // 行动，语义上与 approval/decision_card 同级，必须一起判定为 actionable。
    assert!(is_actionable_block_type("scope_change"));
    for benign in [
        "text",
        "image",
        "thinking",
        "tool",
        "run_card",
        "team_run",
        "dispatch_card",
        "lead_summary",
        "coding_task",
        "context_compacted",
        "context_truncated",
        "run_terminal",
    ] {
        assert!(
            !is_actionable_block_type(benign),
            "{benign} 不应判定为 actionable"
        );
    }
}

#[test]
fn event_joins_l1_aggregation_rejects_actionable_types_at_runtime_not_just_debug_assert() {
    // msgfix2 U1 修单三（G2·独立审查残余 P1）：反向测试——`event_joins_l1_aggregation`
    // 是 `extract_tool_milestones` 里真正被 `if` 调用、决定分支行为的单点函数（不是只在
    // debug 构建下才存在的 `debug_assert!`）。actionable 类型必须被这个函数在运行时真实
    // 拒收（返回 false）；非 actionable 类型必须放行（返回 true）——release 构建下语义
    // 同样成立，因为这不是断言，是一次普通函数调用的返回值。
    for actionable in ["approval", "decision_card", "scope_change"] {
        assert!(
            !event_joins_l1_aggregation(actionable),
            "actionable 类型 {actionable} 必须被单点函数拒收（不允许并入 L1）"
        );
    }
    for benign in ["text", "tool", "thinking", "run_card", "unknown_block_type"] {
        assert!(
            event_joins_l1_aggregation(benign),
            "非 actionable 类型 {benign} 必须被单点函数放行"
        );
    }
}

#[test]
fn skips_agent_event_variants_outside_the_upstream_catalog() {
    use crate::agent_event::{AgentEvent, ToolStatus};

    let events = [
        AgentEvent::SessionStarted {
            conversation_id: "conversation-1".to_owned(),
        },
        AgentEvent::Error {
            message: "boom".to_owned(),
        },
        AgentEvent::GoalDeclared {
            goal: "ship it".to_owned(),
            status: "frozen".to_owned(),
            lead: None,
            criteria: Vec::new(),
        },
        AgentEvent::ToolCompleted {
            id: "tool-1".to_owned(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: Some("done".to_owned()),
        },
    ];

    for event in events {
        assert_eq!(classify(&event, 1), None);
    }
}
