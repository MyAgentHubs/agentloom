import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";
import { ABANDONED_ROOM_RECLAIM_MS, LAST_ACTIVITY_KEY } from "../src/room-abandon.js";

// Reclaim of rooms that never authenticated (no room_meta), claimed or not.
// Minimal ctx stand-in: real node:sqlite plus an alarm/socket recorder.

const ROOM = "b".repeat(32);
const UNCLAIMED_RECLAIM_MS = 20 * 60 * 1000;

function makeCtx() {
  const db = new DatabaseSync(":memory:");
  const sql = {
    exec(query, ...params) {
      const stmt = db.prepare(query);
      if (/^\s*(SELECT|PRAGMA)/i.test(query)) return stmt.all(...params);
      stmt.run(...params);
      return [];
    },
  };
  const alarmLog = { scheduled: null, deleted: false, deleteAllCalled: false };
  const sockets = [];
  const ctx = {
    storage: {
      sql,
      transactionSync: (callback) => callback(),
      async setAlarm(timestamp) {
        alarmLog.scheduled = timestamp;
      },
      async getAlarm() {
        return alarmLog.scheduled;
      },
      async deleteAlarm() {
        alarmLog.deleted = true;
        alarmLog.scheduled = null;
      },
      async deleteAll() {
        const tables = db
          .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
          .all();
        for (const { name } of tables) db.exec(`DROP TABLE "${name}"`);
        alarmLog.deleteAllCalled = true;
      },
    },
    alarmLog,
    acceptWebSocket: (ws) => sockets.push(ws),
    getWebSockets: () => sockets,
    disconnectWebSocket(ws) {
      const index = sockets.indexOf(ws);
      if (index !== -1) sockets.splice(index, 1);
    },
  };
  return ctx;
}

// Never-authenticated room: constructor creates only the room_state sentinel.
function makeSchemaLessRoom() {
  const ctx = makeCtx();
  return { room: new RoomDO(ctx, {}), ctx };
}

function expireRoom(room) {
  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);
}

function claimRequest(hash) {
  return new Request(`https://relay.example/room/${ROOM}/claim`, {
    method: "POST",
    body: JSON.stringify({ v: 1, credential_hash: hash }),
  });
}

test("R8：已 claim 未鉴权的房未到回收时限 alarm() 不回收，也不建出业务 schema", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  assert.equal(store.claimRoom(room.sql, "e".repeat(64), Date.now()), "claimed");

  await room.alarm();

  assert.equal(store.getRoomState(room.sql).owner_credential_hash, "e".repeat(64));
  assert.equal(ctx.alarmLog.deleteAllCalled, false);
  assert.equal(store.hasTable(room.sql, "room_meta"), false, "alarm() must not create the business schema");
});

test("R8：已鉴权过的房（有 room_meta）即使 created_at 早已过回收时限也不被回收", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  store.ensureBusinessSchema(room.sql);
  assert.equal(store.claimRoom(room.sql, "d".repeat(64), Date.now()), "claimed");
  expireRoom(room);

  await room.alarm();

  assert.equal(ctx.alarmLog.deleteAllCalled, false);
  assert.equal(store.getRoomState(room.sql).owner_credential_hash, "d".repeat(64));
});

test("R8：有活连接的未鉴权房到点 alarm() 不被回收", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  assert.equal(store.claimRoom(room.sql, "e".repeat(64), Date.now()), "claimed");
  expireRoom(room);
  ctx.acceptWebSocket({ serializeAttachment() {}, deserializeAttachment: () => ({}), send() {}, close() {} });

  await room.alarm();

  assert.equal(ctx.alarmLog.deleteAllCalled, false, "room with a live socket must not be reclaimed");
  assert.equal(store.getRoomState(room.sql).owner_credential_hash, "e".repeat(64));
});

test("R8：claim 成功（含同 hash 幂等重放）后未鉴权房的回收 alarm 排在 created_at + 20min", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  const createdAt = store.getRoomState(room.sql).created_at;

  const first = await room.fetch(claimRequest("f".repeat(64)));
  assert.equal(first.status, 200);
  assert.equal(ctx.alarmLog.scheduled, createdAt + UNCLAIMED_RECLAIM_MS);

  // Owner is non-null now, so the pre-body arming is skipped; the post-claim
  // arming must still re-establish the reclaim alarm.
  await ctx.storage.deleteAlarm();
  assert.equal(ctx.alarmLog.scheduled, null);
  const second = await room.fetch(claimRequest("f".repeat(64)));
  assert.equal(second.status, 200);
  assert.equal(ctx.alarmLog.scheduled, createdAt + UNCLAIMED_RECLAIM_MS);
});

async function reclaim(room, ctx) {
  expireRoom(room);
  await room.alarm();
  assert.equal(ctx.alarmLog.deleteAllCalled, true, "precondition: room was wiped by deleteAll");
}

test("R8 hardening：清库后同一内存实例再收到 claim 返回 200（重建哨兵），不抛 no such table", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  assert.equal(store.claimRoom(room.sql, "e".repeat(64), Date.now()), "claimed");
  await reclaim(room, ctx);

  const res = await room.fetch(claimRequest("c".repeat(64)));
  assert.equal(res.status, 200);
  assert.equal(store.getRoomState(room.sql).owner_credential_hash, "c".repeat(64));
});

test("R8 hardening：清库后同一内存实例再收到无凭据 WS upgrade 返回 401，再跑 alarm() 也不抛错", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  await reclaim(room, ctx);

  const res = await room.fetch(
    new Request(`https://relay.example/room/${ROOM}`, { headers: { Upgrade: "websocket" } })
  );
  assert.equal(res.status, 401);

  ctx.alarmLog.deleteAllCalled = false;
  await room.alarm();
  assert.equal(ctx.alarmLog.deleteAllCalled, false, "fresh sentinel is not yet due");
});

test("R8 hardening：未到期的未鉴权房 alarm() 触发后重排回收 alarm（差 50ms 到期），不紧密重排", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  assert.equal(store.claimRoom(room.sql, "e".repeat(64), Date.now()), "claimed");
  const createdAt = Date.now() - UNCLAIMED_RECLAIM_MS + 50;
  room.sql.exec("UPDATE room_state SET created_at = ?", createdAt);
  ctx.alarmLog.scheduled = null; // the fired alarm is consumed by the runtime

  await room.alarm();

  assert.equal(ctx.alarmLog.deleteAllCalled, false);
  assert.equal(ctx.alarmLog.scheduled, createdAt + UNCLAIMED_RECLAIM_MS, "reclaim alarm is re-armed at the deadline");
});

test("R8 hardening：过期但有活连接的未鉴权房 alarm() 不重排（避免过去时刻 alarm 紧密循环）", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  expireRoom(room);
  ctx.acceptWebSocket({ serializeAttachment() {}, deserializeAttachment: () => ({}), send() {}, close() {} });
  ctx.alarmLog.scheduled = null;

  await room.alarm();

  assert.equal(ctx.alarmLog.scheduled, null, "must not re-arm an alarm in the past");
});

// ---- Abandoned-but-authenticated rooms ----

const CREDENTIAL = "a".repeat(64);
const CREDENTIAL_HASH = createHash("sha256").update(CREDENTIAL).digest("hex");

function fakeWs() {
  let stored = null;
  return {
    send() {},
    close() {},
    serializeAttachment(value) {
      stored = structuredClone(value);
    },
    deserializeAttachment: () => stored,
  };
}

// Workers globals are absent under Node: stub WebSocketPair and a Response that
// accepts status 101.
async function withUpgradeRuntime(callback) {
  const NativeResponse = globalThis.Response;
  const NativeWebSocketPair = globalThis.WebSocketPair;
  globalThis.Response = class TestResponse {
    constructor(body, init = {}) {
      this.body = body;
      this.status = init.status ?? 200;
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

// The attack: unauthenticated claim of an arbitrary hash, then one desktop
// connection with the matching credential (creates the business schema).
async function claimAndConnectOnce(room, ctx) {
  const claimRes = await room.fetch(claimRequest(CREDENTIAL_HASH));
  assert.equal(claimRes.status, 200);
  const res = await withUpgradeRuntime(() =>
    room.fetch(
      new Request(`https://relay.example/room/${ROOM}`, {
        headers: { Upgrade: "websocket", Authorization: `Bearer ${CREDENTIAL}` },
      })
    )
  );
  assert.equal(res.status, 101);
  assert.equal(store.hasTable(room.sql, "room_meta"), true);
  const ws = ctx.getWebSockets()[0];
  return ws;
}

async function abandonedRoom() {
  const { room, ctx } = makeSchemaLessRoom();
  const ws = await claimAndConnectOnce(room, ctx);
  ctx.disconnectWebSocket(ws);
  await room.webSocketClose(ws);
  return { room, ctx, ws };
}

function setIdleFor(room, idleMs) {
  store.setMeta(room.sql, LAST_ACTIVITY_KEY, Date.now() - idleMs);
}

test("R8 abandoned：攻击者 claim + 带自己凭据连一次，断开后空闲满 7 天，alarm() 整库清空", async () => {
  const { room, ctx } = await abandonedRoom();
  const touched = Number(store.getMeta(room.sql, LAST_ACTIVITY_KEY));
  assert.ok(Math.abs(touched - Date.now()) < 5_000, "connect/close records last activity");

  await room.alarm();
  assert.equal(ctx.alarmLog.deleteAllCalled, false, "recent activity: not reclaimed");

  setIdleFor(room, ABANDONED_ROOM_RECLAIM_MS + 1);
  await room.alarm();
  assert.equal(ctx.alarmLog.deleteAllCalled, true);
  assert.deepEqual(room.sql.exec("SELECT name FROM sqlite_master WHERE type = 'table'"), []);
});

test("R8 abandoned：空闲不足 7 天不回收，且回收 alarm 排在 last_activity + 7d", async () => {
  const { room, ctx } = await abandonedRoom();
  setIdleFor(room, ABANDONED_ROOM_RECLAIM_MS - 60_000);
  const lastActivity = Number(store.getMeta(room.sql, LAST_ACTIVITY_KEY));
  ctx.alarmLog.scheduled = null;

  await room.alarm();

  assert.equal(ctx.alarmLog.deleteAllCalled, false);
  assert.equal(ctx.alarmLog.scheduled, lastActivity + ABANDONED_ROOM_RECLAIM_MS);
});

test("R8 abandoned：有活连接的房即使空闲已满 7 天也不回收、也不重排（避免过去时刻循环）", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  await claimAndConnectOnce(room, ctx);
  setIdleFor(room, ABANDONED_ROOM_RECLAIM_MS + 1);
  ctx.alarmLog.scheduled = null;

  await room.alarm();

  assert.equal(ctx.alarmLog.deleteAllCalled, false);
  assert.ok(ctx.alarmLog.scheduled == null || ctx.alarmLog.scheduled > Date.now(), "never a past alarm");
});

const VALUABLE_DATA = {
  "token_subjects (paired phone)":
    "INSERT INTO token_subjects (subject, generation, state, scope) VALUES ('device:x', 1, 'active', 'remote')",
  "token_subjects (revoked phone)":
    "INSERT INTO token_subjects (subject, generation, state, scope) VALUES ('device:x', 1, 'revoked', NULL)",
  "token_aliases":
    "INSERT INTO token_aliases (token_hash, subject, kind, generation, access_expires, valid_until) VALUES ('h', 's', 'current', 1, NULL, 1)",
  "token_put_fingerprints":
    `INSERT INTO token_put_fingerprints (subject, generation, fingerprint) VALUES ('s', 1, '${"f".repeat(64)}')`,
  "events":
    "INSERT INTO events (seq, epoch, kind, ct, n, ts) VALUES (1, 1, 'event', 'c', 'n', 1)",
  "pairing_routes": "INSERT INTO pairing_routes (subject, connection_id) VALUES ('pairing', 'c')",
  "pending_input (unexpired)":
    `INSERT INTO pending_input (command_id, envelope, created_at, expires_at) VALUES ('c', '{}', 1, ${Date.now() + 3_600_000})`,
  "refresh_requests (unexpired)":
    `INSERT INTO refresh_requests (request_id, subject, request_generation, connection_id, deadline) VALUES ('r', 's', 1, 'c', ${Date.now() + 3_600_000})`,
  "reply_routes (unexpired)":
    `INSERT INTO reply_routes (command_id, subject, connection_id, deadline) VALUES ('c', 's', 'c', ${Date.now() + 3_600_000})`,
};

for (const [name, insert] of Object.entries(VALUABLE_DATA)) {
  test(`R8 abandoned：房里还有 ${name} 时，空闲满 7 天也不回收`, async () => {
    const { room, ctx } = await abandonedRoom();
    room.sql.exec(insert);
    setIdleFor(room, ABANDONED_ROOM_RECLAIM_MS + 1);

    await room.alarm();

    assert.equal(ctx.alarmLog.deleteAllCalled, false);
    assert.equal(store.hasTable(room.sql, "room_meta"), true);
  });
}

test("R8 abandoned：已过期的 pending_input / refresh_requests / reply_routes 不算有价值数据，空闲满 7 天照常回收", async () => {
  const { room, ctx } = await abandonedRoom();
  room.sql.exec("INSERT INTO pending_input (command_id, envelope, created_at, expires_at) VALUES ('c', '{}', 1, 2)");
  room.sql.exec("INSERT INTO refresh_requests (request_id, subject, request_generation, connection_id, deadline) VALUES ('r', 's', 1, 'c', 2)");
  room.sql.exec("INSERT INTO reply_routes (command_id, subject, connection_id, deadline) VALUES ('c', 's', 'c', 2)");
  setIdleFor(room, ABANDONED_ROOM_RECLAIM_MS + 1);

  await room.alarm();

  assert.equal(ctx.alarmLog.deleteAllCalled, true);
});

test("R8 abandoned：回收后同一实例可再 claim→连接（自愈）", async () => {
  const { room, ctx } = await abandonedRoom();
  setIdleFor(room, ABANDONED_ROOM_RECLAIM_MS + 1);
  await room.alarm();
  assert.equal(ctx.alarmLog.deleteAllCalled, true);

  const ws = await claimAndConnectOnce(room, ctx);
  assert.ok(ws, "desktop reconnects into the fresh room");
});

test("R8 abandoned：升级前已鉴权、没有活动时间戳的老房，首次 alarm() 只起算不回收", async () => {
  const { room, ctx } = await abandonedRoom();
  room.sql.exec("DELETE FROM room_meta WHERE key = ?", LAST_ACTIVITY_KEY);

  await room.alarm();

  assert.equal(ctx.alarmLog.deleteAllCalled, false);
  assert.ok(store.getMeta(room.sql, LAST_ACTIVITY_KEY) != null, "clock is seeded, not assumed ancient");
});
