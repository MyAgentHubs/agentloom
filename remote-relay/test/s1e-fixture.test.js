import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";

const FIXTURES = JSON.parse(readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8"));
const TOKEN_FRAMES = FIXTURES.filter((item) => item.layer === "token-frame");
const PUT_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.put");
const DELETE_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.delete");
const ACK_CASES = TOKEN_FRAMES.filter((item) => item.frame.t === "token.ack");
const SUBJECT = "device:11111111-1111-4111-8111-111111111111";

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

function fakeWs(attachment) {
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

function desktopSocket(room, ctx, overrides = {}) {
  const epoch = store.bumpEpoch(room.sql);
  const ws = fakeWs({
    role: "desktop", scope: "desktop", epoch, registry_ready: true,
    connectedAt: Date.now(), ...overrides,
  });
  ctx.acceptWebSocket(ws, ["desktop"]);
  return ws;
}

function seedActive(room, generation = 1, subject = SUBJECT, tokenHash = "f".repeat(64)) {
  const now = Date.now();
  store.putTokenRegistryEntry(room.sql, {
    subject,
    generation,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: tokenHash,
      kind: "current",
      generation,
      access_expires: now + 60_000,
      valid_until: now + 120_000,
    }],
  }, now);
}

async function send(room, ws, frame) {
  await room.webSocketMessage(ws, JSON.stringify(frame));
  return ws.sent.at(-1);
}

function storedEntry(room, subject) {
  const row = store.getTokenSubject(room.sql, subject);
  if (!row) return null;
  return {
    ...row,
    aliases: room.sql.exec(
      "SELECT token_hash, kind, generation, access_expires, valid_until FROM token_aliases WHERE subject = ? ORDER BY kind",
      subject
    ).map((alias) => ({ ...alias })),
  };
}

function expectedStoredPut(frame) {
  const aliases = [{
    token_hash: frame.current.token_hash,
    kind: "current",
    generation: frame.generation,
    access_expires: frame.current.access_expires,
    valid_until: frame.scope === "pairing"
      ? frame.current.access_expires
      : frame.current.refresh_until,
  }];
  if (frame.prev) {
    aliases.push({
      token_hash: frame.prev.token_hash,
      kind: "prev",
      generation: frame.prev.generation,
      access_expires: null,
      valid_until: Math.min(frame.prev.prev_expires, frame.current.refresh_until),
    });
  }
  return {
    subject: frame.subject,
    generation: frame.generation,
    state: "active",
    scope: frame.scope,
    aliases,
  };
}

test("S1e fixture：token.put 16 条逐条打入真实 webSocketMessage", async (t) => {
  assert.equal(PUT_CASES.length, 16);
  for (const item of PUT_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const desktop = desktopSocket(room, ctx);
      const ack = await send(room, desktop, item.frame);

      assert.equal(ack.t, "token.ack");
      assert.equal(ack.result, item.expect.valid ? "ok" : "rejected");
      assert.deepEqual(ack.reason ? [ack.reason] : [], item.expect.errors, `${item.name}: expect.errors`);
      if (item.expect.valid) {
        assert.deepEqual(storedEntry(room, item.frame.subject), expectedStoredPut(item.frame));
      } else {
        assert.equal(storedEntry(room, item.frame.subject), null);
        assert.doesNotMatch(JSON.stringify(ack), /[a-f0-9]{64}/);
      }
    });
  }
});

test("S1e fixture：token.delete 3 条逐条消费并保留 revoked 高水位", async (t) => {
  assert.equal(DELETE_CASES.length, 3);
  for (const item of DELETE_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      seedActive(room);
      const remote = fakeWs({ role: "remote", scope: "remote", subject: SUBJECT, generation: 1, connectedAt: 1 });
      ctx.acceptWebSocket(remote, ["remote"]);
      const desktop = desktopSocket(room, ctx);
      const ack = await send(room, desktop, item.frame);

      assert.equal(ack.t, "token.ack");
      assert.equal(ack.result, item.expect.valid ? "ok" : "rejected");
      const entry = storedEntry(room, SUBJECT);
      if (item.expect.valid) {
        assert.deepEqual(entry, {
          subject: SUBJECT,
          generation: item.frame.generation,
          state: "revoked",
          scope: null,
          aliases: [],
        });
        assert.equal(remote.closed.length, item.frame.close === true ? 1 : 0);
        if (item.frame.close === true) assert.equal(remote.sent.at(-1).reason, "device_revoked");
      } else {
        assert.equal(entry.generation, 1);
        assert.equal(entry.state, "active");
      }
    });
  }
});

test("S1e fixture：token.ack 4 条由 ok/idempotent/rejected 真路径覆盖", async (t) => {
  assert.equal(ACK_CASES.length, 4);
  const put = PUT_CASES.find((item) => item.name === "token_put_current_g100_prev_g5_valid").frame;
  for (const item of ACK_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const desktop = desktopSocket(room, ctx);
      let actual;
      if (item.frame.result === "ok") {
        actual = await send(room, desktop, put);
      } else if (item.frame.result === "idempotent") {
        await send(room, desktop, put);
        actual = await send(room, desktop, put);
      } else if (item.frame.result === "rejected") {
        await send(room, desktop, put);
        actual = await send(room, desktop, {
          ...put,
          generation: item.frame.generation,
          current: { ...put.current },
        });
      } else {
        actual = await send(room, desktop, { ...put, generation: 0 });
      }

      if (item.expect.valid) {
        assert.deepEqual(actual, item.frame);
      } else {
        assert.equal(actual.result, "rejected");
        assert.notEqual(actual.result, item.frame.result);
      }
    });
  }
});

test("管理帧 CAS/floor、同代异内容、未知 subject delete 与单写者/定向 ack", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const desktop = desktopSocket(room, ctx);
  const observer = fakeWs({ role: "remote", scope: "remote" });
  ctx.acceptWebSocket(observer, ["remote"]);
  const base = PUT_CASES.find((item) => item.expect.valid && item.frame.subject !== "pairing").frame;

  assert.equal((await send(room, desktop, base)).result, "ok");
  assert.equal(observer.sent.length, 0);
  assert.equal((await send(room, desktop, {
    ...base,
    current: { ...base.current, token_hash: "d".repeat(64) },
  })).reason, "generation_content_mismatch");
  assert.equal((await send(room, desktop, { ...base, generation: base.generation - 1 })).reason, "generation_too_low");

  room.sql.exec("UPDATE room_state SET registry_floor = ?", base.generation + 1);
  assert.equal((await send(room, desktop, { ...base, generation: base.generation + 1 })).reason, "generation_at_or_below_floor");

  const unknown = "device:22222222-2222-4222-8222-222222222222";
  const deletion = { t: "token.delete", subject: unknown, generation: base.generation + 2 };
  assert.equal((await send(room, desktop, deletion)).result, "ok");
  assert.equal((await send(room, desktop, deletion)).result, "idempotent");

  const staleDesktop = fakeWs({ role: "desktop", scope: "desktop", epoch: desktop.deserializeAttachment().epoch - 1 });
  assert.equal((await send(room, staleDesktop, { ...base, generation: base.generation + 3 })).reason, "stale_epoch");
  const forged = fakeWs({ role: "remote", scope: "desktop", epoch: store.getCurrentEpoch(room.sql) });
  assert.equal((await send(room, forged, { ...base, generation: base.generation + 3 })).reason, "role_forbidden");
});

test("put 校验/撞哈希只回短拒因，不回显哈希", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  seedActive(room, 1, "device:33333333-3333-4333-8333-333333333333");
  const desktop = desktopSocket(room, ctx);
  const frame = {
    t: "token.put",
    subject: SUBJECT,
    generation: 2,
    scope: "remote",
    current: {
      token_hash: "f".repeat(64),
      access_expires: Date.now() + 10_000,
      refresh_until: Date.now() + 20_000,
    },
  };
  const ack = await send(room, desktop, frame);
  assert.equal(ack.result, "rejected");
  assert.equal(ack.reason, "token_hash_conflict");
  assert.doesNotMatch(JSON.stringify(ack), /f{64}/);
});

test("同一规范化原始 put 帧晚重放：relay_now 推进且 clamp 漂移仍幂等、不写表", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const firstNow = 1_800_000_000_000;
  const entry = {
    subject: SUBJECT,
    generation: 3,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: "a".repeat(64),
      kind: "current",
      generation: 3,
      access_expires: firstNow + 100_000_000,
      valid_until: firstNow + 3_000_000_000,
    }],
  };
  assert.equal(store.putTokenRegistryEntry(room.sql, entry, firstNow, { cas: true }).result, "ok");
  const beforeChanges = room.sql.exec("SELECT total_changes() AS n")[0].n;
  assert.equal(store.putTokenRegistryEntry(room.sql, entry, firstNow + 1_000, { cas: true }).result, "idempotent");
  assert.equal(room.sql.exec("SELECT total_changes() AS n")[0].n, beforeChanges);
  assert.equal(room.sql.exec(
    "SELECT COUNT(*) AS n FROM token_put_fingerprints WHERE subject = ? AND generation = ?",
    SUBJECT,
    entry.generation
  )[0].n, 1);
});

test("put 主行与原始帧 fingerprint 同事务：fingerprint 写失败则注册表整体回滚", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  room.sql.exec(`CREATE TRIGGER reject_put_fingerprint
    BEFORE INSERT ON token_put_fingerprints BEGIN SELECT RAISE(ABORT, 'fingerprint write failed'); END`);
  const now = Date.now();
  assert.throws(() => store.putTokenRegistryEntry(room.sql, {
    subject: SUBJECT,
    generation: 3,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: "a".repeat(64),
      kind: "current",
      generation: 3,
      access_expires: now + 60_000,
      valid_until: now + 120_000,
    }],
  }, now, { cas: true }), /fingerprint write failed/);
  assert.equal(store.getTokenSubject(room.sql, SUBJECT), null);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_aliases")[0].n, 0);
});

test("同代 put 把 access_expires 伪装成首存 clamp 边界 S/S+1 均拒绝并计违例", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const firstNow = 1_800_000_000_000;
  const entry = {
    subject: SUBJECT,
    generation: 3,
    state: "active",
    scope: "remote",
    aliases: [{
      token_hash: "a".repeat(64),
      kind: "current",
      generation: 3,
      access_expires: firstNow + 100_000_000,
      valid_until: firstNow + 3_000_000_000,
    }],
  };
  assert.equal(store.putTokenRegistryEntry(room.sql, entry, firstNow, { cas: true }).result, "ok");
  const storedBoundary = store.getTokenAlias(room.sql, SUBJECT, "current").access_expires;
  const desktop = desktopSocket(room, ctx);
  const frame = {
    t: "token.put",
    subject: SUBJECT,
    generation: entry.generation,
    scope: "remote",
    current: {
      token_hash: entry.aliases[0].token_hash,
      access_expires: storedBoundary,
      refresh_until: entry.aliases[0].valid_until,
    },
  };

  assert.deepEqual(await send(room, desktop, frame), {
    t: "token.ack",
    subject: SUBJECT,
    generation: entry.generation,
    result: "rejected",
    reason: "generation_content_mismatch",
  });
  assert.equal((await send(room, desktop, {
    ...frame,
    current: { ...frame.current, access_expires: storedBoundary + 1 },
  })).reason, "generation_content_mismatch");
  await room.webSocketClose(desktop);
  assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "2");
});

test("delete close=true 的同代幂等重试仍强断目标 subject，且不误伤其它 socket", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const otherSubject = "device:22222222-2222-4222-8222-222222222222";
  seedActive(room, 1, SUBJECT);
  seedActive(room, 1, otherSubject, "d".repeat(64));
  const now = Date.now();
  store.putTokenRegistryEntry(room.sql, {
    subject: "pairing",
    generation: 1,
    state: "active",
    scope: "pairing",
    aliases: [{
      token_hash: "e".repeat(64),
      kind: "current",
      generation: 1,
      access_expires: now + 60_000,
      valid_until: now + 60_000,
    }],
  }, now);
  const target = fakeWs({ role: "remote", scope: "remote", subject: SUBJECT });
  const other = fakeWs({ role: "remote", scope: "remote", subject: otherSubject });
  const pairing = fakeWs({ role: "remote", scope: "pairing", subject: "pairing" });
  ctx.acceptWebSocket(target, ["remote"]);
  ctx.acceptWebSocket(other, ["remote"]);
  ctx.acceptWebSocket(pairing, ["remote"]);
  const desktop = desktopSocket(room, ctx);

  const deletion = { t: "token.delete", subject: SUBJECT, generation: 2 };
  assert.equal((await send(room, desktop, deletion)).result, "ok");
  assert.equal(target.closed.length, 0);
  assert.equal((await send(room, desktop, { ...deletion, close: true })).result, "idempotent");
  assert.equal(target.sent.at(-1).reason, "device_revoked");
  assert.equal(target.closed.length, 1);
  assert.equal(other.closed.length, 0);
  assert.equal(pairing.closed.length, 0);
  assert.equal(desktop.closed.length, 0);
  assert.equal(room.sql.exec(
    "SELECT COUNT(*) AS n FROM token_put_fingerprints WHERE subject = ?",
    SUBJECT
  )[0].n, 0);
});
