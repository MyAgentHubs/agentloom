#![cfg(test)]

use super::*;

#[test]
fn auth_retry_classifier_positive() {
    for message in [
        "Failed to authenticate. API Error: 401 Invalid authentication credentials",
        "API Error: 403 Forbidden",
        "OAuth access token has expired",
    ] {
        assert!(
            is_auth_error(message),
            "should classify auth error: {message}"
        );
    }
}

#[test]
fn auth_retry_classifier_negative() {
    for message in [
        "connection refused",
        "rate limit exceeded",
        "network error",
        "ordinary business validation error",
    ] {
        assert!(
            !is_auth_error(message),
            "should not classify non-auth error: {message}"
        );
    }
}

fn none() -> Vec<AgentEvent> {
    Vec::new()
}

fn sample_result() -> MemberResult {
    MemberResult {
        schema_version: 1,
        assignment_id: "a".into(),
        participant_id: "p".into(),
        status: "done".into(),
        failure_reason: None,
        changed_files: vec![],
        anchor: ResultAnchor {
            base_sha: "0".into(),
            head_sha: None,
            diff_ref: None,
            generated_from: "test".into(),
        },
        command_evidence: vec![],
        risk_inputs: RiskInputs {
            files_changed: 0,
            cmd_danger: "none".into(),
            reversibility: "reversible".into(),
        },
        decisions: vec![],
        risks: vec![],
        final_text_ref: None,
        artifact_refs: vec![],
        result_source: "raw".into(),
        requires_long_task: None,
        exit_code: None,
        stderr_tail: None,
        failure_kind: None,
    }
}

#[test]
fn parse_long_task_compact_single_line() {
    let txt = "需盯 40 分钟 CI。\n{\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"ci_watch\",\"reason\":\"超出一次性\",\"suggested_owner\":\"agentloom\"}}";
    let g = parse_requires_long_task(txt).expect("应解析");
    assert_eq!(g.kind, "ci_watch");
    assert_eq!(g.suggested_owner, "agentloom");
}

#[test]
fn parse_long_task_fenced_and_pretty() {
    let txt = "结论：\n```json\n{\n  \"status\": \"incomplete\",\n  \"requires_long_task\": {\n    \"kind\": \"train\",\n    \"reason\": \"2 小时训练\",\n    \"suggested_owner\": \"agentloom\"\n  }\n}\n```\n以上。";
    let g = parse_requires_long_task(txt).expect("围栏+pretty 也应解析");
    assert_eq!(g.kind, "train");
}

#[test]
fn parse_long_task_trailing_prose_after_block() {
    let txt = "{\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"k\",\"reason\":\"r\",\"suggested_owner\":\"agentloom\"}}\n谢谢。";
    assert_eq!(parse_requires_long_task(txt).unwrap().kind, "k");
}

#[test]
fn parse_long_task_none_for_normal_or_done() {
    assert!(parse_requires_long_task("活干完了，改了 3 个文件。").is_none());
    assert!(parse_requires_long_task("{\"status\":\"done\"}").is_none());
}

// opus NIT-A：文档化已知误报面（引用协议块 = 当前会误判·设计上可接受·非 bug·别去消除）。
#[test]
fn parse_long_task_known_falsepositive_on_quoted_block() {
    let txt = "我干完了。说明：需长任务时我会返回 {\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"x\",\"reason\":\"y\",\"suggested_owner\":\"agentloom\"}} 这样的块。";
    assert!(
        parse_requires_long_task(txt).is_some(),
        "已知误报面：正文引用协议块也会被当真信号（文档化·诚实标·非 bug·误报代价仅多显一个诚实档）"
    );
}

#[test]
fn maybe_mark_long_task_only_on_done_with_block() {
    let mut r = sample_result();
    let block = "{\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"k\",\"reason\":\"r\",\"suggested_owner\":\"agentloom\"}}";
    maybe_mark_long_task(&mut r, StatusTransition::Failed, Some(block));
    assert!(r.requires_long_task.is_none(), "failed 不该被标需长任务");
    maybe_mark_long_task(&mut r, StatusTransition::Done, Some(block));
    assert_eq!(r.requires_long_task.as_ref().unwrap().kind, "k");
}

#[test]
fn member_result_serde_roundtrip() {
    let r = MemberResult {
        schema_version: 1,
        assignment_id: "a1".into(),
        participant_id: "w1".into(),
        status: "done".into(),
        failure_reason: None,
        changed_files: vec![ChangedFile {
            path: "x.rs".into(),
            insertions: 3,
            deletions: 1,
        }],
        anchor: ResultAnchor {
            base_sha: "abc".into(),
            head_sha: None,
            diff_ref: None,
            generated_from: "worktree_diff".into(),
        },
        command_evidence: vec![CommandEvidence {
            cmd: "cargo test".into(),
            exit_code: None,
            status: "ok".into(),
            source_provider: "claude".into(),
            output_ref: None,
        }],
        risk_inputs: RiskInputs {
            files_changed: 1,
            cmd_danger: "low".into(),
            reversibility: "reversible".into(),
        },
        decisions: vec![],
        risks: vec![],
        final_text_ref: None,
        artifact_refs: vec![],
        result_source: "raw".into(),
        requires_long_task: None,
        exit_code: Some(1),
        stderr_tail: Some("boom".into()),
        failure_kind: Some("env".into()),
    };
    let j = serde_json::to_string(&r).unwrap();
    let back: MemberResult = serde_json::from_str(&j).unwrap();
    assert_eq!(back.schema_version, 1);
    assert_eq!(back.command_evidence[0].exit_code, None);
    assert_eq!(back.exit_code, Some(1));
    assert_eq!(back.stderr_tail.as_deref(), Some("boom"));
    assert_eq!(back.failure_kind.as_deref(), Some("env"));
    // 老 block 无 result 字段反序列化 = None（serde default）
    let snap: MemberResultOpt = serde_json::from_str("{}").unwrap();
    assert!(snap.result.is_none());
}

/// P1 钉子：旧快照/旧 JSON（无 exit_code/stderr_tail 字段）必须还能反序列化——
/// serde(default) 保后向兼容，别让新字段破旧存档读取。
#[test]
fn member_result_deserializes_without_new_fields_backcompat() {
    let old_json = serde_json::json!({
        "schema_version": 1,
        "assignment_id": "a1",
        "participant_id": "w1",
        "status": "failed",
        "changed_files": [],
        "anchor": {
            "base_sha": "abc",
            "generated_from": "worktree_diff",
        },
        "command_evidence": [],
        "risk_inputs": {
            "files_changed": 0,
            "cmd_danger": "low",
            "reversibility": "reversible",
        },
        "result_source": "raw",
    });
    let back: MemberResult = serde_json::from_value(old_json).expect("旧 JSON 应能反序列化");
    assert_eq!(back.exit_code, None);
    assert_eq!(back.stderr_tail, None);
}

#[derive(serde::Deserialize)]
struct MemberResultOpt {
    #[serde(default)]
    result: Option<MemberResult>,
}

#[test]
fn dispatch_meta_serializes_only_present_fields() {
    let m = DispatchMeta {
        run_id: Some("r1".into()),
        origin_participant_id: Some("worker-1".into()),
        member_name: Some("Claude".into()),
        assignment_id: Some("a1".into()),
        status_transition: Some(StatusTransition::Dispatched),
        ..Default::default()
    };
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["run_id"], "r1");
    assert_eq!(v["assignment_id"], "a1");
    assert_eq!(v["origin_participant_id"], "worker-1");
    assert_eq!(v["member_name"], "Claude");
    assert_eq!(v["status_transition"], "dispatched");
    // 未给的字段不出 key
    assert!(v.get("task_id").is_none());
    assert!(v.get("segment_id").is_none());
    assert!(v.get("parent_event_id").is_none());
}

#[test]
fn status_transition_is_copy_and_reserves_m3_variants() {
    // R6：Copy 让 fake_runner 能 matches!(w.final_status) 后复用，不被 move 走
    let st = StatusTransition::Done;
    let _a = st;
    let _b = st; // 若非 Copy 这行编不过
                 // M3 用的变体 day-1 定义好（本计划不实现停/改派逻辑，仅占位）
    assert_eq!(
        serde_json::to_value(StatusTransition::Stopped).unwrap(),
        serde_json::json!("stopped")
    );
    assert_eq!(
        serde_json::to_value(StatusTransition::Reassigned).unwrap(),
        serde_json::json!("reassigned")
    );
}

#[test]
fn goal_declared_event_serializes_with_kind_and_criteria() {
    // 方案 A：目标随事件流推。GoalDeclared 是内部 tag 事件、带 criteria 快照。
    let e = AgentEvent::GoalDeclared {
        goal: "实现 stage 2 心情记录".into(),
        status: "frozen".into(),
        lead: Some("Claude".into()),
        criteria: vec![GoalCriterion {
            id: "ac1".into(),
            claim: "mood-record 测试通过".into(),
            verifier: Some("npm test mood-record".into()),
            evidence: None,
            status: "pending".into(),
            scope: "task".into(),
        }],
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["kind"], "goal_declared");
    assert_eq!(v["goal"], "实现 stage 2 心情记录");
    assert_eq!(v["status"], "frozen");
    assert_eq!(v["lead"], "Claude");
    assert_eq!(v["criteria"][0]["claim"], "mood-record 测试通过");
    assert_eq!(v["criteria"][0]["scope"], "task");
}

#[test]
fn relativize_summary_root_file_inside_worktree() {
    assert_eq!(
        relativize_summary("/w/2026-05-31.md", std::path::Path::new("/w")),
        "2026-05-31.md"
    );
}

#[test]
fn relativize_summary_keeps_subdirectories_inside_worktree() {
    assert_eq!(
        relativize_summary("/w/src/foo.rs", std::path::Path::new("/w")),
        "src/foo.rs"
    );
}

#[test]
fn relativize_summary_uses_basename_for_absolute_path_outside_worktree() {
    assert_eq!(
        relativize_summary("/other/abs/bar.txt", std::path::Path::new("/w")),
        "bar.txt"
    );
}

#[test]
fn relativize_summary_keeps_non_path_command_unchanged() {
    assert_eq!(
        relativize_summary("ls -la", std::path::Path::new("/w")),
        "ls -la"
    );
}

#[test]
fn relativize_summary_keeps_relative_path_unchanged() {
    assert_eq!(
        relativize_summary("rel/path.md", std::path::Path::new("/w")),
        "rel/path.md"
    );
}

#[test]
fn tool_started_serializes_with_card() {
    let e = AgentEvent::ToolStarted {
        id: "t1".into(),
        tool: "Bash".into(),
        summary: "ls".into(),
        card: CardKind::Command,
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["kind"], "tool_started");
    assert_eq!(v["card"], "command");
    assert_eq!(v["tool"], "Bash");
}

#[test]
fn tool_completed_serializes_status_and_optional_fields() {
    let e = AgentEvent::ToolCompleted {
        id: "t1".into(),
        status: ToolStatus::Failed,
        exit_code: Some(1),
        output: Some("boom".into()),
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["kind"], "tool_completed");
    assert_eq!(v["status"], "failed");
    assert_eq!(v["exit_code"], 1);
    assert_eq!(v["output"], "boom");

    let ok = serde_json::to_value(AgentEvent::ToolCompleted {
        id: "t2".into(),
        status: ToolStatus::Ok,
        exit_code: None,
        output: None,
    })
    .unwrap();
    assert_eq!(ok["status"], "ok");
    assert!(ok["exit_code"].is_null());
    assert!(ok["output"].is_null());
}

#[test]
fn thinking_delta_serializes() {
    let v = serde_json::to_value(AgentEvent::ThinkingDelta { text: "hmm".into() }).unwrap();
    assert_eq!(v["kind"], "thinking_delta");
    assert_eq!(v["text"], "hmm");
}

#[test]
fn usage_delta_serializes_with_frontend_kind() {
    let v = serde_json::to_value(AgentEvent::UsageDelta {
        input_tokens: Some(100),
        output_tokens: Some(25),
    })
    .unwrap();
    assert_eq!(v["kind"], "usage_delta");
    assert_eq!(v["input_tokens"], 100);
    assert_eq!(v["output_tokens"], 25);
}

#[test]
fn run_closeout_serializes_commit_fields() {
    let e = AgentEvent::RunCloseout {
        run_id: "run-1".into(),
        commit_sha: Some("deadbeef".into()),
        files_changed: Some(3),
        insertions: Some(10),
        deletions: Some(2),
        interrupted: Some(true),
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["kind"], "run_closeout");
    assert_eq!(v["run_id"], "run-1");
    assert_eq!(v["commit_sha"], "deadbeef");
    assert_eq!(v["files_changed"], 3);
    assert_eq!(v["insertions"], 10);
    assert_eq!(v["deletions"], 2);
    assert_eq!(v["interrupted"], true);
}

#[test]
fn truncate_short_output_unchanged() {
    assert_eq!(truncate_output("hello", 32 * 1024), "hello");
}

#[test]
fn truncate_long_output_keeps_tail_and_marks() {
    let big = "x".repeat(40 * 1024);
    let out = truncate_output(&big, 32 * 1024);
    assert!(out.starts_with("…[已截断"), "应有头部标记: {}", &out[..40]);
    assert!(out.len() <= 32 * 1024 + 64, "截断后含标记不超上限+标记长度");
    assert!(out.ends_with("xxx"), "保留尾部");
}

#[test]
fn truncate_respects_utf8_boundary() {
    let s = "中".repeat(20 * 1024);
    let out = truncate_output(&s, 32 * 1024);
    assert!(out.ends_with("中"));
}

#[test]
fn generated_messages_keep_zh_and_render_en() {
    let long = "x".repeat(40 * 1024);
    assert!(
        truncate_output_for_locale(&long, 32 * 1024, crate::Locale::Zh)
            .starts_with("…[已截断 8192 字节]\n")
    );
    assert!(
        truncate_output_for_locale(&long, 32 * 1024, crate::Locale::En)
            .starts_with("…[truncated 8192 bytes]\n")
    );

    let blocked = serde_json::json!({
        "reason": "checks failed",
        "attempts": 2,
        "criteria": [
            {"id": "a", "status": "failed"},
            {"id": "b", "status": "passed"}
        ]
    });
    assert_eq!(
        harness_blocked_message(crate::Locale::Zh, &blocked),
        "checks failed（attempts=2；未过：a）"
    );
    assert_eq!(
        harness_blocked_message(crate::Locale::En, &blocked),
        "checks failed (attempts=2; not passed: a)"
    );

    let interrupted = serde_json::json!({"resume_command": "agent resume run-1"});
    assert_eq!(
        harness_interrupted_message(crate::Locale::Zh, &interrupted),
        "运行已中断（可续跑：agent resume run-1）"
    );
    assert_eq!(
        harness_interrupted_message(crate::Locale::En, &interrupted),
        "Run interrupted (resume with: agent resume run-1)"
    );

    let unknown = r#"{"type":"result","is_error":true}"#;
    assert!(matches!(
        parse_claude_line_for_locale(unknown, crate::Locale::Zh).as_slice(),
        [AgentEvent::Error { message }] if message == "未知错误"
    ));
    assert!(matches!(
        parse_claude_line_for_locale(unknown, crate::Locale::En).as_slice(),
        [AgentEvent::Error { message }] if message == "Unknown error"
    ));

    let plan_cases = [
        (
            "plan.worklist.accepted",
            serde_json::json!({"tasks": 3}),
            "\n已拆成 3 个任务。\n",
            "\nSplit into 3 tasks.\n",
        ),
        (
            "plan.worklist.bounced",
            serde_json::json!({"attempt": 1}),
            "\n第 2 次计划没通过，正在重出。\n",
            "\nPlan attempt 2 did not pass; replanning.\n",
        ),
        (
            "plan.preflight.proceed",
            serde_json::json!({"task": "t1"}),
            "\n任务 t1 开工前检查通过。\n",
            "\nTask t1 passed its preflight check.\n",
        ),
        (
            "plan.task.decision",
            serde_json::json!({"task": "t1", "decision": {"kind": "accept"}}),
            "\n任务 t1 验收结果：accept。\n",
            "\nTask t1 review result: accept.\n",
        ),
        (
            "plan.task.done",
            serde_json::json!({"task": "t1"}),
            "\n任务 t1 已通过验收。\n",
            "\nTask t1 passed review.\n",
        ),
        (
            "plan.task.blocked",
            serde_json::json!({"task": "t1", "reason": "waiting"}),
            "\n任务 t1 暂时卡住：waiting\n",
            "\nTask t1 is temporarily blocked: waiting\n",
        ),
        (
            "plan.replan.appended",
            serde_json::json!({"round": 2}),
            "\n第 2 轮补救任务已追加。\n",
            "\nRemediation tasks for round 2 were added.\n",
        ),
        (
            "plan.replan.escalated",
            serde_json::json!({}),
            "\n补救规划没有收敛：需要人工处理\n",
            "\nRemediation planning did not converge: manual intervention required\n",
        ),
        (
            "plan.replan.escalated",
            serde_json::json!({"reason": "still failing"}),
            "\n补救规划没有收敛：still failing\n",
            "\nRemediation planning did not converge: still failing\n",
        ),
    ];
    for (event_type, payload, zh, en) in plan_cases {
        let line = harness_envelope(event_type, payload);
        assert_eq!(
            parse_harness_plan_line_for_locale(&line, crate::Locale::Zh),
            vec![AgentEvent::TextDelta {
                text: zh.to_string()
            }],
            "{event_type} should render zh through the harness parser"
        );
        assert_eq!(
            parse_harness_plan_line_for_locale(&line, crate::Locale::En),
            vec![AgentEvent::TextDelta {
                text: en.to_string()
            }],
            "{event_type} should render en through the harness parser"
        );
    }
}

#[test]
fn init_line_becomes_session_started() {
    let line = r#"{"type":"system","subtype":"init","session_id":"abc-123","model":"claude"}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::SessionStarted {
            conversation_id: "abc-123".into()
        }]
    );
}

#[test]
fn text_delta_line_becomes_text_delta() {
    let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ong"}}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::TextDelta { text: "ong".into() }]
    );
}

#[test]
fn result_line_becomes_completed_with_cost_and_final_text() {
    let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"pong","total_cost_usd":0.046,"usage":{"input_tokens":3,"output_tokens":5}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::Completed {
            cost_usd: Some(0.046),
            input_tokens: Some(3),
            output_tokens: Some(5),
            final_text: Some("pong".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
    );
}

/// G3-A T1：result/Completed 事件同样要把缓存字段计入真实输入 token（与 assistant
/// usage 同一条口径，见 `combined_input_tokens` 注释）。
#[test]
fn result_line_usage_sums_cache_tokens() {
    let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"pong","total_cost_usd":0.046,"usage":{"input_tokens":3,"cache_read_input_tokens":66,"cache_creation_input_tokens":0,"output_tokens":5}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::Completed {
            cost_usd: Some(0.046),
            input_tokens: Some(69),
            output_tokens: Some(5),
            final_text: Some("pong".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
    );
}

#[test]
fn error_result_becomes_error() {
    let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"401 auth fail","total_cost_usd":0}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::Error {
            message: "401 auth fail".into()
        }]
    );
}

#[test]
fn claude_result_missing_is_error_with_success_subtype_is_completed() {
    let line = r#"{"type":"result","subtype":"success","result":"pong"}"#;
    assert!(matches!(
        parse_claude_line(line).as_slice(),
        [AgentEvent::Completed { final_text, .. }] if final_text.as_deref() == Some("pong")
    ));
}

#[test]
fn claude_result_missing_is_error_with_error_subtype_is_error() {
    let line = r#"{"type":"result","subtype":"error_max_turns","result":"max turns reached"}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::Error {
            message: "max turns reached".into()
        }]
    );
}

#[test]
fn claude_result_missing_is_error_without_success_subtype_is_error() {
    let line = r#"{"type":"result","result":"missing terminal status"}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::Error {
            message: "missing terminal status".into()
        }]
    );
}

#[test]
fn claude_result_non_bool_is_error_without_success_subtype_is_error() {
    let line = r#"{"type":"result","is_error":"false","result":"invalid terminal status"}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::Error {
            message: "invalid terminal status".into()
        }]
    );
}

#[test]
fn claude_result_non_bool_is_error_with_success_subtype_is_completed() {
    let line = r#"{"type":"result","subtype":"success","is_error":"false","result":"pong"}"#;
    assert!(matches!(
        parse_claude_line(line).as_slice(),
        [AgentEvent::Completed { final_text, .. }] if final_text.as_deref() == Some("pong")
    ));
}

#[test]
fn assistant_tool_use_becomes_tool_started() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"我来写"},{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"hello.txt","content":"hi"}}]}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::ToolStarted {
            id: "t1".into(),
            tool: "Write".into(),
            summary: "hello.txt".into(),
            card: CardKind::Compact,
        }]
    );
}

#[test]
fn claude_assistant_tool_use_and_usage_emit_both_events() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}],"usage":{"input_tokens":100,"output_tokens":25}}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![
            AgentEvent::ToolStarted {
                id: "t1".into(),
                tool: "Read".into(),
                summary: "a.rs".into(),
                card: CardKind::Compact,
            },
            AgentEvent::UsageDelta {
                input_tokens: Some(100),
                output_tokens: Some(25),
            },
        ]
    );
}

#[test]
fn claude_assistant_without_usage_emits_no_usage_delta() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"完成"}]}}"#;
    assert_eq!(parse_claude_line(line), Vec::<AgentEvent>::new());
}

#[test]
fn claude_assistant_partial_usage_preserves_missing_field_as_none() {
    let input_only =
        r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":100}}}"#;
    assert_eq!(
        parse_claude_line(input_only),
        vec![AgentEvent::UsageDelta {
            input_tokens: Some(100),
            output_tokens: None,
        }]
    );

    let output_only =
        r#"{"type":"assistant","message":{"content":[],"usage":{"output_tokens":25}}}"#;
    assert_eq!(
        parse_claude_line(output_only),
        vec![AgentEvent::UsageDelta {
            input_tokens: None,
            output_tokens: Some(25),
        }]
    );
}

/// G3-A T1：assistant usage 含缓存字段——真实输入 token 应为三者相加
/// （input_tokens + cache_read_input_tokens + cache_creation_input_tokens），
/// 不是只读 input_tokens（那会在缓存命中时严重低报）。
#[test]
fn claude_assistant_usage_sums_cache_tokens() {
    let line = r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":200,"cache_creation_input_tokens":5,"output_tokens":30}}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::UsageDelta {
            input_tokens: Some(215),
            output_tokens: Some(30),
        }]
    );
}

/// G3-A T1：缓存字段显式为 null（Anthropic 有时会发 null 而非直接省略该 key）等价于
/// 缺失——按 0 处理，不当错误、不让整体 input_tokens 塌成 None。
#[test]
fn claude_assistant_usage_null_cache_fields_treated_as_zero() {
    let line = r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":null,"cache_creation_input_tokens":null,"output_tokens":30}}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::UsageDelta {
            input_tokens: Some(10),
            output_tokens: Some(30),
        }]
    );
}

/// G3-A T1：只有缓存字段、没有 input_tokens 本尊——真实场景理论上不该出现（Anthropic
/// usage 对象只要存在就总带 input_tokens），但解析层按「缺失按 0」的既定容错处理，
/// 不因为 base 字段缺失就把整个输入侧判成 None（那两个缓存字段本身就是有效信号）。
#[test]
fn claude_assistant_usage_cache_only_no_base_input_tokens() {
    let line = r#"{"type":"assistant","message":{"content":[],"usage":{"cache_read_input_tokens":50,"output_tokens":30}}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::UsageDelta {
            input_tokens: Some(50),
            output_tokens: Some(30),
        }]
    );
}

#[test]
fn assistant_multiple_tool_use() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}},{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}]}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![
            AgentEvent::ToolStarted {
                id: "t1".into(),
                tool: "Read".into(),
                summary: "a.rs".into(),
                card: CardKind::Compact,
            },
            AgentEvent::ToolStarted {
                id: "t2".into(),
                tool: "Bash".into(),
                summary: "ls".into(),
                card: CardKind::Command,
            },
        ]
    );
}

#[test]
fn claude_tool_use_bash_is_command_card() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::ToolStarted {
            id: "t1".into(),
            tool: "Bash".into(),
            summary: "ls".into(),
            card: CardKind::Command,
        }]
    );
}

#[test]
fn claude_tool_use_read_is_compact_card() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"a.rs"}}]}}"#;
    match &parse_claude_line(line)[0] {
        AgentEvent::ToolStarted { card, .. } => assert_eq!(*card, CardKind::Compact),
        other => panic!("应为 ToolStarted: {other:?}"),
    }
}

#[test]
fn claude_tool_result_string_content_ok() {
    let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"hello\n","is_error":false}]}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::ToolCompleted {
            id: "t1".into(),
            status: ToolStatus::Ok,
            exit_code: None,
            output: Some("hello\n".into()),
        }]
    );
}

#[test]
fn claude_tool_result_array_content_concatenated() {
    let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"line1\n"},{"type":"text","text":"line2"}],"is_error":true}]}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::ToolCompleted {
            id: "t1".into(),
            status: ToolStatus::Failed,
            exit_code: None,
            output: Some("line1\nline2".into()),
        }]
    );
}

#[test]
fn claude_thinking_block_becomes_thinking_delta() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"let me think","signature":"abc"}]}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::ThinkingDelta {
            text: "let me think".into()
        }]
    );
}

#[test]
fn claude_empty_thinking_block_still_emits_event() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"","signature":"abc"}]}}"#;
    assert_eq!(
        parse_claude_line(line),
        vec![AgentEvent::ThinkingDelta { text: "".into() }]
    );
}

#[test]
fn claude_assistant_text_block_ignored() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"完成"}]}}"#;
    assert_eq!(parse_claude_line(line), Vec::<AgentEvent>::new());
}

#[test]
fn assistant_text_only_is_empty() {
    // 纯 text 的 assistant（无 tool_use）不出事件（text 走 delta / 或 result.final_text 兜底）
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"pong"}]}}"#;
    assert_eq!(parse_claude_line(line), none());
}

#[test]
fn hook_and_garbage_are_empty() {
    assert_eq!(
        parse_claude_line(r#"{"type":"system","subtype":"hook_started","hook_name":"X"}"#),
        none()
    );
    assert_eq!(parse_claude_line("not json"), none());
}

#[test]
fn codex_thread_started_becomes_session_started() {
    assert_eq!(
        parse_codex_line(r#"{"type":"thread.started","thread_id":"019e"}"#),
        vec![AgentEvent::SessionStarted {
            conversation_id: "019e".into()
        }]
    );
}

#[test]
fn parse_codex_type_error_becomes_error() {
    let line = r#"{"type":"error","message":"The 'gpt-5' model is not supported when using Codex with a ChatGPT account."}"#;
    assert_eq!(
        parse_codex_line(line),
        vec![AgentEvent::Error {
            message: "The 'gpt-5' model is not supported when using Codex with a ChatGPT account."
                .into()
        }]
    );
}

#[test]
fn parse_codex_reconnect_notice_is_ignored() {
    let line = r#"{"type":"error","message":"Reconnecting... 2/5 (request timed out)"}"#;
    assert_eq!(parse_codex_line(line), Vec::<AgentEvent>::new());
}

#[test]
fn parse_codex_non_retry_reconnecting_error_remains_visible() {
    let line = r#"{"type":"error","message":"Reconnecting to the server failed"}"#;
    assert_eq!(
        parse_codex_line(line),
        vec![AgentEvent::Error {
            message: "Reconnecting to the server failed".into()
        }]
    );
}

#[test]
fn parse_codex_turn_failed_message_becomes_error() {
    let line = r#"{"type":"turn.failed","error":{"message":"The 'gpt-5.3-codex' model is not supported when using Codex with a ChatGPT account."}}"#;
    assert_eq!(
            parse_codex_line(line),
            vec![AgentEvent::Error {
                message: "The 'gpt-5.3-codex' model is not supported when using Codex with a ChatGPT account.".into()
            }]
        );
}

#[test]
fn codex_agent_message_becomes_text_delta() {
    let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Recursion."}}"#;
    assert_eq!(
        parse_codex_line(line),
        vec![AgentEvent::TextDelta {
            text: "Recursion.".into()
        }]
    );
}

#[test]
fn codex_command_started_becomes_tool_started_command() {
    let line = r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cat sample.txt\"","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#;
    assert_eq!(
        parse_codex_line(line),
        vec![AgentEvent::ToolStarted {
            id: "item_1".into(),
            tool: "command".into(),
            summary: "cat sample.txt".into(),
            card: CardKind::Command,
        }]
    );
}

#[test]
fn codex_command_completed_ok_with_output_and_exit() {
    let line = r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cat sample.txt\"","aggregated_output":"hello\n","exit_code":0,"status":"completed"}}"#;
    assert_eq!(
        parse_codex_line(line),
        vec![AgentEvent::ToolCompleted {
            id: "item_1".into(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: Some("hello\n".into()),
        }]
    );
}

#[test]
fn codex_command_completed_failed_when_exit_nonzero() {
    let line = r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"/bin/zsh -lc \"false\"","aggregated_output":"","exit_code":1,"status":"completed"}}"#;
    match &parse_codex_line(line)[0] {
        AgentEvent::ToolCompleted {
            status, exit_code, ..
        } => {
            assert_eq!(*status, ToolStatus::Failed);
            assert_eq!(*exit_code, Some(1));
        }
        other => panic!("应为 ToolCompleted: {other:?}"),
    }
}

#[test]
fn codex_command_completed_null_exit_is_ok() {
    let line = r#"{"type":"item.completed","item":{"id":"i3","type":"command_execution","command":"x","aggregated_output":"","exit_code":null,"status":"completed"}}"#;
    match &parse_codex_line(line)[0] {
        AgentEvent::ToolCompleted {
            status, exit_code, ..
        } => {
            assert_eq!(*status, ToolStatus::Ok);
            assert_eq!(*exit_code, None);
        }
        other => panic!("应为 ToolCompleted: {other:?}"),
    }
}

#[test]
fn codex_file_change_becomes_compact_tool() {
    let started = r#"{"type":"item.started","item":{"id":"item_5","type":"file_change","changes":[{"path":"/tmp/x/out.txt","kind":"add"}],"status":"in_progress"}}"#;
    assert_eq!(
        parse_codex_line(started),
        vec![AgentEvent::ToolStarted {
            id: "item_5".into(),
            tool: "file".into(),
            summary: "add out.txt".into(),
            card: CardKind::Compact,
        }]
    );
    let completed = r#"{"type":"item.completed","item":{"id":"item_5","type":"file_change","changes":[{"path":"/tmp/x/out.txt","kind":"add"}],"status":"completed"}}"#;
    assert_eq!(
        parse_codex_line(completed),
        vec![AgentEvent::ToolCompleted {
            id: "item_5".into(),
            status: ToolStatus::Ok,
            exit_code: None,
            output: None,
        }]
    );
}

/// T24b 规则 A：codex `file_change` 的 `summary` 为可读性只留 basename（如
/// "add logo.svg"），真实完整路径丢了——前端 `imageArtifacts.ts` 靠路径形状识别图片
/// 产物，basename 没有目录分隔符会被形状过滤器拒绝。complete 事件的 `output`
/// 一直是 None（codex file_change 从不产生文本输出），这里把它复用成「本次改动里
/// 识别为图片的完整路径」，前端的通用扫描（summary+output）就能捞到——不新增字段、
/// 不碰 db::Block::Tool，复用既有 output 数据通道（同 imagePathsFromTool 的
/// CONTENT_TOOLS 豁免配套，见 imageArtifacts.ts 把 "file" 移出黑名单那一侧）。
/// 非图片改动（上面 codex_file_change_becomes_compact_tool 那条 .txt 用例）output
/// 仍是 None，不回归。
#[test]
fn codex_file_change_completed_output_carries_full_image_paths() {
    let completed = r#"{"type":"item.completed","item":{"id":"item_6","type":"file_change","changes":[{"path":"/repo/assets/logo.svg","kind":"add"},{"path":"/repo/notes.txt","kind":"add"}],"status":"completed"}}"#;
    match &parse_codex_line(completed)[0] {
        AgentEvent::ToolCompleted { output, .. } => {
            assert_eq!(output.as_deref(), Some("/repo/assets/logo.svg"));
        }
        other => panic!("应为 ToolCompleted: {other:?}"),
    }
}

#[test]
fn codex_file_change_completed_multiple_image_paths_join_with_newline() {
    let completed = r#"{"type":"item.completed","item":{"id":"item_7","type":"file_change","changes":[{"path":"/repo/a.png","kind":"add"},{"path":"/repo/b.PNG","kind":"add"}],"status":"completed"}}"#;
    match &parse_codex_line(completed)[0] {
        AgentEvent::ToolCompleted { output, .. } => {
            assert_eq!(output.as_deref(), Some("/repo/a.png\n/repo/b.PNG"));
        }
        other => panic!("应为 ToolCompleted: {other:?}"),
    }
}

#[test]
fn unwrap_shell_strips_zsh_lc() {
    assert_eq!(unwrap_shell("/bin/zsh -lc \"cat a.txt\""), "cat a.txt");
    assert_eq!(unwrap_shell("/bin/bash -lc \"ls -la\""), "ls -la");
    assert_eq!(unwrap_shell("plain command"), "plain command");
}

#[test]
fn codex_turn_completed_tokens_only() {
    let line = r#"{"type":"turn.completed","usage":{"input_tokens":18575,"output_tokens":255}}"#;
    assert_eq!(
        parse_codex_line(line),
        vec![AgentEvent::Completed {
            cost_usd: None,
            input_tokens: Some(18575),
            output_tokens: Some(255),
            final_text: None,
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
    );
}

#[test]
fn codex_other_lines_empty() {
    assert_eq!(parse_codex_line(r#"{"type":"turn.started"}"#), none());
    assert_eq!(
        parse_codex_line(
            r#"{"type":"item.completed","item":{"id":"i1","type":"reasoning","text":"x"}}"#
        ),
        none()
    );
    assert_eq!(parse_codex_line("not json"), none());
}

#[test]
fn completed_serializes_commit_fields() {
    let e = AgentEvent::Completed {
        cost_usd: Some(0.1),
        input_tokens: Some(5),
        output_tokens: Some(7),
        final_text: Some("done".into()),
        result: None,
        run_id: Some("run-1".into()),
        commit_sha: Some("deadbeef".into()),
        files_changed: Some(3),
        insertions: Some(10),
        deletions: Some(2),
        interrupted: Some(false),
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["kind"], "completed");
    assert_eq!(v["run_id"], "run-1");
    assert_eq!(v["commit_sha"], "deadbeef");
    assert_eq!(v["files_changed"], 3);
    assert_eq!(v["insertions"], 10);
    assert_eq!(v["deletions"], 2);
    assert_eq!(v["interrupted"], false);
}

#[test]
fn completed_commit_fields_null_when_empty_round() {
    let e = AgentEvent::Completed {
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        final_text: None,
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["kind"], "completed");
    assert!(v["commit_sha"].is_null());
    assert!(v["files_changed"].is_null());
}

fn harness_envelope(event_type: &str, payload: serde_json::Value) -> String {
    serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": event_type,
        "payload": payload,
    })
    .to_string()
}

mod harness;
