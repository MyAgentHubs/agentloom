use super::{
    estimate_tokens, estimate_tools_tokens, first_assistant_idx, truncate_middle, BudgetLimits,
};
use crate::error::Result;
use crate::events::EventRecorder;
use crate::goal::GoalState;
use crate::orchestrator::{build_offered_tools_with_roots, EvidenceGate, RunOptions};
use crate::provider::{ChatMessage, FinishReason, ProviderCapabilities, ProviderClient};
use crate::tools::ToolRegistry;
use serde_json::json;
use std::fmt::Write;

pub const SUMMARY_PROMPT: &str = r###"You are performing a context checkpoint compaction for a coding-agent session. You will receive earlier conversation content, possibly beginning with a previous checkpoint summary. Produce ONE updated checkpoint summary that replaces all of it.

Output exactly these 8 Markdown sections, in this order, each starting with "## ". If a section has nothing, write "(none)".

## Primary Request and Intent
## Key Technical Concepts
## Files and Code
## Errors and Fixes
## Pending Jobs
## Current Work
## Next Step
## Critical Context

Rules: preserve exact file paths, commands, error strings, identifiers, numbers, and function signatures; faithfully record user corrections and preferences; summarize older content more briefly and recent content in more detail; if the input begins with a previous checkpoint summary, merge and update it instead of copying it; do not mention the compaction process itself; output plain text only — no tool calls, no code fences; write in the dominant language of the conversation; keep the whole summary under 4000 words."###;

pub const THRESHOLD_RATIO: usize = 35;
pub const MAX_TAIL_BUDGET_TOKENS: usize = 64_000;
const PERCENT_DENOMINATOR: usize = 100;
const TAIL_BUDGET_DIVISOR: usize = 10;
const SUMMARY_SECTION_HEADINGS: [&str; 8] = [
    "## Primary Request and Intent",
    "## Key Technical Concepts",
    "## Files and Code",
    "## Errors and Fixes",
    "## Pending Jobs",
    "## Current Work",
    "## Next Step",
    "## Critical Context",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldSummary {
    pub through: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg {
    pub id: i64,
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTranscript {
    pub preamble: String,
    pub old_summary: Option<OldSummary>,
    pub messages: Vec<Msg>,
    pub trailing: String,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactPlan {
    pub fold_input: String,
    pub tail_start_idx: usize,
    pub through_message_id: i64,
}

#[derive(Debug)]
enum Marker {
    SummaryStart {
        nonce: String,
        through: i64,
    },
    SummaryEnd {
        nonce: String,
    },
    Message {
        nonce: String,
        id: i64,
        role: String,
    },
    HistoryEnd {
        nonce: String,
    },
}

impl Marker {
    fn nonce(&self) -> &str {
        match self {
            Self::SummaryStart { nonce, .. }
            | Self::SummaryEnd { nonce }
            | Self::Message { nonce, .. }
            | Self::HistoryEnd { nonce } => nonce,
        }
    }
}

#[derive(Debug)]
struct LocatedMarker {
    start: usize,
    after_line: usize,
    marker: Marker,
}

fn valid_nonce(nonce: &str) -> bool {
    nonce.len() == 32
        && nonce
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn parse_i64_canonical(raw: &str) -> Option<i64> {
    let value = raw.parse::<i64>().ok()?;
    (value.to_string() == raw).then_some(value)
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn nonce_and_rest<'a>(line: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = line.strip_prefix(prefix)?;
    let nonce = rest.get(..32)?;
    valid_nonce(nonce).then_some((nonce, &rest[32..]))
}

fn parse_marker(line: &str) -> Option<Marker> {
    if let Some((nonce, rest)) = nonce_and_rest(line, "===== AGENTLOOM-COMPACT-SUMMARY ") {
        let through = rest.strip_prefix(" through=")?.strip_suffix(" =====")?;
        return Some(Marker::SummaryStart {
            nonce: nonce.to_string(),
            through: parse_i64_canonical(through)?,
        });
    }

    if let Some((nonce, rest)) = nonce_and_rest(line, "===== /AGENTLOOM-COMPACT-SUMMARY ") {
        if rest == " =====" {
            return Some(Marker::SummaryEnd {
                nonce: nonce.to_string(),
            });
        }
        return None;
    }

    if let Some((nonce, rest)) = nonce_and_rest(line, "===== AGENTLOOM-MSG ") {
        let fields = rest.strip_prefix(" id=")?.strip_suffix(" =====")?;
        let (id, role) = fields.split_once(" role=")?;
        if !valid_token(role) {
            return None;
        }
        return Some(Marker::Message {
            nonce: nonce.to_string(),
            id: parse_i64_canonical(id)?,
            role: role.to_string(),
        });
    }

    if let Some((nonce, rest)) = nonce_and_rest(line, "===== AGENTLOOM-HISTORY-END ") {
        if rest == " =====" {
            return Some(Marker::HistoryEnd {
                nonce: nonce.to_string(),
            });
        }
    }

    None
}

fn located_markers(input: &str) -> Vec<LocatedMarker> {
    let mut markers = Vec::new();
    let mut start = 0;

    while start < input.len() {
        let Some(relative_end) = input[start..].find('\n') else {
            break;
        };
        let line_end = start + relative_end;
        if let Some(marker) = parse_marker(&input[start..line_end]) {
            markers.push(LocatedMarker {
                start,
                after_line: line_end + 1,
                marker,
            });
        }
        start = line_end + 1;
    }

    markers
}

pub fn parse_transcript(input: &str) -> Option<ParsedTranscript> {
    let all_markers = located_markers(input);
    let first_index = all_markers.iter().position(|located| {
        matches!(
            located.marker,
            Marker::SummaryStart { .. } | Marker::Message { .. }
        )
    })?;
    let nonce = all_markers[first_index].marker.nonce().to_string();
    let markers: Vec<&LocatedMarker> = all_markers[first_index..]
        .iter()
        .filter(|located| located.marker.nonce() == nonce)
        .collect();
    let first = *markers.first()?;
    let preamble = input[..first.start].to_string();
    let mut cursor = 0;
    let old_summary = if let Marker::SummaryStart { through, .. } = &first.marker {
        let close = *markers.get(1)?;
        if !matches!(close.marker, Marker::SummaryEnd { .. }) {
            return None;
        }
        cursor = 2;
        Some(OldSummary {
            through: *through,
            text: input[first.after_line..close.start].to_string(),
        })
    } else {
        None
    };

    if old_summary.is_some() {
        let close = markers[1];
        let next = *markers.get(cursor)?;
        if close.after_line != next.start {
            return None;
        }
    }

    let mut messages = Vec::new();
    loop {
        let current = *markers.get(cursor)?;
        match &current.marker {
            Marker::Message { id, role, .. } => {
                let next = *markers.get(cursor + 1)?;
                if !matches!(
                    next.marker,
                    Marker::Message { .. } | Marker::HistoryEnd { .. }
                ) {
                    return None;
                }
                messages.push(Msg {
                    id: *id,
                    role: role.clone(),
                    text: input[current.after_line..next.start].to_string(),
                });
                cursor += 1;
            }
            Marker::HistoryEnd { .. } => {
                if cursor + 1 != markers.len() {
                    return None;
                }
                return Some(ParsedTranscript {
                    preamble,
                    old_summary,
                    messages,
                    trailing: input[current.after_line..].to_string(),
                    nonce,
                });
            }
            Marker::SummaryStart { .. } | Marker::SummaryEnd { .. } => return None,
        }
    }
}

fn append_summary(out: &mut String, nonce: &str, through: i64, text: &str) {
    writeln!(
        out,
        "===== AGENTLOOM-COMPACT-SUMMARY {nonce} through={through} ====="
    )
    .expect("writing to String cannot fail");
    out.push_str(text);
    if !text.is_empty() && !text.ends_with('\n') {
        out.push('\n');
    }
    writeln!(out, "===== /AGENTLOOM-COMPACT-SUMMARY {nonce} =====")
        .expect("writing to String cannot fail");
}

fn append_message(out: &mut String, nonce: &str, message: &Msg) {
    writeln!(
        out,
        "===== AGENTLOOM-MSG {nonce} id={} role={} =====",
        message.id, message.role
    )
    .expect("writing to String cannot fail");
    out.push_str(&message.text);
}

pub fn render_transcript(parsed: &ParsedTranscript) -> String {
    let mut out = parsed.preamble.clone();
    if let Some(summary) = &parsed.old_summary {
        append_summary(&mut out, &parsed.nonce, summary.through, &summary.text);
    }
    for message in &parsed.messages {
        append_message(&mut out, &parsed.nonce, message);
    }
    writeln!(out, "===== AGENTLOOM-HISTORY-END {} =====", parsed.nonce)
        .expect("writing to String cannot fail");
    out.push_str(&parsed.trailing);
    out
}

pub fn should_compact(head_tokens: usize, limits: &BudgetLimits) -> bool {
    let budget = limits.budget();
    let threshold = budget / PERCENT_DENOMINATOR * THRESHOLD_RATIO
        + (budget % PERCENT_DENOMINATOR * THRESHOLD_RATIO).div_ceil(PERCENT_DENOMINATOR);
    head_tokens >= threshold
}

fn push_fold_section(out: &mut String, heading: &str, text: &str) {
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(heading);
    out.push('\n');
    out.push_str(text);
}

pub fn plan_compaction(parsed: &ParsedTranscript, limits: &BudgetLimits) -> Option<CompactPlan> {
    let tail_budget = (limits.budget() / TAIL_BUDGET_DIVISOR).min(MAX_TAIL_BUDGET_TOKENS);
    let mut tail_tokens = 0usize;
    let mut tail_start_idx = parsed.messages.len();

    for index in (0..parsed.messages.len()).rev() {
        tail_start_idx = index;
        let wire_message = ChatMessage::user(&parsed.messages[index].text);
        tail_tokens = tail_tokens
            .saturating_add(estimate_tokens(std::slice::from_ref(&wire_message), limits));
        if tail_tokens >= tail_budget {
            break;
        }
    }

    if tail_start_idx == 0 || tail_start_idx == parsed.messages.len() {
        return None;
    }

    let folded_messages = &parsed.messages[..tail_start_idx];
    let through_message_id = folded_messages.iter().map(|message| message.id).max()?;
    let mut fold_input = String::new();
    if let Some(summary) = &parsed.old_summary {
        push_fold_section(
            &mut fold_input,
            &format!("先前检查点摘要 through={}", summary.through),
            &summary.text,
        );
    }
    for message in folded_messages {
        push_fold_section(
            &mut fold_input,
            &format!("消息 id={} role={}", message.id, message.role),
            &message.text,
        );
    }

    Some(CompactPlan {
        fold_input,
        tail_start_idx,
        through_message_id,
    })
}

pub fn rebuild(parsed: &ParsedTranscript, plan: &CompactPlan, new_summary: &str) -> String {
    let mut out = parsed.preamble.clone();
    append_summary(
        &mut out,
        &parsed.nonce,
        plan.through_message_id,
        new_summary,
    );
    for message in &parsed.messages[plan.tail_start_idx..] {
        append_message(&mut out, &parsed.nonce, message);
    }
    writeln!(out, "===== AGENTLOOM-HISTORY-END {} =====", parsed.nonce)
        .expect("writing to String cannot fail");
    out.push_str(&parsed.trailing);
    out
}

fn summary_is_smaller(summary: &str, fold_input: &str, limits: &BudgetLimits) -> bool {
    let summary_message = ChatMessage::user(summary);
    let fold_message = ChatMessage::user(fold_input);
    estimate_tokens(std::slice::from_ref(&summary_message), limits)
        < estimate_tokens(std::slice::from_ref(&fold_message), limits)
}

fn summary_has_required_sections(summary: &str) -> bool {
    summary
        .lines()
        .filter_map(|line| {
            SUMMARY_SECTION_HEADINGS
                .iter()
                .position(|heading| line == *heading)
        })
        .eq(0..SUMMARY_SECTION_HEADINGS.len())
}

fn summary_contains_nonce_marker(summary: &str, nonce: &str) -> bool {
    summary.lines().any(|line| {
        parse_marker(line)
            .as_ref()
            .is_some_and(|marker| marker.nonce() == nonce)
    })
}

fn emit_compact_failure(recorder: &mut EventRecorder, reason: &str) -> Result<()> {
    recorder.emit(
        "orchestration.step.completed",
        json!({
            "step_id": "solo.compact",
            "turn": 0,
            "outcome": "objective_compact_failed",
            "reason": reason,
        }),
    )?;
    Ok(())
}

pub(crate) async fn compact_objective_at_run_start<P: ProviderClient>(
    provider: &P,
    capabilities: &ProviderCapabilities,
    messages: &mut [ChatMessage],
    goal: &mut GoalState,
    recorder: &mut EventRecorder,
) -> Result<()> {
    let limits = BudgetLimits::from_capabilities(capabilities);
    let original_tokens = estimate_tokens(&messages[..first_assistant_idx(messages)], &limits);
    if !should_compact(original_tokens, &limits) {
        return Ok(());
    }

    let Some((message_index, parsed)) = messages.iter().enumerate().find_map(|(index, message)| {
        if message.role != "user" {
            return None;
        }
        let parsed = parse_transcript(message.content.as_deref()?)?;
        (parsed.old_summary.is_some() || !parsed.messages.is_empty()).then_some((index, parsed))
    }) else {
        return Ok(());
    };
    let Some(plan) = plan_compaction(&parsed, &limits) else {
        return Ok(());
    };

    let request = vec![
        ChatMessage::system(SUMMARY_PROMPT),
        ChatMessage::user(plan.fold_input.clone()),
    ];
    let mut iso = EventRecorder::with_sinks("autocompact", None, None, vec![]);
    let response = match provider.next_turn(&request, &[], &mut iso).await {
        Ok(response) => response,
        Err(crate::error::HarnessError::Provider(reason)) => {
            emit_compact_failure(recorder, &reason)?;
            return Ok(());
        }
        Err(err) => {
            emit_compact_failure(recorder, &err.to_string())?;
            return Ok(());
        }
    };
    let summary = response.text;
    if summary.trim().is_empty() {
        emit_compact_failure(recorder, "empty summary")?;
        return Ok(());
    }
    if !summary_is_smaller(&summary, &plan.fold_input, &limits) {
        emit_compact_failure(recorder, "summary is not smaller than fold input")?;
        return Ok(());
    }
    if !summary_has_required_sections(&summary) {
        emit_compact_failure(recorder, "summary_missing_sections")?;
        return Ok(());
    }
    if summary_contains_nonce_marker(&summary, &parsed.nonce) {
        emit_compact_failure(recorder, "summary_contains_marker")?;
        return Ok(());
    }
    if matches!(response.finish_reason, Some(FinishReason::Length))
        || (response.finish_reason.is_none() && response.interruption.is_some())
    {
        emit_compact_failure(recorder, "summary_truncated")?;
        return Ok(());
    }
    if !response.tool_calls.is_empty() {
        emit_compact_failure(recorder, "summary_tool_calls")?;
        return Ok(());
    }

    let new_objective = rebuild(&parsed, &plan, &summary);
    let compacted_tokens = estimate_tokens(
        std::slice::from_ref(&ChatMessage::user(&new_objective)),
        &limits,
    );
    if should_compact(compacted_tokens, &limits) {
        emit_compact_failure(recorder, "summary_still_over_threshold")?;
        return Ok(());
    }
    messages[message_index].content = Some(new_objective.clone());
    goal.contract.objective = new_objective;
    recorder.emit(
        "orchestration.step.completed",
        json!({
            "step_id": "solo.compact",
            "turn": 0,
            "outcome": "objective_compacted",
            "summary": summary,
            "through_message_id": plan.through_message_id,
            "original_tokens": original_tokens,
            "compacted_tokens": compacted_tokens,
            "budget_tokens": limits.budget(),
        }),
    )?;
    Ok(())
}
pub(crate) fn salvage_head_overflow(
    messages: &mut Vec<ChatMessage>,
    limits: &BudgetLimits,
    tools_tokens: usize,
    recorder: &mut EventRecorder,
) -> Result<bool> {
    let head_len = messages
        .iter()
        .position(|message| message.role == "assistant")
        .unwrap_or(messages.len());
    let original_tokens = estimate_tokens(&messages[..head_len], limits);
    let reserved_tokens = tools_tokens.saturating_add(512);
    let total_tokens = original_tokens.saturating_add(reserved_tokens);
    if total_tokens <= limits.budget() {
        return Ok(false);
    }
    let Some(message_index) = messages
        .iter()
        .take(head_len)
        .enumerate()
        .filter(|(_, message)| message.role == "user")
        .max_by_key(|(_, message)| estimate_tokens(std::slice::from_ref(message), limits))
        .map(|(index, _)| index)
    else {
        return Ok(false);
    };
    let Some(original_content) = messages[message_index].content.clone() else {
        return Ok(false);
    };
    let excess_tokens = total_tokens.saturating_sub(limits.budget());
    let target_bytes = original_content
        .len()
        .saturating_sub(excess_tokens.saturating_mul(limits.chars_per_token))
        .max(256usize.saturating_mul(limits.chars_per_token));
    messages[message_index].content = Some(truncate_middle(&original_content, target_bytes));
    let truncated_tokens = estimate_tokens(&messages[..head_len], limits);
    if truncated_tokens.saturating_add(reserved_tokens) > limits.budget() {
        messages[message_index].content = Some(original_content);
        return Ok(false);
    }
    if let Err(error) = recorder.emit("orchestration.step.completed", json!({"step_id":"solo.compact","turn":0,"outcome":"head_truncated_continue","original_tokens":original_tokens,"truncated_tokens":truncated_tokens,"budget_tokens":limits.budget()})) { messages[message_index].content = Some(original_content); return Err(error); }
    Ok(true)
}
pub(crate) async fn run_start_context_maintenance<P: ProviderClient>(
    provider: &P,
    capabilities: &ProviderCapabilities,
    registry: &ToolRegistry,
    options: &RunOptions,
    messages: &mut Vec<ChatMessage>,
    goal: &mut GoalState,
    recorder: &mut EventRecorder,
) -> Result<()> {
    let mut run_start_disallowed = options.disallowed_tools.clone();
    if options.evidence_gate == EvidenceGate::Off {
        run_start_disallowed.insert("register_issue_probe".to_string());
    }
    let tools = build_offered_tools_with_roots(
        registry,
        capabilities,
        options.network,
        options.native_search_enabled,
        &run_start_disallowed,
        &options.extra_read_roots,
    );
    compact_objective_at_run_start(provider, capabilities, messages, goal, recorder).await?;
    let limits = BudgetLimits::from_capabilities(capabilities);
    let tools_tokens = estimate_tools_tokens(&tools, &limits);
    let objective_index = messages.iter().position(|message| {
        message.role == "user" && message.content.as_deref() == Some(&goal.contract.objective)
    });
    let objective_tokens = objective_index
        .map(|index| estimate_tokens(std::slice::from_ref(&messages[index]), &limits))
        .unwrap_or(0);
    let largest_user_index = messages
        .iter()
        .take_while(|message| message.role != "assistant")
        .enumerate()
        .filter(|(_, message)| message.role == "user")
        .max_by_key(|(_, message)| estimate_tokens(std::slice::from_ref(message), &limits))
        .map(|(index, _)| index);
    let head_tokens = estimate_tokens(
        &messages[..messages
            .iter()
            .position(|message| message.role == "assistant")
            .unwrap_or(messages.len())],
        &limits,
    );
    let duplicate_excess = head_tokens
        .saturating_add(tools_tokens)
        .saturating_add(512)
        .saturating_add(objective_tokens)
        .saturating_sub(limits.budget());
    let objective_reserve = if objective_index == largest_user_index {
        objective_tokens.saturating_sub(duplicate_excess.div_ceil(2))
    } else {
        objective_tokens
    };
    if salvage_head_overflow(
        messages,
        &limits,
        tools_tokens.saturating_add(objective_reserve),
        recorder,
    )? {
        if let Some(content) = objective_index.and_then(|index| messages[index].content.clone()) {
            goal.contract.objective = content;
        }
    }
    Ok(())
}
pub(crate) fn emit_context_budget_exhausted(
    recorder: &mut EventRecorder,
    turn: usize,
    estimate: usize,
    budget: usize,
) -> Result<()> {
    // 连最小钉住上下文都超本模型窗口 → 走已有求助出口（needs_decision·exit4）·别静默发超限请求。
    recorder.emit(
        "run.needs_decision",
        json!({
            "reason": "context_budget_exhausted",
            "turn": turn,
            "estimate_tokens": estimate,
            "budget_tokens": budget,
            "next_step": "拆小任务 / 换更大上下文的模型",
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn limits(budget: usize) -> BudgetLimits {
        BudgetLimits {
            context_tokens: budget,
            output_headroom: 0,
            safety_buffer: 0,
            recent_turns_keep: 3,
            min_recent: 1,
            chars_per_token: 1,
            per_msg_overhead: 0,
            images_count_toward_budget: true,
        }
    }

    #[test]
    fn summary_must_be_strictly_smaller_than_fold_input() {
        let limits = limits(100);

        assert!(!summary_is_smaller("same", "same", &limits));
        assert!(!summary_is_smaller("longer", "short", &limits));
        assert!(summary_is_smaller("short", "strictly longer", &limits));
    }

    #[test]
    fn summary_requires_all_sections_once_in_order() {
        let valid = SUMMARY_SECTION_HEADINGS.join("\ncontent\n");
        assert!(summary_has_required_sections(&valid));

        let missing = SUMMARY_SECTION_HEADINGS[..7].join("\ncontent\n");
        assert!(!summary_has_required_sections(&missing));

        let mut reordered = SUMMARY_SECTION_HEADINGS;
        reordered.swap(2, 3);
        assert!(!summary_has_required_sections(
            &reordered.join("\ncontent\n")
        ));

        let duplicated = format!("{valid}\n{}", SUMMARY_SECTION_HEADINGS[0]);
        assert!(!summary_has_required_sections(&duplicated));
    }

    #[test]
    fn summary_rejects_structural_marker_line_for_selected_nonce_only() {
        let selected = format!("===== AGENTLOOM-MSG {NONCE} id=7 role=user =====");
        assert!(summary_contains_nonce_marker(&selected, NONCE));

        let foreign = "===== AGENTLOOM-MSG deadbeefdeadbeefdeadbeefdeadbeef id=7 role=user =====";
        assert!(!summary_contains_nonce_marker(foreign, NONCE));

        let inline = format!("prose before ===== AGENTLOOM-HISTORY-END {NONCE} =====");
        assert!(!summary_contains_nonce_marker(&inline, NONCE));
    }
}
