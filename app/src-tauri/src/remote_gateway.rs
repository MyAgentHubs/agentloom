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

/// 官方公共中继。relay 地址留空时的缺省值；用户填自建 wss:// 地址可覆盖。
pub(crate) const DEFAULT_PUBLIC_RELAY_URL: &str = "wss://agentloom.myagenthubs.com";

/// relay 地址留空（`None` 或 trim 后为空白）时兜底到官方公共中继；非空原样返回（不 trim——
/// 调用方各自已有/该有自己的 trim 纪律，这里只负责"空则兜底"这一件事，不越权改写用户已填的
/// 非空值）。
pub(crate) fn effective_relay_url(raw: Option<String>) -> Option<String> {
    match raw {
        Some(value) if !value.trim().is_empty() => Some(value),
        _ => Some(DEFAULT_PUBLIC_RELAY_URL.to_owned()),
    }
}

const SETTINGS_POLL_INTERVAL: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_millis(500);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);
// S1i1 返工四：`registry_resync_pending` 断连判据的硬截止——一次 rejected 的 refresh put
// 之后，只要每一轮 `socket.read()` 都能在 `READ_TIMEOUT` 内返回一个完整帧（含繁忙连接：
// Text 帧到达间隔始终短于 `READ_TIMEOUT`、读不超时但每轮都读到完整帧），就必须在这个时限
// 内无条件断开去做重连收敛。但这条上界只在「帧完整」的前提下成立——`READ_TIMEOUT` 约束的
// 是单次底层读，不是「一条完整消息到达」；relay 若持续投喂未拼完的分片消息，`socket.read()`
// 会一直卡在 tungstenite 内部不返回，这条硬截止判断根本不会被求值，收敛此时没有有限上界
// （非本判据独有，主循环后续的 shutdown/liveness/`drain_upstream` 检查同样被挡住）。见
// `run_connection_request` 内断开判据的大段注释。
const REGISTRY_RESYNC_DRAIN_DEADLINE: Duration = Duration::from_secs(2);
const BACKOFF_POLL_INTERVAL: Duration = Duration::from_millis(200);
const WS_MAX_REDIRECTS: u8 = 0;
const ROOM_CLAIM_CONFLICT_STOP_ERROR: &str =
    "房间归属被占；为保已配对设备不自动换房，远程控制已停机";
const ROOM_CLAIM_CONFLICT_STOP_REASON: &str = "room_claim_conflict";
/// M2-4d：per-project 房间撞 claim Conflict 且房内查无已知设备时的专属 Stop——跟
/// `ROOM_CLAIM_CONFLICT_STOP_REASON`（`Ok(true)` 分支，保护已配对设备）是姊妹码但触发条件
/// 不同：这条对应查无设备的 `Ok(false)` 分支。M2-4d 前这条分支还会先判 legacy/`Active` 两态、
/// legacy 走自动换房；单活跃房间模型下换房已撤（per-project 房换不动，换了下一轮 `current_
/// config` 还是解回同一个房），`Ok(false)` 现在直接 Stop，不进任何重试/换房循环。
const ROOM_CLAIM_CONFLICT_PROJECT_STOP_ERROR: &str =
    "房间归属被占；per-project 房间不支持自动换房，远程控制已停机——请到 Settings 重新配对该项目";
const ROOM_CLAIM_CONFLICT_PROJECT_STOP_REASON: &str = "room_claim_conflict_project";
const ROOM_TOMBSTONED_STOP_ERROR: &str = "房间已在服务端终结（410），远程控制已停机";
const ROOM_TOMBSTONED_STOP_REASON: &str = "room_tombstoned";
const ROOM_DEVICE_STATUS_UNAVAILABLE_STOP_REASON: &str = "room_device_status_unavailable";
const REGISTRY_REBASE_LIMIT_STOP_REASON: &str = "registry_rebase_limit";
const REGISTRY_REBASE_LIMIT_STOP_ERROR: &str = "relay 注册表高水位连续抬升，远程控制已停机";
const MAX_REGISTRY_REBASES: u8 = 3;
/// S1i3 F2：`relay_high_water` 允许超过本地计数器（`snapshot.revision`）的最大跨度。取
/// 1_000_000——注册表代号只会以 1 递增（每次配对/设备增删/refresh 轮换各领一个号），房间
/// 存活期内的正常用量远不可能在两次 sync 之间跳出百万级代号差；这个跨度既给连续
/// rebase（`MAX_REGISTRY_REBASES` 轮内 relay 诚实抬高水位）留足余量，又把一个半可信/故障
/// relay 能造成的最大污染钉死在一个远低于 `i64::MAX` 溢出边界的范围——绝不会让
/// `bump_registry_counter_to_in_transaction` 把 `next_generation` 抬到接近溢出。
const REGISTRY_HIGH_WATER_MAX_SPAN: i64 = 1_000_000;
const REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON: &str = "registry_high_water_out_of_range";
const REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_ERROR: &str =
    "relay 报回的注册表高水位远超本地计数器，远程控制已停机";
const MAX_REGISTRY_SYNC_ENTRIES: usize = 256;
const MAX_REGISTRY_FRAME_BYTES: usize = 64 * 1024;
const MAX_INCOMING_BYTES: usize = 1024 * 1024;
const DEFAULT_LIVENESS_INTERVAL: Duration = Duration::from_secs(5);
/// Protocol Ping cadence for defeating middlebox idle timeouts. Production links were observed
/// being cut after roughly 390 seconds of silence; 30 seconds leaves ample safety margin.
const KEEPALIVE_IDLE_INTERVAL: Duration = Duration::from_secs(30);
/// Preserve immediate reconnects for ordinary config changes, but stop transient resolver/DB
/// failures from creating an unbounded ConfigStale reconnect storm.
const CONFIG_STALE_GUARD_WINDOW: Duration = Duration::from_secs(30);
const CONFIG_STALE_GUARD_THRESHOLD: u32 = 3;
const CLOSE_DRAIN_ATTEMPTS: u8 = 8;
const UPSTREAM_CAPACITY: usize = 1024;
const TOOL_CORRELATION_CAPACITY: usize = 1024;
/// P0-b：`partial_snapshots` 表容量上限——正常并发 running 会话数远小于此，超限只拒收新
/// session 条目（防泄漏），不影响已跟踪 session 的持续更新。
const PARTIAL_SNAPSHOT_CAPACITY: usize = 128;
const OUTPUT_TRUNCATE_BYTES: usize = 2048;
/// P0-b 返工·v1.8.12 ③：`build_snapshot_payload` 收敛后整帧序列化的预算——留足信封膨胀
/// （base64/AEAD tag/JSON 转义）与 relay 64KB 硬闸之间的余量，超出即从最老块开始丢。
/// P0-b 微返工第 3 轮：此预算的计量对象是**含 `t` 字段的完整明文 payload**（`milestone_payload`
/// 随后合并进去的 `"t":"snapshot"` 也占预算），不是构造期的裸 payload——否则单个 32KiB text
/// 块这类边界情形，成品会在计量之后再被 `t` 字段撑破预算。
const SNAPSHOT_PAYLOAD_BUDGET_BYTES: usize = 32 * 1024;
/// P0-b 微返工第 3 轮：发送侧兜底阈值——按信封膨胀折算的**明文预算**，不是 relay 64KB 硬闸
/// 本身。经验折算：wire 帧 ≈ 1.37 × 明文 payload + 常数头（base64 展开 + AEAD tag + JSON
/// 结构/转义），在 relay `MAX_REGISTRY_FRAME_BYTES`（64KB）硬闸前留出余量；旧值 60KiB 是把
/// "加密前裸 payload" 直接拿去跟一个接近 relay 硬闸的数字比，量错对象（审查算例：51,395B
/// 明文 payload → 实测 wire 帧 ≈68,792B，已经撞线）。`SNAPSHOT_PAYLOAD_BUDGET_BYTES` 收敛
/// 正确后本兜底在正常路径几乎不可达——它只是最后一道保险丝，不是主收敛机制。
const SNAPSHOT_SEND_BUDGET_BYTES: usize = 44 * 1024;
/// `control.history` 响应与 snapshot 共用 relay 64KiB 硬闸前的明文安全余量：按 base64、
/// AEAD tag 与 JSON 信封膨胀折算，历史页宁可少装一条，也不把超帧交给 relay 断连。
const HISTORY_SEND_BUDGET_BYTES: usize = 44 * 1024;
const HISTORY_PAGE_MAX_ROWS: usize = 50;
/// 收敛丢块后插在 blocks 最前的截断提示——复用 `db::Block::Text` 既有形状，不加新字段。
const SNAPSHOT_TRUNCATED_NOTICE: &str = "（快照已截断，仅含最近内容）";
const MAX_DRAIN_ITEMS_PER_ROUND: usize = 64;
const DRAIN_ROUND_BUDGET: Duration = Duration::from_millis(250);
// M0 v1.7.2 拍板：JSON safe integer 上界（2^53 - 1）。
const JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
const COMMAND_ID_MAX_LEN: usize = 128;
/// P0-b 微返工第 4 轮：`session` 字段的桌面侧长度纵深守卫——M0 v1.8.8 wire grammar 早已约定
/// "`session` ≤128 字节"（relay 侧本就按此拒收），这里是同口径的本地防御，不依赖 relay 已经
/// 拦住。单位是 UTF-8 字节（对应 `str::len()`），不是字符数。命中即走既有 `failed()`（计
/// `bad_frames`），且必须放在归属闸（`command_session_allowed`）之前——不合规的 session 值
/// 没必要去查归属，两类失败语义也不同（"这不是一个合法的 session 标识" vs "合法但不属于当前
/// active repo"）。
const SESSION_ID_MAX_BYTES: usize = 128;
// M0 v1.7.2 拍板：control.stop 有效期硬上限，30 秒。
const CONTROL_STOP_MAX_LIFETIME_MS: u64 = 30_000;
// M0 v1.7.2 拍板：时钟偏差容忍窗口，2 分钟。
const CONTROL_STOP_SKEW_MS: u64 = 120_000;
const CLIENT_MSG_ID_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0xfe4e51ad_468c_4c11_85c2_f15f0c22f030);

pub(crate) type SettingsReader = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub(crate) type TokenProvider = Box<dyn Fn() -> Option<String> + Send + Sync>;
pub(crate) type DesktopCredentialProvider =
    Box<dyn Fn(&str) -> Result<Zeroizing<String>, String> + Send + Sync>;
pub(crate) type ClaimClient =
    Box<dyn Fn(&str, &str, &str) -> Result<ClaimResponse, String> + Send + Sync>;
pub(crate) type ActiveDeviceProvider = Box<dyn Fn(&str) -> Result<bool, String> + Send + Sync>;
/// M2-4b：project_id → 该 project 的 per-project 房间 id。ensure 语义（有房复用 / 无房新建 +
/// 凭据幂等先行，生产实现见 lib.rs 的 `remote_gateway_active_room_resolver`）。只应在「remote
/// 已启用 && active project 已设」时被 `current_config` 调用——门禁由调用方负责，这里只管
/// 解析单个 project_id。
pub(crate) type ActiveRoomResolver = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;
pub(crate) type KRoomProvider = Box<dyn Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync>;
/// Provider contract: execution happens on the dedicated `remote-index-snapshot` thread, leaving
/// the WebSocket read thread with zero DB work. The provider must still hold the DB lock only for
/// the short read, never touch the keychain, network, or child processes, and return `None` on
/// failure without panicking.
pub(crate) type SessionIndexSnapshotProvider =
    Box<dyn Fn() -> Option<serde_json::Value> + Send + Sync>;
/// Provider contract mirrors `SessionIndexSnapshotProvider`: runs on the `remote-index-snapshot`
/// background thread, short DB read only, returns `None` on failure without panicking.
pub(crate) type MilestoneReplayProvider =
    Box<dyn Fn() -> Option<Vec<crate::db::MilestoneReplayRow>> + Send + Sync>;
/// idlefix-T1 缺口②：连接后补发批用——短锁 DB 读取全部（未软删会话的）`session_runtime`
/// 现状行，供 `publish_milestone_replay_batch_on_connect` 把 `run.status` 现状帧一并塞进补发批
/// （不新增帧类型，复用 `publish_run_status_milestone` 同款帧构造）。Provider 契约同上：只在
/// `remote-index-snapshot` 后台线程跑，失败返回 `None`，不 panic。
pub(crate) type SessionRuntimeReplayProvider =
    Box<dyn Fn() -> Option<Vec<crate::db::SessionRuntimeReplayRow>> + Send + Sync>;
/// M2-4c：session → 归属 repo id 查询（`Ok(None)` = 会话不存在 / 无归属，两者对归属闸而言
/// 同一处理——都不属于任何 active repo）。`Err` = 查询本身失败（DB 错误），调用方一律
/// fail-closed 处理，不当"未知即放行"。生产实现见 lib.rs `remote_gateway_session_repo_provider`
/// （包一层 `db::get_session_repo_id`）；M24DR 返工修正过期表述：`RoomSource` 枚举已随 legacy
/// 全局房回落一并撤除，单活跃房间模型下归属闸恒启用——只要有连接（`active_repo_id_for_gating`
/// 恒 `Some`）就恒被调用，不再有"只在某种房间来源下才调用"的开关短路。
pub(crate) type SessionRepoProvider =
    Box<dyn Fn(&str) -> Result<Option<String>, String> + Send + Sync>;
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionHistoryRow {
    pub message_id: i64,
    pub role: String,
    pub content_json: Value,
}
/// `control.history` 的短锁 DB provider：结果保持 `message_id DESC`，组页层负责预算收敛与
/// wire 所需的升序反转。错误必须显式返回，让命令回 failed，不发送半页。
pub(crate) type SessionHistoryProvider =
    Box<dyn Fn(&str, Option<i64>, i64) -> Result<Vec<SessionHistoryRow>, String> + Send + Sync>;
pub(crate) type PairHelloHandler =
    Box<dyn Fn(PairHelloFrame) -> Option<PairAcceptFrame> + Send + Sync>;
pub(crate) type PairDoneHandler = Box<dyn Fn(PairDoneFrame) -> PairDoneAction + Send + Sync>;

/// S1i1 §9.6：relay 盖章后的 `token.refresh.forward`——`subject`/`request_generation` 都是 relay
/// 写的，不是手机自己声称的。
pub(crate) struct RefreshForwardFrame {
    pub request_id: String,
    pub subject: String,
    /// relay 自己的 `refresh_requests` 投递记账用途；桌面侧决策只认密文体解出来的
    /// refresh_token 命中哪个哈希，不拿这个字段做判定依据（relay 已经在它自己的投递谓词里
    /// 核过一次，桌面重复核对没有额外安全收益，只会在两边口径不一致时制造新的分裂点）。
    pub request_generation: i64,
    pub ct: String,
    pub n: String,
}

/// token.ack 到达后要回放的 refresh 回执——挂在对应的 outbox `token.put` 项上，ack 消费时
/// 一并吐出（§9.6 第 250 行「put → ack → 回执」固定顺序）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefreshOkFrame {
    pub request_id: String,
    pub subject: String,
    pub generation: i64,
    pub ct: String,
    pub n: String,
}

/// refresh handler 的即时结果：`Reply` 立即回帧（幂等重放的 ok / 各类 fail）；`Pending` 表示
/// 已经把 token.put 挂进 outbox，真正的 token.refresh.ok 要等 ack 到达才发。
pub(crate) enum RefreshOutcome {
    Reply(Value),
    Pending,
}

pub(crate) type RefreshHandler = Box<dyn Fn(RefreshForwardFrame) -> RefreshOutcome + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenSyncCurrent {
    pub token_hash: String,
    pub access_expires: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_until: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenSyncPrev {
    pub token_hash: String,
    pub generation: i64,
    pub prev_expires: i64,
}

/// §9.3 token.put 去掉 `t` 后的逐项 wire 形状。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenSyncEntry {
    pub subject: String,
    pub generation: i64,
    pub scope: String,
    pub current: TokenSyncCurrent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<TokenSyncPrev>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistrySnapshot {
    pub revision: i64,
    pub entries: Vec<TokenSyncEntry>,
}

pub(crate) type RegistrySnapshotProvider =
    Box<dyn Fn(&str, u64) -> Result<RegistrySnapshot, String> + Send + Sync>;
/// 第 5 个参数 = 仍待送达的 revoke（token.delete）subject 列表（`pending_revoke_subjects`）；
/// 返回三元组新增一项 = 那些 subject 在同一 DB 事务里新领到的代号（S1h §9.3 rebase 重发）。
pub(crate) type RegistryRebaseProvider = Box<
    dyn Fn(
            &str,
            i64,
            u64,
            bool,
            &[String],
        ) -> Result<(RegistrySnapshot, Option<i64>, Vec<(String, i64)>), String>
        + Send
        + Sync,
>;
/// 第 3 个参数 = 仍待送达（未 ack，含 rejected）的 revoke（token.delete）subject 列表
/// （`pending_revoke_subjects`）。S1h R1 返工：调用方必须在把 sync.ack 的 `relay_high_water`
/// 无条件吸收进桌面计数器（§9.4 计数器吸收）之后，同一实现里立刻给这些 subject 各领一个
/// 新代号并原样返回——新代号因此保证严格大于本次吸收后的计数器 floor，天然满足「delete 的
/// 代号必须严格大于本次 sync 的 revision，也必须大于 relay 报回的 relay_high_water」。
pub(crate) type RegistryHighWaterProvider =
    Box<dyn Fn(&str, i64, &[String]) -> Result<Vec<(String, i64)>, String> + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
struct RegistryOutboxItem {
    subject: String,
    generation: i64,
    frame: Value,
    /// S1h 返工二 F3：仅统计/诊断用（`drain_registry_outbox` 每次实发前自增）——真正的发送
    /// 闸门是 `last_sent_at`（已发未回 ack 前不重发）与 `rejected`（除 revoke 外一律停发）。
    /// 这个字段唯一的读者是测试断言，别把它误读成还在挡什么发送逻辑。
    attempts: u32,
    last_sent_at: Option<u64>,
    acked: bool,
    rejected: bool,
    pair_ready: Option<PairReadyFrame>,
    /// S1i1 §9.6 第 250 行：轮换成功后挂在这次 `token.put` 上的 refresh 回执——ack 到达前
    /// 一律不发（put→ack→回执固定顺序），`consume_token_ack` 消费时吐给调用方。跟
    /// `pair_ready` 同款挂法，两者不会同时出现在同一项上（一个 subject 的同一次 put 要么是
    /// 配对授予要么是 refresh 轮换）。
    refresh_ok: Option<RefreshOkFrame>,
}

/// S1h R5 返工：`RegistryState::outbox_snapshot_for_test` 的返回元素——测试专用只读快照，
/// 字段特意逐个克隆（而不是把 `RegistryOutboxItem` 整体放宽成 `pub(crate)`），避免外部代码
/// 拿着它去反向摸/改 outbox 内部状态。
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutboxItemSnapshot {
    pub(crate) frame: Value,
    pub(crate) generation: i64,
    pub(crate) attempts: u32,
    pub(crate) acked: bool,
    pub(crate) rejected: bool,
}

/// S1f2 registry 串行域。锁序是 `registry -> db -> pairing slot`：所有 grant/rotate/revoke、
/// 配对 begin/cancel/done、快照/rebase 路径都必须先拿这把 `Arc<Mutex<_>>`，严禁从 DB 或
/// pairing slot 反向获取 registry；持锁后才可改 DB、slot 或 outbox。
#[derive(Default)]
pub(crate) struct RegistryState {
    pairing_entry: Option<TokenSyncEntry>,
    outbox: VecDeque<RegistryOutboxItem>,
    acknowledged_pair_ready: HashMap<String, PairReadyFrame>,
    staged_pairing_k_room: Option<Zeroizing<[u8; 32]>>,
    /// S1i1 §9.6 第 252 行（配额）：按 subject 记成功轮换时间戳（滑动 1h 窗）与连续无效计数——
    /// 任务书 §2d 明确"计数状态存哪自行决定，内存 registry 域即可"。**进程重启即归零**：这是
    /// 有意的权衡，不为一个节流计数器新开 DB 迁移；后果是重启后的第一小时窗口形同重新计数，
    /// 短暂放宽而不是收紧（不是安全边界，是防滥用节流），可接受。
    refresh_quota: HashMap<String, RefreshQuotaEntry>,
    /// S1i1 返工二 F1：put 被 relay 判 `rejected` 且挂着 refresh 回执时（`consume_token_ack`
    /// 的 `RefreshDropped` 分支），桌面 DB 已经不可逆地轮换到新代号，relay 那边还停在旧代号
    /// ——回一帧 `token.refresh.fail{reason:"put_rejected"}` 只让手机知道要重试，并不能推动
    /// relay 的当前代号，手机凭旧 refresh 的同 request_id 重试只会在 relay §9.6 第 246 行的
    /// 投递谓词上继续落空。这个 flag 是主动收敛意图的触发闸门：`consume_token_ack` 命中
    /// `RefreshDropped` 时置位，连接主循环处理完当前帧后 `take` 出来就地清零——每个 rejected
    /// 事件至多触发一次收敛意图。**S1i1 返工三+返工四**：真正的收敛动作不是在这条存活连接上
    /// 原地重发 `token.sync`，而是主动断开、交给既有重连路径的首次 sync 去做（断开判据 =
    /// 这一轮读超时，或挂起已超过 2 秒硬截止，见 `run_connection_request`），因此不会热循环
    /// （`synchronize_registry` 自身的 rebase 轮次另有 `MAX_REGISTRY_REBASES` 兜底）。
    resync_required: bool,
}

#[derive(Default)]
struct RefreshQuotaEntry {
    successful_rotations_ms: VecDeque<u64>,
    consecutive_invalid: u32,
}

/// S1i1 §2d：成功轮换 6/h/subject。
const REFRESH_QUOTA_MAX_PER_HOUR: usize = 6;
const REFRESH_QUOTA_WINDOW_MS: u64 = 3_600_000;
/// S1i1 §2d：连续 ≥3 次无效 → close:true。
const REFRESH_INVALID_CLOSE_THRESHOLD: u32 = 3;

impl RegistryState {
    pub(crate) fn set_pairing_entry(&mut self, entry: TokenSyncEntry) {
        self.staged_pairing_k_room = None;
        self.pairing_entry = Some(entry);
        // S1h 返工二 F1：`pairing` 是唯一会被复用的 subject（每轮配对换新 uuid，但注册表
        // subject 名恒为 "pairing"）。上一轮配对被取消时排的 token.delete 若还没发出去，
        // 不能继续堵在 outbox 里——`enqueue_outbox` 的豁免规则（同 subject 已排 delete →
        // 后续 put 一律作废）会让这一轮的新 put 永远进不了 outbox，陈旧 delete 最终反而会
        // 在重连 absorb 时被重新领到一个高于本次 sync revision 的代号发出去，把刚起步的
        // 新一轮配对当场撤销。relay 侧已确认存在入站再授权闸（room-do.js
        // authorizeInboundSocket/isOfficialAttachmentLive，每条入站消息都核对 subject 当前
        // generation，旧连接一旦代号不符即被 close code=token_reauthorization_failed）：
        // 新 put 落 relay 时会整体重建该 subject 的 token_aliases（room-store.js:429
        // `DELETE FROM token_aliases WHERE subject = ?`），旧 pairing 令牌自然失效，旧 socket
        // 靠这道闸失权，不需要再靠这条陈旧 delete 去主动断它——作废它是安全的。
        self.outbox
            .retain(|item| item.subject != "pairing" || !is_delete_frame(&item.frame));
    }

    pub(crate) fn clear_pairing_entry(&mut self) {
        self.pairing_entry = None;
        self.outbox.retain(|item| {
            item.subject != "pairing"
                || item.frame.get("t").and_then(Value::as_str) != Some("token.put")
        });
    }

    pub(crate) fn discard_staged_pairing_k_room(&mut self) {
        self.staged_pairing_k_room = None;
    }

    fn active_pairing_entry(&self, now_ms: u64) -> Option<TokenSyncEntry> {
        self.pairing_entry
            .as_ref()
            .filter(|entry| {
                u64::try_from(entry.current.access_expires)
                    .is_ok_and(|expires_ms| now_ms < expires_ms)
            })
            .cloned()
    }

    fn enqueue_outbox(&mut self, item: RegistryOutboxItem) {
        // §9.3 revoke 独立重试豁免：一旦某 subject 已经排了 token.delete（撤销意图），后续
        // 同 subject 的 token.put 一律作废——既不覆盖/取消已排队的撤销，也不让设备靠新 put
        // 复活。revoke 本身（item 自己就是 delete）不受此豁免约束，仍走下面的常规抬代逻辑，
        // 因此“revoke 入队取消同 subject 未发 put”天然成立（revoke 的代号更高）。
        if !is_delete_frame(&item.frame)
            && self
                .outbox
                .iter()
                .any(|queued| queued.subject == item.subject && is_delete_frame(&queued.frame))
        {
            return;
        }
        if self
            .outbox
            .iter()
            .any(|queued| queued.subject == item.subject && queued.generation >= item.generation)
        {
            return;
        }
        self.outbox
            .retain(|queued| queued.subject != item.subject || queued.generation > item.generation);
        self.acknowledged_pair_ready.remove(&item.subject);
        self.outbox.push_back(item);
    }

    pub(crate) fn enqueue_token_put(
        &mut self,
        entry: TokenSyncEntry,
        pair_ready: Option<PairReadyFrame>,
    ) {
        self.enqueue_outbox(RegistryOutboxItem {
            subject: entry.subject.clone(),
            generation: entry.generation,
            frame: token_put_frame(&entry),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready,
            refresh_ok: None,
        });
    }

    /// S1i1 §2a/§2b：轮换成功后的 `token.put`——挂上 `refresh_ok`，ack 到达才发回执
    /// （§9.6 第 250 行固定顺序）。
    pub(crate) fn enqueue_token_put_for_refresh(
        &mut self,
        entry: TokenSyncEntry,
        refresh_ok: RefreshOkFrame,
    ) {
        self.enqueue_outbox(RegistryOutboxItem {
            subject: entry.subject.clone(),
            generation: entry.generation,
            frame: token_put_frame(&entry),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: Some(refresh_ok),
        });
    }

    pub(crate) fn enqueue_token_delete(&mut self, subject: String, generation: i64, close: bool) {
        self.enqueue_outbox(RegistryOutboxItem {
            subject: subject.clone(),
            generation,
            frame: token_delete_frame(&subject, generation, close),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: None,
        });
    }

    /// 测试专用只读视图：outbox 里各项的关键维度快照（frame + generation/attempts/acked/
    /// rejected），按队列顺序。**不要**把 `outbox` 字段本身放宽成 `pub(crate)`——那会让外部
    /// 代码绕开 `enqueue_outbox`/`consume_token_ack` 等方法直接摸内部队列，破坏 registry 串行
    /// 域靠私有字段守住的不变量；这里只开一个窄口径的克隆访问器供 `lib.rs` 侧的集成测试断言
    /// 用。S1h R5 返工：原先（`outbox_frames_for_test`）只暴露 frame，断言看不到
    /// generation/attempts/acked/rejected，这里补全并改名。
    #[cfg(test)]
    pub(crate) fn outbox_snapshot_for_test(&self) -> Vec<OutboxItemSnapshot> {
        self.outbox
            .iter()
            .map(|item| OutboxItemSnapshot {
                frame: item.frame.clone(),
                generation: item.generation,
                attempts: item.attempts,
                acked: item.acked,
                rejected: item.rejected,
            })
            .collect()
    }

    /// S1h R1/R2 返工：仍待送达（未 ack，含 rejected）的 revoke（token.delete）项的 subject
    /// 列表——rebase/reconnect 时用来跟 DB 领新代号，重构 delete 帧后照发。按 §9.3「revoke 类
    /// 独立重试直到 ack」，rejected 的 revoke 项仍然算「待送达」（跟 put 的「rejected 停发」
    /// 惯例不同）：不把它排除在外，否则被 relay 拒绝一次的撤销意图就再也没有机会重新领号重试。
    fn pending_revoke_subjects(&self) -> Vec<String> {
        self.outbox
            .iter()
            .filter(|item| !item.acked && is_delete_frame(&item.frame))
            .map(|item| item.subject.clone())
            .collect()
    }

    fn prepare_outbox_for_reconnect(&mut self) {
        for item in &mut self.outbox {
            if !item.acked && !item.rejected {
                item.last_sent_at = None;
            }
        }
    }

    /// S1i1 返工二 F1：连接主循环处理完当前帧后调用，取出并清零收敛闸门。`take` 语义
    /// （读且清零）是闸门本体：同一次 `RefreshDropped` 至多兑现一次收敛意图，不会在没有新
    /// rejected 事件时被重复触发。**S1i1 返工三+返工四**：`true` 时主循环不在这条连接上原地
    /// 重发 `token.sync`，而是记一个「该断了」的意图（`registry_resync_pending`），等断开判据
    /// 成立（这一轮读超时，或挂起已超过 2 秒硬截止）才主动断开，让下一次连接的既有重连路径
    /// 去做那次 sync。
    fn take_resync_required(&mut self) -> bool {
        std::mem::take(&mut self.resync_required)
    }

    pub(crate) fn consume_token_ack(
        &mut self,
        subject: &str,
        generation: i64,
        result: &str,
    ) -> TokenAckAction {
        let Some(index) = self
            .outbox
            .iter()
            .position(|item| item.subject == subject && item.generation == generation)
        else {
            return TokenAckAction::Ignored;
        };
        if result == "rejected" {
            self.outbox[index].rejected = true;
            // S1i1 R2 返工：put 被 relay 拒绝时，桌面 DB/TokenBook 已经轮换成功——如果这一项
            // 挂着 refresh 回执，不能像其它 rejected 项一样被静默吞掉（此前只置位、永远停发、
            // 再没有人通知手机）。取出回执交给 handle_frame 立即回一帧
            // token.refresh.fail{reason:"put_rejected"}，让手机凭旧 refresh 立刻重试，而不是
            // 干等 relay 侧从未成立过的 ok/fail 超时。item 本身仍然标 rejected——drain 门禁与
            // rebase 的 retain 清理惯例不变。
            //
            // S1i1 返工二 F1：只回 fail 帧换了个死法——手机的重试永远推不动 relay 手上仍是
            // 旧代号的注册表（relay 拒绝的正是这次轮换本身）。这里额外置位收敛闸门，让连接
            // 主循环紧接着主动重发一次 `token.sync`，把 DB 真相（新代号）交给 relay。
            if let Some(refresh_ok) = self.outbox[index].refresh_ok.take() {
                self.resync_required = true;
                return TokenAckAction::RefreshDropped {
                    request_id: refresh_ok.request_id,
                    subject: refresh_ok.subject,
                };
            }
            return TokenAckAction::Rejected;
        }
        let mut item = self
            .outbox
            .remove(index)
            .expect("outbox index was found immediately before removal");
        item.acked = true;
        if let Some(ready) = item.pair_ready {
            self.acknowledged_pair_ready
                .insert(subject.to_owned(), ready.clone());
            return TokenAckAction::PairReady(ready);
        }
        match item.refresh_ok {
            Some(refresh_ok) => TokenAckAction::RefreshOk(refresh_ok),
            None => TokenAckAction::Consumed,
        }
    }

    pub(crate) fn acknowledged_pair_ready(&self, subject: &str) -> Option<PairReadyFrame> {
        self.acknowledged_pair_ready.get(subject).cloned()
    }

    /// A valid pair.done replay is the explicit retry trigger for a device grant whose put has
    /// timed out or was rejected. Preserve the exact grant/ready payload and retry accounting;
    /// only reopen the send barrier for the next live-socket drain.
    pub(crate) fn replay_pair_ready(&mut self, subject: &str) -> Option<PairReadyFrame> {
        if let Some(ready) = self.acknowledged_pair_ready(subject) {
            return Some(ready);
        }
        if let Some(item) = self
            .outbox
            .iter_mut()
            .find(|item| item.subject == subject && item.pair_ready.is_some())
        {
            item.acked = false;
            item.rejected = false;
            item.last_sent_at = None;
        }
        None
    }

    fn stage_pairing_k_room(&mut self, k_room: Zeroizing<[u8; 32]>) {
        self.staged_pairing_k_room = Some(k_room);
    }

    fn pairing_k_room_is_staged(&self) -> bool {
        self.staged_pairing_k_room.is_some()
    }

    fn take_staged_pairing_k_room(&mut self) -> Option<Zeroizing<[u8; 32]>> {
        self.staged_pairing_k_room.take()
    }

    /// `revoke_generations` = (subject, 新代号) 对，仅覆盖 outbox 里仍待送达的 revoke 项
    /// （`pending_revoke_subjects` 的产物）。revoke 项不落在 `entries`（DB 快照本就省略已撤销
    /// 设备），所以走独立的第二遍匹配；`close` 恒为 true（撤销意图不因 rebase 变弱）。
    ///
    /// S1h R3 返工：put 被拒仍然停发丢弃（`retain` 照旧把 rejected 的 put 清掉），但 revoke
    /// （token.delete）被拒不能被这条 retain 连坐丢弃——按 §9.3「revoke 类独立重试直到 ack」，
    /// 它要留在 outbox 里，交给下面 `rearm_revoke_entries` 用新代号重新武装、清掉 rejected。
    fn rebase_outbox_entries(
        &mut self,
        entries: &[TokenSyncEntry],
        revoke_generations: &[(String, i64)],
    ) {
        self.outbox
            .retain(|item| !item.rejected || is_delete_frame(&item.frame));
        for entry in entries {
            let Some(item) = self.outbox.iter_mut().find(|item| {
                item.subject == entry.subject
                    && item.generation < entry.generation
                    && !is_delete_frame(&item.frame)
            }) else {
                continue;
            };
            item.generation = entry.generation;
            item.frame = token_put_frame(entry);
            item.last_sent_at = None;
            item.acked = false;
            self.acknowledged_pair_ready.remove(&entry.subject);
            // S1i1 R1 返工：rebase 只改代号/frame 不够——挂在这一项上的 refresh 回执
            // （`refresh_ok`，见 `enqueue_token_put_for_refresh`）此前一直冻在轮换那一刻的旧代号。
            // relay 侧 §9.6 第 246 行的投递谓词是「回执.generation == subject 当前 generation」，
            // 旧代号一旦不等于 rebase 后的新代号就会被丢弃——手机凭旧 refresh 重试又只拿到同一份
            // 陈旧回执，在 48h journal 窗内死循环。AAD 五元组不含 generation，改这个字段不影响
            // 密文体认证（`ct`/`n` 原样保留），跟 put 帧本身换代号是同一件事的两个字段。
            if let Some(ok) = item.refresh_ok.as_mut() {
                ok.generation = entry.generation;
            }
        }
        self.rearm_revoke_entries(revoke_generations);
    }

    /// `revoke_generations` = [(subject, 新代号)]，套用到 outbox 里对应的 token.delete 项：
    /// 换代号、重建 frame、重新武装为待发（`last_sent_at = None`、`acked = false`）。S1h
    /// R2/R3 返工：一并把 `rejected` 清掉——被 relay 拒绝过一次的撤销要以新代号重新出发，
    /// 不是永久停发，也不是被下一轮 rebase 的清理规则连坐丢弃。找不到匹配项（已经被 ack 掉、
    /// 或从未排过队）时静默跳过，不是错误。
    fn rearm_revoke_entries(&mut self, revoke_generations: &[(String, i64)]) {
        for (subject, generation) in revoke_generations {
            let Some(item) = self
                .outbox
                .iter_mut()
                .find(|item| &item.subject == subject && is_delete_frame(&item.frame))
            else {
                continue;
            };
            item.generation = *generation;
            item.frame = token_delete_frame(subject, *generation, true);
            item.last_sent_at = None;
            item.acked = false;
            item.rejected = false;
        }
    }

    fn cancel_outbox_before_reset(&mut self) {
        self.outbox.clear();
    }

    /// S1i1 §2d：成功轮换 6/h/subject——滑动窗口，探测本身不烧配额（只读，`prune` 在
    /// `record_refresh_rotation_success` 真正记账时才发生），供调用方在真正调用
    /// `store::refresh_device_tokens` 之前先判断要不要放行。
    pub(crate) fn refresh_quota_exceeded(&self, subject: &str, now_ms: u64) -> bool {
        self.refresh_quota.get(subject).is_some_and(|entry| {
            refresh_quota_window_count(entry, now_ms) >= REFRESH_QUOTA_MAX_PER_HOUR
        })
    }

    /// 记一次成功轮换：滑动窗口去掉 1h 前的旧时间戳、追加本次、连续无效计数清零
    /// （轮换成功即证明这不是一次无效/攻击性的尝试）。
    pub(crate) fn record_refresh_rotation_success(&mut self, subject: &str, now_ms: u64) {
        let entry = self.refresh_quota.entry(subject.to_owned()).or_default();
        entry
            .successful_rotations_ms
            .retain(|&sent_at| now_ms.saturating_sub(sent_at) < REFRESH_QUOTA_WINDOW_MS);
        entry.successful_rotations_ms.push_back(now_ms);
        entry.consecutive_invalid = 0;
    }

    /// 幂等重放（命中 prev 且 request_id 与 journal 一致）不烧配额，但同样证明这不是无效请求，
    /// 连续无效计数清零。subject 从未记过账时是 no-op（没什么好清零的）。
    pub(crate) fn record_refresh_replay(&mut self, subject: &str) {
        if let Some(entry) = self.refresh_quota.get_mut(subject) {
            entry.consecutive_invalid = 0;
        }
    }

    /// 记一次无效请求（解密失败/哈希既不命中当前也不命中未过期 prev/设备未知或已吊销）。
    /// 返回 true = 连续无效已达 `REFRESH_INVALID_CLOSE_THRESHOLD`，回执须带 `close:true`。
    /// in_flight（良性单飞行冲突）与配额超限**不**走这条路径——两者都不是"无效"，见调用方。
    pub(crate) fn record_refresh_invalid(&mut self, subject: &str) -> bool {
        let entry = self.refresh_quota.entry(subject.to_owned()).or_default();
        entry.consecutive_invalid = entry.consecutive_invalid.saturating_add(1);
        entry.consecutive_invalid >= REFRESH_INVALID_CLOSE_THRESHOLD
    }

    /// 测试专用：`refresh_quota` map 里现存的 subject 条目数——S1i1 R5-3 返工验证「查不到的
    /// subject 不建条目」用（真正的内存 map 本身不给生产代码开只读长度访问器，避免调用方拿它
    /// 做生产决策）。
    #[cfg(test)]
    pub(crate) fn refresh_quota_entry_count_for_test(&self) -> usize {
        self.refresh_quota.len()
    }
}

fn refresh_quota_window_count(entry: &RefreshQuotaEntry, now_ms: u64) -> usize {
    entry
        .successful_rotations_ms
        .iter()
        .filter(|&&sent_at| now_ms.saturating_sub(sent_at) < REFRESH_QUOTA_WINDOW_MS)
        .count()
}

fn token_put_frame(entry: &TokenSyncEntry) -> Value {
    let mut frame = serde_json::to_value(entry)
        .expect("TokenSyncEntry serialization contains no fallible custom serializer");
    frame
        .as_object_mut()
        .expect("TokenSyncEntry serializes as an object")
        .insert("t".to_owned(), Value::String("token.put".to_owned()));
    frame
}

/// §9.3 token.delete 逐项 wire 形状（对齐 fixtures/wire-v1.json `token_delete_close_valid`）。
fn token_delete_frame(subject: &str, generation: i64, close: bool) -> Value {
    serde_json::json!({
        "t": "token.delete",
        "subject": subject,
        "generation": generation,
        "close": close,
    })
}

/// S1h：区分 outbox 项是不是撤销意图（token.delete），用于豁免/rebase 分流。
fn is_delete_frame(frame: &Value) -> bool {
    frame.get("t").and_then(Value::as_str) == Some("token.delete")
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TokenAckAction {
    Ignored,
    Consumed,
    Rejected,
    PairReady(PairReadyFrame),
    /// S1i1 §2b：轮换的 `token.put` 收到 ack——回执现在才能发。
    RefreshOk(RefreshOkFrame),
    /// S1i1 R2 返工：轮换的 `token.put` 被 relay 拒绝，且原本挂着待发的 refresh 回执——
    /// `handle_frame` 据此立即回一帧 `token.refresh.fail{reason:"put_rejected"}`（不带
    /// close，不计入连续无效计数），而不是让回执永远烂在 outbox 里。S1i1 返工二 F1：这个分支
    /// 同时置位 `RegistryState::resync_required`——**S1i1 返工三+返工四**：连接主循环不在这条
    /// 连接上原地重发 `token.sync`，而是记一个「该断了」的意图，等断开判据成立（这一轮读
    /// 超时，或挂起已超过 2 秒硬截止）才主动断开，由下一次连接的既有重连路径把 DB 真相
    /// （新代号）交给 relay，否则手机的重试推不动 relay 的当前代号。
    RefreshDropped {
        request_id: String,
        subject: String,
    },
}

pub(crate) struct InputSendFrame {
    pub session: String,
    pub command_id: String,
    pub text: String,
}

pub(crate) struct InputAnswerFrame {
    pub session: String,
    pub command_id: String,
    pub decision_id: String,
    pub option: String,
}

pub(crate) struct ControlStopFrame {
    pub session: String,
    pub command_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AckOutcome {
    Ok,
    Queued,
    Failed,
}

pub(crate) type InputSendHandler = Box<dyn Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync>;
pub(crate) type InputAnswerHandler =
    Box<dyn Fn(InputAnswerFrame) -> Option<AckOutcome> + Send + Sync>;
pub(crate) type ControlStopHandler = Box<dyn Fn(ControlStopFrame) -> AckOutcome + Send + Sync>;
pub(crate) type ControlReplayHandler = Box<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// T5d-a provisional wire names. The relay/mobile implementations do not pin these fields yet;
/// keep them aligned with the existing `ct`/`n` convention until the protocol is finalized.
pub(crate) struct PairHelloFrame {
    pub room: String,
    pub remote_pub: [u8; 32],
    pub token_ct: String,
    pub token_n: String,
    pub origin_connection_id: String,
}

pub(crate) struct PairAcceptFrame {
    pub room: String,
    pub device_id: String,
    pub k_room_ct: String,
    pub k_room_n: String,
    pub tokens_ct: String,
    pub tokens_n: String,
    pub k_room: Zeroizing<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PairReadyFrame {
    pub room: String,
    pub device_id: String,
    pub ct: String,
    pub n: String,
}

#[derive(Debug)]
pub(crate) enum PairDoneAction {
    Rejected,
    Accepted {
        newly_paired_device_id: Option<String>,
    },
    Ready(PairReadyFrame),
}

#[derive(Clone, Default)]
pub(crate) struct PairDoneFrame {
    pub room: String,
    pub device_id: String,
    pub confirm_ct: Option<String>,
    pub confirm_n: Option<String>,
    pub origin_connection_id: String,
}

static GATEWAY: OnceLock<Arc<Inner>> = OnceLock::new();

/// M2-4c(B3)：`sessions.repo_id` 并不是真正不可变的——`update_session_repo`（lib.rs）是一条
/// 已注册运行时改绑 IPC（当前前端生产代码没有调用点，但保留给未来"挪会话到别的项目"功能，
/// 且它是普通内核函数，测试 / 未来功能都能直接触发）。这个进程级代号在每次改绑成功后 +1；
/// `upstream_session_allowed` 把它跟这条连接记录的基线比对，一旦不一致就整表清空
/// `session_repo_cache` 重建，防止缓存里的旧归属在同一条远端连接的生命周期内继续被当真。
static SESSION_REPO_EPOCH: AtomicU64 = AtomicU64::new(0);

/// M2-4c(B3)：`update_session_repo` 改绑成功后调用，语义见 `SESSION_REPO_EPOCH` 文档。
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
    /// 工具 output 截断后，单条 history 消息仍超过发送预算而被跳过的条数。
    history_oversized_dropped: AtomicU64,
    /// `msg.completed` 工具 output 截断后，完整明文帧仍超过发送预算而被跳过的条数。
    /// 连接回放与 live 发布共用同一入队闸和同一计数。
    replay_oversized_dropped: AtomicU64,
    /// idlefix-T1 补针 C（TOCTOU）：`publish_run_status_replay_rows`（连接后补发批读 DB + 逐行
    /// 入队 `run.status` 现状帧）与 `enqueue_run_status_milestone_with_gate`（真实运行时
    /// `publish_run_status_milestone` 的实时入队路径）共享这把锁——保证"该连接的 run.status
    /// 现状补发帧必须先于其后任何实时 run.status 帧入队"这一顺序不变量：补发批持锁跨越整个
    /// "读 DB + 入队"过程，期间任何真实状态翻转要么在读之前已落库（读到的就是新值，天然一致），
    /// 要么必须等补发批放锁后才能把新状态入队（必然排在补发帧之后，不会被陈旧帧倒灌覆盖）。
    run_status_replay_gate: Mutex<()>,
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

pub(crate) fn setup(
    settings: SettingsReader,
    token_provider: TokenProvider,
    desktop_credential_provider: DesktopCredentialProvider,
    claim_client: ClaimClient,
    active_device_provider: ActiveDeviceProvider,
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
    session_repo_provider: SessionRepoProvider,
    session_history_provider: SessionHistoryProvider,
) {
    if GATEWAY.get().is_some() {
        return;
    }

    let (upstream_tx, upstream_rx) = mpsc::sync_channel::<(u64, LiveQueueItem)>(UPSTREAM_CAPACITY);
    let (milestone_tx, milestone_rx) =
        mpsc::sync_channel::<(u64, MilestoneItem)>(UPSTREAM_CAPACITY);
    let inner = Arc::new(Inner {
        settings,
        token_provider,
        desktop_credential_provider,
        claim_client,
        active_device_provider,
        active_room_resolver,
        k_room_provider,
        session_index_snapshot_provider,
        milestone_replay_provider,
        session_runtime_replay_provider,
        pair_hello_handler,
        pair_done_handler,
        registry,
        registry_snapshot_provider,
        registry_rebase_provider,
        registry_high_water_provider,
        refresh_handler,
        input_send_handler,
        input_answer_handler,
        control_replay_handler,
        control_stop_handler,
        session_repo_provider,
        session_history_provider,
        upstream_tx,
        milestone_tx,
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    });
    if GATEWAY.set(Arc::clone(&inner)).is_err() {
        return;
    }
    install_panic_hook();

    let weak_inner = Arc::downgrade(&inner);
    thread::Builder::new()
        .name("remote-gateway".into())
        .spawn(move || connect_loop(weak_inner, upstream_rx, milestone_rx))
        .expect("failed to start remote gateway thread");
}

pub(crate) fn parse_config(
    enabled_raw: Option<&str>,
    relay_url_raw: Option<&str>,
    room_id_raw: Option<&str>,
    active_repo_id: Option<String>,
) -> (bool, Option<GatewayConfig>) {
    let enabled = enabled_raw == Some("true");
    let normalized_room_id = room_id_raw.map(str::to_lowercase);
    let config = match (relay_url_raw, normalized_room_id) {
        (Some(relay_url), Some(room_id)) if !relay_url.is_empty() && is_valid_room_id(&room_id) => {
            Some(GatewayConfig {
                relay_url: relay_url.to_owned(),
                room_id,
                active_repo_id,
            })
        }
        _ => None,
    };
    (enabled, config)
}

fn is_valid_room_id(room_id: &str) -> bool {
    room_id.len() == 32
        && room_id
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, 'a'..='f'))
}

// S1ja §9.7 后门退役: desktop identifies itself exclusively via the `Authorization: Bearer`
// header (S1b) and never follows redirects, so the legacy `?role=desktop`/`&token=` query
// params are pure dead weight now — worse, `&token=` used to carry the plaintext dev token
// straight into CF edge logs (P2-1). The relay no longer honors either param either
// (room-do.js §9.1 fetch() dropped the legacy admission path in this same batch).
pub(crate) fn build_ws_url(base: &str, room: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/room/{room}")
}

fn build_ws_request(
    url: &str,
    credential: &DesktopCredential,
) -> Result<tungstenite::handshake::client::Request, String> {
    let mut request = url
        .into_client_request()
        .map_err(|error| format!("invalid relay URL: {error}"))?;
    let authorization =
        HeaderValue::from_str(&format!("Bearer {}", credential.expose())).map_err(|_| {
            "desktop credential cannot be encoded as an Authorization header".to_owned()
        })?;
    request.headers_mut().insert(AUTHORIZATION, authorization);
    Ok(request)
}

pub(crate) fn claim_room_blocking(
    relay_url: &str,
    room_id: &str,
    credential_hash: &str,
) -> Result<ClaimResponse, String> {
    let http_base = relay_url
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            relay_url
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .ok_or_else(|| "relay URL must use ws:// or wss://".to_owned())?;
    let url = format!("{}/room/{room_id}/claim", http_base.trim_end_matches('/'));
    let body = serde_json::json!({"v": 1, "credential_hash": credential_hash}).to_string();
    if body.len() > 1024 {
        return Err("room claim request body exceeds 1KB".to_owned());
    }
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("claim client setup failed: {error}"))?;
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .map_err(|error| format!("claim request failed: {error}"))?;
    match response.status().as_u16() {
        200 => Ok(ClaimResponse::Claimed),
        409 => Ok(ClaimResponse::Conflict),
        410 => Ok(ClaimResponse::Tombstoned),
        429 => Ok(ClaimResponse::RateLimited),
        status @ 300..=399 => Err(format!("claim redirect refused (HTTP {status})")),
        status => Err(format!("claim returned unexpected HTTP {status}")),
    }
}

fn redact(input: &str, token: Option<&str>) -> String {
    let mut redacted = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(offset) = input[cursor..].find("token=") {
        let value_start = cursor + offset + "token=".len();
        redacted.push_str(&input[cursor..value_start]);
        redacted.push_str("***");

        match input[value_start..].find('&') {
            Some(value_len) => cursor = value_start + value_len,
            None => {
                cursor = input.len();
                break;
            }
        }
    }
    redacted.push_str(&input[cursor..]);

    // S1ja F4: the query `token=` pass above only covers the retired `?token=` leak vector.
    // Now that backdoor is gone, `Authorization: Bearer <credential>` is the desktop's only
    // credential channel, and `Sec-WebSocket-Protocol: agentloom-rc-v1, token.<hex>` is the
    // remote scope's — scrub both value shapes wherever they appear in a text blob (e.g. a
    // Debug-formatted request/headers dump ending up in a panic or connection-failure
    // message), not just in URL query strings. No known leak path emits either header into
    // such a string today (M0 §9.8 log-hygiene assertion covers that), but once query tokens
    // are gone these two are the only remaining credential shapes worth a standing defense.
    let redacted = scrub_after_marker(&redacted, "Bearer ");
    let redacted = scrub_after_marker(&redacted, "token.");

    match token {
        Some(token) if !token.is_empty() && redacted.contains(token) => {
            redacted.replace(token, "***")
        }
        _ => redacted,
    }
}

/// Both real credential shapes this guards are pure hex64 (256-bit CSPRNG desktop credential
/// / remote capability token, §9.1) — `min_hex_len` should stay well under that, not at it, so
/// legitimate hex64 material is never missed by an off-by-a-little threshold.
const MIN_SCRUBBED_HEX_LEN: usize = 32;

/// Strips the opaque value following `marker` up to the first non-hex-digit character, but
/// only when that run is at least `MIN_SCRUBBED_HEX_LEN` characters long. R3 (双路审):
/// without this floor, `scrub_after_marker` clobbers unrelated short hex-*looking* runs that
/// happen to follow a marker string — `t=token.ack` becomes `t=token.***k` (F3's own new
/// diagnostic string, `read_registry_sync_ack`'s frame-type detail, self-inflicted damage),
/// and English prose after `Bearer ` (e.g. `Bearer bad`, all-hex-alphabet words) gets
/// needlessly mangled too. Both real credential shapes this guards are pure hex64, so a
/// 32-character floor can't miss genuine credential material while it stops these short,
/// non-credential false positives.
fn scrub_after_marker(input: &str, marker: &str) -> String {
    let mut redacted = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(offset) = input[cursor..].find(marker) {
        let value_start = cursor + offset + marker.len();
        let value_len = input[value_start..]
            .find(|character: char| !character.is_ascii_hexdigit())
            .unwrap_or(input.len() - value_start);
        redacted.push_str(&input[cursor..value_start]);
        if value_len >= MIN_SCRUBBED_HEX_LEN {
            redacted.push_str("***");
        } else {
            // Below the floor: not credential-shaped enough to risk clobbering — leave the
            // run as-is (e.g. `token.ack`'s `ack`, `token.refresh_failed`'s `refresh_failed`).
            redacted.push_str(&input[value_start..value_start + value_len]);
        }
        cursor = value_start + value_len;
    }
    redacted.push_str(&input[cursor..]);
    redacted
}

fn registry_ack_reason_for_log(reason: Option<&str>) -> String {
    let reason = reason.unwrap_or("unspecified");
    let safe_shape = reason.len() <= 96
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    let contains_hash_like_run = reason
        .as_bytes()
        .windows(64)
        .any(|window| window.iter().all(u8::is_ascii_hexdigit));
    if safe_shape && !contains_hash_like_run {
        reason.to_owned()
    } else {
        "redacted".to_owned()
    }
}

pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let seconds = (1_u64 << attempt.min(6)).min(60);
    Duration::from_secs(seconds)
}

#[derive(Default)]
struct ConfigStaleBackoffGuard {
    consecutive_fast_exits: u32,
    backoff_attempts: u32,
    last_config_stale_at: Option<Instant>,
}

impl ConfigStaleBackoffGuard {
    fn delay_attempt(&mut self, now: Instant, connected_for: Duration) -> Option<u32> {
        let previous_was_recent = self.last_config_stale_at.is_some_and(|previous| {
            now.saturating_duration_since(previous) < CONFIG_STALE_GUARD_WINDOW
        });
        if connected_for >= CONFIG_STALE_GUARD_WINDOW || !previous_was_recent {
            self.consecutive_fast_exits = 0;
            self.backoff_attempts = 0;
        }

        self.consecutive_fast_exits = self.consecutive_fast_exits.saturating_add(1);
        self.last_config_stale_at = Some(now);
        if self.consecutive_fast_exits < CONFIG_STALE_GUARD_THRESHOLD {
            return None;
        }

        let attempt = self.backoff_attempts;
        self.backoff_attempts = self.backoff_attempts.saturating_add(1);
        Some(attempt)
    }

    fn reset(&mut self) {
        self.consecutive_fast_exits = 0;
        self.backoff_attempts = 0;
        self.last_config_stale_at = None;
    }
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

fn connect_loop(
    inner: Weak<Inner>,
    upstream_rx: Receiver<(u64, LiveQueueItem)>,
    milestone_rx: Receiver<(u64, MilestoneItem)>,
) {
    connect_loop_with(
        inner,
        upstream_rx,
        milestone_rx,
        attempt_once,
        interruptible_sleep,
        wait_for_reload,
    );
}

fn connect_loop_with(
    inner: Weak<Inner>,
    upstream_rx: Receiver<(u64, LiveQueueItem)>,
    milestone_rx: Receiver<(u64, MilestoneItem)>,
    mut attempt_connection: impl FnMut(
        &Arc<Inner>,
        &Receiver<(u64, LiveQueueItem)>,
        &Receiver<(u64, MilestoneItem)>,
    ) -> ConnectAttempt,
    mut sleep: impl FnMut(&Inner, Duration) -> bool,
    mut wait: impl FnMut(&Inner) -> bool,
) {
    let mut failed_attempts = 0_u32;
    let mut config_stale_guard = ConfigStaleBackoffGuard::default();

    loop {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        if inner.shutdown.load(Ordering::Acquire) {
            return;
        }
        if inner.reload_requested.swap(false, Ordering::AcqRel) {
            failed_attempts = 0;
            config_stale_guard.reset();
            set_status(&inner.state, GatewayState::Waiting, None);
        } else if inner.registry_publish_wake.swap(false, Ordering::AcqRel) {
            failed_attempts = 0;
            set_status(&inner.state, GatewayState::Waiting, None);
        }

        let attempt = catch_unwind(AssertUnwindSafe(|| {
            attempt_connection(&inner, &upstream_rx, &milestone_rx)
        }));
        if inner.shutdown.load(Ordering::Acquire) {
            return;
        }

        let delay_attempt = match attempt {
            Ok(ConnectAttempt::Disabled) => {
                config_stale_guard.reset();
                set_status(&inner.state, GatewayState::Disabled, None);
                if sleep(&inner, SETTINGS_POLL_INTERVAL) {
                    return;
                }
                continue;
            }
            Ok(ConnectAttempt::Waiting) => {
                config_stale_guard.reset();
                set_status(&inner.state, GatewayState::Waiting, None);
                if sleep(&inner, SETTINGS_POLL_INTERVAL) {
                    return;
                }
                continue;
            }
            Ok(ConnectAttempt::Ran {
                result: Ok(ConnectionExit::ClosedByPeer),
                ..
            }) => {
                config_stale_guard.reset();
                record_disconnect(&inner.state, DisconnectKind::ClosedByPeer, "closed_by_peer");
                failed_attempts = 0;
                set_status(&inner.state, GatewayState::Backoff, None);
                0
            }
            Ok(ConnectAttempt::Ran {
                result: Ok(ConnectionExit::ConfigStale { connected_for }),
                ..
            }) => {
                record_disconnect(&inner.state, DisconnectKind::ConfigStale, "config_stale");
                let Some(delay_attempt) =
                    config_stale_guard.delay_attempt(Instant::now(), connected_for)
                else {
                    continue;
                };
                set_status(&inner.state, GatewayState::Backoff, None);
                delay_attempt
            }
            Ok(ConnectAttempt::Ran {
                result: Ok(ConnectionExit::PairingReloadRequested),
                ..
            }) => {
                config_stale_guard.reset();
                failed_attempts = 0;
                continue;
            }
            Ok(ConnectAttempt::Stopped(reason)) => {
                config_stale_guard.reset();
                let redacted = redact(&reason.message, None);
                eprintln!("remote gateway stopped: {redacted}");
                set_stopped_status(&inner.state, redacted, Some(reason.code.to_owned()));
                if wait(&inner) {
                    return;
                }
                continue;
            }
            Ok(ConnectAttempt::Ran {
                token_for_redact,
                result: Err(error),
            }) => {
                config_stale_guard.reset();
                record_failure(
                    &inner,
                    FailureKind::Connection,
                    error.to_string(),
                    token_for_redact.as_ref().map(SecretToken::expose),
                    &mut failed_attempts,
                )
            }
            Err(payload) => {
                config_stale_guard.reset();
                let token_literal = take_active_token(&inner);
                record_failure(
                    &inner,
                    FailureKind::Panic,
                    panic_message(payload),
                    token_literal.as_deref(),
                    &mut failed_attempts,
                )
            }
        };

        if sleep(&inner, backoff_delay(delay_attempt)) {
            return;
        }
    }
}

#[derive(Debug)]
enum ConnectAttempt {
    Disabled,
    Waiting,
    Stopped(StopReason),
    Ran {
        token_for_redact: Option<SecretToken>,
        result: Result<ConnectionExit, ConnectionFailure>,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum ConnectionFailure {
    Unauthorized,
    Tombstoned,
    Stopped(StopReason),
    Other(String),
}

impl std::fmt::Display for ConnectionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("relay rejected desktop authorization (401)"),
            Self::Tombstoned => formatter.write_str("relay room is tombstoned (410)"),
            Self::Stopped(reason) => formatter.write_str(&reason.message),
            Self::Other(error) => formatter.write_str(error),
        }
    }
}

impl From<String> for ConnectionFailure {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionExit {
    ClosedByPeer,
    ConfigStale { connected_for: Duration },
    PairingReloadRequested,
}

#[derive(Clone, Copy)]
enum DisconnectKind {
    ConfigStale,
    ClosedByPeer,
    Error,
}

#[derive(Clone, Copy)]
enum FailureKind {
    Connection,
    Panic,
}

/// M2-4d（§8 M2-4d·母设计 §0.5 决策 1「单活跃房间」）：解析顺序两态——legacy 全局
/// `remote_room_id` 回落已撤（手机端从未存在、无真实配对可保全，迁移策略钉死不迁移，见设计
/// 稿 §0.5 决策 1 / M0 v1.8.9 变更日志「legacy 回落(M2-4d 撤)」）：
/// ① `remote_active_repo_id` 已设 && remote 已启用 → 用 `active_room_resolver` ensure 该
///    project 的房间（有房复用 / 无房新建）+ 凭据幂等先行，用这间房；
/// ② 否则（active 未设/纯空白，或虽设了但 remote 未启用，或 resolver 解析失败）→
///    `room_id_raw` 恒 `None`，`parse_config` 走既有「未配置」分支（网关 Waiting·不连接）。
///    不再读 `remote_room_id` 这个 app_setting——它的存量数据不迁移、一行不动，只是网关从此
///    不读。
///
/// active 解析失败（DB 错误 / `ensure_remote_room_for_project` 重试耗尽等）按状态②处理（暂无
/// 可用配置，等下一次解析自愈），只打日志，不回落任何全局房间。
///
/// 本函数被 `attempt_once`（真正尝试连接前）与已连接期间的 liveness 轮询（每
/// `liveness_interval` 一次）共用同一份逻辑；`active_room_resolver` 的生产实现自带「已确认
/// 过凭据存在的房间」缓存，稳态下重复调用不会重复触碰钥匙串（见
/// `remote_gateway_active_room_resolver` doc）。
///
/// M2-4d：`GatewayConfig` 一旦被构造出来，`active_repo_id` 恒 `Some`（只有 active 分支成功时
/// 才会走到 `parse_config` 的「已配置」分支）——`RoomSource` 枚举与它承载的「claim 冲突时能不
/// 能自动换房」分流已随 legacy 一并撤除，`ensure_claim` 现在对查无设备的冲突恒直接 Stop，不
/// 再进任何换房路径。
fn current_config(inner: &Inner) -> (bool, Option<GatewayConfig>) {
    let enabled_raw = (inner.settings)("remote_control_enabled");
    // relay 地址留空（未设置/纯空白）时兜底到官方公共中继——见 `effective_relay_url` doc。
    let relay_url_raw = effective_relay_url((inner.settings)("remote_relay_url"));
    let enabled = enabled_raw.as_deref() == Some("true");
    let active_repo_id_raw = (inner.settings)("remote_active_repo_id");
    // R4：读侧 trim，跟写侧 `remote_set_active_project_in_conn` 的 trim+filter 对称——两边都
    // 不把纯空白值当成"已设"。trim 后的值同时也是喂给 `active_room_resolver` 的值，不是原始
    // 未 trim 的字符串。
    let active_repo_id = active_repo_id_raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let (room_id_raw, config_active_repo_id): (Option<String>, Option<String>) =
        match active_repo_id {
            Some(project_id) if enabled => match (inner.active_room_resolver)(project_id) {
                Ok(room_id) => (Some(room_id), Some(project_id.to_owned())),
                Err(error) => {
                    eprintln!(
                        "remote gateway: active room resolution failed for project \
                         {project_id}: {error}"
                    );
                    // 解析失败：不再有 legacy 可回落——room_id 是 None 时 `parse_config` 走既有
                    // 「未配置」分支，`GatewayConfig` 根本不会被构造出来。
                    (None, None)
                }
            },
            // active 未设（或纯空白）或 remote 未启用——两者都直接判「未配置」，不读任何全局
            // 房间设置。
            _ => (None, None),
        };

    parse_config(
        enabled_raw.as_deref(),
        relay_url_raw.as_deref(),
        room_id_raw.as_deref(),
        config_active_repo_id,
    )
}

fn attempt_once(
    inner: &Arc<Inner>,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
) -> ConnectAttempt {
    let (enabled, config) = current_config(inner);
    if !enabled {
        inner.state.disable_upstream_gate();
        return ConnectAttempt::Disabled;
    }
    let Some(config) = config else {
        inner.state.disable_upstream_gate();
        return ConnectAttempt::Waiting;
    };

    set_status(&inner.state, GatewayState::Connecting, None);
    // S1ja §9.7 后门退役: `token_provider` used to read the dev-only `remote_dev_token`
    // app-setting (T5c interop placeholder) — that read is gone (lib.rs wires a
    // `|| None` stub here now), so this always evaluates to `None`. The plumbing itself
    // (SecretToken/active_token/evaluate_connection_liveness's token comparison) stays:
    // it is the generic "credential material rotated mid-connection → force reconnect"
    // path, not specific to the retired dev token.
    let token = (inner.token_provider)().map(SecretToken::new);
    set_active_token(inner, token.as_ref());
    let credential = match (inner.desktop_credential_provider)(&config.room_id) {
        Ok(credential) => DesktopCredential::new(credential),
        Err(error) => {
            clear_active_token(inner);
            return ConnectAttempt::Ran {
                token_for_redact: token,
                result: Err(ConnectionFailure::Other(format!(
                    "desktop credential unavailable: {error}"
                ))),
            };
        }
    };
    let k_room = (inner.k_room_provider)(&config.room_id);
    let url = build_ws_url(&config.relay_url, &config.room_id);
    let mut retried_after_claim = false;
    let result = loop {
        let result = run_authenticated_connection(
            inner,
            &url,
            &credential,
            &config,
            token.as_ref(),
            upstream_rx,
            milestone_rx,
            k_room.as_ref(),
        );
        match result {
            Err(ConnectionFailure::Tombstoned) => {
                clear_active_token(inner);
                return ConnectAttempt::Stopped(StopReason::new(
                    ROOM_TOMBSTONED_STOP_REASON,
                    ROOM_TOMBSTONED_STOP_ERROR,
                ));
            }
            Err(ConnectionFailure::Unauthorized) if !retried_after_claim => {}
            Err(ConnectionFailure::Stopped(reason)) => {
                clear_active_token(inner);
                return ConnectAttempt::Stopped(reason);
            }
            result => break result,
        }

        retried_after_claim = true;
        match ensure_claim(inner, &config, &credential) {
            ClaimAction::Reconnect => continue,
            ClaimAction::Backoff(error) => break Err(ConnectionFailure::Other(error)),
            ClaimAction::Stop(error) => {
                clear_active_token(inner);
                return ConnectAttempt::Stopped(error);
            }
        }
    };
    clear_active_token(inner);
    ConnectAttempt::Ran {
        token_for_redact: token,
        result,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ClaimAction {
    Reconnect,
    Backoff(String),
    Stop(StopReason),
}

#[derive(Debug, PartialEq, Eq)]
struct StopReason {
    code: &'static str,
    message: String,
}

impl StopReason {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn ensure_claim(
    inner: &Inner,
    config: &GatewayConfig,
    credential: &DesktopCredential,
) -> ClaimAction {
    let credential_hash = crate::remote_pairing::desktop_credential_hash(credential.expose());
    let response = match (inner.claim_client)(&config.relay_url, &config.room_id, &credential_hash)
    {
        Ok(response) => response,
        Err(error) => return ClaimAction::Backoff(format!("room claim failed: {error}")),
    };

    match response {
        ClaimResponse::Claimed => ClaimAction::Reconnect,
        ClaimResponse::RateLimited => {
            ClaimAction::Backoff("room claim rate-limited (429)".to_owned())
        }
        ClaimResponse::Tombstoned => ClaimAction::Stop(StopReason::new(
            ROOM_TOMBSTONED_STOP_REASON,
            ROOM_TOMBSTONED_STOP_ERROR,
        )),
        // M2-4d：Conflict 分支下 `Ok(true)`（房内有已知设备）与 `Ok(false)`（房内查无设备）现在
        // 都收敛到明确 Stop（fail-closed）——前者是既有行为（保护已配对设备不自动换房），后者
        // 是单活跃房间模型的行为：per-project 房间换不动房（换房只会解回同一个 project 绑定的
        // 房间），与其空转重试不如直接停机让用户去 Settings 处理。legacy 全局房间的自动换房
        // 自愈路径（`room_regenerator`/`ClaimAction::RoomRegenerated`/`MAX_ROOM_REGENERATIONS`）
        // 随 legacy 回落一并撤除——它只服务这条 `Ok(false)` 分支，没有其它触发点（不服务
        // Tombstoned/`Ok(true)`，那两条从来都是直接 Stop）。
        ClaimResponse::Conflict => match (inner.active_device_provider)(&config.room_id) {
            Err(error) => ClaimAction::Stop(StopReason::new(
                ROOM_DEVICE_STATUS_UNAVAILABLE_STOP_REASON,
                format!("无法确认当前房间的配对设备状态；为保护既有设备，远程控制已停机: {error}"),
            )),
            Ok(true) => ClaimAction::Stop(StopReason::new(
                ROOM_CLAIM_CONFLICT_STOP_REASON,
                ROOM_CLAIM_CONFLICT_STOP_ERROR,
            )),
            Ok(false) => ClaimAction::Stop(StopReason::new(
                ROOM_CLAIM_CONFLICT_PROJECT_STOP_REASON,
                ROOM_CLAIM_CONFLICT_PROJECT_STOP_ERROR,
            )),
        },
    }
}

fn record_failure(
    inner: &Inner,
    kind: FailureKind,
    error: String,
    token: Option<&str>,
    failed_attempts: &mut u32,
) -> u32 {
    if matches!(kind, FailureKind::Panic) {
        inner.state.panics.fetch_add(1, Ordering::Relaxed);
    }
    inner
        .state
        .connection_failures
        .fetch_add(1, Ordering::Relaxed);

    let redacted = redact(&error, token);
    if matches!(kind, FailureKind::Connection) {
        record_disconnect(&inner.state, DisconnectKind::Error, &redacted);
    }
    match kind {
        FailureKind::Connection => eprintln!("remote gateway connection failed: {redacted}"),
        FailureKind::Panic => eprintln!("remote gateway connection panicked: {redacted}"),
    }
    set_status(&inner.state, GatewayState::Backoff, Some(redacted));

    let attempt = *failed_attempts;
    *failed_attempts = failed_attempts.saturating_add(1);
    attempt
}

fn record_disconnect(state: &GatewayInnerState, kind: DisconnectKind, reason: &str) {
    let counter = match kind {
        DisconnectKind::ConfigStale => &state.disconnect_config_stale,
        DisconnectKind::ClosedByPeer => &state.disconnect_closed_by_peer,
        DisconnectKind::Error => &state.disconnect_error,
    };
    counter.fetch_add(1, Ordering::Relaxed);
    *lock(&state.last_disconnect_reason) = reason.to_owned();
}

#[derive(Debug, PartialEq, Eq)]
enum ConnectionDecision {
    Continue,
    Disconnect,
}

fn evaluate_connection_liveness(
    current_enabled: bool,
    current_config: Option<&GatewayConfig>,
    connected_config: &GatewayConfig,
    current_token: Option<&SecretToken>,
    connected_token: Option<&SecretToken>,
    connected_k_room_available: bool,
    current_k_room_available: bool,
) -> ConnectionDecision {
    if !current_enabled || (!connected_k_room_available && current_k_room_available) {
        return ConnectionDecision::Disconnect;
    }
    match current_config {
        Some(config) if config == connected_config && current_token == connected_token => {
            ConnectionDecision::Continue
        }
        _ => ConnectionDecision::Disconnect,
    }
}

/// 由 `run_session_index_snapshot_worker` 在专门的后台线程上调用，`connection_generation` 是
/// worker 那一轮开始时读到的"当前最新请求的 generation"（见 `request_session_index_snapshot`）。
/// provider 调用本身耗时不可控（DB mutex 竞争 + O(会话数)扫描 + JSON 序列化），这段时间里连接
/// 完全可能已经被更新的连接顶替——但这不再需要在这里额外核对：`enqueue_milestone_with_generation`
/// 用的就是这里传入的 `connection_generation`，不会重读"当前"值，所以即使被顶替，入队的条目
/// 依然老老实实打着捕获时刻的旧标签；下游既有的陈旧过滤器
/// （`drain_milestone_queue`/`drain_live_queue` 里 `item_generation != connection_generation`）
/// 会在 drain 时把它当陈旧丢弃。正确性只依赖这一条路径，这里不再做任何"重新核对是否被顶替"的
/// 快路径优化——之前那版"核对通过之后、真正入队之前只有几条原子指令"的说法并不成立（M#4/M#5
/// 复审定罪：线程可以在任意一条指令后被抢占，这个说法从一开始就是错的），干脆删掉靠不住的
/// 快路径，只保留这一条经得起复审的正确性路径。
fn publish_session_index_snapshot_on_connect(inner: &Inner, connection_generation: u64) {
    match (inner.session_index_snapshot_provider)() {
        Some(sessions) => {
            // M2-4c（a）：Active 模式下快照只含当前 active repo 的会话——见函数文档。
            let sessions = filter_session_index_snapshot_for_active_repo(inner, sessions);
            // M2-4x：顶层"当前被远程的项目"摘要——见 active_repo_summary_for_snapshot 文档。
            // 摘要取自过滤后（截尾前）的行：截尾只会从尾部丢行，首行（摘要取名字的来源）
            // 在正常数据规模下恒存活，摘要不需要等截尾完成才能算。
            let repo = active_repo_summary_for_snapshot(inner, &sessions);
            // B2（backlog 跟进）：过滤后、组装 payload 前的发送前尺寸闸——见
            // truncate_session_index_snapshot_rows 文档。
            let (sessions, truncated) =
                truncate_session_index_snapshot_rows(sessions, SNAPSHOT_SEND_BUDGET_BYTES);
            let client_msg_id = try_random_client_msg_id().unwrap_or_default();
            let payload = build_session_index_snapshot_payload(sessions, repo);
            let payload = mark_session_index_snapshot_truncated(payload, truncated);
            enqueue_milestone_with_generation(
                &inner.state,
                &inner.milestone_tx,
                connection_generation,
                MilestoneItem {
                    session: None,
                    t: "session.index".to_owned(),
                    payload,
                    client_msg_id,
                },
            );
        }
        None => {
            inner
                .state
                .session_index_snapshot_unavailable
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// M0 v1.7.5 §4d：连接后重发批——紧随 session.index(full) 快照之后，在同一个
/// remote-index-snapshot 后台线程里从 DB 直接重建最近里程碑并入队。client_msg_id 的推导/payload
/// 构造直接复用 derive_msg_completed_client_msg_id / build_msg_completed_payload /
/// derive_card_created_client_msg_id / build_card_created_payload /
/// derive_card_resolved_client_msg_id / build_card_resolved_payload——与首发路径完全同一份函数，
/// 不允许另起拼接逻辑（防漂移，见函数级测试）。generation 语义同
/// publish_session_index_snapshot_on_connect：调用方捕获的 connection_generation 原样透传给
/// enqueue_milestone_with_generation，这里不重读"当前"值。
/// idlefix-T1 缺口②追加：msg.completed/card.* 之外，同一批还追加 `run.status` 现状帧（见下面
/// `publish_run_status_replay_rows`）——拆成两个子函数，各自独立 provider、独立失败。
fn publish_milestone_replay_batch_on_connect(inner: &Inner, connection_generation: u64) {
    publish_msg_and_card_replay_rows(inner, connection_generation);
    publish_run_status_replay_rows(inner, connection_generation);
}

/// 拆出 msg.completed/card.* 那段（原 `publish_milestone_replay_batch_on_connect` 函数体），
/// 与下面的 `publish_run_status_replay_rows` 各自独立 provider、独立失败——`milestone_replay_
/// provider` 读失败不该连累 `run.status` 现状帧补发也一起跳过，两者是各自 best-effort 的 DB 读。
fn publish_msg_and_card_replay_rows(inner: &Inner, connection_generation: u64) {
    let Some(rows) = (inner.milestone_replay_provider)() else {
        return;
    };
    for row in rows {
        let client_msg_id = derive_msg_completed_client_msg_id(&row.session_id, &row.dedup_key);
        // 显示当前 agent（MA1）已知 gap：`db::MilestoneReplayRow` 尚不携带
        // `agent_name_snapshot`（另立单），补发路径这里暂传 `None`——首发（live）
        // msg.completed 帧会带 agent，重连补发的同一条消息暂不带，与 fixture
        // coverage 里 msg.completed 条目的 gap 说明保持一致，不是遗漏。
        let payload =
            build_msg_completed_payload(row.message_id, &row.role, row.content_json.clone(), None);
        enqueue_milestone_with_generation(
            &inner.state,
            &inner.milestone_tx,
            connection_generation,
            MilestoneItem {
                session: Some(row.session_id.clone()),
                t: "msg.completed".to_owned(),
                payload,
                client_msg_id,
            },
        );

        let Some(blocks) = row.content_json.as_array() else {
            continue;
        };
        for block in blocks {
            let Some(obj) = block.as_object() else {
                continue;
            };
            if obj.get("type").and_then(Value::as_str) != Some("decision_card") {
                continue;
            }
            let Some(decision_id) = obj.get("decision_id").and_then(Value::as_str) else {
                continue;
            };
            let created_client_msg_id = derive_card_created_client_msg_id(decision_id);
            let created_payload = build_card_created_payload(block.clone());
            enqueue_milestone_with_generation(
                &inner.state,
                &inner.milestone_tx,
                connection_generation,
                MilestoneItem {
                    session: Some(row.session_id.clone()),
                    t: "card.created".to_owned(),
                    payload: created_payload,
                    client_msg_id: created_client_msg_id,
                },
            );

            let status = obj
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            if status != "pending" {
                let chosen_option = obj.get("chosen_option").and_then(Value::as_str);
                let resolved_client_msg_id =
                    derive_card_resolved_client_msg_id(decision_id, status);
                let resolved_payload =
                    build_card_resolved_payload(decision_id, status, chosen_option);
                enqueue_milestone_with_generation(
                    &inner.state,
                    &inner.milestone_tx,
                    connection_generation,
                    MilestoneItem {
                        session: Some(row.session_id.clone()),
                        t: "card.resolved".to_owned(),
                        payload: resolved_payload,
                        client_msg_id: resolved_client_msg_id,
                    },
                );
            }
        }
    }
}

/// idlefix-T1 缺口②：连接后补发批追加——除了 msg.completed/card.*，把 `session_runtime`
/// 现状也逐会话重建成 `run.status` 帧一并塞进补发批（不新增帧类型/不改帧结构，只是把既有类型
/// 的现状帧加进这批）。手机顶栏唯一数据源就是 `run.status` 里程碑，此前只在状态变化时 publish
/// 一次、错过就永久卡住——现在中途接入也能补到当前状态，与会话列表绿点（session.index 行
/// status，同样连接后必补发）同源，不会再灰绿不一致。
fn publish_run_status_replay_rows(inner: &Inner, connection_generation: u64) {
    // idlefix-T1 补针 C（TOCTOU）：持锁跨越"读 DB + 逐行入队"整个过程，不是只护入队循环——见
    // `run_status_replay_gate` 字段头注的顺序不变量论证；`enqueue_run_status_milestone_with_gate`
    // 是唯一另一个持有同一把锁的调用方。
    let _replay_gate = lock(&inner.state.run_status_replay_gate);
    let Some(rows) = (inner.session_runtime_replay_provider)() else {
        return;
    };
    for row in rows {
        let client_msg_id = derive_run_status_replay_client_msg_id(
            &row.session_id,
            &row.status,
            row.run_id.as_deref(),
        );
        let payload = build_run_status_payload(&row.session_id, &row.status, row.run_id.as_deref());
        enqueue_milestone_with_generation(
            &inner.state,
            &inner.milestone_tx,
            connection_generation,
            MilestoneItem {
                session: Some(row.session_id.clone()),
                t: "run.status".to_owned(),
                payload,
                client_msg_id,
            },
        );
    }
}

/// 常驻单 worker：整个进程生命周期内至多 spawn 一次（见 `ensure_snapshot_worker`），靠
/// `rx.recv()` 阻塞等待唤醒信号，latest-wins 轮询 `snapshot_requested_generation`。持有的是
/// `Weak<Inner>` 而不是 `Arc<Inner>`——如果这里持有强引用，`Inner`（连同它里面存着的
/// `snapshot_wake_tx` 那个 Sender）就永远不会被析构，`rx.recv()` 也就永远不会因为"所有
/// Sender 都被丢弃"而返回 `Err`，这个线程会在 `Inner` 该退场时依然阻塞在这里、造成线程泄漏
/// （尤其是测试场景：每个测试自己建一个 `Inner`，如果 worker 强引用它，成百上千个测试跑下来
/// 会攒下同样多个永不退出的阻塞线程）。`Weak` 让"最后一个 `Arc<Inner>` 被丢弃 → `Inner` 析构
/// → 内部 `snapshot_wake_tx` 的 `SyncSender` 析构 → channel 断连 → `rx.recv()` 返回 `Err`"
/// 这条链路自然成立，线程随 `Inner` 的生命周期自动收尾，不需要显式关闭信号。
fn run_session_index_snapshot_worker(inner: Weak<Inner>, rx: Receiver<()>) {
    while rx.recv().is_ok() {
        loop {
            let Some(strong_inner) = inner.upgrade() else {
                return;
            };
            let generation = strong_inner
                .state
                .snapshot_requested_generation
                .load(Ordering::SeqCst);
            publish_session_index_snapshot_on_connect(&strong_inner, generation);
            publish_milestone_replay_batch_on_connect(&strong_inner, generation);
            let latest = strong_inner
                .state
                .snapshot_requested_generation
                .load(Ordering::SeqCst);
            drop(strong_inner);
            if latest == generation {
                break;
            }
        }
    }
}

/// 惰性建立 session-index 常驻 worker。`OnceLock::get_or_init` 在并发调用下只允许一个初始化
/// 闭包成功完成，因此"整个 `Inner` 生命周期至多 spawn 一个 worker 线程"现在由数据结构直接
/// 保证，不再依赖旧版 clear-then-recheck 退休窗口中的交错推理。spawn 失败沿用 gateway 主线程
/// 的启动策略直接 panic；`OnceLock` 不会在闭包 panic 时留下已初始化值，后续调用仍能自然重试。
fn ensure_snapshot_worker(inner: &Arc<Inner>) -> &SyncSender<()> {
    inner.state.snapshot_wake_tx.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let weak_inner = Arc::downgrade(inner);
        thread::Builder::new()
            .name("remote-index-snapshot".to_owned())
            .spawn(move || run_session_index_snapshot_worker(weak_inner, rx))
            .expect("failed to start remote-index-snapshot thread");
        inner
            .state
            .snapshot_worker_spawn_count
            .fetch_add(1, Ordering::Relaxed);
        tx
    })
}

/// 请求一次 session-index 快照（每次连接建立时调用）：先发布最新 generation，再给容量为 1 的
/// channel 尝试投递唤醒信号。`Full` 表示已有一次唤醒尚未消费，常驻 worker 醒来后仍会重读上面
/// 发布的最新值，不需要按请求次数排队；在 provider 遵守文件顶部"失败返回 None、不 panic"合约
/// 的前提下，`Disconnected` 只可能发生在 `Inner` 析构收尾附近，这里保持非阻塞、静默丢弃。
/// 配合 `ensure_snapshot_worker`，重连风暴只会唤醒同一个常驻线程，不会再在任何退休窗口里堆积
/// 线程对象。
fn request_session_index_snapshot(inner: &Arc<Inner>, connection_generation: u64) {
    inner
        .state
        .snapshot_requested_generation
        .store(connection_generation, Ordering::SeqCst);
    let tx = ensure_snapshot_worker(inner);
    let _ = tx.try_send(());
}

fn run_authenticated_connection(
    inner: &Arc<Inner>,
    url: &str,
    credential: &DesktopCredential,
    connected_config: &GatewayConfig,
    connected_token: Option<&SecretToken>,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Result<ConnectionExit, ConnectionFailure> {
    let request = build_ws_request(url, credential).map_err(ConnectionFailure::Other)?;
    run_connection_request(
        inner,
        request,
        connected_config,
        connected_token,
        upstream_rx,
        milestone_rx,
        k_room,
    )
}

fn registry_snapshot_for_send(
    inner: &Inner,
    room_id: &str,
    now_ms: u64,
    rebase_high_water: Option<i64>,
) -> Result<RegistrySnapshot, ConnectionFailure> {
    // This is the process-wide §9.4 registry lock. Future grant/rotate/revoke producers must use
    // this exact lock before touching their DB rows or outbox entries.
    let mut registry = lock(&inner.registry);
    if registry.pairing_entry.is_some() && registry.active_pairing_entry(now_ms).is_none() {
        // Natural expiry only removes the local snapshot/outbox source. It deliberately does not
        // enqueue token.delete: the relay-side pairing grant expires on its own access_expires.
        registry.clear_pairing_entry();
    }
    let (mut snapshot, revoke_generations) = if let Some(high_water) = rebase_high_water {
        let include_pairing = registry.active_pairing_entry(now_ms).is_some();
        let revoke_subjects = registry.pending_revoke_subjects();
        let (snapshot, pairing_generation, revoke_generations) = (inner.registry_rebase_provider)(
            room_id,
            high_water,
            now_ms,
            include_pairing,
            &revoke_subjects,
        )
        .map_err(ConnectionFailure::Other)?;
        if let (Some(entry), Some(generation)) =
            (registry.pairing_entry.as_mut(), pairing_generation)
        {
            entry.generation = generation;
        }
        (snapshot, revoke_generations)
    } else {
        let snapshot = (inner.registry_snapshot_provider)(room_id, now_ms)
            .map_err(ConnectionFailure::Other)?;
        (snapshot, Vec::new())
    };
    if let Some(pairing) = registry.active_pairing_entry(now_ms) {
        snapshot.entries.push(pairing);
    }
    registry.rebase_outbox_entries(&snapshot.entries, &revoke_generations);
    drop(registry);

    if snapshot.entries.len() > MAX_REGISTRY_SYNC_ENTRIES {
        return Err(ConnectionFailure::Other(format!(
            "registry snapshot has {} entries; maximum is {MAX_REGISTRY_SYNC_ENTRIES}",
            snapshot.entries.len()
        )));
    }
    Ok(snapshot)
}

fn registry_sync_frame(snapshot: &RegistrySnapshot) -> Result<String, ConnectionFailure> {
    let frame = serde_json::json!({
        "t": "token.sync",
        "revision": snapshot.revision,
        "entries": snapshot.entries,
    });
    let encoded = serde_json::to_string(&frame)
        .map_err(|error| ConnectionFailure::Other(format!("sync serialize failed: {error}")))?;
    if encoded.len() > MAX_REGISTRY_FRAME_BYTES {
        return Err(ConnectionFailure::Other(format!(
            "registry sync frame is {} bytes; maximum is {MAX_REGISTRY_FRAME_BYTES}",
            encoded.len()
        )));
    }
    Ok(encoded)
}

fn read_registry_sync_ack(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    inner: &Inner,
    expected_revision: i64,
) -> Result<i64, ConnectionFailure> {
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            let _ = socket.close(None);
            drain_close(socket);
            return Err(ConnectionFailure::Other(
                "gateway shutdown while waiting for registry sync ack".to_owned(),
            ));
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let frame: Value = serde_json::from_str(text.as_ref()).map_err(|_| {
                    ConnectionFailure::Other(
                        "invalid JSON received before registry sync ack".to_owned(),
                    )
                })?;
                if frame.get("t").and_then(Value::as_str) != Some("token.sync.ack") {
                    // S1ja F3: mixed-version rollout (desktop upgraded, relay not yet) makes
                    // relay answer with `{t:"error", reason:"unknown_frame_type"}` instead of
                    // the expected ack — the old bare string gave the operator no way to tell
                    // that apart from any other unexpected-frame cause, so every reconnect in a
                    // mixed fleet failed with zero diagnostic signal. Frame type and reason are
                    // protocol metadata, not credentials — safe to surface (log hygiene: no
                    // token/hash material here) — reuse the existing hash-scrubbing/length-cap
                    // sanitizer so a hostile or buggy relay can't smuggle oversized/hash-shaped
                    // text into the UI-visible error string via either field.
                    let frame_type =
                        registry_ack_reason_for_log(frame.get("t").and_then(Value::as_str));
                    let reason = frame.get("reason").and_then(Value::as_str);
                    let detail = match reason {
                        Some(reason) => format!(
                            "t={frame_type}, reason={}",
                            registry_ack_reason_for_log(Some(reason))
                        ),
                        None => format!("t={frame_type}"),
                    };
                    return Err(ConnectionFailure::Other(format!(
                        "relay sent a frame before registry sync ack ({detail})"
                    )));
                }
                let revision = frame
                    .get("revision")
                    .and_then(Value::as_i64)
                    .filter(|revision| *revision > 0)
                    .ok_or_else(|| {
                        ConnectionFailure::Other("sync ack revision is invalid".to_owned())
                    })?;
                if revision != expected_revision {
                    return Err(ConnectionFailure::Other(format!(
                        "sync ack revision mismatch: expected {expected_revision}, received {revision}"
                    )));
                }
                return frame
                    .get("relay_high_water")
                    .and_then(Value::as_i64)
                    .filter(|high_water| *high_water >= 0)
                    .ok_or_else(|| {
                        ConnectionFailure::Other("sync ack relay_high_water is invalid".to_owned())
                    });
            }
            Ok(Message::Close(_)) => {
                return Err(ConnectionFailure::Other(
                    "relay closed before registry sync ack".to_owned(),
                ))
            }
            Ok(Message::Ping(_)) => {}
            Ok(Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {}
            Err(WebSocketError::Io(error)) if is_read_timeout(&error) => {}
            Err(WebSocketError::ConnectionClosed) => {
                return Err(ConnectionFailure::Other(
                    "relay closed before registry sync ack".to_owned(),
                ))
            }
            Err(error) => {
                return Err(ConnectionFailure::Other(format!(
                    "sync ack read failed: {error}"
                )))
            }
        }
    }
}

fn synchronize_registry(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    inner: &Inner,
    room_id: &str,
) -> Result<(), ConnectionFailure> {
    let mut rebase_high_water = None;
    let mut rebases = 0_u8;
    loop {
        let snapshot =
            registry_snapshot_for_send(inner, room_id, now_unix_ms(), rebase_high_water)?;
        let frame = registry_sync_frame(&snapshot)?;
        socket
            .send(Message::Text(frame.into()))
            .map_err(|error| ConnectionFailure::Other(format!("sync write failed: {error}")))?;
        inner.state.frames_sent.fetch_add(1, Ordering::Relaxed);

        let relay_high_water = read_registry_sync_ack(socket, inner, snapshot.revision)?;
        // S1i3 F2：fail-closed 上界——`relay_high_water` 协议上只查了 `>= 0`（见
        // `read_registry_sync_ack`），没有上界校验。一个半可信/故障 relay 若回一个逼近
        // `i64::MAX` 的值（比如 9223372036854775806），下面的吸收会原样把它落进
        // `remote_registry_counter.next_generation`——`bump_registry_counter_to_in_
        // transaction` 这一次调用本身不会溢出（`floor.checked_add(1)` 离 `i64::MAX` 还有
        // 富余），真正炸的是**下一次**任何 `next_registry_generation_in_transaction`（配对/
        // 撤销/refresh 轮换都要走它）：`generation.checked_add(1)` 在 `i64::MAX` 上溢出、
        // 永久 `IntegralValueOutOfRange`——配对开不了、设备撤不掉、refresh 全死，且损坏已经
        // 落进桌面 DB，换回诚实 relay 也不会自愈。不能只钉 JSON safe integer（2^53）上界：
        // relay 侧 `normalizeTokenRegistryEntry` 要求 `Number.isSafeInteger(generation)`，
        // 代号一旦越过 2^53 之后每个 put 都会被 relay 判无效——只钉 2^53 仍会毁房（能配对
        // 但配不成）。这里改用「本地计数器（`snapshot.revision`）+ 合理跨度」判定，一旦
        // 越界立即停机、**绝不吸收进计数器**（不落库，不给下一轮 `absorb_registry_high_
        // water_and_rearm_revokes` 任何机会）。
        // K3.4 点名：§9.4 第 228 行字面是「每次 sync.ack 后桌面计数器必须**无条件**抬过
        // relay_high_water」——这里越界时不吸收就 return 是对那句「无条件」**有意的唯一
        // 例外**（v1.8.6 已把这条例外回写进 M0 协议规范的对应章节），不是把「无条件」漏
        // 实现了。
        // 无害：整条连接放弃、下轮重连自愈，不影响本函数上面论证的「绝不落库」。别把这
        // 个分支当回归改回「不管越界都吸收」。
        if relay_high_water
            > snapshot
                .revision
                .saturating_add(REGISTRY_HIGH_WATER_MAX_SPAN)
        {
            return Err(ConnectionFailure::Stopped(StopReason::new(
                REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON,
                REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_ERROR,
            )));
        }
        // S1h 返工二 F2：fail-closed——`relay_high_water` 协议上没有下界校验（只查
        // `>= 0`，见 `read_registry_sync_ack`），一个协议外的 relay 若回了 `H < 本次 sync
        // 的 revision`，绝不能把它原样当 floor：真实 relay 恒有 `relay_high_water >=
        // revision`（`relayHighWater = max(tableHighWater, floor, revision)`），floor 至少
        // 要跟调用方手上已有的 `snapshot.revision` 取 max，否则吸收会在 `本地计数器 ==
        // revision` 时退化成新代号等于 revision——正好撞上 relay「同代号比 fingerprint」
        // 分支，永久 rejected。
        absorb_registry_high_water_and_rearm_revokes(
            inner,
            room_id,
            relay_high_water.max(snapshot.revision),
        )?;
        if relay_high_water <= snapshot.revision {
            return Ok(());
        }
        if rebases >= MAX_REGISTRY_REBASES {
            return Err(ConnectionFailure::Stopped(StopReason::new(
                REGISTRY_REBASE_LIMIT_STOP_REASON,
                REGISTRY_REBASE_LIMIT_STOP_ERROR,
            )));
        }
        rebases += 1;
        rebase_high_water = Some(relay_high_water);
    }
}

/// S1h R1 返工：sync.ack 后立刻——计数器吸收之后、`drain_registry_outbox` 排空 outbox 之
/// 前——给 outbox 里仍待送达（含 rejected）的 revoke（token.delete）项重新领号。新代号来自
/// `registry_high_water_provider`（已把 relay_high_water 无条件吸收进桌面计数器之后再领
/// 号），因此严格大于本次 sync 的 revision、也严格大于 relay 报回的 relay_high_water——不管
/// 这一轮是否触发了后面的 rebase 循环都要做，因为「首次 sync 就直接被 relay 接受、不需要
/// rebase」正是 S1h §9.3 证据链描述的主用例（断线撤销，重连后第一次 sync 就把旧代号的
/// delete 撞成 rejected，从来没机会进入 rebase 分支）。没有待送达 revoke 时只做计数器吸收
/// 本身，不额外领号。
///
/// S1h 返工二 F2：`relay_high_water` 参数在调用点（`synchronize_registry`）已经
/// fail-closed 地跟 `snapshot.revision` 取过 max——`read_registry_sync_ack` 只校验
/// `relay_high_water >= 0`，一个协议外的 relay 若回了比本次 sync revision 还低的值，这里
/// 不能原样信它，否则会在本地计数器等于 revision 时把新代号退化成等于 revision，撞上
/// relay「同代号比 fingerprint」分支，永久 rejected。
fn absorb_registry_high_water_and_rearm_revokes(
    inner: &Inner,
    room_id: &str,
    relay_high_water: i64,
) -> Result<(), ConnectionFailure> {
    let mut registry = lock(&inner.registry);
    let revoke_subjects = registry.pending_revoke_subjects();
    let revoke_generations =
        (inner.registry_high_water_provider)(room_id, relay_high_water, &revoke_subjects)
            .map_err(ConnectionFailure::Other)?;
    registry.rearm_revoke_entries(&revoke_generations);
    Ok(())
}

fn drain_registry_outbox(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    inner: &Inner,
) -> Result<(), String> {
    drain_registry_outbox_with(inner, |message| {
        socket
            .send(message)
            .map_err(|error| format!("registry outbox write failed: {error}"))
    })
}

fn drain_registry_outbox_with<E>(
    inner: &Inner,
    mut send: impl FnMut(Message) -> Result<(), E>,
) -> Result<(), E> {
    let mut registry = lock(&inner.registry);
    for item in registry
        .outbox
        .iter_mut()
        .filter(|item| !item.acked && !item.rejected && item.last_sent_at.is_none())
    {
        item.attempts = item.attempts.saturating_add(1);
        item.last_sent_at = Some(now_unix_ms());
        if let Err(error) = send(Message::Text(item.frame.to_string().into())) {
            inner.registry_publish_wake.store(true, Ordering::Release);
            return Err(error);
        }
        inner.state.frames_sent.fetch_add(1, Ordering::Relaxed);
    }
    // Clear while holding the registry lock. A producer cannot enqueue between the completed
    // drain and this store; a producer that runs afterwards will publish a fresh wake.
    inner.registry_publish_wake.store(false, Ordering::Release);
    Ok(())
}

struct KeepaliveIdle {
    last_activity_at: Instant,
}

impl KeepaliveIdle {
    fn new(now: Instant) -> Self {
        Self {
            last_activity_at: now,
        }
    }

    fn record_activity(&mut self, now: Instant) {
        self.last_activity_at = now;
    }

    fn send_ping_if_due<E>(
        &mut self,
        now: Instant,
        state: &GatewayInnerState,
        send: impl FnOnce(Message) -> Result<(), E>,
    ) -> Result<bool, E> {
        if now.saturating_duration_since(self.last_activity_at) < KEEPALIVE_IDLE_INTERVAL {
            return Ok(false);
        }

        send(Message::Ping(Vec::new().into()))?;
        state.frames_sent.fetch_add(1, Ordering::Relaxed);
        state.keepalive_pings_sent.fetch_add(1, Ordering::Relaxed);
        self.record_activity(now);
        Ok(true)
    }
}

fn run_connection_request(
    inner: &Arc<Inner>,
    request: tungstenite::handshake::client::Request,
    connected_config: &GatewayConfig,
    connected_token: Option<&SecretToken>,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Result<ConnectionExit, ConnectionFailure> {
    // M2-4c/M2-4d：把这次连接解析出的 active repo 上下文"带进"归属判定用的共享状态，越早越
    // 好——必须抢在下面的 `request_session_index_snapshot` 之前完成，否则后台 snapshot worker
    // 可能用上一条连接（甚至上一个 project）遗留的 gating 状态构建快照。下行
    // `handle_command_envelope`、上行 drain 与 snapshot provider 全部只读这份值，不再各自
    // 重读 `remote_active_repo_id` 设置（避免设置的"当前"值与这条连接实际连的房间产生
    // 双真相）。单活跃房间模型下 `active_repo_id` 恒 `Some`（`GatewayConfig` 能被构造出来就
    // 意味着 active 解析成功了），命令归属闸恒启用，不再需要一个独立的开关布尔量。
    *lock(&inner.state.active_repo_id_for_gating) = connected_config.active_repo_id.clone();

    let pairing_k_room_is_staged = lock(&inner.registry).pairing_k_room_is_staged();
    let mut active_k_room = if pairing_k_room_is_staged {
        None
    } else {
        k_room.cloned()
    };
    // Never accept tungstenite's much larger defaults: a bad relay must not allocate huge frames.
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_INCOMING_BYTES))
        .max_frame_size(Some(MAX_INCOMING_BYTES));
    // Known limitation: DNS, TCP, TLS, and WebSocket handshakes do not yet have a timeout.
    let (mut socket, _) = match connect_with_config(request, Some(config), WS_MAX_REDIRECTS) {
        Ok(connection) => connection,
        Err(WebSocketError::Http(response))
            if response.status() == tungstenite::http::StatusCode::UNAUTHORIZED =>
        {
            return Err(ConnectionFailure::Unauthorized);
        }
        Err(WebSocketError::Http(response))
            if response.status() == tungstenite::http::StatusCode::GONE =>
        {
            return Err(ConnectionFailure::Tombstoned);
        }
        Err(error) => return Err(ConnectionFailure::Other(format!("connect failed: {error}"))),
    };

    // Blocking tungstenite has no cancellation primitive. A TCP read timeout lets this thread
    // wake periodically to observe shutdown without adding an async runtime or a wake-up socket.
    set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT))
        .map_err(|error| format!("failed to set read timeout: {error}"))?;
    set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT))
        .map_err(|error| format!("failed to set write timeout: {error}"))?;
    // §9.4 registry_ready: token.sync is the first desktop application frame, and no existing
    // activation side effect is published until the matching ack (including any rebase loop).
    synchronize_registry(&mut socket, inner, &connected_config.room_id)?;
    lock(&inner.registry).prepare_outbox_for_reconnect();
    drain_registry_outbox(&mut socket, inner).map_err(ConnectionFailure::Other)?;
    set_status(&inner.state, GatewayState::Connected, None);
    // Generation and gate are published together in a single AtomicU64 store so that a sink
    // reading upstream_state in one Acquire load can never observe a (gate, generation) pair
    // that spans two different connections.
    let connection_generation = inner
        .state
        .advance_generation_and_set_gate(active_k_room.is_some());
    // Generation filtering makes disconnect-time queue draining unnecessary, while this guard
    // closes the gate on both ordinary returns and panic unwinds.
    let _upstream_gate_guard = UpstreamGateGuard::new(&inner.state.upstream_state);
    request_session_index_snapshot(inner, connection_generation);
    let connection_loop_started_at = Instant::now();
    let mut last_liveness_check = connection_loop_started_at;
    let mut keepalive_idle = KeepaliveIdle::new(connection_loop_started_at);
    let mut last_observed_frames_sent = inner.state.frames_sent.load(Ordering::Relaxed);
    // S1i1 返工三：一次 rejected 且挂着 refresh 回执的 put 之后（`consume_token_ack` 的
    // `RefreshDropped` 分支），relay 手上的注册表仍停在旧代号——必须让 relay 学到 DB 当前
    // 真相，手机凭旧 refresh 的重试才推得动。这里只记「该断了」的意图，真正断开延后到
    // 下方断开判据成立（这一轮读超时，或挂起已超过硬截止）才执行，见下方大段注释。
    let mut registry_resync_pending = false;
    // S1i1 返工四：`registry_resync_pending` 从 false 翻到 true 的那一刻记下来，供硬截止
    // 判据用作 `Instant::elapsed` 起点；`None` 表示尚未挂起过。
    let mut registry_resync_pending_since: Option<Instant> = None;
    // M2-4c：连接生命周期内的 session → repo 归属缓存——放在这里（而不是每轮 drain 内部）
    // 是因为它要跨多轮 `drain_upstream` 调用累积命中，否则每轮清空就退化成逐条查询。
    // **`sessions.repo_id` 不是真正不可变**（B3 修正：`update_session_repo`〔lib.rs〕是已注册
    // 的运行时改绑 IPC，当前前端生产代码零调用点，但保留给未来"挪会话到别的项目"功能，且是
    // 普通内核函数，测试 / 未来功能都能直接触发——早先"实勘=不可变"的结论是错的，写死的
    // 安全不变量比没写更危险）。真正兜底的是下面这个局部变量记的 `SESSION_REPO_EPOCH` 基线
    // （F1：改成这条连接线程私有的普通 `u64`，不是 `GatewayInnerState` 里的原子——
    // `upstream_session_allowed` 只从这条连接的 drain 循环被调用，见该函数 doc；不是共享状态
    // 就不需要原子/锁，也顺手让"判定开始/判定结束前各同步一次"的两段式检测写起来更直白）：
    // `upstream_session_allowed` 每次都拿它跟全局代号比对，改绑发生后同一条连接的下一次判定
    // 就会清空缓存重建。
    let mut session_repo_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut session_repo_epoch_seen: u64 = SESSION_REPO_EPOCH.load(Ordering::Acquire);

    // S1i1 返工二 F1：闭包返回类型从 `String` 改为 `ConnectionFailure`。`ConnectionFailure`
    // 已有 `impl From<String>`，闭包内所有既有 `Result<(), String>` 早退路径的 `?` 自动走这条
    // `From` 转换，外部可观察的错误文案与此前逐字节相同。返工三额外需要它：下方 resync 分支
    // 直接构造一个 `ConnectionFailure::Other` 触发「可重试断开」，且闭包整体就是
    // `run_connection_request` 的返回值（签名同为 `Result<ConnectionExit, ConnectionFailure>`），
    // 类型必须一致。
    (|| -> Result<ConnectionExit, ConnectionFailure> {
        loop {
            if inner.shutdown.load(Ordering::Acquire) {
                let _ = socket.close(None);
                drain_close(&mut socket);
                return Ok(ConnectionExit::ClosedByPeer);
            }
            if inner.reload_requested.swap(false, Ordering::AcqRel) {
                let _ = socket.close(None);
                drain_close(&mut socket);
                return Ok(ConnectionExit::PairingReloadRequested);
            }
            // S1i1 返工四：这一轮 `socket.read()` 是不是真的读超时了——只有这个分支能证明
            // relay 这一刻确实没有新帧在路上。Ping/Pong/Binary 只说明这一轮读到的不是业务
            // Text，不代表安静（对端仍活着，缓冲区里可能还有紧随其后的 Text），不能再被当
            // 「安静」处理（返工三的 `!frame_delivered` 会把 Ping/Pong 那一轮也算安静，评审
            // 判定「太急」）。
            let mut read_timed_out_this_round = false;
            let read_result = socket.read();
            if read_result.is_ok() {
                keepalive_idle.record_activity(Instant::now());
            }
            match read_result {
                Ok(Message::Text(text)) => {
                    if let Some(response) =
                        handle_frame(inner, text.as_ref(), active_k_room.as_ref())
                    {
                        let activates_pairing =
                            response.get("t").and_then(Value::as_str) == Some("pair.ready");
                        socket
                            .send(Message::Text(response.to_string().into()))
                            .map_err(|error| format!("frame response write failed: {error}"))?;
                        inner.state.frames_sent.fetch_add(1, Ordering::Relaxed);
                        if activates_pairing {
                            if active_k_room.is_none() {
                                active_k_room = lock(&inner.registry).take_staged_pairing_k_room();
                            } else {
                                let _ = lock(&inner.registry).take_staged_pairing_k_room();
                            }
                            if active_k_room.is_some() {
                                inner.state.enable_upstream_gate();
                                request_session_index_snapshot(inner, connection_generation);
                            }
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    let _ = socket.close(None);
                    drain_close(&mut socket);
                    return Ok(ConnectionExit::ClosedByPeer);
                }
                Ok(Message::Ping(_)) => {
                    // tungstenite 0.30 queues the matching Pong in read() and flushes it itself on
                    // the next read/flush call; sending another Pong here would duplicate the reply.
                }
                Ok(Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {}
                Err(WebSocketError::ConnectionClosed) => return Ok(ConnectionExit::ClosedByPeer),
                Err(WebSocketError::Io(error)) if is_read_timeout(&error) => {
                    read_timed_out_this_round = true;
                }
                Err(error) => return Err(format!("read failed: {error}").into()),
            }

            // S1i1 返工三：`consume_token_ack` 在 `RefreshDropped` 分支置位 `resync_required`
            // 闸门（沿用返工二的字段与 `take` 语义：读且清零，一次 rejected 至多兑现一次收敛
            // 意图）。返工二在这里原地重发一次 `synchronize_registry` 并阻塞等它的
            // `token.sync.ack`——但 `read_registry_sync_ack`（本文件上方）在等 ack 期间把任何
            // 非 ack 帧都当协议违规直接报错断连；relay 侧在线 `input` 帧是直接投递、不入
            // pending（`remote-relay/src/room-do.js:576-589` + 规范 §3 第 76-77 行），若插在
            // 「重发 sync」与「ack 回来」之间会被 ack waiter 读走、报错断连——该帧从未进
            // `handle_frame`、没有本地落账、没有回 ack，用户从手机发的这条指令永久静默丢失。
            // 评审判定 BLOCKER。
            //
            // 改弦更张：不在这条存活连接里等第二次 ack，只记一个「该断了」的意图
            // （`registry_resync_pending`），本轮循环仍然只用同一套 `match socket.read()` 分发
            // 继续正常处理接下来的帧——`handle_frame` 从始至终只有这一套解释，没有第二套并行
            // 的、把非预期帧当协议违规的状态机（这正是评审点名严禁的「等 ack 期间复刻一份主
            // 循环分发逻辑」）。让既有的重连路径（`attempt_once` 重新调用
            // `run_connection_request`）去做那次 sync——顶部（下方 `synchronize_registry` 首次
            // 调用那行）就是连接建立时那条久经测试的老路，会把 DB 真相（新代号）交给 relay；
            // `prepare_outbox_for_reconnect` 也由那条路径自己负责（紧跟在顶部
            // `synchronize_registry` 之后），这里不需要重复调用。返回 `ConnectionFailure::Other`
            // 而非 `Stopped`——这是可重试的断开，`attempt_once`/`connect_loop` 走既有的失败计数
            // + 指数退避重连（`record_failure`/`backoff_delay`），天然兜住万一反复 rejected 的
            // 热循环，不会无限制地贴着 relay 的 protocol violation 预算撞
            // （`room-do.js:724-726`）。
            //
            // S1i1 返工四：返工三「只有这一轮恰好没收到新应用帧（`!frame_delivered`）才断开」
            // 的判据本身有两个反向缺陷，评审判 BLOCKER：
            // ① 太懒：`!frame_delivered` 只要求这一轮没读到 Text，而 `READ_TIMEOUT`
            // （500ms）只约束阻塞读本身——只要 relay 持续以 < 500ms 的间隔投 Text，读永远不
            // 超时、`frame_delivered` 恒真，断开永不触发，收敛没有上界；
            // ② 太急：Ping/Pong/Binary 那一轮 `frame_delivered` 也是假，会被当成「安静」立刻
            // 断开，可能把紧随其后、已经在缓冲区里的业务 Text 帧晾在原地。
            // 改法：判据只认「这一轮 `socket.read()` 真的读超时了」
            // （`read_timed_out_this_round`，只在 `is_read_timeout` 分支置位）——Ping/Pong/
            // Binary 不算安静，说明对端仍活着、后面可能还有帧，继续用同一套 `match` 正常处理；
            // 同时加一条硬截止 `REGISTRY_RESYNC_DRAIN_DEADLINE`（2 秒，从
            // `registry_resync_pending` 第一次置位的 `registry_resync_pending_since` 起算），
            // 帧完整到达时繁忙连接也必须在这个时限内无条件断开，不会被持续到达的完整 Text 帧
            // 无限期拖住。帧完整时两头都收住：安静连接第一次读超时（≤ `READ_TIMEOUT` = 500ms）
            // 就断；繁忙连接最多 2 秒内必定断，Ping/Pong 不再造成过早断开。
            //
            // 但这条硬截止本身也要「`socket.read()` 会返回」才谈得上被求值——`READ_TIMEOUT`
            // （500ms）约束的是单次底层读，不是「一条完整消息到达」。relay 若持续投喂未拼完
            // 的分片消息（字节不断到达、每次间隔都 < 500ms，但消息迟迟不完整），
            // `socket.read()` 会一直卡在 tungstenite 内部不返回——本段之后的这条硬截止判断、
            // 下方的 shutdown 检查、liveness 检查、`drain_upstream` 全都不会被求值，此时收敛
            // 没有有限上界。这不是本判据独有的缺陷，而是主循环「只在 `socket.read()` 返回后才
            // 跑一轮判据」这个既有结构的性质。触发它需要恶意或损坏的 relay；这样的 relay 本来
            // 就能直接丢弃全部帧做拒绝服务，不因此获得新能力。彻底解决要在 socket 层加读截止/
            // 看门狗，属另一件事（已记 BACKLOG）。
            let was_registry_resync_pending = registry_resync_pending;
            registry_resync_pending |= lock(&inner.registry).take_resync_required();
            if registry_resync_pending && !was_registry_resync_pending {
                registry_resync_pending_since = Some(Instant::now());
            }
            let registry_resync_deadline_elapsed = registry_resync_pending_since
                .is_some_and(|since| since.elapsed() >= REGISTRY_RESYNC_DRAIN_DEADLINE);
            if registry_resync_pending
                && (read_timed_out_this_round || registry_resync_deadline_elapsed)
            {
                return Err(ConnectionFailure::Other(
                    "refresh put rejected; reconnecting to resync registry".to_owned(),
                ));
            }

            if inner.shutdown.load(Ordering::Acquire) {
                let _ = socket.close(None);
                drain_close(&mut socket);
                return Ok(ConnectionExit::ClosedByPeer);
            }

            if let Err(error) = drain_upstream(
                &mut socket,
                &inner.state,
                upstream_rx,
                milestone_rx,
                active_k_room.as_ref(),
                &connected_config.room_id,
                &inner.session_repo_provider,
                &mut session_repo_cache,
                &mut session_repo_epoch_seen,
            ) {
                return Err(error.into());
            }
            drain_registry_outbox(&mut socket, inner)?;

            let frames_sent = inner.state.frames_sent.load(Ordering::Relaxed);
            if frames_sent != last_observed_frames_sent {
                keepalive_idle.record_activity(Instant::now());
                last_observed_frames_sent = frames_sent;
            }
            if read_timed_out_this_round {
                keepalive_idle
                    .send_ping_if_due(Instant::now(), &inner.state, |message| socket.send(message))
                    .map_err(|error| format!("keepalive ping write failed: {error}"))?;
                last_observed_frames_sent = inner.state.frames_sent.load(Ordering::Relaxed);
            }

            if last_liveness_check.elapsed() >= inner.liveness_interval {
                last_liveness_check = Instant::now();
                let (enabled, fresh_config) = current_config(inner);
                let fresh_token = (inner.token_provider)().map(SecretToken::new);
                // 已持有 K_room 时不重读钥匙串，避免钥匙串 IPC 卡住 ws 读线程；无钥匙时保持轮询自愈，钥匙撤销检测不在这里做。
                let pairing_k_room_is_staged = lock(&inner.registry).pairing_k_room_is_staged();
                let fresh_k_room_available = active_k_room.is_some()
                    || (!pairing_k_room_is_staged
                        && (inner.k_room_provider)(&connected_config.room_id).is_some());
                if evaluate_connection_liveness(
                    enabled,
                    fresh_config.as_ref(),
                    connected_config,
                    fresh_token.as_ref(),
                    connected_token,
                    active_k_room.is_some(),
                    fresh_k_room_available,
                ) == ConnectionDecision::Disconnect
                {
                    let _ = socket.close(None);
                    drain_close(&mut socket);
                    return Ok(ConnectionExit::ConfigStale {
                        connected_for: connection_loop_started_at.elapsed(),
                    });
                }
            }
        }
    })()
}

fn drain_upstream(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Result<(), String> {
    drain_upstream_with_budget(
        socket,
        state,
        upstream_rx,
        milestone_rx,
        k_room,
        room,
        session_repo_provider,
        session_repo_cache,
        session_repo_epoch_seen,
        DRAIN_ROUND_BUDGET,
    )
}

fn drain_upstream_with_budget(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
    budget: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    let connection_generation = state.connection_generation_snapshot();
    let mut drained_items = 0;

    if drain_milestone_queue(
        socket,
        state,
        milestone_rx,
        k_room,
        room,
        connection_generation,
        deadline,
        &mut drained_items,
        session_repo_provider,
        session_repo_cache,
        session_repo_epoch_seen,
    )? {
        return Ok(());
    }
    drain_live_queue(
        socket,
        state,
        upstream_rx,
        k_room,
        room,
        connection_generation,
        deadline,
        &mut drained_items,
        session_repo_provider,
        session_repo_cache,
        session_repo_epoch_seen,
    )?;
    Ok(())
}

fn drain_milestone_queue(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    connection_generation: u64,
    deadline: Instant,
    drained_items: &mut usize,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Result<bool, String> {
    while *drained_items < MAX_DRAIN_ITEMS_PER_ROUND {
        let Ok((item_generation, item)) = milestone_rx.try_recv() else {
            break;
        };
        *drained_items += 1;
        if item_generation != connection_generation {
            state
                .upstream_stale_generation_dropped
                .fetch_add(1, Ordering::Relaxed);
        } else if let Some(k_room) = k_room {
            let MilestoneItem {
                session,
                t,
                payload,
                client_msg_id,
            } = item;
            // 顺手②：先判 `contains('|')`（永远是坏帧的判据），再算归属——跟 `drain_live_queue`
            // 同款惰性写法，pipe 帧直接短路成 classify_skipped，不多花一次归属判定/查库。
            let contains_pipe = session.as_deref().is_some_and(|value| value.contains('|'));
            if contains_pipe {
                state.classify_skipped.fetch_add(1, Ordering::Relaxed);
            } else if session.is_none()
                && t == "session.index"
                && payload.get("full").and_then(Value::as_bool) != Some(true)
            {
                // M2-4c（B1）/M2-4d：session.index 的增量 diff 按 op 四路分治（详见
                // `filter_session_index_incremental_for_active_repo` 文档）；全量快照
                // （`full == true`，走 (a) 的 `filter_session_index_snapshot_for_active_repo`）
                // 不会走到这条分支。
                match filter_session_index_incremental_for_active_repo(
                    state,
                    session_repo_provider,
                    session_repo_cache,
                    session_repo_epoch_seen,
                    payload,
                ) {
                    Some(rewritten_payload) => {
                        send_upstream_value(
                            socket,
                            state,
                            k_room,
                            room,
                            "event",
                            session,
                            milestone_payload(&t, rewritten_payload),
                            Some(&client_msg_id),
                        )?;
                    }
                    None => {
                        state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else {
                // M2-4c（b）：逐条里程碑（msg.completed/card.created/card.resolved/run.status/
                // snapshot 等）发布前判 session 归属；不属于当前 active repo 时静默跳过，这是
                // 过滤不是错误。`session.is_none()` 的条目（除上面的 session.index 增量分支外，
                // 理论上不该再出现，但保持 fail-open-not：`is_some_and` 对 `None` 恒 false，不
                // 阻断）原样放行——没有 session 就没有归属可判。
                // P0-b 返工⑦如实记档（既有模式，不改行为）：snapshot 里程碑在
                // `handle_command_envelope` 里已经过一次归属闸（入队前）才被放进队列；这里是
                // 出队前的第二次判定，两次之间存在一个窄窗——如果归属在这段时间内变更（切换
                // active repo），一条已经 ack Ok 给远端的 snapshot 可能在这里被 drain 静默滤
                // 掉。这是既有的"入队时校验、出队时复核"模式（其它里程碑同款），不是 snapshot
                // 特有的新洞，此处只记档不改行为。
                let attribution_blocked = session.as_deref().is_some_and(|session_id| {
                    !upstream_session_allowed(
                        state,
                        session_repo_provider,
                        session_repo_cache,
                        session_repo_epoch_seen,
                        session_id,
                    )
                });
                if attribution_blocked {
                    state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
                    if let Some(session_id) = session.as_deref() {
                        eprintln!(
                            "remote gateway: upstream frame filtered — session={} type={}",
                            session_id.chars().take(8).collect::<String>(),
                            t
                        );
                    }
                } else {
                    send_upstream_value(
                        socket,
                        state,
                        k_room,
                        room,
                        "event",
                        session,
                        milestone_payload(&t, payload),
                        Some(&client_msg_id),
                    )?;
                }
            }
        } else {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        }

        if Instant::now() >= deadline {
            state
                .upstream_budget_dropped
                .fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }
    }
    Ok(false)
}

fn drain_live_queue(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
    room: &str,
    connection_generation: u64,
    deadline: Instant,
    drained_items: &mut usize,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Result<bool, String> {
    while *drained_items < MAX_DRAIN_ITEMS_PER_ROUND {
        let Ok((item_generation, item)) = upstream_rx.try_recv() else {
            break;
        };
        *drained_items += 1;
        if item_generation != connection_generation {
            state
                .upstream_stale_generation_dropped
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let Some(k_room) = k_room else {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        };

        match item {
            LiveQueueItem::Batch(payload) => {
                // 既有 delta 路径保持逐 batch / 逐 event 的 classify 与预算行为不变。
                for batch in payload.batches {
                    for sequenced in batch.events {
                        let classified = classify(&sequenced.event, sequenced.seq)
                            .map(|(kind, value)| (kind, value, None::<String>));

                        if let Some((kind, value, client_msg_id)) = classified {
                            // M2-4c（b）：live 事件同 msg.completed 一样按 session 归属过滤——
                            // `batch.session_id` 非 Option，恒有值。
                            let contains_pipe = batch.session_id.contains('|');
                            let attribution_blocked = !contains_pipe
                                && !upstream_session_allowed(
                                    state,
                                    session_repo_provider,
                                    session_repo_cache,
                                    session_repo_epoch_seen,
                                    &batch.session_id,
                                );
                            if contains_pipe {
                                state.classify_skipped.fetch_add(1, Ordering::Relaxed);
                            } else if attribution_blocked {
                                state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
                                eprintln!(
                                    "remote gateway: upstream frame filtered — session={} type={}",
                                    batch.session_id.chars().take(8).collect::<String>(),
                                    kind
                                );
                            } else {
                                send_upstream_value(
                                    socket,
                                    state,
                                    k_room,
                                    room,
                                    kind,
                                    Some(batch.session_id.clone()),
                                    value,
                                    client_msg_id.as_deref(),
                                )?;
                            }
                        }

                        if Instant::now() >= deadline {
                            state
                                .upstream_budget_dropped
                                .fetch_add(1, Ordering::Relaxed);
                            return Ok(true);
                        }
                    }
                }
            }
            LiveQueueItem::Prebuilt(item) => {
                if let Some(frame) = prepare_prebuilt_live_for_drain(
                    state,
                    item,
                    session_repo_provider,
                    session_repo_cache,
                    session_repo_epoch_seen,
                ) {
                    send_upstream_value(
                        socket,
                        state,
                        k_room,
                        room,
                        frame.kind,
                        Some(frame.session),
                        frame.payload,
                        Some(&frame.client_msg_id),
                    )?;
                }
                if Instant::now() >= deadline {
                    state
                        .upstream_budget_dropped
                        .fetch_add(1, Ordering::Relaxed);
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[derive(Debug, PartialEq)]
struct PreparedLiveFrame {
    kind: &'static str,
    session: String,
    payload: Value,
    client_msg_id: String,
}

/// `drain_live_queue` 的预构建分支在真正写 socket 前统一经过这里：保留 session 归属闸，
/// 并把已经定型的 history payload 包成 kind="live"。拆成纯步骤让无网络测试也能锁住这两个
/// wire/安全不变量；实际 drain 只消费这里放行的结果。
fn prepare_prebuilt_live_for_drain(
    state: &GatewayInnerState,
    item: MilestoneItem,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
) -> Option<PreparedLiveFrame> {
    let Some(session) = item.session else {
        state.classify_skipped.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let contains_pipe = session.contains('|');
    let attribution_blocked = !contains_pipe
        && !upstream_session_allowed(
            state,
            session_repo_provider,
            session_repo_cache,
            session_repo_epoch_seen,
            &session,
        );
    if contains_pipe {
        state.classify_skipped.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if attribution_blocked {
        state.upstream_repo_filtered.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "remote gateway: upstream frame filtered — session={} type={}",
            session.chars().take(8).collect::<String>(),
            item.t
        );
        return None;
    }
    Some(PreparedLiveFrame {
        kind: "live",
        session,
        payload: milestone_payload(&item.t, item.payload),
        client_msg_id: item.client_msg_id,
    })
}

fn send_upstream_value(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
    k_room: &[u8; 32],
    room: &str,
    kind: &str,
    session: Option<String>,
    value: Value,
    client_msg_id: Option<&str>,
) -> Result<(), String> {
    let plaintext = match serde_json::to_vec(&value) {
        Ok(plaintext) => plaintext,
        Err(_) => {
            state.classify_skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };
    let meta = EnvelopeMeta {
        v: 1,
        room: room.to_owned(),
        epoch: state.epoch.load(Ordering::Acquire),
        kind: kind.to_owned(),
        session,
        command_id: None,
    };
    let (ct, n) = crate::remote_crypto::seal(k_room, &meta, &plaintext);
    let envelope = build_envelope_json(&meta, &ct, &n, now_unix_ms(), client_msg_id);
    socket
        .send(Message::Text(envelope.to_string().into()))
        .map_err(|error| format!("upstream write failed: {error}"))?;
    state.frames_sent.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn milestone_payload(t: &str, payload: Value) -> Value {
    match payload {
        Value::Object(mut fields) => {
            fields.insert("t".to_owned(), Value::String(t.to_owned()));
            Value::Object(fields)
        }
        value => serde_json::json!({"t": t, "payload": value}),
    }
}

/// 量最终写入加密明文的完整里程碑帧：裸 builder payload 先合并 `t`，再按 JSON 字节数计量。
fn milestone_frame_bytes(t: &str, payload: &Value) -> usize {
    serde_json::to_vec(&milestone_payload(t, payload.clone()))
        .map(|json| json.len())
        .unwrap_or(usize::MAX)
}

/// M2-4c：归属闸共用的 fail-closed 判定核——`active_repo_id` 为 `None`（理论不可达：单活跃
/// 房间模型下有连接就必然有 `Some(active_repo_id)`，这里仍按 fail-closed 处理，不假设
/// "不可达"永远成立）、查询失败（`Err`）、会话无归属（`Ok(None)`）三种情况一律判不属于——
/// 只有"查询成功且 repo id 相等"才放行。
fn repo_id_is_active(
    active_repo_id: &Option<String>,
    lookup: &Result<Option<String>, String>,
) -> bool {
    match (active_repo_id, lookup) {
        (Some(active), Ok(Some(repo_id))) => repo_id == active,
        _ => false,
    }
}

/// F1：`session_repo_epoch_seen` 是这条远端连接自己的"上次看到的改绑代号"——线程私有的普通
/// `u64`（不是原子/不是共享状态），因为 `upstream_session_allowed` 只从 `run_connection_request`
/// 那一条连接线程被调用（`drain_upstream`/`drain_milestone_queue`/`drain_live_queue` 在生产
/// 代码里唯一的调用点就在那条循环里，无其它线程并发调用——见 `run_connection_request` 里
/// `&mut session_repo_epoch_seen` 与 `&mut session_repo_cache` 相邻声明、相邻传递）。跟全局
/// `SESSION_REPO_EPOCH`（真正跨线程，`update_session_repo` 可能从任意 IPC 调用线程 bump）
/// swap 比对，不一致就清空 `session_repo_cache` 并把本地值追上去。
///
/// 如果这次调用需要真的重新计算，返回 `true`（调用方据此决定是否清缓存+重查）。
fn sync_session_repo_cache_epoch(
    session_repo_epoch_seen: &mut u64,
    session_repo_cache: &mut HashMap<String, Option<String>>,
) -> bool {
    let current_epoch = SESSION_REPO_EPOCH.load(Ordering::Acquire);
    if current_epoch != *session_repo_epoch_seen {
        session_repo_cache.clear();
        *session_repo_epoch_seen = current_epoch;
        true
    } else {
        false
    }
}

fn lookup_session_repo(
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_id: &str,
) -> Result<Option<String>, String> {
    if let Some(cached) = session_repo_cache.get(session_id) {
        return Ok(cached.clone());
    }
    let result = (session_repo_provider)(session_id);
    if let Ok(Some(repo_id)) = &result {
        session_repo_cache.insert(session_id.to_owned(), Some(repo_id.clone()));
    }
    result
}

/// M2-4c（b）/M2-4d：上行里程碑/live 归属过滤共用判定——单活跃房间模型下恒启用，先查连接
/// 生命周期缓存，未命中才调用 `session_repo_provider`，避免发布热路径逐条开 DB 查询。
///
/// F1（B3 返工，修竞态）：`sessions.repo_id` 并非真正不可变（见 `SESSION_REPO_EPOCH`
/// 文档）——上一版只在函数**开头** load 一次全局代号，若 `update_session_repo` 恰好在
/// "load 完代号"与"缓存/查询完成"之间那段窗口里改绑，本次判定用的还是改绑前的旧归属
/// （要等下一次调用才能自愈，审查判定这个窗口必须关死，不能靠"下次"补救）。这里改成三段：
/// ①判定开始先同步一次代号（不一致就清缓存）；②做缓存命中或 provider 查询；③**判定结束前
/// 再同步一次代号**——如果这一步发现代号又变了（说明 bump 恰好夹在①②之间），说明②查到的
/// 结果可能是改绑前的旧值，直接丢弃、清缓存、重查一次；重查结果不再进行第三次核对就直接
/// 采信（至多重试一次，防止持续 bump 造成活锁——这个极窄的残余窗口留给下一次调用自愈，
/// 跟"完全不检测"是两回事）。
fn upstream_session_allowed(
    state: &GatewayInnerState,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
    session_id: &str,
) -> bool {
    let active_repo_id = lock(&state.active_repo_id_for_gating).clone();

    sync_session_repo_cache_epoch(session_repo_epoch_seen, session_repo_cache);
    let mut lookup = lookup_session_repo(session_repo_provider, session_repo_cache, session_id);

    // 判定结束前的第二次同步：查出这次判定期间代号又变了，说明上面那次查询可能拿到的是
    // 改绑前的旧值——丢弃、清缓存（`sync_session_repo_cache_epoch` 已经做了）、重查一次。
    if sync_session_repo_cache_epoch(session_repo_epoch_seen, session_repo_cache) {
        lookup = lookup_session_repo(session_repo_provider, session_repo_cache, session_id);
    }

    repo_id_is_active(&active_repo_id, &lookup)
}

/// M2-4c（a）/M2-4d：session.index 快照只含当前 active repo 的会话——在这唯一的快照汇聚点
/// 过滤 provider 已经查出来的整份 JSON 数组（O(会话数) 的内存过滤，不追加任何新 DB 查询；
/// provider 的 SQL 本身继续查全部会话）。`repo_id` 字段缺失/为 null（`SessionIndexSnapshotRow::
/// repo_id` 是 `Option<String>`）按 fail-closed 处理，不放行进快照。
fn filter_session_index_snapshot_for_active_repo(inner: &Inner, sessions: Value) -> Value {
    let Some(active_repo_id) = lock(&inner.state.active_repo_id_for_gating).clone() else {
        // 没有可判定的 active repo（理论不可达，见 repo_id_is_active 文档：单活跃房间模型下
        // 有连接就必然有 active repo）——宁可回空列表也不把全量会话当默认值泄漏出去。
        return Value::Array(Vec::new());
    };
    match sessions {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .filter(|item| {
                    item.get("repo_id").and_then(Value::as_str) == Some(active_repo_id.as_str())
                })
                .collect(),
        ),
        // 顺手①（codex 防御性）：provider 理论上恒回 `Value::Array`（生产实现序列化
        // `Vec<SessionIndexSnapshotRow>`），但这里不能假设契约不会被破坏——非数组时原样放行
        // 等于把过滤器变成摆设，fail-closed 改回空数组。
        _ => Value::Array(Vec::new()),
    }
}

/// M2-4x：全量快照顶层的"当前被远程的项目"摘要——手机端设置页切换远程项目会让已配对连接
/// 断线重连（下一单），重连后收到的第一条全量快照理应明确告诉手机端"你现在连的是哪个项目"，
/// 不能只靠会话行自己的 `repo_id` 猜。
///
/// **设计取舍（不新开一条 provider）**：`id` 直接读 `active_repo_id_for_gating`（这条连接生命
/// 周期内的单一真相源，同 `filter_session_index_snapshot_for_active_repo`）；`name` 不另开一条
/// "按 repo id 查名字"的 provider（那要往 `Inner`/`GatewayInnerState` 加新字段、改遍全部测试
/// 构造点——过度设计），而是从**已经**随快照行过滤出来的 `sessions`（`filter_session_index_
/// snapshot_for_active_repo` 的返回值，此刻传入的每一行都保证同属 active repo）里取第一行的
/// `repo_name` 字段——同一个 repo 的所有行这个字段值相同，取哪一行都一样。项目当前零会话时
/// `sessions` 为空数组，取不到任何行，`name` 退化为 `null`（`id` 仍然可靠，手机端至少知道项目
/// 换了，只是暂时没有人类可读名字——不是 fail-closed 的例外，是"数据源里本来就没有"）。
/// `active_repo_id_for_gating` 为 `None`（理论不可达，见 `repo_id_is_active` 文档）时整个摘要
/// fail-closed 回 `Value::Null`，不把"曾经见过的某个项目"当默认值泄漏出去。
///
/// **已知边界（B3 backlog 跟进·纯记档，未修）**：项目改名（`rename_repo`）不发 session.index
/// 增量——在线手机端的顶部摘要与既有会话行会一直显示旧名，直到下一次断连重连拿到新的全量
/// 快照才刷新；改名后若又有新会话在同一项目下创建，其 `created` 增量会带新名，此时同一屏可能
/// 新旧名并存（老会话行仍是旧名，新会话行已是新名）。属低频可接受；如需消除，应在 rename 路径
/// 主动发一条 session.index 增量或直接触发一次全量快照重发，本条只记档，不在本刀修。
fn active_repo_summary_for_snapshot(inner: &Inner, sessions: &Value) -> Value {
    let Some(active_repo_id) = lock(&inner.state.active_repo_id_for_gating).clone() else {
        return Value::Null;
    };
    let name = sessions
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item.get("repo_name"))
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({ "id": active_repo_id, "name": name })
}

/// B2（backlog 跟进）：`list_session_index_snapshot_rows` 的 SQL 无 `LIMIT`——过滤后的
/// `sessions` 行数组理论上随会话数增长无上限。`msg.completed` 早已在 `enqueue_milestone_item`
/// 有发送前尺寸兜底（`SNAPSHOT_SEND_BUDGET_BYTES`，见该处文档），session.index 全量快照此前
/// 完全没有——超过 relay 的 64KiB 明文帧硬闸会被 `frame_too_large` 断连，断连后客户端重连会
/// 再请求同一份超大快照，形成死循环（现状单项目最多约 20 个会话、离预算还有约 5 倍余量，
/// 属于防患于未然，不是已复现故障）。
///
/// **度量对象是 `sessions` 行数组自身的序列化字节量**，不是整帧——`repo` 摘要只是
/// `{id, name}` 两个字段，相对 `sessions` 数组的体积是常数级噪声，`SNAPSHOT_SEND_BUDGET_BYTES`
/// 本身已经在 relay 64KiB 硬闸前留出信封膨胀（base64/AEAD tag/JSON 转义）的余量，够吸收这点
/// 常数开销，不需要为此再拉高精度反而把简单问题复杂化。
///
/// **保留排序靠前的行**：`list_session_index_snapshot_rows` 的 SQL 已经按 `pinned DESC,
/// created_at DESC` 排好序，重要行天然排在数组前面——从尾部整行丢弃，直到累计字节量不超
/// `budget`。`budget` 参数化（生产调用点传 `SNAPSHOT_SEND_BUDGET_BYTES`）只是为了让单测能用
/// 小预算构造可控场景，不必在测试里堆出真正 44KiB 的 JSON。
///
/// 返回 `(截尾后的 sessions, 是否发生了截断)`；未截断时第二个值为 `false`，调用方据此决定
/// 要不要在 payload 上插 `truncated` 键（键本身只在截断时出现，见
/// `mark_session_index_snapshot_truncated`）。非数组输入（理论不可达——调用方恒传
/// `filter_session_index_snapshot_for_active_repo` 的返回值，那个函数已经 fail-closed 成
/// 数组）原样放行、不截断，不假设契约不会被破坏但也不在这里重新发明一次 fail-closed 逻辑。
fn truncate_session_index_snapshot_rows(sessions: Value, budget: usize) -> (Value, bool) {
    let Value::Array(rows) = sessions else {
        return (sessions, false);
    };
    let full_bytes = serde_json::to_vec(&Value::Array(rows.clone()))
        .map(|json| json.len())
        .unwrap_or(usize::MAX);
    if full_bytes <= budget {
        return (Value::Array(rows), false);
    }
    let mut kept: Vec<Value> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut candidate = kept.clone();
        candidate.push(row);
        let candidate_bytes = serde_json::to_vec(&Value::Array(candidate.clone()))
            .map(|json| json.len())
            .unwrap_or(usize::MAX);
        if candidate_bytes > budget {
            break;
        }
        kept = candidate;
    }
    (Value::Array(kept), true)
}

/// B2：只在真的发生截断时插入 `truncated: true` 这一键；不截断时该键整个不存在（不是
/// `false`）——同 `repo`/`repo_name` 系列可选键"键缺失 vs 显式值"的一贯纪律，手机端
/// `parseFrame` 对未知键本就无视（本任务不改手机端 UI），向后兼容零风险。
fn mark_session_index_snapshot_truncated(mut payload: Value, truncated: bool) -> Value {
    if truncated {
        if let Value::Object(fields) = &mut payload {
            fields.insert("truncated".to_owned(), Value::Bool(true));
        }
    }
    payload
}

/// M2-4c（B1）：Active 模式下 session.index 的增量 diff（`full == false`，`session` 恒
/// `None`，走 `drain_milestone_queue` 而非快照过滤）按 `op` 四路分治，不能套用逐条里程碑那套
/// "按 session 查一次"的通用判定——四种 op 的 payload 形状各不相同，各自要用不同的信息源：
/// - `created`：payload 里的 `session.repo_id` 是现成字段（`build_session_index_created_
///   payload`），直接跟 active repo 字符串比较，**零查库**；字段缺失/非字符串按 fail-closed
///   丢弃。
/// - `renamed`：payload 只有 `{id, title}`，行还在——用 `upstream_session_allowed`（走同一份
///   连接缓存）查 `id` 的归属，不属于就整条丢（连 title 一起丢，不能只脱敏 title 不挡 op）。
/// - `archived`/`unarchived`：payload 是 `{ids: [...]}`，行都还在——逐 id 查，**重写 `ids`
///   数组**只留 active repo 的；过滤后为空则整条丢（没有 id 剩下就没有 diff 好发的）。
/// - `deleted`：payload 只有 `{id}`，**行已经被删**，查 `session_repo_provider` 只会拿到
///   `Ok(None)`（会话不存在）——若照搬"查不到就丢"的规则，这条 deleted 事件会被永远吞掉：
///   手机端此前已经收到过这个会话（`created`/全量快照），现在它被删了却收不到 `deleted`
///   diff，会一直显示一个幽灵会话。`deleted` 事件本身只是一个不透明 id，不含任何用户可读
///   内容，泄漏面等于零——**显式放行，不查库**。这是审查点名过的"错误补法"陷阱：把
///   "查不到"一律当"不属于"在这个 op 上会产生比不过滤更糟的用户可见 bug。**F2 加固**：
///   "放行"不等于"原样转发调用方给的整个 payload 对象"——只信任 `id` 字段本身（必须是
///   字符串，缺失/非字符串 fail-closed 丢），其余字段一律不採信；用规范构造函数
///   `build_session_index_deleted_payload` 从 `id` 重建一份干净 payload 再转发，等价于对
///   `{op, full, id}` 这三个字段之外的一切做白名单剔除——但用"重建"而不是"逐字段核对
///   白名单再剔除"实现：前者结构上不可能让任何未预期字段survive（不依赖剔除逻辑本身没
///   bug），后者要额外维护一份字段名单还可能漏删。防的是：万一日后有 bug/改动往这条 op
///   的 payload 里夹带了额外字段（比如误把 title 也塞进来），原样转发会绕开整套归属过滤
///   白白泄漏——`deleted` 是唯一一个"不查库就放行"的分支，形状必须锁死到不给任何夹带空间。
///
/// 返回 `None` 表示整条应被丢弃；返回 `Some(payload)` 为可以真正发布的 payload（`archived`/
/// `unarchived` 分支可能已经把 `ids` 重写过，`deleted` 分支恒为重建后的干净 payload）。调用方
/// （`drain_milestone_queue`）只在 `session.index` 增量 diff（`full == false`）时调用这里。
fn filter_session_index_incremental_for_active_repo(
    state: &GatewayInnerState,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
    payload: Value,
) -> Option<Value> {
    let active_repo_id = lock(&state.active_repo_id_for_gating).clone()?;
    let op = payload.get("op").and_then(Value::as_str)?.to_owned();
    match op.as_str() {
        "created" => {
            let matches = payload
                .get("session")
                .and_then(|session| session.get("repo_id"))
                .and_then(Value::as_str)
                .is_some_and(|repo_id| repo_id == active_repo_id);
            matches.then_some(payload)
        }
        "renamed" => {
            let session_id = payload.get("id").and_then(Value::as_str)?.to_owned();
            upstream_session_allowed(
                state,
                session_repo_provider,
                session_repo_cache,
                session_repo_epoch_seen,
                &session_id,
            )
            .then_some(payload)
        }
        "archived" | "unarchived" => {
            let ids = payload.get("ids").and_then(Value::as_array)?.clone();
            let filtered: Vec<Value> = ids
                .into_iter()
                .filter(|id_value| {
                    id_value.as_str().is_some_and(|session_id| {
                        upstream_session_allowed(
                            state,
                            session_repo_provider,
                            session_repo_cache,
                            session_repo_epoch_seen,
                            session_id,
                        )
                    })
                })
                .collect();
            if filtered.is_empty() {
                None
            } else {
                let mut payload = payload;
                payload["ids"] = Value::Array(filtered);
                Some(payload)
            }
        }
        "deleted" => {
            let id = payload.get("id").and_then(Value::as_str)?;
            Some(build_session_index_deleted_payload(id))
        }
        _ => None,
    }
}

/// M2-4c/M2-4d：下行命令归属闸——单活跃房间模型下恒启用，校验 `session` 归属这条连接解析
/// 出的 active repo。fail-closed：查询失败 / 会话无归属 / 归属另一个 repo，一律拒绝；拒绝时
/// 只在非 UI 日志记一行不含 session id / repo id 的原因，供排障定位方向、不泄漏内容。
fn command_session_allowed(inner: &Inner, session: &str) -> bool {
    let active_repo_id = lock(&inner.state.active_repo_id_for_gating).clone();
    let lookup = (inner.session_repo_provider)(session);
    let allowed = repo_id_is_active(&active_repo_id, &lookup);
    if !allowed {
        eprintln!("remote gateway: command rejected — session is not owned by the active project");
    }
    allowed
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn build_envelope_json(
    meta: &EnvelopeMeta,
    ct: &str,
    n: &str,
    ts_ms: u64,
    client_msg_id: Option<&str>,
) -> serde_json::Value {
    let mut envelope = serde_json::json!({
        "v": meta.v,
        "room": meta.room,
        "epoch": meta.epoch,
        "kind": meta.kind,
        "session": meta.session,
        "command_id": serde_json::Value::Null,
        "seq": serde_json::Value::Null,
        "ct": ct,
        "n": n,
        "ts": ts_ms,
    });
    if let Some(client_msg_id) = client_msg_id {
        envelope
            .as_object_mut()
            .expect("envelope is constructed as a JSON object")
            .insert(
                "client_msg_id".to_owned(),
                Value::String(client_msg_id.to_owned()),
            );
    }
    envelope
}

fn classify(
    event: &crate::agent_event::AgentEvent,
    seq: u64,
) -> Option<(&'static str, serde_json::Value)> {
    use crate::agent_event::AgentEvent;

    match event {
        AgentEvent::TextDelta { text } => Some((
            "live",
            serde_json::json!({
                "t": "text_delta",
                "seq": seq,
                "text": truncate_utf8(text, OUTPUT_TRUNCATE_BYTES),
            }),
        )),
        AgentEvent::ThinkingDelta { text } => Some((
            "live",
            serde_json::json!({
                "t": "thinking_delta",
                "seq": seq,
                "text": truncate_utf8(text, OUTPUT_TRUNCATE_BYTES),
            }),
        )),
        AgentEvent::ToolOutputDelta { id, text } => Some((
            "live",
            serde_json::json!({
                "t": "tool_output_delta",
                "seq": seq,
                "id": id,
                "text": truncate_utf8(text, OUTPUT_TRUNCATE_BYTES),
            }),
        )),
        AgentEvent::UsageDelta {
            input_tokens,
            output_tokens,
        } => Some((
            "live",
            serde_json::json!({
                "t": "usage_delta",
                "seq": seq,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }),
        )),
        // ToolStarted/ToolCompleted 在 sink 入队路径由 extract_tool_milestones 处理；这里
        // 返回 None，保留原始 batch 走 live 队列时不会重复发送。M0 §2 目录里的
        // msg.completed / card.created / card.resolved / run.status / session.index 都必须在
        // 各自的真实咽喉上接线，因此这里同样返回 None。
        //
        // 其余变体（SessionStarted / Completed / RunCloseout / GoalDeclared / CriteriaUpdated /
        // GoalUpdated / NeedsDecision / ApprovalRequested / ApprovalResolved / Error / Blocked）目前
        // 不在 M0 §2 里程碑目录也不在 live 目录里——内部诊断/协议尚未覆盖，不上行。
        _ => None,
    }
}

fn tool_status_wire_str(status: &crate::agent_event::ToolStatus) -> &'static str {
    match status {
        crate::agent_event::ToolStatus::Ok => "ok",
        crate::agent_event::ToolStatus::Failed => "failed",
    }
}

fn remember_tool_name(state: &GatewayInnerState, run_id: &str, tool_id: &str, tool: &str) {
    let key = (run_id.to_owned(), tool_id.to_owned());
    let mut correlation = lock(&state.tool_correlation);
    if correlation.names.contains_key(&key) {
        correlation.names.insert(key, tool.to_owned());
        return;
    }
    if correlation.names.len() >= TOOL_CORRELATION_CAPACITY {
        // MEDIUM#6: evict the oldest orphan to admit the new key instead of permanently rejecting
        // all new correlations once the table fills.
        if let Some(oldest) = correlation.order.pop_front() {
            correlation.names.remove(&oldest);
            state
                .tool_correlation_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    correlation.order.push_back(key.clone());
    correlation.names.insert(key, tool.to_owned());
}

fn take_tool_name(state: &GatewayInnerState, run_id: &str, tool_id: &str) -> String {
    let key = (run_id.to_owned(), tool_id.to_owned());
    let mut correlation = lock(&state.tool_correlation);
    let name = correlation.names.remove(&key);
    if name.is_some() {
        if let Some(pos) = correlation.order.iter().position(|entry| entry == &key) {
            correlation.order.remove(pos);
        }
    }
    name.unwrap_or_default()
}

/// Run-terminal cleanup (MEDIUM#6): remove every orphaned correlation for this run when its
/// Completed or RunCloseout event arrives, even if no matching ToolCompleted was emitted.
fn purge_tool_correlation_for_run(state: &GatewayInnerState, run_id: &str) {
    let mut correlation = lock(&state.tool_correlation);
    correlation
        .names
        .retain(|(entry_run_id, _), _| entry_run_id != run_id);
    correlation
        .order
        .retain(|(entry_run_id, _)| entry_run_id != run_id);
}

fn derive_client_msg_id(name: &str) -> String {
    uuid::Uuid::new_v5(&CLIENT_MSG_ID_NAMESPACE, name.as_bytes()).to_string()
}

#[cfg(test)]
thread_local! {
    static FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// 测试专用：强制 `try_random_client_msg_id` 在本线程内返回 `None`，模拟 OS CSPRNG
/// (`getrandom`) 失败。RAII 守卫，drop 时自动复位，不可重入（复用仓内 `detect.rs` 的
/// `CliPathOverrideTestGuard` 同款风格）。
#[cfg(test)]
struct ForceClientMsgIdEntropyFailureGuard;

#[cfg(test)]
impl ForceClientMsgIdEntropyFailureGuard {
    fn new() -> Self {
        FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE.with(|flag| {
            assert!(
                !flag.replace(true),
                "entropy failure guard is not reentrant"
            );
        });
        Self
    }
}

#[cfg(test)]
impl Drop for ForceClientMsgIdEntropyFailureGuard {
    fn drop(&mut self) {
        FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE.with(|flag| flag.set(false));
    }
}

/// 不会 panic 地铸造一个随机 client_msg_id（M#5）。`uuid::Uuid::new_v4()` 在 OS CSPRNG
/// (`getrandom`) 失败时内部会 panic；这里所有的调用方都可能在持有 DB mutex 的情况下调用
/// （刚写完一行、马上要发对应的里程碑），panic unwind 会把那把锁毒化，波及进程里所有后续持锁
/// 者。改成自己拿 16 字节随机数（可失败）、再用 `Builder::from_random_bytes` 拼 UUID
/// （这一步本身不需要熵，只是在调用方给的字节上设置 version/variant 位），失败时返回 `None`
/// 让调用方退化成空字符串——`enqueue_milestone_for_upstream` 已有的
/// `is_valid_client_msg_id` 空值校验会把它当无效 id 丢弃、计入既有 `milestone_dropped`，
/// 和"channel 满"、"调用方给了坏 id"走的是同一条不阻塞、不重试、不 panic 的既有路径，不需要
/// 新计数器。
fn try_random_client_msg_id() -> Option<String> {
    #[cfg(test)]
    if FORCE_CLIENT_MSG_ID_ENTROPY_FAILURE.with(std::cell::Cell::get) {
        return None;
    }
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(
        uuid::Builder::from_random_bytes(bytes)
            .into_uuid()
            .to_string(),
    )
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// 在 sink 入队路径（enqueue 时，emitter 线程同步调用）识别 batch 中的
/// ToolStarted/ToolCompleted 事件，维护 `(run_id, tool_id)` 名字关联，并把 ToolCompleted
/// 转成里程碑优先级的 MilestoneItem 后 try_send 进 milestone_tx。它不再等待原始 batch
/// 随低优先级 live 通道被 drain，从结构上避免里程碑占满预算导致饿死、live 队列满时整批
/// 丢弃，以及 drain 预算在 batch 中途耗尽造成后半段 ToolCompleted 永久丢失（HIGH#2）。
/// 遇到该 run 的 Completed/RunCloseout 终态时，还会清理该 run 的残留关联（MEDIUM#6）。
///
/// 单次 tick 每条 lane 最多累积 `event_transport::LANE_CAPACITY`（512）个事件，batch 总量
/// 还受活跃 lane 数（近似同时运行的 run 数，实践中很小）约束。本函数只进行一次有界的
/// O(事件数) 遍历，每个 ToolCompleted 仅触发一次内部为 try_send 的里程碑入队；除字段上
/// 已论证安全的私有短 Mutex 外，不阻塞、不做 I/O，符合 add_sink 契约的实质要求。
///
/// 原始 batch（包括这两类事件）不做过滤，仍原样 try_send 到 upstream_tx。live drain 统一
/// 交给 classify，而 classify 对 ToolStarted/ToolCompleted 返回 None，因此不会重复上行。
fn extract_tool_milestones(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    payload: &crate::event_transport::BatchPayload,
) {
    use crate::agent_event::AgentEvent;

    for batch in &payload.batches {
        for sequenced in &batch.events {
            match &sequenced.event {
                AgentEvent::ToolStarted { id, tool, .. } => {
                    remember_tool_name(state, &batch.run_id, id, tool);
                }
                AgentEvent::ToolCompleted {
                    id,
                    status,
                    exit_code,
                    output,
                } => {
                    let tool = take_tool_name(state, &batch.run_id, id);
                    let client_msg_id =
                        derive_client_msg_id(&format!("tool.completed|{}|{}", batch.run_id, id));
                    enqueue_milestone_for_upstream(
                        state,
                        milestone_tx,
                        MilestoneItem {
                            session: Some(batch.session_id.clone()),
                            t: "tool.completed".to_owned(),
                            payload: serde_json::json!({
                                "id": id,
                                "tool": tool,
                                "status": tool_status_wire_str(status),
                                "exit_code": exit_code,
                                "output": output
                                    .as_deref()
                                    .map(|value| truncate_utf8(value, OUTPUT_TRUNCATE_BYTES)),
                            }),
                            client_msg_id,
                        },
                    );
                }
                AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. } => {
                    purge_tool_correlation_for_run(state, &batch.run_id);
                }
                _ => {}
            }
        }
    }
}

/// P0-b：维护每 session 的"当前 run 归约态"（`partial_snapshots`），供 `control.snapshot`
/// 臂原子读取。与 `extract_tool_milestones` 同层——同样在 sink 入队路径（emitter 线程，
/// `enqueue_batch_payload_for_upstream`）同步调用，同样只做有界纯内存工作（见
/// `GatewayInnerState::partial_snapshots` 字段文档的豁免论证）；关注点不同（这里只累积归约
/// 状态，不产生里程碑）所以拆成独立函数，不塞进 `extract_tool_milestones` 内部。
///
/// 每个 `SequencedEvent`：条目不存在，或已存在但 `run_id` 与本 batch 不同 → 以 batch 的
/// `run_id` 重建条目（旧归约态整个丢弃，新起一个 `DisplayReducer`）；随后 `reducer.feed`
/// 该事件、`last_seq` 记为该事件的 `seq`（sink 看到的是 coalesce 后的 seq，天然就是"已交给
/// 远端下行管线的最后一条 live seq"，正是水印语义）。批内任一事件是 `Completed`/
/// `RunCloseout` 终态 → 整条 batch 处理完后清掉该 session 的条目，`control.snapshot` 据此
/// 自动回退到 idle 三 null（不需要额外的"是否 idle"判断）。
///
/// **team 多 lane 局限（如实记档，非本轮设计范围）**：team 会话的多个 member lane 可能对
/// 同一 `session_id` 交错发来不同 `run_id` 的 batch；本函数按"run_id 变化即重建"策略处理，
/// 交错到达时**重建会丢弃已积累的归约态**——跨 lane 的 seq 不可比，客户端可能收到水位更高
/// 但内容更少的快照并覆盖本地已有状态；一期契约只保证 solo / lead 主线程会话的快照准确，
/// team member lane 精确快照留 BACKLOG。
///
/// **调用时机（P0-b 返工①）**：本函数在 `enqueue_batch_payload_for_upstream` 里排在 gate
/// 判断之前，无条件执行——桌面断连（gate 关闭）期间事件仍要喂 reducer，否则重连后
/// `control.snapshot` 会读到假 idle/陈旧态（同时是"断连窗内条目永久泄漏"与"水位打洞"两个
/// 同族问题的根修）。gate 只决定"是否入上行队列"（`extract_tool_milestones` 与
/// `upstream_tx.try_send` 那一段），不影响归约态维护。
fn maintain_partial_snapshots(
    state: &GatewayInnerState,
    payload: &crate::event_transport::BatchPayload,
) {
    use crate::agent_event::AgentEvent;

    if payload.batches.is_empty() {
        return;
    }
    let mut snapshots = lock(&state.partial_snapshots);
    for batch in &payload.batches {
        if batch.events.is_empty() {
            continue;
        }
        let needs_rebuild = snapshots
            .get(&batch.session_id)
            .map(|existing| existing.run_id != batch.run_id)
            .unwrap_or(true);
        if needs_rebuild {
            if !snapshots.contains_key(&batch.session_id)
                && snapshots.len() >= PARTIAL_SNAPSHOT_CAPACITY
            {
                // P0-b 返工⑥c 如实补记：被拒 session 的 snapshot 回 idle 三 null——有界降级，
                // 不是数据损坏；且修复项①（gate 前无条件维护）落地后，断连不再让条目滞留到
                // 超过 liveness 判死的窗口，128 并发 running session 实际不可达。
                state
                    .partial_snapshot_capacity_dropped
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            snapshots.insert(
                batch.session_id.clone(),
                PartialSnapshotState {
                    run_id: batch.run_id.clone(),
                    last_seq: 0,
                    reducer: crate::display_reduce::DisplayReducer::new(&batch.run_id),
                },
            );
        }
        let Some(entry) = snapshots.get_mut(&batch.session_id) else {
            continue;
        };
        let mut terminal = false;
        for sequenced in &batch.events {
            entry.reducer.feed(&sequenced.event);
            entry.last_seq = sequenced.seq;
            if matches!(
                sequenced.event,
                AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. }
            ) {
                terminal = true;
            }
        }
        if terminal {
            snapshots.remove(&batch.session_id);
        }
    }
}

fn enqueue_batch_payload_for_upstream(
    state: &GatewayInnerState,
    upstream_tx: &SyncSender<(u64, LiveQueueItem)>,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    payload: crate::event_transport::BatchPayload,
) {
    // P0-b 返工①【阻断修复】：归约态维护必须排在 gate 判断之前、无条件执行——理由与不变量见
    // `maintain_partial_snapshots` 文档顶部"调用时机"一段。gate 只管下面"是否入上行队列"。
    maintain_partial_snapshots(state, &payload);

    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return;
    }
    let generation = snapshot >> 1;

    extract_tool_milestones(state, milestone_tx, &payload);

    match upstream_tx.try_send((generation, LiveQueueItem::Batch(payload))) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// `control.history` 使用的预构建 live 入队口。与 delta 共用同一有界 FIFO、generation
/// 标签和 drain 归属闸；只绕过 classify，因为 payload 已由 history 契约构造函数定型。
fn enqueue_prebuilt_live_for_upstream(
    state: &GatewayInnerState,
    upstream_tx: &SyncSender<(u64, LiveQueueItem)>,
    item: MilestoneItem,
) {
    if !is_valid_client_msg_id(&item.client_msg_id) {
        state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return;
    }
    let generation = snapshot >> 1;
    match upstream_tx.try_send((generation, LiveQueueItem::Prebuilt(item))) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 真正执行入队：校验 client_msg_id、`try_send`、计数满/断连丢弃。不做任何门控读取——调用方
/// 必须已经自己决定好"现在要不要发"。两条上层路径（"当前快照"语义的
/// `enqueue_milestone_for_upstream` 与"捕获 generation、独立读门控"语义的
/// `enqueue_milestone_with_generation`）各自负责按自己的正确性论证决定门控，这里只管落地。
fn enqueue_milestone_item(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    generation: u64,
    mut item: MilestoneItem,
) {
    if !is_valid_client_msg_id(&item.client_msg_id) {
        state.milestone_dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if item.t == "msg.completed" {
        if let Some(blocks) = item.payload.get_mut("blocks") {
            *blocks = truncate_history_tool_outputs(blocks.clone());
        }
        if milestone_frame_bytes(&item.t, &item.payload) > SNAPSHOT_SEND_BUDGET_BYTES {
            state
                .replay_oversized_dropped
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    match milestone_tx.try_send((generation, item)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.milestone_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 用调用方显式给定的 `generation` 打标入队，而不是重读"当前" generation——这正是 M#4/M#5
/// 复审定罪的 TOCTOU 修复点。凡是"捕获 generation 的时刻"和"真正入队的时刻"之间可能被任意
/// 时长抢占的调用方（目前只有 session-index 快照的后台 worker，`publish_session_index_snapshot_on_connect`
/// 那条路径，provider 要做 DB 读 + JSON 序列化，耗时不可控）都必须走这条路径。正确性只依赖
/// 这里：下游既有的陈旧过滤器（`drain_milestone_queue`/`drain_live_queue` 里
/// `item_generation != connection_generation`）会用这里打的标签自然识别、丢弃被顶替连接产生
/// 的陈旧条目——不需要在入队前另外核对一次"当前" generation 是否还等于调用方捕获时的那个值。
/// 是否仍处于 upstream 开启状态，只看这次调用瞬间"当前"的门控位——门控位决定"现在要不要发"，
/// generation 标签决定"这条目该算哪一轮连接的"，这是两件独立的事，即使两者不是从同一次
/// atomic load 里取的也不影响正确性。注意这条独立读论证只适用于本函数这条显式 generation
/// 路径；`enqueue_milestone_for_upstream` 的"当前快照"语义必须用一次打包读同时取得两者。
fn enqueue_milestone_with_generation(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    generation: u64,
    item: MilestoneItem,
) {
    if !state.upstream_enabled_snapshot() {
        return;
    }
    enqueue_milestone_item(state, milestone_tx, generation, item);
}

/// 大多数调用方不涉及"捕获与入队之间存在被抢占窗口"的场景，用这个薄壳：门控位与
/// generation 必须出自同一次 `upstream_state.load()`（M#2 复审定罪修复——此前拆成两次独立读，
/// 存在"第一次读到 generation=A、gate 位在两次读之间被别的连接推进成 true"的窗口，会把打着
/// 陈旧 A 标签的条目当"当前有效"入队；单次打包读让这两个值必然来自同一个原子快照，gate 关
/// 就直接整体 no-op，不会出现这种撕裂）。这条约束无法仅靠普通的单线程黑盒调用暴露旧窗口，
/// 因此回归测试除行为覆盖外还锁定此函数只能读取一次 packed state；两次独立读取的变异必须红。
fn enqueue_milestone_for_upstream(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    item: MilestoneItem,
) {
    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return;
    }
    let generation = snapshot >> 1;
    enqueue_milestone_item(state, milestone_tx, generation, item);
}

fn is_valid_client_msg_id(client_msg_id: &str) -> bool {
    !client_msg_id.is_empty() && client_msg_id.len() <= 64 && !client_msg_id.contains('|')
}

/// Publishes a durable remote milestone without blocking.
///
/// This function is safe to call while a database transaction lock is held: before gateway
/// setup or while upstream is disabled it is a no-op; otherwise it performs one atomic snapshot
/// and one bounded-channel `try_send`. A full/disconnected channel or invalid client message ID
/// is counted and dropped, never retried, blocked, or panicked here.
pub(crate) fn publish_milestone(
    session: Option<&str>,
    t: &str,
    payload: Value,
    client_msg_id: String,
) {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    enqueue_milestone_for_upstream(
        &inner.state,
        &inner.milestone_tx,
        MilestoneItem {
            session: session.map(str::to_owned),
            t: t.to_owned(),
            payload,
            client_msg_id,
        },
    );
}

#[cfg(test)]
thread_local! {
    static TEST_PUBLISH_LOG: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static TEST_RUN_STATUS_PAYLOAD_LOG: std::cell::RefCell<Vec<Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static TEST_SESSION_INDEX_ARCHIVED_PAYLOAD_LOG: std::cell::RefCell<Vec<Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static TEST_SESSION_INDEX_CREATED_PAYLOAD_LOG: std::cell::RefCell<Vec<Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn record_test_publish(t: &'static str) {
    TEST_PUBLISH_LOG.with(|log| log.borrow_mut().push(t));
}

#[cfg(not(test))]
fn record_test_publish(_t: &'static str) {}

#[cfg(test)]
pub(crate) fn test_take_publish_log() -> Vec<&'static str> {
    TEST_PUBLISH_LOG.with(|log| log.borrow_mut().drain(..).collect())
}

#[cfg(test)]
pub(crate) fn test_take_run_status_payload_log() -> Vec<Value> {
    TEST_RUN_STATUS_PAYLOAD_LOG.with(|log| log.borrow_mut().drain(..).collect())
}

#[cfg(test)]
pub(crate) fn test_take_session_index_archived_payload_log() -> Vec<Value> {
    TEST_SESSION_INDEX_ARCHIVED_PAYLOAD_LOG.with(|log| log.borrow_mut().drain(..).collect())
}

#[cfg(test)]
pub(crate) fn test_take_session_index_created_payload_log() -> Vec<Value> {
    TEST_SESSION_INDEX_CREATED_PAYLOAD_LOG.with(|log| log.borrow_mut().drain(..).collect())
}

/// `agent` = 落库时的 `agent_name_snapshot`（如 `"Claude"`/`"Codex"`）——`Some` 时插入
/// optional `"agent"` 键，`None` 时整个键省略（不是 `null`），保持老消费方（无该键时按老形状
/// 解析）向后兼容。user 消息 / 无 agent 归属的场景走 `None`。
pub(crate) fn build_msg_completed_payload(
    message_id: i64,
    role: &str,
    blocks: Value,
    agent: Option<&str>,
) -> Value {
    let mut payload = serde_json::json!({
        "message_id": message_id,
        "role": role,
        "blocks": blocks,
    });
    if let Some(agent) = agent {
        payload["agent"] = Value::String(agent.to_owned());
    }
    payload
}

pub(crate) fn derive_msg_completed_client_msg_id(session_id: &str, dedup_key: &str) -> String {
    derive_client_msg_id(&format!("msg.completed|{session_id}|{dedup_key}"))
}

/// idlefix-T1 缺口②：连接后补发批里的 `run.status` 现状帧专用——与首发（live，
/// `publish_run_status_milestone` 走 `try_random_client_msg_id`）区分开：补发是"重放当前已知
/// 状态"，同一状态/run_id 组合确定性推导同一个 client_msg_id，避免每次重连补发都造出不同的
/// client_msg_id（同 msg.completed/card.* 补发的确定性推导惯例）。
pub(crate) fn derive_run_status_replay_client_msg_id(
    session_id: &str,
    status: &str,
    run_id: Option<&str>,
) -> String {
    derive_client_msg_id(&format!(
        "run.status|{session_id}|{status}|{}",
        run_id.unwrap_or("")
    ))
}

pub(crate) fn publish_msg_completed_milestone(
    session_id: &str,
    dedup_key: &str,
    message_id: i64,
    role: &str,
    blocks: Value,
    agent: Option<&str>,
) {
    record_test_publish("msg.completed");
    let client_msg_id = derive_msg_completed_client_msg_id(session_id, dedup_key);
    let payload = build_msg_completed_payload(message_id, role, blocks, agent);
    publish_milestone(Some(session_id), "msg.completed", payload, client_msg_id);
}

pub(crate) fn build_card_created_payload(block: Value) -> Value {
    serde_json::json!({ "block": block })
}

pub(crate) fn derive_card_created_client_msg_id(decision_id: &str) -> String {
    derive_client_msg_id(&format!("card.created|{decision_id}"))
}

pub(crate) fn publish_card_created_milestone(session_id: &str, decision_id: &str, block: Value) {
    record_test_publish("card.created");
    let client_msg_id = derive_card_created_client_msg_id(decision_id);
    let payload = build_card_created_payload(block);
    publish_milestone(Some(session_id), "card.created", payload, client_msg_id);
}

pub(crate) fn build_card_resolved_payload(
    decision_id: &str,
    status: &str,
    chosen_option: Option<&str>,
) -> Value {
    serde_json::json!({
        "decision_id": decision_id,
        "status": status,
        "chosen_option": chosen_option,
    })
}

pub(crate) fn derive_card_resolved_client_msg_id(decision_id: &str, next_status: &str) -> String {
    derive_client_msg_id(&format!("card.resolved|{decision_id}|{next_status}"))
}

pub(crate) fn publish_card_resolved_milestone(
    session_id: &str,
    decision_id: &str,
    next_status: &str,
    chosen_option: Option<&str>,
) {
    record_test_publish("card.resolved");
    let client_msg_id = derive_card_resolved_client_msg_id(decision_id, next_status);
    let payload = build_card_resolved_payload(decision_id, next_status, chosen_option);
    publish_milestone(Some(session_id), "card.resolved", payload, client_msg_id);
}

pub(crate) fn build_run_status_payload(
    session_id: &str,
    status: &str,
    run_id: Option<&str>,
) -> Value {
    serde_json::json!({
        "session_id": session_id,
        "status": status,
        "run_id": run_id,
    })
}

/// idlefix-T1 补针 C（TOCTOU）：`publish_run_status_milestone`（真实运行时经全局 `GATEWAY` 调用
/// 的实时入队路径）真正落地的入队逻辑拆到这里、显式接收 `&Inner`——单测里 `GATEWAY` 故意不装
/// （见 `entropy_failure_does_not_panic_in_random_id_publish_facades` 头注），若把入队逻辑锁在
/// `GATEWAY.get()` 后面，测试就没有任何办法绕开全局单例去验证锁的互斥语义。持有
/// `inner.state.run_status_replay_gate` 期间入队——与 `publish_run_status_replay_rows`（补发批
/// 读 DB + 入队）共享同一把锁，维持"补发帧必须先于其后任何实时帧入队"的顺序不变量。
fn enqueue_run_status_milestone_with_gate(
    inner: &Inner,
    session_id: &str,
    payload: Value,
    client_msg_id: String,
) {
    let _replay_gate = lock(&inner.state.run_status_replay_gate);
    enqueue_milestone_for_upstream(
        &inner.state,
        &inner.milestone_tx,
        MilestoneItem {
            session: Some(session_id.to_owned()),
            t: "run.status".to_owned(),
            payload,
            client_msg_id,
        },
    );
}

pub(crate) fn publish_run_status_milestone(session_id: &str, status: &str, run_id: Option<&str>) {
    record_test_publish("run.status");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_run_status_payload(session_id, status, run_id);
    #[cfg(test)]
    TEST_RUN_STATUS_PAYLOAD_LOG.with(|log| log.borrow_mut().push(payload.clone()));
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    enqueue_run_status_milestone_with_gate(inner, session_id, payload, client_msg_id);
}

/// `repo_name`：手机端会话列表副标题要的人类可读项目名（M2-4x）——None 时序列化成
/// `"repo_name": null`（键恒在，值可空），不是整键省略；旧手机端 parseFrame 按未知/可选字段
/// 处理，新手机端拿到 null 时回退渲染裸 `repo_id`（同 `SessionIndexRow.repo_name` 的既有
/// optional 惯例）。
pub(crate) fn build_session_index_created_payload(
    id: &str,
    title: &str,
    repo_id: &str,
    namespace_id: &str,
    repo_name: Option<&str>,
) -> Value {
    serde_json::json!({
        "op": "created",
        "full": false,
        "session": {
            "id": id,
            "title": title,
            "repo_id": repo_id,
            "namespace_id": namespace_id,
            "archived": false,
            "repo_name": repo_name,
        },
    })
}

pub(crate) fn publish_session_index_created(
    id: &str,
    title: &str,
    repo_id: &str,
    namespace_id: &str,
    repo_name: Option<&str>,
) {
    record_test_publish("session.index.created");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_created_payload(id, title, repo_id, namespace_id, repo_name);
    #[cfg(test)]
    TEST_SESSION_INDEX_CREATED_PAYLOAD_LOG.with(|log| log.borrow_mut().push(payload.clone()));
    publish_milestone(None, "session.index", payload, client_msg_id);
}

pub(crate) fn build_session_index_renamed_payload(id: &str, title: &str) -> Value {
    serde_json::json!({ "op": "renamed", "full": false, "id": id, "title": title })
}

pub(crate) fn publish_session_index_renamed(id: &str, title: &str) {
    record_test_publish("session.index.renamed");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_renamed_payload(id, title);
    publish_milestone(None, "session.index", payload, client_msg_id);
}

pub(crate) fn build_session_index_deleted_payload(id: &str) -> Value {
    serde_json::json!({ "op": "deleted", "full": false, "id": id })
}

pub(crate) fn publish_session_index_deleted(id: &str) {
    record_test_publish("session.index.deleted");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_deleted_payload(id);
    publish_milestone(None, "session.index", payload, client_msg_id);
}

pub(crate) fn build_session_index_archived_payload(ids: &[String], archived: bool) -> Value {
    serde_json::json!({
        "op": if archived { "archived" } else { "unarchived" },
        "full": false,
        "ids": ids,
    })
}

pub(crate) fn publish_session_index_archived(ids: &[String], archived: bool) {
    record_test_publish(if archived {
        "session.index.archived"
    } else {
        "session.index.unarchived"
    });
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_archived_payload(ids, archived);
    #[cfg(test)]
    TEST_SESSION_INDEX_ARCHIVED_PAYLOAD_LOG.with(|log| log.borrow_mut().push(payload.clone()));
    publish_milestone(None, "session.index", payload, client_msg_id);
}

/// `repo`：全量快照顶层的"当前被远程的项目"摘要（M2-4x）——调用方传 `Value::Null`（没有可
/// 判定的 active repo，理论不可达但 fail-closed）或 `json!({"id":.., "name":..})`（`name` 本身
/// 也可能是 `null`——active repo 已知但拿不到名字，见 `active_repo_summary_for_snapshot`）。
/// 键恒在（不是可选省略），手机端 `repo?: {...} | null` 按 optional 解析，兼容旧桌面不带这个
/// 键的帧。
pub(crate) fn build_session_index_snapshot_payload(sessions: Value, repo: Value) -> Value {
    serde_json::json!({ "full": true, "sessions": sessions, "repo": repo })
}

/// sink 回调契约（`EventTransport::add_sink` doc）：只许非阻塞入队，禁止 send()/网络 I/O/
/// EventTransport 内部锁/panic。这里除 Atomic 读和有界 try_send 外，新增的提取步骤只拿
/// 上述已单独论证过、无锁序交叉的私有短 Mutex，并只做有界纯内存工作，仍无阻塞路径。
pub(crate) fn install_event_sink(transport: &crate::event_transport::EventTransport) {
    transport.add_sink(move |payload: crate::event_transport::BatchPayload| {
        let Some(inner) = GATEWAY.get() else {
            return;
        };
        enqueue_batch_payload_for_upstream(
            &inner.state,
            &inner.upstream_tx,
            &inner.milestone_tx,
            payload,
        );
    });
}

fn drain_close(socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>) {
    for _ in 0..CLOSE_DRAIN_ATTEMPTS {
        match socket.read() {
            Err(WebSocketError::ConnectionClosed) => return,
            Err(WebSocketError::Io(error)) if is_read_timeout(&error) => continue,
            Err(_) => return,
            Ok(_) => continue,
        }
    }
}

fn set_read_timeout(
    stream: &MaybeTlsStream<TcpStream>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported TLS stream for read timeout",
        )),
    }
}

fn set_write_timeout(
    stream: &MaybeTlsStream<TcpStream>,
    timeout: Option<Duration>,
) -> io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_write_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_write_timeout(timeout),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported TLS stream for write timeout",
        )),
    }
}

fn is_read_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn handle_frame(inner: &Inner, raw: &str, k_room: Option<&Zeroizing<[u8; 32]>>) -> Option<Value> {
    let state = &inner.state;
    state.frames_seen.fetch_add(1, Ordering::Relaxed);
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        return None;
    };

    if matches!(
        value.get("kind").and_then(Value::as_str),
        Some("input" | "control")
    ) {
        return handle_command_envelope(inner, &value, k_room);
    }

    match value.get("t").and_then(Value::as_str) {
        Some("pair.hello") => {
            let Some(frame) = parse_pair_hello(&value) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(accept) = (inner.pair_hello_handler)(frame) else {
                // Authentication failures and pair.hello frames received outside Waiting are
                // protocol-invalid for the current desktop state, so reuse bad_frames.
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            lock(&inner.registry).stage_pairing_k_room(accept.k_room.clone());
            Some(pair_accept_json(accept))
        }
        Some("pair.done") => {
            let Some(frame) = parse_pair_done(&value) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            match (inner.pair_done_handler)(frame) {
                PairDoneAction::Rejected => {
                    state.bad_frames.fetch_add(1, Ordering::Relaxed);
                    None
                }
                PairDoneAction::Accepted { .. } => None,
                PairDoneAction::Ready(ready) => Some(pair_ready_json(ready)),
            }
        }
        Some("token.ack") => {
            let Some(subject) = value.get("subject").and_then(Value::as_str) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(generation) = value
                .get("generation")
                .and_then(Value::as_i64)
                .filter(|generation| *generation > 0)
            else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(result) = value
                .get("result")
                .and_then(Value::as_str)
                .filter(|result| matches!(*result, "ok" | "idempotent" | "rejected"))
            else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let action = lock(&inner.registry).consume_token_ack(subject, generation, result);
            match action {
                TokenAckAction::PairReady(ready) => Some(pair_ready_json(ready)),
                // S1i1 §2b：轮换的 token.put 收到 ack——回执现在才能发（put→ack→回执固定顺序）。
                TokenAckAction::RefreshOk(refresh_ok) => Some(refresh_ok_json(
                    &refresh_ok.request_id,
                    &refresh_ok.subject,
                    refresh_ok.generation,
                    &refresh_ok.ct,
                    &refresh_ok.n,
                )),
                TokenAckAction::Rejected => {
                    let reason =
                        registry_ack_reason_for_log(value.get("reason").and_then(Value::as_str));
                    eprintln!(
                        "remote registry token.ack rejected: subject={subject}, generation={generation}, reason={reason}"
                    );
                    None
                }
                // S1i1 R2 返工：put 被拒且挂着 refresh 回执——立即自愈回一帧 fail，不带 close
                // （良性：桌面↔relay 之间的问题，不是设备的无效请求），也不经过
                // refresh_fail_reply/record_refresh_invalid，不计入连续无效计数。
                TokenAckAction::RefreshDropped {
                    request_id,
                    subject,
                } => Some(refresh_fail_json(
                    &request_id,
                    &subject,
                    "put_rejected",
                    false,
                )),
                TokenAckAction::Ignored | TokenAckAction::Consumed => None,
            }
        }
        Some("token.refresh.forward") => {
            let Some(frame) = parse_refresh_forward(&value) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            match (inner.refresh_handler)(frame) {
                RefreshOutcome::Reply(value) => Some(value),
                RefreshOutcome::Pending => None,
            }
        }
        Some("replay.head") => {
            let Some(epoch) = value.get("epoch").and_then(Value::as_u64) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Some(head_seq) = value.get("headSeq").and_then(Value::as_u64) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            state.epoch.store(epoch, Ordering::Release);
            state.head_seq.store(head_seq, Ordering::Release);
            None
        }
        Some("error") if value.get("reason").and_then(Value::as_str) == Some("stale_epoch") => {
            let Some(epoch) = value.get("currentEpoch").and_then(Value::as_u64) else {
                state.bad_frames.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            state.epoch.store(epoch, Ordering::Release);
            None
        }
        _ => None,
    }
}

/// P0-b：构造 `control.snapshot` 应答的裸 payload（不含 `t`——跟 `build_run_status_payload`
/// 等既有 builder 同款约定，`t` 由 `milestone_payload` 在 drain 时合并进去）。M0 §3
/// v1.8.12 的形状不变量在此单点收敛：有进行中 run ⟺ `run_id` 非 null ⟺ `through_run_seq`
/// 非 null 且 **≥ 1**（`through_run_seq = 0` 这个取值自 v1.8.12 废除——生产序号器先自增后
/// 返回，首条事件即 seq=1；首事件到达前桌面没有归约条目，落 idle 分支）；`blocks` 为空 →
/// `partial_msg` 为 null；调用方对"idle 无条目"传 `(None, &[])`，三字段自然全 null。
///
/// **builder 类型收敛（P0-b 返工④）**：`run` 参数把原先的 `(Option<String>, Option<u64>)`
/// 两个独立 `Option` 收成一个 `Option<(String, u64)>`——run 对儿要么整体在要么整体无，
/// `run_id=Some/through=None` 这种非法组合在这个签名下编译期就不可表示。
///
/// **尺寸预算（P0-b 返工②·v1.8.12 ③·微返工第 3 轮改写）**：`blocks` 是 run 进行中的原始
/// 归约累积（未经 `DisplayReducer::finish` 收尾截断），一条大工具输出或长叙述都可能撞 relay
/// 帧 ≤64KB 硬闸（1009 踢断桌面 + 客户端重请求 = 断连环）。收敛两步，见
/// `shrink_snapshot_blocks_to_budget`：① 工具卡 output 逐块截到 `OUTPUT_TRUNCATE_BYTES`
/// （与远端面 tool.completed/live 出口同口径）；② 收敛后整帧序列化（**计量对象含 `t` 字段**，
/// 与 `milestone_payload` 随后合并进去的一致）仍超 `SNAPSHOT_PAYLOAD_BUDGET_BYTES` →
/// 截断提示 text block **先计入预算**、再从尾向前单次遍历累计确定业务块能保留的最长后缀
/// （业务块允许丢尽，最坏成品只剩提示块 + 水印字段）。绝不允许把超帧交给 relay 裁决——发送
/// 侧兜底见 snapshot 臂调用点（`SNAPSHOT_SEND_BUDGET_BYTES`）。
fn build_snapshot_payload(
    session: &str,
    run: Option<(&str, u64)>,
    blocks: &[crate::db::Block],
) -> Value {
    let (run_id, through_run_seq) = match run {
        Some((run_id, through_run_seq)) => (Some(run_id), Some(through_run_seq)),
        None => (None, None),
    };
    let partial_msg = if blocks.is_empty() {
        Value::Null
    } else {
        let bounded = shrink_snapshot_blocks_to_budget(session, run_id, through_run_seq, blocks);
        serde_json::json!({ "role": "assistant", "blocks": bounded })
    };
    serde_json::json!({
        "session": session,
        "run_id": run_id,
        "through_run_seq": through_run_seq,
        "partial_msg": partial_msg,
    })
}

/// P0-b 微返工第 4 轮：整帧尺寸计量的唯一实现——`payload` 是不含 `t` 的裸 snapshot payload
/// （`session`/`run_id`/`through_run_seq`/`partial_msg` 四字段的 Object），内部克隆一份、
/// 插入 `milestone_payload` 随后会合并进去的 `"t":"snapshot"` 字段再量序列化长度（不改动
/// 调用方持有的原值）。两处调用者共用同一把尺子：① `shrink_snapshot_blocks_to_budget`
/// 试探不同 blocks 后缀时反复调用（原微返工第 3 轮的 `frame_bytes` 闭包收敛到此）；
/// ② `control.snapshot` 臂的发送侧兜底（`SNAPSHOT_SEND_BUDGET_BYTES`）量最终成品——旧实现
/// 在②处直接量未合并 `t` 的裸 payload，与①处含 `t` 的计量口径分裂，一个真实逼近阈值的边界
/// 帧可能被①判定"已收敛在预算内"、却被②用更小的裸尺寸误判"未超 44KiB"而照发。
fn snapshot_frame_bytes(payload: &Value) -> usize {
    milestone_frame_bytes("snapshot", payload)
}

/// P0-b 返工②·微返工第 3 轮重写：`build_snapshot_payload` 的收敛实体——见该函数文档"尺寸
/// 预算"一段的顺序论证。`session`/`run_id`/`through_run_seq` 只用于重算试探帧的序列化尺寸
/// （跟真正发出去的信封结构一致，不是只测 `blocks` 数组本身），不参与截断判断本身。
///
/// **计量对象含 `t`**：`frame_bytes` 内部直接把 `milestone_payload` 随后会合并进去的
/// `"t":"snapshot"` 字段一并算进去——否则预算判断用的是裸 payload，成品还要再被 `t` 字段
/// 撑大一截，单个 32KiB text 块这类边界情形会被判定"在预算内"但实际成品超预算。
///
/// **允许丢尽 + 单次尾向前累计**：截断提示块先按"仅提示块"的整帧尺寸占用预算基线，剩余
/// 预算用来从最新（数组尾部）往最老单次遍历累计每块的边际字节数（序列化长度 + 1 个数组分
/// 隔逗号），一旦某块放不下就停——保留的是能装下的最长后缀，不再对整帧反复重序列化（消
/// 原实现"每丢一块就整帧重序列化一次"的 O(n²)）。业务块允许被丢尽：预算连一个块都装不下
/// 时，成品退化为只有提示块 + 水印字段。
///
/// P0-b 微返工第 4 轮：整帧尺寸计量收敛到 `snapshot_frame_bytes`——`control.snapshot` 臂
/// 发送侧兜底（`SNAPSHOT_SEND_BUDGET_BYTES`）量最终成品也调用同一个函数，两处口径统一
/// （见该函数文档）。
fn shrink_snapshot_blocks_to_budget(
    session: &str,
    run_id: Option<&str>,
    through_run_seq: Option<u64>,
    blocks: &[crate::db::Block],
) -> Vec<crate::db::Block> {
    let bounded: Vec<crate::db::Block> = blocks
        .iter()
        .cloned()
        .map(truncate_snapshot_tool_output)
        .collect();

    let frame_bytes = |bounded: &[crate::db::Block]| -> usize {
        let payload = serde_json::json!({
            "session": session,
            "run_id": run_id,
            "through_run_seq": through_run_seq,
            "partial_msg": { "role": "assistant", "blocks": bounded },
        });
        snapshot_frame_bytes(&payload)
    };

    if frame_bytes(&bounded) <= SNAPSHOT_PAYLOAD_BUDGET_BYTES {
        return bounded;
    }

    let notice = crate::db::Block::Text {
        text: SNAPSHOT_TRUNCATED_NOTICE.to_owned(),
    };
    // 提示块先计入预算：以"仅提示块"的整帧尺寸做基线，而不是在丢块循环之后才把它加进去
    // （旧实现的 bug (b)——那样插入的提示块本身完全不受预算约束）。
    //
    // P0-b 微返工第 4 轮如实论证：下面这行算出的 `base_bytes` 在当前输入契约下不可能超过
    // `SNAPSHOT_PAYLOAD_BUDGET_BYTES`（32,768B）——`if base_bytes <= ...` 这条分支保留作
    // 纵深防御，不代表判定它会被触发。论证四要素：
    // · `session` ≤128 字节——`handle_command_envelope` 的 control.snapshot 臂已经用
    //   `SESSION_ID_MAX_BYTES` 挡住超长值，走不到这里；
    // · `run_id` 是内部生成的固定格式短 id（`new_run_id()` → `run-{16 hex}-{8 hex}-{8 hex}`，
    //   恒 38 字节），不是外部输入拼接进来的；
    // · `through_run_seq` 是 `u64`，十进制最多 20 位；
    // · 提示文案（`SNAPSHOT_TRUNCATED_NOTICE`）是固定短字符串，序列化成 `Block::Text` 后
    //   67 字节。
    // 把这四项连同 JSON 结构开销（字段名、引号、大括号/逗号、合并进去的 `"t":"snapshot"`）
    // 实测拼出的"仅提示块"成品是 360 字节（128 字节 ASCII session + `u64::MAX` 的 20 位
    // 水位 + 38 字节 run_id）；即便 session 里全是需要 JSON 转义的字符把它翻倍，也只是
    // 几百字节量级——比 32,768B 预算低接近两个数量级，故这条分支不可达。仍原样保留：未来
    // 任何一项假设被打破（例如 run_id 生成规则改成拼接外部字符串），它会自动接住，不会让
    // 超帧静默溜出去。
    let base_bytes = frame_bytes(std::slice::from_ref(&notice));
    let mut budget_left = SNAPSHOT_PAYLOAD_BUDGET_BYTES.saturating_sub(base_bytes);

    // 从尾（最新）向前单次遍历累计，确定能保留的最长后缀；业务块允许丢尽（bug (a) 修复——
    // 旧实现的 `bounded.len() > 1` 循环守卫永远保留最后一块，哪怕它自己就超预算）。
    let mut kept_from = bounded.len();
    if base_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES {
        for (idx, block) in bounded.iter().enumerate().rev() {
            let marginal = serde_json::to_string(block)
                .map(|json| json.len() + 1) // +1：这块与前一个元素之间的数组分隔逗号
                .unwrap_or(usize::MAX);
            if marginal > budget_left {
                break;
            }
            budget_left -= marginal;
            kept_from = idx;
        }
    }

    let mut result = Vec::with_capacity(1 + bounded.len().saturating_sub(kept_from));
    result.push(notice);
    result.extend(bounded.into_iter().skip(kept_from));
    result
}

/// P0-b 返工②(a)：工具卡 output 逐块截到 `OUTPUT_TRUNCATE_BYTES`——与远端面
/// tool.completed/live 出口同口径（`extract_tool_milestones`/`classify` 都用同一个常量）。
/// 其余块型无自由长文本字段需要在这一层收敛（`Text`/`Thinking` 交给整帧尺寸预算的丢块处理）。
fn truncate_snapshot_tool_output(mut block: crate::db::Block) -> crate::db::Block {
    if let crate::db::Block::Tool {
        output: Some(output),
        ..
    } = &mut block
    {
        *output = truncate_utf8(output, OUTPUT_TRUNCATE_BYTES);
    }
    block
}

#[derive(Debug, PartialEq)]
struct HistoryPage {
    payload: Value,
    oversized_dropped: u64,
    next_scan_before: Option<i64>,
}

fn truncate_history_tool_outputs(mut blocks: Value) -> Value {
    let Some(items) = blocks.as_array_mut() else {
        return blocks;
    };
    for block in items {
        let Some(fields) = block.as_object_mut() else {
            continue;
        };
        if fields.get("type").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(output) = fields.get_mut("output") else {
            continue;
        };
        if let Some(text) = output.as_str() {
            *output = Value::String(truncate_utf8(text, OUTPUT_TRUNCATE_BYTES));
        }
    }
    blocks
}

fn history_payload(
    session: &str,
    before_message_id: Option<i64>,
    messages: &[Value],
    next_before: Option<i64>,
) -> Value {
    serde_json::json!({
        "t": "history",
        "session": session,
        "before_message_id": before_message_id,
        "messages": messages,
        "next_before": next_before,
    })
}

fn history_message(row: &SessionHistoryRow) -> Value {
    serde_json::json!({
        "message_id": row.message_id,
        "role": row.role,
        "blocks": truncate_history_tool_outputs(row.content_json.clone()),
    })
}

/// DB 行按最新→最旧输入。先把每条工具 output 收敛到既有上限，再从最新向前装页；wire 输出
/// 最后反转成 message_id 升序。额外取的一行只用于精确判断最早页的 `next_before=null`。
fn build_history_page(
    session: &str,
    before_message_id: Option<i64>,
    rows: Vec<SessionHistoryRow>,
) -> HistoryPage {
    build_history_page_with_limit(session, before_message_id, rows, HISTORY_PAGE_MAX_ROWS)
}

fn build_history_page_with_limit(
    session: &str,
    before_message_id: Option<i64>,
    mut rows: Vec<SessionHistoryRow>,
    page_max_rows: usize,
) -> HistoryPage {
    let database_has_more = rows.len() > page_max_rows;
    rows.truncate(page_max_rows);

    let mut kept_desc: Vec<Value> = Vec::new();
    let mut oversized_dropped = 0_u64;
    let mut budget_truncated = false;
    let mut oldest_scanned = None;

    for (index, row) in rows.iter().enumerate() {
        let message = history_message(row);
        let row_has_older = database_has_more || index + 1 < rows.len();
        let single = history_payload(
            session,
            before_message_id,
            std::slice::from_ref(&message),
            row_has_older.then_some(row.message_id),
        );
        if serde_json::to_vec(&single)
            .map(|json| json.len())
            .unwrap_or(usize::MAX)
            > HISTORY_SEND_BUDGET_BYTES
        {
            oversized_dropped += 1;
            oldest_scanned = Some(row.message_id);
            continue;
        }

        kept_desc.push(message);
        let mut candidate_ascending = kept_desc.clone();
        candidate_ascending.reverse();
        let min_id = kept_desc
            .last()
            .and_then(|message| message.get("message_id"))
            .and_then(Value::as_i64);
        let candidate = history_payload(
            session,
            before_message_id,
            &candidate_ascending,
            row_has_older.then_some(min_id).flatten(),
        );
        if serde_json::to_vec(&candidate)
            .map(|json| json.len())
            .unwrap_or(usize::MAX)
            > HISTORY_SEND_BUDGET_BYTES
        {
            kept_desc.pop();
            budget_truncated = true;
            break;
        }
        oldest_scanned = Some(row.message_id);
    }

    let mut messages = kept_desc;
    messages.reverse();
    // 只有确实还有未扫描行时才给游标。超大行属于有意消费/丢弃，游标要越过它；因整页预算
    // 装不下的普通行则没有被消费，游标停在上一条已扫描行，下一页仍能取到它。
    let next_scan_before = (budget_truncated || database_has_more)
        .then_some(oldest_scanned)
        .flatten();

    HistoryPage {
        payload: history_payload(session, before_message_id, &messages, next_scan_before),
        oversized_dropped,
        next_scan_before,
    }
}

fn handle_command_envelope(
    inner: &Inner,
    value: &Value,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Option<Value> {
    let state = &inner.state;
    let Some(command_id) = value
        .get("command_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let failed = || {
        state.bad_frames.fetch_add(1, Ordering::Relaxed);
        Some(input_ack_json(&command_id, AckOutcome::Failed))
    };
    if command_id.contains('|') || command_id.len() > COMMAND_ID_MAX_LEN {
        return failed();
    }

    let Some(v) = value
        .get("v")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
    else {
        return failed();
    };
    let Some(room) = value.get("room").and_then(Value::as_str) else {
        return failed();
    };
    let Some(epoch) = value.get("epoch").and_then(Value::as_u64) else {
        return failed();
    };
    let Some(kind) = value.get("kind").and_then(Value::as_str) else {
        return failed();
    };
    let session = match value.get("session") {
        Some(Value::String(session)) => Some(session.clone()),
        Some(Value::Null) => None,
        _ => return failed(),
    };
    if let Some(session_value) = session.as_deref() {
        if session_value.contains('|') {
            return failed();
        }
    }
    let Some(ct) = value.get("ct").and_then(Value::as_str) else {
        return failed();
    };
    let Some(n) = value.get("n").and_then(Value::as_str) else {
        return failed();
    };
    let Some(k_room) = k_room else {
        // v1.7.3：K_room 不可用（钥匙串读失败/未配对）时不回 ack——回 failed 会让 relay
        // 删除这条 pending 行，用户离线期间发来的消息会被永久销毁；不回 ack 则 relay 保留该行，
        // 走 30 分钟 TTL 或桌面下次重连后重投。不增 bad_frames（这不是一个坏帧，只是桌面此刻
        // 拿不到钥匙）。
        return None;
    };
    let meta = EnvelopeMeta {
        v,
        room: room.to_owned(),
        epoch,
        kind: kind.to_owned(),
        session,
        command_id: Some(command_id.clone()),
    };
    let Ok(plaintext) = crate::remote_crypto::open(k_room, &meta, ct, n) else {
        return failed();
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&plaintext) else {
        return failed();
    };

    match (kind, payload.get("t").and_then(Value::as_str)) {
        ("input", Some("input.send")) => {
            let Some(session) = payload.get("session").and_then(Value::as_str) else {
                return failed();
            };
            // M2-4c：下行归属闸——不属于当前 active repo 的 session 一律 failed（fail-closed）。
            if !command_session_allowed(inner, session) {
                return failed();
            }
            let Some(text) = payload.get("text").and_then(Value::as_str) else {
                return failed();
            };
            let Some(outcome) = (inner.input_send_handler)(InputSendFrame {
                session: session.to_owned(),
                command_id: command_id.clone(),
                text: text.to_owned(),
            }) else {
                // receipt 无法持久化或终态暂不可知时不回 ack，让 relay 保留并重投；
                // 这不是坏帧，不增加 bad_frames。
                return None;
            };
            Some(input_ack_json(&command_id, outcome))
        }
        ("control", Some("control.stop")) => {
            let Some(session) = payload.get("session").and_then(Value::as_str) else {
                return failed();
            };
            // M2-4c：下行归属闸——同 input.send。
            if !command_session_allowed(inner, session) {
                return failed();
            }
            let Some(issued_at_ms) = payload
                .get("issued_at_ms")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0 && *value <= JSON_SAFE_INTEGER_MAX)
            else {
                return failed();
            };
            let Some(expires_at_ms) = payload
                .get("expires_at_ms")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0 && *value <= JSON_SAFE_INTEGER_MAX)
            else {
                return failed();
            };
            if issued_at_ms > expires_at_ms {
                return failed();
            }
            if expires_at_ms - issued_at_ms > CONTROL_STOP_MAX_LIFETIME_MS {
                return failed();
            }
            if is_control_stop_stale(issued_at_ms, expires_at_ms, now_unix_ms()) {
                return failed();
            }
            if !(inner.control_replay_handler)(session, &command_id) {
                return failed();
            }
            let outcome = (inner.control_stop_handler)(ControlStopFrame {
                session: session.to_owned(),
                command_id: command_id.clone(),
            });
            Some(input_ack_json(&command_id, outcome))
        }
        ("input", Some("input.answer")) => {
            let Some(session) = payload.get("session").and_then(Value::as_str) else {
                return failed();
            };
            // M2-4c：下行归属闸——同 input.send。
            if !command_session_allowed(inner, session) {
                return failed();
            }
            let Some(decision_id) = payload.get("decision_id").and_then(Value::as_str) else {
                return failed();
            };
            let Some(option) = payload.get("option").and_then(Value::as_str) else {
                return failed();
            };
            let Some(outcome) = (inner.input_answer_handler)(InputAnswerFrame {
                session: session.to_owned(),
                command_id: command_id.clone(),
                decision_id: decision_id.to_owned(),
                option: option.to_owned(),
            }) else {
                return None;
            };
            Some(input_ack_json(&command_id, outcome))
        }
        ("control", Some("control.history")) => {
            let Some(session) = payload.get("session").and_then(Value::as_str) else {
                return failed();
            };
            if session.len() > SESSION_ID_MAX_BYTES {
                return failed();
            }
            if !command_session_allowed(inner, session) {
                return failed();
            }
            let before_message_id = match payload.get("before_message_id") {
                None | Some(Value::Null) => None,
                Some(value) => {
                    let Some(cursor) = value.as_i64().filter(|cursor| *cursor >= 0) else {
                        return failed();
                    };
                    Some(cursor)
                }
            };
            // 多取一行只为判断是否确有更早消息，wire 页仍至多 50 条。若整窗都因单条超预算
            // 被有意丢弃，用最老已扫描 id 继续查下一窗；否则空页的 null 游标会让更早消息永远
            // 不可达。响应里的 before_message_id 始终回显手机原始请求，而不是内部扫描游标。
            let mut scan_before = before_message_id;
            let mut oversized_dropped = 0_u64;
            let page = loop {
                let Ok(rows) = (inner.session_history_provider)(
                    session,
                    scan_before,
                    (HISTORY_PAGE_MAX_ROWS + 1) as i64,
                ) else {
                    return failed();
                };
                let page = build_history_page(session, before_message_id, rows);
                oversized_dropped = oversized_dropped.saturating_add(page.oversized_dropped);
                let emitted = page
                    .payload
                    .get("messages")
                    .and_then(Value::as_array)
                    .map(|messages| !messages.is_empty())
                    .unwrap_or(false);
                let Some(next_scan_before) = page.next_scan_before else {
                    break page;
                };
                if emitted {
                    break page;
                }
                if scan_before.is_some_and(|cursor| next_scan_before >= cursor) {
                    return failed();
                }
                scan_before = Some(next_scan_before);
            };
            state
                .history_oversized_dropped
                .fetch_add(oversized_dropped, Ordering::Relaxed);
            let cursor_name = before_message_id
                .map(|cursor| cursor.to_string())
                .unwrap_or_else(|| "latest".to_owned());
            let client_msg_id =
                derive_client_msg_id(&format!("history|{session}|{command_id}|{cursor_name}"));
            enqueue_prebuilt_live_for_upstream(
                state,
                &inner.upstream_tx,
                MilestoneItem {
                    session: Some(session.to_owned()),
                    t: "history".to_owned(),
                    payload: page.payload,
                    client_msg_id,
                },
            );
            Some(input_ack_json(&command_id, AckOutcome::Ok))
        }
        ("control", Some("control.snapshot")) => {
            let Some(session) = payload.get("session").and_then(Value::as_str) else {
                return failed();
            };
            // P0-b 微返工第 4 轮：session 长度纵深守卫——见 `SESSION_ID_MAX_BYTES` 定义处的
            // 完整推导。放在归属闸之前：协议都不合规的 session 值不值得再去查归属。
            if session.len() > SESSION_ID_MAX_BYTES {
                return failed();
            }
            // P0-b：下行归属闸——同 input.send/control.stop/input.answer。
            if !command_session_allowed(inner, session) {
                return failed();
            }
            // P0-b：单次临界区原子取出该 session 的当前归约态——不存在即 idle。
            let (run, blocks) = {
                let snapshots = lock(&state.partial_snapshots);
                match snapshots.get(session) {
                    Some(entry) => (
                        Some((entry.run_id.clone(), entry.last_seq)),
                        entry.reducer.snapshot_blocks(),
                    ),
                    None => (None, Vec::new()),
                }
            };
            let snapshot_payload = build_snapshot_payload(
                session,
                run.as_ref()
                    .map(|(run_id, through_run_seq)| (run_id.as_str(), *through_run_seq)),
                &blocks,
            );
            // P0-b 返工②(c)·微返工第 3 轮改写发送侧兜底：`build_snapshot_payload` 已经做过
            // 收敛，这里只兜住"收敛后仍超预算"的边缘情形——绝不把超帧交给 relay 裁决（1009
            // 踢断桌面 + 客户端重请求 = 断连环）。`SNAPSHOT_SEND_BUDGET_BYTES`（44KiB）是按
            // 信封膨胀折算过的明文预算，不是拿裸 payload 直接比 relay 64KB 硬闸——见该常量
            // 定义处的完整推导；`SNAPSHOT_PAYLOAD_BUDGET_BYTES` 收敛正确后本分支正常路径几乎
            // 不可达，纯保险丝。只护 snapshot 这一条路径，不改其它里程碑的既有 enqueue 行为。
            //
            // P0-b 微返工第 4 轮：改用 `snapshot_frame_bytes` 计量——与 `shrink_snapshot_
            // blocks_to_budget` 收敛判断同一把尺子（都含 `t`），不再是这里量未合并 `t` 的裸
            // `snapshot_payload`。旧写法两处口径分裂：①收敛判断按含 `t` 的尺寸算已在预算内，
            // ②这里却按裸尺寸算是否超 44KiB 兜底阈值，一个真实逼近阈值的边界帧可能被①放行、
            // 又被②的偏小裸尺寸误判"未超"而照发。
            //
            // 窄窗如实记档：命中这个分支时函数已经回 `AckOutcome::Ok`（下面这行）而对应的
            // snapshot 帧其实从未入队/发出——与 drain 阶段过滤丢弃属同一族"ack 已回但帧未达"
            // 窄窗，不是本轮修复范围，只是不新增语义、如实标注既有取舍。
            let snapshot_payload_bytes = snapshot_frame_bytes(&snapshot_payload);
            if snapshot_payload_bytes > SNAPSHOT_SEND_BUDGET_BYTES {
                state
                    .snapshot_oversized_dropped
                    .fetch_add(1, Ordering::Relaxed);
                return Some(input_ack_json(&command_id, AckOutcome::Ok));
            }
            // v1.8.11：client_msg_id 由请求 command_id 确定性派生——重投同一请求天然幂等去重。
            let client_msg_id = derive_client_msg_id(&format!("snapshot|{session}|{command_id}"));
            // snapshot 应答走既有里程碑队列出口（同 drain 轮里程碑先于 live）。直接用本次调用
            // 已持有的 inner.state/inner.milestone_tx 入队，而不是经全局单例 publish_milestone
            // 重新查找——生产环境下两者指向同一个 Inner，直接用局部引用可测（单元测试不会注册
            // 全局 GATEWAY 单例，经全局查找会在测试里静默无法验证已构造好的 payload）。
            enqueue_milestone_for_upstream(
                state,
                &inner.milestone_tx,
                MilestoneItem {
                    session: Some(session.to_owned()),
                    t: "snapshot".to_owned(),
                    payload: snapshot_payload,
                    client_msg_id,
                },
            );
            Some(input_ack_json(&command_id, AckOutcome::Ok))
        }
        _ => failed(),
    }
}

fn is_control_stop_stale(issued_at_ms: u64, expires_at_ms: u64, now_ms: u64) -> bool {
    now_ms >= expires_at_ms.saturating_add(CONTROL_STOP_SKEW_MS)
        || issued_at_ms >= now_ms.saturating_add(CONTROL_STOP_SKEW_MS)
}

fn input_ack_json(command_id: &str, outcome: AckOutcome) -> Value {
    let outcome = match outcome {
        AckOutcome::Ok => "ok",
        AckOutcome::Queued => "queued",
        AckOutcome::Failed => "failed",
    };
    serde_json::json!({
        "t": "input.ack",
        "command_id": command_id,
        "outcome": outcome,
    })
}

fn parse_pair_hello(value: &Value) -> Option<PairHelloFrame> {
    let room = value.get("room")?.as_str()?.to_owned();
    let remote_pub: [u8; 32] = STANDARD
        .decode(value.get("remote_pub")?.as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    Some(PairHelloFrame {
        room,
        remote_pub,
        token_ct: value.get("token_ct")?.as_str()?.to_owned(),
        token_n: value.get("token_n")?.as_str()?.to_owned(),
        origin_connection_id: value.get("origin_connection_id")?.as_str()?.to_owned(),
    })
}

fn parse_pair_done(value: &Value) -> Option<PairDoneFrame> {
    Some(PairDoneFrame {
        room: value.get("room")?.as_str()?.to_owned(),
        device_id: value.get("device_id")?.as_str()?.to_owned(),
        confirm_ct: value
            .get("confirm_ct")
            .and_then(Value::as_str)
            .map(str::to_owned),
        confirm_n: value
            .get("confirm_n")
            .and_then(Value::as_str)
            .map(str::to_owned),
        origin_connection_id: value.get("origin_connection_id")?.as_str()?.to_owned(),
    })
}

/// S1i1 §2e：relay 盖章的 `token.refresh.forward`——字段缺失/类型错走 gateway 既有 bad_frame
/// 计数（调用方在 `handle_frame` 判 `None` 就短路，不进 refresh handler）。
fn parse_refresh_forward(value: &Value) -> Option<RefreshForwardFrame> {
    Some(RefreshForwardFrame {
        request_id: value.get("request_id")?.as_str()?.to_owned(),
        subject: value.get("subject")?.as_str()?.to_owned(),
        request_generation: value.get("request_generation")?.as_i64()?,
        ct: value.get("ct")?.as_str()?.to_owned(),
        n: value.get("n")?.as_str()?.to_owned(),
    })
}

/// 对齐 fixtures/wire-v1.json `token_refresh_ok_valid`：`{t, request_id, subject, generation,
/// ct, n}`。幂等重放（§2c）与轮换 ack 之后（§2b）共用这一个构造点。
pub(crate) fn refresh_ok_json(
    request_id: &str,
    subject: &str,
    generation: i64,
    ct: &str,
    n: &str,
) -> Value {
    serde_json::json!({
        "t": "token.refresh.ok",
        "request_id": request_id,
        "subject": subject,
        "generation": generation,
        "ct": ct,
        "n": n,
    })
}

/// 对齐 fixtures/wire-v1.json `token_refresh_fail_valid`（`close:true`，连续 ≥3 次无效）/
/// `token_refresh_fail_in_flight_no_close_valid`（省略 `close`，良性单飞行冲突）——`close`
/// 字段本身按样张只在 true 时出现，不写 `close:false`。
pub(crate) fn refresh_fail_json(
    request_id: &str,
    subject: &str,
    reason: &str,
    close: bool,
) -> Value {
    let mut frame = serde_json::json!({
        "t": "token.refresh.fail",
        "request_id": request_id,
        "subject": subject,
        "reason": reason,
    });
    if close {
        frame["close"] = Value::Bool(true);
    }
    frame
}

fn pair_ready_json(frame: PairReadyFrame) -> Value {
    serde_json::json!({
        "t": "pair.ready",
        "room": frame.room,
        "device_id": frame.device_id,
        "ct": frame.ct,
        "n": frame.n,
    })
}

fn pair_accept_json(frame: PairAcceptFrame) -> Value {
    serde_json::json!({
        "t": "pair.accept",
        "room": frame.room,
        "device_id": frame.device_id,
        "k_room_ct": frame.k_room_ct,
        "k_room_n": frame.k_room_n,
        "tokens_ct": frame.tokens_ct,
        "tokens_n": frame.tokens_n,
    })
}

fn set_status(state: &GatewayInnerState, gateway_state: GatewayState, last_error: Option<String>) {
    *lock(&state.status) = GatewayStatus {
        state: gateway_state,
        last_error,
        stopped_reason: None,
        counters: GatewayCounters::default(),
    };
}

fn set_stopped_status(
    state: &GatewayInnerState,
    last_error: String,
    stopped_reason: Option<String>,
) {
    *lock(&state.status) = GatewayStatus {
        state: GatewayState::Disabled,
        last_error: Some(last_error),
        stopped_reason,
        counters: GatewayCounters::default(),
    };
}

fn interruptible_sleep(inner: &Inner, duration: Duration) -> bool {
    let mut remaining = duration;
    while !remaining.is_zero() {
        if inner.shutdown.load(Ordering::Acquire) {
            return true;
        }
        if inner.reload_requested.load(Ordering::Acquire) {
            return false;
        }
        if inner.registry_publish_wake.load(Ordering::Acquire) {
            return false;
        }
        let slice = remaining.min(BACKOFF_POLL_INTERVAL);
        thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
    inner.shutdown.load(Ordering::Acquire)
}

fn wait_for_reload(inner: &Inner) -> bool {
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            return true;
        }
        if inner.reload_requested.load(Ordering::Acquire) {
            return false;
        }
        if inner.registry_publish_wake.load(Ordering::Acquire) {
            return false;
        }
        thread::sleep(BACKOFF_POLL_INTERVAL);
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let is_gateway_thread = std::thread::current()
            .name()
            .map(|name| name == "remote-gateway")
            .unwrap_or(false);
        if !is_gateway_thread {
            previous(info);
            return;
        }
        match GATEWAY
            .get()
            .and_then(|inner| inner.active_token.try_lock().ok())
        {
            Some(guard) => {
                let message = any_payload_message(info.payload());
                let redacted = redact_panic_message(&message, guard.as_deref());
                eprintln!("remote gateway panic (redacted): {redacted}");
            }
            None => {
                eprintln!("redacted panic in remote-gateway thread");
            }
        }
    }));
}

fn set_active_token(inner: &Inner, token: Option<&SecretToken>) {
    *lock(&inner.active_token) = token.map(|token| token.expose().to_owned());
}

fn clear_active_token(inner: &Inner) {
    *lock(&inner.active_token) = None;
}

fn take_active_token(inner: &Inner) -> Option<String> {
    lock(&inner.active_token).take()
}

fn redact_panic_message(message: &str, active_token: Option<&str>) -> String {
    redact(message, active_token)
}

fn any_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    any_payload_message(payload.as_ref())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn gateway_status_carries_all_internal_counters() {
        let state = GatewayInnerState::default();
        state.frames_seen.store(1, Ordering::Relaxed);
        state.frames_sent.store(2, Ordering::Relaxed);
        state.bad_frames.store(3, Ordering::Relaxed);
        state.upstream_dropped.store(4, Ordering::Relaxed);
        state
            .upstream_stale_generation_dropped
            .store(5, Ordering::Relaxed);
        state.upstream_budget_dropped.store(6, Ordering::Relaxed);
        state.milestone_dropped.store(7, Ordering::Relaxed);
        state
            .session_index_snapshot_unavailable
            .store(8, Ordering::Relaxed);
        state
            .snapshot_worker_spawn_count
            .store(9, Ordering::Relaxed);
        state.tool_correlation_dropped.store(10, Ordering::Relaxed);
        state.classify_skipped.store(11, Ordering::Relaxed);
        state.connection_failures.store(12, Ordering::Relaxed);
        state.panics.store(13, Ordering::Relaxed);
        state.upstream_repo_filtered.store(14, Ordering::Relaxed);
        state
            .partial_snapshot_capacity_dropped
            .store(15, Ordering::Relaxed);
        state
            .snapshot_oversized_dropped
            .store(16, Ordering::Relaxed);
        state.history_oversized_dropped.store(17, Ordering::Relaxed);
        state.replay_oversized_dropped.store(18, Ordering::Relaxed);
        state.keepalive_pings_sent.store(19, Ordering::Relaxed);
        state.disconnect_config_stale.store(20, Ordering::Relaxed);
        state.disconnect_closed_by_peer.store(21, Ordering::Relaxed);
        state.disconnect_error.store(22, Ordering::Relaxed);
        *lock(&state.last_disconnect_reason) = "read failed: redacted diagnostic".to_owned();

        assert_eq!(
            gateway_status_from_state(&state).counters,
            GatewayCounters {
                frames_seen: 1,
                frames_sent: 2,
                bad_frames: 3,
                upstream_dropped: 4,
                upstream_stale_generation_dropped: 5,
                upstream_budget_dropped: 6,
                milestone_dropped: 7,
                session_index_snapshot_unavailable: 8,
                snapshot_worker_spawn_count: 9,
                tool_correlation_dropped: 10,
                classify_skipped: 11,
                connection_failures: 12,
                panics: 13,
                upstream_repo_filtered: 14,
                partial_snapshot_capacity_dropped: 15,
                snapshot_oversized_dropped: 16,
                history_oversized_dropped: 17,
                replay_oversized_dropped: 18,
                keepalive_pings_sent: 19,
                disconnect_config_stale: 20,
                disconnect_closed_by_peer: 21,
                disconnect_error: 22,
                last_disconnect_reason: "read failed: redacted diagnostic".to_owned(),
            }
        );
    }

    #[test]
    fn remote_config_stale_guard_backs_off_third_fast_exit_and_climbs_normally() {
        let started_at = Instant::now();
        let mut guard = ConfigStaleBackoffGuard::default();

        assert_eq!(
            guard.delay_attempt(started_at, Duration::from_secs(2)),
            None
        );
        assert_eq!(
            guard.delay_attempt(started_at + Duration::from_secs(5), Duration::from_secs(2)),
            None
        );
        assert_eq!(
            guard.delay_attempt(started_at + Duration::from_secs(10), Duration::from_secs(2)),
            Some(0)
        );
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(
            guard.delay_attempt(started_at + Duration::from_secs(15), Duration::from_secs(2)),
            Some(1)
        );
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
    }

    #[test]
    fn remote_connect_loop_real_error_starts_at_first_backoff_after_config_stale_guard() {
        let inner = test_inner(|_| None, || None);
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: "pairing".to_owned(),
                generation: 8,
                scope: "pairing".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_430_700_000,
                    refresh_until: None,
                },
                prev: None,
            },
            None,
        );
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let mut attempts = VecDeque::from([
            ConnectAttempt::Ran {
                token_for_redact: None,
                result: Ok(ConnectionExit::ConfigStale {
                    connected_for: Duration::from_secs(1),
                }),
            },
            ConnectAttempt::Ran {
                token_for_redact: None,
                result: Ok(ConnectionExit::ConfigStale {
                    connected_for: Duration::from_secs(1),
                }),
            },
            ConnectAttempt::Ran {
                token_for_redact: None,
                result: Ok(ConnectionExit::ConfigStale {
                    connected_for: Duration::from_secs(1),
                }),
            },
            ConnectAttempt::Ran {
                token_for_redact: None,
                result: Err(ConnectionFailure::Other("real connection error".to_owned())),
            },
        ]);
        let mut completed_delays = Vec::new();

        connect_loop_with(
            Arc::downgrade(&inner),
            upstream_rx,
            milestone_rx,
            |_, _, _| {
                attempts
                    .pop_front()
                    .expect("unexpected extra connection attempt")
            },
            |inner, delay| {
                if inner.registry_publish_wake.load(Ordering::Acquire) {
                    return false;
                }
                completed_delays.push(delay);
                completed_delays.len() == 2
            },
            |_| panic!("the test sequence must not enter terminal wait"),
        );

        assert_eq!(
            completed_delays,
            vec![Duration::from_secs(1), Duration::from_secs(1)],
            "an unacked outbox must not interrupt either the guard backoff or the following real-error backoff"
        );
        assert!(!inner.registry_publish_wake.load(Ordering::Acquire));
        assert_eq!(inner.state.connection_failures.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn remote_disconnect_status_counts_categories_and_redacts_error_reason() {
        let inner = test_inner(|_| None, || None);
        let secret = "ab".repeat(32);
        let mut failed_attempts = 0;

        record_disconnect(&inner.state, DisconnectKind::ConfigStale, "config_stale");
        record_disconnect(&inner.state, DisconnectKind::ClosedByPeer, "closed_by_peer");
        record_failure(
            &inner,
            FailureKind::Connection,
            format!("read failed: token={secret}"),
            Some(&secret),
            &mut failed_attempts,
        );

        let counters = gateway_status_from_state(&inner.state).counters;
        assert_eq!(counters.disconnect_config_stale, 1);
        assert_eq!(counters.disconnect_closed_by_peer, 1);
        assert_eq!(counters.disconnect_error, 1);
        assert!(counters.last_disconnect_reason.starts_with("read failed:"));
        assert!(!counters.last_disconnect_reason.contains(&secret));
    }

    #[test]
    fn remote_config_stale_guard_resets_after_stable_connection_or_other_exit() {
        let started_at = Instant::now();
        let mut guard = ConfigStaleBackoffGuard::default();

        for seconds in [0, 5, 10] {
            let _ = guard.delay_attempt(
                started_at + Duration::from_secs(seconds),
                Duration::from_secs(2),
            );
        }
        assert_eq!(guard.backoff_attempts, 1);
        assert_eq!(
            guard.delay_attempt(
                started_at + Duration::from_secs(15),
                CONFIG_STALE_GUARD_WINDOW,
            ),
            None
        );
        assert_eq!(guard.backoff_attempts, 0);

        assert_eq!(
            guard.delay_attempt(started_at + Duration::from_secs(20), Duration::from_secs(2)),
            None
        );
        guard.reset();
        assert_eq!(
            guard.delay_attempt(started_at + Duration::from_secs(21), Duration::from_secs(2)),
            None
        );
    }

    #[test]
    fn remote_failed_registry_drain_keeps_publish_wake_set() {
        let inner = test_inner(|_| None, || None);
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: "pairing".to_owned(),
                generation: 8,
                scope: "pairing".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_430_700_000,
                    refresh_until: None,
                },
                prev: None,
            },
            None,
        );
        inner.registry_publish_wake.store(true, Ordering::Release);

        let result = drain_registry_outbox_with(&inner, |_| Err::<(), _>("write failed"));
        assert_eq!(result, Err("write failed"));
        assert!(inner.registry_publish_wake.load(Ordering::Acquire));
    }

    #[test]
    fn remote_retryable_exit_with_unacked_outbox_keeps_wake_cold_and_backs_off() {
        let inner = test_inner(|_| None, || None);
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: "pairing".to_owned(),
                generation: 8,
                scope: "pairing".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_430_700_000,
                    refresh_until: None,
                },
                prev: None,
            },
            None,
        );
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let mut attempts = 0;
        let mut wake_during_backoff = Vec::new();
        let mut completed_delays = Vec::new();

        connect_loop_with(
            Arc::downgrade(&inner),
            upstream_rx,
            milestone_rx,
            |inner, _, _| {
                attempts += 1;
                if attempts == 1 {
                    ConnectAttempt::Ran {
                        token_for_redact: None,
                        result: Ok(ConnectionExit::ClosedByPeer),
                    }
                } else {
                    inner.shutdown.store(true, Ordering::Release);
                    ConnectAttempt::Waiting
                }
            },
            |inner, delay| {
                let wake = inner.registry_publish_wake.load(Ordering::Acquire);
                wake_during_backoff.push(wake);
                if wake {
                    return false;
                }
                completed_delays.push(delay);
                false
            },
            |_| panic!("a retryable exit must not enter terminal wait"),
        );

        assert_eq!(
            attempts, 2,
            "the retry must happen after one completed backoff"
        );
        assert_eq!(wake_during_backoff, vec![false]);
        assert_eq!(completed_delays, vec![Duration::from_secs(1)]);
        assert!(!inner.registry_publish_wake.load(Ordering::Acquire));
    }

    #[test]
    fn remote_registry_publish_wake_clears_after_complete_drain_without_empty_redrain() {
        let inner = test_inner(|_| None, || None);
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: "pairing".to_owned(),
                generation: 8,
                scope: "pairing".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_430_700_000,
                    refresh_until: None,
                },
                prev: None,
            },
            None,
        );
        inner.registry_publish_wake.store(true, Ordering::Release);
        let mut sent = 0;

        drain_registry_outbox_with(&inner, |_| {
            sent += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(sent, 1);
        assert!(!inner.registry_publish_wake.load(Ordering::Acquire));

        drain_registry_outbox_with(&inner, |_| {
            sent += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(
            sent, 1,
            "a completed drain must not resend or keep the wake latch hot"
        );
        assert!(!inner.registry_publish_wake.load(Ordering::Acquire));
    }

    #[test]
    fn remote_keepalive_sends_one_ping_after_quiet_timeout_rounds_reach_idle_threshold() {
        let state = GatewayInnerState::default();
        let started_at = Instant::now();
        let mut idle = KeepaliveIdle::new(started_at);
        let mut sent = Vec::new();

        for round in 1..=60 {
            let now = started_at + Duration::from_millis(500 * round);
            idle.send_ping_if_due(now, &state, |message| {
                sent.push(message);
                Ok::<_, ()>(())
            })
            .unwrap();
        }

        assert_eq!(sent, vec![Message::Ping(Vec::new().into())]);
        assert_eq!(state.keepalive_pings_sent.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn remote_keepalive_does_not_ping_while_business_frames_refresh_idle_time() {
        let state = GatewayInnerState::default();
        let started_at = Instant::now();
        let mut idle = KeepaliveIdle::new(started_at);
        let mut sent = Vec::new();

        for seconds in [10, 20, 29, 39, 49, 58] {
            idle.record_activity(started_at + Duration::from_secs(seconds));
            idle.send_ping_if_due(
                started_at + Duration::from_secs(seconds + 1),
                &state,
                |message| {
                    sent.push(message);
                    Ok::<_, ()>(())
                },
            )
            .unwrap();
        }

        assert!(sent.is_empty());
        assert_eq!(state.keepalive_pings_sent.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn remote_keepalive_sends_at_most_one_ping_per_consecutive_idle_window() {
        let state = GatewayInnerState::default();
        let started_at = Instant::now();
        let mut idle = KeepaliveIdle::new(started_at);
        let mut sent = Vec::new();

        for seconds in [30, 31, 59, 60, 61, 89, 90] {
            idle.send_ping_if_due(
                started_at + Duration::from_secs(seconds),
                &state,
                |message| {
                    sent.push(message);
                    Ok::<_, ()>(())
                },
            )
            .unwrap();
        }

        assert_eq!(
            sent,
            vec![
                Message::Ping(Vec::new().into()),
                Message::Ping(Vec::new().into()),
                Message::Ping(Vec::new().into()),
            ]
        );
        assert_eq!(state.keepalive_pings_sent.load(Ordering::Relaxed), 3);
    }

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

    /// 见 `test_session_repo_provider_allowing_default_repo` 文档——两者成对使用。
    fn with_default_active_repo(inner: Arc<Inner>) -> Arc<Inner> {
        *lock(&inner.state.active_repo_id_for_gating) =
            Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
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

    #[test]
    fn remote_registry_sync_frame_matches_wire_v1_fixture_sample() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../../remote-relay/fixtures/wire-v1.json"))
                .unwrap();
        let expected = fixtures
            .as_array()
            .unwrap()
            .iter()
            .find(|fixture| fixture["name"] == "token_sync_two_entries_valid")
            .unwrap()["frame"]
            .clone();
        let snapshot = RegistrySnapshot {
            revision: 106,
            entries: vec![
                TokenSyncEntry {
                    subject: "device:11111111-1111-4111-8111-111111111111".to_owned(),
                    generation: 100,
                    scope: "remote".to_owned(),
                    current: TokenSyncCurrent {
                        token_hash: "aa".repeat(32),
                        access_expires: 1_765_434_000_000,
                        refresh_until: Some(1_768_022_400_000),
                    },
                    prev: None,
                },
                TokenSyncEntry {
                    subject: "device:22222222-2222-4222-8222-222222222222".to_owned(),
                    generation: 105,
                    scope: "remote".to_owned(),
                    current: TokenSyncCurrent {
                        token_hash: "bb".repeat(32),
                        access_expires: 1_765_434_000_000,
                        refresh_until: Some(1_768_022_400_000),
                    },
                    prev: None,
                },
            ],
        };

        let actual: Value = serde_json::from_str(&registry_sync_frame(&snapshot).unwrap()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn remote_registry_outbox_higher_generation_cancels_only_lower_same_subject() {
        let mut registry = RegistryState::default();
        for (subject, generation) in [("device:a", 4), ("device:b", 2), ("device:a", 7)] {
            registry.enqueue_outbox(RegistryOutboxItem {
                subject: subject.to_owned(),
                generation,
                frame: serde_json::json!({"generation": generation}),
                attempts: 0,
                last_sent_at: None,
                acked: false,
                rejected: false,
                pair_ready: None,
                refresh_ok: None,
            });
        }
        registry.enqueue_outbox(RegistryOutboxItem {
            subject: "device:a".to_owned(),
            generation: 6,
            frame: serde_json::json!({"generation": 6}),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: None,
        });

        assert_eq!(registry.outbox.len(), 2);
        assert!(registry
            .outbox
            .iter()
            .any(|item| item.subject == "device:a" && item.generation == 7));
        assert!(registry
            .outbox
            .iter()
            .any(|item| item.subject == "device:b" && item.generation == 2));
    }

    #[test]
    fn remote_registry_outbox_same_generation_keeps_retry_metadata() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: "device:a".to_owned(),
                generation: 7,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            None,
        );
        registry.outbox[0].attempts = 3;
        registry.outbox[0].last_sent_at = Some(1_765_430_400_000);

        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: "device:a".to_owned(),
                generation: 7,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_001,
                    refresh_until: Some(1_768_022_400_001),
                },
                prev: None,
            },
            None,
        );

        assert_eq!(registry.outbox.len(), 1);
        assert_eq!(registry.outbox[0].attempts, 3);
        assert_eq!(registry.outbox[0].last_sent_at, Some(1_765_430_400_000));
        assert_eq!(
            registry.outbox[0].frame["current"]["token_hash"],
            "aa".repeat(32)
        );
    }

    #[test]
    fn pairing_token_put_matches_wire_fixture_and_uses_absolute_millis() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../../remote-relay/fixtures/wire-v1.json"))
                .unwrap();
        let expected = fixtures
            .as_array()
            .unwrap()
            .iter()
            .find(|fixture| fixture["name"] == "token_put_pairing_valid")
            .unwrap()["frame"]
            .clone();
        let entry = TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 102,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "52b6419d27bd7f547cee3b92f8c17a908b8a49601ecbec161e5030de1dfe9e0a"
                    .to_owned(),
                access_expires: 1_765_430_700_000,
                refresh_until: None,
            },
            prev: None,
        };

        let actual = token_put_frame(&entry);

        assert_eq!(actual, expected);
        assert!(actual["current"]["access_expires"].as_i64().unwrap() >= 100_000_000_000);
    }

    #[test]
    fn token_ack_ok_removes_matching_item_but_rejected_stays_separately_marked() {
        let mut registry = RegistryState::default();
        for generation in [1, 2] {
            registry.enqueue_outbox(RegistryOutboxItem {
                subject: format!("device:{generation}"),
                generation,
                frame: serde_json::json!({"t": "token.put", "generation": generation}),
                attempts: 1,
                last_sent_at: Some(100),
                acked: false,
                rejected: false,
                pair_ready: None,
                refresh_ok: None,
            });
        }

        assert!(matches!(
            registry.consume_token_ack("device:1", 1, "ok"),
            TokenAckAction::Consumed
        ));
        assert!(matches!(
            registry.consume_token_ack("device:2", 2, "rejected"),
            TokenAckAction::Rejected
        ));
        assert_eq!(registry.outbox.len(), 1);
        assert_eq!(registry.outbox[0].subject, "device:2");
        assert!(!registry.outbox[0].acked);
        assert!(registry.outbox[0].rejected);
    }

    // S1h 2c：revoke（token.delete）独立重试通道一致性测试。

    #[test]
    fn remote_registry_revoke_delete_ack_ok_removes_item() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_delete("device:a".to_owned(), 5, true);

        assert!(matches!(
            registry.consume_token_ack("device:a", 5, "ok"),
            TokenAckAction::Consumed
        ));
        assert!(
            registry.outbox.is_empty(),
            "ok ack 必须把 revoke 项从 outbox 删掉"
        );
    }

    #[test]
    fn remote_registry_revoke_delete_ack_idempotent_removes_item_same_as_ok() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_delete("device:b".to_owned(), 6, true);

        assert!(matches!(
            registry.consume_token_ack("device:b", 6, "idempotent"),
            TokenAckAction::Consumed
        ));
        assert!(
            registry.outbox.is_empty(),
            "idempotent ack 与 ok 皆算成，必须同样删项"
        );
    }

    #[test]
    fn remote_registry_revoke_outbox_survives_higher_generation_put() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_delete("device:c".to_owned(), 5, true);

        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: "device:c".to_owned(),
                generation: 9,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            None,
        );

        assert_eq!(
            registry.outbox.len(),
            1,
            "撤销后设备不复活：更高代的 put 必须被作废，不得挤掉已排队的 revoke"
        );
        assert_eq!(registry.outbox[0].frame["t"], "token.delete");
        assert_eq!(registry.outbox[0].generation, 5);
        assert!(!registry.outbox[0].acked);
        assert!(!registry.outbox[0].rejected);
    }

    #[test]
    fn remote_registry_revoke_enqueue_cancels_unsent_put_same_subject() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: "device:d".to_owned(),
                generation: 3,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            None,
        );

        registry.enqueue_token_delete("device:d".to_owned(), 9, true);

        assert_eq!(
            registry.outbox.len(),
            1,
            "高水位方向一致：revoke 入队必须取消同 subject 未发的 put"
        );
        assert_eq!(registry.outbox[0].frame["t"], "token.delete");
        assert_eq!(registry.outbox[0].generation, 9);
    }

    #[test]
    fn remote_registry_revoke_reconnect_rearms_unacked_item_for_resend() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_delete("device:e".to_owned(), 7, true);
        registry.outbox[0].attempts = 2;
        registry.outbox[0].last_sent_at = Some(1_765_430_400_000);

        registry.prepare_outbox_for_reconnect();

        assert_eq!(registry.outbox.len(), 1);
        assert_eq!(
            registry.outbox[0].last_sent_at, None,
            "重连后未 ack 的 revoke 项必须随未 ack 项一起重发"
        );
        assert_eq!(
            registry.outbox[0].attempts, 2,
            "重连只解锁发送闸门，不清零重试计数"
        );
        assert!(!registry.outbox[0].acked);
        assert!(!registry.outbox[0].rejected);
    }

    #[test]
    fn remote_registry_revoke_rebase_reissues_with_new_generation_instead_of_dropping() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_delete("device:f".to_owned(), 3, true);
        registry.outbox[0].attempts = 1;
        registry.outbox[0].last_sent_at = Some(1_765_430_400_000);

        registry.rebase_outbox_entries(&[], &[("device:f".to_owned(), 42)]);

        assert_eq!(
            registry.outbox.len(),
            1,
            "rebase 对 revoke 项的处置=重臂照发，不是 rejected 内容不能被丢弃"
        );
        assert_eq!(registry.outbox[0].generation, 42);
        assert_eq!(registry.outbox[0].frame["generation"], 42);
        assert_eq!(registry.outbox[0].frame["subject"], "device:f");
        assert_eq!(registry.outbox[0].frame["close"], true);
        assert_eq!(registry.outbox[0].last_sent_at, None);
        assert!(!registry.outbox[0].acked);
    }

    #[test]
    fn remote_registry_pending_revoke_subjects_includes_rejected_delete() {
        // S1h R2 返工：rejected 的 delete 仍要算「待送达」，否则永远没有机会重新领号重试
        // （按 §9.3「revoke 类独立重试直到 ack」，跟 put 的「rejected 停发」惯例不同）。
        let mut registry = RegistryState::default();
        registry.enqueue_token_delete("device:h".to_owned(), 3, true);
        assert_eq!(
            registry.consume_token_ack("device:h", 3, "rejected"),
            TokenAckAction::Rejected
        );

        assert_eq!(
            registry.pending_revoke_subjects(),
            vec!["device:h".to_owned()],
            "rejected 但未 ack 的 revoke 项仍属于待重试的 pending 集合"
        );
    }

    #[test]
    fn remote_registry_rebase_keeps_rejected_revoke_item_and_rearms_it_with_new_generation() {
        // S1h R2/R3 返工：put 被拒仍然停发丢弃，但 revoke（token.delete）被拒不能被 rebase 的
        // 清理规则连坐丢弃——它要留在 outbox 里参与下一轮重臂，并且重新领号后 rejected 标记
        // 要清掉，不然即使代号刷新了，drain_registry_outbox 的 `!item.rejected` 闸门还是不会
        // 把它发出去。
        let mut registry = RegistryState::default();
        registry.enqueue_token_delete("device:g".to_owned(), 3, true);
        assert_eq!(
            registry.consume_token_ack("device:g", 3, "rejected"),
            TokenAckAction::Rejected
        );
        assert!(registry.outbox[0].rejected);

        registry.rebase_outbox_entries(&[], &[("device:g".to_owned(), 9)]);

        assert_eq!(
            registry.outbox.len(),
            1,
            "rejected 的 revoke 项不能被 rebase 连坐丢弃"
        );
        assert_eq!(registry.outbox[0].generation, 9);
        assert_eq!(registry.outbox[0].frame["generation"], 9);
        assert_eq!(registry.outbox[0].frame["subject"], "device:g");
        assert!(
            !registry.outbox[0].rejected,
            "重新领号后必须把 rejected 翻回 false，否则 drain 的闸门永远不会把它发出去"
        );
        assert!(!registry.outbox[0].acked);
        assert_eq!(registry.outbox[0].last_sent_at, None);
    }

    #[test]
    fn remote_registry_revoke_delete_frame_matches_wire_v1_fixture_sample() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../../remote-relay/fixtures/wire-v1.json"))
                .unwrap();
        let expected = fixtures
            .as_array()
            .unwrap()
            .iter()
            .find(|fixture| fixture["name"] == "token_delete_close_valid")
            .unwrap()["frame"]
            .clone();

        let mut registry = RegistryState::default();
        registry.enqueue_token_delete(
            "device:11111111-1111-4111-8111-111111111111".to_owned(),
            104,
            true,
        );

        assert_eq!(registry.outbox[0].frame, expected);
    }

    #[test]
    fn remote_registry_rebase_drops_rejected_outbox_item_instead_of_rearming_it() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: "device:a".to_owned(),
                generation: 7,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            None,
        );
        assert_eq!(
            registry.consume_token_ack("device:a", 7, "rejected"),
            TokenAckAction::Rejected
        );

        registry.rebase_outbox_entries(
            &[TokenSyncEntry {
                subject: "device:a".to_owned(),
                generation: 11,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_100,
                    refresh_until: Some(1_768_022_400_100),
                },
                prev: None,
            }],
            &[],
        );

        assert!(registry.outbox.is_empty());
    }

    #[test]
    fn remote_registry_rebase_updates_generation_on_mounted_refresh_ok_receipt() {
        // S1i1 R1 返工：rebase 此前只改 item.generation/item.frame，没碰挂着的 refresh_ok——
        // 回执带着轮换那一刻冻结的旧代号出门，relay 侧 §9.6 第 246 行「回执.generation ==
        // subject 当前 generation」校验不过，被丢弃，手机凭旧 refresh 重试又只拿到同一份
        // 陈旧回执，48h journal 窗内死循环，只能重新配对。
        let mut registry = RegistryState::default();
        let refresh_ok = RefreshOkFrame {
            request_id: "req-rebase-1".to_owned(),
            subject: "device:i".to_owned(),
            generation: 5,
            ct: "response-ct".to_owned(),
            n: "response-n".to_owned(),
        };
        registry.enqueue_token_put_for_refresh(
            TokenSyncEntry {
                subject: "device:i".to_owned(),
                generation: 5,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "cc".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            refresh_ok,
        );

        registry.rebase_outbox_entries(
            &[TokenSyncEntry {
                subject: "device:i".to_owned(),
                generation: 9,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "dd".repeat(32),
                    access_expires: 1_765_434_000_100,
                    refresh_until: Some(1_768_022_400_100),
                },
                prev: None,
            }],
            &[],
        );

        assert_eq!(registry.outbox.len(), 1);
        assert_eq!(registry.outbox[0].generation, 9);
        let mounted = registry.outbox[0]
            .refresh_ok
            .as_ref()
            .expect("refresh_ok 必须仍然挂在 rebase 后的项上");
        assert_eq!(
            mounted.generation, 9,
            "rebase 后挂载的回执代号必须同步更新，不能停在轮换时刻冻结的 5"
        );
        // ct/n 不含 generation，AAD 五元组也不含 generation——密文体必须原样保留。
        assert_eq!(mounted.ct, "response-ct");
        assert_eq!(mounted.n, "response-n");

        let action = registry.consume_token_ack("device:i", 9, "ok");
        let TokenAckAction::RefreshOk(refresh_ok) = action else {
            panic!("ack 命中 rebase 后的代号必须吐出 RefreshOk，实际是 {action:?}");
        };
        assert_eq!(
            refresh_ok.generation, 9,
            "ack 释放的回执 generation 必须等于 rebase 后 DB 的当前代号"
        );
    }

    #[test]
    fn device_token_ack_rejected_with_mounted_refresh_ok_self_heals_via_fail_frame_without_burning_invalid_streak(
    ) {
        // S1i1 R2 返工：put 被 relay 拒绝时，挂在项上的 refresh_ok 不能被 Rejected 静默吞掉——
        // 桌面 DB/TokenBook 已经轮换成功，手机既收不到 ok 也收不到 fail，只能干等超时；本单要求
        // handle_frame 立即回一帧 fail，让手机凭旧 refresh 马上重试。
        let inner = test_inner(|_| None, || None);
        let subject = "device:11111111-1111-4111-8111-111111111111".to_owned();
        {
            let mut registry = lock(&inner.registry);
            // 先攒 1 次「真」无效（count=1），方便后面用「rejected 自愈之后的下一次真无效
            // 是否提前跨过阈值」来判定 rejected 有没有偷偷计数——阈值是 3，如果只留判断
            // 「最终有没有到 3」是分辨不出来的（不管 rejected 计不计数，多打几次总会到 3）；
            // 必须看 rejected 之后紧接着那一次真无效是不是还没到阈值。
            assert!(!registry.record_refresh_invalid(&subject));
            registry.enqueue_token_put_for_refresh(
                TokenSyncEntry {
                    subject: subject.clone(),
                    generation: 7,
                    scope: "remote".to_owned(),
                    current: TokenSyncCurrent {
                        token_hash: "aa".repeat(32),
                        access_expires: 1_765_434_000_000,
                        refresh_until: Some(1_768_022_400_000),
                    },
                    prev: None,
                },
                RefreshOkFrame {
                    request_id: "req-rejected-1".to_owned(),
                    subject: subject.clone(),
                    generation: 7,
                    ct: "response-ct".to_owned(),
                    n: "response-n".to_owned(),
                },
            );
        }

        let response = handle_frame(
            &inner,
            &format!(
                r#"{{"t":"token.ack","subject":"{subject}","generation":7,"result":"rejected"}}"#
            ),
            None,
        )
        .expect("put_rejected 自愈必须立即回一帧，不能悄悄丢弃");

        assert_eq!(response["t"], "token.refresh.fail");
        assert_eq!(response["request_id"], "req-rejected-1");
        assert_eq!(response["subject"], subject);
        assert_eq!(response["reason"], "put_rejected");
        assert!(
            response.get("close").is_none(),
            "put_rejected 是良性自愈路径，不带 close"
        );

        let mut registry = lock(&inner.registry);
        assert!(
            registry.outbox_snapshot_for_test()[0].rejected,
            "outbox 项仍要标 rejected，交给既有 drain/rebase 清理惯例"
        );
        // rejected 自愈之前已经攒了 1 次真无效（count=1）。如果 rejected 自愈没有偷偷计数，
        // 这里紧接着的一次真无效只是第 2 次（count=2），还不该到阈值 3；如果 rejected 悄悄
        // 计了一次（count 提前变成 2），这次就会是第 3 次，提前触发 close——这才是真正能分辨
        // 出「计没计数」的断言。
        assert!(
            !registry.record_refresh_invalid(&subject),
            "put_rejected 自愈不该计入连续无效计数——如果计了，这里会提前到第 3 次触发 close"
        );
        assert!(
            registry.record_refresh_invalid(&subject),
            "紧接着真正的第 3 次无效才该跨过阈值"
        );
    }

    #[test]
    fn remote_done_replay_rearms_timed_out_pair_ready_put_and_ack_releases_ready() {
        let ready = PairReadyFrame {
            room: "0123456789abcdef0123456789abcdef".to_owned(),
            device_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            ct: "ready-ct".to_owned(),
            n: "ready-n".to_owned(),
        };
        let subject = format!("device:{}", ready.device_id);
        let mut registry = RegistryState::default();
        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: subject.clone(),
                generation: 7,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            Some(ready.clone()),
        );
        registry.outbox[0].attempts = 3;
        registry.outbox[0].last_sent_at = Some(123);
        let original_frame = registry.outbox[0].frame.clone();

        assert_eq!(registry.replay_pair_ready(&subject), None);
        assert_eq!(registry.outbox[0].generation, 7);
        assert_eq!(registry.outbox[0].frame, original_frame);
        assert_eq!(registry.outbox[0].attempts, 3);
        assert_eq!(registry.outbox[0].last_sent_at, None);
        assert!(!registry.outbox[0].acked);

        let (addr, frames, server) = spawn_recording_server(1);
        let (mut socket, _) = tungstenite::connect(format!("ws://{addr}"))
            .expect("test client should connect to recording relay");
        let inner = test_inner(|_| None, || None);
        *lock(&inner.registry) = registry;
        drain_registry_outbox(&mut socket, &inner).unwrap();
        let resent = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(resent, original_frame);
        assert_eq!(lock(&inner.registry).outbox[0].attempts, 4);
        assert!(matches!(
            lock(&inner.registry).consume_token_ack(&subject, 7, "ok"),
            TokenAckAction::PairReady(released) if released == ready
        ));
        drop(socket);
        server.join().unwrap();
    }

    #[test]
    fn remote_done_replay_rearms_rejected_pair_ready_put_without_changing_payload_or_attempts() {
        let ready = PairReadyFrame {
            room: "0123456789abcdef0123456789abcdef".to_owned(),
            device_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            ct: "ready-ct".to_owned(),
            n: "ready-n".to_owned(),
        };
        let subject = format!("device:{}", ready.device_id);
        let mut registry = RegistryState::default();
        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: subject.clone(),
                generation: 11,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            Some(ready.clone()),
        );
        registry.outbox[0].attempts = 2;
        registry.outbox[0].last_sent_at = Some(456);
        let original = registry.outbox[0].clone();
        assert_eq!(
            registry.consume_token_ack(&subject, 11, "rejected"),
            TokenAckAction::Rejected
        );
        assert!(!registry.outbox[0].acked);
        assert!(registry.outbox[0].rejected);

        assert_eq!(registry.replay_pair_ready(&subject), None);
        let rearmed = &registry.outbox[0];
        assert_eq!(rearmed.subject, original.subject);
        assert_eq!(rearmed.generation, original.generation);
        assert_eq!(rearmed.frame, original.frame);
        assert_eq!(rearmed.attempts, original.attempts);
        assert_eq!(rearmed.pair_ready, original.pair_ready);
        assert_eq!(rearmed.last_sent_at, None);
        assert!(!rearmed.acked);
        assert!(!rearmed.rejected);

        let (addr, frames, server) = spawn_recording_server(1);
        let (mut socket, _) = tungstenite::connect(format!("ws://{addr}"))
            .expect("test client should connect to recording relay");
        let inner = test_inner(|_| None, || None);
        *lock(&inner.registry) = registry;
        drain_registry_outbox(&mut socket, &inner).unwrap();
        let resent = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(resent, original.frame);
        drop(socket);
        server.join().unwrap();
    }

    #[test]
    fn device_token_ack_is_the_pair_ready_barrier() {
        let ready = PairReadyFrame {
            room: "0123456789abcdef0123456789abcdef".to_owned(),
            device_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            ct: "ready-ct".to_owned(),
            n: "ready-n".to_owned(),
        };
        let inner = test_inner_with_pair_handlers(
            |_| None,
            |_| PairDoneAction::Accepted {
                newly_paired_device_id: None,
            },
        );
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: "device:11111111-1111-4111-8111-111111111111".to_owned(),
                generation: 7,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            Some(ready.clone()),
        );

        assert!(handle_frame(
            &inner,
            r#"{"t":"pair.done","room":"0123456789abcdef0123456789abcdef","device_id":"11111111-1111-4111-8111-111111111111","confirm_ct":"ct","confirm_n":"n","origin_connection_id":"conn-pairing-1"}"#,
            None,
        )
        .is_none());
        let response = handle_frame(
            &inner,
            r#"{"t":"token.ack","subject":"device:11111111-1111-4111-8111-111111111111","generation":7,"result":"ok"}"#,
            None,
        )
        .expect("matching successful token.ack must release pair.ready");

        assert_eq!(response, pair_ready_json(ready));
        let fixtures: Value =
            serde_json::from_str(include_str!("../../../remote-relay/fixtures/wire-v1.json"))
                .unwrap();
        let expected = fixtures
            .as_array()
            .unwrap()
            .iter()
            .find(|fixture| fixture["name"] == "pair_ready_valid")
            .unwrap()["frame"]
            .clone();
        assert_eq!(response, expected);
    }

    #[test]
    fn remote_registry_reset_cancels_whole_room_outbox() {
        let mut registry = RegistryState::default();
        registry.enqueue_outbox(RegistryOutboxItem {
            subject: "device:a".to_owned(),
            generation: 1,
            frame: serde_json::json!({"t": "token.put"}),
            attempts: 0,
            last_sent_at: None,
            acked: false,
            rejected: false,
            pair_ready: None,
            refresh_ok: None,
        });
        registry.cancel_outbox_before_reset();
        assert!(registry.outbox.is_empty());
    }

    #[test]
    fn remote_pairing_natural_cleanup_drops_put_but_keeps_explicit_delete() {
        let mut registry = RegistryState::default();
        registry.enqueue_token_put(
            TokenSyncEntry {
                subject: "pairing".to_owned(),
                generation: 1,
                scope: "pairing".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_430_700_000,
                    refresh_until: None,
                },
                prev: None,
            },
            None,
        );
        registry.clear_pairing_entry();

        assert!(registry.outbox.is_empty());
        registry.enqueue_token_delete("pairing".to_owned(), 2, true);
        registry.clear_pairing_entry();

        assert_eq!(registry.outbox.len(), 1);
        assert_eq!(registry.outbox[0].frame["t"], "token.delete");
        assert_eq!(registry.outbox[0].frame["subject"], "pairing");
        assert_eq!(registry.outbox[0].frame["generation"], 2);
        assert_eq!(registry.outbox[0].frame["close"], true);
    }

    #[test]
    fn remote_pairing_new_round_put_survives_stale_cancel_delete_in_outbox() {
        // S1h 返工二 F1：pairing 是唯一会被复用的 subject——begin#1 排 put(gen 10) → cancel
        // 排 delete(gen 11，顺手挤掉未发的 gen 10 put) → begin#2 之前，这条陈旧 delete 不能
        // 继续堵在 outbox 里：`enqueue_outbox` 的豁免规则（同 subject 已排 delete → 后续 put
        // 一律作废）会把 begin#2 的新 put 吞掉，只剩这条陈旧 delete 会在下次重连 absorb 时被
        // 重新领到一个高于本次 sync revision 的代号发出去，把刚开始的新一轮配对当场撤销
        // （详见 `set_pairing_entry` 处的返工二 F1 注释与失败时序）。
        let mut registry = RegistryState::default();
        let pairing_entry = |generation: i64| TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_430_700_000,
                refresh_until: None,
            },
            prev: None,
        };

        // begin#1
        registry.set_pairing_entry(pairing_entry(10));
        registry.enqueue_token_put(pairing_entry(10), None);
        assert_eq!(registry.outbox.len(), 1);
        assert_eq!(registry.outbox[0].frame["t"], "token.put");

        // cancel（断线/退避态：不建模 request_registry_publish，只关心 outbox 状态）
        registry.clear_pairing_entry();
        registry.enqueue_token_delete("pairing".to_owned(), 11, true);
        assert_eq!(registry.outbox.len(), 1);
        assert_eq!(registry.outbox[0].frame["t"], "token.delete");
        assert_eq!(registry.outbox[0].generation, 11);

        // begin#2
        registry.set_pairing_entry(pairing_entry(12));
        registry.enqueue_token_put(pairing_entry(12), None);

        assert_eq!(
            registry.outbox.len(),
            1,
            "新一轮配对的 put 必须落进 outbox，不能被陈旧 delete 的豁免规则吞掉"
        );
        assert_eq!(
            registry.outbox[0].frame["t"], "token.put",
            "陈旧 delete 必须被 set_pairing_entry 作废，不能继续挂在 outbox 里等重连时被\
             重新领号打死刚开始的新一轮配对"
        );
        assert_eq!(registry.outbox[0].generation, 12);
        assert!(
            registry.pending_revoke_subjects().is_empty(),
            "作废后不该再有 pairing 的待送达 revoke，重连 absorb 不应再给它重新领号"
        );
    }

    #[test]
    fn remote_registry_snapshot_includes_only_unexpired_pairing_window() {
        let inner = test_inner_with_registry_providers(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 2,
                    entries: Vec::new(),
                })
            }),
            test_registry_rebase_provider(),
        );
        lock(&inner.registry).set_pairing_entry(TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: 1,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_000,
                refresh_until: None,
            },
            prev: None,
        });

        let active = registry_snapshot_for_send(&inner, "room", 999, None).unwrap();
        assert_eq!(active.entries.len(), 1);
        assert_eq!(active.entries[0].subject, "pairing");
        assert_eq!(active.entries[0].current.refresh_until, None);

        let expired = registry_snapshot_for_send(&inner, "room", 1_000, None).unwrap();
        assert!(expired.entries.is_empty());
    }

    #[test]
    fn remote_legacy_active_device_with_null_registry_metadata_aborts_sync_before_send() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let room_id = "0123456789abcdef0123456789abcdef";
        let device_id = "11111111-1111-4111-8111-111111111111";
        crate::db::insert_remote_device(
            &conn,
            device_id,
            Some(room_id),
            "",
            &"aa".repeat(32),
            &"bb".repeat(32),
            1_700_003_600_000,
            1_700_000_000,
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let db_for_snapshot = Arc::clone(&db);
        let inner = test_inner_with_registry_providers(
            Box::new(move |requested_room, now_ms| {
                crate::load_remote_registry_snapshot(
                    &lock(&db_for_snapshot),
                    requested_room,
                    now_ms,
                )
            }),
            Box::new(|_, _, _, _, _| panic!("rebase must not run")),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (received_tx, received_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            received_tx
                .send(matches!(socket.read(), Ok(Message::Text(_))))
                .unwrap();
        });
        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: room_id.to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let result = run_authenticated_connection(
            &inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([2_u8; 32])),
        );

        assert!(matches!(
            result,
            Err(ConnectionFailure::Other(ref error)) if error.contains("generation_missing")
        ));
        assert!(
            !received_rx.recv().unwrap(),
            "relay must receive no token.sync"
        );
        server.join().unwrap();
    }

    #[test]
    fn remote_registry_mock_relay_high_water_rebases_and_resends_before_activation() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            for relay_high_water in [10_i64, 12_i64] {
                let Message::Text(text) = socket.read().unwrap() else {
                    panic!("sync must be text");
                };
                let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                frames_tx.send(frame.clone()).unwrap();
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "t": "token.sync.ack",
                            "revision": frame["revision"],
                            "relay_high_water": relay_high_water,
                        })
                        .to_string()
                        .into(),
                    ))
                    .unwrap();
            }
            let _ = socket.read();
        });
        let rebase_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rebase_call_counter = Arc::clone(&rebase_calls);
        let initial_entry = TokenSyncEntry {
            subject: "device:11111111-1111-4111-8111-111111111111".to_owned(),
            generation: 1,
            scope: "remote".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_765_434_000_000,
                refresh_until: Some(1_768_022_400_000),
            },
            prev: Some(TokenSyncPrev {
                token_hash: "bb".repeat(32),
                generation: 7,
                prev_expires: 1_765_606_800_000,
            }),
        };
        let initial_for_snapshot = initial_entry.clone();
        let initial_for_rebase = initial_entry.clone();
        let inner = test_inner_with_registry_providers(
            Box::new(move |_, _| {
                Ok(RegistrySnapshot {
                    revision: 2,
                    entries: vec![initial_for_snapshot.clone()],
                })
            }),
            Box::new(move |_, high_water, _, include_pairing, _| {
                assert_eq!(high_water, 10);
                assert!(!include_pairing);
                rebase_call_counter.fetch_add(1, Ordering::Relaxed);
                let mut entry = initial_for_rebase.clone();
                entry.generation = 11;
                Ok((
                    RegistrySnapshot {
                        revision: 12,
                        entries: vec![entry],
                    },
                    None,
                    Vec::new(),
                ))
            }),
        );
        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection_config = config.clone();
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connection_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&Zeroizing::new([3_u8; 32])),
            )
        });

        wait_until_connected(&inner);
        assert!(inner.state.upstream_enabled_snapshot());
        let first = frames_rx.recv().unwrap();
        let second = frames_rx.recv().unwrap();
        assert_eq!(first["revision"], 2);
        assert_eq!(first["entries"][0]["generation"], 1);
        assert_eq!(second["revision"], 12);
        assert_eq!(second["entries"][0]["generation"], 11);
        assert_eq!(second["entries"][0]["prev"]["generation"], 7);
        assert_eq!(rebase_calls.load(Ordering::Relaxed), 1);

        inner.shutdown.store(true, Ordering::Release);
        assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
        server.join().unwrap();
    }

    #[test]
    fn remote_registry_ack_equal_revision_is_absorbed_before_activation() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release_ack_tx, release_ack_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("sync must be text");
            };
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            release_ack_rx.recv().unwrap();
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": frame["revision"],
                        "relay_high_water": 5,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            let _ = socket.read();
        });
        let next_generation = Arc::new(AtomicU64::new(5));
        let counter_for_ack = Arc::clone(&next_generation);
        let snapshot_provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_provider = Arc::clone(&snapshot_provider_calls);
        let inner = test_inner_with_registry_sync_providers(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 5,
                    entries: Vec::new(),
                })
            }),
            Box::new(|_, _, _, _, _| panic!("rebase must not run")),
            Box::new(move |_, high_water, _revoke_subjects: &[String]| {
                counter_for_ack.fetch_max((high_water + 1) as u64, Ordering::AcqRel);
                Ok(Vec::new())
            }),
            Box::new(move || {
                calls_for_provider.fetch_add(1, Ordering::Relaxed);
                None
            }),
        );
        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection_config = config.clone();
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connection_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&Zeroizing::new([4_u8; 32])),
            )
        });

        thread::sleep(Duration::from_millis(50));
        assert_ne!(lock(&inner.state.status).state, GatewayState::Connected);
        assert!(!inner.state.upstream_enabled_snapshot());
        assert_eq!(snapshot_provider_calls.load(Ordering::Relaxed), 0);
        release_ack_tx.send(()).unwrap();
        wait_until_connected(&inner);
        assert!(inner.state.upstream_enabled_snapshot());
        let claimed_generation = next_generation.fetch_add(1, Ordering::AcqRel);
        assert!(claimed_generation > 5);

        inner.shutdown.store(true, Ordering::Release);
        assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
        server.join().unwrap();
    }

    #[test]
    fn remote_registry_high_water_out_of_range_stops_without_persisting() {
        // S1i3 F2：relay 若回一个远超本地计数器的 relay_high_water（半可信/故障 relay，
        // 或纯粹的协议误用），必须 fail-closed 停机、绝不吸收进桌面计数器——否则
        // `bump_registry_counter_to_in_transaction` 会把 `next_generation` 抬到接近
        // `i64::MAX`，后续每次真正领号都会在 `checked_add(1)` 上溢出、永久
        // `IntegralValueOutOfRange`（配对开不了、设备撤不掉、refresh 全死），换回诚实
        // relay 也不恢复（损坏已经落进桌面 DB）。两个 panic 探针（rebase provider /
        // high_water provider）不是断言手段，是硬性前提：只要越界检查没有在两者之前
        // 拦下，这条测试本身就会因为探针触发而失败，比事后查询 DB 计数器更直接地证明
        // 「绝不落库」。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("initial sync must be text");
            };
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(frame["revision"], 100);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": frame["revision"],
                        "relay_high_water": 100_i64 + REGISTRY_HIGH_WATER_MAX_SPAN + 1,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            // 桌面判定越界后应当 fail-closed 放弃这条连接，不会再发别的帧——等它挂断
            // （读到错误/EOF）即可，不需要断言具体错误种类。
            let _ = socket.read();
        });

        let inner = test_inner_with_registry_sync_providers(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 100,
                    entries: Vec::new(),
                })
            }),
            Box::new(|_, _, _, _, _| {
                panic!("relay_high_water 越界必须在触发任何 rebase 之前就 fail-closed")
            }),
            Box::new(|_, high_water, _revoke_subjects: &[String]| {
                panic!(
                    "relay_high_water 越界（{high_water}）必须 fail-closed，绝不能吸收进桌面计数器"
                )
            }),
            Box::new(|| None),
        );
        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let result = run_authenticated_connection(
            &inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([9_u8; 32])),
        );

        match result {
            Err(ConnectionFailure::Stopped(reason)) => {
                assert_eq!(reason.code, REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON);
            }
            other => panic!("expected Stopped(registry_high_water_out_of_range)，got {other:?}"),
        }
        server.join().unwrap();
    }

    #[test]
    fn remote_terminal_stop_with_unacked_outbox_keeps_wait_for_reload_asleep() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let reconnect_probe_listener = listener.try_clone().unwrap();
        let accepted = Arc::new(AtomicU64::new(0));
        let server_accepted = Arc::clone(&accepted);
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server_accepted.fetch_add(1, Ordering::Relaxed);
            let mut socket = tungstenite::accept(stream).unwrap();
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("initial sync must be text");
            };
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(frame["revision"], 100);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": frame["revision"],
                        "relay_high_water": 100_i64 + REGISTRY_HIGH_WATER_MAX_SPAN + 1,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            // 桌面判定越界后应当 fail-closed 放弃这条连接，不会再发别的帧——等它挂断
            // （读到错误/EOF）即可，不需要断言具体错误种类。
            let _ = socket.read();
        });

        let relay_url = format!("ws://{address}");
        let settings_relay_url = relay_url.clone();
        let inner = test_inner_with_registry_sync_providers_and_settings(
            move |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some(settings_relay_url.clone()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            },
            Box::new(|_project_id| Ok("0123456789abcdef0123456789abcdef".to_owned())),
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 100,
                    entries: Vec::new(),
                })
            }),
            Box::new(|_, _, _, _, _| panic!("terminal high-water stop must not rebase")),
            Box::new(|_, _, _| panic!("terminal high-water stop must not persist")),
            Box::new(|| None),
        );
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: "pairing".to_owned(),
                generation: 8,
                scope: "pairing".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_430_700_000,
                    refresh_until: None,
                },
                prev: None,
            },
            None,
        );
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let loop_inner = Arc::clone(&inner);
        let gateway_thread = thread::spawn(move || {
            connect_loop(Arc::downgrade(&loop_inner), upstream_rx, milestone_rx)
        });

        wait_until_stopped_reason(&inner, REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON);
        server.join().unwrap();
        let (reconnect_accepted_tx, reconnect_accepted_rx) = mpsc::sync_channel(1);
        let (release_reconnect_probe_tx, release_reconnect_probe_rx) = mpsc::sync_channel(1);
        let reconnect_probe = thread::spawn(move || {
            let _connection = reconnect_probe_listener.accept().unwrap();
            reconnect_accepted_tx.send(()).unwrap();
            release_reconnect_probe_rx.recv().unwrap();
        });
        let reconnected = reconnect_accepted_rx
            .recv_timeout(BACKOFF_POLL_INTERVAL + Duration::from_millis(100))
            .is_ok();
        let accepted_count = accepted.load(Ordering::Relaxed) + u64::from(reconnected);
        let wake_rearmed = inner.registry_publish_wake.load(Ordering::Acquire);
        let stopped_reason = lock(&inner.state.status).stopped_reason.clone();

        inner.shutdown.store(true, Ordering::Release);
        if !reconnected {
            let _ = std::net::TcpStream::connect(address).unwrap();
        }
        release_reconnect_probe_tx.send(()).unwrap();
        reconnect_probe.join().unwrap();
        gateway_thread.join().unwrap();

        assert_eq!(
            accepted_count, 1,
            "terminal Stop must remain in wait_for_reload instead of reconnecting"
        );
        assert!(
            !wake_rearmed,
            "an unacked outbox must not rearm wake for a terminal Stop"
        );
        assert_eq!(
            stopped_reason.as_deref(),
            Some(REGISTRY_HIGH_WATER_OUT_OF_RANGE_STOP_REASON)
        );
    }

    #[test]
    fn remote_registry_high_water_at_max_span_boundary_is_absorbed_not_stopped() {
        // S1i3 K3.3：上面那条测试钉死了 `revision + REGISTRY_HIGH_WATER_MAX_SPAN + 1`
        // fail-closed；这条补另一半——恰好等于上界（`+1` 之前那个值）必须放行、正常吸收。
        // 判定用的是 `>`，不是 `>=`：只测「超一点必挂」不能防住有人手滑把 `>` 改成
        // `>=`（那样恰好等于上界也会被拦，仍然全绿）。用 registry_high_water_provider
        // 断言实际吸收到的 high_water 就是边界值本身，证明代码走过了那一行判断，不是
        // 靠事后猜。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let boundary_high_water = 100_i64 + REGISTRY_HIGH_WATER_MAX_SPAN; // 恰好等于上界
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            // 第一轮 sync：relay 报回恰好等于上界的 relay_high_water——必须被吸收，不停机。
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("initial sync must be text");
            };
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(frame["revision"], 100);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": frame["revision"],
                        "relay_high_water": boundary_high_water,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            // relay_high_water（边界值）> revision（100），桌面必须再发一轮 rebase
            // sync；这一轮直接把 relay_high_water 报成跟新 revision 相等，让循环收敛、
            // 连接进入 Connected 态，不需要再模拟第三轮。
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("rebase sync must be text");
            };
            let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(frame["revision"], boundary_high_water);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": frame["revision"],
                        "relay_high_water": boundary_high_water,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            let _ = socket.read();
        });

        let absorbed_high_water = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let absorbed_for_provider = Arc::clone(&absorbed_high_water);
        let inner = test_inner_with_registry_providers_and_high_water(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 100,
                    entries: Vec::new(),
                })
            }),
            Box::new(move |_, high_water, _, include_pairing, _| {
                assert_eq!(high_water, boundary_high_water);
                assert!(!include_pairing);
                Ok((
                    RegistrySnapshot {
                        revision: boundary_high_water,
                        entries: Vec::new(),
                    },
                    None,
                    Vec::new(),
                ))
            }),
            Box::new(move |_, high_water, _revoke_subjects: &[String]| {
                // 每次吸收都会调用；第一轮吸收的正是边界值本身——这才是本测试真正要
                // 证明的事：`>` 判定放行了它，代码走到了这里而不是提前 fail-closed
                // 返回（对照上一条测试：越界值会在这里 panic，走不到这一行）。
                absorbed_for_provider.store(high_water, Ordering::Relaxed);
                Ok(Vec::new())
            }),
        );

        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection_config = config.clone();
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connection_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&Zeroizing::new([9_u8; 32])),
            )
        });

        wait_until_connected(&inner);
        assert!(inner.state.upstream_enabled_snapshot());
        assert_eq!(
            absorbed_high_water.load(Ordering::Relaxed),
            boundary_high_water,
            "边界值必须被吸收进桌面计数器，而不是被上界判定拦下"
        );

        inner.shutdown.store(true, Ordering::Release);
        let _ = connection.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn remote_registry_reconnect_resends_unacked_put_after_sync_then_releases_ready() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            ack_initial_registry_sync(&mut socket);
            for _ in 0..2 {
                let Message::Text(text) = socket.read().unwrap() else {
                    panic!("registry frame must be text")
                };
                let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                frames_tx.send(frame.clone()).unwrap();
                if frame["t"] == "token.put" {
                    socket
                        .send(Message::Text(
                            serde_json::json!({
                                "t": "token.ack",
                                "subject": frame["subject"],
                                "generation": frame["generation"],
                                "result": "idempotent",
                            })
                            .to_string()
                            .into(),
                        ))
                        .unwrap();
                }
            }
            let _ = socket.close(None);
        });
        let inner = test_inner_with_registry_providers(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 7,
                    entries: Vec::new(),
                })
            }),
            test_registry_rebase_provider(),
        );
        let device_id = "11111111-1111-4111-8111-111111111111";
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: format!("device:{device_id}"),
                generation: 7,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            Some(PairReadyFrame {
                room: "0123456789abcdef0123456789abcdef".to_owned(),
                device_id: device_id.to_owned(),
                ct: "ready-ct".to_owned(),
                n: "ready-n".to_owned(),
            }),
        );
        lock(&inner.registry).outbox[0].attempts = 1;
        lock(&inner.registry).outbox[0].last_sent_at = Some(1);
        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let result = run_authenticated_connection(
            &inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([4_u8; 32])),
        );

        assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
        assert_eq!(frames_rx.recv().unwrap()["t"], "token.put");
        assert_eq!(frames_rx.recv().unwrap()["t"], "pair.ready");
        server.join().unwrap();
    }

    #[test]
    fn remote_registry_rejected_refresh_put_disconnects_so_reconnect_carries_db_truth() {
        // S1i1 返工三：refresh 轮换成功后挂着回执的 put 被 relay 判 `rejected`——桌面 DB
        // 已经不可逆地轮换到新代号（下面 provider 第二次调用起返回新代号 9），relay 那边还停在
        // 旧代号（第一次调用返回代号 5，就是这条连接建立时 relay 学到的状态）。返工二在这条
        // 存活连接里原地重发一次 token.sync 换收敛，被评审判定 BLOCKER（等 ack 期间会把 relay
        // 直投的在线 input 帧当协议违规吞掉，本文件另一条回归测试专门钉这一点）。返工三改为：
        // 回一帧 fail 之后主动断开，让**下一次连接**（既有重连路径）的首次 sync 把 DB 真相
        // 交给 relay，之后手机同 request_id 的重试才能命中 relay §9.6 第 246 行「回执.generation
        // == subject 当前 generation」的投递谓词。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let device_id = "66666666-6666-4666-8666-666666666666";
        let subject = format!("device:{device_id}");
        let new_generation = 9;
        let request_id = "req-resync-1";

        let server_subject = subject.clone();
        let server_request_id = request_id.to_owned();
        let server = thread::spawn(move || {
            // ── 第一条连接：refresh 轮换的 put 被判 rejected ──
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            // 连接建立时的首次 sync：relay 学到 subject 还停在旧代号 5。
            ack_initial_registry_sync(&mut socket);

            // 首次 sync 之后，outbox 里挂着 refresh 回执的 put（代号 9，来自
            // enqueue_token_put_for_refresh，见下方 registry 预置）被正常 drain 出来。
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("token.put must be text");
            };
            let put_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(put_frame["t"], "token.put");
            assert_eq!(put_frame["subject"], server_subject);
            assert_eq!(put_frame["generation"], new_generation);
            frames_tx.send(put_frame).unwrap();

            // relay 判 rejected（模拟它手上仍是代号 5 的旧注册表，拒绝了这次代号 9 的 put）。
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.ack",
                        "subject": server_subject,
                        "generation": new_generation,
                        "result": "rejected",
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            // 桌面回一帧 fail{put_rejected}（不带 close）——R2 既有行为，本轮不许削弱。
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("refresh fail must be text");
            };
            let fail_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(fail_frame["t"], "token.refresh.fail");
            assert_eq!(fail_frame["reason"], "put_rejected");
            assert_eq!(fail_frame["request_id"], server_request_id);
            assert!(
                fail_frame.get("close").is_none(),
                "R2 既有行为：put_rejected 自愈帧不带 close，不许被本轮削弱"
            );
            frames_tx.send(fail_frame).unwrap();

            // 返工三本体：桌面**不**在这条连接上重发 sync——relay 侧再读不到任何后续应用帧，
            // 只会看到桌面主动断开这条连接（EOF/关闭），不是收到一帧第二次 sync。
            let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
            assert!(
                closed,
                "返工三：处理完 rejected 的 refresh put 之后必须主动断开这条连接，\
                 不能在原地等第二次 sync.ack"
            );

            // ── 第二条连接：既有重连路径的首次 sync 必须带上 DB 当前真相（新代号）──
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let resync_frame = ack_initial_registry_sync(&mut socket);
            frames_tx.send(resync_frame).unwrap();
            let _ = socket.close(None);
        });

        let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_calls_for_closure = provider_calls.clone();
        let provider_subject = subject.clone();
        let inner = test_inner_with_registry_providers(
            Box::new(move |_, _| {
                let call = provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
                if call == 0 {
                    // 第一条连接建立时的首次 sync：DB 快照仍是这次 refresh 发生之前的旧状态。
                    Ok(RegistrySnapshot {
                        revision: 7,
                        entries: vec![TokenSyncEntry {
                            subject: provider_subject.clone(),
                            generation: 5,
                            scope: "remote".to_owned(),
                            current: TokenSyncCurrent {
                                token_hash: "aa".repeat(32),
                                access_expires: 1_765_000_000_000,
                                refresh_until: Some(1_768_000_000_000),
                            },
                            prev: None,
                        }],
                    })
                } else {
                    // 第二条连接（重连）建立时的首次 sync：provider 重新读 DB，这次看到的
                    // 已经是轮换后的真相（代号 9）——真实实现里 provider 就是读当前 DB 行，
                    // 这里用调用计数模拟「两次连接之间 DB 状态推进了」。
                    Ok(RegistrySnapshot {
                        revision: new_generation,
                        entries: vec![TokenSyncEntry {
                            subject: provider_subject.clone(),
                            generation: new_generation,
                            scope: "remote".to_owned(),
                            current: TokenSyncCurrent {
                                token_hash: "bb".repeat(32),
                                access_expires: 1_765_434_000_000,
                                refresh_until: Some(1_768_022_400_000),
                            },
                            prev: None,
                        }],
                    })
                }
            }),
            test_registry_rebase_provider(),
        );

        lock(&inner.registry).enqueue_token_put_for_refresh(
            TokenSyncEntry {
                subject: subject.clone(),
                generation: new_generation,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            RefreshOkFrame {
                request_id: request_id.to_owned(),
                subject: subject.clone(),
                generation: new_generation,
                ct: "refresh-ct".to_owned(),
                n: "refresh-n".to_owned(),
            },
        );

        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);

        // 第一条连接：处理 rejected 的 refresh put 之后必须在有限时间内主动断开——用
        // `join_connection_within` 把「连接是否真的结束」变成一条硬断言，不靠超时/panic 兜底
        // （G2 变异自证：把下方收敛动作临时改回 no-op，这条 `assert!(finished_in_time, ...)`
        // 会先于其它断言干净地失败）。
        let (_upstream_tx_1, upstream_rx_1) = mpsc::sync_channel(1);
        let (_milestone_tx_1, milestone_rx_1) = mpsc::sync_channel(1);
        let connection_inner = Arc::clone(&inner);
        let connection_url = url.clone();
        let connection_config = config.clone();
        let connection = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner,
                &connection_url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connection_config,
                None,
                &upstream_rx_1,
                &milestone_rx_1,
                Some(&Zeroizing::new([6_u8; 32])),
            )
        });
        let result = join_connection_within(connection, &inner);
        assert_eq!(
            result,
            Err(ConnectionFailure::Other(
                "refresh put rejected; reconnecting to resync registry".to_owned()
            )),
            "rejected 且挂着 refresh 回执的 put 之后必须是一次可重试的断开（Other），\
             不能变成 Stopped 那种硬停，也不能在原地等第二次 ack"
        );

        let put_frame = frames_rx.recv().unwrap();
        assert_eq!(put_frame["t"], "token.put");

        let fail_frame = frames_rx.recv().unwrap();
        assert_eq!(fail_frame["t"], "token.refresh.fail");

        // 第二条连接：既有重连路径（`attempt_once` 重新调 `run_connection_request`）的首次
        // sync 必须带上 DB 当前真相（新代号），不是第一条连接建立时那次 sync 还带着的旧代号。
        let (_upstream_tx_2, upstream_rx_2) = mpsc::sync_channel(1);
        let (_milestone_tx_2, milestone_rx_2) = mpsc::sync_channel(1);
        let connection_inner_2 = Arc::clone(&inner);
        let connection_url_2 = url.clone();
        let connection_config_2 = config.clone();
        let connection_2 = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner_2,
                &connection_url_2,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connection_config_2,
                None,
                &upstream_rx_2,
                &milestone_rx_2,
                Some(&Zeroizing::new([6_u8; 32])),
            )
        });
        let result_2 = join_connection_within(connection_2, &inner);
        assert_eq!(result_2, Ok(ConnectionExit::ClosedByPeer));

        let resync_frame = frames_rx.recv().unwrap();
        assert_eq!(
            resync_frame["t"], "token.sync",
            "断开之后，下一次连接的首次 sync 就是既有重连路径本身，必须真实发生"
        );
        assert_eq!(resync_frame["revision"], new_generation);
        let entries = resync_frame["entries"].as_array().unwrap();
        let synced_entry = entries
            .iter()
            .find(|entry| entry["subject"] == subject)
            .expect("reconnect sync entries must include the rotated subject");
        assert_eq!(
            synced_entry["generation"], new_generation,
            "重连后的 token.sync 必须携带 DB 当前代号（轮换后的新代号），\
             不能还是第一条连接建立时那次 sync 的旧代号"
        );
        assert!(
            provider_calls.load(Ordering::Relaxed) >= 2,
            "重连必须真的再打一次 registry_snapshot_provider（重新读 DB 真相），\
             不能复用第一条连接缓存的旧快照"
        );

        server.join().unwrap();
    }

    #[test]
    fn remote_registry_rejected_refresh_put_disconnect_does_not_drop_relay_pushed_input() {
        // S1i1 返工三丢帧回归测试（评审点名要求）：relay 判 rejected 之后、不等桌面任何响应，
        // 紧接着直投一帧在线 input——这正是评审描述的「插在收敛动作之间」的危险位置。返工二
        // 的老实现在这里会去原地等第二次 sync.ack，这帧会被 `read_registry_sync_ack` 当协议
        // 违规吞掉、永久静默丢失（从未进 `handle_frame`、没有本地落账、没有回 ack）。返工三
        // 的新实现只用同一套 `match socket.read()` 分发继续正常处理——这帧必须被正常处理
        // （落账 + 回 input.ack），断开只发生在这一轮真正安静下来之后。这条测试就是钉死
        // 「不要再回到原地等 ack」这个结论的核心验收。
        //
        // S1i1 返工四 H2：光凭「收到 command_id 匹配的 input.ack」分不清「真的解密并派发到
        // 业务 handler」与「解密/解析就先失败了」——两条路径都会产出同一个 command_id 的
        // `input.ack`（解密/解析失败见 `handle_command_envelope` 的 `failed()` 早退，固定回
        // `AckOutcome::Failed`；默认 fixture handler 本身也固定返回 `Some(AckOutcome::Failed)`，
        // 两者长得一模一样）。这里改用可注入的 fixture handler，记调用次数 + 收到的
        // command_id/text，并返回一个跟失败路径可区分的 outcome（`AckOutcome::Ok`，
        // `input.ack.outcome` 会是 `"ok"` 而不是解密失败路径固定吐出的 `"failed"`）。
        let k_room = Zeroizing::new([42_u8; 32]);
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let device_id = "77777777-7777-4777-8777-777777777777";
        let subject = format!("device:{device_id}");
        let new_generation = 11;
        let request_id = "req-input-race-1";
        let command_id = "cmd-input-race-1";

        let server_subject = subject.clone();
        let server_request_id = request_id.to_owned();
        let server_room_id = room_id.clone();
        let server_k_room = k_room.clone();
        let server_command_id = command_id.to_owned();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            ack_initial_registry_sync(&mut socket);

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("token.put must be text");
            };
            let put_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(put_frame["t"], "token.put");
            assert_eq!(put_frame["subject"], server_subject);
            assert_eq!(put_frame["generation"], new_generation);

            // relay 判 rejected——桌面即将进入「该收敛了」的状态。
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.ack",
                        "subject": server_subject,
                        "generation": new_generation,
                        "result": "rejected",
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            // 不等桌面任何回应，紧接着直投一帧在线 input——正是评审描述的「插在收敛动作之间」
            // 的位置。
            let envelope = seal_command_envelope(
                &server_k_room,
                &server_room_id,
                7,
                "input",
                "s-race",
                &server_command_id,
                &serde_json::json!({
                    "t": "input.send",
                    "session": "s-race",
                    "text": "hello from race",
                }),
            );
            socket
                .send(Message::Text(envelope.to_string().into()))
                .unwrap();

            // 两帧都必须在同一条连接上被正常处理：先看到 fail（不带 close），再看到
            // input.ack——证明「正要收敛」的这一刻，relay 紧跟着送来的帧没有被当协议违规吞掉。
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("refresh fail must be text");
            };
            let fail_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(fail_frame["request_id"], server_request_id);
            frames_tx.send(fail_frame).unwrap();

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("input ack must be text");
            };
            let input_ack: Value = serde_json::from_str(text.as_ref()).unwrap();
            frames_tx.send(input_ack).unwrap();

            // 不再送任何东西——桌面必须在这一轮真正安静下来之后自己断开（既有重连路径接手）。
            let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
            assert!(
                closed,
                "处理完 relay 插进来的 input 帧之后，桌面仍必须完成收敛断开——不能因为多处理了\
                 一帧就卡住不断"
            );
        });

        let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_calls_for_closure = provider_calls.clone();
        let provider_subject = subject.clone();
        // S1i1 返工四 H2：记业务 handler 真实被调用的次数与收到的内容——跟失败路径的固定
        // `Failed` outcome区分开。
        let input_handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let input_handler_calls_for_closure = input_handler_calls.clone();
        let input_handler_seen: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
        let input_handler_seen_for_closure = input_handler_seen.clone();
        let inner = test_inner_with_registry_providers_and_input_handler(
            Box::new(move |_, _| {
                let call = provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
                let (revision, entry_generation): (i64, i64) = if call == 0 {
                    (7, 5)
                } else {
                    (new_generation, new_generation)
                };
                Ok(RegistrySnapshot {
                    revision,
                    entries: vec![TokenSyncEntry {
                        subject: provider_subject.clone(),
                        generation: entry_generation,
                        scope: "remote".to_owned(),
                        current: TokenSyncCurrent {
                            token_hash: "aa".repeat(32),
                            access_expires: 1_765_000_000_000,
                            refresh_until: Some(1_768_000_000_000),
                        },
                        prev: None,
                    }],
                })
            }),
            test_registry_rebase_provider(),
            move |frame: InputSendFrame| {
                input_handler_calls_for_closure.fetch_add(1, Ordering::Relaxed);
                *lock(&input_handler_seen_for_closure) =
                    Some((frame.command_id.clone(), frame.text.clone()));
                Some(AckOutcome::Ok)
            },
        );

        lock(&inner.registry).enqueue_token_put_for_refresh(
            TokenSyncEntry {
                subject: subject.clone(),
                generation: new_generation,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            RefreshOkFrame {
                request_id: request_id.to_owned(),
                subject: subject.clone(),
                generation: new_generation,
                ct: "refresh-ct".to_owned(),
                n: "refresh-n".to_owned(),
            },
        );

        // M2-4d：`run_connection_request` 建连时会用这份 config 的 active_repo_id 覆盖
        // `active_repo_id_for_gating`（`with_default_active_repo` 建的初值会被这里盖掉），必须
        // 跟 builder 那份默认值一致，不然 "s-race" 的 input 帧会被归属闸 fail-closed 挡下。
        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: room_id.clone(),
            active_repo_id: Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned()),
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let connection_inner = Arc::clone(&inner);
        let connection_url = url.clone();
        let connection_config = config.clone();
        let connection_k_room = k_room.clone();
        let connection = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner,
                &connection_url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connection_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&connection_k_room),
            )
        });
        let result = join_connection_within(connection, &inner);
        assert_eq!(
            result,
            Err(ConnectionFailure::Other(
                "refresh put rejected; reconnecting to resync registry".to_owned()
            )),
            "本轮循环最终仍必须走到可重试断开——多处理一帧 input 不该改变收敛结论"
        );

        let fail_frame = frames_rx.recv().unwrap();
        assert_eq!(fail_frame["t"], "token.refresh.fail");
        assert_eq!(fail_frame["reason"], "put_rejected");
        assert!(
            fail_frame.get("close").is_none(),
            "put_rejected 自愈帧不带 close，不许被本轮削弱"
        );

        let input_ack = frames_rx.recv().unwrap();
        assert_eq!(
            input_ack["t"], "input.ack",
            "relay 紧跟着 rejected ack 直投的在线 input 帧，必须像平时一样落账并回 \
             input.ack——不能因为桌面正要收敛就静默丢弃"
        );
        assert_eq!(input_ack["command_id"], command_id);
        // S1i1 返工四 H2：outcome 必须是业务 handler 返回的 "ok"，不是解密/解析失败路径
        // （`handle_command_envelope` 的 `failed()` 早退）固定吐出的 "failed"——否则这条
        // ack 分不清是「真的派发到业务 handler」还是「半路解密就失败了」。
        assert_eq!(
            input_ack["outcome"], "ok",
            "input.ack 的 outcome 必须是业务 handler 返回的值，不能跟解密失败路径撞成一样的 \
             \"failed\""
        );
        assert_eq!(
            input_handler_calls.load(Ordering::Relaxed),
            1,
            "业务 handler（input_send_handler）必须真的被调用恰好一次"
        );
        assert_eq!(
            lock(&input_handler_seen).clone(),
            Some((command_id.to_owned(), "hello from race".to_owned())),
            "业务 handler 收到的 command_id/text 必须与 relay 投递的一致"
        );

        server.join().unwrap();
    }

    #[test]
    fn remote_registry_resync_pending_disconnects_within_hard_deadline_when_relay_stays_busy() {
        // S1i1 返工四 H1「太懒」钉死：rejected 之后 relay 持续以 < `READ_TIMEOUT`（500ms）的
        // 间隔投帧——这里用 unsolicited Pong，帧类型不影响结论（判据只认「这一轮读超时与否」，
        // 不区分 Text/Ping/Pong），`socket.read()` 因此永远不会超时、`read_timed_out_this_round`
        // 恒假。若断开判据漏掉硬截止分支（只剩 `read_timed_out_this_round`），这条连接会被
        // 持续喂着、永远不会主动断开——`join_connection_within_budget` 的预算耗尽会先于任何
        // 其它断言干净地失败。**变异自证**：把 H1 判据里的 `registry_resync_deadline_elapsed`
        // 这一项去掉，这条测试必须红。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let device_id = "88888888-8888-4888-8888-888888888888";
        let subject = format!("device:{device_id}");
        let new_generation = 13;
        let request_id = "req-busy-deadline-1";

        let server_subject = subject.clone();
        let server_request_id = request_id.to_owned();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            ack_initial_registry_sync(&mut socket);

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("token.put must be text");
            };
            let put_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(put_frame["t"], "token.put");
            assert_eq!(put_frame["subject"], server_subject);
            assert_eq!(put_frame["generation"], new_generation);

            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.ack",
                        "subject": server_subject,
                        "generation": new_generation,
                        "result": "rejected",
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            let fail_frame = recv_text_skip_control(&mut socket);
            assert_eq!(fail_frame["t"], "token.refresh.fail");
            assert_eq!(fail_frame["request_id"], server_request_id);

            // 持续以 100ms 间隔投未经请求的 Pong——总时长（60 * 100ms = 6 秒）比 2 秒硬截止
            // 长得多，逼真模拟「一直有帧到达、读永不超时」的繁忙连接。客户端一旦按硬截止
            // 主动断开，这里的 send 会因管道破裂报错，静默跳出即可——断言本体不靠这个提前
            // 退出，靠外面对 `join_connection_within_budget` 结果的检查。
            for _ in 0..60 {
                if socket.send(Message::Pong(Vec::new().into())).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }

            let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
            assert!(
                closed,
                "繁忙连接也必须在硬截止内主动断开——不能被持续到达的帧无限期拖住"
            );
        });

        let provider_subject = subject.clone();
        let inner = test_inner_with_registry_providers(
            Box::new(move |_, _| {
                Ok(RegistrySnapshot {
                    revision: 7,
                    entries: vec![TokenSyncEntry {
                        subject: provider_subject.clone(),
                        generation: 5,
                        scope: "remote".to_owned(),
                        current: TokenSyncCurrent {
                            token_hash: "aa".repeat(32),
                            access_expires: 1_765_000_000_000,
                            refresh_until: Some(1_768_000_000_000),
                        },
                        prev: None,
                    }],
                })
            }),
            test_registry_rebase_provider(),
        );

        lock(&inner.registry).enqueue_token_put_for_refresh(
            TokenSyncEntry {
                subject: subject.clone(),
                generation: new_generation,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            RefreshOkFrame {
                request_id: request_id.to_owned(),
                subject: subject.clone(),
                generation: new_generation,
                ct: "refresh-ct".to_owned(),
                n: "refresh-n".to_owned(),
            },
        );

        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&Zeroizing::new([6_u8; 32])),
            )
        });

        // 预算给足 3 秒：正确实现在 ~2 秒硬截止后很快断开（连接建立 + 处理 rejected 帧的
        // 前置耗时可忽略不计），3 秒预算留了充分余量；一旦硬截止分支被去掉，60 帧 * 100ms
        // = 6 秒的持续投帧在 3 秒预算耗尽那一刻仍在继续，`finished_in_time` 必然是 false。
        let result = join_connection_within_budget(connection, &inner, Duration::from_secs(3));
        assert_eq!(
            result,
            Err(ConnectionFailure::Other(
                "refresh put rejected; reconnecting to resync registry".to_owned()
            )),
            "繁忙连接最终仍必须走到可重试断开，且必须在硬截止预算内完成"
        );

        server.join().unwrap();
    }

    #[test]
    fn remote_registry_resync_pending_ping_does_not_disconnect_before_pending_text() {
        // S1i1 返工四 H1「太急」钉死：rejected 之后 relay 先发一帧 Ping、紧接着（不等桌面
        // 任何响应）发一帧业务 Text（在线 input）——如果断开判据仍是返工三的
        // `!frame_delivered`，Ping 那一轮 `frame_delivered` 是假，会被当成「安静」立刻断开，
        // 永远读不到紧跟在后面、已经在缓冲区里的 Text 帧：`handle_frame` 从未被调用、没有
        // 本地落账、没有回 ack。修法把判据换成「这一轮 `socket.read()` 真的读超时了」——Ping
        // 那一轮不是超时，继续用同一套 `match` 正常处理，后面的 Text 帧必须被落账 + 回 ack。
        // **变异自证**：把判据改回 `!frame_delivered`（丢掉 `read_timed_out_this_round`
        // 语义），这条测试必须红。
        let k_room = Zeroizing::new([44_u8; 32]);
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let device_id = "99999999-9999-4999-8999-999999999999";
        let subject = format!("device:{device_id}");
        let new_generation = 15;
        let request_id = "req-ping-not-quiet-1";
        let command_id = "cmd-ping-not-quiet-1";

        let server_subject = subject.clone();
        let server_request_id = request_id.to_owned();
        let server_room_id = room_id.clone();
        let server_k_room = k_room.clone();
        let server_command_id = command_id.to_owned();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            ack_initial_registry_sync(&mut socket);

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("token.put must be text");
            };
            let put_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(put_frame["t"], "token.put");
            assert_eq!(put_frame["subject"], server_subject);
            assert_eq!(put_frame["generation"], new_generation);

            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.ack",
                        "subject": server_subject,
                        "generation": new_generation,
                        "result": "rejected",
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            // 先发一帧 Ping，紧接着（不等桌面任何响应）发业务 input 帧——评审描述的「太急」
            // 场景：判据若仍按「Ping 这一轮没收到 Text」算安静，会在这里就地断开，永远读不到
            // 后面这帧 Text。
            socket.send(Message::Ping(Vec::new().into())).unwrap();
            let envelope = seal_command_envelope(
                &server_k_room,
                &server_room_id,
                7,
                "input",
                "s-ping",
                &server_command_id,
                &serde_json::json!({
                    "t": "input.send",
                    "session": "s-ping",
                    "text": "hello after ping",
                }),
            );
            socket
                .send(Message::Text(envelope.to_string().into()))
                .unwrap();

            // 两帧都必须在同一条连接上被正常处理：先看到 fail（不带 close），再看到
            // input.ack——证明 Ping 那一轮没有触发过早断开。用 skip-control 读法容错客户端
            // 对服务端 Ping 的自动 Pong 回复可能夹在中间到达。
            let fail_frame = recv_text_skip_control(&mut socket);
            assert_eq!(fail_frame["t"], "token.refresh.fail");
            assert_eq!(fail_frame["request_id"], server_request_id);
            frames_tx.send(fail_frame).unwrap();

            let input_ack = recv_text_skip_control(&mut socket);
            frames_tx.send(input_ack).unwrap();

            // 不再送任何东西——桌面必须在这一轮真正安静下来之后（真读超时）才自己断开。
            let closed = matches!(socket.read(), Err(_) | Ok(Message::Close(_)));
            assert!(
                closed,
                "处理完 Ping 和紧随其后的业务帧之后，桌面仍必须完成收敛断开"
            );
        });

        let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_calls_for_closure = provider_calls.clone();
        let provider_subject = subject.clone();
        let inner = test_inner_with_registry_providers(
            Box::new(move |_, _| {
                let call = provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
                let (revision, entry_generation): (i64, i64) = if call == 0 {
                    (7, 5)
                } else {
                    (new_generation, new_generation)
                };
                Ok(RegistrySnapshot {
                    revision,
                    entries: vec![TokenSyncEntry {
                        subject: provider_subject.clone(),
                        generation: entry_generation,
                        scope: "remote".to_owned(),
                        current: TokenSyncCurrent {
                            token_hash: "aa".repeat(32),
                            access_expires: 1_765_000_000_000,
                            refresh_until: Some(1_768_000_000_000),
                        },
                        prev: None,
                    }],
                })
            }),
            test_registry_rebase_provider(),
        );

        lock(&inner.registry).enqueue_token_put_for_refresh(
            TokenSyncEntry {
                subject: subject.clone(),
                generation: new_generation,
                scope: "remote".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "bb".repeat(32),
                    access_expires: 1_765_434_000_000,
                    refresh_until: Some(1_768_022_400_000),
                },
                prev: None,
            },
            RefreshOkFrame {
                request_id: request_id.to_owned(),
                subject: subject.clone(),
                generation: new_generation,
                ct: "refresh-ct".to_owned(),
                n: "refresh-n".to_owned(),
            },
        );

        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: room_id.clone(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let connection_inner = Arc::clone(&inner);
        let connection_k_room = k_room.clone();
        let connection = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&connection_k_room),
            )
        });
        let result = join_connection_within(connection, &inner);
        assert_eq!(
            result,
            Err(ConnectionFailure::Other(
                "refresh put rejected; reconnecting to resync registry".to_owned()
            )),
            "Ping 之后本轮循环最终仍必须走到可重试断开"
        );

        let fail_frame = frames_rx.recv().unwrap();
        assert_eq!(fail_frame["t"], "token.refresh.fail");
        assert_eq!(fail_frame["reason"], "put_rejected");

        let input_ack = frames_rx.recv().unwrap();
        assert_eq!(
            input_ack["t"], "input.ack",
            "Ping 之后紧跟着的业务 input 帧必须被正常处理并回 input.ack——不能因为 Ping 那一轮 \
             被误判成安静就提前断开、永远读不到这帧"
        );
        assert_eq!(input_ack["command_id"], command_id);

        server.join().unwrap();
    }

    #[test]
    fn remote_registry_absorb_floor_is_fail_closed_when_relay_high_water_below_revision() {
        // S1h 返工二 F2：`relay_high_water` 协议上没有下界校验——`read_registry_sync_ack` 只
        // 查 `>= 0`。一个协议外的 relay 若回了比本次 sync revision 还低的 H，
        // `synchronize_registry` 传给 `absorb_registry_high_water_and_rearm_revokes` 的 floor
        // 不能原样信它，必须跟 `snapshot.revision` 取 max，否则会在本地计数器等于 revision
        // 时把新代号退化成等于 revision，撞上 relay「同代号比 fingerprint」分支，永久
        // rejected。这里 revision(10) > relay_high_water(8)：H(8) <= revision(10) 让循环在
        // 第一轮就返回（不牵扯 rebase，rebase provider 故意 panic 钉死这一点），新代号必须
        // 严格大于 revision（10），不能只满足严格大于 relay_high_water（8+1=9 会被下面的
        // 断言当场抓到——那正是 floor 没有跟 revision 取 max 时会产出的值）。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("sync must be text");
            };
            let sync_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(sync_frame["revision"], 10);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": sync_frame["revision"],
                        "relay_high_water": 8,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("delete resend must be text");
            };
            let delete_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            frames_tx.send(delete_frame.clone()).unwrap();
            let _ = socket.close(None);
        });

        let device_id = "44444444-4444-4444-8444-444444444444";
        let subject = format!("device:{device_id}");
        let inner = test_inner_with_registry_providers_and_high_water(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 10,
                    entries: Vec::new(),
                })
            }),
            Box::new(|_, _, _, _, _| {
                panic!("rebase must not run: relay_high_water(8) <= revision(10)")
            }),
            Box::new(|_, high_water, revoke_subjects: &[String]| {
                Ok(revoke_subjects
                    .iter()
                    .map(|subject| (subject.clone(), high_water + 1))
                    .collect())
            }),
        );
        lock(&inner.registry).enqueue_token_delete(subject.clone(), 3, true);

        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let result = run_authenticated_connection(
            &inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([7_u8; 32])),
        );

        assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
        let delete_frame = frames_rx.recv().unwrap();
        assert_eq!(delete_frame["t"], "token.delete");
        assert_eq!(delete_frame["subject"], subject);
        let sent_generation = delete_frame["generation"].as_i64().unwrap();
        assert!(
            sent_generation > 10,
            "relay_high_water（8）< revision（10）时新代号仍必须严格大于 revision，\
             实际 {sent_generation}"
        );
        assert_ne!(
            sent_generation, 9,
            "9 = relay_high_water(8)+1，意味着 floor 没有跟 revision 取 max——正是本刀要堵的\
             fail-open 回归"
        );
        server.join().unwrap();
    }

    #[test]
    fn remote_registry_rejected_revoke_rearmed_and_resent_after_reconnect_sync_ack() {
        // S1h 返工二 F3：§9.3「revoke 独立重试直到 ack」在跨重连这一半——delete 被 relay 回
        // rejected 后不是永久停发；下一次连接的 sync.ack 后（`absorb_registry_high_water_and_
        // rearm_revokes`，每轮 sync.ack 后都会跑，不需要触发 rebase）必须被重新武装（rejected
        // 清掉、换新代号、last_sent_at 清空）并重发。（活连接内 rejected 的 put/代号类拒绝不
        // 重试是另一件事——复审已判定结构性不可达，本单不修，Lead 记 BACKLOG。）
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let sync_frame = ack_initial_registry_sync(&mut socket);
            assert_eq!(sync_frame["revision"], 10);

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("delete resend must be text");
            };
            let delete_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            frames_tx.send(delete_frame.clone()).unwrap();
            let _ = socket.close(None);
        });

        let device_id = "55555555-5555-4555-8555-555555555555";
        let subject = format!("device:{device_id}");
        let inner = test_inner_with_registry_providers_and_high_water(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 10,
                    entries: Vec::new(),
                })
            }),
            Box::new(|_, _, _, _, _| {
                panic!("rebase must not run: relay_high_water(10) <= revision(10)")
            }),
            Box::new(|_, high_water, revoke_subjects: &[String]| {
                Ok(revoke_subjects
                    .iter()
                    .map(|subject| (subject.clone(), high_water + 1))
                    .collect())
            }),
        );
        // 这条 delete 曾经真的发出去过一次，被 relay 拒绝——rejected=true，不是「从未送达」。
        lock(&inner.registry).enqueue_token_delete(subject.clone(), 5, true);
        assert_eq!(
            lock(&inner.registry).consume_token_ack(&subject, 5, "rejected"),
            TokenAckAction::Rejected
        );
        assert!(lock(&inner.registry).outbox_snapshot_for_test()[0].rejected);

        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let result = run_authenticated_connection(
            &inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([8_u8; 32])),
        );

        assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
        let delete_frame = frames_rx.recv().unwrap();
        assert_eq!(delete_frame["t"], "token.delete");
        assert_eq!(delete_frame["subject"], subject);
        let sent_generation = delete_frame["generation"].as_i64().unwrap();
        assert_ne!(sent_generation, 5, "rejected 的旧代号不能原样重发");
        assert!(sent_generation > 10);

        let outbox = lock(&inner.registry).outbox_snapshot_for_test();
        assert_eq!(
            outbox.len(),
            1,
            "relay 还没 ack 这次重发，revoke 项仍应留在 outbox 里"
        );
        assert!(
            !outbox[0].rejected,
            "跨重连 sync.ack 后 rejected 必须被清掉，否则 drain 的发送闸门永远不会放行"
        );
        server.join().unwrap();
    }

    #[test]
    fn remote_registry_revoke_reconnect_resends_delete_with_generation_above_sync_revision() {
        // S1h R4 返工：现有两条 revoke 单测都绕过了「sync → revision 墓碑 → 代号比较」这段真实
        // 链路——一条（rebase_reissues_with_new_generation）直接把 rebase 后的新代号注入，另一
        // 条（reconnect_rearms_unacked_item_for_resend）只调 `prepare_outbox_for_reconnect`，
        // 都没有真正走一遍连接。这里补一条连接级测试，复刻 S1h §9.3 证据链②-④描述的场景：
        // 撤销时最初领到的代号（5）早于这次重连要发的 sync revision（10）。
        //
        // S1h 返工二 F4：原版本让 relay_high_water 跟 revision 恒等（10=10），于是「只按
        // revision+1 领号、彻底忽略 relay_high_water」的错误实现也能巧合地满足唯一那条
        // 「严格大于」断言——两个上界重合就测不出谁被忽略了。这里改成 revision(10) 与
        // relay_high_water(12) 取不同值且 H > revision：这在真实 `synchronize_registry`
        // 里会如实触发一次 rebase（`relay_high_water > snapshot.revision` 时循环不会在第一轮
        // 就返回），所以 mock relay 也要如实走完第二轮 sync/ack，而不是回避它——第二轮
        // relay_high_water(12) 与 rebase 后的 revision(12) 相等，循环到此正常收敛，不再牵扯
        // 第三轮。rebase provider 这次不再 panic：它必须存在且被真实调用一次，只是刻意不去
        // 重新领 revoke 代号（`revoke_generations` 传空 `Vec`），把「新代号是否正确纳入两个
        // 不同上界」这件事完全留给 `registry_high_water_provider`（`absorb_registry_high_
        // water_and_rearm_revokes` 每轮 sync.ack 后都会调它）去回答。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("initial sync must be text");
            };
            let sync_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(sync_frame["revision"], 10);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": sync_frame["revision"],
                        "relay_high_water": 12,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            // relay_high_water(12) > revision(10)：真实客户端必须再发一轮 rebase sync 才能
            // 推进，这里如实模拟 relay 侧对应的第二轮应答（回同一代号 12，循环到此收敛）。
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("rebase sync must be text");
            };
            let rebase_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(rebase_frame["revision"], 12);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.sync.ack",
                        "revision": rebase_frame["revision"],
                        "relay_high_water": 12,
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            let Message::Text(text) = socket.read().unwrap() else {
                panic!("delete resend must be text");
            };
            let delete_frame: Value = serde_json::from_str(text.as_ref()).unwrap();
            frames_tx.send(delete_frame.clone()).unwrap();

            socket
                .send(Message::Text(
                    serde_json::json!({
                        "t": "token.ack",
                        "subject": delete_frame["subject"],
                        "generation": delete_frame["generation"],
                        "result": "ok",
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            let _ = socket.close(None);
        });

        let device_id = "11111111-1111-4111-8111-111111111111";
        let subject = format!("device:{device_id}");
        // S1h 收尾：记录每次 `registry_high_water_provider` 被调用时收到的 floor 入参——
        // 正确实现（floor = relay_high_water.max(snapshot.revision)）两轮都应传 12；若生产
        // 代码把 floor 错改成只用 `snapshot.revision`，第一轮会传成 10，序列会变成
        // `[10, 12]`。最终 `sent_generation` 只看最后一轮（两种实现最后一轮都恰好是
        // 12），所以只断言 `sent_generation` 测不出这处回归，必须直接断言入参序列。
        let high_water_floors = Arc::new(Mutex::new(Vec::<i64>::new()));
        let high_water_floors_for_provider = Arc::clone(&high_water_floors);
        let inner = test_inner_with_registry_providers_and_high_water(
            Box::new(|_, _| {
                Ok(RegistrySnapshot {
                    revision: 10,
                    entries: Vec::new(),
                })
            }),
            Box::new(|_, high_water, _, include_pairing, _revoke_subjects| {
                assert_eq!(high_water, 12, "rebase 必须拿到吸收后的 relay_high_water");
                assert!(!include_pairing);
                // 刻意不重新领 revoke 代号：这条 delete 最终发出的代号只能来自
                // `registry_high_water_provider`（每轮 sync.ack 后都跑一次的吸收步骤），
                // 不能靠 rebase 这条支路掩盖 absorb 有没有做对。
                Ok((
                    RegistrySnapshot {
                        revision: high_water,
                        entries: Vec::new(),
                    },
                    None,
                    Vec::new(),
                ))
            }),
            Box::new(move |_, high_water, revoke_subjects: &[String]| {
                high_water_floors_for_provider
                    .lock()
                    .unwrap()
                    .push(high_water);
                Ok(revoke_subjects
                    .iter()
                    .map(|subject| (subject.clone(), high_water + 7))
                    .collect())
            }),
        );
        // 「首次 delete 未送达（连接断）」：不经过一次真实连接，直接把撤销时领到的旧代号（5）
        // 放进 outbox，代表它是从更早、已经死掉的连接遗留下来的撤销意图。
        lock(&inner.registry).enqueue_token_delete(subject.clone(), 5, true);

        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let result = run_authenticated_connection(
            &inner,
            &url,
            &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
            &config,
            None,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([6_u8; 32])),
        );

        assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
        let delete_frame = frames_rx.recv().unwrap();
        assert_eq!(delete_frame["t"], "token.delete");
        assert_eq!(delete_frame["subject"], subject);
        let sent_generation = delete_frame["generation"].as_i64().unwrap();
        // 12 已经覆盖 10（12 > 10），单独再断言 `> 10` 是冗余的，故只留 `> 12` 这一半。
        assert!(
            sent_generation > 12,
            "delete 代号必须严格大于两轮 sync 里较大的那个上界 relay_high_water（12），\
             实际 {sent_generation}"
        );
        assert_ne!(sent_generation, 5, "不能沿用撤销时领到的旧代号");
        // 只看 `sent_generation` 测不出「floor 错改成只用 revision」的回归：两种实现最后一轮
        // 传给 provider 的 high_water 恰好都是 12（错误实现只有第一轮的 10 被吞掉），所以最终
        // 代号照样是 19。真正能区分两者的是 provider 两轮各自收到的入参序列。
        assert_eq!(
            *high_water_floors.lock().unwrap(),
            vec![12, 12],
            "两轮 sync.ack 后传给 registry_high_water_provider 的 floor 都应是吸收后的 12；\
             若 floor 被错改成只用 revision，第一轮会传成 10"
        );

        let outbox = lock(&inner.registry).outbox_snapshot_for_test();
        assert!(
            outbox.is_empty(),
            "收到 ok ack 后 revoke 项必须被删除，不是继续挂在 outbox 里"
        );
        server.join().unwrap();
    }

    #[test]
    fn remote_omitted_pairing_ack_makes_next_pairing_snapshot_generation_exceed_high_water() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let frame = ack_initial_registry_sync(&mut socket);
            assert!(frame["entries"].as_array().unwrap().is_empty());
            let _ = socket.read();
        });
        let next_generation = Arc::new(AtomicU64::new(5));
        let counter_for_snapshot = Arc::clone(&next_generation);
        let counter_for_ack = Arc::clone(&next_generation);
        let inner = test_inner_with_registry_providers_and_high_water(
            Box::new(move |_, _| {
                Ok(RegistrySnapshot {
                    revision: counter_for_snapshot.load(Ordering::Acquire) as i64,
                    entries: Vec::new(),
                })
            }),
            Box::new(|_, _, _, _, _| panic!("rebase must not run")),
            Box::new(move |_, high_water, _revoke_subjects: &[String]| {
                counter_for_ack.fetch_max((high_water + 1) as u64, Ordering::AcqRel);
                Ok(Vec::new())
            }),
        );
        let config = GatewayConfig {
            relay_url: format!("ws://{address}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection_config = config.clone();
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connection_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&Zeroizing::new([5_u8; 32])),
            )
        });

        wait_until_connected(&inner);
        let pairing_generation = next_generation.fetch_add(1, Ordering::AcqRel) as i64;
        lock(&inner.registry).set_pairing_entry(TokenSyncEntry {
            subject: "pairing".to_owned(),
            generation: pairing_generation,
            scope: "pairing".to_owned(),
            current: TokenSyncCurrent {
                token_hash: "aa".repeat(32),
                access_expires: 1_800_000_000_000,
                refresh_until: None,
            },
            prev: None,
        });
        let next_snapshot =
            registry_snapshot_for_send(&inner, &config.room_id, 1_700_000_000_000, None).unwrap();
        let pairing = next_snapshot
            .entries
            .iter()
            .find(|entry| entry.subject == "pairing")
            .unwrap();
        assert!(pairing.generation > 5);

        inner.shutdown.store(true, Ordering::Release);
        assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
        server.join().unwrap();
    }

    #[test]
    fn builds_msg_completed_payload() {
        let blocks = serde_json::json!([
            { "type": "text", "text": "done" },
            { "type": "code", "code": "ok" }
        ]);

        assert_eq!(
            build_msg_completed_payload(42, "assistant", blocks.clone(), None),
            serde_json::json!({
                "message_id": 42,
                "role": "assistant",
                "blocks": blocks,
            })
        );
    }

    /// 显示当前 agent（MA1）：`agent` 为 `Some` 时 payload 插入 `"agent"` 键；`None` 时该键
    /// 整个省略（不是 `null`）——保持老消费方（不认识 `agent` 键的旧解析逻辑）向后兼容。
    #[test]
    fn builds_msg_completed_payload_agent_field_optional() {
        let blocks = serde_json::json!([{ "type": "text", "text": "done" }]);

        let with_agent =
            build_msg_completed_payload(42, "assistant", blocks.clone(), Some("Claude"));
        assert_eq!(with_agent["agent"], "Claude");
        assert_eq!(
            with_agent,
            serde_json::json!({
                "message_id": 42,
                "role": "assistant",
                "blocks": blocks,
                "agent": "Claude",
            })
        );

        let without_agent = build_msg_completed_payload(42, "assistant", blocks.clone(), None);
        assert!(
            without_agent.get("agent").is_none(),
            "agent key must be omitted (not null) when agent is None"
        );
        assert_eq!(
            without_agent,
            serde_json::json!({
                "message_id": 42,
                "role": "assistant",
                "blocks": blocks,
            })
        );
    }

    #[test]
    fn builds_card_created_payload() {
        let block = serde_json::json!({
            "type": "decision_card",
            "decision_id": "decision-1",
            "status": "pending",
        });

        assert_eq!(
            build_card_created_payload(block.clone()),
            serde_json::json!({ "block": block })
        );
    }

    #[test]
    fn builds_card_resolved_payload_including_null_chosen_option() {
        assert_eq!(
            build_card_resolved_payload("decision-1", "resolved", Some("A")),
            serde_json::json!({
                "decision_id": "decision-1",
                "status": "resolved",
                "chosen_option": "A",
            })
        );
        assert_eq!(
            build_card_resolved_payload("decision-1", "dismissed", None),
            serde_json::json!({
                "decision_id": "decision-1",
                "status": "dismissed",
                "chosen_option": null,
            })
        );
    }

    #[test]
    fn builds_running_run_status_payload_with_run_id() {
        assert_eq!(
            build_run_status_payload("session-1", "running", Some("run-1")),
            serde_json::json!({
                "session_id": "session-1",
                "status": "running",
                "run_id": "run-1",
            })
        );
    }

    #[test]
    fn builds_idle_run_status_payload_without_run_id() {
        assert_eq!(
            build_run_status_payload("session-1", "idle", None),
            serde_json::json!({
                "session_id": "session-1",
                "status": "idle",
                "run_id": null,
            })
        );
    }

    #[test]
    fn builds_session_index_created_payload() {
        assert_eq!(
            build_session_index_created_payload("s1", "Title", "repo-1", "local", None),
            serde_json::json!({
                "op": "created",
                "full": false,
                "session": {
                    "id": "s1",
                    "title": "Title",
                    "repo_id": "repo-1",
                    "namespace_id": "local",
                    "archived": false,
                    "repo_name": null,
                },
            })
        );
    }

    #[test]
    fn builds_session_index_created_payload_with_repo_name() {
        assert_eq!(
            build_session_index_created_payload(
                "s1",
                "Title",
                "repo-1",
                "local",
                Some("Acme Corp")
            ),
            serde_json::json!({
                "op": "created",
                "full": false,
                "session": {
                    "id": "s1",
                    "title": "Title",
                    "repo_id": "repo-1",
                    "namespace_id": "local",
                    "archived": false,
                    "repo_name": "Acme Corp",
                },
            })
        );
    }

    #[test]
    fn builds_session_index_renamed_payload() {
        assert_eq!(
            build_session_index_renamed_payload("s1", "Renamed"),
            serde_json::json!({
                "op": "renamed",
                "full": false,
                "id": "s1",
                "title": "Renamed",
            })
        );
    }

    #[test]
    fn builds_session_index_deleted_payload_without_session_fields() {
        let payload = build_session_index_deleted_payload("s1");
        assert_eq!(
            payload,
            serde_json::json!({ "op": "deleted", "full": false, "id": "s1" })
        );
        assert!(payload.get("title").is_none());
        assert!(payload.get("session").is_none());
    }

    #[test]
    fn builds_session_index_archived_and_unarchived_payloads() {
        let ids = vec!["s1".to_owned(), "s2".to_owned()];
        assert_eq!(
            build_session_index_archived_payload(&ids, true),
            serde_json::json!({
                "op": "archived",
                "full": false,
                "ids": ["s1", "s2"],
            })
        );
        assert_eq!(
            build_session_index_archived_payload(&ids, false),
            serde_json::json!({
                "op": "unarchived",
                "full": false,
                "ids": ["s1", "s2"],
            })
        );
    }

    #[test]
    fn builds_session_index_snapshot_payload_without_op() {
        let sessions = serde_json::json!([{"id": "s1"}, {"id": "s2"}]);
        let payload = build_session_index_snapshot_payload(sessions.clone(), Value::Null);
        assert_eq!(payload["full"], true);
        assert_eq!(payload["sessions"], sessions);
        assert_eq!(payload["repo"], Value::Null);
        assert!(payload.get("op").is_none());
    }

    #[test]
    fn builds_session_index_snapshot_payload_with_repo_summary() {
        let sessions = serde_json::json!([{"id": "s1"}]);
        let repo = serde_json::json!({"id": "repo-1", "name": "Acme Corp"});
        let payload = build_session_index_snapshot_payload(sessions, repo.clone());
        assert_eq!(payload["repo"], repo);
    }

    // B2（backlog 跟进）：session.index 全量快照发送前尺寸闸。

    #[test]
    fn truncate_session_index_snapshot_rows_passes_through_unchanged_when_within_budget() {
        let sessions = serde_json::json!([{"id": "s1"}, {"id": "s2"}]);
        let (result, truncated) = truncate_session_index_snapshot_rows(sessions.clone(), 4096);
        assert_eq!(result, sessions);
        assert!(!truncated);
    }

    #[test]
    fn truncate_session_index_snapshot_rows_empty_array_does_not_panic() {
        let (result, truncated) = truncate_session_index_snapshot_rows(serde_json::json!([]), 4096);
        assert_eq!(result, serde_json::json!([]));
        assert!(!truncated);
    }

    #[test]
    fn truncate_session_index_snapshot_rows_drops_tail_rows_when_over_budget() {
        // 每行序列化后约 30 字节（`{"id":"row-N","pad":"..."}`），budget=100 只够放下前几行——
        // 断言：① 结果不超预算；② truncated=true；③ 保留的是排在前面的行（SQL 已按
        // pinned DESC, created_at DESC 排好序，重要行天然在前，这里只需验证"从尾部丢"这个
        // 截断策略本身，不需要真的模拟 pinned/created_at 排序）。
        let rows: Vec<Value> = (0..10)
            .map(|i| serde_json::json!({ "id": format!("row-{i}"), "pad": "xxxxxxxxxx" }))
            .collect();
        let sessions = Value::Array(rows.clone());
        let budget = 100;
        let full_bytes = serde_json::to_vec(&sessions).expect("must serialize").len();
        assert!(
            full_bytes > budget,
            "test fixture must actually exceed budget"
        );

        let (result, truncated) = truncate_session_index_snapshot_rows(sessions, budget);
        assert!(truncated);
        let result_bytes = serde_json::to_vec(&result).expect("must serialize").len();
        assert!(result_bytes <= budget);

        let kept = result.as_array().expect("result must be an array");
        assert!(!kept.is_empty(), "budget must fit at least the first row");
        assert!(
            kept.len() < rows.len(),
            "some tail rows must have been dropped"
        );
        // 保留的行必须是原数组的一个前缀（顺序不变、内容不变），不是任意子集。
        assert_eq!(kept.as_slice(), &rows[..kept.len()]);
    }

    #[test]
    fn truncate_session_index_snapshot_rows_non_array_input_passes_through_unchanged() {
        // 理论不可达（调用方恒传 filter_session_index_snapshot_for_active_repo 的 fail-closed
        // 数组返回值）——防御性：不假设契约不会被破坏，但也不在这里重新发明一次 fail-closed。
        let (result, truncated) = truncate_session_index_snapshot_rows(Value::Null, 10);
        assert_eq!(result, Value::Null);
        assert!(!truncated);
    }

    #[test]
    fn marks_session_index_snapshot_truncated_inserts_key_only_when_truncated() {
        let payload = serde_json::json!({ "full": true, "sessions": [], "repo": null });

        let untruncated = mark_session_index_snapshot_truncated(payload.clone(), false);
        assert!(untruncated.get("truncated").is_none());

        let truncated = mark_session_index_snapshot_truncated(payload, true);
        assert_eq!(truncated["truncated"], Value::Bool(true));
    }

    #[test]
    fn derives_msg_completed_client_msg_id_from_kat() {
        assert_eq!(
            derive_msg_completed_client_msg_id("s-1", "dk-1"),
            "73996db9-9424-5e73-acb6-965bf87bfb80"
        );
    }

    #[test]
    fn derives_card_created_client_msg_id_from_kat() {
        assert_eq!(
            derive_card_created_client_msg_id("decision-1"),
            "d158145f-8ee9-58aa-a45e-77c5f364a596"
        );
    }

    #[test]
    fn derives_card_resolved_client_msg_id_from_kat() {
        assert_eq!(
            derive_card_resolved_client_msg_id("decision-1", "resolved"),
            "6ac580b3-1b88-5198-b7f5-27808dc027e3"
        );
    }

    #[test]
    fn builds_websocket_urls_without_legacy_role_or_token_query_params() {
        // S1ja §9.7 后门退役：desktop 认证已完全走 `Authorization: Bearer`
        // （build_ws_request），build_ws_url 不再接受/拼接任何令牌——`?role=desktop`
        // 与 `&token=` 两个 legacy query 参数结构上不可能再出现（`&token=` 曾把
        // remote_dev_token 明文送进 CF 边缘日志，P2-1）。
        let room = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            build_ws_url("wss://relay.example.com", room),
            format!("wss://relay.example.com/room/{room}")
        );
        assert_eq!(
            build_ws_url("wss://relay.example.com/", room),
            format!("wss://relay.example.com/room/{room}")
        );
        let url = build_ws_url("wss://relay.example.com", room);
        assert!(!url.contains("role="));
        assert!(!url.contains("token="));
        assert!(!url.contains('?'));
    }

    #[test]
    fn websocket_upgrade_request_carries_bearer_without_exposing_secret_debug() {
        let credential_text = "ab".repeat(32);
        let credential = DesktopCredential::new(Zeroizing::new(credential_text.clone()));
        let request = build_ws_request(
            "wss://relay.example.com/room/0123456789abcdef0123456789abcdef?role=desktop",
            &credential,
        )
        .unwrap();

        assert_eq!(
            request.headers().get(AUTHORIZATION).unwrap(),
            format!("Bearer {credential_text}").as_str()
        );
        assert_eq!(format!("{credential:?}"), "***");
    }

    #[test]
    fn websocket_upgrade_refuses_redirects() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: ws://127.0.0.1:9/redirected\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
        });
        let credential = DesktopCredential::new(Zeroizing::new("cd".repeat(32)));
        let request = build_ws_request(
            &format!("ws://{address}/room/test?role=desktop"),
            &credential,
        )
        .unwrap();

        let error = connect_with_config(request, None, WS_MAX_REDIRECTS).unwrap_err();

        assert!(matches!(
            error,
            WebSocketError::Http(response)
                if response.status() == tungstenite::http::StatusCode::FOUND
        ));
        assert_eq!(WS_MAX_REDIRECTS, 0);
        server.join().unwrap();
    }

    #[test]
    fn ensure_claim_200_reconnects_and_calls_claim_once() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let claim_calls = Arc::clone(&calls);
        let inner = test_inner_with_claim_handlers(
            move |_, _, hash| {
                claim_calls.fetch_add(1, Ordering::Relaxed);
                assert_eq!(
                    hash,
                    crate::remote_pairing::desktop_credential_hash(&"ef".repeat(32))
                );
                Ok(ClaimResponse::Claimed)
            },
            |_| Ok(false),
        );

        let action = ensure_claim(
            &inner,
            &sample_gateway_config(),
            &DesktopCredential::new(Zeroizing::new("ef".repeat(32))),
        );

        assert_eq!(action, ClaimAction::Reconnect);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unauthorized_reconnect_cycle_claims_at_most_once() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let relay_url = format!("ws://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..read])
                    .to_ascii_lowercase()
                    .contains("authorization: bearer "));
                stream
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                    .unwrap();
            }
        });
        let claim_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = test_inner_for_claim_cycle(&relay_url, Arc::clone(&claim_calls));
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let attempt = attempt_once(&inner, &upstream_rx, &milestone_rx);

        assert!(matches!(
            attempt,
            ConnectAttempt::Ran {
                result: Err(ConnectionFailure::Unauthorized),
                ..
            }
        ));
        assert_eq!(claim_calls.load(Ordering::Relaxed), 1);
        server.join().unwrap();
    }

    #[test]
    fn websocket_upgrade_410_stops_without_claim_or_retry() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let relay_url = format!("ws://{}", listener.local_addr().unwrap());
        let upgrade_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_upgrade_calls = Arc::clone(&upgrade_calls);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            server_upgrade_calls.fetch_add(1, Ordering::Relaxed);
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read])
                .to_ascii_lowercase()
                .contains("authorization: bearer "));
            stream
                .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let claim_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = test_inner_for_claim_cycle(&relay_url, Arc::clone(&claim_calls));
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let attempt = attempt_once(&inner, &upstream_rx, &milestone_rx);

        assert!(matches!(
            attempt,
            ConnectAttempt::Stopped(reason) if reason.code == ROOM_TOMBSTONED_STOP_REASON
        ));
        assert_eq!(upgrade_calls.load(Ordering::Relaxed), 1);
        assert_eq!(claim_calls.load(Ordering::Relaxed), 0);
        server.join().unwrap();
    }

    #[test]
    fn stopped_loop_idles_until_settings_reload_then_reconnects() {
        use std::io::{Read, Write};

        let tombstone_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tombstone_url = format!("ws://{}", tombstone_listener.local_addr().unwrap());
        let upgrade_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_upgrade_calls = Arc::clone(&upgrade_calls);
        let tombstone_server = thread::spawn(move || {
            let (mut stream, _) = tombstone_listener.accept().unwrap();
            server_upgrade_calls.fetch_add(1, Ordering::Relaxed);
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let enabled = Arc::new(AtomicBool::new(true));
        let settings_enabled = Arc::clone(&enabled);
        let relay_url = Arc::new(Mutex::new(tombstone_url));
        let settings_relay_url = Arc::clone(&relay_url);
        let claim_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let claim_call_counter = Arc::clone(&claim_calls);
        let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let inner = Arc::new(Inner {
            settings: Box::new(move |key| match key {
                "remote_control_enabled" => {
                    Some(settings_enabled.load(Ordering::Acquire).to_string())
                }
                "remote_relay_url" => Some(lock(&settings_relay_url).clone()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            }),
            token_provider: Box::new(|| None),
            desktop_credential_provider: test_desktop_credential_provider(),
            claim_client: Box::new(move |_, _, _| {
                claim_call_counter.fetch_add(1, Ordering::Relaxed);
                Ok(ClaimResponse::Claimed)
            }),
            active_device_provider: test_active_device_provider(),
            active_room_resolver: Box::new(|_project_id| {
                Ok("0123456789abcdef0123456789abcdef".to_owned())
            }),
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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: Duration::from_millis(100),
        });
        let loop_inner = Arc::clone(&inner);
        let gateway_thread = thread::spawn(move || {
            connect_loop(Arc::downgrade(&loop_inner), upstream_rx, milestone_rx)
        });

        wait_until_stopped_reason(&inner, ROOM_TOMBSTONED_STOP_REASON);
        tombstone_server.join().unwrap();
        thread::sleep(BACKOFF_POLL_INTERVAL + Duration::from_millis(100));
        assert_eq!(upgrade_calls.load(Ordering::Relaxed), 1);
        assert_eq!(claim_calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            lock(&inner.state.status).stopped_reason.as_deref(),
            Some(ROOM_TOMBSTONED_STOP_REASON)
        );

        let (recovery_addr, recovery_server) = spawn_frame_pump_server();
        *lock(&relay_url) = format!("ws://{recovery_addr}");
        enabled.store(false, Ordering::Release);
        inner.reload_requested.store(true, Ordering::Release);
        wait_until_gateway_state(&inner, GatewayState::Disabled, None);

        enabled.store(true, Ordering::Release);
        inner.reload_requested.store(true, Ordering::Release);
        wait_until_connected(&inner);
        assert_eq!(lock(&inner.state.status).stopped_reason, None);

        inner.shutdown.store(true, Ordering::Release);
        gateway_thread.join().unwrap();
        recovery_server.join().unwrap();
    }

    #[test]
    fn remote_stopped_loop_registry_publish_wake_retries_connection() {
        use std::io::{Read, Write};

        let tombstone_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tombstone_url = format!("ws://{}", tombstone_listener.local_addr().unwrap());
        let tombstone_server = thread::spawn(move || {
            let (mut stream, _) = tombstone_listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let relay_url = Arc::new(Mutex::new(tombstone_url));
        let settings_relay_url = Arc::clone(&relay_url);
        let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let inner = Arc::new(Inner {
            settings: Box::new(move |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some(lock(&settings_relay_url).clone()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            }),
            token_provider: Box::new(|| None),
            desktop_credential_provider: test_desktop_credential_provider(),
            claim_client: Box::new(|_, _, _| Ok(ClaimResponse::Claimed)),
            active_device_provider: test_active_device_provider(),
            active_room_resolver: Box::new(|_project_id| {
                Ok("0123456789abcdef0123456789abcdef".to_owned())
            }),
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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: Duration::from_millis(100),
        });
        let loop_inner = Arc::clone(&inner);
        let gateway_thread = thread::spawn(move || {
            connect_loop(Arc::downgrade(&loop_inner), upstream_rx, milestone_rx)
        });

        wait_until_stopped_reason(&inner, ROOM_TOMBSTONED_STOP_REASON);
        tombstone_server.join().unwrap();
        let (recovery_addr, recovery_server) = spawn_frame_pump_server();
        *lock(&relay_url) = format!("ws://{recovery_addr}");
        inner.registry_publish_wake.store(true, Ordering::Release);

        wait_until_connected(&inner);
        assert_eq!(lock(&inner.state.status).stopped_reason, None);
        assert!(!inner.reload_requested.load(Ordering::Acquire));

        inner.shutdown.store(true, Ordering::Release);
        gateway_thread.join().unwrap();
        recovery_server.join().unwrap();
    }

    #[test]
    fn remote_registry_publish_wake_interrupts_backoff_without_settings_reload() {
        let inner = test_inner(|_| None, || None);
        inner.registry_publish_wake.store(true, Ordering::Release);

        let started = Instant::now();
        assert!(!interruptible_sleep(&inner, Duration::from_secs(60)));

        assert!(started.elapsed() < BACKOFF_POLL_INTERVAL);
        assert!(inner.registry_publish_wake.load(Ordering::Acquire));
        assert!(!inner.reload_requested.load(Ordering::Acquire));
    }

    #[test]
    fn ensure_claim_409_with_devices_stops_without_regeneration() {
        let inner =
            test_inner_with_claim_handlers(|_, _, _| Ok(ClaimResponse::Conflict), |_| Ok(true));

        assert!(matches!(
            ensure_claim(
                &inner,
                &sample_gateway_config(),
                &DesktopCredential::new(Zeroizing::new("34".repeat(32))),
            ),
            ClaimAction::Stop(reason) if reason.code == ROOM_CLAIM_CONFLICT_STOP_REASON
        ));
    }

    /// M2-4d：单活跃房间模型下换房机制已撤——per-project 房间撞 conflict 且房内查无设备时，
    /// 必须直接 Stop 专属码，不进任何"换房"自愈路径（那条路径连同 `room_regenerator`/
    /// `ClaimAction::RoomRegenerated`/`MAX_ROOM_REGENERATIONS` 已整个删除，见 `ensure_claim`
    /// 撤除说明）。
    #[test]
    fn ensure_claim_conflict_without_devices_stops_immediately() {
        let inner =
            test_inner_with_claim_handlers(|_, _, _| Ok(ClaimResponse::Conflict), |_| Ok(false));
        let config = GatewayConfig {
            relay_url: "wss://relay.example.com".to_owned(),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };

        assert!(matches!(
            ensure_claim(
                &inner,
                &config,
                &DesktopCredential::new(Zeroizing::new("56".repeat(32))),
            ),
            ClaimAction::Stop(reason) if reason.code == ROOM_CLAIM_CONFLICT_PROJECT_STOP_REASON
        ));
    }

    #[test]
    fn ensure_claim_410_stops_as_tombstoned() {
        let inner = test_inner_with_claim_handlers(
            |_, _, _| Ok(ClaimResponse::Tombstoned),
            |_| unreachable!(),
        );

        assert!(matches!(
            ensure_claim(
                &inner,
                &sample_gateway_config(),
                &DesktopCredential::new(Zeroizing::new("56".repeat(32))),
            ),
            ClaimAction::Stop(reason) if reason.code == ROOM_TOMBSTONED_STOP_REASON
        ));
    }

    /// M2-4d：`ROOM_REGENERATION_FAILED_STOP_REASON`/`ROOM_REGENERATION_LIMIT_STOP_REASON` 两条
    /// 分支已随换房机制一起删除（这两个字符串常量本身保留，见其定义处说明），这个测试原本
    /// 覆盖的「无效房间号/换房失败」两种子情形不再可达，只剩「设备状态查询失败」这一条
    /// 结构化码断言。
    #[test]
    fn ensure_claim_device_status_unavailable_stop_is_stable() {
        let credential = DesktopCredential::new(Zeroizing::new("90".repeat(32)));
        let config = sample_gateway_config();

        let device_status_unavailable = test_inner_with_claim_handlers(
            |_, _, _| Ok(ClaimResponse::Conflict),
            |_| Err("database unavailable".to_owned()),
        );
        assert!(matches!(
            ensure_claim(&device_status_unavailable, &config, &credential),
            ClaimAction::Stop(reason)
                if reason.code == ROOM_DEVICE_STATUS_UNAVAILABLE_STOP_REASON
        ));
    }

    #[test]
    fn ensure_claim_429_returns_to_normal_backoff() {
        let inner = test_inner_with_claim_handlers(
            |_, _, _| Ok(ClaimResponse::RateLimited),
            |_| unreachable!(),
        );

        assert!(matches!(
            ensure_claim(
                &inner,
                &sample_gateway_config(),
                &DesktopCredential::new(Zeroizing::new("78".repeat(32))),
            ),
            ClaimAction::Backoff(message) if message.contains("429")
        ));
    }

    #[test]
    fn computes_capped_exponential_backoff_without_overflow() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(10), Duration::from_secs(60));
        assert_eq!(backoff_delay(100), Duration::from_secs(60));
        assert_eq!(backoff_delay(u32::MAX), Duration::from_secs(60));
    }

    #[test]
    fn parses_enabled_flag_and_complete_config() {
        let relay = "wss://relay.example.com";
        let room = "0123456789abcdef0123456789ABCDEF";

        assert!(!parse_config(None, Some(relay), Some(room), None).0);
        assert!(!parse_config(Some("false"), Some(relay), Some(room), None).0);
        assert!(parse_config(Some("true"), Some(relay), Some(room), None).0);

        for invalid_room in [
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert_eq!(
                parse_config(Some("true"), Some(relay), Some(invalid_room), None),
                (true, None)
            );
        }

        assert_eq!(
            parse_config(Some("true"), Some(relay), Some(room), None),
            (
                true,
                Some(GatewayConfig {
                    relay_url: relay.to_owned(),
                    room_id: room.to_lowercase(),
                    active_repo_id: None,
                })
            )
        );
    }

    #[test]
    fn canonicalizes_room_id_before_building_the_wire_url() {
        let relay = "wss://relay.example.com";
        let uppercase_room = "0123456789ABCDEF0123456789ABCDEF";
        let (_, config) = parse_config(Some("true"), Some(relay), Some(uppercase_room), None);
        let config = config.expect("mixed-case hexadecimal room id should be accepted");

        assert_eq!(config.room_id, uppercase_room.to_lowercase());
        let url = build_ws_url(&config.relay_url, &config.room_id);
        assert!(url.ends_with("/room/0123456789abcdef0123456789abcdef"));
        assert!(!url.chars().any(|character| character.is_ascii_uppercase()));
    }

    #[test]
    fn redacts_query_tokens_and_unknown_token_shapes() {
        // S1ja §9.7: build_ws_url no longer accepts a token to embed in the URL (the whole
        // point of the retirement), so this test now synthesizes the shape a legacy-relay
        // error message used to have by hand — `redact()` itself is still a real, generic
        // string scrubber (F4 extends it to headers in this same batch) and stays exercised.
        let token = "SENTINEL-TOKEN-ABC123";
        let room = "0123456789abcdef0123456789abcdef";
        let url = format!("wss://relay.example.com/room/{room}?token={token}");
        let error = format!("connect failed: URL error: Unable to connect to {url}");
        let redacted = redact(&error, Some(token));

        assert!(!redacted.contains(token));
        assert!(!redacted.contains(&format!("token={token}")));
        assert!(redacted.contains("token=***"));

        let unknown_shape = format!("relay rejected capability {token} during handshake");
        assert_eq!(
            redact(&unknown_shape, Some(token)),
            "relay rejected capability *** during handshake"
        );

        assert_eq!(
            redact("token=first&reason=retry&token=second&done=true", None),
            "token=***&reason=retry&token=***&done=true"
        );
    }

    #[test]
    fn redacts_authorization_bearer_header_values() {
        // S1ja F4: once the query-string `?token=` backdoor is retired, `Authorization:
        // Bearer <credential>` is the desktop's only credential channel — a Debug-formatted
        // request/headers dump ending up in a panic or connection-failure string must not
        // leak the 64-hex desktop credential.
        let credential = "a1".repeat(32);
        let dump = format!(
            r#"handshake failed: request Request {{ headers: {{"authorization": "Bearer {credential}"}} }}"#
        );
        let redacted = redact(&dump, None);
        assert!(!redacted.contains(&credential));
        assert!(redacted.contains("Bearer ***"));

        // Raw HTTP header line shape too, not just the Debug-quoted one.
        let raw_line = format!("Authorization: Bearer {credential}\r\nHost: relay.example.com");
        let redacted_raw = redact(&raw_line, None);
        assert!(!redacted_raw.contains(&credential));
        assert_eq!(
            redacted_raw,
            "Authorization: Bearer ***\r\nHost: relay.example.com"
        );
    }

    #[test]
    fn redacts_sec_websocket_protocol_token_dot_offers() {
        // S1ja F4: `Sec-WebSocket-Protocol: agentloom-rc-v1, token.<hex>` (§9.1 remote scope
        // subprotocol) must be scrubbed the same way — only the `token.<hex>` value is
        // sensitive, the `agentloom-rc-v1` version offer alongside it is not and must survive.
        let capability_token = "d".repeat(64);
        let dump = format!(
            "upgrade rejected: Sec-WebSocket-Protocol: agentloom-rc-v1, token.{capability_token}"
        );
        let redacted = redact(&dump, None);
        assert!(!redacted.contains(&capability_token));
        assert_eq!(
            redacted,
            "upgrade rejected: Sec-WebSocket-Protocol: agentloom-rc-v1, token.***"
        );
    }

    #[test]
    fn redact_leaves_short_token_dot_suffixed_reasons_alone() {
        // R3 (双路审): `reason=token.refresh_failed` isn't a `token.<hex64>` credential —
        // "refresh_failed" isn't even hex-shaped ('r' isn't a hex digit) — but the
        // un-floored scrubber still matched on the bare "token." marker and spliced in a
        // spurious "***" (0-length match still triggered the unconditional `push_str("***")`
        // in the pre-R3 implementation), corrupting a legitimate diagnostic reason string.
        // The hex-length floor (MIN_SCRUBBED_HEX_LEN) fixes this: a run below the floor is
        // left untouched instead of being replaced.
        let message = "relay rejected refresh: reason=token.refresh_failed";
        assert_eq!(
            redact(message, None),
            message,
            "token.refresh_failed must survive redact() byte-for-byte — it was never a credential"
        );
    }

    #[test]
    fn registry_ack_diagnostic_reason_never_logs_hash_like_material() {
        assert_eq!(
            registry_ack_reason_for_log(Some("generation_conflict")),
            "generation_conflict"
        );
        assert_eq!(
            registry_ack_reason_for_log(Some(&"ab".repeat(32))),
            "redacted"
        );
        assert_eq!(
            registry_ack_reason_for_log(Some("bad reason with spaces")),
            "redacted"
        );
    }

    #[test]
    fn secret_token_debug_and_display_never_expose_the_value() {
        let token = SecretToken::new("SENTINEL-TOKEN-ABC123".to_owned());
        assert_eq!(format!("{token:?}"), "***");
        assert_eq!(format!("{token}"), "***");
        assert_eq!(token.expose(), "SENTINEL-TOKEN-ABC123");
    }

    #[test]
    fn redact_panic_message_strips_active_token_from_panic_text() {
        let message = "settings callback failed for token=SENTINEL-TOKEN-ABC123";
        let redacted = redact_panic_message(message, Some("SENTINEL-TOKEN-ABC123"));
        assert!(!redacted.contains("SENTINEL-TOKEN-ABC123"));
        assert!(redacted.contains("token=***"));
    }

    #[test]
    fn leaves_errors_without_tokens_unchanged() {
        let error = "read failed: connection reset by peer";
        assert_eq!(redact(error, Some("unused-secret")), error);
        assert_eq!(redact(error, None), error);
    }

    #[test]
    fn connection_failure_is_redacted_before_reaching_status() {
        // S1ja §9.7: build_ws_url structurally cannot embed a token anymore, so this test
        // hand-builds the legacy shape to keep exercising record_failure's redaction call.
        let token = "SENTINEL-TOKEN-ABC123";
        let room = "0123456789abcdef0123456789abcdef";
        let inner = test_inner(|_| None, || None);
        let url = format!("wss://relay.example.com/room/{room}?token={token}");
        let error = format!("connect failed: URL error: Unable to connect to {url}");
        let mut failed_attempts = 0;

        assert_eq!(
            record_failure(
                &inner,
                FailureKind::Connection,
                error,
                Some(token),
                &mut failed_attempts,
            ),
            0
        );

        let status = lock(&inner.state.status).clone();
        assert_eq!(status.state, GatewayState::Backoff);
        let last_error = status
            .last_error
            .expect("connection failure should be recorded");
        assert!(!last_error.contains(token));
        assert!(last_error.contains("token=***"));
        assert_eq!(inner.state.connection_failures.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn settings_reader_panic_is_caught_and_recovered_as_backoff() {
        let inner = test_inner(
            |_| {
                panic!(
                    "settings callback failed for token={}",
                    SecretToken::new("SENTINEL-TOKEN-ABC123".to_owned())
                )
            },
            || None,
        );

        assert_panicking_attempt_enters_backoff(&inner);
        let last_error = lock(&inner.state.status)
            .last_error
            .clone()
            .expect("panic should be recorded");
        assert!(!last_error.contains("SENTINEL-TOKEN-ABC123"));
        assert!(last_error.contains("token=***"));
    }

    #[test]
    fn token_provider_panic_is_caught_and_recovered_as_backoff() {
        let room = "0123456789abcdef0123456789abcdef".to_owned();
        let inner = test_inner_with_token_provider_and_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            },
            || panic!("token provider failed"),
            move |_project_id| Ok(room.clone()),
        );

        assert_panicking_attempt_enters_backoff(&inner);
    }

    // ---------------------------------------------------------------------------------
    // M2-4b：网关配置解析三态 + 切房触发 liveness 判死
    // ---------------------------------------------------------------------------------

    const ACTIVE_ROOM: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const LEGACY_ROOM: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn current_config_prefers_active_project_room_over_legacy_when_enabled() {
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                "remote_room_id" => Some(LEGACY_ROOM.to_owned()),
                _ => None,
            },
            |project_id| {
                assert_eq!(project_id, "proj-1");
                Ok(ACTIVE_ROOM.to_owned())
            },
        );

        let (enabled, config) = current_config(&inner);
        assert!(enabled);
        let config = config.expect("active project room must resolve to a config");
        assert_eq!(
            config.room_id, ACTIVE_ROOM,
            "active project 已设且 remote 已启用时，必须优先用 active_room_resolver 解出的房间，\
             不能用 legacy remote_room_id"
        );
    }

    /// relay 地址留空（空串——对应 DB 里手写 `set_app_setting(..., "remote_relay_url", "")` 那
    /// 类"未自定义"存量，或从未写过 `None`）时，`current_config` 必须兜底到官方公共中继，构造
    /// 出的 `GatewayConfig.relay_url` 恒等于 `DEFAULT_PUBLIC_RELAY_URL`——不是「未配置」态。
    #[test]
    fn current_config_falls_back_to_default_relay_when_relay_url_setting_is_empty() {
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("".to_owned()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            },
            |project_id| {
                assert_eq!(project_id, "proj-1");
                Ok(ACTIVE_ROOM.to_owned())
            },
        );

        let (enabled, config) = current_config(&inner);
        assert!(enabled);
        let config = config.expect("relay_url 留空不该是「未配置」态——必须兜底出一份可连接的配置");
        assert_eq!(
            config.relay_url, DEFAULT_PUBLIC_RELAY_URL,
            "relay_url 留空时必须兜底到官方公共中继"
        );
    }

    /// 同上，但 `remote_relay_url` 这个 app_setting 干脆没写过（`None`，而非空串）——两种"未
    /// 自定义"存量形态都必须收敛到同一个默认值。
    #[test]
    fn current_config_falls_back_to_default_relay_when_relay_url_setting_is_unset() {
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                // remote_relay_url 故意不设。
                _ => None,
            },
            |project_id| {
                assert_eq!(project_id, "proj-1");
                Ok(ACTIVE_ROOM.to_owned())
            },
        );

        let (enabled, config) = current_config(&inner);
        assert!(enabled);
        let config = config.expect("relay_url 未设不该是「未配置」态——必须兜底出一份可连接的配置");
        assert_eq!(
            config.relay_url, DEFAULT_PUBLIC_RELAY_URL,
            "relay_url 未设时必须兜底到官方公共中继"
        );
    }

    /// M2-4d：legacy 全局房回落已撤——active project 未设时不再有"回落 legacy 房间"这条路，
    /// 哪怕 `remote_room_id` 这个 app_setting 存量还有值（不迁移，一行不动，见撤除说明），
    /// `current_config` 也不再读它，必须直接判"未配置"。
    #[test]
    fn current_config_unconfigured_when_active_project_unset_even_if_legacy_room_id_has_a_value() {
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                "remote_room_id" => Some(LEGACY_ROOM.to_owned()),
                // remote_active_repo_id 故意不设。
                _ => None,
            },
            // active_room_resolver 在 active 未设时不该被调用；调用即测试失败。
            |project_id| panic!("active_room_resolver must not be called when unset: {project_id}"),
        );

        let (enabled, config) = current_config(&inner);
        assert!(enabled);
        assert!(
            config.is_none(),
            "active project 未设时必须是未配置态（None），不能回落 legacy remote_room_id；\
             实际={config:?}"
        );
    }

    /// R7①（opus P2-1·本单最安全敏感的零覆盖分支）：active project 已设但 resolver 解析失败
    /// （例如 R3 存在性检查查无该 repo，或 DB 错误）时，即便 legacy `remote_room_id` 恰好有
    /// 值，也绝不能悄悄回落用它——那样会把网关连去一个用户实际上没有选中的房间。解析失败必须
    /// 收敛到"暂无可用配置"（`None`），等下一次解析自愈，而不是产出一个看似正常、实际上房间
    /// 归属已经不对的连接。
    #[test]
    fn current_config_resolver_error_does_not_fall_back_to_legacy_even_when_legacy_has_a_value() {
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                "remote_active_repo_id" => Some("proj-deleted".to_owned()),
                "remote_room_id" => Some(LEGACY_ROOM.to_owned()),
                _ => None,
            },
            |project_id| Err(format!("repo not found: {project_id}")),
        );

        let (enabled, config) = current_config(&inner);
        assert!(enabled);
        assert!(
            config.is_none(),
            "resolver 报错时必须是 None（未配置态），绝不能悄悄回落 legacy；实际={config:?}"
        );
    }

    #[test]
    fn current_config_unconfigured_when_neither_active_nor_legacy_set() {
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                // 既没有 remote_active_repo_id 也没有 remote_room_id。
                _ => None,
            },
            |project_id| panic!("active_room_resolver must not be called: {project_id}"),
        );

        let (enabled, config) = current_config(&inner);
        assert!(enabled);
        assert!(
            config.is_none(),
            "active 和 legacy 都没有可用 room_id 时必须是未配置态（None），\
             实际={config:?}"
        );
    }

    #[test]
    fn current_config_does_not_ensure_active_room_when_remote_control_disabled() {
        // 分配时机纪律：remote 未启用时不该白白建房 / 碰凭据——即使 active project 已设。
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("false".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            },
            |project_id| {
                panic!("active_room_resolver must not be called while disabled: {project_id}")
            },
        );

        let (enabled, _config) = current_config(&inner);
        assert!(!enabled);
    }

    /// M24DR 返工·项 6①（nit F8）：纯空白 `remote_active_repo_id`（手改 DB / 陈旧数据留下的
    /// 空白值）必须按"未设"处理——`current_config` 必须判"无配置"，且不能把这个空白字符串
    /// 当成真实 project id 喂给 `active_room_resolver` 去建房。
    #[test]
    fn current_config_unconfigured_when_active_project_is_whitespace() {
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                "remote_active_repo_id" => Some("   ".to_owned()),
                _ => None,
            },
            |project_id| {
                panic!(
                    "active_room_resolver must not be called for a whitespace active repo id: \
                     {project_id}"
                )
            },
        );

        let (enabled, config) = current_config(&inner);
        assert!(enabled);
        assert!(
            config.is_none(),
            "纯空白 remote_active_repo_id 必须按未设处理，不能被当成真实 project id 去建房；\
             实际={config:?}"
        );
    }

    /// 任务 6②：active project 切换后，`current_config` 解出的新鲜配置与已连接配置在
    /// `evaluate_connection_liveness` 里必须判定为 Disconnect——这条正是"切房复用既有判死
    /// 重连机制"的落地证据：不需要为切房另造一条判活路径，`GatewayConfig` 的
    /// `#[derive(PartialEq)]` 天然覆盖 room_id 变化。
    #[test]
    fn switching_active_project_room_makes_evaluate_connection_liveness_disconnect() {
        let active_project = Arc::new(Mutex::new("proj-a".to_owned()));
        let rooms: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::from([
            (
                "proj-a".to_owned(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            ),
            ("proj-b".to_owned(), "c".repeat(32)),
        ])));
        let resolver_rooms = Arc::clone(&rooms);
        let settings_project = Arc::clone(&active_project);
        let inner = test_inner_with_active_room_resolver(
            move |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
                "remote_active_repo_id" => Some(lock(&settings_project).clone()),
                _ => None,
            },
            move |project_id| {
                lock(&resolver_rooms)
                    .get(project_id)
                    .cloned()
                    .ok_or_else(|| format!("no room for {project_id}"))
            },
        );

        let (_enabled, initial_config) = current_config(&inner);
        let connected_config = initial_config.expect("proj-a must resolve to a config");
        assert_eq!(connected_config.room_id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        // 切房：active project 从 A 换到 B。
        *lock(&active_project) = "proj-b".to_owned();
        let (fresh_enabled, fresh_config) = current_config(&inner);
        let fresh_config = fresh_config.expect("proj-b must also resolve to a config");
        assert_ne!(
            fresh_config.room_id, connected_config.room_id,
            "切换 active project 后解出的房间必须与旧房间不同"
        );

        let token = SecretToken::new("token".to_owned());
        assert_eq!(
            evaluate_connection_liveness(
                fresh_enabled,
                Some(&fresh_config),
                &connected_config,
                Some(&token),
                Some(&token),
                true,
                true,
            ),
            ConnectionDecision::Disconnect,
            "config 比较必须覆盖 room 变化：active 切房后 evaluate_connection_liveness 必须判死重连"
        );
    }

    #[test]
    fn disconnects_when_disabled_or_connected_config_or_token_changes() {
        let connected = GatewayConfig {
            relay_url: "wss://relay.example.com".to_owned(),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let changed = GatewayConfig {
            relay_url: connected.relay_url.clone(),
            room_id: "fedcba9876543210fedcba9876543210".to_owned(),
            active_repo_id: None,
        };
        let initial_token = SecretToken::new("initial-token".to_owned());
        let rotated_token = SecretToken::new("rotated-token".to_owned());

        assert_eq!(
            evaluate_connection_liveness(
                false,
                Some(&connected),
                &connected,
                Some(&initial_token),
                Some(&initial_token),
                true,
                true,
            ),
            ConnectionDecision::Disconnect
        );
        assert_eq!(
            evaluate_connection_liveness(
                true,
                Some(&changed),
                &connected,
                Some(&initial_token),
                Some(&initial_token),
                true,
                true,
            ),
            ConnectionDecision::Disconnect
        );
        assert_eq!(
            evaluate_connection_liveness(
                true,
                Some(&connected),
                &connected,
                Some(&rotated_token),
                Some(&initial_token),
                true,
                true,
            ),
            ConnectionDecision::Disconnect
        );
        assert_eq!(
            evaluate_connection_liveness(
                true,
                Some(&connected),
                &connected,
                None,
                Some(&initial_token),
                true,
                true,
            ),
            ConnectionDecision::Disconnect
        );
        assert_eq!(
            evaluate_connection_liveness(
                true,
                Some(&connected),
                &connected,
                Some(&initial_token),
                Some(&initial_token),
                true,
                true,
            ),
            ConnectionDecision::Continue
        );
    }

    #[test]
    fn disconnects_when_k_room_becomes_available_after_connect() {
        let connected = GatewayConfig {
            relay_url: "wss://relay.example.com".to_owned(),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let initial_token = SecretToken::new("initial-token".to_owned());

        for (connected_k_room_available, current_k_room_available, expected) in [
            (false, true, ConnectionDecision::Disconnect),
            (false, false, ConnectionDecision::Continue),
            (true, true, ConnectionDecision::Continue),
        ] {
            assert_eq!(
                evaluate_connection_liveness(
                    true,
                    Some(&connected),
                    &connected,
                    Some(&initial_token),
                    Some(&initial_token),
                    connected_k_room_available,
                    current_k_room_available,
                ),
                expected
            );
        }
    }

    #[test]
    fn continuous_frames_do_not_starve_liveness_check_when_disabled() {
        let (addr, server) = spawn_frame_pump_server();
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let enabled = Arc::new(AtomicBool::new(true));
        let settings_enabled = Arc::clone(&enabled);
        let settings_relay_url = relay_url.clone();
        let settings_room_id = room_id.clone();
        let inner = test_inner_with_interval(
            move |key| match key {
                "remote_control_enabled" => {
                    Some(settings_enabled.load(Ordering::Acquire).to_string())
                }
                "remote_relay_url" => Some(settings_relay_url.clone()),
                "remote_room_id" => Some(settings_room_id.clone()),
                _ => None,
            },
            || None,
            Duration::from_millis(150),
        );
        let connected_config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connected_config,
                None,
                &upstream_rx,
                &milestone_rx,
                None,
            )
        });

        wait_until_connected(&inner);
        enabled.store(false, Ordering::Release);

        let result = join_connection_within(connection, &inner);
        assert!(matches!(result, Ok(ConnectionExit::ConfigStale { .. })));
        server.join().expect("frame pump server should not panic");
    }

    #[test]
    fn liveness_does_not_poll_k_room_provider_when_connection_has_k_room() {
        let (addr, server) = spawn_frame_pump_server();
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let enabled = Arc::new(AtomicBool::new(true));
        let settings_enabled = Arc::clone(&enabled);
        let settings_relay_url = relay_url.clone();
        let settings_room_id = room_id.clone();
        let liveness_checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let token_provider_checks = Arc::clone(&liveness_checks);
        let k_room_provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_calls = Arc::clone(&k_room_provider_calls);
        let resolver_room_id = settings_room_id.clone();
        let inner = test_inner_with_interval_k_room_and_active_room_resolver(
            move |key| match key {
                "remote_control_enabled" => {
                    Some(settings_enabled.load(Ordering::Acquire).to_string())
                }
                "remote_relay_url" => Some(settings_relay_url.clone()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            },
            move || {
                token_provider_checks.fetch_add(1, Ordering::Relaxed);
                None
            },
            move |_| {
                provider_calls.fetch_add(1, Ordering::Relaxed);
                None
            },
            move |_project_id| Ok(resolver_room_id.clone()),
            Duration::from_millis(150),
        );
        // active_repo_id 必须跟 current_config 每轮 liveness 重新解析出的值一致（都是
        // "proj-1"）——GatewayConfig 的 PartialEq 把这个字段也比进去了，不一致会让
        // evaluate_connection_liveness 在第一轮就判定配置陈旧并断开，测试永远等不到期望的
        // 轮询次数。
        let connected_config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: Some("proj-1".to_owned()),
        };
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            let k_room = Zeroizing::new([7u8; 32]);
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connected_config,
                None,
                &upstream_rx,
                &milestone_rx,
                Some(&k_room),
            )
        });

        wait_until_connected(&inner);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && liveness_checks.load(Ordering::Relaxed) < 3 {
            thread::sleep(Duration::from_millis(20));
        }
        let checks_before_disconnect = liveness_checks.load(Ordering::Relaxed);
        let provider_calls_before_disconnect = k_room_provider_calls.load(Ordering::Relaxed);
        enabled.store(false, Ordering::Release);

        let result = join_connection_within(connection, &inner);
        assert!(matches!(result, Ok(ConnectionExit::ConfigStale { .. })));
        server.join().expect("frame pump server should not panic");
        assert!(
            checks_before_disconnect >= 3,
            "expected at least three liveness checks, got {checks_before_disconnect}"
        );
        assert_eq!(provider_calls_before_disconnect, 0);
    }

    #[test]
    fn token_rotation_disconnects_a_live_connection_with_continuous_frames() {
        let (addr, server) = spawn_frame_pump_server();
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let token = Arc::new(Mutex::new("initial-token".to_owned()));
        let token_provider_value = Arc::clone(&token);
        let settings_relay_url = relay_url.clone();
        let settings_room_id = room_id.clone();
        let inner = test_inner_with_interval(
            move |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some(settings_relay_url.clone()),
                "remote_room_id" => Some(settings_room_id.clone()),
                _ => None,
            },
            move || Some(lock(&token_provider_value).clone()),
            Duration::from_millis(150),
        );
        let connected_config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let connected_token = SecretToken::new("initial-token".to_owned());
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connected_config,
                Some(&connected_token),
                &upstream_rx,
                &milestone_rx,
                None,
            )
        });

        wait_until_connected(&inner);
        *lock(&token) = "rotated-token".to_owned();

        let result = join_connection_within(connection, &inner);
        assert!(matches!(result, Ok(ConnectionExit::ConfigStale { .. })));
        server.join().expect("frame pump server should not panic");
    }

    #[test]
    fn pre_ack_error_frame_from_relay_carries_frame_type_and_reason_in_the_message() {
        // S1ja F3 (批次审 3④)：mixed-version rollout — desktop upgraded, relay hasn't yet —
        // makes relay answer the initial `token.sync` with `{t:"error",
        // reason:"unknown_frame_type"}` instead of `token.sync.ack`. Before this fix the
        // resulting error was a bare, undiagnosable "relay sent a frame before registry sync
        // ack"; it must now name the frame type and reason it actually received.
        let (addr, server) = spawn_pre_ack_decoy_server(serde_json::json!({
            "t": "error",
            "reason": "unknown_frame_type",
        }));
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let inner = test_inner(move |_| None, || None);
        let config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &config,
                None,
                &upstream_rx,
                &milestone_rx,
                None,
            )
        });
        let result = join_connection_within(connection, &inner);
        assert_eq!(
            result,
            Err(ConnectionFailure::Other(
                "relay sent a frame before registry sync ack (t=error, reason=unknown_frame_type)"
                    .to_owned()
            ))
        );
        server
            .join()
            .expect("pre-ack decoy server should not panic");
    }

    #[test]
    fn pre_ack_frame_missing_reason_still_names_the_frame_type() {
        // Companion to the above: no `reason` field present — the message must still name the
        // frame type without a dangling/placeholder reason.
        let (addr, server) =
            spawn_pre_ack_decoy_server(serde_json::json!({ "t": "presence", "role": "desktop" }));
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let inner = test_inner(move |_| None, || None);
        let config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &config,
                None,
                &upstream_rx,
                &milestone_rx,
                None,
            )
        });
        let result = join_connection_within(connection, &inner);
        assert_eq!(
            result,
            Err(ConnectionFailure::Other(
                "relay sent a frame before registry sync ack (t=presence)".to_owned()
            ))
        );
        server
            .join()
            .expect("pre-ack decoy server should not panic");
    }

    #[test]
    fn pre_ack_decoy_token_ack_frame_survives_record_failure_redact_intact() {
        // R3 (双路审): F4's `scrub_after_marker("token.")` pass and F3's new
        // frame-type-carrying error string collide unless the hex-length floor exists — a
        // decoy `{t:"token.ack"}` frame produces the message `"...(t=token.ack)"`, and
        // without a floor the "token." marker inside "t=token.ack" would eat "ac" (the
        // first two hex-looking characters of "ack") and turn it into
        // "t=token.***k)" — F4 clobbering F3's own diagnostic. This drives the real
        // read_registry_sync_ack → ConnectionFailure → record_failure → redact() chain end
        // to end and asserts the frame type survives every step unmangled.
        let (addr, server) = spawn_pre_ack_decoy_server(serde_json::json!({ "t": "token.ack" }));
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let inner = test_inner(move |_| None, || None);
        let config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let url = build_ws_url(&config.relay_url, &config.room_id);
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &config,
                None,
                &upstream_rx,
                &milestone_rx,
                None,
            )
        });
        let result = join_connection_within(connection, &inner);
        let Err(ConnectionFailure::Other(message)) = result else {
            panic!("expected a connection failure carrying the frame-type detail, got {result:?}");
        };
        assert!(
            message.contains("t=token.ack"),
            "pre-redact message must name the frame type: {message}"
        );

        let mut failed_attempts = 0;
        record_failure(
            &inner,
            FailureKind::Connection,
            message,
            None,
            &mut failed_attempts,
        );
        let status = lock(&inner.state.status).clone();
        let last_error = status
            .last_error
            .expect("connection failure should be recorded");
        assert!(
            last_error.contains("t=token.ack"),
            "redact()'s hex-length floor must not clobber F3's frame-type detail: {last_error}"
        );
        server
            .join()
            .expect("pre-ack decoy server should not panic");
    }

    #[test]
    fn pairing_reload_requested_disconnects_a_live_connection_with_continuous_frames() {
        let (addr, server) = spawn_frame_pump_server();
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let settings_relay_url = relay_url.clone();
        let settings_room_id = room_id.clone();
        let inner = test_inner_with_interval(
            move |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some(settings_relay_url.clone()),
                "remote_room_id" => Some(settings_room_id.clone()),
                _ => None,
            },
            || None,
            Duration::from_millis(150),
        );
        let connected_config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connected_config,
                None,
                &upstream_rx,
                &milestone_rx,
                None,
            )
        });

        wait_until_connected(&inner);
        inner.reload_requested.store(true, Ordering::Release);

        let result = join_connection_within(connection, &inner);
        assert_eq!(result, Ok(ConnectionExit::PairingReloadRequested));
        server.join().expect("frame pump server should not panic");
    }

    #[test]
    fn remote_registry_publish_wake_keeps_live_connection_generation_and_sends_put() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (frame_tx, frame_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            ack_initial_registry_sync(&mut socket);
            loop {
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        let frame: Value = serde_json::from_str(text.as_ref()).unwrap();
                        if frame["t"] == "token.put" {
                            frame_tx.send(frame).unwrap();
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => panic!("live relay should receive token.put: {error}"),
                }
            }
            let _ = socket.close(None);
        });
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let settings_relay_url = relay_url.clone();
        let settings_room_id = room_id.clone();
        let inner = test_inner_with_interval(
            move |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some(settings_relay_url.clone()),
                "remote_room_id" => Some(settings_room_id.clone()),
                _ => None,
            },
            || None,
            Duration::from_secs(30),
        );
        let connected_config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let connection_inner = Arc::clone(&inner);
        let connection = thread::spawn(move || {
            let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            run_authenticated_connection(
                &connection_inner,
                &url,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &connected_config,
                None,
                &upstream_rx,
                &milestone_rx,
                None,
            )
        });

        wait_until_connected(&inner);
        let generation_before = inner.state.connection_generation_snapshot();
        let epoch_before = inner.state.epoch.load(Ordering::Acquire);
        lock(&inner.registry).enqueue_token_put(
            TokenSyncEntry {
                subject: "pairing".to_owned(),
                generation: 8,
                scope: "pairing".to_owned(),
                current: TokenSyncCurrent {
                    token_hash: "aa".repeat(32),
                    access_expires: 1_765_430_700_000,
                    refresh_until: None,
                },
                prev: None,
            },
            None,
        );
        inner.registry_publish_wake.store(true, Ordering::Release);

        let frame = frame_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(frame["subject"], "pairing");
        assert_eq!(
            inner.state.connection_generation_snapshot(),
            generation_before
        );
        assert_eq!(inner.state.epoch.load(Ordering::Acquire), epoch_before);
        assert!(!inner.reload_requested.load(Ordering::Acquire));
        assert_eq!(connection.join().unwrap(), Ok(ConnectionExit::ClosedByPeer));
        server.join().unwrap();
    }

    #[test]
    fn dispatches_control_frames_and_counts_bad_or_ignored_frames() {
        let inner = test_inner(|_| None, || None);
        let state = &inner.state;

        assert!(handle_frame(
            &inner,
            r#"{"t":"replay.head","epoch":3,"headSeq":17}"#,
            None,
        )
        .is_none());
        assert_eq!(state.epoch.load(Ordering::Acquire), 3);
        assert_eq!(state.head_seq.load(Ordering::Acquire), 17);
        assert_eq!(state.frames_seen.load(Ordering::Relaxed), 1);

        handle_frame(
            &inner,
            r#"{"t":"error","reason":"stale_epoch","currentEpoch":9}"#,
            None,
        );
        assert_eq!(state.epoch.load(Ordering::Acquire), 9);
        assert_eq!(state.frames_seen.load(Ordering::Relaxed), 2);

        assert!(handle_frame(&inner, "not json{{{", None).is_none());
        assert_eq!(state.bad_frames.load(Ordering::Relaxed), 1);
        assert_eq!(state.frames_seen.load(Ordering::Relaxed), 3);

        assert!(handle_frame(&inner, r#"{"t":"msg.completed","x":1}"#, None).is_none());
        assert_eq!(state.frames_seen.load(Ordering::Relaxed), 4);
        assert_eq!(state.epoch.load(Ordering::Acquire), 9);
        assert_eq!(state.head_seq.load(Ordering::Acquire), 17);
        assert_eq!(state.bad_frames.load(Ordering::Relaxed), 1);

        assert!(handle_frame(&inner, r#"{"kind":"event","t":"ignored"}"#, None).is_none());
        assert_eq!(state.frames_seen.load(Ordering::Relaxed), 5);
        assert_eq!(state.bad_frames.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn routes_encrypted_input_send_and_maps_handler_outcomes() {
        let k_room = Zeroizing::new([7_u8; 32]);
        for (outcome, expected) in [
            (AckOutcome::Ok, "ok"),
            (AckOutcome::Queued, "queued"),
            (AckOutcome::Failed, "failed"),
        ] {
            let received = Arc::new(Mutex::new(None));
            let received_for_handler = Arc::clone(&received);
            let inner = test_inner_with_input_control_handlers(
                move |frame| {
                    *received_for_handler.lock().unwrap() =
                        Some((frame.session, frame.command_id, frame.text));
                    Some(outcome)
                },
                |_| AckOutcome::Failed,
            );
            let envelope = seal_command_envelope(
                &k_room,
                "0123456789abcdef0123456789abcdef",
                7,
                "input",
                "s-1",
                "cmd-input-1",
                &serde_json::json!({
                    "t": "input.send",
                    "session": "s-1",
                    "text": "hello",
                }),
            );

            let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
            assert_eq!(
                response,
                serde_json::json!({
                    "t": "input.ack",
                    "command_id": "cmd-input-1",
                    "outcome": expected,
                })
            );
            assert_eq!(
                received.lock().unwrap().as_ref(),
                Some(&(
                    "s-1".to_owned(),
                    "cmd-input-1".to_owned(),
                    "hello".to_owned(),
                ))
            );
        }
    }

    #[test]
    fn encrypted_input_send_ack_loss_retry_uses_terminal_ledger_without_redelivery() {
        let k_room = Zeroizing::new([27_u8; 32]);
        let conn = Arc::new(Mutex::new(crate::test_support::mem_db()));
        let delivery_calls = Arc::new(AtomicU64::new(0));
        let conn_for_handler = Arc::clone(&conn);
        let delivery_calls_for_handler = Arc::clone(&delivery_calls);
        let inner = test_inner_with_input_control_handlers(
            move |frame| {
                let payload = serde_json::json!({"text": frame.text}).to_string();
                crate::remote_input_send_ack(
                    || {
                        crate::db::enqueue_remote_input(
                            &lock(&conn_for_handler),
                            &frame.session,
                            &frame.command_id,
                            "input.send",
                            &payload,
                        )
                        .map_err(|e| e.to_string())
                    },
                    || {
                        let entry = crate::db::next_pending_remote_input(
                            &lock(&conn_for_handler),
                            &frame.session,
                        )
                        .unwrap()
                        .expect("新 input.send 应在即时排空时可见");
                        delivery_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                        crate::db::mark_remote_input_delivered(&lock(&conn_for_handler), entry.id)
                            .unwrap();
                    },
                    || {
                        crate::db::remote_inbox_terminal_state_by_command_id(
                            &lock(&conn_for_handler),
                            &frame.command_id,
                        )
                        .map_err(|e| e.to_string())
                    },
                )
            },
            |_| AckOutcome::Failed,
        );
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-wire-ledger",
            "cmd-wire-ledger",
            &serde_json::json!({
                "t": "input.send",
                "session": "s-wire-ledger",
                "text": "hello",
            }),
        );

        let first = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(first["outcome"], "queued");
        assert_eq!(delivery_calls.load(Ordering::Relaxed), 1);
        let delivered_at: Option<i64> = lock(&conn)
            .query_row(
                "SELECT delivered_at FROM remote_inbox WHERE command_id = 'cmd-wire-ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(delivered_at.is_some());

        // 模拟 relay 未收到第一次 ack 后原样重发：台账终态仍映射 ok，且绝不再次排空/投递。
        let retry = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(retry["outcome"], "ok");
        assert_eq!(delivery_calls.load(Ordering::Relaxed), 1);
        let count: i64 = lock(&conn)
            .query_row(
                "SELECT COUNT(*) FROM remote_inbox WHERE command_id = 'cmd-wire-ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn control_stop_uses_authenticated_freshness_and_ignores_envelope_ts() {
        let k_room = Zeroizing::new([8_u8; 32]);
        let calls = Arc::new(AtomicU64::new(0));
        let replay_calls = Arc::new(AtomicU64::new(0));
        let received = Arc::new(Mutex::new(None));
        let calls_for_handler = Arc::clone(&calls);
        let replay_calls_for_handler = Arc::clone(&replay_calls);
        let received_for_handler = Arc::clone(&received);
        let inner = test_inner_with_input_control_replay_handlers(
            |_| Some(AckOutcome::Failed),
            move |_, _| {
                replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                true
            },
            move |frame| {
                calls_for_handler.fetch_add(1, Ordering::Relaxed);
                *received_for_handler.lock().unwrap() = Some((frame.session, frame.command_id));
                AckOutcome::Ok
            },
        );
        inner.state.epoch.store(7, Ordering::Release);
        let now = now_unix_ms();
        let mut expired = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-2",
            "cmd-stop-expired",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-2",
                "issued_at_ms": now.saturating_sub(160_000),
                "expires_at_ms": now.saturating_sub(130_000),
            }),
        );
        expired["ts"] = Value::from(now.saturating_sub(300_000));
        let expired_response = handle_frame(&inner, &expired.to_string(), Some(&k_room)).unwrap();
        assert_eq!(
            expired_response,
            serde_json::json!({
                "t": "input.ack",
                "command_id": "cmd-stop-expired",
                "outcome": "failed",
            })
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(replay_calls.load(Ordering::Relaxed), 0);

        let original_ct = expired["ct"].clone();
        let original_nonce = expired["n"].clone();
        expired["ts"] = Value::from(now_unix_ms());
        assert_eq!(expired["ct"], original_ct);
        assert_eq!(expired["n"], original_nonce);
        let rewritten_ts_response =
            handle_frame(&inner, &expired.to_string(), Some(&k_room)).unwrap();
        assert_eq!(
            rewritten_ts_response,
            serde_json::json!({
                "t": "input.ack",
                "command_id": "cmd-stop-expired",
                "outcome": "failed",
            })
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(replay_calls.load(Ordering::Relaxed), 0);

        let fresh = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-2",
            "cmd-stop-fresh",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-2",
                "issued_at_ms": now,
                "expires_at_ms": now.saturating_add(30_000),
            }),
        );
        let fresh_response = handle_frame(&inner, &fresh.to_string(), Some(&k_room)).unwrap();
        assert_eq!(
            fresh_response,
            serde_json::json!({
                "t": "input.ack",
                "command_id": "cmd-stop-fresh",
                "outcome": "ok",
            })
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            received.lock().unwrap().as_ref(),
            Some(&("s-2".to_owned(), "cmd-stop-fresh".to_owned()))
        );
    }

    #[test]
    fn legacy_ttl_counterfactual_proves_rewritten_envelope_ts_was_accepted() {
        fn legacy_is_command_ttl_expired(ts_ms: u64, ttl_s: u64, now_ms: u64) -> bool {
            now_ms > ts_ms.saturating_add(ttl_s.saturating_mul(1000))
        }

        let now_ms = 1_000_000_u64;
        let captured_ts_ms = now_ms - 60_000;
        let ttl_s = 30;
        assert!(legacy_is_command_ttl_expired(captured_ts_ms, ttl_s, now_ms));
        assert!(!legacy_is_command_ttl_expired(now_ms, ttl_s, now_ms));
    }

    #[test]
    fn control_stop_replay_ledger_rejects_resealed_duplicate_command_id() {
        let k_room = Zeroizing::new([18_u8; 32]);
        let calls = Arc::new(AtomicU64::new(0));
        let calls_for_handler = Arc::clone(&calls);
        let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let seen_for_handler = Arc::clone(&seen);
        let inner = test_inner_with_input_control_replay_handlers(
            |_| Some(AckOutcome::Failed),
            move |session, command_id| {
                let mut seen = seen_for_handler.lock().unwrap();
                let key = (session.to_owned(), command_id.to_owned());
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            },
            move |_| {
                calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        inner.state.epoch.store(7, Ordering::Release);
        let now = now_unix_ms();
        let first = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-dup",
            "cmd-dup-1",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-dup",
                "issued_at_ms": now,
                "expires_at_ms": now.saturating_add(30_000),
            }),
        );
        let second = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-dup",
            "cmd-dup-1",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-dup",
                "issued_at_ms": now.saturating_add(1),
                "expires_at_ms": now.saturating_add(30_001),
            }),
        );

        let first_response = handle_frame(&inner, &first.to_string(), Some(&k_room)).unwrap();
        assert_eq!(first_response["outcome"], "ok");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let second_response = handle_frame(&inner, &second.to_string(), Some(&k_room)).unwrap();
        assert_eq!(second_response["outcome"], "failed");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn control_stop_rejects_reversed_authenticated_times_before_replay_claim() {
        let k_room = Zeroizing::new([21_u8; 32]);
        let replay_calls = Arc::new(AtomicU64::new(0));
        let stop_calls = Arc::new(AtomicU64::new(0));
        let replay_calls_for_handler = Arc::clone(&replay_calls);
        let stop_calls_for_handler = Arc::clone(&stop_calls);
        let inner = test_inner_with_input_control_replay_handlers(
            |_| Some(AckOutcome::Failed),
            move |_, _| {
                replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                true
            },
            move |_| {
                stop_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        inner.state.epoch.store(7, Ordering::Release);
        let now = now_unix_ms();
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-reversed",
            "cmd-reversed-time",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-reversed",
                "issued_at_ms": now.saturating_add(5_000),
                "expires_at_ms": now,
            }),
        );

        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["outcome"], "failed");
        assert_eq!(replay_calls.load(Ordering::Relaxed), 0);
        assert_eq!(stop_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn control_stop_enforces_max_lifetime_at_thirty_seconds() {
        let k_room = Zeroizing::new([22_u8; 32]);
        let replay_calls = Arc::new(AtomicU64::new(0));
        let stop_calls = Arc::new(AtomicU64::new(0));
        let replay_calls_for_handler = Arc::clone(&replay_calls);
        let stop_calls_for_handler = Arc::clone(&stop_calls);
        let inner = test_inner_with_input_control_replay_handlers(
            |_| Some(AckOutcome::Failed),
            move |_, _| {
                replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                true
            },
            move |_| {
                stop_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        inner.state.epoch.store(7, Ordering::Release);
        let now = now_unix_ms();
        let at_limit = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-lifetime",
            "cmd-lifetime-30000",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-lifetime",
                "issued_at_ms": now,
                "expires_at_ms": now.saturating_add(30_000),
            }),
        );
        let over_limit = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-lifetime",
            "cmd-lifetime-30001",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-lifetime",
                "issued_at_ms": now,
                "expires_at_ms": now.saturating_add(30_001),
            }),
        );

        let at_limit_response = handle_frame(&inner, &at_limit.to_string(), Some(&k_room)).unwrap();
        assert_eq!(at_limit_response["outcome"], "ok");
        assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
        assert_eq!(stop_calls.load(Ordering::Relaxed), 1);

        let over_limit_response =
            handle_frame(&inner, &over_limit.to_string(), Some(&k_room)).unwrap();
        assert_eq!(over_limit_response["outcome"], "failed");
        assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
        assert_eq!(stop_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn control_stop_stale_includes_clock_skew_boundaries() {
        let issued_at_ms = 1_990_000;
        let expires_at_ms = 2_000_000;

        assert!(!is_control_stop_stale(
            issued_at_ms,
            expires_at_ms,
            2_119_999
        ));
        assert!(is_control_stop_stale(
            issued_at_ms,
            expires_at_ms,
            2_120_000
        ));

        let now_ms = 2_000_000;
        assert!(!is_control_stop_stale(2_119_999, 2_129_999, now_ms));
        assert!(is_control_stop_stale(2_120_000, 2_130_000, now_ms));
    }

    #[test]
    fn control_stop_accepts_mismatched_epoch_when_aead_time_window_and_ledger_all_pass() {
        let k_room = Zeroizing::new([23_u8; 32]);
        let replay_calls = Arc::new(AtomicU64::new(0));
        let stop_calls = Arc::new(AtomicU64::new(0));
        let replay_calls_for_handler = Arc::clone(&replay_calls);
        let stop_calls_for_handler = Arc::clone(&stop_calls);
        let inner = test_inner_with_input_control_replay_handlers(
            |_| Some(AckOutcome::Failed),
            move |_, _| {
                replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                true
            },
            move |_| {
                stop_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        inner.state.epoch.store(5, Ordering::Release);
        let now = now_unix_ms();
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            4,
            "control",
            "s-old-epoch",
            "cmd-old-epoch",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-old-epoch",
                "issued_at_ms": now,
                "expires_at_ms": now.saturating_add(30_000),
            }),
        );

        // v1.7.3 裁定：真防线是 AEAD 时间窗 + 持久重放账本，撤销桌面 epoch 前置门，staleness 改由 relay 权威执行。
        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["outcome"], "ok");
        assert_eq!(replay_calls.load(Ordering::Relaxed), 1);
        assert_eq!(stop_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn control_stop_honors_clock_skew_boundaries() {
        let k_room = Zeroizing::new([19_u8; 32]);
        let calls = Arc::new(AtomicU64::new(0));
        let calls_for_handler = Arc::clone(&calls);
        let inner = test_inner_with_input_control_handlers(
            |_| Some(AckOutcome::Failed),
            move |_| {
                calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        inner.state.epoch.store(7, Ordering::Release);
        let now = now_unix_ms();
        let cases = [
            (
                "cmd-skew-expired-within",
                now.saturating_sub(110_000),
                now.saturating_sub(100_000),
                "ok",
                1,
            ),
            (
                "cmd-skew-future-within",
                now.saturating_add(100_000),
                now.saturating_add(110_000),
                "ok",
                2,
            ),
            (
                "cmd-skew-expired-beyond",
                now.saturating_sub(140_000),
                now.saturating_sub(130_000),
                "failed",
                2,
            ),
            (
                "cmd-skew-future-beyond",
                now.saturating_add(130_000),
                now.saturating_add(140_000),
                "failed",
                2,
            ),
        ];

        for (command_id, issued_at_ms, expires_at_ms, expected, expected_calls) in cases {
            let envelope = seal_command_envelope(
                &k_room,
                "0123456789abcdef0123456789abcdef",
                7,
                "control",
                "s-skew",
                command_id,
                &serde_json::json!({
                    "t": "control.stop",
                    "session": "s-skew",
                    "issued_at_ms": issued_at_ms,
                    "expires_at_ms": expires_at_ms,
                }),
            );
            let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
            assert_eq!(response["outcome"], expected, "case {command_id}");
            assert_eq!(
                calls.load(Ordering::Relaxed),
                expected_calls,
                "case {command_id}"
            );
        }
    }

    #[test]
    fn control_stop_rejects_missing_non_integer_zero_or_unsafe_authenticated_times() {
        let k_room = Zeroizing::new([20_u8; 32]);
        let calls = Arc::new(AtomicU64::new(0));
        let replay_calls = Arc::new(AtomicU64::new(0));
        let calls_for_handler = Arc::clone(&calls);
        let replay_calls_for_handler = Arc::clone(&replay_calls);
        let inner = test_inner_with_input_control_replay_handlers(
            |_| Some(AckOutcome::Failed),
            move |_, _| {
                replay_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                true
            },
            move |_| {
                calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        inner.state.epoch.store(7, Ordering::Release);
        let now = now_unix_ms();
        let payloads = [
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "expires_at_ms": now.saturating_add(30_000),
            }),
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "issued_at_ms": now,
            }),
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "issued_at_ms": "not-an-integer",
                "expires_at_ms": now.saturating_add(30_000),
            }),
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "issued_at_ms": now,
                "expires_at_ms": "not-an-integer",
            }),
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "issued_at_ms": 0,
                "expires_at_ms": now.saturating_add(30_000),
            }),
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "issued_at_ms": now,
                "expires_at_ms": 0,
            }),
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "issued_at_ms": JSON_SAFE_INTEGER_MAX + 1,
                "expires_at_ms": now.saturating_add(30_000),
            }),
            serde_json::json!({
                "t": "control.stop",
                "session": "s-bad-time",
                "issued_at_ms": now,
                "expires_at_ms": JSON_SAFE_INTEGER_MAX + 1,
            }),
        ];

        for (index, payload) in payloads.into_iter().enumerate() {
            let command_id = format!("cmd-bad-time-{index}");
            let envelope = seal_command_envelope(
                &k_room,
                "0123456789abcdef0123456789abcdef",
                7,
                "control",
                "s-bad-time",
                &command_id,
                &payload,
            );
            let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
            assert_eq!(response["outcome"], "failed", "case {index}");
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(replay_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn rejects_commands_encrypted_with_an_unpaired_key() {
        let paired_k_room = Zeroizing::new([9_u8; 32]);
        let unpaired_k_room = Zeroizing::new([10_u8; 32]);
        let input_calls = Arc::new(AtomicU64::new(0));
        let control_calls = Arc::new(AtomicU64::new(0));
        let input_calls_for_handler = Arc::clone(&input_calls);
        let control_calls_for_handler = Arc::clone(&control_calls);
        let inner = test_inner_with_input_control_handlers(
            move |_| {
                input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                Some(AckOutcome::Ok)
            },
            move |_| {
                control_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        let input = seal_command_envelope(
            &unpaired_k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-3",
            "cmd-wrong-input",
            &serde_json::json!({"t": "input.send", "session": "s-3", "text": "x"}),
        );
        let control = seal_command_envelope(
            &unpaired_k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-3",
            "cmd-wrong-control",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-3",
                "issued_at_ms": now_unix_ms(),
                "expires_at_ms": now_unix_ms().saturating_add(30_000),
            }),
        );

        for (envelope, command_id) in [(input, "cmd-wrong-input"), (control, "cmd-wrong-control")] {
            let response =
                handle_frame(&inner, &envelope.to_string(), Some(&paired_k_room)).unwrap();
            assert_eq!(response["command_id"], command_id);
            assert_eq!(response["outcome"], "failed");
        }
        assert_eq!(input_calls.load(Ordering::Relaxed), 0);
        assert_eq!(control_calls.load(Ordering::Relaxed), 0);
        assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn malformed_command_envelopes_fail_without_calling_handlers() {
        let k_room = Zeroizing::new([11_u8; 32]);
        let input_calls = Arc::new(AtomicU64::new(0));
        let control_calls = Arc::new(AtomicU64::new(0));
        let input_calls_for_handler = Arc::clone(&input_calls);
        let control_calls_for_handler = Arc::clone(&control_calls);
        let inner = test_inner_with_input_control_handlers(
            move |_| {
                input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                Some(AckOutcome::Ok)
            },
            move |_| {
                control_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );

        assert!(handle_frame(
            &inner,
            r#"{"kind":"input","ct":"x","n":"y"}"#,
            Some(&k_room)
        )
        .is_none());

        let mut missing_ct = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-4",
            "cmd-missing-ct",
            &serde_json::json!({"t": "input.send", "session": "s-4", "text": "x"}),
        );
        missing_ct.as_object_mut().unwrap().remove("ct");
        let missing_ct_response =
            handle_frame(&inner, &missing_ct.to_string(), Some(&k_room)).unwrap();
        assert_eq!(missing_ct_response["outcome"], "failed");

        let mut damaged = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "control",
            "s-4",
            "cmd-damaged-ct",
            &serde_json::json!({
                "t": "control.stop",
                "session": "s-4",
                "issued_at_ms": now_unix_ms(),
                "expires_at_ms": now_unix_ms().saturating_add(30_000),
            }),
        );
        let mut ciphertext = STANDARD.decode(damaged["ct"].as_str().unwrap()).unwrap();
        ciphertext[0] ^= 1;
        damaged["ct"] = Value::from(STANDARD.encode(ciphertext));
        let damaged_response = handle_frame(&inner, &damaged.to_string(), Some(&k_room)).unwrap();
        assert_eq!(damaged_response["outcome"], "failed");

        let missing_key = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-4",
            "cmd-missing-key",
            &serde_json::json!({"t": "input.send", "session": "s-4", "text": "x"}),
        );
        assert!(handle_frame(&inner, &missing_key.to_string(), None).is_none());

        assert_eq!(input_calls.load(Ordering::Relaxed), 0);
        assert_eq!(control_calls.load(Ordering::Relaxed), 0);
        assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn input_send_without_k_room_returns_no_ack_and_preserves_ledger() {
        let k_room = Zeroizing::new([31_u8; 32]);
        let input_calls = Arc::new(AtomicU64::new(0));
        let input_calls_for_handler = Arc::clone(&input_calls);
        let inner = test_inner_with_input_control_handlers(
            move |_| {
                input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                Some(AckOutcome::Queued)
            },
            |_| AckOutcome::Failed,
        );
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-key-retry",
            "cmd-key-retry",
            &serde_json::json!({
                "t": "input.send",
                "session": "s-key-retry",
                "text": "retry after key recovery",
            }),
        );
        let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

        assert!(handle_frame(&inner, &envelope.to_string(), None).is_none());
        assert_eq!(
            inner.state.bad_frames.load(Ordering::Relaxed),
            bad_frames_before
        );
        assert_eq!(input_calls.load(Ordering::Relaxed), 0);

        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["command_id"], "cmd-key-retry");
        assert_eq!(response["outcome"], "queued");
        assert_eq!(input_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn input_send_handler_without_outcome_returns_no_ack_or_bad_frame() {
        let k_room = Zeroizing::new([34_u8; 32]);
        let input_calls = Arc::new(AtomicU64::new(0));
        let input_calls_for_handler = Arc::clone(&input_calls);
        let inner = test_inner_with_input_control_handlers(
            move |_| {
                let previous = input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                (previous > 0).then_some(AckOutcome::Queued)
            },
            |_| AckOutcome::Failed,
        );
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-enqueue-retry",
            "cmd-enqueue-retry",
            &serde_json::json!({
                "t": "input.send",
                "session": "s-enqueue-retry",
                "text": "retry after enqueue recovery",
            }),
        );
        let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

        assert!(handle_frame(&inner, &envelope.to_string(), Some(&k_room)).is_none());
        assert_eq!(
            inner.state.bad_frames.load(Ordering::Relaxed),
            bad_frames_before
        );

        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["command_id"], "cmd-enqueue-retry");
        assert_eq!(response["outcome"], "queued");
        assert_eq!(input_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn input_send_with_pipe_in_session_is_rejected_before_handler() {
        let k_room = Zeroizing::new([32_u8; 32]);
        let input_calls = Arc::new(AtomicU64::new(0));
        let input_calls_for_handler = Arc::clone(&input_calls);
        let inner = test_inner_with_input_control_handlers(
            move |_| {
                input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                Some(AckOutcome::Ok)
            },
            |_| AckOutcome::Failed,
        );
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s|pipe",
            "cmd-session-pipe",
            &serde_json::json!({"t": "input.send", "session": "s|pipe", "text": "x"}),
        );
        let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["outcome"], "failed");
        assert_eq!(
            inner.state.bad_frames.load(Ordering::Relaxed),
            bad_frames_before + 1
        );
        assert_eq!(input_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn input_send_with_command_id_over_128_bytes_is_rejected_before_handler() {
        let k_room = Zeroizing::new([33_u8; 32]);
        let input_calls = Arc::new(AtomicU64::new(0));
        let input_calls_for_handler = Arc::clone(&input_calls);
        let inner = test_inner_with_input_control_handlers(
            move |_| {
                input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                Some(AckOutcome::Ok)
            },
            |_| AckOutcome::Failed,
        );
        let command_id = "c".repeat(COMMAND_ID_MAX_LEN + 1);
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-long-command-id",
            &command_id,
            &serde_json::json!({
                "t": "input.send",
                "session": "s-long-command-id",
                "text": "x",
            }),
        );
        let bad_frames_before = inner.state.bad_frames.load(Ordering::Relaxed);

        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room)).unwrap();
        assert_eq!(response["outcome"], "failed");
        assert_eq!(response["command_id"], command_id);
        assert_eq!(
            inner.state.bad_frames.load(Ordering::Relaxed),
            bad_frames_before + 1
        );
        assert_eq!(input_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_wire_v1_fixture_matches_aad_and_rejects_invalid_commands_before_handlers() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../../remote-relay/fixtures/wire-v1.json"))
                .expect("wire-v1 fixture must be valid JSON");
        let fixtures = fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array");
        let input_calls = Arc::new(AtomicU64::new(0));
        let control_calls = Arc::new(AtomicU64::new(0));
        let input_calls_for_handler = Arc::clone(&input_calls);
        let control_calls_for_handler = Arc::clone(&control_calls);
        let inner = test_inner_with_input_control_handlers(
            move |_| {
                input_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                Some(AckOutcome::Ok)
            },
            move |_| {
                control_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                AckOutcome::Ok
            },
        );
        let k_room = Zeroizing::new([21_u8; 32]);

        for fixture in fixtures {
            if fixture.get("layer").is_some() {
                continue;
            }
            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("fixture name must be a string");
            let envelope_value = fixture
                .get("envelope")
                .expect("fixture envelope must exist");
            let envelope = envelope_value
                .as_object()
                .expect("fixture envelope must be an object");
            for field in [
                "v",
                "room",
                "epoch",
                "kind",
                "session",
                "command_id",
                "seq",
                "ct",
                "n",
                "ts",
            ] {
                assert!(
                    envelope.contains_key(field),
                    "{name}: missing envelope.{field}"
                );
            }

            let expect = fixture
                .get("expect")
                .and_then(Value::as_object)
                .expect("fixture expect must be an object");
            let valid = expect
                .get("valid")
                .and_then(Value::as_bool)
                .expect("fixture expect.valid must be a bool");
            let errors = expect
                .get("errors")
                .and_then(Value::as_array)
                .expect("fixture expect.errors must be an array");
            let aad = expect.get("aad").expect("fixture expect.aad must exist");
            assert!(
                aad.is_string() || aad.is_null(),
                "{name}: aad must be string|null"
            );

            if valid {
                let session = match envelope.get("session") {
                    Some(Value::String(value)) => Some(value.clone()),
                    Some(Value::Null) => None,
                    _ => panic!("{name}: session must be string|null"),
                };
                let command_id = match envelope.get("command_id") {
                    Some(Value::String(value)) => Some(value.clone()),
                    Some(Value::Null) => None,
                    _ => panic!("{name}: command_id must be string|null"),
                };
                let meta = crate::remote_crypto::EnvelopeMeta {
                    v: envelope
                        .get("v")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .expect("valid fixture v must fit u32"),
                    room: envelope
                        .get("room")
                        .and_then(Value::as_str)
                        .expect("valid fixture room must be a string")
                        .to_owned(),
                    epoch: envelope
                        .get("epoch")
                        .and_then(Value::as_u64)
                        .expect("valid fixture epoch must be u64"),
                    kind: envelope
                        .get("kind")
                        .and_then(Value::as_str)
                        .expect("valid fixture kind must be a string")
                        .to_owned(),
                    session,
                    command_id,
                };
                let expected_aad = aad
                    .as_str()
                    .expect("valid fixture expect.aad must be a string");
                assert_eq!(
                    crate::remote_crypto::build_aad(&meta).as_bytes(),
                    expected_aad.as_bytes(),
                    "{name}: AAD mismatch"
                );
            } else if errors
                .iter()
                .any(|error| error.as_str() == Some("command_id_required_for_kind"))
            {
                assert!(
                    handle_command_envelope(&inner, envelope_value, Some(&k_room)).is_none(),
                    "{name}: missing command_id must be rejected before ack/decryption"
                );
                assert_eq!(input_calls.load(Ordering::Relaxed), 0, "{name}");
                assert_eq!(control_calls.load(Ordering::Relaxed), 0, "{name}");
            } else if errors.iter().any(|error| {
                matches!(
                    error.as_str(),
                    Some("session_must_not_contain_pipe")
                        | Some("command_id_must_not_contain_pipe")
                        | Some("command_id_too_long")
                )
            }) {
                let response = handle_command_envelope(&inner, envelope_value, Some(&k_room))
                    .expect("invalid command with a usable command_id must receive failed ack");
                assert_eq!(response["outcome"], "failed", "{name}");
                assert_eq!(input_calls.load(Ordering::Relaxed), 0, "{name}");
                assert_eq!(control_calls.load(Ordering::Relaxed), 0, "{name}");
            }
        }
    }

    const WIRE_V1_TOKEN_LAYERS: [&str; 9] = [
        "token-frame",
        "subprotocol",
        "http",
        "desktop-upgrade",
        "inbound-matrix",
        "time-window",
        "ttl-clamp",
        "aad-kat",
        "chain",
    ];

    fn wire_v1_fixtures() -> Value {
        serde_json::from_str(include_str!("../../../remote-relay/fixtures/wire-v1.json"))
            .expect("wire-v1 fixture must be valid JSON")
    }

    fn wire_v1_is_hex64(value: &str) -> bool {
        value.len() == 64
            && value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    }

    fn wire_v1_decode_hex_32(value: &str, name: &str, field: &str) -> [u8; 32] {
        assert!(wire_v1_is_hex64(value), "{name}: {field} must be hex64");
        let mut decoded = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(pair)
                .unwrap_or_else(|_| panic!("{name}: {field} must be ASCII"));
            decoded[index] = u8::from_str_radix(pair, 16)
                .unwrap_or_else(|_| panic!("{name}: {field} invalid hex at byte {index}"));
        }
        decoded
    }

    fn wire_v1_lower_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        encoded
    }

    const WIRE_V1_JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

    fn wire_v1_is_safe_u64(value: &Value) -> bool {
        value
            .as_u64()
            .is_some_and(|value| value <= WIRE_V1_JSON_SAFE_INTEGER_MAX)
    }

    fn wire_v1_required_string_error(field: &str) -> &'static str {
        match field {
            "subject" => "subject_required",
            "request_id" => "request_id_required",
            "ct" => "ct_required",
            "n" => "n_required",
            "room" => "room_required",
            "device_id" => "device_id_required",
            "k_room_ct" => "k_room_ct_required",
            "k_room_n" => "k_room_n_required",
            "tokens_ct" => "tokens_ct_required",
            "tokens_n" => "tokens_n_required",
            "reason" => "reason_required",
            "remote_pub" => "remote_pub_required",
            "token_ct" => "token_ct_required",
            "token_n" => "token_n_required",
            "origin_connection_id" => "origin_connection_id_required",
            "confirm_ct" => "confirm_ct_required",
            "confirm_n" => "confirm_n_required",
            _ => "string_field_required",
        }
    }

    fn wire_v1_required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, &'static str> {
        value
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| wire_v1_required_string_error(field))
    }

    fn wire_v1_positive_integer(
        value: &Value,
        field: &str,
        missing: &'static str,
        non_positive: &'static str,
    ) -> Result<(), &'static str> {
        let Some(number) = value.get(field) else {
            return Err(missing);
        };
        if number.as_u64().is_some_and(|number| number > 0) {
            Ok(())
        } else {
            Err(non_positive)
        }
    }

    fn wire_v1_timestamp_error(value: Option<&Value>) -> Option<&'static str> {
        let Some(timestamp) = value.and_then(Value::as_u64) else {
            return Some("timestamp_must_be_positive");
        };
        if timestamp == 0 {
            Some("timestamp_must_be_positive")
        } else if timestamp > WIRE_V1_JSON_SAFE_INTEGER_MAX {
            Some("timestamp_exceeds_json_safe_integer")
        } else {
            None
        }
    }

    fn wire_v1_subject_error(subject: &str) -> Option<&'static str> {
        if subject == "pairing" {
            return None;
        }
        let Some(uuid) = subject.strip_prefix("device:") else {
            return Some("subject_invalid");
        };
        let valid = uuid.len() == 36
            && uuid.bytes().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            });
        (!valid).then_some("subject_invalid")
    }

    fn wire_v1_put_body_error(body: &Value) -> Option<&'static str> {
        let subject = match wire_v1_required_string(body, "subject") {
            Ok(subject) => subject,
            Err(error) => return Some(error),
        };
        if let Some(error) = wire_v1_subject_error(subject) {
            return Some(error);
        }
        if let Err(error) = wire_v1_positive_integer(
            body,
            "generation",
            "generation_required",
            "generation_must_be_positive",
        ) {
            return Some(error);
        }
        let Some(scope) = body.get("scope").and_then(Value::as_str) else {
            return Some("scope_invalid");
        };
        if !["remote", "pairing"].contains(&scope) {
            return Some("scope_invalid");
        }
        if subject == "pairing" && scope != "pairing" {
            return Some("pairing_scope_required");
        }
        let Some(current) = body.get("current").filter(|value| value.is_object()) else {
            return Some("current_required");
        };
        let Some(token_hash) = current.get("token_hash") else {
            return Some("current_token_hash_required");
        };
        if !token_hash.as_str().is_some_and(wire_v1_is_hex64) {
            return Some("token_hash_invalid");
        }
        if let Some(error) = wire_v1_timestamp_error(current.get("access_expires")) {
            return Some(error);
        }
        if scope == "remote" {
            if let Some(error) = wire_v1_timestamp_error(current.get("refresh_until")) {
                return Some(error);
            }
            if current["access_expires"].as_u64() > current["refresh_until"].as_u64() {
                return Some("access_expires_after_refresh_until");
            }
        }
        if let Some(prev) = body.get("prev") {
            if subject == "pairing" {
                return Some("pairing_prev_forbidden");
            }
            if !prev.is_object() {
                return Some("prev_invalid");
            }
            let Some(token_hash) = prev.get("token_hash") else {
                return Some("prev_token_hash_required");
            };
            if !token_hash.as_str().is_some_and(wire_v1_is_hex64) {
                return Some("token_hash_invalid");
            }
            if let Err(error) = wire_v1_positive_integer(
                prev,
                "generation",
                "generation_required",
                "generation_must_be_positive",
            ) {
                return Some(error);
            }
            if let Some(error) = wire_v1_timestamp_error(prev.get("prev_expires")) {
                return Some(error);
            }
        }
        None
    }

    fn wire_v1_token_frame_error(frame: &Value) -> Option<&'static str> {
        let required_strings = |fields: &[&str]| {
            fields
                .iter()
                .find_map(|field| wire_v1_required_string(frame, field).err())
        };
        let subject = || match wire_v1_required_string(frame, "subject") {
            Ok(subject) => wire_v1_subject_error(subject),
            Err(error) => Some(error),
        };
        let generation = |field, missing, non_positive| {
            wire_v1_positive_integer(frame, field, missing, non_positive).err()
        };
        let entries = |limit_error: &'static str| {
            let Some(items) = frame.get("entries").and_then(Value::as_array) else {
                return Some("entries_required");
            };
            if items.len() > 256 {
                return Some(limit_error);
            }
            for entry in items {
                if !entry.is_object() {
                    return Some("entry_invalid");
                }
                if let Some(error) = wire_v1_put_body_error(entry) {
                    return Some(error);
                }
            }
            None
        };

        match frame.get("t").and_then(Value::as_str) {
            Some("token.put") => wire_v1_put_body_error(frame),
            Some("token.delete") => subject()
                .or_else(|| {
                    generation(
                        "generation",
                        "generation_required",
                        "generation_must_be_positive",
                    )
                })
                .or_else(|| {
                    frame
                        .get("close")
                        .is_some_and(|value| !value.is_boolean())
                        .then_some("close_invalid")
                }),
            Some("token.ack") => subject()
                .or_else(|| {
                    generation(
                        "generation",
                        "generation_required",
                        "generation_must_be_positive",
                    )
                })
                .or_else(|| {
                    (!matches!(
                        frame.get("result").and_then(Value::as_str),
                        Some("ok" | "idempotent" | "rejected")
                    ))
                    .then_some("result_invalid")
                })
                .or_else(|| {
                    frame
                        .get("reason")
                        .is_some_and(|value| !value.is_string())
                        .then_some("reason_invalid")
                }),
            Some(kind @ ("token.sync" | "token.reset")) => {
                generation("revision", "revision_required", "revision_must_be_positive").or_else(
                    || {
                        entries(if kind == "token.sync" {
                            "sync_entries_too_many"
                        } else {
                            "reset_entries_too_many"
                        })
                    },
                )
            }
            Some("token.sync.ack") => {
                generation("revision", "revision_required", "revision_must_be_positive").or_else(
                    || {
                        (!frame
                            .get("relay_high_water")
                            .is_some_and(|value| value.as_u64().is_some()))
                        .then_some("relay_high_water_required")
                    },
                )
            }
            Some("token.refresh") => required_strings(&["request_id", "ct", "n"]),
            Some("token.refresh.forward") => required_strings(&["request_id"])
                .or_else(subject)
                .or_else(|| {
                    generation(
                        "request_generation",
                        "request_generation_required",
                        "request_generation_must_be_positive",
                    )
                })
                .or_else(|| required_strings(&["ct", "n"])),
            Some("token.refresh.ok") => required_strings(&["request_id"])
                .or_else(subject)
                .or_else(|| {
                    generation(
                        "generation",
                        "generation_required",
                        "generation_must_be_positive",
                    )
                })
                .or_else(|| required_strings(&["ct", "n"])),
            Some("token.refresh.fail") => required_strings(&["request_id"])
                .or_else(subject)
                .or_else(|| required_strings(&["reason"]))
                .or_else(|| {
                    frame
                        .get("close")
                        .is_some_and(|value| !value.is_boolean())
                        .then_some("close_invalid")
                }),
            // S1i3 K3.5：这两条只校验 wire-v1.json 里静态样张的形状（"origin_connection_id
            // 必须存在"），不驱动、也不消费 relay 侧 room-do.js 真实的转发/盖章/落路由
            // 逻辑——「样张有测试」不等于「转发代码有测试」。真正驱动真路径、钉住 relay
            // 实际转发出的帧的是 remote-relay/test/room-do.test.js 里
            // 「pair.hello：relay 转发前盖章 origin_connection_id」与
            // 「pair.done：relay 转发前盖章 origin_connection_id」两条真路径测试
            // （别再写死行号——行号会随后续插入测试漂移，陈旧的行号引用比没有引用更坏）。
            Some("pair.hello") => required_strings(&[
                "room",
                "remote_pub",
                "token_ct",
                "token_n",
                "origin_connection_id",
            ])
            .or_else(|| {
                (!frame["room"].as_str().is_some_and(|room| {
                    room.len() == 32
                        && room
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }))
                .then_some("room_invalid")
            }),
            Some("pair.done") => required_strings(&[
                "room",
                "device_id",
                "confirm_ct",
                "confirm_n",
                "origin_connection_id",
            ])
            .or_else(|| {
                (!frame["room"].as_str().is_some_and(|room| {
                    room.len() == 32
                        && room
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }))
                .then_some("room_invalid")
            }),
            Some("pair.ready") => {
                required_strings(&["room", "device_id", "ct", "n"]).or_else(|| {
                    (!frame["room"].as_str().is_some_and(|room| {
                        room.len() == 32
                            && room
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    }))
                    .then_some("room_invalid")
                })
            }
            Some("pair.accept") => {
                if frame.get("capability_token").is_some() || frame.get("refresh_token").is_some() {
                    return Some("plaintext_token_forbidden");
                }
                required_strings(&[
                    "room",
                    "device_id",
                    "k_room_ct",
                    "k_room_n",
                    "tokens_ct",
                    "tokens_n",
                ])
                .or_else(|| {
                    (!frame["room"].as_str().is_some_and(|room| {
                        room.len() == 32
                            && room
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    }))
                    .then_some("room_invalid")
                })
            }
            _ => Some("frame_type_invalid"),
        }
    }

    fn wire_v1_collect_named_values<'a>(
        value: &'a Value,
        field: &str,
        values: &mut Vec<&'a Value>,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    wire_v1_collect_named_values(item, field, values);
                }
            }
            Value::Object(object) => {
                for (key, child) in object {
                    if key == field {
                        values.push(child);
                    }
                    wire_v1_collect_named_values(child, field, values);
                }
            }
            _ => {}
        }
    }

    fn wire_v1_subprotocol_decision<'a>(offers: &[&'a str]) -> Option<&'a str> {
        let token_offers = offers
            .iter()
            .copied()
            .filter(|offer| offer.starts_with("token."))
            .collect::<Vec<_>>();
        if !offers.contains(&"agentloom-rc-v1") || token_offers.len() != 1 {
            return None;
        }
        let token_hex = token_offers[0].strip_prefix("token.")?;
        wire_v1_is_hex64(token_hex).then_some(token_hex)
    }

    fn wire_v1_sha256_ascii_hex(value: &str) -> String {
        use sha2::Digest as _;

        wire_v1_lower_hex(&sha2::Sha256::digest(value.as_bytes()))
    }

    fn wire_v1_desktop_upgrade_decision(fixture: &Value) -> Value {
        let credential_matches = fixture
            .get("authorization")
            .and_then(Value::as_str)
            .and_then(|authorization| authorization.strip_prefix("Bearer "))
            .is_some_and(|credential| {
                wire_v1_is_hex64(credential)
                    && fixture.get("credential_hex").and_then(Value::as_str) == Some(credential)
                    && fixture["pre_state"]["owner_credential_hash"].as_str()
                        == Some(wire_v1_sha256_ascii_hex(credential).as_str())
            });
        if fixture["pre_state"]["tombstoned"] == Value::Bool(true) {
            return serde_json::json!({ "accept": false, "status": 410 });
        }
        if !credential_matches {
            return serde_json::json!({ "accept": false, "status": 401 });
        }
        serde_json::json!({ "accept": true, "role": "desktop", "epoch_bump": true })
    }

    fn wire_v1_http_body_byte_len(body: &Value) -> usize {
        body.as_str().map_or_else(
            || {
                serde_json::to_vec(body)
                    .expect("HTTP fixture body must serialize")
                    .len()
            },
            |body| body.len(),
        )
    }

    fn wire_v1_http_status_decision(fixture: &Value) -> u64 {
        let request = &fixture["request"];
        let pre_state = &fixture["pre_state"];
        if pre_state["rate_limited"] == Value::Bool(true) {
            return 429;
        }
        if pre_state["tombstoned"] == Value::Bool(true)
            || pre_state["owner"].as_str() == Some("tombstoned")
        {
            return 410;
        }

        match request["method"].as_str() {
            Some("POST") => {
                let body = &request["body"];
                if body.as_object().is_some_and(|body| {
                    !body
                        .get("credential_hash")
                        .and_then(Value::as_str)
                        .is_some_and(wire_v1_is_hex64)
                }) {
                    return 400;
                }
                if wire_v1_http_body_byte_len(body) > 1024 {
                    return 413;
                }
                if !body.is_object() {
                    return 400;
                }
                match pre_state["owner"].as_str() {
                    Some("none") => 200,
                    Some("same" | "other") => {
                        if body["credential_hash"] == pre_state["owner_credential_hash"] {
                            200
                        } else {
                            409
                        }
                    }
                    _ => 401,
                }
            }
            Some("DELETE") => {
                let credential_matches = request["headers"]["authorization"]
                    .as_str()
                    .and_then(|authorization| authorization.strip_prefix("Bearer "))
                    .is_some_and(|credential| {
                        wire_v1_is_hex64(credential)
                            && fixture.get("credential_hex").and_then(Value::as_str)
                                == Some(credential)
                            && pre_state["owner_credential_hash"].as_str()
                                == Some(wire_v1_sha256_ascii_hex(credential).as_str())
                    });
                if credential_matches {
                    200
                } else {
                    401
                }
            }
            _ => 400,
        }
    }

    fn wire_v1_inbound_matrix_decision(scope: &str, frame_type: &str) -> Value {
        let allowed = match scope {
            "pairing" => ["pair.hello", "pair.done"].contains(&frame_type),
            "remote" => ["input", "control", "presence", "token.refresh"].contains(&frame_type),
            "refresh" => frame_type == "token.refresh",
            "desktop" => [
                "event",
                "live",
                "control.notify_hint",
                "input.ack",
                "pair.accept",
                "pair.ready",
                "token.put",
                "token.delete",
                "token.sync",
                "token.reset",
                "token.refresh.ok",
                "token.refresh.fail",
            ]
            .contains(&frame_type),
            _ => false,
        };
        if allowed {
            serde_json::json!({ "allowed": true })
        } else {
            serde_json::json!({ "allowed": false, "error": "role_forbidden" })
        }
    }

    fn wire_v1_time_window_decision(now_ms: u64, row: &Value) -> &'static str {
        let kind = row
            .get("kind")
            .and_then(Value::as_str)
            .expect("time-window row.kind must be a string");
        let scope = row
            .get("scope")
            .and_then(Value::as_str)
            .expect("time-window row.scope must be a string");
        let subject_state = row
            .get("subject_state")
            .and_then(Value::as_str)
            .expect("time-window row.subject_state must be a string");
        let access_expires = row
            .get("access_expires")
            .and_then(Value::as_u64)
            .expect("time-window row.access_expires must be u64");
        let valid_until = row
            .get("valid_until")
            .and_then(Value::as_u64)
            .expect("time-window row.valid_until must be u64");

        if subject_state != "active" {
            return "reject:401";
        }
        if row.get("generation").is_some()
            && row.get("generation").and_then(Value::as_u64)
                != row.get("current_generation").and_then(Value::as_u64)
        {
            return "reject:stale_generation";
        }
        if scope == "pairing" {
            if kind == "current" && valid_until == access_expires && now_ms < access_expires {
                return "scope:pairing";
            }
            return "reject:401";
        }
        if kind == "current" && now_ms < access_expires {
            return "scope:remote";
        }
        if kind == "current" && now_ms < valid_until {
            return "scope:refresh";
        }
        if kind == "prev" && now_ms < valid_until {
            return "scope:refresh";
        }
        "reject:401"
    }

    fn wire_v1_ttl_cap_ms(cap: &str) -> Option<u64> {
        match cap {
            "pairing" => Some(330_000),
            "access" => Some(3_900_000),
            "prev" => Some(172_800_000),
            "refresh_until" => Some(2_592_000_000),
            _ => None,
        }
    }

    #[test]
    fn shared_connect_kdf_v1_vectors_match_ascii_hex_spec() {
        use sha2::Digest as _;

        let fixtures: Value = serde_json::from_str(include_str!(
            "../../../remote-relay/fixtures/connect-kdf-v1.json"
        ))
        .expect("connect-kdf-v1 fixture must be valid JSON");
        let fixtures = fixtures
            .as_array()
            .expect("connect-kdf-v1 fixture root must be an array");
        assert!(
            fixtures.len() >= 3,
            "at least three connect-KDF vectors are required"
        );
        let mut names = std::collections::HashSet::new();

        for fixture in fixtures {
            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("connect-KDF fixture name must be a string");
            assert!(names.insert(name), "duplicate fixture name: {name}");
            let pairing_token_hex = fixture
                .get("pairing_token_hex")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: pairing_token_hex must be a string"));
            assert!(
                wire_v1_is_hex64(pairing_token_hex),
                "{name}: pairing_token_hex must be lowercase hex64"
            );
            let expect = fixture
                .get("expect")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("{name}: expect must be an object"));
            let expected_connect_token_hex = expect
                .get("connect_token_hex")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: expect.connect_token_hex must be a string"));
            let expected_token_hash_hex = expect
                .get("token_hash_hex")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: expect.token_hash_hex must be a string"));
            assert!(
                wire_v1_is_hex64(expected_connect_token_hex),
                "{name}: expect.connect_token_hex"
            );
            assert!(
                wire_v1_is_hex64(expected_token_hash_hex),
                "{name}: expect.token_hash_hex"
            );

            let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(None, pairing_token_hex.as_bytes());
            let mut connect_token = [0_u8; 32];
            hkdf.expand(b"agentloom-rc-connect-v1", &mut connect_token)
                .expect("32-byte HKDF-SHA256 output is valid");
            let connect_token_hex = wire_v1_lower_hex(&connect_token);
            let token_hash = sha2::Sha256::digest(connect_token_hex.as_bytes());
            let token_hash_hex = wire_v1_lower_hex(&token_hash);

            assert_eq!(
                connect_token_hex, expected_connect_token_hex,
                "{name}: connect_token"
            );
            assert_eq!(
                token_hash_hex, expected_token_hash_hex,
                "{name}: token_hash"
            );
        }
    }

    #[test]
    fn shared_wire_v1_token_plane_layers_structurally_valid() {
        let fixtures = wire_v1_fixtures();
        let fixtures = fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array");
        let mut names = std::collections::HashSet::new();
        let mut directions = WIRE_V1_TOKEN_LAYERS
            .into_iter()
            .filter(|layer| !["aad-kat", "chain"].contains(layer))
            .map(|layer| (layer, (0_u32, 0_u32)))
            .collect::<HashMap<_, _>>();
        let mut aad_kat_count = 0_u32;
        let mut aad_kat_kinds = std::collections::HashSet::new();

        for fixture in fixtures {
            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("fixture name must be a string");
            assert!(names.insert(name), "duplicate fixture name: {name}");

            let Some(layer_value) = fixture.get("layer") else {
                continue;
            };
            let layer = layer_value
                .as_str()
                .unwrap_or_else(|| panic!("{name}: layer must be a string"));
            assert!(
                WIRE_V1_TOKEN_LAYERS.contains(&layer),
                "{name}: unknown layer {layer}"
            );
            let expect = fixture
                .get("expect")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("{name}: expect must be an object"));
            if layer == "aad-kat" {
                aad_kat_count += 1;
                let meta = fixture
                    .get("meta")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| panic!("{name}: meta must be an object"));
                assert_eq!(
                    meta.get("v").and_then(Value::as_u64),
                    Some(1),
                    "{name}: meta.v"
                );
                assert!(
                    meta.get("room")
                        .and_then(Value::as_str)
                        .is_some_and(|room| room.len() == 32
                            && room
                                .as_bytes()
                                .iter()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))),
                    "{name}: meta.room"
                );
                assert_eq!(
                    meta.get("epoch").and_then(Value::as_u64),
                    Some(0),
                    "{name}: meta.epoch"
                );
                let kind = meta
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: meta.kind must be a string"));
                assert!(
                    [
                        "pair-ready",
                        "pair-accept-tokens",
                        "token.refresh",
                        "token.refresh.ok"
                    ]
                    .contains(&kind),
                    "{name}: meta.kind"
                );
                aad_kat_kinds.insert(kind);
                let device_id = fixture
                    .get("device_id")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: device_id must be a string"));
                assert_eq!(
                    meta.get("session").and_then(Value::as_str),
                    Some(device_id),
                    "{name}: meta.session/device_id"
                );
                if ["pair-ready", "pair-accept-tokens"].contains(&kind) {
                    assert_eq!(
                        meta.get("command_id"),
                        Some(&Value::Null),
                        "{name}: meta.command_id"
                    );
                    assert!(
                        fixture.get("request_id").is_none(),
                        "{name}: request_id must be absent"
                    );
                } else {
                    let request_id = fixture
                        .get("request_id")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| panic!("{name}: request_id must be a string"));
                    assert_eq!(
                        meta.get("command_id").and_then(Value::as_str),
                        Some(request_id),
                        "{name}: meta.command_id/request_id"
                    );
                }
                assert!(
                    expect.get("aad").is_some_and(Value::is_string),
                    "{name}: expect.aad"
                );
                let kat = fixture
                    .get("kat")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| panic!("{name}: kat must be an object"));
                assert!(
                    kat.get("key_hex")
                        .and_then(Value::as_str)
                        .is_some_and(wire_v1_is_hex64),
                    "{name}: kat.key_hex"
                );
                assert_eq!(
                    kat.get("n_b64")
                        .and_then(Value::as_str)
                        .and_then(|value| STANDARD.decode(value).ok())
                        .map(|bytes| bytes.len()),
                    Some(12),
                    "{name}: kat.n_b64"
                );
                assert!(
                    kat.get("ct_b64")
                        .and_then(Value::as_str)
                        .and_then(|value| STANDARD.decode(value).ok())
                        .is_some_and(|bytes| bytes.len() > 16),
                    "{name}: kat.ct_b64"
                );
                let plaintext = kat
                    .get("plaintext")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: kat.plaintext must be a string"));
                if kind == "pair-accept-tokens" {
                    let plaintext: Value = serde_json::from_str(plaintext)
                        .unwrap_or_else(|error| panic!("{name}: plaintext JSON: {error}"));
                    let plaintext = plaintext
                        .as_object()
                        .unwrap_or_else(|| panic!("{name}: plaintext must be an object"));
                    assert_eq!(plaintext.len(), 2, "{name}: plaintext field count");
                    for field in ["capability_token", "refresh_token"] {
                        assert!(
                            plaintext
                                .get(field)
                                .and_then(Value::as_str)
                                .is_some_and(wire_v1_is_hex64),
                            "{name}: plaintext.{field}"
                        );
                    }
                }
                continue;
            }
            if layer == "chain" {
                let token_hex = fixture
                    .get("connect_token_hex")
                    .or_else(|| fixture.get("capability_token_hex"))
                    .and_then(Value::as_str);
                assert!(
                    token_hex.is_some_and(wire_v1_is_hex64),
                    "{name}: access token"
                );
                if fixture.get("connect_token_hex").is_some() {
                    assert!(
                        fixture
                            .get("pairing_token_hex")
                            .and_then(Value::as_str)
                            .is_some_and(wire_v1_is_hex64),
                        "{name}: pairing_token_hex"
                    );
                }
                assert!(
                    fixture
                        .get("token_hash_hex")
                        .and_then(Value::as_str)
                        .is_some_and(wire_v1_is_hex64),
                    "{name}: token_hash_hex"
                );
                assert!(
                    fixture
                        .get("subprotocol_offer")
                        .is_some_and(Value::is_array),
                    "{name}: subprotocol_offer"
                );
                assert!(
                    fixture.get("put_frame").is_some_and(Value::is_object),
                    "{name}: put_frame"
                );
                assert!(
                    fixture.get("window").is_some_and(Value::is_object),
                    "{name}: window"
                );
                assert_eq!(
                    expect
                        .get("scope")
                        .and_then(Value::as_str)
                        .is_some_and(|scope| ["pairing", "remote"].contains(&scope)),
                    true,
                    "{name}: expect.scope"
                );
                continue;
            }
            let direction = directions
                .get_mut(layer)
                .unwrap_or_else(|| panic!("{name}: layer direction missing"));

            match layer {
                "token-frame" => {
                    let frame = fixture
                        .get("frame")
                        .and_then(Value::as_object)
                        .unwrap_or_else(|| panic!("{name}: frame must be an object"));
                    assert!(
                        frame.get("t").is_some_and(Value::is_string),
                        "{name}: frame.t"
                    );
                    let valid = expect
                        .get("valid")
                        .and_then(Value::as_bool)
                        .unwrap_or_else(|| panic!("{name}: expect.valid must be a bool"));
                    let errors = expect
                        .get("errors")
                        .and_then(Value::as_array)
                        .unwrap_or_else(|| panic!("{name}: expect.errors must be an array"));
                    assert!(
                        errors.iter().all(Value::is_string),
                        "{name}: errors must be strings"
                    );
                    assert_eq!(valid, errors.is_empty(), "{name}: valid/errors disagree");

                    let mut hashes = Vec::new();
                    wire_v1_collect_named_values(
                        fixture.get("frame").expect("frame exists"),
                        "token_hash",
                        &mut hashes,
                    );
                    let hash_invalid = errors
                        .iter()
                        .any(|error| error.as_str() == Some("token_hash_invalid"));
                    if hash_invalid {
                        assert!(
                            hashes.iter().any(|hash| {
                                hash.as_str().map_or(true, |value| !wire_v1_is_hex64(value))
                            }),
                            "{name}: malformed hash missing"
                        );
                    } else {
                        assert!(
                            hashes
                                .iter()
                                .all(|hash| hash.as_str().is_some_and(wire_v1_is_hex64)),
                            "{name}: token_hash"
                        );
                    }
                    if valid {
                        direction.0 += 1;
                    } else {
                        direction.1 += 1;
                    }
                }
                "subprotocol" => {
                    let offers = fixture
                        .get("offers")
                        .and_then(Value::as_array)
                        .unwrap_or_else(|| panic!("{name}: offers must be an array"));
                    assert!(offers.iter().all(Value::is_string), "{name}: offers");
                    let accept = expect
                        .get("accept")
                        .and_then(Value::as_bool)
                        .unwrap_or_else(|| panic!("{name}: expect.accept must be a bool"));
                    if accept {
                        assert_eq!(
                            expect.get("echo").and_then(Value::as_str),
                            Some("agentloom-rc-v1"),
                            "{name}: echo"
                        );
                        assert!(
                            expect
                                .get("token_hex")
                                .and_then(Value::as_str)
                                .is_some_and(wire_v1_is_hex64),
                            "{name}: token_hex"
                        );
                    } else {
                        assert_eq!(
                            expect.get("status").and_then(Value::as_u64),
                            Some(401),
                            "{name}: reject status"
                        );
                    }
                    if accept {
                        direction.0 += 1;
                    } else {
                        direction.1 += 1;
                    }
                }
                "http" => {
                    let request = fixture
                        .get("request")
                        .and_then(Value::as_object)
                        .unwrap_or_else(|| panic!("{name}: request must be an object"));
                    assert!(
                        request.get("method").is_some_and(Value::is_string),
                        "{name}: request.method"
                    );
                    assert!(
                        request.get("path").is_some_and(Value::is_string),
                        "{name}: request.path"
                    );
                    let pre_state = fixture
                        .get("pre_state")
                        .and_then(Value::as_object)
                        .unwrap_or_else(|| panic!("{name}: pre_state must be an object"));
                    assert!(
                        pre_state
                            .get("owner")
                            .and_then(Value::as_str)
                            .is_some_and(
                                |owner| ["none", "same", "other", "tombstoned"].contains(&owner)
                            ),
                        "{name}: pre_state.owner"
                    );
                    if let Some(rate_limited) = pre_state.get("rate_limited") {
                        assert!(rate_limited.is_boolean(), "{name}: pre_state.rate_limited");
                    }
                    if let Some(tombstoned) = pre_state.get("tombstoned") {
                        assert!(tombstoned.is_boolean(), "{name}: pre_state.tombstoned");
                    }
                    if let Some(credential) = fixture.get("credential_hex") {
                        assert!(
                            credential.as_str().is_some_and(wire_v1_is_hex64),
                            "{name}: credential_hex"
                        );
                    }
                    let status = expect
                        .get("status")
                        .and_then(Value::as_u64)
                        .unwrap_or_else(|| panic!("{name}: expect.status must be u64"));
                    assert_eq!(wire_v1_http_status_decision(fixture), status, "{name}");
                    if status < 400 {
                        direction.0 += 1;
                    } else {
                        direction.1 += 1;
                    }
                }
                "desktop-upgrade" => {
                    assert!(
                        fixture.get("authorization") == Some(&Value::Null)
                            || fixture.get("authorization").is_some_and(Value::is_string),
                        "{name}: authorization"
                    );
                    if let Some(credential) = fixture.get("credential_hex") {
                        assert!(
                            credential.as_str().is_some_and(wire_v1_is_hex64),
                            "{name}: credential_hex"
                        );
                    }
                    let pre_state = fixture
                        .get("pre_state")
                        .and_then(Value::as_object)
                        .unwrap_or_else(|| panic!("{name}: pre_state must be an object"));
                    assert!(
                        pre_state
                            .get("owner_credential_hash")
                            .and_then(Value::as_str)
                            .is_some_and(wire_v1_is_hex64),
                        "{name}: owner_credential_hash"
                    );
                    assert!(
                        pre_state.get("tombstoned").is_some_and(Value::is_boolean),
                        "{name}: tombstoned"
                    );
                    let accept = expect
                        .get("accept")
                        .and_then(Value::as_bool)
                        .unwrap_or_else(|| panic!("{name}: expect.accept must be a bool"));
                    if accept {
                        assert_eq!(
                            expect.get("role").and_then(Value::as_str),
                            Some("desktop"),
                            "{name}: role"
                        );
                        assert_eq!(
                            expect.get("epoch_bump").and_then(Value::as_bool),
                            Some(true),
                            "{name}: epoch_bump"
                        );
                        direction.0 += 1;
                    } else {
                        assert!(
                            expect
                                .get("status")
                                .and_then(Value::as_u64)
                                .is_some_and(|status| [401, 410].contains(&status)),
                            "{name}: status"
                        );
                        direction.1 += 1;
                    }
                }
                "inbound-matrix" => {
                    assert!(
                        fixture
                            .get("scope")
                            .and_then(Value::as_str)
                            .is_some_and(|scope| {
                                ["pairing", "remote", "refresh", "desktop"].contains(&scope)
                            }),
                        "{name}: scope"
                    );
                    assert!(
                        fixture.get("frame_t").is_some_and(Value::is_string),
                        "{name}: frame_t"
                    );
                    let allowed = expect
                        .get("allowed")
                        .and_then(Value::as_bool)
                        .unwrap_or_else(|| panic!("{name}: expect.allowed must be a bool"));
                    if allowed {
                        direction.0 += 1;
                    } else {
                        assert_eq!(
                            expect.get("error").and_then(Value::as_str),
                            Some("role_forbidden"),
                            "{name}: expect.error"
                        );
                        direction.1 += 1;
                    }
                }
                "time-window" => {
                    assert!(
                        fixture.get("now_ms").is_some_and(wire_v1_is_safe_u64),
                        "{name}: now_ms"
                    );
                    let row = fixture
                        .get("row")
                        .and_then(Value::as_object)
                        .unwrap_or_else(|| panic!("{name}: row must be an object"));
                    assert!(
                        row.get("kind")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| ["current", "prev"].contains(&kind)),
                        "{name}: row.kind"
                    );
                    assert!(
                        row.get("scope")
                            .and_then(Value::as_str)
                            .is_some_and(|scope| ["remote", "pairing"].contains(&scope)),
                        "{name}: row.scope"
                    );
                    assert!(
                        row.get("subject_state")
                            .and_then(Value::as_str)
                            .is_some_and(|state| ["active", "revoked"].contains(&state)),
                        "{name}: row.subject_state"
                    );
                    assert!(
                        row.get("access_expires").is_some_and(wire_v1_is_safe_u64),
                        "{name}: access_expires"
                    );
                    assert!(
                        row.get("valid_until").is_some_and(wire_v1_is_safe_u64),
                        "{name}: valid_until"
                    );
                    let has_generation = row.get("generation").is_some();
                    assert_eq!(
                        has_generation,
                        row.get("current_generation").is_some(),
                        "{name}: generation fields must appear together"
                    );
                    if has_generation {
                        assert!(
                            row.get("generation").is_some_and(wire_v1_is_safe_u64),
                            "{name}: generation"
                        );
                        assert!(
                            row.get("current_generation")
                                .is_some_and(wire_v1_is_safe_u64),
                            "{name}: current_generation"
                        );
                    }
                    let decision = expect
                        .get("decision")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| panic!("{name}: expect.decision must be a string"));
                    assert!(
                        [
                            "scope:remote",
                            "scope:pairing",
                            "scope:refresh",
                            "reject:401",
                            "reject:stale_generation"
                        ]
                        .contains(&decision),
                        "{name}: expect.decision"
                    );
                    if decision == "reject:stale_generation" {
                        assert_eq!(
                            expect.get("close").and_then(Value::as_bool),
                            Some(true),
                            "{name}: stale generation must close"
                        );
                    }
                    if decision.starts_with("reject:") {
                        direction.1 += 1;
                    } else {
                        direction.0 += 1;
                    }
                }
                "ttl-clamp" => {
                    let cap = fixture
                        .get("cap")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| panic!("{name}: cap must be a string"));
                    assert!(wire_v1_ttl_cap_ms(cap).is_some(), "{name}: cap");
                    for field in ["cap_ms", "relay_now_ms", "input_ms"] {
                        assert!(
                            fixture.get(field).is_some_and(wire_v1_is_safe_u64),
                            "{name}: {field}"
                        );
                    }
                    let input_ms = fixture
                        .get("input_ms")
                        .and_then(Value::as_u64)
                        .expect("input_ms checked");
                    assert!(
                        expect.get("stored_ms").is_some_and(wire_v1_is_safe_u64),
                        "{name}: stored_ms"
                    );
                    let stored_ms = expect
                        .get("stored_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or_else(|| panic!("{name}: stored_ms must be u64"));
                    if stored_ms == input_ms {
                        direction.0 += 1;
                    } else {
                        direction.1 += 1;
                    }
                }
                _ => unreachable!("layer whitelist checked"),
            }
        }

        assert_eq!(aad_kat_count, 4, "aad-kat: exactly four cases required");
        assert_eq!(
            aad_kat_kinds,
            std::collections::HashSet::from([
                "pair-ready",
                "pair-accept-tokens",
                "token.refresh",
                "token.refresh.ok"
            ]),
            "aad-kat: §9.5 kind coverage"
        );

        for (layer, (passing, rejecting)) in directions {
            assert!(
                passing > 0,
                "{layer}: at least one passing/unchanged case required"
            );
            assert!(
                rejecting > 0,
                "{layer}: at least one rejecting/clamped case required"
            );
        }
    }

    #[test]
    fn shared_wire_v1_token_frame_expectations_recomputed_from_spec() {
        let fixtures = wire_v1_fixtures();
        for fixture in fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array")
            .iter()
            .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("token-frame"))
        {
            let name = fixture["name"]
                .as_str()
                .expect("fixture name must be a string");
            let error = wire_v1_token_frame_error(&fixture["frame"]);
            let expected_valid = fixture["expect"]["valid"]
                .as_bool()
                .expect("token-frame expect.valid must be a bool");
            assert_eq!(error.is_none(), expected_valid, "{name}");
            if let Some(error) = error {
                assert!(
                    fixture["expect"]["errors"]
                        .as_array()
                        .is_some_and(|errors| errors
                            .iter()
                            .any(|value| value.as_str() == Some(error))),
                    "{name}: missing {error}"
                );
            }
        }
    }

    #[test]
    fn shared_wire_v1_subprotocol_cases_match_spec() {
        let fixtures = wire_v1_fixtures();
        let fixtures = fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array");

        for fixture in fixtures
            .iter()
            .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("subprotocol"))
        {
            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("fixture name must be a string");
            let offers = fixture
                .get("offers")
                .and_then(Value::as_array)
                .expect("subprotocol offers must be an array")
                .iter()
                .map(|offer| {
                    offer
                        .as_str()
                        .unwrap_or_else(|| panic!("{name}: offer must be a string"))
                })
                .collect::<Vec<_>>();
            let actual = wire_v1_subprotocol_decision(&offers);
            let expect = fixture
                .get("expect")
                .and_then(Value::as_object)
                .expect("subprotocol expect must be an object");
            let accept = expect
                .get("accept")
                .and_then(Value::as_bool)
                .expect("subprotocol expect.accept must be a bool");
            assert_eq!(actual.is_some(), accept, "{name}");
            if let Some(token_hex) = actual {
                assert_eq!(
                    expect.get("echo").and_then(Value::as_str),
                    Some("agentloom-rc-v1"),
                    "{name}"
                );
                assert_eq!(
                    expect.get("token_hex").and_then(Value::as_str),
                    Some(token_hex),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn shared_wire_v1_desktop_upgrade_cases_recompute_bearer_ascii_hex() {
        let fixtures = wire_v1_fixtures();
        for fixture in fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array")
            .iter()
            .filter(|fixture| {
                fixture.get("layer").and_then(Value::as_str) == Some("desktop-upgrade")
            })
        {
            let name = fixture["name"]
                .as_str()
                .expect("fixture name must be a string");
            assert_eq!(
                wire_v1_desktop_upgrade_decision(fixture),
                fixture["expect"],
                "{name}"
            );
        }
    }

    #[test]
    fn shared_wire_v1_inbound_matrix_matches_hard_coded_fail_closed_table() {
        let fixtures = wire_v1_fixtures();
        for fixture in fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array")
            .iter()
            .filter(|fixture| {
                fixture.get("layer").and_then(Value::as_str) == Some("inbound-matrix")
            })
        {
            let name = fixture["name"]
                .as_str()
                .expect("fixture name must be a string");
            let scope = fixture["scope"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: scope must be a string"));
            let frame_type = fixture["frame_t"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: frame_t must be a string"));
            assert_eq!(
                wire_v1_inbound_matrix_decision(scope, frame_type),
                fixture["expect"],
                "{name}"
            );
        }
    }

    #[test]
    fn shared_wire_v1_time_window_decisions_match_spec() {
        let fixtures = wire_v1_fixtures();
        let fixtures = fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array");

        for fixture in fixtures
            .iter()
            .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("time-window"))
        {
            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("fixture name must be a string");
            let now_ms = fixture
                .get("now_ms")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{name}: now_ms must be u64"));
            let row = fixture
                .get("row")
                .unwrap_or_else(|| panic!("{name}: row must exist"));
            let expected = fixture
                .get("expect")
                .and_then(|expect| expect.get("decision"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: expect.decision must be a string"));
            assert_eq!(
                wire_v1_time_window_decision(now_ms, row),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn shared_wire_v1_ttl_clamp_cases_match_spec() {
        let fixtures = wire_v1_fixtures();
        let fixtures = fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array");

        for fixture in fixtures
            .iter()
            .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("ttl-clamp"))
        {
            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("fixture name must be a string");
            let cap = fixture
                .get("cap")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: cap must be a string"));
            let cap_ms = fixture
                .get("cap_ms")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{name}: cap_ms must be u64"));
            assert_eq!(wire_v1_ttl_cap_ms(cap), Some(cap_ms), "{name}: cap table");
            let relay_now_ms = fixture
                .get("relay_now_ms")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{name}: relay_now_ms must be u64"));
            let input_ms = fixture
                .get("input_ms")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{name}: input_ms must be u64"));
            let stored_ms = fixture
                .get("expect")
                .and_then(|expect| expect.get("stored_ms"))
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("{name}: expect.stored_ms must be u64"));
            let expected = input_ms.min(relay_now_ms + cap_ms + 120_000);
            assert_eq!(stored_ms, expected, "{name}");
        }
    }

    #[test]
    fn shared_wire_v1_chain_recomputes_pairing_and_device_paths() {
        let fixtures = wire_v1_fixtures();
        for fixture in fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array")
            .iter()
            .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("chain"))
        {
            let name = fixture["name"]
                .as_str()
                .expect("fixture name must be a string");
            let token_hex = fixture
                .get("connect_token_hex")
                .or_else(|| fixture.get("capability_token_hex"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: access token must be a string"));
            let token_hash_hex = fixture["token_hash_hex"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: token_hash_hex must be a string"));
            assert_eq!(
                wire_v1_sha256_ascii_hex(token_hex),
                token_hash_hex,
                "{name}: sha256(access token ASCII)"
            );

            let offers = fixture["subprotocol_offer"]
                .as_array()
                .unwrap_or_else(|| panic!("{name}: subprotocol_offer must be an array"))
                .iter()
                .map(|offer| {
                    offer
                        .as_str()
                        .unwrap_or_else(|| panic!("{name}: offer must be a string"))
                })
                .collect::<Vec<_>>();
            assert_eq!(
                wire_v1_subprotocol_decision(&offers),
                Some(token_hex),
                "{name}: subprotocol offer"
            );

            let put_frame = &fixture["put_frame"];
            assert_eq!(
                wire_v1_token_frame_error(put_frame),
                None,
                "{name}: put frame"
            );
            assert_eq!(
                put_frame["current"]["token_hash"].as_str(),
                Some(token_hash_hex),
                "{name}: put token_hash"
            );
            assert_eq!(
                put_frame["scope"].as_str(),
                fixture["expect"]["scope"].as_str(),
                "{name}: put scope"
            );

            let window = &fixture["window"];
            assert_eq!(
                put_frame["current"]["access_expires"], window["access_expires"],
                "{name}: put/window access_expires"
            );
            let valid_until = if put_frame["scope"].as_str() == Some("remote") {
                &put_frame["current"]["refresh_until"]
            } else {
                &put_frame["current"]["access_expires"]
            };
            assert_eq!(
                valid_until, &window["valid_until"],
                "{name}: put/window valid_until"
            );
            let window_kind = window["kind"].as_str().unwrap_or("current");
            let window_scope = window["scope"]
                .as_str()
                .or_else(|| put_frame["scope"].as_str())
                .unwrap_or_else(|| panic!("{name}: window scope must be a string"));
            let subject_state = window["subject_state"].as_str().unwrap_or("active");
            let row = serde_json::json!({
                "kind": window_kind,
                "scope": window_scope,
                "subject_state": subject_state,
                "access_expires": window["access_expires"],
                "valid_until": window["valid_until"],
            });
            let now_ms = window["now_ms"]
                .as_u64()
                .unwrap_or_else(|| panic!("{name}: window.now_ms must be u64"));
            let expected_scope = format!(
                "scope:{}",
                fixture["expect"]["scope"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}: expect.scope must be a string"))
            );
            assert_eq!(
                wire_v1_time_window_decision(now_ms, &row),
                expected_scope,
                "{name}: access window"
            );
        }
    }

    #[test]
    fn shared_wire_v1_aad_kat_fixtures_build_and_decrypt() {
        let fixtures = wire_v1_fixtures();
        let fixtures = fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array");
        let mut aad_kat_count = 0_u32;

        for fixture in fixtures
            .iter()
            .filter(|fixture| fixture.get("layer").and_then(Value::as_str) == Some("aad-kat"))
        {
            aad_kat_count += 1;
            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("AAD KAT fixture name must be a string");
            let meta = fixture
                .get("meta")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("{name}: meta must be an object"));
            let session = match meta.get("session") {
                Some(Value::String(value)) => Some(value.clone()),
                Some(Value::Null) => None,
                _ => panic!("{name}: meta.session must be string|null"),
            };
            let command_id = match meta.get("command_id") {
                Some(Value::String(value)) => Some(value.clone()),
                Some(Value::Null) => None,
                _ => panic!("{name}: meta.command_id must be string|null"),
            };
            let meta = EnvelopeMeta {
                v: meta
                    .get("v")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_else(|| panic!("{name}: meta.v must fit u32")),
                room: meta
                    .get("room")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: meta.room must be a string"))
                    .to_owned(),
                epoch: meta
                    .get("epoch")
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| panic!("{name}: meta.epoch must be u64")),
                kind: meta
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: meta.kind must be a string"))
                    .to_owned(),
                session,
                command_id,
            };
            let expected_aad = fixture
                .get("expect")
                .and_then(Value::as_object)
                .and_then(|expect| expect.get("aad"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: expect.aad must be a string"));
            assert_eq!(
                crate::remote_crypto::build_aad(&meta),
                expected_aad,
                "{name}: AAD"
            );

            let kat = fixture
                .get("kat")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("{name}: kat must be an object"));
            let key_hex = kat
                .get("key_hex")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: kat.key_hex must be a string"));
            let key = wire_v1_decode_hex_32(key_hex, name, "kat.key_hex");
            let ct_b64 = kat
                .get("ct_b64")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: kat.ct_b64 must be a string"));
            let n_b64 = kat
                .get("n_b64")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: kat.n_b64 must be a string"));
            let expected_plaintext = kat
                .get("plaintext")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: kat.plaintext must be a string"));
            let plaintext = crate::remote_crypto::open(&key, &meta, ct_b64, n_b64)
                .unwrap_or_else(|error| panic!("{name}: AAD KAT decryption failed: {error:?}"));
            assert_eq!(
                plaintext.as_slice(),
                expected_plaintext.as_bytes(),
                "{name}: plaintext"
            );
        }

        assert_eq!(
            aad_kat_count, 4,
            "exactly four AAD KAT fixtures are required"
        );
    }

    #[test]
    fn shared_wire_v1_kat_fixtures_decrypt_and_authenticate() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../../remote-relay/fixtures/wire-v1.json"))
                .expect("wire-v1 fixture must be valid JSON");
        let fixtures = fixtures
            .as_array()
            .expect("wire-v1 fixture root must be an array");
        let mut kat_count = 0;
        let mut mutations_checked = false;

        for fixture in fixtures {
            if fixture.get("layer").and_then(Value::as_str) == Some("aad-kat") {
                continue;
            }
            let Some(kat) = fixture.get("kat").and_then(Value::as_object) else {
                continue;
            };
            kat_count += 1;

            let name = fixture
                .get("name")
                .and_then(Value::as_str)
                .expect("KAT fixture name must be a string");
            let key_hex = kat
                .get("k_room_hex")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: kat.k_room_hex must be a string"));
            assert_eq!(
                key_hex.len(),
                64,
                "{name}: kat.k_room_hex must encode exactly 32 bytes"
            );
            assert!(
                key_hex.is_ascii(),
                "{name}: kat.k_room_hex must contain only ASCII hex digits"
            );
            let mut key = [0_u8; 32];
            for (index, pair) in key_hex.as_bytes().chunks_exact(2).enumerate() {
                let pair = std::str::from_utf8(pair)
                    .unwrap_or_else(|_| panic!("{name}: kat.k_room_hex must be valid ASCII"));
                key[index] = u8::from_str_radix(pair, 16).unwrap_or_else(|_| {
                    panic!("{name}: kat.k_room_hex contains invalid hex at byte {index}")
                });
            }

            let envelope = fixture
                .get("envelope")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("{name}: envelope must be an object"));
            let session = match envelope.get("session") {
                Some(Value::String(value)) => Some(value.clone()),
                Some(Value::Null) => None,
                _ => panic!("{name}: session must be string|null"),
            };
            let command_id = match envelope.get("command_id") {
                Some(Value::String(value)) => Some(value.clone()),
                Some(Value::Null) => None,
                _ => panic!("{name}: command_id must be string|null"),
            };
            let meta = EnvelopeMeta {
                v: envelope
                    .get("v")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_else(|| panic!("{name}: envelope.v must fit u32")),
                room: envelope
                    .get("room")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: envelope.room must be a string"))
                    .to_owned(),
                epoch: envelope
                    .get("epoch")
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| panic!("{name}: envelope.epoch must be u64")),
                kind: envelope
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: envelope.kind must be a string"))
                    .to_owned(),
                session,
                command_id,
            };
            let expected_aad = fixture
                .get("expect")
                .and_then(Value::as_object)
                .and_then(|expect| expect.get("aad"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: expect.aad must be a string"));
            assert_eq!(
                crate::remote_crypto::build_aad(&meta).as_bytes(),
                expected_aad.as_bytes(),
                "{name}: AAD mismatch"
            );

            let ct_b64 = envelope
                .get("ct")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: envelope.ct must be a base64 string"));
            let n_b64 = envelope
                .get("n")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: envelope.n must be a base64 string"));
            let expected_plaintext = kat
                .get("plaintext")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name}: kat.plaintext must be a string"));
            let plaintext = crate::remote_crypto::open(&key, &meta, ct_b64, n_b64)
                .unwrap_or_else(|error| panic!("{name}: KAT decryption failed: {error:?}"));
            assert_eq!(
                plaintext.as_slice(),
                expected_plaintext.as_bytes(),
                "{name}: plaintext mismatch"
            );

            if !mutations_checked {
                let mut tampered_ct = STANDARD
                    .decode(ct_b64)
                    .unwrap_or_else(|error| panic!("{name}: invalid ciphertext base64: {error}"));
                let first_byte = tampered_ct
                    .first_mut()
                    .unwrap_or_else(|| panic!("{name}: ciphertext must not be empty"));
                *first_byte ^= 1;
                let tampered_ct_b64 = STANDARD.encode(tampered_ct);

                // These mutations prove the test exercises AEAD authentication, not just wire shape.
                assert_eq!(
                    crate::remote_crypto::open(&key, &meta, &tampered_ct_b64, n_b64),
                    Err(crate::remote_crypto::CryptoError::DecryptFailed),
                    "{name}: tampered ciphertext must fail authentication"
                );

                let tampered_meta = EnvelopeMeta {
                    v: meta.v,
                    room: meta.room.clone(),
                    epoch: meta
                        .epoch
                        .checked_add(1)
                        .unwrap_or_else(|| panic!("{name}: envelope.epoch cannot be incremented")),
                    kind: meta.kind.clone(),
                    session: meta.session.clone(),
                    command_id: meta.command_id.clone(),
                };
                assert_eq!(
                    crate::remote_crypto::open(&key, &tampered_meta, ct_b64, n_b64),
                    Err(crate::remote_crypto::CryptoError::DecryptFailed),
                    "{name}: tampered AAD must fail authentication"
                );
                mutations_checked = true;
            }
        }

        assert!(kat_count >= 1, "at least one KAT fixture must exist");
    }

    #[test]
    fn encrypted_input_answer_routes_handler_outcomes_without_counting_them_bad() {
        let k_room = Zeroizing::new([12_u8; 32]);
        for (outcome, expected) in [
            (AckOutcome::Queued, "queued"),
            (AckOutcome::Ok, "ok"),
            (AckOutcome::Failed, "failed"),
        ] {
            let received = Arc::new(Mutex::new(None));
            let received_for_handler = Arc::clone(&received);
            let inner = test_inner_with_input_answer_handler(move |frame| {
                *received_for_handler.lock().unwrap() = Some((
                    frame.session,
                    frame.command_id,
                    frame.decision_id,
                    frame.option,
                ));
                Some(outcome)
            });
            let answer = seal_command_envelope(
                &k_room,
                "0123456789abcdef0123456789abcdef",
                7,
                "input",
                "s-5",
                "cmd-answer",
                &serde_json::json!({
                    "t": "input.answer",
                    "session": "s-5",
                    "decision_id": "d-1",
                    "option": "yes",
                }),
            );

            let response = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
            assert_eq!(response["command_id"], "cmd-answer");
            assert_eq!(response["outcome"], expected);
            assert_eq!(
                received.lock().unwrap().as_ref(),
                Some(&(
                    "s-5".to_owned(),
                    "cmd-answer".to_owned(),
                    "d-1".to_owned(),
                    "yes".to_owned(),
                ))
            );
            assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn encrypted_input_answer_ack_loss_retry_uses_terminal_ledger_without_reprocessing() {
        let k_room = Zeroizing::new([28_u8; 32]);
        let conn = Arc::new(Mutex::new(crate::test_support::mem_db()));
        let processing_calls = Arc::new(AtomicU64::new(0));
        let conn_for_handler = Arc::clone(&conn);
        let processing_calls_for_handler = Arc::clone(&processing_calls);
        let inner = test_inner_with_input_answer_handler(move |frame| {
            let payload = serde_json::json!({
                "decision_id": frame.decision_id,
                "option": frame.option,
            })
            .to_string();
            crate::remote_input_send_ack(
                || {
                    crate::db::enqueue_remote_input(
                        &lock(&conn_for_handler),
                        &frame.session,
                        &frame.command_id,
                        "input.answer",
                        &payload,
                    )
                    .map_err(|e| e.to_string())
                },
                || {
                    processing_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                    crate::db::mark_remote_input_delivered_by_command_id(
                        &lock(&conn_for_handler),
                        &frame.command_id,
                    )
                    .unwrap();
                },
                || {
                    crate::db::remote_inbox_terminal_state_by_command_id(
                        &lock(&conn_for_handler),
                        &frame.command_id,
                    )
                    .map_err(|e| e.to_string())
                },
            )
        });
        let answer = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            7,
            "input",
            "s-answer-ledger",
            "cmd-answer-ledger",
            &serde_json::json!({
                "t": "input.answer",
                "session": "s-answer-ledger",
                "decision_id": "d-ledger",
                "option": "yes",
            }),
        );

        let first = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
        assert_eq!(first["outcome"], "queued");
        assert_eq!(processing_calls.load(Ordering::Relaxed), 1);
        let delivered_at: Option<i64> = lock(&conn)
            .query_row(
                "SELECT delivered_at FROM remote_inbox WHERE command_id = 'cmd-answer-ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(delivered_at.is_some());

        let retry = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
        assert_eq!(retry["outcome"], "ok");
        assert_eq!(processing_calls.load(Ordering::Relaxed), 1);
        let count: i64 = lock(&conn)
            .query_row(
                "SELECT COUNT(*) FROM remote_inbox WHERE command_id = 'cmd-answer-ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn encrypted_input_answer_missing_fields_return_failed_and_count_bad_frames() {
        let k_room = Zeroizing::new([29_u8; 32]);
        for payload in [
            serde_json::json!({
                "t": "input.answer",
                "session": "s-bad-answer",
                "option": "yes",
            }),
            serde_json::json!({
                "t": "input.answer",
                "session": "s-bad-answer",
                "decision_id": "d-bad-answer",
            }),
            serde_json::json!({
                "t": "input.answer",
                "session": 7,
                "decision_id": "d-bad-answer",
                "option": "yes",
            }),
        ] {
            let inner = test_inner_with_input_answer_handler(|_| {
                panic!("字段校验失败时不得调用 answer handler")
            });
            let answer = seal_command_envelope(
                &k_room,
                "0123456789abcdef0123456789abcdef",
                7,
                "input",
                "s-bad-answer",
                "cmd-bad-answer",
                &payload,
            );

            let response = handle_frame(&inner, &answer.to_string(), Some(&k_room)).unwrap();
            assert_eq!(response["outcome"], "failed");
            assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 1);
        }
    }

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

        let first =
            handle_frame(&inner, &request("cmd-repeat").to_string(), Some(&k_room)).unwrap();
        assert_eq!(first["outcome"], "ok");
        let second =
            handle_frame(&inner, &request("cmd-repeat").to_string(), Some(&k_room)).unwrap();
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

    #[test]
    fn routes_pair_frames_and_builds_provisional_accept_wire_shape() {
        let hello_calls = Arc::new(AtomicU64::new(0));
        let done_calls = Arc::new(AtomicU64::new(0));
        let hello_calls_for_handler = Arc::clone(&hello_calls);
        let done_calls_for_handler = Arc::clone(&done_calls);
        let inner = test_inner_with_pair_handlers(
            move |frame| {
                hello_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                assert_eq!(frame.room, "room-1");
                assert_eq!(frame.remote_pub, [7_u8; 32]);
                assert_eq!(frame.token_ct, "token-ciphertext");
                assert_eq!(frame.token_n, "token-nonce");
                Some(PairAcceptFrame {
                    room: frame.room,
                    device_id: "device-1".to_owned(),
                    k_room_ct: "room-ciphertext".to_owned(),
                    k_room_n: "room-nonce".to_owned(),
                    tokens_ct: "tokens-ciphertext".to_owned(),
                    tokens_n: "tokens-nonce".to_owned(),
                    k_room: Zeroizing::new([3_u8; 32]),
                })
            },
            move |frame| {
                done_calls_for_handler.fetch_add(1, Ordering::Relaxed);
                if frame.room == "room-1" && frame.device_id == "device-1" {
                    PairDoneAction::Accepted {
                        newly_paired_device_id: None,
                    }
                } else {
                    PairDoneAction::Rejected
                }
            },
        );
        let remote_pub = STANDARD.encode([7_u8; 32]);
        let response = handle_frame(
            &inner,
            &serde_json::json!({
                "t": "pair.hello",
                "room": "room-1",
                "remote_pub": remote_pub,
                "token_ct": "token-ciphertext",
                "token_n": "token-nonce",
                "origin_connection_id": "conn-pairing-1",
            })
            .to_string(),
            None,
        )
        .expect("accepted hello should produce pair.accept");
        assert_eq!(
            response,
            serde_json::json!({
                "t": "pair.accept",
                "room": "room-1",
                "device_id": "device-1",
                "k_room_ct": "room-ciphertext",
                "k_room_n": "room-nonce",
                "tokens_ct": "tokens-ciphertext",
                "tokens_n": "tokens-nonce",
            })
        );

        assert!(handle_frame(
            &inner,
            r#"{"t":"pair.done","room":"room-1","device_id":"device-1","origin_connection_id":"conn-pairing-1"}"#,
            None,
        )
        .is_none());
        assert_eq!(hello_calls.load(Ordering::Relaxed), 1);
        assert_eq!(done_calls.load(Ordering::Relaxed), 1);
        assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn pair_accept_serialization_never_exposes_plaintext_tokens() {
        let capability_token = "a1".repeat(32);
        let refresh_token = "b2".repeat(32);
        let room = "0123456789abcdef0123456789abcdef";
        let device_id = "11111111-1111-4111-8111-111111111111";
        let (tokens_ct, tokens_n) = crate::remote_pairing::seal_pair_accept_tokens(
            &[0x42_u8; 32],
            room,
            device_id,
            &capability_token,
            &refresh_token,
        );
        let serialized = pair_accept_json(PairAcceptFrame {
            room: room.to_owned(),
            device_id: device_id.to_owned(),
            k_room_ct: "room-ciphertext".to_owned(),
            k_room_n: "room-nonce".to_owned(),
            tokens_ct,
            tokens_n,
            k_room: Zeroizing::new([3_u8; 32]),
        })
        .to_string();

        assert!(!serialized.contains(&capability_token));
        assert!(!serialized.contains(&refresh_token));
        assert!(!serialized.contains("capability_token"));
        assert!(!serialized.contains("refresh_token"));
    }

    #[test]
    fn wild_or_malformed_pair_frames_are_ignored_and_counted() {
        let inner = test_inner(|_| None, || None);
        let remote_pub = STANDARD.encode([7_u8; 32]);

        assert!(handle_frame(
            &inner,
            &serde_json::json!({
                "t": "pair.hello",
                "room": "room-1",
                "remote_pub": remote_pub,
                "token_ct": "ct",
                "token_n": "n",
            })
            .to_string(),
            None,
        )
        .is_none());
        assert!(handle_frame(
            &inner,
            r#"{"t":"pair.done","room":"room-1","device_id":"device-1"}"#,
            None,
        )
        .is_none());
        assert!(handle_frame(&inner, r#"{"t":"pair.hello","room":"room-1"}"#, None,).is_none());
        assert_eq!(inner.state.bad_frames.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn validates_exactly_32_hexadecimal_room_id_characters() {
        assert!(is_valid_room_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_valid_room_id("0123456789abcdef0123456789ABCDEF"));
        assert!(!is_valid_room_id("0123456789abcdef0123456789abcde"));
        assert!(!is_valid_room_id("0123456789abcdef0123456789abcdef0"));
        assert!(!is_valid_room_id("0123456789abcdef0123456789abcdeg"));
        assert!(!is_valid_room_id("0123456789abcdef0123456789ABCDE "));
    }

    #[test]
    fn effective_relay_url_falls_back_to_default_when_none() {
        assert_eq!(
            effective_relay_url(None),
            Some(DEFAULT_PUBLIC_RELAY_URL.to_owned())
        );
    }

    #[test]
    fn effective_relay_url_falls_back_to_default_when_blank() {
        assert_eq!(
            effective_relay_url(Some("   ".to_owned())),
            Some(DEFAULT_PUBLIC_RELAY_URL.to_owned())
        );
        assert_eq!(
            effective_relay_url(Some(String::new())),
            Some(DEFAULT_PUBLIC_RELAY_URL.to_owned())
        );
    }

    #[test]
    fn effective_relay_url_passes_through_non_empty_value_unchanged() {
        assert_eq!(
            effective_relay_url(Some("wss://relay.example.com".to_owned())),
            Some("wss://relay.example.com".to_owned())
        );
        // 非空值不做额外 trim——只负责"空则兜底"，不越权改写用户已填的值。
        assert_eq!(
            effective_relay_url(Some("  wss://relay.example.com  ".to_owned())),
            Some("  wss://relay.example.com  ".to_owned())
        );
    }

    #[test]
    fn classifies_supported_live_variants() {
        use crate::agent_event::AgentEvent;

        let cases = [
            (
                AgentEvent::TextDelta {
                    text: "hello".to_owned(),
                },
                11,
                (
                    "live",
                    serde_json::json!({"t": "text_delta", "seq": 11, "text": "hello"}),
                ),
            ),
            (
                AgentEvent::ThinkingDelta {
                    text: "hmm".to_owned(),
                },
                12,
                (
                    "live",
                    serde_json::json!({"t": "thinking_delta", "seq": 12, "text": "hmm"}),
                ),
            ),
            (
                AgentEvent::ToolOutputDelta {
                    id: "tool-1".to_owned(),
                    text: "chunk".to_owned(),
                },
                13,
                (
                    "live",
                    serde_json::json!({
                        "t": "tool_output_delta",
                        "seq": 13,
                        "id": "tool-1",
                        "text": "chunk",
                    }),
                ),
            ),
            (
                AgentEvent::UsageDelta {
                    input_tokens: Some(21),
                    output_tokens: None,
                },
                14,
                (
                    "live",
                    serde_json::json!({
                        "t": "usage_delta",
                        "seq": 14,
                        "input_tokens": 21,
                        "output_tokens": null,
                    }),
                ),
            ),
        ];

        for (event, seq, expected) in cases {
            assert_eq!(classify(&event, seq), Some(expected));
        }
    }

    #[test]
    fn classify_truncates_oversized_live_text_to_output_cap() {
        use crate::agent_event::AgentEvent;

        let oversized = "x".repeat(OUTPUT_TRUNCATE_BYTES + 17);
        let events = [
            AgentEvent::TextDelta {
                text: oversized.clone(),
            },
            AgentEvent::ThinkingDelta {
                text: oversized.clone(),
            },
            AgentEvent::ToolOutputDelta {
                id: "tool-1".to_owned(),
                text: oversized,
            },
        ];

        for event in events {
            let (_, value) = classify(&event, 1).expect("live delta should be classified");
            assert_eq!(
                value["text"]
                    .as_str()
                    .expect("classified live delta should have text")
                    .len(),
                OUTPUT_TRUNCATE_BYTES
            );
        }
    }

    #[test]
    fn truncate_utf8_respects_byte_limit_and_character_boundary() {
        let oversized = "x".repeat(OUTPUT_TRUNCATE_BYTES + 17);
        assert_eq!(
            truncate_utf8(&oversized, OUTPUT_TRUNCATE_BYTES).len(),
            OUTPUT_TRUNCATE_BYTES
        );

        let boundary = format!("{}界", "a".repeat(OUTPUT_TRUNCATE_BYTES - 1));
        let truncated = truncate_utf8(&boundary, OUTPUT_TRUNCATE_BYTES);
        assert_eq!(truncated.len(), OUTPUT_TRUNCATE_BYTES - 1);
        assert_eq!(truncated, "a".repeat(OUTPUT_TRUNCATE_BYTES - 1));
    }

    #[test]
    fn skips_agent_event_variants_outside_the_upstream_catalog() {
        use crate::agent_event::{AgentEvent, ToolStatus};

        let events = [
            AgentEvent::SessionStarted {
                conversation_id: "conversation-1".to_owned(),
            },
            AgentEvent::Error {
                message: "boom".to_owned(),
            },
            AgentEvent::GoalDeclared {
                goal: "ship it".to_owned(),
                status: "frozen".to_owned(),
                lead: None,
                criteria: Vec::new(),
            },
            AgentEvent::ToolCompleted {
                id: "tool-1".to_owned(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: Some("done".to_owned()),
            },
        ];

        for event in events {
            assert_eq!(classify(&event, 1), None);
        }
    }

    #[test]
    fn builds_envelope_json_with_explicit_null_fields() {
        let meta = EnvelopeMeta {
            v: 1,
            room: "0123456789abcdef0123456789abcdef".to_owned(),
            epoch: 7,
            kind: "live".to_owned(),
            session: Some("sess-1".to_owned()),
            command_id: None,
        };
        let envelope = build_envelope_json(&meta, "fixed-ct", "fixed-n", 123_456, None);

        assert_eq!(envelope["v"], 1);
        assert_eq!(envelope["room"], meta.room);
        assert_eq!(envelope["epoch"], 7);
        assert_eq!(envelope["kind"], "live");
        assert_eq!(envelope["session"], "sess-1");
        assert_eq!(envelope["command_id"], serde_json::Value::Null);
        assert_eq!(envelope["seq"], serde_json::Value::Null);
        assert_eq!(envelope["ct"], "fixed-ct");
        assert_eq!(envelope["n"], "fixed-n");
        assert_eq!(envelope["ts"], 123_456);
    }

    #[test]
    fn sealed_envelope_uses_lowercase_room_and_standard_twelve_byte_nonce() {
        let meta = EnvelopeMeta {
            v: 1,
            room: "0123456789abcdef0123456789abcdef".to_owned(),
            epoch: 7,
            kind: "event".to_owned(),
            session: Some("sess-1".to_owned()),
            command_id: None,
        };
        let (ct, nonce) = crate::remote_crypto::seal(&[9_u8; 32], &meta, br#"{"t":"probe"}"#);
        let envelope = build_envelope_json(&meta, &ct, &nonce, 42, Some("client-1"));

        assert_eq!(envelope["room"], meta.room.to_lowercase());
        assert!(!envelope["room"]
            .as_str()
            .unwrap()
            .chars()
            .any(|c| c.is_ascii_uppercase()));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(envelope["n"].as_str().unwrap())
                .unwrap()
                .len(),
            12
        );
        assert!(base64::engine::general_purpose::STANDARD
            .decode(&ct)
            .is_ok());
    }

    #[test]
    fn client_msg_id_derivation_matches_every_shared_uuid_v5_vector() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../remote-relay/fixtures/client-msg-id-derivation-v1.json"
        ))
        .expect("client-msg-id fixture must be valid JSON");
        let vectors = fixture["vectors"]
            .as_array()
            .expect("client-msg-id fixture vectors must be an array");

        assert_eq!(vectors.len(), 4);
        for vector in vectors {
            let name = vector["name"]
                .as_str()
                .expect("vector name must be a string");
            let expected = vector["expect"]
                .as_str()
                .expect("vector expectation must be a string");
            assert_eq!(
                derive_client_msg_id(name),
                expected,
                "KAT failed for {name}"
            );
        }
    }

    #[test]
    fn random_client_msg_id_is_valid_uuid_v4_text() {
        let client_msg_id = try_random_client_msg_id().expect("OS entropy should be available");

        assert_eq!(client_msg_id.len(), 36);
        assert!(is_valid_client_msg_id(&client_msg_id));
    }

    #[test]
    fn random_client_msg_id_returns_none_when_entropy_fails() {
        let _guard = ForceClientMsgIdEntropyFailureGuard::new();

        assert_eq!(try_random_client_msg_id(), None);
    }

    #[test]
    fn session_index_snapshot_runs_on_named_background_thread_and_is_delivered() {
        let provider_thread_name = Arc::new(Mutex::new(None));
        let recorded_thread_name = Arc::clone(&provider_thread_name);
        let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            move || {
                *recorded_thread_name.lock().unwrap() = thread::current().name().map(str::to_owned);
                Some(serde_json::json!([]))
            },
        );
        let connection_generation = inner.state.advance_generation_and_set_gate(true);

        request_session_index_snapshot(&inner, connection_generation);

        let (item_generation, item) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("session.index snapshot should be delivered");
        assert_eq!(item_generation, connection_generation);
        assert_eq!(item.t, "session.index");
        assert_eq!(
            provider_thread_name.lock().unwrap().as_deref(),
            Some("remote-index-snapshot")
        );
    }

    #[test]
    fn milestone_replay_uses_shared_msg_completed_client_msg_id_derivation() {
        let c = crate::test_support::mem_db();
        crate::db::create_session(&c, "replay-derive", "Replay", "local-default", "local").unwrap();
        crate::db::append_message_dedup(
            &c,
            "replay-derive",
            "assistant",
            &[crate::db::Block::Text {
                text: "complete".into(),
            }],
            None,
            None,
            None,
            "run_flush:derive",
        )
        .unwrap();
        let rows = crate::db::list_recent_milestone_replay_rows(&c, 10).unwrap();
        assert_eq!(rows.len(), 1);
        let expected = derive_msg_completed_client_msg_id("replay-derive", "run_flush:derive");
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(rows.clone()),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        request_session_index_snapshot(&inner, generation);

        let (_, snapshot) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("session.index should precede replay");
        assert_eq!(snapshot.t, "session.index");
        let (item_generation, replay) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("msg.completed replay should be delivered");
        assert_eq!(item_generation, generation);
        assert_eq!(replay.t, "msg.completed");
        assert_eq!(replay.client_msg_id.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn replay_batch_oversized_message_and_live_publish_are_dropped_and_counted() {
        let oversized_blocks = serde_json::json!([{
            "type": "text",
            "text": "x".repeat(SNAPSHOT_SEND_BUDGET_BYTES + 1024),
        }]);
        let replay_row = crate::db::MilestoneReplayRow {
            session_id: "replay-oversized".into(),
            message_id: 41,
            role: "assistant".into(),
            content_json: oversized_blocks.clone(),
            dedup_key: "replay-oversized-dedup".into(),
        };
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(vec![replay_row.clone()]),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        publish_milestone_replay_batch_on_connect(&inner, generation);
        assert!(milestone_rx.try_recv().is_err());
        assert_eq!(
            inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
            1
        );

        enqueue_milestone_for_upstream(
            &inner.state,
            &inner.milestone_tx,
            MilestoneItem {
                session: Some("live-oversized".into()),
                t: "msg.completed".into(),
                payload: build_msg_completed_payload(42, "assistant", oversized_blocks, None),
                client_msg_id: derive_msg_completed_client_msg_id(
                    "live-oversized",
                    "live-oversized-dedup",
                ),
            },
        );
        assert!(milestone_rx.try_recv().is_err());
        assert_eq!(
            inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
            2
        );
    }

    #[test]
    fn replay_batch_normal_message_is_enqueued_unchanged() {
        let blocks = serde_json::json!([{"type": "text", "text": "normal replay"}]);
        let replay_row = crate::db::MilestoneReplayRow {
            session_id: "replay-normal".into(),
            message_id: 43,
            role: "assistant".into(),
            content_json: blocks.clone(),
            dedup_key: "replay-normal-dedup".into(),
        };
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(vec![replay_row.clone()]),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        publish_milestone_replay_batch_on_connect(&inner, generation);

        let (item_generation, item) = milestone_rx.try_recv().unwrap();
        assert_eq!(item_generation, generation);
        assert_eq!(item.t, "msg.completed");
        assert_eq!(item.payload["blocks"], blocks);
        assert!(milestone_rx.try_recv().is_err());
        assert_eq!(
            inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn replay_batch_tool_output_truncation_makes_message_sendable() {
        let replay_row = crate::db::MilestoneReplayRow {
            session_id: "replay-truncated-tool".into(),
            message_id: 44,
            role: "assistant".into(),
            content_json: serde_json::json!([{
                "type": "tool",
                "id": "tool-1",
                "tool": "shell",
                "summary": "ran",
                "card": "command",
                "status": "ok",
                "exit_code": 0,
                "output": "y".repeat(SNAPSHOT_SEND_BUDGET_BYTES + 1024),
            }]),
            dedup_key: "replay-truncated-tool-dedup".into(),
        };
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(vec![replay_row.clone()]),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        publish_milestone_replay_batch_on_connect(&inner, generation);

        let (_, item) = milestone_rx.try_recv().unwrap();
        assert_eq!(item.t, "msg.completed");
        assert_eq!(
            item.payload["blocks"][0]["output"].as_str().unwrap().len(),
            OUTPUT_TRUNCATE_BYTES
        );
        assert!(milestone_frame_bytes(&item.t, &item.payload) <= SNAPSHOT_SEND_BUDGET_BYTES);
        assert_eq!(
            inner.state.replay_oversized_dropped.load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn milestone_replay_batch_includes_user_row_role_agnostic_publish() {
        // P0-c 真路径消费测试：`list_recent_milestone_replay_rows` 现在纳入带 dedup_key 的
        // user 行（db.rs 语义反转），这里验证消费方 `publish_milestone_replay_batch_on_connect`
        // 对 role 确实无感——user 行原样走到 msg.completed 补发帧，client_msg_id 推导与
        // assistant 行同一条公式（session_id + dedup_key），不因 role 分叉。
        let c = crate::test_support::mem_db();
        crate::db::create_session(&c, "replay-user", "Replay User", "local-default", "local")
            .unwrap();
        crate::db::append_message_dedup(
            &c,
            "replay-user",
            "user",
            &[crate::db::Block::Text {
                text: "你好".into(),
            }],
            None,
            None,
            None,
            "remote_input:cmd-replay-user",
        )
        .unwrap();
        let rows = crate::db::list_recent_milestone_replay_rows(&c, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].role, "user");
        let expected =
            derive_msg_completed_client_msg_id("replay-user", "remote_input:cmd-replay-user");
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(rows.clone()),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        request_session_index_snapshot(&inner, generation);

        let (_, snapshot) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("session.index should precede replay");
        assert_eq!(snapshot.t, "session.index");
        let (item_generation, replay) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("msg.completed replay should be delivered for a user row");
        assert_eq!(item_generation, generation);
        assert_eq!(replay.t, "msg.completed");
        assert_eq!(replay.payload["role"], "user");
        assert_eq!(replay.client_msg_id.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn connection_snapshot_is_followed_by_replay_rows_in_provider_order() {
        let replay_rows = vec![
            crate::db::MilestoneReplayRow {
                session_id: "s1".into(),
                message_id: 11,
                role: "assistant".into(),
                content_json: serde_json::json!([]),
                dedup_key: "d1".into(),
            },
            crate::db::MilestoneReplayRow {
                session_id: "s2".into(),
                message_id: 12,
                role: "assistant".into(),
                content_json: serde_json::json!([]),
                dedup_key: "d2".into(),
            },
        ];
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(replay_rows.clone()),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        request_session_index_snapshot(&inner, generation);

        let mut received = Vec::new();
        for _ in 0..3 {
            received.push(
                milestone_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("snapshot and ordered replay frames should be delivered"),
            );
        }
        assert!(received
            .iter()
            .all(|(item_generation, _)| *item_generation == generation));
        assert_eq!(received[0].1.t, "session.index");
        assert_eq!(received[1].1.t, "msg.completed");
        assert_eq!(received[1].1.payload["message_id"], 11);
        assert_eq!(received[2].1.t, "msg.completed");
        assert_eq!(received[2].1.payload["message_id"], 12);
    }

    #[test]
    fn milestone_replay_rebuilds_resolved_and_pending_decision_cards() {
        let chosen_row = crate::db::MilestoneReplayRow {
            session_id: "card-session".into(),
            message_id: 21,
            role: "assistant".into(),
            content_json: serde_json::json!([{
                "type": "decision_card",
                "decision_id": "dc-1",
                "status": "chosen",
                "chosen_option": "A",
                "unknown_future_field": { "preserved": true }
            }]),
            dedup_key: "card-chosen".into(),
        };
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(vec![chosen_row.clone()]),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);
        request_session_index_snapshot(&inner, generation);

        let (_, snapshot) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (_, completed) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (_, created) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (_, resolved) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(snapshot.t, "session.index");
        assert_eq!(completed.t, "msg.completed");
        assert_eq!(created.t, "card.created");
        assert_eq!(
            created.client_msg_id,
            derive_card_created_client_msg_id("dc-1")
        );
        assert_eq!(
            created.payload["block"]["unknown_future_field"]["preserved"],
            true
        );
        assert_eq!(resolved.t, "card.resolved");
        assert_eq!(
            resolved.client_msg_id,
            derive_card_resolved_client_msg_id("dc-1", "chosen")
        );
        assert_eq!(resolved.payload["chosen_option"], "A");

        let pending_row = crate::db::MilestoneReplayRow {
            session_id: "card-session".into(),
            message_id: 22,
            role: "assistant".into(),
            content_json: serde_json::json!([{
                "type": "decision_card",
                "decision_id": "dc-2",
                "status": "pending",
                "chosen_option": null
            }]),
            dedup_key: "card-pending".into(),
        };
        let (pending_inner, pending_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(vec![pending_row.clone()]),
            || None,
        );
        let pending_generation = pending_inner.state.advance_generation_and_set_gate(true);
        request_session_index_snapshot(&pending_inner, pending_generation);

        let (_, snapshot) = pending_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (_, completed) = pending_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (_, created) = pending_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(snapshot.t, "session.index");
        assert_eq!(completed.t, "msg.completed");
        assert_eq!(created.t, "card.created");
        assert_eq!(
            created.client_msg_id,
            derive_card_created_client_msg_id("dc-2")
        );
        assert!(
            pending_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "pending card must not emit card.resolved"
        );
    }

    /// idlefix-T1 缺口②：连接后补发批必须追加 `run.status` 现状帧——手机顶栏唯一数据源就是它，
    /// 此前只在状态变化时 publish 一次、连接后补发批没有它，中途接入/错过一帧顶栏就永久卡在
    /// Idle。这里断言补发批（session.index 之后）含一帧 `run.status`，值来自
    /// `session_runtime_replay_provider`，且 client_msg_id 走确定性推导（不是每次重连都变）。
    #[test]
    fn milestone_replay_batch_includes_run_status_current_state_frame() {
        let runtime_row = crate::db::SessionRuntimeReplayRow {
            session_id: "runstatus-session".into(),
            status: "running".into(),
            run_id: Some("run-77".into()),
        };
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            || None,
            move || Some(vec![runtime_row.clone()]),
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        request_session_index_snapshot(&inner, generation);

        let (_, snapshot) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("session.index should precede replay");
        assert_eq!(snapshot.t, "session.index");
        let (item_generation, run_status) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("run.status replay frame should be delivered");
        assert_eq!(item_generation, generation);
        assert_eq!(run_status.t, "run.status");
        assert_eq!(run_status.payload["session_id"], "runstatus-session");
        assert_eq!(run_status.payload["status"], "running");
        assert_eq!(run_status.payload["run_id"], "run-77");
        assert_eq!(
            run_status.client_msg_id,
            derive_run_status_replay_client_msg_id("runstatus-session", "running", Some("run-77"))
        );
    }

    /// 缺口② round-trip：`session_runtime_replay_provider` 读失败（返回 None）不该连累
    /// msg.completed/card.* 那半补发——两个 provider 各自 best-effort，互不拖累。
    #[test]
    fn milestone_replay_batch_msg_completed_survives_run_status_provider_failure() {
        let replay_row = crate::db::MilestoneReplayRow {
            session_id: "runstatus-fail-session".into(),
            message_id: 31,
            role: "assistant".into(),
            content_json: serde_json::json!([]),
            dedup_key: "d-runstatus-fail".into(),
        };
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            move || Some(vec![replay_row.clone()]),
            || None,
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        request_session_index_snapshot(&inner, generation);

        let (_, snapshot) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (_, completed) = milestone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(snapshot.t, "session.index");
        assert_eq!(completed.t, "msg.completed");
        assert!(
            milestone_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "run_status provider 返回 None 时不该有 run.status 帧，但也不该吞掉上面已发的 msg.completed"
        );
    }

    /// idlefix-T1 补针 C（skeptic 点名 TOCTOU）：`list_session_runtime_replay_rows` 读出的是
    /// "读那一刻"的现状——本用例里故意造出陈旧行（status=running），且让 provider 阻塞在"已被
    /// 调用、尚未返回"这个窗口里，模拟"读之后、入队之前，真实状态已经翻转"。这个窗口期间，一次
    /// "真实"翻转（走 `enqueue_run_status_milestone_with_gate`——`publish_run_status_milestone`
    /// 真正落地时调的同一份函数）并发尝试把新状态（idle）入队。断言：客户端最终收到的最后一帧
    /// 是新状态，陈旧的补发帧排不到它后面——不是靠时序侥幸，是靠 `run_status_replay_gate`
    /// 强制互斥（provider 未放行前，"实时"入队被挡在锁外）。
    #[test]
    fn run_status_replay_batch_is_ordered_before_a_racing_live_transition_toctou() {
        let stale_row = crate::db::SessionRuntimeReplayRow {
            session_id: "toctou-sess".into(),
            status: "running".into(),
            run_id: Some("run-old".into()),
        };
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let provider_release_rx = Arc::clone(&release_rx);
        let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
            || None,
            move || {
                // provider 被调用即代表 replay worker 已经拿到锁、正在"读 DB"——发信号让测试
                // 主线程确定性地知道这一刻，再阻塞直到测试放行，撑大"读后、入队前"的窗口。
                started_tx.send(()).unwrap();
                provider_release_rx.lock().unwrap().recv().unwrap();
                Some(vec![stale_row.clone()])
            },
        );
        let generation = inner.state.advance_generation_and_set_gate(true);

        let worker_inner = Arc::clone(&inner);
        let worker = thread::spawn(move || {
            publish_run_status_replay_rows(&worker_inner, generation);
        });

        // 确定性等待：provider 已经被调用（= replay worker 已经持有 run_status_replay_gate），
        // 而不是用 sleep 赌时序。
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let live_inner = Arc::clone(&inner);
        let live = thread::spawn(move || {
            enqueue_run_status_milestone_with_gate(
                &live_inner,
                "toctou-sess",
                build_run_status_payload("toctou-sess", "idle", None),
                "live-idle".to_owned(),
            );
        });

        // 不需要额外 sleep 硬等"实时"线程真正排到锁上——正确性不依赖调度时机：无论 `live`
        // 线程此刻是否已经开始阻塞在 `lock()` 上，它都不可能在 worker 释放 `run_status_replay_
        // gate` 之前完成入队；这里放行 provider 让补发批走完它自己的读+入队。
        release_tx.send(()).unwrap();

        worker.join().unwrap();
        live.join().unwrap();

        let (_, first) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("补发的陈旧现状帧应该先入队");
        let (_, second) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("实时翻转帧应该紧随其后入队");
        assert_eq!(first.t, "run.status");
        assert_eq!(first.payload["status"], "running", "补发帧携带 provider 读到的陈旧状态");
        assert_eq!(second.t, "run.status");
        assert_eq!(
            second.payload["status"], "idle",
            "实时翻转帧必须排在补发帧之后——客户端最终看到的是新状态，不会被陈旧帧倒灌覆盖"
        );
        assert!(
            milestone_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "不该有第三帧"
        );
    }

    #[test]
    fn snapshot_generation_captured_at_spawn_survives_a_mid_flight_connection_switch() {
        // M#4/M#5 复审定罪的 TOCTOU：session-index 快照 provider 耗时不可控（DB mutex 竞争 +
        // O(会话数)扫描 + JSON 序列化），这段时间里连接完全可能已经被顶替。正确性现在完全依赖
        // `enqueue_milestone_with_generation` 用调用方在 spawn 前捕获的 generation 打标、不
        // 重读"当前"值——即使连接切代发生在 provider 返回之后（即历史上那个"核对通过之后、
        // 入队之前"的窄窗口），打的标签也必须还是捕获时刻的 generation_a，而不是被顶替后的
        // generation_b。
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let provider_release_rx = Arc::clone(&release_rx);
        let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            move || {
                provider_release_rx.lock().unwrap().recv().unwrap();
                Some(serde_json::json!([]))
            },
        );
        let generation_a = inner.state.advance_generation_and_set_gate(true);
        let thread_inner = Arc::clone(&inner);
        let handle = thread::Builder::new()
            .name("remote-index-snapshot".to_owned())
            .spawn(move || publish_session_index_snapshot_on_connect(&thread_inner, generation_a))
            .unwrap();

        // 连接切代发生在 provider 卡住期间。
        let generation_b = inner.state.advance_generation_and_set_gate(true);
        assert_ne!(generation_a, generation_b);
        release_tx.send(()).unwrap();
        handle.join().unwrap();

        // 打标必须还是捕获时刻的 generation_a，不能被顶替后的 generation_b 污染。
        let (item_generation, item) = milestone_rx.recv_timeout(Duration::from_secs(2)).expect(
            "stale snapshot should still be enqueued, tagged with the generation captured \
             before the connection switch",
        );
        assert_eq!(item_generation, generation_a);
        assert_ne!(item_generation, generation_b);
        assert_eq!(item.t, "session.index");
        inner
            .milestone_tx
            .try_send((item_generation, item))
            .expect("inspected stale snapshot should be available to the real drain");

        // 下游既有的陈旧过滤器（drain_milestone_queue 里 item_generation != connection_generation）
        // 必须把这条打了旧标签的条目当陈旧丢弃、不出线——用真实的 drain 而不是只信任标签本身。
        let (addr, server) = spawn_discarding_server();
        let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
            .expect("client should connect to discarding server");
        set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
        set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);

        drain_upstream_with_budget(
            &mut socket,
            &inner.state,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([1_u8; 32])),
            "0123456789abcdef0123456789abcdef",
            &test_session_repo_provider(),
            &mut HashMap::new(),
            &mut 0u64,
            Duration::from_secs(2),
        )
        .unwrap();

        assert_eq!(
            inner
                .state
                .upstream_stale_generation_dropped
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(inner.state.frames_sent.load(Ordering::Relaxed), 0);
        drop(socket);
        server.join().expect("discarding server should not panic");
    }

    #[test]
    fn snapshot_requests_are_single_flight_and_serve_the_latest_generation() {
        // g4.2 复审定罪的"退休窗口无界 spawn"：对端反复断连时，所有请求必须由同一个常驻
        // worker 串行服务；provider 卡住期间的新代次只更新 latest 值和容量 1 的唤醒信号，绝不
        // 再创建第二个线程。容量 1 的 channel 可能保留一次已合并的冗余唤醒，所以这里不把
        // provider 总调用数绑死为 2，只锁定真正的安全不变量与 latest-wins 结果。
        let calls_started = Arc::new(AtomicU64::new(0));
        let concurrent = Arc::new(AtomicU64::new(0));
        let peak_concurrent = Arc::new(AtomicU64::new(0));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        let calls_started_provider = Arc::clone(&calls_started);
        let concurrent_provider = Arc::clone(&concurrent);
        let peak_concurrent_provider = Arc::clone(&peak_concurrent);
        let release_rx_provider = Arc::clone(&release_rx);
        let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            move || {
                let now = concurrent_provider.fetch_add(1, Ordering::SeqCst) + 1;
                peak_concurrent_provider.fetch_max(now, Ordering::SeqCst);
                calls_started_provider.fetch_add(1, Ordering::SeqCst);
                release_rx_provider.lock().unwrap().recv().unwrap();
                concurrent_provider.fetch_sub(1, Ordering::SeqCst);
                Some(serde_json::json!([]))
            },
        );

        const CONNECTION_COUNT: usize = 20;
        let mut generations = Vec::with_capacity(CONNECTION_COUNT);
        for _ in 0..CONNECTION_COUNT {
            let generation = inner.state.advance_generation_and_set_gate(true);
            generations.push(generation);
            request_session_index_snapshot(&inner, generation);
            if generations.len() == 1 {
                wait_until_counter_at_least(&calls_started, 1);
            }
        }
        let latest_generation = *generations.last().unwrap();

        // 20 次连接建立全部发生在 provider 放行之前：并发调用数必须恒为 1，常驻线程也只应该
        // spawn 一次（其余请求全部靠 latest generation + 容量 1 唤醒信号折叠）。
        wait_until_counter_at_least(&calls_started, 1);
        assert_eq!(peak_concurrent.load(Ordering::SeqCst), 1);
        assert_eq!(
            inner
                .state
                .snapshot_worker_spawn_count
                .load(Ordering::Relaxed),
            1
        );

        release_tx.send(()).unwrap();
        let (first_item_generation, first_item) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first served round should deliver a snapshot");
        assert_eq!(first_item_generation, generations[0]);
        assert_eq!(first_item.t, "session.index");

        // 同一个 worker 发现请求已经变新，直接在内层循环继续跑下一轮——不是新开线程。
        wait_until_counter_at_least(&calls_started, 2);
        assert_eq!(peak_concurrent.load(Ordering::SeqCst), 1);
        assert_eq!(
            inner
                .state
                .snapshot_worker_spawn_count
                .load(Ordering::Relaxed),
            1
        );

        // 第二轮结束后，channel 里可能还留着请求风暴期间合并出的一个唤醒信号；多给一个 permit
        // 让这次无害的重复读取也能结束，避免测试把实现允许的唤醒合并细节误判成死锁。
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        let (final_item_generation, final_item) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("coalesced round should deliver the latest generation's snapshot");
        assert_eq!(final_item_generation, latest_generation);
        assert_eq!(final_item.t, "session.index");

        assert_eq!(peak_concurrent.load(Ordering::SeqCst), 1);
        assert_eq!(
            inner
                .state
                .snapshot_worker_spawn_count
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn snapshot_worker_wakes_again_after_returning_to_recv() {
        // 第一轮完成后不再有 generation 变化，worker 必须回到外层 `rx.recv()` 挂起；稍后再来的
        // 请求仍要唤醒同一个线程并正常送达，不能把常驻循环误写成只服务第一轮的一次性线程。
        let calls_started = Arc::new(AtomicU64::new(0));
        let calls_started_provider = Arc::clone(&calls_started);
        let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            move || {
                calls_started_provider.fetch_add(1, Ordering::SeqCst);
                Some(serde_json::json!([]))
            },
        );

        let first_generation = inner.state.advance_generation_and_set_gate(true);
        request_session_index_snapshot(&inner, first_generation);
        let (first_item_generation, first_item) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first wake should deliver a snapshot");
        assert_eq!(first_item_generation, first_generation);
        assert_eq!(first_item.t, "session.index");

        // 故意用同一个 generation 再请求一次：内层 latest-wins recheck 看到值没变，绝不可能自己
        // 多跑一轮来代偿；第二次 provider 调用只能来自外层重新消费 channel wake。这样无需 sleep
        // 猜调度，也能证明 worker 第一轮结束后仍保留了再次挂起/唤醒的能力。
        request_session_index_snapshot(&inner, first_generation);
        let (second_item_generation, second_item) = milestone_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should wake again after returning to recv");
        assert_eq!(second_item_generation, first_generation);
        assert_eq!(second_item.t, "session.index");
        assert_eq!(calls_started.load(Ordering::SeqCst), 2);
        assert_eq!(
            inner
                .state
                .snapshot_worker_spawn_count
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn milestone_current_snapshot_uses_one_packed_load_and_each_path_owns_its_gate_semantics() {
        // "当前快照"路径的黑盒契约：gate=false 时整体 no-op；状态完整推进到 (B, true) 后必须
        // 打 B 标签，不能残留先前 (A, false) 的 generation。纯入队层故意不读 gate，而显式
        // generation 路径仍独立读取当前 gate 并保留调用方捕获的标签——三层分工不能重新混合。
        let state = GatewayInnerState::default();
        let generation_a = state.advance_generation_and_set_gate(false);
        let item = || MilestoneItem {
            session: Some("sess-1".to_owned()),
            t: "run.status".to_owned(),
            payload: serde_json::json!({"status": "running"}),
            client_msg_id: "client-packed-snapshot".to_owned(),
        };

        let (current_tx, current_rx) = mpsc::sync_channel(2);
        enqueue_milestone_for_upstream(&state, &current_tx, item());
        assert!(
            current_rx.try_recv().is_err(),
            "closed gate must be a no-op"
        );

        let (raw_tx, raw_rx) = mpsc::sync_channel(1);
        enqueue_milestone_item(&state, &raw_tx, generation_a, item());
        assert_eq!(raw_rx.try_recv().unwrap().0, generation_a);

        let (captured_tx, captured_rx) = mpsc::sync_channel(2);
        enqueue_milestone_with_generation(&state, &captured_tx, generation_a, item());
        assert!(
            captured_rx.try_recv().is_err(),
            "explicit-generation path still owns an independent current-gate check"
        );

        let generation_b = generation_a + 1;
        state
            .upstream_state
            .store((generation_b << 1) | 1, Ordering::Release);
        enqueue_milestone_for_upstream(&state, &current_tx, item());
        assert_eq!(current_rx.try_recv().unwrap().0, generation_b);

        enqueue_milestone_with_generation(&state, &captured_tx, generation_a, item());
        assert_eq!(captured_rx.try_recv().unwrap().0, generation_a);

        // 普通单线程黑盒调用无法在旧实现的两次 load 之间插入切代，统计式竞态测试又会把回归
        // 保护交给调度运气。因此这里额外把复审结论变成代码形态断言：薄壳只能直接做一次 packed
        // load，不能重新委托给会独立读 gate 的显式-generation 路径。该断言专门接受变异验证：
        // 恢复 `connection_generation_snapshot` + `enqueue_milestone_with_generation` 时必须稳定变红。
        let source = include_str!("remote_gateway.rs");
        let function_start = source
            .find("fn enqueue_milestone_for_upstream(")
            .expect("current-snapshot enqueue function must exist");
        let function_tail = &source[function_start..];
        let function_end = function_tail
            .find("\n}\n\nfn is_valid_client_msg_id")
            .expect("current-snapshot enqueue function boundary must remain recognizable");
        let function_source = &function_tail[..function_end];
        assert_eq!(function_source.matches("upstream_state.load").count(), 1);
        assert!(!function_source.contains("connection_generation_snapshot"));
        assert!(!function_source.contains("enqueue_milestone_with_generation"));
    }

    #[test]
    fn entropy_failure_uses_existing_invalid_client_msg_id_drop_path() {
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        let (tx, rx) = mpsc::sync_channel(1);
        let client_msg_id = {
            let _guard = ForceClientMsgIdEntropyFailureGuard::new();
            try_random_client_msg_id().unwrap_or_default()
        };

        enqueue_milestone_for_upstream(
            &state,
            &tx,
            MilestoneItem {
                session: Some("sess-1".to_owned()),
                t: "run.status".to_owned(),
                payload: serde_json::json!({"status": "running"}),
                client_msg_id,
            },
        );

        assert_eq!(state.milestone_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_iter().count(), 0);
    }

    #[test]
    fn entropy_failure_drops_session_index_snapshot_without_panicking() {
        let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
            || None,
            |_| Some(Zeroizing::new([1_u8; 32])),
            || Some(serde_json::json!([])),
        );
        let connection_generation = inner.state.advance_generation_and_set_gate(true);

        {
            let _guard = ForceClientMsgIdEntropyFailureGuard::new();
            publish_session_index_snapshot_on_connect(&inner, connection_generation);
        }

        assert_eq!(inner.state.milestone_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(milestone_rx.try_iter().count(), 0);
    }

    #[test]
    fn entropy_failure_does_not_panic_in_random_id_publish_facades() {
        // `GATEWAY` is intentionally not installed in unit tests, so this only proves that every
        // random-ID facade survives entropy failure; enqueue/drop accounting is covered directly.
        let _guard = ForceClientMsgIdEntropyFailureGuard::new();

        publish_run_status_milestone("sess-1", "running", Some("run-1"));
        publish_session_index_created("sess-1", "Session", "repo-1", "namespace-1", None);
        publish_session_index_renamed("sess-1", "Renamed");
        publish_session_index_deleted("sess-1");
        publish_session_index_archived(&["sess-1".to_owned()], true);
    }

    #[test]
    fn published_milestone_round_trips_without_client_msg_id_in_aad() {
        let room = "0123456789abcdef0123456789abcdef";
        let client_msg_id = "73996db9-9424-5e73-acb6-965bf87bfb80";
        let k_room = Zeroizing::new([41_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是 client_msg_id 不进 AAD 的
        // 加密不变量），必须配一个 active repo，不然 "sess-1" 会被 fail-closed 挡下。
        *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: Some("sess-1".to_owned()),
                t: "msg.completed".to_owned(),
                payload: serde_json::json!({"message_id": "message-1"}),
                client_msg_id: client_msg_id.to_owned(),
            },
        );

        let (addr, frames, server) = spawn_recording_server(1);
        let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
        drain_upstream(
            &mut socket,
            &state,
            &upstream_rx,
            &milestone_rx,
            Some(&k_room),
            room,
            &test_session_repo_provider_allowing_default_repo(),
            &mut HashMap::new(),
            &mut 0u64,
        )
        .unwrap();
        let envelope = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(envelope["kind"], "event");
        assert_eq!(envelope["client_msg_id"], client_msg_id);
        assert_eq!(envelope["seq"], Value::Null);
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        assert_eq!(plaintext["t"], "msg.completed");
        assert_eq!(plaintext["message_id"], "message-1");

        let mut changed_id = envelope.clone();
        changed_id["client_msg_id"] = Value::String("different-client-id".to_owned());
        assert_eq!(open_upstream_envelope(&k_room, &changed_id), plaintext);
        drop(upstream_tx);
        drop(socket);
        server.join().unwrap();
    }

    #[test]
    fn milestone_channel_full_is_counted_and_never_blocks() {
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        let (tx, rx) = mpsc::sync_channel(1);
        let item = |client_msg_id: &str| MilestoneItem {
            session: Some("sess-1".to_owned()),
            t: "msg.completed".to_owned(),
            payload: serde_json::json!({"message_id": "message-1"}),
            client_msg_id: client_msg_id.to_owned(),
        };

        enqueue_milestone_for_upstream(&state, &tx, item("client-1"));
        enqueue_milestone_for_upstream(&state, &tx, item("client-2"));

        assert_eq!(state.milestone_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn drain_sends_milestones_before_live_frames_in_the_same_round() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([42_u8; 32]);
        let state = GatewayInnerState::default();
        // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是同一轮 drain 内里程碑先于
        // live 帧发出的顺序），必须配一个 active repo，不然两条 session 都会被 fail-closed
        // 挡下。
        *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        let generation = state.advance_generation_and_set_gate(true);
        let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        upstream_tx
            .try_send((
                generation,
                LiveQueueItem::Batch(text_delta_payload("sess-live")),
            ))
            .unwrap();
        milestone_tx
            .try_send((
                generation,
                MilestoneItem {
                    session: Some("sess-milestone".to_owned()),
                    t: "msg.completed".to_owned(),
                    payload: serde_json::json!({"message_id": "message-1"}),
                    client_msg_id: "client-priority".to_owned(),
                },
            ))
            .unwrap();
        let (addr, frames, server) = spawn_recording_server(2);
        let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();

        drain_upstream(
            &mut socket,
            &state,
            &upstream_rx,
            &milestone_rx,
            Some(&k_room),
            room,
            &test_session_repo_provider_allowing_default_repo(),
            &mut HashMap::new(),
            &mut 0u64,
        )
        .unwrap();

        let first = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(first["kind"], "event");
        assert_eq!(
            open_upstream_envelope(&k_room, &first)["t"],
            "msg.completed"
        );
        assert_eq!(second["kind"], "live");
        assert_eq!(open_upstream_envelope(&k_room, &second)["t"], "text_delta");
        drop(socket);
        server.join().unwrap();
    }

    #[test]
    fn tool_started_name_survives_generation_change_and_completed_removes_it() {
        use crate::agent_event::{AgentEvent, CardKind, ToolStatus};

        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([43_u8; 32]);
        let state = GatewayInnerState::default();
        // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是 tool 名字关联跨代号存活
        // + completed 清理关联表），必须配一个 active repo，不然 "sess-1" 会被 fail-closed
        // 挡下。
        *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        let generation_a = state.advance_generation_and_set_gate(true);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (milestone_tx_a, milestone_rx_a) = mpsc::sync_channel(2);
        assert_eq!(generation_a, 1);
        enqueue_batch_payload_for_upstream(
            &state,
            &started_tx,
            &milestone_tx_a,
            single_event_payload(
                "run-1",
                "sess-1",
                AgentEvent::ToolStarted {
                    id: "tool-1".to_owned(),
                    tool: "shell".to_owned(),
                    summary: "run command".to_owned(),
                    card: CardKind::Command,
                },
            ),
        );
        let (addr_a, server_a) = spawn_discarding_server();
        let (mut socket_a, _) = connect_with_config(format!("ws://{addr_a}"), None, 3).unwrap();
        drain_upstream(
            &mut socket_a,
            &state,
            &started_rx,
            &milestone_rx_a,
            Some(&k_room),
            room,
            &test_session_repo_provider_allowing_default_repo(),
            &mut HashMap::new(),
            &mut 0u64,
        )
        .unwrap();
        assert_eq!(state.frames_sent.load(Ordering::Relaxed), 0);
        drop(socket_a);
        server_a.join().unwrap();

        state.disable_upstream_gate();
        let generation_b = state.advance_generation_and_set_gate(true);
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let (milestone_tx_b, milestone_rx_b) = mpsc::sync_channel(2);
        assert_eq!(generation_b, 2);
        enqueue_batch_payload_for_upstream(
            &state,
            &completed_tx,
            &milestone_tx_b,
            crate::event_transport::BatchPayload {
                batches: vec![crate::event_transport::RunBatch {
                    session_id: "sess-1".to_owned(),
                    run_id: "run-1".to_owned(),
                    dispatch: None,
                    events: vec![
                        crate::event_transport::SequencedEvent {
                            seq: 2,
                            event: AgentEvent::ToolCompleted {
                                id: "tool-1".to_owned(),
                                status: ToolStatus::Ok,
                                exit_code: Some(0),
                                output: Some("done".to_owned()),
                            },
                        },
                        crate::event_transport::SequencedEvent {
                            seq: 3,
                            event: AgentEvent::ToolCompleted {
                                id: "unknown".to_owned(),
                                status: ToolStatus::Failed,
                                exit_code: None,
                                output: None,
                            },
                        },
                    ],
                }],
            },
        );
        let (addr_b, frames, server_b) = spawn_recording_server(2);
        let (mut socket_b, _) = connect_with_config(format!("ws://{addr_b}"), None, 3).unwrap();
        drain_upstream(
            &mut socket_b,
            &state,
            &completed_rx,
            &milestone_rx_b,
            Some(&k_room),
            room,
            &test_session_repo_provider_allowing_default_repo(),
            &mut HashMap::new(),
            &mut 0u64,
        )
        .unwrap();

        let known_envelope = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(known_envelope["kind"], "event");
        assert_eq!(
            known_envelope["client_msg_id"],
            "d0c166ac-91e7-53b3-a992-73d8f4246a0e"
        );
        assert_eq!(known_envelope["seq"], Value::Null);
        let known = open_upstream_envelope(&k_room, &known_envelope);
        assert_eq!(known["t"], "tool.completed");
        assert_eq!(known["tool"], "shell");
        assert_eq!(known["status"], "ok");
        assert_eq!(known["exit_code"], 0);
        assert_eq!(known["output"], "done");

        let unknown = open_upstream_envelope(
            &k_room,
            &frames.recv_timeout(Duration::from_secs(2)).unwrap(),
        );
        assert_eq!(unknown["tool"], "");
        assert_eq!(unknown["status"], "failed");
        assert_eq!(unknown["exit_code"], Value::Null);
        assert_eq!(unknown["output"], Value::Null);
        let correlation = lock(&state.tool_correlation);
        assert!(correlation.names.is_empty());
        assert!(correlation.order.is_empty());
        drop(correlation);
        drop(socket_b);
        server_b.join().unwrap();
    }

    #[test]
    fn tool_correlation_capacity_evicts_oldest_orphan_and_admits_new_key() {
        let state = GatewayInnerState::default();
        for index in 0..TOOL_CORRELATION_CAPACITY {
            remember_tool_name(
                &state,
                "run-x",
                &format!("tool-{index}"),
                &format!("name-{index}"),
            );
        }
        remember_tool_name(&state, "run-x", "overflow", "must-not-drop");

        assert_eq!(
            lock(&state.tool_correlation).names.len(),
            TOOL_CORRELATION_CAPACITY
        );
        assert_eq!(state.tool_correlation_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(take_tool_name(&state, "run-x", "tool-0"), "");
        assert_eq!(take_tool_name(&state, "run-x", "overflow"), "must-not-drop");
    }

    #[test]
    fn tool_correlation_key_keeps_reused_tool_id_isolated_by_run() {
        let state = GatewayInnerState::default();
        remember_tool_name(&state, "run-a", "tool-1", "shell-a");
        remember_tool_name(&state, "run-b", "tool-1", "shell-b");

        assert_eq!(take_tool_name(&state, "run-a", "tool-1"), "shell-a");
        assert_eq!(take_tool_name(&state, "run-b", "tool-1"), "shell-b");
        let correlation = lock(&state.tool_correlation);
        assert!(correlation.names.is_empty());
        assert!(correlation.order.is_empty());
    }

    #[test]
    fn completed_and_run_closeout_purge_only_their_run_correlations() {
        use crate::agent_event::AgentEvent;

        let state = GatewayInnerState::default();
        remember_tool_name(&state, "run-completed", "tool-1", "shell-completed");
        remember_tool_name(&state, "run-closeout", "tool-1", "shell-closeout");
        remember_tool_name(&state, "run-active", "tool-1", "shell-active");
        state.upstream_state.fetch_or(1, Ordering::Release);
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![
                    crate::event_transport::RunBatch {
                        session_id: "sess-1".to_owned(),
                        run_id: "run-completed".to_owned(),
                        dispatch: None,
                        events: vec![crate::event_transport::SequencedEvent {
                            seq: 1,
                            event: AgentEvent::Completed {
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
                            },
                        }],
                    },
                    crate::event_transport::RunBatch {
                        session_id: "sess-1".to_owned(),
                        run_id: "run-closeout".to_owned(),
                        dispatch: None,
                        events: vec![crate::event_transport::SequencedEvent {
                            seq: 2,
                            event: AgentEvent::RunCloseout {
                                run_id: "run-closeout".to_owned(),
                                commit_sha: None,
                                files_changed: None,
                                insertions: None,
                                deletions: None,
                                interrupted: None,
                            },
                        }],
                    },
                ],
            },
        );

        assert_eq!(take_tool_name(&state, "run-completed", "tool-1"), "");
        assert_eq!(take_tool_name(&state, "run-closeout", "tool-1"), "");
        assert_eq!(
            take_tool_name(&state, "run-active", "tool-1"),
            "shell-active"
        );
        let correlation = lock(&state.tool_correlation);
        assert!(correlation.names.is_empty());
        assert!(correlation.order.is_empty());
    }

    // ---- P0-b：maintain_partial_snapshots（sink 归约态维护，同层 extract_tool_milestones）----

    #[test]
    fn partial_snapshot_accumulates_across_multiple_sink_calls_and_tracks_last_seq() {
        use crate::agent_event::AgentEvent;

        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(2);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(2);

        // 第一次 sink 调用（模拟一轮 drain tick 的 coalesce 批）。
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![crate::event_transport::RunBatch {
                    session_id: "sess-p1".to_owned(),
                    run_id: "run-p1".to_owned(),
                    dispatch: None,
                    events: vec![
                        crate::event_transport::SequencedEvent {
                            seq: 3,
                            event: AgentEvent::TextDelta {
                                text: "hello ".to_owned(),
                            },
                        },
                        crate::event_transport::SequencedEvent {
                            seq: 7,
                            event: AgentEvent::TextDelta {
                                text: "world".to_owned(),
                            },
                        },
                    ],
                }],
            },
        );
        // 第二次 sink 调用（下一轮 tick）——归约态必须跨调用持续累积，不是每次重建。
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![crate::event_transport::RunBatch {
                    session_id: "sess-p1".to_owned(),
                    run_id: "run-p1".to_owned(),
                    dispatch: None,
                    events: vec![crate::event_transport::SequencedEvent {
                        seq: 9,
                        event: AgentEvent::TextDelta {
                            text: "!".to_owned(),
                        },
                    }],
                }],
            },
        );

        let snapshots = lock(&state.partial_snapshots);
        let entry = snapshots
            .get("sess-p1")
            .expect("entry must exist after two sink calls");
        assert_eq!(entry.run_id, "run-p1");
        assert_eq!(
            entry.last_seq, 9,
            "through_run_seq 水位必须是 sink 看到的最后一条 seq"
        );
        assert_eq!(
            entry.reducer.snapshot_blocks(),
            vec![crate::db::Block::Text {
                text: "hello world!".to_owned()
            }]
        );
    }

    #[test]
    fn partial_snapshot_rebuilds_reducer_when_run_id_changes_for_same_session() {
        use crate::agent_event::AgentEvent;

        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(2);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(2);

        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            single_event_payload(
                "run-old",
                "sess-p2",
                AgentEvent::TextDelta {
                    text: "stale".to_owned(),
                },
            ),
        );
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![crate::event_transport::RunBatch {
                    session_id: "sess-p2".to_owned(),
                    run_id: "run-new".to_owned(),
                    dispatch: None,
                    events: vec![crate::event_transport::SequencedEvent {
                        seq: 4,
                        event: AgentEvent::TextDelta {
                            text: "fresh".to_owned(),
                        },
                    }],
                }],
            },
        );

        let snapshots = lock(&state.partial_snapshots);
        let entry = snapshots.get("sess-p2").unwrap();
        assert_eq!(entry.run_id, "run-new");
        assert_eq!(entry.last_seq, 4);
        assert_eq!(
            entry.reducer.snapshot_blocks(),
            vec![crate::db::Block::Text {
                text: "fresh".to_owned()
            }],
            "旧 run 的归约态必须被整个丢弃重建，不能跟新 run 的内容合并"
        );
    }

    #[test]
    fn partial_snapshot_cleared_when_completed_or_run_closeout_arrives() {
        use crate::agent_event::AgentEvent;

        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![
                    crate::event_transport::RunBatch {
                        session_id: "sess-completed".to_owned(),
                        run_id: "run-completed".to_owned(),
                        dispatch: None,
                        events: vec![
                            crate::event_transport::SequencedEvent {
                                seq: 1,
                                event: AgentEvent::TextDelta {
                                    text: "hi".to_owned(),
                                },
                            },
                            crate::event_transport::SequencedEvent {
                                seq: 2,
                                event: AgentEvent::Completed {
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
                                },
                            },
                        ],
                    },
                    crate::event_transport::RunBatch {
                        session_id: "sess-closeout".to_owned(),
                        run_id: "run-closeout".to_owned(),
                        dispatch: None,
                        events: vec![
                            crate::event_transport::SequencedEvent {
                                seq: 1,
                                event: AgentEvent::TextDelta {
                                    text: "hi".to_owned(),
                                },
                            },
                            crate::event_transport::SequencedEvent {
                                seq: 2,
                                event: AgentEvent::RunCloseout {
                                    run_id: "run-closeout".to_owned(),
                                    commit_sha: None,
                                    files_changed: None,
                                    insertions: None,
                                    deletions: None,
                                    interrupted: None,
                                },
                            },
                        ],
                    },
                    crate::event_transport::RunBatch {
                        session_id: "sess-active".to_owned(),
                        run_id: "run-active".to_owned(),
                        dispatch: None,
                        events: vec![crate::event_transport::SequencedEvent {
                            seq: 1,
                            event: AgentEvent::TextDelta {
                                text: "still going".to_owned(),
                            },
                        }],
                    },
                ],
            },
        );

        let snapshots = lock(&state.partial_snapshots);
        assert!(
            !snapshots.contains_key("sess-completed"),
            "Completed 必须清掉该 session 的 partial 条目——下次 snapshot 回退到 idle"
        );
        assert!(
            !snapshots.contains_key("sess-closeout"),
            "RunCloseout 必须清掉该 session 的 partial 条目"
        );
        assert!(
            snapshots.contains_key("sess-active"),
            "仍在跑的其它 session 不受影响"
        );
    }

    // P0-b 返工①【阻断修复】：gate 关闭（桌面断连）期间归约态必须照常推进/清理——这是修复
    // 项①的核心回归测试；把 `maintain_partial_snapshots` 挪回 gate 判断之后会让这条测试变红
    // （变异自证，见任务书硬约束）。
    #[test]
    fn maintain_partial_snapshots_runs_even_when_upstream_gate_is_closed() {
        use crate::agent_event::AgentEvent;

        let state = GatewayInnerState::default();
        assert!(
            !state.upstream_enabled_snapshot(),
            "前置：GatewayInnerState::default() 的 gate 必须是关闭的"
        );
        let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        // 断连期间喂一条非终态事件——归约态必须照常推进，即使 gate 关闭、事件不入上行队列。
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![crate::event_transport::RunBatch {
                    session_id: "sess-disconnected".to_owned(),
                    run_id: "run-disc".to_owned(),
                    dispatch: None,
                    events: vec![crate::event_transport::SequencedEvent {
                        seq: 3,
                        event: AgentEvent::TextDelta {
                            text: "offline work".to_owned(),
                        },
                    }],
                }],
            },
        );
        {
            let snapshots = lock(&state.partial_snapshots);
            let entry = snapshots.get("sess-disconnected").expect(
                "gate 关闭也必须维护归约态——否则重连后 control.snapshot 会回假 idle/陈旧态",
            );
            assert_eq!(entry.last_seq, 3);
            assert_eq!(
                entry.reducer.snapshot_blocks(),
                vec![crate::db::Block::Text {
                    text: "offline work".to_owned()
                }]
            );
        }
        assert!(
            upstream_rx.try_recv().is_err(),
            "gate 关闭时事件不得入上行队列"
        );
        assert!(milestone_rx.try_recv().is_err(), "gate 关闭时不产生里程碑");

        // 断连期间也喂一条 Completed 终态——归约态必须照常清理，不能卡在断连窗内永久泄漏。
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![crate::event_transport::RunBatch {
                    session_id: "sess-disconnected".to_owned(),
                    run_id: "run-disc".to_owned(),
                    dispatch: None,
                    events: vec![crate::event_transport::SequencedEvent {
                        seq: 4,
                        event: AgentEvent::Completed {
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
                        },
                    }],
                }],
            },
        );
        assert!(
            !lock(&state.partial_snapshots).contains_key("sess-disconnected"),
            "gate 关闭时 Completed 也必须清理归约态条目，不能永久泄漏"
        );

        // 重连开 gate 后，归约态维护继续正常工作（不是被"卡死"）。
        state.advance_generation_and_set_gate(true);
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            single_event_payload(
                "run-after-reconnect",
                "sess-after-reconnect",
                AgentEvent::TextDelta {
                    text: "back online".to_owned(),
                },
            ),
        );
        assert!(
            lock(&state.partial_snapshots).contains_key("sess-after-reconnect"),
            "重连开 gate 后归约态维护必须继续正常工作"
        );
    }

    #[test]
    fn partial_snapshot_capacity_rejects_new_session_but_keeps_updating_existing_ones() {
        use crate::agent_event::AgentEvent;

        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

        for index in 0..PARTIAL_SNAPSHOT_CAPACITY {
            enqueue_batch_payload_for_upstream(
                &state,
                &upstream_tx,
                &milestone_tx,
                single_event_payload(
                    &format!("run-{index}"),
                    &format!("sess-{index}"),
                    AgentEvent::TextDelta {
                        text: "x".to_owned(),
                    },
                ),
            );
        }
        assert_eq!(
            lock(&state.partial_snapshots).len(),
            PARTIAL_SNAPSHOT_CAPACITY
        );

        // 超限：新 session 必须被拒收，表大小不越界。
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            single_event_payload(
                "run-overflow",
                "sess-overflow",
                AgentEvent::TextDelta {
                    text: "y".to_owned(),
                },
            ),
        );
        {
            let snapshots_after_overflow = lock(&state.partial_snapshots);
            assert_eq!(
                snapshots_after_overflow.len(),
                PARTIAL_SNAPSHOT_CAPACITY,
                "满表时新 session 必须被拒收，不能越界增长"
            );
            assert!(!snapshots_after_overflow.contains_key("sess-overflow"));
        }
        // P0-b 返工⑤：拒收计原子计数器（原为 eprintln! 无界刷屏）。
        assert_eq!(
            state
                .partial_snapshot_capacity_dropped
                .load(Ordering::Relaxed),
            1
        );

        // 满表状态下，已有 session 仍必须能正常更新（只拒收*新*键，不冻结旧键）。
        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            crate::event_transport::BatchPayload {
                batches: vec![crate::event_transport::RunBatch {
                    session_id: "sess-0".to_owned(),
                    run_id: "run-0".to_owned(),
                    dispatch: None,
                    events: vec![crate::event_transport::SequencedEvent {
                        seq: 5,
                        event: AgentEvent::TextDelta {
                            text: " more".to_owned(),
                        },
                    }],
                }],
            },
        );
        let snapshots_final = lock(&state.partial_snapshots);
        assert_eq!(snapshots_final.len(), PARTIAL_SNAPSHOT_CAPACITY);
        let entry0 = snapshots_final.get("sess-0").unwrap();
        assert_eq!(entry0.last_seq, 5);
        assert_eq!(
            entry0.reducer.snapshot_blocks(),
            vec![crate::db::Block::Text {
                text: "x more".to_owned()
            }]
        );
    }

    #[test]
    fn tool_completed_uses_milestone_queue_when_live_queue_is_full() {
        use crate::agent_event::{AgentEvent, ToolStatus};

        let state = GatewayInnerState::default();
        let generation = state.advance_generation_and_set_gate(true);
        remember_tool_name(&state, "run-1", "tool-1", "shell");
        let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        upstream_tx
            .try_send((
                generation,
                LiveQueueItem::Batch(text_delta_payload("occupied")),
            ))
            .unwrap();
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        enqueue_batch_payload_for_upstream(
            &state,
            &upstream_tx,
            &milestone_tx,
            single_event_payload(
                "run-1",
                "sess-1",
                AgentEvent::ToolCompleted {
                    id: "tool-1".to_owned(),
                    status: ToolStatus::Ok,
                    exit_code: Some(0),
                    output: Some("done".to_owned()),
                },
            ),
        );

        assert_eq!(state.upstream_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(
            upstream_rx.try_recv().unwrap().1,
            LiveQueueItem::Batch(text_delta_payload("occupied"))
        );
        let (item_generation, item) = milestone_rx.try_recv().unwrap();
        assert_eq!(item_generation, generation);
        assert_eq!(item.session.as_deref(), Some("sess-1"));
        assert_eq!(item.t, "tool.completed");
        assert_eq!(item.payload["id"], "tool-1");
        assert_eq!(item.payload["tool"], "shell");
        assert_eq!(item.payload["status"], "ok");
        assert_eq!(item.payload["exit_code"], 0);
        assert_eq!(item.payload["output"], "done");
    }

    #[test]
    fn tool_completed_milestone_exists_before_live_budget_drain() {
        use crate::agent_event::{AgentEvent, ToolStatus};

        let (addr, server) = spawn_discarding_server();
        let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
            .expect("client should connect to discarding server");
        set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
        set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();

        let state = GatewayInnerState::default();
        // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是 tool.completed 里程碑先于
        // budget 耗尽的 live drain 存在），必须配一个 active repo，不然 "sess-1" 会被
        // fail-closed 挡下。
        *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        state.advance_generation_and_set_gate(true);
        remember_tool_name(&state, "run-1", "tool-1", "shell");
        let (upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let payload = crate::event_transport::BatchPayload {
            batches: vec![crate::event_transport::RunBatch {
                session_id: "sess-1".to_owned(),
                run_id: "run-1".to_owned(),
                dispatch: None,
                events: vec![
                    crate::event_transport::SequencedEvent {
                        seq: 1,
                        event: AgentEvent::TextDelta {
                            text: "before completion".to_owned(),
                        },
                    },
                    crate::event_transport::SequencedEvent {
                        seq: 2,
                        event: AgentEvent::ToolCompleted {
                            id: "tool-1".to_owned(),
                            status: ToolStatus::Ok,
                            exit_code: Some(0),
                            output: Some("done".to_owned()),
                        },
                    },
                ],
            }],
        };

        enqueue_batch_payload_for_upstream(&state, &upstream_tx, &milestone_tx, payload);

        let (_, milestone) = milestone_rx
            .try_recv()
            .expect("tool.completed milestone must exist before any live drain");
        assert_eq!(milestone.t, "tool.completed");
        assert_eq!(milestone.payload["tool"], "shell");
        assert_eq!(milestone.payload["status"], "ok");
        assert_eq!(milestone.payload["exit_code"], 0);
        assert_eq!(milestone.payload["output"], "done");

        drain_upstream_with_budget(
            &mut socket,
            &state,
            &upstream_rx,
            &milestone_rx,
            Some(&Zeroizing::new([7_u8; 32])),
            "0123456789abcdef0123456789abcdef",
            &test_session_repo_provider_allowing_default_repo(),
            &mut HashMap::new(),
            &mut 0u64,
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(state.frames_sent.load(Ordering::Relaxed), 1);
        assert_eq!(state.upstream_budget_dropped.load(Ordering::Relaxed), 1);
        drop(socket);
        server.join().expect("discarding server should not panic");
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

    fn multi_event_payload(
        batch_count: usize,
        events_per_batch: usize,
    ) -> crate::event_transport::BatchPayload {
        crate::event_transport::BatchPayload {
            batches: (0..batch_count)
                .map(|batch_index| crate::event_transport::RunBatch {
                    session_id: format!("sess-{batch_index}"),
                    run_id: format!("run-{batch_index}"),
                    dispatch: None,
                    events: (0..events_per_batch)
                        .map(|event_index| crate::event_transport::SequencedEvent {
                            seq: event_index as u64,
                            event: crate::agent_event::AgentEvent::TextDelta {
                                text: "hi".to_owned(),
                            },
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn sink_gate_moves_owned_payload_unchanged_and_counts_full_queue() {
        let state = GatewayInnerState::default();
        let (tx, rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
        let gated_payload = text_delta_payload("gated");

        enqueue_batch_payload_for_upstream(&state, &tx, &milestone_tx, gated_payload);
        assert!(rx.try_recv().is_err());
        assert_eq!(state.classify_skipped.load(Ordering::Relaxed), 0);
        assert_eq!(state.upstream_dropped.load(Ordering::Relaxed), 0);

        state.upstream_state.fetch_or(1, Ordering::Release);
        let first = text_delta_payload("sess-1");
        enqueue_batch_payload_for_upstream(&state, &tx, &milestone_tx, first.clone());
        enqueue_batch_payload_for_upstream(
            &state,
            &tx,
            &milestone_tx,
            text_delta_payload("sess-2"),
        );

        assert_eq!(rx.try_recv().unwrap(), (0, LiveQueueItem::Batch(first)));
        assert_eq!(state.upstream_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(state.classify_skipped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn upstream_gate_guard_disables_flag_on_normal_scope_exit() {
        let state = AtomicU64::new(1);
        {
            let _guard = UpstreamGateGuard::new(&state);
            assert!(state.load(Ordering::Acquire) & 1 == 1);
        }
        assert_eq!(state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn upstream_gate_guard_disables_flag_on_panic_unwind() {
        let state = AtomicU64::new(1);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = UpstreamGateGuard::new(&state);
            panic!("boom");
        }));

        assert!(result.is_err());
        assert_eq!(state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn drain_upstream_processes_at_most_one_bounded_round() {
        let (addr, server) = spawn_discarding_server();
        let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
            .expect("client should connect to discarding server");
        set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
        set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();

        let state = GatewayInnerState::default();
        // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是每轮最多处理
        // MAX_DRAIN_ITEMS_PER_ROUND 条的预算上限），必须配一个 active repo，不然全部 100 条
        // "sess-N" 都会被 fail-closed 挡下，永远数不出 MAX_DRAIN_ITEMS_PER_ROUND 条已发送帧。
        *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        let (tx, rx) = mpsc::sync_channel(100);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        for index in 0..100 {
            tx.try_send((
                0,
                LiveQueueItem::Batch(text_delta_payload(&format!("sess-{index}"))),
            ))
            .unwrap();
        }
        let k_room = Zeroizing::new([7_u8; 32]);

        drain_upstream(
            &mut socket,
            &state,
            &rx,
            &milestone_rx,
            Some(&k_room),
            "0123456789abcdef0123456789abcdef",
            &test_session_repo_provider_allowing_default_repo(),
            &mut HashMap::new(),
            &mut 0u64,
        )
        .unwrap();

        assert_eq!(
            state.frames_sent.load(Ordering::Relaxed),
            MAX_DRAIN_ITEMS_PER_ROUND as u64
        );
        let mut remaining = 0;
        while rx.try_recv().is_ok() {
            remaining += 1;
        }
        assert_eq!(remaining, 100 - MAX_DRAIN_ITEMS_PER_ROUND);
        drop(socket);
        server.join().expect("discarding server should not panic");
    }

    #[test]
    fn drain_upstream_budget_drops_the_rest_of_a_multi_event_payload() {
        let (addr, server) = spawn_discarding_server();
        let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
            .expect("client should connect to discarding server");
        set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
        set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();

        let state = GatewayInnerState::default();
        // M2-4d：归属闸恒启用，这条测试不关心归属过滤本身（覆盖的是单条多事件 payload 被
        // budget 截断），必须配一个 active repo，不然 payload 里的 session 会被 fail-closed
        // 挡下。
        *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        let (tx, rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        tx.try_send((0, LiveQueueItem::Batch(multi_event_payload(5, 10))))
            .unwrap();
        let k_room = Zeroizing::new([7_u8; 32]);

        drain_upstream_with_budget(
            &mut socket,
            &state,
            &rx,
            &milestone_rx,
            Some(&k_room),
            "0123456789abcdef0123456789abcdef",
            &test_session_repo_provider_allowing_default_repo(),
            &mut HashMap::new(),
            &mut 0u64,
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(state.frames_sent.load(Ordering::Relaxed), 1);
        assert_eq!(state.upstream_budget_dropped.load(Ordering::Relaxed), 1);
        drop(socket);
        server.join().expect("discarding server should not panic");
    }

    #[test]
    fn write_timeout_is_fixed_at_five_hundred_milliseconds() {
        assert_eq!(WRITE_TIMEOUT, Duration::from_millis(500));
    }

    #[test]
    fn attempt_without_k_room_disables_upstream() {
        let room = "0123456789abcdef0123456789abcdef".to_owned();
        let resolved_room = room.clone();
        // M2-4d：legacy 全局 `remote_room_id` 回落已撤，`current_config` 只认
        // `remote_active_repo_id`——这里改走 active 房解析路径，不然设置里的 `remote_room_id`
        // 不会再被读到，`attempt_once` 会在 current_config 判"未配置"那一步直接短路返回
        // `Waiting`（也会 disable_upstream_gate，断言会"因为没跑到"而不是"因为真的没有
        // k_room"而通过——见本测试改写说明），测试就测不到本该覆盖的"缺 K_room"路径了。
        let inner = test_inner_with_active_room_resolver(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("ws://127.0.0.1:1".to_owned()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            },
            move |_project_id| Ok(resolved_room.clone()),
        );
        let (_tx, rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let _ = attempt_once(&inner, &rx, &milestone_rx);

        assert!(!inner.state.upstream_enabled_snapshot());
    }

    #[test]
    fn failed_attempt_with_k_room_keeps_upstream_disabled() {
        let room = "0123456789abcdef0123456789abcdef".to_owned();
        let resolved_room = room.clone();
        let provider_room = room.clone();
        // M2-4d：同上一条测试，改走 active 房解析路径（不再是 legacy `remote_room_id` 回落）。
        let inner = test_inner_with_active_room_resolver_and_k_room(
            |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some("ws://127.0.0.1:1".to_owned()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            },
            move |_project_id| Ok(resolved_room.clone()),
            move |room_id| (room_id == provider_room).then(|| Zeroizing::new([1_u8; 32])),
        );
        let (_tx, rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

        let _ = attempt_once(&inner, &rx, &milestone_rx);

        assert!(!inner.state.upstream_enabled_snapshot());
    }

    #[test]
    fn live_connection_gates_upstream_and_raii_guard_disables_it_on_disconnect() {
        let (addr, release_server, server) = spawn_holding_server();
        let relay_url = format!("ws://{addr}");
        let room_id = "0123456789abcdef0123456789abcdef".to_owned();
        let connected_config = GatewayConfig {
            relay_url,
            room_id,
            active_repo_id: None,
        };
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let inner = test_inner(|_| None, || None);
        let (_tx, rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let k_room = Zeroizing::new([1_u8; 32]);

        let result = thread::scope(|scope| {
            let connection_inner = &inner;
            let connection_url = &url;
            let connection_config = &connected_config;
            let connection_k_room = &k_room;
            let connection = scope.spawn(move || {
                let result = run_authenticated_connection(
                    connection_inner,
                    connection_url,
                    &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                    connection_config,
                    None,
                    &rx,
                    &milestone_rx,
                    Some(connection_k_room),
                );
                result
            });

            wait_until_connected(&inner);
            assert!(inner.state.upstream_enabled_snapshot());
            inner.shutdown.store(true, Ordering::Release);
            release_server.send(()).unwrap();

            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && !connection.is_finished() {
                thread::sleep(Duration::from_millis(20));
            }
            assert!(
                connection.is_finished(),
                "connection did not finish within three seconds"
            );
            connection
                .join()
                .expect("connection thread should not panic")
        });

        assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
        assert!(!inner.state.upstream_enabled_snapshot());
        server.join().expect("holding server should not panic");
    }

    #[test]
    fn live_connection_publishes_session_index_snapshot() {
        // M2-4d：归属闸恒启用——这条测试不关心归属过滤本身（覆盖的是快照帧的信封/负载形状），
        // 必须显式配一个 active repo，且会话的 repo_id 要跟它一致，不然全会被
        // `filter_session_index_snapshot_for_active_repo` fail-closed 成空数组。
        let sessions = serde_json::json!([{
            "id": "s1",
            "title": "T",
            "repo_id": "repo-a",
            "archived": false,
            "status": Value::Null,
            "run_id": Value::Null,
            "updated_at": 1,
        }]);
        let provider_sessions = sessions.clone();
        let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
            || None,
            |_| Some(Zeroizing::new([17_u8; 32])),
            move || Some(provider_sessions.clone()),
        );
        *lock(&inner.state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (addr, frames, server) = spawn_recording_server_after_sync(1);
        let connected_config = GatewayConfig {
            relay_url: format!("ws://{addr}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: Some("repo-a".to_owned()),
        };
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let k_room = Zeroizing::new([17_u8; 32]);

        let frame_result = thread::scope(|scope| {
            let connection_inner = &inner;
            let connection_url = &url;
            let connection_config = &connected_config;
            let connection_k_room = &k_room;
            let connection = scope.spawn(move || {
                run_authenticated_connection(
                    connection_inner,
                    connection_url,
                    &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                    connection_config,
                    None,
                    &upstream_rx,
                    &milestone_rx,
                    Some(connection_k_room),
                )
            });

            let frame_result = frames.recv_timeout(Duration::from_secs(3));
            inner.shutdown.store(true, Ordering::Release);
            let _ = connection
                .join()
                .expect("connection thread should not panic");
            frame_result
        });

        let server_result = server.join();
        let envelope = frame_result.expect("session.index snapshot frame should arrive");
        assert_eq!(envelope["kind"], "event");
        assert_eq!(envelope["session"], Value::Null);
        assert_eq!(envelope["seq"], Value::Null);
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        assert_eq!(plaintext["t"], "session.index");
        assert_eq!(plaintext["full"], true);
        assert_eq!(plaintext["sessions"], sessions);
        server_result.expect("recording server should not panic");
    }

    #[test]
    fn live_connection_survives_unavailable_session_index_snapshot() {
        let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
            || None,
            |_| Some(Zeroizing::new([18_u8; 32])),
            || None,
        );
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (addr, release_server, server) = spawn_holding_server();
        let connected_config = GatewayConfig {
            relay_url: format!("ws://{addr}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url = build_ws_url(&connected_config.relay_url, &connected_config.room_id);
        let k_room = Zeroizing::new([18_u8; 32]);

        let result = thread::scope(|scope| {
            let connection_inner = &inner;
            let connection_url = &url;
            let connection_config = &connected_config;
            let connection_k_room = &k_room;
            let connection = scope.spawn(move || {
                run_authenticated_connection(
                    connection_inner,
                    connection_url,
                    &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                    connection_config,
                    None,
                    &upstream_rx,
                    &milestone_rx,
                    Some(connection_k_room),
                )
            });

            wait_until_connected(&inner);
            wait_until_counter_at_least(&inner.state.session_index_snapshot_unavailable, 1);
            assert_eq!(
                inner
                    .state
                    .session_index_snapshot_unavailable
                    .load(Ordering::Relaxed),
                1
            );
            release_server.send(()).unwrap();
            inner.shutdown.store(true, Ordering::Release);
            connection
                .join()
                .expect("connection thread should not panic")
        });

        assert_eq!(result, Ok(ConnectionExit::ClosedByPeer));
        server.join().expect("holding server should not panic");
    }

    #[test]
    fn sink_snapshot_generation_is_immune_to_later_connection_switch() {
        let state = GatewayInnerState::default();
        let (tx, rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);

        let generation_a = state.advance_generation_and_set_gate(true);
        assert_eq!(generation_a, 1);
        let payload = text_delta_payload("from-a");
        enqueue_batch_payload_for_upstream(&state, &tx, &milestone_tx, payload.clone());

        state.disable_upstream_gate();
        let generation_b = state.advance_generation_and_set_gate(true);
        assert_eq!(generation_b, 2);

        let (item_generation, queued_payload) = rx.try_recv().unwrap();
        assert_eq!(queued_payload, LiveQueueItem::Batch(payload));
        assert_eq!(item_generation, generation_a);
        assert_ne!(item_generation, generation_b);
        assert_eq!(state.connection_generation_snapshot(), generation_b);
        assert_ne!(item_generation, state.connection_generation_snapshot());
    }

    #[test]
    fn stale_generation_payload_is_not_replayed_into_the_next_connection() {
        let inner = test_inner(|_| None, || None);
        let (tx, rx) = mpsc::sync_channel(1);
        let k_room = Zeroizing::new([1_u8; 32]);

        let (addr_a, release_a, server_a) = spawn_holding_server();
        let config_a = GatewayConfig {
            relay_url: format!("ws://{addr_a}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url_a = build_ws_url(&config_a.relay_url, &config_a.room_id);
        let inner_a = Arc::clone(&inner);
        let k_room_a = k_room.clone();
        let connection_a = thread::spawn(move || {
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            let result = run_authenticated_connection(
                &inner_a,
                &url_a,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &config_a,
                None,
                &rx,
                &milestone_rx,
                Some(&k_room_a),
            );
            (result, rx)
        });

        wait_until_generation(&inner, 1);
        let generation_a = inner.state.connection_generation_snapshot();
        inner.shutdown.store(true, Ordering::Release);
        release_a.send(()).unwrap();
        let (result_a, rx) = connection_a
            .join()
            .expect("connection A thread should not panic");
        assert_eq!(result_a, Ok(ConnectionExit::ClosedByPeer));
        assert!(!inner.state.upstream_enabled_snapshot());
        server_a.join().expect("holding server A should not panic");

        inner.shutdown.store(false, Ordering::Release);
        tx.try_send((
            generation_a,
            LiveQueueItem::Batch(text_delta_payload("late-from-a")),
        ))
        .unwrap();

        let (addr_b, release_b, server_b) = spawn_holding_server();
        let config_b = GatewayConfig {
            relay_url: format!("ws://{addr_b}"),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        };
        let url_b = build_ws_url(&config_b.relay_url, &config_b.room_id);
        let inner_b = Arc::clone(&inner);
        let k_room_b = k_room.clone();
        let connection_b = thread::spawn(move || {
            let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
            let result = run_authenticated_connection(
                &inner_b,
                &url_b,
                &DesktopCredential::new(Zeroizing::new("ab".repeat(32))),
                &config_b,
                None,
                &rx,
                &milestone_rx,
                Some(&k_room_b),
            );
            (result, rx)
        });

        wait_until_generation(&inner, generation_a + 1);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && inner
                .state
                .upstream_stale_generation_dropped
                .load(Ordering::Relaxed)
                == 0
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            inner
                .state
                .upstream_stale_generation_dropped
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            inner.state.frames_sent.load(Ordering::Relaxed),
            2,
            "only the two mandatory connection sync frames should have been sent"
        );

        inner.shutdown.store(true, Ordering::Release);
        release_b.send(()).unwrap();
        let (result_b, rx) = connection_b
            .join()
            .expect("connection B thread should not panic");
        assert_eq!(result_b, Ok(ConnectionExit::ClosedByPeer));
        assert!(rx.try_recv().is_err());
        assert!(!inner.state.upstream_enabled_snapshot());
        server_b.join().expect("holding server B should not panic");
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
        test_inner_with_token_provider_and_active_room_resolver(
            settings,
            || None,
            active_room_resolver,
        )
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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        })
    }

    /// M2-4d：跟 `test_inner_with_active_room_resolver` 同一套默认 fixture，额外把
    /// `k_room_provider` 也换成调用方提供的实现——需要同时控制"active 房解析结果"与"这间房
    /// 有没有 K_room"两个维度的测试专用（k_room 缺失/连接失败类场景，legacy 回落撤除后不能
    /// 再靠只读 `remote_room_id` 让 `current_config` 解出配置了，必须真的把 active 房解析
    /// 通道接上）。
    fn test_inner_with_active_room_resolver_and_k_room(
        settings: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
        active_room_resolver: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
        k_room_provider: impl Fn(&str) -> Option<Zeroizing<[u8; 32]>> + Send + Sync + 'static,
    ) -> Arc<Inner> {
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
        Arc::new(Inner {
            settings: Box::new(settings),
            token_provider: Box::new(|| None),
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

    /// S1i1 H2：跟 `test_inner_with_registry_providers` 同一套 registry provider 组合，
    /// 但 `input_send_handler` 可由调用方注入——用来在丢帧回归测试里证明业务 handler
    /// 真的被调用，而不是靠固定返回 `AckOutcome::Failed` 的默认 fixture（那样分不清
    /// 「真的解密并派发」与「解密/解析就先失败了」，两条路径产出的 `input.ack` 长得一样）。
    fn test_inner_with_registry_providers_and_input_handler(
        registry_snapshot_provider: RegistrySnapshotProvider,
        registry_rebase_provider: RegistryRebaseProvider,
        input_send_handler: impl Fn(InputSendFrame) -> Option<AckOutcome> + Send + Sync + 'static,
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
            k_room_provider: Box::new(|_| Some(Zeroizing::new([9_u8; 32]))),
            session_index_snapshot_provider: Box::new(|| None),
            milestone_replay_provider: Box::new(|| None),
            session_runtime_replay_provider: Box::new(|| None),
            pair_hello_handler: Box::new(|_| None),
            pair_done_handler: Box::new(|_| PairDoneAction::Rejected),
            registry: test_registry(),
            refresh_handler: test_refresh_handler(),
            registry_snapshot_provider,
            registry_rebase_provider,
            registry_high_water_provider: test_registry_high_water_provider(),
            input_send_handler: Box::new(input_send_handler),
            input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
            control_replay_handler: Box::new(|_, _| true),
            control_stop_handler: Box::new(|_| AckOutcome::Failed),
            upstream_tx,
            milestone_tx,
            session_repo_provider: test_session_repo_provider_allowing_default_repo(),
            session_history_provider: test_session_history_provider(),
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        }))
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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        })
    }

    fn sample_gateway_config() -> GatewayConfig {
        GatewayConfig {
            relay_url: "wss://relay.example.com".to_owned(),
            room_id: "0123456789abcdef0123456789abcdef".to_owned(),
            active_repo_id: None,
        }
    }

    fn test_inner_with_claim_handlers(
        claim_client: impl Fn(&str, &str, &str) -> Result<ClaimResponse, String> + Send + Sync + 'static,
        active_device_provider: impl Fn(&str) -> Result<bool, String> + Send + Sync + 'static,
    ) -> Arc<Inner> {
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
        Arc::new(Inner {
            settings: Box::new(|_| None),
            token_provider: Box::new(|| None),
            desktop_credential_provider: test_desktop_credential_provider(),
            claim_client: Box::new(claim_client),
            active_device_provider: Box::new(active_device_provider),
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
            session_repo_provider: test_session_repo_provider(),
            session_history_provider: test_session_history_provider(),
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        })
    }

    fn test_inner_for_claim_cycle(
        relay_url: &str,
        claim_calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Arc<Inner> {
        let relay_url = relay_url.to_owned();
        let settings_relay_url = relay_url.clone();
        let (upstream_tx, _upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, _milestone_rx) = mpsc::sync_channel(1);
        Arc::new(Inner {
            settings: Box::new(move |key| match key {
                "remote_control_enabled" => Some("true".to_owned()),
                "remote_relay_url" => Some(settings_relay_url.clone()),
                "remote_active_repo_id" => Some("proj-1".to_owned()),
                _ => None,
            }),
            token_provider: Box::new(|| None),
            desktop_credential_provider: test_desktop_credential_provider(),
            claim_client: Box::new(move |_, _, _| {
                claim_calls.fetch_add(1, Ordering::Relaxed);
                Ok(ClaimResponse::Claimed)
            }),
            active_device_provider: test_active_device_provider(),
            active_room_resolver: Box::new(|_project_id| {
                Ok("0123456789abcdef0123456789abcdef".to_owned())
            }),
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

    fn test_inner_with_input_answer_handler(
        input_answer_handler: impl Fn(InputAnswerFrame) -> Option<AckOutcome> + Send + Sync + 'static,
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
            input_send_handler: Box::new(|_| Some(AckOutcome::Failed)),
            input_answer_handler: Box::new(input_answer_handler),
            control_replay_handler: Box::new(|_, _| true),
            control_stop_handler: Box::new(|_| AckOutcome::Failed),
            upstream_tx,
            milestone_tx,
            session_repo_provider: test_session_repo_provider_allowing_default_repo(),
            session_history_provider: test_session_history_provider(),
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        }))
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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        })
    }

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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        });
        (inner, milestone_rx)
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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        });
        *lock(&inner.state.active_repo_id_for_gating) =
            Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        inner.state.advance_generation_and_set_gate(true);
        (inner, milestone_rx)
    }

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
            state: GatewayInnerState::default(),
            shutdown: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            registry_publish_wake: AtomicBool::new(false),
            active_token: Mutex::new(None),
            liveness_interval: DEFAULT_LIVENESS_INTERVAL,
        });
        *lock(&inner.state.active_repo_id_for_gating) =
            Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
        inner.state.advance_generation_and_set_gate(true);
        (inner, upstream_rx)
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
        let (ct, n) =
            crate::remote_crypto::seal(k_room, &meta, &serde_json::to_vec(payload).unwrap());
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
                let value = serde_json::from_str(text.as_ref())
                    .expect("recorded upstream text must be JSON");
                frame_tx.send(value).unwrap();
            }
        });
        (addr, frame_rx, handle)
    }

    fn spawn_recording_server_after_sync(
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
            ack_initial_registry_sync(&mut socket);
            for _ in 0..expected_frames {
                let message = socket
                    .read()
                    .expect("recording server should receive a frame");
                let Message::Text(text) = message else {
                    panic!("recording server expected a text frame");
                };
                let value = serde_json::from_str(text.as_ref())
                    .expect("recorded upstream text must be JSON");
                frame_tx.send(value).unwrap();
            }
        });
        (addr, frame_rx, handle)
    }

    fn spawn_holding_server() -> (
        std::net::SocketAddr,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("holding listener should bind");
        let addr = listener
            .local_addr()
            .expect("holding listener should have an address");
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                if let Ok(mut socket) = tungstenite::accept(stream) {
                    ack_initial_registry_sync(&mut socket);
                    let _ = release_rx.recv();
                    let _ = socket.close(None);
                }
            }
        });
        (addr, release_tx, handle)
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

    /// S1ja F3: mixed-version fixture — reads the desktop's initial `token.sync` (like
    /// `ack_initial_registry_sync`) but answers with `decoy_frame` instead of the expected
    /// `token.sync.ack`, simulating an old relay that doesn't yet understand the registry
    /// sync handshake and replies with a generic protocol error.
    fn spawn_pre_ack_decoy_server(
        decoy_frame: Value,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("pre-ack decoy listener should bind");
        let addr = listener
            .local_addr()
            .expect("pre-ack decoy listener should have an address");
        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                if let Ok(mut socket) = tungstenite::accept(stream) {
                    let _initial_sync = recv_text_skip_control(&mut socket);
                    let _ = socket.send(Message::Text(decoy_frame.to_string().into()));
                    // Keep the socket open briefly so the client's read doesn't race a TCP
                    // reset instead of observing the decoy frame's WebSocketError path.
                    thread::sleep(Duration::from_millis(200));
                }
            }
        });
        (addr, handle)
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

    fn wait_until_gateway_state(
        inner: &Inner,
        expected_state: GatewayState,
        expected_stopped_reason: Option<&str>,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let status = lock(&inner.state.status);
            if status.state == expected_state
                && status.stopped_reason.as_deref() == expected_stopped_reason
            {
                return;
            }
            drop(status);
            thread::sleep(Duration::from_millis(20));
        }
        panic!("gateway did not reach expected state within two seconds");
    }

    fn wait_until_generation(inner: &Inner, expected: u64) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if inner.state.connection_generation_snapshot() == expected
                && inner.state.upstream_enabled_snapshot()
            {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("connection generation did not reach {expected} within two seconds");
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

    fn assert_panicking_attempt_enters_backoff(inner: &Arc<Inner>) {
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        let result = catch_unwind(AssertUnwindSafe(|| {
            attempt_once(inner, &upstream_rx, &milestone_rx)
        }));
        let payload = result.expect_err("callback panic must be caught around the whole attempt");
        let mut failed_attempts = 0;
        assert_eq!(
            record_failure(
                inner,
                FailureKind::Panic,
                panic_message(payload),
                None,
                &mut failed_attempts,
            ),
            0
        );

        assert_eq!(inner.state.panics.load(Ordering::Relaxed), 1);
        assert_eq!(inner.state.connection_failures.load(Ordering::Relaxed), 1);
        assert_eq!(lock(&inner.state.status).state, GatewayState::Backoff);
        assert_eq!(failed_attempts, 1);
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
    fn m2_4c_active_mode_session_index_snapshot_top_level_repo_summary_derived_from_filtered_rows()
    {
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
    fn m2_4c_active_mode_milestone_drain_filters_other_repo_session_and_sends_active_repo_session()
    {
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

    /// M24DR 返工·项 7：⑥（上面这条测试）只盖了快照（a）与逐条里程碑（b）两面，唯独漏了
    /// session.index **增量** diff 这第三面——`filter_session_index_incremental_for_active_repo`
    /// 顶部 `let active_repo_id = lock(&state.active_repo_id_for_gating).clone()?;` 这个提前
    /// 返回此前完全没有专门测试顶着。用 `renamed` op（B1 四路里唯一"正常情况下会查库"的一
    /// 路）+ 一个"被调用就 panic"的 `session_repo_provider`：active 缺失时函数必须在触碰
    /// `op` 分支之前就整条丢弃，provider 连一次都不该被摸到——如果哪天有人把这个 `?` 改成别
    /// 的什么（比如误当"没有 active 限制=放行"），这条测试要么因 provider 被调用而 panic，
    /// 要么因帧被送上线而断言失败。
    #[test]
    fn m2_4c_active_mode_session_index_incremental_is_filtered_without_active_repo() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([70_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        // 不 store active_repo_id_for_gating——保持默认 None，跟⑥同一条"理论不可达但仍要
        // fail-closed"的边界。
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_renamed_payload("sess-a", "A session renamed"),
                client_msg_id: "client-renamed-a".to_owned(),
            },
        );
        let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
            panic!(
                "without an active repo the incremental filter must short-circuit before \
                 querying session_repo_provider ({session_id})"
            )
        });

        // expected_frames=0：过滤掉意味着什么都不会写到 socket 上。
        let (addr, frames, server) = spawn_recording_server(0);
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

        assert!(
            frames.try_recv().is_err(),
            "without an active repo the session.index increment must be filtered, not delivered"
        );
        assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 1);
        drop(socket);
        server.join().unwrap();
    }

    // ---- M2-4c(B1)：session.index 增量 diff 四路分治 --------------------------------------

    /// B1「created」：payload 里现成的 `session.repo_id` 直接跟 active repo 比——不匹配的整条
    /// 丢，匹配的正常出线；`session_repo_provider` 传入即 panic，证明这一路真的是零查库。
    #[test]
    fn m2_4c_active_mode_session_index_created_uses_payload_repo_id_without_querying() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([67_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_created_payload(
                    "sess-b",
                    "B session",
                    "repo-b",
                    "ns-1",
                    None,
                ),
                client_msg_id: "client-created-b".to_owned(),
            },
        );
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_created_payload(
                    "sess-a",
                    "A session",
                    "repo-a",
                    "ns-1",
                    None,
                ),
                client_msg_id: "client-created-a".to_owned(),
            },
        );
        let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
            panic!(
                "created must be judged from the payload's own repo_id, not a query \
                 ({session_id})"
            )
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
            .expect("the active repo's created session must reach the wire");
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        assert_eq!(
            plaintext["session"]["id"], "sess-a",
            "only the active repo's created session may appear"
        );
        assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 1);
        assert!(
            frames.try_recv().is_err(),
            "the other repo's created session must not have reached the wire"
        );
        drop(socket);
        server.join().unwrap();
    }

    /// B1「renamed」：payload 只有 `{id, title}`，行还在——查 `session_repo_provider`（走同一份
    /// 连接缓存）；不属于就整条丢（**连 title 一起丢**，不是只脱敏 title）。
    #[test]
    fn m2_4c_active_mode_session_index_renamed_drops_other_repo_session_and_keeps_title() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([68_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_renamed_payload("sess-b", "B session renamed"),
                client_msg_id: "client-renamed-b".to_owned(),
            },
        );
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_renamed_payload("sess-a", "A session renamed"),
                client_msg_id: "client-renamed-a".to_owned(),
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
            .expect("the active repo's renamed session must reach the wire");
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        assert_eq!(plaintext["id"], "sess-a");
        assert_eq!(
            plaintext["title"], "A session renamed",
            "the renamed payload must still carry the title, not be stripped of it"
        );
        assert_eq!(
            state.upstream_repo_filtered.load(Ordering::Relaxed),
            1,
            "the other repo's renamed session must be blocked entirely, not pass through with \
             its title exposed"
        );
        assert!(frames.try_recv().is_err());
        drop(socket);
        server.join().unwrap();
    }

    /// B1「archived/unarchived」：payload 是 `{ids: [...]}`，行都还在——逐 id 查、重写 `ids`
    /// 数组只留 active repo 的；全部被过滤掉时整条丢（不发一个空 `ids` 出去）。
    #[test]
    fn m2_4c_active_mode_session_index_archived_rewrites_ids_to_active_repo_only() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([69_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_archived_payload(&["sess-b".to_owned()], true),
                client_msg_id: "client-archived-empty".to_owned(),
            },
        );
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_archived_payload(
                    &[
                        "sess-a".to_owned(),
                        "sess-b".to_owned(),
                        "sess-a2".to_owned(),
                    ],
                    true,
                ),
                client_msg_id: "client-archived-mixed".to_owned(),
            },
        );
        let session_repo_provider: SessionRepoProvider = Box::new(|session_id| match session_id {
            "sess-a" | "sess-a2" => Ok(Some("repo-a".to_owned())),
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

        // 全部过滤掉的那条（只含 sess-b）必须整条丢——不出线；只有部分匹配的那条会真正出线，
        // 且 ids 已经被重写成只剩 active repo 的两个 id。
        let envelope = frames
            .recv_timeout(Duration::from_secs(2))
            .expect("the partially-matching archived event must still reach the wire");
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        let ids: Vec<&str> = plaintext["ids"]
            .as_array()
            .expect("archived payload must carry an ids array")
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec!["sess-a", "sess-a2"],
            "the ids array must be rewritten to only the active repo's sessions"
        );
        assert_eq!(
            state.upstream_repo_filtered.load(Ordering::Relaxed),
            1,
            "only the fully-empty-after-filtering event counts as filtered; a rewrite is not \
             a drop"
        );
        assert!(
            frames.try_recv().is_err(),
            "the fully-filtered-out archived event must not have reached the wire"
        );
        drop(socket);
        server.join().unwrap();
    }

    /// B1「deleted」：行已经被删，查不到归属——显式放行、不查库（`session_repo_provider`
    /// 传入即 panic，证明真的没有调用）。这条是审查点名过的"错误补法"陷阱：deleted 事件若被
    /// 当成"查不到就丢"处理，手机端会永远挂着一个已经不存在的幽灵会话。
    #[test]
    fn m2_4c_active_mode_session_index_deleted_passes_through_without_querying() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([70_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: build_session_index_deleted_payload("sess-ghost"),
                client_msg_id: "client-deleted".to_owned(),
            },
        );
        let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
            panic!(
                "deleted must not consult the session repo provider — the row is already gone \
                 ({session_id})"
            )
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
            .expect("deleted events must always reach the wire, even in active mode");
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        assert_eq!(plaintext["id"], "sess-ghost");
        assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 0);
        drop(socket);
        server.join().unwrap();
    }

    /// F2：`deleted` 不查库不等于原样转发调用方给的整个 payload——如果 payload 里夹带了
    /// `id` 之外的字段（比如误把 `title` 也塞了进来），出线帧不能带着这些字段一起走。
    /// 用手写 `Value`（不走 `build_session_index_deleted_payload`）模拟"payload 形状被污染"
    /// 的场景，断言出线的只有干净的 `{op, full, id}`。
    #[test]
    fn m2_4c_active_mode_session_index_deleted_strips_smuggled_fields() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([71_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: serde_json::json!({
                    "op": "deleted",
                    "full": false,
                    "id": "sess-ghost",
                    "title": "should not leak",
                    "repo_id": "repo-b",
                }),
                client_msg_id: "client-deleted-smuggled".to_owned(),
            },
        );
        let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
            panic!(
                "deleted must not consult the session repo provider — the row is already gone \
                 ({session_id})"
            )
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
            .expect("deleted events must still reach the wire once the shape is rebuilt");
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        assert_eq!(plaintext["id"], "sess-ghost");
        assert!(
            plaintext.get("title").is_none(),
            "a smuggled title field must not survive onto the wire"
        );
        assert!(
            plaintext.get("repo_id").is_none(),
            "a smuggled repo_id field must not survive onto the wire"
        );
        let fields: std::collections::BTreeSet<&str> = plaintext
            .as_object()
            .expect("payload must be a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            ["op", "full", "id", "t"].into_iter().collect(),
            "the rebuilt payload must be exactly {{op, full, id}} plus the milestone_payload \
             t field, nothing smuggled"
        );
        drop(socket);
        server.join().unwrap();
    }

    /// F2：`id` 缺失或非字符串——`deleted` 不能盲目放行，fail-closed 整条丢。
    #[test]
    fn m2_4c_active_mode_session_index_deleted_drops_when_id_is_missing_or_not_a_string() {
        let room = "0123456789abcdef0123456789abcdef";
        let k_room = Zeroizing::new([72_u8; 32]);
        let state = GatewayInnerState::default();
        state.advance_generation_and_set_gate(true);
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
        let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: serde_json::json!({"op": "deleted", "full": false}),
                client_msg_id: "client-deleted-missing-id".to_owned(),
            },
        );
        enqueue_milestone_for_upstream(
            &state,
            &milestone_tx,
            MilestoneItem {
                session: None,
                t: "session.index".to_owned(),
                payload: serde_json::json!({"op": "deleted", "full": false, "id": 12345}),
                client_msg_id: "client-deleted-non-string-id".to_owned(),
            },
        );
        let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
            panic!(
                "a malformed deleted payload must be rejected before ever consulting the \
                 session repo provider ({session_id})"
            )
        });

        let (addr, server) = spawn_discarding_server();
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

        assert_eq!(
            state.frames_sent.load(Ordering::Relaxed),
            0,
            "a deleted event with a missing or non-string id must never reach the wire"
        );
        assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 2);
        drop(socket);
        server.join().unwrap();
    }

    // ---- M2-4c(B3)：session→repo 归属不可变假设证伪后的缓存失效 ---------------------------

    /// B3：`sessions.repo_id` 不是真正不可变的——`update_session_repo` 能在会话生命周期内把它
    /// 改绑到另一个 repo。这条测试直接钉 `upstream_session_allowed` 的缓存失效行为：改绑
    /// 发生并调用 `note_session_repo_reassignment()` 之后，同一份连接缓存不能继续吃里面那份
    /// 已经作废的旧归属，必须现查一次并得到新结果。
    #[test]
    fn m2_4c_upstream_session_repo_cache_invalidates_after_mid_connection_reassignment() {
        let state = GatewayInnerState::default();
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        // 模拟 run_connection_request 在连接建立时记的基线——不依赖这一刻全局
        // SESSION_REPO_EPOCH 恰好是什么值（同进程内其它测试可能已经推过它），测试自己起手
        // 对齐，避免因为并行测试执行顺序不同而变得不确定。
        let mut epoch_seen = SESSION_REPO_EPOCH.load(Ordering::Acquire);

        let current_repo = Arc::new(Mutex::new("repo-a".to_owned()));
        let current_repo_for_provider = Arc::clone(&current_repo);
        let query_count = Arc::new(AtomicU64::new(0));
        let query_count_for_provider = Arc::clone(&query_count);
        let session_repo_provider: SessionRepoProvider = Box::new(move |_session_id| {
            query_count_for_provider.fetch_add(1, Ordering::Relaxed);
            Ok(Some(current_repo_for_provider.lock().unwrap().clone()))
        });
        let mut cache = HashMap::new();

        assert!(
            upstream_session_allowed(
                &state,
                &session_repo_provider,
                &mut cache,
                &mut epoch_seen,
                "sess-x"
            ),
            "sess-x currently belongs to the active repo"
        );
        assert_eq!(query_count.load(Ordering::Relaxed), 1);
        assert!(
            upstream_session_allowed(
                &state,
                &session_repo_provider,
                &mut cache,
                &mut epoch_seen,
                "sess-x"
            ),
            "a second lookup within the same connection must be served from cache"
        );
        assert_eq!(
            query_count.load(Ordering::Relaxed),
            1,
            "a cache hit must not re-query the provider"
        );

        // 改绑：sess-x 被挪去 repo-b，触发计数器 +1（模拟 update_session_repo 改绑成功后的
        // note_session_repo_reassignment 调用）。
        *current_repo.lock().unwrap() = "repo-b".to_owned();
        note_session_repo_reassignment();

        assert!(
            !upstream_session_allowed(
                &state,
                &session_repo_provider,
                &mut cache,
                &mut epoch_seen,
                "sess-x"
            ),
            "after a mid-connection reassignment sess-x no longer belongs to the active repo"
        );
        assert_eq!(
            query_count.load(Ordering::Relaxed),
            2,
            "the epoch bump must force the cache to be cleared and re-queried, not keep serving \
             the stale repo-a verdict"
        );
    }

    /// F1：跟上一条测试不同——这条钉的是"改绑恰好夹在同一次 `upstream_session_allowed` 调用
    /// 内部"这个更窄的窗口（判定开始已经 load 过一次全局代号、正在做 provider 查询的过程中
    /// 才发生 bump），不是两次独立调用之间的窗口。用 `session_repo_provider` 闭包本身在
    /// **第一次**被调用时同步触发 `note_session_repo_reassignment()`，精确复现"查询进行中
    /// 代号才变"的时序，不需要额外的 `cfg(test)` seam——provider 调用天然就发生在判定开始
    /// 的第一次同步之后、判定结束前的第二次同步之前。
    #[test]
    fn m2_4c_upstream_session_repo_lookup_detects_reassignment_racing_the_lookup_itself() {
        let state = GatewayInnerState::default();
        *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
        let mut epoch_seen = SESSION_REPO_EPOCH.load(Ordering::Acquire);

        let call_count = Arc::new(AtomicU64::new(0));
        let call_count_for_provider = Arc::clone(&call_count);
        let session_repo_provider: SessionRepoProvider = Box::new(move |_session_id| {
            let call = call_count_for_provider.fetch_add(1, Ordering::Relaxed) + 1;
            if call == 1 {
                // 模拟：本次判定的第一趟 provider 查询正在进行时，另一个线程/调用刚好完成了
                // 改绑——查询本身仍然拿到改绑前的旧值（查询开始时的数据快照），但全局代号
                // 已经变了。
                note_session_repo_reassignment();
                Ok(Some("repo-a".to_owned()))
            } else {
                Ok(Some("repo-b".to_owned()))
            }
        });
        let mut cache = HashMap::new();

        let allowed = upstream_session_allowed(
            &state,
            &session_repo_provider,
            &mut cache,
            &mut epoch_seen,
            "sess-x",
        );

        assert_eq!(
            call_count.load(Ordering::Relaxed),
            2,
            "a bump landing during the first lookup must trigger exactly one retry"
        );
        assert!(
            !allowed,
            "this call must already reflect the post-reassignment repo (repo-b), not the stale \
             repo-a value the first lookup happened to return — waiting for the next call would \
             leak one command/milestone through under the old attribution"
        );
    }

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
            "../../../remote-relay/fixtures/data-plane-v1.json"
        ))
        .expect("data-plane-v1 fixture must parse as JSON")
    }

    fn history_row(message_id: i64, role: &str, text: String) -> SessionHistoryRow {
        SessionHistoryRow {
            message_id,
            role: role.to_owned(),
            content_json: serde_json::json!([{"type": "text", "text": text}]),
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
        let page = build_history_page(
            "history-tool",
            None,
            vec![SessionHistoryRow {
                message_id: 7,
                role: "assistant".to_owned(),
                content_json: serde_json::json!([
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
                ]),
            }],
        );
        assert_eq!(
            page.payload["messages"][0]["blocks"][0]["text"],
            "keep verbatim"
        );
        assert_eq!(
            page.payload["messages"][0]["blocks"][1]["output"]
                .as_str()
                .unwrap()
                .len(),
            OUTPUT_TRUNCATE_BYTES
        );
    }

    #[test]
    fn history_oversized_single_message_is_dropped_counted_and_acknowledged() {
        let provider_calls = Arc::new(AtomicU64::new(0));
        let calls = Arc::clone(&provider_calls);
        let (inner, upstream_rx) = test_inner_for_history(
            |_| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())),
            move |session, before, max_rows| {
                assert_eq!(session, "history-oversized");
                assert_eq!(before, None);
                assert_eq!(max_rows, (HISTORY_PAGE_MAX_ROWS + 1) as i64);
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(vec![history_row(
                    9,
                    "assistant",
                    "z".repeat(HISTORY_SEND_BUDGET_BYTES + 1024),
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
        assert_eq!(item.payload["messages"], serde_json::json!([]));
        assert_eq!(item.payload["next_before"], Value::Null);
        assert_eq!(
            item.client_msg_id,
            derive_client_msg_id("history|history-oversized|cmd-history-oversized|latest")
        );
    }

    #[test]
    fn history_full_oversized_window_advances_until_an_older_message_is_reachable() {
        let provider_calls = Arc::new(AtomicU64::new(0));
        let calls = Arc::clone(&provider_calls);
        let (inner, upstream_rx) = test_inner_for_history(
            |_| Ok(Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned())),
            move |session, before, max_rows| {
                assert_eq!(session, "history-many-oversized");
                assert_eq!(max_rows, (HISTORY_PAGE_MAX_ROWS + 1) as i64);
                calls.fetch_add(1, Ordering::Relaxed);
                match before {
                    None => {
                        let mut rows: Vec<_> = (2..=51)
                            .rev()
                            .map(|id| {
                                history_row(
                                    id,
                                    "assistant",
                                    "z".repeat(HISTORY_SEND_BUDGET_BYTES + 1024),
                                )
                            })
                            .collect();
                        rows.push(history_row(1, "user", "reachable".to_owned()));
                        Ok(rows)
                    }
                    Some(2) => Ok(vec![history_row(1, "user", "reachable".to_owned())]),
                    other => panic!("unexpected internal history cursor: {other:?}"),
                }
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
        assert_eq!(provider_calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            inner
                .state
                .history_oversized_dropped
                .load(Ordering::Relaxed),
            HISTORY_PAGE_MAX_ROWS as u64
        );
        let (_, LiveQueueItem::Prebuilt(item)) = upstream_rx.try_recv().unwrap() else {
            panic!("history response must use the prebuilt live queue path");
        };
        assert_eq!(item.payload["before_message_id"], Value::Null);
        assert_eq!(item.payload["messages"].as_array().unwrap().len(), 1);
        assert_eq!(item.payload["messages"][0]["message_id"], 1);
        assert_eq!(
            item.payload["messages"][0]["blocks"][0]["text"],
            "reachable"
        );
        assert_eq!(item.payload["next_before"], Value::Null);
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
            include_bytes!("../../../remote-relay/fixtures/history-v1.json")
        );
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
        let payload = milestone_payload(
            "msg.completed",
            build_msg_completed_payload(101, "assistant", blocks_json, Some("Claude")),
        );
        assert_eq!(
            payload,
            data_plane_v1_case(&fixture, "msg_completed")["frame"]
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
    fn data_plane_v1_control_snapshot_request_cases_match_fixture_and_drive_handle_command_envelope(
    ) {
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

    // ----------------------------------------------------------------------------------------
    // P0-b 返工②：snapshot payload 尺寸预算——`build_snapshot_payload` 单点收敛的行为测试。
    // ----------------------------------------------------------------------------------------

    #[test]
    fn build_snapshot_payload_shrinks_oversized_blocks_and_truncates_tool_output() {
        // 一条远超 OUTPUT_TRUNCATE_BYTES 的工具输出 + 一串长叙述块，逼真帧越过
        // SNAPSHOT_PAYLOAD_BUDGET_BYTES（32KiB）。工具块放最后（最新），验证它在丢老块的过程
        // 中存活并被截到 OUTPUT_TRUNCATE_BYTES。
        let filler_text = "z".repeat(6 * 1024);
        let mut blocks: Vec<crate::db::Block> = (0..8)
            .map(|i| crate::db::Block::Text {
                text: format!("{filler_text}-{i}"),
            })
            .collect();
        let oversized_output = "y".repeat(OUTPUT_TRUNCATE_BYTES * 4);
        blocks.push(crate::db::Block::Tool {
            id: "tool-oversized".to_owned(),
            tool: "Bash".to_owned(),
            summary: "big output".to_owned(),
            card: crate::db::BlockCardKind::Command,
            status: crate::db::BlockToolStatus::Ok,
            exit_code: Some(0),
            output: Some(oversized_output),
        });

        let payload = build_snapshot_payload("s-1", Some(("run-x", 99)), &blocks);
        let frame_bytes = serde_json::to_string(&payload).unwrap().len();
        assert!(
            frame_bytes < SNAPSHOT_PAYLOAD_BUDGET_BYTES,
            "收敛后帧必须落回预算内，实际 {frame_bytes} 字节"
        );

        let partial_blocks = payload["partial_msg"]["blocks"].as_array().unwrap();
        assert_eq!(
            partial_blocks[0]["type"], "text",
            "截断提示块必须在 blocks 首位"
        );
        assert!(
            partial_blocks[0]["text"].as_str().unwrap().contains("截断"),
            "首块必须是截断提示文案"
        );

        let tool_block = partial_blocks
            .iter()
            .find(|block| block["type"] == "tool")
            .expect("最新的工具块必须在丢老块过程中存活");
        assert_eq!(
            tool_block["output"].as_str().unwrap().len(),
            OUTPUT_TRUNCATE_BYTES,
            "工具输出必须被截到 OUTPUT_TRUNCATE_BYTES"
        );
    }

    #[test]
    fn build_snapshot_payload_leaves_small_blocks_untouched() {
        let blocks = vec![crate::db::Block::Text {
            text: "small enough".to_owned(),
        }];
        let payload = build_snapshot_payload("s-1", Some(("run-x", 3)), &blocks);
        assert_eq!(
            payload["partial_msg"]["blocks"],
            serde_json::json!([{ "type": "text", "text": "small enough" }]),
            "预算内的 blocks 不应被截断提示块污染"
        );
    }

    // ----------------------------------------------------------------------------------------
    // P0-b 微返工第 3 轮：尺寸收敛数学闭合边界测试——审查抓出「32KiB 收敛不闭合」+「60KiB 兜
    // 底量错对象」两处后补的边界回归，成品尺寸一律按 `milestone_payload` 合并 `t` 之后的完
    // 整明文帧计量（跟真正下行的帧一致）。
    // ----------------------------------------------------------------------------------------

    /// 单个 ~32KiB text 块：裸 blocks 数组本身就已经贴着预算线，`t` 字段一旦补上去（旧实现在
    /// 预算判断之后才算）就会撑破 32KiB——收敛后的成品（含 `t`）必须仍 ≤
    /// `SNAPSHOT_PAYLOAD_BUDGET_BYTES`。文本长度（32,700 字节）刻意不取整 32KiB，是为了精确
    /// 落在"需要走截断路径、但业务块单独放不下"的敏感区间——够大以至于单块直接通过（不截断）
    /// 不成立，但如果截断提示块的开销不先占预算（旧 bug (b)）、这块反而会被误判"装得下"，
    /// 实际拼上提示块后的成品会超预算（实测超出约 140 字节）；这个尺寸下真跑一遍两种实现的
    /// 差异是本测试要盯住的东西，不是"32KiB"这个数字本身的字面意义。
    #[test]
    fn build_snapshot_payload_single_32kib_block_stays_within_budget_including_t() {
        let blocks = vec![crate::db::Block::Text {
            text: "a".repeat(32_700),
        }];
        let payload = build_snapshot_payload("s-1", Some(("run-x", 7)), &blocks);
        let full_frame = milestone_payload("snapshot", payload);
        let frame_bytes = serde_json::to_string(&full_frame).unwrap().len();
        assert!(
            frame_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES,
            "含 t 的成品必须 ≤ {SNAPSHOT_PAYLOAD_BUDGET_BYTES} 字节，实际 {frame_bytes}"
        );
        let blocks_out = full_frame["partial_msg"]["blocks"].as_array().unwrap();
        assert_eq!(
            blocks_out.len(),
            1,
            "预算判断必须把截断提示块自身的开销先算进去——这块业务内容在这个尺寸下必须被\
             整块丢尽，只剩截断提示块"
        );
        assert!(
            blocks_out[0]["text"].as_str().unwrap().contains("截断"),
            "唯一剩下的块必须是截断提示文案"
        );
    }

    /// 全部业务块都超限（每块单独就已经装不进预算剩余空间）时，允许把业务块丢尽——成品只剩
    /// 截断提示块 + 水印字段，且仍落在预算内（旧实现的 `bounded.len() > 1` 循环守卫永远保留
    /// 最后一块，这里验证它已被移除）。
    #[test]
    fn build_snapshot_payload_drops_all_business_blocks_when_none_fit() {
        let blocks: Vec<crate::db::Block> = (0..5)
            .map(|i| crate::db::Block::Text {
                text: format!("{}-{i}", "b".repeat(50 * 1024)),
            })
            .collect();
        let payload = build_snapshot_payload("s-1", Some(("run-x", 42)), &blocks);
        let full_frame = milestone_payload("snapshot", payload);
        let frame_bytes = serde_json::to_string(&full_frame).unwrap().len();
        assert!(
            frame_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES,
            "仅剩提示块的成品也必须 ≤ {SNAPSHOT_PAYLOAD_BUDGET_BYTES} 字节，实际 {frame_bytes}"
        );

        let blocks_out = full_frame["partial_msg"]["blocks"].as_array().unwrap();
        assert_eq!(
            blocks_out.len(),
            1,
            "全部业务块超限时必须丢尽，只剩截断提示块一条"
        );
        assert!(
            blocks_out[0]["text"].as_str().unwrap().contains("截断"),
            "唯一剩下的块必须是截断提示文案"
        );
    }

    /// P0-b 微返工第 4 轮：给 `shrink_snapshot_blocks_to_budget` 里"仅提示块基线也超预算"
    /// 分支不可达的论证做实证——`session` 卡在 `SESSION_ID_MAX_BYTES` 上限（128 字节，仍
    /// 合法，守卫只挡 >128）、`through_run_seq` 顶到 `u64::MAX`（20 位十进制），加上严重超
    /// 预算的 blocks，收敛仍必须正常完成且成品（含 `t`）落在 `SNAPSHOT_PAYLOAD_BUDGET_BYTES`
    /// （32,768B）预算内——把「最小必需帧有界」这条论证测出来，不是只靠注释里的算例自证。
    #[test]
    fn build_snapshot_payload_converges_within_budget_at_max_legal_session_length() {
        let session = "s".repeat(SESSION_ID_MAX_BYTES);
        let blocks: Vec<crate::db::Block> = (0..5)
            .map(|i| crate::db::Block::Text {
                text: format!("{}-{i}", "b".repeat(50 * 1024)),
            })
            .collect();
        let payload = build_snapshot_payload(
            &session,
            Some(("run-max-legal-session-len", u64::MAX)),
            &blocks,
        );
        let full_frame = milestone_payload("snapshot", payload);
        let frame_bytes = serde_json::to_string(&full_frame).unwrap().len();
        assert!(
            frame_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES,
            "session 恰好合法(128B)+超长 blocks 仍必须正常收敛到 {SNAPSHOT_PAYLOAD_BUDGET_BYTES} \
             字节预算内，实际 {frame_bytes}"
        );

        let blocks_out = full_frame["partial_msg"]["blocks"].as_array().unwrap();
        assert!(
            !blocks_out.is_empty(),
            "收敛必须正常产出成品（哪怕只剩截断提示块），不能因 session 变长就整体失败"
        );
    }

    /// P0-b 微返工第 3 轮：发送侧兜底阈值必须是按信封膨胀折算过的明文预算（44KiB），不是
    /// 直接照抄 relay 64KB 硬闸的裸 payload 比较（旧值 60KiB 就是这个量错——审查算例
    /// 51,395B 明文 payload 实测 wire 帧 ≈68,792B，已经撞 relay 65,536B 硬闸）。
    #[test]
    fn snapshot_send_budget_is_folded_for_envelope_inflation_not_raw_relay_cap() {
        assert_eq!(
            SNAPSHOT_SEND_BUDGET_BYTES,
            44 * 1024,
            "发送侧兜底阈值必须是折算后的 44KiB 明文预算——回到 60KiB 会让加密/base64 膨胀后的\
             wire 帧撞上 relay 64KB 硬闸"
        );
    }
}
