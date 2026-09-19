#![cfg(test)]

use super::*;
use base64::Engine;

fn test_desktop_credential_provider() -> DesktopCredentialProvider {
    Box::new(|_| Ok(Zeroizing::new("a".repeat(64))))
}

fn test_claim_client() -> ClaimClient {
    Box::new(|_, _, _| Ok(ClaimResponse::RateLimited))
}

fn test_active_device_provider() -> ActiveDeviceProvider {
    Box::new(|_| Ok(false))
}

/// M2-4b：默认 fixture 大喊即挂——既有测试都不设置 `remote_active_repo_id`，`current_config`
/// 的两态分支只在 active 已设时才调用这个 provider，所以正常情况下它永远不该被触发；一旦
/// 触发说明分支判定改坏了（「意外调用即失败」惯例）。
fn test_active_room_resolver() -> ActiveRoomResolver {
    Box::new(|project_id| {
        Err(format!(
            "unexpected active room resolution for project {project_id}"
        ))
    })
}

/// M2-4d：归属闸现在恒启用，`upstream_session_allowed`/`command_session_allowed`/
/// `filter_session_index_snapshot_for_active_repo` 不再有开关短路——只要真的调用到这些
/// 函数就必然会调用这个 provider。默认返回 `Err(...)`（同 `test_active_room_resolver` 的
/// "意外调用即失败"惯例）——**这个默认值只适合"确认归属闸压根没被触发"的测试**（因为
/// `active_repo_id_for_gating` 也保持默认 `None`，两者都不动才谈得上"没被触发"）；一旦
/// 测试真的会经过归属闸（`handle_frame`/`drain_upstream`/`drain_milestone_queue` 处理带
/// `session` 的帧），必须换成 `test_session_repo_provider_allowing_default_repo()` 并配
/// `with_default_active_repo(...)`，否则 `Err` 会被 `repo_id_is_active` 判成"不属于"，
/// 把一堆不关心归属过滤本身的既有测试（input 路由/control stop/replay/drain 预算等）
/// 全部 fail-closed 挡下（M2-4d 收尾时实测踩过这个坑：以为"默认拒绝无害"，实际让 20 个
/// 无关测试全红）。
fn test_session_repo_provider() -> SessionRepoProvider {
    Box::new(|session_id| {
        Err(format!(
            "unexpected session repo lookup for session {session_id}"
        ))
    })
}

/// M2-4d：单活跃房间模型下归属闸恒启用，多数不关心归属过滤本身的既有测试需要一个"每个
/// session 都属于同一个 active repo"的默认世界，而不是每条测试各自显式配置。跟
/// `with_default_active_repo` 配套使用——两者必须成对出现，只设其中一个没有意义（provider
/// 说"属于" active repo 但 `active_repo_id_for_gating` 仍是 `None` 时，`repo_id_is_active`
/// 照样 fail-closed；反之亦然）。
const TEST_DEFAULT_ACTIVE_REPO_ID: &str = "test-active-repo";

fn test_session_repo_provider_allowing_default_repo() -> SessionRepoProvider {
    Box::new(|_session_id| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())))
}

fn test_session_history_provider() -> SessionHistoryProvider {
    Box::new(|session_id, _, _| {
        Err(format!(
            "unexpected session history lookup for session {session_id}"
        ))
    })
}

/// msgfix1 T4：默认对未预期的 `msg.fetch` provider 调用报错——同
/// `test_session_history_provider` 既有姿势，测试没预期到会命中这里就该显式失败，而不是
/// 悄悄返回一个看似合理的默认值掩盖误用。
fn test_message_fetch_provider() -> MessageFetchProvider {
    Box::new(|session_id, message_id| {
        Err(format!(
            "unexpected message fetch lookup for session {session_id} message {message_id}"
        ))
    })
}

/// 见 `test_session_repo_provider_allowing_default_repo` 文档——两者成对使用。
fn with_default_active_repo(inner: Arc<Inner>) -> Arc<Inner> {
    *lock(&inner.state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    inner
}

fn test_registry() -> Arc<Mutex<RegistryState>> {
    Arc::new(Mutex::new(RegistryState::default()))
}

fn test_registry_snapshot_provider() -> RegistrySnapshotProvider {
    Box::new(|_, _| {
        Ok(RegistrySnapshot {
            revision: 1,
            entries: Vec::new(),
        })
    })
}

fn test_registry_rebase_provider() -> RegistryRebaseProvider {
    Box::new(|_, high_water, _, include_pairing, revoke_subjects| {
        let revoke_generations = revoke_subjects
            .iter()
            .enumerate()
            .map(|(offset, subject)| (subject.clone(), high_water + 2 + offset as i64))
            .collect();
        Ok((
            RegistrySnapshot {
                revision: high_water + 1,
                entries: Vec::new(),
            },
            include_pairing.then_some(high_water + 1),
            revoke_generations,
        ))
    })
}

fn test_registry_high_water_provider() -> RegistryHighWaterProvider {
    Box::new(|_, _, _| Ok(Vec::new()))
}

/// S1i1：不给 refresh 编排的填充默认——直接回一个通用 fail，不影响不专门打
/// `token.refresh.forward` 的既有测试。真正的 refresh 行为测试见本文件专门的
/// `refresh_*` 测试组，那些测试各自构造自己的 `RefreshHandler`。
fn test_refresh_handler() -> RefreshHandler {
    Box::new(|frame| {
        RefreshOutcome::Reply(refresh_fail_json(
            &frame.request_id,
            &frame.subject,
            "unsupported",
            false,
        ))
    })
}

/// `build_msg_completed_payload` 把预算好的 content_ref 挂在私有键
/// `MSG_COMPLETED_REF_SOURCE_KEY` 下（供 `enqueue_milestone_item` 消费，见该常量文档）——
/// 测试断言公开可见的 wire 形状时先剥掉它，就像生产路径最终发出去之前必然经历的那一步。
fn strip_ref_source(mut payload: Value) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.remove(MSG_COMPLETED_REF_SOURCE_KEY);
    }
    payload
}

fn text_delta_payload(session_id: &str) -> crate::event_transport::BatchPayload {
    crate::event_transport::BatchPayload {
        batches: vec![crate::event_transport::RunBatch {
            session_id: session_id.to_owned(),
            run_id: "run-1".to_owned(),
            dispatch: None,
            events: vec![crate::event_transport::SequencedEvent {
                seq: 1,
                event: crate::agent_event::AgentEvent::TextDelta {
                    text: "hi".to_owned(),
                },
            }],
        }],
    }
}

fn single_event_payload(
    run_id: &str,
    session_id: &str,
    event: crate::agent_event::AgentEvent,
) -> crate::event_transport::BatchPayload {
    crate::event_transport::BatchPayload {
        batches: vec![crate::event_transport::RunBatch {
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            dispatch: None,
            events: vec![crate::event_transport::SequencedEvent { seq: 1, event }],
        }],
    }
}

fn test_inner(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
) -> Arc<Inner> {
    test_inner_with_interval(settings, token_provider, DEFAULT_LIVENESS_INTERVAL)
}

fn test_inner_with_interval(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
    liveness_interval: Duration,
) -> Arc<Inner> {
    test_inner_with_interval_and_k_room(settings, token_provider, |_| None, liveness_interval)
}

fn test_inner_with_k_room_provider(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
    k_room_provider: impl Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync + 'static,
) -> Arc<Inner> {
    test_inner_with_interval_and_k_room(
        settings,
        token_provider,
        k_room_provider,
        DEFAULT_LIVENESS_INTERVAL,
    )
}

fn test_inner_with_interval_and_k_room(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
    k_room_provider: impl Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync + 'static,
    liveness_interval: Duration,
) -> Arc<Inner> {
    test_inner_with_interval_k_room_and_active_room_resolver(
        settings,
        token_provider,
        k_room_provider,
        test_active_room_resolver(),
        liveness_interval,
    )
}

/// M2-4d：`test_inner_with_interval_and_k_room` 的通用版——额外暴露 `active_room_resolver`。
/// 需要覆盖 liveness 轮询期间的 `current_config` 反复重新解析（例如验证长连接期间轮询
/// 次数的测试）时，settings 必须真的带上 `remote_active_repo_id` 才能让 `current_config`
/// 持续解出跟已连接配置一致的房间，不然每一轮轮询都会判"未配置"→ 立刻当成配置陈旧断开，
/// 测试永远等不到期望的轮询次数。
fn test_inner_with_interval_k_room_and_active_room_resolver(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
    k_room_provider: impl Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync + 'static,
    active_room_resolver: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
    liveness_interval: Duration,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    Arc::new(Inner {
        settings: Box::new(settings),
        token_provider: Box::new(token_provider),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: Box::new(active_room_resolver),
        k_room_provider: Box::new(k_room_provider),
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
        session_repo_provider: test_session_repo_provider(),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval,
    })
}

/// M2-4b：跟 `test_inner` 同一套默认 fixture，只是把 `active_room_resolver` 换成调用方
/// 提供的实现——三态解析测试 / 切房 liveness 测试专用。`token_provider` 固定 `|| None`；
/// 需要自定义 token_provider（例如让它 panic）时用
/// `test_inner_with_token_provider_and_active_room_resolver`。
fn test_inner_with_active_room_resolver(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    active_room_resolver: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
) -> Arc<Inner> {
    test_inner_with_token_provider_and_active_room_resolver(settings, || None, active_room_resolver)
}

/// M2-4d：`test_inner_with_active_room_resolver` 的通用版——额外暴露 `token_provider`。
/// legacy 全局房回落撤除后，`token_provider_panic_is_caught_and_recovered_as_backoff` 这类
/// 「验证 token_provider 面板」的测试也必须真的把 active 房解析通道接上，才能让
/// `attempt_once` 走到调用 token_provider 那一步（不然 `current_config` 判"未配置"，
/// `attempt_once` 提前短路返回 `Waiting`，token_provider 压根不会被调用）。
fn test_inner_with_token_provider_and_active_room_resolver(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
    active_room_resolver: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    Arc::new(Inner {
        settings: Box::new(settings),
        token_provider: Box::new(token_provider),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: Box::new(active_room_resolver),
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
        session_repo_provider: test_session_repo_provider(),
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

fn test_inner_with_registry_providers(
    registry_snapshot_provider: RegistrySnapshotProvider,
    registry_rebase_provider: RegistryRebaseProvider,
) -> Arc<Inner> {
    test_inner_with_registry_providers_and_high_water(
        registry_snapshot_provider,
        registry_rebase_provider,
        test_registry_high_water_provider(),
    )
}

fn test_inner_with_registry_providers_and_high_water(
    registry_snapshot_provider: RegistrySnapshotProvider,
    registry_rebase_provider: RegistryRebaseProvider,
    registry_high_water_provider: RegistryHighWaterProvider,
) -> Arc<Inner> {
    test_inner_with_registry_sync_providers(
        registry_snapshot_provider,
        registry_rebase_provider,
        registry_high_water_provider,
        Box::new(|| None),
    )
}

fn test_inner_with_registry_sync_providers(
    registry_snapshot_provider: RegistrySnapshotProvider,
    registry_rebase_provider: RegistryRebaseProvider,
    registry_high_water_provider: RegistryHighWaterProvider,
    session_index_snapshot_provider: SessionIndexSnapshotProvider,
) -> Arc<Inner> {
    test_inner_with_registry_sync_providers_and_settings(
        |_| None,
        test_active_room_resolver(),
        registry_snapshot_provider,
        registry_rebase_provider,
        registry_high_water_provider,
        session_index_snapshot_provider,
    )
}

fn test_inner_with_registry_sync_providers_and_settings(
    settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    active_room_resolver: ActiveRoomResolver,
    registry_snapshot_provider: RegistrySnapshotProvider,
    registry_rebase_provider: RegistryRebaseProvider,
    registry_high_water_provider: RegistryHighWaterProvider,
    session_index_snapshot_provider: SessionIndexSnapshotProvider,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    Arc::new(Inner {
        settings: Box::new(settings),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver,
        k_room_provider: Box::new(|_| Some(Zeroizing::new([9_u8; 32]))),
        session_index_snapshot_provider,
        milestone_replay_provider: Box::new(|| None),
        session_runtime_replay_provider: Box::new(|| None),
        pair_hello_handler: Box::new(|_| None),
        pair_done_handler: Box::new(|_| PairDoneAction::Rejected),
        registry: test_registry(),
        refresh_handler: test_refresh_handler(),
        registry_snapshot_provider,
        registry_rebase_provider,
        registry_high_water_provider,
        input_send_handler: Box::new(|_| Some(AckOutcome::Failed)),
        input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(|_| AckOutcome::Failed),
        upstream_tx,
        milestone_tx,
        session_repo_provider: test_session_repo_provider(),
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

fn test_inner_with_pair_handlers(
    pair_hello_handler: impl Fn(PairHelloFrame) -> Option<PairAcceptFrame> + Send + Sync + 'static,
    pair_done_handler: impl Fn(PairDoneFrame) -> PairDoneAction + Send + Sync + 'static,
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
        pair_hello_handler: Box::new(pair_hello_handler),
        pair_done_handler: Box::new(pair_done_handler),
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
        session_repo_provider: test_session_repo_provider(),
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

fn test_inner_with_input_control_handlers(
    input_send_handler: impl Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync + 'static,
    control_stop_handler: impl Fn(ControlStopFrame) -> AckOutcome + Send + Sync + 'static,
) -> Arc<Inner> {
    test_inner_with_input_control_replay_handlers(
        input_send_handler,
        |_, _| true,
        control_stop_handler,
    )
}

fn test_inner_with_input_control_replay_handlers(
    input_send_handler: impl Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync + 'static,
    control_replay_handler: impl Fn(&str, &str) -> bool + Send + Sync + 'static,
    control_stop_handler: impl Fn(ControlStopFrame) -> AckOutcome + Send + Sync + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
    with_default_active_repo(Arc::new(Inner {
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
        input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
        control_replay_handler: Box::new(control_replay_handler),
        control_stop_handler: Box::new(control_stop_handler),
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
    }))
}

/// M2-4c/M2-4d：命令归属闸测试专用 fixture——现有 `test_inner_with_*` 组合都不暴露
/// `session_repo_provider` 参数（它们默认走 `test_session_repo_provider()` 的"意外调用即
/// 失败"占位）。这里显式接收调用方控制的 `session_repo_provider` + `input_send_handler`；
/// `active_repo_id_for_gating` 由调用方在拿到 `Arc<Inner>` 之后自己按需 store
/// （`GatewayInnerState::default()` 里恒 `None`，不需要这里额外分支——归属闸本身已恒
/// 启用，不再有开关字段要设）。
fn test_inner_for_command_attribution(
    session_repo_provider: impl Fn(&str) -> Result<Option<String>, String> + Send + Sync + 'static,
    input_send_handler: impl Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync + 'static,
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
        input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(|_| AckOutcome::Failed),
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

fn test_inner_with_k_room_and_session_index_provider(
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
    k_room_provider: impl Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync + 'static,
    session_index_snapshot_provider: impl Fn() -> Option<Value> + Send + Sync + 'static,
) -> (Arc<Inner>, Receiver<(u64, MilestoneItem)>) {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    let inner = Arc::new(Inner {
        settings: Box::new(|_| None),
        token_provider: Box::new(token_provider),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: test_active_room_resolver(),
        k_room_provider: Box::new(k_room_provider),
        session_index_snapshot_provider: Box::new(session_index_snapshot_provider),
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
        session_repo_provider: test_session_repo_provider(),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    });
    (inner, milestone_rx)
}

fn test_inner_with_k_room_snapshot_and_replay_providers(
    token_provider: impl Fn() -> Option<String> + Send + Sync + 'static,
    k_room_provider: impl Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync + 'static,
    session_index_snapshot_provider: impl Fn() -> Option<Value> + Send + Sync + 'static,
    milestone_replay_provider: impl Fn() -> Option<Vec<crate::db::MilestoneReplayRow>>
        + Send
        + Sync
        + 'static,
    session_runtime_replay_provider: impl Fn() -> Option<Vec<crate::db::SessionRuntimeReplayRow>>
        + Send
        + Sync
        + 'static,
) -> (Arc<Inner>, Receiver<(u64, MilestoneItem)>) {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(16);
    let inner = Arc::new(Inner {
        settings: Box::new(|_| None),
        token_provider: Box::new(token_provider),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: test_active_room_resolver(),
        k_room_provider: Box::new(k_room_provider),
        session_index_snapshot_provider: Box::new(session_index_snapshot_provider),
        milestone_replay_provider: Box::new(milestone_replay_provider),
        session_runtime_replay_provider: Box::new(session_runtime_replay_provider),
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
        session_repo_provider: test_session_repo_provider(),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: test_message_fetch_provider(),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    });
    (inner, milestone_rx)
}

fn seal_command_envelope(
    k_room: &Zeroizing<[u8; 32]>,
    room: &str,
    epoch: u64,
    kind: &str,
    session: &str,
    command_id: &str,
    payload: &Value,
) -> Value {
    let meta = crate::remote_crypto::EnvelopeMeta {
        v: 1,
        room: room.to_owned(),
        epoch,
        kind: kind.to_owned(),
        session: Some(session.to_owned()),
        command_id: Some(command_id.to_owned()),
    };
    let (ct, n) = crate::remote_crypto::seal(k_room, &meta, &serde_json::to_vec(payload).unwrap());
    serde_json::json!({
        "v": 1,
        "room": room,
        "epoch": epoch,
        "kind": kind,
        "session": session,
        "command_id": command_id,
        "seq": Value::Null,
        "ct": ct,
        "n": n,
        "ts": now_unix_ms(),
    })
}

fn open_upstream_envelope(k_room: &[u8; 32], envelope: &Value) -> Value {
    let meta = EnvelopeMeta {
        v: envelope["v"].as_u64().unwrap() as u32,
        room: envelope["room"].as_str().unwrap().to_owned(),
        epoch: envelope["epoch"].as_u64().unwrap(),
        kind: envelope["kind"].as_str().unwrap().to_owned(),
        session: envelope["session"].as_str().map(str::to_owned),
        command_id: envelope["command_id"].as_str().map(str::to_owned),
    };
    let plaintext = crate::remote_crypto::open(
        k_room,
        &meta,
        envelope["ct"].as_str().unwrap(),
        envelope["n"].as_str().unwrap(),
    )
    .expect("upstream envelope must decrypt with protocol AAD");
    serde_json::from_slice(&plaintext).expect("upstream plaintext must be JSON")
}

fn spawn_frame_pump_server() -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("frame pump listener should bind");
    let addr = listener
        .local_addr()
        .expect("frame pump listener should have an address");
    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            if let Ok(mut socket) = tungstenite::accept(stream) {
                ack_initial_registry_sync(&mut socket);
                for _ in 0..60 {
                    if socket.send(Message::Text("{}".into())).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
    });
    (addr, handle)
}

fn spawn_discarding_server() -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("discarding listener should bind");
    let addr = listener
        .local_addr()
        .expect("discarding listener should have an address");
    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            if let Ok(mut socket) = tungstenite::accept(stream) {
                while socket.read().is_ok() {}
            }
        }
    });
    (addr, handle)
}

fn spawn_recording_server(
    expected_frames: usize,
) -> (
    std::net::SocketAddr,
    mpsc::Receiver<Value>,
    thread::JoinHandle<()>,
) {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("recording listener should bind");
    let addr = listener
        .local_addr()
        .expect("recording listener should have an address");
    let (frame_tx, frame_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("recording server should accept");
        let mut socket = tungstenite::accept(stream).expect("websocket handshake should pass");
        for _ in 0..expected_frames {
            let message = socket
                .read()
                .expect("recording server should receive a frame");
            let Message::Text(text) = message else {
                panic!("recording server expected a text frame");
            };
            let value =
                serde_json::from_str(text.as_ref()).expect("recorded upstream text must be JSON");
            frame_tx.send(value).unwrap();
        }
    });
    (addr, frame_rx, handle)
}

fn ack_initial_registry_sync(socket: &mut tungstenite::WebSocket<TcpStream>) -> Value {
    let message = socket
        .read()
        .expect("relay should receive initial token.sync");
    let Message::Text(text) = message else {
        panic!("initial registry frame must be text");
    };
    let frame: Value = serde_json::from_str(text.as_ref()).expect("token.sync must be JSON");
    assert_eq!(frame["t"], "token.sync");
    let revision = frame["revision"]
        .as_i64()
        .expect("token.sync revision must be an integer");
    socket
        .send(Message::Text(
            serde_json::json!({
                "t": "token.sync.ack",
                "revision": revision,
                "relay_high_water": revision,
            })
            .to_string()
            .into(),
        ))
        .expect("relay should send token.sync.ack");
    frame
}

/// S1i1 返工四 H1：等待下一帧**业务** Text，中途路过的 Ping/Pong/Binary/Frame 一律跳过
/// 不当数据——tungstenite 收到对端 Ping 时会在**自己**下一次 `read`/`flush` 调用里自动补发
/// 一帧 Pong（本文件 `Ok(Message::Ping(_))` 分支的注释也提过这一点），从服务端视角，这个
/// 自动 Pong 可能夹在两帧业务 Text 之间到达；用这个 helper 读，不管夹不夹都能拿到期望的
/// 那帧 Text，不会被控制帧类型的具体到达时序绊倒。
fn recv_text_skip_control(socket: &mut tungstenite::WebSocket<TcpStream>) -> Value {
    loop {
        match socket
            .read()
            .expect("socket read failed while waiting for a text frame")
        {
            Message::Text(text) => {
                return serde_json::from_str(text.as_ref()).expect("frame must be JSON");
            }
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {
                continue;
            }
            Message::Close(_) => panic!("connection closed while waiting for a text frame"),
        }
    }
}

fn wait_until_connected(inner: &Inner) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if lock(&inner.state.status).state == GatewayState::Connected {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("connection did not reach Connected state within two seconds");
}

fn wait_until_stopped_reason(inner: &Inner, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if lock(&inner.state.status).stopped_reason.as_deref() == Some(expected) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("gateway did not publish stopped reason {expected} within two seconds");
}

fn wait_until_counter_at_least(counter: &AtomicU64, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if counter.load(Ordering::Relaxed) >= expected {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("counter did not reach {expected} within two seconds");
}

fn join_connection_within(
    connection: thread::JoinHandle<Result<ConnectionExit, ConnectionFailure>>,
    inner: &Inner,
) -> Result<ConnectionExit, ConnectionFailure> {
    join_connection_within_budget(connection, inner, Duration::from_secs(2))
}

/// S1i1 返工四 H1：跟 `join_connection_within` 同一套「把『是否按时结束』变成可断言的值」
/// 手法，budget 可调——`registry_resync_pending` 的硬截止本身就是 2 秒，繁忙连接测试从
/// `pending_since` 起算到真正断开还要再加上连接建立、处理 rejected 帧等前置耗时，套用
/// 字面 2 秒会跟被测的硬截止打得太近、天然不稳定，需要更宽的预算；`finished_in_time` 仍在
/// 预算耗尽的那一刻算好，之后即便强制 shutdown 让线程尽快退出，也不会把「迟到」洗白成
/// 「按时」。
fn join_connection_within_budget(
    connection: thread::JoinHandle<Result<ConnectionExit, ConnectionFailure>>,
    inner: &Inner,
    budget: Duration,
) -> Result<ConnectionExit, ConnectionFailure> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline && !connection.is_finished() {
        thread::sleep(Duration::from_millis(20));
    }
    let finished_in_time = connection.is_finished();
    if !finished_in_time {
        inner.shutdown.store(true, Ordering::Release);
    }
    let result = connection
        .join()
        .expect("connection thread should not panic");
    assert!(
        finished_in_time,
        "connection did not finish within budget {budget:?}"
    );
    result
}

/// 测试专用直连——绕开 `configure_activity_summary_writer`（需要真实 writer + 起线程），
/// 只把 `state.activity_summary_tx` 设成测试自己持有的 tx 端，这样 `extract_tool_milestones`
/// 就会走"聚合器已启用"分支产 delta，测试直接在 `rx` 端断言，不需要真的跑写线程。
fn configure_activity_summary_writer_test_hook(
    state: &GatewayInnerState,
    tx: SyncSender<ActivitySummaryDelta>,
) {
    state
        .activity_summary_tx
        .set(tx)
        .expect("test hook must only be called once per state");
}

mod activity_milestones;
mod activity_persistence;
mod activity_state;
mod activity_worker;
mod command_attribution;
mod command_dispatch;
mod command_freshness;
mod configuration;
mod connection_health;
mod connection_liveness;
mod data_plane_contract;
mod diagnostics;
mod handshake_claim;
mod history;
mod live_classification;
mod live_connection;
mod message_chunks;
mod message_fetch;
mod milestone_delivery;
mod milestone_replay;
mod oversized_previews;
mod pairing;
mod partial_snapshots;
mod payload_builders;
mod registry_outbox;
mod registry_refresh_reconnect;
mod registry_resync_deadline;
mod registry_revoke_reconnect;
mod registry_sync;
mod session_index_filter;
mod snapshot_budget;
mod snapshot_commands;
mod snapshot_worker;
mod tool_correlation;
mod tool_milestone_delivery;
mod upstream_gate;
mod wire;
mod wire_envelopes;
