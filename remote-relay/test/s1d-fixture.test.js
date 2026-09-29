import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";

const FIXTURES = JSON.parse(readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8"));
const cases = (layer) => FIXTURES.filter((item) => item.layer === layer);
const ROOM = "0123456789abcdef0123456789abcdef";
const CT = Buffer.from("s1d ciphertext").toString("base64");
const N12 = Buffer.alloc(12, 4).toString("base64");

function sha256(value) {
  return createHash("sha256").update(value, "ascii").digest("hex");
}

function makeRuntime() {
  const db = new DatabaseSync(":memory:");
  const sql = {
    exec(query, ...params) {
      const stmt = db.prepare(query);
      if (/^\s*(SELECT|PRAGMA)/i.test(query)) return stmt.all(...params);
      stmt.run(...params);
      return [];
    },
  };
  const registry = [];
  const alarmTimes = [];
  const storage = {
    sql,
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
    async setAlarm(timestamp) {
      alarmTimes.push(timestamp);
    },
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
  return { ctx, sql, alarmTimes };
}

function fakeWs(attachment = null) {
  let currentAttachment = attachment;
  return {
    sent: [],
    closed: [],
    // R1：deliverRefreshReceipt 现在核 readyState !== OPEN(1) 才投递（真实
    // Cloudflare 运行时里 close 握手期间 getWebSockets() 仍可能吐出 CLOSING
    // 的 socket）——这个薄 mock 不建模 CLOSING 中间态（那是 s1ja-fake-mobile-
    // e2e.test.js 私有 mock 的职责，本文件的测试不测那条竞态窗口），close()
    // 后直接置 CLOSED，只要能让"已断=不再投递"这条不变量成立即可。
    readyState: 1, // WebSocket.OPEN
    send(text) {
      this.sent.push(typeof text === "string" ? JSON.parse(text) : text);
    },
    close(code, reason) {
      this.closed.push({ code, reason });
      this.readyState = 3; // WebSocket.CLOSED
    },
    serializeAttachment(value) {
      currentAttachment = value;
    },
    deserializeAttachment() {
      return currentAttachment;
    },
  };
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

function seedSubject(room, {
  subject = "device:22222222-2222-4222-8222-222222222222",
  generation = 7,
  state = "active",
  scope = "remote",
  aliases,
  now = Date.now(),
}) {
  return store.putTokenRegistryEntry(room.sql, { subject, generation, state, scope, aliases }, now);
}

function inboundPayload(frameType, epoch) {
  if (["event", "live", "input", "control"].includes(frameType)) {
    return {
      v: 1,
      room: ROOM,
      epoch,
      kind: frameType,
      session: "s1",
      command_id: ["input", "control"].includes(frameType) ? `cmd-${frameType}` : null,
      seq: null,
      client_msg_id: frameType === "event" ? "s1d-matrix-event" : null,
      ct: CT,
      n: N12,
      ts: Date.now(),
    };
  }
  return { t: frameType };
}

function offeredTokenCandidates(offers) {
  return offers
    .filter((offer) => offer.startsWith("token."))
    .map((offer) => offer.slice("token.".length));
}

test("wire-v1 subprotocol 层 9 条逐条驱动真实 upgrade", async (t) => {
  const fixtures = cases("subprotocol");
  assert.equal(fixtures.length, 9);
  for (const item of fixtures) {
    await t.test(item.name, async () => {
      await withUpgradeRuntime(async () => {
        const { ctx, alarmTimes } = makeRuntime();
        const room = new RoomDO(ctx, {});
        store.ensureBusinessSchema(room.sql);
        if (!item.expect.accept) {
          const now = Date.now();
          for (const [index, token] of offeredTokenCandidates(item.offers).entries()) {
            const ordinal = String(index + 1);
            seedSubject(room, {
              subject: `device:0000000${ordinal}-0000-4000-8000-${ordinal.padStart(12, "0")}`,
              generation: index + 1,
              aliases: [{
                token_hash: sha256(token),
                kind: "current",
                generation: index + 1,
                access_expires: now + 60_000,
                valid_until: now + 120_000,
              }],
              now,
            });
            assert.equal(store.resolveTokenAdmission(room.sql, sha256(token), now).scope, "remote");
          }
        } else {
          const now = Date.now();
          seedSubject(room, {
            aliases: [{
              token_hash: sha256(item.expect.token_hex),
              kind: "current",
              generation: 7,
              access_expires: now + 60_000,
              valid_until: now + 120_000,
            }],
            now,
          });
        }
        const response = await room.fetch(new Request(`https://relay.example/room/${ROOM}?last_seq=3`, {
          headers: {
            Upgrade: "websocket",
            "Sec-WebSocket-Protocol": item.offers.join(", "),
            "CF-Connecting-IP": "203.0.113.9",
          },
        }));

        if (!item.expect.accept) {
          assert.equal(response.status, item.expect.status);
          assert.equal(ctx.getWebSockets().length, 0);
          return;
        }
        assert.equal(response.status, 101);
        assert.equal(response.headers.get("Sec-WebSocket-Protocol"), item.expect.echo);
        const ws = ctx.getWebSockets("remote")[0];
        const attachment = ws.deserializeAttachment();
        assert.equal(attachment.role, "remote");
        assert.equal(attachment.scope, "remote");
        assert.equal(attachment.subject, "device:22222222-2222-4222-8222-222222222222");
        assert.equal(attachment.kind, "current");
        assert.equal(attachment.generation, 7);
        assert.equal(attachment.alias_generation, 7);
        assert.equal(typeof attachment.access_expires, "number");
        assert.equal(typeof attachment.valid_until, "number");
        assert.equal(typeof attachment.epoch, "number");
        assert.equal(typeof attachment.connectedAt, "number");
        assert.equal(attachment.lastSeq, 3);
        assert.match(attachment.connection_id, /^[0-9a-f-]{36}$/);
        assert.match(attachment.ip_bucket_key, /^[0-9a-f]+$/);
        assert.deepEqual(alarmTimes, [attachment.access_expires]);
      });
    });
  }
});

test("wire-v1 time-window 层 12 条逐条驱动真实注册表准入", async (t) => {
  const fixtures = cases("time-window");
  assert.equal(fixtures.length, 12);
  for (const [index, item] of fixtures.entries()) {
    await t.test(item.name, () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const token = String(index + 1).padStart(64, "0");
      const subjectGeneration = item.row.current_generation ?? item.row.generation ?? 7;
      const subject = item.row.scope === "pairing" ? "pairing" : "device:22222222-2222-4222-8222-222222222222";
      seedSubject(room, {
        subject,
        generation: subjectGeneration,
        state: "active",
        scope: item.row.scope,
        aliases: [{
          token_hash: sha256(token),
          kind: item.row.kind,
          generation: item.row.generation ?? subjectGeneration,
          access_expires: item.row.kind === "current" ? item.row.access_expires : null,
          valid_until: item.row.valid_until,
        }],
        now: item.now_ms,
      });
      if (item.row.subject_state === "revoked") {
        room.sql.exec("UPDATE token_subjects SET state = 'revoked', scope = NULL WHERE subject = ?", subject);
      }

      const decision = store.resolveTokenAdmission(room.sql, sha256(token), item.now_ms);
      if (item.expect.decision.startsWith("scope:")) {
        assert.equal(decision.scope, item.expect.decision.slice("scope:".length));
      } else if (item.expect.decision === "reject:stale_generation") {
        assert.equal(decision, null);
      } else {
        assert.equal(decision, null);
      }
    });
  }
});

test("wire-v1 chain 层 2 条贯穿 ASCII hash、种表与时窗准入", async (t) => {
  const fixtures = cases("chain");
  assert.equal(fixtures.length, 2);
  for (const item of fixtures) {
    await t.test(item.name, () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const token = item.connect_token_hex ?? item.capability_token_hex;
      assert.equal(sha256(token), item.token_hash_hex);
      const frame = item.put_frame;
      seedSubject(room, {
        subject: frame.subject,
        generation: frame.generation,
        scope: frame.scope,
        aliases: [{
          token_hash: frame.current.token_hash,
          kind: "current",
          generation: frame.generation,
          access_expires: frame.current.access_expires,
          valid_until: frame.current.refresh_until ?? frame.current.access_expires,
        }],
        now: item.window.now_ms,
      });
      const decision = store.resolveTokenAdmission(room.sql, sha256(token), item.window.now_ms);
      assert.equal(decision.scope, item.expect.scope);
    });
  }
});

test("wire-v1 ttl-clamp 层 6 条逐条消费写入口截断", async (t) => {
  const fixtures = cases("ttl-clamp");
  assert.equal(fixtures.length, 6);
  for (const item of fixtures) {
    await t.test(item.name, () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const pairing = item.cap === "pairing";
      const prev = item.cap === "prev";
      const accessExpires = pairing || item.cap === "access" || item.name.includes("access") || item.name.includes("below")
        ? item.input_ms
        : item.relay_now_ms + 1_000;
      const validUntil = item.cap === "refresh_until" || item.cap === "access" ? item.input_ms : item.relay_now_ms + 60_000;
      const subject = pairing ? "pairing" : "device:55555555-5555-4555-8555-555555555555";
      seedSubject(room, {
        subject,
        generation: 1,
        scope: pairing ? "pairing" : "remote",
        aliases: [{
          token_hash: "6".repeat(64),
          kind: prev ? "prev" : "current",
          generation: 1,
          access_expires: prev ? null : accessExpires,
          valid_until: pairing ? item.input_ms : (prev ? item.input_ms : validUntil),
        }],
        now: item.relay_now_ms,
      });
      const row = room.sql.exec("SELECT access_expires, valid_until FROM token_aliases")[0];
      const stored = item.cap === "access" || item.name.includes("access") || item.name.includes("below")
        ? row.access_expires
        : row.valid_until;
      assert.equal(stored, item.expect.stored_ms);
    });
  }
});

test("注册表 schema 与写/清 helpers 强制 CHECK、UNIQUE、跨 subject 哈希防撞", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const schemas = Object.fromEntries(
    room.sql.exec("SELECT name, sql FROM sqlite_master WHERE type = 'table' AND name LIKE 'token_%'")
      .map((row) => [row.name, row.sql])
  );
  assert.match(schemas.token_subjects, /generation > 0/);
  assert.match(schemas.token_subjects, /active','revoked/);
  assert.match(schemas.token_subjects, /remote','pairing/);
  assert.match(schemas.token_aliases, /current','prev/);
  assert.match(schemas.token_aliases, /UNIQUE \(subject, kind\)/i);

  const now = Date.now();
  seedSubject(room, {
    generation: 1,
    aliases: [{ token_hash: "a".repeat(64), kind: "current", generation: 1, access_expires: now + 1_000, valid_until: now + 2_000 }],
    now,
  });
  assert.throws(() => seedSubject(room, {
    subject: "device:33333333-3333-4333-8333-333333333333",
    generation: 1,
    aliases: [{ token_hash: "a".repeat(64), kind: "current", generation: 1, access_expires: now + 1_000, valid_until: now + 2_000 }],
    now,
  }), /another subject/);
  store.clearTokenRegistry(room.sql);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_subjects")[0].n, 0);
  assert.equal(room.sql.exec("SELECT COUNT(*) AS n FROM token_aliases")[0].n, 0);
});

test("双别名写入口把 prev.valid_until 截到 current refresh_until", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedSubject(room, {
    generation: 100,
    aliases: [
      { token_hash: "a".repeat(64), kind: "current", generation: 100, access_expires: now + 30_000, valid_until: now + 60_000 },
      { token_hash: "b".repeat(64), kind: "prev", generation: 5, access_expires: null, valid_until: now + 120_000 },
    ],
    now,
  });
  const rows = room.sql.exec("SELECT kind, generation, valid_until FROM token_aliases ORDER BY kind")
    .map((row) => ({ ...row }));
  assert.deepEqual(rows, [
    { kind: "current", generation: 100, valid_until: now + 60_000 },
    { kind: "prev", generation: 5, valid_until: now + 60_000 },
  ]);
});

test("subprotocol 有额外项或空项也默认拒绝", async () => {
  await withUpgradeRuntime(async () => {
    for (const offered of [
      `agentloom-rc-v1, token.${"a".repeat(64)}, extra`,
      `agentloom-rc-v1, , token.${"a".repeat(64)}`,
    ]) {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const response = await room.fetch(new Request(`https://relay.example/room/${ROOM}`, {
        headers: { Upgrade: "websocket", "Sec-WebSocket-Protocol": offered },
      }));
      assert.equal(response.status, 401);
    }
  });
});

test("wire-v1 inbound-matrix 层 20 条逐条驱动真实 webSocketMessage", async (t) => {
  const fixtures = cases("inbound-matrix");
  assert.equal(fixtures.length, 20);
  for (const item of fixtures) {
    await t.test(item.name, async () => {
      const { ctx } = makeRuntime();
      const room = new RoomDO(ctx, {});
      store.ensureBusinessSchema(room.sql);
      const epoch = item.scope === "desktop" ? store.bumpEpoch(room.sql) : store.getCurrentEpoch(room.sql);
      const attachment = { role: item.scope === "desktop" ? "desktop" : "remote", scope: item.scope, epoch };
      const now = Date.now();
      if (item.scope === "pairing") {
        seedSubject(room, {
          subject: "pairing",
          generation: 1,
          scope: "pairing",
          aliases: [{ token_hash: "8".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 60_000 }],
          now,
        });
        Object.assign(attachment, { subject: "pairing", kind: "current", generation: 1, alias_generation: 1, access_expires: now + 60_000, valid_until: now + 60_000 });
      } else if (item.scope === "refresh") {
        const subject = "device:77777777-7777-4777-8777-777777777777";
        seedSubject(room, {
          subject,
          generation: 1,
          aliases: [{ token_hash: "7".repeat(64), kind: "prev", generation: 1, access_expires: null, valid_until: now + 60_000 }],
          now,
        });
        Object.assign(attachment, { subject, kind: "prev", generation: 1, alias_generation: 1, access_expires: null, valid_until: now + 60_000 });
      } else if (item.scope === "remote") {
        // S1ja F2：legacy 无 subject 的 remote 豁免已撤——这份 20 条样张过去靠
        // 那条豁免让 scope=remote 的用例免于挂一个真实 subject 也能过
        // authorizeInboundSocket，退役后必须像 pairing/refresh 分支一样落一个
        // 真实 registry-backed subject，否则这 5 条 remote 用例会全部先在
        // authorizeInboundSocket 判 reauthorization_failed 出局，根本走不到
        // 这份样张真正要测的「§9.1 scope→帧型矩阵」那一步。
        const subject = "device:66666666-6666-4666-8666-666666666666";
        seedSubject(room, {
          subject,
          generation: 1,
          aliases: [{ token_hash: "6".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 }],
          now,
        });
        Object.assign(attachment, { subject, kind: "current", generation: 1, alias_generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 });
      }
      const ws = fakeWs(attachment);
      ctx.acceptWebSocket(ws, [item.scope === "desktop" ? "desktop" : "remote"]);

      await room.webSocketMessage(ws, JSON.stringify(inboundPayload(item.frame_t, epoch)));
      room.flushProtocolViolations();

      // S1ja 加固：识别 authorizeInboundSocket + 矩阵闸能产出的全部三种拒绝
      // 理由（role_forbidden / stale_generation / reauthorization_failed），
      // 不再只认 role_forbidden 一种——退役前这里曾经只看 role_forbidden，
      // 导致 5 条 scope=remote 用例即使被 authorizeInboundSocket 用
      // reauthorization_failed 拒绝，这条断言也会误判「符合预期」而不报红
      // （两条独立断言凑巧都不触发，详见 F2 收尾报告）。**不**扩大成「任何
      // error 帧都算被拒」——pair.hello/pair.done/token.refresh 等 allowed
      // 用例本就会在这两道闸之后的下游业务处理阶段产出其它、与鉴权无关的
      // error（如本文件裸帧触发的 request_id_required），那些不该被计入。
      const AUTH_GATE_DENY_REASONS = new Set(["role_forbidden", "stale_generation", "reauthorization_failed"]);
      if (item.expect.allowed) {
        const denied = ws.sent.some((frame) => frame.t === "error" && AUTH_GATE_DENY_REASONS.has(frame.reason));
        assert.equal(denied, false, `${item.name}: expected allowed（不该撞上任何鉴权闸拒绝），实际 sent=${JSON.stringify(ws.sent)}`);
      } else {
        // R4（双路审）：这份样张真正要测的是 §9.1「scope→帧型矩阵」——forbidden
        // 用例必须精确落在 role_forbidden（矩阵闸拒绝），不能只要「三种理由
        // 里随便一种」就算过。若命中的是 stale_generation/reauthorization_failed，
        // 说明连 authorizeInboundSocket 这道更早的闸都没放行——测试自己的
        // registry seed（subject/generation/alias 三件套）跟 attachment 对不上，
        // 这是 fixture 前置条件坏了，不是矩阵真的挡住了它；两者混着算「通过」
        // 会把「样张前置坏了」悄悄伪装成「矩阵测对了」。
        const authGateError = ws.sent.find((frame) => frame.t === "error" && AUTH_GATE_DENY_REASONS.has(frame.reason));
        assert.ok(authGateError, `${item.name}: expected 被拒，但没有任何鉴权闸 error 帧，实际 sent=${JSON.stringify(ws.sent)}`);
        assert.equal(
          authGateError.reason,
          "role_forbidden",
          `${item.name}: 期望矩阵闸拒绝（role_forbidden），实际是 ${authGateError.reason}——这通常意味着这条用例的 registry seed（subject/generation/alias）没配对上 attachment，是 fixture 前置坏了，不是矩阵真挡住了它`,
        );
      }
      // token.put 与 token.refresh 都是「矩阵放行但帧本身裸得只剩 {t}」——
      // S1i2 起 token.refresh 也有真实的必填字段校验（request_id/ct/n），
      // 这份 20 条 inbound-matrix 样张里的裸帧同样会先在这道结构闸上被拒。
      const malformedManagedFrame = item.expect.allowed &&
        ["token.put", "token.refresh"].includes(item.frame_t);
      assert.equal(
        Number(store.getMeta(room.sql, "protocol_violation_count", "0")),
        item.expect.allowed && !malformedManagedFrame ? 0 : 1
      );
      if (item.frame_t === "token.put" && malformedManagedFrame) {
        assert.equal(ws.sent.at(-1).t, "token.ack");
        assert.equal(ws.sent.at(-1).result, "rejected");
      } else if (item.frame_t === "token.refresh" && malformedManagedFrame) {
        assert.equal(ws.sent.at(-1).t, "error");
        assert.equal(ws.sent.at(-1).reason, "request_id_required");
      }
    });
  }
});

test("旧形态 attachment scope 缺失或未知时 error + close 并计数", async () => {
  for (const scope of [undefined, "future-scope"]) {
    const { ctx } = makeRuntime();
    const room = new RoomDO(ctx, {});
    store.ensureBusinessSchema(room.sql);
    const ws = fakeWs({ epoch: 0, role: "remote", lastSeq: 0, connectedAt: 1, ...(scope ? { scope } : {}) });
    await room.webSocketMessage(ws, JSON.stringify({ t: "presence" }));
    assert.equal(ws.sent.at(-1).reason, "role_forbidden");
    assert.equal(ws.closed.length, 1);
    assert.equal(store.getMeta(room.sql, "protocol_violation_count"), "1");
  }
});

test("持续再授权闸：低代 official socket 回 error 并 close；legacy remote（无 subject）现在同样被拒", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  seedSubject(room, {
    generation: 8,
    aliases: [{
      token_hash: "a".repeat(64),
      kind: "current",
      generation: 8,
      access_expires: now + 60_000,
      valid_until: now + 120_000,
    }],
    now,
  });
  const stale = fakeWs({
    role: "remote",
    scope: "remote",
    subject: "device:22222222-2222-4222-8222-222222222222",
    kind: "current",
    generation: 7,
    alias_generation: 7,
    access_expires: now + 60_000,
    valid_until: now + 120_000,
    epoch: 0,
  });
  await room.webSocketMessage(stale, JSON.stringify({ t: "presence" }));
  assert.equal(stale.sent.at(-1).reason, "stale_generation");
  assert.equal(stale.closed.length, 1);

  // S1ja F2：legacy 无 subject 的 remote 豁免已撤——过去这条走「豁免、不拒」，
  // 现在跟任何真实 subject 缺失/失活的情形一样，落进 authorizeInboundSocket 的
  // fail-closed 分支：回 reauthorization_failed + close（本测试原名「legacy
  // remote 豁免」正是钉着这条旧行为，F2 改成钉它的反面）。
  const legacy = fakeWs({ role: "remote", scope: "remote", epoch: 0 });
  await room.webSocketMessage(legacy, JSON.stringify({ t: "presence" }));
  // closed 断言放前面（且用 ?. 防 sent 为空时以 TypeError 挂而不是「该拒没拒」
  // 的实质断言失败——若这条豁免被误改回，legacy 会正常处理这帧 presence、
  // sent 数组不会自己塞进任何东西，只有 closed.length 这条能干净地报出
  // 「预期 1，实际 0」）。
  assert.equal(legacy.closed.length, 1);
  assert.equal(legacy.sent.at(-1)?.reason, "reauthorization_failed");
});

test("非连续代 prev g5 → current g100 可持续 refresh；subject 升到 g101 后·入站仍失活·出站回执改走自有谓词", async () => {
  // S1i2 P1-b：本测试原先直接调用 room.broadcastRaw({t:"token.refresh.ok",...})
  // 模拟回执出站，钉死的是「subject 升代后回执不再投递」——那正是旧 bug 本身
  // （桌面轮换升代后，手机 socket 的 attachment 章还是旧代，canDeliverOutbound
  // 的 isOfficialAttachmentLive 会先把它判死，回执永远送不到，每次轮换都被迫
  // 走「断线 + prev 重连 + journal 重放」的慢路径）。新实现里回执不再经
  // broadcastRaw/canDeliverOutbound，而是 deliverRefreshReceipt 自有谓词——
  // 直接查 token_subjects 的当前权威 generation，不依赖目标 socket 自己的
  // attachment 是否还新鲜。本测试改为直接驱动 deliverRefreshReceipt，证明
  // 新语义的两面：谓词④挡住「回执自带的 generation 落后于 subject 当前
  // generation」的旧代回执（唯一防线，见母文档 §9.6 第 246 行 + S1i1 遗留：
  // 桌面幂等重放不等 put 的 ack 就直接发回执）；但只要回执自带 generation
  // 追上了 subject 的新代，即便目标连接自己的 attachment 章仍是旧代，也照样
  // 送达——这正是撤掉的旧行为的反面。
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:99999999-9999-4999-8999-999999999999";
  const prevToken = "b".repeat(64);
  seedSubject(room, {
    subject,
    generation: 100,
    aliases: [
      { token_hash: "a".repeat(64), kind: "current", generation: 100, access_expires: now + 30_000, valid_until: now + 60_000 },
      { token_hash: sha256(prevToken), kind: "prev", generation: 5, access_expires: null, valid_until: now + 60_000 },
    ],
    now,
  });
  const admission = store.resolveTokenAdmission(room.sql, sha256(prevToken), now);
  assert.deepEqual(admission, {
    scope: "refresh",
    subject,
    kind: "prev",
    generation: 100,
    alias_generation: 5,
    access_expires: null,
    valid_until: now + 60_000,
  });
  const inboundWs = fakeWs({ role: "remote", ...admission, epoch: 0, connection_id: "conn-inbound" });
  const outboundWs = fakeWs({ role: "remote", ...admission, epoch: 0, connection_id: "conn-outbound" });
  ctx.acceptWebSocket(inboundWs, ["remote"]);
  ctx.acceptWebSocket(outboundWs, ["remote"]);

  await room.webSocketMessage(inboundWs, JSON.stringify({ t: "token.refresh" }));
  assert.equal(inboundWs.sent.at(-1).reason, "request_id_required");
  assert.equal(inboundWs.closed.length, 0);

  // 直接建行（跳过完整 forward 往返，聚焦回执投递谓词本身）：request_id
  // 绑定 inboundWs 的 connection_id，request_generation 记的是轮换前的 100。
  store.upsertRefreshRequest(room.sql, {
    requestId: "req-100",
    subject,
    requestGeneration: 100,
    connectionId: "conn-inbound",
    deadline: now + 60_000,
  });
  room.deliverRefreshReceipt({
    t: "token.refresh.ok", request_id: "req-100", subject, generation: 100, ct: "x", n: "y",
  }, now);
  assert.equal(inboundWs.sent.at(-1).t, "token.refresh.ok");
  assert.equal(outboundWs.sent.length, 0, "只投请求行 connection_id 对应的那一个连接，同 subject 的旁观连接不该收到");
  assert.equal(inboundWs.closed.length, 0);
  assert.equal(outboundWs.closed.length, 0);

  seedSubject(room, {
    subject,
    generation: 101,
    aliases: [
      { token_hash: "a".repeat(64), kind: "current", generation: 101, access_expires: now + 30_000, valid_until: now + 60_000 },
      { token_hash: sha256(prevToken), kind: "prev", generation: 5, access_expires: null, valid_until: now + 60_000 },
    ],
    now,
  });

  // 入站闸未变：inboundWs 自己的 attachment 章仍是旧代 100，subject 已到
  // 101，authorizeInboundSocket 照常拒绝并断开。
  await room.webSocketMessage(inboundWs, JSON.stringify({ t: "token.refresh" }));
  assert.equal(inboundWs.sent.at(-1).reason, "stale_generation");
  assert.equal(inboundWs.closed.length, 1);

  // 手机走新连接重发同一个 request_id → 安全重绑 connection_id（§9.6）。
  store.upsertRefreshRequest(room.sql, {
    requestId: "req-100",
    subject,
    requestGeneration: 100,
    connectionId: "conn-outbound",
    deadline: now + 60_000,
  });

  // 谓词④·唯一防线：桌面幂等重放了一份代号已过时（仍是 100）的旧回执——
  // 即便目标连接（outboundWs）此刻活得好好的，也必须丢弃，不能带着旧代号
  // 的 payload 混过去。变异自证见下方专门测试；这里断言真实效果：不投递、
  // 不关连接、行不删（deadline 未到，行仍保留，等下面新代回执再投）。
  room.deliverRefreshReceipt({
    t: "token.refresh.ok", request_id: "req-100", subject, generation: 100, ct: "stale-ct", n: "stale-n",
  }, now);
  assert.equal(outboundWs.sent.length, 0);
  assert.equal(outboundWs.closed.length, 0);
  assert.notEqual(store.getRefreshRequest(room.sql, "req-100"), null);

  // 新代回执（generation:101，匹配 subject 当前代）用同一个 request_id 仍可
  // 投递给它当前绑定的连接——旧实现下 canDeliverOutbound 会因为「目标 socket
  // 自己的 attachment 章是旧代」直接判死，回执永远送不到；新实现只认
  // token_subjects 的当前权威 generation，与目标 socket 自己的 attachment
  // 新鲜与否无关。
  room.deliverRefreshReceipt({
    t: "token.refresh.ok", request_id: "req-100", subject, generation: 101, ct: "fresh-ct", n: "fresh-n",
  }, now);
  assert.equal(outboundWs.sent.at(-1).t, "token.refresh.ok");
  assert.equal(outboundWs.sent.at(-1).generation, 101);
  assert.equal(store.getRefreshRequest(room.sql, "req-100"), null, "投完删行");
});

test("current 中间带真实 upgrade 可持续 refresh；subject 升代后失活", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx, alarmTimes } = makeRuntime();
    const room = new RoomDO(ctx, {});
    store.ensureBusinessSchema(room.sql);
    const now = Date.now();
    const subject = "device:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const token = "c".repeat(64);
    seedSubject(room, {
      subject,
      generation: 9,
      aliases: [{
        token_hash: sha256(token),
        kind: "current",
        generation: 9,
        access_expires: now - 1_000,
        valid_until: now + 60_000,
      }],
      now: now - 120_000,
    });
    const response = await room.fetch(new Request(`https://relay.example/room/${ROOM}`, {
      headers: {
        Upgrade: "websocket",
        "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${token}`,
      },
    }));
    assert.equal(response.status, 101);
    const ws = ctx.getWebSockets("remote")[0];
    const attachment = ws.deserializeAttachment();
    assert.equal(attachment.scope, "refresh");
    assert.equal(attachment.kind, "current");
    assert.equal(attachment.generation, 9);
    assert.equal(attachment.alias_generation, 9);
    assert.deepEqual(alarmTimes, [attachment.valid_until]);

    await room.webSocketMessage(ws, JSON.stringify({ t: "token.refresh" }));
    assert.equal(ws.sent.at(-1).reason, "request_id_required");
    assert.equal(ws.closed.length, 0);

    // S1i2：回执改走 deliverRefreshReceipt 自有谓词（不再经 broadcastRaw/
    // canDeliverOutbound）——直接建行 + 投递，验证升代前仍可正常送达这条
    // 仍是活 refresh 的连接。
    store.upsertRefreshRequest(room.sql, {
      requestId: "req-current-9",
      subject,
      requestGeneration: 9,
      connectionId: attachment.connection_id,
      deadline: now + 60_000,
    });
    room.deliverRefreshReceipt({
      t: "token.refresh.ok", request_id: "req-current-9", subject, generation: 9, ct: "x", n: "y",
    }, now);
    assert.equal(ws.sent.at(-1).t, "token.refresh.ok");
    assert.equal(ws.closed.length, 0);

    seedSubject(room, {
      subject,
      generation: 10,
      aliases: [{
        token_hash: sha256(token),
        kind: "current",
        generation: 9,
        access_expires: now - 1_000,
        valid_until: now + 60_000,
      }],
      now: now - 120_000,
    });

    await room.webSocketMessage(ws, JSON.stringify({ t: "token.refresh" }));
    assert.equal(ws.sent.at(-1).reason, "stale_generation");
    assert.equal(ws.closed.length, 1);
  });
});

test("出站业务闸：仅活 current remote 收到，过期 current 被 close", () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const currentSubject = "device:22222222-2222-4222-8222-222222222222";
  const expiredSubject = "device:33333333-3333-4333-8333-333333333333";
  const prevSubject = "device:44444444-4444-4444-8444-444444444444";
  seedSubject(room, { subject: currentSubject, generation: 3, aliases: [{ token_hash: "1".repeat(64), kind: "current", generation: 3, access_expires: now + 60_000, valid_until: now + 120_000 }], now });
  seedSubject(room, { subject: expiredSubject, generation: 4, aliases: [{ token_hash: "2".repeat(64), kind: "current", generation: 4, access_expires: now - 1, valid_until: now + 120_000 }], now: now - 180_000 });
  seedSubject(room, { subject: prevSubject, generation: 5, aliases: [{ token_hash: "3".repeat(64), kind: "prev", generation: 5, access_expires: null, valid_until: now + 120_000 }], now });
  seedSubject(room, { subject: "pairing", generation: 6, scope: "pairing", aliases: [{ token_hash: "4".repeat(64), kind: "current", generation: 6, access_expires: now + 60_000, valid_until: now + 60_000 }], now });

  const active = fakeWs({ role: "remote", scope: "remote", subject: currentSubject, kind: "current", generation: 3, alias_generation: 3, access_expires: now + 60_000, valid_until: now + 120_000 });
  const expired = fakeWs({ role: "remote", scope: "remote", subject: expiredSubject, kind: "current", generation: 4, alias_generation: 4, access_expires: now - 1, valid_until: now + 120_000 });
  const prev = fakeWs({ role: "remote", scope: "refresh", subject: prevSubject, kind: "prev", generation: 5, alias_generation: 5, access_expires: null, valid_until: now + 120_000 });
  const pairing = fakeWs({ role: "remote", scope: "pairing", subject: "pairing", kind: "current", generation: 6, alias_generation: 6, access_expires: now + 60_000, valid_until: now + 60_000 });
  for (const ws of [active, expired, prev, pairing]) ctx.acceptWebSocket(ws, ["remote"]);

  room.broadcastRaw({ v: 1, kind: "live" }, null);

  assert.equal(active.sent.length, 1);
  assert.equal(expired.sent.length, 0);
  assert.equal(expired.closed.length, 1);
  assert.equal(prev.sent.length, 0);
  assert.equal(pairing.sent.length, 0);
});

test("出站业务闸：legacy 无 subject 的 remote（canDeliverOutbound 同族豁免）不再收任何广播", () => {
  // S1ja F2 第二条 fail-closed：canDeliverOutbound 曾经对没有 subject 的
  // remote attachment 视同「活的 current remote」放行业务帧（`legacy remote
  // 在 S1j 前视同活 current remote` 那条注释）——这条跟 authorizeInboundSocket
  // 的入站豁免是同一批后门的两面，本单一并撤掉。没有其它测试专门驱动这个
  // 分支（其它出站闸测试全部改用真实 registry-backed subject 了），必须单独
  // 补一条，否则把这条 fail-closed 改回豁免也不会有任何测试报红。
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const currentSubject = "device:55555555-5555-4555-8555-555555555555";
  seedSubject(room, { subject: currentSubject, generation: 1, aliases: [{ token_hash: "9".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 }], now });

  const active = fakeWs({ role: "remote", scope: "remote", subject: currentSubject, kind: "current", generation: 1, alias_generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 });
  const legacy = fakeWs({ role: "remote", scope: "remote" });
  for (const ws of [active, legacy]) ctx.acceptWebSocket(ws, ["remote"]);

  room.broadcastRaw({ v: 1, kind: "live" }, null);

  assert.equal(active.sent.length, 1, "真实 registry-backed remote 照常收到广播");
  assert.equal(legacy.sent.length, 0, "legacy 无 subject 的 remote 不再收任何出站帧");
});

test("alarm 是无流量兜底：关闭过期 official socket，保留活连接", async () => {
  const { ctx, alarmTimes } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subject = "device:66666666-6666-4666-8666-666666666666";
  seedSubject(room, {
    subject,
    generation: 2,
    aliases: [{ token_hash: "5".repeat(64), kind: "current", generation: 2, access_expires: now + 60_000, valid_until: now + 120_000 }],
    now,
  });
  const live = fakeWs({ role: "remote", scope: "remote", subject, kind: "current", generation: 2, alias_generation: 2, access_expires: now + 60_000, valid_until: now + 120_000 });
  const expired = fakeWs({ role: "remote", scope: "remote", subject, kind: "current", generation: 2, alias_generation: 2, access_expires: now - 1, valid_until: now + 120_000 });
  ctx.acceptWebSocket(live, ["remote"]);
  ctx.acceptWebSocket(expired, ["remote"]);

  await room.scheduleNextTokenAlarm(now);
  await room.alarm();

  assert.equal(live.closed.length, 0);
  assert.equal(expired.closed.length, 1);
  assert.deepEqual(alarmTimes, [now + 60_000, now + 60_000]);
});

test("refresh 回执出站只投请求行 connection_id 对应的那一个连接", () => {
  // S1i2 P1-b：回执不再经 broadcastRaw/canDeliverOutbound（那条闸现在对
  // token.refresh.ok/fail 恒返回 false），投递改由 deliverRefreshReceipt
  // 自有谓词负责——目标不是「同 subject 的所有 refresh socket」，而是精确
  // 到请求行记的那一个 connection_id。
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const now = Date.now();
  const subjects = [
    "device:11111111-1111-4111-8111-111111111111",
    "device:22222222-2222-4222-8222-222222222222",
  ];
  const sockets = subjects.map((subject, index) => {
    seedSubject(room, {
      subject,
      generation: index + 1,
      aliases: [{ token_hash: String(index + 1).repeat(64), kind: "prev", generation: index + 1, access_expires: null, valid_until: now + 60_000 }],
      now,
    });
    const ws = fakeWs({
      role: "remote", scope: "refresh", subject, kind: "prev", generation: index + 1,
      alias_generation: index + 1, access_expires: null, valid_until: now + 60_000,
      connection_id: `conn-${index}`,
    });
    ctx.acceptWebSocket(ws, ["remote"]);
    return ws;
  });

  store.upsertRefreshRequest(room.sql, {
    requestId: "req-only-first",
    subject: subjects[0],
    requestGeneration: 1,
    connectionId: "conn-0",
    deadline: now + 60_000,
  });
  room.deliverRefreshReceipt({
    t: "token.refresh.ok", request_id: "req-only-first", subject: subjects[0], generation: 1, ct: "x", n: "y",
  }, now);

  assert.equal(sockets[0].sent.length, 1);
  assert.equal(sockets[1].sent.length, 0);
});
