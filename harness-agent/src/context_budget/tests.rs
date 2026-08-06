use super::*;
use crate::provider::{ChatMessage, FunctionCall, ProviderCapabilities, ToolCall};

fn caps(max_ctx: Option<u32>, out: Option<u32>) -> ProviderCapabilities {
    ProviderCapabilities {
        provider_id: "p".into(),
        model_id: "m".into(),
        supports_streaming: false,
        supports_reasoning_deltas: false,
        supports_tool_calling: true,
        supports_images: false,
        supports_computer_use: false,
        supports_shell_tool: true,
        max_context_tokens: max_ctx,
        output_token_limit: out,
        server_side_search: false,
    }
}

#[test]
fn from_capabilities_uses_caps_when_some_else_defaults() {
    let l = BudgetLimits::from_capabilities(&caps(Some(100_000), Some(4_096)));
    assert_eq!(l.context_tokens, 100_000);
    assert_eq!(l.output_headroom, 4_096);
    let d = BudgetLimits::from_capabilities(&caps(None, None));
    assert_eq!(d.context_tokens, 16_384);
    assert_eq!(d.output_headroom, DEFAULT_OUTPUT_HEADROOM);
}

#[test]
fn budget_subtracts_headroom_and_buffer() {
    let l = BudgetLimits::from_capabilities(&caps(Some(10_000), Some(2_000)));
    assert_eq!(l.budget(), 10_000 - 2_000 - SAFETY_BUFFER);
}

#[test]
fn estimate_is_conservative_and_counts_all_text_fields() {
    let l = BudgetLimits::from_capabilities(&caps(None, None));
    // role + content + reasoning + tool metadata + tool_call metadata/functions 都要算进去
    let mut tool = ChatMessage::tool_named("tool-id-123", "shell", "result text");
    tool.reasoning_content = Some("tool reasoning".to_string());
    let msgs = vec![
        ChatMessage::system("x".repeat(30)),
        ChatMessage::assistant(
            "assistant text",
            Some("assistant reasoning".to_string()),
            vec![ToolCall {
                id: "call-id-456".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "run_shell".to_string(),
                    arguments: "{\"cmd\":\"pwd\"}".to_string(),
                },
            }],
        ),
        tool,
    ];
    let bytes = "system".len()
        + 30
        + "assistant".len()
        + "assistant text".len()
        + "assistant reasoning".len()
        + "call-id-456".len()
        + "function".len()
        + "run_shell".len()
        + "{\"cmd\":\"pwd\"}".len()
        + "tool".len()
        + "result text".len()
        + "tool-id-123".len()
        + "shell".len()
        + "tool reasoning".len();
    // 保守：估值 >= 字节/cpt（不低估）
    assert!(estimate_tokens(&msgs, &l) >= bytes / l.chars_per_token);
    // 加 per_msg_overhead*n
    assert_eq!(
        estimate_tokens(&msgs, &l),
        bytes.div_ceil(l.chars_per_token) + l.per_msg_overhead * msgs.len()
    );

    let tools = vec![
        serde_json::json!({"type":"function","function":{"name":"run_shell","parameters":{"type":"object"}}}),
        serde_json::json!({"type":"function","function":{"name":"read_file","parameters":{"type":"object"}}}),
    ];
    let tool_bytes: usize = tools.iter().map(|t| t.to_string().len()).sum();
    assert_eq!(
        estimate_tools_tokens(&tools, &l),
        tool_bytes.div_ceil(l.chars_per_token) + l.per_msg_overhead * tools.len()
    );
}

// cpt=1/overhead=0/buffer=0 让「字节==token」便于精确算预算。
fn tight_limits(context_tokens: usize, recent: usize, min_recent: usize) -> BudgetLimits {
    BudgetLimits {
        context_tokens,
        output_headroom: 0,
        safety_buffer: 0,
        recent_turns_keep: recent,
        min_recent,
        chars_per_token: 1,
        per_msg_overhead: 0,
    }
}

fn fat(n: usize) -> String {
    "x".repeat(n)
}

// 钉住头（system 含状态帧 marker + 验收标准）+ 任务 + n 个 (assistant + 胖 tool) 轮。
fn build_wire(tool_bytes: usize, turns: usize) -> Vec<ChatMessage> {
    let mut w = vec![
        ChatMessage::system("FRAME objective=ship X | criteria=[PASS c1]"),
        ChatMessage::user("do the task"),
    ];
    for i in 0..turns {
        w.push(ChatMessage::assistant(format!("step {i}"), None, vec![]));
        w.push(ChatMessage::tool(format!("call{i}"), fat(tool_bytes)));
    }
    w
}

fn unwrap_fit(o: FitOutcome) -> Vec<ChatMessage> {
    match o {
        FitOutcome::Fit(m) => m,
        FitOutcome::Overflow { estimate, budget } => {
            panic!("expected Fit, got Overflow estimate={estimate} budget={budget}")
        }
    }
}

fn assert_message_fields_eq(actual: &ChatMessage, expected: &ChatMessage) {
    assert_eq!(actual.role, expected.role);
    assert_eq!(actual.content, expected.content);
    assert_eq!(actual.tool_call_id, expected.tool_call_id);
    assert_eq!(actual.reasoning_content, expected.reasoning_content);
    assert_eq!(actual.name, expected.name);
    match (&actual.tool_calls, &expected.tool_calls) {
        (None, None) => {}
        (Some(actual_calls), Some(expected_calls)) => {
            assert_eq!(actual_calls.len(), expected_calls.len());
            for (actual_call, expected_call) in actual_calls.iter().zip(expected_calls.iter()) {
                assert_eq!(actual_call.id, expected_call.id);
                assert_eq!(actual_call.call_type, expected_call.call_type);
                assert_eq!(actual_call.function.name, expected_call.function.name);
                assert_eq!(
                    actual_call.function.arguments,
                    expected_call.function.arguments
                );
            }
        }
        _ => panic!("tool_calls differ"),
    }
}

#[test]
fn effective_cap_ladder_skips_no_op_tiers() {
    let budget = 10_000;
    assert_eq!(effective_cap_ladder(budget, 10), vec![None]);
    assert_eq!(
        effective_cap_ladder(budget, 100_000),
        msg_cap_ladder(budget)
    );
    assert_eq!(effective_cap_ladder(budget, 1_000), vec![None, Some(312)]);
}

#[test]
fn under_budget_returns_wire_unchanged() {
    let limits = tight_limits(10_000, 3, 1);
    let wire = build_wire(50, 3);
    let before = wire.clone();
    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    assert_eq!(out.len(), before.len());
    // 逐条 content 不变
    for (a, b) in out.iter().zip(before.iter()) {
        assert_eq!(a.content, b.content);
    }
}

#[test]
fn folds_old_tool_bodies_keeps_frame_task_and_recent() {
    // 7 轮·每 tool 1000 字节·budget 3500：折叠 5 老 tool 正文后能塞下、近 2 轮留全。
    let limits = tight_limits(3_500, 2, 1);
    let wire = build_wire(1_000, 7);
    let out = unwrap_fit(fit_to_budget(wire.clone(), &limits, 0));
    assert!(estimate_tokens(&out, &limits) <= limits.budget());
    // 钉住头在
    assert!(out[0]
        .content
        .as_ref()
        .unwrap()
        .contains("objective=ship X"));
    assert!(out[0]
        .content
        .as_ref()
        .unwrap()
        .contains("criteria=[PASS c1]"));
    assert!(out[1].content.as_ref().unwrap().contains("do the task"));
    // 老 tool 正文被折叠（占位在·胖 x 串不全在）
    let joined: String = out.iter().filter_map(|m| m.content.clone()).collect();
    assert!(joined.contains("elided to save context"));
    // 近 2 轮 tool 正文留全（最后一条 tool 仍是 1000 个 x）
    let last_tool = out.iter().rev().find(|m| m.role == "tool").unwrap();
    assert_eq!(last_tool.content.as_ref().unwrap().len(), 1_000);
    // 确定性：跑两次一样
    let out2 = unwrap_fit(fit_to_budget(wire, &limits, 0));
    assert_eq!(out.len(), out2.len());
    for (a, b) in out.iter().zip(out2.iter()) {
        assert_eq!(a.content, b.content);
        assert_eq!(a.role, b.role);
    }
}

#[test]
fn drops_oldest_turns_when_folding_not_enough_and_marks() {
    // 60 轮·小 tool 200 字节：折叠后的占位仍累积超 budget 1500 → 必须整段丢老轮（折叠不够）。
    // （注意：若 tool 很大但轮数少·折叠+缩窗就够·不会丢轮——丢轮的前提是「折叠后占位累积仍超」。）
    let limits = tight_limits(1_500, 2, 1);
    let wire = build_wire(200, 60);
    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    assert!(estimate_tokens(&out, &limits) <= limits.budget());
    // 钉住头仍在
    assert!(out[0]
        .content
        .as_ref()
        .unwrap()
        .contains("objective=ship X"));
    assert!(out[1].content.as_ref().unwrap().contains("do the task"));
    // 有「丢了 N 轮」标记（真发生了整轮丢弃）
    let joined: String = out.iter().filter_map(|m| m.content.clone()).collect();
    assert!(joined.contains("earlier turn"));
    // 最近 2 轮的 tool 正文留全（200·keep_recent=2）
    let last_tool = out.iter().rev().find(|m| m.role == "tool").unwrap();
    assert_eq!(last_tool.content.as_ref().unwrap().len(), 200);
}

// 语义变更 2026-08-06：单条巨型工具结果现在剪中段而不是投降；head 本身超窗口的 Overflow 由 overflow_only_when_head_alone_exceeds_budget 守。
#[test]
fn giant_single_tool_now_fits_after_middle_truncation() {
    let limits = tight_limits(3_500, 2, 1);
    let wire = build_wire(5_000, 4);
    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    assert!(out
        .iter()
        .filter_map(|m| m.content.as_ref())
        .any(|content| { content.contains("bytes elided from the middle") }));
}

#[test]
fn truncation_is_preferred_over_dropping_turns() {
    let limits = tight_limits(500, 2, 1);
    let wire = vec![
        ChatMessage::system("FRAME objective=preserve all turns"),
        ChatMessage::user("do the task"),
        ChatMessage::assistant("A".repeat(5_000), None, vec![]),
        ChatMessage::tool("call0", "small result"),
        ChatMessage::assistant("final step", None, vec![]),
        ChatMessage::tool("call1", "done"),
    ];
    let input_assistant_count = wire.iter().filter(|m| m.role == "assistant").count();

    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    let joined: String = out.iter().filter_map(|m| m.content.clone()).collect();
    assert!(!joined.contains("earlier turn"));
    assert!(joined.contains("bytes elided from the middle"));
    assert_eq!(
        out.iter().filter(|m| m.role == "assistant").count(),
        input_assistant_count
    );
    assert!(estimate_tokens(&out, &limits) <= limits.budget());
}

#[test]
fn wider_cap_is_preferred_over_tighter_cap() {
    let budget = 8_192;
    let limits = tight_limits(budget, 1, 1);
    let wire = vec![
        ChatMessage::system("FRAME objective=prefer the least destructive cap"),
        ChatMessage::user("do the task"),
        ChatMessage::assistant("run tool", None, vec![]),
        ChatMessage::tool("call0", "x".repeat(20_000)),
    ];

    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    let content = out
        .iter()
        .find(|m| m.role == "tool")
        .and_then(|m| m.content.as_ref())
        .expect("the recent tool result must remain");
    let tighter_max_bytes = (budget / MSG_CAP_DIVISORS[1]) * limits.chars_per_token;
    // 宽档是 budget/8，紧档是 budget/32；超过紧档字节上限的两倍即可区分两档。
    assert!(content.len() > tighter_max_bytes * 2);
    assert!(content.contains("bytes elided from the middle"));
    assert!(estimate_tokens(&out, &limits) <= limits.budget());
}

#[test]
fn truncates_giant_recent_tool_instead_of_overflowing() {
    let limits = tight_limits(3_500, 2, 1);
    let wire = build_wire(5_000, 4);
    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));

    assert!(out[0]
        .content
        .as_ref()
        .unwrap()
        .contains("objective=ship X"));
    assert!(out[1].content.as_ref().unwrap().contains("do the task"));
    let truncated_tool = out
        .iter()
        .find(|m| {
            m.role == "tool"
                && m.content
                    .as_ref()
                    .is_some_and(|c| c.contains("bytes elided from the middle"))
        })
        .expect("expected a middle-truncated tool result");
    let content = truncated_tool.content.as_ref().unwrap();
    assert!(content.starts_with('x'));
    assert!(content.ends_with('x'));
    assert!(estimate_tokens(&out, &limits) <= limits.budget());
}

#[test]
fn overflow_only_when_head_alone_exceeds_budget() {
    let limits = tight_limits(3_500, 2, 1);
    let mut wire = vec![
        ChatMessage::system("x".repeat(10_000)),
        ChatMessage::user("do the task"),
    ];
    for i in 0..3 {
        wire.push(ChatMessage::assistant(format!("step {i}"), None, vec![]));
        wire.push(ChatMessage::tool(format!("call{i}"), "small result"));
    }

    match fit_to_budget(wire, &limits, 0) {
        FitOutcome::Overflow { estimate, budget } => {
            assert!(estimate > budget);
        }
        FitOutcome::Fit(_) => panic!("expected Overflow"),
    }
}

#[test]
fn pinned_head_is_byte_identical_after_truncation() {
    let limits = tight_limits(3_000, 1, 1);
    let head_call = ToolCall {
        id: "head-call-id".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "head_function".to_string(),
            arguments: format!("{{\"payload\":\"{}\"}}", "a".repeat(120)),
        },
    };
    let mut head_user = ChatMessage::user("pinned user request ".repeat(10));
    head_user.tool_call_id = Some("pinned-user-tool-id".to_string());
    head_user.name = Some("pinned-user-name".to_string());
    head_user.tool_calls = Some(vec![head_call]);
    let mut head_tool = ChatMessage::tool_named(
        "pinned-tool-id",
        "pinned-tool-name",
        "pinned tool content ".repeat(35),
    );
    head_tool.reasoning_content = Some("pinned reasoning ".repeat(15));
    let head = vec![
        ChatMessage::system("pinned system content ".repeat(45)),
        head_user,
        head_tool,
    ];
    let head_len = head.len();
    let mut wire = head.clone();
    wire.push(ChatMessage::assistant("run tool", None, vec![]));
    wire.push(ChatMessage::tool("body-call", "z".repeat(6_000)));

    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    assert!(out
        .iter()
        .skip(head_len)
        .filter_map(|m| m.content.as_ref())
        .any(|content| content.contains("bytes elided from the middle")));
    assert_eq!(out[..head_len].len(), head.len());
    for (actual, expected) in out[..head_len].iter().zip(head.iter()) {
        assert_message_fields_eq(actual, expected);
    }
}

#[test]
fn cap_never_truncates_non_tool_or_assistant_body_messages() {
    let limits = tight_limits(4_000, 1, 1);
    let user_content = "用户原话与引擎注入指令".repeat(70);
    let wire = vec![
        ChatMessage::system("FRAME objective=keep user bytes"),
        ChatMessage::user("do the task"),
        ChatMessage::assistant("run tool", None, vec![]),
        ChatMessage::user(user_content.clone()),
        ChatMessage::tool("body-call", "t".repeat(6_000)),
    ];

    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    let kept_user = out
        .iter()
        .find(|m| m.role == "user" && m.content.as_deref() == Some(user_content.as_str()))
        .expect("body user message must remain byte-identical");
    assert_eq!(kept_user.content.as_deref(), Some(user_content.as_str()));
    let capped_tool = out
        .iter()
        .find(|m| {
            m.role == "tool"
                && m.content
                    .as_ref()
                    .is_some_and(|c| c.contains("bytes elided from the middle"))
        })
        .expect("tool message in the same turn must be truncated");
    assert_ne!(
        capped_tool.content.as_deref(),
        Some("t".repeat(6_000).as_str())
    );
}

#[test]
fn head_split_matches_three_fifths() {
    let values = (0..=200).chain([
        usize::MAX / 4,
        usize::MAX / 3 - 1,
        usize::MAX / 3,
        usize::MAX,
    ]);
    for body_budget in values {
        let actual = head_split_bytes(body_budget);
        let expected = ((body_budget as u128) * 3 / 5) as usize;
        assert_eq!(actual, expected, "body_budget={body_budget}");
    }
}

#[test]
fn truncate_middle_is_utf8_safe_and_within_cap() {
    fn assert_complete_marker(s: &str, out: &str, max_bytes: usize, require_non_empty: bool) {
        assert!(out.len() <= max_bytes);
        let number_end = out
            .find(" bytes elided from the middle")
            .expect("这一档必须产出可解析的完整标记");
        let number_start = out[..number_end]
            .rfind("[… ")
            .expect("完整标记必须包含起始部分")
            + "[… ".len();
        let elided: usize = out[number_start..number_end]
            .parse()
            .expect("标记必须包含省略字节数");
        let marker = middle_elided_marker(elided);
        let (head_part, tail_part) = out.split_once(&marker).expect("这一档必须产出完整标记");
        assert!(s.starts_with(head_part));
        assert!(s.ends_with(tail_part));
        assert_eq!(elided, s.len() - head_part.len() - tail_part.len());
        if require_non_empty {
            assert!(!head_part.is_empty());
            assert!(!tail_part.is_empty());
        }
    }

    let s = "中文测试abc".repeat(2_000);
    let marker_len = middle_elided_marker(s.len()).len();
    let marker = middle_elided_marker(s.len());
    let degenerate_caps = [
        0,
        1,
        marker_len.saturating_sub(3),
        marker_len.saturating_sub(1),
        marker_len,
    ];
    for max_bytes in degenerate_caps {
        let out = truncate_middle(&s, max_bytes);
        assert!(out.len() <= max_bytes);
        assert!(marker.starts_with(&out));
    }

    for max_bytes in [256, 257, 258, 259, 300, 1_024, 4_096] {
        let out = truncate_middle(&s, max_bytes);
        assert_complete_marker(&s, &out, max_bytes, true);
    }

    for max_bytes in [marker_len + 1, marker_len + 3, marker_len + 10] {
        let out = truncate_middle(&s, max_bytes);
        assert_complete_marker(&s, &out, max_bytes, false);
    }

    let short = "中文abc";
    assert_eq!(truncate_middle(short, short.len()), short);
}

#[test]
fn cap_message_clones_small_messages_byte_for_byte() {
    let limits = tight_limits(10_000, 1, 1);
    let message = ChatMessage {
        role: "assistant".to_string(),
        content: Some("small assistant content".to_string()),
        tool_call_id: Some("small-tool-id".to_string()),
        tool_calls: Some(vec![ToolCall {
            id: "small-call-id".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "small_function".to_string(),
                arguments: "{\"small\":true}".to_string(),
            },
        }]),
        reasoning_content: Some("small reasoning".to_string()),
        name: Some("small-name".to_string()),
    };

    let capped = cap_message(&message, 1_000, &limits);
    assert_message_fields_eq(&capped, &message);
}

#[test]
fn truncation_is_deterministic() {
    let limits = tight_limits(3_500, 2, 1);
    let wire = build_wire(5_000, 4);
    let out1 = unwrap_fit(fit_to_budget(wire.clone(), &limits, 0));
    let out2 = unwrap_fit(fit_to_budget(wire, &limits, 0));

    assert_eq!(out1.len(), out2.len());
    for (a, b) in out1.iter().zip(out2.iter()) {
        assert_eq!(a.role, b.role);
        assert_eq!(a.content, b.content);
    }
}

#[test]
fn never_touches_tool_call_arguments() {
    let limits = tight_limits(10_000, 1, 1);
    let arguments = format!("{{\"payload\":\"{}\"}}", "a".repeat(6_000));
    let call = ToolCall {
        id: "call-id-keep".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "run_shell".to_string(),
            arguments: arguments.clone(),
        },
    };
    let wire = vec![
        ChatMessage::system("FRAME objective=ship X"),
        ChatMessage::user("do the task"),
        ChatMessage::assistant("x".repeat(8_000), None, vec![call.clone()]),
        ChatMessage::tool("call-id-keep", "y".repeat(12_000)),
    ];

    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    let kept = out
        .iter()
        .find_map(|m| m.tool_calls.as_ref())
        .and_then(|calls| calls.first())
        .expect("assistant tool call must remain");
    assert_eq!(kept.function.arguments.as_bytes(), arguments.as_bytes());
    assert_eq!(kept.function.name, call.function.name);
    assert_eq!(kept.id, call.id);
}

#[test]
fn small_messages_are_untouched_by_the_cap() {
    let limits = tight_limits(1_500, 2, 1);
    let wire = build_wire(80, 60);
    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    let joined: String = out.iter().filter_map(|m| m.content.clone()).collect();

    assert!(joined.contains("earlier turn"));
    assert!(!joined.contains("bytes elided from the middle"));
    let last_tool = out.iter().rev().find(|m| m.role == "tool").unwrap();
    assert_eq!(last_tool.content.as_ref().unwrap().len(), 80);
}

#[test]
fn pinned_head_is_never_dropped_even_under_pressure() {
    let limits = tight_limits(3_500, 2, 1);
    let wire = build_wire(1_000, 10);
    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));
    // system[0] 一定是状态帧·user 任务一定在
    assert_eq!(out[0].role, "system");
    assert!(out[0]
        .content
        .as_ref()
        .unwrap()
        .contains("objective=ship X"));
    assert!(out.iter().any(|m| m.role == "user"
        && m.content
            .as_ref()
            .is_some_and(|c| c.contains("do the task"))));
}

#[test]
fn reserve_tightens_budget_and_can_force_compaction() {
    let limits = tight_limits(800, 2, 1);
    let wire = build_wire(100, 4);

    let unchanged = unwrap_fit(fit_to_budget(wire.clone(), &limits, 0));
    assert_eq!(unchanged.len(), wire.len());

    match fit_to_budget(wire, &limits, 300) {
        FitOutcome::Fit(out) => {
            let joined: String = out.iter().filter_map(|m| m.content.clone()).collect();
            assert!(
                joined.contains("elided to save context") || joined.contains("earlier turn"),
                "reserve should force folding or dropping, got {joined}"
            );
            assert!(estimate_tokens(&out, &limits) <= limits.budget() - 300);
        }
        FitOutcome::Overflow { estimate, budget } => {
            assert!(
                estimate > budget,
                "reserve-forced Overflow should report estimate over budget"
            );
            assert_eq!(budget, limits.budget() - 300);
        }
    }
}
