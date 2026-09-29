import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import worker from "../src/index.js";
import * as store from "../src/room-store.js";

const FIXTURES = JSON.parse(
  readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8")
);
const HTTP_CASES = FIXTURES.filter((item) => item.layer === "http");
const DESKTOP_UPGRADE_CASES = FIXTURES.filter((item) => item.layer === "desktop-upgrade");
const ROOM = "0123456789abcdef0123456789abcdef";

function makeRuntimeStorage() {
  const db = new DatabaseSync(":memory:");
  const statements = [];
  const sql = {
    exec(query, ...params) {
      statements.push(query);
      const returnsRows = /^\s*(SELECT|PRAGMA)/i.test(query);
      const stmt = db.prepare(query);
      if (returnsRows) return stmt.all(...params);
      stmt.run(...params);
      return [];
    },
  };
  let alarmDeleted = false;
  let alarmScheduled = null;
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
    async deleteAlarm() {
      alarmDeleted = true;
    },
    // Provide setAlarm so scheduleNextTokenAlarm executes its scheduling
    // logic instead of returning early because the method is absent.
    // This exercises the hasTable guard for refresh_requests even when
    // the business schema does not exist. Recording the scheduled time
    // is sufficient; the mock does not need to fire the alarm.
    async setAlarm(timestamp) {
      alarmScheduled = timestamp;
    },
  };
  return { storage, statements, alarmWasDeleted: () => alarmDeleted, alarmScheduledAt: () => alarmScheduled };
}

function makeCtx(runtime = makeRuntimeStorage()) {
  const registry = [];
  return {
    runtime,
    ctx: {
      storage: runtime.storage,
      acceptWebSocket(ws, tags = []) {
        registry.push({ ws, tags });
      },
      getWebSockets(tag) {
        if (!tag) return registry.map((item) => item.ws);
        return registry.filter((item) => item.tags.includes(tag)).map((item) => item.ws);
      },
    },
  };
}

function makeFakeWs() {
  let attachment = null;
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
      attachment = value;
    },
    deserializeAttachment() {
      return attachment;
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
    }
  };
  globalThis.WebSocketPair = class TestWebSocketPair {
    constructor() {
      this[0] = makeFakeWs();
      this[1] = makeFakeWs();
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

function requestForFixture(item) {
  const body = item.request.body;
  const headers = new Headers(item.request.headers || {});
  let encodedBody;
  if (body !== undefined) {
    encodedBody = typeof body === "string" ? body : JSON.stringify(body);
    if (typeof body !== "string") headers.set("Content-Type", "application/json");
  }
  return new Request(`https://relay.example${item.request.path}`, {
    method: item.request.method,
    headers,
    body: encodedBody,
  });
}

function seedRoomState(room, preState) {
  const ownerHash = preState.owner_credential_hash ?? null;
  const tombstonedAt = preState.tombstoned_at ?? (preState.tombstoned || preState.owner === "tombstoned" ? 1 : null);
  room.sql.exec(
    "UPDATE room_state SET owner_credential_hash = ?, tombstoned_at = ?",
    ownerHash,
    tombstonedAt
  );
  if (preState.rate_limited) {
    room.sql.exec(
      "INSERT INTO claim_rate_limits (id, window_started_at, attempts) VALUES (1, ?, 30) " +
        "ON CONFLICT(id) DO UPDATE SET window_started_at = excluded.window_started_at, attempts = 30",
      Date.now()
    );
  }
}

test("wire-v1 http 层 13 条逐条驱动真实 edge/RoomDO.fetch 状态码", async (t) => {
  assert.equal(HTTP_CASES.length, 13);
  for (const item of HTTP_CASES) {
    await t.test(item.name, async () => {
      const { ctx } = makeCtx();
      const room = new RoomDO(ctx, {});
      // SEC-1：业务 schema 延到鉴权成功后才建，构造器不再无条件建表——这条
      // 用例只测 claim/delete 的 HTTP 状态码（不是 schema 时机本身），显式建
      // 好业务 schema 保持既有断言不受影响（rate_limited 分支下面会往
      // claim_rate_limits 直接写一行，这张表得先在）。
      store.ensureBusinessSchema(room.sql);
      seedRoomState(room, item.pre_state);

      let response;
      if (item.pre_state.rate_limited) {
        const ip = `192.0.2.${HTTP_CASES.indexOf(item) + 1}`;
        let stubCalls = 0;
        const env = {
          ROOMS: {
            idFromName(roomId) { return roomId; },
            get() {
              return { fetch() { stubCalls += 1; return new Response("warm", { status: 200 }); } };
            },
          },
        };
        for (let attempt = 0; attempt < 10; attempt += 1) {
          const warm = await worker.fetch(new Request(
            `https://relay.example/room/${ROOM}/claim`,
            { method: "POST", headers: { "CF-Connecting-IP": ip } }
          ), env);
          assert.equal(warm.status, 200);
        }
        const request = requestForFixture(item);
        request.headers.set("CF-Connecting-IP", ip);
        response = await worker.fetch(request, env);
        assert.equal(stubCalls, 10, "edge 429 must not instantiate/call the DO stub");
      } else {
        response = await room.fetch(requestForFixture(item));
      }

      assert.equal(response.status, item.expect.status);
    });
  }
});

test("wire-v1 desktop-upgrade 层 4 条逐条驱动真实 upgrade 鉴权与 epoch", async (t) => {
  assert.equal(DESKTOP_UPGRADE_CASES.length, 4);
  for (const item of DESKTOP_UPGRADE_CASES) {
    await t.test(item.name, async () => {
      await withUpgradeRuntime(async () => {
        const { ctx } = makeCtx();
        const room = new RoomDO(ctx, {});
        // SEC-1：这条用例测的是 upgrade 鉴权与 epoch 语义（不是 schema 时机），
        // getCurrentEpoch 要读 room_meta——鉴权成功前手动建好业务 schema，
        // 保持既有断言不受影响。
        store.ensureBusinessSchema(room.sql);
        seedRoomState(room, item.pre_state);
        const beforeEpoch = store.getCurrentEpoch(room.sql);
        const headers = { Upgrade: "websocket" };
        if (item.authorization) headers.Authorization = item.authorization;

        const response = await room.fetch(
          new Request(`https://relay.example/room/${ROOM}`, { headers })
        );

        if (!item.expect.accept) {
          assert.equal(response.status, item.expect.status);
          assert.equal(ctx.getWebSockets().length, 0);
          return;
        }
        assert.equal(response.status, 101);
        const accepted = ctx.getWebSockets(item.expect.role);
        assert.equal(accepted.length, 1);
        const attachment = accepted[0].deserializeAttachment();
        assert.equal(attachment.role, item.expect.role);
        assert.equal(attachment.registry_ready, false);
        assert.ok(attachment.registry_sync_deadline > attachment.connectedAt);
        assert.equal(accepted[0].sent.length, 0, "desktop must not receive replay.head before sync.ack");
        assert.equal(store.getCurrentEpoch(room.sql) > beforeEpoch, item.expect.epoch_bump);
      });
    });
  }
});

test("epoch bump 在新桌面 sync 前广播被取代桌面 offline，旧 close 不双发", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx } = makeCtx();
    const room = new RoomDO(ctx, {});
    const credential = "1".repeat(64);
    seedRoomState(room, {
      owner_credential_hash: "3138bb9bc78df27c473ecfd1410f7bd45ebac1f59cf3ff9cfe4db77aab7aedd3",
    });
    const desktopRequest = () => new Request(`https://relay.example/room/${ROOM}`, {
      headers: { Upgrade: "websocket", Authorization: `Bearer ${credential}` },
    });
    assert.equal((await room.fetch(desktopRequest())).status, 101);
    const oldDesktop = ctx.getWebSockets("desktop").at(-1);
    const observer = makeFakeWs();
    // S1ja F2：legacy 无 subject 的 remote 豁免已撤（canDeliverOutbound 现在
    // fail-closed）——presence 广播投递前必过这道闸，这里改用真实
    // registry-backed 的 remote attachment。generation 定为 2（不是 1）：本测试
    // 后面会喂两次空 entries 的 token.sync（revision 1、revision 2）——§9.4
    // 「省略即撤销」对任何 generation < revision 的存量 subject 都会强断其
    // socket，generation=1 活不过 revision=2 那次 sync，会把 observer 一并
    // 误杀，跟本测试真正要测的「桌面 offline/online 广播」无关。
    const observerNow = Date.now();
    const observerSubject = "device:44444444-4444-4444-8444-444444444444";
    store.putTokenRegistryEntry(room.sql, {
      subject: observerSubject,
      generation: 2,
      state: "active",
      scope: "remote",
      aliases: [{
        token_hash: "3".repeat(64),
        kind: "current",
        generation: 2,
        access_expires: observerNow + 60_000,
        valid_until: observerNow + 120_000,
      }],
    }, observerNow);
    observer.serializeAttachment({
      role: "remote",
      scope: "remote",
      subject: observerSubject,
      kind: "current",
      generation: 2,
      alias_generation: 2,
      access_expires: observerNow + 60_000,
      valid_until: observerNow + 120_000,
    });
    ctx.acceptWebSocket(observer, ["remote"]);
    await room.webSocketMessage(oldDesktop, JSON.stringify({
      t: "token.sync", revision: 1, entries: [],
    }));
    assert.equal(oldDesktop.deserializeAttachment().registry_ready, true);
    const oldEpoch = oldDesktop.deserializeAttachment().epoch;

    const response = await room.fetch(desktopRequest());
    assert.equal(response.status, 101);
    const newDesktop = ctx.getWebSockets("desktop").at(-1);
    assert.equal(newDesktop.deserializeAttachment().registry_ready, false);
    assert.ok(newDesktop.deserializeAttachment().epoch > oldEpoch);

    let desktopPresence = observer.sent.filter((frame) =>
      frame.t === "presence" && frame.role === "desktop"
    );
    assert.deepEqual(desktopPresence.map((frame) => frame.event), ["online", "offline"]);
    assert.equal(desktopPresence.filter((frame) => frame.event === "offline").length, 1);

    await room.webSocketClose(oldDesktop);
    desktopPresence = observer.sent.filter((frame) =>
      frame.t === "presence" && frame.role === "desktop"
    );
    assert.equal(desktopPresence.filter((frame) => frame.event === "offline").length, 1);

    await room.webSocketMessage(newDesktop, JSON.stringify({
      t: "token.sync", revision: 2, entries: [],
    }));
    desktopPresence = observer.sent.filter((frame) =>
      frame.t === "presence" && frame.role === "desktop"
    );
    assert.deepEqual(desktopPresence.map((frame) => frame.event), ["online", "offline", "online"]);
  });
});

// ============================================================================
// P0-d1（C1 设计 v0.5 §8·G4 关一半）：桌面重连抬升 epoch 后，relay 之前从不
// 通知已在线的 remote——旧 epoch 只能等下一次写入撞 stale_epoch 才发现代已经
// 变了。room-do.js:230 附近现在在 bump 落库后立即向 remote 广播明文帧
// `{t:"epoch.changed", epoch, ts}`，走 broadcastToRemotes 同族出站闸
// （canDeliverOutbound：只投给当前权威、kind="current" 的活 remote 连接）。
// 下面两条测试驱动**真实**桌面握手（同上面 epoch bump 广播 offline 那条一样
// 经 room.fetch() 走 §9.1 鉴权全链路，不是手搭 attachment），对已在线 remote
// socket 实际收到的帧做真断言。
// ============================================================================

test("P0-d1：桌面重连第二次握手抬升 epoch 后，已在线 remote 收到 epoch.changed 且 epoch 值正确", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx } = makeCtx();
    const room = new RoomDO(ctx, {});
    const credential = "1".repeat(64);
    seedRoomState(room, {
      owner_credential_hash: "3138bb9bc78df27c473ecfd1410f7bd45ebac1f59cf3ff9cfe4db77aab7aedd3",
    });
    const desktopRequest = () => new Request(`https://relay.example/room/${ROOM}`, {
      headers: { Upgrade: "websocket", Authorization: `Bearer ${credential}` },
    });

    // 第一次握手：房间刚起时的首次桌面上线（epoch 0 → 1）——此时房间里还没有
    // 任何 remote，这次 bump 广播天然无人可收，不干扰下面的断言。
    assert.equal((await room.fetch(desktopRequest())).status, 101);
    const epochAfterFirstConnect = store.getCurrentEpoch(room.sql);

    // 已在线的 remote：registry-backed subject，attachment 字段与注册表逐条
    // 一致才能过 isOfficialAttachmentLive（canDeliverOutbound 的活闸）——同上面
    // observer 夹具同款写法。
    const observer = makeFakeWs();
    const observerNow = Date.now();
    const observerSubject = "device:77777777-7777-4777-8777-777777777777";
    store.putTokenRegistryEntry(room.sql, {
      subject: observerSubject,
      generation: 1,
      state: "active",
      scope: "remote",
      aliases: [{
        token_hash: "7".repeat(64),
        kind: "current",
        generation: 1,
        access_expires: observerNow + 60_000,
        valid_until: observerNow + 120_000,
      }],
    }, observerNow);
    observer.serializeAttachment({
      role: "remote",
      scope: "remote",
      subject: observerSubject,
      kind: "current",
      generation: 1,
      alias_generation: 1,
      access_expires: observerNow + 60_000,
      valid_until: observerNow + 120_000,
    });
    ctx.acceptWebSocket(observer, ["remote"]);

    // 第二次握手：同一桌面重连，epoch 再抬升一次——这是 P0-d1 要修的场景。
    const sentBeforeSecondConnect = observer.sent.length;
    const response = await room.fetch(desktopRequest());
    assert.equal(response.status, 101);
    const epochAfterSecondConnect = store.getCurrentEpoch(room.sql);
    assert.ok(epochAfterSecondConnect > epochAfterFirstConnect, "第二次握手必须真的抬升了 epoch");

    const newFrames = observer.sent.slice(sentBeforeSecondConnect);
    const epochChangedFrames = newFrames.filter((frame) => frame.t === "epoch.changed");
    assert.equal(epochChangedFrames.length, 1, "已在线 remote 必须收到恰好一条 epoch.changed 广播");
    assert.equal(epochChangedFrames[0].epoch, epochAfterSecondConnect, "广播的 epoch 值必须等于抬升后的新值");
    assert.equal(typeof epochChangedFrames[0].ts, "number");
  });
});

test("fresh 构造器只建 room_state sentinel，业务 schema 延到鉴权成功后才建（SEC-1）", () => {
  const { ctx, runtime } = makeCtx();

  new RoomDO(ctx, {});

  const firstCreate = runtime.statements.find((statement) => /^CREATE TABLE/i.test(statement));
  assert.match(firstCreate, /CREATE TABLE IF NOT EXISTS room_state/i);
  const tableNames = runtime.storage.sql
    .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
    .map((row) => row.name);
  assert.deepEqual(tableNames, ["room_state"]);
});

test("SEC-1①：失败的 WS 鉴权（无令牌 / 错 bearer / 裸随机 token）不建任何业务表，room_id 不留痕", async () => {
  await withUpgradeRuntime(async () => {
    const noTokenRoom = new RoomDO(makeCtx().ctx, {});
    const noTokenRes = await noTokenRoom.fetch(
      new Request(`https://relay.example/room/${ROOM}`, { headers: { Upgrade: "websocket" } })
    );
    assert.equal(noTokenRes.status, 401);

    const { ctx: bearerCtx } = makeCtx();
    const wrongBearerRoom = new RoomDO(bearerCtx, {});
    seedRoomState(wrongBearerRoom, {
      owner_credential_hash: "3138bb9bc78df27c473ecfd1410f7bd45ebac1f59cf3ff9cfe4db77aab7aedd3",
    });
    const wrongBearerRes = await wrongBearerRoom.fetch(
      new Request(`https://relay.example/room/${ROOM}`, {
        headers: { Upgrade: "websocket", Authorization: `Bearer ${"9".repeat(64)}` },
      })
    );
    assert.equal(wrongBearerRes.status, 401);

    const { ctx: tokenCtx } = makeCtx();
    const randomTokenRoom = new RoomDO(tokenCtx, {});
    const randomTokenRes = await randomTokenRoom.fetch(
      new Request(`https://relay.example/room/${ROOM}`, {
        headers: { Upgrade: "websocket", "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${"e".repeat(64)}` },
      })
    );
    assert.equal(randomTokenRes.status, 401);

    for (const room of [noTokenRoom, wrongBearerRoom, randomTokenRoom]) {
      const tableNames = room.sql
        .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .map((row) => row.name);
      // room_meta（room_id 的落点）本身都没建出来——比「room_id 是 null」更强的
      // 断言：业务 schema 整个没碰，鉴权失败绝不留一丁点持久化痕迹。
      assert.deepEqual(tableNames, ["room_state"]);
    }
  });
});

test("SEC-1②：desktop bearer 鉴权成功后，13 张业务表齐现", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx } = makeCtx();
    const room = new RoomDO(ctx, {});
    const credential = "1".repeat(64);
    seedRoomState(room, {
      owner_credential_hash: "3138bb9bc78df27c473ecfd1410f7bd45ebac1f59cf3ff9cfe4db77aab7aedd3",
    });

    const response = await room.fetch(
      new Request(`https://relay.example/room/${ROOM}`, {
        headers: { Upgrade: "websocket", Authorization: `Bearer ${credential}` },
      })
    );
    assert.equal(response.status, 101);

    const tableNames = room.sql
      .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
      .map((row) => row.name);
    assert.deepEqual(tableNames, [
      "claim_rate_limits",
      "events",
      "message_rate_limits",
      "pairing_routes",
      "pending_input",
      "quota_counters",
      "refresh_requests",
      "reply_byte_limits",
      "reply_routes",
      "room_meta",
      "room_state",
      "token_aliases",
      "token_put_fingerprints",
      "token_subjects",
    ]);
  });
});

test("SEC-1③：remote 打空 schema 房（业务 schema 未建）→ 401，且不建表", async () => {
  await withUpgradeRuntime(async () => {
    const { ctx } = makeCtx();
    const room = new RoomDO(ctx, {});

    const response = await room.fetch(
      new Request(`https://relay.example/room/${ROOM}`, {
        headers: {
          Upgrade: "websocket",
          "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${"f".repeat(64)}`,
        },
      })
    );

    assert.equal(response.status, 401);
    const tableNames = room.sql
      .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
      .map((row) => row.name);
    assert.deepEqual(tableNames, ["room_state"]);
  });
});

// ============================================================================
// SEC-1 修复轮（skeptic 复现的真实阻断）：存量房——room_state 表已存在，但是
// 预 SEC-1 的旧结构（没有 ip_bucket_salt 列）。回归前的 bug：补列的守卫式
// ALTER 唯一挂在 initRoomStateSentinel 里，构造器只在
// `!hasTable(sql,"room_state")` 时才调它——存量房这张表已经在，构造器直接跳过
// 整个 sentinel 初始化，ALTER 永远不会跑，列永远补不上，deriveIpBucketKey（鉴权
// 前、任何 WS upgrade 都会先摸一次 room_state.ip_bucket_salt）必然撞
// `no such column: ip_bucket_salt` 崩掉——存量房的 WS upgrade 入口整个 brick。
// 修法：构造器不再拿 `!hasTable` 挡 initRoomStateSentinel，无条件调一次（内部
// 三步全幂等：CREATE TABLE IF NOT EXISTS / 守卫式 ALTER / INSERT ... WHERE NOT
// EXISTS，fresh 房三步全是空操作到真正建表，存量房三步是空操作/真正补列/空
// 操作）。这条测试必须真实走「构造器 + fetch() upgrade」这条生产路径（不能
// 直调 initRoomStateSentinel 绕过构造器守卫，也不能用 fresh 房——那样测不出
// 回归本身）。
// ============================================================================

test("SEC-1 修复轮：存量房（room_state 已存在但没有 ip_bucket_salt 列）走真实构造器 + 真实 fetch() upgrade 不崩，列被补齐，桶 key 能算", async () => {
  const { ctx, runtime } = makeCtx();
  // 手工搭一张「预 SEC-1」的存量 room_state——3 列，没有 ip_bucket_salt，模拟
  // 已部署 DO 在这次修复之前落的哨兵表。故意不调 store.initRoomStateSentinel
  // （那样会绕过构造器的 hasTable 守卫，测不出真实回归），也不用 fresh 房
  // （fresh 房 CREATE 自带列，从不会踩这条路径）。
  runtime.storage.sql.exec(`CREATE TABLE room_state (
    owner_credential_hash TEXT,
    tombstoned_at INTEGER,
    registry_floor INTEGER NOT NULL DEFAULT 0
  )`);
  runtime.storage.sql.exec(
    "INSERT INTO room_state (owner_credential_hash, tombstoned_at, registry_floor) VALUES (NULL, NULL, 0)"
  );
  const columnsBefore = runtime.storage.sql
    .exec("PRAGMA table_info(room_state)")
    .map((row) => row.name);
  assert.equal(columnsBefore.includes("ip_bucket_salt"), false, "迁移前先确认这张表确实还没有这一列");

  const room = new RoomDO(ctx, {}); // 真实构造器——不是直调 initRoomStateSentinel

  // ①「不崩」：deriveIpBucketKey 在鉴权前就会摸 room_state.ip_bucket_salt——
  // 回归前这一行会直接抛 SQL 异常，room.fetch() 整个 reject；这里没带任何
  // 凭据，鉴权照常按 401 收场，但过程不能抛异常。
  const response = await room.fetch(
    new Request(`https://relay.example/room/${ROOM}`, { headers: { Upgrade: "websocket" } })
  );
  assert.equal(response.status, 401);

  // ② 列被补上（PRAGMA 直接看列结构，不是只看某次读到的值是不是 null）。
  const columnsAfter = runtime.storage.sql
    .exec("PRAGMA table_info(room_state)")
    .map((row) => row.name);
  assert.equal(columnsAfter.includes("ip_bucket_salt"), true);

  // ③ deriveIpBucketKey 能算出桶 key（且盐已经落在 room_state 里、跨这次
  // fetch() 稳定）。存量房这次迁移会让盐旋转一次（迁移前 room_meta 里的旧盐
  // 不搬，直接在 room_state 里现生成一份新的）——ip_bucket_key 的全部消费方
  // （限速桶/refresh_requests.ip_bucket_key 只写不读比较）只要「同房间同 IP
  // 落同一个桶」这种迁移之后的相对稳定性，不要求跨迁移前后逐比特相等，旋转
  // 一次没有持久正确性/安全损害。
  const key = await room.deriveIpBucketKey("198.51.100.230");
  assert.match(key, /^[0-9a-f]{32}$/);
  assert.match(store.getRoomIpBucketSalt(room.sql), /^[0-9a-f]{64}$/);
});

// ============================================================================
// Existing rooms may have room_state with ip_bucket_salt but no created_at.
// The guarded ALTER for created_at must run in initRoomStateSentinel,
// which the constructor calls even when room_state already exists.
// Restricting the migration to the missing-table branch would leave
// existing rooms failing on upgrade requests with a missing-column error.
// Exercise the constructor and fetch() upgrade path against an existing
// table to verify the column is added and backfilled without a failure.
// Calling initRoomStateSentinel directly would bypass constructor guards;
// using a fresh room would miss the existing-table migration regression.
// ============================================================================

test("SEC-3 修复轮：存量房（room_state 已存在但没有 created_at 列）走真实构造器 + 真实 fetch() upgrade 不崩，created_at 列被补齐且回填非 NULL", async () => {
  const { ctx, runtime } = makeCtx();
  const before = Date.now();
  // 手工搭一张「预 SEC-3、后 SEC-1」的存量 room_state——4 列，带
  // ip_bucket_salt（SEC-1 已交付）但没有 created_at（SEC-3 新列）。
  runtime.storage.sql.exec(`CREATE TABLE room_state (
    owner_credential_hash TEXT,
    tombstoned_at INTEGER,
    registry_floor INTEGER NOT NULL DEFAULT 0,
    ip_bucket_salt TEXT
  )`);
  runtime.storage.sql.exec(
    "INSERT INTO room_state (owner_credential_hash, tombstoned_at, registry_floor, ip_bucket_salt) " +
      "VALUES (NULL, NULL, 0, ?)",
    "d".repeat(64)
  );
  const columnsBefore = runtime.storage.sql
    .exec("PRAGMA table_info(room_state)")
    .map((row) => row.name);
  assert.equal(columnsBefore.includes("created_at"), false, "迁移前先确认这张表确实还没有这一列");

  const room = new RoomDO(ctx, {}); // 真实构造器——不是直调 initRoomStateSentinel

  // ①「不崩」：没带任何凭据，鉴权照常按 401 收场，但过程不能抛异常——本单
  // 新加的 rejectUpgradeAuthentication 内 scheduleNextTokenAlarm() 调用也要
  // 在这条存量房路径上安全跑完（含它内部摸 refresh_requests 的 hasTable 守
  // 卫，业务 schema 这时还没建）。
  // The setAlarm mock lets scheduleNextTokenAlarm reach the hasTable guard
  // for refresh_requests instead of returning early for a missing method.
  // An upgrade response alone cannot establish that this guard ran:
  // an early return would also avoid throwing with no business schema.
  // Assert that scheduling records a safe integer reclamation deadline
  // to verify the scheduling path executes rather than silently skipping
  // the logic this fixture is intended to exercise.
  const response = await room.fetch(
    new Request(`https://relay.example/room/${ROOM}`, { headers: { Upgrade: "websocket" } })
  );
  assert.equal(response.status, 401);
  assert.ok(
    Number.isSafeInteger(runtime.alarmScheduledAt()),
    "scheduleNextTokenAlarm 应已跑完并真正排出回收候选（不是被早退的 no-op）"
  );

  // ② 列被补上（PRAGMA 直接看列结构，不是只看某次读到的值是不是 null）。
  const columnsAfter = runtime.storage.sql
    .exec("PRAGMA table_info(room_state)")
    .map((row) => row.name);
  assert.equal(columnsAfter.includes("created_at"), true);

  // ③ 回填非 NULL，且用的是「首次触碰」（这次构造）的时刻，不是某个陈旧/
  // 猜测值——落在这次构造前后的合理区间内。
  const after = Date.now();
  const state = store.getRoomState(room.sql);
  assert.ok(Number.isSafeInteger(state.created_at), "created_at 应被回填为非 NULL 的安全整数");
  assert.ok(
    state.created_at >= before && state.created_at <= after,
    "回填时刻应落在这次构造前后区间内，不是猜测的历史值"
  );
});

test("tombstoned 构造器只读 sentinel，且 fetch/message/close/alarm 全入口拒绝", async () => {
  const { ctx, runtime } = makeCtx();
  runtime.storage.sql.exec(`CREATE TABLE room_state (
    owner_credential_hash TEXT,
    tombstoned_at INTEGER,
    registry_floor INTEGER NOT NULL DEFAULT 0
  )`);
  runtime.storage.sql.exec(
    "INSERT INTO room_state (owner_credential_hash, tombstoned_at, registry_floor) VALUES (NULL, 123, 0)"
  );
  const room = new RoomDO(ctx, {});
  const tables = runtime.storage.sql
    .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
    .map((row) => row.name);
  assert.deepEqual(tables, ["room_state"]);

  const response = await room.fetch(
    new Request(`https://relay.example/room/${ROOM}/__admin/register-token`, { method: "POST" })
  );
  assert.equal(response.status, 410);

  for (const invoke of [
    (ws) => room.webSocketMessage(ws, "{}"),
    (ws) => room.webSocketClose(ws),
    (ws) => {
      ctx.acceptWebSocket(ws, ["remote"]);
      return room.alarm();
    },
  ]) {
    const ws = makeFakeWs();
    await invoke(ws);
    assert.equal(ws.closed.length, 1);
  }
});

test("DELETE 单事务清业务表但保留 room_state，提交后关 socket 并取消 alarm", async () => {
  const { ctx, runtime } = makeCtx();
  const room = new RoomDO(ctx, {});
  const credential = "1".repeat(64);
  seedRoomState(room, {
    owner_credential_hash: "3138bb9bc78df27c473ecfd1410f7bd45ebac1f59cf3ff9cfe4db77aab7aedd3",
  });
  // SEC-1：这条用例测的是 DELETE 事务清表语义（不是 schema 时机），构造器不再
  // 无条件建业务表——手动建好才能让下面几行直接落库的调用照旧工作。
  store.ensureBusinessSchema(room.sql);
  store.ensureRoomId(room.sql, ROOM);
  store.incrementQuotaCount(room.sql, "2026-08", 3);
  const now = Date.now();
  store.putTokenRegistryEntry(room.sql, {
    subject: "device:22222222-2222-4222-8222-222222222222",
    generation: 1,
    state: "active",
    scope: "remote",
    aliases: [{ token_hash: "a".repeat(64), kind: "current", generation: 1, access_expires: now + 1_000, valid_until: now + 2_000 }],
  }, now);
  const ws = makeFakeWs();
  ctx.acceptWebSocket(ws, ["remote"]);

  const response = await room.fetch(
    new Request(`https://relay.example/room/${ROOM}`, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${credential}` },
    })
  );

  assert.equal(response.status, 200);
  const businessTables = runtime.storage.sql.exec(
    "SELECT name FROM sqlite_master WHERE type = 'table' AND name <> 'room_state' AND name NOT LIKE 'sqlite_%'"
  );
  for (const { name } of businessTables) {
    assert.equal(runtime.storage.sql.exec(`SELECT COUNT(*) AS n FROM "${name}"`)[0].n, 0, `${name} must be empty`);
  }
  assert.equal(runtime.storage.sql.exec("SELECT COUNT(*) AS n FROM room_state")[0].n, 1);
  assert.notEqual(runtime.storage.sql.exec("SELECT tombstoned_at FROM room_state")[0].tombstoned_at, null);
  assert.equal(ws.closed.length, 1);
  assert.equal(runtime.alarmWasDeleted(), true);
});

test("?role=desktop 不再授予 desktop；legacy ?token= 查询参数不再有任何准入路径 → 401", async () => {
  // S1ja F1 后门退役：这条曾经的回归对照钉死「?role=desktop 拿不到 desktop 身份，
  // 只能靠 valid_tokens 降级成 remote」——那条降级路径本身（legacy `?token=` 准入 +
  // room_meta.valid_tokens）随本单一并删除，现在唯一的准入路径是 Bearer（desktop）
  // 或 §9.1 子协议 token（remote）；裸查询参数请求现在必须 401，不留任何回落。
  await withUpgradeRuntime(async () => {
    const { ctx } = makeCtx();
    const room = new RoomDO(ctx, {});

    const response = await room.fetch(
      new Request(`https://relay.example/room/${ROOM}?role=desktop&token=dev-token`, {
        headers: { Upgrade: "websocket" },
      })
    );

    assert.equal(response.status, 401);
    assert.equal(ctx.getWebSockets("desktop").length, 0);
    assert.equal(ctx.getWebSockets("remote").length, 0);
    // SEC-1：鉴权失败不建业务 schema——room_meta（epoch 计数器的落点）压根
    // 不存在，比「epoch 仍是 0」更强的断言：epoch 追踪从未被初始化过。
    assert.equal(store.hasTable(room.sql, "room_meta"), false);
  });
});

test("claim 首次登记计一次；同哈希重试幂等且不烧 30/h 持久桶", async () => {
  const runtime = makeRuntimeStorage();
  const { ctx } = makeCtx(runtime);
  let room = new RoomDO(ctx, {});
  const credentialHash = "2".repeat(64);
  const makeClaim = () =>
    new Request(`https://relay.example/room/${ROOM}/claim`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ v: 1, credential_hash: credentialHash }),
    });

  assert.equal((await room.fetch(makeClaim())).status, 200);
  assert.equal((await room.fetch(makeClaim())).status, 200);
  assert.equal(runtime.storage.sql.exec("SELECT attempts FROM claim_rate_limits WHERE id = 1")[0].attempts, 1);

  room = new RoomDO(ctx, {}); // 模拟休眠后重建，桶与 owner 都来自持久 SQL。
  assert.equal((await room.fetch(makeClaim())).status, 200);
  assert.equal(runtime.storage.sql.exec("SELECT attempts FROM claim_rate_limits WHERE id = 1")[0].attempts, 1);
});

test("P2-①：DO 30/h 桶前 30 次放行业务判定、第 31 次 429，且排在 assertRoomLive 之后", async () => {
  const liveRuntime = makeRuntimeStorage();
  const { ctx: liveCtx } = makeCtx(liveRuntime);
  const liveRoom = new RoomDO(liveCtx, {});
  const makeClaim = (attempt) => new Request(`https://relay.example/room/${ROOM}/claim`, {
    method: "POST",
    body: JSON.stringify({ v: 1, credential_hash: attempt.toString(16).padStart(64, "0") }),
  });
  for (let attempt = 1; attempt <= 30; attempt += 1) {
    const response = await liveRoom.fetch(makeClaim(attempt));
    assert.equal(response.status, attempt === 1 ? 200 : 409, `attempt ${attempt} must pass the 30/h gate`);
  }
  assert.equal((await liveRoom.fetch(makeClaim(31))).status, 429);
  assert.equal(liveRuntime.storage.sql.exec(
    "SELECT attempts FROM claim_rate_limits WHERE id = 1"
  )[0].attempts, 30);

  const goneRuntime = makeRuntimeStorage();
  const { ctx: goneCtx } = makeCtx(goneRuntime);
  const goneRoom = new RoomDO(goneCtx, {});
  // SEC-1：seedRoomState 的 rate_limited 分支直接写 claim_rate_limits，构造器
  // 不再无条件建业务表——手动建好，保持这条 429/410 优先级用例不受影响。
  store.ensureBusinessSchema(goneRoom.sql);
  seedRoomState(goneRoom, { owner: "tombstoned", tombstoned: true, rate_limited: true });
  assert.equal((await goneRoom.fetch(makeClaim(32))).status, 410);
});

test("DELETE 事务中任一业务表清理失败会整体回滚，且不提前关 socket/删 alarm", async () => {
  const { ctx, runtime } = makeCtx();
  const room = new RoomDO(ctx, {});
  seedRoomState(room, {
    owner_credential_hash: "3138bb9bc78df27c473ecfd1410f7bd45ebac1f59cf3ff9cfe4db77aab7aedd3",
  });
  // SEC-1：这条用例测的是 DELETE 事务回滚语义（不是 schema 时机），构造器不再
  // 无条件建业务表——手动建好才能让下面几行直接落库的调用照旧工作。
  store.ensureBusinessSchema(room.sql);
  store.ensureRoomId(room.sql, ROOM);
  store.incrementQuotaCount(room.sql, "2026-08", 1);
  runtime.storage.sql.exec(`CREATE TRIGGER fail_quota_delete
    BEFORE DELETE ON quota_counters BEGIN SELECT RAISE(ABORT, 'forced rollback'); END`);
  const ws = makeFakeWs();
  ctx.acceptWebSocket(ws, ["remote"]);

  await assert.rejects(
    room.fetch(
      new Request(`https://relay.example/room/${ROOM}`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${"1".repeat(64)}` },
      })
    ),
    /forced rollback/
  );

  // SEC-1：ip_bucket_salt 已从 room_meta 迁到 room_state；这条 DELETE 路径
  // 从不经过 deriveIpBucketKey（handleDelete 在 fetch() 里比 upgrade 分支
  // 更早 return），room_meta 里现在只有 ensureRoomId 写的 room_id 这一行。
  assert.deepEqual(
    runtime.storage.sql.exec("SELECT key FROM room_meta ORDER BY key").map((row) => row.key),
    ["room_id"]
  );
  assert.equal(runtime.storage.sql.exec("SELECT COUNT(*) AS n FROM quota_counters")[0].n, 1);
  assert.equal(runtime.storage.sql.exec("SELECT tombstoned_at FROM room_state")[0].tombstoned_at, null);
  assert.equal(ws.closed.length, 0);
  assert.equal(runtime.alarmWasDeleted(), false);
});
