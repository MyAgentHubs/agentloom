//! Context Budget: 确定性估 token + 分层压缩，让历史在撑爆 context window 前瘦身。
//! 只作用于每轮临时 wire；canonical 历史 + journal 永不受影响（零数据丢失）。

use crate::provider::{ChatMessage, ProviderCapabilities};
use serde_json::Value;

// 建议起始常量（spec §4.5·往安全侧调·宁可早压宁可高估）。
const DEFAULT_CONTEXT_TOKENS: usize = 16_384;
const DEFAULT_OUTPUT_HEADROOM: usize = 8_192;
const SAFETY_BUFFER: usize = 2_048;
const CHARS_PER_TOKEN: usize = 3;
const PER_MSG_OVERHEAD: usize = 4;

/// 一次 turn 的预算参数。预算 = context_tokens − output_headroom − safety_buffer。
#[derive(Debug, Clone)]
pub struct BudgetLimits {
    pub context_tokens: usize,
    pub output_headroom: usize,
    pub safety_buffer: usize,
    pub recent_turns_keep: usize,
    pub min_recent: usize,
    pub chars_per_token: usize,
    pub per_msg_overhead: usize,
}

impl BudgetLimits {
    /// capabilities 有就用、没有（真 provider 报 None）落保守默认。
    pub fn from_capabilities(caps: &ProviderCapabilities) -> Self {
        Self {
            context_tokens: caps
                .max_context_tokens
                .map(|v| v as usize)
                .unwrap_or(DEFAULT_CONTEXT_TOKENS),
            output_headroom: caps
                .output_token_limit
                .map(|v| v as usize)
                .unwrap_or(DEFAULT_OUTPUT_HEADROOM),
            safety_buffer: SAFETY_BUFFER,
            recent_turns_keep: 3,
            min_recent: 1,
            chars_per_token: CHARS_PER_TOKEN,
            per_msg_overhead: PER_MSG_OVERHEAD,
        }
    }

    /// 触发线：估值 ≤ 此值不动·超了开始压·压不下去 → Overflow。
    pub fn budget(&self) -> usize {
        self.context_tokens
            .saturating_sub(self.output_headroom)
            .saturating_sub(self.safety_buffer)
    }
}

/// 一条消息里所有「会发给模型的文本」字节：role/content/reasoning/tool metadata + tool_call metadata/函数名/参数。
fn msg_text_bytes(m: &ChatMessage) -> usize {
    let mut n = m.role.len();
    if let Some(content) = &m.content {
        n += content.len();
    }
    if let Some(tool_call_id) = &m.tool_call_id {
        n += tool_call_id.len();
    }
    if let Some(r) = &m.reasoning_content {
        n += r.len();
    }
    if let Some(name) = &m.name {
        n += name.len();
    }
    if let Some(calls) = &m.tool_calls {
        for c in calls {
            n +=
                c.id.len() + c.call_type.len() + c.function.name.len() + c.function.arguments.len();
        }
    }
    n
}

/// 保守确定性估值：字节/cpt（宁可高估·cpt 取 3 比 CC 的 4 保守）+ 每条固定开销。
pub fn estimate_tokens(messages: &[ChatMessage], limits: &BudgetLimits) -> usize {
    let bytes: usize = messages.iter().map(msg_text_bytes).sum();
    bytes.div_ceil(limits.chars_per_token) + limits.per_msg_overhead * messages.len()
}

pub fn estimate_tools_tokens(tools: &[Value], limits: &BudgetLimits) -> usize {
    let bytes: usize = tools.iter().map(|t| t.to_string().len()).sum();
    bytes.div_ceil(limits.chars_per_token) + limits.per_msg_overhead * tools.len()
}

/// 折叠掉的老 tool 正文用的确定占位（指向 journal + 状态帧/笔记作为耐久载体）。
const ELIDED_TOOL_STUB: &str = "[earlier tool result elided to save context; the full record is in the run journal — re-read the file or re-run the command if you need it]";

/// 丢掉 n 整轮时插的边界标记（提示耐久事实在状态帧/笔记/journal）。
fn dropped_turns_marker(n: usize) -> String {
    format!("[{n} earlier turn(s) elided to fit the model's context window; their facts are reflected in the state dashboard above and your working notes. Full history is in the run journal.]")
}

/// 压缩结果：Fit(可能已瘦身的 wire) 或 Overflow(连最小钉住都超·走求助)。
#[derive(Debug)]
pub enum FitOutcome {
    Fit(Vec<ChatMessage>),
    Overflow { estimate: usize, budget: usize },
}

/// 首个 assistant 的下标 = body 起点；之前全是钉住头（system + 任务/注入）。
fn first_assistant_idx(msgs: &[ChatMessage]) -> usize {
    msgs.iter()
        .position(|m| m.role == "assistant")
        .unwrap_or(msgs.len())
}

/// 把 body 按「每个 assistant 起一组」切成 turn-group。
fn group_turns(body: &[ChatMessage]) -> Vec<Vec<ChatMessage>> {
    let mut groups: Vec<Vec<ChatMessage>> = Vec::new();
    for m in body {
        if m.role == "assistant" || groups.is_empty() {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(m.clone());
    }
    groups
}

/// 折叠一条 tool 消息：留 role/tool_call_id/name·正文换占位。
fn fold_tool(m: &ChatMessage) -> ChatMessage {
    ChatMessage {
        role: m.role.clone(),
        content: Some(ELIDED_TOOL_STUB.to_string()),
        tool_call_id: m.tool_call_id.clone(),
        tool_calls: None,
        reasoning_content: None,
        name: m.name.clone(),
    }
}

/// 组装候选：头 + (丢了 N 轮的标记) + 中段(非 recent 的 tool 正文折叠) + 最近 keep_recent 组(留全)。
/// drop_oldest 个最老组整段丢；keep_recent 组保留 tool 正文不折叠。
fn build_candidate(
    head: &[ChatMessage],
    groups: &[Vec<ChatMessage>],
    drop_oldest: usize,
    keep_recent: usize,
) -> Vec<ChatMessage> {
    let n = groups.len();
    let recent_from = n.saturating_sub(keep_recent);
    let mut out: Vec<ChatMessage> = head.to_vec();
    if drop_oldest > 0 {
        out.push(ChatMessage::user(dropped_turns_marker(drop_oldest)));
    }
    for (gi, group) in groups.iter().enumerate() {
        if gi < drop_oldest {
            continue; // 整段丢
        }
        let is_recent = gi >= recent_from;
        for m in group {
            if !is_recent && m.role == "tool" {
                out.push(fold_tool(m));
            } else {
                out.push(m.clone());
            }
        }
    }
    out
}

/// 把 wire 压进预算。返回「能塞下的最省删」候选；连最小钉住都超 → Overflow。
/// 纯函数·确定性·无 LLM·无落盘。只作用于临时 wire（canonical 不受影响）。
pub fn fit_to_budget(
    wire: Vec<ChatMessage>,
    limits: &BudgetLimits,
    reserve_tokens: usize,
) -> FitOutcome {
    let budget = limits.budget().saturating_sub(reserve_tokens);
    if estimate_tokens(&wire, limits) <= budget {
        return FitOutcome::Fit(wire);
    }

    let body_start = first_assistant_idx(&wire);
    let head: Vec<ChatMessage> = wire[..body_start].to_vec();
    let groups = group_turns(&wire[body_start..]);
    let n = groups.len();

    // keep_recent 从高(RECENT)到低(MIN)·每档 drop 从少到多·返回第一个能塞下的（最省删）。
    let hi = limits.recent_turns_keep.min(n);
    let lo = limits.min_recent.min(n);
    for keep_recent in (lo..=hi).rev() {
        let max_drop = n.saturating_sub(keep_recent);
        for drop_oldest in 0..=max_drop {
            let cand = build_candidate(&head, &groups, drop_oldest, keep_recent);
            if estimate_tokens(&cand, limits) <= budget {
                return FitOutcome::Fit(cand);
            }
        }
    }

    // 地板：连「头 + min_recent 组（其余全丢）」都超 → Overflow。
    let keep = lo;
    let smallest = build_candidate(&head, &groups, n.saturating_sub(keep), keep);
    FitOutcome::Overflow {
        estimate: estimate_tokens(&smallest, limits),
        budget,
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn overflow_when_pinned_plus_min_recent_exceeds_budget() {
        // 单条 tool 5000 > budget 3500：连「头 + 1 近轮」都塞不下 → Overflow（不返回截掉帧的 wire）。
        let limits = tight_limits(3_500, 2, 1);
        let wire = build_wire(5_000, 4);
        match fit_to_budget(wire, &limits, 0) {
            FitOutcome::Overflow { estimate, budget } => {
                assert!(
                    estimate > budget,
                    "estimate {estimate} should exceed budget {budget}"
                );
                assert_eq!(budget, 3_500);
            }
            FitOutcome::Fit(_) => panic!("expected Overflow"),
        }
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
}
