#![cfg(test)]

use super::*;
fn test_inner_for_history(
    session_repo_provider: impl Fn(&str) -> Result<Option<String>, String> + Send + Sync + 'static,
    session_history_provider: impl Fn(&str, Option<i64>, i64) -> Result<Vec<SessionHistoryRow>, String>
        + Send
        + Sync
        + 'static,
) -> (Arc<Inner>, Receiver<(u64, LiveQueueItem)>) {
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    let inner = Arc::new(Inner {
        settings: Box::new(|_| None),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: test_active_room_resolver(),
        k_room_provider: Box::new(|_| None),
        session_index_snapshot_provider: Box::new(|| None),
        milestone_replay_provider: Box::new(|| None),
        session_runtime_replay_provider: Box::new(|| None),
        pair_hello_handler: Box::new(|_| None),
        pair_done_handler: Box::new(|_| PairDoneAction::Rejected),
        registry: test_registry(),
        refresh_handler: test_refresh_handler(),
        registry_snapshot_provider: test_registry_snapshot_provider(),
        registry_rebase_provider: test_registry_rebase_provider(),
        registry_high_water_provider: test_registry_high_water_provider(),
        input_send_handler: Box::new(|_| Some(AckOutcome::Failed)),
        input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(|_| AckOutcome::Failed),
        upstream_tx,
        milestone_tx,
        session_repo_provider: Box::new(session_repo_provider),
        session_history_provider: Box::new(session_history_provider),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    });
    *lock(&inner.state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    inner.state.advance_generation_and_set_gate(true);
    (inner, upstream_rx)
}

fn history_row(message_id: i64, role: &str, text: String) -> SessionHistoryRow {
    history_row_with_revision(message_id, role, text, 1)
}

fn history_row_with_revision(
    message_id: i64,
    role: &str,
    text: String,
    revision: i64,
) -> SessionHistoryRow {
    let content_json = serde_json::json!([{"type": "text", "text": text}]);
    SessionHistoryRow {
        content_raw: serde_json::to_string(&content_json).unwrap(),
        message_id,
        role: role.to_owned(),
        content_json,
        revision,
    }
}

#[test]
fn history_pagination_latest_and_earliest_pages_have_exact_next_before() {
    let latest_rows: Vec<_> = (1..=51)
        .rev()
        .map(|id| history_row(id, "assistant", format!("message-{id}")))
        .collect();
    let latest = build_history_page("history-session", None, latest_rows);
    let messages = latest.payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), HISTORY_PAGE_MAX_ROWS);
    assert_eq!(messages.first().unwrap()["message_id"], 2);
    assert_eq!(messages.last().unwrap()["message_id"], 51);
    assert_eq!(latest.payload["before_message_id"], Value::Null);
    assert_eq!(latest.payload["next_before"], 2);

    let earliest = build_history_page(
        "history-session",
        Some(3),
        vec![
            history_row(2, "assistant", "second".to_owned()),
            history_row(1, "user", "first".to_owned()),
        ],
    );
    assert_eq!(earliest.payload["before_message_id"], 3);
    assert_eq!(earliest.payload["messages"][0]["message_id"], 1);
    assert_eq!(earliest.payload["messages"][1]["message_id"], 2);
    assert_eq!(earliest.payload["next_before"], Value::Null);
}

#[test]
fn history_budget_truncates_page_without_skipping_the_older_cursor() {
    let page = build_history_page(
        "history-budget",
        None,
        vec![
            history_row(2, "assistant", "a".repeat(30 * 1024)),
            history_row(1, "user", "b".repeat(30 * 1024)),
        ],
    );
    assert_eq!(page.oversized_dropped, 0);
    assert_eq!(page.payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page.payload["messages"][0]["message_id"], 2);
    assert_eq!(page.payload["next_before"], 2);
    assert!(serde_json::to_vec(&page.payload).unwrap().len() <= HISTORY_SEND_BUDGET_BYTES);
}

#[test]
fn history_tool_output_uses_existing_truncation_limit_without_rewriting_other_blocks() {
    let content_json = serde_json::json!([
        {"type": "text", "text": "keep verbatim"},
        {
            "type": "tool",
            "id": "tool-1",
            "tool": "shell",
            "summary": "ran",
            "card": "command",
            "status": "ok",
            "exit_code": 0,
            "output": "x".repeat(OUTPUT_TRUNCATE_BYTES + 17),
        }
    ]);
    let page = build_history_page(
        "history-tool",
        None,
        vec![SessionHistoryRow {
            message_id: 7,
            role: "assistant".to_owned(),
            content_raw: serde_json::to_string(&content_json).unwrap(),
            content_json,
            revision: 1,
        }],
    );
    assert_eq!(
        page.payload["messages"][0]["blocks"][0]["text"],
        "keep verbatim"
    );
    let truncated_output = page.payload["messages"][0]["blocks"][1]["output"]
        .as_str()
        .unwrap();
    assert_eq!(truncated_output.len(), OUTPUT_TRUNCATE_BYTES);
    // msgfix1 T7 B2：history 口截断同样必须带可见化标记（与 msg.completed 口共用
    // truncate_history_tool_outputs/truncate_utf8_with_marker，两口同一份行为）。
    assert!(
        truncated_output.ends_with(TOOL_OUTPUT_TRUNCATION_MARKER),
        "截断的工具输出必须带 {TOOL_OUTPUT_TRUNCATION_MARKER:?} 标记"
    );
}

#[test]
fn truncate_history_tool_outputs_leaves_short_output_untouched_without_marker() {
    // msgfix1 T7 B2：未真正发生截断时不该附加标记——短输出原样透传。
    let content_json = serde_json::json!([{
        "type": "tool",
        "id": "tool-1",
        "tool": "shell",
        "summary": "ran",
        "card": "command",
        "status": "ok",
        "exit_code": 0,
        "output": "short output, well under the cap",
    }]);
    let result = truncate_history_tool_outputs(content_json);
    assert_eq!(result[0]["output"], "short output, well under the cap");
    assert!(
        !result[0]["output"]
            .as_str()
            .unwrap()
            .contains(TOOL_OUTPUT_TRUNCATION_MARKER),
        "未截断的输出不该带截断标记"
    );
}

#[test]
fn history_oversized_single_message_is_downgraded_to_preview_and_acknowledged() {
    let provider_calls = Arc::new(AtomicU64::new(0));
    let calls = Arc::clone(&provider_calls);
    let oversized_text = "z".repeat(HISTORY_SEND_BUDGET_BYTES + 1024);
    let expected_content_raw =
        serde_json::to_string(&serde_json::json!([{"type": "text", "text": &oversized_text}]))
            .unwrap();
    let (inner, upstream_rx) = test_inner_for_history(
        |_| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())),
        move |session, before, max_rows| {
            assert_eq!(session, "history-oversized");
            assert_eq!(before, None);
            assert_eq!(max_rows, (HISTORY_PAGE_MAX_ROWS + 1) as i64);
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![history_row_with_revision(
                9,
                "assistant",
                oversized_text.clone(),
                4,
            )])
        },
    );
    let k_room = Zeroizing::new([17_u8; 32]);
    let envelope = seal_command_envelope(
        &k_room,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        0,
        "control",
        "history-oversized",
        "cmd-history-oversized",
        &serde_json::json!({
            "t": "control.history",
            "session": "history-oversized",
            "before_message_id": Value::Null,
        }),
    );

    // msgfix1 T3（设计稿 §A）：超预算的单条 history 消息不再让页面变空——降级为
    // preview + content_ref，`history_oversized_dropped` 计数语义改为"降级次数"。
    assert_eq!(
        handle_command_envelope(&inner, &envelope, Some(&k_room)),
        Some(input_ack_json("cmd-history-oversized", AckOutcome::Ok))
    );
    assert_eq!(provider_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        inner
            .state
            .history_oversized_dropped
            .load(Ordering::Relaxed),
        1
    );
    let (_, queued) = upstream_rx.try_recv().unwrap();
    let LiveQueueItem::Prebuilt(item) = queued else {
        panic!("history response must use the prebuilt live queue path");
    };
    assert_eq!(item.session.as_deref(), Some("history-oversized"));
    assert_eq!(item.t, "history");
    let messages = item.payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "oversized row must survive as a preview");
    assert_eq!(messages[0]["message_id"], 9);
    assert!(messages[0]["blocks"][0]["text"]
        .as_str()
        .unwrap()
        .ends_with(OVERSIZED_PREVIEW_TRUNCATION_NOTICE));
    let content_ref = &messages[0]["content_ref"];
    assert_eq!(content_ref["message_id"], 9);
    assert_eq!(content_ref["revision"], 4);
    assert_eq!(
        content_ref["total_bytes"],
        expected_content_raw.len() as u64
    );
    assert_eq!(
        content_ref["content_sha256"],
        sha256_hex_lower(expected_content_raw.as_bytes())
    );
    assert!(
        serde_json::to_vec(&item.payload).unwrap().len() <= HISTORY_SEND_BUDGET_BYTES,
        "downgraded page must itself pass the budget"
    );
    assert_eq!(item.payload["next_before"], Value::Null);
    assert_eq!(
        item.client_msg_id,
        derive_client_msg_id("history|history-oversized|cmd-history-oversized|latest")
    );
}

/// msgfix1 T3（设计稿 §A）：一整窗全部由超限消息组成——旧行为是整页变空、逼着命令臂
/// 循环推进游标去够更老的一条可读消息（该机制的旧版本见本文件历史）；新行为是每一条都
/// 降级为 preview + content_ref，一页足以同时带回全部，不再需要多次查询。
#[test]
fn history_full_oversized_window_downgrades_every_row_to_preview_in_one_page() {
    const OVERSIZED_ROW_COUNT: i64 = 5;
    let provider_calls = Arc::new(AtomicU64::new(0));
    let calls = Arc::clone(&provider_calls);
    let (inner, upstream_rx) = test_inner_for_history(
        |_| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())),
        move |session, before, max_rows| {
            assert_eq!(session, "history-many-oversized");
            assert_eq!(before, None);
            assert_eq!(max_rows, (HISTORY_PAGE_MAX_ROWS + 1) as i64);
            calls.fetch_add(1, Ordering::Relaxed);
            Ok((1..=OVERSIZED_ROW_COUNT)
                .rev()
                .map(|id| {
                    history_row_with_revision(
                        id,
                        "assistant",
                        "z".repeat(HISTORY_SEND_BUDGET_BYTES + 1024),
                        id, // revision 与 message_id 同值，仅用于逐条核对没有串号。
                    )
                })
                .collect())
        },
    );
    let k_room = Zeroizing::new([21_u8; 32]);
    let envelope = seal_command_envelope(
        &k_room,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        0,
        "control",
        "history-many-oversized",
        "cmd-history-many-oversized",
        &serde_json::json!({
            "t": "control.history",
            "session": "history-many-oversized",
            "before_message_id": Value::Null,
        }),
    );

    assert_eq!(
        handle_command_envelope(&inner, &envelope, Some(&k_room)),
        Some(input_ack_json("cmd-history-many-oversized", AckOutcome::Ok))
    );
    // 一页已经把全部消息以 preview 形式带回，命令臂不需要再推进游标查下一窗。
    assert_eq!(provider_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        inner
            .state
            .history_oversized_dropped
            .load(Ordering::Relaxed),
        OVERSIZED_ROW_COUNT as u64
    );
    let (_, LiveQueueItem::Prebuilt(item)) = upstream_rx.try_recv().unwrap() else {
        panic!("history response must use the prebuilt live queue path");
    };
    assert_eq!(item.payload["before_message_id"], Value::Null);
    let messages = item.payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), OVERSIZED_ROW_COUNT as usize);
    for (index, message) in messages.iter().enumerate() {
        // wire 输出按 message_id 升序。
        let expected_id = (index as i64) + 1;
        assert_eq!(message["message_id"], expected_id);
        assert!(message["blocks"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with(OVERSIZED_PREVIEW_TRUNCATION_NOTICE));
        assert_eq!(message["content_ref"]["message_id"], expected_id);
        assert_eq!(message["content_ref"]["revision"], expected_id);
    }
    assert!(
        serde_json::to_vec(&item.payload).unwrap().len() <= HISTORY_SEND_BUDGET_BYTES,
        "downgraded page must itself pass the budget"
    );
    assert_eq!(item.payload["next_before"], Value::Null);
}

/// msgfix1 T3（缺口①·M0 §10.9 同一姿势）：`control.history` 入队目标队列满时，此前会
/// 无条件回 `AckOutcome::Ok`（客户端以为帧已在路上，实际从未入队、永不会到达）。现在必须
/// 如实回 `Failed`，让客户端知道要重试。
#[test]
fn history_upstream_queue_full_returns_failed_ack_instead_of_fake_ok() {
    let (inner, _upstream_rx) = test_inner_for_history(
        |_| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())),
        |_, _, _| Ok(Vec::new()),
    );
    // `test_inner_for_history` 的 upstream_tx 容量固定为 4——先塞满，让随后 history
    // 响应的 try_send 必然命中 `TrySendError::Full`。
    for _ in 0..4 {
        inner
            .upstream_tx
            .try_send((0, LiveQueueItem::Batch(text_delta_payload("filler"))))
            .expect("filler sends must succeed before the queue is full");
    }

    let k_room = Zeroizing::new([23_u8; 32]);
    let envelope = seal_command_envelope(
        &k_room,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        0,
        "control",
        "history-queue-full",
        "cmd-history-queue-full",
        &serde_json::json!({
            "t": "control.history",
            "session": "history-queue-full",
            "before_message_id": Value::Null,
        }),
    );

    assert_eq!(
        handle_command_envelope(&inner, &envelope, Some(&k_room)),
        Some(input_ack_json("cmd-history-queue-full", AckOutcome::Failed))
    );
}

#[test]
fn history_attribution_gate_rejects_before_query_and_returns_failed_ack() {
    let history_calls = Arc::new(AtomicU64::new(0));
    let calls = Arc::clone(&history_calls);
    let (inner, upstream_rx) = test_inner_for_history(
        |_| Ok(Some("other-repo".to_owned())),
        move |_, _, _| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        },
    );
    let k_room = Zeroizing::new([18_u8; 32]);
    let envelope = seal_command_envelope(
        &k_room,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        0,
        "control",
        "history-wrong-repo",
        "cmd-history-rejected",
        &serde_json::json!({
            "t": "control.history",
            "session": "history-wrong-repo",
            "before_message_id": 10,
        }),
    );

    assert_eq!(
        handle_command_envelope(&inner, &envelope, Some(&k_room)),
        Some(input_ack_json("cmd-history-rejected", AckOutcome::Failed))
    );
    assert_eq!(history_calls.load(Ordering::Relaxed), 0);
    assert!(upstream_rx.try_recv().is_err());
}

#[test]
fn history_session_over_limit_is_rejected_before_attribution_or_query() {
    let repo_calls = Arc::new(AtomicU64::new(0));
    let repo_calls_for_provider = Arc::clone(&repo_calls);
    let history_calls = Arc::new(AtomicU64::new(0));
    let history_calls_for_provider = Arc::clone(&history_calls);
    let (inner, upstream_rx) = test_inner_for_history(
        move |_| {
            repo_calls_for_provider.fetch_add(1, Ordering::Relaxed);
            Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned()))
        },
        move |_, _, _| {
            history_calls_for_provider.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        },
    );
    let session = "s".repeat(SESSION_ID_MAX_BYTES + 1);
    let k_room = Zeroizing::new([22_u8; 32]);
    let envelope = seal_command_envelope(
        &k_room,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        0,
        "control",
        &session,
        "cmd-history-long-session",
        &serde_json::json!({
            "t": "control.history",
            "session": session,
            "before_message_id": Value::Null,
        }),
    );

    assert_eq!(
        handle_command_envelope(&inner, &envelope, Some(&k_room)),
        Some(input_ack_json(
            "cmd-history-long-session",
            AckOutcome::Failed
        ))
    );
    assert_eq!(repo_calls.load(Ordering::Relaxed), 0);
    assert_eq!(history_calls.load(Ordering::Relaxed), 0);
    assert!(upstream_rx.try_recv().is_err());
}

#[test]
fn history_prebuilt_live_frame_keeps_kind_and_passes_through_drain_repo_gate() {
    let state = GatewayInnerState::default();
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let repo_calls = Arc::new(AtomicU64::new(0));
    let calls = Arc::clone(&repo_calls);
    let provider: SessionRepoProvider = Box::new(move |session| {
        calls.fetch_add(1, Ordering::Relaxed);
        Ok(Some(
            if session == "history-allowed" {
                TEST_DEFAULT_ACTIVE_REPO_ID
            } else {
                "other-repo"
            }
            .to_owned(),
        ))
    });
    let item = |session: &str, client_msg_id: &str| MilestoneItem {
        session: Some(session.to_owned()),
        t: "history".to_owned(),
        payload: history_payload(session, None, &[], None),
        client_msg_id: client_msg_id.to_owned(),
    };
    let mut cache = HashMap::new();
    let mut epoch_seen = 0;
    let allowed = prepare_prebuilt_live_for_drain(
        &state,
        item("history-allowed", "history-client-allowed"),
        &provider,
        &mut cache,
        &mut epoch_seen,
    )
    .expect("same-repo prebuilt live frame must pass the drain gate");
    let blocked = prepare_prebuilt_live_for_drain(
        &state,
        item("history-blocked", "history-client-blocked"),
        &provider,
        &mut cache,
        &mut epoch_seen,
    );

    assert_eq!(repo_calls.load(Ordering::Relaxed), 2);
    assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 1);
    assert!(
        blocked.is_none(),
        "cross-repo prebuilt frame must be filtered"
    );
    assert_eq!(allowed.kind, "live");
    assert_eq!(allowed.session, "history-allowed");
    assert_eq!(allowed.client_msg_id, "history-client-allowed");
    assert_eq!(allowed.payload["t"], "history");
    assert_eq!(allowed.payload["session"], "history-allowed");
}

#[test]
fn history_invalid_cursor_values_return_failed_ack_without_querying_provider() {
    let history_calls = Arc::new(AtomicU64::new(0));
    let calls = Arc::clone(&history_calls);
    let (inner, upstream_rx) = test_inner_for_history(
        |_| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())),
        move |_, _, _| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        },
    );
    let k_room = Zeroizing::new([19_u8; 32]);
    for (index, invalid) in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("1"),
    ]
    .into_iter()
    .enumerate()
    {
        let command_id = format!("cmd-history-invalid-{index}");
        let payload = serde_json::json!({
            "t": "control.history",
            "session": "history-invalid",
            "before_message_id": invalid,
        });
        let envelope = seal_command_envelope(
            &k_room,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            0,
            "control",
            "history-invalid",
            &command_id,
            &payload,
        );
        assert_eq!(
            handle_command_envelope(&inner, &envelope, Some(&k_room)),
            Some(input_ack_json(&command_id, AckOutcome::Failed))
        );
    }
    assert_eq!(history_calls.load(Ordering::Relaxed), 0);
    assert!(upstream_rx.try_recv().is_err());
}

#[test]
fn history_contract_fixture_matches_production_payload_byte_for_byte() {
    let request = serde_json::json!({
        "t": "control.history",
        "session": "sess-history",
        "before_message_id": 120,
    });
    let rows = vec![
        history_row(119, "assistant", "Done.".to_owned()),
        history_row(101, "user", "Hello".to_owned()),
        history_row(88, "assistant", "Older".to_owned()),
    ];
    let response = build_history_page_with_limit("sess-history", Some(120), rows, 2).payload;
    let fixture = serde_json::json!({"request": request, "response": response});
    let mut serialized = serde_json::to_vec(&fixture).unwrap();
    serialized.push(b'\n');
    assert_eq!(
        serialized.as_slice(),
        include_bytes!("../../../../../remote-relay/fixtures/history-v1.json")
    );
}
