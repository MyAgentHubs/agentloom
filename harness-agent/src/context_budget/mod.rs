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
/// 单条消息在 wire 里允许占的 token 上限阶梯（预算的几分之一·从宽到紧）。
const MSG_CAP_DIVISORS: [usize; 2] = [8, 32];
/// 单条上限的硬下限（防小预算下把消息剪成没有信息量）。
const MIN_MSG_CAP_TOKENS: usize = 256;

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

/// 折叠掉的老 tool 正文用的确定占位。
const ELIDED_TOOL_STUB: &str = "[earlier tool result elided to save context; re-read a smaller line range or re-run the command with a narrower filter (for example, grep/head/tail) if you need specific details]";

/// 丢掉 n 整轮时插的边界标记（提示耐久事实在状态帧/笔记/journal）。
fn dropped_turns_marker(n: usize) -> String {
    format!("[{n} earlier turn(s) elided to fit the model's context window; their facts are reflected in the state dashboard above and your working notes. Full history is in the run journal.]")
}

/// 单条上限阶梯：先不截断，再从宽到紧试。相邻档取值相同就去重。
fn msg_cap_ladder(budget: usize) -> Vec<Option<usize>> {
    let mut out = vec![None];
    let mut last: Option<usize> = None;
    for d in MSG_CAP_DIVISORS {
        let cap = (budget / d).max(MIN_MSG_CAP_TOKENS);
        if Some(cap) != last {
            out.push(Some(cap));
            last = Some(cap);
        }
    }
    out
}

/// 有效 cap 阶梯：先不截断，再从宽到紧试；
/// `max_body_msg_tokens` = body 里最大单条消息的估值——cap >= 它时截断必然是空操作
/// （产出与 None 档逐条相同），直接跳过，省掉一整轮候选构建。
fn effective_cap_ladder(budget: usize, max_body_msg_tokens: usize) -> Vec<Option<usize>> {
    msg_cap_ladder(budget)
        .into_iter()
        .filter(|cap| match cap {
            None => true,
            Some(c) => *c < max_body_msg_tokens,
        })
        .collect()
}

/// 中段被省略时插的标记。
fn middle_elided_marker(bytes: usize) -> String {
    format!("\n[… {bytes} bytes elided from the middle to fit the model's context window; repeating this same call is useless: it gives the same elided view — narrow it with grep/head/tail or read a smaller line range …]\n")
}

/// 头部占 body 预算的五分之三。先除后乘，避免 `body_budget * 3` 发生 usize 溢出
/// （`body_budget % 5 <= 4`，乘 3 不可能溢出）。
fn head_split_bytes(body_budget: usize) -> usize {
    body_budget / 5 * 3 + body_budget % 5 * 3 / 5
}

/// 把超长正文剪成「头 + 省略标记 + 尾」。UTF-8 边界安全·确定性。
/// 保证：返回串字节数 <= max_bytes。
/// 头部三分之五的拆分先除后乘，避免 `body_budget * 3` 发生 usize 溢出。
fn truncate_middle(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }

    let marker_len = middle_elided_marker(s.len()).len();
    let body_budget = max_bytes.saturating_sub(marker_len);
    if body_budget == 0 {
        let marker = middle_elided_marker(s.len());
        let mut end = max_bytes.min(marker.len());
        while !marker.is_char_boundary(end) {
            end -= 1;
        }
        return marker[..end].to_string();
    }

    let head_bytes = head_split_bytes(body_budget);
    let tail_bytes = body_budget - head_bytes;
    let mut head_end = head_bytes;
    while !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - tail_bytes;
    while !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    // 防御性分支：经上述预算与边界推导证明不可达。
    if head_end >= tail_start {
        return s.to_string();
    }

    let elided = tail_start - head_end;
    let marker = middle_elided_marker(elided);
    format!("{}{}{}", &s[..head_end], marker, &s[tail_start..])
}

/// 按单条上限剪一条 body 消息：只动 content / reasoning_content，
/// 绝不动 role / tool_call_id / name / tool_calls（函数名与参数是协议数据，剪了会坏）。
/// user 消息是用户原话或引擎注入的指令，不剪；body 里的巨型 user 目前救不了，属已知边界。
/// content 和 reasoning_content 各自剪到 max_bytes，因此同时带两者的消息实际可达 2× cap；
/// 名义 cap 与实际上限不完全一致，但不影响收敛（更紧的档仍会继续缩小）。
/// 注：一条消息的 token 还含 metadata 贡献，只剪正文不保证严格压到 cap 以下——
/// 这是刻意接受的近似（正文是主体），且本函数只剪一次、不循环，不会死循环。
fn cap_message(m: &ChatMessage, cap_tokens: usize, limits: &BudgetLimits) -> ChatMessage {
    if m.role != "tool" && m.role != "assistant" {
        return m.clone();
    }
    if estimate_tokens(std::slice::from_ref(m), limits) <= cap_tokens {
        return m.clone();
    }

    let max_bytes = cap_tokens.saturating_mul(limits.chars_per_token);
    let mut capped = m.clone();
    capped.content = m
        .content
        .as_deref()
        .map(|content| truncate_middle(content, max_bytes));
    capped.reasoning_content = m
        .reasoning_content
        .as_deref()
        .map(|content| truncate_middle(content, max_bytes));
    capped
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
    msg_cap_tokens: Option<usize>,
    limits: &BudgetLimits,
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
            } else if let Some(cap) = msg_cap_tokens {
                out.push(cap_message(m, cap, limits));
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
    let max_body_msg = groups
        .iter()
        .flatten()
        .map(|m| estimate_tokens(std::slice::from_ref(m), limits))
        .max()
        .unwrap_or(0);
    let ladder = effective_cap_ladder(budget, max_body_msg);
    for keep_recent in (lo..=hi).rev() {
        let max_drop = n.saturating_sub(keep_recent);
        for drop_oldest in 0..=max_drop {
            for cap in &ladder {
                let cand = build_candidate(&head, &groups, drop_oldest, keep_recent, *cap, limits);
                if estimate_tokens(&cand, limits) <= budget {
                    return FitOutcome::Fit(cand);
                }
            }
        }
    }

    // 地板：最紧 cap + 只留 min_recent。
    let keep = lo;
    let tightest = ladder.last().copied().flatten();
    let smallest = build_candidate(
        &head,
        &groups,
        n.saturating_sub(keep),
        keep,
        tightest,
        limits,
    );
    FitOutcome::Overflow {
        estimate: estimate_tokens(&smallest, limits),
        budget,
    }
}

#[cfg(test)]
mod tests;
