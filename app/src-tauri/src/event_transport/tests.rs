#![cfg(test)]

use super::*;
use crate::agent_event::{CardKind, ToolStatus};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;
use tempfile::{tempdir, NamedTempFile, TempDir};

fn test_transport(lane_capacity: usize) -> (TempDir, EventTransport) {
    let root = tempdir().unwrap();
    let transport = EventTransport::with_config(
        root.path().to_path_buf(),
        lane_capacity,
        128,
        Duration::from_secs(60),
    );
    (root, transport)
}

fn recorder(transport: &EventTransport) -> Arc<Mutex<Vec<BatchPayload>>> {
    let payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = payloads.clone();
    transport.install_emitter_for_test(move |payload| lock(&recorded).push(payload));
    payloads
}

fn text(text: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: text.into() }
}

fn thinking(text: &str) -> AgentEvent {
    AgentEvent::ThinkingDelta { text: text.into() }
}

fn terminal(message: &str) -> AgentEvent {
    AgentEvent::Error {
        message: message.into(),
    }
}

fn tool_started(id: &str) -> AgentEvent {
    AgentEvent::ToolStarted {
        id: id.into(),
        tool: "shell".into(),
        summary: "run".into(),
        card: CardKind::Command,
    }
}

fn tool_completed(id: &str) -> AgentEvent {
    AgentEvent::ToolCompleted {
        id: id.into(),
        status: ToolStatus::Ok,
        exit_code: Some(0),
        output: None,
    }
}

fn only_events(payloads: &Arc<Mutex<Vec<BatchPayload>>>) -> Vec<SequencedEvent> {
    let payloads = lock(payloads);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].batches.len(), 1);
    payloads[0].batches[0].events.clone()
}

#[test]
fn fan_out_sends_identical_payloads_to_sinks_in_registration_order() {
    let (_root, transport) = test_transport(8);
    let first_payloads = Arc::new(Mutex::new(Vec::new()));
    let second_payloads = Arc::new(Mutex::new(Vec::new()));
    let call_order = Arc::new(Mutex::new(Vec::new()));

    let recorded = first_payloads.clone();
    let calls = call_order.clone();
    transport
        .start(move |payload| {
            lock(&calls).push("first");
            lock(&recorded).push(payload);
        })
        .unwrap();
    let recorded = second_payloads.clone();
    let calls = call_order.clone();
    transport.add_sink(move |payload| {
        lock(&calls).push("second");
        lock(&recorded).push(payload);
    });

    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    transport.push("r", text("first"));
    transport.push("r", text("second"));
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();

    assert_eq!(*lock(&first_payloads), *lock(&second_payloads));
    assert_eq!(lock(&first_payloads).len(), 1);
    assert_eq!(*lock(&call_order), vec!["first", "second"]);
}

#[test]
fn tick_fans_out_to_every_sink() {
    let (_root, transport) = test_transport(8);
    let a = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::new(Mutex::new(Vec::new()));
    let (ra, rb) = (a.clone(), b.clone());
    transport.start(move |p| lock(&ra).push(p)).unwrap();
    transport.add_sink(move |p| lock(&rb).push(p));
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    transport.push("r", text("hello"));
    transport.tick_once_for_test();
    assert_eq!(*lock(&a), *lock(&b));
    assert_eq!(lock(&a).len(), 1);
}

#[test]
fn run_id_is_available_to_native_sinks_but_skipped_from_serialized_batches() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run(
            "run-private",
            "session-public",
            None,
            TextGranularity::Token,
        )
        .unwrap();
    transport.push("run-private", text("hello"));
    transport.tick_once_for_test();

    let payloads = lock(&payloads);
    let batch = &payloads[0].batches[0];
    assert_eq!(batch.run_id, "run-private");
    let serialized = serde_json::to_value(batch).unwrap();
    assert_eq!(serialized["session_id"], "session-public");
    assert!(
        serialized.get("run_id").is_none(),
        "run_id must remain private to native sinks"
    );
}

#[test]
fn panicking_sink_does_not_block_other_sinks_or_future_batches() {
    let (_root, transport) = test_transport(8);
    let received = Arc::new(Mutex::new(Vec::new()));
    transport.start(|_| panic!("boom")).unwrap();
    let recorded = received.clone();
    transport.add_sink(move |payload| lock(&recorded).push(payload));

    transport
        .register_run("tick", "tick", None, TextGranularity::Token)
        .unwrap();
    transport.push("tick", text("first"));
    transport.tick_once_for_test();
    assert_eq!(lock(&received).len(), 1);

    transport
        .register_run("flush", "flush", None, TextGranularity::Token)
        .unwrap();
    transport.push("flush", text("second"));
    assert!(transport
        .flush_barrier("flush", vec![terminal("done")])
        .unwrap());
    assert_eq!(lock(&received).len(), 2);
    assert_eq!(transport.high_watermarks("flush").unwrap().emitted_seq, 2);
    assert!(!transport.flush_barrier("flush", Vec::new()).unwrap());
    assert_eq!(transport.diagnostics().sink_panics, 2);
}

#[test]
fn sink_added_later_only_receives_subsequent_payloads() {
    let (_root, transport) = test_transport(8);
    let first_payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = first_payloads.clone();
    transport
        .start(move |payload| lock(&recorded).push(payload))
        .unwrap();

    transport
        .register_run("first", "first", None, TextGranularity::Token)
        .unwrap();
    transport.push("first", text("before"));
    transport
        .flush_barrier("first", vec![terminal("first done")])
        .unwrap();

    let second_payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = second_payloads.clone();
    transport.add_sink(move |payload| lock(&recorded).push(payload));
    transport
        .register_run("second", "second", None, TextGranularity::Token)
        .unwrap();
    transport.push("second", text("after"));
    transport
        .flush_barrier("second", vec![terminal("second done")])
        .unwrap();

    let first_payloads = lock(&first_payloads);
    let second_payloads = lock(&second_payloads);
    assert_eq!(first_payloads.len(), 2);
    assert_eq!(second_payloads.len(), 1);
    assert_eq!(first_payloads[1], second_payloads[0]);
    assert_eq!(second_payloads[0].batches[0].session_id, "second");
}

#[test]
fn start_still_rejects_a_second_call() {
    let (_root, transport) = test_transport(8);
    transport.start(|_| {}).unwrap();
    assert_eq!(transport.start(|_| {}), Err(TransportError::AlreadyStarted));
}

#[test]
fn preserves_order_and_sequence_with_barrier_terminal_last() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    assert_eq!(transport.push("r", text("a")), Some(1));
    assert_eq!(transport.push("r", tool_started("t")), Some(2));
    assert_eq!(transport.push("r", text("b")), Some(3));
    assert!(transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap());

    let events = only_events(&payloads);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(matches!(
        events.last().unwrap().event,
        AgentEvent::Error { .. }
    ));
    assert_eq!(
        transport.high_watermarks("r"),
        Some(HighWatermarks {
            parsed_seq: 4,
            emitted_seq: 4,
            frontend_applied_seq: 0,
        })
    );
    assert!(transport.report_frontend_applied("r", 4));
    assert_eq!(
        transport.high_watermarks("r").unwrap().frontend_applied_seq,
        4
    );
}

#[test]
fn line_and_token_text_merging_have_exact_newline_boundaries() {
    for (run_id, granularity, expected) in [
        ("line", TextGranularity::Line, "first\n\nthird"),
        ("token", TextGranularity::Token, "firstthird"),
    ] {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run(run_id, run_id, None, granularity)
            .unwrap();
        transport.push(run_id, text("first"));
        transport.push(run_id, text(""));
        transport.push(run_id, text("third"));
        transport
            .flush_barrier(run_id, vec![terminal("done")])
            .unwrap();

        let events = only_events(&payloads);
        assert_eq!(
            events[0],
            SequencedEvent {
                seq: 3,
                event: text(expected),
            }
        );
        assert_eq!(events.len(), 2, "terminal must not merge with text");
    }
}

/// 2026-07-24 dogfood 回归钉子：DeepSeek 借壳（走 claude 解析器，`ParseFn::Claude` →
/// `TextGranularity::Token`）逐 token 快吐同批合并的 `TextDelta`，Token 粒度下必须原样零缝拼接
/// ——修前误用 Line 粒度会在两段中间插 `'\n'`，把 "**DeepSeek**" 断成 "**DeepSe\nek**"，
/// markdown 渲染成单换行→视觉上词中间出现空格（用户报「DeepSe ek」的根因）。
#[test]
fn token_granularity_merges_split_word_and_bold_marker_without_inserting_chars() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    transport.push("r", text("**DeepSe"));
    transport.push("r", text("ek**"));
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();

    let events = only_events(&payloads);
    assert_eq!(
        events[0],
        SequencedEvent {
            seq: 2,
            event: text("**DeepSeek**"),
        },
        "token 粒度合并不应插入任何字符"
    );
}

/// 同上，ThinkingDelta 分支同源覆盖（coalesce 对 TextDelta/ThinkingDelta 走同一个
/// `append_text`，两个分支都要钉住，避免只改一半留下 thinking 流回归）。
#[test]
fn token_granularity_merges_thinking_delta_without_inserting_chars() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    transport.push("r", thinking("**DeepSe"));
    transport.push("r", thinking("ek**"));
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();

    let events = only_events(&payloads);
    assert_eq!(
        events[0],
        SequencedEvent {
            seq: 2,
            event: thinking("**DeepSeek**"),
        },
        "token 粒度合并 ThinkingDelta 不应插入任何字符"
    );
}

/// Line 粒度（codex：`item.completed`/`agent_message` 每条 TextDelta 是整条完整消息）
/// 行为保持原样——多条消息合并需要补 `'\n'` 分隔，否则相邻消息会黏在一起。
#[test]
fn line_granularity_still_inserts_newline_between_codex_messages() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Line)
        .unwrap();
    transport.push("r", text("first message"));
    transport.push("r", text("second message"));
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();

    let events = only_events(&payloads);
    assert_eq!(
        events[0],
        SequencedEvent {
            seq: 2,
            event: text("first message\nsecond message"),
        },
        "line 粒度仍需在消息间补换行"
    );
}

#[test]
fn thinking_merges_by_granularity_but_tool_boundaries_split_segments() {
    let (_root, transport) = test_transport(16);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Line)
        .unwrap();
    transport.push("r", text("a"));
    transport.push("r", text("b"));
    transport.push("r", tool_started("t"));
    transport.push("r", text("c"));
    transport.push("r", text("d"));
    transport.push("r", thinking("x"));
    transport.push("r", thinking("y"));
    transport.push("r", tool_completed("t"));
    transport.push("r", thinking("z"));
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();

    let events = only_events(&payloads);
    assert_eq!(events.len(), 7);
    assert_eq!(events[0].event, text("a\nb"));
    assert!(matches!(events[1].event, AgentEvent::ToolStarted { .. }));
    assert_eq!(events[2].event, text("c\nd"));
    assert_eq!(events[3].event, thinking("x\ny"));
    assert!(matches!(events[4].event, AgentEvent::ToolCompleted { .. }));
    assert_eq!(events[5].event, thinking("z"));
    assert!(matches!(events[6].event, AgentEvent::Error { .. }));
}

#[test]
fn token_thinking_merges_without_inserting_a_separator() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    transport.push("r", thinking("Received"));
    transport.push("r", thinking("."));
    transport.push("r", thinking(" Connectivity OK"));
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();

    let events = only_events(&payloads);
    assert_eq!(events[0].event, thinking("Received. Connectivity OK"));
    assert_eq!(events[0].seq, 3);
}

#[test]
fn a_global_tick_groups_runs_without_cross_run_merging() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("a", "session-a", None, TextGranularity::Token)
        .unwrap();
    transport
        .register_run("b", "session-b", None, TextGranularity::Token)
        .unwrap();
    transport.push("a", text("left"));
    transport.push("b", text("right"));
    transport.tick_once_for_test();

    let payloads = lock(&payloads);
    assert_eq!(payloads.len(), 1, "one global tick emits one payload");
    assert_eq!(payloads[0].batches.len(), 2);
    assert_eq!(payloads[0].batches[0].session_id, "session-a");
    assert_eq!(payloads[0].batches[0].events[0].event, text("left"));
    assert_eq!(payloads[0].batches[1].session_id, "session-b");
    assert_eq!(payloads[0].batches[1].events[0].event, text("right"));
}

#[test]
fn usage_deltas_sum_across_interleaved_events_at_the_last_usage_position() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    transport.push(
        "r",
        AgentEvent::UsageDelta {
            input_tokens: Some(2),
            output_tokens: None,
        },
    );
    transport.push("r", tool_started("tool-between-usage"));
    transport.push(
        "r",
        AgentEvent::UsageDelta {
            input_tokens: None,
            output_tokens: Some(5),
        },
    );
    transport.push(
        "r",
        AgentEvent::UsageDelta {
            input_tokens: Some(3),
            output_tokens: Some(7),
        },
    );
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();

    let events = only_events(&payloads);
    assert_eq!(
        events[1],
        SequencedEvent {
            seq: 4,
            event: AgentEvent::UsageDelta {
                input_tokens: Some(5),
                output_tokens: Some(12),
            },
        }
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::UsageDelta { .. }))
            .count(),
        1,
        "one drain must aggregate every usage delta even when tools interleave"
    );
    assert!(matches!(
        &events[0].event,
        AgentEvent::ToolStarted { id, .. } if id == "tool-between-usage"
    ));
}

#[test]
fn concurrent_tick_and_barriers_never_emit_after_terminal() {
    let root = tempdir().unwrap();
    let transport = EventTransport::with_config(
        root.path().to_path_buf(),
        64,
        4096,
        Duration::from_millis(1),
    );
    let payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = payloads.clone();
    transport
        .start(move |payload| {
            thread::yield_now();
            lock(&recorded).push(payload);
        })
        .unwrap();

    const RUNS: usize = 48;
    for index in 0..RUNS {
        let run = format!("run-{index}");
        transport
            .register_run(&run, &run, None, TextGranularity::Token)
            .unwrap();
        for part in 0..12 {
            transport.push(&run, text(&format!("{part},")));
        }
    }

    let mut barriers = Vec::new();
    for index in 0..RUNS {
        let transport = transport.clone();
        barriers.push(thread::spawn(move || {
            if index % 3 == 0 {
                thread::yield_now();
            }
            let run = format!("run-{index}");
            assert!(transport
                .flush_barrier(&run, vec![terminal("terminal")])
                .unwrap());
        }));
    }
    for barrier in barriers {
        barrier.join().unwrap();
    }

    let mut by_session: HashMap<String, Vec<AgentEvent>> = HashMap::new();
    for payload in lock(&payloads).iter() {
        for batch in &payload.batches {
            by_session
                .entry(batch.session_id.clone())
                .or_default()
                .extend(batch.events.iter().map(|event| event.event.clone()));
        }
    }
    assert_eq!(by_session.len(), RUNS);
    for index in 0..RUNS {
        let events = &by_session[&format!("run-{index}")];
        assert!(
            matches!(events.last(), Some(AgentEvent::Error { message }) if message == "terminal")
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::Error { .. }))
                .count(),
            1
        );
    }
}

#[test]
fn closed_push_counts_protocol_error_and_second_barrier_is_noop() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    assert!(transport
        .flush_barrier("r", vec![terminal("first")])
        .unwrap());
    assert!(!transport
        .flush_barrier("r", vec![terminal("second")])
        .unwrap());
    assert_eq!(transport.push("r", text("late")), None);
    assert_eq!(transport.diagnostics().protocol_errors, 1);

    let payloads = lock(&payloads);
    assert_eq!(payloads.len(), 1);
    assert!(matches!(
        &payloads[0].batches[0].events[0].event,
        AgentEvent::Error { message } if message == "first"
    ));
}

#[test]
fn closed_lane_is_retired_after_two_ticks_without_affecting_active_lane() {
    let (_root, transport) = test_transport(8);
    recorder(&transport);
    transport
        .register_run("closed", "closed-session", None, TextGranularity::Token)
        .unwrap();
    transport
        .register_run("active", "active-session", None, TextGranularity::Token)
        .unwrap();
    assert_eq!(transport.push("closed", text("streaming")), Some(1));
    assert!(transport
        .flush_barrier("closed", vec![terminal("done")])
        .unwrap());
    assert!(transport.report_frontend_applied("closed", 2));
    assert_eq!(lock(&transport.inner.lanes).len(), 2);

    transport.tick_once_for_test();
    assert_eq!(lock(&transport.inner.lanes).len(), 2);
    assert!(!transport
        .flush_barrier("closed", vec![terminal("duplicate")])
        .unwrap());
    assert_eq!(transport.push("closed", text("late-before-retire")), None);

    transport.tick_once_for_test();
    assert_eq!(lock(&transport.inner.lanes).len(), 1);
    assert!(lock(&transport.inner.lanes).contains_key("active"));
    assert_eq!(transport.push("closed", text("late-after-retire")), None);
    assert_eq!(transport.push("active", text("still-open")), Some(1));

    let diagnostics = transport.diagnostics();
    assert_eq!(diagnostics.protocol_errors, 2);
    assert_eq!(diagnostics.retired_runs, 1);
    assert_eq!(diagnostics.retired_parsed_seq, 2);
    assert_eq!(diagnostics.retired_emitted_seq, 2);
    assert_eq!(diagnostics.retired_frontend_applied_seq, 2);
}

#[test]
fn journal_contains_one_original_envelope_per_event_in_sequence() {
    let (root, transport) = test_transport(8);
    recorder(&transport);
    let dispatch = DispatchMeta {
        run_id: Some("r".into()),
        task_id: Some("task".into()),
        ..DispatchMeta::default()
    };
    transport
        .register_run("r", "session", Some(dispatch), TextGranularity::Line)
        .unwrap();
    transport.push("r", text("one"));
    transport.push("r", text("two"));
    transport
        .flush_barrier("r", vec![terminal("done")])
        .unwrap();
    transport.flush_journal_for_test();

    let contents = fs::read_to_string(root.path().join("r.jsonl")).unwrap();
    let lines = contents
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        3,
        "journal records originals, not coalesced output"
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(lines.iter().all(|line| line["run_id"] == "r"));
    assert!(lines.iter().all(|line| line["session_id"] == "session"));
    assert!(lines
        .iter()
        .all(|line| line["dispatch"]["task_id"] == "task"));
    assert_eq!(lines[0]["kind"], "text_delta");
    assert_eq!(lines[0]["text"], "one");
    assert_eq!(lines[1]["kind"], "text_delta");
    assert_eq!(lines[1]["text"], "two");
    assert_eq!(lines[2]["kind"], "error");
    assert_eq!(lines[2]["message"], "done");
}

#[test]
fn per_event_dispatch_survives_stream_and_terminal_batches() {
    let (_root, transport) = test_transport(8);
    let payloads = recorder(&transport);
    let base = DispatchMeta {
        run_id: Some("team-run".into()),
        assignment_id: Some("assignment-1".into()),
        ..DispatchMeta::default()
    };
    let mut dispatched = base.clone();
    dispatched.status_transition = Some(crate::agent_event::StatusTransition::Dispatched);
    dispatched.task_pack = Some("brief".into());
    let mut done = base.clone();
    done.status_transition = Some(crate::agent_event::StatusTransition::Done);

    transport
        .register_run(
            "member-lane",
            "session",
            Some(base.clone()),
            TextGranularity::Line,
        )
        .unwrap();
    transport.push_with_dispatch("member-lane", dispatched.clone(), text("subtask"));
    transport.push_with_dispatch("member-lane", base.clone(), text("answer"));
    transport
        .flush_barrier_with_dispatch("member-lane", vec![(done.clone(), terminal("done"))])
        .unwrap();

    let payloads = lock(&payloads);
    assert_eq!(payloads.len(), 1, "barrier emits one payload");
    assert_eq!(payloads[0].batches.len(), 3);
    assert_eq!(payloads[0].batches[0].dispatch, Some(dispatched));
    assert_eq!(payloads[0].batches[1].dispatch, Some(base));
    assert_eq!(payloads[0].batches[2].dispatch, Some(done));
    assert!(matches!(
        payloads[0].batches[2].events.last().unwrap().event,
        AgentEvent::Error { .. }
    ));
}

#[test]
fn journal_write_failure_is_counted_without_blocking_push() {
    let bad_root = NamedTempFile::new().unwrap();
    let transport =
        EventTransport::with_config(bad_root.path().to_path_buf(), 8, 8, Duration::from_secs(60));
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();

    let started = Instant::now();
    assert_eq!(transport.push("r", text("payload")), Some(1));
    assert!(started.elapsed() < Duration::from_millis(100));
    transport.flush_journal_for_test();
    assert_eq!(transport.diagnostics().journal_write_errors, 1);
}

#[test]
fn full_lane_backpressures_until_capacity_is_drained_without_loss() {
    let (_root, transport) = test_transport(1);
    transport.install_emitter_for_test(|_| {});
    transport
        .register_run("r", "s", None, TextGranularity::Token)
        .unwrap();
    assert_eq!(transport.push("r", text("first")), Some(1));

    let pushing = transport.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        done_tx.send(pushing.push("r", text("second"))).unwrap();
    });
    assert_eq!(
        done_rx.recv_timeout(Duration::from_millis(40)),
        Err(RecvTimeoutError::Timeout),
        "second push must block while the bounded lane is full"
    );

    transport.tick_once_for_test();
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Some(2)
    );
    worker.join().unwrap();
    transport.tick_once_for_test();
    assert_eq!(transport.high_watermarks("r").unwrap().emitted_seq, 2);
}

#[test]
fn default_tick_is_fifty_milliseconds() {
    assert_eq!(TICK_INTERVAL, Duration::from_millis(50));
}
