#![cfg(test)]

use super::*;

fn harness_envelope_with_run_id(
    event_type: &str,
    run_id: &str,
    payload: serde_json::Value,
) -> String {
    serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": run_id,
        "client_session_id": "s1",
        "workspace": "/w",
        "type": event_type,
        "payload": payload,
    })
    .to_string()
}

#[test]
fn parse_harness_run_completed_with_usage() {
    let evs = parse_harness_line(&harness_envelope(
        "run.completed",
        serde_json::json!({
            "usage": {
                "input_tokens": 123,
                "output_tokens": 45,
            }
        }),
    ));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Completed {
            cost_usd: None,
            input_tokens: Some(123),
            output_tokens: Some(45),
            ..
        }]
    ));
}

#[test]
fn parse_harness_run_completed_without_usage() {
    let evs = parse_harness_line(&harness_envelope("run.completed", serde_json::json!({})));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Completed {
            input_tokens: None,
            output_tokens: None,
            ..
        }]
    ));
}

#[test]
fn parse_harness_run_completed_with_null_usage() {
    let evs = parse_harness_line(&harness_envelope(
        "run.completed",
        serde_json::json!({ "usage": null }),
    ));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Completed {
            input_tokens: None,
            output_tokens: None,
            ..
        }]
    ));
}

#[test]
fn parse_harness_run_completed_parses_usage_fields_independently() {
    let evs = parse_harness_line(&harness_envelope(
        "run.completed",
        serde_json::json!({
            "usage": {
                "input_tokens": "not a number",
                "output_tokens": 45,
            }
        }),
    ));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Completed {
            input_tokens: None,
            output_tokens: Some(45),
            ..
        }]
    ));
}

#[test]
fn known_harness_event_types_cover_engine_vocabulary() {
    // 直接与 harness-agent/src/vocabulary.rs 对齐；engine 加事件时同步 app 白名单。
    let vocabulary_source = include_str!("../../../../../harness-agent/src/vocabulary.rs");
    let engine_event_types = vocabulary_source
        .lines()
        .skip_while(|line| !line.contains("pub const VOCABULARY"))
        .skip(1)
        .take_while(|line| line.trim() != "];")
        .filter_map(|line| {
            line.trim()
                .strip_prefix('"')
                .and_then(|line| line.strip_suffix("\","))
        })
        .collect::<Vec<_>>();

    assert!(
        !engine_event_types.is_empty(),
        "failed to read harness-agent event vocabulary"
    );
    for event_type in engine_event_types {
        assert!(
            KNOWN_HARNESS_EVENT_TYPES.contains(&event_type),
            "{event_type} is in the engine vocabulary; sync the app whitelist"
        );
    }
}

#[test]
fn parse_harness_plan_known_ignored_events_yield_empty() {
    let ignored = [
        "plan.preflight.considered",
        "plan.preflight.pre_green",
        "plan.preflight.refine_requested",
        "plan.preflight.refine_planned",
        "plan.preflight.refine_bounced",
        "plan.preflight.refine_escalated",
        "plan.preflight.refine_appended",
        "plan.preflight.superseded",
        "plan.preflight.suspended",
        "plan.preflight.escalated",
        "plan.task.report",
        "plan.task.reverified",
        "plan.task.advisory",
        "plan.task.scope_formatting_advisory",
        "plan.replan.considered",
        "plan.replan.planned",
        "plan.replan.bounced",
        "plan.replan.reverified",
    ];

    for event_type in ignored {
        assert!(
            KNOWN_HARNESS_EVENT_TYPES.contains(&event_type),
            "{event_type} should be known"
        );
        let evs = parse_harness_line(&harness_envelope(
            event_type,
            serde_json::json!({ "task": "t1" }),
        ));
        assert!(evs.is_empty(), "{event_type} should be ignored");
    }
}

#[test]
fn parse_harness_plan_progress_events_to_text() {
    let cases = [
        (
            "plan.worklist.accepted",
            serde_json::json!({ "tasks": 3, "attempt": 0 }),
            vec!["3", "任务"],
        ),
        (
            "plan.worklist.bounced",
            serde_json::json!({ "attempt": 1 }),
            vec!["2", "计划"],
        ),
        (
            "plan.preflight.proceed",
            serde_json::json!({ "task": "t1" }),
            vec!["t1", "检查"],
        ),
        (
            "plan.task.decision",
            serde_json::json!({ "task": "t1", "decision": { "kind": "green" } }),
            vec!["t1", "green"],
        ),
        (
            "plan.task.done",
            serde_json::json!({ "task": "t1" }),
            vec!["t1", "通过"],
        ),
        (
            "plan.task.blocked",
            serde_json::json!({ "task": "t2", "reason": "failed_by_acceptance" }),
            vec!["t2", "failed_by_acceptance"],
        ),
        (
            "plan.replan.appended",
            serde_json::json!({ "round": 2 }),
            vec!["2", "追加"],
        ),
        (
            "plan.replan.escalated",
            serde_json::json!({ "reason": "overall_red" }),
            vec!["overall_red", "规划"],
        ),
    ];

    for (event_type, payload, expected) in cases {
        let evs = parse_harness_line(&harness_envelope(event_type, payload));
        assert!(
            matches!(
                evs.as_slice(),
                [AgentEvent::TextDelta { text }]
                    if expected.iter().all(|needle| text.contains(needle))
            ),
            "{event_type} should produce progress text, got {evs:?}"
        );
    }
}

#[test]
fn parse_harness_plan_needs_decision_maps_to_blocked() {
    let evs = parse_harness_plan_line(&harness_envelope_with_run_id(
        "run.needs_decision",
        "run_plan_1",
        serde_json::json!({
            "reason": "overall_red",
            "next_step": "总验收红，回 Planner 追加任务"
        }),
    ));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Blocked { message, .. }]
            if message.contains("overall_red") && message.contains("总验收红")
    ));
}

#[test]
fn harness_needs_decision_message_prefers_known_blocked_reason() {
    // 白名单内的 blocked_reason（no_progress/stuck_repeating/
    // budget_exhausted_still_progressing）顶替笼统的顶层 reason=blocked_questions，
    // 让 app 侧收工人话化映射能认出具体停手缘由。
    for code in [
        "no_progress",
        "stuck_repeating",
        "budget_exhausted_still_progressing",
    ] {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "blocked_reason": code,
            "trigger": "harness",
        });
        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            code
        );
    }
}

#[test]
fn harness_needs_decision_message_ignores_agent_free_text_blocked_reason() {
    // agent 主动调 block_with_questions 时 blocked_reason 是模型自由文本（不在白名单
    // 里）——必须维持用顶层 reason="blocked_questions" 泛化展示，不能把任意模型文本
    // 误当系统状态码显示。
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "blocked_reason": "需要用户确认是否可以删除生产数据库",
        "trigger": "agent",
    });
    assert_eq!(
        harness_needs_decision_message(crate::Locale::Zh, &payload),
        "blocked_questions"
    );
}

#[test]
fn harness_needs_decision_message_agent_triggered_lookalike_value_is_not_promoted() {
    // 顺手加固（opus 对抗审）：blocked_reason 字面值恰好等于白名单词（如 "no_progress"），
    // 但 trigger="agent"（模型自己调 block_with_questions 时碰巧/学舌写出这个词，不是
    // 系统真的判定 no_progress）——不能被顶替，必须维持笼统的顶层
    // reason="blocked_questions"，不能把模型语句冒充系统状态码。
    for code in [
        "no_progress",
        "stuck_repeating",
        "budget_exhausted_still_progressing",
    ] {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "blocked_reason": code,
            "trigger": "agent",
        });
        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            "blocked_questions",
            "trigger=agent 时字面命中白名单的 blocked_reason={code} 也不该被顶替"
        );
    }
}

#[test]
fn harness_needs_decision_message_context_budget_exhausted_keeps_next_step() {
    let payload = serde_json::json!({
        "reason": "context_budget_exhausted",
        "next_step": "拆小任务 / 换更大上下文的模型",
    });
    assert_eq!(
        harness_needs_decision_message(crate::Locale::Zh, &payload),
        "context_budget_exhausted: 拆小任务 / 换更大上下文的模型"
    );
}

#[test]
fn harness_needs_decision_message_surfaces_agent_questions_and_diagnosis_in_chinese() {
    let evs = parse_harness_line_for_locale(
        &harness_envelope(
            "run.needs_decision",
            serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": "需要产品决策",
                "questions": ["要保留草稿吗？", "谁可以批准？", "截止日期是哪天？"],
                "agent_diagnosis": "当前需求存在三个未决点",
                "failed_criteria": ["criterion-1"],
                "evidence_refs": ["evidence-1"],
                "attempts_summary": { "turns": 2, "attempts": 1 },
                "trigger": "agent",
            }),
        ),
        crate::Locale::Zh,
    );

    assert_eq!(
            evs,
            vec![AgentEvent::Blocked {
                message: "blocked_questions:\n\n需要你回答：\n\n- 要保留草稿吗？\n- 谁可以批准？\n- 截止日期是哪天？\n\nagent 的判断：当前需求存在三个未决点"
                    .to_string(),
                reason: None,
            }]
        );
}

#[test]
fn harness_needs_decision_message_surfaces_agent_questions_and_diagnosis_in_english() {
    let evs = parse_harness_line_for_locale(
        &harness_envelope(
            "run.needs_decision",
            serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": "A product decision is required",
                "questions": ["Keep the draft?", "Who can approve?", "What is the deadline?"],
                "agent_diagnosis": "Three decisions are still open",
                "trigger": "agent",
            }),
        ),
        crate::Locale::En,
    );

    assert_eq!(
            evs,
            vec![AgentEvent::Blocked {
                message: "blocked_questions:\n\nQuestions for you:\n\n- Keep the draft?\n- Who can approve?\n- What is the deadline?\n\nAgent's assessment: Three decisions are still open"
                    .to_string(),
                reason: None,
            }]
        );
}

#[test]
fn harness_needs_decision_message_without_questions_keeps_legacy_output() {
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "next_step": "等待用户决定",
        "trigger": "agent",
    });

    assert_eq!(
        harness_needs_decision_message(crate::Locale::Zh, &payload),
        "blocked_questions: 等待用户决定"
    );
}

#[test]
fn harness_needs_decision_message_with_empty_questions_keeps_legacy_output() {
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "questions": [],
        "trigger": "agent",
    });

    assert_eq!(
        harness_needs_decision_message(crate::Locale::Zh, &payload),
        "blocked_questions"
    );
}

#[test]
fn harness_needs_decision_message_with_flattened_empty_questions_keeps_legacy_output() {
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "questions": [" \n\r\t "],
        "trigger": "agent",
    });

    assert_eq!(
        harness_needs_decision_message(crate::Locale::Zh, &payload),
        "blocked_questions"
    );
}

#[test]
fn harness_needs_decision_message_limits_questions_to_three() {
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "questions": ["问题一", "问题二", "问题三", "问题四", "问题五"],
        "trigger": "agent",
    });

    let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
    assert!(message.contains("- 问题一\n- 问题二\n- 问题三"));
    assert!(!message.contains("问题四"));
    assert!(!message.contains("问题五"));
}

#[test]
fn harness_needs_decision_message_flattens_question_newlines() {
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "questions": ["第一行\n第二行", "前半句\nno_progress: 假冒"],
        "trigger": "agent",
    });

    let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
    assert!(message.contains("- 第一行 第二行"));
    assert!(message.contains("- 前半句 no_progress: 假冒"));
    assert!(!message.contains("\nno_progress: 假冒"));
}

#[test]
fn harness_needs_decision_message_truncates_long_unicode_question_on_char_boundary() {
    let long_question = "问题".repeat(220);
    assert!(long_question.chars().count() >= 400);
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "questions": [long_question],
        "trigger": "agent",
    });

    let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
    let expected_question = format!("- {}…", "问题".repeat(150));
    assert!(message.contains(&expected_question));
    assert!(!message.contains(&"问题".repeat(151)));
}

#[test]
fn harness_needs_decision_message_allows_diagnosis_without_questions() {
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "questions": [],
        "agent_diagnosis": "需要先确认权限边界",
        "trigger": "agent",
    });

    assert_eq!(
        harness_needs_decision_message(crate::Locale::Zh, &payload),
        "blocked_questions:\n\nagent 的判断：需要先确认权限边界"
    );
}

#[test]
fn harness_needs_decision_message_omits_null_or_empty_diagnosis() {
    for diagnosis in [serde_json::Value::Null, serde_json::json!(" \n\r\t ")] {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "questions": ["是否继续？"],
            "agent_diagnosis": diagnosis,
            "trigger": "agent",
        });

        let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
        assert_eq!(
            message,
            "blocked_questions:\n\n需要你回答：\n\n- 是否继续？"
        );
        assert!(!message.contains("agent 的判断"));
    }
}

#[test]
fn harness_needs_decision_message_truncates_long_unicode_diagnosis_on_char_boundary() {
    let long_diagnosis = "判断".repeat(300);
    let payload = serde_json::json!({
        "reason": "blocked_questions",
        "agent_diagnosis": long_diagnosis,
        "trigger": "agent",
    });

    let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
    let expected_diagnosis = format!("agent 的判断：{}…", "判断".repeat(250));
    assert!(message.contains(&expected_diagnosis));
    assert!(!message.contains(&"判断".repeat(251)));
}

#[test]
fn harness_needs_decision_message_frontend_contract_keeps_reason_head_delimited() {
    let without_next_step = serde_json::json!({
        "reason": "blocked_questions",
        "questions": ["可以继续吗？"],
        "trigger": "agent",
    });
    let with_next_step = serde_json::json!({
        "reason": "blocked_questions",
        "next_step": "先确认范围",
        "questions": ["可以继续吗？"],
        "trigger": "agent",
    });

    assert!(
        harness_needs_decision_message(crate::Locale::Zh, &without_next_step)
            .starts_with("blocked_questions:")
    );
    assert!(
        harness_needs_decision_message(crate::Locale::Zh, &with_next_step)
            .starts_with("blocked_questions:")
    );
}

#[test]
fn parse_harness_needs_decision_agent_questions_keep_structured_reason_empty() {
    let evs = parse_harness_line(&harness_envelope(
        "run.needs_decision",
        serde_json::json!({
            "reason": "blocked_questions",
            "blocked_reason": "请用户决定是否继续",
            "questions": ["是否继续？"],
            "agent_diagnosis": "范围尚未确认",
            "trigger": "agent",
        }),
    ));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Blocked { message, reason }]
            if reason.is_none() && message.contains("是否继续？")
    ));
}

/// 本刀钉子：`AgentEvent::Blocked.reason` 只在白名单命中（`trigger=="harness"` 且
/// `blocked_reason` 在 `HARNESS_BLOCKED_REASON_CODES` 里）时才有值——覆盖
/// budget_exhausted_still_progressing 这个下游（member_runner.rs）要分流的具体值。
#[test]
fn parse_harness_needs_decision_budget_exhausted_carries_structured_reason() {
    let evs = parse_harness_line(&harness_envelope(
        "run.needs_decision",
        serde_json::json!({
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        }),
    ));
    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Blocked { reason, .. }]
            if reason.as_deref() == Some("budget_exhausted_still_progressing")
    ));
}

/// 白名单命中但 trigger=="agent"（模型自己调 block_with_questions 冒充白名单词）——
/// 结构化 reason 必须是 None，不能被模型语句冒充成系统状态码。
#[test]
fn parse_harness_needs_decision_agent_triggered_lookalike_has_no_structured_reason() {
    let evs = parse_harness_line(&harness_envelope(
        "run.needs_decision",
        serde_json::json!({
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "agent",
        }),
    ));
    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Blocked { reason, .. }] if reason.is_none()
    ));
}

/// 本刀钉子（第四类·context_budget_exhausted）：单轮上下文 token 预算溢出——payload
/// 没有 blocked_reason/trigger 字段，顶层 reason 直接就是硬编码字面量
/// "context_budget_exhausted"（emit 点见 harness-agent run_loop.rs 的 fit_to_budget
/// 溢出分支）。`AgentEvent::Blocked.reason` 必须原样透出这个字面值，供下游
/// member_runner.rs 分流成第四类 failure_kind="context_exhausted"（跟
/// "budget_exhausted_still_progressing" 那类轮次预算耗尽是两回事，别混）。
#[test]
fn parse_harness_needs_decision_context_budget_exhausted_carries_structured_reason() {
    let evs = parse_harness_line(&harness_envelope(
        "run.needs_decision",
        serde_json::json!({
            "reason": "context_budget_exhausted",
            "turn": 3,
            "estimate_tokens": 200_000,
            "budget_tokens": 180_000,
            "next_step": "拆小任务 / 换更大上下文的模型",
        }),
    ));
    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Blocked { reason, .. }]
            if reason.as_deref() == Some("context_budget_exhausted")
    ));
}

/// 伪造面探针：agent 主动触发的 block_with_questions 顶层 reason 恒硬编码
/// "blocked_questions"（模型自由文本落的是 blocked_reason 字段，根本碰不到顶层
/// reason）——这里构造一个「模型即便把 blocked_reason 写成字面
/// "context_budget_exhausted" 来碰瓷」的 payload，结构化 reason 必须仍是 None：
/// 顶层 reason 没有变成目标字面值，判据不该被 blocked_reason 里的同名词绕过。
#[test]
fn parse_harness_needs_decision_agent_cannot_forge_context_budget_exhausted_via_blocked_reason() {
    let evs = parse_harness_line(&harness_envelope(
        "run.needs_decision",
        serde_json::json!({
            "reason": "blocked_questions",
            "blocked_reason": "context_budget_exhausted",
            "trigger": "agent",
        }),
    ));
    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::Blocked { reason, .. }] if reason.is_none()
    ));
}

/// run.blocked / run.interrupted 不经过 needs_decision 白名单逻辑——reason 恒 None
/// （这两条路径没有 budget_exhausted 语义，别误带出结构化值）。
#[test]
fn parse_harness_run_blocked_and_interrupted_have_no_structured_reason() {
    let blocked = parse_harness_line(&harness_envelope(
        "run.blocked",
        serde_json::json!({ "reason": "blocked_questions" }),
    ));
    assert!(matches!(
        blocked.as_slice(),
        [AgentEvent::Blocked { reason, .. }] if reason.is_none()
    ));

    let interrupted =
        parse_harness_line(&harness_envelope("run.interrupted", serde_json::json!({})));
    assert!(matches!(
        interrupted.as_slice(),
        [AgentEvent::Blocked { reason, .. }] if reason.is_none()
    ));
}

#[test]
fn parse_harness_plan_scope_change_still_maps_to_needs_decision() {
    let evs = parse_harness_plan_line(&harness_envelope_with_run_id(
        "run.needs_decision",
        "run_7",
        serde_json::json!({
            "reason": "scope_change",
            "changes": [{
                "proposal_id": "p1",
                "kind": "scope",
                "detail": { "text": "把后端接口纳入改动" }
            }]
        }),
    ));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::NeedsDecision { run_id, reason, changes }]
            if run_id == "run_7" && reason == "scope_change" && changes.len() == 1
    ));
}

fn harness_plan_test_line(event_type: &str, payload: serde_json::Value) -> String {
    serde_json::json!({
        "schema_version": "harness.runtime.v1",
        "event_id": "evt_test",
        "seq": 1,
        "ts": "2026-07-03T00:00:00Z",
        "run_id": "plan_test",
        "workspace": "/tmp/agentloom-test",
        "type": event_type,
        "payload": payload
    })
    .to_string()
}

fn apply_harness_plan_filter(filter: &mut HarnessPlanDisplayFilter, line: &str) -> Vec<AgentEvent> {
    filter.apply(line, parse_harness_plan_line(line))
}

#[test]
fn harness_plan_display_filter_flushes_answer_only_note_on_completed() {
    let mut filter = HarnessPlanDisplayFilter::default();
    let note = harness_plan_test_line(
        "agent.note.delta",
        serde_json::json!({ "text": "已进入 plan 模式；这条请求只需要回复。" }),
    );

    assert!(apply_harness_plan_filter(&mut filter, &note).is_empty());

    let completed = harness_plan_test_line("run.completed", serde_json::json!({}));
    let events = apply_harness_plan_filter(&mut filter, &completed);

    assert!(matches!(
        events.as_slice(),
        [
            AgentEvent::TextDelta { text },
            AgentEvent::Completed { .. }
        ] if text.contains("已进入 plan 模式")
    ));
}

#[test]
fn harness_plan_display_filter_discards_chunked_planner_json_before_plan_event() {
    let mut filter = HarnessPlanDisplayFilter::default();
    for chunk in [
        "{\n  \"tasks\":",
        " [{\"id\":\"t1\",\"intent\":\"write file\"}],\n",
        "  \"depends_on\": []\n}",
    ] {
        let note = harness_plan_test_line("agent.note.delta", serde_json::json!({ "text": chunk }));
        assert!(apply_harness_plan_filter(&mut filter, &note).is_empty());
    }

    let accepted =
        harness_plan_test_line("plan.worklist.accepted", serde_json::json!({ "tasks": 1 }));
    let events = apply_harness_plan_filter(&mut filter, &accepted);

    assert!(matches!(
        events.as_slice(),
        [AgentEvent::TextDelta { text }]
            if text.contains("已拆成 1 个任务")
                && !text.contains("\"tasks\"")
                && !text.contains("write file")
    ));
}

#[test]
fn parse_harness_plan_line_hides_raw_notes_and_reasoning() {
    let raw_worklist = r#"{"tasks":[{"id":"t1","intent":"raw"}]}"#;

    assert!(parse_harness_plan_line(&harness_envelope(
        "agent.note.delta",
        serde_json::json!({ "text": raw_worklist })
    ))
    .is_empty());
    assert!(parse_harness_plan_line(&harness_envelope(
        "agent.reasoning.delta",
        serde_json::json!({ "text": "private model thought" })
    ))
    .is_empty());
}

#[test]
fn parse_harness_plan_line_keeps_answer_only_note() {
    let evs = parse_harness_plan_line(&harness_envelope(
        "agent.note.delta",
        serde_json::json!({
            "text": "已进入 plan 模式；这条请求明确要求不改文件、只回复。"
        }),
    ));

    assert_eq!(
        evs,
        vec![AgentEvent::TextDelta {
            text: "已进入 plan 模式；这条请求明确要求不改文件、只回复。".into()
        }]
    );
}

#[test]
fn parse_harness_plan_line_keeps_plan_progress_text() {
    let evs = parse_harness_plan_line(&harness_envelope(
        "plan.worklist.accepted",
        serde_json::json!({ "tasks": 1 }),
    ));

    assert!(matches!(
        evs.as_slice(),
        [AgentEvent::TextDelta { text }] if text.contains("已拆成 1 个任务")
    ));
}

#[test]
fn parse_harness_new_event_types_return_empty_and_no_panic() {
    // "context.terrain.attached" 和 "safety_net.checkpoint" 是引擎新增事件，
    // 目前消费方不映射 → 应返回空 vec 且不 panic（在 KNOWN 表里，静默忽略）。
    for event_type in ["context.terrain.attached", "safety_net.checkpoint"] {
        assert!(
            KNOWN_HARNESS_EVENT_TYPES.contains(&event_type),
            "{event_type} should be known"
        );
        let evs = parse_harness_line(&harness_envelope(event_type, serde_json::json!({})));
        assert!(
            evs.is_empty(),
            "{event_type} should yield empty vec, got {evs:?}"
        );
    }
}

#[test]
fn context_compacted_valid_payload_maps_fields() {
    let events = parse_harness_line(&harness_envelope(
        "orchestration.step.completed",
        serde_json::json!({
            "step_id": "solo.compact",
            "turn": 0,
            "outcome": "objective_compacted",
            "summary": "压实后的摘要",
            "through_message_id": 42,
            "original_tokens": 1000,
            "compacted_tokens": 200,
            "budget_tokens": 800
        }),
    ));

    assert_eq!(
        events,
        vec![AgentEvent::ContextCompacted {
            summary: "压实后的摘要".into(),
            through_message_id: 42,
        }]
    );
}

#[test]
fn context_compacted_golden_event_parses() {
    let events = parse_harness_line_for_locale(
        include_str!(
            "../../../../../harness-agent/tests/fixtures/objective-compacted-event-golden.json"
        ),
        crate::Locale::Zh,
    );

    assert_eq!(
            events,
            vec![AgentEvent::ContextCompacted {
                summary: "## Primary Request and Intent\nShip the deterministic sample.\n## Key Technical Concepts\ncheckpoint\n## Files and Code\n(none)\n## Errors and Fixes\n(none)\n## Pending Jobs\n(none)\n## Current Work\nverify event consumers\n## Next Step\nfinish\n## Critical Context\npreserve the event contract".into(),
                through_message_id: 27,
            }]
        );
}

#[test]
fn context_truncated_head_truncated_outcome_maps_to_event() {
    let events = parse_harness_line(&harness_envelope(
        "orchestration.step.completed",
        serde_json::json!({
            "step_id": "solo.compact",
            "turn": 0,
            "outcome": "head_truncated_continue",
            "original_tokens": 13200,
            "truncated_tokens": 9000,
            "budget_tokens": 8000
        }),
    ));

    assert_eq!(events, vec![AgentEvent::HeadTruncated {}]);
}

#[test]
fn context_truncated_accepts_payload_without_numeric_fields() {
    let events = parse_harness_line(&harness_envelope(
        "orchestration.step.completed",
        serde_json::json!({
            "step_id": "solo.compact",
            "outcome": "head_truncated_continue"
        }),
    ));

    assert_eq!(events, vec![AgentEvent::HeadTruncated {}]);
}

#[test]
fn context_truncated_nonmatching_step_or_outcome_is_silently_dropped() {
    for payload in [
        // 对的 outcome、错的 step_id（别的编排步骤借用同名 outcome）
        serde_json::json!({
            "step_id": "lead.compact",
            "outcome": "head_truncated_continue"
        }),
        // 对的 outcome、缺 step_id
        serde_json::json!({ "outcome": "head_truncated_continue" }),
        // 对的 step_id、别的 outcome
        serde_json::json!({
            "step_id": "solo.compact",
            "outcome": "head_truncate_failed"
        }),
        serde_json::json!({
            "step_id": "solo.compact",
            "outcome": "head_truncated_continue_extra"
        }),
    ] {
        let events = parse_harness_line(&harness_envelope("orchestration.step.completed", payload));
        assert!(events.is_empty(), "unexpected events: {events:?}");
    }
}

#[test]
fn context_truncated_does_not_disturb_objective_compacted_path() {
    let events = parse_harness_line(&harness_envelope(
        "orchestration.step.completed",
        serde_json::json!({
            "step_id": "solo.compact",
            "outcome": "objective_compacted",
            "summary": "压实后的摘要",
            "through_message_id": 42
        }),
    ));

    assert_eq!(
        events,
        vec![AgentEvent::ContextCompacted {
            summary: "压实后的摘要".into(),
            through_message_id: 42,
        }]
    );
}

#[test]
fn context_compacted_failed_outcome_is_silently_dropped() {
    let events = parse_harness_line(&harness_envelope(
        "orchestration.step.completed",
        serde_json::json!({
            "outcome": "objective_compact_failed",
            "summary": "不应落库",
            "through_message_id": 42
        }),
    ));

    assert!(events.is_empty());
}

#[test]
fn context_compacted_missing_or_empty_summary_is_silently_dropped() {
    for payload in [
        serde_json::json!({
            "outcome": "objective_compacted",
            "through_message_id": 42
        }),
        serde_json::json!({
            "outcome": "objective_compacted",
            "summary": "",
            "through_message_id": 42
        }),
    ] {
        assert!(
            parse_harness_line(&harness_envelope("orchestration.step.completed", payload))
                .is_empty()
        );
    }
}

#[test]
fn context_compacted_non_integer_watermark_is_silently_dropped() {
    for through_message_id in [
        serde_json::json!("42"),
        serde_json::json!(42.5),
        serde_json::Value::Null,
    ] {
        let events = parse_harness_line(&harness_envelope(
            "orchestration.step.completed",
            serde_json::json!({
                "outcome": "objective_compacted",
                "summary": "摘要",
                "through_message_id": through_message_id
            }),
        ));
        assert!(events.is_empty());
    }
}
