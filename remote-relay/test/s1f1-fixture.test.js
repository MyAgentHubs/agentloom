import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";

const FIXTURES = JSON.parse(readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8"));
const TOKEN_FRAMES = FIXTURES.filter((item) => item.layer === "token-frame");
const SYNC_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.sync");
const SYNC_ACK_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.sync.ack");
const RESET_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.reset");
const SUBJECT = "device:11111111-1111-4111-8111-111111111111";
const OTHER_SUBJECT = "device:22222222-2222-4222-8222-222222222222";
// S1ja F2：专给「随便一个在线旁观 remote」用的 subject——跟上面两个代表真实
// 设备场景的 subject 分开，免得同一测试里既要 seedActive 一个设备又要给
// observer 挂 subject 时互相覆写同一行。
const OBSERVER_SUBJECT = "device:99999999-9999-4999-8999-999999999998";

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

function fakeWs(attachment = null, timeline = null, name = "ws") {
  let currentAttachment = attachment;
  return {
    sent: [], closed: [],
    // R1：deliverRefreshReceipt 现在核 readyState !== OPEN(1) 才投递——close()
    // 后置 CLOSED，让"已断=不再投递"这条不变量在这个 mock 上继续成立。
    readyState: 1, // WebSocket.OPEN
    send(text) {
      const frame = typeof text === "string" ? JSON.parse(text) : text;
      this.sent.push(frame);
      timeline?.push({ name, frame });
    },
    close(code, reason) {
      this.closed.push({ code, reason });
      this.readyState = 3; // WebSocket.CLOSED
      timeline?.push({ name, close: reason });
    },
    serializeAttachment(value) { currentAttachment = value; },
    deserializeAttachment() { return currentAttachment; },
  };
}

function desktopSocket(room, ctx, { ready = false, epoch, ...overrides } = {}) {
  const currentEpoch = epoch ?? store.bumpEpoch(room.sql);
  const ws = fakeWs({
    role: "desktop", scope: "desktop", epoch: currentEpoch,
    registry_ready: ready, registry_sync_deadline: Date.now() + 30_000,
    connectedAt: Date.now(), ...overrides,
  });
  ctx.acceptWebSocket(ws, ["desktop"]);
  return ws;
}

function seedActive(room, subject, generation, tokenHash) {
  const now = Date.now();
  return store.putTokenRegistryEntry(room.sql, {
    subject, generation, state: "active", scope: subject === "pairing" ? "pairing" : "remote",
    aliases: [{
      token_hash: tokenHash, kind: "current", generation,
      access_expires: now + 60_000, valid_until: now + 120_000,
    }],
  }, now, { cas: true });
}

async function send(room, ws, frameOrText) {
  const before = ws.sent.length;
  await room.webSocketMessage(ws, typeof frameOrText === "string" ? frameOrText : JSON.stringify(frameOrText));
  return ws.sent[before];
}

// S1ja F2：legacy 无 subject 的 remote 豁免已撤（canDeliverOutbound 现在
// fail-closed）——这几个测试用 fakeWs({role:"remote",scope:"remote"}) 当「随便
// 一个在线的旁观远端」，presence/replay.head/input.expired 等出站广播投递前
// 必过这道闸，现在需要真实 registry-backed subject 才能收到东西。复用
// seedActive 落库，再从它的返回值取回精确写入的 access_expires/valid_until
// 组进 attachment——不各自现取一次 Date.now()，避免跟 isOfficialAttachmentLive
// 的逐字段比对因几毫秒时钟漂移打架（本文件其它地方对 pairing 双连接已踩过
// 同款坑）。
function remoteObserverAttachment(room, subject, generation, tokenHash) {
  const seeded = seedActive(room, subject, generation, tokenHash);
  const alias = seeded.entry.aliases.find((item) => item.kind === "current");
  return {
    role: "remote",
    scope: "remote",
    subject,
    kind: "current",
    generation,
    alias_generation: alias.generation,
    access_expires: alias.access_expires,
    valid_until: alias.valid_until,
  };
}

test("S1f1 fixture：token.sync 4 条逐条走真实 webSocketMessage", async (t) => {
  assert.equal(SYNC_CASES.length, 4);
  for (const item of SYNC_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const desktop = desktopSocket(room, ctx);
      const actual = await send(room, desktop, item.frame);
      assert.equal(actual.t, item.expect.valid ? "token.sync.ack" : "error");
      if (item.expect.valid) {
        assert.equal(actual.revision, item.frame.revision);
        assert.equal(desktop.deserializeAttachment().registry_ready, true);
        assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_subjects")[0].n, item.frame.entries.length);
      } else {
        assert.deepEqual(actual.reason ? [actual.reason] : [], item.expect.errors);
        assert.equal(desktop.deserializeAttachment().registry_ready, false);
        if (actual.reason === "sync_entries_too_many") {
          assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "1");
        }
      }
    });
  }
});

test("S1f1 fixture：token.sync.ack 2 条由 high-water 真路径产出", async (t) => {
  assert.equal(SYNC_ACK_CASES.length, 2);
  const sync = SYNC_CASES.find((item) => item.name === "token_sync_two_entries_valid").frame;
  for (const item of SYNC_ACK_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      if (item.frame.relay_high_water > item.frame.revision) {
        seedActive(room, "device:33333333-3333-4333-8333-333333333333", item.frame.relay_high_water, "d".repeat(64));
      }
      const desktop = desktopSocket(room, ctx);
      assert.deepEqual(await send(room, desktop, sync), item.frame);
    });
  }
});

test("S1f1 fixture：token.reset 2 条消费，prev 原代号重建且 floor 计入 ack", async (t) => {
  assert.equal(RESET_CASES.length, 2);
  for (const item of RESET_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      seedActive(room, OTHER_SUBJECT, 150, "e".repeat(64));
      const desktop = desktopSocket(room, ctx);
      await send(room, desktop, { t: "token.sync", revision: 1, entries: [] });
      const actual = await send(room, desktop, item.frame);
      if (item.expect.valid) {
        assert.deepEqual(actual, { t: "token.sync.ack", revision: 200, relay_high_water: 200 });
        assert.equal(store.getRoomState(room.sql).registry_floor, 150);
        assert.equal(store.getTokenAlias(room.sql, SUBJECT, "prev").generation, 5);
        assert.equal(store.getTokenSubject(room.sql, OTHER_SUBJECT), null);
      } else {
        assert.equal(actual.reason, item.expect.errors[0]);
        assert.equal(store.getTokenSubject(room.sql, OTHER_SUBJECT).generation, 150);
        assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "1");
      }
    });
  }
});

test("sync 省略撤销、保留高代/revoked，并强断被撤销 subject", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  seedActive(room, SUBJECT, 5, "a".repeat(64));
  seedActive(room, OTHER_SUBJECT, 20, "b".repeat(64));
  store.putTokenRegistryEntry(room.sql, {
    subject: "device:33333333-3333-4333-8333-333333333333",
    generation: 4, state: "revoked", scope: null, aliases: [],
  }, Date.now(), { cas: true });
  const revokedSocket = fakeWs({ role: "remote", scope: "remote", subject: SUBJECT, generation: 5 });
  ctx.acceptWebSocket(revokedSocket, ["remote"]);
  const desktop = desktopSocket(room, ctx);
  await send(room, desktop, { t: "token.sync", revision: 10, entries: [] });
  const revoked = store.getTokenSubject(room.sql, SUBJECT);
  assert.equal(revoked.subject, SUBJECT);
  assert.equal(revoked.generation, 10);
  assert.equal(revoked.state, "revoked");
  assert.equal(revoked.scope, null);
  assert.equal(store.getTokenSubject(room.sql, OTHER_SUBJECT).state, "active");
  assert.equal(store.getTokenSubject(room.sql, "device:33333333-3333-4333-8333-333333333333").generation, 4);
  assert.equal(revokedSocket.sent.at(-1).reason, "device_revoked");
  assert.ok(revokedSocket.closed.length >= 1);
});

test("reset 逐条 CAS：低于新 floor 的条目被拒不回滚其它条目", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  seedActive(room, OTHER_SUBJECT, 50, "a".repeat(64));
  const desktop = desktopSocket(room, ctx);
  await send(room, desktop, { t: "token.sync", revision: 1, entries: [] });
  const now = Date.now();
  await send(room, desktop, {
    t: "token.reset", revision: 61,
    entries: [
      { subject: SUBJECT, generation: 50, scope: "remote", current: { token_hash: "b".repeat(64), access_expires: now + 60_000, refresh_until: now + 120_000 } },
      { subject: OTHER_SUBJECT, generation: 60, scope: "remote", current: { token_hash: "c".repeat(64), access_expires: now + 60_000, refresh_until: now + 120_000 } },
    ],
  });
  assert.equal(store.getTokenSubject(room.sql, SUBJECT), null);
  assert.equal(store.getTokenSubject(room.sql, OTHER_SUBJECT).generation, 60);
  assert.equal(store.getRoomState(room.sql).registry_floor, 50);
});

test("sync 非协议型 SQL 故障整帧回滚，保持单事务边界", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  room.sql.exec(`CREATE TRIGGER fail_second_sync_entry
    BEFORE INSERT ON token_subjects WHEN NEW.subject = '${OTHER_SUBJECT}'
    BEGIN SELECT RAISE(ABORT, 'storage failure'); END`);
  const now = Date.now();
  const desktop = desktopSocket(room, ctx);
  await assert.rejects(() => room.webSocketMessage(desktop, JSON.stringify({
    t: "token.sync", revision: 3,
    entries: [
      { subject: SUBJECT, generation: 1, scope: "remote", current: { token_hash: "a".repeat(64), access_expires: now + 60_000, refresh_until: now + 120_000 } },
      { subject: OTHER_SUBJECT, generation: 2, scope: "remote", current: { token_hash: "b".repeat(64), access_expires: now + 60_000, refresh_until: now + 120_000 } },
    ],
  })), /storage failure/);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_subjects")[0].n, 0);
});

test("registry_ready：sync 前不选中，ack 后按 replay.head → pending → presence 激活", async () => {
  const timeline = [];
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const epoch = store.bumpEpoch(room.sql);
  const desktop = fakeWs({ role: "desktop", scope: "desktop", epoch, registry_ready: false, registry_sync_deadline: Date.now() + 30_000 }, timeline, "desktop");
  ctx.acceptWebSocket(desktop, ["desktop"]);
  const observer = fakeWs(
    remoteObserverAttachment(room, OBSERVER_SUBJECT, 1, "e1".repeat(32)),
    timeline,
    "observer"
  );
  ctx.acceptWebSocket(observer, ["remote"]);
  room.sql.exec(
    "INSERT INTO pending_input (command_id, session, envelope, created_at, expires_at, subject, generation) VALUES (?, ?, ?, ?, ?, ?, ?)",
    "legacy", null, JSON.stringify({ kind: "input", command_id: "legacy" }), Date.now(), Date.now() + 60_000, null, null
  );
  assert.equal(room.onlineDesktop(), null);
  await send(room, desktop, { t: "token.sync", revision: 1, entries: [] });
  assert.equal(room.onlineDesktop(), desktop);
  const sentKinds = timeline.map((item) => item.frame?.t ?? item.frame?.kind).filter(Boolean);
  assert.deepEqual(sentKinds.slice(0, 4), ["token.sync.ack", "replay.head", "input.expired", "presence"]);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM pending_input")[0].n, 0);
});

test("epoch bump 后旧 ready desktop close 静默，presence 尾状态保持 online", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const oldDesktop = desktopSocket(room, ctx, { ready: true });
  const observer = fakeWs(remoteObserverAttachment(room, OBSERVER_SUBJECT, 1, "e2".repeat(32)));
  ctx.acceptWebSocket(observer, ["remote"]);
  const newDesktop = desktopSocket(room, ctx);

  await send(room, newDesktop, { t: "token.sync", revision: 1, entries: [] });
  await room.webSocketClose(oldDesktop);

  const desktopPresence = observer.sent.filter((frame) =>
    frame.t === "presence" && frame.role === "desktop"
  );
  assert.deepEqual(desktopPresence.map((frame) => frame.event), ["online"]);
  assert.equal(desktopPresence.at(-1).event, "online");
});

test("sync 撤销 pending 来源与存量 NULL 行：均删除并回 expired，不投递", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  seedActive(room, SUBJECT, 5, "a".repeat(64));
  // 这条测试稍后发 token.sync {revision:6, entries:[]}——§9.4「省略即撤销」
  // 会把任何 generation < 6 的存量 subject 顺手撤销掉；observer 的 subject
  // 跟设备 SUBJECT 无关，但同样会被这次 sync 扫到，generation 给够 10 才不会
  // 被这次 sync 误杀。
  const observer = fakeWs(remoteObserverAttachment(room, OBSERVER_SUBJECT, 10, "e3".repeat(32)));
  ctx.acceptWebSocket(observer, ["remote"]);
  const now = Date.now();
  store.enqueueInput(room.sql, { commandId: "revoked", envelopeJson: JSON.stringify({ kind: "input", command_id: "revoked" }), now, subject: SUBJECT, generation: 5 });
  room.sql.exec(
    "INSERT INTO pending_input (command_id, session, envelope, created_at, expires_at, subject, generation) VALUES (?, ?, ?, ?, ?, ?, ?)",
    "legacy", null, JSON.stringify({ kind: "input", command_id: "legacy" }), now + 1, now + 60_000, null, null
  );
  const desktop = desktopSocket(room, ctx);
  await send(room, desktop, { t: "token.sync", revision: 6, entries: [] });
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM pending_input")[0].n, 0);
  assert.deepEqual(observer.sent.filter((frame) => frame.t === "input.expired").map((frame) => frame.command_id), ["revoked", "legacy"]);
  assert.equal(desktop.sent.some((frame) => frame.kind === "input"), false);
});

test("sync 首帧闸：reset/delete/refresh.ok 拒绝，sync 后 reset 与二次 sync 放行", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const desktop = desktopSocket(room, ctx);
  const resetReject = await send(room, desktop, { t: "token.reset", revision: 1, entries: [] });
  assert.deepEqual(resetReject, { t: "error", reason: "sync_required", frame: "token.reset" });
  const deleteReject = await send(room, desktop, {
    t: "token.delete", subject: SUBJECT, generation: 1,
  });
  assert.equal(deleteReject.t, "token.ack");
  assert.equal(deleteReject.reason, "sync_required");
  const refreshReject = await send(room, desktop, { t: "token.refresh.ok", subject: SUBJECT });
  assert.deepEqual(refreshReject, { t: "error", reason: "sync_required", frame: "token.refresh.ok" });
  assert.equal(store.getTokenSubject(room.sql, SUBJECT), null);

  assert.deepEqual(await send(room, desktop, { t: "token.sync", revision: 2, entries: [] }), {
    t: "token.sync.ack", revision: 2, relay_high_water: 2,
  });
  assert.deepEqual(await send(room, desktop, { t: "token.reset", revision: 3, entries: [] }), {
    t: "token.sync.ack", revision: 3, relay_high_water: 3,
  });
  assert.deepEqual(await send(room, desktop, { t: "token.sync", revision: 4, entries: [] }), {
    t: "token.sync.ack", revision: 4, relay_high_water: 4,
  });
});

test("缺 registry_ready 的旧 desktop attachment 仍受首帧闸约束", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const epoch = store.bumpEpoch(room.sql);
  const legacyDesktop = fakeWs({ role: "desktop", scope: "desktop", epoch });
  ctx.acceptWebSocket(legacyDesktop, ["desktop"]);

  assert.deepEqual(await send(room, legacyDesktop, {
    t: "token.reset", revision: 1, entries: [],
  }), { t: "error", reason: "sync_required", frame: "token.reset" });
  assert.equal(legacyDesktop.deserializeAttachment().registry_ready, undefined);
});

test("连续小 revision reset：floor 主导 high-water、只升不降并清空旧 fingerprints", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  seedActive(room, OTHER_SUBJECT, 300, "d".repeat(64));
  const desktop = desktopSocket(room, ctx);
  await send(room, desktop, { t: "token.sync", revision: 1, entries: [] });
  const now = Date.now();

  assert.deepEqual(await send(room, desktop, {
    t: "token.reset", revision: 10,
    entries: [{
      subject: SUBJECT, generation: 301, scope: "remote",
      current: {
        token_hash: "e".repeat(64), access_expires: now + 60_000,
        refresh_until: now + 120_000,
      },
    }],
  }), { t: "token.sync.ack", revision: 10, relay_high_water: 301 });
  assert.equal(store.getRoomState(room.sql).registry_floor, 300);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_put_fingerprints")[0].n, 1);

  assert.deepEqual(await send(room, desktop, {
    t: "token.reset", revision: 20, entries: [],
  }), { t: "token.sync.ack", revision: 20, relay_high_water: 301 });
  assert.equal(store.getRoomState(room.sql).registry_floor, 301);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_subjects")[0].n, 0);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_put_fingerprints")[0].n, 0);
});

test("明文 epoch 闸：旧 desktop input.ack error+close，pending 行不删", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const oldDesktop = desktopSocket(room, ctx, { ready: true });
  desktopSocket(room, ctx, { ready: true });
  store.enqueueInput(room.sql, { commandId: "keep", envelopeJson: "{}", now: Date.now(), subject: SUBJECT, generation: 1 });
  await send(room, oldDesktop, { t: "input.ack", command_id: "keep", outcome: "ok" });
  assert.equal(oldDesktop.sent.at(-1).reason, "stale_epoch");
  assert.equal(oldDesktop.closed.length, 1);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM pending_input WHERE command_id = 'keep'")[0].n, 1);
});

test("pair.hello 只路由到 current epoch 且 registry_ready 的桌面", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const stale = desktopSocket(room, ctx, { ready: true });
  const currentUnready = desktopSocket(room, ctx, { ready: false });
  const now = Date.now();
  seedActive(room, "pairing", 1, "9".repeat(64));
  const alias = store.getTokenAlias(room.sql, "pairing", "current");
  // S1i3 K3.6 订正：这里手搭连接要带 connection_id 是因为 relay 转发 pair.hello 前要
  // upsert pairing_routes（connection_id 列 NOT NULL），生产连接在 fetch() 握手时必带
  // 这个字段。但不是「第一次 pair.hello 就会抛错」——下面第一次 pair.hello 走的是
  // desktop_offline 早返回（两个桌面都不满足 current+ready），根本没跑到
  // setPairingRoute；真正需要这个字段撑住 SQL 绑定的是下面 `attempt: 2` 那次重放
  // （currentUnready 补上 registry_ready 后，desktop 在线，才会走到 upsert）。
  // 别再写死行号指路——行号会随后续插入测试漂移，陈旧的行号引用比没有引用更坏。
  const pairing = fakeWs({ role: "remote", scope: "pairing", subject: "pairing", kind: "current", generation: 1, alias_generation: 1, access_expires: alias.access_expires, valid_until: alias.valid_until, connection_id: "pairing-conn" });
  ctx.acceptWebSocket(pairing, ["remote"]);
  await send(room, pairing, { t: "pair.hello" });
  assert.equal(pairing.sent.at(-1).reason, "desktop_offline");
  assert.equal(stale.sent.length, 0);
  assert.equal(currentUnready.sent.length, 0);
  currentUnready.serializeAttachment({ ...currentUnready.deserializeAttachment(), registry_ready: true });
  await send(room, pairing, { t: "pair.hello", attempt: 2 });
  assert.equal(currentUnready.sent.at(-1).t, "pair.hello");
});

test("同步超时走既有 alarm：只关闭未 ready desktop，不影响远端注册表", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  seedActive(room, SUBJECT, 1, "a".repeat(64));
  const desktop = desktopSocket(room, ctx, { registry_sync_deadline: Date.now() - 1 });
  const remote = fakeWs({ role: "remote", scope: "remote" });
  ctx.acceptWebSocket(remote, ["remote"]);
  await room.alarm();
  assert.equal(desktop.sent.at(-1).reason, "sync_timeout");
  assert.equal(desktop.closed.length, 1);
  assert.equal(remote.closed.length, 0);
  assert.equal(store.getTokenSubject(room.sql, SUBJECT).state, "active");
});

test("解析硬上限：63KB 过；65KB（含非法 JSON）在 parse 前 error+close", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  // S1ja F2：63KB 分支要走到「presence 帧正常被受理」才能证明帧字节预算没
  // 提前拦下它——subject-less 的 legacy remote 现在会先在 authorizeInboundSocket
  // 被 fail-closed 关掉，跟本测试真正要测的「大小边界」混在一起，误判成
  // under.closed.length 非零。65KB 分支不受影响（frame_too_large 检查发生在
  // authorizeInboundSocket 之前），继续用裸 attachment。
  const under = fakeWs(remoteObserverAttachment(room, OBSERVER_SUBJECT, 1, "e4".repeat(32)));
  ctx.acceptWebSocket(under, ["remote"]);
  const underFrame = JSON.stringify({ t: "presence", pad: "x".repeat(63 * 1024 - 30) });
  assert.ok(Buffer.byteLength(underFrame) < 64 * 1024);
  await send(room, under, underFrame);
  assert.equal(under.closed.length, 0);
  assert.equal(under.sent.some((frame) => frame.reason === "frame_too_large"), false);
  let violationCount = 0;
  for (const oversized of [
    JSON.stringify({ t: "presence", pad: "x".repeat(65 * 1024) }),
    "{" + "x".repeat(65 * 1024),
  ]) {
    const ws = fakeWs({ role: "remote", scope: "remote" });
    await send(room, ws, oversized);
    assert.equal(ws.sent.at(-1).reason, "frame_too_large");
    assert.equal(ws.closed.length, 1);
    assert.notEqual(ws.sent.at(-1).reason, "bad_json");
    await room.webSocketClose(ws);
    violationCount += 1;
    assert.equal(Number(store.getMeta(room.sql, "protocol_violation_count")), violationCount);
  }
});

test("P2-7：ttl-clamp 六条兼容后启用 unix 秒口径下界拒绝", () => {
  assert.throws(
    () => store.clampTokenExpiry("access", 1_765_430_400, 1_765_430_400_000),
    /unix milliseconds/
  );
});
