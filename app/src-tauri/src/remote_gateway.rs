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
/// 缺口②：文案带被淘汰块数（"(N 块折叠)"），拼接见 `snapshot_truncated_notice_text`。
const SNAPSHOT_TRUNCATED_NOTICE: &str = "（快照已截断，仅含最近内容）";
/// msgfix1 T3（设计稿 §A）：超预算 `msg.completed`/history row 降级为 preview 时，text 块保留
/// 的首段字节数——与 content_ref 的 `total_bytes` 同口径按 UTF-8 字节数量，不是字符数。
const OVERSIZED_PREVIEW_TEXT_HEAD_BYTES: usize = 512;
/// msgfix1 T3（设计稿 §A）：preview 截断提示，追加在保留的首段文本之后（与
/// `remote-relay/fixtures/data-plane-v1.json` 里 `msg_completed_with_content_ref`
/// 样张的 `blocks[0].text` 尾部逐字节一致——msgfix1 T7 B5：pending 版已随 T6 合入正式文件并
/// 删除，改指正式文件）。
const OVERSIZED_PREVIEW_TRUNCATION_NOTICE: &str = "内容较长，已截断——点击加载全文查看完整报告。";
/// msgfix1 T3：`build_msg_completed_payload` 把预计算好的 content_ref 临时挂在这个私有键下；
/// `enqueue_milestone_item` 在测量/发送前必须无条件 `remove` 掉——不管消息最终是否超预算，这
/// 个键都绝不能流到 wire（非超预算消息本就不该带 `content_ref`，§10.6「可选字段」）。之所以
/// 不直接把 content_ref 摆进 payload 顶层：`content_ref` 只有在真正降级为 preview 时才附加，
/// 而 revision/原始 content 字节在构造 payload 的那一刻就已具备，这个私有键是两个时刻之间唯一
/// 的搬运方式（`MilestoneItem` 是本文件内 33+ 处构造的通用结构体，不为这一种帧型单独加字段）。
const MSG_COMPLETED_REF_SOURCE_KEY: &str = "__msgfix1_content_ref_source";
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

/// M0 §10.9：`msg.fetch` 目标消息 `total_bytes` 上限——超过直接回 `too_large`，不进入分片流程。
const MSG_FETCH_TOTAL_BYTES_LIMIT: usize = 4 * 1024 * 1024;

/// msgfix1 T4（M0 §10.8，机制钉死·数值由本刀生成式测量定）：`msg.chunk` 每片裸内容字节数
/// （切片前的原始字节，不是 base64 后的长度）。
///
/// **测量方法**（见 `tests::msg_chunk_raw_bytes_worst_case_wire_frame_stays_under_relay_limit_
/// with_margin`，真实走 `remote_crypto::seal` + `build_envelope_json`，不是手算）：构造整条
/// wire 帧的最坏情形——`bytes_b64` 来自最坏转义原始字节（高位不可打印字节与 `"` 交替，排除
/// "巧合被 base64 表友好对待"的侥幸）、`message_id`/`revision`/`epoch`/`ts` 取各自类型的最大值、
/// `content_sha256` 固定 64 hex（sha256 的真实长度，不是"尽量长"）、`total_bytes`/`offset` 取
/// 4MiB 上限（`MSG_FETCH_TOTAL_BYTES_LIMIT`，本类型字段在通过校验链后不可能再大）、
/// `room`/`session`/`command_id` 取各自协议允许的最长值——序列化整条明文 payload、真实
/// AES-256-GCM 加密（密文 = 明文 + 16B tag，不是估算）、base64、拼进完整 wire envelope JSON，
/// 量出最终字节数。
///
/// 候选值实测（wire_len / margin，relay 64KiB=65536 硬闸，要求 margin ≥ 10%=6553）：
/// 16384→29914(35622) / 20480→37194(28342) / **24576→44474(21062)** / 28672→51762(13774) /
/// 30000→54118(11418) / 32768→59042(6494，margin 已低于 10% 门槛)。双重 base64
/// （`bytes_b64` 一层 + AEAD 密文再 base64 成 `ct` 一层）实测膨胀系数 ≈1.778×，与设计稿 v3
/// §B「40KiB 经双重 base64 ≈73KiB 必撞 64KiB」的量级判断吻合（1.778×40960≈72827）。
///
/// 选 **24576（24KiB）**——落在设计稿 v3 §B 与本任务书都预估的 "~20-24KiB" 区间正中，
/// margin 21062B（占硬闸 32%，远超 10% 门槛的 6553B），给未来信封字段增长/密钥材料变化留足
/// 冗余，同时 4MiB 消息只需约 171 片（4194304 / 24576 ≈ 170.7）——分片数与
/// `REPLY_QUEUE_CAPACITY` 的关系见该常量文档。
const CHUNK_RAW_BYTES: usize = 24 * 1024;

/// msgfix1 T4：`reply` 独立有界队列容量（M0 §10.9「满→回 busy 终态、不静默」）。单飞行闸
/// （`msg_fetch_inflight`，见 `GatewayInnerState` doc）保证同一时刻只有一个 session 的 chunk
/// 序列在往这条队列里塞；一次满额 4MiB fetch 按 `CHUNK_RAW_BYTES`（24KiB）切片产生
/// ⌈4194304/24576⌉=171 片。容量取 256——覆盖单次满额 fetch 的全部分片并留约 50% 冗余（应对
/// "旧 fetch 超时释放单飞行槽位但其分片仍未排空、新 fetch 紧接着开始入队"这类双重饱和边缘
/// 情形，见 `handle_msg_fetch` 里 chunk 入队失败后改发 `busy` 的兜底路径），不需要为多 session
/// 并发预留更多（单飞行闸已经排除了并发）。
const REPLY_QUEUE_CAPACITY: usize = 256;

/// msgfix1 T4（M0 §10.9 单飞行 + 超时释放）：一次 `msg.fetch` 在途最长存活时间——超过后单飞行
/// 闸判定该占用已释放，允许同 session 的新请求进来（"超时即终止该次拉取并释放占用"）。量级
/// 推导：一次满额 4MiB fetch（171 片）按 `MAX_DRAIN_ITEMS_PER_ROUND`（64 片/轮）需要 ≥3 轮
/// `drain_reply_queue`，每轮最迟卡在 `READ_TIMEOUT`（500ms，读循环没有新帧到达时的最长阻塞）
/// 才会被驱动到——最坏情形（对端完全安静、只靠读超时推进）≈3×500ms=1.5s；30s 留了一个数量级
/// 以上的余量给真实网络往返/relay 排队，同时不会让一个真正卡死的连接把单飞行槽位锁死太久。
const MSG_FETCH_INFLIGHT_TIMEOUT_MS: u64 = 30_000;

/// msgfix1 T4（M0 §10.9 滥用闸，桌面侧独立第二层）；msgfix1 T7 B1（opus 整盘审 P1-2
/// 后半）改口径：**gateway 全局聚合**的 60 秒滑动窗口字节预算，不再按 session 分桶——relay
/// 自己在 §9.8/§10.3 是 per-subject（单连接维度）字节计费，桌面若仍按 session 分桶，同一部
/// 桌面下的多个 session 各自领一份 8MiB/60s，叠加起来能远超 relay 那边单连接 16MiB 的固定窗
/// （`REPLY_BYTE_BUDGET_LIMIT_BYTES`，见 `remote-relay/src/room-do.js`），等于桌面这层闸形同
/// 虚设。改成单连接总量记账后：8MiB/60s 上限 × 双重 base64 膨胀系数 ≈1.81×（同
/// `CHUNK_RAW_BYTES` doc 量出的 wire 膨胀）≈14.5MiB，仍落在 relay 16MiB 桶之内，多 session
/// 叠加不再越桶。量级对照单次 `msg.fetch` 上限 `MSG_FETCH_TOTAL_BYTES_LIMIT`（4MiB）：8MiB/60s
/// 允许 60 秒内（跨全部 session 合计）接受两次满额拉取（含一次合理重试/续传），第三次起判
/// `busy`。计量口径按**接受时刻的 `total_bytes`**（消息全量大小，不是实际切出的分片字节数）
/// 计入预算——单飞行闸只保证单个 session 内同一时刻至多一个 fetch 在计费，多 session 之间仍可
/// 并发接受，这正是本条要堵的叠加口子。
const MSG_FETCH_BYTE_BUDGET_PER_WINDOW: u64 = 8 * 1024 * 1024;
const MSG_FETCH_BYTE_BUDGET_WINDOW_MS: u64 = 60_000;

/// msgfix1 T4 返修②（skeptic 补审）：`msg_fetch_command_ledger` 的容量——见
/// `MsgFetchCommandLedger` doc。量级：单飞行闸决定同一 session 任意时刻至多 1 条在途 fetch，
/// 一次典型会话在 `MSG_FETCH_INFLIGHT_TIMEOUT_MS`（30s）的时间尺度上不太可能提交远超几十个
/// 不同 command_id 的 fetch 请求；512 覆盖全部当前活跃 session 的正常换页/重试流量并留出充足
/// 冗余，同时对内存是可忽略的量（每条记录两个短字符串）。容量满时 FIFO 淘汰最老一条——这只是
/// 让"很久以前用过的 command_id"重新变得可提交，不是安全边界（真正的安全边界是
/// `MsgFetchInflightEntry.generation`，不依赖这张表的完整性）。
const MSG_FETCH_COMMAND_LEDGER_CAPACITY: usize = 512;

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
    /// msgfix1 T3（M0 §10.6）：`content_json` 之外原样保留的原始 DB `content` 字符串——
    /// content_ref 的 sha256/total_bytes 必须对这份原文字节计算，不能用重新序列化过的
    /// `content_json`（`Value` 内部 `Map` 默认按 key 排序，字节不保证与原文相同）。
    pub content_raw: String,
    /// msgfix1 T3（M0 §10.7）：该消息当前的 `messages.revision`。
    pub revision: i64,
}
/// `control.history` 的短锁 DB provider：结果保持 `message_id DESC`，组页层负责预算收敛与
/// wire 所需的升序反转。错误必须显式返回，让命令回 failed，不发送半页。
pub(crate) type SessionHistoryProvider =
    Box<dyn Fn(&str, Option<i64>, i64) -> Result<Vec<SessionHistoryRow>, String> + Send + Sync>;

/// msgfix1 T4（M0 §10.9 联合授权闸）：`msg.fetch` 校验链第①步的 DB provider——按精确
/// `(session, message_id)` 查询单条消息用于全文拉取。三态镜像 `db::MessageForFetch`（生产实现
/// 见 lib.rs `remote_gateway_message_fetch_provider` 包一层 `db::get_message_for_fetch`），这里
/// 单独定义一份而不是直接引用 db.rs 的类型——同 `SessionHistoryRow`/`SessionRepoProvider` 既有
/// 惯例，让 `remote_gateway.rs` 的测试不依赖 db.rs 也能构造任意三态。`Err` = 查询本身失败（DB
/// 错误），调用方 fail-closed 处理（同 `SessionRepoProvider` 既有姿势）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MessageForFetchResult {
    /// 消息确属该 session；`content_raw` 是原始 DB `content` 字符串（`content_ref.content_sha256`
    /// /`msg.chunk` 切片必须按这份原文字节计算，不能用任何反序列化/重序列化后的值）；
    /// `session_deleted` = 其所属 session 是否已软删。
    Found {
        content_raw: String,
        revision: i64,
        session_deleted: bool,
    },
    /// `message_id` 存在，但不属于调用方声称的 `session`（越权）。
    WrongSession,
    /// `message_id` 完全不存在。
    NotFound,
}

pub(crate) type MessageFetchProvider =
    Box<dyn Fn(&str, i64) -> Result<MessageForFetchResult, String> + Send + Sync>;
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
    /// msgfix1 T4：`msg.fetch` 校验链第①步用——见 `MessageFetchProvider` doc。
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
    /// msgfix1 T3：工具 output 截断后，单条 history 消息仍超过发送预算——**不再整条丢弃**，
    /// 降级为块级 preview + content_ref（设计稿 §A）。该计数器语义随之从"丢弃条数"改为"降级
    /// 为 preview 的条数"（极端兜底——连 preview+ref 单条页都装不下——仍如实丢弃，同样计入）。
    history_oversized_dropped: AtomicU64,
    /// msgfix1 T3：`msg.completed` 工具 output 截断后，完整明文帧仍超过发送预算——**不再
    /// 静默丢弃**，降级为块级 preview + content_ref（设计稿 §A）。连接回放与 live 发布共用
    /// 同一入队闸和同一计数；语义同上改为"降级为 preview 的条数"（无 content_ref 来源的防御性
    /// 兜底分支仍保留旧的丢弃语义，同样计入本计数器）。
    replay_oversized_dropped: AtomicU64,
    /// idlefix-T1 补针 C（TOCTOU）：`publish_run_status_replay_rows`（连接后补发批读 DB + 逐行
    /// 入队 `run.status` 现状帧）与 `enqueue_run_status_milestone_with_gate`（真实运行时
    /// `publish_run_status_milestone` 的实时入队路径）共享这把锁——保证"该连接的 run.status
    /// 现状补发帧必须先于其后任何实时 run.status 帧入队"这一顺序不变量：补发批持锁跨越整个
    /// "读 DB + 入队"过程，期间任何真实状态翻转要么在读之前已落库（读到的就是新值，天然一致），
    /// 要么必须等补发批放锁后才能把新状态入队（必然排在补发帧之后，不会被陈旧帧倒灌覆盖）。
    run_status_replay_gate: Mutex<()>,
    /// msgfix1 T4（M0 §10.9「reply 独立有界队列」）：`msg.chunk`/`msg.fetch.error` 出帧专属队列
    /// ——与 event/live（`upstream_tx`/`milestone_tx`）完全独立，互不挤占容量。`drain_reply_
    /// queue` 每轮连接主循环先于 milestone/live 排空这里（见 `drain_upstream_with_budget`）。
    /// 之所以挂在 `GatewayInnerState`（一个 `Mutex<VecDeque<_>>`）而不是像 `upstream_tx`/
    /// `milestone_tx` 那样另开一对 `mpsc::sync_channel` 挂在 `Inner` 上：`GatewayInnerState`
    /// 全仓 60+ 处构造都走 `GatewayInnerState::default()`（只有 1 处 `Default` 实现），新增字段
    /// 零改动这些调用点；而 `Inner` 的新增字段需要同步改 ~20 处结构体字面量 + 把新 `Receiver`
    /// 一路穿 `connect_loop`/`connect_loop_with`/`attempt_once`/`run_connection_request` 的签名
    /// 和它们各自的测试闭包——量级差一个数量级，选前者。
    reply_queue: Mutex<VecDeque<ReplyQueueItem>>,
    /// 诊断：`reply_queue` 已满且连兜底的 `msg.fetch.error{busy}` 本身也塞不进去时的丢弃计数
    /// （双重饱和的极端情形，见 `handle_msg_fetch` 里 chunk 入队失败后的兜底分支）。
    reply_queue_dropped: AtomicU64,
    /// msgfix1 T4 返修③（skeptic 补审·对照 `upstream_stale_generation_dropped`）：出队时二次
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
    /// msgfix1 T4（M0 §10.9 单飞行闸）：session → 在途 fetch 记账。任务书 §3④明确按 **session**
    /// 维度（比 §10.9 条文字面的 `(session, message_id)` 更严——同一 session 同时只服务一条
    /// fetch，无论 message_id 是否相同），键为 session id。条目在 `drain_reply_queue` 真正送出
    /// 该 fetch 最后一帧时清除，或被 `MSG_FETCH_INFLIGHT_TIMEOUT_MS` 超时后新请求接管。
    /// 返修②（skeptic 补审）：**不再兼作"command_id 账本"**——`MsgFetchInflightEntry` 只保留
    /// 当前占用者的身份判定所需信息（含 `generation`），command_id 的"近期已终态、拒绝复用"
    /// 语义搬去独立的 `msg_fetch_command_ledger`（原设计里两者共用一张表，会在同 command_id
    /// 复用时把"新占用者的身份"和"旧 command_id 是否用过"这两个不同问题绑在同一条记录上）。
    msg_fetch_inflight: Mutex<HashMap<String, MsgFetchInflightEntry>>,
    /// msgfix1 T4：桌面侧 60 秒滑动窗口字节预算——第二层防滥用闸（relay 已有 §9.8/§10.3 的
    /// per-subject 字节计费；这层独立生效，不依赖 relay 是否正确执行）。msgfix1 T7 B1 改口径为
    /// **gateway 全局聚合**（不再按 session 分桶）——见 `MSG_FETCH_BYTE_BUDGET_PER_WINDOW`
    /// 定义处的量级推导。
    msg_fetch_byte_budget: Mutex<VecDeque<(u64, usize)>>,
    /// msgfix1 T4 返修②（skeptic 补审）：`msg.fetch` 每次被 `handle_msg_fetch_at` 接受处理
    /// （无论最终成功还是走某个 error code）就从这里领一个全局单调递增的新值，写进
    /// `MsgFetchInflightEntry.generation`/`ReplyQueueItem.generation`。见
    /// `clear_msg_fetch_inflight_if_matches` doc——generation 保证"同一 session 先后两次接受
    /// （哪怕 command_id 相同）绝不会被彼此的残片/终片误伤"，是 command_id 复用防线的结构性
    /// 兜底（`msg_fetch_command_ledger` 是行为兜底，容量满了会被淘汰失效；generation 判定
    /// 不依赖容量、恒正确）。
    msg_fetch_generation_counter: AtomicU64,
    /// msgfix1 T4 返修②（skeptic 补审）：`(session, command_id)` 终态账本——见
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

/// msgfix1 T4：`GatewayInnerState::msg_fetch_inflight` 单条记账。返修②（skeptic 补审）新增
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

/// msgfix1 T4（M0 §10.9）：`reply` 独立有界队列的一条待发条目——`drain_reply_queue` 逐条取出、
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
    /// msgfix1 T4 返修②（skeptic 补审）：这条 reply 条目所属的那次 `msg.fetch` 接受的
    /// generation——`final_frame` 帧真正发出时用它（不是 `command_id`）去匹配/清除
    /// `msg_fetch_inflight`，见 `clear_msg_fetch_inflight_if_matches` doc。同时也是
    /// `purge_stale_reply_queue_generation` 精确清除"被新请求接管前那次接受"残片的判据。
    generation: u64,
    /// msgfix1 T4 返修③（skeptic 补审）：入队那一刻的 `connection_generation`
    /// （`GatewayInnerState::connection_generation_snapshot`）——`drain_reply_queue` 出队时
    /// 与当前连接的 generation 比对，跨连接的残片（断线重连后仍在队列里的旧数据）判过期丢弃，
    /// 对照 milestone/live 既有的 `(u64, Item)` generation 标记同一套机制。
    connection_generation: u64,
}

/// msgfix1 T4 返修②（skeptic 补审·M0 §10.9「command_id 账本」）：`(session, command_id)`
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
    message_fetch_provider: MessageFetchProvider,
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
        message_fetch_provider,
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
        let client_msg_id =
            derive_msg_completed_client_msg_id(&row.session_id, &row.dedup_key, row.revision);
        // 显示当前 agent（MA1）已知 gap：`db::MilestoneReplayRow` 尚不携带
        // `agent_name_snapshot`（另立单），补发路径这里暂传 `None`——首发（live）
        // msg.completed 帧会带 agent，重连补发的同一条消息暂不带，与 fixture
        // coverage 里 msg.completed 条目的 gap 说明保持一致，不是遗漏。
        let payload = build_msg_completed_payload(
            row.message_id,
            &row.role,
            row.content_json.clone(),
            None,
            row.revision,
            &row.content,
        );
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

    // msgfix1 T4：`reply` 排在 milestone/live 之前——`msg.fetch` 是远端主动按需拉取，理应比
    // 背景里程碑/live 广播更快送达；且 `reply_queue` 与另外两条队列完全独立（不同的
    // `Mutex<VecDeque<_>>`），排在前面不会让 milestone/live 挨饿（各自预算互不借用）。
    if drain_reply_queue(
        socket,
        state,
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

/// msgfix1 T4（M0 §10.9「reply 独立有界队列」）+ 返修③（skeptic 补审）：排空
/// `state.reply_queue`，逐条 seal 成 `reply` kind 信封发出（见 `send_upstream_value`）。
///
/// **出队时二次归属复核**（返修③）：`reply_queue` 是持久队列，一条分片从入队到真正出队之间
/// 可能跨越"断线重连"（`connection_generation` 变了）或"用户切换 active repo"（原来放行的
/// session 现在不再属于 active repo）——入队时（`handle_msg_fetch_at` 步骤①）过的那次闸只
/// 保证"入队那一刻合法"，不保证"出队那一刻仍然合法"。对照 `drain_milestone_queue`/
/// `drain_live_queue` 既有的"入队时校验、出队时复核"两段式模式：这里同样复核
/// `connection_generation` 未变 + `upstream_session_allowed`（session 仍属 active repo），
/// 不过闸的残片直接丢弃（不发出、不触碰 socket），分别计入
/// `reply_stale_connection_dropped`/`reply_repo_filtered_dropped`（不复用
/// `upstream_stale_generation_dropped`/`upstream_repo_filtered`——那两个计数器的既有文档明确
/// 只描述"上行里程碑 + live 事件"，为 reply 单开一对避免混淆诊断来源）。
///
/// 真正**发出**一帧 `final_frame == true` 的条目后才清 `msg_fetch_inflight` 对应 session 的
/// 占用（见 `ReplyQueueItem::final_frame` doc）——这一步同时是"发帧"与"释放单飞行槽位"的唯一
/// 入口。返修②：清除现在按 `generation`（而不是 `command_id`）匹配——见
/// `clear_msg_fetch_inflight_if_matches` doc，防止 command_id 复用场景下旧终片误清新占用。
/// 不管发送是否因 `k_room` 缺失、二次归属复核不过闸而被丢弃都会尝试释放（连接没有可用密钥/
/// session 不再属于 active repo 时，继续占着单飞行槽位没有额外保护意义，同 `upstream_dropped`/
/// `milestone_dropped` 在 `k_room` 缺失时的既有姿势）。
#[allow(clippy::too_many_arguments)]
fn drain_reply_queue(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    state: &GatewayInnerState,
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
        let Some(item) = lock(&state.reply_queue).pop_front() else {
            break;
        };
        *drained_items += 1;
        let ReplyQueueItem {
            session,
            command_id,
            payload,
            final_frame,
            generation,
            connection_generation: enqueued_connection_generation,
        } = item;
        let session_for_inflight = session.clone();

        let stale_connection = enqueued_connection_generation != connection_generation;
        let repo_denied = !stale_connection
            && session.as_deref().is_some_and(|session_id| {
                !upstream_session_allowed(
                    state,
                    session_repo_provider,
                    session_repo_cache,
                    session_repo_epoch_seen,
                    session_id,
                )
            });
        if stale_connection {
            state
                .reply_stale_connection_dropped
                .fetch_add(1, Ordering::Relaxed);
        } else if repo_denied {
            state
                .reply_repo_filtered_dropped
                .fetch_add(1, Ordering::Relaxed);
        } else if let Some(k_room) = k_room {
            send_upstream_value(
                socket,
                state,
                k_room,
                room,
                "reply",
                session,
                payload,
                None,
                Some(&command_id),
            )?;
        } else {
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
        }
        if final_frame {
            if let Some(session_id) = session_for_inflight {
                clear_msg_fetch_inflight_if_matches(state, &session_id, generation);
            }
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

/// msgfix1 T4 返修②（skeptic 补审）：只在该 session 当前的单飞行占用仍是**这次
/// generation**（全局单调递增，`handle_msg_fetch_at` 每接受一次请求就领一个新值，见
/// `GatewayInnerState::msg_fetch_generation_counter`）时才清除——此前按 `command_id` 匹配，
/// 一旦客户端复用同一个 command_id（旧请求已超时被新请求接管，二者 command_id 相同），旧
/// 请求残留在 `reply_queue` 里迟迟才被 drain 的终片会命中 `command_id` 相等、误清新请求刚占
/// 上的槽位——`generation` 对每次接受都是全新值，天然不会跟任何更早或更晚的接受撞上，从根上
/// 消除这条误清路径（配合 `msg_fetch_command_ledger_admit` 从源头拒绝 command_id 复用是双重
/// 防线：账本可能因为容量淘汰而失效，generation 判定始终正确）。
fn clear_msg_fetch_inflight_if_matches(state: &GatewayInnerState, session: &str, generation: u64) {
    let mut inflight = lock(&state.msg_fetch_inflight);
    if inflight
        .get(session)
        .is_some_and(|entry| entry.generation == generation)
    {
        inflight.remove(session);
    }
}

/// msgfix1 T4（M0 §10.9「reply 独立有界队列」满→回 busy 终态）：尝试把一条 reply 条目塞进
/// `state.reply_queue`——容量见 `REPLY_QUEUE_CAPACITY`。满时返回 `false`，调用方据此中止当前
/// 分片序列并改发 `msg.fetch.error{busy}` 终态（`handle_msg_fetch_at` 的兜底路径），不静默
/// 丢帧。用于**单帧**入队（`msg.fetch.error`、`busy` 兜底本身、以及各类正常单条 reply）——
/// 多分片传输请用 `try_enqueue_reply_chunk`（少留 1 个坑位，保证兜底 error 总有地方放）。
fn try_enqueue_reply(state: &GatewayInnerState, item: ReplyQueueItem) -> bool {
    let mut queue = lock(&state.reply_queue);
    if queue.len() >= REPLY_QUEUE_CAPACITY {
        return false;
    }
    queue.push_back(item);
    true
}

/// msgfix1 T4：多分片传输专用入队——比 `try_enqueue_reply` 严格预留 1 个坑位。一旦某片因为
/// "满"而失败，调用方要紧接着回退发一条 `msg.fetch.error{busy}` 终态（M0 §10.9「满→回 busy
/// 终态、不静默」），这条终态帧必须总有地方放——如果分片本身把队列写到刚好 100% 满才失败，
/// 兜底 error 会紧接着在同一次饱和里也失败，退化成"满→连 busy 都发不出去"的双重饱和（这条
/// 极窄的边仍在——见 `reply_queue_dropped` 与两条测试
/// `handle_msg_fetch_reply_queue_full_aborts_transfer_and_appends_busy_terminal`/
/// `handle_msg_fetch_double_saturation_drops_and_releases_inflight_when_even_the_busy_error_
/// cannot_fit`——但只在队列已经被预先写到刚好等于容量时才会命中，正常的"分片途中撞满"必然
/// 留得出这 1 个坑位）。
fn try_enqueue_reply_chunk(state: &GatewayInnerState, item: ReplyQueueItem) -> bool {
    let mut queue = lock(&state.reply_queue);
    if queue.len() + 1 >= REPLY_QUEUE_CAPACITY {
        return false;
    }
    queue.push_back(item);
    true
}

/// msgfix1 T4 返修②（skeptic 补审）：单飞行槽位因超时被新请求接管时，把上一个 generation
/// 还没发出的残片从 `reply_queue` 里整体清掉——这些分片属于一次客户端早已放弃等待（超过
/// `MSG_FETCH_INFLIGHT_TIMEOUT_MS` 未见任何回应）的旧 fetch，继续让它们被正常 drain 发出只会
/// 把跟当前请求毫不相干的旧数据推给客户端（旧 command_id 因为 `msg_fetch_command_ledger_admit`
/// 已经不可能被重新提交，客户端此刻根本不会在等这个 command_id 的任何后续帧）。`generation`
/// 判定保证只清这一个 session 里恰好属于被取代的那次接受的残片，不会误伤同 session 更早或
/// 更晚的其它 generation（正常情况下同一时刻只有一个 generation 在队——这里按 generation 而
/// 不是"清空整个 session 在队条目"过滤，是为了在理论上的极端时序下也保持精确）。
fn purge_stale_reply_queue_generation(state: &GatewayInnerState, session: &str, generation: u64) {
    let mut queue = lock(&state.reply_queue);
    let before = queue.len();
    queue.retain(|item| {
        !(item.session.as_deref() == Some(session) && item.generation == generation)
    });
    let purged = before - queue.len();
    if purged > 0 {
        state
            .reply_queue_stale_generation_purged
            .fetch_add(purged as u64, Ordering::Relaxed);
    }
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
                            None,
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
                        None,
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
                                    None,
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
                        None,
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
    // msgfix1 T4：`reply`（M0 §10.1）是第一个真正需要非空 `command_id` 的出站 kind——
    // 既有 event/live 调用点全部继续传 `None`，行为逐字节不变；`drain_reply_queue` 是
    // 唯一会传 `Some(..)` 的调用方。
    command_id: Option<&str>,
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
        command_id: command_id.map(str::to_owned),
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
    // msgfix1 T4：`command_id` 现在如实回显 `meta.command_id`——此前这里硬编码
    // `Value::Null`，因为唯一的调用方 `send_upstream_value` 服务 event/live，两者的
    // `EnvelopeMeta.command_id` 恒为 `None`。`reply`（M0 §10.1）是第一个真正需要非空
    // `command_id` 的出站 kind——取值必须与 `crate::remote_crypto::seal` 用来算 AAD 的那份
    // `meta.command_id` 逐字节一致（AAD 拼串含 `command_id`，见 `remote_crypto::build_aad`），
    // 两处各写一份必然产生"JSON 里的 command_id"与"AAD 里签的 command_id"不同源的风险——
    // 单一读点排除这种分裂。
    let command_id = match meta.command_id.as_deref() {
        Some(command_id) => Value::String(command_id.to_owned()),
        None => Value::Null,
    };
    let mut envelope = serde_json::json!({
        "v": meta.v,
        "room": meta.room,
        "epoch": meta.epoch,
        "kind": meta.kind,
        "session": meta.session,
        "command_id": command_id,
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

/// msgfix1 T7 B2（opus 整盘审 P1-4 裁决=最小可见化）：工具输出被裁到 `OUTPUT_TRUNCATE_BYTES`
/// 时，纯粹砍掉尾部字节会让远端读者以为内容天然到此为止、完全看不出发生过截断——这是一种
/// "不可见的信息损失"：用户可能依据不完整的工具输出做判断而自己毫无察觉。这里只做最小可见化：
/// 真正发生截断时在文本尾追加固定标记 `TOOL_OUTPUT_TRUNCATION_MARKER`；能整份取回被截掉尾部的
/// 完整 ref 化留作 BACKLOG（M0 条文/设计稿本任务不动，见 HANDOFF 交接单），这里只解决
/// "看不看得出被截断"，不解决"截断之后怎么找回全文"。
///
/// 标记必须**计入 `max_bytes` 预算之内**，不能先按 `max_bytes` 截完正文再往后拼标记——那样会把
/// 总字节数顶到 `max_bytes` 之上，重新撞上调用方紧接着做的预算判定（`enqueue_milestone_item`
/// 里 `milestone_frame_bytes(...) > SNAPSHOT_SEND_BUDGET_BYTES` 那道闸）。做法：先给正文腾出
/// `max_bytes - marker.len()` 字节的截断空间，标记再拼上去，总字节数恒 ≤ max_bytes。
const TOOL_OUTPUT_TRUNCATION_MARKER: &str = "…[输出已截断]";

fn truncate_utf8_with_marker(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let body_budget = max_bytes.saturating_sub(TOOL_OUTPUT_TRUNCATION_MARKER.len());
    let mut truncated = truncate_utf8(text, body_budget);
    truncated.push_str(TOOL_OUTPUT_TRUNCATION_MARKER);
    truncated
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
///
/// msgfix2 U1（设计稿 v4.1 §4.1）：本函数**同时**是 L1 活动摘要聚合器的 delta 提取点——不是
/// 拆成独立函数重新扫一遍 batch，是刻意合并：`ToolCompleted` 分支已经调用
/// `take_tool_name`（消费式查询，查到即从关联表摘除），活动摘要需要同一个工具名判断
/// `mcp__` 前缀；若拆成两个各自独立遍历同一批事件的函数，无论谁先跑，`take_tool_name`
/// 都会把关联表清空，另一个函数就再也拿不到工具名——所以两个关注点必须共享同一次
/// `take_tool_name` 调用结果，只能同函数内完成。`extract_activity_summary_delta`
/// 只在 `state.activity_summary_tx` 已配置（`configure_activity_summary_writer` 启用过
/// 聚合器）时才真正 try_send；未配置时整个功能是 no-op，不计入任何丢弃计数（"未启用"≠
/// "启用了但丢了"，见该字段文档）。
fn extract_tool_milestones(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    payload: &crate::event_transport::BatchPayload,
) {
    use crate::agent_event::AgentEvent;

    let Some(activity_summary_tx) = state.activity_summary_tx.get() else {
        // 聚合器未配置——退化为原有纯 tool.completed 提取路径，零额外开销。
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
                        publish_tool_completed_milestone(
                            state,
                            milestone_tx,
                            batch,
                            id,
                            &tool,
                            status,
                            *exit_code,
                            output.as_deref(),
                        );
                    }
                    AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. } => {
                        purge_tool_correlation_for_run(state, &batch.run_id);
                    }
                    _ => {}
                }
            }
        }
        return;
    };

    for batch in &payload.batches {
        let logical_run_id = activity_summary_logical_run_id(batch);
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
                    publish_tool_completed_milestone(
                        state,
                        milestone_tx,
                        batch,
                        id,
                        &tool,
                        status,
                        *exit_code,
                        output.as_deref(),
                    );
                    send_activity_summary_delta(
                        state,
                        activity_summary_tx,
                        batch.session_id.clone(),
                        logical_run_id.clone(),
                        ActivitySummaryDeltaKind::ToolCompleted {
                            mcp: tool.starts_with("mcp__"),
                            failed: matches!(status, crate::agent_event::ToolStatus::Failed),
                        },
                    );
                }
                AgentEvent::ApprovalRequested { .. } => {
                    // msgfix2 U1 修单三（G2·独立审查 P1）：真路由——approval 是否允许把内容
                    // 原样并入 L1 由 `event_joins_l1_aggregation` 在运行时判定（release 构建
                    // 同样生效，不再只是 debug_assert）。approval 是 actionable 类型，函数
                    // 返回 false，这里因此只走受限路径：产生不带字段的 `PermissionPrompt`
                    // 计数 delta；approval_id/command/summary/cwd 等原卡内容依旧绝不进入
                    // activity_summary——`PermissionPrompt` 变体本身没有字段位置可以携带
                    // 内容，编译期即锁死。
                    if !event_joins_l1_aggregation("approval") {
                        send_activity_summary_delta(
                            state,
                            activity_summary_tx,
                            batch.session_id.clone(),
                            logical_run_id.clone(),
                            ActivitySummaryDeltaKind::PermissionPrompt,
                        );
                    } else {
                        // 理论不可达：当前白名单恒把 approval 判为 actionable。若未来白名单
                        // 漂移把它移出 actionable 集合，debug 构建在这里立即炸出来，而不是
                        // release 环境悄悄改变路由行为却毫无信号——这正是 G2 要堵的漂移窗口。
                        debug_assert!(
                            false,
                            "白名单漂移：approval 不再被判定为 actionable，L1 路由需要重新设计（本刀未实现）"
                        );
                    }
                }
                AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. } => {
                    purge_tool_correlation_for_run(state, &batch.run_id);
                    // msgfix2 F1（spec §4.1「粒度=逻辑 run」）：member lane 自己的终态只代表
                    // "这条 member lane 结束了"，不代表"整个逻辑（lead）run 结束了"——只有
                    // lead/solo 自己的 batch（不带 dispatch，`activity_summary_logical_run_id`
                    // 此时就是它自己）才允许封父 run。member lane 的非终态 delta（上面
                    // ToolCompleted/PermissionPrompt 分支）不受此门槛限制，仍照常并入父 run 计数
                    // ——否则 member1 先完成会把 lead run 提前 sealed，丢掉 member2/lead 之后的
                    // 计数与终态。
                    if batch.dispatch.is_none() {
                        send_activity_summary_delta(
                            state,
                            activity_summary_tx,
                            batch.session_id.clone(),
                            logical_run_id.clone(),
                            ActivitySummaryDeltaKind::Terminal { failed: false },
                        );
                    }
                }
                AgentEvent::Error { .. } => {
                    // msgfix2 F1：同上——member lane 自己的异常终态同样不得封父 run。
                    if batch.dispatch.is_none() {
                        send_activity_summary_delta(
                            state,
                            activity_summary_tx,
                            batch.session_id.clone(),
                            logical_run_id.clone(),
                            ActivitySummaryDeltaKind::Terminal { failed: true },
                        );
                    }
                }
                AgentEvent::NeedsDecision { .. } => {
                    // msgfix2 U1 修单三（G2·独立审查 P1）+ msgfix2 U1b 尾单 B2：真路由——
                    // scope_change 是否允许并入 L1 由 `event_joins_l1_aggregation` 在运行时
                    // 判定；与 ApprovalRequested 分支同构成 if/else 两支，路由决策（调用单点
                    // 函数）和"是否产生 delta"分离成显式两条路径——不让"跳过"靠 match arm 本身
                    // 的沉默兜底（即便 scope_change 目前没有专属 delta kind、"非 actionable"
                    // 分支恒是显式空跳过，这件事也要可见、可测，不是碰巧沉默）。它走
                    // `db::Block::ScopeChange` 独立卡片路径（lib.rs 侧 lead 编排构造，不在本
                    // 文件）。
                    if !event_joins_l1_aggregation("scope_change") {
                        // 正确路由结果（scope_change 是 actionable）：不产生任何 delta——
                        // 没有专属 delta kind 可用，`PermissionPrompt` 语义上专属 approval，
                        // 不能借用。
                    } else {
                        // 理论不可达：当前白名单恒把 scope_change 判为 actionable。若未来
                        // 白名单漂移把它移出 actionable 集合，debug 构建立即炸出来——见
                        // ApprovalRequested 分支同款注释。
                        debug_assert!(
                            false,
                            "白名单漂移：scope_change 不再被判定为 actionable，L1 路由需要重新设计（本刀未实现）"
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

/// `ToolCompleted` → `tool.completed` 里程碑的公共构造尾段——`extract_tool_milestones`
/// 两条路径（聚合器配置/未配置）共用，避免两份重复的 `enqueue_milestone_for_upstream` 调用
/// 各自维护一份字段列表而漂移。
#[allow(clippy::too_many_arguments)]
fn publish_tool_completed_milestone(
    state: &GatewayInnerState,
    milestone_tx: &SyncSender<(u64, MilestoneItem)>,
    batch: &crate::event_transport::RunBatch,
    id: &str,
    tool: &str,
    status: &crate::agent_event::ToolStatus,
    exit_code: Option<i64>,
    output: Option<&str>,
) {
    let client_msg_id = derive_client_msg_id(&format!("tool.completed|{}|{}", batch.run_id, id));
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
                "output": output.map(|value| truncate_utf8(value, OUTPUT_TRUNCATE_BYTES)),
            }),
            client_msg_id,
        },
    );
}

/// msgfix2 U1（设计稿 v4.1 §4.1）：一个 batch 的"逻辑 run"——team 会话按 lead run 聚合、member
/// lane 计数并入父 run。`RunBatch.run_id` 对 member lane 是传输层复合 lane id
/// （`member_transport_lane_id`："member:{lead_run_id}:{assignment_id}"），不是逻辑 run；真正
/// 的 lead run id 在 `batch.dispatch.run_id`（member_runner.rs::member_dispatch_meta 注册时
/// 填的就是 lead 传入的 run_id）。lead/solo 自己的 lane 不带 dispatch（`register_run(&run_id,
/// ..., None, ...)`，见 lib.rs 咽喉），此时 `batch.run_id` 本身就是真实 run_id，直接回退即可。
fn activity_summary_logical_run_id(batch: &crate::event_transport::RunBatch) -> String {
    batch
        .dispatch
        .as_ref()
        .and_then(|dispatch| dispatch.run_id.clone())
        .unwrap_or_else(|| batch.run_id.clone())
}

fn send_activity_summary_delta(
    state: &GatewayInnerState,
    tx: &SyncSender<ActivitySummaryDelta>,
    session_id: String,
    run_id: String,
    kind: ActivitySummaryDeltaKind,
) {
    if tx
        .try_send(ActivitySummaryDelta {
            session_id,
            run_id,
            kind,
        })
        .is_err()
    {
        state
            .activity_summary_dropped
            .fetch_add(1, Ordering::Relaxed);
    }
}

// ============================================================================
// msgfix2 U1（设计稿 v4.1 §4.1·M0 §10.11）：L1 活动摘要聚合器——独立串行写线程。
//
// 数据流：`extract_tool_milestones`（sink 回调，禁止 DB I/O）→ try_send 进
// `ActivitySummaryDelta` 有界 channel → `run_activity_summary_worker`（独立 OS 线程，串行
// 消费）在内存里累积计数，按节流/终态规则决定何时调用注入的 `ActivitySummaryWriter` 落库 +
// republish（真正的 DB 写只发生在这个线程里，绝不在 sink 回调内）。
// ============================================================================

/// 一条运行时增量——由 `extract_tool_milestones` 产生。
#[derive(Clone, Debug, PartialEq)]
struct ActivitySummaryDelta {
    session_id: String,
    run_id: String,
    kind: ActivitySummaryDeltaKind,
}

#[derive(Clone, Debug, PartialEq)]
enum ActivitySummaryDeltaKind {
    ToolCompleted {
        mcp: bool,
        failed: bool,
    },
    PermissionPrompt,
    /// run 终态——`failed=true` 对应 `AgentEvent::Error`；`false` 对应
    /// `Completed`/`RunCloseout`（`Blocked`/`NeedsDecision` 不是终态：run 仍可能继续，
    /// 不封口）。
    Terminal {
        failed: bool,
    },
}

/// 落库签名镜像 `db::upsert_activity_summary_and_publish`（少 `&Connection`——生产环境的注入
/// 闭包在实际接线时捕获真实连接）：`(session_id, run_id, tool_calls, failed, mcp_calls,
/// permission_prompts, state)`。生产接线（真实 DB 写 provider）留后续刀，见
/// `configure_activity_summary_writer` 文档。
pub(crate) type ActivitySummaryWriter =
    Box<dyn Fn(&str, &str, i64, i64, i64, i64, &str) -> Result<(), String> + Send + Sync>;

const ACTIVITY_SUMMARY_THROTTLE_MS: u64 = 2_000;
const ACTIVITY_SUMMARY_CHANNEL_CAPACITY: usize = 2048;
const ACTIVITY_SUMMARY_TICK_MS: u64 = 250;
/// R6③（msgfix2 整盘审 P2 顺手）：连续写失败的退避窗口上限——`activity_summary_retry_due`
/// 按 `ACTIVITY_SUMMARY_THROTTLE_MS * 2^consecutive_failures` 指数增长，封顶在这个值，不会
/// 因为失败次数持续攀升就无限拉长下一次重试的等待时间。
const ACTIVITY_SUMMARY_MAX_BACKOFF_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Default)]
struct ActivityCounters {
    tool_calls: i64,
    failed: i64,
    mcp_calls: i64,
    permission_prompts: i64,
}

#[derive(Debug, PartialEq)]
struct RunActivityEntry {
    session_id: String,
    counters: ActivityCounters,
    state: &'static str,
    /// 终态 delta **一到达**（不是"成功写库后"）即置 true——之后到达的任何非终态 delta 一律
    /// 丢弃（设计稿「终态置位后到达的 running 更新丢弃」）；终态本身只应用一次
    /// （`apply_activity_summary_delta` 对已 sealed 的 run 直接忽略后续 Terminal delta）。
    /// **`sealed` 只代表"不再接受新的 running/终态 delta"，不代表"终态已经成功落库"**——
    /// 那是 `terminal_pending` 的职责（msgfix2 U1 修单三·独立审查 G1：过去这两个语义挤在
    /// 同一个位上，写库失败时无法区分"该挡迟到 delta 了"与"还需要重试落库"，导致终态永久
    /// 卡在未落库状态，见 `terminal_pending` 文档）。tombstone（sealed 条目）本身不会从
    /// `state.runs` 移除，有界生命周期淘汰策略留 BACKLOG（本刀不实现淘汰）。
    sealed: bool,
    /// 终态已 sealed 但**尚未成功写库**——true 期间该条目仍会被
    /// `activity_summary_due_flushes` 按既有节流/重试节奏（`ACTIVITY_SUMMARY_THROTTLE_MS`）
    /// 收进待写批次重试，直到 `flush_activity_summary` 真正写成功才清 false。修复前 writer
    /// 失败直接 `return`、且 tick 扫描把 sealed 条目一律排除在待写批次外——终态一旦写失败就
    /// 永远停在"内存里已 sealed、DB 里仍是 running"的状态，违反 M0「有活动 run 终态必封口
    /// 恰好一次」（独立审查 G1·P1）。非终态条目此字段恒为 false。
    terminal_pending: bool,
    dirty: bool,
    last_flushed_at_ms: Option<u64>,
    /// R6③（msgfix2 整盘审 P2 顺手）：连续写失败计数——`flush_activity_summary` 每次调用
    /// writer 失败就 +1，写成功清零。`activity_summary_retry_due` 据此算指数退避窗口。
    consecutive_failures: u32,
    /// R6③：最近一次真正尝试写（无论成功失败）的时刻——跟 `last_flushed_at_ms`（只在成功时
    /// 推进，语义是"最近一次成功发布"）分开：失败也要留一个"上次动手的时间"才能计算退避，
    /// 不能像旧实现那样只看 `last_flushed_at_ms`（一个从未成功过的条目该字段恒 `None`，旧的
    /// `map_or(true, ..)` 让它每个 tick 都判定"到期"，写线程对着注定失败的目标原地空转）。
    last_attempt_at_ms: Option<u64>,
}

impl RunActivityEntry {
    fn new(session_id: String) -> Self {
        Self {
            session_id,
            counters: ActivityCounters::default(),
            state: "running",
            sealed: false,
            terminal_pending: false,
            dirty: false,
            last_flushed_at_ms: None,
            consecutive_failures: 0,
            last_attempt_at_ms: None,
        }
    }
}

#[derive(Default)]
struct ActivitySummaryAggregatorState {
    runs: HashMap<String, RunActivityEntry>,
}

/// 一次待落库的快照——`apply_activity_summary_delta`/`activity_summary_due_flushes` 产出。
#[derive(Debug, PartialEq)]
struct ActivitySummaryFlush {
    run_id: String,
    session_id: String,
    counters: ActivityCounters,
    state: &'static str,
}

/// 应用一条 delta 到聚合态，纯函数（不做 I/O，可脱离线程/channel 直接单测）。
///
/// **节流留给 tick**：本函数本身**不**判断是否该立即发布——非终态 delta 只更新计数/dirty，
/// 是否到了节流窗口统一交给 `activity_summary_due_flushes`（worker 每个 tick 调一次），避免
/// 两处各自判断"是否该发"而彼此不一致。**唯一例外是终态**：终态 delta 命中且这是该 run
/// 第一次被封口时，本函数恒返回 `Some`——设计稿「终态写取消/压过排队中的 running 节流更新」
/// 要求终态立即写，不等下一个 tick。
///
/// 已 sealed 的 run：任何后续 delta（无论终态还是非终态）一律忽略，返回 `None`（"终态置位后
/// 到达的 running 更新丢弃"，且终态本身不可能被应用两次）。
fn apply_activity_summary_delta(
    state: &mut ActivitySummaryAggregatorState,
    delta: ActivitySummaryDelta,
) -> Option<ActivitySummaryFlush> {
    match delta.kind {
        ActivitySummaryDeltaKind::Terminal { failed } => {
            let entry = state.runs.get_mut(&delta.run_id)?;
            if entry.sealed {
                return None;
            }
            entry.state = if failed { "failed" } else { "done" };
            entry.sealed = true;
            // msgfix2 U1 修单三（G1）：sealed 立即挡迟到 running，但落库是否成功是另一件事——
            // terminal_pending 一直保持 true，直到 `flush_activity_summary` 真正写成功才清掉；
            // 期间即便这次 immediate attempt（调用方紧接着触发的那次写）失败，`activity_
            // summary_due_flushes` 也会把这个条目继续收进重试批次（见该函数文档）。
            entry.terminal_pending = true;
            entry.dirty = false;
            Some(ActivitySummaryFlush {
                run_id: delta.run_id,
                session_id: entry.session_id.clone(),
                counters: entry.counters.clone(),
                state: entry.state,
            })
        }
        ActivitySummaryDeltaKind::ToolCompleted { mcp, failed } => {
            let entry = state
                .runs
                .entry(delta.run_id)
                .or_insert_with(|| RunActivityEntry::new(delta.session_id));
            if entry.sealed {
                return None;
            }
            entry.counters.tool_calls += 1;
            if failed {
                entry.counters.failed += 1;
            }
            if mcp {
                entry.counters.mcp_calls += 1;
            }
            entry.dirty = true;
            None
        }
        ActivitySummaryDeltaKind::PermissionPrompt => {
            let entry = state
                .runs
                .entry(delta.run_id)
                .or_insert_with(|| RunActivityEntry::new(delta.session_id));
            if entry.sealed {
                return None;
            }
            entry.counters.permission_prompts += 1;
            entry.dirty = true;
            None
        }
    }
}

/// 每个 tick 调一次：扫描待写批次，收进两类条目——① 未 sealed 且 dirty 的常规 running
/// 快照；② `sealed && terminal_pending`（msgfix2 U1 修单三·G1：终态已到达但尚未成功写库，
/// 见 `RunActivityEntry::terminal_pending` 文档）——一旦落库成功，`flush_activity_summary`
/// 会清掉 `terminal_pending`，之后这个 sealed 条目就再也不会被本函数收进批次（既不 dirty
/// 也不 pending）。两类条目都受同一退避窗口（`activity_summary_retry_due`）约束。不是"有新
/// delta 才检查"，否则一个 run 停在最后一次工具调用后就再也不会被扫到，它的摘要会永远卡在
/// 上一次发布（甚至从未发布过）的旧计数上；同理终态如果只在到达那一刻尝试一次，写失败后不会
/// 再被 tick 捡回来（独立审查 G1 抓到的正是这个缺口）。
fn activity_summary_due_flushes(
    state: &ActivitySummaryAggregatorState,
    now_ms: u64,
) -> Vec<ActivitySummaryFlush> {
    state
        .runs
        .iter()
        .filter(|(_, entry)| {
            (entry.dirty && !entry.sealed) || (entry.sealed && entry.terminal_pending)
        })
        .filter(|(_, entry)| activity_summary_retry_due(entry, now_ms))
        .map(|(run_id, entry)| ActivitySummaryFlush {
            run_id: run_id.clone(),
            session_id: entry.session_id.clone(),
            counters: entry.counters.clone(),
            state: entry.state,
        })
        .collect()
}

/// R6③（msgfix2 整盘审 P2 顺手）：`activity_summary_due_flushes` 的节流/退避窗口判据，独立
/// 抽出方便单测。以 `last_attempt_at_ms`（上次真正尝试写的时刻，无论成败，见该字段文档）为
/// 起点：`consecutive_failures == 0`（从未失败过，或上次已写成功）沿用既有
/// `ACTIVITY_SUMMARY_THROTTLE_MS`（2s）常规节流；`consecutive_failures > 0` 时窗口按
/// `THROTTLE_MS * 2^consecutive_failures` 指数放大，封顶 `ACTIVITY_SUMMARY_MAX_BACKOFF_MS`
/// （30s）。`last_attempt_at_ms` 为 `None`（从未真正尝试过写）时无条件到期——首次写不该等
/// 节流窗口。
///
/// **旧实现的问题**：只看 `last_flushed_at_ms`（只在成功时推进）——一个从未成功过的条目
/// （目标持续不可写：DB 忙/磁盘满等）该字段恒 `None`，`map_or(true, ..)` 让它每个 tick
/// （`ACTIVITY_SUMMARY_TICK_MS` = 250ms，4Hz）都判定"到期"，写线程对着注定失败的目标原地
/// 空转重试，纯粹浪费（真实故障持续期间可能是几十次/秒的无效写尝试）。`.min(20)` 只是防御性
/// 地界定移位量（`1u64 << n`），避免失败计数在长跑进程里增长到荒谬大小时移位溢出——达到
/// 20 次失败时退避已经远超 30s 封顶，`.min(20)` 之后再 `.min(MAX)` 结果不变，纯粹是安全网。
fn activity_summary_retry_due(entry: &RunActivityEntry, now_ms: u64) -> bool {
    let Some(last_attempt) = entry.last_attempt_at_ms else {
        return true;
    };
    let backoff_ms = if entry.consecutive_failures == 0 {
        ACTIVITY_SUMMARY_THROTTLE_MS
    } else {
        ACTIVITY_SUMMARY_THROTTLE_MS
            .saturating_mul(1u64 << entry.consecutive_failures.min(20))
            .min(ACTIVITY_SUMMARY_MAX_BACKOFF_MS)
    };
    now_ms.saturating_sub(last_attempt) >= backoff_ms
}

/// 实际调用注入的 writer 落库一次快照，并按结果更新聚合态。`now_ms` 由调用方传入（而不是
/// 内部自己调 `now_unix_ms()`）——R6③：同一轮 worker tick 里"扫描到期批次"与"落库这批"共用
/// 同一个时间戳，且测试能注入确定性时钟验证退避窗口，不依赖真实系统时间推移：
/// - 写成功 → 清 dirty、推进 `last_flushed_at_ms`/`last_attempt_at_ms`（节流窗口重新计时）、
///   `consecutive_failures` 清零——**无论该 run 是否已 sealed，条目都不从 `state.runs` 移除**
///   （msgfix2 F2 修单：终态成功写库后曾经把条目从 map 里删掉，之后一条迟到的 running delta
///   会在 `apply_activity_summary_delta` 里因为 entry 不存在而 `or_insert_with` 重新造一个新
///   （未 sealed）条目，等于把已经宣告终态的 run"复活"成 running 重新发布——sealed 必须是
///   永久 tombstone，`apply_activity_summary_delta` 对已存在且 `sealed==true` 的条目本来就会
///   直接丢弃后续 delta，只要条目还在就天然挡得住；代价是长跑桌面进程的内存表无界增长，这是
///   设计稿明文认领的取舍（§4.1「sealed 标记持久保留在内存 map」），不是遗留 bug）。
/// - 写失败 → 保留 dirty、不推进 `last_flushed_at_ms`，但**推进 `last_attempt_at_ms` 并把
///   `consecutive_failures` +1**（R6③：旧实现这里完全不碰 `state.runs`，只增计数就
///   `return`——`activity_summary_due_flushes` 因此拿不到任何"上次失败是什么时候"的信号，
///   只能靠 `last_flushed_at_ms` 恒 `None` 的旧逻辑判定"到期"，于是原地空转），下一轮 tick
///   由 `activity_summary_retry_due` 按指数退避窗口自然重试——best-effort，与既有缺口④
///   republish 失败"只记日志、不回滚已成功的状态"同一容错姿势。**终态快照走的是同一条路径**：
///   写失败时同样保留 `terminal_pending`（`sealed` 已经在到达那一刻置过，不受这里影响），
///   下一轮 tick 由 `activity_summary_due_flushes` 把它重新收进批次重试（msgfix2 U1 修单三·
///   G1）——直到写成功，`entry.terminal_pending` 才在下面清掉。
fn flush_activity_summary(
    state: &mut ActivitySummaryAggregatorState,
    writer: &ActivitySummaryWriter,
    write_failures: &AtomicU64,
    flush: ActivitySummaryFlush,
    now_ms: u64,
) {
    let result = writer(
        &flush.session_id,
        &flush.run_id,
        flush.counters.tool_calls,
        flush.counters.failed,
        flush.counters.mcp_calls,
        flush.counters.permission_prompts,
        flush.state,
    );
    if result.is_err() {
        write_failures.fetch_add(1, Ordering::Relaxed);
        if let Some(entry) = state.runs.get_mut(&flush.run_id) {
            entry.last_attempt_at_ms = Some(now_ms);
            entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        }
        return;
    }
    if let Some(entry) = state.runs.get_mut(&flush.run_id) {
        entry.dirty = false;
        // 常规 running 条目此字段恒为 false，这里无条件清是无害 no-op；只有 sealed 的终态
        // 条目才会真的从 true 翻到 false——写成功之后这个条目既不 dirty 也不再 pending，
        // `activity_summary_due_flushes` 从此再也不会把它收进任何批次（G1）。
        entry.terminal_pending = false;
        entry.last_flushed_at_ms = Some(now_ms);
        entry.last_attempt_at_ms = Some(now_ms);
        entry.consecutive_failures = 0;
    }
}

/// 应用一批已经到手（非阻塞可读）的 delta，返回按应用顺序产生的待写快照列表——msgfix2 F2
/// 修单①：worker 主循环过去是"每收一条就检查一次节流到期"，如果 channel 里已经排着
/// `[running, terminal]`（同一个 run），处理完 running 后立即扫一遍
/// `activity_summary_due_flushes`——running 那条 `last_flushed_at_ms` 还是 `None`（首次发布
/// 恒判"到期"，见其文档），会在 terminal 还没被这个函数看到之前就先发布出去，制造出"明明
/// 已经终态了、却先看到一条 running 摘要"的用户可见闪烁。
///
/// 修法：worker 从 channel 拿到第一条后，先把当下已经排队、非阻塞可取的其余 delta 一次性
/// drain 进同一批（见调用方），整批按到达顺序喂给本函数——终态 delta 本身在
/// `apply_activity_summary_delta` 里恒定立即返回待写快照（不受节流约束），非终态 delta 只
/// 置 dirty、从不在这里触发发布；只有 batch 处理完之后调用方才会再去检查节流到期的常规
/// running 发布。这样"同批里终态排在 running 后面"的情形，只会产出一次 terminal 快照，
/// running 那条因为函数本身不做节流判断而从未被单独发布过（`state.runs` 里的 dirty 标记
/// 被 terminal 分支一并清掉，见 `apply_activity_summary_delta` Terminal 分支）——不需要额外
/// 的"同 run 多条只留最新"去重表，纯函数组合本身就是正确的。
fn drain_activity_summary_deltas(
    state: &mut ActivitySummaryAggregatorState,
    deltas: impl IntoIterator<Item = ActivitySummaryDelta>,
) -> Vec<ActivitySummaryFlush> {
    let mut flushes = Vec::new();
    for delta in deltas {
        if let Some(flush) = apply_activity_summary_delta(state, delta) {
            flushes.push(flush);
        }
    }
    flushes
}

/// 独立串行写线程主循环。`recv_timeout` 短周期 tick——没有新 delta 到达时也定期醒来检查节流
/// 窗口到期的 dirty run（见 `activity_summary_due_flushes` 文档）。channel 所有发送端析构后
/// `recv_timeout` 返回 `Disconnected`，线程随之退出（同 `run_session_index_snapshot_worker`
/// 的"随 Inner 生命周期自然收尾"惯例）。
fn run_activity_summary_worker(rx: Receiver<ActivitySummaryDelta>, writer: ActivitySummaryWriter) {
    let mut state = ActivitySummaryAggregatorState::default();
    let write_failures = AtomicU64::new(0);
    loop {
        match rx.recv_timeout(Duration::from_millis(ACTIVITY_SUMMARY_TICK_MS)) {
            Ok(first) => {
                // msgfix2 F2 修单①：不是收到就立刻各自判一次节流到期——先把当下已经非阻塞
                // 可取的其余 delta 一次性 drain 进同一批（`try_recv` 不等待，channel 空了就
                // 停），整批一起喂给 `drain_activity_summary_deltas`，本轮循环末尾才统一做一次
                // 节流到期扫描。见该函数文档："排队中的 running→terminal 序列仍会先发布
                // running" 的根因就是过去每条 delta 各自触发一次 due-flush 检查。
                let mut batch = vec![first];
                while let Ok(delta) = rx.try_recv() {
                    batch.push(delta);
                }
                let now_ms = now_unix_ms();
                for flush in drain_activity_summary_deltas(&mut state, batch) {
                    flush_activity_summary(&mut state, &writer, &write_failures, flush, now_ms);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let now_ms = now_unix_ms();
        for flush in activity_summary_due_flushes(&state, now_ms) {
            flush_activity_summary(&mut state, &writer, &write_failures, flush, now_ms);
        }
    }
}

/// 配置并启动 L1 活动摘要聚合器（幂等——`OnceLock::get_or_init` 保证进程生命周期内至多 spawn
/// 一个写线程，同 `ensure_snapshot_worker` 惯例）。调用前功能整体 no-op（`extract_tool_milestones`
/// 直接跳过，见 `GatewayInnerState::activity_summary_tx` 文档）；调用后 `extract_tool_milestones`
/// 才开始产 delta、独立写线程才开始落库。
///
/// msgfix2 U1b：生产接线已激活——见 `install_activity_summary_writer`（本文件，`GATEWAY` 单例
/// 建立之后才可能有真实 `&Inner` 可用，同 `install_event_sink` 惯例）+ lib.rs 侧
/// `remote_gateway_activity_summary_writer`（真实 DB 写 provider，捕获 `AppHandle` 短锁访问
/// `Db` state，调用 `db::upsert_activity_summary_and_publish`）。**visibility 仍是模块私有**
/// （不是 `pub(crate)`）——`GatewayInnerState` 本身是模块私有类型，把这个函数提到 `pub(crate)`
/// 只会产生"函数比它的参数类型更公开"的编译警告，没有实际收益：唯一需要跨模块调用的场景已经
/// 由 `install_activity_summary_writer`（真正的 `pub(crate)` 入口，签名只暴露 `pub(crate)`
/// 类型 `ActivitySummaryWriter`，不泄漏 `GatewayInnerState`）覆盖，本函数在模块内的唯一调用方
/// 也正是它。
fn configure_activity_summary_writer(state: &GatewayInnerState, writer: ActivitySummaryWriter) {
    state.activity_summary_tx.get_or_init(|| {
        let (tx, rx) =
            mpsc::sync_channel::<ActivitySummaryDelta>(ACTIVITY_SUMMARY_CHANNEL_CAPACITY);
        thread::Builder::new()
            .name("remote-activity-summary".to_owned())
            .spawn(move || run_activity_summary_worker(rx, writer))
            .expect("failed to start remote-activity-summary thread");
        state
            .activity_summary_worker_spawn_count
            .fetch_add(1, Ordering::Relaxed);
        tx
    });
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
/// 交错到达时**重建仍会丢弃已积累的归约态**（内容层面的多 lane 合并/隔离不在本轮范围内）——
/// 跨 lane 的 seq 不可比，客户端可能收到水位更高但内容更少的快照并覆盖本地已有状态；一期
/// 契约只保证 solo / lead 主线程会话的快照准确，team member lane 精确快照留 BACKLOG。
///
/// **缺口⑥（team 多 lane·保 lead/latest 活跃 lane；R3·msgfix2 整盘审扩展到流式 batch）**：
/// 上面这条"内容会被覆盖"的局限保留不动，但**顶占/清空占用者槽位**这件事收窄——一条 batch
/// 如果裸 `run_id` 与"处理这条 batch 之前"该 session 槽位里已经占用的 run_id 不同，且满足
/// 下面任一条件，视为**不该触碰占用者**：① 这条 batch 是终态（Completed/RunCloseout）；
/// ② 它折回的逻辑 run（`activity_summary_logical_run_id`——member lane 的传输层复合 lane id
/// 折回 `batch.dispatch.run_id`，即它真正归属的 lead run id）与当前占用者相等（说明这是占用
/// 者名下的一条子 lane，不管终态还是流式）。①②任一成立，整条 batch 对该 session 的 partial
/// 条目完全不生效（不 rebuild、不 feed、不 remove），当前占用者的归约态原样保留；都不成立
/// （裸 run_id 不同、且逻辑 run 也不同——真正无关的另一个 run）才走下面正常的
/// "needs_rebuild/喂事件/终态清空"路径。**R3 之前只有①**：member lane 中途的**流式**事件
/// 完全没有守卫、会直接命中 needs_rebuild 把占用者的槽位顶替成 member 自己；等 member 自己
/// 的终态紧随而至时，占用者已经变成了 member（裸 run_id 相等），①这道守卫反而不成立、正常
/// 清空——两步绕开同一道防线，占用者的归约态照样丢失（红测试
/// `partial_snapshot_member_lane_streaming_batch_does_not_seize_or_wipe_lead_slot`）。空槽位
/// （当前无人占用）时任何 run 都能正常声明/清空——这条收窄只保护"槽位已被别的 run 占用"这一
/// 种情形。
///
/// **调用时机（P0-b 返工①·R4 扩展）**：本函数在 `enqueue_batch_payload_for_upstream` 里排在
/// gate 判断之前，无条件执行——桌面断连（gate 关闭）期间事件仍要喂 reducer，否则重连后
/// `control.snapshot` 会读到假 idle/陈旧态（同时是"断连窗内条目永久泄漏"与"水位打洞"两个
/// 同族问题的根修）。`extract_tool_milestones`（R4）挪到同一位置、同一理由，紧随其后调用
/// （见该函数就近调用点的 R4 注释）；gate 现在只决定 `upstream_tx.try_send` 那一段本身，
/// 以及 `extract_tool_milestones` 内部真正的上行帧（`tool.completed` 等，走
/// `enqueue_milestone_for_upstream` 自带的独立 gate 检查）是否真的入队——不再影响归约态维护
/// 或聚合器 delta 提取。
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
        let batch_is_terminal = batch.events.iter().any(|sequenced| {
            matches!(
                sequenced.event,
                AgentEvent::Completed { .. } | AgentEvent::RunCloseout { .. }
            )
        });
        let occupant_run_id = snapshots.get(&batch.session_id).map(|e| e.run_id.clone());
        if let Some(occupant) = &occupant_run_id {
            if occupant != &batch.run_id {
                // R3（缺口⑥扩展·msgfix2 整盘审 P1）：占用者存在、本 batch 裸 run_id 与占用者
                // 不同——两种情形都必须保护占用者不被触碰：
                // ① `batch_is_terminal`（旧缺口⑥已挡的那半，原样保留）：任何裸 run_id 不同
                //    的终态一律不得清空/触碰占用者，不管它是不是同一逻辑 run 的子 lane——一条
                //    真正无关的陌生终态同样不该有资格清掉别人的槽位。
                // ② 新增：非终态（流式）batch，如果它折回的逻辑 run
                //    （`activity_summary_logical_run_id`，把 member lane 的传输层复合 lane id
                //    折回 `batch.dispatch.run_id`，即 lead 的真实 run_id）与占用者相等——说明
                //    这条 batch 是占用者名下的一条子 lane（典型：member）。旧实现只在①挡过，
                //    ②完全没有守卫、一律落进下面的 `needs_rebuild`：member lane 的中途流式
                //    事件先把占用者（如 lead）的 partial 顶掉重建，member 自己的终态随后到
                //    达时 occupant 已经被换成了 member 的裸 run_id、跟这条终态 batch 相等，
                //    ①这道守卫（`occupant != batch.run_id`）反而不成立、正常清空——两步就
                //    绕开了同一道防线，占用者的归约态彻底丢失。
                //
                // ①②任一成立就整条 batch 对该 session 的 partial 条目完全不生效：不
                // rebuild、不 feed、不 remove。真正跟占用者逻辑 run 无关的陌生非终态 batch
                // （①不成立、②也不成立）才继续落到下面的 `needs_rebuild=true` 正常改朝换代
                // 路径——与快照维护同层的聚合器（`extract_tool_milestones`）本就用同一个
                // 函数把 member lane 计数并入父 run，这里改用同一个函数比对，快照槽位归属与
                // 聚合器归属口径统一，不再各按各的 run_id 定义"这条 batch 属于谁"（旧实现是
                // "双源漂移"：快照按裸 `batch.run_id`，聚合器按 `dispatch.run_id`）。
                if batch_is_terminal || activity_summary_logical_run_id(batch) == *occupant {
                    continue;
                }
            }
        }
        let needs_rebuild = occupant_run_id
            .as_deref()
            .map(|occupant| occupant != batch.run_id)
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
        for sequenced in &batch.events {
            entry.reducer.feed(&sequenced.event);
            entry.last_seq = sequenced.seq;
        }
        if batch_is_terminal {
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

    // R4（msgfix2 整盘审 P1）：`extract_tool_milestones` 挪到 gate 判断之前、无条件执行——
    // 与上面 `maintain_partial_snapshots` 同一姿势，理由更直接：`extract_tool_milestones`
    // 内部产两类东西——① `tool.completed` 等真正的上行帧（走 `enqueue_milestone_for_upstream`
    // → `publish_tool_completed_milestone`），这条路径本来就会自己重新读一次
    // `state.upstream_state` 再决定要不要真正 `try_send`（`enqueue_milestone_for_upstream`
    // 内部有一份独立的 gate 检查），外层这道 gate 对它是重复保护，挪到 gate 之前不改变它
    // "只在 gate 打开时才真正入队"的行为；② L1 活动摘要聚合器 delta（`send_activity_
    // summary_delta`，走独立 channel 落 DB 消息，**不是**上行帧、根本不该受"手机有没有连"
    // 影响）——旧实现把①②整个函数一起挂在 gate 判断之后，手机没连时函数整体不执行，②唯一
    // 的生产入口被一并挡住：`ToolCompleted`/`ApprovalRequested`/`Completed`/`RunCloseout`
    // 等事件从未走到这里，聚合器永远拿不到计数/终态信号，活动摘要永久卡在 running（即便
    // 之后手机连上，也没有任何补发机制会重新灌入这些已经错过的 delta）。挪到 gate 之前后，
    // ①的行为不变（内层 gate 兜底），②不再受外层 gate 牵连。
    extract_tool_milestones(state, milestone_tx, &payload);

    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return;
    }
    let generation = snapshot >> 1;

    match upstream_tx.try_send((generation, LiveQueueItem::Batch(payload))) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// `control.history` 使用的预构建 live 入队口。与 delta 共用同一有界 FIFO、generation
/// 标签和 drain 归属闸；只绕过 classify，因为 payload 已由 history 契约构造函数定型。
///
/// msgfix1 T3（缺口①·M0 §10.9 同一姿势）：返回值 `true` = 已成功 try_send 进队列（尽力而为，
/// 不保证送达，但已在制品）；`false` = 未能入队（client_msg_id 非法 / upstream 门控关闭 /
/// 队列满或断连）——调用方（`control.history` 命令臂）据此回真实失败 ack，不再无条件回 `Ok`。
fn enqueue_prebuilt_live_for_upstream(
    state: &GatewayInnerState,
    upstream_tx: &SyncSender<(u64, LiveQueueItem)>,
    item: MilestoneItem,
) -> bool {
    if !is_valid_client_msg_id(&item.client_msg_id) {
        state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let snapshot = state.upstream_state.load(Ordering::Acquire);
    if snapshot & 1 == 0 {
        return false;
    }
    let generation = snapshot >> 1;
    match upstream_tx.try_send((generation, LiveQueueItem::Prebuilt(item))) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            state.upstream_dropped.fetch_add(1, Ordering::Relaxed);
            false
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
        // msgfix1 T3（设计稿 §A）：`build_msg_completed_payload` 把预算好的 content_ref
        // 挂在私有键下——无论是否超预算都必须先把它取出并从 payload 上剥离，绝不能让它
        // 混进下面的尺寸测量（否则会把整条原始内容的哈希/字节数误算进 payload 尺寸）或
        // 流到 wire（非超预算消息不该带 content_ref，§10.6「可选字段」）。
        let content_ref_source = item
            .payload
            .as_object_mut()
            .and_then(|obj| obj.remove(MSG_COMPLETED_REF_SOURCE_KEY));
        if milestone_frame_bytes(&item.t, &item.payload) > SNAPSHOT_SEND_BUDGET_BYTES {
            match content_ref_source {
                Some(content_ref) => {
                    // 超预算不再静默丢弃——降级为块级 preview + content_ref，让远端至少
                    // 看得见这条消息、按需可拉全文（缺口①②）。计数器语义随之从「丢弃次数」
                    // 改为「降级为 preview 的次数」，继续保持指标可观测。
                    item.payload =
                        downgrade_to_preview_payload(&item.payload, content_ref, &item.t);
                    state
                        .replay_oversized_dropped
                        .fetch_add(1, Ordering::Relaxed);
                    // 不 return——降级后的 payload 走下面正常入队路径。
                }
                None => {
                    // 防御性兜底：理论上生产两条调用点（`publish_msg_completed_milestone`/
                    // `publish_msg_and_card_replay_rows`）都经 `build_msg_completed_payload`
                    // 构造、恒带 ref source。没有 ref source 就造不出合规 content_ref（宁可
                    // 保留旧的丢弃语义，也不伪造 revision/sha256——§10.6 硬约束）。
                    state
                        .replay_oversized_dropped
                        .fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
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

/// 生产用 sha256 hex（小写）——`content_ref.content_sha256`（M0 §10.6）唯一的落地点。测试
/// 模块另有一份同构的 `wire_v1_sha256_ascii_hex`（服务 wire fixture KAT），二者用途不同、
/// 互不复用：那份在 `#[cfg(test)]` 区域内，生产代码不可见。
fn sha256_hex_lower(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// content_ref 四字段（M0 §10.6）：`total_bytes`/`content_sha256` 必须对调用方传入的
/// `content_raw`（DB `messages.content` 原始 JSON 字符串，未经任何重序列化）计算——与
/// `SNAPSHOT_SEND_BUDGET_BYTES`/`HISTORY_SEND_BUDGET_BYTES` 的尺寸预算同口径（UTF-8 字节数）。
fn build_content_ref(message_id: i64, revision: i64, content_raw: &str) -> Value {
    serde_json::json!({
        "message_id": message_id,
        "revision": revision,
        "content_sha256": sha256_hex_lower(content_raw.as_bytes()),
        "total_bytes": content_raw.len(),
    })
}

/// msgfix1 T4（M0 §10.5 `msg.fetch.error`）：六值 code 枚举的通用构造——`current_ref` 仅
/// `stale_revision` 必填，其余 code 整个省略该字段（不是 `null`，见样张
/// `data-plane-v1.json` 的 `msg_fetch_error_not_found`——msgfix1 T7 B5：pending 版已合入
/// 正式文件并删除，改指正式文件）。
fn msg_fetch_error_payload(code: &str, current_ref: Option<Value>) -> Value {
    let mut frame = serde_json::json!({"t": "msg.fetch.error", "code": code});
    if let Some(current_ref) = current_ref {
        frame["current_ref"] = current_ref;
    }
    frame
}

/// msgfix1 T4（M0 §10.5 `msg.chunk`）+ §10.9（重组安全/offset 续传）：把消息原文按
/// `CHUNK_RAW_BYTES` 切成有序分片，逐片产 `msg.chunk` 密文体。`start_offset` 支持 `msg.fetch`
/// 的 `offset` 续传语义——只从这个偏移开始切，不重复发送客户端已经拿到的前缀；越界（
/// `start_offset >= content.len()`）时钳到末尾。`content_sha256`/`total_bytes` 恒对**全量**
/// `content`（不是从 `start_offset` 起的子串）计算，与 `content_ref`（M0 §10.6）同一份数，供
/// 客户端做§10.9「完成后整体校验」。切片按裸字节边界切，不保证落在 UTF-8 字符边界上——
/// `msg.chunk` 传输的是不透明字节序列，客户端要拼完全部分片才按 UTF-8 解释，切一半的多字节
/// 序列本就是合法的中间态。
///
/// **恒返回非空 `Vec`**——`start_offset >= content.len()`（客户端已经拿到全部内容、纯粹用同一
/// `offset` 收尾确认，或消息原文恰好为空）时不会退化成空序列悄悄什么都不发：产出唯一一片
/// `chunk_len: 0`、`offset: total_bytes` 的终态空分片。这片本身就满足 §10.9 的重组连续性
/// （它是这次响应的第一也是最后一片，没有"上一片"要对齐）与整体 SHA-256 校验（客户端此前已
/// 经把 `content` 拼全，这片只是补一个显式终态信号，不携带新字节），让调用方（`handle_msg_
/// fetch_at`）永远有恰好一帧可以标记 `final_frame: true` 去释放单飞行占用——不需要在没有分片
/// 可发时额外分叉出"到底该不该占单飞行槽位"的第二套判断。
fn build_msg_chunks(
    message_id: i64,
    revision: i64,
    content: &[u8],
    start_offset: usize,
) -> Vec<Value> {
    let total_bytes = content.len();
    let content_sha256 = sha256_hex_lower(content);
    let mut chunks = Vec::new();
    let mut offset = start_offset.min(total_bytes);
    while offset < total_bytes {
        let end = (offset + CHUNK_RAW_BYTES).min(total_bytes);
        let slice = &content[offset..end];
        chunks.push(serde_json::json!({
            "t": "msg.chunk",
            "message_id": message_id,
            "revision": revision,
            "content_sha256": content_sha256,
            "total_bytes": total_bytes,
            "offset": offset,
            "chunk_len": slice.len(),
            "bytes_b64": STANDARD.encode(slice),
        }));
        offset = end;
    }
    if chunks.is_empty() {
        chunks.push(serde_json::json!({
            "t": "msg.chunk",
            "message_id": message_id,
            "revision": revision,
            "content_sha256": content_sha256,
            "total_bytes": total_bytes,
            "offset": total_bytes,
            "chunk_len": 0,
            "bytes_b64": "",
        }));
    }
    chunks
}

/// msgfix1 T4（M0 §10.9 单飞行 + 超时释放）：某 session 现有的在途 fetch 记录是否仍然"占着"
/// 单飞行槽位——超时（`MSG_FETCH_INFLIGHT_TIMEOUT_MS`）后视为已释放。纯函数，供不依赖
/// provider/socket 的单元测试直接钉超时边界（含时钟回拨的 `saturating_sub` 防御）。
fn msg_fetch_inflight_is_active(accepted_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(accepted_at_ms) < MSG_FETCH_INFLIGHT_TIMEOUT_MS
}

/// msgfix1 T4（M0 §10.9 per-source 60s 字节预算）：纯函数版本——先按
/// `now_ms - MSG_FETCH_BYTE_BUDGET_WINDOW_MS` 剪掉窗口外的陈旧条目，再判断加上
/// `requested_bytes` 是否仍在 `MSG_FETCH_BYTE_BUDGET_PER_WINDOW` 之内；放行时把这次请求记进
/// 窗口。返回 `(是否放行, 剪枝并可能追加记账后的窗口)`——调用方在同一次加锁临界区内原地替换
/// map 里的 `VecDeque`，保证"剪枝 + 判定 + 放行记账"三步原子。
fn msg_fetch_budget_admit(
    mut window: VecDeque<(u64, usize)>,
    now_ms: u64,
    requested_bytes: usize,
) -> (bool, VecDeque<(u64, usize)>) {
    while let Some((at_ms, _)) = window.front() {
        if now_ms.saturating_sub(*at_ms) >= MSG_FETCH_BYTE_BUDGET_WINDOW_MS {
            window.pop_front();
        } else {
            break;
        }
    }
    let used: u64 = window.iter().map(|(_, bytes)| *bytes as u64).sum();
    let admit = used.saturating_add(requested_bytes as u64) <= MSG_FETCH_BYTE_BUDGET_PER_WINDOW;
    if admit {
        window.push_back((now_ms, requested_bytes));
    }
    (admit, window)
}

/// msgfix1 T4 返修②（skeptic 补审）：check-and-insert——若 `(session, command_id)` 已经在
/// 账本里（意味着这个 command_id 之前已经被 `handle_msg_fetch_at` 处理过一次，无论那次成功
/// 还是失败），返回 `false`（拒绝复用）；否则记入账本（容量满时 FIFO 淘汰最老一条）并返回
/// `true`（放行——本次是这个 `(session, command_id)` 第一次被处理）。见 `MsgFetchCommandLedger`
/// doc。
fn msg_fetch_command_ledger_admit(
    state: &GatewayInnerState,
    session: &str,
    command_id: &str,
) -> bool {
    let mut ledger = lock(&state.msg_fetch_command_ledger);
    let key = (session.to_owned(), command_id.to_owned());
    if ledger.seen.contains(&key) {
        return false;
    }
    if ledger.order.len() >= MSG_FETCH_COMMAND_LEDGER_CAPACITY {
        if let Some(oldest) = ledger.order.pop_front() {
            ledger.seen.remove(&oldest);
        }
    }
    ledger.seen.insert(key.clone());
    ledger.order.push_back(key);
    true
}

/// actionable 块型名单（设计稿 §A「权限请求/确认卡」）：`approval`（审批卡，对应用户需要放行/
/// 拒绝的权限请求，见 `db::Block::Approval`）、`decision_card`（ask/dispatch_confirm 两种
/// kind 共用同一块型，对应确认卡/决策卡，见 `db::Block::DecisionCard`）、`scope_change`
/// （msgfix1 T3 返修 P0-1：来自 `AgentEvent::NeedsDecision`，UI 呈现「接受并继续」这类需要
/// 用户放行的动作，见 `db::Block::ScopeChange`/`agent_event::ScopeChange`——语义上与
/// approval/decision_card 同级，都是"此刻等待用户处理"）。这三类块承载"需要用户立即行动"
/// 的语义，preview 降级时必须原样保留，绝不能被截断/丢弃吞掉。其余块型
/// （text/image/thinking/tool/run_card/team_run/dispatch_card/lead_summary/coding_task/
/// context_compacted/context_truncated/run_terminal）均为状态展示或历史记录，不携带"此刻
/// 必须由用户处理"的语义，preview 降级时按块级选择规则处理（text 特殊，其余丢弃）。
fn is_actionable_block_type(block_type: &str) -> bool {
    matches!(block_type, "approval" | "decision_card" | "scope_change")
}

/// msgfix2 U1 修单三（G2·独立审查残余 P1）：`extract_tool_milestones` 里 L1 聚合器"哪些事件
/// 的原始内容允许进入摘要"这个判定，在本函数出现之前只以 `debug_assert!(is_actionable_
/// block_type(..))` 的形式存在——release 构建这行整体消失，实际路由行为仍由各 match 分支各自
/// 手写的逻辑决定，两者只是碰巧一致，没有代码层面的绑定：白名单改了却忘记同步改分支（或反
/// 过来）会在生产环境悄无声息地漂移，不会有任何信号。
///
/// 本函数把这道判定收拢成一次**运行时真调用**（release 构建同样生效，不是只在 debug 构建下
/// 才存在的断言）：`true` = `block_type` 不是 actionable 类型，事件内容允许正常参与 L1 聚合；
/// `false` = actionable，调用方必须走受限路径（如 approval 只产生不带字段的计数 delta）或
/// 完全跳过（如 scope_change 零 delta 贡献）——调用方现在真的 `if` 这个返回值来决定分支行为，
/// 不是"写了个断言，两条分支各自硬编码同样的结论，靠人眼保持一致"。内部直接复用
/// `is_actionable_block_type`（刀 1 单点白名单），不是另起一份判断表。
fn event_joins_l1_aggregation(block_type: &str) -> bool {
    !is_actionable_block_type(block_type)
}

/// 块级 preview 构造（纯函数，设计稿 §A）：actionable 块原样保留；首个非 actionable 的 text
/// 块 UTF-8 安全截断到 `OVERSIZED_PREVIEW_TEXT_HEAD_BYTES` 后追加"内容较长"提示，合并进同一个
/// text 块（与样张 `msg_completed_with_content_ref` 的 `blocks[0].text` 形状一致）；其余块型
/// （tool 等自由长文本载体）与超出首个的 text 块一律丢弃——只留 content_ref 可按需拉全文。
/// `truncate_utf8` 按 `str::is_char_boundary` 回退，Rust `str` 恒为合法 UTF-8，因此这里的
/// 截断天然不会劈开任何码点，也就不存在"劈开代理对"的可能（代理对是 UTF-16 概念，Rust 字符串
/// 里一个 Unicode 标量值要么整体保留、要么整体不含，见函数级测试）。
fn build_oversized_preview_blocks(blocks: &Value) -> Value {
    let mut preview: Vec<Value> = Vec::new();
    let mut preview_text: Option<String> = None;
    if let Some(items) = blocks.as_array() {
        for block in items {
            let Some(block_type) = block.get("type").and_then(Value::as_str) else {
                continue;
            };
            if is_actionable_block_type(block_type) {
                preview.push(block.clone());
                continue;
            }
            if block_type == "text" && preview_text.is_none() {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    preview_text = Some(truncate_utf8(text, OVERSIZED_PREVIEW_TEXT_HEAD_BYTES));
                }
            }
        }
    }
    let mut combined = preview_text.unwrap_or_default();
    combined.push_str(OVERSIZED_PREVIEW_TRUNCATION_NOTICE);
    preview.push(serde_json::json!({"type": "text", "text": combined}));
    Value::Array(preview)
}

/// 把一条已超预算的 `msg.completed`/history-row payload 降级为 preview + content_ref
/// （设计稿 §A）。`payload` 必须已含 `message_id`/`role`/`blocks`（可选 `agent`）；返回值在此
/// 基础上替换 `blocks` 为 preview 版本、附加 `content_ref`。**构造后整帧仍须过预算**——preview
/// 本身超预算时（如 actionable 块本身就很大）进一步退化为"仅提示块 + content_ref"，ref 永不
/// 丢（设计稿 §A 明文约束）。
/// 二次退化的最终形态——仅一个提示文本块，不含任何原始内容（连 actionable 块也不留）。
/// msgfix1 T3 返修 P0-1：`downgrade_to_preview_payload`（msg.completed 口）与 history
/// 单行降级（`build_history_page_with_limit`）共用同一常量形态，保证两口"仅提示块 +
/// content_ref"的最终表示逐字节一致——ref 永不丢是两口共同的硬不变量，不是各自各写一份。
fn notice_only_preview_blocks() -> Value {
    serde_json::json!([{"type": "text", "text": OVERSIZED_PREVIEW_TRUNCATION_NOTICE}])
}

fn downgrade_to_preview_payload(payload: &Value, content_ref: Value, t: &str) -> Value {
    let blocks = payload
        .get("blocks")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let mut degraded = payload.clone();
    degraded["blocks"] = build_oversized_preview_blocks(&blocks);
    degraded["content_ref"] = content_ref;
    if milestone_frame_bytes(t, &degraded) > SNAPSHOT_SEND_BUDGET_BYTES {
        degraded["blocks"] = notice_only_preview_blocks();
    }
    degraded
}

/// `agent` = 落库时的 `agent_name_snapshot`（如 `"Claude"`/`"Codex"`）——`Some` 时插入
/// optional `"agent"` 键，`None` 时整个键省略（不是 `null`），保持老消费方（无该键时按老形状
/// 解析）向后兼容。user 消息 / 无 agent 归属的场景走 `None`。
///
/// msgfix1 T3：`revision`/`content_raw` 用于预计算 content_ref（M0 §10.6），挂在私有键
/// `MSG_COMPLETED_REF_SOURCE_KEY` 下随 payload 一起传递给 `enqueue_milestone_item`——该函数
/// 决定是否需要把消息降级为 preview，需要时才会真正把 content_ref 摆上顶层；不需要时会把这个
/// 私有键整个剥掉，绝不流到 wire（非超预算消息不该带 content_ref，见该常量文档）。
///
/// msgfix2 U1（M0 §10.10）：`revision` 现在**恒**摆上顶层（不再只塞进 content_ref 内部）——
/// 与 content_ref.revision 同值并存（旧客户端不识别未知字段仍照常渲染其余字段，见该条文
/// 「前向兼容」段）。这是本函数覆盖的两个产出点（live 首发 `publish_msg_completed_milestone`
/// / 重连补发批 `publish_msg_and_card_replay_rows`）共用同一份逻辑之所以只改一处就能覆盖两点
/// 的原因；第三点（history 分页）走独立的 `history_message_from_parts`，同样补了顶层 revision。
pub(crate) fn build_msg_completed_payload(
    message_id: i64,
    role: &str,
    blocks: Value,
    agent: Option<&str>,
    revision: i64,
    content_raw: &str,
) -> Value {
    let mut payload = serde_json::json!({
        "message_id": message_id,
        "role": role,
        "blocks": blocks,
        "revision": revision,
    });
    if let Some(agent) = agent {
        payload["agent"] = Value::String(agent.to_owned());
    }
    payload[MSG_COMPLETED_REF_SOURCE_KEY] = build_content_ref(message_id, revision, content_raw);
    payload
}

/// msgfix1 T5（缺口④·M0 §10.7）：`revision` 参与派生——`revision==1` 与旧派生逐字节相同
/// （存量零扰动，client-msg-id-derivation-v1.json 首条既有向量钉死），`revision>1` 在 name
/// 末尾追加 `|<revision>`（与合入的 revision KAT 向量互证）。每个 revision 天然是一个新
/// client_msg_id，relay 幂等去重不会把"终态改写后的重发"当成旧事件吞掉。
pub(crate) fn derive_msg_completed_client_msg_id(
    session_id: &str,
    dedup_key: &str,
    revision: i64,
) -> String {
    if revision == 1 {
        derive_client_msg_id(&format!("msg.completed|{session_id}|{dedup_key}"))
    } else {
        derive_client_msg_id(&format!(
            "msg.completed|{session_id}|{dedup_key}|{revision}"
        ))
    }
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
    revision: i64,
    content_raw: &str,
) {
    record_test_publish("msg.completed");
    let client_msg_id = derive_msg_completed_client_msg_id(session_id, dedup_key, revision);
    let payload =
        build_msg_completed_payload(message_id, role, blocks, agent, revision, content_raw);
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

/// msgfix2 U1b：L1 聚合器生产接线的挂载点——`GatewayInnerState` 是模块私有类型、`GATEWAY`
/// 是模块私有 static，lib.rs 拿不到 `&GatewayInnerState`，因此不能直接调
/// `configure_activity_summary_writer`；这个 `pub(crate)` 包装函数是唯一的跨模块入口，同
/// `install_event_sink` 同一惯例（`setup()` 之后才有 `GATEWAY.get()`，调用方——lib.rs——在
/// `remote_gateway::setup(...)` 紧随其后调用本函数，传入捕获真实 DB 连接的 writer）。
pub(crate) fn install_activity_summary_writer(writer: ActivitySummaryWriter) {
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    configure_activity_summary_writer(&inner.state, writer);
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

/// 缺口②：提示文案拼上被淘汰块数——`"（快照已截断，仅含最近内容）(N 块折叠)"`。`folded_count`
/// 是本次收敛整体淘汰掉的块数（不含提示块自身）。
fn snapshot_truncated_notice_text(folded_count: usize) -> String {
    format!("{SNAPSHOT_TRUNCATED_NOTICE}({folded_count} 块折叠)")
}

/// 缺口②：`shrink_snapshot_blocks_to_budget` 的"淘汰候选优先级"分类——只有 `Block::Text`
/// （面向用户的叙述正文）算"text 叙述块"、次先淘汰；其余非 actionable 块型（`Tool`/
/// `Thinking`/`DispatchCard`/…，笼统称"工具类块"）优先淘汰。
fn is_snapshot_narrative_text_block(block: &crate::db::Block) -> bool {
    matches!(block, crate::db::Block::Text { .. })
}

/// R2（msgfix2 整盘审）：`shrink_snapshot_blocks_to_budget` 的第三级判据——actionable 块
/// （approval/decision_card/scope_change，见 `is_actionable_block_type`）永不进淘汰候选序列，
/// 不管它是不是 `Block::Text`（三者都不是，但显式排除比"恰好不落进 narrative 分类"更稳）。
/// 复用 `is_actionable_block_type`（block_type 字符串白名单单点）而不是另起一份
/// `matches!(block, Block::Approval{..}|Block::DecisionCard{..}|Block::ScopeChange{..})`——
/// `Block` 是带 `#[serde(tag = "type")]` 的 tagged union，序列化取回 `"type"` 字段字符串
/// 就是该块在 wire 上的真实类型标签，与白名单判据同一口径，不会因为两份手写列表各自维护而
/// 漂移（`build_oversized_preview_blocks` 也是同一白名单同一姿势，只是那边天然是 `Value`）。
fn is_snapshot_actionable_block(block: &crate::db::Block) -> bool {
    serde_json::to_value(block)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|block_type| is_actionable_block_type(block_type))
        })
        .unwrap_or(false)
}

/// P0-b 返工②·微返工第 3 轮重写·缺口②（v4.1）再重写：`build_snapshot_payload` 的收敛实体
/// ——见该函数文档"尺寸预算"一段的顺序论证。`session`/`run_id`/`through_run_seq` 只用于重算
/// 试探帧的序列化尺寸（跟真正发出去的信封结构一致，不是只测 `blocks` 数组本身），不参与
/// 截断判断本身。
///
/// **计量对象含 `t`**：`frame_bytes` 内部直接把 `milestone_payload` 随后会合并进去的
/// `"t":"snapshot"` 字段一并算进去——否则预算判断用的是裸 payload，成品还要再被 `t` 字段
/// 撑大一截，单个 32KiB text 块这类边界情形会被判定"在预算内"但实际成品超预算。
///
/// **按类型选择淘汰、保留原块顺序（缺口②·codex P2 修正，取代旧"保尾弃头"；R2·msgfix2 整盘
/// 审改三级）**：淘汰候选按"类型优先级、同优先级内由旧到新"排出一个固定顺序——**三级**：
/// ① 工具类块（非 actionable 且非 `Block::Text` 的一切，如 `Tool`/`Thinking`/`DispatchCard`）
/// 最先淘汰；② text 叙述块（`Block::Text`）次之；③ actionable 块（approval/decision_card/
/// scope_change，`is_snapshot_actionable_block`）**永不进候选序列**——不管预算多紧，都不会
/// 被淘汰到。旧实现是二元判据（只分"text"/"非 text"），actionable 块（如 approval 卡）恰好
/// 不是 `Block::Text`，被并进"工具类块"那一档跟真正的工具输出一起最先陪跑淘汰，与
/// `is_actionable_block_type`（L0 白名单：actionable 块的原始内容不允许被丢弃/降级）直接
/// 打架——一条超预算快照如果同时有 approval 卡和大量 text/tool 块，旧实现可能把 approval 卡
/// 也淘汰掉，用户永远看不到那张等待批准的卡。同一优先级内仍延续旧实现"越老越先丢"的时间
/// 近因偏好。沿这个候选顺序累计淘汰，直到剩余块的边际字节总和落回预算内（或非 actionable
/// 候选全部淘汰尽——见下方"允许丢尽"段：即便如此，actionable 块依旧不在候选序列里，不受
/// 影响）。**返回时严格按 `blocks` 原始顺序过滤保留集合**——本函数只决定"淘汰哪些"，不改变
/// 被保留块之间的相对顺序，渲染顺序不受影响。提示文案（`snapshot_truncated_notice_text`）
/// 带上被淘汰的块数计数。已删除旧版"已落库前缀"判据的位置——实勘本函数从未真正实现过按
/// "是否已落库"分层的淘汰（那是一个从未落地的设想，无元数据支撑），此处不留任何相关分支。
/// 完整 ref 化（把丢弃的块换成可按需拉取的引用）仍不做，维持现状（丢弃即真丢弃）。
///
/// **允许丢尽**：预算连提示块自身都装不下的极端情形（见下方论证，当前输入契约下不可达）
/// 会把业务块全部淘汰，成品只剩提示块 + 水印字段。
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

    // 淘汰候选顺序（R2 三级）：工具类块（由旧到新）排在前面，text 叙述块（由旧到新）排在
    // 中间，actionable 块（approval/decision_card/scope_change）整个不进这个列表——见
    // `is_snapshot_narrative_text_block`/`is_snapshot_actionable_block` 与函数文档"按类型
    // 选择淘汰"一段。
    let eviction_order: Vec<usize> = bounded
        .iter()
        .enumerate()
        .filter(|(_, block)| {
            !is_snapshot_narrative_text_block(block) && !is_snapshot_actionable_block(block)
        })
        .map(|(idx, _)| idx)
        .chain(
            bounded
                .iter()
                .enumerate()
                .filter(|(_, block)| {
                    is_snapshot_narrative_text_block(block) && !is_snapshot_actionable_block(block)
                })
                .map(|(idx, _)| idx),
        )
        .collect();

    // 提示块先计入预算，且用"全部块都被淘汰"这一最坏位数场景估算提示文案里的计数长度——
    // 保证选块阶段绝不会低估提示块开销（真实淘汰数 ≤ 这个估算值，真实文案只会更短，选块
    // 阶段判定"装得下"的结果不会被最终文案反过来撑爆）。
    //
    // P0-b 微返工第 4 轮如实论证（缺口②延续同一结论，量级不变）：下面这行算出的
    // `base_bytes` 在当前输入契约下不可能超过 `SNAPSHOT_PAYLOAD_BUDGET_BYTES`（32,768B）
    // ——`if base_bytes <= ...` 这条分支保留作纵深防御，不代表判定它会被触发。论证要素：
    // `session` ≤128 字节（`SESSION_ID_MAX_BYTES` 挡住超长值）、`run_id` 恒 38 字节固定
    // 格式、`through_run_seq` 十进制最多 20 位、提示文案是固定短字符串加个位数不多的淘汰
    // 计数——四项加 JSON 结构开销，实测量级是几百字节，比 32,768B 预算低接近两个数量级。
    let worst_case_notice = crate::db::Block::Text {
        text: snapshot_truncated_notice_text(bounded.len()),
    };
    let base_bytes = frame_bytes(std::slice::from_ref(&worst_case_notice));

    let mut kept = vec![true; bounded.len()];
    let mut kept_bytes: usize = bounded
        .iter()
        .map(|block| {
            serde_json::to_string(block)
                .map(|json| json.len() + 1) // +1：数组分隔逗号
                .unwrap_or(usize::MAX)
        })
        .sum();
    let mut evicted_count = 0usize;

    if base_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES {
        let budget_left_for_blocks = SNAPSHOT_PAYLOAD_BUDGET_BYTES - base_bytes;
        for idx in eviction_order {
            if kept_bytes <= budget_left_for_blocks {
                break;
            }
            let marginal = serde_json::to_string(&bounded[idx])
                .map(|json| json.len() + 1)
                .unwrap_or(usize::MAX);
            kept[idx] = false;
            kept_bytes = kept_bytes.saturating_sub(marginal);
            evicted_count += 1;
        }
    } else {
        // 极端兜底（论证同上）：全部业务块丢尽，只剩提示块。
        kept = vec![false; bounded.len()];
        evicted_count = bounded.len();
    }

    let notice = crate::db::Block::Text {
        text: snapshot_truncated_notice_text(evicted_count),
    };
    let mut result = Vec::with_capacity(1 + kept.iter().filter(|keep| **keep).count());
    result.push(notice);
    // 按 `bounded` 原始顺序过滤——被保留块之间的相对顺序不变，不按淘汰候选顺序重排。
    result.extend(
        bounded
            .into_iter()
            .zip(kept)
            .filter(|(_, keep)| *keep)
            .map(|(block, _)| block),
    );
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
            // msgfix1 T7 B2：msg.completed（4592 一带经 enqueue_milestone_item）与 history
            // 查询（5653 一带）共用本函数——两口都要在截断发生时带上可见化标记。
            *output = Value::String(truncate_utf8_with_marker(text, OUTPUT_TRUNCATE_BYTES));
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

/// history wire message 的通用构造：`content_ref` 为 `None` 时整个键省略（不是 `null`）——
/// 普通大小的消息不带该字段（M0 §10.6「可选字段」），只有降级为 preview 时才附加。
///
/// msgfix2 U1（M0 §10.10）：`revision` 恒摆上顶层——history row 是顶层 revision 三个产出点
/// 之一（另两点是 live 首发/重连补发批，都走 `build_msg_completed_payload`）。
fn history_message_from_parts(
    message_id: i64,
    role: &str,
    blocks: Value,
    revision: i64,
    content_ref: Option<Value>,
) -> Value {
    let mut message = serde_json::json!({
        "message_id": message_id,
        "role": role,
        "blocks": blocks,
        "revision": revision,
    });
    if let Some(content_ref) = content_ref {
        message["content_ref"] = content_ref;
    }
    message
}

fn history_message(row: &SessionHistoryRow) -> Value {
    history_message_from_parts(
        row.message_id,
        &row.role,
        truncate_history_tool_outputs(row.content_json.clone()),
        row.revision,
        None,
    )
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
        let row_has_older = database_has_more || index + 1 < rows.len();
        let mut message = history_message(row);
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
            // msgfix1 T3（设计稿 §A）：超预算不再整条丢弃——降级为块级 preview +
            // content_ref，让远端至少看得见这条消息、按需可拉全文。`oversized_dropped`
            // 计数器语义随之改为「本条降级为 preview 的次数」，继续保持指标可观测。
            let content_ref = build_content_ref(row.message_id, row.revision, &row.content_raw);
            let preview_message = history_message_from_parts(
                row.message_id,
                &row.role,
                build_oversized_preview_blocks(&row.content_json),
                row.revision,
                Some(content_ref.clone()),
            );
            let preview_single = history_payload(
                session,
                before_message_id,
                std::slice::from_ref(&preview_message),
                row_has_older.then_some(row.message_id),
            );
            oversized_dropped += 1;
            // msgfix1 T3 返修 P0-1：块级 preview 本身仍超预算时（如 actionable 块本身巨大）
            // 不再丢行——退化到与 `downgrade_to_preview_payload`（msg.completed 口）共用
            // 的同一"仅提示块 + content_ref"终态（`notice_only_preview_blocks`）。这一级在
            // 设计上必然装得下（content_ref 固定四字段 + 定长提示文案 + 有界 session/游标
            // 开销，远小于 44KiB HISTORY_SEND_BUDGET_BYTES）——ref 永不丢是硬不变量，这里
            // 不再有"如实丢弃"这条退路。
            if serde_json::to_vec(&preview_single)
                .map(|json| json.len())
                .unwrap_or(usize::MAX)
                > HISTORY_SEND_BUDGET_BYTES
            {
                message = history_message_from_parts(
                    row.message_id,
                    &row.role,
                    notice_only_preview_blocks(),
                    row.revision,
                    Some(content_ref),
                );
            } else {
                message = preview_message;
            }
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

/// msgfix1 T4（M0 §10.4/§10.9）：`msg.fetch` 的完整校验链 + 分片入队——生产入口，`now_ms` 取
/// 真实时钟。**没有直接返回值**：`msg.fetch` 不像 `input.send`/`control.stop` 那样回一个
/// `input.ack`——按 M0 §10.5，它唯一的应答形态是 `reply` kind 下的 `msg.chunk`/
/// `msg.fetch.error`，全部经 `try_enqueue_reply` 送进 `state.reply_queue`，由
/// `drain_reply_queue` 异步发出（见 `handle_command_envelope` 该分支 doc）。
fn handle_msg_fetch(
    inner: &Inner,
    session: &str,
    command_id: &str,
    message_id: i64,
    requested_revision: i64,
    offset: usize,
) {
    handle_msg_fetch_at(
        inner,
        session,
        command_id,
        message_id,
        requested_revision,
        offset,
        now_unix_ms(),
    );
}

/// `handle_msg_fetch` 的可测试核心——`now_ms` 显式传入，供单元测试钉死单飞行超时/字节预算
/// 窗口的边界（同 `is_control_stop_stale` 既有姿势）。校验链顺序固定（任务书 §3 明文 + 返修
/// 补的两条）：
/// ⓪ command_id 复用账本（返修②）→ busy；①归属(active repo)+消息存在/归属+会话软删 →
/// forbidden/not_found/soft_deleted；②revision 校验 → stale_revision(+current_ref)；
/// ③total_bytes 上限 → too_large；③.5 offset 越界（返修④，M0 §10.4）→ not_found；④单飞行+60s
/// 字节预算 → busy；⑤全过 → 切片入队。
fn handle_msg_fetch_at(
    inner: &Inner,
    session: &str,
    command_id: &str,
    message_id: i64,
    requested_revision: i64,
    offset: usize,
    now_ms: u64,
) {
    let state = &inner.state;
    // msgfix1 T4 返修③（skeptic 补审）：入队那一刻的连接 generation 一并存进每条
    // `ReplyQueueItem`——`drain_reply_queue` 出队时据此判断这条数据是不是跨连接残留（见该
    // 函数 doc）。
    let connection_generation = state.connection_generation_snapshot();

    // 步骤⓪（返修②·skeptic 补审）：command_id 复用账本——这个 `(session, command_id)`
    // 之前处理过，一律拒绝复用（回 busy，引导客户端换新 command_id）。必须放在最前面：一旦
    // 放行，下面每一条早退路径都会把这次 `(session, command_id)` 记进 `msg_fetch_inflight`/
    // `reply_queue`，而 `generation` 判定的正确性前提就是"同一 `(session, command_id)` 只会
    // 被这个函数处理一次"。
    if !msg_fetch_command_ledger_admit(state, session, command_id) {
        // 这条 busy 回复本身也走一次全新 generation——它不会被写进 `msg_fetch_inflight`
        // （压根没到步骤④），`generation` 字段只是满足 `ReplyQueueItem` 的结构要求，不参与
        // 任何后续比对。
        let generation = state
            .msg_fetch_generation_counter
            .fetch_add(1, Ordering::Relaxed);
        if !try_enqueue_reply(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: msg_fetch_error_payload("busy", None),
                final_frame: true,
                generation,
                connection_generation,
            },
        ) {
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }
    let generation = state
        .msg_fetch_generation_counter
        .fetch_add(1, Ordering::Relaxed);

    let reply_error = |code: &str, current_ref: Option<Value>| {
        if !try_enqueue_reply(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: msg_fetch_error_payload(code, current_ref),
                final_frame: true,
                generation,
                connection_generation,
            },
        ) {
            // reply_queue 已满到连这条终态错误都塞不进去——双重饱和的极端情形（见
            // `REPLY_QUEUE_CAPACITY` doc），如实计数，不假装发出去了。此刻这个 session 还没有
            // 被本次请求占用单飞行槽位（这条早退发生在①-③步，占用要等第④步才写入），不需要
            // 额外释放。
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
        }
    };

    // 步骤①：active repo 归属闸——先于任何消息级查询，越权请求连"这个 message_id 存不存在"
    // 都探不出来（不泄露存在性差异，同 `command_session_allowed` 既有姿势）。
    if !command_session_allowed(inner, session) {
        reply_error("forbidden", None);
        return;
    }
    let (content_raw, revision, session_deleted) =
        match (inner.message_fetch_provider)(session, message_id) {
            Ok(MessageForFetchResult::Found {
                content_raw,
                revision,
                session_deleted,
            }) => (content_raw, revision, session_deleted),
            Ok(MessageForFetchResult::WrongSession) => {
                reply_error("forbidden", None);
                return;
            }
            Ok(MessageForFetchResult::NotFound) => {
                reply_error("not_found", None);
                return;
            }
            Err(error) => {
                // 查询本身失败（DB 错误）——fail-closed，不当"未知即放行"；也不把内部错误细节
                // 泄露给远端，`forbidden` 是六值枚举里最贴近"拒绝、不解释原因"的现成 code。
                eprintln!("remote gateway: msg.fetch lookup failed — {error}");
                reply_error("forbidden", None);
                return;
            }
        };
    if session_deleted {
        reply_error("soft_deleted", None);
        return;
    }

    // 步骤②：revision 校验——`offset` 续传按当前 revision 校验，不符即 stale。
    if requested_revision != revision {
        reply_error(
            "stale_revision",
            Some(build_content_ref(message_id, revision, &content_raw)),
        );
        return;
    }

    // 步骤③：total_bytes 上限。
    if content_raw.len() > MSG_FETCH_TOTAL_BYTES_LIMIT {
        reply_error("too_large", None);
        return;
    }

    // 步骤③.5（返修④，M0 §10.4 新条文）：offset 越界——严格大于 total_bytes 是协议误用（不是
    // "已经拿到全部内容"这种合法收尾态，那种情形是 `offset == total_bytes`，继续走步骤⑤产出
    // 单片零长终态帧，见 `build_msg_chunks` doc）。回 `not_found` 而不是此前的静默钳位——更
    // 诚实地告诉客户端这个请求本身不合法，而不是假装成功却什么都不做。
    if offset > content_raw.len() {
        reply_error("not_found", None);
        return;
    }

    // 步骤④：单飞行闸（per-session）+ 60s 字节预算（msgfix1 T7 B1：gateway 全局聚合，不再
    // per-session 分桶）——两把锁按固定顺序（inflight 先于 budget）依次拿、依次放，不嵌套持锁
    // 跨越业务逻辑，避免与其它持锁路径产生锁序分歧。
    let total_bytes = content_raw.len();
    {
        let mut inflight = lock(&state.msg_fetch_inflight);
        if let Some(existing) = inflight.get(session) {
            if msg_fetch_inflight_is_active(existing.accepted_at_ms, now_ms) {
                drop(inflight);
                reply_error("busy", None);
                return;
            }
        }
        // 返修②（skeptic 补审）：接管前记下被取代那次接受的 generation——释放锁之后立刻拿它去
        // 清 `reply_queue` 里同 generation 的残片（见 `purge_stale_reply_queue_generation`
        // doc），避免旧 fetch 的分片继续被正常 drain 发给已经不再关心它们的客户端。
        let superseded_generation = inflight.get(session).map(|entry| entry.generation);
        inflight.insert(
            session.to_owned(),
            MsgFetchInflightEntry {
                command_id: command_id.to_owned(),
                accepted_at_ms: now_ms,
                generation,
            },
        );
        drop(inflight);
        if let Some(superseded_generation) = superseded_generation {
            purge_stale_reply_queue_generation(state, session, superseded_generation);
        }
    }
    let admitted = {
        let mut budget_slot = lock(&state.msg_fetch_byte_budget);
        let window = std::mem::take(&mut *budget_slot);
        let (admit, window) = msg_fetch_budget_admit(window, now_ms, total_bytes);
        *budget_slot = window;
        admit
    };
    if !admitted {
        // 预算不放行——释放刚占的单飞行槽位（这次请求从未真正开始传输，不该占着槽位等
        // 30 秒超时才被动释放），再回 busy。
        clear_msg_fetch_inflight_if_matches(state, session, generation);
        reply_error("busy", None);
        return;
    }

    // 步骤⑤：全过——切片入队。`build_msg_chunks` 恒返回非空序列（见其 doc），因此
    // `last_index` 恒可算，`final_frame` 恒有归宿。
    let chunks = build_msg_chunks(message_id, revision, content_raw.as_bytes(), offset);
    let last_index = chunks.len() - 1;
    let mut fully_enqueued = true;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let enqueued = try_enqueue_reply_chunk(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: chunk,
                final_frame: index == last_index,
                generation,
                connection_generation,
            },
        );
        if !enqueued {
            fully_enqueued = false;
            break;
        }
    }
    if !fully_enqueued {
        // M0 §10.9：满→丢整个传输并回 busy 终态（可观测，不静默）。已经入队的前缀分片仍会
        // 被正常发出——客户端按同一 command_id 收到"部分分片 + busy 终态"，§10.9 的整体
        // SHA-256 校验天然通不过，会弃拉并可用同一/新 command_id 重试，不会展示半截内容。
        if !try_enqueue_reply(
            state,
            ReplyQueueItem {
                session: Some(session.to_owned()),
                command_id: command_id.to_owned(),
                payload: msg_fetch_error_payload("busy", None),
                final_frame: true,
                generation,
                connection_generation,
            },
        ) {
            // 双重饱和——连兜底 error 都塞不进去。释放单飞行槽位，不然会一直卡到超时；
            // 如实计数，不假装已经通知到客户端。
            state.reply_queue_dropped.fetch_add(1, Ordering::Relaxed);
            clear_msg_fetch_inflight_if_matches(state, session, generation);
        }
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
            // msgfix1 T3（缺口①）：入队失败（门控关闭/队列满断连）必须回真实失败 ack——
            // 此前无条件回 `Ok`，客户端会以为帧已在路上，实则从未入队、永不到达。
            let enqueued = enqueue_prebuilt_live_for_upstream(
                state,
                &inner.upstream_tx,
                MilestoneItem {
                    session: Some(session.to_owned()),
                    t: "history".to_owned(),
                    payload: page.payload,
                    client_msg_id,
                },
            );
            Some(input_ack_json(
                &command_id,
                if enqueued {
                    AckOutcome::Ok
                } else {
                    AckOutcome::Failed
                },
            ))
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
        ("control", Some("msg.fetch")) => {
            // msgfix1 T4（M0 §10.4）：`{session, message_id, revision, offset}` 四字段——字段
            // 缺失/类型不符是协议违例（走既有 `failed()`），与业务级拒绝（越权/软删/过大/
            // stale/busy——那些走 `msg.fetch.error` reply，不是这里）是两层不同的失败。
            let Some(session) = payload.get("session").and_then(Value::as_str) else {
                return failed();
            };
            if session.len() > SESSION_ID_MAX_BYTES {
                return failed();
            }
            let Some(message_id) = payload.get("message_id").and_then(Value::as_i64) else {
                return failed();
            };
            let Some(requested_revision) = payload.get("revision").and_then(Value::as_i64) else {
                return failed();
            };
            let Some(offset) = payload
                .get("offset")
                .and_then(Value::as_i64)
                .filter(|value| *value >= 0)
                .and_then(|value| usize::try_from(value).ok())
            else {
                return failed();
            };
            // msgfix1 T4：`msg.fetch` 不像 `input.send`/`control.stop`/`control.history`/
            // `control.snapshot` 那样回一个 `input.ack`——按 M0 §10.5，它唯一的应答形态是
            // `reply` kind 下的 `msg.chunk`/`msg.fetch.error`，全部由 `handle_msg_fetch`
            // 经 `try_enqueue_reply` 送进独立的 `reply_queue`（M0 §10.9），由
            // `drain_reply_queue` 异步发出。这里返回 `None`——不是"处理失败"，是"响应已经
            // 走另一条通道，这条 `handle_frame` 调用没有直接要回写 socket 的 Value"。
            handle_msg_fetch(
                inner,
                session,
                &command_id,
                message_id,
                requested_revision,
                offset,
            );
            None
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
mod tests;
