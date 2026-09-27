#![allow(dead_code)] // Some gateway diagnostics remain reserved for the remote-control status UI.

use crate::remote_crypto::EnvelopeMeta;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::TcpStream;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::client::{connect_with_config, IntoClientRequest};
use tungstenite::http::header::AUTHORIZATION;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error as WebSocketError, Message};
use zeroize::Zeroizing;

mod activity_summary;
mod command_dispatch;
mod command_envelope;
mod connect_loop;
mod connect_replay;
mod connection_loop;
mod connection_request;
mod constants;
mod frame_dispatch;
mod frames;
mod input_ack_outbox;
mod live_milestone_drain;
mod milestone_payloads;
mod msg_fetch;
mod msg_fetch_content;
mod msg_fetch_handlers;
mod partial_snapshot;
mod provider_types;
mod refresh_pairing_wire;
mod registry_state;
mod registry_sync;
mod session_repo_gate;
mod setup_claim;
mod snapshot_history;
mod token_lifecycle;
mod tool_milestone;

pub(crate) use activity_summary::ActivitySummaryWriter;
use activity_summary::*;
use command_dispatch::*;
use connect_loop::*;
use connect_replay::*;
use connection_loop::*;
use constants::*;
pub(crate) use constants::{effective_relay_url, DEFAULT_PUBLIC_RELAY_URL};
use frame_dispatch::*;
pub(crate) use frame_dispatch::{install_activity_summary_writer, install_event_sink};
pub(crate) use frames::{
    AckOutcome, ControlReplayHandler, ControlStopFrame, ControlStopHandler, InputAnswerFrame,
    InputAnswerHandler, InputSendFrame, InputSendHandler, PairAcceptFrame, PairDoneAction,
    PairDoneFrame, PairHelloFrame, PairReadyFrame, TokenAckAction,
};
use input_ack_outbox::drain_input_ack_outbox;
pub(crate) use input_ack_outbox::enqueue_failed_input_ack;
#[cfg(test)]
use input_ack_outbox::{drain_input_ack_outbox_with, enqueue_failed_input_ack_into};
use live_milestone_drain::*;
use milestone_payloads::*;
pub(crate) use milestone_payloads::{
    derive_msg_completed_client_msg_id, publish_card_created_milestone,
    publish_card_resolved_milestone, publish_msg_completed_milestone, publish_run_status_milestone,
    publish_session_index_archived, publish_session_index_created, publish_session_index_deleted,
    publish_session_index_renamed,
};
use msg_fetch_content::*;
use msg_fetch_handlers::*;
use partial_snapshot::*;
#[cfg(test)]
pub(crate) use partial_snapshot::{
    test_take_publish_log, test_take_run_status_payload_log,
    test_take_session_index_archived_payload_log, test_take_session_index_created_payload_log,
};
pub(crate) use provider_types::{
    ActiveDeviceProvider, ActiveRoomResolver, ClaimClient, DesktopCredentialProvider,
    KRoomProvider, MessageFetchProvider, MessageForFetchResult, MilestoneReplayProvider,
    PairDoneHandler, PairHelloHandler, RefreshForwardFrame, RefreshHandler, RefreshOkFrame,
    RefreshOutcome, RegistryHighWaterProvider, RegistryRebaseProvider, RegistrySnapshot,
    RegistrySnapshotProvider, SessionHistoryProvider, SessionHistoryRow,
    SessionIndexSnapshotProvider, SessionRepoProvider, SessionRuntimeReplayProvider,
    SettingsReader, TokenProvider, TokenSyncCurrent, TokenSyncEntry, TokenSyncPrev,
};
pub(crate) use refresh_pairing_wire::refresh_fail_json;
pub(crate) use refresh_pairing_wire::refresh_ok_json;
use refresh_pairing_wire::*;
pub(crate) use registry_state::RegistryState;
#[cfg(test)]
use registry_state::*;
use registry_sync::*;
use session_repo_gate::*;
use setup_claim::*;
pub(crate) use setup_claim::{claim_room_blocking, setup};
use snapshot_history::*;
use token_lifecycle::*;
use tool_milestone::*;

static GATEWAY: OnceLock<Arc<Inner>> = OnceLock::new();

/// `sessions.repo_id` is mutable through the registered `update_session_repo` IPC in `lib.rs`, so ownership caches require an invalidation epoch.
/// 已注册运行时改绑 IPC（当前前端生产代码没有调用点，但保留给未来"挪会话到别的项目"功能，
/// 且它是普通内核函数，测试 / 未来功能都能直接触发）。这个进程级代号在每次改绑成功后 +1；
/// `upstream_session_allowed` 把它跟这条连接记录的基线比对，一旦不一致就整表清空
/// `session_repo_cache` 重建，防止缓存里的旧归属在同一条远端连接的生命周期内继续被当真。
static SESSION_REPO_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Call after successful `update_session_repo` reassignment to advance `SESSION_REPO_EPOCH` and invalidate cached ownership.
pub(crate) fn note_session_repo_reassignment() {
    SESSION_REPO_EPOCH.fetch_add(1, Ordering::Release);
}

#[derive(Clone, Debug, PartialEq)]
struct MilestoneItem {
    session: Option<String>,
    t: String,
    payload: Value,
    client_msg_id: String,
}

#[derive(Clone, Debug, PartialEq)]
enum LiveQueueItem {
    Batch(crate::event_transport::BatchPayload),
    Prebuilt(MilestoneItem),
}

struct Inner {
    settings: SettingsReader,
    token_provider: TokenProvider,
    desktop_credential_provider: DesktopCredentialProvider,
    claim_client: ClaimClient,
    active_device_provider: ActiveDeviceProvider,
    /// M2-4b：per-project 房间解析（含 ensure 房 + 凭据幂等先行），只在 `current_config` 判定
    /// active project 已设时调用。
    active_room_resolver: ActiveRoomResolver,
    k_room_provider: KRoomProvider,
    session_index_snapshot_provider: SessionIndexSnapshotProvider,
    milestone_replay_provider: MilestoneReplayProvider,
    session_runtime_replay_provider: SessionRuntimeReplayProvider,
    pair_hello_handler: PairHelloHandler,
    pair_done_handler: PairDoneHandler,
    registry: Arc<Mutex<RegistryState>>,
    registry_snapshot_provider: RegistrySnapshotProvider,
    registry_rebase_provider: RegistryRebaseProvider,
    registry_high_water_provider: RegistryHighWaterProvider,
    refresh_handler: RefreshHandler,
    input_send_handler: InputSendHandler,
    input_answer_handler: InputAnswerHandler,
    control_replay_handler: ControlReplayHandler,
    control_stop_handler: ControlStopHandler,
    upstream_tx: SyncSender<(u64, LiveQueueItem)>,
    milestone_tx: SyncSender<(u64, MilestoneItem)>,
    /// M2-4c/M2-4d：命令归属 fail-closed 判定用的 session → repo 查询——单活跃房间模型下归属
    /// 闸恒启用，一旦有活连接（`GatewayConfig` 被构造出来即意味着 `active_repo_id` 恒
    /// `Some`），这个 provider 就会被调用。
    session_repo_provider: SessionRepoProvider,
    session_history_provider: SessionHistoryProvider,
    /// DB provider for the first `msg.fetch` validation step, checking exact message ownership; see `MessageFetchProvider`.
    message_fetch_provider: MessageFetchProvider,
    state: GatewayInnerState,
    shutdown: AtomicBool,
    reload_requested: AtomicBool,
    /// Registry publication wakes stopped/backoff connection attempts without making a live
    /// socket stale. The live loop consumes this flag and drains the outbox in place.
    registry_publish_wake: AtomicBool,
    active_token: Mutex<Option<String>>,
    liveness_interval: Duration,
}

struct GatewayInnerState {
    epoch: AtomicU64,
    head_seq: AtomicU64,
    frames_seen: AtomicU64,
    frames_sent: AtomicU64,
    keepalive_pings_sent: AtomicU64,
    bad_frames: AtomicU64,
    upstream_state: AtomicU64,
    upstream_dropped: AtomicU64,
    upstream_stale_generation_dropped: AtomicU64,
    upstream_budget_dropped: AtomicU64,
    milestone_dropped: AtomicU64,
    session_index_snapshot_unavailable: AtomicU64,
    /// 每次连接建立时，`request_session_index_snapshot` 把它请求的 generation 存到这里，再给
    /// 常驻 worker 发一个容量为 1 的唤醒信号。worker 每轮都重新读这个字段拿"当前最新请求的
    /// generation"来服务，一串快速重连最终会被折叠成"同一线程串行跑若干轮、每轮服务当时
    /// 最新的 generation"，中间被超越的请求不需要各自跑一轮（M#4/M#5 复审修复）。
    snapshot_requested_generation: AtomicU64,
    /// 常驻 worker 的唤醒端，由 `ensure_snapshot_worker` 惰性初始化。`OnceLock` 让进程生命周期
    /// 内至多执行一次成功的线程创建，从结构上消灭旧 clear-then-recheck 退休窗口里的重复 spawn。
    snapshot_wake_tx: OnceLock<SyncSender<()>>,
    /// 诊断用：常驻线程实际 spawn 成功的次数。未请求过快照时为 0；一旦请求过，进程生命周期
    /// 内恒为 1，不随连接重建次数增长。
    snapshot_worker_spawn_count: AtomicU64,
    /// Counts FIFO evictions when the bounded tool-correlation table admits a new key.
    tool_correlation_dropped: AtomicU64,
    classify_skipped: AtomicU64,
    connection_failures: AtomicU64,
    panics: AtomicU64,
    disconnect_config_stale: AtomicU64,
    disconnect_closed_by_peer: AtomicU64,
    disconnect_error: AtomicU64,
    last_disconnect_reason: Mutex<String>,
    status: Mutex<GatewayStatus>,
    /// M2-4d：这条连接解析出的 active project id（= `GatewayConfig::active_repo_id`，由
    /// `run_connection_request` 在连接建立时写入一次）。特意不重读 `remote_active_repo_id`
    /// 设置——那样会在"设置的当前值"与"这条连接实际连的是哪个房间"之间造出双真相（例如切
    /// 项目后旧连接还没被 liveness 判死重连的窗口内）。`handle_command_envelope`（下行）与
    /// `drain_milestone_queue`/`drain_live_queue`/`publish_session_index_snapshot_on_connect`
    /// （上行）只读这个值——单活跃房间模型下命令归属闸恒启用（原先按 `RoomSource::Active` 分流
    /// 的 `command_gating_active` 布尔闸已随 legacy 回落一并撤除，见 M2-4d），有连接就必然有
    /// `Some(active_repo_id)`。
    active_repo_id_for_gating: Mutex<Option<String>>,
    /// M2-4c：诊断计数——因归属闸判定"不属于 active repo"而在 drain 阶段被静默丢弃的上行
    /// 里程碑 + live 事件条数（这是过滤，不是错误，不计入 `upstream_dropped`/
    /// `classify_skipped` 等既有语义不同的计数器）。
    upstream_repo_filtered: AtomicU64,
    /// Tool-name correlation is keyed by `(run_id, tool_id)`, so a reused tool id cannot pick up
    /// a stale name from another run in the same session. `names` and `order` always contain the
    /// same keys: insertion appends to `order`, consumption and run-terminal cleanup remove from
    /// both, and a full table evicts the oldest orphan from both before admitting the new key.
    /// Thus `order.len() == names.len()` remains bounded by `TOOL_CORRELATION_CAPACITY`.
    ///
    /// This private mutex — together with `partial_snapshots` below (P0-b) — is the deliberate,
    /// narrow exception to `EventTransport::add_sink`'s "no other lock" wording. Only the
    /// correlation helpers and the `maintain_partial_snapshots`/snapshot-read helpers acquire
    /// either of these two mutexes; EventTransport never does, and neither helper ever acquires
    /// the other lock or any other lock, so there is no crossed lock order between them. While
    /// held they perform only in-memory HashMap/VecDeque insertion, removal, or a bounded scan
    /// (at most 1024 tool-correlation entries / at most `PARTIAL_SNAPSHOT_CAPACITY` sessions): no
    /// I/O, blocking send, panic, or nested locking. The work is bounded and microsecond-scale
    /// (`DisplayReducer::feed` on one event is pure in-memory Vec/String mutation — the same cost
    /// class as its existing use on the run-finalization path). The same correlation access
    /// formerly happened in the network thread; moving it to the serialized sink path removes
    /// that old access rather than adding another concurrent accessor, preserving the
    /// non-blocking/deadlock-free intent of the sink contract.
    tool_correlation: Mutex<ToolCorrelationState>,
    /// P0-b：每 session 的"当前 run 归约态"——`control.snapshot` 臂原子读取的唯一真相源。维护
    /// 点与 `tool_correlation` 同层：`maintain_partial_snapshots` 在 sink 入队路径（emitter
    /// 线程，`enqueue_batch_payload_for_upstream`）同步调用；`handle_command_envelope` 的
    /// snapshot 臂在命令处理线程原子读取——两者互不嵌套持锁，安全论证见上方共享豁免段落。
    /// P0-b 返工⑥a 如实补记：ws 读线程在锁内深拷贝 blocks（`reducer.snapshot_blocks()`）·
    /// 对持 `emit_serial` 的 sink 构成短暂反压——这是新增的跨线程耦合，量级是 memcpy，不是新
    /// 的阻塞/I/O 风险，但与上方共享豁免段落"仅 sink 侧访问"的表述不完全等价，故单独记档。
    /// **P0-b 微返工第 3 轮如实改写（原措辞误称 clone "bounded/短暂"）**：clone 的 blocks
    /// 量随 run 内容持续增长、**无总量上限**——`maintain_partial_snapshots` 在 run 进行期间
    /// 只管往 reducer 里累积喂事件，不做任何裁剪；`SNAPSHOT_PAYLOAD_BUDGET_BYTES` 那套预算
    /// 收敛只发生在构造 `control.snapshot` 应答 payload 的那一刻（出帧时），这里的维护态
    /// （`partial_snapshots` 里累积的 blocks 本身）不裁。
    partial_snapshots: Mutex<HashMap<String, PartialSnapshotState>>,
    /// P0-b 返工⑤：`partial_snapshots` 满表时拒收新 session 条目的计数——原为 `eprintln!`
    /// （sink 回调锁内无界刷屏），改原子计数器，命名照 `tool_correlation_dropped` 族惯例。
    partial_snapshot_capacity_dropped: AtomicU64,
    /// P0-b 返工·v1.8.12 ③ 发送侧兜底：`build_snapshot_payload` 收敛后仍超
    /// `SNAPSHOT_SEND_BUDGET_BYTES` 而被跳过发送的计数——只护 snapshot 这一条路径。
    snapshot_oversized_dropped: AtomicU64,
    /// Count history messages still over budget after tool-output truncation; preserve them as previews with content references when they fit.
    /// 降级为块级 preview + content_ref（设计稿 §A）。该计数器语义随之从"丢弃条数"改为"降级
    /// 为 preview 的条数"（极端兜底——连 preview+ref 单条页都装不下——仍如实丢弃，同样计入）。
    history_oversized_dropped: AtomicU64,
    /// Count `msg.completed` frames still over budget after tool-output truncation; use preview plus content reference when available instead of silently dropping.
    /// 静默丢弃**，降级为块级 preview + content_ref（设计稿 §A）。连接回放与 live 发布共用
    /// 同一入队闸和同一计数；语义同上改为"降级为 preview 的条数"（无 content_ref 来源的防御性
    /// 兜底分支仍保留旧的丢弃语义，同样计入本计数器）。
    replay_oversized_dropped: AtomicU64,
    /// Serialize the DB read and enqueue phase of `publish_run_status_replay_rows` with live status enqueues to preserve replay-before-live ordering.
    /// 入队 `run.status` 现状帧）与 `enqueue_run_status_milestone_with_gate`（真实运行时
    /// `publish_run_status_milestone` 的实时入队路径）共享这把锁——保证"该连接的 run.status
    /// 现状补发帧必须先于其后任何实时 run.status 帧入队"这一顺序不变量：补发批持锁跨越整个
    /// "读 DB + 入队"过程，期间任何真实状态翻转要么在读之前已落库（读到的就是新值，天然一致），
    /// 要么必须等补发批放锁后才能把新状态入队（必然排在补发帧之后，不会被陈旧帧倒灌覆盖）。
    run_status_replay_gate: Mutex<()>,
    /// Independent bounded queue for `msg.chunk` and `msg.fetch.error` frames prevents reply and event traffic from sharing capacity.
    /// ——与 event/live（`upstream_tx`/`milestone_tx`）完全独立，互不挤占容量。`drain_reply_
    /// queue` 每轮连接主循环先于 milestone/live 排空这里（见 `drain_upstream_with_budget`）。
    /// 之所以挂在 `GatewayInnerState`（一个 `Mutex<VecDeque<_>>`）而不是像 `upstream_tx`/
    /// `milestone_tx` 那样另开一对 `mpsc::sync_channel` 挂在 `Inner` 上：`GatewayInnerState`
    /// 全仓 60+ 处构造都走 `GatewayInnerState::default()`（只有 1 处 `Default` 实现），新增字段
    /// 零改动这些调用点；而 `Inner` 的新增字段需要同步改 ~20 处结构体字面量 + 把新 `Receiver`
    /// 一路穿 `connect_loop`/`connect_loop_with`/`attempt_once`/`run_connection_request` 的签名
    /// 和它们各自的测试闭包——量级差一个数量级，选前者。
    reply_queue: Mutex<VecDeque<ReplyQueueItem>>,
    /// Bounded queue for asynchronous terminal `input.ack` failures produced by remote inbox
    /// draining. Producers only enqueue plain JSON; the active connection loop owns all socket I/O.
    input_ack_outbox: Mutex<VecDeque<Value>>,
    /// 诊断：`reply_queue` 已满且连兜底的 `msg.fetch.error{busy}` 本身也塞不进去时的丢弃计数
    /// （双重饱和的极端情形，见 `handle_msg_fetch` 里 chunk 入队失败后的兜底分支）。
    reply_queue_dropped: AtomicU64,
    /// Count replies rejected by the dequeue-time connection-generation recheck separately from upstream event drops.
    /// 复核 `connection_generation` 不匹配（跨连接残留）而丢弃的 reply 条数——单开一对新计数器
    /// 而不是复用 `upstream_stale_generation_dropped`/`upstream_repo_filtered`，避免混淆那两个
    /// 计数器"上行里程碑 + live 事件"的既有文档语义（见 `drain_reply_queue` doc）。
    reply_stale_connection_dropped: AtomicU64,
    /// 返修③：出队时二次复核 session 不再属于 active repo（用户切换项目/重连后归属变化）而
    /// 丢弃的 reply 条数。
    reply_repo_filtered_dropped: AtomicU64,
    /// 返修②（skeptic 补审）：单飞行槽位超时被新请求接管时，从 `reply_queue` 里整体清掉的
    /// "上一个 generation 还没发出的残片"条数——见 `purge_stale_reply_queue_generation`。
    reply_queue_stale_generation_purged: AtomicU64,
    /// Track in-flight fetches by session so each session serves at most one fetch at a time, regardless of message identity.
    /// 维度（比 §10.9 条文字面的 `(session, message_id)` 更严——同一 session 同时只服务一条
    /// fetch，无论 message_id 是否相同），键为 session id。条目在 `drain_reply_queue` 真正送出
    /// 该 fetch 最后一帧时清除，或被 `MSG_FETCH_INFLIGHT_TIMEOUT_MS` 超时后新请求接管。
    /// 返修②（skeptic 补审）：**不再兼作"command_id 账本"**——`MsgFetchInflightEntry` 只保留
    /// 当前占用者的身份判定所需信息（含 `generation`），command_id 的"近期已终态、拒绝复用"
    /// 语义搬去独立的 `msg_fetch_command_ledger`（原设计里两者共用一张表，会在同 command_id
    /// 复用时把"新占用者的身份"和"旧 command_id 是否用过"这两个不同问题绑在同一条记录上）。
    msg_fetch_inflight: Mutex<HashMap<String, MsgFetchInflightEntry>>,
    /// Desktop-side byte budget over a sliding 60-second window provides an independent abuse guard in addition to relay accounting.
    /// Relay per-subject byte accounting does not replace this independent desktop guard; aggregate usage across the entire gateway.
    /// **gateway 全局聚合**（不再按 session 分桶）——见 `MSG_FETCH_BYTE_BUDGET_PER_WINDOW`
    /// 定义处的量级推导。
    msg_fetch_byte_budget: Mutex<VecDeque<(u64, usize)>>,
    /// Allocate a globally increasing generation for each `msg.fetch` accepted by `handle_msg_fetch_at`, including requests ending in errors.
    /// （无论最终成功还是走某个 error code）就从这里领一个全局单调递增的新值，写进
    /// `MsgFetchInflightEntry.generation`/`ReplyQueueItem.generation`。见
    /// `clear_msg_fetch_inflight_if_matches` doc——generation 保证"同一 session 先后两次接受
    /// （哪怕 command_id 相同）绝不会被彼此的残片/终片误伤"，是 command_id 复用防线的结构性
    /// 兜底（`msg_fetch_command_ledger` 是行为兜底，容量满了会被淘汰失效；generation 判定
    /// 不依赖容量、恒正确）。
    msg_fetch_generation_counter: AtomicU64,
    /// Terminal-command ledger keyed by `(session, command_id)` rejects recent reuse; see
    /// `msg_fetch_command_ledger_admit`。
    msg_fetch_command_ledger: Mutex<MsgFetchCommandLedger>,
    /// msgfix2 U1（设计稿 v4.1 §4.1）：L1 活动摘要聚合器的输入端——`extract_tool_milestones`
    /// （sink 回调，禁止 DB I/O）把 delta `try_send` 进这里；独立串行写线程
    /// （`run_activity_summary_worker`）在另一端消费、做节流/终态压制、调用注入的
    /// `ActivitySummaryWriter` 落库。惰性配置：`None` 时 `extract_tool_milestones` 直接跳过
    /// （功能整体禁用，不是丢弃——生产接线（真实 DB 写 provider）留后续刀，见
    /// `configure_activity_summary_writer` 文档）。挂在 `GatewayInnerState` 而不是 `Inner`：
    /// 同 `reply_queue` 既有理由（该字段文档已详述）——`GatewayInnerState::default()` 全仓
    /// 60+ 处零改动，`Inner` 的新增字段则要同步改 ~20+ 处结构体字面量 + `setup()` 签名。
    activity_summary_tx: OnceLock<SyncSender<ActivitySummaryDelta>>,
    /// 惰性 spawn 单发保证，同 `snapshot_wake_tx`/`ensure_snapshot_worker` 惯例。
    activity_summary_worker_spawn_count: AtomicU64,
    /// 诊断：`activity_summary_tx` 已配置但 channel 满/断连时的 try_send 失败计数（不是"功能
    /// 未启用"那种整体跳过——是"启用了但这一条具体丢了"）。
    activity_summary_dropped: AtomicU64,
}

/// One `GatewayInnerState::msg_fetch_inflight` entry separates timeout tracking from fetch ownership through its acceptance generation.
/// `generation`——`accepted_at_ms` 仍是超时判定唯一依据；`generation` 是"这个占用到底是不是
/// 我这次接受的那个"唯一判据（`command_id` 不再可靠，见 `clear_msg_fetch_inflight_if_matches`
/// doc：client 复用同一 command_id 时两次接受的 `command_id` 逐字节相同，只有 `generation`
/// 能区分）。
#[derive(Clone, Debug, PartialEq)]
struct MsgFetchInflightEntry {
    command_id: String,
    accepted_at_ms: u64,
    generation: u64,
}

/// Pending entry in the independent bounded reply queue; `drain_reply_queue` removes entries individually for sealing and transmission.
/// seal 成 `reply` kind 信封发出。
#[derive(Clone, Debug, PartialEq)]
struct ReplyQueueItem {
    session: Option<String>,
    command_id: String,
    payload: Value,
    /// 这一帧是不是该 fetch 的最后一帧——`msg.fetch.error` 恒为 `true`（单帧终态）；
    /// `msg.chunk` 序列只有最后一片为 `true`。`drain_reply_queue` 只在真正**发出**这一帧后才
    /// 清除 `msg_fetch_inflight` 里对应 session 的占用（不是入队时就清）——保证"占用直到真正
    /// 送达/终态"，而不是"一入队就当作已完成"，避免客户端在还有大量分片排队等发时就抢发新
    /// 一轮 fetch 把队列灌爆。
    final_frame: bool,
    /// Identify the accepted `msg.fetch` that owns this reply by generation so stale frames cannot release a newer fetch slot.
    /// generation——`final_frame` 帧真正发出时用它（不是 `command_id`）去匹配/清除
    /// `msg_fetch_inflight`，见 `clear_msg_fetch_inflight_if_matches` doc。同时也是
    /// `purge_stale_reply_queue_generation` 精确清除"被新请求接管前那次接受"残片的判据。
    generation: u64,
    /// Capture `connection_generation` at enqueue time so replies left over from an earlier connection are rejected during draining.
    /// （`GatewayInnerState::connection_generation_snapshot`）——`drain_reply_queue` 出队时
    /// 与当前连接的 generation 比对，跨连接的残片（断线重连后仍在队列里的旧数据）判过期丢弃，
    /// 对照 milestone/live 既有的 `(u64, Item)` generation 标记同一套机制。
    connection_generation: u64,
}

/// Bounded terminal-command ledger keyed by `(session, command_id)` prevents recent command reuse from creating ambiguous fetch ownership.
/// 终态账本——`handle_msg_fetch_at` 每接受一次请求（无论最终成功还是走某个 error code）就把
/// 这次的 `(session, command_id)` 记进来；下次同一 `(session, command_id)` 再来一次
/// `msg.fetch`，一律拒绝（回 `busy`，引导客户端换一个新 command_id）——从根上避免"同一
/// command_id 对应两次不同的处理"这条会让 `msg_fetch_inflight`/`reply_queue` 记账产生歧义的
/// 路径，而不是只靠 `generation` 号事后补救（见 `MsgFetchInflightEntry`/`ReplyQueueItem` doc）。
/// `seen`/`order` 键集合恒一致，容量满时 FIFO 淘汰最老一条——同 `ToolCorrelationState` 既有
/// 惯例（这份表本身仍是有界的，`generation` 判定不依赖它的完整性，容量淘汰不破坏正确性，只是
/// 让极老的 command_id 重新变得"可提交"）。
#[derive(Default)]
struct MsgFetchCommandLedger {
    seen: std::collections::HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
}

/// Tool-name correlation table between ToolStarted and ToolCompleted.
///
/// `names` and `order` have the same key set at every helper boundary.
#[derive(Default)]
struct ToolCorrelationState {
    names: HashMap<(String, String), String>,
    order: VecDeque<(String, String)>,
}

/// P0-b：单 session 的"当前 run 归约态"——一次 `control.snapshot` 请求原子读取
/// `(run_id, last_seq, reducer.snapshot_blocks())` 三件套所需的全部状态。`last_seq` 是 sink
/// 看到的（coalesce 后的）最后一条事件 `seq`——天然等于"已交给远端下行管线的最后一条 live
/// 帧 seq"，正是 M0 §3 水印语义要求的 `through_run_seq`。
struct PartialSnapshotState {
    run_id: String,
    last_seq: u64,
    reducer: crate::display_reduce::DisplayReducer,
}

impl Default for GatewayInnerState {
    fn default() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            head_seq: AtomicU64::new(0),
            frames_seen: AtomicU64::new(0),
            frames_sent: AtomicU64::new(0),
            keepalive_pings_sent: AtomicU64::new(0),
            bad_frames: AtomicU64::new(0),
            upstream_state: AtomicU64::new(0),
            upstream_dropped: AtomicU64::new(0),
            upstream_stale_generation_dropped: AtomicU64::new(0),
            upstream_budget_dropped: AtomicU64::new(0),
            milestone_dropped: AtomicU64::new(0),
            session_index_snapshot_unavailable: AtomicU64::new(0),
            snapshot_requested_generation: AtomicU64::new(0),
            snapshot_wake_tx: OnceLock::new(),
            snapshot_worker_spawn_count: AtomicU64::new(0),
            tool_correlation_dropped: AtomicU64::new(0),
            classify_skipped: AtomicU64::new(0),
            connection_failures: AtomicU64::new(0),
            panics: AtomicU64::new(0),
            disconnect_config_stale: AtomicU64::new(0),
            disconnect_closed_by_peer: AtomicU64::new(0),
            disconnect_error: AtomicU64::new(0),
            last_disconnect_reason: Mutex::new(String::new()),
            status: Mutex::new(GatewayStatus {
                state: GatewayState::Disabled,
                last_error: None,
                stopped_reason: None,
                counters: GatewayCounters::default(),
            }),
            tool_correlation: Mutex::new(ToolCorrelationState::default()),
            active_repo_id_for_gating: Mutex::new(None),
            upstream_repo_filtered: AtomicU64::new(0),
            partial_snapshots: Mutex::new(HashMap::new()),
            partial_snapshot_capacity_dropped: AtomicU64::new(0),
            snapshot_oversized_dropped: AtomicU64::new(0),
            history_oversized_dropped: AtomicU64::new(0),
            replay_oversized_dropped: AtomicU64::new(0),
            run_status_replay_gate: Mutex::new(()),
            reply_queue: Mutex::new(VecDeque::new()),
            input_ack_outbox: Mutex::new(VecDeque::new()),
            reply_queue_dropped: AtomicU64::new(0),
            reply_stale_connection_dropped: AtomicU64::new(0),
            reply_repo_filtered_dropped: AtomicU64::new(0),
            reply_queue_stale_generation_purged: AtomicU64::new(0),
            msg_fetch_inflight: Mutex::new(HashMap::new()),
            msg_fetch_byte_budget: Mutex::new(VecDeque::new()),
            msg_fetch_generation_counter: AtomicU64::new(0),
            msg_fetch_command_ledger: Mutex::new(MsgFetchCommandLedger::default()),
            activity_summary_tx: OnceLock::new(),
            activity_summary_worker_spawn_count: AtomicU64::new(0),
            activity_summary_dropped: AtomicU64::new(0),
        }
    }
}

impl GatewayInnerState {
    fn upstream_enabled_snapshot(&self) -> bool {
        self.upstream_state.load(Ordering::Acquire) & 1 == 1
    }

    fn connection_generation_snapshot(&self) -> u64 {
        self.upstream_state.load(Ordering::Acquire) >> 1
    }

    fn disable_upstream_gate(&self) {
        let current = self.upstream_state.load(Ordering::Acquire);
        self.upstream_state
            .store(current & !1u64, Ordering::Release);
    }

    fn enable_upstream_gate(&self) {
        self.upstream_state.fetch_or(1, Ordering::Release);
    }

    fn advance_generation_and_set_gate(&self, enabled: bool) -> u64 {
        let previous_generation = self.upstream_state.load(Ordering::Acquire) >> 1;
        let next_generation = previous_generation + 1;
        let bit = u64::from(enabled);
        self.upstream_state
            .store((next_generation << 1) | bit, Ordering::Release);
        next_generation
    }
}

/// Resets the upstream gate even when a connection exits by unwinding through a panic.
struct UpstreamGateGuard<'a> {
    state: &'a AtomicU64,
}

impl<'a> UpstreamGateGuard<'a> {
    fn new(state: &'a AtomicU64) -> Self {
        Self { state }
    }
}

impl Drop for UpstreamGateGuard<'_> {
    fn drop(&mut self) {
        let current = self.state.load(Ordering::Acquire);
        self.state.store(current & !1u64, Ordering::Release);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GatewayConfig {
    pub relay_url: String,
    pub room_id: String,
    /// M2-4d：单活跃房间模型下，`GatewayConfig` 只在 active project 已设、resolver 解析成功
    /// 时才会被构造出来（`current_config` 的其余分支一律 `room_id_raw = None`，走既有「未配置」
    /// 分支，压根不会走到这里）——因此这个字段现在永远是 `Some`（=解析时的 `repos.id`），不再
    /// 存在"房间没有单一归属项目"的 legacy 中间态，`RoomSource` 枚举随之整个撤除（原先靠它
    /// 分流的 claim 冲突自愈路径与命令归属闸，现在分别收敛成"恒不换房"与"恒启用"）。命令
    /// 归属 fail-closed 判定读的就是这个字段被"带进"连接上下文后的值（`run_connection_request`
    /// 写进 `GatewayInnerState::active_repo_id_for_gating`），不重读 `remote_active_repo_id`
    /// 设置。
    pub active_repo_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimResponse {
    Claimed,
    Conflict,
    Tombstoned,
    RateLimited,
}

#[derive(PartialEq, Eq)]
pub(crate) struct SecretToken(String);

impl SecretToken {
    fn new(value: String) -> Self {
        Self(value)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "***")
    }
}

impl std::fmt::Display for SecretToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "***")
    }
}

struct DesktopCredential(Zeroizing<String>);

impl DesktopCredential {
    fn new(value: Zeroizing<String>) -> Self {
        Self(value)
    }

    fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Debug for DesktopCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("***")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GatewayState {
    Disabled,
    Waiting,
    Connecting,
    Connected,
    Backoff,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct GatewayCounters {
    pub frames_seen: u64,
    pub frames_sent: u64,
    pub keepalive_pings_sent: u64,
    pub bad_frames: u64,
    pub upstream_dropped: u64,
    pub upstream_stale_generation_dropped: u64,
    pub upstream_budget_dropped: u64,
    pub milestone_dropped: u64,
    pub session_index_snapshot_unavailable: u64,
    pub snapshot_worker_spawn_count: u64,
    pub tool_correlation_dropped: u64,
    pub classify_skipped: u64,
    pub connection_failures: u64,
    pub panics: u64,
    pub disconnect_config_stale: u64,
    pub disconnect_closed_by_peer: u64,
    pub disconnect_error: u64,
    pub last_disconnect_reason: String,
    pub upstream_repo_filtered: u64,
    pub partial_snapshot_capacity_dropped: u64,
    pub snapshot_oversized_dropped: u64,
    pub history_oversized_dropped: u64,
    pub replay_oversized_dropped: u64,
}

impl Default for GatewayCounters {
    fn default() -> Self {
        Self {
            frames_seen: 0,
            frames_sent: 0,
            keepalive_pings_sent: 0,
            bad_frames: 0,
            upstream_dropped: 0,
            upstream_stale_generation_dropped: 0,
            upstream_budget_dropped: 0,
            milestone_dropped: 0,
            session_index_snapshot_unavailable: 0,
            snapshot_worker_spawn_count: 0,
            tool_correlation_dropped: 0,
            classify_skipped: 0,
            connection_failures: 0,
            panics: 0,
            disconnect_config_stale: 0,
            disconnect_closed_by_peer: 0,
            disconnect_error: 0,
            last_disconnect_reason: String::new(),
            upstream_repo_filtered: 0,
            partial_snapshot_capacity_dropped: 0,
            snapshot_oversized_dropped: 0,
            history_oversized_dropped: 0,
            replay_oversized_dropped: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GatewayStatus {
    pub state: GatewayState,
    pub last_error: Option<String>,
    pub stopped_reason: Option<String>,
    pub counters: GatewayCounters,
}

pub(crate) fn status() -> GatewayStatus {
    GATEWAY
        .get()
        .map(|inner| gateway_status_from_state(&inner.state))
        .unwrap_or(GatewayStatus {
            state: GatewayState::Disabled,
            last_error: None,
            stopped_reason: None,
            counters: GatewayCounters::default(),
        })
}

fn gateway_status_from_state(state: &GatewayInnerState) -> GatewayStatus {
    let mut status = lock(&state.status).clone();
    status.counters = GatewayCounters {
        frames_seen: state.frames_seen.load(Ordering::Relaxed),
        frames_sent: state.frames_sent.load(Ordering::Relaxed),
        keepalive_pings_sent: state.keepalive_pings_sent.load(Ordering::Relaxed),
        bad_frames: state.bad_frames.load(Ordering::Relaxed),
        upstream_dropped: state.upstream_dropped.load(Ordering::Relaxed),
        upstream_stale_generation_dropped: state
            .upstream_stale_generation_dropped
            .load(Ordering::Relaxed),
        upstream_budget_dropped: state.upstream_budget_dropped.load(Ordering::Relaxed),
        milestone_dropped: state.milestone_dropped.load(Ordering::Relaxed),
        session_index_snapshot_unavailable: state
            .session_index_snapshot_unavailable
            .load(Ordering::Relaxed),
        snapshot_worker_spawn_count: state.snapshot_worker_spawn_count.load(Ordering::Relaxed),
        tool_correlation_dropped: state.tool_correlation_dropped.load(Ordering::Relaxed),
        classify_skipped: state.classify_skipped.load(Ordering::Relaxed),
        connection_failures: state.connection_failures.load(Ordering::Relaxed),
        panics: state.panics.load(Ordering::Relaxed),
        disconnect_config_stale: state.disconnect_config_stale.load(Ordering::Relaxed),
        disconnect_closed_by_peer: state.disconnect_closed_by_peer.load(Ordering::Relaxed),
        disconnect_error: state.disconnect_error.load(Ordering::Relaxed),
        last_disconnect_reason: lock(&state.last_disconnect_reason).clone(),
        upstream_repo_filtered: state.upstream_repo_filtered.load(Ordering::Relaxed),
        partial_snapshot_capacity_dropped: state
            .partial_snapshot_capacity_dropped
            .load(Ordering::Relaxed),
        snapshot_oversized_dropped: state.snapshot_oversized_dropped.load(Ordering::Relaxed),
        history_oversized_dropped: state.history_oversized_dropped.load(Ordering::Relaxed),
        replay_oversized_dropped: state.replay_oversized_dropped.load(Ordering::Relaxed),
    };
    status
}

pub(crate) fn shutdown() {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    inner.shutdown.store(true, Ordering::Release);
    set_status(&inner.state, GatewayState::Disabled, None);
    // No separate wake-up channel is needed: reads wake within 500ms and backoff sleeps poll.
}

fn request_gateway_reload() {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    inner.reload_requested.store(true, Ordering::Release);
}

/// A successful settings write is an explicit user request to leave any stopped state and
/// reevaluate the gateway configuration without restarting the app.
pub(crate) fn request_settings_reload() {
    request_gateway_reload();
}

/// Wake connection establishment so a newly queued registry frame is publishable even when the
/// gateway is stopped or sleeping in backoff. A live connection deliberately stays connected.
pub(crate) fn request_registry_publish() {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    inner.registry_publish_wake.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests;
