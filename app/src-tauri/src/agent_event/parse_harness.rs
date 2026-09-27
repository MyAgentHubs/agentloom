use serde_json::Value;

use super::{
    harness_blocked_message, harness_interrupted_message, harness_needs_decision_message,
    harness_needs_decision_reason, is_check_cmd_tool_event, parse_goal_criterion,
    plan_progress_text, AgentEvent, CardKind, GoalCriterionUpdate, ScopeChange, ToolStatus,
    KNOWN_HARNESS_EVENT_TYPES,
};

pub(super) fn parse_harness_run_started_event(v: &Value) -> Vec<AgentEvent> {
    let id = v
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    vec![AgentEvent::SessionStarted {
        conversation_id: id,
    }]
}

pub(super) fn parse_harness_agent_note_delta_event(payload: &Value) -> Vec<AgentEvent> {
    match payload.get("text").and_then(Value::as_str) {
        Some(t) => vec![AgentEvent::TextDelta {
            text: t.to_string(),
        }],
        None => vec![],
    }
}

pub(super) fn parse_harness_agent_reasoning_delta_event(payload: &Value) -> Vec<AgentEvent> {
    match payload.get("text").and_then(Value::as_str) {
        Some(t) => vec![AgentEvent::ThinkingDelta {
            text: t.to_string(),
        }],
        None => vec![],
    }
}

pub(super) fn parse_harness_goal_created_event(payload: &Value) -> Vec<AgentEvent> {
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    let Some(criteria_values) = payload.get("criteria").and_then(Value::as_array) else {
        return vec![];
    };
    if criteria_values.is_empty() {
        return vec![];
    }
    let criteria = criteria_values.iter().map(parse_goal_criterion).collect();
    vec![AgentEvent::GoalDeclared {
        goal: s("objective").unwrap_or("").to_string(),
        status: "frozen".into(),
        lead: None,
        criteria,
    }]
}

pub(super) fn parse_harness_goal_updated_event(payload: &Value) -> Vec<AgentEvent> {
    let criteria = payload
        .get("criteria")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(parse_goal_criterion).collect())
        .unwrap_or_default();
    vec![AgentEvent::GoalUpdated { criteria }]
}

pub(super) fn parse_harness_run_needs_decision_event(
    v: &Value,
    payload: &Value,
    locale: crate::Locale,
) -> Vec<AgentEvent> {
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    if s("reason") != Some("scope_change") {
        return vec![AgentEvent::Blocked {
            message: harness_needs_decision_message(locale, payload),
            reason: harness_needs_decision_reason(payload).map(str::to_string),
        }];
    }
    let changes: Vec<ScopeChange> = payload
        .get("changes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let detail = c.get("detail");
                    ScopeChange {
                        proposal_id: c
                            .get("proposal_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        kind: c
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        detail_text: detail
                            .and_then(|d| d.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        detail_summary: detail
                            .and_then(|d| d.get("summary"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }
                })
                .filter(|c| !c.detail_text.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    if changes.is_empty() {
        return vec![];
    }
    let run_id = v
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    vec![AgentEvent::NeedsDecision {
        run_id,
        reason: "scope_change".to_string(),
        changes,
    }]
}

pub(super) fn parse_harness_completion_evaluated_event(payload: &Value) -> Vec<AgentEvent> {
    let criteria = payload
        .get("criteria")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| GoalCriterionUpdate {
                    id: c
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    status: c
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    evidence: c
                        .get("evidence_ref")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    vec![AgentEvent::CriteriaUpdated { criteria }]
}

pub(super) fn parse_harness_tool_started_event(payload: &Value) -> Vec<AgentEvent> {
    if is_check_cmd_tool_event(payload) {
        return vec![];
    }
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    let tool = s("tool").unwrap_or("").to_string();
    let id = s("tool_call_id").unwrap_or("").to_string();
    let summary = s("command")
        .or_else(|| s("path"))
        .unwrap_or(&tool)
        .to_string();
    let card = if tool == "shell_exec" {
        CardKind::Command
    } else {
        CardKind::Compact
    };
    vec![AgentEvent::ToolStarted {
        id,
        tool,
        summary,
        card,
    }]
}

pub(super) fn parse_harness_tool_completed_event(payload: &Value) -> Vec<AgentEvent> {
    if is_check_cmd_tool_event(payload) {
        return vec![];
    }
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    let id = s("tool_call_id").unwrap_or("").to_string();
    let exit_code = payload.get("exit_code").and_then(Value::as_i64);
    let status = if exit_code.unwrap_or(0) == 0 {
        ToolStatus::Ok
    } else {
        ToolStatus::Failed
    };
    vec![AgentEvent::ToolCompleted {
        id,
        status,
        exit_code,
        output: None,
    }]
}

pub(super) fn parse_harness_tool_failed_event(payload: &Value) -> Vec<AgentEvent> {
    if is_check_cmd_tool_event(payload) {
        return vec![];
    }
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    let id = s("tool_call_id").unwrap_or("").to_string();
    let output = s("error").map(|e| e.to_string());
    vec![AgentEvent::ToolCompleted {
        id,
        status: ToolStatus::Failed,
        exit_code: None,
        output,
    }]
}

pub(super) fn parse_harness_tool_stdout_delta_event(payload: &Value) -> Vec<AgentEvent> {
    parse_harness_tool_output_delta_event(payload)
}

pub(super) fn parse_harness_tool_stderr_delta_event(payload: &Value) -> Vec<AgentEvent> {
    parse_harness_tool_output_delta_event(payload)
}

fn parse_harness_tool_output_delta_event(payload: &Value) -> Vec<AgentEvent> {
    if is_check_cmd_tool_event(payload) {
        return vec![];
    }
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    let id = s("tool_call_id").unwrap_or("").to_string();
    let text = s("text").unwrap_or("").to_string();
    vec![AgentEvent::ToolOutputDelta { id, text }]
}

pub(super) fn parse_harness_orchestration_step_completed_event(payload: &Value) -> Vec<AgentEvent> {
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    // T7a: head-overflow truncation recognizes only the solo.compact step; numeric fields (original/truncated/
    // budget_tokens) do not participate in the determination, and the event is still emitted if they are missing. All other outcomes follow the original path unchanged.
    if s("outcome") == Some("head_truncated_continue") {
        if s("step_id") != Some("solo.compact") {
            return vec![];
        }
        return vec![AgentEvent::HeadTruncated {}];
    }
    if s("outcome") != Some("objective_compacted") {
        return vec![];
    }
    let Some(summary) = s("summary").filter(|summary| !summary.is_empty()) else {
        return vec![];
    };
    let Some(through_message_id) = payload.get("through_message_id").and_then(Value::as_i64) else {
        return vec![];
    };
    vec![AgentEvent::ContextCompacted {
        summary: summary.to_string(),
        through_message_id,
    }]
}

pub(super) fn parse_harness_run_completed_event(payload: &Value) -> Vec<AgentEvent> {
    vec![AgentEvent::Completed {
        cost_usd: None,
        input_tokens: payload
            .get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64),
        output_tokens: payload
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64),
        final_text: None,
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    }]
}

pub(super) fn parse_harness_run_failed_event(payload: &Value) -> Vec<AgentEvent> {
    parse_harness_error_payload(payload)
}

pub(super) fn parse_harness_error_event(payload: &Value) -> Vec<AgentEvent> {
    parse_harness_error_payload(payload)
}

fn parse_harness_error_payload(payload: &Value) -> Vec<AgentEvent> {
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    let msg = s("error")
        .or_else(|| s("message"))
        .unwrap_or("error")
        .to_string();
    vec![AgentEvent::Error { message: msg }]
}

pub(super) fn parse_harness_run_blocked_event(
    payload: &Value,
    locale: crate::Locale,
) -> Vec<AgentEvent> {
    vec![AgentEvent::Blocked {
        message: harness_blocked_message(locale, payload),
        reason: None,
    }]
}

pub(super) fn parse_harness_run_interrupted_event(
    payload: &Value,
    locale: crate::Locale,
) -> Vec<AgentEvent> {
    vec![AgentEvent::Blocked {
        message: harness_interrupted_message(locale, payload),
        reason: None,
    }]
}

pub(super) fn parse_harness_approval_requested_event(v: &Value) -> Vec<AgentEvent> {
    let p = &v["payload"];
    let command_str = p["command"].as_str().unwrap_or_default().to_string();
    let summary_str = p["summary"].as_str().unwrap_or(&command_str).to_string();
    vec![AgentEvent::ApprovalRequested {
        approval_id: p["approval_id"].as_str().unwrap_or_default().to_string(),
        run_id: v["run_id"].as_str().unwrap_or_default().to_string(),
        tool: p["tool"].as_str().unwrap_or_default().to_string(),
        command: command_str,
        summary: summary_str,
        cwd: p["cwd"].as_str().unwrap_or_default().to_string(),
        request_kind: p["request_kind"].as_str().map(str::to_string),
        proposal_id: p["proposal_id"].as_str().map(str::to_string),
    }]
}

pub(super) fn parse_harness_approval_resolved_event(v: &Value) -> Vec<AgentEvent> {
    let p = &v["payload"];
    vec![AgentEvent::ApprovalResolved {
        approval_id: p["approval_id"].as_str().unwrap_or_default().to_string(),
        decision: p["decision"].as_str().unwrap_or_default().to_string(),
        reason: p["reason"].as_str().map(|s| s.to_string()),
    }]
}

pub(super) fn parse_harness_plan_event(
    locale: crate::Locale,
    event_type: &str,
    payload: &Value,
) -> Vec<AgentEvent> {
    plan_progress_text(locale, event_type, payload)
        .map(|text| vec![AgentEvent::TextDelta { text }])
        .unwrap_or_default()
}

pub(super) fn parse_harness_unknown_event(event_type: Option<&str>) -> Vec<AgentEvent> {
    if let Some(t) = event_type {
        if !KNOWN_HARNESS_EVENT_TYPES.contains(&t) {
            eprintln!("harness: 未知事件类型已丢弃（CONTRACT §9）: {t}");
        }
    }
    vec![]
}
