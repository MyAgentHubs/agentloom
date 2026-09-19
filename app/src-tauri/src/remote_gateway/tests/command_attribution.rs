#![cfg(test)]

use super::*;
/// M2-4c(B2)：`test_inner_for_command_attribution` 只暴露 `input_send_handler`——参数化跑
/// 三种下行命令（input.send/control.stop/input.answer）的测试需要同时控制三个 handler，
/// 这里补一个全量版本；`control_replay_handler` 固定放行（`|_, _| true`），三种命令测试
/// 都不关心 replay 去重语义。
fn test_inner_for_command_attribution_all_handlers(
    session_repo_provider: impl Fn(&str) -> Result<Option<String>, String> + Send + Sync + 'static,
    input_send_handler: impl Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync + 'static,
    input_answer_handler: impl Fn(InputAnswerFrame) -> Option<AckOutcome> + Send + Sync + 'static,
    control_stop_handler: impl Fn(ControlStopFrame) -> AckOutcome + Send + Sync + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    Arc::new(Inner {
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
        input_send_handler: Box::new(input_send_handler),
        input_answer_handler: Box::new(input_answer_handler),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(control_stop_handler),
        upstream_tx,
        milestone_tx,
        session_repo_provider: Box::new(session_repo_provider),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    })
}

// ---- M2-4c：命令归属 fail-closed ----------------------------------------------------

/// ①下行：active 模式，B 项目会话的命令必须 failed（不到达业务 handler）；A 项目（active
/// repo 本身）的会话必须正常放行、真的到达 handler。**B2 参数化**：三种下行命令
/// input.send/control.stop/input.answer 各走一遍闸——三个命令类型各有自己的
/// `command_session_allowed(inner, session)` 调用点（`handle_command_envelope`
/// 的三个 match 分支），闸被删掉一个就该只有对应那种命令的红。这三个都被拿去做变异
/// 自证：把某个命令类型的闸删掉，这里对应那个 case 必须从红变绿地失败（细节见收尾报告
/// 里贴的变异输出）。**P0-b（2026-08-14）新增第四臂 control.snapshot**：它没有独立的
/// `*_handler` 函数指针可挂 `received` 钩子（直接原子读 `partial_snapshots` + 直接入队
/// 里程碑），正例改用「ack outcome==ok」判定，`checks_received` 置 `false` 跳过共享
/// `received` 向量断言（负例分支仍照旧，因为对所有 case 它天然保持空，不受影响）。
#[test]
fn m2_4c_active_mode_rejects_other_repo_session_and_allows_active_repo_session() {
    struct Case {
        label: &'static str,
        kind: &'static str,
        payload: fn(&str) -> Value,
        checks_received: bool,
    }
    let cases = [
        Case {
            label: "input.send",
            kind: "input",
            payload: |session_id| serde_json::json!({"t": "input.send", "session": session_id, "text": "hi"}),
            checks_received: true,
        },
        Case {
            label: "control.stop",
            kind: "control",
            payload: |session_id| {
                let now = now_unix_ms();
                serde_json::json!({
                    "t": "control.stop",
                    "session": session_id,
                    "issued_at_ms": now,
                    "expires_at_ms": now + 1_000,
                })
            },
            checks_received: true,
        },
        Case {
            label: "input.answer",
            kind: "input",
            payload: |session_id| {
                serde_json::json!({
                    "t": "input.answer",
                    "session": session_id,
                    "decision_id": "decision-1",
                    "option": "opt-a",
                })
            },
            checks_received: true,
        },
        Case {
            label: "control.snapshot",
            kind: "control",
            payload: |session_id| serde_json::json!({"t": "control.snapshot", "session": session_id}),
            checks_received: false,
        },
    ];

    for case in cases {
        let k_room = Zeroizing::new([61_u8; 32]);
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_for_send = Arc::clone(&received);
        let received_for_answer = Arc::clone(&received);
        let received_for_stop = Arc::clone(&received);
        let inner = test_inner_for_command_attribution_all_handlers(
            |session_id| match session_id {
                "sess-a" => Ok(Some("repo-a".to_owned())),
                "sess-b" => Ok(Some("repo-b".to_owned())),
                other => panic!("unexpected session repo lookup for {other}"),
            },
            move |frame: InputSendFrame| {
                received_for_send.lock().unwrap().push(frame.session);
                Some(AckOutcome::Ok)
            },
            move |frame: InputAnswerFrame| {
                received_for_answer.lock().unwrap().push(frame.session);
                Some(AckOutcome::Ok)
            },
            move |frame: ControlStopFrame| {
                received_for_stop.lock().unwrap().push(frame.session);
                AckOutcome::Ok
            },
        );
        *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());

        let envelope_b = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            1,
            case.kind,
            "sess-b",
            "cmd-b",
            &(case.payload)("sess-b"),
        );
        let response_b = handle_frame(&inner, &envelope_b.to_string(), Some(&k_room)).unwrap();
        assert_eq!(
            response_b["outcome"], "failed",
            "{}: a session belonging to a different project's room must be rejected \
                 fail-closed",
            case.label
        );
        assert!(
            received.lock().unwrap().is_empty(),
            "{}: handler must never be reached for a session outside the active repo",
            case.label
        );

        let envelope_a = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            1,
            case.kind,
            "sess-a",
            "cmd-a",
            &(case.payload)("sess-a"),
        );
        let response_a = handle_frame(&inner, &envelope_a.to_string(), Some(&k_room)).unwrap();
        assert_eq!(
            response_a["outcome"], "ok",
            "{}: a session belonging to the active repo must pass through normally",
            case.label
        );
        if case.checks_received {
            assert_eq!(
                received.lock().unwrap().as_slice(),
                ["sess-a".to_owned()],
                "{}",
                case.label
            );
        }
    }
}

/// ②M2-4d：legacy 全局房回落已撤——不再存在"归属闸整体不启用"的模式了。旧测试断言的是
/// "legacy 房下两个项目的会话都放行、且压根不查 session_repo_provider"；新语义反过来：
/// `active_repo_id_for_gating` 保持 `GatewayInnerState::default()` 的 `None`（模拟单活跃
/// 房间模型下"理论不可达但仍要 fail-closed"的边界，见 `repo_id_is_active` 文档）时，闸
/// 依然会去查 provider（不再有开关短路——`command_gating_active` 字段已随 `RoomSource`
/// 一起删除），且因为没有 active repo 可比对，两个会话都必须被拒绝，handler 永远不该被
/// 调用到。
#[test]
fn command_gating_is_unconditional_and_fails_closed_without_active_repo() {
    let k_room = Zeroizing::new([62_u8; 32]);
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_for_handler = Arc::clone(&received);
    let provider_calls = Arc::new(AtomicU64::new(0));
    let provider_calls_for_closure = Arc::clone(&provider_calls);
    let inner = test_inner_for_command_attribution(
        move |_session_id| {
            provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        },
        move |frame| {
            received_for_handler.lock().unwrap().push(frame.session);
            Some(AckOutcome::Ok)
        },
    );
    // 不 store active_repo_id_for_gating——保持默认 None。

    for (session_id, command_id) in [("sess-a", "cmd-a"), ("sess-b", "cmd-b")] {
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            1,
            "input",
            session_id,
            command_id,
            &serde_json::json!({"t": "input.send", "session": session_id, "text": "hi"}),
        );
        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(
            response["outcome"], "failed",
            "no active repo configured must fail-closed for {session_id}"
        );
    }
    assert!(
        received.lock().unwrap().is_empty(),
        "handler must never be reached without an active repo"
    );
    assert!(
        provider_calls.load(Ordering::Relaxed) >= 2,
        "the gate must consult session_repo_provider unconditionally now — there is no more \
             legacy short-circuit that skips it"
    );
}

/// ③active 模式下 session repo 查询失败（DB 错误）必须 fail-closed 拒绝，不能把"查不到"
/// 当"放行"处理，也不能落到业务 handler。
#[test]
fn m2_4c_active_mode_rejects_when_session_repo_lookup_fails() {
    let k_room = Zeroizing::new([63_u8; 32]);
    let handler_calls = Arc::new(AtomicU64::new(0));
    let handler_calls_for_closure = Arc::clone(&handler_calls);
    let inner = test_inner_for_command_attribution(
        |_session_id| Err("db busy".to_owned()),
        move |_frame| {
            handler_calls_for_closure.fetch_add(1, Ordering::Relaxed);
            Some(AckOutcome::Ok)
        },
    );
    *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());

    let envelope = seal_command_envelope(
        &k_room,
        "0123456789abcdef0123456789abcdef",
        1,
        "input",
        "sess-a",
        "cmd-db-error",
        &serde_json::json!({"t": "input.send", "session": "sess-a", "text": "hi"}),
    );
    let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
    assert_eq!(
        response["outcome"], "failed",
        "a DB lookup error must fail-closed rather than fall through to the handler"
    );
    assert_eq!(handler_calls.load(Ordering::Relaxed), 0);
}

/// ④上行快照：active 模式下 `publish_session_index_snapshot_on_connect` 发布的
/// session.index 快照只含 active repo 的会话——覆盖 `filter_session_index_snapshot_for_
/// active_repo`（provider 层一次过滤方案，选择理由见收尾报告）。
#[test]
fn m2_4c_active_mode_session_index_snapshot_only_contains_active_repo_sessions() {
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || {
            Some(serde_json::json!([
                {
                    "id": "sess-a", "title": "A", "repo_id": "repo-a",
                    "archived": false, "status": Value::Null, "run_id": Value::Null,
                    "updated_at": 1,
                },
                {
                    "id": "sess-b", "title": "B", "repo_id": "repo-b",
                    "archived": false, "status": Value::Null, "run_id": Value::Null,
                    "updated_at": 2,
                },
            ]))
        },
    );
    *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let generation = inner.state.advance_generation_and_set_gate(true);

    publish_session_index_snapshot_on_connect(&inner, generation);

    let (item_generation, item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("active-repo-filtered snapshot must still be enqueued");
    assert_eq!(item_generation, generation);
    assert_eq!(item.t, "session.index");
    let ids: Vec<&str> = item.payload["sessions"]
        .as_array()
        .expect("snapshot payload must carry a sessions array")
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["sess-a"],
        "only the active repo's session may appear"
    );
}

/// ④a（M2-4x）：全量快照顶层的 `repo` 摘要——`id` 取自 `active_repo_id_for_gating`，`name`
/// 从已过滤出的 sessions 行里取第一行的 `repo_name`（同一个 repo 的所有行值相同）。
#[test]
fn m2_4c_active_mode_session_index_snapshot_top_level_repo_summary_derived_from_filtered_rows() {
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || {
            Some(serde_json::json!([
                {
                    "id": "sess-a", "title": "A", "repo_id": "repo-a",
                    "archived": false, "status": Value::Null, "run_id": Value::Null,
                    "updated_at": 1, "repo_name": "Acme Corp",
                },
                {
                    "id": "sess-b", "title": "B", "repo_id": "repo-b",
                    "archived": false, "status": Value::Null, "run_id": Value::Null,
                    "updated_at": 2, "repo_name": "Other Repo",
                },
            ]))
        },
    );
    *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let generation = inner.state.advance_generation_and_set_gate(true);

    publish_session_index_snapshot_on_connect(&inner, generation);

    let (_, item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("snapshot must still be enqueued");
    assert_eq!(
        item.payload["repo"],
        serde_json::json!({"id": "repo-a", "name": "Acme Corp"}),
        "top-level repo summary must reflect the active repo's id/name, not the other \
             repo's — even though its row also carries a repo_name"
    );
}

/// ④b（M2-4x）：active repo 已知但该项目当前零会话——`sessions` 过滤后为空数组，取不到任何
/// 一行的 `repo_name`，`name` 退化为 `null`；`id` 仍然可靠（不依赖 sessions 是否非空）。
#[test]
fn m2_4c_active_mode_session_index_snapshot_repo_name_null_when_active_repo_has_no_sessions() {
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || {
            Some(serde_json::json!([
                {
                    "id": "sess-b", "title": "B", "repo_id": "repo-b",
                    "archived": false, "status": Value::Null, "run_id": Value::Null,
                    "updated_at": 2, "repo_name": "Other Repo",
                },
            ]))
        },
    );
    *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let generation = inner.state.advance_generation_and_set_gate(true);

    publish_session_index_snapshot_on_connect(&inner, generation);

    let (_, item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("snapshot must still be enqueued, empty sessions array and all");
    assert_eq!(
        item.payload["repo"],
        serde_json::json!({"id": "repo-a", "name": null}),
        "active repo id is known even with zero sessions; name degrades to null instead \
             of being fabricated or leaking another repo's name"
    );
}

/// ⑤上行里程碑：active 模式下 B 项目会话的 msg.completed 在 drain 阶段被静默过滤，A 项目
/// 会话正常出线——覆盖 `drain_milestone_queue` 里新增的 `upstream_session_allowed` 分支。
#[test]
fn m2_4c_active_mode_milestone_drain_filters_other_repo_session_and_sends_active_repo_session() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([64_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: Some("sess-b".to_owned()),
            t: "msg.completed".to_owned(),
            payload: serde_json::json!({"message_id": "b-msg"}),
            client_msg_id: "client-b".to_owned(),
        },
    );
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: Some("sess-a".to_owned()),
            t: "msg.completed".to_owned(),
            payload: serde_json::json!({"message_id": "a-msg"}),
            client_msg_id: "client-a".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| match session_id {
        "sess-a" => Ok(Some("repo-a".to_owned())),
        "sess-b" => Ok(Some("repo-b".to_owned())),
        other => panic!("unexpected session repo lookup for {other}"),
    });

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let envelope = frames
        .recv_timeout(Duration::from_secs(2))
        .expect("the active repo's milestone must still be delivered");
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    assert_eq!(
        plaintext["message_id"], "a-msg",
        "only the active repo's session's milestone may reach the wire"
    );
    assert_eq!(
        state.upstream_repo_filtered.load(Ordering::Relaxed),
        1,
        "the other repo's milestone must be counted as filtered, not as an error"
    );
    assert!(
        frames.try_recv().is_err(),
        "no second frame should have been sent for the filtered-out session"
    );
    drop(socket);
    server.join().unwrap();
}

/// ⑤b（B2 补漏）上行 live：跟 ⑤ 同形但走 `upstream_tx`/`drain_live_queue`——active 模式下
/// B 项目会话的 live 事件在 drain 阶段被静默过滤，A 项目会话正常出线。⑤ 只覆盖了
/// `drain_milestone_queue` 那半，`drain_live_queue` 里新增的同款判定此前完全没有专门测试
/// 顶着（上一轮"live 的 attribution 恒 false"变异能存活正是因为这里没有测试）。
#[test]
fn m2_4c_active_mode_live_queue_filters_other_repo_session_and_sends_active_repo_session() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([66_u8; 32]);
    let state = GatewayInnerState::default();
    let generation = state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (upstream_tx, upstream_rx) = mpsc::sync_channel(2);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    upstream_tx
        .try_send((
            generation,
            LiveQueueItem::Batch(text_delta_payload("sess-b")),
        ))
        .unwrap();
    upstream_tx
        .try_send((
            generation,
            LiveQueueItem::Batch(text_delta_payload("sess-a")),
        ))
        .unwrap();
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| match session_id {
        "sess-a" => Ok(Some("repo-a".to_owned())),
        "sess-b" => Ok(Some("repo-b".to_owned())),
        other => panic!("unexpected session repo lookup for {other}"),
    });

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let envelope = frames
        .recv_timeout(Duration::from_secs(2))
        .expect("the active repo's live event must still be delivered");
    assert_eq!(envelope["kind"], "live");
    assert_eq!(
        envelope["session"], "sess-a",
        "only the active repo's session's live event may reach the wire"
    );
    assert_eq!(
        state.upstream_repo_filtered.load(Ordering::Relaxed),
        1,
        "the other repo's live event must be counted as filtered, not as an error"
    );
    assert!(
        frames.try_recv().is_err(),
        "no second frame should have been sent for the filtered-out session"
    );
    drop(socket);
    server.join().unwrap();
}

/// ⑥M2-4d：legacy 全局房回落已撤——不再有"归属过滤整体关闭"的模式。旧测试断言 legacy 房下
/// 快照/里程碑全量不过滤；新语义反过来：`active_repo_id_for_gating` 保持默认 `None`
/// （模拟单活跃房间模型下"理论不可达但仍要 fail-closed"的边界）时，(a) session.index 快照
/// 必须回空数组（不能把全量会话当默认值泄漏），(b) 逐条里程碑必须被过滤丢弃——
/// `session_repo_provider` 仍会被调用（不再有开关短路），但它的返回值不影响结果，因为
/// 没有 active repo 可比对，`repo_id_is_active` 恒判"不属于"。
#[test]
fn command_gating_fails_closed_for_upstream_snapshot_and_milestones_without_active_repo() {
    // (a) session.index 快照：没有 active repo 时必须回空，不能泄漏全量会话。
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || {
            Some(serde_json::json!([
                {
                    "id": "sess-a", "title": "A", "repo_id": "repo-a",
                    "archived": false, "status": Value::Null, "run_id": Value::Null,
                    "updated_at": 1,
                },
                {
                    "id": "sess-b", "title": "B", "repo_id": "repo-b",
                    "archived": false, "status": Value::Null, "run_id": Value::Null,
                    "updated_at": 2,
                },
            ]))
        },
    );
    // 不 store active_repo_id_for_gating——保持默认 None。
    let generation = inner.state.advance_generation_and_set_gate(true);
    publish_session_index_snapshot_on_connect(&inner, generation);
    let (_, item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the snapshot must still be enqueued — empty, not skipped");
    let ids: Vec<&str> = item.payload["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.is_empty(),
        "without an active repo the snapshot must fail-closed to empty, not leak every \
             session"
    );
    // (a2, M2-4x) 顶层 repo 摘要同样必须 fail-closed 到显式 null，不是省略键、也不是留着
    // 上一次连接残留的项目 id/name。
    assert_eq!(
        item.payload["repo"],
        Value::Null,
        "without an active repo the top-level repo summary must be explicit null"
    );

    // (b) 逐条里程碑：没有 active repo 时必须被过滤丢弃；provider 仍会被调用，但返回值
    // 不影响结果。
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([65_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx_b) = mpsc::sync_channel(2);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: Some("sess-b".to_owned()),
            t: "msg.completed".to_owned(),
            payload: serde_json::json!({"message_id": "b-msg"}),
            client_msg_id: "client-b".to_owned(),
        },
    );
    let provider_calls = Arc::new(AtomicU64::new(0));
    let provider_calls_for_closure = Arc::clone(&provider_calls);
    let session_repo_provider: SessionRepoProvider = Box::new(move |_session_id| {
        provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
        Ok(Some("repo-b".to_owned()))
    });
    // expected_frames=0：filtered-out 意味着什么都不会写到 socket 上，服务端不该等一条
    // 永远不会到达的帧。
    let (addr, frames, server) = spawn_recording_server(0);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx_b,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();
    // `drain_upstream` runs synchronously — by the time it returns, a filtered item was
    // never written to the socket at all (no race to wait out, unlike checking for the
    // *absence* of a second frame after a first one already proved the pipe is live).
    assert!(
        frames.try_recv().is_err(),
        "without an active repo the milestone must be filtered, not delivered"
    );
    assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 1);
    assert!(
        provider_calls.load(Ordering::Relaxed) >= 1,
        "the gate must still consult session_repo_provider — no more legacy short-circuit"
    );
    drop(socket);
    server.join().unwrap();
}
