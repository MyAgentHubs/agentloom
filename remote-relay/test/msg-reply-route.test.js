import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import {
  RoomDO,
  REPLY_ROUTES_ROW_LIMIT,
  REPLY_BYTE_BUDGET_LIMIT_BYTES,
  REPLY_BYTE_BUDGET_WINDOW_MS,
} from "../src/room-do.js";
import * as store from "../src/room-store.js";

// This test verifies directed reply routing through the relay: a route is
// registered for a command, and the desktop's reply is delivered only to
// the connection that sent that command. The mock runtime setup mirrors
// the pattern used in test/s1i2-fixture.test.js.

const ROOM = "b".repeat(32);
const CT = Buffer.from("hello ciphertext").toString("base64");
const N12 = Buffer.alloc(12, 7).toString("base64");
const SUBJECT = "device:11111111-1111-4111-8111-111111111111";
const OTHER_SUBJECT = "device:22222222-2222-4222-8222-222222222222";

function makeRuntime() {
  const db = new DatabaseSync(":memory:");
  const registry = [];
  const alarmTimes = [];
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
    async setAlarm(timestamp) { alarmTimes.push(timestamp); },
  };
  const ctx = {
    storage,
    acceptWebSocket(ws, tags = []) { registry.push({ ws, tags }); },
    getWebSockets(tag) {
      if (!tag) return registry.map((item) => item.ws);
      return registry.filter((item) => item.tags.includes(tag)).map((item) => item.ws);
    },
    // 模拟一条连接真的断开——同 room-do.test.js disconnectWebSocket 同款姿势，
    // 驱动「目标已断则丢弃」分支。
    disconnectWebSocket(ws) {
      const index = registry.findIndex((item) => item.ws === ws);
      if (index !== -1) registry.splice(index, 1);
    },
  };
  return { ctx, alarmTimes };
}

function fakeWs(attachment = null) {
  let currentAttachment = attachment;
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
    serializeAttachment(value) { currentAttachment = value; },
    deserializeAttachment() { return currentAttachment; },
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

function seedRemoteSubject(room, { subject, generation = 1, tokenHash, now = Date.now() }) {
  store.putTokenRegistryEntry(room.sql, {
    subject,
    generation,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: tokenHash,
      kind: "current",
      generation,
      access_expires: now + 3_600_000,
      valid_until: now + 7_200_000,
    }],
  }, now);
}

function remoteSocket(room, ctx, {
  subject = SUBJECT, generation = 1, connectionId, now = Date.now(), tokenHash = "a".repeat(64),
} = {}) {
  seedRemoteSubject(room, { subject, generation, tokenHash, now });
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const ws = fakeWs({
    role: "remote", scope: "remote", subject, kind: "current",
    generation, alias_generation: generation, access_expires: now + 3_600_000,
    valid_until: now + 7_200_000, connection_id: connectionId, epoch: currentEpoch,
  });
  ctx.acceptWebSocket(ws, ["remote"]);
  return ws;
}

function controlEnvelope({ commandId, epoch, session = "sess-1" }) {
  return {
    v: 1, room: ROOM, epoch, kind: "control", session, command_id: commandId,
    seq: null, client_msg_id: null, ct: CT, n: N12, ts: Date.now(),
  };
}

function replyEnvelope({ commandId, epoch, session = "sess-1" }) {
  return {
    v: 1, room: ROOM, epoch, kind: "reply", session, command_id: commandId,
    seq: null, client_msg_id: null, ct: "cmVwbHkgY2lwaGVydGV4dA==", n: N12, ts: Date.now(),
  };
}

async function send(room, ws, payload) {
  await room.webSocketMessage(ws, JSON.stringify(payload));
}

function lastSent(ws) {
  return ws.sent[ws.sent.length - 1];
}

function newRoom(ctx) {
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  return room;
}

// ---- 登记 → reply 定向达 ----

test("route 登记：remote 发 control(command_id) 后，desktop 回 reply 定向投给该 remote", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "mobile-conn", now });

  const control = controlEnvelope({ commandId: "cmd-fetch-1", epoch: currentEpoch });
  await send(room, mobile, control);
  assert.deepEqual(lastSent(desktop), control, "control 帧应原样转发给桌面，登记路由是纯副作用");

  const route = store.getReplyRoute(room.sql, "cmd-fetch-1");
  assert.ok(route, "route 行应已登记");
  assert.equal(route.subject, SUBJECT);
  assert.equal(route.connection_id, "mobile-conn");
  assert.equal(Number(route.bytes_sent), 0);

  const reply = replyEnvelope({ commandId: "cmd-fetch-1", epoch: currentEpoch });
  await send(room, desktop, reply);

  assert.deepEqual(lastSent(mobile), reply, "reply 应原样定向投给发起 fetch 的远端");
  assert.equal(desktop.sent.filter((frame) => frame.t === "error").length, 0, "桌面不该收到任何错误回执");

  const afterRoute = store.getReplyRoute(room.sql, "cmd-fetch-1");
  assert.ok(Number(afterRoute.bytes_sent) > 0, "bytes_sent 应按整帧字节数累计");
});

test("route 登记：desktop 离线时 control 帧不转发也不登记路由", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  desktopSocket(room, ctx); // 先建一个已在线的桌面把 epoch 推到 1，再断开模拟离线
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "mobile-conn-2", now });
  // 把桌面从 registry 里摘掉，模拟真实离线（onlineDesktopForEpoch 找不到它）。
  for (const ws of ctx.getWebSockets("desktop")) ctx.disconnectWebSocket(ws);

  await send(room, mobile, controlEnvelope({ commandId: "cmd-offline", epoch: currentEpoch }));

  assert.deepEqual(lastSent(mobile), { t: "error", reason: "desktop_offline" });
  assert.equal(store.getReplyRoute(room.sql, "cmd-offline"), null, "desktop 离线时不该建路由行");
});

// ---- 跨 subject 拒 ----

test("跨 subject：同 command_id 被另一个 subject 使用时拒绝，既有路由不被篡改", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const owner = remoteSocket(room, ctx, { subject: SUBJECT, connectionId: "owner-conn", now, tokenHash: "a".repeat(64) });

  await send(room, owner, controlEnvelope({ commandId: "cmd-conflict", epoch: currentEpoch }));
  assert.equal(desktop.sent.length, 1, "首次登记应正常转发给桌面");

  const intruder = remoteSocket(room, ctx, {
    subject: OTHER_SUBJECT, connectionId: "intruder-conn", now, tokenHash: "b".repeat(64),
  });
  await send(room, intruder, controlEnvelope({ commandId: "cmd-conflict", epoch: currentEpoch }));

  const rejection = lastSent(intruder);
  assert.equal(rejection.t, "error");
  assert.equal(rejection.reason, "reply_route_subject_conflict");
  assert.equal(desktop.sent.length, 1, "被拒的入侵帧不该转发给桌面");

  const route = store.getReplyRoute(room.sql, "cmd-conflict");
  assert.equal(route.subject, SUBJECT, "既有路由行的 subject 不被篡改");
  assert.equal(route.connection_id, "owner-conn", "既有路由行的 connection_id 不被篡改");
});

// ---- 重连重绑后 reply 达新连接 ----

test("重连重绑：同 subject 用同 command_id 从新连接重发 control 后，reply 投给新连接", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const first = remoteSocket(room, ctx, { connectionId: "conn-first", now, tokenHash: "c".repeat(64) });

  await send(room, first, controlEnvelope({ commandId: "cmd-rebind", epoch: currentEpoch }));
  assert.equal(store.getReplyRoute(room.sql, "cmd-rebind").connection_id, "conn-first");

  // 手机断线重连：新 socket、同 subject、同 command_id 续传。
  ctx.disconnectWebSocket(first);
  const second = remoteSocket(room, ctx, { connectionId: "conn-second", now, tokenHash: "c".repeat(64) });
  await send(room, second, controlEnvelope({ commandId: "cmd-rebind", epoch: currentEpoch }));
  assert.equal(store.getReplyRoute(room.sql, "cmd-rebind").connection_id, "conn-second", "路由行应重绑到新连接");
  assert.equal(desktop.sent.length, 2, "重发也会重新转发一次给桌面");

  const reply = replyEnvelope({ commandId: "cmd-rebind", epoch: currentEpoch });
  await send(room, desktop, reply);

  assert.equal(first.sent.length, 0, "旧连接不该再收到任何 reply");
  assert.deepEqual(lastSent(second), reply, "reply 应投给重绑后的新连接");
});

// ---- 返修【必修 2】：stale attachment epoch 的桌面发 reply 被拒 ----

test("stale epoch：被顶替的旧桌面即便伪造信封 epoch=当前值，仍按自己 attachment 的 epoch 被拒并关连接", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const oldDesktop = desktopSocket(room, ctx); // epoch -> 1
  const oldEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-stale-writer", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-stale-writer", epoch: oldEpoch }));
  mobile.sent.length = 0;

  const newDesktop = desktopSocket(room, ctx); // epoch -> 2，oldDesktop 从此是陈旧连接
  const currentEpoch = store.getCurrentEpoch(room.sql);
  assert.notEqual(currentEpoch, oldEpoch, "前置：epoch 确实已经推进");

  // 关键：oldDesktop 自己的 attachment.epoch 仍是 1，但它在信封里把 epoch
  // 字段伪造成当前值——如果 handleReply 只比对信封自报的 epoch 会被这一步
  // 骗过去；必须核发送方连接自己的 attachment 才挡得住。
  const forged = replyEnvelope({ commandId: "cmd-stale-writer", epoch: currentEpoch });
  await send(room, oldDesktop, forged);

  assert.equal(mobile.sent.length, 0, "伪造 epoch 也不该让陈旧桌面的 reply 投递成功");
  const rejection = lastSent(oldDesktop);
  assert.equal(rejection.t, "error");
  assert.equal(rejection.reason, "stale_epoch");
  assert.equal(oldDesktop.closed.length, 1, "陈旧桌面应被断开，同 §9.3 token 管理帧单写者判定姿势");
  assert.equal(newDesktop.sent.length, 0, "新桌面不受影响，没有被误发任何帧");
});

// ---- 返修【必修 1a】：deadline 复核·迟到 reply 不投且行当场被删 ----

test("过期 route：desktop 发 reply 时（不经过 alarm）现场复核发现已过期，丢弃且当场删行", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-late-reply", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-late-reply", epoch: currentEpoch }));
  mobile.sent.length = 0;

  // 直接把 deadline 改到过去，但**不调用 room.alarm()**——模拟「迟到的 reply
  // 在 alarm 清扫之前就先到达」这个竞态，专门验证 handleReply 自己的发送前
  // 复核，而不是依赖 alarm 已经先跑过一遍。
  room.sql.exec("UPDATE reply_routes SET deadline = ? WHERE command_id = ?", now - 1, "cmd-late-reply");
  assert.ok(store.getReplyRoute(room.sql, "cmd-late-reply"), "前置：行还在，alarm 还没跑");

  await send(room, desktop, replyEnvelope({ commandId: "cmd-late-reply", epoch: currentEpoch }));

  assert.equal(mobile.sent.length, 0, "过期行不该被当成仍然有效而投递");
  assert.equal(store.getReplyRoute(room.sql, "cmd-late-reply"), null, "handleReply 自己应该把过期行当场删掉");
});

// ---- 返修【必修 1b】：升代/撤销后旧 socket 不再收包 ----

test("升代后旧连接不收包：目标 socket 的 attachment 落后于 subject 当前 generation 时丢弃", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { generation: 1, connectionId: "conn-rotated", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-rotated", epoch: currentEpoch }));
  mobile.sent.length = 0;

  // 轮换：subject 当前代升到 2，但 mobile 这条物理连接的 attachment 仍停留
  // 在旧代 1（真实场景=令牌轮换后旧连接还没来得及被断开/重连）。
  store.putTokenRegistryEntry(room.sql, {
    subject: SUBJECT, generation: 2, state: "active", scope: "remote",
    aliases: [{
      token_hash: "f".repeat(64), kind: "current", generation: 2,
      access_expires: now + 3_600_000, valid_until: now + 7_200_000,
    }],
  }, now);

  await send(room, desktop, replyEnvelope({ commandId: "cmd-rotated", epoch: currentEpoch }));

  assert.equal(mobile.sent.length, 0, "attachment 落后于 subject 当前代的旧连接不该再收到 reply");
});

// ---- 返修【必修 3】：per-subject 字节闸 ----

test("字节闸：per-subject 窗口超限时纯丢弃该 reply（不顺延 route、不计违规、不踢无辜手机），改记独立计数器", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-bytebudget", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-bytebudget", epoch: currentEpoch }));
  mobile.sent.length = 0;
  desktop.sent.length = 0;

  // 预先把该 subject 本窗口的已用量顶到只剩 10 字节余量——任何真实信封
  // 序列化后都远超 10 字节，必定触发超限。
  room.sql.exec(
    "INSERT INTO reply_byte_limits (subject, window_started_at, bytes_sent) VALUES (?, ?, ?)",
    SUBJECT, now, REPLY_BYTE_BUDGET_LIMIT_BYTES - 10
  );

  await send(room, desktop, replyEnvelope({ commandId: "cmd-bytebudget", epoch: currentEpoch }));

  assert.equal(mobile.sent.length, 0, "超限的 reply 不该投递");
  const routeAfter = store.getReplyRoute(room.sql, "cmd-bytebudget");
  assert.equal(Number(routeAfter.bytes_sent), 0, "被拒的 reply 不该顺带累计进 route 的 bytes_sent");

  // Exceeding the reply budget drops the reply and increments a separate counter.
  // The passive mobile recipient stays connected and incurs no protocol violation.
  room.flushProtocolViolations();
  assert.equal(
    store.getMeta(room.sql, "protocol_violation_count", "0"),
    "0",
    "不该把桌面推送超量记成远端手机的协议违规"
  );
  assert.equal(
    store.getMeta(room.sql, "reply_budget_dropped", "0"),
    "1",
    "越限丢弃改记独立计数器 reply_budget_dropped"
  );
  assert.equal(mobile.closed.length, 0, "无辜的接收方手机连接不该被关闭");
});

test("字节闸：连续多次越限只累加丢弃计数器、手机连接持续保持在线（不再因反复超限被踢下线成环）", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-bytebudget-repeat", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-bytebudget-repeat", epoch: currentEpoch }));
  mobile.sent.length = 0;

  room.sql.exec(
    "INSERT INTO reply_byte_limits (subject, window_started_at, bytes_sent) VALUES (?, ?, ?)",
    SUBJECT, now, REPLY_BYTE_BUDGET_LIMIT_BYTES - 10
  );

  // 连续触发超限的次数刻意超过 PROTOCOL_VIOLATION_LIMIT（8），验证旧写法
  // 会踢人、新写法不会。
  for (let i = 0; i < 10; i += 1) {
    await send(room, desktop, replyEnvelope({ commandId: "cmd-bytebudget-repeat", epoch: currentEpoch }));
  }

  assert.equal(mobile.sent.length, 0, "全程越限、全程不投递");
  assert.equal(mobile.closed.length, 0, "连续 10 次越限也不该踢无辜的接收方手机下线");
  room.flushProtocolViolations();
  assert.equal(
    store.getMeta(room.sql, "reply_budget_dropped", "0"),
    "10",
    "丢弃计数器如实累加每一次越限"
  );
});

test("字节闸：窗口未满时正常放行且累计字节数", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-bytebudget-ok", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-bytebudget-ok", epoch: currentEpoch }));
  mobile.sent.length = 0;

  await send(room, desktop, replyEnvelope({ commandId: "cmd-bytebudget-ok", epoch: currentEpoch }));

  assert.equal(mobile.sent.length, 1, "预算充足时应正常投递");
  const budgetRow = room.sql.exec(
    "SELECT bytes_sent FROM reply_byte_limits WHERE subject = ?", SUBJECT
  )[0];
  assert.ok(Number(budgetRow.bytes_sent) > 0, "正常投递应累计进 per-subject 字节预算");
});

// ---- 返修【必修 3】：reply_routes 每房行数上限 ----

test("route 行数闸：达到上限后新 command_id 被拒登记，不转发给桌面；续盘既有行不受影响", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-rowlimit", now });

  // 直接把表灌到上限，不逐条真跑 256 次 webSocketMessage（等价且快得多）。
  for (let i = 0; i < REPLY_ROUTES_ROW_LIMIT; i += 1) {
    room.sql.exec(
      "INSERT INTO reply_routes (command_id, subject, generation, connection_id, deadline, bytes_sent) VALUES (?, ?, ?, ?, ?, 0)",
      `filler-${i}`, SUBJECT, 1, "conn-rowlimit", now + 60_000
    );
  }
  assert.equal(store.countReplyRoutes(room.sql), REPLY_ROUTES_ROW_LIMIT);

  await send(room, mobile, controlEnvelope({ commandId: "cmd-over-limit", epoch: currentEpoch }));

  const rejection = lastSent(mobile);
  assert.equal(rejection.t, "error");
  assert.equal(rejection.reason, "reply_routes_row_limit");
  assert.equal(desktop.sent.length, 0, "被行数闸拒绝的帧不该转发给桌面");
  assert.equal(store.getReplyRoute(room.sql, "cmd-over-limit"), null, "超限的新 command_id 不该建行");

  // 续盘既有行（同 command_id 重连重绑）不受行数闸影响——它不会让表再多一行。
  mobile.sent.length = 0;
  desktop.sent.length = 0;
  await send(room, mobile, controlEnvelope({ commandId: "filler-0", epoch: currentEpoch }));
  assert.equal(mobile.sent.length, 0, "续盘既有行不该收到任何错误帧");
  assert.equal(desktop.sent.length, 1, "续盘既有行应正常转发给桌面");
  assert.equal(store.countReplyRoutes(room.sql), REPLY_ROUTES_ROW_LIMIT, "续盘不新增行");
});

test("route 行数闸：过期行占满上限时，新登记先清死行再判容量，成功登记", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-expired-rowlimit", now });

  // 把表灌到上限，但全部是已过期（deadline 早于 now）的死行——alarm 还没
  // 来得及清扫。旧写法（判容量前不清过期行）会误判为「满」而拒绝新登记；
  // 本任务要求容量判断前先 deleteExpiredReplyRoutes，死行不该占着容量。
  for (let i = 0; i < REPLY_ROUTES_ROW_LIMIT; i += 1) {
    room.sql.exec(
      "INSERT INTO reply_routes (command_id, subject, generation, connection_id, deadline, bytes_sent) VALUES (?, ?, ?, ?, ?, 0)",
      `expired-${i}`, SUBJECT, 1, "conn-expired-rowlimit", now - 1_000
    );
  }
  assert.equal(store.countReplyRoutes(room.sql), REPLY_ROUTES_ROW_LIMIT);

  await send(room, mobile, controlEnvelope({ commandId: "cmd-after-expiry-cleanup", epoch: currentEpoch }));

  assert.equal(desktop.sent.length, 1, "过期行占满时新登记应正常转发给桌面");
  const rejection = mobile.sent.find((frame) => frame.t === "error");
  assert.equal(rejection, undefined, "不该收到 reply_routes_row_limit 拒绝");
  assert.notEqual(
    store.getReplyRoute(room.sql, "cmd-after-expiry-cleanup"), null,
    "新 command_id 应成功建行"
  );
  assert.equal(store.countReplyRoutes(room.sql), 1, "死行已被清空，只剩新登记的这一行");
});

// ---- TTL 过期后 reply 丢弃 ----

test("TTL 过期：alarm 清理过期路由行后，desktop 的 reply 被丢弃（远端零收）", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-ttl", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-ttl", epoch: currentEpoch }));
  assert.ok(store.getReplyRoute(room.sql, "cmd-ttl"), "前置：路由行已登记");

  // 直接把 deadline 改到过去，模拟「登记后桌面迟迟不回、TTL 到期」——不依赖
  // 具体 TTL 常量值，只验证「过期后被 alarm 真删、reply 因此被丢弃」这条
  // 不变量本身。
  room.sql.exec("UPDATE reply_routes SET deadline = ? WHERE command_id = ?", now - 1, "cmd-ttl");
  await room.alarm();
  assert.equal(store.getReplyRoute(room.sql, "cmd-ttl"), null, "alarm 应已把过期路由行真删");

  await send(room, desktop, replyEnvelope({ commandId: "cmd-ttl", epoch: currentEpoch }));
  assert.equal(mobile.sent.length, 0, "TTL 过期后 reply 应被丢弃，远端零收");
  assert.equal(desktop.sent.filter((frame) => frame.t === "error").length, 0, "对桌面无回执（丢弃是静默的）");
});

test("目标已断：无 route 行/连接已断都静默丢弃，不报错给桌面", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);

  // 未知 command_id：从未登记过路由。
  await send(room, desktop, replyEnvelope({ commandId: "cmd-unknown", epoch: currentEpoch }));
  assert.equal(desktop.sent.length, 0, "未知 command_id 的 reply 应静默丢弃");

  // 已登记但目标连接断开。
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-vanish", now });
  await send(room, mobile, controlEnvelope({ commandId: "cmd-vanish", epoch: currentEpoch }));
  ctx.disconnectWebSocket(mobile);

  await send(room, desktop, replyEnvelope({ commandId: "cmd-vanish", epoch: currentEpoch }));
  assert.equal(desktop.sent.filter((frame) => frame.t === "error").length, 0, "目标已断也应静默丢弃，不报错给桌面");
  assert.ok(store.getReplyRoute(room.sql, "cmd-vanish"), "已断连接不该删行——留给重连重绑恢复");
});

// ---- 不落库、不广播 ----

test("reply 不落库不广播：events 表零新行，房间内其它 socket 零收", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-noleak", now, tokenHash: "d".repeat(64) });
  const bystander = remoteSocket(room, ctx, {
    subject: OTHER_SUBJECT, connectionId: "conn-bystander", now, tokenHash: "e".repeat(64),
  });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-noleak", epoch: currentEpoch }));
  // 排空登记这一步产生的所有帧后，再单独观测 reply 转发这一步。
  mobile.sent.length = 0;
  bystander.sent.length = 0;
  desktop.sent.length = 0;

  // 返修【测试补强①】：直接查 events 表行数前后相等——比 headSeq 更直接，
  // headSeq 只是「序号没往前走」的间接推论，不能排除「插了一行但没盖 seq」
  // 这种理论上的旁路；COUNT(*) 才是「真的没有新行落库」本身。
  const eventsCountBefore = room.sql.exec("SELECT COUNT(*) AS n FROM events")[0].n;
  await send(room, desktop, replyEnvelope({ commandId: "cmd-noleak", epoch: currentEpoch }));

  const eventsCountAfter = room.sql.exec("SELECT COUNT(*) AS n FROM events")[0].n;
  assert.equal(Number(eventsCountAfter), Number(eventsCountBefore), "reply 不应落 events 表新行");
  assert.equal(bystander.sent.length, 0, "reply 不该广播给房间内其它远端");
  assert.equal(mobile.sent.length, 1, "只有发起 fetch 的那个远端应收到");
  assert.equal(mobile.sent[0].kind, "reply");
});

// ---- scope 矩阵：remote 入站 reply 拒（fail-closed） ----

test("scope 矩阵：remote 连接主动发 kind=reply 被拒（矩阵外帧）", async () => {
  const { ctx } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-forbidden", now });

  await send(room, mobile, replyEnvelope({ commandId: "cmd-forbidden", epoch: currentEpoch }));

  const actual = lastSent(mobile);
  assert.equal(actual.t, "error");
  assert.equal(actual.reason, "role_forbidden");
});

// ---- 【测试补强】reply 与 token 面 alarm 共存排班 ----

test("alarm 排班：reply_routes.deadline 与 refresh_requests.deadline 共存时取更早的那个，过期后回落到另一个", async () => {
  const { ctx, alarmTimes } = makeRuntime();
  const room = newRoom(ctx);
  const now = Date.now();
  desktopSocket(room, ctx);
  const currentEpoch = store.getCurrentEpoch(room.sql);
  const mobile = remoteSocket(room, ctx, { connectionId: "conn-alarm-coexist", now });

  await send(room, mobile, controlEnvelope({ commandId: "cmd-alarm-coexist", epoch: currentEpoch }));
  // 手动把 reply_routes 的 deadline 钉成一个已知值，隔离出一个确定的候选，
  // 不依赖 REPLY_ROUTE_TTL_MS 的具体数值。
  room.sql.exec("UPDATE reply_routes SET deadline = ? WHERE command_id = ?", now + 5_000, "cmd-alarm-coexist");

  store.upsertRefreshRequest(room.sql, {
    requestId: "req-alarm-coexist", subject: SUBJECT, requestGeneration: 1,
    connectionId: "conn-alarm-coexist", deadline: now + 10_000,
  });

  await room.scheduleNextTokenAlarm(now);
  assert.equal(alarmTimes.at(-1), now + 5_000, "reply_routes.deadline(+5s) 早于 refresh_requests.deadline(+10s)，min 应取更早者");

  // reply 候选过期消失后（+5001ms），min 应回落到 refresh_requests 那一侧。
  const past = now + 5_001;
  await room.scheduleNextTokenAlarm(past);
  assert.equal(alarmTimes.at(-1), now + 10_000, "reply 候选过期被排除后，min 回落到 refresh_requests.deadline");
});
