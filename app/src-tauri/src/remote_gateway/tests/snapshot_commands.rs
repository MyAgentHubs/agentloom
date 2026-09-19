#![cfg(test)]

use super::*;
/// P0-b：过渡期条文（M2-6，M0 §3）"snapshot 无 handler 必回 failed"已不适用——`control.
/// snapshot` 现已真正接线，这里改写为端到端真路径：喂两条 TextDelta 累积归约态、请求
/// snapshot 拿到「进行中带 partial」应答；再喂 RunCloseout 收尾、第二次请求验证回退到
/// idle 三 null（水位语义 + 收尾清空同一条测试链路里验证）。
#[test]
fn encrypted_control_snapshot_reports_partial_state_then_idle_after_run_closeout() {
    let k_room = Zeroizing::new([13_u8; 32]);
    let (inner, milestone_rx) = test_inner_for_snapshot();

    // 真实 sink 入队路径：enqueue_batch_payload_for_upstream -> maintain_partial_snapshots。
    enqueue_batch_payload_for_upstream(
        &inner.state,
        &inner.upstream_tx,
        &inner.milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "s-6".to_owned(),
                run_id: "run-9".to_owned(),
                dispatch: None,
                events: vec![
                    crate::event_transport::SequencedEvent {
                        seq: 5,
                        event: crate::agent_event::AgentEvent::TextDelta {
                            text: "Working on ".to_owned(),
                        },
                    },
                    crate::event_transport::SequencedEvent {
                        seq: 12,
                        event: crate::agent_event::AgentEvent::TextDelta {
                            text: "the fix".to_owned(),
                        },
                    },
                ],
            }],
        },
    );

    let snapshot_request = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-6",
        "cmd-snapshot-1",
        &serde_json::json!({"t": "control.snapshot", "session": "s-6"}),
    );
    let response = handle_frame(&inner, &snapshot_request.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["command_id"], "cmd-snapshot-1");
    assert_eq!(response["outcome"], "ok");
    assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 0);

    let (_, item) = milestone_rx
        .try_recv()
        .expect("control.snapshot must enqueue a snapshot milestone");
    assert_eq!(item.t, "snapshot");
    assert_eq!(item.session.as_deref(), Some("s-6"));
    assert_eq!(item.payload["run_id"], "run-9");
    assert_eq!(item.payload["through_run_seq"], 12);
    assert_eq!(
        item.payload["partial_msg"],
        serde_json::json!({
            "role": "assistant",
            "blocks": [{"type": "text", "text": "Working on the fix"}],
        })
    );

    // run 收尾（RunCloseout）——partial_snapshots 条目应被清掉，下次请求回退到 idle 三 null。
    enqueue_batch_payload_for_upstream(
        &inner.state,
        &inner.upstream_tx,
        &inner.milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "s-6".to_owned(),
                run_id: "run-9".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 13,
                    event: crate::agent_event::AgentEvent::RunCloseout {
                        run_id: "run-9".to_owned(),
                        commit_sha: None,
                        files_changed: None,
                        insertions: None,
                        deletions: None,
                        interrupted: None,
                    },
                }],
            }],
        },
    );

    let idle_request = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "s-6",
        "cmd-snapshot-2",
        &serde_json::json!({"t": "control.snapshot", "session": "s-6"}),
    );
    let idle_response = handle_frame(&inner, &idle_request.to_string(), Some(&k_room)).unwrap();
    assert_eq!(idle_response["outcome"], "ok");

    let (_, idle_item) = milestone_rx
        .try_recv()
        .expect("second control.snapshot must enqueue another snapshot milestone");
    assert_eq!(idle_item.payload["run_id"], Value::Null);
    assert_eq!(idle_item.payload["through_run_seq"], Value::Null);
    assert_eq!(idle_item.payload["partial_msg"], Value::Null);
}

/// P0-b：幂等——同一 `command_id` 重投两次（relay/客户端重连补发场景），两次入队的
/// `client_msg_id` 必须相同（由 `snapshot|<session>|<command_id>` 确定性派生），relay 侧
/// 据此天然去重。
#[test]
fn encrypted_control_snapshot_repeated_command_id_derives_same_client_msg_id() {
    let k_room = Zeroizing::new([14_u8; 32]);
    let (inner, milestone_rx) = test_inner_for_snapshot();

    let request = |command_id: &str| {
        seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-7",
            command_id,
            &serde_json::json!({"t": "control.snapshot", "session": "s-7"}),
        )
    };

    let first = handle_frame(&inner, &request("cmd-repeat").to_string(), Some(&k_room)).unwrap();
    assert_eq!(first["outcome"], "ok");
    let second = handle_frame(&inner, &request("cmd-repeat").to_string(), Some(&k_room)).unwrap();
    assert_eq!(second["outcome"], "ok");

    let (_, first_item) = milestone_rx.try_recv().expect("first snapshot milestone");
    let (_, second_item) = milestone_rx.try_recv().expect("second snapshot milestone");
    assert_eq!(first_item.client_msg_id, second_item.client_msg_id);
    assert!(!first_item.client_msg_id.is_empty());
}

/// P0-b：归属闸负例（挂靠 M2-4c 参数化闸测试之外的独立最小回归）——session 不属 active
/// repo 时 snapshot 请求必须 fail-closed，不得原子读取 `partial_snapshots`（不属于当前
/// active repo 的 session 理论上不该出现在表里，但闸必须在读表之前短路）。
#[test]
fn encrypted_control_snapshot_for_non_active_repo_session_fails_closed() {
    let k_room = Zeroizing::new([15_u8; 32]);
    let inner = test_inner_for_command_attribution(
        |session_id| match session_id {
            "sess-other-repo" => Ok(Some("repo-b".to_owned())),
            other => panic!("unexpected session repo lookup for {other}"),
        },
        |_| Some(AckOutcome::Ok),
    );
    *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());

    let request = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        "sess-other-repo",
        "cmd-other-repo",
        &serde_json::json!({"t": "control.snapshot", "session": "sess-other-repo"}),
    );
    let response = handle_frame(&inner, &request.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["outcome"], "failed");
    assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 1);
}

/// P0-b 微返工第 4 轮：session 长度纵深守卫必须在归属闸之前生效——用会 `panic!` 的
/// `session_repo_provider` 当"归属闸绝不能被调用"的哨兵。129 字节 session（卡在
/// `SESSION_ID_MAX_BYTES` 上限之上一个字节）如果守卫漏放或顺序被改成排在归属闸之后，
/// 这个 provider 就会被调用而 panic——比只断言 `outcome == "failed"` 更硬地钉住"必须在
/// 归属闸之前短路"这条顺序要求，不是只测最终结果。
///
/// 长度**故意硬编码 129**（不是 `SESSION_ID_MAX_BYTES + 1`）：变异自证要把守卫阈值改到
/// 1MB 来验证这条测试会红——如果长度改成跟着常量算，阈值一起变大，测试会"自适应"到
/// 新阈值而永远不红，变异就测不出东西。
#[test]
fn encrypted_control_snapshot_oversized_session_fails_closed_before_attribution_gate() {
    let k_room = Zeroizing::new([16_u8; 32]);
    let inner = test_inner_for_command_attribution(
        |session_id| panic!("归属闸不应在 session 长度守卫之前被调用：{session_id}"),
        |_| Some(AckOutcome::Ok),
    );

    let oversized_session = "s".repeat(129);
    let payload_session = oversized_session.clone();
    let request = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        7,
        "control",
        &oversized_session,
        "cmd-oversized-session",
        &serde_json::json!({"t": "control.snapshot", "session": payload_session}),
    );
    let response = handle_frame(&inner, &request.to_string(), Some(&k_room)).unwrap();
    assert_eq!(response["outcome"], "failed");
    assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 1);
}

/// P0-b：control.snapshot 测试专用 fixture——需要同时满足两件既有 `test_inner_with_*`
/// 变体没有一个两者都占的条件：① 暴露 milestone_rx（校验 snapshot 实际入队的
/// payload/client_msg_id）；② 归属闸默认放行（session 一律属
/// `TEST_DEFAULT_ACTIVE_REPO_ID`，不然还没走到 snapshot 臂内部逻辑就被 M2-4c 闸
/// fail-closed 挡死）。额外把 upstream 门控开起来——`enqueue_batch_payload_for_upstream`/
/// `maintain_partial_snapshots` 在门控关闭时整体 no-op（生产连接建立时才会开），这里显式
/// 模拟"已连接"状态才能喂事件进 `partial_snapshots`。
fn test_inner_for_snapshot() -> (Arc<Inner>, Receiver<(u64, MilestoneItem)>) {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(8);
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
        session_repo_provider: test_session_repo_provider_allowing_default_repo(),
        session_history_provider: test_session_history_provider(),
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
    (inner, milestone_rx)
}
