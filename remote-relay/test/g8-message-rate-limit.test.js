import { createHash } from "node:crypto";
import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";

// G8 · relay:消息期限速（C1 设计 v0.5 §8 支线·codex 设计审 Critical-2）。
//
// 双路（codex+opus）二审对第一版实现判 fix_required，Lead 按裁决拍板重构了
// 桶拓扑，本文件驱动重构后的真实行为：
//   R1 per-subject 桶从 attachment 搬进 SQL（message_rate_limits 表）——
//      被盗已配对手机完全掌控自己何时重连，存 attachment 的桶断线重连即
//      清零，形同虚设；SQL 桶跨 socket/跨重连/跨休眠不丢。
//   R2 per-IP 粗桶挪到中央入站点（webSocketMessage，scope 矩阵通过之后），
//      对所有 role=remote 入站帧计费（不再局限 input/control），阈值
//      240→480，超限只拒不踢。
//   R3 per-subject 桶超限只在本窗口第一次记协议违例（不是每帧都记），踢
//      连接用独立 close reason "message_rate_limited"（不是
//      "token_reauthorization_failed"）。
//   R4 pending_input 入队前先清过期行、幂等先于容量判断。
//   R5 限速/排队拒绝帧都带 command_id。
//
// 照 s1d/s1e/s1i2 既有 fixture 风格自建 runtime/helpers，不依赖
// room-do.test.js 的 connect()（那个 helper 每次调用都现领一个独立
// subject，测不出「同一 subject 断线重连」「per-subject 共享配额」这些
// 场景）。

const ROOM = "d".repeat(32);
const CT = Buffer.from("g8 ciphertext").toString("base64");
const N12 = Buffer.alloc(12, 5).toString("base64");
// 与 room-do.js 内部常量同值——测试侧独立声明，不跨文件引用生产内部常量
// （room-do.js 不导出它们，也不该为了测试导出）。
const INPUT_RATE_LIMIT_FOR_TEST = 30;
const IP_MESSAGE_RATE_LIMIT_FOR_TEST = 480;
const PENDING_INPUT_BYTE_LIMIT_FOR_TEST = 4 * 1024 * 1024;
const PROTOCOL_VIOLATION_LIMIT_FOR_TEST = 8;
const IP_MESSAGE_BUCKET_MAX_ENTRIES_FOR_TEST = 4096;

function sha256(value) {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

function makeRuntime() {
  const db = new DatabaseSync(":memory:");
  const registry = [];
  const storage = {
    sql: {
      exec(query, ...params) {
        const stmt = db.prepare(query);
        if (/^\s*(SELECT|PRAGMA)/i.test(query)) return stmt.all(...params);
        stmt.run(...params);
        return [];
      },
    },
    transactionSync(callback) {
      db.exec("BEGIN IMMEDIATE");
      try {
        const result = callback();
        db.exec("COMMIT");
        return result;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
    async setAlarm() {},
  };
  const ctx = {
    storage,
    acceptWebSocket(ws, tags = []) {
      registry.push({ ws, tags });
    },
    getWebSockets(tag) {
      if (!tag) return registry.map((item) => item.ws);
      return registry.filter((item) => item.tags.includes(tag)).map((item) => item.ws);
    },
  };
  return { ctx };
}

// R6 ⑧（codex Q6）：deserializeAttachment 必须返回克隆，不能是同一个对象
// 引用——真实 Cloudflare Hibernation API 的 serializeAttachment/
// deserializeAttachment 是真的 JSON 序列化往返，不是"存一个指针"。如果这个
// mock 返回同一个对象引用，任何生产代码「读出 attachment、直接改字段、忘了
// 调 serializeAttachment 写回」的 bug 都会因为「反正读出来的和存的是同一个
// 对象」而侥幸测出绿色——本文件这一整套桶的正确性依赖 attachment 读写的
// 真实语义，必须堵死这个假绿的口子。
function fakeWs(attachment = null) {
  let stored = attachment == null ? attachment : structuredClone(attachment);
  return {
    sent: [],
    closed: [],
    readyState: 1, // WebSocket.OPEN
    send(text) {
      this.sent.push(typeof text === "string" ? JSON.parse(text) : text);
    },
    close(code, reason) {
      this.closed.push({ code, reason });
      this.readyState = 3; // WebSocket.CLOSED
    },
    serializeAttachment(value) {
      stored = value == null ? value : structuredClone(value);
    },
    deserializeAttachment() {
      return stored == null ? stored : structuredClone(stored);
    },
  };
}

function desktopSocket(room, ctx, overrides = {}) {
  const epoch = store.bumpEpoch(room.sql);
  const ws = fakeWs({
    role: "desktop", scope: "desktop", epoch, registry_ready: true,
    connectedAt: Date.now(), connection_id: "desktop-conn", ...overrides,
  });
  ctx.acceptWebSocket(ws, ["desktop"]);
  return ws;
}

// 种一个 scope=remote 的 registry-backed subject（kind=current），供
// remoteMessageSocket() 手搭的多条连接（并发的、或代表同一 subject 先后
// 重连的）共用同一份注册表行——必须只种一次（多条连接共用同一个 subject
// 时，各自 attachment 记的 access_expires/valid_until 必须与这一行的实际值
// 逐字段相等，isOfficialAttachmentLive 才会判它们同时是活连接）。
function seedRemoteSubject(room, { subject, generation = 1, now = Date.now(), validUntil = now + 3_600_000, accessExpires = now + 3_600_000 }) {
  return store.putTokenRegistryEntry(room.sql, {
    subject, generation, state: "active", scope: "remote",
    aliases: [{
      token_hash: sha256(subject),
      kind: "current",
      generation,
      access_expires: accessExpires,
      valid_until: validUntil,
    }],
  }, now);
}

// 手工搭一条已完成鉴权的 remote 连接——同 s1i2-fixture.test.js 的
// remoteRefreshSocket()/room-do.test.js 的 connect() 同款姿势：不走
// fetch()/WebSocketPair（enforceSubjectSocketLimit 只在 fetch() 握手路径
// 触发，这里刻意绕开它）。
function remoteMessageSocket(room, ctx, {
  subject, generation = 1, connectionId, ipBucketKey = "a".repeat(32),
  now = Date.now(), validUntil = now + 3_600_000, accessExpires = now + 3_600_000,
  epoch,
} = {}) {
  const ws = fakeWs({
    role: "remote", scope: "remote", subject, kind: "current",
    generation, alias_generation: generation,
    access_expires: accessExpires, valid_until: validUntil,
    connection_id: connectionId, epoch: epoch ?? store.getCurrentEpoch(room.sql),
    connectedAt: now, ip_bucket_key: ipBucketKey,
  });
  ctx.acceptWebSocket(ws, ["remote"]);
  return ws;
}

function inputEnvelope(overrides = {}) {
  return {
    v: 1, room: ROOM, epoch: 1, kind: "input", session: "s1",
    command_id: `cmd-in-${Math.random().toString(36).slice(2)}`,
    seq: null, client_msg_id: null, ct: CT, n: N12, ts: Date.now(),
    ...overrides,
  };
}

function controlEnvelope(overrides = {}) {
  return {
    v: 1, room: ROOM, epoch: 1, kind: "control", session: "s1",
    command_id: `cmd-ctrl-${Math.random().toString(36).slice(2)}`,
    seq: null, client_msg_id: null, ct: CT, n: N12, ts: Date.now(),
    ...overrides,
  };
}

function presenceFrame() {
  // 明文帧——handlePlainFrame 的 "presence" 分支不校验载荷内容，只广播；
  // 用它填 per-IP 粗桶最干净：presence 不挂任何 per-subject 限速，480 条
  // 全部命中的只有 IP 桶本身，不会被 per-subject 桶提前拦下来污染计数。
  return { t: "presence", role: "remote", event: "ping", ts: Date.now() };
}

async function send(room, ws, frameOrText) {
  const before = ws.sent.length;
  await room.webSocketMessage(ws, typeof frameOrText === "string" ? frameOrText : JSON.stringify(frameOrText));
  return ws.sent[before];
}

function rewindSubjectChannelWindow(room, subject, channel, now = Date.now()) {
  room.sql.exec(
    "UPDATE message_rate_limits SET window_started_at = ? WHERE subject = ? AND channel = ?",
    now - 61_000, subject, channel
  );
}

// G8 三审 fix_required（codex xhigh 差量审）R5③ 缺口测试用：kind=input 但
// ct/n 是垃圾——frameType（"input"）本身在 scope 矩阵里，能一路走到中央
// IP 挂点被计费，之后才在 validateEnvelope 那步失败回 invalid_envelope；
// 挂点覆盖了这条路径正是 R2「堵掉免费探测回环」要证明的事。
function garbageInputFrame(overrides = {}) {
  return {
    v: 1, room: ROOM, epoch: 0, kind: "input", session: "s1",
    command_id: "cmd-garbage", seq: null, client_msg_id: null,
    ct: "not-valid-base64!!!", n: "also-not-valid!!", ts: Date.now(),
    ...overrides,
  };
}

// 手工搭一条 scope=refresh 的连接——同 s1i2-fixture.test.js 的
// remoteRefreshSocket()/seedRefreshSubject() 同款姿势，独立复刻在本文件里
// （不跨文件 import 私有 test helper），只为 R5③「token.refresh 也吃 IP
// 桶」这一条测试服务。
function seedRefreshCapableSubject(room, { subject, generation = 1, now = Date.now(), prevTokenHash = "9".repeat(64) }) {
  return store.putTokenRegistryEntry(room.sql, {
    subject, generation, state: "active", scope: "remote",
    aliases: [{ token_hash: prevTokenHash, kind: "prev", generation, access_expires: null, valid_until: now + 3_600_000 }],
  }, now);
}

function remoteRefreshScopeSocket(room, ctx, {
  subject, generation = 1, connectionId, ipBucketKey = "a".repeat(32), now = Date.now(),
} = {}) {
  const ws = fakeWs({
    role: "remote", scope: "refresh", subject, kind: "prev",
    generation, alias_generation: generation, access_expires: null,
    valid_until: now + 3_600_000, connection_id: connectionId, epoch: 0,
    ip_bucket_key: ipBucketKey,
  });
  ctx.acceptWebSocket(ws, ["remote"]);
  return ws;
}

// ---- G8 三审 fix_required（codex xhigh 差量审）R4：真实 fetch() 升级 ----
// 不手搭 attachment，走 parseRemoteSubprotocol 的真实 token 校验路径——证明
// "生产握手重建的 attachment 带同一 subject"这件事本身，而不是靠测试自己
// 在两条连接上都手写同一个 subject 字符串"证明"了什么都没证明的东西。
// 照 s1d-fixture.test.js 的 withUpgradeRuntime 同款姿势，本文件独立复刻
// （不跨文件 import 私有 test helper）。
async function withUpgradeRuntime(callback) {
  const NativeResponse = globalThis.Response;
  const NativeWebSocketPair = globalThis.WebSocketPair;
  globalThis.Response = class TestResponse {
    constructor(body, init = {}) {
      this.body = body;
      this.status = init.status ?? 200;
      this.webSocket = init.webSocket ?? null;
      this.headers = new Headers(init.headers || {});
    }
  };
  globalThis.WebSocketPair = class TestWebSocketPair {
    constructor() {
      this[0] = fakeWs();
      this[1] = fakeWs();
    }
  };
  try {
    return await callback();
  } finally {
    globalThis.Response = NativeResponse;
    if (NativeWebSocketPair === undefined) delete globalThis.WebSocketPair;
    else globalThis.WebSocketPair = NativeWebSocketPair;
  }
}

function seedRealSubject(room, { subject, token, now = Date.now(), generation = 1 }) {
  return store.putTokenRegistryEntry(room.sql, {
    subject, generation, state: "active", scope: "remote",
    aliases: [{
      token_hash: sha256(token),
      kind: "current",
      generation,
      access_expires: now + 3_600_000,
      valid_until: now + 3_600_000,
    }],
  }, now);
}

async function realRemoteUpgrade(room, ctx, { token, ip = "203.0.113.50" } = {}) {
  const response = await room.fetch(new Request(`https://relay.example/room/${ROOM}`, {
    headers: {
      Upgrade: "websocket",
      "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${token}`,
      "CF-Connecting-IP": ip,
    },
  }));
  assert.equal(response.status, 101, "真实 upgrade 必须成功——前置条件没搭对");
  return ctx.getWebSockets("remote").at(-1);
}

// ---- 1a：per-subject input 桶——29 帧连发全过，第 31 帧被拒 ----

test("G8 1a：per-subject input 桶——29 帧连发全过（未触限）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  seedRemoteSubject(room, { subject, now });
  const desktop = desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-29", now });

  for (let i = 1; i <= 29; i += 1) {
    const actual = await send(room, remote, inputEnvelope());
    assert.equal(actual, undefined, `第 ${i} 帧应正常转发，不应有直接回执`);
  }
  assert.equal(desktop.sent.length, 29);
  assert.equal(remote.closed.length, 0);
});

test("G8 1a：per-subject input 桶——第 31 帧被拒（error 帧 reason/command_id 正确·计入协议违例）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
  seedRemoteSubject(room, { subject, now });
  desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-31", now });

  for (let i = 1; i <= 30; i += 1) {
    await send(room, remote, inputEnvelope());
  }
  assert.equal(remote.closed.length, 0, "30 帧以内不该关连接（只攒了 1 次违例，远未到 8 次阈值）");

  const rejected = await send(room, remote, inputEnvelope({ command_id: "cmd-31st" }));
  assert.ok(rejected, "第 31 帧必须回一帧拒绝");
  assert.equal(rejected.t, "error");
  assert.equal(rejected.reason, "input_rate_limited");
  assert.equal(rejected.frame, "input");
  assert.equal(rejected.command_id, "cmd-31st", "G8 R5：限速拒绝帧必须带 command_id");
  assert.equal(remote.closed.length, 0, "第 1 次违例不该立刻踢连接——复用既有 8 次阈值机制");

  room.flushProtocolViolations();
  assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "1");
});

test("G8 1a：per-subject input 桶——第 61 秒窗口翻转后恢复放行（SQL 持久窗口，不是 attachment）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:cccccccc-cccc-4ccc-8ccc-cccccccccccc";
  seedRemoteSubject(room, { subject, now });
  const desktop = desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-window", now });

  for (let i = 1; i <= INPUT_RATE_LIMIT_FOR_TEST; i += 1) {
    await send(room, remote, inputEnvelope());
  }
  // 直接把 SQL 里的窗口起点拨到 61 秒前——不真的等 60 秒；这条状态现在活在
  // message_rate_limits 表里，不是 attachment，验证方式必须换成操作这张表。
  rewindSubjectChannelWindow(room, subject, "input");

  const actual = await send(room, remote, inputEnvelope());
  assert.equal(actual, undefined, "窗口已过期，应重置计数并正常转发");
  assert.equal(desktop.sent.length, INPUT_RATE_LIMIT_FOR_TEST + 1);
  assert.equal(remote.closed.length, 0);
});

// ---- 1b：control 独立 per-subject 桶——与 input 互不串味 ----

test("G8 1b：per-subject control 桶——独立计数，30 帧内全过，第 31 帧被拒", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:dddddddd-dddd-4ddd-8ddd-dddddddddddd";
  seedRemoteSubject(room, { subject, now });
  const desktop = desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-ctrl-31", now });

  for (let i = 1; i <= 30; i += 1) {
    const actual = await send(room, remote, controlEnvelope());
    assert.equal(actual, undefined, `第 ${i} 帧 control 应正常转发`);
  }
  assert.equal(desktop.sent.length, 30);

  const rejected = await send(room, remote, controlEnvelope({ command_id: "cmd-ctrl-31st" }));
  assert.equal(rejected.reason, "control_rate_limited");
  assert.equal(rejected.frame, "control");
  assert.equal(rejected.command_id, "cmd-ctrl-31st");
});

test("G8 1b：input 桶打满不影响 control 桶——同一 subject 上两条通道互不串味", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
  seedRemoteSubject(room, { subject, now });
  const desktop = desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-crosstalk", now });

  for (let i = 1; i <= 30; i += 1) {
    await send(room, remote, inputEnvelope());
  }
  const inputRejected = await send(room, remote, inputEnvelope());
  assert.equal(inputRejected.reason, "input_rate_limited", "前置：input 桶已打满");

  const controlOk = await send(room, remote, controlEnvelope());
  assert.equal(controlOk, undefined, "control 桶不受 input 桶打满影响，应正常转发");
  assert.equal(desktop.sent.filter((frame) => frame.kind === "control").length, 1);
});

// ---- R6①：重连不清零——per-subject 桶持久跨连接 ----

test("G8 R6①：同 subject 断开重连后继续发，第 31 条（累计）仍被拒", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:11111111-aaaa-4111-8111-111111111111";
  seedRemoteSubject(room, { subject, now });
  desktopSocket(room, ctx);

  const before = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-before-reconnect", now });
  for (let i = 1; i <= 20; i += 1) {
    const actual = await send(room, before, inputEnvelope());
    assert.equal(actual, undefined, `重连前第 ${i} 帧应成功`);
  }

  // 模拟断线重连：新开一条连接，同一个 subject，不同 connection_id——旧版
  // attachment 桶在这里会清零（新 socket 的 attachment 上没有任何计数字段），
  // 新版 SQL 桶不会，因为配额记在 message_rate_limits 表里，认 subject 不认
  // 具体哪条 socket。
  const after = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-after-reconnect", now });
  for (let i = 21; i <= 30; i += 1) {
    const actual = await send(room, after, inputEnvelope());
    assert.equal(actual, undefined, `重连后累计第 ${i} 帧应仍在 30 帧配额内成功`);
  }

  const rejected = await send(room, after, inputEnvelope({ command_id: "cmd-reconnect-31st" }));
  assert.ok(rejected, "重连后累计第 31 条必须被拒——证明配额没有因为断线重连被清零");
  assert.equal(rejected.reason, "input_rate_limited");
  assert.equal(rejected.command_id, "cmd-reconnect-31st");
});

// ---- 2：per-IP 粗桶——中央入站点、480 阈值、只拒不踢 ----

test("G8 R6④：同 IP 打满 480 后，另一 subject 的 socket 连发 8 条只收错误帧，不被踢", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subjectA = "device:22222222-2222-4222-8222-222222222222";
  const subjectB = "device:33333333-3333-4333-8333-333333333333";
  seedRemoteSubject(room, { subject: subjectA, now });
  seedRemoteSubject(room, { subject: subjectB, now });
  const sharedIp = "f".repeat(32);
  const fillerSocket = remoteMessageSocket(room, ctx, { subject: subjectA, connectionId: "conn-ip-filler", ipBucketKey: sharedIp, now });

  // presence 不挂任何 per-subject 限速，480 条全部命中的只有 IP 桶本身。
  for (let i = 0; i < IP_MESSAGE_RATE_LIMIT_FOR_TEST; i += 1) {
    const actual = await send(room, fillerSocket, presenceFrame());
    assert.equal(actual, undefined, `第 ${i + 1} 条 presence 应成功广播`);
  }
  assert.equal(fillerSocket.closed.length, 0);

  // 另一个 subject、同一个 IP 的全新 socket——自己的 per-subject 桶完全没用过。
  const victimSocket = remoteMessageSocket(room, ctx, { subject: subjectB, connectionId: "conn-ip-victim", ipBucketKey: sharedIp, now });
  for (let i = 1; i <= 8; i += 1) {
    const actual = await send(room, victimSocket, inputEnvelope({ command_id: `cmd-victim-${i}` }));
    assert.ok(actual, `第 ${i} 条必须回一帧拒绝——IP 桶已打满`);
    assert.equal(actual.reason, "ip_message_rate_limited");
    assert.equal(actual.frame, "input");
    assert.equal(actual.command_id, `cmd-victim-${i}`);
  }
  // A shared IP limit rejects all eight frames from another device on that IP.
  // Repeated rejections must leave that device connected.
  assert.equal(victimSocket.closed.length, 0, "per-IP 粗桶超限绝不踢连接");

  // G8 三审 fix_required R5②：审计计数从「>0」改成精确值——victimSocket 连发
  // 8 条全被 IP 桶拒绝，每条都调一次 recordProtocolViolation(null)，除此
  // 之外这个房间里没有任何其它路径会记协议违例（填桶的 480 条 presence 全部
  // 在预算内成功，压根不会触发），精确计数才是这条闸「只审计不多不少」的
  // 真实证明——`>0` 弱到连"审计计数是不是偷偷翻了倍/漏了几次"都测不出来。
  room.flushProtocolViolations();
  assert.equal(store.getMeta(room.sql, "protocol_violation_count", "0"), "8");
});

test("G8 R6⑤：不同 IP 独立计数——先把 A 打满 480，再验 B 通过（B 的 presence 真送达接收方）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subjectA = "device:44444444-4444-4444-8444-444444444444";
  const subjectB = "device:55555555-5555-4555-8555-555555555555";
  seedRemoteSubject(room, { subject: subjectA, now });
  seedRemoteSubject(room, { subject: subjectB, now });
  const remoteA = remoteMessageSocket(room, ctx, { subject: subjectA, connectionId: "conn-ip-a", ipBucketKey: "1".repeat(32), now });
  const remoteB = remoteMessageSocket(room, ctx, { subject: subjectB, connectionId: "conn-ip-b", ipBucketKey: "2".repeat(32), now });
  // G8 三审 fix_required R5①：只验 B 自己没收到错误帧证明不了 presence 真的
  // 送到了谁手里——万一某天 broadcastRaw 的转发这一半悄悄坏掉，B 本身仍然
  // "没收到拒绝"，这条测试还是会绿。加一个独立的接收方（桌面），断言它
  // 真收到了 B 发的那条 presence 内容，才是"B 的桶没被 A 连累"的正向证明。
  const observer = desktopSocket(room, ctx);

  for (let i = 0; i < IP_MESSAGE_RATE_LIMIT_FOR_TEST; i += 1) {
    await send(room, remoteA, presenceFrame());
  }
  const overflowOnA = await send(room, remoteA, presenceFrame());
  assert.equal(overflowOnA.reason, "ip_message_rate_limited", "前置：A 自己的 IP 桶确实已打满");

  const before = observer.sent.length;
  const actualOnB = await send(room, remoteB, presenceFrame());
  assert.equal(actualOnB, undefined, "另一个 IP 的桶应完全不受 A 打满的影响");
  const delivered = observer.sent.slice(before).filter((frame) => frame.t === "presence");
  assert.equal(delivered.length, 1, "B 发的 presence 必须真的送达到了接收方，不只是 B 自己没收到拒绝");
});

// ---- 3：pending_input 上限——行数 256 / 字节 4MB ----

test("G8 3：pending_input 行数——255 行时再入 1 行成功", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:66666666-6666-4666-8666-666666666666";
  seedRemoteSubject(room, { subject, now });
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-row-255", now });
  // 桌面离线：不建 desktopSocket。

  for (let i = 0; i < 255; i += 1) {
    store.enqueueInput(room.sql, {
      commandId: `seed-row-${i}`, session: "s1", envelopeJson: JSON.stringify({ seed: i }), now,
    });
  }
  assert.equal(store.pendingInputStats(room.sql).rowCount, 255);

  // 桌面离线、从未 bumpEpoch 过，房间当前 epoch 恒为 0——envelope 必须显式
  // 对齐，否则会先撞 stale_epoch 短路，测不到本测试真正要测的行数闸。
  const actual = await send(room, remote, inputEnvelope({ command_id: "cmd-row-256", epoch: 0 }));
  // C1-RQ（dogfood 修障第二批）：离线入队成功后现在回一条 input.relay_queued
  // 确认帧（不再一声不吭）——不是拒绝回执，只是不再是 undefined。
  assert.equal(actual.t, "input.relay_queued", "255 行时第 256 行应成功入队并回 relay_queued，不该有拒绝回执");
  assert.equal(actual.command_id, "cmd-row-256");
  assert.equal(store.pendingInputStats(room.sql).rowCount, 256);
});

test("G8 3：pending_input 行数——满 256 行后 queue_full（带 command_id），行数不再增长", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:77777777-7777-4777-8777-777777777777";
  seedRemoteSubject(room, { subject, now });
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-row-256full", now });

  for (let i = 0; i < 256; i += 1) {
    store.enqueueInput(room.sql, {
      commandId: `seed-full-${i}`, session: "s1", envelopeJson: JSON.stringify({ seed: i }), now,
    });
  }
  assert.equal(store.pendingInputStats(room.sql).rowCount, 256);

  const actual = await send(room, remote, inputEnvelope({ command_id: "cmd-row-overflow", epoch: 0 }));
  assert.ok(actual, "满 256 行后必须回一帧拒绝");
  assert.equal(actual.t, "error");
  assert.equal(actual.reason, "queue_full");
  assert.equal(actual.frame, "input");
  assert.equal(actual.command_id, "cmd-row-overflow", "G8 R5：queue_full 必须带 command_id");
  assert.equal(store.pendingInputStats(room.sql).rowCount, 256, "行数不该再增长");
});

test("G8 3：pending_input 总字节——逼近 4MB 后 queue_full，行数极小（隔离字节闸与行数闸）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:88888888-8888-4888-8888-888888888888";
  seedRemoteSubject(room, { subject, now });
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-bytes", now });

  // 精确算：种一行 envelope，把总字节顶到「离 4MB 上限只剩 200 字节」——
  // 留的缺口远小于任何真实 input envelope 的编码大小，保证接下来那条真实
  // 入队请求必然把总数推过上限。
  const HEADROOM_BYTES = 200;
  const seedPrefix = JSON.stringify({ seed: 0, payload: "" });
  const seedPrefixBytes = new TextEncoder().encode(seedPrefix).length;
  const padLength = PENDING_INPUT_BYTE_LIMIT_FOR_TEST - HEADROOM_BYTES - seedPrefixBytes;
  const envelopeJson = JSON.stringify({ seed: 0, payload: "x".repeat(padLength) });
  store.enqueueInput(room.sql, { commandId: "seed-bytes-0", session: "s1", envelopeJson, now });

  const stats = store.pendingInputStats(room.sql);
  assert.equal(stats.rowCount, 1, "行数远低于 256——本测试要单独证明字节闸，不是行数闸在起作用");
  assert.ok(
    stats.totalBytes > PENDING_INPUT_BYTE_LIMIT_FOR_TEST - HEADROOM_BYTES - 10 &&
      stats.totalBytes < PENDING_INPUT_BYTE_LIMIT_FOR_TEST,
    `种下的总字节应精确逼近 4MB 上限但未超（实际 ${stats.totalBytes}）`
  );

  const actual = await send(room, remote, inputEnvelope({ command_id: "cmd-bytes-overflow", epoch: 0 }));
  assert.ok(actual, "总字节将超 4MB 时必须回一帧拒绝");
  assert.equal(actual.reason, "queue_full");
  assert.equal(actual.command_id, "cmd-bytes-overflow");
  assert.equal(store.pendingInputStats(room.sql).rowCount, 1, "行数不该增长");
});

// ---- R6②：容量判断前先清过期行 ----

test("G8 R6②：种 256 条已过期行——新 input 成功入队且收到对应 input.expired 广播", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:99999999-9999-4999-8999-999999999999";
  seedRemoteSubject(room, { subject, now });
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-purge-expired", now });

  const longAgo = now - 1_000_000;
  for (let i = 0; i < 256; i += 1) {
    store.enqueueInput(room.sql, {
      commandId: `seed-expired-${i}`, session: "s1", envelopeJson: JSON.stringify({ seed: i }),
      now: longAgo, ttlMs: 1, // expires_at = longAgo + 1，早就过期
    });
  }
  assert.equal(store.pendingInputStats(room.sql).rowCount, 256, "前置：队列已满，且全是过期行");

  // 不用通用 send() 助手——这条路径会先广播 256 条 input.expired 给同一个
  // remote 连接，再决定要不要给"这条新 input 本身"回一帧；send() 只捞
  // sent[before] 那一条（会捞到第一条 input.expired，不是"对这条帧的直接
  // 回执"），必须直接扫整个 sent 数组找有没有 queue_full。
  await room.webSocketMessage(remote, JSON.stringify(inputEnvelope({ command_id: "cmd-after-purge", epoch: 0 })));
  const queueFullFrames = remote.sent.filter((frame) => frame.reason === "queue_full");
  assert.equal(queueFullFrames.length, 0, "过期行被清空腾出容量后，新 input 应成功入队，不该收到 queue_full");

  const stats = store.pendingInputStats(room.sql);
  assert.equal(stats.rowCount, 1, "256 条过期行应被全部清空，只剩这条新入队的");

  const expiredFrames = remote.sent.filter((frame) => frame.t === "input.expired");
  assert.equal(expiredFrames.length, 256, "256 条过期行都应广播 input.expired（照既有 flushPendingInputTo 语义）");
});

// ---- R6③：幂等先于容量——满队列同 command_id 重试不收假 queue_full ----

test("G8 R6③：满队列同 command_id 重试——不收 queue_full（幂等静默，不重插）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:12121212-1212-4121-8121-121212121212";
  seedRemoteSubject(room, { subject, now });
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-idempotent-retry", now });

  for (let i = 0; i < 255; i += 1) {
    store.enqueueInput(room.sql, {
      commandId: `seed-idem-${i}`, session: "s1", envelopeJson: JSON.stringify({ seed: i }), now,
    });
  }
  store.enqueueInput(room.sql, {
    commandId: "cmd-idempotent-retry", session: "s1", envelopeJson: JSON.stringify({ original: true }), now,
  });
  assert.equal(store.pendingInputStats(room.sql).rowCount, 256, "前置：队列已满 256 行，其中一行是待重试的那条");

  const actual = await send(room, remote, inputEnvelope({ command_id: "cmd-idempotent-retry", epoch: 0 }));
  // C1-RQ（dogfood 修障第二批）：幂等命中现在也回一条 input.relay_queued
  // （拿既存行的 expires_at）——不是 queue_full，也不再是一声不吭。
  assert.equal(actual.t, "input.relay_queued", "同 command_id 重试应回 relay_queued，不该收到 queue_full");
  assert.equal(actual.command_id, "cmd-idempotent-retry");
  assert.equal(store.pendingInputStats(room.sql).rowCount, 256, "幂等重试不该让行数变化");
});

// ---- R6⑦：手快场景——单窗口内多次超限只吃 1 次协议违例 ----

test("G8 R6⑦：单窗口 60 条（31-60 全拒）只吃 1 次协议违例，不断连", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:13131313-1313-4131-8131-131313131313";
  seedRemoteSubject(room, { subject, now });
  desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-fast-hands", now });

  for (let i = 1; i <= 30; i += 1) {
    const actual = await send(room, remote, inputEnvelope());
    assert.equal(actual, undefined, `前 30 帧第 ${i} 帧应成功`);
  }
  for (let i = 31; i <= 60; i += 1) {
    const actual = await send(room, remote, inputEnvelope());
    assert.equal(actual.reason, "input_rate_limited", `第 ${i} 帧应被拒`);
  }

  room.flushProtocolViolations();
  assert.equal(
    store.getMeta(room.sql, "protocol_violation_count", "0"), "1",
    "31-60 共 30 条被拒的帧，本窗口应只记 1 次协议违例（R3：按窗口去重，不按帧数线性累加）"
  );
  assert.equal(remote.closed.length, 0, "1 次违例远未到 8 次踢连接阈值，不该断连");
});

// ---- 4：违例攒够阈值触发既有踢连接机制（跨窗口累积，R3 close reason 独立）----

test("G8 4：per-subject 限速违例跨 8 个窗口攒够阈值后，用独立 close reason 踢连接", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:14141414-1414-4141-8141-141414141414";
  seedRemoteSubject(room, { subject, now });
  desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-8-windows", now });

  // R3 把「持续超速」而非「超速帧数」映射到违例——单个窗口内不管发多少条
  // 被拒的帧都只算 1 次。要真的攒够 8 次踢连接阈值，必须跨 8 个不同的窗口
  // 各触发一次「本窗口第一次超限」。
  for (let windowIndex = 1; windowIndex <= PROTOCOL_VIOLATION_LIMIT_FOR_TEST; windowIndex += 1) {
    for (let i = 0; i < 30; i += 1) {
      await send(room, remote, inputEnvelope());
    }
    const rejected = await send(room, remote, inputEnvelope({ command_id: `cmd-window-${windowIndex}` }));
    assert.equal(rejected.reason, "input_rate_limited");

    if (windowIndex < PROTOCOL_VIOLATION_LIMIT_FOR_TEST) {
      assert.equal(remote.closed.length, 0, `窗口 ${windowIndex} 不该踢连接（未到 8 次阈值）`);
      rewindSubjectChannelWindow(room, subject, "input");
    }
  }

  assert.equal(remote.closed.length, 1, "第 8 个窗口的违例应触发既有踢连接阈值");
  assert.equal(
    remote.closed[0].reason, "message_rate_limited",
    "G8 R3：close reason 必须是新的 message_rate_limited，不能复用 token_reauthorization_failed（否则手机端会误判成凭据坏了去重配对）"
  );
});

// ---- 5：桌面角色帧不受这些桶影响（R6⑥ 补正向断言）----

test("G8 5：桌面发 kind=live 不受 input/control/IP 桶影响，remote 端实收 300 条", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:15151515-1515-4151-8151-151515151515";
  seedRemoteSubject(room, { subject, now });
  const desktop = desktopSocket(room, ctx);
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-live-guard", now });

  for (let i = 0; i < 300; i += 1) {
    await send(room, desktop, {
      v: 1, room: ROOM, epoch: 1, kind: "live", session: "s1",
      command_id: null, seq: null, client_msg_id: null, ct: CT, n: N12, ts: Date.now(),
    });
  }
  assert.equal(remote.closed.length, 0);
  assert.equal(desktop.closed.length, 0, "300 条 live 帧远超 input/control/IP 的桶阈值，但桌面角色帧不挂这些桶，不该被限速/踢连接");
  // R6⑥：不能只验"没被踢"，要正向证明这 300 条确实都送到了——否则一个把
  // live 转发悄悄短路掉的回归也会被这条测试放过。
  assert.equal(remote.sent.filter((frame) => frame.kind === "live").length, 300, "remote 端应实收全部 300 条 live 帧");
});

// ============================================================================
// G8 三审 fix_required（codex xhigh 差量审）：语义修对了，但三处实现会反噬
// 自己 + 一条测试不是真路径。R1 IP Map 真硬上限 / R2 per-subject 桶饱和写 /
// R3 purge 改按索引定向删（无独立测试，靠既有测试 + 全量门禁验证行为等价）
// / R4 R6① 换真路径 / R5 弱断言加强（已并入上面 R6④/⑤）+ 两条缺口测试。
// ============================================================================

// ---- 三审 R1：IP Map 真硬上限 ----

// 4097 个不同 key 的压测专用：不走 ctx.acceptWebSocket 登记（这条测试只关心
// 每个 filler socket 自己收到的直接回执，不需要被别的 socket 观察到）+ 用
// 垃圾 input 帧而不是 presence——presence 会走 broadcastRaw 扫一遍
// ctx.getWebSockets() 全量在线连接表，登记 4096+ 条后这一步会变成 O(n) 每帧、
// 累计 O(n²)（首次跑这条测试实测卡到 110 秒，就是这个原因，不是被测代码本身
// 慢）；垃圾帧在 validateEnvelope 那步直接被拒、只回发送方一帧，不碰
// ctx.getWebSockets()，压测几千次也是常数级开销。
// G8 三审 R1 压测用：subject 必须是真实、活跃的注册表 subject——
// authorizeInboundSocket 对 attachment.subject 缺失/非活跃一律 fail-closed
// （S1ja F2 起），空 subject 的连接连中央 IP 挂点都到不了，会假绿。所有
// filler socket 共享同一个已种好的 subject 也没关系：垃圾帧走的是
// validateEnvelope 那条路径，压根不会摸到 per-subject 桶。
function unregisteredIpProbeSocket({ subject, ipBucketKey, now = Date.now() }) {
  return fakeWs({
    role: "remote", scope: "remote", subject, kind: "current",
    generation: 1, alias_generation: 1,
    access_expires: now + 3_600_000, valid_until: now + 3_600_000,
    connection_id: `unregistered-${ipBucketKey}`, epoch: 0, connectedAt: now,
    ip_bucket_key: ipBucketKey,
  });
}

test("G8 三审 R1：IP Map 真硬上限——4097 个不同 key 同窗口内，size 恒 ≤4096，第 4097 个被拒，既有活跃桶不受影响", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:17171717-1717-4171-8171-171717171717";
  seedRemoteSubject(room, { subject, now });

  // 先用一把固定 key 占一个"活跃桶"名额，等会儿要证明它在 Map 满员之后
  // 仍然正常计数，不会被为了腾位置而牺牲掉。
  const probe = unregisteredIpProbeSocket({ subject, ipBucketKey: "probe-key", now });
  const probeFirst = await send(room, probe, garbageInputFrame());
  assert.equal(probeFirst.reason, "invalid_envelope", "前置：垃圾帧本身应正常走到中央 IP 挂点之后、在 envelope 校验被拒");

  for (let i = 1; i <= 4095; i += 1) {
    const ws = unregisteredIpProbeSocket({ subject, ipBucketKey: `filler-key-${i}`, now });
    const actual = await send(room, ws, garbageInputFrame());
    assert.equal(actual.reason, "invalid_envelope", `第 ${i} 把新 filler key 应该正常放行到 envelope 校验（没被 IP 桶拦）`);
  }
  // probe（1 把）+ 4095 把 filler = 4096，Map 应恰好顶到硬上限。
  assert.equal(room.ipMessageBuckets.size, IP_MESSAGE_BUCKET_MAX_ENTRIES_FOR_TEST, "灌够 4096 把不同 key 后，Map 应恰好顶到上限");

  // 第 4097 把全新 key（同一窗口内，谁都没过期）——必须被 fail-closed 拒绝，
  // 不能为了腾位置去踢掉任何一条已经在计数的活跃桶。
  const overflowWs = unregisteredIpProbeSocket({ subject, ipBucketKey: "overflow-key", now });
  const overflowActual = await send(room, overflowWs, garbageInputFrame());
  assert.ok(overflowActual, "第 4097 个新 key 必须回一帧拒绝");
  assert.equal(overflowActual.reason, "ip_message_rate_limited", "必须在中央 IP 挂点被拒，根本走不到 envelope 校验那步");
  assert.equal(room.ipMessageBuckets.size, IP_MESSAGE_BUCKET_MAX_ENTRIES_FOR_TEST, "拒绝新 key 不该让 Map 继续涨——硬上限，不是软提示");

  // 既有活跃桶（那把 probe key）不受影响——继续正常放行，没有因为 Map
  // 满员就被牵连成"查不到/被清空/被顶掉"。
  const probeSecond = await send(room, probe, garbageInputFrame());
  assert.equal(probeSecond.reason, "invalid_envelope", "已在 Map 里的活跃 key 应继续正常放行到 envelope 校验，不受满员影响");
  assert.equal(room.ipMessageBuckets.size, IP_MESSAGE_BUCKET_MAX_ENTRIES_FOR_TEST, "已有 key 的后续调用不该改变 Map 尺寸");
});

// ---- 三审 R2：per-subject 桶饱和写 ----

test("G8 三审 R2：per-subject 桶超限后饱和写——连续 100 帧超限，SQL 里 attempts 恒为 31，违例仍只记 1 次，翻窗后恢复 1", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:18181818-1818-4181-8181-181818181818";
  seedRemoteSubject(room, { subject, now });
  desktopSocket(room, ctx); // epoch -> 1，缺了这行会全程撞 stale_epoch，测不到真正想测的饱和写行为
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-saturate", now });

  for (let i = 1; i <= 30; i += 1) {
    await send(room, remote, inputEnvelope());
  }
  for (let i = 1; i <= 100; i += 1) {
    const actual = await send(room, remote, inputEnvelope());
    assert.equal(actual.reason, "input_rate_limited", `第 ${i} 次超限应被拒`);
    const row = room.sql.exec(
      "SELECT attempts FROM message_rate_limits WHERE subject = ? AND channel = 'input'", subject
    )[0];
    assert.equal(Number(row.attempts), 31, `第 ${i} 次超限后 attempts 必须饱和钉在 31（limit+1），不能继续往上涨`);
  }

  room.flushProtocolViolations();
  assert.equal(
    store.getMeta(room.sql, "protocol_violation_count", "0"), "1",
    "100 次超限只应记 1 次协议违例（沿用 G8 R3：按窗口去重）"
  );

  // 翻窗后恢复：attempts 重置为 1，不是继续从饱和值往上涨。
  rewindSubjectChannelWindow(room, subject, "input");
  const afterWindow = await send(room, remote, inputEnvelope());
  assert.equal(afterWindow, undefined, "窗口翻转后应恢复放行");
  const rowAfter = room.sql.exec(
    "SELECT attempts FROM message_rate_limits WHERE subject = ? AND channel = 'input'", subject
  )[0];
  assert.equal(Number(rowAfter.attempts), 1, "翻窗后 attempts 应重置为 1");
});

// ---- 三审 R4：真实 fetch() 双次升级 + DO 重建，配额跨真实重连/isolate 重建持久 ----

test("G8 三审 R4：真实 fetch() 双次升级（close 第一条）+ 同 storage 重建 RoomDO，per-subject 配额全程持久", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx } = makeRuntime();
    const room = new RoomDO(ctx, {});
    store.ensureBusinessSchema(room.sql);
    const now = Date.now();
    const subject = "device:16161616-1616-4161-8161-161616161616";
    const token = "7".repeat(64);
    seedRealSubject(room, { subject, token, now });

    const ws1 = await realRemoteUpgrade(room, ctx, { token });
    for (let i = 1; i <= 20; i += 1) {
      const actual = await send(room, ws1, inputEnvelope({ epoch: 0 }));
      // C1-RQ（dogfood 修障第二批）：桌面全程未连接——离线入队成功后现在
      // 回一条 input.relay_queued（不再是一声不吭的 undefined）。
      assert.equal(actual.t, "input.relay_queued", `重连前第 ${i} 帧应成功入队并回 relay_queued（真实 fetch() 升级出的连接）`);
    }

    // 真实关闭第一条连接——走生产 close 入口，不是随手扔掉不管。
    await room.webSocketClose(ws1);

    // 真实第二次 upgrade：同一个 token/subject，全新 WebSocketPair、全新
    // connection_id、全新 attachment——这条连接除了 subject 相同之外，跟
    // 第一条没有任何共享的 JS 状态，attachment 是生产 fetch() 逻辑真实
    // 现算出来的，不是测试手写进去的。
    const ws2 = await realRemoteUpgrade(room, ctx, { token });
    assert.notEqual(ws2, ws1, "第二次升级必须是全新的 socket 对象，不是复用第一条");

    for (let i = 21; i <= 30; i += 1) {
      const actual = await send(room, ws2, inputEnvelope({ epoch: 0 }));
      assert.equal(actual.t, "input.relay_queued", `重连后累计第 ${i} 帧应仍在 30 帧配额内成功并回 relay_queued`);
    }
    const rejected = await send(room, ws2, inputEnvelope({ epoch: 0, command_id: "cmd-real-31st" }));
    assert.ok(rejected, "重连后累计第 31 条必须被拒——per-subject 配额没有因为断线重连被清零");
    assert.equal(rejected.reason, "input_rate_limited");
    assert.equal(rejected.command_id, "cmd-real-31st");

    // 再加一层：同一份 storage（ctx 共享同一套 sql/registry）上重建第二个
    // RoomDO 实例——模拟 isolate 被淘汰后重新唤醒，JS 层状态（含
    // ipMessageBuckets/socketProtocolViolations 这些内存字段）全部归零，
    // 但 message_rate_limits 这张表活在 SQL 里，理应原样还在。照
    // room-do.test.js「休眠唤醒安全」那条既有先例的手法：直接 new 第二个
    // RoomDO(ctx, {})，不带上一个实例任何 JS 层字段；ws2 这条物理连接在
    // 真实 Hibernation API 下本就不会因为 DO 的 JS 对象被重建就消失，这里
    // 复用同一个 ws2 对象驱动新实例的 webSocketMessage，就是这层语义。
    const roomAfterHibernation = new RoomDO(ctx, {});
    store.ensureBusinessSchema(roomAfterHibernation.sql);
    const stillRejected = await send(roomAfterHibernation, ws2, inputEnvelope({ epoch: 0, command_id: "cmd-after-hibernation" }));
    assert.ok(stillRejected, "DO 实例重建后，配额仍必须延续——证明状态活在 SQL 而不是任何 JS 对象里");
    assert.equal(stillRejected.reason, "input_rate_limited");
  });
});

// ---- 三审 R5③：两条中央挂点缺口测试 ----

test("G8 三审 R5③：无效信封垃圾帧也吃 IP 桶（堵住免费探测回环）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:19191919-1919-4191-8191-191919191919";
  seedRemoteSubject(room, { subject, now });
  const remote = remoteMessageSocket(room, ctx, { subject, connectionId: "conn-garbage-ip", now });

  for (let i = 0; i < IP_MESSAGE_RATE_LIMIT_FOR_TEST; i += 1) {
    const actual = await send(room, remote, garbageInputFrame({ command_id: `cmd-garbage-${i}` }));
    assert.equal(actual.reason, "invalid_envelope", `第 ${i + 1} 条垃圾帧应正常走到 envelope 校验、回 invalid_envelope`);
  }
  const overflow = await send(room, remote, garbageInputFrame({ command_id: "cmd-garbage-overflow" }));
  assert.equal(
    overflow.reason, "ip_message_rate_limited",
    "第 481 条即便还是垃圾帧，也该先被 IP 桶拦下——证明垃圾帧确实消耗了 IP 预算，不是能无限重试的免费探测回环"
  );
});

test("G8 三审 R5③：token.refresh 也吃 IP 桶（不是只有 input/control/presence 才受中央挂点约束）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const fillerSubject = "device:20202020-2020-4202-8202-202020202020";
  const refreshSubject = "device:21212121-2121-4212-8212-212121212121";
  seedRemoteSubject(room, { subject: fillerSubject, now });
  seedRefreshCapableSubject(room, { subject: refreshSubject, now });
  const sharedIp = "e".repeat(32);
  desktopSocket(room, ctx);
  const filler = remoteMessageSocket(room, ctx, { subject: fillerSubject, connectionId: "conn-refresh-ip-filler", ipBucketKey: sharedIp, now });
  const refreshSocket = remoteRefreshScopeSocket(room, ctx, { subject: refreshSubject, connectionId: "conn-refresh-ip", ipBucketKey: sharedIp, now });

  for (let i = 0; i < IP_MESSAGE_RATE_LIMIT_FOR_TEST - 1; i += 1) {
    await send(room, filler, presenceFrame());
  }

  const firstRefresh = await send(room, refreshSocket, { t: "token.refresh", request_id: "req-ip-1", ct: "x", n: "y" });
  assert.equal(firstRefresh, undefined, "第 480 条（combined）——仍在 IP 预算内，token.refresh 应正常转发，不该被 IP 桶拦");

  const secondRefresh = await send(room, refreshSocket, { t: "token.refresh", request_id: "req-ip-2", ct: "x2", n: "y2" });
  assert.ok(secondRefresh, "第 481 条 combined 必须被拒");
  assert.equal(
    secondRefresh.reason, "ip_message_rate_limited",
    "token.refresh 自己的 6/min 桶远没打满（只发了 2 次），这条拒绝必须来自中央 IP 桶，不是 token_refresh_rate_limited"
  );
});
