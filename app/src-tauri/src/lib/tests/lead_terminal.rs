#![cfg(test)]

use super::*;

#[test]
fn lead_terminal_decision_saw_completed() {
    assert_eq!(
        lead_terminal_decision(true, false, false, false, true, false),
        LeadTerminal::None
    );
}

#[test]
fn lead_terminal_decision_saw_error() {
    let decision = lead_terminal_decision(false, true, false, false, true, false);
    assert_eq!(decision, LeadTerminal::EmitRunCloseout);

    let event = build_lead_terminal_release_event("lead-error-run", &decision, false)
        .expect("lead Error 后必须有终态释放事件");
    assert!(matches!(
        event,
        agent_event::AgentEvent::RunCloseout {
            ref run_id,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: Some(false),
        } if run_id == "lead-error-run"
    ));
}

#[test]
fn lead_terminal_decision_stopped() {
    let decision = lead_terminal_decision(false, false, false, false, false, true);
    assert_eq!(decision, LeadTerminal::EmitRunCloseout);

    let event = build_lead_terminal_release_event("lead-stopped-run", &decision, true)
        .expect("用户停止 lead 后必须有终态释放事件");
    assert!(matches!(
        event,
        agent_event::AgentEvent::RunCloseout {
            ref run_id,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: Some(true),
        } if run_id == "lead-stopped-run"
    ));
}

#[test]
fn lead_terminal_decision_nonzero_no_terminal() {
    assert_eq!(
        lead_terminal_decision(false, false, false, false, false, false),
        LeadTerminal::EmitError
    );
}

#[test]
fn lead_terminal_decision_clean_exit_no_completed() {
    assert_eq!(
        lead_terminal_decision(false, false, false, false, true, false),
        LeadTerminal::EmitCompleted
    );
}

/// Bug B（验收 b）：见过 NeedsDecision 终态事件时，即便退出码非零，也不得合成假 error、
/// 也不能当成 metadata-bearing Completed——走 EmitRunCloseout（只释放前端运行态，真实
/// NeedsDecision 事件已经在 pending_terminals 里）。
#[test]
fn lead_terminal_decision_needs_decision_no_synthetic_error() {
    let decision = lead_terminal_decision(false, false, false, true, false, false);
    assert_eq!(decision, LeadTerminal::EmitRunCloseout);
}

/// Bug B（验收 c）：Blocked 终态事件同理。
#[test]
fn lead_terminal_decision_blocked_no_synthetic_error() {
    let decision = lead_terminal_decision(false, false, true, false, false, false);
    assert_eq!(decision, LeadTerminal::EmitRunCloseout);
}

#[test]
fn lead_terminal_decision_arms_release_then_flush_barrier_terminal_last() {
    let completed =
        build_lead_terminal_release_event("lead-run", &LeadTerminal::EmitCompleted, false).unwrap();
    // 4 元组：(name, decision, interrupted, pending)。合成 error 现在由调用方在调用
    // barrier 前直接 push 进 pending（对齐生产代码 lib.rs `record_synthetic_cli_error`
    // 之后 `pending_terminals.push(event)` 的写法），不再走退休掉的 `Option<String>` 参数。
    let cases = [
        ("completed", LeadTerminal::None, false, vec![completed]),
        (
            "saw-error",
            LeadTerminal::EmitRunCloseout,
            false,
            vec![agent_event::AgentEvent::Error {
                message: "parsed error".into(),
            }],
        ),
        ("stopped", LeadTerminal::EmitRunCloseout, true, Vec::new()),
        (
            "nonzero",
            LeadTerminal::EmitError,
            false,
            vec![agent_event::AgentEvent::Error {
                message: "cli failed".into(),
            }],
        ),
        (
            "clean-fallback",
            LeadTerminal::EmitCompleted,
            false,
            Vec::new(),
        ),
    ];

    for (name, decision, interrupted, pending) in cases {
        let root = tempfile::tempdir().unwrap();
        let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
        transport
            .register_run(name, name, None, member_runner::TextGranularity::Line)
            .unwrap();
        transport.push(
            name,
            agent_event::AgentEvent::TextDelta {
                text: "stream".into(),
            },
        );
        let running = Running::default();
        let team_running = member_runner::TeamRunning::default();
        running.0.lock().unwrap().insert(
            name.into(),
            RunSlot::Finalizing {
                stop_requested: interrupted,
            },
        );
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let recorded = payloads.clone();
        let observed_running = running.clone();
        let session_id = name.to_string();
        transport.install_emitter_for_test(move |payload| {
            assert!(!observed_running.0.lock().unwrap().contains_key(&session_id));
            recorded.lock().unwrap().push(payload);
        });
        let terminals = lead_terminal_events_for_barrier(name, &decision, interrupted, pending);
        assert!(emit_terminal_after_releasing_run_slot(
            &running,
            &team_running,
            name,
            name,
            terminals,
            &transport,
            None,
        ));

        let payloads = payloads.lock().unwrap();
        let last = &payloads[0]
            .batches
            .last()
            .unwrap()
            .events
            .last()
            .unwrap()
            .event;
        assert!(
            matches!(last, agent_event::AgentEvent::Completed { .. })
                || matches!(last, agent_event::AgentEvent::RunCloseout { .. }),
            "{name} must end in a release terminal: {last:?}"
        );
    }
}

#[test]
fn lead_runtime_failures_use_barrier_and_closeout_tail() {
    for failure in ["mcp", "command-build", "process-start"] {
        let root = tempfile::tempdir().unwrap();
        let transport = event_transport::EventTransport::new_for_test(root.path().to_path_buf());
        transport
            .register_run(failure, failure, None, member_runner::TextGranularity::Line)
            .unwrap();
        let running = Running::default();
        let team_running = member_runner::TeamRunning::default();
        let terminated = AtomicBool::new(false);
        running.0.lock().unwrap().insert(
            failure.into(),
            RunSlot::Launching {
                stop_requested: false,
            },
        );
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let recorded = payloads.clone();
        transport.install_emitter_for_test(move |payload| recorded.lock().unwrap().push(payload));

        emit_lead_error_and_release(
            &running,
            &team_running,
            &terminated,
            failure,
            failure,
            &transport,
            format!("{failure} failed"),
            None,
        );

        assert!(!running.0.lock().unwrap().contains_key(failure));
        assert!(terminated.load(Ordering::SeqCst));
        let payloads = payloads.lock().unwrap();
        let events = &payloads[0].batches[0].events;
        assert!(matches!(
            events[0].event,
            agent_event::AgentEvent::Error { .. }
        ));
        assert!(matches!(
            events.last().unwrap().event,
            agent_event::AgentEvent::RunCloseout { .. }
        ));
    }
}

#[test]
fn lead_production_source_does_not_emit_legacy_agent_event() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let lead = source
        .split("fn start_lead_session(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]\nfn stop_session(").next())
        .expect("start_lead_session source slice");
    assert!(lead.contains("transport.push(&lead_run_id, event)"));
    assert!(
        !lead.contains("emit_agent_event("),
        "lead streaming and terminal events must be exclusive to EventTransport"
    );
}

/// 验收 d 的强钉法（review 收尾）：`lead_terminal_decision_nonzero_no_terminal` 只能证明
/// 「决策函数本身在这堆布尔值下算出 EmitError」，证不了「事件循环真的把 saw_blocked/
/// saw_needs_decision 见证接了线、合成 error 真的喂进了归约器」——lead_terminal_decision
/// 根本不接受退出码，纯布尔构造下「exit 4 不豁免」恒真、无从测起。改用源码切片钉住调用点
/// 本身（同款手法见 lead_production_source_does_not_emit_legacy_agent_event）：谁把
/// record_synthetic_cli_error 那行删掉、或把 lead_terminal_decision 调用点的
/// saw_blocked/saw_needs_decision 实参改回硬编码 false，这条测试立刻变红。
#[test]
fn lead_production_source_wires_synthetic_error_and_blocked_needs_decision_witnesses() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let lead = source
        .split("fn start_lead_session(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]\nfn stop_session(").next())
        .expect("start_lead_session source slice");

    // Bug A：合成 error 必须真正喂进归约器，不能退化回「只造字符串、reducer 从没见过」。
    assert!(
        lead.contains("record_synthetic_cli_error(&mut reducer"),
        "lead 合成 error 必须经 record_synthetic_cli_error 喂进 DisplayReducer"
    );

    // Bug B：lead_terminal_decision 调用点必须真的把 saw_blocked/saw_needs_decision
    // 见证传进去——不能被改回硬编码 false 或漏传参数。
    let call = lead
        .split("let terminal_decision = lead_terminal_decision(")
        .nth(1)
        .and_then(|tail| tail.split(");").next())
        .expect("lead_terminal_decision call-site slice");
    assert!(
        call.contains("saw_blocked"),
        "lead_terminal_decision 调用点必须传 saw_blocked 实参: {call}"
    );
    assert!(
        call.contains("saw_needs_decision"),
        "lead_terminal_decision 调用点必须传 saw_needs_decision 实参: {call}"
    );
}

/// G3-A T2 结构钉子（同款手法：`lead_production_source_wires_synthetic_error_and_
/// blocked_needs_decision_witnesses`）：`start_lead_session` 是巨型 `#[tauri::command]`
/// 闭包·真跑一次要 spawn 真进程，测不动其内部逐行 wiring，只能钉源码切片。
/// 钉两件事：① lead 自己 stdout 里的 Completed usage 必须被捕获进 `lead_completed_usage`
/// （不能被静默删掉退回「lead run 恒 0」）；② 收尾处必须真调 `add_session_usage` 落库
/// （且只应出现一次——双记账钉在下面的计数断言）。
#[test]
fn lead_production_source_wires_usage_capture_and_persist() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let lead = source
        .split("fn start_lead_session(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]\nfn stop_session(").next())
        .expect("start_lead_session source slice");

    assert!(
        lead.contains("lead_completed_usage = Some((*input_tokens, *output_tokens))"),
        "lead 事件循环必须从 stdout 流里的 Completed 事件捕获 usage 到 lead_completed_usage"
    );

    let usage_call_count = lead
        .matches("db::add_session_usage(&conn, &session_id_t")
        .count();
    assert_eq!(
        usage_call_count, 1,
        "lead run 落账必须恰好一次调用 add_session_usage（防双记账），实际 {usage_call_count} 次"
    );

    // 落账必须门在 `lead_completed_usage` 之上——不能改成恒定落 0/None，也不能绕过
    // lead_completed_usage 直接从别处硬编码调用（防止「调用存在但素材来源被偷换」）。
    // 用「守卫行到调用行之间距离足够近」代替脆弱的「之间不含 fn」全文扫描。
    let guard = "if let Some((input_tokens, output_tokens)) = lead_completed_usage";
    let guard_pos = lead.find(guard).expect("usage guard site");
    let call_pos = lead
        .find("db::add_session_usage(&conn, &session_id_t")
        .expect("usage call site");
    assert!(
            call_pos > guard_pos && call_pos - guard_pos < 400,
            "add_session_usage 落库必须紧跟在 `if let Some(...) = lead_completed_usage` 守卫之内（guard@{guard_pos} call@{call_pos}）"
        );
}

#[test]
fn context_compacted_latest_event_wins_in_pending_state() {
    let mut pending = None;
    remember_context_compacted(
        &mut pending,
        &agent_event::AgentEvent::ContextCompacted {
            summary: "第一次摘要".into(),
            through_message_id: 10,
        },
    );
    remember_context_compacted(
        &mut pending,
        &agent_event::AgentEvent::TextDelta {
            text: "无关事件".into(),
        },
    );
    remember_context_compacted(
        &mut pending,
        &agent_event::AgentEvent::ContextCompacted {
            summary: "最后一次摘要".into(),
            through_message_id: 20,
        },
    );

    assert_eq!(pending, Some(("最后一次摘要".into(), 20)));
}

#[test]
fn context_compacted_upsert_has_exactly_one_production_call_site() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();

    assert_eq!(
        production.matches("db::upsert_compact_state(").count(),
        1,
        "compact state must have exactly one production write call shared by solo and lead"
    );
}
