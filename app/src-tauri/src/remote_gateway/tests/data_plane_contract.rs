#![cfg(test)]

use super::*;
// ========================================================================================
// DP-1（data-plane-v1 fixture 层）：九类数据面帧解密后明文形状的真路径消费方。
//
// 每个 case 都从 `remote-relay/fixtures/data-plane-v1.json` 按 name 取样张，再驱动生产
// builder/parser（不是手写校验器自证）产出实际值比对；wire 信封/AAD/令牌面仍是
// wire-v1.json 的地盘，这里只管解密后的 payload 形状。session.index/msg.completed/
// card.*/run.status 权威来自各自的 `build_*_payload` 函数；tool.completed 权威来自真实
// sink 入队路径 `extract_tool_milestones`（经 `enqueue_batch_payload_for_upstream`）；live
// 四变体权威来自 `classify`；control.snapshot 请求权威来自 `handle_command_envelope` 的
// 解析路径（P0-b 已接线：归属闸 fail-closed + payload session 字段校验真路径）；snapshot
// 应答（v1.8.12 水印/尺寸预算契约）权威来自 `maintain_partial_snapshots`（经
// `enqueue_batch_payload_for_upstream` 的真实 sink 入队路径）+ `build_snapshot_payload`
// （P0-b 已销 pending）。
// ========================================================================================

fn load_data_plane_v1_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/data-plane-v1.json"
    ))
    .expect("data-plane-v1 fixture must parse as JSON")
}

fn data_plane_v1_case(fixture: &Value, name: &str) -> Value {
    fixture["cases"]
        .as_array()
        .expect("data-plane-v1 fixture `cases` must be an array")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("data-plane-v1 fixture missing case `{name}`"))
        .clone()
}

#[test]
fn data_plane_v1_session_index_variants_match_fixture_and_drive_builders() {
    let fixture = load_data_plane_v1_fixture();

    // session.index(full)：sessions 数组抄自 db::SessionIndexSnapshotRow 的真实 Serialize
    // 输出（不是手打 JSON），与快照 provider 生产路径同一份类型。
    let rows = vec![
        crate::db::SessionIndexSnapshotRow {
            id: "sess-1".to_owned(),
            title: "Fix login bug".to_owned(),
            repo_id: Some("repo-a".to_owned()),
            archived: false,
            status: Some("running".to_owned()),
            run_id: Some("run-42".to_owned()),
            updated_at: 1_765_430_400_123,
            last_msg_preview: Some("Latest assistant reply".to_owned()),
            last_activity_at: Some(1_765_430_450),
            repo_name: None,
        },
        crate::db::SessionIndexSnapshotRow {
            id: "sess-2".to_owned(),
            title: "Update docs".to_owned(),
            repo_id: Some("repo-a".to_owned()),
            archived: false,
            status: None,
            run_id: None,
            updated_at: 1_765_430_300_000,
            last_msg_preview: None,
            last_activity_at: None,
            repo_name: None,
        },
    ];
    let sessions = serde_json::to_value(&rows).expect("rows must serialize");
    let full_payload = milestone_payload(
        "session.index",
        build_session_index_snapshot_payload(sessions, Value::Null),
    );
    let expected_full = data_plane_v1_case(&fixture, "session_index_full")["frame"].clone();
    assert_eq!(full_payload, expected_full);

    let created_payload = milestone_payload(
        "session.index",
        build_session_index_created_payload("sess-3", "New session", "repo-a", "ns-1", None),
    );
    assert_eq!(
        created_payload,
        data_plane_v1_case(&fixture, "session_index_created")["frame"]
    );

    let renamed_payload = milestone_payload(
        "session.index",
        build_session_index_renamed_payload("sess-1", "Renamed title"),
    );
    assert_eq!(
        renamed_payload,
        data_plane_v1_case(&fixture, "session_index_renamed")["frame"]
    );

    let archived_ids = vec!["sess-1".to_owned(), "sess-2".to_owned()];
    let archived_payload = milestone_payload(
        "session.index",
        build_session_index_archived_payload(&archived_ids, true),
    );
    assert_eq!(
        archived_payload,
        data_plane_v1_case(&fixture, "session_index_archived")["frame"]
    );

    let deleted_payload = milestone_payload(
        "session.index",
        build_session_index_deleted_payload("sess-9"),
    );
    assert_eq!(
        deleted_payload,
        data_plane_v1_case(&fixture, "session_index_deleted")["frame"]
    );
}

/// B1（backlog 跟进）：`session_index_full`/`session_index_created` 样张只钉了 `repo`/
/// `repo_name` 恒 `null` 的形状——填充态（字段真有值）两端各自造语料测，样张对拍缺口，
/// 字段改名可能「Rust 自测红、手机端全绿」地悄悄裂开。本测试用同一份共享样张的填充态
/// case（`session_index_full_with_repo_name`/`session_index_created_with_repo_name`）
/// 覆盖：sessions 数组仍抄自 `db::SessionIndexSnapshotRow` 的真实 Serialize 输出（不是
/// 手打 JSON），`repo_name` 这次是 `Some(..)`；顶层 `repo` 摘要是 `{id, name}` 均非 null
/// 的 `Value`（构造层面等价于 `active_repo_summary_for_snapshot` 在有名字时会产出的形状，
/// 不经过那个函数本身——同 `data_plane_v1_session_index_variants_match_fixture_and_drive_
/// builders` 只探 builder 契约、不探 `Inner` 状态装配的既有分工）。
#[test]
fn data_plane_v1_session_index_filled_variant_matches_fixture_and_drives_builders() {
    let fixture = load_data_plane_v1_fixture();

    let rows = vec![
        crate::db::SessionIndexSnapshotRow {
            id: "sess-1".to_owned(),
            title: "Fix login bug".to_owned(),
            repo_id: Some("repo-a".to_owned()),
            archived: false,
            status: Some("running".to_owned()),
            run_id: Some("run-42".to_owned()),
            updated_at: 1_765_430_400_123,
            last_msg_preview: Some("Latest assistant reply".to_owned()),
            last_activity_at: Some(1_765_430_450),
            repo_name: Some("Acme Metrics".to_owned()),
        },
        crate::db::SessionIndexSnapshotRow {
            id: "sess-2".to_owned(),
            title: "Update docs".to_owned(),
            repo_id: Some("repo-a".to_owned()),
            archived: false,
            status: None,
            run_id: None,
            updated_at: 1_765_430_300_000,
            last_msg_preview: None,
            last_activity_at: None,
            repo_name: Some("Acme Metrics".to_owned()),
        },
    ];
    let sessions = serde_json::to_value(&rows).expect("rows must serialize");
    let repo_summary = serde_json::json!({ "id": "repo-a", "name": "Acme Metrics" });
    let full_payload = milestone_payload(
        "session.index",
        build_session_index_snapshot_payload(sessions, repo_summary),
    );
    let expected_full =
        data_plane_v1_case(&fixture, "session_index_full_with_repo_name")["frame"].clone();
    assert_eq!(full_payload, expected_full);

    let created_payload = milestone_payload(
        "session.index",
        build_session_index_created_payload(
            "sess-3",
            "New session",
            "repo-a",
            "ns-1",
            Some("Acme Metrics"),
        ),
    );
    assert_eq!(
        created_payload,
        data_plane_v1_case(&fixture, "session_index_created_with_repo_name")["frame"]
    );
}

#[test]
fn data_plane_v1_msg_completed_matches_fixture_and_drives_builder() {
    let fixture = load_data_plane_v1_fixture();

    // blocks 抄自 db::Block 的真实 Serialize 输出（tag="type"/snake_case），不是手打 JSON。
    let blocks = vec![
        crate::db::Block::Text {
            text: "Fixed the login bug and added a regression test.".to_owned(),
        },
        crate::db::Block::Tool {
            id: "tool-1".to_owned(),
            tool: "shell".to_owned(),
            summary: "cargo test".to_owned(),
            card: crate::db::BlockCardKind::Command,
            status: crate::db::BlockToolStatus::Ok,
            exit_code: Some(0),
            output: Some("test result: ok. 42 passed".to_owned()),
        },
    ];
    let blocks_json = serde_json::to_value(&blocks).expect("blocks must serialize");
    // 显示当前 agent（MA1）：样张的 assistant case 带 "agent": "Claude"（Some 分支）。
    // msgfix1 T3：revision/content_raw 只喂给内部私有 ref-source 键（供超预算降级消费），
    // 正常大小消息的 wire 形状不受影响——比对前先剥掉它，同生产路径入队前必经的一步。
    let payload = milestone_payload(
        "msg.completed",
        strip_ref_source(build_msg_completed_payload(
            101,
            "assistant",
            blocks_json,
            Some("Claude"),
            1,
            "raw-content-not-part-of-fixture-shape",
        )),
    );
    assert_eq!(
        payload,
        data_plane_v1_case(&fixture, "msg_completed")["frame"]
    );
}

#[test]
fn data_plane_v1_activity_summary_matches_fixture_and_drives_upsert() {
    // msgfix2 U1（M0 §10.11）：activity_summary 首发（revision=1）与改写重发（revision=2）
    // 两张样张，驱动真实生产路径 db::upsert_activity_summary_and_publish 产出、经
    // build_msg_completed_payload 序列化的 wire 形状——不是手打 JSON 自证。
    let fixture = load_data_plane_v1_fixture();
    let c = crate::test_support::mem_db();
    crate::db::create_session(&c, "s-dp1-activity", "x", "local-default", "local").unwrap();

    // 样张的 message_id（501）是自包含 illustrative 常量，与 DB 真实自增 id 无关——同
    // data_plane_v1_msg_completed_matches_fixture_and_drives_builder 既有惯例（那个测试
    // 也是给 build_msg_completed_payload 传字面量 101，不读任何真实 DB id）。这里既要证明
    // "db::upsert_activity_summary_and_publish 产出的 content/revision 是对的"，也要证明
    // "build_msg_completed_payload 拿着这份 content 包出来的 wire 形状与样张一致"——两件事
    // 分别验证：content/revision 直接读 DB 断言；wire 形状用样张同款字面量 message_id 驱动
    // 同一个生产 builder。
    crate::remote_gateway::test_take_publish_log();
    crate::db::upsert_activity_summary_and_publish(
        &c,
        "s-dp1-activity",
        "run-9",
        3,
        0,
        1,
        0,
        "running",
    )
    .unwrap();
    let (revision, content_raw): (i64, String) = c
        .query_row(
            "SELECT revision, content FROM messages WHERE session_id = ?1",
            ["s-dp1-activity"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision, 1);
    let blocks_json: Value = serde_json::from_str(&content_raw).unwrap();
    let first_send_payload = milestone_payload(
        "msg.completed",
        strip_ref_source(build_msg_completed_payload(
            501,
            "assistant",
            blocks_json,
            None,
            revision,
            &content_raw,
        )),
    );
    assert_eq!(
        first_send_payload,
        data_plane_v1_case(&fixture, "activity_summary_first_send")["frame"]
    );

    crate::db::upsert_activity_summary_and_publish(
        &c,
        "s-dp1-activity",
        "run-9",
        5,
        1,
        1,
        1,
        "done",
    )
    .unwrap();
    let row_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            ["s-dp1-activity"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(row_count, 1, "同一条消息原地改写，不是新插一条");
    let (revision_2, content_raw_2): (i64, String) = c
        .query_row(
            "SELECT revision, content FROM messages WHERE session_id = ?1",
            ["s-dp1-activity"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision_2, 2);
    let blocks_json_2: Value = serde_json::from_str(&content_raw_2).unwrap();
    let revision_update_payload = milestone_payload(
        "msg.completed",
        strip_ref_source(build_msg_completed_payload(
            501,
            "assistant",
            blocks_json_2,
            None,
            revision_2,
            &content_raw_2,
        )),
    );
    assert_eq!(
        revision_update_payload,
        data_plane_v1_case(&fixture, "activity_summary_revision_update")["frame"]
    );
}

#[test]
fn data_plane_v1_card_and_run_status_milestones_match_fixture_and_drive_builders() {
    let fixture = load_data_plane_v1_fixture();

    // block 抄自真实 db::Block::DecisionCard 的 Serialize 输出，不是手打 JSON。
    let block = crate::db::Block::DecisionCard {
        decision_id: "d-1".to_owned(),
        kind: "ask".to_owned(),
        question: "Deploy the hotfix to production now?".to_owned(),
        options: vec!["yes".to_owned(), "no".to_owned()],
        recommended: Some("yes".to_owned()),
        rationale: Some("Regression test passes; fix is isolated.".to_owned()),
        payload: Value::Null,
        source_run_id: "run-7".to_owned(),
        status: "pending".to_owned(),
        chosen_option: None,
        created_at: 1_765_430_400_123,
    };
    let block_json = serde_json::to_value(&block).expect("block must serialize");
    let card_created_payload =
        milestone_payload("card.created", build_card_created_payload(block_json));
    assert_eq!(
        card_created_payload,
        data_plane_v1_case(&fixture, "card_created")["frame"]
    );

    let card_resolved_payload = milestone_payload(
        "card.resolved",
        build_card_resolved_payload("d-1", "resolved", Some("yes")),
    );
    assert_eq!(
        card_resolved_payload,
        data_plane_v1_case(&fixture, "card_resolved")["frame"]
    );

    let running_payload = milestone_payload(
        "run.status",
        build_run_status_payload("sess-1", "running", Some("run-7")),
    );
    assert_eq!(
        running_payload,
        data_plane_v1_case(&fixture, "run_status_running")["frame"]
    );

    let idle_payload = milestone_payload(
        "run.status",
        build_run_status_payload("sess-1", "idle", None),
    );
    assert_eq!(
        idle_payload,
        data_plane_v1_case(&fixture, "run_status_idle")["frame"]
    );
}

#[test]
fn data_plane_v1_tool_completed_matches_fixture_and_drives_extract_tool_milestones() {
    use crate::agent_event::{AgentEvent, ToolStatus};

    let fixture = load_data_plane_v1_fixture();
    let state = GatewayInnerState::default();
    let generation = state.advance_generation_and_set_gate(true);
    remember_tool_name(&state, "run-dp1", "tool-1", "shell");
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    // 真实 sink 入队路径：enqueue_batch_payload_for_upstream 内部调用
    // extract_tool_milestones，不是直接手工拼 MilestoneItem。
    enqueue_batch_payload_for_upstream(
        &state,
        &upstream_tx,
        &milestone_tx,
        single_event_payload(
            "run-dp1",
            "sess-1",
            AgentEvent::ToolCompleted {
                id: "tool-1".to_owned(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: Some("build succeeded".to_owned()),
            },
        ),
    );

    let (item_generation, item) = milestone_rx
        .try_recv()
        .expect("extract_tool_milestones must enqueue a tool.completed milestone");
    assert_eq!(item_generation, generation);
    assert_eq!(item.t, "tool.completed");
    // item.payload 是入队时的裸 payload（尚未合并 t）——真正上线前还要过
    // drain_milestone_queue 里的 milestone_payload(&t, payload)（remote_gateway.rs:2778），
    // 这里显式重放那一步再对拍，才是解密后明文的真实形状。
    assert_eq!(
        milestone_payload(&item.t, item.payload.clone()),
        data_plane_v1_case(&fixture, "tool_completed")["frame"]
    );
}

#[test]
fn data_plane_v1_live_deltas_match_fixture_and_drive_classify() {
    use crate::agent_event::AgentEvent;

    let fixture = load_data_plane_v1_fixture();

    let (_, text_delta) = classify(
        &AgentEvent::TextDelta {
            text: "Hello, ".to_owned(),
        },
        5,
    )
    .expect("text_delta must classify as live");
    assert_eq!(
        text_delta,
        data_plane_v1_case(&fixture, "live_text_delta")["frame"]
    );

    let (_, thinking_delta) = classify(
        &AgentEvent::ThinkingDelta {
            text: "Let me check the tests...".to_owned(),
        },
        6,
    )
    .expect("thinking_delta must classify as live");
    assert_eq!(
        thinking_delta,
        data_plane_v1_case(&fixture, "live_thinking_delta")["frame"]
    );

    let (_, tool_output_delta) = classify(
        &AgentEvent::ToolOutputDelta {
            id: "tool-1".to_owned(),
            text: "Running cargo test\n".to_owned(),
        },
        7,
    )
    .expect("tool_output_delta must classify as live");
    assert_eq!(
        tool_output_delta,
        data_plane_v1_case(&fixture, "live_tool_output_delta")["frame"]
    );

    let (_, usage_delta) = classify(
        &AgentEvent::UsageDelta {
            input_tokens: Some(120),
            output_tokens: Some(45),
        },
        8,
    )
    .expect("usage_delta must classify as live");
    assert_eq!(
        usage_delta,
        data_plane_v1_case(&fixture, "live_usage_delta")["frame"]
    );

    // 超长被截断正样张：源事件文本 OUTPUT_TRUNCATE_BYTES+17 字节，classify 内部
    // truncate_utf8 必须截到恰好 OUTPUT_TRUNCATE_BYTES 字节（与 fixture text 长度对齐）。
    let oversized = "x".repeat(OUTPUT_TRUNCATE_BYTES + 17);
    let (_, truncated) = classify(&AgentEvent::TextDelta { text: oversized }, 9)
        .expect("oversized text_delta must still classify as live");
    assert_eq!(
        truncated,
        data_plane_v1_case(&fixture, "live_text_delta_truncated")["frame"]
    );
}

#[test]
fn data_plane_v1_control_snapshot_request_cases_match_fixture_and_drive_handle_command_envelope() {
    let fixture = load_data_plane_v1_fixture();
    let k_room = Zeroizing::new([44_u8; 32]);
    // P0-b：归属闸恒启用——session 一律判属 active repo，专测「payload 结构/字段校验」
    // 这一层；归属闸负例单独在 M2-4c 参数化测试第四臂 + 独立回归测试覆盖。
    let inner = with_default_active_repo(test_inner_for_command_attribution(
        |_session_id| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())),
        |_| Some(AckOutcome::Ok),
    ));

    for (name, expect_valid) in [
        ("control_snapshot_request_accepted_todo_stub", true),
        ("control_snapshot_request_unknown_t_rejected", false),
        ("control_snapshot_request_missing_session_rejected", false),
        (
            "control_snapshot_request_session_wrong_type_rejected",
            false,
        ),
    ] {
        let case = data_plane_v1_case(&fixture, name);
        assert_eq!(case["valid"], expect_valid, "{name}: fixture valid flag");
        let frame = case["frame"].clone();
        // 请求 payload 的 session 字段在坏样张里缺失/非字符串，信封层 session（AAD 用途、
        // 归属闸判定输入）固定用一个字符串——这里专测「payload 内 session 字段校验」这一
        // 层，不与信封层 session 混为一谈（归属闸负例见上方独立测试）。
        let command_id = format!("cmd-{name}");
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-6",
            &command_id,
            &frame,
        );

        let before_bad_frames = inner.state.bad_frames.load(Ordering::Relaxed);
        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room))
            .expect("handle_frame must always ack a well-formed command envelope");
        assert_eq!(response["command_id"], command_id.as_str());
        let after_bad_frames = inner.state.bad_frames.load(Ordering::Relaxed);

        if expect_valid {
            assert_eq!(
                response["outcome"], "ok",
                "{name}: 结构合法且 session 属 active repo 的 control.snapshot 请求必须回 ok"
            );
            assert_eq!(
                after_bad_frames, before_bad_frames,
                "{name}: 成功处理的 control.snapshot 请求不应计入 bad_frames"
            );
        } else {
            assert_eq!(
                response["outcome"], "failed",
                "{name}: 结构不合法/未知 t 的 control 帧必须回 failed"
            );
            assert_eq!(
                after_bad_frames,
                before_bad_frames + 1,
                "{name}: 结构不合法/未知 t 的 control 帧必须被计入 bad_frames"
            );
        }
    }
}

// ----------------------------------------------------------------------------------------
// P0-b：snapshot 应答（v1.8.12 水印/尺寸契约）三态样张的真路径消费方——DP-1 pending 销账。
// ----------------------------------------------------------------------------------------

#[test]
fn data_plane_v1_snapshot_response_matches_fixture_and_drives_partial_snapshot_pipeline() {
    use crate::agent_event::AgentEvent;

    let fixture = load_data_plane_v1_fixture();

    // case 1：进行中且已纳入 live 帧——真实 sink 入队路径喂一条 TextDelta（seq=12），
    // 归约态非空，partial_msg 非 null。
    let state_running = GatewayInnerState::default();
    state_running.advance_generation_and_set_gate(true);
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    enqueue_batch_payload_for_upstream(
        &state_running,
        &upstream_tx,
        &milestone_tx,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "s-1".to_owned(),
                run_id: "run-7".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 12,
                    event: AgentEvent::TextDelta {
                        text: "Working on the fix, running tests now...".to_owned(),
                    },
                }],
            }],
        },
    );
    let (run_running, blocks) = {
        let snapshots = lock(&state_running.partial_snapshots);
        let entry = snapshots
            .get("s-1")
            .expect("partial snapshot entry must exist after feeding one batch");
        (
            Some((entry.run_id.clone(), entry.last_seq)),
            entry.reducer.snapshot_blocks(),
        )
    };
    let running_payload = milestone_payload(
        "snapshot",
        build_snapshot_payload(
            "s-1",
            run_running
                .as_ref()
                .map(|(run_id, through_run_seq)| (run_id.as_str(), *through_run_seq)),
            &blocks,
        ),
    );
    assert_eq!(
        running_payload,
        data_plane_v1_case(&fixture, "snapshot_response_running_with_partial")["frame"]
    );

    // case 2：进行中但已纳入的事件不产可显示内容——v1.8.12 订正：through_run_seq=0 取值
    // 废除，生产序号器先自增后返回、首条事件即 seq=1；batch 里只有一条不产 block 的事件
    // （UsageDelta），seq=1，reducer 不出块，through_run_seq=Some(1) 且 partial_msg 仍为
    // null（"已纳入事件但无可显示内容"，不是"无条目"）。
    let state_no_partial = GatewayInnerState::default();
    state_no_partial.advance_generation_and_set_gate(true);
    let (upstream_tx2, _upstream_rx2) = mpsc::sync_channel(1);
    let (milestone_tx2, _milestone_rx2) = mpsc::sync_channel(1);
    enqueue_batch_payload_for_upstream(
        &state_no_partial,
        &upstream_tx2,
        &milestone_tx2,
        crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "s-1".to_owned(),
                run_id: "run-8".to_owned(),
                dispatch: None,
                events: vec![crate::event_transport::SequencedEvent {
                    seq: 1,
                    event: AgentEvent::UsageDelta {
                        input_tokens: Some(42),
                        output_tokens: Some(7),
                    },
                }],
            }],
        },
    );
    let (run_no_partial, blocks_no_partial) = {
        let snapshots = lock(&state_no_partial.partial_snapshots);
        let entry = snapshots
            .get("s-1")
            .expect("partial snapshot entry must exist even with zero-block events");
        (
            Some((entry.run_id.clone(), entry.last_seq)),
            entry.reducer.snapshot_blocks(),
        )
    };
    assert!(
        blocks_no_partial.is_empty(),
        "UsageDelta must not push a displayable block"
    );
    assert_eq!(
        run_no_partial.as_ref().map(|(_, seq)| *seq),
        Some(1),
        "生产序号器先自增后返回——首条事件的 seq 必须是 1，不许手造 seq:0"
    );
    let no_partial_payload = milestone_payload(
        "snapshot",
        build_snapshot_payload(
            "s-1",
            run_no_partial
                .as_ref()
                .map(|(run_id, through_run_seq)| (run_id.as_str(), *through_run_seq)),
            &blocks_no_partial,
        ),
    );
    assert_eq!(
        no_partial_payload,
        data_plane_v1_case(&fixture, "snapshot_response_running_no_partial")["frame"]
    );

    // case 3：idle——没有条目（从未 feed 过，或已被 Completed/RunCloseout 清掉），三字段
    // 全 null。
    let idle_payload = milestone_payload("snapshot", build_snapshot_payload("s-1", None, &[]));
    assert_eq!(
        idle_payload,
        data_plane_v1_case(&fixture, "snapshot_response_idle_all_null")["frame"]
    );
}
