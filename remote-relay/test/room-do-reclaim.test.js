import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";

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
