import { createHash } from "node:crypto";
import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import worker, { ipBucketKey } from "../src/index.js";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";

const ROOM = "abcdef0123456789abcdef0123456789";
const SUBJECT = "device:44444444-4444-4444-8444-444444444444";

function sha256(value) {
  return createHash("sha256").update(value, "ascii").digest("hex");
}

function fakeWs(attachment = null) {
  let currentAttachment = attachment;
  return {
    sent: [],
    closed: [],
    send(text) {
      this.sent.push(typeof text === "string" ? JSON.parse(text) : text);
    },
    close(code, reason) {
      this.closed.push({ code, reason });
    },
    serializeAttachment(value) {
      currentAttachment = value;
    },
    deserializeAttachment() {
      return currentAttachment;
    },
  };
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

function seedRemoteToken(room, token, generation = 1) {
  const now = Date.now();
  store.putTokenRegistryEntry(room.sql, {
    subject: SUBJECT,
    generation,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: sha256(token),
      kind: "current",
      generation,
      access_expires: now + 60_000,
      valid_until: now + 120_000,
    }],
  }, now);
}

function upgradeRequest(token, ip) {
  return new Request(`https://relay.example/room/${ROOM}`, {
    headers: {
      Upgrade: "websocket",
      "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${token}`,
      "CF-Connecting-IP": ip,
    },
  });
}

test("claim edge per-IP 10/min：前十次进 stub，第十一次 429；非法 room 不取 stub", async () => {
  let idCalls = 0;
  let fetchCalls = 0;
  const env = {
    ROOMS: {
      idFromName(name) {
        idCalls += 1;
        return name;
      },
      get() {
        return { fetch() { fetchCalls += 1; return new Response("ok", { status: 200 }); } };
      },
    },
  };
  const bad = await worker.fetch(new Request("https://relay.example/room/not-a-room/claim", { method: "POST" }), env);
  assert.equal(bad.status, 404);
  assert.equal(idCalls, 0);

  const ip = "198.51.100.210";
  for (let attempt = 1; attempt <= 10; attempt += 1) {
    const response = await worker.fetch(new Request(`https://relay.example/room/${ROOM}/claim`, {
      method: "POST",
      headers: { "CF-Connecting-IP": ip },
    }), env);
    assert.equal(response.status, 200, `attempt ${attempt}`);
  }
  const limited = await worker.fetch(new Request(`https://relay.example/room/${ROOM}/claim`, {
    method: "POST",
    headers: { "CF-Connecting-IP": ip },
  }), env);
  assert.equal(limited.status, 429);
  assert.equal(idCalls, 10);
  assert.equal(fetchCalls, 10);
});

test("edge 与 DO 的 ip_bucket_key 均加盐：非裸哈希、同层同 IP 稳定且房间盐持久", async () => {
  const ip = "198.51.100.219";
  const rawPrefix = sha256(ip).slice(0, 32);
  const request = new Request(`https://relay.example/room/${ROOM}/claim`, {
    method: "POST",
    headers: { "CF-Connecting-IP": ip },
  });
  const edgeFirst = await ipBucketKey(request);
  assert.equal(await ipBucketKey(request), edgeFirst);
  assert.notEqual(edgeFirst, rawPrefix);

  const { ctx } = makeRuntime();
  let room = new RoomDO(ctx, {});
  const doFirst = await room.deriveIpBucketKey(ip);
  assert.equal(await room.deriveIpBucketKey(ip), doFirst);
  assert.notEqual(doFirst, rawPrefix);
  // SEC-1：盐从 room_meta 迁到 room_state（哨兵表，鉴权前也恒存在）。
  assert.match(store.getRoomIpBucketSalt(room.sql), /^[0-9a-f]{64}$/);

  await withUpgradeRuntime(async () => {
    const token = "6".repeat(64);
    // SEC-1：seedRemoteToken 直接落业务表，构造器不再无条件建——手动建好，
    // 不影响本条要测的东西（IP 盐派生与持久性）。
    store.ensureBusinessSchema(room.sql);
    seedRemoteToken(room, token);
    assert.equal((await room.fetch(upgradeRequest(token, ip))).status, 101);
    assert.equal(ctx.getWebSockets("remote").at(-1).deserializeAttachment().ip_bucket_key, doFirst);
  });

  room = new RoomDO(ctx, {});
  assert.equal(await room.deriveIpBucketKey(ip), doFirst, "room salt must survive hibernation/reconstruction");
});

test("SEC-1④：deriveIpBucketKey 在裸房（业务 schema 未建）可用，盐落 room_state 且跨重建稳定，全程不建业务表", async () => {
  const ip = "198.51.100.220";
  const { ctx } = makeRuntime();
  let room = new RoomDO(ctx, {});

  const tableNamesBefore = room.sql
    .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
    .map((row) => row.name);
  assert.deepEqual(tableNamesBefore, ["room_state"]);

  const first = await room.deriveIpBucketKey(ip);
  assert.match(first, /^[0-9a-f]{32}$/);
  assert.match(store.getRoomIpBucketSalt(room.sql), /^[0-9a-f]{64}$/);

  // 派生盐这一步本身不该连带建出任何业务表——只碰 room_state。
  const tableNamesAfter = room.sql
    .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
    .map((row) => row.name);
  assert.deepEqual(tableNamesAfter, ["room_state"]);

  room = new RoomDO(ctx, {}); // 模拟休眠重建，同一份 storage
  assert.equal(await room.deriveIpBucketKey(ip), first, "裸房盐必须跨重建持久，不需要业务 schema 已建");
});

// S1ja F2：legacy 无 subject 的 remote 豁免已撤——本测试要打的是「§9.1 帧型
// 矩阵违例」的协议违例计数（role_forbidden），不是鉴权闸本身；不给 socket 挂
// 真实 registry-backed subject，authorizeInboundSocket 会先用
// reauthorization_failed 把它拦在矩阵检查之前，这份测试从第一条断言就会看错
// reason。三个 socket 各领一个独立 subject，避免跟无关的并发 socket 数配额
// 撞在一起。
function seedRemoteAttachment(room, subject) {
  const now = Date.now();
  store.putTokenRegistryEntry(room.sql, {
    subject,
    generation: 1,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: sha256(subject),
      kind: "current",
      generation: 1,
      access_expires: now + 60_000,
      valid_until: now + 120_000,
    }],
  }, now);
  return {
    role: "remote",
    scope: "remote",
    subject,
    kind: "current",
    generation: 1,
    alias_generation: 1,
    access_expires: now + 60_000,
    valid_until: now + 120_000,
    epoch: 0,
  };
}

test("矩阵违例按 socket 累计：第 8 次 error 后 close，聚合计数仅按批/close/alarm 落盘", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  // SEC-1：构造器不再无条件建业务 schema——下面直接调 seedRemoteAttachment/
  // getMeta(room_meta) 需要业务表已在，手动建好。
  store.ensureBusinessSchema(room.sql);
  const originalExec = room.sql.exec.bind(room.sql);
  let violationWrites = 0;
  room.sql.exec = (query, ...params) => {
    if (/^INSERT INTO room_meta/.test(query) && params[0] === "protocol_violation_count") {
      violationWrites += 1;
    }
    return originalExec(query, ...params);
  };

  const remote = fakeWs(seedRemoteAttachment(room, "device:55555555-5555-4555-8555-555555555551"));
  ctx.acceptWebSocket(remote, ["remote"]);
  for (let attempt = 1; attempt <= 7; attempt += 1) {
    await room.webSocketMessage(remote, JSON.stringify({ t: `unlisted.${attempt}` }));
    assert.equal(remote.sent.at(-1).reason, "role_forbidden");
    assert.equal(remote.closed.length, 0);
  }
  assert.equal(violationWrites, 0);
  assert.equal(store.getMeta(room.sql, "protocol_violation_count", "0"), "0");

  await room.webSocketMessage(remote, JSON.stringify({ t: "unlisted.8" }));
  assert.equal(remote.sent.at(-1).reason, "role_forbidden");
  assert.equal(remote.closed.length, 1);
  assert.equal(violationWrites, 1, "threshold close must not double-flush the same batch");
  assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "8");

  const closeFlush = fakeWs(seedRemoteAttachment(room, "device:55555555-5555-4555-8555-555555555552"));
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    await room.webSocketMessage(closeFlush, JSON.stringify({ t: `close-flush.${attempt}` }));
  }
  assert.equal(violationWrites, 1);
  await room.webSocketClose(closeFlush);
  assert.equal(violationWrites, 2);
  assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "11");

  const alarmFlush = fakeWs(seedRemoteAttachment(room, "device:55555555-5555-4555-8555-555555555553"));
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    await room.webSocketMessage(alarmFlush, JSON.stringify({ t: `alarm-flush.${attempt}` }));
  }
  assert.equal(violationWrites, 2);
  await room.alarm();
  assert.equal(violationWrites, 3);
  assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "13");
});

test("upgrade 失败细桶 60/min：前 60 次 401，第 61 次 429", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  const token = "a".repeat(64);
  for (let attempt = 1; attempt <= 60; attempt += 1) {
    assert.equal((await room.fetch(upgradeRequest(token, "198.51.100.211"))).status, 401, `attempt ${attempt}`);
  }
  assert.equal((await room.fetch(upgradeRequest(token, "198.51.100.211"))).status, 429);
  for (let attempt = 62; attempt <= 120; attempt += 1) {
    assert.equal((await room.fetch(upgradeRequest(token, "198.51.100.211"))).status, 429);
  }
  assert.equal(
    (await room.fetch(upgradeRequest("e".repeat(64), "198.51.100.211"))).status,
    429,
    "fine-boxed failures must continue feeding the coarse bucket"
  );
});

test("upgrade 失败粗桶 120/min：不同哈希前 120 次 401，第 121 次 429", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  for (let attempt = 1; attempt <= 120; attempt += 1) {
    const token = attempt.toString(16).padStart(64, "0");
    assert.equal((await room.fetch(upgradeRequest(token, "198.51.100.212"))).status, 401, `attempt ${attempt}`);
  }
  assert.equal((await room.fetch(upgradeRequest("f".repeat(64), "198.51.100.212"))).status, 429);
});

test("罚箱不误伤随后已认证成功的 upgrade 与既有 socket 消息", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx } = makeRuntime();
    const room = new RoomDO(ctx, {});
    // SEC-1：seedRemoteToken 直接落业务表，构造器不再无条件建——手动建好；
    // 不影响本条要测的东西（罚箱/限速行为），60 次失败 401 本就不该建表，这里
    // 只是为了后面 seedRemoteToken + 成功握手做准备。
    store.ensureBusinessSchema(room.sql);
    const token = "b".repeat(64);
    for (let attempt = 1; attempt <= 60; attempt += 1) {
      assert.equal((await room.fetch(upgradeRequest(token, "198.51.100.213"))).status, 401);
    }
    seedRemoteToken(room, token);
    const accepted = await room.fetch(upgradeRequest(token, "198.51.100.213"));
    assert.equal(accepted.status, 101);
    const ws = ctx.getWebSockets("remote")[0];
    await room.webSocketMessage(ws, JSON.stringify({ t: "presence" }));
    assert.equal(ws.closed.length, 0);
    assert.equal(ws.sent.some((frame) => frame.reason === "rate_limited"), false);
  });
});

test("pair.hello per-socket 6/min：前六次转发，第七次 error + close", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  // SEC-1：构造器不再无条件建业务 schema——bumpEpoch/putTokenRegistryEntry
  // 需要业务表已在，手动建好。
  store.ensureBusinessSchema(room.sql);
  const epoch = store.bumpEpoch(room.sql);
  const desktop = fakeWs({ role: "desktop", scope: "desktop", epoch, registry_ready: true });
  ctx.acceptWebSocket(desktop, ["desktop"]);
  const now = Date.now();
  store.putTokenRegistryEntry(room.sql, {
    subject: "pairing",
    generation: 1,
    state: "active",
    scope: "pairing",
    aliases: [{ token_hash: "9".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 60_000 }],
  }, now);
  const pairing = fakeWs({
    role: "remote",
    scope: "pairing",
    subject: "pairing",
    kind: "current",
    generation: 1,
    alias_generation: 1,
    access_expires: now + 60_000,
    valid_until: now + 60_000,
    ip_bucket_key: "pair-ip",
    // S1i3 F1：relay 转发 pair.hello 前要 upsert pairing_routes（connection_id 列
    // NOT NULL）——生产连接在 fetch() 握手时必带这个字段，测试手搭的连接也要带，
    // 否则第一次 pair.hello 就会在 SQL 绑定上直接抛错（undefined 不能绑 TEXT NOT NULL）。
    connection_id: "pairing-conn",
  });
  ctx.acceptWebSocket(pairing, ["remote"]);

  for (let attempt = 1; attempt <= 6; attempt += 1) {
    await room.webSocketMessage(pairing, JSON.stringify({ t: "pair.hello", attempt }));
    assert.equal(desktop.sent.length, attempt);
    assert.equal(pairing.closed.length, 0);
  }
  await room.webSocketMessage(pairing, JSON.stringify({ t: "pair.hello", attempt: 7 }));
  assert.equal(desktop.sent.length, 6);
  assert.equal(pairing.sent.at(-1).reason, "pair_hello_rate_limited");
  assert.equal(pairing.closed.length, 1);
  assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "1");
});

test("并发同 subject 第五条连接踢最旧，legacy socket 不计桶", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx } = makeRuntime();
    const room = new RoomDO(ctx, {});
    // SEC-1：seedRemoteToken 直接落业务表，构造器不再无条件建——手动建好。
    store.ensureBusinessSchema(room.sql);
    const token = "c".repeat(64);
    seedRemoteToken(room, token);
    const legacy = fakeWs({ role: "remote", scope: "remote", subject: null, connectedAt: 0 });
    ctx.acceptWebSocket(legacy, ["remote"]);

    const accepted = [];
    for (let attempt = 1; attempt <= 5; attempt += 1) {
      const response = await room.fetch(upgradeRequest(token, `203.0.113.${attempt}`));
      assert.equal(response.status, 101);
      accepted.push(ctx.getWebSockets("remote").at(-1));
    }
    assert.equal(accepted[0].sent.at(-1).reason, "subject_socket_limit");
    assert.equal(accepted[0].closed.length, 1);
    assert.equal(accepted.slice(1).every((ws) => ws.closed.length === 0), true);
    assert.equal(legacy.closed.length, 0);
  });
});
