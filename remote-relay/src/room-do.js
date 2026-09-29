"use strict";
// room-do.js — S2 房间 DO：一个 Durable Object = 一个房间。
//
// 用 Hibernation API（ctx.acceptWebSocket / webSocketMessage / webSocketClose）
// ——不是 `server.accept()`。区别很关键：`accept()` 建的连接只要开着就一直算
// active、一直计费；Hibernation 下 DO 在没消息时可以被冻结（不计费），消息
// 一来自动解冻——代价是解冻后内存清零，所以任何要跨消息记住的东西都不能放
// 在实例字段上，只能落 DO storage（SQL）或 ws.serializeAttachment()。
//
// 集成测试待 wrangler dev 手验：本文件依赖 Cloudflare 运行时全局
// （WebSocketPair、ctx.acceptWebSocket 等），本机没有真实 DO 环境跑不了它——
// 已把所有可以脱离运行时单测的逻辑抽进 envelope.js / auth.js / quota.js /
// room-store.js，那四个文件在 test/ 下有真跑过的单测；本文件（room-do.js）
// 用一个 mock-ctx 单测替身（test/room-do.test.js）绕开 WebSocketPair/
// Hibernation API，直接驱动 webSocketMessage/webSocketClose/fetch 的鉴权
// 分支，覆盖了消息路由/角色方向/配额降级这几块本来测不到的逻辑；真正测不了
// 的只剩 Hibernation 生命周期本身。
//
// ============================================================================
// Encrypted messages use a single envelope:
//   { v, room, epoch, kind, session, command_id, seq, ct, n, ts }
// The kind field determines routing and persistence: event frames are persisted,
// while live frames are forwarded without storage and always have a null seq.
// input and control require a top-level command_id; event, live, and presence
// forbid it. Both command kinds share the plaintext input.ack frame containing
// command_id and outcome so the relay can clear pending_input without decrypting ct.

import {
  PROTOCOL_VERSION,
  ENVELOPE_KINDS,
  validateEnvelope,
  validatePairAcceptFrame,
} from "./envelope.js";
import { reclaimIfAbandoned, touchRoomActivity, withAbandonedCandidate } from "./room-abandon.js";
import {
  matchesOwnerCredential,
  parseBearerAuthorization,
  parseRemoteSubprotocol,
  sha256AsciiHex,
} from "./auth.js";
import { DEFAULT_MONTHLY_MILESTONE_LIMIT, currentPeriod, evaluateQuota, shouldDegrade } from "./quota.js";
import * as store from "./room-store.js";
import * as ratelimit from "./room-do-ratelimit.js";
import { safeAttachment } from "./room-do-attachment.js";

// serializeAttachment 的字节预算：母文档给的口径是 ≤16KB（M0 施工约定 §7）。
// 这里只做一个粗防线（用 JSON 字符串长度近似字节数，纯 ASCII 场景下等价；
// 附件里目前只有 epoch/role/lastSeq/connectedAt 几个数字/短字符串字段，
// 天然远小于预算——真正需要这道防线的是未来往附件里塞更多东西的人）。
const ATTACHMENT_BYTE_BUDGET = 16 * 1024;
const FRAME_BYTE_BUDGET = 64 * 1024;
const SYNC_ENTRY_LIMIT = 256;
const REGISTRY_SYNC_TIMEOUT_MS = 30_000;
const CLAIM_BODY_BYTE_BUDGET = 1024;
const TOMBSTONE_CLOSE_CODE = 1008;
const TOMBSTONE_CLOSE_REASON = "room_tombstoned";
const REAUTH_CLOSE_CODE = 1008;
const REAUTH_CLOSE_REASON = "token_reauthorization_failed";
const RC_SUBPROTOCOL = "agentloom-rc-v1";
// S1i2 §9.6 第 246 行：relay 侧等桌面 ok/fail 回执的窗口——桌面此时必已在线
// 且 registry_ready（否则 handleTokenRefresh 直接回 desktop_offline、不建
// 行），与 REGISTRY_SYNC_TIMEOUT_MS 同一口径：一段正常往返该在这窗口内走完，
// 过期就删行，手机走 resend 重绑 connection_id 恢复。
const REFRESH_REQUEST_TTL_MS = 30_000;
// §9.6/§9.8 配额：per-socket 6/min，同 request_id 重发不计配额——复用
// pair.hello 的 attachment 滑窗设施同款模式（takeRefreshRequestSlot）。
const REFRESH_REQUEST_LIMIT = 6;
const REFRESH_REQUEST_WINDOW_MS = 60_000;
// S1i2 返工 R4（§9.6/§9.8 v1.8.5）：resend 免主配额是规范字面（同 request_id
// 重放不计配额），但字面本身留了口子——relay 只按 request_id+subject 判定
// 重放、不核 ct/n（也不该核，零解析）,持合法凭据者可用同一个 id 无限触发
// 「一次桌面 forward + 一次 DB upsert」、绕过主桶持续消耗桌面解密。独立宽松
// 桶兜底：默认 30/min/socket——远高于任何正常崩溃恢复重放（手机重启复用同
// id）的真实频率，只挡「持续刷同一个 id」这一种滥用形态。
const REFRESH_RESEND_LIMIT = 30;
const REFRESH_RESEND_WINDOW_MS = 60_000;
// S1i2 返工 R2（同 §9.5 command_id 128 字节上限口径·envelope.js:135-138）：
// request_id 直接落 refresh_requests.request_id PRIMARY KEY 并回写进 forward
// 帧，帧预算 64KB 下不设上限等于放行「已认证设备 → 持久存储膨胀」的成本
// 不对称写入；同口径 128 字节，按 UTF-8 字节计。
const REQUEST_ID_MAX_BYTES = 128;
// Allow 30 seconds for a reply to start or resume, matching the refresh-request
// round-trip window. Each successfully forwarded reply extends the deadline by
// another 30 seconds, so active chunk streams need no oversized lifetime limit.
// This tolerates normal network delays while promptly releasing stalled routes.
const REPLY_ROUTE_TTL_MS = 30_000;
// Cap reply routes at 256 rows per room to prevent control frames with distinct
// command_id values from growing SQLite storage without bound. This matches the
// pending-input table's abuse budget; routes awaiting a reply also expire after
// 30 seconds. Export the limit so tests can reference it without duplicating it.
export const REPLY_ROUTES_ROW_LIMIT = 256;
// Charge whole-frame bytes because the relay cannot decrypt ct, and enforce a
// 16 MiB budget per subject per 60 seconds to bound sustained reply traffic.
// This allows roughly four fetches at the 4 MiB message limit within a minute,
// leaving room for normal history retrieval while limiting repeated large fetches.
// Export the limit so tests can reference it without duplicating it.
export const REPLY_BYTE_BUDGET_LIMIT_BYTES = 16 * 1024 * 1024;
export const REPLY_BYTE_BUDGET_WINDOW_MS = 60_000;
// SEC-3：从未鉴权房（无 room_meta，无论是否已 claim）到点自杀回收
// 的固定宽限窗——起点是 room_state.created_at（fresh 房=真实建房时刻，存量
// 迁移房=首次触碰时刻，见 room-store.js initRoomStateSentinel），窗口本身
// 固定不随后续 fetch()/alarm() 滑动。远大于正常「建房→配对→claim」耗时，
// 避免误杀仍在正常配对流程中的房间。
const UNCLAIMED_RECLAIM_MS = 20 * 60 * 1000;
// 单 socket 攒够这么多协议违规就踢掉它；聚合计数也按同一批量落库一次
// （每帧一次 SELECT+UPSERT 等于把「乱发帧」变成「白嫖写」，见 §6b 防打爆）。
const PROTOCOL_VIOLATION_LIMIT = 8;
// G8（C1 设计 v0.5 §8·消息期限速·codex 设计审 Critical-2，双路（codex+opus）
// 二审 fix_required 后按 Lead 拍板的桶拓扑重构）：威胁模型是被盗的已配对
// 手机（合法凭据）——① 桌面在线时无限灌 input.send/control 直达 agent 执行
// 路径；② 桌面离线时无限撑 pending_input；③ 反复 control.stop 骚扰。
//
// R1：per-subject 桶不再存 attachment——威胁模型的行为体（被盗已配对手机）
// 完全掌控自己何时断线重连，存 attachment 的桶断一次重连就清零，形同虚设。
// 改成 message_rate_limits SQL 表（room-store.js takeMessageRateSlot）：
// 同 subject 的多个并发 socket 共享同一份 (subject, channel) 配额，跨
// socket/跨重连/跨休眠不丢——这才是设计稿 §8 字面的「per-subject」，不是
// 先前误解的「per-subject 有效上界 = 4×并发数」。见
// takeSubjectChannelRateSlot。
const INPUT_RATE_LIMIT = 30;
const INPUT_RATE_WINDOW_MS = 60_000;
const CONTROL_RATE_LIMIT = 30;
const CONTROL_RATE_WINDOW_MS = 60_000;
// pending_input 离线暂存上限（威胁模型②）：行数与总字节各自独立闸，任一
// 超限即拒绝入队，回 queue_full——不影响已暂存的行，只挡新的膨胀。
// R4：容量判断前先清过期行（死行不占死容量）、幂等先于容量（满队列时同
// command_id 重试不该收假 queue_full）——见 handleInput 调用点注释。
const PENDING_INPUT_ROW_LIMIT = 256;
const PENDING_INPUT_BYTE_LIMIT = 4 * 1024 * 1024; // 4MB（envelope 长度和，UTF-8 字节）
const INBOUND_SCOPE_MATRIX = Object.freeze({
  pairing: new Set(["pair.hello", "pair.done"]),
  remote: new Set(["input", "control", "presence", "token.refresh"]),
  refresh: new Set(["token.refresh"]),
  desktop: new Set([
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
    // v1.9 §10.2：桌面是 `reply` 帧的发送方（对某条 msg.fetch 的定向应答）；
    // `remote` 行故意不列 `reply`——远端只作接收方，矩阵默认 fail-closed。
    "reply",
  ]),
});

function cfSqlAdapter(storage) {
  return {
    exec(query, ...params) {
      return [...storage.sql.exec(query, ...params)];
    },
    transactionSync(callback) {
      return storage.transactionSync(callback);
    },
  };
}

export class RoomDO {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.sql = cfSqlAdapter(ctx.storage);
    this.upgradeFineBuckets = new Map();
    this.upgradeCoarseBuckets = new Map();
    // G8：per-IP 消息期粗桶——isolate 生命周期内存 Map，同 upgrade 粗桶一族，
    // 不落 SQL、不跨休眠（休眠重建即清零，够挡持续洪泛）。
    this.ipMessageBuckets = new Map();
    // 违规计数分两层：per-socket 在内存里（踢连接用，休眠清零即重新计），
    // 房间聚合攒够 PROTOCOL_VIOLATION_LIMIT 或 close/alarm 时才落一次库。
    this.socketProtocolViolations = new WeakMap();
    this.pendingProtocolViolations = 0;
    this.ipBucketSalt = null;

    // Sentinel bootstrap 顺序是生命周期契约的一部分：无条件调一次，不再拿
    // `!hasTable("room_state")` 挡——initRoomStateSentinel 内部全幂等
    // （CREATE TABLE IF NOT EXISTS + 守卫式 ALTER 补 ip_bucket_salt 列 +
    // INSERT ... WHERE NOT EXISTS），fresh 房三步全是空操作到真正建表，存量房
    // （room_state 已存在但预 SEC-1、没有 ip_bucket_salt 列）三步分别是
    // 空操作/真正补列/空操作。SEC-1 复现修复：guard 版本下存量房的守卫式
    // ALTER 永远够不到（挡在 !hasTable 外面），列补不上，deriveIpBucketKey
    // 每次 upgrade 都会撞 `no such column: ip_bucket_salt` 崩——去掉这层
    // guard 后存量房也能在构造时把列补齐。（存量房这次迁移会让 ip_bucket_key
    // 的盐旋转一次：迁移前 room_meta 里的旧盐不搬，直接在 room_state 里现生成
    // 一份新的——salt 消费方全部只是「同房间同 IP 落同一个桶」的相对稳定性
    // 而非跨迁移前后必须逐比特相等的持久正确性，旋转一次无安全/正确性损害）。
    // 墓碑房绝不能再触发业务 schema/backfill——业务 schema（11 张表 + IP 盐
    // 落库时机）不在这里无条件建：构造器只建/补 room_state 哨兵，业务 schema
    // 延到 fetch() 鉴权成功后才建（见下方 ensureBusinessSchema 插桩点）；
    // this.ipBucketSalt 留 null，deriveIpBucketKey 已有懒加载分支。
    store.initRoomStateSentinel(this.sql);
  }

  async fetch(request) {
    // After alarm()'s deleteAll this instance may still get requests: rebuild the sentinel.
    if (!store.hasTable(this.sql, "room_state")) store.initRoomStateSentinel(this.sql);
    const url = new URL(request.url);
    const roomIdMatch = url.pathname.match(/^\/room\/([0-9a-f]{32})/);
    const roomId = roomIdMatch ? roomIdMatch[1] : null;
    const claimPath = roomId ? `/room/${roomId}/claim` : null;
    const roomPath = roomId ? `/room/${roomId}` : null;

    if (!this.assertRoomLive()) {
      return new Response("room gone", { status: 410 });
    }

    if (request.method === "POST" && url.pathname === claimPath) {
      return this.handleClaim(request);
    }

    if (request.method === "DELETE" && url.pathname === roomPath) {
      return this.handleDelete(request);
    }

    if (request.headers.get("Upgrade") !== "websocket") {
      // SEC-3: bare GET / wrong-path POST land here and never reach the
      // post-handshake scheduleNextTokenAlarm(); arm the reclaim alarm here too,
      // otherwise this cheap probe could dodge room reclamation forever. Gated on
      // owner == null only as a cheap filter: handleClaim arms claimed
      // never-authenticated rooms itself.
      if (store.getRoomState(this.sql).owner_credential_hash == null) {
        await this.scheduleNextTokenAlarm();
      }
      return new Response("expected websocket upgrade", { status: 426 });
    }

    const ip = request.headers.get("CF-Connecting-IP") || "unknown";
    const ipBucketKey = await this.deriveIpBucketKey(ip);
    const bearer = parseBearerAuthorization(request);
    let role = "remote";
    let admission = null;
    let echoRemoteProtocol = false;
    if (bearer.provided) {
      const ownerHash = store.getRoomState(this.sql).owner_credential_hash;
      if (!(await matchesOwnerCredential(bearer.credential, ownerHash))) {
        const prefix = (await sha256AsciiHex(bearer.credential || "missing")).slice(0, 8);
        return this.rejectUpgradeAuthentication(ipBucketKey, prefix);
      }
      if (!this.assertRoomLive()) {
        return new Response("room gone", { status: 410 });
      }
      role = "desktop";
      admission = { scope: "desktop" };
    } else {
      // S1ja §9.7 后门退役：__admin/register-token + ADMIN_TOKEN 与 legacy
      // `?token=`/valid_tokens 准入路径已删——正式远端连接只认 §9.1 的子协议
      // token（下面这一枝），不再有任何回落到 room_meta.valid_tokens 的旁路。
      const subprotocol = parseRemoteSubprotocol(request);
      if (!subprotocol.provided || !subprotocol.ok) {
        const prefix = (await sha256AsciiHex(
          request.headers.get("Sec-WebSocket-Protocol") || "missing"
        )).slice(0, 8);
        return this.rejectUpgradeAuthentication(ipBucketKey, prefix);
      }
      const tokenHash = await sha256AsciiHex(subprotocol.token);
      admission = store.resolveTokenAdmission(this.sql, tokenHash, Date.now());
      if (!admission) {
        return this.rejectUpgradeAuthentication(ipBucketKey, tokenHash.slice(0, 8));
      }
      echoRemoteProtocol = true;
    }

    // SEC-1：业务 schema（11 张表）延到鉴权成功后才建——两条鉴权枝（bearer
    // desktop / 子协议 remote token）在这里汇流，鉴权失败的 reject 分支（上面
    // 的 rejectUpgradeAuthentication 提前 return）永远够不到这一行；socket
    // accept 在这一行之后，业务 handler 用表前 schema 已经在。刷房打随机
    // /room/<hex> 不再触发 11 张业务表 + IP 盐持久存储的账单。
    store.ensureBusinessSchema(this.sql);

    // M2：鉴权通过之后才落 room_meta 行。之前这行在鉴权前面跑，意味着任何
    // 打中 /room/<随机32位hex> 的请求——不管带没带对令牌——都会先触发一次
    // DO 实例化 + 一行 SQL 写入，把「枚举房间 id」的攻击面变成了「白嫖一次
    // 写」。鉴权失败的请求现在到不了这一行。
    if (roomId) store.ensureRoomId(this.sql, roomId);
    touchRoomActivity(this.sql);

    const requestedLastSeq = Number(url.searchParams.get("last_seq") || 0);

    // eslint-disable-next-line no-undef -- Cloudflare Workers 运行时全局
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);

    // 桌面每次连上来，epoch +1、此后拒绝旧 epoch 的写入（防双写，母文档 §0）。
    // 远端连接只是「读」当前 epoch，不推进它。
    const epoch = role === "desktop" ? store.bumpEpoch(this.sql) : store.getCurrentEpoch(this.sql);
    if (role === "desktop") {
      this.broadcastReplacedDesktopOffline(epoch);
      // P0-d1（C1 设计 v0.5 §8·母文档 M0 协议 §3 v1.7 订正遗留的可用性缺口）：
      // 桌面重连后 epoch 抬升，relay 之前从不通知在线远端——旧 epoch 只能等
      // 下一次写入撞 stale_epoch 才发现代已经变了。这里在 bump 落库后立即广播
      // 明文帧，走 broadcastToRemotes 同族出站闸（canDeliverOutbound：只投给
      // 当前权威、kind="current" 的活 remote 连接，同 presence 等既有明文
      // 广播帧一致），不新开投递路径、不绕闸。
      this.broadcastToRemotes({ t: "epoch.changed", epoch, ts: Date.now() });
    }

    const attachment = {
      epoch,
      role,
      scope: admission.scope,
      subject: admission.subject ?? null,
      kind: admission.kind ?? null,
      generation: admission.generation ?? null,
      alias_generation: admission.alias_generation ?? null,
      access_expires: admission.access_expires ?? null,
      valid_until: admission.valid_until ?? null,
      connection_id: globalThis.crypto.randomUUID(),
      lastSeq: admission.scope === "remote" ? requestedLastSeq : store.headSeq(this.sql),
      connectedAt: Date.now(),
      ip_bucket_key: ipBucketKey,
      ...(role === "desktop" ? {
        registry_ready: false,
        registry_sync_deadline: Date.now() + REGISTRY_SYNC_TIMEOUT_MS,
      } : {}),
    };
    this.assertAttachmentBudget(attachment);
    server.serializeAttachment(attachment);
    if (attachment.subject) this.enforceSubjectSocketLimit(attachment.subject);
    this.ctx.acceptWebSocket(server, [role]); // Hibernation API —— 不是 server.accept()
    if (attachment.subject || role === "desktop") await this.scheduleNextTokenAlarm();

    if (admission.scope === "remote") {
      this.replayTo(server, requestedLastSeq);
      // C1-PS（dogfood 修障第二批·手机发消息桌面离线无反馈）：remote 腿刚接入时拿不到当前桌面在线态——
      // presence 帧只在状态**变化**时广播（webSocketClose/broadcastReplacedDesktopOffline/
      // handleTokenReconcile 上线），发生在这条新连接接入之前的任何变化，它都是盲的，
      // 只能干等下一次真实变化才第一次收到一条 presence。这里定向回一条当前快照，只发
      // 给这条新 socket、不广播（其它已在线的远端不需要、也不该收到重复快照）。
      // R5（返工·快照陈旧 epoch）：不复用第 313 行捕获的局部 `epoch`——那个值与这里之间
      // 隔着一次真 `await`（上面 `scheduleNextTokenAlarm()`），理论上存在桌面在这段窗口内
      // 抢先重连、epoch 又被 bump 一次的交错可能。现算 `store.getCurrentEpoch(this.sql)`，
      // 保证快照用的是此刻真正权威的 epoch，不是握手最开始那一刻的快照值。
      this.sendDesktopPresenceSnapshot(server, store.getCurrentEpoch(this.sql));
    }
    if (role !== "desktop") this.broadcastPresence(role, "online", server);

    const headers = echoRemoteProtocol ? { "Sec-WebSocket-Protocol": RC_SUBPROTOCOL } : undefined;
    return new Response(null, { status: 101, webSocket: client, headers });
  }

  async handleClaim(request) {
    // SEC-3: arm first so every exit below (incl. 400 early returns) is covered;
    // claimed rooms are armed at the end of this handler instead.
    if (store.getRoomState(this.sql).owner_credential_hash == null) {
      await this.scheduleNextTokenAlarm();
    }
    const contentLength = Number(request.headers.get("Content-Length"));
    if (Number.isFinite(contentLength) && contentLength > CLAIM_BODY_BYTE_BUDGET) {
      return new Response("payload too large", { status: 413 });
    }

    let bytes;
    try {
      bytes = new Uint8Array(await request.arrayBuffer());
    } catch {
      return new Response("bad request", { status: 400 });
    }
    if (bytes.byteLength > CLAIM_BODY_BYTE_BUDGET) {
      return new Response("payload too large", { status: 413 });
    }

    let body;
    try {
      body = JSON.parse(new TextDecoder().decode(bytes));
    } catch {
      return new Response("bad request", { status: 400 });
    }
    if (body?.v !== 1 || !/^[0-9a-f]{64}$/.test(body?.credential_hash || "")) {
      return new Response("bad request", { status: 400 });
    }

    const result = store.claimRoom(this.sql, body.credential_hash, Date.now());
    if (result === "rate_limited") return new Response("rate limited", { status: 429 });
    if (result === "tombstoned") return new Response("room gone", { status: 410 });
    if (result === "conflict") return new Response("owner conflict", { status: 409 });
    // Claimed-but-never-authenticated rooms are reclaimed too: arm the alarm.
    if (!store.hasTable(this.sql, "room_meta")) await this.scheduleNextTokenAlarm();
    return new Response("ok", { status: 200 });
  }

  async handleDelete(request) {
    const bearer = parseBearerAuthorization(request);
    const ownerHash = store.getRoomState(this.sql).owner_credential_hash;
    if (!bearer.provided || !(await matchesOwnerCredential(bearer.credential, ownerHash))) {
      // SEC-3: DELETE on an unclaimed room always 401s and never reaches the
      // post-handshake scheduleNextTokenAlarm(); arm the reclaim alarm here so
      // this probe cannot dodge reclamation. Claimed rooms were armed at claim.
      if (ownerHash == null) {
        await this.scheduleNextTokenAlarm();
      }
      return new Response("unauthorized", { status: 401 });
    }

    const tombstoned = store.tombstoneRoom(this.sql, Date.now());
    if (!tombstoned) {
      return new Response("room gone", { status: 410 });
    }

    // 事务提交后才处理 SQL 外的副作用，避免回滚时房间已被提前踢空。
    this.closeAllSockets();
    if (typeof this.ctx.storage.deleteAlarm === "function") {
      await this.ctx.storage.deleteAlarm();
    }
    return new Response("ok", { status: 200 });
  }

  async webSocketMessage(ws, message) {
    if (!this.assertRoomLive()) {
      this.closeSocketForTombstone(ws);
      return;
    }

    let text;
    try {
      if (typeof message === "string") {
        text = message;
        if (new TextEncoder().encode(text).byteLength > FRAME_BYTE_BUDGET) {
          this.rejectOversizedFrame(ws);
          return;
        }
      } else {
        const bytes = message instanceof ArrayBuffer
          ? new Uint8Array(message)
          : ArrayBuffer.isView(message)
            ? new Uint8Array(message.buffer, message.byteOffset, message.byteLength)
            : null;
        if (!bytes) throw new TypeError("unsupported websocket message");
        if (bytes.byteLength > FRAME_BYTE_BUDGET) {
          this.rejectOversizedFrame(ws);
          return;
        }
        text = new TextDecoder().decode(bytes);
      }
    } catch {
      ws.send(JSON.stringify({ t: "error", reason: "bad_json" }));
      return;
    }

    const attachment = safeAttachment(ws);
    if (!this.authorizeInboundSocket(ws, attachment)) return;

    let payload;
    try {
      payload = JSON.parse(text);
    } catch {
      ws.send(JSON.stringify({ t: "error", reason: "bad_json" }));
      return;
    }

    if (!payload || typeof payload !== "object") {
      ws.send(JSON.stringify({ t: "error", reason: "bad_payload" }));
      return;
    }

    const role = attachment.role;
    const frameType = typeof payload.t === "string" ? payload.t : payload.kind;
    const allowedTypes = INBOUND_SCOPE_MATRIX[attachment.scope];
    if (typeof frameType !== "string" || !allowedTypes?.has(frameType)) {
      const exceeded = this.recordProtocolViolation(ws);
      const error = {
        t: "error",
        reason: "role_forbidden",
        role: role ?? null,
      };
      if (typeof payload.t === "string") error.frame = payload.t;
      else if (typeof payload.kind === "string") error.kind = payload.kind;
      ws.send(JSON.stringify(error));
      // 矩阵外帧本身是「拿着合法凭据乱扫」的形态：回错误但连接留着，等它攒够
      // PROTOCOL_VIOLATION_LIMIT 次再踢，免得一次手滑就断线。
      if (exceeded) this.closeSocketForReauthorization(ws);
      return;
    }

    // G8 R2（双路审 fix_required②）：per-IP 消息期粗桶——中央入站点，scope
    // 矩阵刚通过（frameType 确认是这个 scope 允许的帧型）、明文/信封分流之前，
    // 对这个 IP 上所有 role=remote 的入站帧计费，不管这条帧接下来是走
    // handlePlainFrame 还是 validateEnvelope、也不管 validateEnvelope 最终
    // 判它合法还是垃圾——旧版把闸设在 handleInput/handleControl 内部，只在
    // envelope 校验通过、真正进了这两个 handler 之后才计费，一条 kind=input
    // 但 ct/n 是垃圾的帧会在 validateEnvelope 那步就被挡掉、根本走不到那
    // 两个闸，等于一条不计费的免费探测回环。desktop 角色的帧不挂这个桶——
    // 它们走完全不同的 scope="desktop" 分支，人是自己人，不是威胁模型里的
    // 「被盗手机」。
    if (role === "remote" && !this.takeIpMessageSlot(ws, frameType, payload.command_id)) return;

    if (typeof payload.t === "string" && attachment.role === "desktop" &&
        attachment.scope === "desktop" &&
        Number(attachment.epoch) !== store.getCurrentEpoch(this.sql)) {
      const currentEpoch = store.getCurrentEpoch(this.sql);
      try {
        ws.send(JSON.stringify({ t: "error", reason: "stale_epoch", currentEpoch }));
      } finally {
        this.closeSocketForReauthorization(ws);
      }
      return;
    }

    if (attachment.role === "desktop" && attachment.scope === "desktop" &&
        attachment.registry_ready !== true && frameType !== "token.sync") {
      if (["token.put", "token.delete"].includes(frameType)) {
        this.rejectTokenMutation(ws, payload, "sync_required");
      } else {
        ws.send(JSON.stringify({ t: "error", reason: "sync_required", frame: frameType }));
      }
      return;
    }

    // A WebSocket message is either a plaintext control frame identified by a
    // top-level t field (presence, control.notify_hint, or input.ack), or the
    // envelope itself. Validate and forward envelopes without an outer wrapper.
    if (typeof payload.t === "string") {
      this.handlePlainFrame(ws, payload, role);
      return;
    }

    const validation = validateEnvelope(payload);
    if (!validation.ok) {
      ws.send(JSON.stringify({ t: "error", reason: "invalid_envelope", errors: validation.errors }));
      return;
    }
    const envelope = validation.envelope;

    // H1：role 强制方向。远端不产里程碑——否则任何持有 K_room 的远端设备
    // 都能伪造一条「agent 说的话」广播给房间里其它人，还会被落库、被将来的
    // 重连当成真实历史回放。桌面不发 input——input 通道的语义是「远端替
    // 用户敲的东西」，桌面自己不需要通过这条通道给自己发指令。违反方向的
    // 一律拒绝 + 回错误帧，不静默丢弃（同母文档 §6b「防打爆」一贯的做法）。
    if (envelope.kind === "event" && role !== "desktop") {
      ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", kind: envelope.kind, role }));
      return;
    }
    if (envelope.kind === "input" && role !== "remote") {
      ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", kind: envelope.kind, role }));
      return;
    }
    // 远端发 live 等于冒充桌面广播 agent 流式输出，是瞬时视觉污染攻击。
    if (envelope.kind === "live" && role !== "desktop") {
      ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", kind: envelope.kind, role }));
      return;
    }
    // v1.9 §10.1/§10.2：`reply` 只能由桌面发出（对某条 msg.fetch 的定向应答）。
    // 理论不可达——INBOUND_SCOPE_MATRIX 的 remote/refresh/pairing 三个 Set 都
    // 不含 "reply"，远端连接在更早的矩阵闸就已经被拒——留作纵深防御，同文件
    // 其它「理论不存在仍 fail-closed」的既有写法一致（如 handleInput 的
    // missing_command_id 分支）。
    if (envelope.kind === "reply" && role !== "desktop") {
      ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", kind: envelope.kind, role }));
      return;
    }

    switch (envelope.kind) {
      case "event":
        this.handleEvent(ws, envelope);
        return;
      case "live":
        this.handleLive(ws, envelope);
        return;
      case "input":
        this.handleInput(ws, envelope, envelope.command_id);
        return;
      case "control":
        this.handleControl(ws, envelope);
        return;
      case "presence":
        this.broadcastRaw(envelope, ws);
        return;
      case "reply":
        this.handleReply(ws, envelope);
        return;
      default:
        // validateEnvelope 已经把 kind 限定在 ENVELOPE_KINDS 内，这里到不了；
        // 留一条防线防未来 ENVELOPE_KINDS 加了新值却忘了在这里接。
        ws.send(JSON.stringify({ t: "error", reason: "unhandled_kind", kinds: ENVELOPE_KINDS }));
    }
  }

  async webSocketClose(ws) {
    if (!this.assertRoomLive()) {
      this.closeSocketForTombstone(ws);
      return;
    }
    this.flushProtocolViolations();
    touchRoomActivity(this.sql);
    await this.scheduleNextTokenAlarm(Date.now(), ws);
    const att = safeAttachment(ws);
    if (att.role !== "desktop" || (att.registry_ready === true &&
        Number(att.epoch) === store.getCurrentEpoch(this.sql))) {
      this.broadcastPresence(att.role || "remote", "offline", ws);
    }
  }

  async webSocketError(ws) {
    // Reconnecting is the client's job; only re-arm reclaim like a close would.
    touchRoomActivity(this.sql);
    await this.scheduleNextTokenAlarm(Date.now(), ws);
  }

  async alarm() {
    if (!store.hasTable(this.sql, "room_state")) store.initRoomStateSentinel(this.sql);
    if (!this.assertRoomLive()) {
      this.closeAllSockets();
      return;
    }
    // SEC-3: reclaim before ensureBusinessSchema; deleteAll wipes whatever tables exist.
    // Reclaim guard: the hard gate is "never authenticated" (room_meta absent;
    // ensureBusinessSchema only runs after desktop/token auth succeeds), whether
    // or not an owner was claimed: an unauthenticated POST /claim can register any
    // hash, so owner presence proves nothing. Authenticated rooms never enter here.
    // The deadline is re-checked live; after a reclaim a later claim starts a fresh room.
    const unclaimedState = store.getRoomState(this.sql);
    if (!store.hasTable(this.sql, "room_meta") && this.ctx.getWebSockets().length === 0) {
      const createdAt = unclaimedState.created_at == null ? null : Number(unclaimedState.created_at);
      if (createdAt != null && Date.now() >= createdAt + UNCLAIMED_RECLAIM_MS) {
        // Fully erase expired unclaimed rooms: retaining a tombstone leaves storage
        // allocated indefinitely, allowing repeated room creation to accumulate it.
        // deleteAll atomically clears the private SQLite database, including SQL
        // tables and key-value data. Explicitly delete the alarm first so cleanup
        // does not depend on compatibility settings that make deleteAll remove it.
        // SQL tables no longer exist afterward; return without accessing this.sql.
        // Room identifiers are 128-bit random values and a never-authenticated
        // room has no legitimate owner (claim is unauthenticated). Reopening one
        // recreates the sentinel as a fresh room that expires again, so retaining
        // a tombstone provides no needed guarantee. Explicit deletion takes a
        // separate path and retains its tombstone and 410 responses.
        await this.ctx.storage.deleteAlarm?.();
        await this.ctx.storage.deleteAll();
        return;
      }
      // Not due yet: re-arm at the deadline (never in the past, so no hot loop).
      await this.scheduleNextTokenAlarm();
    }
    // Unauthenticated claims can persist an owner without creating the business
    // schema. Alarms must not create that schema on behalf of such requests.
    // Without a successful authenticated WebSocket session, there are no business
    // tables or sockets to maintain, so return before running token cleanup.
    if (!store.hasTable(this.sql, "room_meta")) return;
    if (await reclaimIfAbandoned(this.ctx, this.sql)) return;
    // SEC-1 防御性兜底：正常路径业务 schema 已在 fetch() 鉴权成功时建好，这里
    // 只防未来排序意外（比如某次改动让 alarm 抢在任何鉴权请求之前先被调度）。
    // ensureBusinessSchema 幂等，重复调用安全。
    store.ensureBusinessSchema(this.sql);
    this.flushProtocolViolations();
    // R1（§9.6 第 249 行）：过期 refresh_requests 行真删，不只是查询时过滤——
    // 见 room-store.js deleteExpiredRefreshRequests 头注释。
    store.deleteExpiredRefreshRequests(this.sql, Date.now());
    // Delete expired reply routes rather than merely filtering them from queries,
    // so stale rows cannot distort replay handling or resource accounting.
    store.deleteExpiredReplyRoutes(this.sql, Date.now());
    // C1-TTL（dogfood 修障第二批·手机发消息桌面离线无反馈）：pending_input
    // TTL 清扫的 alarm 兜底——之前只有 handleInput 高频路径里顺带清一次，房间
    // 若从此再没有新 input 涌入，早该过期的暂存行会一直滞留、input.expired
    // 回执永远等不到。purgeExpiredPendingInput 内部已会广播 input.expired
    // （见其头注释）。
    this.purgeExpiredPendingInput(Date.now());
    for (const ws of this.ctx.getWebSockets()) {
      const attachment = safeAttachment(ws);
      if (attachment.role === "desktop" && attachment.scope === "desktop" &&
          attachment.registry_ready !== true &&
          Number.isSafeInteger(Number(attachment.registry_sync_deadline)) &&
          Number(attachment.registry_sync_deadline) <= Date.now()) {
        this.closeDesktopForSyncTimeout(ws);
        continue;
      }
      if (attachment.subject && !this.isOfficialAttachmentLive(attachment, Date.now())) {
        this.closeSocketForReauthorization(ws);
      }
    }
    await this.scheduleNextTokenAlarm();
  }

  // ---- kind=event：里程碑，恒落库盖 seq。H3：配额超限也不丢（母文档 §6b，
  // 见 quota.js 文件头的理由）——handleEvent 因此不查配额，只查 stale_epoch。 ----
  handleEvent(ws, envelope) {
    const currentEpoch = store.getCurrentEpoch(this.sql);
    if (envelope.epoch !== currentEpoch) {
      ws.send(JSON.stringify({ t: "error", reason: "stale_epoch", currentEpoch }));
      return;
    }

    const result = store.insertMilestone(this.sql, {
      epoch: envelope.epoch,
      session: envelope.session,
      kind: envelope.kind,
      ct: envelope.ct,
      n: envelope.n,
      ts: envelope.ts,
      client_msg_id: envelope.client_msg_id,
    });
    if (!result.ok) {
      ws.send(JSON.stringify({ t: "error", reason: result.reason, currentEpoch: result.currentEpoch }));
      return;
    }

    const stamped = { ...envelope, seq: result.seq };

    if (result.dedup) {
      // 幂等命中（母文档 v1.7.4）：不落新行、不重复计数配额、不广播给全房间——
      // 只回发送方一份既有 seq 的确认帧，形态与非去重路径下发送方收到的
      // stamped 自确认一致。
      ws.send(JSON.stringify(stamped));
      return;
    }

    store.incrementQuotaCount(this.sql, currentPeriod(Date.now()), 1);
    this.broadcastRaw(stamped, null); // 含发送方自己也收一份 seq 确认
  }

  // ---- kind=live：只转发、不落库、seq 恒 null（母文档 §2）；H3：配额超限时
  // 唯一允许被 shed 的流量——断的是这条通道，不是里程碑。 ----
  handleLive(ws, envelope) {
    const currentEpoch = store.getCurrentEpoch(this.sql);
    if (envelope.epoch !== currentEpoch) {
      ws.send(JSON.stringify({ t: "error", reason: "stale_epoch", currentEpoch }));
      return;
    }

    const quota = this.currentQuotaState();
    if (shouldDegrade("live", quota)) {
      // 同时广播给全房间（含远端），不是只回发送方——远端要能渲染出「本月
      // 额度已用完」，不能被静默丢包看着像网络抽风（母文档 §6b）。
      this.broadcastRaw({ t: "quota.exceeded", channel: "live" }, null);
      return;
    }

    this.broadcastRaw(envelope, ws);
  }

  // ---- kind=input：FIFO·在线直转·离线暂存 30min TTL ----
  // G8：限速闸在最前——不管信封本身是否 stale_epoch/缺 command_id，都先占
  // 一个桶名额，防止用无效帧绕过限速白嫖判断成本（同 rejectUpgradeAuthentication
  // 的既有姿势：先判限速再判鉴权内容）。per-IP 粗桶已挪到中央入站点
  // （webSocketMessage，见那里的注释），这里只剩 per-subject 桶。
  handleInput(ws, envelope, commandId) {
    if (!this.takeSubjectChannelRateSlot(ws, envelope, {
      channel: "input", limit: INPUT_RATE_LIMIT, windowMs: INPUT_RATE_WINDOW_MS,
      reason: "input_rate_limited",
    })) return;

    const currentEpoch = store.getCurrentEpoch(this.sql);
    if (envelope.epoch !== currentEpoch) {
      ws.send(JSON.stringify({ t: "error", reason: "stale_epoch", currentEpoch }));
      return;
    }

    if (!commandId) {
      // G8 R5（双路审）：理论死代码——validateEnvelope 已保证 kind=input 的
      // command_id 非空非 undefined（envelope.js command_id_required_for_kind），
      // 这条分支到不了这里。留着当纵深防御，不删（同文件别处「理论不存在
      // 仍 fail-closed」的既有写法一致，例如 isPendingInputAuthorized 对
      // generation 为 NULL 的显式判断）。
      ws.send(JSON.stringify({ t: "error", reason: "missing_command_id" }));
      return;
    }

    const desktop = this.onlineDesktopForEpoch(currentEpoch);
    if (desktop) {
      desktop.send(JSON.stringify(envelope)); // envelope 自带 command_id（顶层唯一真相）
      return;
    }

    // G8 R4（双路审 fix_required④）：容量判断前先清过期行——TTL 到期的死行
    // 不该继续占着行数/字节容量，不清的话，房间可能因为一堆早该过期的旧行
    // 占满 256 行/4MB，把新的、真正需要暂存的 input 挤成假 queue_full。
    this.purgeExpiredPendingInput(Date.now());
    // C1-TTL（dogfood 修障第二批·手机发消息桌面离线无反馈）：清扫可能刚删掉
    // 了曾是现排 alarm 依据的那一行——重算一次 min（同款
    // handleTokenRefresh/handleTokenReconcile 既有 fire-and-forget 写法，
    // 「入队/补投/清扫后都重算」三处纪律之一）。
    void this.scheduleNextTokenAlarm();

    // G8 R4：幂等先于容量——同一个 command_id 重试（比如手机没收到上一次的
    // 确认、超时重发）本就不会让表再多一行（enqueueInput 是 ON CONFLICT
    // DO NOTHING），不该被容量闸误伤：不判断的话，队列刚好满员时的一次
    // 无害重试会被 rowCount>=LIMIT 挡下、回一个名不副实的 queue_full。
    // C1-RQ：幂等命中现在也要回一条 input.relay_queued（拿既存行的
    // expires_at）——之前这里直接空手 return，手机端第一次的确认没收到
    // （进程重启/掉线重连）时，重试一次只换来沉默，跟第一次发送时桌面离线
    // 那声不吭是同一种不对称，这次一并补上。
    const existingExpiry = store.getPendingInputExpiry(this.sql, commandId);
    if (existingExpiry != null) {
      ws.send(JSON.stringify({ t: "input.relay_queued", command_id: commandId, expires_at: existingExpiry }));
      return;
    }

    // G8 威胁模型②：桌面离线时无限撑 pending_input——入队前核该房现存行数/
    // 总字节，任一超限就不入队，回 queue_full；不影响已暂存的行。
    const envelopeJson = JSON.stringify(envelope);
    const envelopeBytes = new TextEncoder().encode(envelopeJson).length;
    const pendingStats = store.pendingInputStats(this.sql);
    if (pendingStats.rowCount >= PENDING_INPUT_ROW_LIMIT ||
        pendingStats.totalBytes + envelopeBytes > PENDING_INPUT_BYTE_LIMIT) {
      // G8 R5：带 command_id——手机端要能把这条拒绝跟自己发出的哪条 input
      // 对上号，不然收到一条不知道是谁被拒的 queue_full 没法做精确重试/提示。
      ws.send(JSON.stringify({ t: "error", reason: "queue_full", frame: "input", command_id: commandId }));
      return;
    }

    // C1-RQ（dogfood 修障第二批·手机发消息桌面离线无反馈）：handleInput 之前
    // 把 input 存进 pending_input 后一声不吭——对比 handleControl 同场景回
    // desktop_offline 错误帧，这里不对称（消息没丢，只是没反馈）。现在成功
    // 入队后回一条新帧 input.relay_queued（不是 input.ack——那个语义是「桌面
    // 已接收」；也不是 error——消息并没失败），带上这次真正生效的 expires_at
    // 让手机端知道大概什么时候该放弃等待。
    const enqueued = store.enqueueInput(this.sql, {
      commandId,
      session: envelope.session,
      envelopeJson,
      now: Date.now(),
      subject: safeAttachment(ws).subject ?? null,
      generation: safeAttachment(ws).generation ?? null,
    });
    ws.send(JSON.stringify({ t: "input.relay_queued", command_id: commandId, expires_at: enqueued.expiresAt }));
    void this.scheduleNextTokenAlarm();
  }

  // ---- kind=control：即刻投递、不暂存；桌面离线则丢弃并告知发送方 ----
  handleControl(ws, envelope) {
    if (!this.takeSubjectChannelRateSlot(ws, envelope, {
      channel: "control", limit: CONTROL_RATE_LIMIT, windowMs: CONTROL_RATE_WINDOW_MS,
      reason: "control_rate_limited",
    })) return;

    const currentEpoch = store.getCurrentEpoch(this.sql);
    if (envelope.epoch !== currentEpoch) {
      ws.send(JSON.stringify({ t: "error", reason: "stale_epoch", currentEpoch }));
      return;
    }
    const desktop = this.onlineDesktopForEpoch(currentEpoch);
    if (!desktop) {
      ws.send(JSON.stringify({ t: "error", reason: "desktop_offline" }));
      return;
    }

    // The relay cannot decrypt control frames to determine which expect replies,
    // so register or renew a route for every control frame. Unused routes expire.
    // Inbound authorization requires a subject, but defensively skip registration
    // if it is absent, preserving forwarding and matching the rate limiter.
    const attachment = safeAttachment(ws);
    if (attachment.subject) {
      // Remove expired routes before checking capacity so stale rows cannot fill
      // the route limit and incorrectly reject new control frames.
      store.deleteExpiredReplyRoutes(this.sql, Date.now());

      // Apply the row limit only when adding a route. Renewing an existing command
      // route during reconnection adds no row and must remain possible at capacity.
      const isNewRoute = store.getReplyRoute(this.sql, envelope.command_id) == null;
      if (isNewRoute && store.countReplyRoutes(this.sql) >= REPLY_ROUTES_ROW_LIMIT) {
        const exceeded = this.recordProtocolViolation(ws);
        ws.send(JSON.stringify({ t: "error", reason: "reply_routes_row_limit", command_id: envelope.command_id }));
        if (exceeded) this.closeSocketForReauthorization(ws);
        return;
      }

      const registered = store.upsertReplyRoute(this.sql, {
        commandId: envelope.command_id,
        subject: attachment.subject,
        generation: Number.isSafeInteger(Number(attachment.generation)) ? Number(attachment.generation) : null,
        connectionId: attachment.connection_id,
        deadline: Date.now() + REPLY_ROUTE_TTL_MS,
      });
      if (!registered.ok) {
        // §10.3「同 command_id 跨 subject：拒绝……不得劫持路由」——协议违例，
        // 不转发给桌面，回错误帧给发送端（同 rejectTokenRefresh 既有姿势）。
        const exceeded = this.recordProtocolViolation(ws);
        ws.send(JSON.stringify({ t: "error", reason: registered.reason, command_id: envelope.command_id }));
        if (exceeded) this.closeSocketForReauthorization(ws);
        return;
      }
      void this.scheduleNextTokenAlarm();
    }

    desktop.send(JSON.stringify(envelope));
  }

  // ---- kind=reply（v1.9 §10.1/§10.3）：桌面对某条 control 帧（msg.fetch）的
  // 定向应答。relay 不解密 ct，不理解 msg.chunk/msg.fetch.error 的区别，只按
  // route 账本把整帧原样转发给发起 fetch 的那个远端连接——不落库、不进
  // replay 补发、不广播（§10.1）。无 route 行/目标已断/过期/身份不符/超字节
  // 预算 → 丢弃，**对桌面无回执**（§10 条文口径：见母文档「无 route
  // 行/连接已断 → 丢弃（对桌面无回执·§10 条文口径）」原句——这与桌面发
  // event/live 从不等 relay 回执的既有姿势一致，不是本条新引入的不对称）。 ----
  handleReply(ws, envelope) {
    // A replaced desktop could forge the current epoch in the envelope. Validate
    // the sender's connection attachment, stamped during the handshake and beyond
    // the desktop's control, to enforce the current epoch's single desktop writer.
    if (!this.isCurrentDesktopWriter(ws)) {
      this.rejectStaleDesktopFrame(ws);
      return;
    }

    const now = Date.now();
    const route = store.getReplyRoute(this.sql, envelope.command_id);
    if (!route) return; // 无 route 行：丢弃，对桌面无回执（§10.3/§10.9）

    // Recheck the deadline before delivery: a late reply can arrive before the
    // alarm removes its expired route. Delete it now to prevent route revival.
    if (Number(route.deadline) <= now) {
      store.deleteReplyRoute(this.sql, route.command_id);
      return;
    }

    const target = this.ctx.getWebSockets().find(
      (candidate) => safeAttachment(candidate).connection_id === route.connection_id
    );
    // 目标已断（含 CLOSING，同 deliverRefreshReceipt 既有姿势·真实 Cloudflare
    // Hibernation 下 close 握手期 getWebSockets() 仍可能吐出一条 CLOSING
    // socket）：丢弃、不删行——手机走同 command_id 重连重绑或自然 TTL 收尾，
    // 不是本条新引入的一次性判死。
    if (!target || Number(target.readyState) !== 1 /* WebSocket.OPEN */) return;

    // Require the target attachment to match the route's subject and generation
    // and remain authoritative. Revoked or superseded sockets must not receive
    // replies even if their connection identifiers still resolve to open sockets.
    // Explicit identity checks also reject routes mapped to the wrong connection.
    const targetAttachment = safeAttachment(target);
    if (targetAttachment.subject !== route.subject) return;
    if (route.generation != null && Number(targetAttachment.generation) !== Number(route.generation)) return;
    if (!this.isOfficialAttachmentLive(targetAttachment, now)) return;

    const frameText = JSON.stringify(envelope);
    const bytesDelta = new TextEncoder().encode(frameText).length;

    // Meter each subject's reply budget using serialized wire bytes because the
    // relay cannot decrypt the ciphertext. Drop over-budget frames without
    // delivery or extending the route.
    const budget = store.takeReplyByteBudget(this.sql, {
      subject: route.subject,
      limitBytes: REPLY_BYTE_BUDGET_LIMIT_BYTES,
      windowMs: REPLY_BYTE_BUDGET_WINDOW_MS,
      bytesDelta,
      now,
    });
    if (!budget.allowed) {
      // The target passively receives desktop replies; charging dropped replies
      // as its protocol violations could repeatedly disconnect it after reconnects.
      // Drop the frame and increment a separate monitoring counter without
      // consuming the target's protocol violation allowance.
      const dropped = Number(store.getMeta(this.sql, "reply_budget_dropped", "0")) + 1;
      store.setMeta(this.sql, "reply_budget_dropped", dropped);
      return;
    }

    target.send(frameText);

    // §10.3 第 295 行：同一次调用顺延 deadline（多 chunk 传输期保活，见
    // upsertReplyRoute/bumpReplyRouteDelivery 头注释）。
    store.bumpReplyRouteDelivery(this.sql, envelope.command_id, {
      bytesDelta,
      deadline: now + REPLY_ROUTE_TTL_MS,
    });
    void this.scheduleNextTokenAlarm();
  }

  // ---- 明文控制帧 ----
  handlePlainFrame(ws, payload, role) {
    switch (payload.t) {
      case "presence": {
        // R3（返工·硬化）：remote scope 本就放行 presence 帧（INBOUND_SCOPE_MATRIX.remote
        // 含 "presence"），但这不代表"任意角色的 presence 内容都能广播"——payload.role 是
        // 发送方自报的字段，relay 从不校验它与真实连接角色是否一致；不加这道检查，任何
        // 已授权远端都能伪造 {t:"presence",role:"desktop",event:"offline"} 操纵同房别的
        // 手机看到的桌面在线态横幅。照本函数 control.notify_hint（:879 一带）/input.ack
        // （:889 一带）的既有 H1 姿势：只有 role==="desktop" 的连接才能广播 role:"desktop"
        // 的 presence；伪造属于"拿着合法凭据乱来"的形态，计违规（同上面 scope 矩阵外帧
        // 那条既有样式），不立即踢——攒够 PROTOCOL_VIOLATION_LIMIT 次才断。远端广播自己
        // 真实角色的 presence（正常用法）不受影响。
        if (payload.role === "desktop" && role !== "desktop") {
          const exceeded = this.recordProtocolViolation(ws);
          ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", frame: "presence", role }));
          if (exceeded) this.closeSocketForReauthorization(ws);
          return;
        }
        this.broadcastRaw(payload, ws);
        return;
      }
      case "control.notify_hint":
        // 桌面 → relay，只带 {category}（母文档 §1）；转发给房间内在线远端。
        // 触发 Web Push 是 M5 的事，本单不做。
        // H1：仅接受来自桌面连接——这条通道的语义是「桌面通知远端」，远端
        // 假冒这一帧等于给房间广播一条冒充桌面发出的假提醒。
        if (role !== "desktop") {
          ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", frame: "control.notify_hint", role }));
          return;
        }
        this.broadcastToRemotes(payload);
        return;
      case "input.ack":
        // H1：仅接受来自桌面连接——ack 的语义是「桌面确认收到了这条
        // input」，远端假冒会让 relay 把还没真正投递的 pending_input 行
        // 提前删掉，等于远端能替桌面「确认」一条它自己都没收到的指令。
        if (role !== "desktop") {
          ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", frame: "input.ack", role }));
          return;
        }
        if (typeof payload.command_id === "string") {
          store.removePendingInput(this.sql, payload.command_id);
        }
        this.broadcastToRemotes(payload);
        return;
      case "pair.hello": {
        // H1：配对只能由远端发起；桌面反向发送会伪造一个并不存在的待配对设备。
        if (role !== "remote") {
          ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", frame: payload.t, role }));
          return;
        }
        if (!this.takePairHelloSlot(ws)) return;
        const desktop = this.onlineDesktop();
        if (!desktop) {
          ws.send(JSON.stringify({ t: "error", reason: "desktop_offline" }));
          return;
        }
        // S1i3 F1（§9.5 第 235 行）：转发前在帧上盖章 origin_connection_id——绝不信手机
        // 自报。写法上先展开 payload 再覆盖同名字段，最终这个键的值只可能是 relay 自己
        // 认定的这条 socket 的 connection_id（等价于「先删手机自带的同名字段再盖」，因为
        // 无论手机是否夹带这个字段，展开顺序都保证后写的这一行赢）。同时把这条连接
        // 持久写进单行路由表（DO 休眠后内存 Map 会丢，pair.accept/pair.ready 定向投递
        // 靠这行找回目标）。
        const connectionId = safeAttachment(ws).connection_id;
        this.recordPairingRoute(connectionId);
        desktop.send(JSON.stringify({ ...payload, origin_connection_id: connectionId }));
        return;
      }
      case "pair.accept": {
        // H1：配对凭据只能由桌面签发；远端反向发送等于自行伪造授权结果。
        if (role !== "desktop") {
          ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", frame: payload.t, role }));
          return;
        }
        const validation = validatePairAcceptFrame(payload);
        if (!validation.ok) {
          ws.send(JSON.stringify({
            t: "error",
            reason: "invalid_pair_accept",
            errors: validation.errors,
          }));
          return;
        }
        // S1i3 F1（§9.5 第 235 行）：撤回 broadcastToRemotes 广播——同窗多手机不互收对方
        // 的 accept。只按 pairing_routes 里记的那一行 connection_id 定向投递。
        this.deliverToPairingRoute(payload);
        return;
      }
      case "pair.done": {
        // H1：完成确认只能由收到 accept 的远端发出；桌面反向发送会伪造远端已收妥。
        if (role !== "remote") {
          ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", frame: payload.t, role }));
          return;
        }
        const desktop = this.onlineDesktop();
        if (!desktop) {
          ws.send(JSON.stringify({ t: "error", reason: "desktop_offline" }));
          return;
        }
        // S1i3 F1 返工（§9.5 第 235 行「relay 转发 pair.hello / pair.done 给桌面时
        // ……并把它持久写入单行路由表」——字面管两种帧，不是只管 hello）：done 也必须
        // upsert，不能只信 hello 时写好的那一行还在指着正确的连接。两条真实后果撑住
        // 这个「必须」：
        // ① 恢复路径死：§9.5 明写 pair.done 幂等可重放是手机在 accept→ready 窗口掉线
        //   后唯一的救命路；重连后 connection_id 是握手现生成的新随机值（room-do.js
        //   的 fetch() 握手逻辑），若这里不重新写路由，路由行仍指向已断的旧连接，桌面
        //   重新产出的 ready 会在 deliverToPairingRoute 里因「目标 socket 已断」被静默
        //   丢弃，手机怎么重放 done 都拿不到 ready。
        // ② 同窗第一台被饿死：桌面在 SentAccept 态会忽略第二台手机的 hello，但那次
        //   hello 已经把路由行搬走；若这里的 done 不把路由行写回发出这条 done 的连接，
        //   ready 会投给抢路由的第二台，真正完成配对的第一台永远收不到自己的 ready。
        const connectionId = safeAttachment(ws).connection_id;
        this.recordPairingRoute(connectionId);
        desktop.send(JSON.stringify({ ...payload, origin_connection_id: connectionId }));
        return;
      }
      case "pair.ready":
        // S1i3 F1：同 pair.accept，撤回广播改定向投递。
        this.deliverToPairingRoute(payload);
        return;
      case "token.refresh":
        this.handleTokenRefresh(ws, payload);
        return;
      case "token.refresh.ok":
      case "token.refresh.fail":
        this.handleTokenRefreshReceipt(ws, payload);
        return;
      case "token.sync":
        this.handleTokenReconcile(ws, payload, false);
        return;
      case "token.reset":
        this.handleTokenReconcile(ws, payload, true);
        return;
      case "token.put":
        this.handleTokenPut(ws, payload);
        return;
      case "token.delete":
        this.handleTokenDelete(ws, payload);
        return;
      default:
        ws.send(JSON.stringify({ t: "error", reason: "unknown_frame_type" }));
    }
  }

  handleTokenPut(ws, payload) {
    if (!this.isCurrentDesktopWriter(ws)) {
      this.rejectTokenMutation(ws, payload, this.writerRejectionReason(ws));
      return;
    }
    let result;
    try {
      const entry = tokenPutFrameToEntry(payload);
      result = store.putTokenRegistryEntry(this.sql, entry, Date.now(), { cas: true });
    } catch (error) {
      const reason = tokenMutationRejectReason(error);
      if (!reason) throw error;
      result = { result: "rejected", reason };
    }
    const exceeded = result.result === "rejected" && this.recordProtocolViolation(ws);
    this.sendTokenAck(ws, payload, result);
    if (exceeded) this.closeSocketForReauthorization(ws);
  }

  handleTokenDelete(ws, payload) {
    if (!this.isCurrentDesktopWriter(ws)) {
      this.rejectTokenMutation(ws, payload, this.writerRejectionReason(ws));
      return;
    }
    let result;
    try {
      if (!Number.isSafeInteger(payload.generation) || payload.generation <= 0) {
        throw new TypeError("invalid token subject generation");
      }
      if (payload.close !== undefined && typeof payload.close !== "boolean") {
        throw new TypeError("invalid close flag");
      }
      result = store.putTokenRegistryEntry(this.sql, {
        subject: payload.subject,
        generation: payload.generation,
        state: "revoked",
        scope: null,
        aliases: [],
      }, Date.now(), { cas: true });
    } catch (error) {
      const reason = tokenMutationRejectReason(error);
      if (!reason) throw error;
      result = { result: "rejected", reason };
    }
    let exceeded = false;
    if (result.result === "rejected") {
      exceeded = this.recordProtocolViolation(ws);
    } else if (payload.close === true) {
      this.closeSocketsForRevokedSubject(payload.subject);
    }
    this.sendTokenAck(ws, payload, result);
    if (exceeded) this.closeSocketForReauthorization(ws);
  }

  // ---- S1i2 §9.6 第 246 行：refresh 三帧成套的手机→relay 半边 ----
  // 身份=attachment（母文档「手机 token.refresh 到达 → 盖章身份」）：subject/
  // request_generation 绝不信手机自报，一律读已通过 authorizeInboundSocket
  // 再授权闸的 attachment（到这里时 attachment.generation 已由该闸保证等于
  // subject 当前 generation）。
  handleTokenRefresh(ws, payload) {
    const attachment = safeAttachment(ws);
    const shapeError = tokenRefreshShapeError(payload);
    if (shapeError) {
      this.rejectTokenRefresh(ws, shapeError);
      return;
    }
    if (!attachment.subject) {
      // legacy dev remote（S1j 前遗留、无 subject 可盖章身份）明确拒绝，不
      // 假装能处理——§9.6 的整套投递谓词都要靠一个真实 subject 撑住。
      this.rejectTokenRefresh(ws, "subject_required");
      return;
    }

    const existing = store.getRefreshRequest(this.sql, payload.request_id);
    const isResend = existing != null && existing.subject === attachment.subject;
    // §9.8 6/min 主桶；resend 不计主配额，但 R4 补一道独立宽松桶（见常量注释）。
    if (isResend) {
      if (!this.takeRefreshResendSlot(ws)) return;
    } else if (!this.takeRefreshRequestSlot(ws)) {
      return;
    }

    const desktop = this.onlineDesktopForEpoch(store.getCurrentEpoch(this.sql));
    if (!desktop) {
      ws.send(JSON.stringify({ t: "error", reason: "desktop_offline" }));
      return;
    }

    const requestGeneration = Number(attachment.generation);
    const upsert = store.upsertRefreshRequest(this.sql, {
      requestId: payload.request_id,
      subject: attachment.subject,
      requestGeneration,
      connectionId: attachment.connection_id,
      deadline: Date.now() + REFRESH_REQUEST_TTL_MS,
      // R3（§9.8 第 263 行）：休眠唤醒后的 webSocketMessage 拿不到原始
      // Request，per-IP 计费一律用 upgrade 时写进 attachment 的 key，不现取。
      ipBucketKey: attachment.ip_bucket_key ?? null,
    });
    if (!upsert.ok) {
      this.rejectTokenRefresh(ws, upsert.reason);
      return;
    }

    desktop.send(JSON.stringify({
      t: "token.refresh.forward",
      request_id: payload.request_id,
      subject: attachment.subject,
      request_generation: requestGeneration,
      ct: payload.ct,
      n: payload.n,
    }));
    void this.scheduleNextTokenAlarm();
  }

  rejectTokenRefresh(ws, reason) {
    const exceeded = this.recordProtocolViolation(ws);
    ws.send(JSON.stringify({ t: "error", reason, frame: "token.refresh" }));
    if (exceeded) this.closeSocketForReauthorization(ws);
  }

  // pair.hello 同款 per-socket 滑窗设施（S1e 既有模式，非新造）：窗口过期即
  // 重置计数；攒满即回错误 + 关连接，聚合计数走既有 recordProtocolViolation。
  takeRefreshRequestSlot(ws, now = Date.now()) {
    const attachment = safeAttachment(ws);
    if (!Number.isSafeInteger(attachment.refresh_window_started_at) ||
        now >= attachment.refresh_window_started_at + REFRESH_REQUEST_WINDOW_MS) {
      attachment.refresh_window_started_at = now;
      attachment.refresh_attempts = 0;
    }
    if (Number(attachment.refresh_attempts) >= REFRESH_REQUEST_LIMIT) {
      this.recordProtocolViolation(ws);
      try {
        ws.send(JSON.stringify({ t: "error", reason: "token_refresh_rate_limited" }));
      } finally {
        this.closeSocketForReauthorization(ws);
      }
      return false;
    }
    attachment.refresh_attempts = Number(attachment.refresh_attempts) + 1;
    this.assertAttachmentBudget(attachment);
    ws.serializeAttachment(attachment);
    return true;
  }

  // R4（§9.6/§9.8 v1.8.5）：resend 独立宽松桶——与 takeRefreshRequestSlot 同款
  // 滑窗设施，但用另一组 attachment 字段，不共享计数（否则一次 resend 就会
  // 提前耗尽本该留给新请求的 6/min 主配额，二者语义不同不能混用同一个桶）。
  takeRefreshResendSlot(ws, now = Date.now()) {
    const attachment = safeAttachment(ws);
    if (!Number.isSafeInteger(attachment.refresh_resend_window_started_at) ||
        now >= attachment.refresh_resend_window_started_at + REFRESH_RESEND_WINDOW_MS) {
      attachment.refresh_resend_window_started_at = now;
      attachment.refresh_resend_attempts = 0;
    }
    if (Number(attachment.refresh_resend_attempts) >= REFRESH_RESEND_LIMIT) {
      this.recordProtocolViolation(ws);
      try {
        ws.send(JSON.stringify({ t: "error", reason: "token_refresh_resend_rate_limited" }));
      } finally {
        this.closeSocketForReauthorization(ws);
      }
      return false;
    }
    attachment.refresh_resend_attempts = Number(attachment.refresh_resend_attempts) + 1;
    this.assertAttachmentBudget(attachment);
    ws.serializeAttachment(attachment);
    return true;
  }

  // ---- S1i2 §9.6 第 246 行：refresh 三帧成套的桌面→relay→手机半边 ----
  // ok/fail 的形状校验就地做（桌面→relay 走 desktop 入站矩阵，到这里时
  // epoch/registry_ready 前置闸已经过——见 webSocketMessage 顶部 stale_epoch
  // 与 sync_required 两道闸，desktop 写者身份已由它们保证，不重复判）。
  handleTokenRefreshReceipt(ws, payload) {
    const shapeError = tokenRefreshReceiptShapeError(payload);
    if (shapeError) {
      const exceeded = this.recordProtocolViolation(ws);
      ws.send(JSON.stringify({ t: "error", reason: shapeError, frame: payload.t }));
      if (exceeded) this.closeSocketForReauthorization(ws);
      return;
    }
    this.deliverRefreshReceipt(payload);
  }

  /**
   * §9.6 第 246 行投递谓词，逐条实现：
   *   ① 房 live
   *   ② subject active
   *   ③ 回执.subject == 请求行.subject
   *   ④ 回执.generation == subject 当前 generation（轮换后新代·不要求等于
   *      request_generation；fail 帧结构上不带 generation，天然豁免本条）
   *   ⑤ now < deadline
   *   ⑥ 目标 = 请求行 connection_id（已断则丢弃·手机走别名重放/resend 恢复）
   * 投完/过期删行。**自有谓词，故意绕过 canDeliverOutbound**——S1i1 遗留
   * （桌面幂等重放不等 put 的 ack 就直接发回执）使得回执到达时手机 socket
   * 的 attachment 章可能还是旧代；canDeliverOutbound 的 isOfficialAttachmentLive
   * 会先把这条 socket 判死、回执因此永远送不到，每次轮换都被迫走「断线 +
   * prev 重连 + journal 重放」的慢路径。这里独立复核 token_subjects 的当前
   * 权威 generation（不信 attachment 缓存的旧值），是这条路径上唯一防线：
   * 只要④成立就投，不管发送这条回执的桌面连接此刻的 epoch/attachment 是否
   * 仍与目标 socket 的旧 attachment 一致。
   */
  deliverRefreshReceipt(payload, now = Date.now()) {
    // R5.2：assertRoomLive 前置——与「assertRoomLive 无路由例外」的写法纪律
    // 对齐（fetch/webSocketMessage/webSocketClose/alarm/构造器一律先判 live
    // 再摸 DB），本函数之前是先读行再判 live，顺序反了。
    if (!this.assertRoomLive()) return; // ①

    const row = store.getRefreshRequest(this.sql, payload.request_id);
    if (!row) return; // 未知/已消费的 request_id：安全丢弃，不是本谓词的六条之一

    const subject = store.getTokenSubject(this.sql, row.subject);
    if (!subject || subject.state !== "active") return; // ②

    if (payload.subject !== row.subject) return; // ③

    if (payload.t === "token.refresh.ok" &&
        Number(payload.generation) !== Number(subject.generation)) {
      return; // ④
    }

    if (!(now < Number(row.deadline))) { // ⑤
      store.deleteRefreshRequest(this.sql, row.request_id);
      return;
    }

    const target = this.ctx.getWebSockets().find(
      (candidate) => safeAttachment(candidate).connection_id === row.connection_id
    );
    if (!target) return; // ⑥ 已断则丢弃·不删行——手机走别名重放/resend 重绑恢复
    // R1（双路审）：真实 Cloudflare 运行时里，close 握手期间 getWebSockets()
    // 仍可能返回一条正处于 CLOSING 的 socket——它已经被 canDeliverOutbound
    // 判过期而调了 closeSocketForReauthorization()，但 Hibernation API 不保证
    // 那一刻它就从 getWebSockets() 里消失。此刻投递等于把回执塞给正在离场的
    // 连接，而「投完删行」还会把这份回执从 refresh_requests 里删掉——手机走
    // prev 别名/resend 的恢复路径就再也捞不回它。同 ⑥ 语义：readyState 非
    // OPEN 时也按 ⑥ 丢弃且不删行。选 readyState 检查而不是 try/catch send
    // 失败：WebSocket 规范里 `send()` 在 CLOSING/CLOSED 状态下是**静默丢弃**、
    // 不抛异常（MDN：「If you call send() when the connection is in the
    // CLOSING or CLOSED states, the browser will silently discard the
    // data」），try/catch 在这个场景下根本捕不到东西，只有先查 readyState
    // 才能在投递前发现——@cloudflare/workers-types 的 WebSocket 接口本就有
    // `readyState: number`（WebSocketPair 两端都是 WebSocket 类型），
    // Hibernation server socket 上这个属性存在。
    if (Number(target.readyState) !== 1 /* WebSocket.OPEN */) return;
    // R5.1：对齐 canDeliverOutbound:1318 的纵深检查——不是本谓词的六条之一，
    // 是额外一道防线。目标连接正因 §9.8 并发上限被清退（enforceSubjectSocketLimit
    // 已标记但可能还没真正 close 完成），此刻投递等于把回执塞给一个正在离场
    // 的连接；同 ⑥ 不删行，交给之后的重试/resend 走另一个存活连接。
    if (safeAttachment(target).subject_limit_closed === true) return;

    target.send(JSON.stringify(payload));
    store.deleteRefreshRequest(this.sql, row.request_id); // 投完删行
  }

  handleTokenReconcile(ws, payload, reset) {
    if (!this.isCurrentDesktopWriter(ws)) {
      this.rejectStaleDesktopFrame(ws);
      return;
    }
    if (!Number.isSafeInteger(payload.revision) || payload.revision <= 0) {
      ws.send(JSON.stringify({ t: "error", reason: "revision_required", frame: payload.t }));
      return;
    }
    if (!Array.isArray(payload.entries)) {
      ws.send(JSON.stringify({ t: "error", reason: "entries_required", frame: payload.t }));
      return;
    }
    if (payload.entries.length > SYNC_ENTRY_LIMIT) {
      this.recordProtocolViolation(ws);
      const reason = reset ? "reset_entries_too_many" : "sync_entries_too_many";
      try {
        ws.send(JSON.stringify({ t: "error", reason, frame: payload.t }));
      } finally {
        this.closeSocketForReauthorization(ws);
      }
      return;
    }

    const entries = payload.entries.map((entry) => {
      try {
        return tokenPutFrameToEntry(entry);
      } catch {
        return { invalid: true, subject: entry?.subject };
      }
    });
    const result = store.reconcileTokenRegistry(this.sql, {
      revision: payload.revision,
      entries,
      reset,
    }, Date.now());

    for (const subject of new Set(result.revokedSubjects)) {
      this.closeSocketsForRevokedSubject(subject);
    }
    ws.send(JSON.stringify({
      t: "token.sync.ack",
      revision: payload.revision,
      relay_high_water: result.relayHighWater,
    }));

    const attachment = safeAttachment(ws);
    if (attachment.registry_ready === true) return;
    attachment.registry_ready = true;
    delete attachment.registry_sync_deadline;
    this.assertAttachmentBudget(attachment);
    ws.serializeAttachment(attachment);
    this.desktopHello(ws, attachment.epoch);
    this.flushPendingInputTo(ws);
    this.broadcastPresence("desktop", "online", ws);
    void this.scheduleNextTokenAlarm();
  }

  isCurrentDesktopWriter(ws) {
    const attachment = safeAttachment(ws);
    return attachment.role === "desktop" &&
      attachment.scope === "desktop" &&
      Number(attachment.epoch) === store.getCurrentEpoch(this.sql);
  }

  writerRejectionReason(ws) {
    const attachment = safeAttachment(ws);
    if (attachment.role === "desktop" && attachment.scope === "desktop") return "stale_epoch";
    return "writer_forbidden";
  }

  rejectTokenMutation(ws, payload, reason) {
    const exceeded = this.recordProtocolViolation(ws);
    this.sendTokenAck(ws, payload, { result: "rejected", reason });
    if (exceeded) this.closeSocketForReauthorization(ws);
  }

  sendTokenAck(ws, payload, result) {
    const ack = {
      t: "token.ack",
      subject: typeof payload.subject === "string" ? payload.subject : null,
      generation: Number.isSafeInteger(payload.generation) ? payload.generation : null,
      result: result.result,
    };
    if (result.reason) ack.reason = result.reason;
    ws.send(JSON.stringify(ack));
  }

  closeSocketsForRevokedSubject(subject) {
    for (const socket of this.ctx.getWebSockets()) {
      if (safeAttachment(socket).subject !== subject) continue;
      try {
        socket.send(JSON.stringify({ t: "error", reason: "device_revoked" }));
      } finally {
        this.closeSocketForReauthorization(socket);
      }
    }
  }

  // ---- 重连补发（母文档 §6） ----
  // 桌面不走 replayTo 的里程碑补发，握手后需单独同步新 epoch/headSeq（§6.4 v1.4）。
  desktopHello(ws, epoch) {
    ws.send(JSON.stringify({ t: "replay.head", epoch, headSeq: store.headSeq(this.sql) }));
  }

  replayTo(ws, lastSeq) {
    const rows = store.replaySince(this.sql, lastSeq);
    const headSeq = store.headSeq(this.sql);
    const epoch = store.getCurrentEpoch(this.sql);
    const roomId = store.getMeta(this.sql, "room_id");

    const head = { t: "replay.head", epoch, headSeq };
    if (!this.canDeliverOutbound(ws, head)) return;
    ws.send(JSON.stringify(head));

    for (const row of rows) {
      const envelope = {
        v: PROTOCOL_VERSION,
        room: roomId,
        epoch: row.epoch,
        kind: row.kind,
        session: row.session,
        command_id: null, // 里程碑不走 input 通道，command_id 恒无意义
        seq: row.seq,
        client_msg_id: row.client_msg_id ?? null,
        ct: row.ct,
        n: row.n,
        ts: row.ts,
      };
      if (!this.canDeliverOutbound(ws, envelope)) return;
      ws.send(JSON.stringify(envelope));
    }
  }

  // ---- 桌面重连：把暂存的 input 排空（FIFO）+ 清理过期项 ----
  flushPendingInputTo(desktopWs) {
    const { deliverable, expired } = store.drainDeliverableInput(this.sql, Date.now());

    for (const row of deliverable) {
      if (!store.isPendingInputAuthorized(this.sql, row)) expired.push(row);
    }

    for (const row of expired) {
      store.removePendingInput(this.sql, row.command_id);
      this.broadcastToRemotes({ t: "input.expired", command_id: row.command_id });
    }

    for (const row of deliverable) {
      if (expired.includes(row)) continue;
      // row.envelope 已经是入队时存下的、含 command_id 的单层信封 JSON 字符串
      // ——原样转发，不需要 parse 再包一层。
      desktopWs.send(row.envelope);
    }
  }

  // ---- G8 R4（双路审 fix_required④）：handleInput 入队前的死行清扫 ----
  // 同款 input.expired 广播语义（照既有 flushPendingInputTo 的做法），但
  // 不核 isPendingInputAuthorized 撤销分支——那是桌面上线交付时才需要核的
  // 额外条件（订阅撤销），入队前这一步只关心纯 TTL 过期，两者是不同的
  // 「过期」概念，不能混用同一段逻辑。
  //
  // G8 三审 fix_required（codex xhigh 差量审）R3：不再复用
  // drainDeliverableInput——这条路径现在每次 handleInput 入队前都会跑一遍
  // （高频），drainDeliverableInput 是"SELECT * ... 把整表（含大字段
  // envelope）搬进 JS"给桌面上线那条低频路径用的，两条路径的读取代价不该
  // 混在一起；改用 store.deleteExpiredPendingInput——只按 expires_at 索引
  // 取 command_id、按同一谓词批量删，全程不碰 envelope 列（那条路径本身
  // 不动，仍服务 flushPendingInputTo）。
  purgeExpiredPendingInput(now = Date.now()) {
    const expiredCommandIds = store.deleteExpiredPendingInput(this.sql, now);
    for (const commandId of expiredCommandIds) {
      this.broadcastToRemotes({ t: "input.expired", command_id: commandId });
    }
  }

  // ---- 小工具 ----
  assertRoomLive() {
    return store.isRoomLive(this.sql);
  }

  closeSocketForTombstone(ws) {
    try {
      ws.close(TOMBSTONE_CLOSE_CODE, TOMBSTONE_CLOSE_REASON);
    } catch {
      // Socket 可能已经进入 close 回调；生命周期真相仍已由 sentinel 拒绝。
    }
  }

  closeAllSockets() {
    for (const ws of this.ctx.getWebSockets()) {
      this.closeSocketForTombstone(ws);
    }
  }

  rejectOversizedFrame(ws) {
    this.recordProtocolViolation(ws);
    try {
      ws.send(JSON.stringify({ t: "error", reason: "frame_too_large" }));
    } finally {
      try {
        ws.close(1009, "frame_too_large");
      } catch {
        // 已关闭连接无需二次处置；尺寸闸已在 parse 前完成。
      }
    }
  }

  rejectStaleDesktopFrame(ws) {
    const currentEpoch = store.getCurrentEpoch(this.sql);
    try {
      ws.send(JSON.stringify({ t: "error", reason: "stale_epoch", currentEpoch }));
    } finally {
      this.closeSocketForReauthorization(ws);
    }
  }

  closeDesktopForSyncTimeout(ws) {
    try {
      ws.send(JSON.stringify({ t: "error", reason: "sync_timeout" }));
    } finally {
      try {
        ws.close(1008, "sync_timeout");
      } catch {
        // alarm 与 close 回调可竞态；只要该连接不再成为 selector 候选即可。
      }
    }
  }

  onlineDesktop() {
    return this.onlineDesktopForEpoch(store.getCurrentEpoch(this.sql));
  }

  broadcastReplacedDesktopOffline(currentEpoch) {
    const replacedReadyDesktop = this.ctx.getWebSockets("desktop").some((ws) => {
      const attachment = safeAttachment(ws);
      return attachment.role === "desktop" && attachment.scope === "desktop" &&
        attachment.registry_ready === true &&
        Number(attachment.epoch) < Number(currentEpoch);
    });
    if (replacedReadyDesktop) this.broadcastPresence("desktop", "offline");
  }

  onlineDesktopForEpoch(currentEpoch) {
    return this.ctx.getWebSockets("desktop").find((ws) => {
      const attachment = safeAttachment(ws);
      return Number(attachment.epoch) === Number(currentEpoch) && attachment.registry_ready === true;
    }) ?? null;
  }

  async rejectUpgradeAuthentication(ipBucketKey, hashPrefix, now = Date.now()) {
    return ratelimit.rejectUpgradeAuthentication(this, ipBucketKey, hashPrefix, now);
  }

  takePairHelloSlot(ws, now = Date.now()) {
    return ratelimit.takePairHelloSlot(this, ws, now);
  }

  takeSubjectChannelRateSlot(ws, envelope, { channel, limit, windowMs, reason }, now = Date.now()) {
    return ratelimit.takeSubjectChannelRateSlot(this, ws, envelope, { channel, limit, windowMs, reason }, now);
  }

  takeIpMessageSlot(ws, frameType, commandId, now = Date.now()) {
    return ratelimit.takeIpMessageSlot(this, ws, frameType, commandId, now);
  }

  closeSocketForMessageRateLimit(ws) {
    return ratelimit.closeSocketForMessageRateLimit(this, ws);
  }

  enforceSubjectSocketLimit(subject) {
    return ratelimit.enforceSubjectSocketLimit(this, subject);
  }

  broadcastRaw(payload, excludeWs) {
    const text = JSON.stringify(payload);
    for (const ws of this.ctx.getWebSockets()) {
      if (ws === excludeWs) continue;
      if (this.canDeliverOutbound(ws, payload)) ws.send(text);
    }
  }

  broadcastToRemotes(payload) {
    const text = JSON.stringify(payload);
    for (const ws of this.ctx.getWebSockets("remote")) {
      if (this.canDeliverOutbound(ws, payload)) ws.send(text);
    }
  }

  // S1i3 F1（§9.5 第 235 行）：pair.accept / pair.ready 定向投递——只发给 pairing_routes
  // 里记的那一条 connection_id（最近一次 pair.hello/pair.done 的发起者），不再
  // broadcastToRemotes 广播给房间内全部远端。复用 deliverRefreshReceipt 按 connection_id
  // 查 socket 的写法。没有路由行（从未发生过配对）或目标连接已断——安全丢弃，不是错误：
  // 配对本就可能因手机中途断线而失败，注册表那条 pairing 授权自会在 300s TTL 后自然过期。
  deliverToPairingRoute(payload) {
    // S1i3 K3.1：assertRoomLive 前置——与 deliverRefreshReceipt 的既有纪律（:957 摸 DB
    // 前先判 live）对齐；「assertRoomLive 无路由例外」，摸 DB 的方法各自独立判 live，
    // 不依赖调用方（当前唯一调用方 webSocketMessage 已经判过）不出错。
    if (!this.assertRoomLive()) return;
    const connectionId = store.getPairingRoute(this.sql);
    if (!connectionId) return;
    // S1i3 K3.2：目标恒是发起配对的那条 remote（手机）连接，收窄成按 "remote" tag 扫，
    // 不再像之前那样囫囵扫全量（含 desktop）——跟已撤掉的 broadcastToRemotes 用
    // getWebSockets("remote") 是同一层纵深防线，缩小误投面。
    const target = this.ctx.getWebSockets("remote").find(
      (candidate) => safeAttachment(candidate).connection_id === connectionId
    );
    if (!target) return;
    if (!this.canDeliverOutbound(target, payload)) return;
    target.send(JSON.stringify(payload));
  }

  // S1i3 K3.1：pair.hello / pair.done 转发前落路由行都要走这里——与 deliverToPairingRoute
  // 的既有纪律对齐（摸 DB 前先判 live），也让「这张表唯一的写口」只有一处，不给未来任何
  // 新调用点留「忘了先判 live 就写库」的空子。
  recordPairingRoute(connectionId) {
    if (!this.assertRoomLive()) return;
    store.setPairingRoute(this.sql, connectionId);
  }

  broadcastPresence(role, event, excludeWs) {
    this.broadcastRaw({ t: "presence", role, event, ts: Date.now() }, excludeWs);
  }

  // C1-PS（dogfood 修障第二批·手机发消息桌面离线无反馈）：定向投给单条刚接入的 remote socket——不走
  // broadcastRaw/broadcastPresence（那两个会打给房间内全部符合投递闸的连接，
  // 这里只该打给这一条新连接自己）。仍过 canDeliverOutbound 同一道投递闸——
  // 跟 replayTo 对 replay.head/里程碑帧的既有纪律一致，不因为是"新连接自己"
  // 就绕开检查。
  sendDesktopPresenceSnapshot(ws, epoch) {
    const online = this.onlineDesktopForEpoch(epoch) != null;
    const frame = { t: "presence", role: "desktop", event: online ? "online" : "offline", ts: Date.now() };
    if (!this.canDeliverOutbound(ws, frame)) return;
    ws.send(JSON.stringify(frame));
  }

  authorizeInboundSocket(ws, attachment, now = Date.now()) {
    if (attachment.subject_limit_closed === true) return false;
    if (attachment.scope === "desktop" && attachment.role !== "desktop") {
      const exceeded = this.recordProtocolViolation(ws);
      ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", role: attachment.role ?? null }));
      if (exceeded) this.closeSocketForReauthorization(ws);
      return false;
    }
    if (!INBOUND_SCOPE_MATRIX[attachment.scope]) {
      // scope 缺失（S1d 之前的老 attachment）或不在矩阵里（未来新 scope 的
      // 连接撞上旧代码）都没有任何可放行的帧型——留着这条连接只能让它一直
      // 空转重试，直接 close 让客户端带新凭据重连。
      this.recordProtocolViolation(ws);
      try {
        ws.send(JSON.stringify({ t: "error", reason: "role_forbidden", role: attachment.role ?? null }));
      } finally {
        this.closeSocketForReauthorization(ws);
      }
      return false;
    }
    // desktop 的对称权威闸仍是各 handler 的 epoch 检查（S1a）。
    if (attachment.role === "desktop" && attachment.scope === "desktop") return true;
    // S1ja F2：legacy dev valid_tokens 的过渡期豁免已撤——没有 subject 行的
    // remote/pairing/refresh socket 只可能来自已删除的后门（room_meta 存量行
    // 也随 F2 迁移一并清空），永不过期、不可吊销，是一条永久后门；现在跟任何
    // 其它没有真实 subject 的 attachment 一样，落进下面这条 fail-closed 分支。
    if (!attachment.subject || !this.isOfficialAttachmentLive(attachment, now)) {
      const subject = attachment.subject ? store.getTokenSubject(this.sql, attachment.subject) : null;
      const reason = subject && Number(subject.generation) !== Number(attachment.generation)
        ? "stale_generation"
        : "reauthorization_failed";
      try {
        ws.send(JSON.stringify({ t: "error", reason }));
      } finally {
        this.closeSocketForReauthorization(ws);
      }
      return false;
    }
    return true;
  }

  isOfficialAttachmentLive(attachment, now = Date.now()) {
    const subject = store.getTokenSubject(this.sql, attachment.subject);
    if (!subject || subject.state !== "active") return false;
    if (Number(subject.generation) !== Number(attachment.generation)) return false;
    const alias = store.getTokenAlias(
      this.sql,
      attachment.subject,
      attachment.kind
    );
    if (!alias || Number(alias.generation) !== Number(attachment.alias_generation)) return false;
    if (attachment.kind === "current") {
      if (Number(attachment.access_expires) !== Number(alias.access_expires) ||
          Number(attachment.valid_until) !== Number(alias.valid_until)) {
        return false;
      }
    } else if (attachment.kind === "prev") {
      if (Number(attachment.valid_until) !== Number(alias.valid_until)) return false;
    } else {
      return false;
    }
    if (attachment.scope === "refresh") return now < Number(attachment.valid_until);
    if (["remote", "pairing"].includes(attachment.scope)) {
      return attachment.kind === "current" && now < Number(attachment.access_expires);
    }
    return false;
  }

  async scheduleNextTokenAlarm(now = Date.now(), closingWs = null) {
    if (typeof this.ctx.storage.setAlarm !== "function") return;
    let nearest = null;
    // The closing socket may still be listed inside its own close callback.
    const liveSockets = this.ctx.getWebSockets().filter((ws) => ws !== closingWs);
    for (const ws of liveSockets) {
      const attachment = safeAttachment(ws);
      if (attachment.role === "desktop" && attachment.scope === "desktop" && attachment.registry_ready !== true) {
        const deadline = Number(attachment.registry_sync_deadline);
        if (Number.isSafeInteger(deadline) && (nearest == null || deadline < nearest)) nearest = deadline;
        continue;
      }
      if (!attachment.subject || !this.isOfficialAttachmentLive(attachment, now)) continue;
      const expires = attachment.scope === "refresh"
        ? Number(attachment.valid_until)
        : Number(attachment.access_expires);
      if (Number.isSafeInteger(expires) && expires > now && (nearest == null || expires < nearest)) {
        nearest = expires;
      }
    }
    // P2-3：refresh_requests.deadline 是第三类待办时刻，与上面两类（registry
    // sync 超时 / token 过期）统一进同一个 min 计算，一处算、一处设——不再是
    // 「最后 setAlarm 的那类独占」，避免二者互踩。
    const refreshDeadline = store.nextRefreshRequestDeadline(this.sql, now);
    if (refreshDeadline != null && (nearest == null || refreshDeadline < nearest)) {
      nearest = refreshDeadline;
    }
    // Include the next reply route deadline in the shared minimum so route
    // cleanup uses the same alarm slot as refresh and other scheduled work.
    const replyRouteDeadline = store.nextReplyRouteDeadline(this.sql, now);
    if (replyRouteDeadline != null && (nearest == null || replyRouteDeadline < nearest)) {
      nearest = replyRouteDeadline;
    }
    // SEC-3: fixed reclaim deadline of never-authenticated rooms (same gate as
    // alarm(): no room_meta, claimed or not). created_at should never be null;
    // if it is, skip the candidate rather than guess.
    const unclaimedState = store.getRoomState(this.sql);
    if (!store.hasTable(this.sql, "room_meta") && unclaimedState.created_at != null) {
      const reclaimAt = Number(unclaimedState.created_at) + UNCLAIMED_RECLAIM_MS;
      if (nearest == null || reclaimAt < nearest) nearest = reclaimAt;
    }
    nearest = withAbandonedCandidate(this.sql, liveSockets.length, now, nearest);
    // C1-TTL（dogfood 修障第二批·手机发消息桌面离线无反馈）：pending_input
    // 暂存 TTL 到期时刻——第五类候选，同款「一处算一处设」结构。之前
    // purgeExpiredPendingInput 只在 handleInput 高频路径里顺带触发，没有
    // alarm 驱动的兜底——房间若从此再没有新 input 涌入，早该过期的暂存行会
    // 一直滞留，手机端在这条通道上永远等不到 input.expired 回执。
    const pendingInputExpiry = store.nextPendingInputExpiry(this.sql, now);
    if (pendingInputExpiry != null) {
      // R4（返工·TTL 死区）：`nextPendingInputExpiry` 用 `expires_at > now`（只看未来的
      // 行），`purgeExpiredPendingInput`/`alarm()` 收尾清扫用 `expires_at < now`（只清
      // 已过去的行）——`expires_at === now` 恰好落在两条查询的公共盲区之外：这一刻这行
      // 既不算"未来"（不会被这里选中当下一次候选）也不算"过去"（不会被清扫删掉）。若
      // alarm 精确排在 `expires_at` 这个时刻触发，`alarm()` 内那次 `Date.now()` 一旦
      // 恰好等于它，两条查询都会漏掉这一行——它就永久滞留在 `pending_input` 里，
      // `input.expired` 回执再也不会广播。这里把排的候选时刻定为 `expires_at + 1`
      // （比真正到期时刻晚 1 毫秒）：保证 alarm 触发时 `Date.now() > expires_at`，
      // 稳定落进 purge 那条 `<` 判据的真过去一侧。**不**改 `nextPendingInputExpiry`
      // 本身的 `>` 为 `>=`——那会让还没真正过期（`expires_at === now` 那一刻）的行
      // 立即被当"未来候选"选中、算出 `nearest === now`，与"已有相同/更早候选就不
      // 重排"那条去重判断（下面几行）反复打架，形成 setAlarm 紧循环。
      const candidate = pendingInputExpiry + 1;
      if (nearest == null || candidate < nearest) nearest = candidate;
    }
    if (nearest != null) {
      // All alarm scheduling converges here, including rejected requests, so
      // unconditional writes would let repeated requests amplify storage writes.
      // Keep an existing alarm at the same or an earlier time: when it fires,
      // scheduling is recomputed and rearmed as needed. Write only if no alarm
      // exists or the new minimum is earlier, preserving the shared slot's
      // earliest-deadline behavior across token and room cleanup candidates.
      const existingAlarm = typeof this.ctx.storage.getAlarm === "function"
        ? await this.ctx.storage.getAlarm()
        : null;
      if (existingAlarm == null || existingAlarm > nearest) {
        await this.ctx.storage.setAlarm(nearest);
      }
    }
  }

  canDeliverOutbound(ws, payload, now = Date.now()) {
    const attachment = safeAttachment(ws);
    if (attachment.subject_limit_closed === true) return false;
    if (attachment.role === "desktop" && attachment.scope === "desktop") {
      return attachment.registry_ready === true &&
        Number(attachment.epoch) === store.getCurrentEpoch(this.sql);
    }
    if (!attachment.scope) return false;

    const frameType = typeof payload?.t === "string" ? payload.t : payload?.kind;
    const isPairingFrame = typeof frameType === "string" && frameType.startsWith("pair.");
    const isRefreshForward = frameType === "token.refresh.forward";
    const isRefreshReceipt = ["token.refresh.ok", "token.refresh.fail"].includes(frameType);

    // S1ja F2：legacy remote 的「视同活 current remote」豁免已撤（同族于
    // authorizeInboundSocket 那条，同一批一并收紧）——没有 subject 就没有
    // token_subjects/token_aliases 行可查，不可能是真实的当前授权连接，
    // 不再投递任何出站帧给它。
    if (!attachment.subject) return false;
    if (!this.isOfficialAttachmentLive(attachment, now)) {
      this.closeSocketForReauthorization(ws);
      return false;
    }
    if (isPairingFrame) return attachment.scope === "pairing";
    if (isRefreshForward) return false;
    // S1i2 §9.6/P1-b：refresh 回执改走 deliverRefreshReceipt 自有谓词——本闸
    // 恒不投递这两个帧型，不是遗漏。旧实现在这里放行的条件是「发送目标 socket
    // 自己的 attachment.generation 仍等于 subject 当前 generation」，但桌面
    // 轮换升代后手机 socket 的 attachment 章还是旧代，isOfficialAttachmentLive
    // 已经在上面把它判死、直接 return false——回执因此永远送不到这条分支，
    // 每次轮换都被迫走「断线 + prev 重连 + journal 重放」的慢路径。
    // deliverRefreshReceipt 独立复核 token_subjects 的当前权威 generation
    // （不依赖目标 socket 自己的 attachment 是否还新鲜），是真正的投递闸。
    if (isRefreshReceipt) return false;
    return attachment.scope === "remote" && attachment.kind === "current";
  }

  closeSocketForReauthorization(ws) {
    try {
      ws.close(REAUTH_CLOSE_CODE, REAUTH_CLOSE_REASON);
    } catch {
      // close/alarm 可能与当前投递竞态；SQL 行状态仍是最终真相。
    } finally {
      // 连接就此消失，它攒着的聚合计数不能跟着丢——close 是三个 flush 时机之一
      // （另两个是攒满一批和 alarm）。close 本身抛错也不影响 flush。
      this.flushProtocolViolations();
    }
  }

  /**
   * 记一次协议违规：聚合计数先攒在内存里、攒满一批才落一次库；per-socket 计数
   * 走 WeakMap。返回 true 表示这条 socket 已到 PROTOCOL_VIOLATION_LIMIT，调用
   * 方应当在回完错误帧之后关掉它。
   */
  recordProtocolViolation(ws = null) {
    this.pendingProtocolViolations += 1;
    if (this.pendingProtocolViolations >= PROTOCOL_VIOLATION_LIMIT) this.flushProtocolViolations();
    if (!ws) return false;
    const count = Number(this.socketProtocolViolations.get(ws) || 0) + 1;
    this.socketProtocolViolations.set(ws, count);
    return count >= PROTOCOL_VIOLATION_LIMIT;
  }

  flushProtocolViolations() {
    if (this.pendingProtocolViolations === 0) return;
    const pending = this.pendingProtocolViolations;
    this.pendingProtocolViolations = 0;
    // 墓碑房的业务表已被 purge，聚合计数没有可写的去处——丢掉即可，房间已终结。
    if (!this.assertRoomLive()) return;
    const count = Number(store.getMeta(this.sql, "protocol_violation_count", "0"));
    store.setMeta(this.sql, "protocol_violation_count", count + pending);
  }

  /**
   * 房间级 IP 桶盐：首次活房初始化时随机生成 32 字节并落 room_state（生成与
   * 落库同一个事务内完成）。有盐之后 ip_bucket_key = SHA-256(salt || IP) 截断，
   * 同一 IP 在同一房间恒定命中同一个桶，但拿到 key 的人反推不出 IP——裸
   * sha256(ip) 的取值空间只有全部 IPv4 地址那么大，离线枚举就能还原。盐只留
   * 在 SQL 里，不进 attachment、不进日志。
   * SEC-1：盐从 room_meta（业务 schema，鉴权成功后才建）迁到 room_state（哨兵
   * 表，恒存在）——deriveIpBucketKey 在鉴权前（S1i2 §9.8）就要用它，若仍挂
   * 业务 schema，这一步会把业务表拽回鉴权前无条件建的老问题。
   */
  ensureIpBucketSalt() {
    return store.withTransaction(this.sql, () => {
      const existing = store.getRoomIpBucketSalt(this.sql);
      if (existing) return existing;
      const salt = bytesToHex(globalThis.crypto.getRandomValues(new Uint8Array(32)));
      store.setRoomIpBucketSalt(this.sql, salt);
      return salt;
    });
  }

  async deriveIpBucketKey(ip) {
    if (!this.ipBucketSalt) this.ipBucketSalt = this.ensureIpBucketSalt();
    const digest = await globalThis.crypto.subtle.digest(
      "SHA-256",
      concatBytes(hexToBytes(this.ipBucketSalt), new TextEncoder().encode(ip))
    );
    return bytesToHex(new Uint8Array(digest).slice(0, 16));
  }

  currentQuotaState() {
    const period = currentPeriod(Date.now());
    const count = store.getQuotaCount(this.sql, period);
    const limit = Number(this.env && this.env.MONTHLY_MILESTONE_LIMIT) || DEFAULT_MONTHLY_MILESTONE_LIMIT;
    return evaluateQuota({ count, limit });
  }

  assertAttachmentBudget(obj) {
    const bytes = new TextEncoder().encode(JSON.stringify(obj)).length;
    if (bytes > ATTACHMENT_BYTE_BUDGET) {
      throw new Error(`serializeAttachment payload ${bytes}B exceeds ${ATTACHMENT_BYTE_BUDGET}B budget`);
    }
  }
}

// §9.6 wire-v1.json token_refresh_valid / token_refresh_request_id_missing 同源。
function requiredNonEmptyString(payload, field) {
  return typeof payload[field] === "string" && payload[field].length > 0 ? null : `${field}_required`;
}

// R2：request_id 落 refresh_requests PRIMARY KEY，128 字节口径同
// envelope.js:135-138 的 command_id 上限——按 UTF-8 字节计，不用 .length。
function requestIdTooLong(value) {
  return typeof value === "string" && new TextEncoder().encode(value).length > REQUEST_ID_MAX_BYTES;
}

function tokenRefreshShapeError(payload) {
  return requiredNonEmptyString(payload, "request_id") ??
    (requestIdTooLong(payload.request_id) ? "request_id_too_long" : null) ??
    requiredNonEmptyString(payload, "ct") ??
    requiredNonEmptyString(payload, "n");
}

// token_refresh_ok_valid / token_refresh_fail_valid / token_refresh_fail_in_flight_no_close_valid
// 同源；fail 帧结构上不带 generation（§9.6：close=true 仅用于连续 ≥3 次
// 无效判连续失败，缺省=良性重试不断连接）。
function tokenRefreshReceiptShapeError(payload) {
  const requestIdError = requiredNonEmptyString(payload, "request_id");
  if (requestIdError) return requestIdError;
  const subjectError = requiredNonEmptyString(payload, "subject");
  if (subjectError) return subjectError;

  if (payload.t === "token.refresh.ok") {
    if (!Number.isSafeInteger(payload.generation) || payload.generation <= 0) return "generation_required";
    return requiredNonEmptyString(payload, "ct") ?? requiredNonEmptyString(payload, "n");
  }
  if (payload.t === "token.refresh.fail") {
    const reasonError = requiredNonEmptyString(payload, "reason");
    if (reasonError) return reasonError;
    if (Object.hasOwn(payload, "close") && typeof payload.close !== "boolean") return "close_invalid";
    return null;
  }
  return "frame_type_invalid";
}

function tokenPutFrameToEntry(payload) {
  if (!payload.current || typeof payload.current !== "object" || Array.isArray(payload.current)) {
    throw new TypeError("current token alias required");
  }
  if (payload.subject === "pairing") {
    if (payload.scope !== "pairing") throw new TypeError("pairing subject requires pairing scope");
    if (payload.prev !== undefined) throw new TypeError("pairing subject forbids prev alias");
    if (payload.current.refresh_until !== undefined) {
      throw new TypeError("pairing subject forbids refresh_until");
    }
  }
  const aliases = [{
    token_hash: payload.current.token_hash,
    kind: "current",
    generation: payload.generation,
    access_expires: payload.current.access_expires,
    valid_until: payload.subject === "pairing"
      ? payload.current.access_expires
      : payload.current.refresh_until,
  }];
  if (payload.prev !== undefined) {
    if (!payload.prev || typeof payload.prev !== "object" || Array.isArray(payload.prev)) {
      throw new TypeError("invalid prev token alias");
    }
    if (payload.prev.token_hash === undefined) {
      throw new TypeError("prev token hash required");
    }
    aliases.push({
      token_hash: payload.prev.token_hash,
      kind: "prev",
      generation: payload.prev.generation,
      access_expires: null,
      valid_until: payload.prev.prev_expires,
    });
  }
  return {
    subject: payload.subject,
    generation: payload.generation,
    state: "active",
    scope: payload.scope,
    aliases,
  };
}

function tokenMutationRejectReason(error) {
  const message = error instanceof Error ? error.message : "";
  if (message.includes("already belongs")) return "token_hash_conflict";
  if (message.includes("pairing scope")) return "pairing_scope_required";
  if (message.includes("forbids prev") || message.includes("only supports a current")) return "pairing_prev_forbidden";
  if (message.includes("generation must be positive")) return "generation_must_be_positive";
  if (message.includes("token subject generation")) return "generation_invalid";
  if (message.includes("requires scope") || message.includes("device subject")) return "scope_invalid";
  if (message.includes("token subject")) return "subject_invalid";
  if (message.includes("token scope")) return "scope_invalid";
  if (message.includes("prev token hash required")) return "prev_token_hash_required";
  if (message.includes("token hash")) return "token_hash_invalid";
  if (message.includes("access_expires")) return "access_expires_after_refresh_until";
  if (message.includes("exceeds JSON safe integer")) return "timestamp_exceeds_json_safe_integer";
  if (message.includes("must be positive")) return "timestamp_must_be_positive";
  if (message.includes("expiry") || message.includes("timestamp") || message.includes("refresh_until")) return "timestamp_invalid";
  if (message.includes("close flag")) return "close_invalid";
  if (message.includes("current")) return "current_required";
  if (message.includes("prev")) return "prev_invalid";
  return null;
}

function bytesToHex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function hexToBytes(hex) {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

function concatBytes(left, right) {
  const joined = new Uint8Array(left.length + right.length);
  joined.set(left, 0);
  joined.set(right, left.length);
  return joined;
}
