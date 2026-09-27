use super::*;

fn harness_goal_verifier(criterion: &Value) -> Option<String> {
    let verifier = criterion.get("verifier")?;
    match verifier.get("kind").and_then(Value::as_str) {
        Some("verifiable") => {
            let check_cmd = verifier
                .get("check_cmd")
                .and_then(Value::as_str)
                .unwrap_or("");
            let success = verifier.get("success")?;
            if success.as_str() == Some("exit_zero") {
                return Some(format!("cmd: {check_cmd}"));
            }
            success
                .get("stdout_contains")
                .and_then(Value::as_str)
                .map(|s| format!("contains:{s}: {check_cmd}"))
        }
        Some("judgmental") => verifier
            .get("rubric")
            .and_then(Value::as_str)
            .map(|rubric| format!("judge: {rubric}")),
        _ => None,
    }
}

pub(super) fn parse_goal_criterion(criterion: &Value) -> GoalCriterion {
    GoalCriterion {
        id: criterion
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        claim: criterion
            .get("claim")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        verifier: harness_goal_verifier(criterion),
        evidence: criterion
            .get("evidence_ref")
            .and_then(Value::as_str)
            .map(|s| s.to_string()),
        status: criterion
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        scope: criterion
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("run")
            .to_string(),
    }
}

pub(super) fn is_check_cmd_tool_event(payload: &Value) -> bool {
    payload.get("tool").and_then(Value::as_str) == Some("check_cmd")
}

pub(super) fn harness_blocked_message(locale: crate::Locale, payload: &Value) -> String {
    let reason = payload
        .get("reason")
        .or_else(|| payload.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("run blocked");
    let Some(attempts) = payload.get("attempts").and_then(Value::as_u64) else {
        return reason.to_string();
    };
    let ids = payload
        .get("criteria")
        .and_then(Value::as_array)
        .map(|criteria| {
            criteria
                .iter()
                .filter(|criterion| {
                    !matches!(
                        criterion.get("status").and_then(Value::as_str),
                        Some("passed" | "waived")
                    )
                })
                .filter_map(|criterion| criterion.get("id").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    match locale {
        crate::Locale::Zh => format!("{reason}（attempts={attempts}；未过：{ids}）"),
        crate::Locale::En => format!("{reason} (attempts={attempts}; not passed: {ids})"),
    }
}

/// Render an interruption visibly instead of dropping it.
/// The payload has no reason or error; include the resume command when present.
pub(super) fn harness_interrupted_message(locale: crate::Locale, payload: &Value) -> String {
    match (
        locale,
        payload.get("resume_command").and_then(Value::as_str),
    ) {
        (crate::Locale::Zh, Some(cmd)) if !cmd.trim().is_empty() => {
            format!("运行已中断（可续跑：{cmd}）")
        }
        (crate::Locale::Zh, _) => "运行已中断".to_string(),
        (crate::Locale::En, Some(cmd)) if !cmd.trim().is_empty() => {
            format!("Run interrupted (resume with: {cmd})")
        }
        (crate::Locale::En, _) => "Run interrupted".to_string(),
    }
}

/// System-generated decision reasons accepted from `blocked_reason` when the top-level reason is
/// the generic `blocked_questions`. Agent-provided `blocked_reason` values remain free text and must
/// not be interpreted as system status codes. Requiring `trigger=="harness"` prevents an agent tool
/// call from imitating one of these trusted values.
const HARNESS_BLOCKED_REASON_CODES: [&str; 3] = [
    "no_progress",
    "stuck_repeating",
    "budget_exhausted_still_progressing",
];

/// Recognize the separate context-budget signal from its top-level reason. This event is emitted
/// before the model is called, has neither `blocked_reason` nor `trigger`, and uses a fixed reason.
/// Agent-triggered decision requests use `blocked_questions` at the top level, so no additional
/// trigger check is needed for this value.
fn harness_context_budget_exhausted_reason(payload: &Value) -> Option<&'static str> {
    match payload.get("reason").and_then(Value::as_str) {
        Some("context_budget_exhausted") => Some("context_budget_exhausted"),
        _ => None,
    }
}

/// Return a verified system reason or `None`. Message rendering and structured blocked events share
/// this trust check so their interpretations cannot drift.
pub(super) fn harness_needs_decision_reason(payload: &Value) -> Option<&str> {
    let blocked_reason = payload.get("blocked_reason").and_then(Value::as_str);
    let trigger = payload.get("trigger").and_then(Value::as_str);
    if let Some(br) = blocked_reason {
        if trigger == Some("harness") && HARNESS_BLOCKED_REASON_CODES.contains(&br) {
            return Some(br);
        }
    }
    harness_context_budget_exhausted_reason(payload)
}

fn flatten_and_truncate_needs_decision_detail(value: &str, max_chars: usize) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= max_chars {
        return flattened;
    }

    let mut truncated = flattened.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

pub(super) fn harness_needs_decision_message(locale: crate::Locale, payload: &Value) -> String {
    let reason = harness_needs_decision_reason(payload).unwrap_or_else(|| {
        payload
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("needs_decision")
    });
    let next_step = payload
        .get("next_step")
        .and_then(Value::as_str)
        .unwrap_or("");
    let next_step_is_empty = next_step.trim().is_empty();
    let head = if next_step_is_empty {
        reason.to_string()
    } else {
        format!("{reason}: {next_step}")
    };

    let questions = payload
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(3)
        .filter_map(Value::as_str)
        .map(|question| flatten_and_truncate_needs_decision_detail(question, 300))
        .filter(|question| !question.is_empty())
        .collect::<Vec<_>>();
    let diagnosis = payload
        .get("agent_diagnosis")
        .and_then(Value::as_str)
        .map(|diagnosis| flatten_and_truncate_needs_decision_detail(diagnosis, 500))
        .filter(|diagnosis| !diagnosis.is_empty());

    let mut sections = Vec::new();
    if !questions.is_empty() {
        let label = match locale {
            crate::Locale::Zh => "需要你回答：",
            crate::Locale::En => "Questions for you:",
        };
        let questions = questions
            .into_iter()
            .map(|question| format!("- {question}"))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("{label}\n\n{questions}"));
    }
    if let Some(diagnosis) = diagnosis {
        let label = match locale {
            crate::Locale::Zh => "agent 的判断：",
            crate::Locale::En => "Agent's assessment: ",
        };
        sections.push(format!("{label}{diagnosis}"));
    }

    if sections.is_empty() {
        return head;
    }

    let detail = format!("\n\n{}", sections.join("\n\n"));
    if next_step_is_empty {
        format!("{head}:{detail}")
    } else {
        format!("{head}{detail}")
    }
}

pub(super) fn plan_progress_text(
    locale: crate::Locale,
    event_type: &str,
    payload: &Value,
) -> Option<String> {
    let task = payload.get("task").and_then(Value::as_str).unwrap_or("");
    let reason = payload.get("reason").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "plan.worklist.accepted" => {
            let tasks = payload.get("tasks").and_then(Value::as_u64).unwrap_or(0);
            Some(match locale {
                crate::Locale::Zh => format!("\n已拆成 {tasks} 个任务。\n"),
                crate::Locale::En => format!("\nSplit into {tasks} tasks.\n"),
            })
        }
        "plan.worklist.bounced" => {
            let attempt = payload.get("attempt").and_then(Value::as_u64).unwrap_or(0) + 1;
            Some(match locale {
                crate::Locale::Zh => format!("\n第 {attempt} 次计划没通过，正在重出。\n"),
                crate::Locale::En => {
                    format!("\nPlan attempt {attempt} did not pass; replanning.\n")
                }
            })
        }
        "plan.preflight.proceed" => Some(match locale {
            crate::Locale::Zh => format!("\n任务 {task} 开工前检查通过。\n"),
            crate::Locale::En => format!("\nTask {task} passed its preflight check.\n"),
        }),
        "plan.task.decision" => {
            let decision = payload
                .get("decision")
                .and_then(|d| d.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            Some(match locale {
                crate::Locale::Zh => format!("\n任务 {task} 验收结果：{decision}。\n"),
                crate::Locale::En => format!("\nTask {task} review result: {decision}.\n"),
            })
        }
        "plan.task.done" => Some(match locale {
            crate::Locale::Zh => format!("\n任务 {task} 已通过验收。\n"),
            crate::Locale::En => format!("\nTask {task} passed review.\n"),
        }),
        "plan.task.blocked" => Some(match locale {
            crate::Locale::Zh => format!("\n任务 {task} 暂时卡住：{reason}\n"),
            crate::Locale::En => format!("\nTask {task} is temporarily blocked: {reason}\n"),
        }),
        "plan.replan.appended" => {
            let round = payload.get("round").and_then(Value::as_u64).unwrap_or(0);
            Some(match locale {
                crate::Locale::Zh => format!("\n第 {round} 轮补救任务已追加。\n"),
                crate::Locale::En => {
                    format!("\nRemediation tasks for round {round} were added.\n")
                }
            })
        }
        "plan.replan.escalated" => {
            let msg = if reason.is_empty() {
                match locale {
                    crate::Locale::Zh => "需要人工处理",
                    crate::Locale::En => "manual intervention required",
                }
            } else {
                reason
            };
            Some(match locale {
                crate::Locale::Zh => format!("\n补救规划没有收敛：{msg}\n"),
                crate::Locale::En => {
                    format!("\nRemediation planning did not converge: {msg}\n")
                }
            })
        }
        _ => None,
    }
}

pub fn parse_harness_line(line: &str) -> Vec<AgentEvent> {
    parse_harness_line_for_locale(line, crate::Locale::Zh)
}

pub(crate) fn parse_harness_line_for_locale(line: &str, locale: crate::Locale) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    match v.get("type").and_then(Value::as_str) {
        Some("run.started") => parse_harness_run_started_event(&v),
        Some("agent.note.delta") => parse_harness_agent_note_delta_event(&payload),
        Some("agent.reasoning.delta") => parse_harness_agent_reasoning_delta_event(&payload),
        Some("goal.created") => parse_harness_goal_created_event(&payload),
        Some("goal.updated") => parse_harness_goal_updated_event(&payload),
        Some("run.needs_decision") => parse_harness_run_needs_decision_event(&v, &payload, locale),
        Some("completion.evaluated") => parse_harness_completion_evaluated_event(&payload),
        Some("tool.started") => parse_harness_tool_started_event(&payload),
        Some("tool.completed") => parse_harness_tool_completed_event(&payload),
        Some("tool.failed") => parse_harness_tool_failed_event(&payload),
        Some("tool.stdout.delta") => parse_harness_tool_stdout_delta_event(&payload),
        Some("tool.stderr.delta") => parse_harness_tool_stderr_delta_event(&payload),
        Some("orchestration.step.completed") => {
            parse_harness_orchestration_step_completed_event(&payload)
        }
        Some("run.completed") => parse_harness_run_completed_event(&payload),
        Some("run.failed") => parse_harness_run_failed_event(&payload),
        Some("error") => parse_harness_error_event(&payload),
        Some("run.blocked") => parse_harness_run_blocked_event(&payload, locale),
        Some("run.interrupted") => parse_harness_run_interrupted_event(&payload, locale),
        Some("approval.requested") => parse_harness_approval_requested_event(&v),
        Some("approval.resolved") => parse_harness_approval_resolved_event(&v),
        Some(t) if t.starts_with("plan.") => parse_harness_plan_event(locale, t, &payload),
        other => parse_harness_unknown_event(other),
    }
}

#[derive(Default)]
pub struct HarnessPlanDisplayFilter {
    pending_note: String,
    decided: bool,
}

impl HarnessPlanDisplayFilter {
    pub fn apply(&mut self, line: &str, events: Vec<AgentEvent>) -> Vec<AgentEvent> {
        let (line_type, note_text) = harness_line_type_and_note_text(line);
        match line_type.as_deref() {
            Some("agent.note.delta") if !self.decided => {
                if let Some(text) = note_text {
                    self.pending_note.push_str(&text);
                }
                vec![]
            }
            Some("agent.reasoning.delta") => vec![],
            Some(t) if t.starts_with("plan.") => {
                // Structured plan progress is authoritative, so discard buffered raw planner notes.
                // Do not latch `decided`: replanning must continue buffering and suppressing notes.
                self.pending_note.clear();
                events
            }
            Some("run.completed") => {
                let mut out = self.flush_pending_answer();
                out.extend(events);
                out
            }
            Some("run.blocked")
            | Some("run.needs_decision")
            | Some("run.failed")
            | Some("run.interrupted") => {
                let mut out = self.flush_pending_answer();
                out.extend(events);
                out
            }
            _ => events,
        }
    }

    fn flush_pending_answer(&mut self) -> Vec<AgentEvent> {
        self.decided = true;
        if self.pending_note.trim().is_empty()
            || looks_like_raw_planner_note(self.pending_note.as_str())
        {
            self.pending_note.clear();
            return vec![];
        }
        vec![AgentEvent::TextDelta {
            text: std::mem::take(&mut self.pending_note),
        }]
    }
}

fn harness_line_type_and_note_text(line: &str) -> (Option<String>, Option<String>) {
    let Ok(v): Result<serde_json::Value, _> = serde_json::from_str(line) else {
        return (None, None);
    };
    let line_type = v
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let note_text = if line_type.as_deref() == Some("agent.note.delta") {
        v.get("payload")
            .and_then(|payload| payload.get("text"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    } else {
        None
    };
    (line_type, note_text)
}

fn looks_like_raw_planner_note(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with('[')) && trimmed.contains("\"tasks\"")
}

pub fn parse_harness_plan_line(line: &str) -> Vec<AgentEvent> {
    parse_harness_plan_line_for_locale(line, crate::Locale::Zh)
}

pub(crate) fn parse_harness_plan_line_for_locale(
    line: &str,
    locale: crate::Locale,
) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    match v.get("type").and_then(Value::as_str) {
        Some("agent.note.delta") => {
            let text = v
                .get("payload")
                .and_then(|payload| payload.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let trimmed = text.trim_start();
            if trimmed.starts_with('{') && trimmed.contains("\"tasks\"") {
                vec![]
            } else {
                parse_harness_line_for_locale(line, locale)
            }
        }
        Some("agent.reasoning.delta") => vec![],
        _ => parse_harness_line_for_locale(line, locale),
    }
}
