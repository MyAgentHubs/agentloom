import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";

// S1i2 · relay:refresh 三帧 + refresh_requests 表 + 投递谓词 + 限速桶

const FIXTURES = JSON.parse(readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8"));
const TOKEN_FRAMES = FIXTURES.filter((item) => item.layer === "token-frame");
const REFRESH_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.refresh");
const FORWARD_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.refresh.forward");
const OK_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.refresh.ok");
const FAIL_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.refresh.fail");

const SUBJECT = "device:11111111-1111-4111-8111-111111111111";

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
  };
  return { ctx, alarmTimes };
}

function fakeWs(attachment = null) {
  let currentAttachment = attachment;
  return {
    sent: [],
    closed: [],
    // R1：deliverRefreshReceipt 现在核 readyState !== OPEN(1) 才投递——close()
    // 后置 CLOSED，让"已断=不再投递"这条不变量在这个 mock 上继续成立。
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

function seedRefreshSubject(room, {
  subject = SUBJECT,
  generation,
  prevTokenHash = "7".repeat(64),
  now = Date.now(),
  validUntil = now + 3_600_000,
}) {
  return store.putTokenRegistryEntry(room.sql, {
    subject,
    generation,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: prevTokenHash,
      kind: "prev",
      generation,
      access_expires: null,
      valid_until: validUntil,
    }],
  }, now, { cas: true });
}

// R3：真实 upgrade 路径下每个连接的 attachment 都恒带 ip_bucket_key（见
// room-do.js fetch() 里 `ip_bucket_key: ipBucketKey` 是顶层字段、desktop/
// remote 都有）——这里给一个 32 位 hex 形态的默认假值，贴近
// deriveIpBucketKey 的真实输出形状（16 字节截断哈希的 hex）。
function remoteRefreshSocket(room, ctx, {
  subject = SUBJECT, generation, connectionId, now = Date.now(), ipBucketKey = "b".repeat(32),
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

async function send(room, ws, frameOrText) {
  const before = ws.sent.length;
  await room.webSocketMessage(ws, typeof frameOrText === "string" ? frameOrText : JSON.stringify(frameOrText));
  return ws.sent[before];
}

// ---- 2a/2b：wire-v1 token-frame 六条 token_refresh_* 逐条走真实 webSocketMessage ----

test("S1i2 fixture：token.refresh 2 条逐条走真实 webSocketMessage（含转发到桌面）", async (t) => {
  assert.equal(REFRESH_CASES.length, 2);
  for (const item of REFRESH_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const now = Date.now();
      seedRefreshSubject(room, { generation: 100, now });
      const desktop = desktopSocket(room, ctx);
      const mobile = remoteRefreshSocket(room, ctx, { generation: 100, connectionId: "mobile-conn", now });

      const actual = await send(room, mobile, item.frame);
      if (item.expect.valid) {
        assert.equal(desktop.sent.length, 1);
        assert.deepEqual(desktop.sent[0], {
          t: "token.refresh.forward",
          request_id: item.frame.request_id,
          subject: SUBJECT,
          request_generation: 100,
          ct: item.frame.ct,
          n: item.frame.n,
        });
        assert.equal(mobile.closed.length, 0);
        const row = store.getRefreshRequest(room.sql, item.frame.request_id);
        assert.equal(row.subject, SUBJECT);
        assert.equal(Number(row.request_generation), 100);
        assert.equal(row.connection_id, "mobile-conn");
      } else {
        assert.equal(actual.t, "error");
        assert.deepEqual([actual.reason], item.expect.errors);
        assert.equal(desktop.sent.length, 0, "结构不合法的请求不该转发给桌面");
        room.flushProtocolViolations();
        assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "1");
      }
    });
  }
});

test("S1i2 fixture：token.refresh.forward 出站形状与样张一致（relay 盖章 subject/request_generation）", async () => {
  assert.equal(FORWARD_CASES.length, 1);
  const item = FORWARD_CASES[0];
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: item.frame.request_generation, now });
  const desktop = desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: item.frame.request_generation, connectionId: "mobile-conn", now });

  await send(room, mobile, {
    t: "token.refresh", request_id: item.frame.request_id, ct: item.frame.ct, n: item.frame.n,
  });

  assert.deepEqual(desktop.sent.at(-1), item.frame);
});

test("S1i2 fixture：token.refresh.ok / token.refresh.fail 3 条逐条走真实 webSocketMessage（自有谓词投递）", async (t) => {
  const cases = [...OK_CASES, ...FAIL_CASES];
  assert.equal(cases.length, 3);
  for (const item of cases) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const now = Date.now();
      // ok_valid 样张里 request_generation(建行时)=100、回执 generation=101——
      // 故意演练谓词④「不要求等于 request_generation，只认 subject 当前代」。
      seedRefreshSubject(room, { generation: item.frame.generation ?? 100, now });
      const desktop = desktopSocket(room, ctx);
      const mobile = remoteRefreshSocket(room, ctx, { generation: item.frame.generation ?? 100, connectionId: "mobile-conn", now });
      store.upsertRefreshRequest(room.sql, {
        requestId: item.frame.request_id,
        subject: item.frame.subject,
        requestGeneration: 100,
        connectionId: "mobile-conn",
        deadline: now + 60_000,
      });

      await send(room, desktop, item.frame);

      assert.deepEqual(mobile.sent.at(-1), item.frame);
      assert.equal(store.getRefreshRequest(room.sql, item.frame.request_id), null, "投完删行");
    });
  }
});

// ---- 2b：投递谓词六条，逐条独立可证（配合变异自证；见 worker 报告） ----

function baseRow(overrides = {}) {
  return {
    requestId: "req-pred",
    subject: SUBJECT,
    requestGeneration: 5,
    connectionId: "target-conn",
    deadline: Date.now() + 60_000,
    ...overrides,
  };
}

test("投递谓词①：房 live——tombstoned 房间不投递", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now + 60_000 }));

  // 故意不走 store.tombstoneRoom（它会在同一事务里把 refresh_requests 行也
  // 清空，导致本条谓词永远走不到——if (!row) return 会先短路，测不出①本身）。
  // 直接改 room_state.tombstoned_at，让「房不 live」与「行还在」这个（正常
  // 生产路径不会出现、但代码仍要防的）组合独立成立，才能真正验到①这条闸。
  room.sql.exec("UPDATE room_state SET tombstoned_at = ?", now);
  assert.equal(store.isRoomLive(room.sql), false);
  assert.notEqual(store.getRefreshRequest(room.sql, "req-pred"), null, "本测试特意保留行，隔离验证①本身");

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "x", n: "y" }, now);
  assert.equal(target.sent.length, 0);
});

test("投递谓词②：subject active——subject 已注销不投递", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now + 60_000 }));

  // 故意保持同一个 generation(5) 撤销——只想单独触发②（active 检查），不想
  // 顺带把 generation 也改动而误触④（那样测试对②的 mutation 就不敏感了：
  // ④会先因为代号不符而拒，②被不被真的执行都测不出来）。同代号撤销走 CAS
  // 会被「同代号必须内容幂等」的 generation_content_mismatch 挡住，所以这里
  // 不传 cas，直接覆盖写（纯测试手段，不代表生产 token.delete 的真实调用姿势
  // ——生产撤销走新代号，见 handleTokenDelete）。
  store.putTokenRegistryEntry(room.sql, {
    subject: SUBJECT, generation: 5, state: "revoked", scope: null, aliases: [],
  }, now);

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "x", n: "y" }, now);
  assert.equal(target.sent.length, 0);
});

test("投递谓词③：回执.subject == 请求行.subject——不同 subject 不投递", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now + 60_000 }));

  room.deliverRefreshReceipt({
    t: "token.refresh.ok", request_id: "req-pred", subject: "device:22222222-2222-4222-8222-222222222222",
    generation: 5, ct: "x", n: "y",
  }, now);
  assert.equal(target.sent.length, 0);
});

test("投递谓词④：回执.generation == subject 当前 generation——旧代回执不投递，新代回执可投", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now }); // 建行时 subject 代=5
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ requestGeneration: 5, deadline: now + 60_000 }));

  seedRefreshSubject(room, { generation: 6, now }); // 轮换：subject 当前代变成 6

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "stale", n: "stale" }, now);
  assert.equal(target.sent.length, 0, "回执自带的代号(5)落后于 subject 当前代(6)，不该投递");

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 6, ct: "fresh", n: "fresh" }, now);
  assert.equal(target.sent.length, 1, "回执自带的代号(6)追上 subject 当前代，应投递——不要求等于 request_generation(5)");
});

test("投递谓词④豁免：token.refresh.fail 帧结构上不带 generation，不受本条约束", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ requestGeneration: 5, deadline: now + 60_000 }));

  seedRefreshSubject(room, { generation: 6, now }); // 轮换后

  room.deliverRefreshReceipt({ t: "token.refresh.fail", request_id: "req-pred", subject: SUBJECT, reason: "invalid_repeated" }, now);
  assert.equal(target.sent.length, 1);
});

test("投递谓词⑤：now < deadline——过期不投递且删行", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now - 1 }));

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "x", n: "y" }, now);
  assert.equal(target.sent.length, 0);
  assert.equal(store.getRefreshRequest(room.sql, "req-pred"), null, "过期删行");
});

test("投递谓词⑥：目标=请求行 connection_id——已断则丢弃且不删行（手机走 resend 重绑恢复）", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  // 留一个活 socket，但它的 connection_id 跟请求行记的不是同一个——必须证明
  // 「场上有别的连接」不会被误当成目标（否则只测了「一个连接都没有」这种
  // 弱场景，取第一个能碰到的 socket 也会误打误撞通过）。
  const bystander = fakeWs({ role: "remote", scope: "refresh", connection_id: "someone-elses-conn" });
  ctx.acceptWebSocket(bystander, ["remote"]);
  // 故意不 accept 任何 connection_id === "target-conn" 的 socket——模拟已断线。
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now + 60_000 }));

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "x", n: "y" }, now);
  assert.equal(bystander.sent.length, 0, "connection_id 对不上的旁观连接不该被当成投递目标");
  assert.notEqual(store.getRefreshRequest(room.sql, "req-pred"), null, "已断连接不删行，等 resend 重绑");
});

test("投递成功：目标收到原样 payload 且行被删（投完删行）", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now + 60_000 }));

  const payload = { t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "x", n: "y" };
  room.deliverRefreshReceipt(payload, now);

  assert.deepEqual(target.sent.at(-1), payload);
  assert.equal(store.getRefreshRequest(room.sql, "req-pred"), null);
});

test("未知 request_id：安全丢弃，不抛错", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "no-such-request", subject: SUBJECT, generation: 1, ct: "x", n: "y" }, now);
  // 不抛错即通过；没有任何行为可断言。
});

// ---- 2a：同 request_id 从新连接重发·安全重绑 connection_id；不同 subject 一律拒 ----

test("2a：同 request_id 从新连接重发（同 subject）→ 安全重绑 connection_id", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const desktop = desktopSocket(room, ctx);
  const first = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-first", now });

  await send(room, first, { t: "token.refresh", request_id: "req-rebind", ct: "x", n: "y" });
  assert.equal(store.getRefreshRequest(room.sql, "req-rebind").connection_id, "conn-first");

  const second = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-second", now });
  await send(room, second, { t: "token.refresh", request_id: "req-rebind", ct: "x2", n: "y2" });

  assert.equal(store.getRefreshRequest(room.sql, "req-rebind").connection_id, "conn-second");
  assert.equal(desktop.sent.length, 2, "重发也会重新转发一次给桌面（幂等由桌面侧 journal 兜底）");
});

test("2a：同 request_id 不同 subject 一律拒——不篡改既有行", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const otherSubject = "device:22222222-2222-4222-8222-222222222222";
  seedRefreshSubject(room, { subject: SUBJECT, generation: 5, prevTokenHash: "7".repeat(64), now });
  seedRefreshSubject(room, { subject: otherSubject, generation: 5, prevTokenHash: "8".repeat(64), now });
  desktopSocket(room, ctx);
  const owner = remoteRefreshSocket(room, ctx, { subject: SUBJECT, generation: 5, connectionId: "conn-owner", now });
  await send(room, owner, { t: "token.refresh", request_id: "req-conflict", ct: "x", n: "y" });
  assert.equal(store.getRefreshRequest(room.sql, "req-conflict").subject, SUBJECT);

  const intruder = remoteRefreshSocket(room, ctx, { subject: otherSubject, generation: 5, connectionId: "conn-intruder", now });
  const actual = await send(room, intruder, { t: "token.refresh", request_id: "req-conflict", ct: "x2", n: "y2" });

  assert.equal(actual.t, "error");
  assert.equal(actual.reason, "refresh_request_subject_conflict");
  assert.equal(store.getRefreshRequest(room.sql, "req-conflict").subject, SUBJECT, "既有行不被篡改");
  assert.equal(store.getRefreshRequest(room.sql, "req-conflict").connection_id, "conn-owner");
});

test("2a：desktop 不在线 → 回 desktop_offline，不建行", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-1", now });

  const actual = await send(room, mobile, { t: "token.refresh", request_id: "req-offline", ct: "x", n: "y" });

  assert.deepEqual(actual, { t: "error", reason: "desktop_offline" });
  assert.equal(store.getRefreshRequest(room.sql, "req-offline"), null);
});

test("2a：legacy remote（无 subject）不能盖章身份，明确拒绝", async () => {
  // S1ja F2：legacy 无 subject 的 remote 豁免已撤——过去这类连接能一路走到
  // handleTokenRefresh 自己的 `!attachment.subject` 兜底（reason
  // "subject_required"）才被拒；现在 authorizeInboundSocket 这道更早的闸就已经
  // fail-closed（reason "reauthorization_failed"），handleTokenRefresh 那条
  // 兜底对这条路径已经摸不到——留着当纵深防御，不删（不是本单 F2 点名的那
  // 两条豁免）。本测试改钉住新的、更早生效的拒绝点。
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  desktopSocket(room, ctx);
  const legacy = fakeWs({ role: "remote", scope: "remote", epoch: 0 });
  ctx.acceptWebSocket(legacy, ["remote"]);

  const actual = await send(room, legacy, { t: "token.refresh", request_id: "req-legacy", ct: "x", n: "y" });

  assert.equal(actual.t, "error");
  assert.equal(actual.reason, "reauthorization_failed");
  assert.equal(legacy.closed.length, 1);
  room.flushProtocolViolations();
  // authorizeInboundSocket 的 fail-closed 分支不计协议违例（它不是「拿着合法
  // 凭据乱扫帧型」那类违例，是身份本身就站不住——见 room-do.js
  // authorizeInboundSocket 与 recordProtocolViolation 的调用点）。
  assert.equal(store.getMeta(room.sql, "protocol_violation_count", "0"), "0");
});

// ---- 2d：P2-3 alarm 统一 min 计算 ----

test("2d：refresh_requests.deadline 与 token 过期并存时，alarm 落在更早那个；到期后另一个仍会被重新排上", async () => {
  const { ctx, alarmTimes } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:55555555-5555-4555-8555-555555555555";
  // attachment.valid_until 必须与实际种下的 alias.valid_until 一致——
  // isOfficialAttachmentLive 会逐字段核对两者，不一致就判死、这个候选
  // 时刻从此不会进入 scheduleNextTokenAlarm 的 min 计算。
  seedRefreshSubject(room, { subject, generation: 1, prevTokenHash: "9".repeat(64), now, validUntil: now + 10_000 });
  const remote = fakeWs({
    role: "remote", scope: "refresh", subject, kind: "prev", generation: 1, alias_generation: 1,
    access_expires: null, valid_until: now + 10_000, connection_id: "conn-token-expiry", epoch: 0,
  });
  ctx.acceptWebSocket(remote, ["remote"]);
  store.upsertRefreshRequest(room.sql, {
    requestId: "req-earlier", subject, requestGeneration: 1, connectionId: "conn-token-expiry", deadline: now + 5_000,
  });

  await room.scheduleNextTokenAlarm(now);
  assert.equal(alarmTimes.at(-1), now + 5_000, "refresh_requests.deadline(+5s) 早于 token valid_until(+10s)");

  // 到期后（refresh 行已过期）再算一次：min 应回落到 token 过期时刻。
  const past = now + 5_001;
  await room.scheduleNextTokenAlarm(past);
  assert.equal(alarmTimes.at(-1), now + 10_000, "过期的 refresh 行被排除，另一类时刻(token valid_until)重新排上");
});

// ---- 2e：generation 为 NULL 的 pending_input 行显式 fail-closed ----

test("2e：isPendingInputAuthorized 对 generation 为 NULL 的行显式返回 false", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:66666666-6666-4666-8666-666666666666";
  store.putTokenRegistryEntry(room.sql, {
    subject, generation: 1, state: "active", scope: "remote",
    aliases: [{ token_hash: "a".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 }],
  }, now);

  assert.equal(store.isPendingInputAuthorized(room.sql, { subject, generation: null }), false);
  assert.equal(store.isPendingInputAuthorized(room.sql, { subject, generation: undefined }), false);
  // 对照组：generation 匹配时应为 true——证明上面两条不是因为 subject 查不到。
  assert.equal(store.isPendingInputAuthorized(room.sql, { subject, generation: 1 }), true);
});

// ---- 2f：per-socket 6/min，同 request_id 重发不计配额 ----

test("2f：token.refresh per-socket 6/min，第七次不同 request_id 被限速并关连接", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-quota", now });

  for (let attempt = 1; attempt <= 6; attempt += 1) {
    const actual = await send(room, mobile, { t: "token.refresh", request_id: `req-quota-${attempt}`, ct: "x", n: "y" });
    assert.equal(actual, undefined, `第 ${attempt} 次应成功转发，不该有直接回执`);
    assert.equal(mobile.closed.length, 0);
  }
  const seventh = await send(room, mobile, { t: "token.refresh", request_id: "req-quota-7", ct: "x", n: "y" });
  assert.equal(seventh.reason, "token_refresh_rate_limited");
  assert.equal(mobile.closed.length, 1);
  assert.equal(store.getRefreshRequest(room.sql, "req-quota-7"), null, "被限速的新请求不建行");
});

test("2f：同 request_id 重发不计配额——6 次新请求耗尽配额后，重发老 request_id 仍放行", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-quota-2", now });

  for (let attempt = 1; attempt <= 6; attempt += 1) {
    await send(room, mobile, { t: "token.refresh", request_id: `req-quota2-${attempt}`, ct: "x", n: "y" });
  }
  // 配额已耗尽；重发第一次的 request_id（同一个待决行）不该再算一次新配额。
  const resend = await send(room, mobile, { t: "token.refresh", request_id: "req-quota2-1", ct: "x2", n: "y2" });
  assert.equal(resend, undefined, "resend 不计配额，应正常转发而非限速拒绝");
  assert.equal(mobile.closed.length, 0);
});

// ---- 2c/P1-b：canDeliverOutbound 对 refresh 回执恒不投递（自有谓词是例外的另一半） ----

test("2c：canDeliverOutbound 对 token.refresh.ok/fail 恒返回 false——回执改走 deliverRefreshReceipt，不再是它的管辖范围", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  // 特意构造一个「按旧逻辑本该放行」的 attachment：scope=refresh、subject
  // 匹配、且此刻仍是活 official attachment——旧实现下 canDeliverOutbound 对
  // isRefreshReceipt 就是判它 true。新实现里这类判断已经整个搬去
  // deliverRefreshReceipt 自己的谓词，canDeliverOutbound 不再经手这两个帧型。
  const ws = fakeWs({
    role: "remote", scope: "refresh", subject: SUBJECT, kind: "prev",
    generation: 5, alias_generation: 5, access_expires: null, valid_until: now + 3_600_000,
  });
  ctx.acceptWebSocket(ws, ["remote"]);

  assert.equal(room.canDeliverOutbound(ws, { t: "token.refresh.ok", subject: SUBJECT, generation: 5 }, now), false);
  assert.equal(room.canDeliverOutbound(ws, { t: "token.refresh.fail", subject: SUBJECT }, now), false);
  // 对照组：forward 帧同理恒不投递（relay 自己直发给桌面，从不走这条闸）。
  assert.equal(room.canDeliverOutbound(ws, { t: "token.refresh.forward", subject: SUBJECT, request_generation: 5 }, now), false);
});

// ---- S1i2 返工 R1：过期行必须真的删，不只是查询时过滤（§9.6 第 249 行） ----

test("R1：alarm 到期后过期行确实被删（不是查询时过滤）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  store.upsertRefreshRequest(room.sql, {
    requestId: "req-expired", subject: SUBJECT, requestGeneration: 5,
    connectionId: "target-conn", deadline: now - 1_000,
  });
  assert.notEqual(store.getRefreshRequest(room.sql, "req-expired"), null, "前置：行建好，尚未被 alarm 清理");

  await room.alarm();

  assert.equal(store.getRefreshRequest(room.sql, "req-expired"), null, "alarm 到期后应真的删行");
});

test("R1 回归：过期行被 alarm 清理后，重放同 request_id 不再永久免疫限速——需重新计费配额", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-recycle", now });

  // 先建一行已过期的 request（模拟「回执从未到达」而滞留的行——谓词⑥保留行
  // 的场景，或桌面掉线场景）。
  store.upsertRefreshRequest(room.sql, {
    requestId: "req-recycle", subject: SUBJECT, requestGeneration: 5,
    connectionId: "conn-recycle", deadline: now - 1_000,
  });

  await room.alarm();
  assert.equal(store.getRefreshRequest(room.sql, "req-recycle"), null, "前置条件：alarm 已清理过期行");

  // 打满 6/min 主配额（全新 request_id，与 req-recycle 无关）。
  for (let attempt = 1; attempt <= 6; attempt += 1) {
    await send(room, mobile, { t: "token.refresh", request_id: `req-recycle-fill-${attempt}`, ct: "x", n: "y" });
  }
  assert.equal(mobile.closed.length, 0);

  // 配额耗尽后重新发起同一个 request_id——行已被删，不再命中 isResend，必须
  // 重新过主配额闸、被限速关连接。修复前：行永远不删 → isResend 永远为真 →
  // 这次调用会正常转发（undefined），不会被限速。
  const actual = await send(room, mobile, { t: "token.refresh", request_id: "req-recycle", ct: "z", n: "w" });
  assert.equal(actual.reason, "token_refresh_rate_limited");
  assert.equal(mobile.closed.length, 1);
});

// ---- S1i2 返工 R2：request_id 128 字节上限（同 command_id 口径·UTF-8 字节计） ----

test("R2：request_id 恰好 128 字节合法（边界含）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const desktop = desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-r2-128", now });
  const requestId = "a".repeat(128);

  const actual = await send(room, mobile, { t: "token.refresh", request_id: requestId, ct: "x", n: "y" });

  assert.equal(actual, undefined, "128 字节应正常转发，不应被拒");
  assert.equal(desktop.sent.length, 1);
  assert.notEqual(store.getRefreshRequest(room.sql, requestId), null);
});

test("R2：request_id 129 字节被拒（ASCII，恰超边界一字节）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const desktop = desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-r2-129", now });
  const requestId = "a".repeat(129);

  const actual = await send(room, mobile, { t: "token.refresh", request_id: requestId, ct: "x", n: "y" });

  // 先钉住可观测副作用（越界请求不该打到桌面/不该建行）——这条比「回了什么」
  // 更贴近命题：即便变异让 send() 误放行、只返回 undefined，这里也会因为
  // 副作用不成立而挂在实质断言上，而不是挂在读 undefined.t 的 TypeError 上。
  assert.equal(desktop.sent.length, 0, "超限请求不该转发给桌面");
  assert.equal(store.getRefreshRequest(room.sql, requestId), null, "超限请求不该建行");
  assert.ok(actual, "越界请求必须回一帧拒绝");
  assert.equal(actual.t, "error");
  assert.equal(actual.reason, "request_id_too_long");
});

test("R2：超 128 字节按 UTF-8 字节计，非字符数——43 个中文字符(129 字节)被拒", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-r2-utf8", now });
  const requestId = "中".repeat(43); // 43*3=129 字节，字符数只有 43

  const actual = await send(room, mobile, { t: "token.refresh", request_id: requestId, ct: "x", n: "y" });

  assert.equal(actual.reason, "request_id_too_long", "129 字节应被拒——若误按 .length(43) 计会误判合法");
});

// ---- S1i2 返工 R3：refresh_requests 补 ip_bucket_key 列（§9.8 第 263 行） ----

test("R3：建行时把 attachment.ip_bucket_key 落盘", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, {
    generation: 5, connectionId: "conn-ipkey", now, ipBucketKey: "c".repeat(32),
  });

  await send(room, mobile, { t: "token.refresh", request_id: "req-ipkey", ct: "x", n: "y" });

  const row = store.getRefreshRequest(room.sql, "req-ipkey");
  assert.equal(row.ip_bucket_key, "c".repeat(32));
});

test("R3：同 request_id 从新连接重发——ip_bucket_key 跟着新连接更新", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  desktopSocket(room, ctx);
  const first = remoteRefreshSocket(room, ctx, {
    generation: 5, connectionId: "conn-ipkey-first", now, ipBucketKey: "1".repeat(32),
  });

  await send(room, first, { t: "token.refresh", request_id: "req-ipkey-rebind", ct: "x", n: "y" });
  assert.equal(store.getRefreshRequest(room.sql, "req-ipkey-rebind").ip_bucket_key, "1".repeat(32));

  const second = remoteRefreshSocket(room, ctx, {
    generation: 5, connectionId: "conn-ipkey-second", now, ipBucketKey: "2".repeat(32),
  });
  await send(room, second, { t: "token.refresh", request_id: "req-ipkey-rebind", ct: "x2", n: "y2" });

  assert.equal(store.getRefreshRequest(room.sql, "req-ipkey-rebind").ip_bucket_key, "2".repeat(32));
});

// ---- S1i2 返工 R4：resend 免主配额要有独立宽松桶兜底 ----

test("R4：resend 独立宽松桶——超过 30/min 后第 31 次同 request_id 重放被限速并关连接", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-resend-flood", now });

  // 先建行（消耗主配额 1 次，与 resend 桶无关）。
  await send(room, mobile, { t: "token.refresh", request_id: "req-resend-flood", ct: "x", n: "y" });
  assert.equal(mobile.closed.length, 0);

  // 同一个 request_id 疯狂重放 30 次——全部落在 isResend 分支，不该碰主配额，
  // 应该全部成功（不关连接）。
  for (let attempt = 1; attempt <= 30; attempt += 1) {
    const actual = await send(room, mobile, { t: "token.refresh", request_id: "req-resend-flood", ct: `x${attempt}`, n: `y${attempt}` });
    assert.equal(actual, undefined, `第 ${attempt} 次 resend 应成功转发`);
  }
  assert.equal(mobile.closed.length, 0, "30 次以内不该被限速");

  // 第 31 次 resend 撞上独立宽松桶，应被限速并关连接。
  const overflow = await send(room, mobile, { t: "token.refresh", request_id: "req-resend-flood", ct: "over", n: "over" });
  // 先钉住「关连接」这条可观测副作用——变异让 isResend 分支绕过宽松桶时，
  // 第 31 次会被当正常 resend 放行（overflow===undefined、连接不关），这里
  // 会先在「关连接」这条断言上给出实质失败，而不是先在读 undefined.reason
  // 时抛 TypeError。
  assert.equal(mobile.closed.length, 1, "第 31 次 resend 应被限速并关连接");
  assert.ok(overflow, "被限速的第 31 次必须回一帧拒绝");
  assert.equal(overflow.reason, "token_refresh_resend_rate_limited");
});

test("R4：合理重放（远低于宽松桶上限）不受影响——正常崩溃恢复重放不该被挡", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const desktop = desktopSocket(room, ctx);
  const mobile = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "conn-resend-normal", now });

  await send(room, mobile, { t: "token.refresh", request_id: "req-resend-normal", ct: "x", n: "y" });
  assert.equal(desktop.sent.length, 1);

  // 手机重启复用同一个 request_id 重放几次（远低于 30/min）——应正常转发，
  // 不受影响、不关连接。
  for (let attempt = 1; attempt <= 5; attempt += 1) {
    const actual = await send(room, mobile, { t: "token.refresh", request_id: "req-resend-normal", ct: `retry${attempt}`, n: `retry${attempt}` });
    assert.equal(actual, undefined);
  }
  assert.equal(desktop.sent.length, 6, "首次 + 5 次合理重放，均应转发给桌面");
  assert.equal(mobile.closed.length, 0);
});

// ---- S1i2 返工 R5 束 ----

test("R5.1：目标 socket 已被 subject_limit_closed 标记——不投递（对齐 canDeliverOutbound）", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({
    role: "remote", scope: "refresh", connection_id: "target-conn", subject_limit_closed: true,
  });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now + 60_000 }));

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "x", n: "y" }, now);

  assert.equal(target.sent.length, 0, "subject_limit_closed 的连接不该收到回执");
  assert.notEqual(store.getRefreshRequest(room.sql, "req-pred"), null, "同 ⑥ 不删行，交给重试恢复");
});

test("R5.2：assertRoomLive 先于任何 DB 读——tombstoned 房间即便行已被清空也安全丢弃", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now });
  const target = fakeWs({ role: "remote", scope: "refresh", connection_id: "target-conn" });
  ctx.acceptWebSocket(target, ["remote"]);
  store.upsertRefreshRequest(room.sql, baseRow({ deadline: now + 60_000 }));

  // 这次走真实 tombstoneRoom（会在同一事务里把 refresh_requests 行也清空）——
  // 与①的隔离测试不同，这里就是要证明：即便行已经不在了，先判 live 再读 DB
  // 的新顺序下也不会因为「先读 DB 拿到 null」而在成分不明确的路径上出错，
  // 而是干净地被 live 闸挡在最前面。
  store.tombstoneRoom(room.sql, now);
  assert.equal(store.isRoomLive(room.sql), false);
  assert.equal(store.getRefreshRequest(room.sql, "req-pred"), null, "tombstone 已把行清空");

  room.deliverRefreshReceipt({ t: "token.refresh.ok", request_id: "req-pred", subject: SUBJECT, generation: 5, ct: "x", n: "y" }, now);
  assert.equal(target.sent.length, 0);
});

test("R5.4 端到端负例：桌面从真实 webSocketMessage 重放陈旧代号回执——真实入口也丢弃（不再只直调 deliverRefreshReceipt）", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedRefreshSubject(room, { generation: 5, now }); // 建行时 subject 代=5
  const desktop = desktopSocket(room, ctx);
  const target = remoteRefreshSocket(room, ctx, { generation: 5, connectionId: "target-conn", now });
  store.upsertRefreshRequest(room.sql, {
    requestId: "req-stale-e2e", subject: SUBJECT, requestGeneration: 5,
    connectionId: "target-conn", deadline: now + 60_000,
  });

  seedRefreshSubject(room, { generation: 6, now }); // 轮换：subject 当前代变成 6，行里记的仍是陈旧代 5

  await room.webSocketMessage(desktop, JSON.stringify({
    t: "token.refresh.ok", request_id: "req-stale-e2e", subject: SUBJECT, generation: 5, ct: "stale-ct", n: "stale-n",
  }));

  assert.equal(target.sent.length, 0, "陈旧代号回执必须被丢弃——真实走 webSocketMessage 入口也不例外");
  assert.notEqual(store.getRefreshRequest(room.sql, "req-stale-e2e"), null, "④ 拒绝不删行，留给新代回执重投");
});
