import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";
import { validateEnvelope } from "../src/envelope.js";
import { currentPeriod } from "../src/quota.js";

// room-do.js 是骨架里风险最高、也是唯一之前零单测的文件（它是消息路由 + 角色
// 方向 + 配额降级这几块「真正决定谁能对谁做什么」的地方）。它依赖 Cloudflare
// Workers 运行时全局（WebSocketPair、ctx.acceptWebSocket 的 Hibernation
// API），本机没有真实 DO 环境跑不了——这里不是去模拟那套运行时，而是给
// RoomDO 需要的两个外部依赖（`ctx.storage.sql` / `ctx.getWebSockets` +
// `ctx.acceptWebSocket`）搭一个足够薄、行为忠实的替身，然后直接驱动
// `fetch()`（鉴权分支）和 `webSocketMessage()`（消息路由分支）——这两个是
// RoomDO 真正暴露给外界的入口，替身只顶掉它们依赖的运行时基础设施，业务
// 逻辑本身一行没改。
//
// 没有替身的部分：`fetch()` 里 `new WebSocketPair()` 到 `return new
// Response(null, {status:101, webSocket:client})` 这一段（握手成功后的
// 101 响应）本身没法在 Node 里构造——所以这里不测「握手成功」这条路径的
// 101 响应本身，只测握手成功前会跑到的东西（token 校验、ensureRoomId 时机）
// 和握手成功后会做的事（用 connect() 手工搭一个等价状态，见下方）。

const ROOM = "b".repeat(32);
const CT = Buffer.from("hello ciphertext").toString("base64");
const N12 = Buffer.alloc(12, 7).toString("base64"); // 12 字节 nonce
const wireFixtures = JSON.parse(
  readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8"),
);

function wireFrame(name) {
  const fixture = wireFixtures.find((item) => item.name === name);
  assert.ok(fixture, `${name}: fixture exists`);
  return fixture.frame;
}

// ---- sql 适配器：与 test/room-store.test.js 同款薄适配器，包一个真实
// node:sqlite 数据库，满足 room-do.js 里 cfSqlAdapter 期望的
// `ctx.storage.sql.exec(query, ...params) -> 可迭代的行` 形状。 ----
function makeStorageSql() {
  const db = new DatabaseSync(":memory:");
  return {
    exec(query, ...params) {
      const isSelect = /^\s*(SELECT|PRAGMA)/i.test(query);
      const stmt = db.prepare(query);
      if (isSelect) return stmt.all(...params);
      stmt.run(...params);
      return [];
    },
    // F2-A：deleteAll 替身——生产 SQLite-backed DO 的 storage.deleteAll() 清空
    // 整个私有 SQLite 数据库（SQL 表 + KV 数据，全部、原子）。这里复用同一个
    // `db` 引用把当前全部用户表 DROP 掉（排除 sqlite_ 内部表），落回「一张表
    // 都没有」的空库状态——不是换一个新 db 对象，跟 hibernation 测试复用同一
    // 个 storageSql/db 是同一种忠实度：之后同 storageSql 上重建的 RoomDO 实例
    // 的 `CREATE TABLE IF NOT EXISTS` 是在这同一个物理库上重新建表。
    _dropAllTables() {
      const tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .all();
      for (const { name } of tables) {
        db.exec(`DROP TABLE "${String(name).replaceAll('"', '""')}"`);
      }
    },
  };
}

// ---- mock ctx：Hibernation API 只用到两个方法——acceptWebSocket(ws, tags)
// 登记一个连接、getWebSockets(tag?) 按 tag 查在线连接。用一个数组当登记表
// 就够了，不需要真正的 WebSocket 语义（这里没有网络，只有对象引用）。 ----
// SEC-3：alarmLog——之前这个文件的 storage mock 没有 setAlarm/deleteAlarm，
// scheduleNextTokenAlarm 见 `typeof ctx.storage.setAlarm !== "function"` 直接
// 早退（no-op），alarm() 回收枝要断言的「到点删没删」也够不到。补一对会记录
// 的薄替身（同 room-lifecycle-fixture.test.js/s1f1-fixture.test.js 既有姿势），
// 供 SEC-3 回收测试读 ctx.alarmLog；其余既有测试不读这个字段，行为不受影响。
function makeCtx(storageSql = makeStorageSql()) {
  const registry = []; // { ws, tags: string[] }
  // Count actual setAlarm calls to verify that repeated requests avoid
  // redundant storage writes. scheduleNextTokenAlarm checks getAlarm()
  // before scheduling and skips setAlarm when the existing alarm is
  // scheduled for the same time or earlier.
  const alarmLog = { scheduled: null, deleted: false, setAlarmCalls: 0, deleteAllCalled: false };
  return {
    storage: {
      sql: storageSql,
      transactionSync(callback) {
        return callback();
      },
      async setAlarm(timestamp) {
        alarmLog.scheduled = timestamp;
        alarmLog.setAlarmCalls += 1;
      },
      async getAlarm() {
        return alarmLog.scheduled;
      },
      async deleteAlarm() {
        alarmLog.deleted = true;
        alarmLog.scheduled = null;
      },
      // F2-A：deleteAll——alarm() 回收枝改走 storage.deleteAll() 彻底清房后要
      // 断言「真的清了」，这里记一个标志位；实际清表动作转给 storageSql
      // 的 _dropAllTables()（同一个 db 引用，见上面 makeStorageSql 注释）。
      async deleteAll() {
        storageSql._dropAllTables();
        alarmLog.deleteAllCalled = true;
      },
    },
    alarmLog,
    acceptWebSocket(ws, tags = []) {
      registry.push({ ws, tags });
    },
    getWebSockets(tag) {
      if (!tag) return registry.map((r) => r.ws);
      return registry.filter((r) => r.tags.includes(tag)).map((r) => r.ws);
    },
    // S1i3 K2-1：测试专用——模拟一条连接真的断开。真实 Durable Object Hibernation
    // API 里，socket 断线后 getWebSockets() 自然就不会再吐出它；这个薄替身本身不会
    // 自动做这件事，用这个方法手动把它从登记表摘掉，才能驱动 deliverToPairingRoute
    // 「目标 socket 已断」那个分支（room-do.js 的 `if (!target) return`）。
    disconnectWebSocket(ws) {
      const index = registry.findIndex((r) => r.ws === ws);
      if (index !== -1) registry.splice(index, 1);
    },
  };
}

// ---- 假 WebSocket：room-do.js 只用到 send / serializeAttachment /
// deserializeAttachment 三个方法。`sent` 数组供测试直接断言收到了什么。 ----
function makeFakeWs() {
  let attachment = null;
  const sent = [];
  return {
    sent,
    send(text) {
      sent.push(typeof text === "string" ? JSON.parse(text) : text);
    },
    close() {},
    serializeAttachment(obj) {
      attachment = obj;
    },
    deserializeAttachment() {
      return attachment;
    },
  };
}

function makeRoom(env = {}) {
  const ctx = makeCtx();
  const room = new RoomDO(ctx, env);
  // SEC-1：构造器不再无条件建业务 schema（延到鉴权成功后才建）——这个文件
  // 里大量测试构造完就直接调 bumpEpoch/insertMilestone/putTokenRegistryEntry
  // 等需要业务表的 store 函数，不经过真实 fetch() 鉴权，手动建好保持既有
  // 断言不受影响。ensureBusinessSchema 幂等，重复调用安全。
  store.ensureBusinessSchema(room.sql);
  return { room, ctx };
}

// S1i3：pair.hello/pair.done 盖章 + pair.accept/pair.ready 定向投递都靠
// attachment.connection_id 找目标——生产路径里这是 fetch() 握手时
// globalThis.crypto.randomUUID() 现生成的（room-do.js:265）；这里手工搭连接同样
// 必须带上这个字段，否则盖章值会是 undefined（JSON.stringify 会把它整个键静默
// 丢掉，测试会以为「没盖章也一样」而测不出回归）。不显式传 connectionId 时用一个
// 自增计数器兜底，保证同一个测试里多次 connect() 天然不会撞号。
let autoConnectionIdCounter = 0;

// 手工搭一个「握手已完成」的连接：跟 fetch() 里握手成功后实际会做的事
// （bumpEpoch/读 epoch、ctx.acceptWebSocket(ws,[role])、写 attachment）保持
// 同样的语义，只是不经过 new WebSocketPair()/Response(101) 那段没法在
// Node 里复刻的部分。
// R6：每次调用给 scope="remote" 分支现领一个独立 subject（S1ja F2 修复
// 时补的）——这意味着 §9.8 的 SUBJECT_SOCKET_LIMIT（并发 socket ≤4/subject）
// 在这个 helper 上永远不会触发（同一测试里连再多次 connect() 也是各自不同
// 的 subject）。要测并发配额相关行为，别用这个 helper，得显式给多条连接
// 共用同一个 subject。
function connect(room, ctx, role, {
  lastSeq = 0,
  scope = role === "desktop" ? "desktop" : "remote",
  connectionId,
  // scope:"pairing" 写的是单行共享 subject（"pairing"）——同一测试里 connect() 两条
  // pairing 连接（S1i3 定向投递的旁观者测试要这么做）时，若各自现取 Date.now()，第二次
  // 调用会用一个可能不同的时刻重写同一行的 access_expires/valid_until，第一条连接的
  // attachment 里存的是旧时刻，isOfficialAttachmentLive 逐字段比对就会不等而判死连接
  // ——不是必现，但是真实存在的计时竞态。调用方要开两条 pairing 连接时显式传同一个
  // now，从根上让这行数据不可能不一致。
  now = Date.now(),
} = {}) {
  const ws = makeFakeWs();
  const epoch = role === "desktop" ? store.bumpEpoch(room.sql) : store.getCurrentEpoch(room.sql);
  ctx.acceptWebSocket(ws, [role]);
  const autoId = ++autoConnectionIdCounter;
  const attachment = {
    epoch,
    role,
    scope,
    lastSeq,
    connectedAt: now,
    connection_id: connectionId ?? `auto-conn-${autoId}`,
    ...(role === "desktop" ? { registry_ready: true } : {}),
  };
  if (scope === "pairing") {
    store.putTokenRegistryEntry(room.sql, {
      subject: "pairing",
      generation: 1,
      state: "active",
      scope: "pairing",
      aliases: [{ token_hash: "9".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 60_000 }],
    }, now);
    Object.assign(attachment, { subject: "pairing", kind: "current", generation: 1, alias_generation: 1, access_expires: now + 60_000, valid_until: now + 60_000 });
  } else if (scope === "remote") {
    // S1ja F2：legacy 无 subject 的 remote 豁免已撤（authorizeInboundSocket/
    // canDeliverOutbound 现在两处都 fail-closed）——这个 helper 是全文件几十个
    // 测试共用的「随便接一个能收发的 remote 连接」夹具，必须落一个真实
    // registry-backed subject 才能继续代表「一个已授权的远端连接」；专门测试
    // legacy（无 subject）该被拒的用例仍直接手搭 attachment，不走这个分支。
    const subject = `device:11111111-1111-4111-8111-${String(autoId).padStart(12, "0")}`;
    store.putTokenRegistryEntry(room.sql, {
      subject,
      generation: 1,
      state: "active",
      scope: "remote",
      aliases: [{
        token_hash: String(autoId).padStart(64, "0"),
        kind: "current",
        generation: 1,
        access_expires: now + 60_000,
        valid_until: now + 120_000,
      }],
    }, now);
    Object.assign(attachment, {
      subject,
      kind: "current",
      generation: 1,
      alias_generation: 1,
      access_expires: now + 60_000,
      valid_until: now + 120_000,
    });
  }
  ws.serializeAttachment(attachment);
  return ws;
}

function envelope(overrides = {}) {
  const kind = overrides.kind ?? "event";
  return {
    v: 1,
    room: ROOM,
    epoch: 1,
    kind,
    session: "s1",
    command_id: null,
    seq: null,
    client_msg_id: kind === "event" ? "cmid-test-default" : null,
    ct: CT,
    n: N12,
    ts: Date.now(),
    ...overrides,
  };
}

function req(url, init) {
  return new Request(url, init);
}

async function send(room, ws, payload) {
  await room.webSocketMessage(ws, JSON.stringify(payload));
}

function lastSent(ws) {
  return ws.sent[ws.sent.length - 1];
}

// ============================================================================
// fetch()：鉴权 401 / M2 鉴权前置
// ============================================================================
//
// S1ja F1 后门退役：__admin/register-token + ADMIN_TOKEN 路由已整个删除（不是
// 改形状——功能不复存在），原先钉死这条路由 404/200 行为的两个用例随之删除；
// legacy `?token=` 查询参数/`valid_tokens` 准入分支同批删除，下面两条测试改为
// 钉死「裸 `?token=` 请求现在没有任何准入路径、恒 401」。

test("fetch：无令牌请求 WS 升级 → 401", async () => {
  const { room } = makeRoom();
  const res = await room.fetch(req(`https://relay.example/room/${ROOM}`, { headers: { Upgrade: "websocket" } }));
  assert.equal(res.status, 401);
});

test("fetch：裸 ?token= 查询参数请求 WS 升级 → 401（legacy 准入路径已删，不再有任何回落）", async () => {
  const { room } = makeRoom();
  const res = await room.fetch(
    req(`https://relay.example/room/${ROOM}?token=anything`, { headers: { Upgrade: "websocket" } })
  );
  assert.equal(res.status, 401);
});

test("M2：鉴权失败的请求不会触发 ensureRoomId（room_meta 不留痕）", async () => {
  const { room } = makeRoom();
  // 不带任何合法凭据——没有 Bearer、没有正确子协议，S1ja 起唯一准入路径
  // （子协议 token）在这里直接拒绝；如果 ensureRoomId 还在鉴权前面跑，
  // 这里 room_id 会被写进去。
  const res = await room.fetch(
    req(`https://relay.example/room/${ROOM}?token=anything`, { headers: { Upgrade: "websocket" } })
  );
  assert.equal(res.status, 401);
  assert.equal(store.getMeta(room.sql, "room_id"), null);
});

// ============================================================================
// 握手同步：desktop 主动 hello / remote replayTo 回归对照
// ============================================================================

test("desktop registry_ready 激活后发送 replay.head（epoch/headSeq 与 store 现状一致）", () => {
  const { room } = makeRoom();
  const epoch = store.bumpEpoch(room.sql);
  store.insertMilestone(room.sql, { epoch, session: "s1", kind: "event", ct: CT, n: N12, ts: 1 });
  store.insertMilestone(room.sql, { epoch, session: "s1", kind: "event", ct: CT, n: N12, ts: 2 });
  const ws = makeFakeWs();

  room.desktopHello(ws, epoch);

  assert.deepEqual(ws.sent[0], { t: "replay.head", epoch, headSeq: 2 });
});

test("remote 分支 replayTo 首帧仍是 replay.head（回归对照）", () => {
  const { room } = makeRoom();
  const epoch = store.bumpEpoch(room.sql);
  store.insertMilestone(room.sql, { epoch, session: "s1", kind: "event", ct: CT, n: N12, ts: 1 });
  const ws = makeFakeWs();
  // S1ja F2：legacy 无 subject 的 remote 豁免已撤（canDeliverOutbound 现在
  // fail-closed）——replayTo 内部投递前必过这道闸，这里改用真实
  // registry-backed 的 remote attachment，跟 connect() helper 同一套形状。
  const now = Date.now();
  const subject = "device:22222222-2222-4222-8222-222222222222";
  store.putTokenRegistryEntry(room.sql, {
    subject,
    generation: 1,
    state: "active",
    scope: "remote",
    aliases: [{ token_hash: "1".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 }],
  }, now);
  ws.serializeAttachment({
    role: "remote",
    scope: "remote",
    subject,
    kind: "current",
    generation: 1,
    alias_generation: 1,
    access_expires: now + 60_000,
    valid_until: now + 120_000,
  });

  room.replayTo(ws, 0);

  assert.deepEqual(ws.sent[0], { t: "replay.head", epoch, headSeq: 1 });
});

// C1-PS（dogfood 修障第二批·手机发消息桌面离线无反馈）：remote 腿刚接入时
// 拿不到当前桌面在线态快照——presence 帧只在状态变化时广播，新连接接入前
// 发生的任何变化它都是盲的。room-do.js fetch() 在 replayTo 之后为
// scope="remote" 连接定向调用 sendDesktopPresenceSnapshot；这里直接测这个
// 被提取出来的方法本身（同 desktopHello/replayTo 既有先例——101 响应本身
// 没法在 Node 里构造真实 WebSocketPair，这个文件测的是 fetch() 握手成功后
// 会做的事，不是握手过程本身）。
test("sendDesktopPresenceSnapshot：桌面在线时投一条 presence online 给这条新 remote 连接", () => {
  const { room, ctx } = makeRoom();
  connect(room, ctx, "desktop"); // registry_ready: true，epoch -> 1
  const remoteWs = connect(room, ctx, "remote");

  room.sendDesktopPresenceSnapshot(remoteWs, store.getCurrentEpoch(room.sql));

  const frame = lastSent(remoteWs);
  assert.equal(frame.t, "presence");
  assert.equal(frame.role, "desktop");
  assert.equal(frame.event, "online");
});

test("sendDesktopPresenceSnapshot：桌面离线时投一条 presence offline", () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote"); // 没有桌面连接

  room.sendDesktopPresenceSnapshot(remoteWs, store.getCurrentEpoch(room.sql));

  const frame = lastSent(remoteWs);
  assert.equal(frame.t, "presence");
  assert.equal(frame.role, "desktop");
  assert.equal(frame.event, "offline");
});

test("sendDesktopPresenceSnapshot：只投给这条新 socket，不广播给房间里其它已在线的远端", () => {
  const { room, ctx } = makeRoom();
  connect(room, ctx, "desktop");
  const bystanderWs = connect(room, ctx, "remote");
  const newRemoteWs = connect(room, ctx, "remote");

  room.sendDesktopPresenceSnapshot(newRemoteWs, store.getCurrentEpoch(room.sql));

  assert.equal(bystanderWs.sent.length, 0, "旁观者远端不该收到这条定向快照");
  assert.equal(newRemoteWs.sent.length, 1);
  assert.equal(lastSent(newRemoteWs).event, "online");
});

test("旧库重启迁移后 replayTo 补发的历史 event 带合法 legacy client_msg_id", () => {
  const storageSql = makeStorageSql();
  storageSql.exec(`CREATE TABLE events (
    seq     INTEGER PRIMARY KEY,
    epoch   INTEGER NOT NULL,
    session TEXT,
    kind    TEXT NOT NULL,
    ct      TEXT NOT NULL,
    n       TEXT NOT NULL,
    ts      INTEGER NOT NULL
  )`);
  storageSql.exec(`CREATE TABLE room_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
  )`);
  storageSql.exec("INSERT INTO room_meta (key, value) VALUES (?, ?)", "room_id", ROOM);
  storageSql.exec("INSERT INTO room_meta (key, value) VALUES (?, ?)", "current_epoch", "4");
  storageSql.exec("INSERT INTO room_meta (key, value) VALUES (?, ?)", "seq_counter", "1");
  storageSql.exec(
    "INSERT INTO events (seq, epoch, session, kind, ct, n, ts) VALUES (?, ?, ?, ?, ?, ?, ?)",
    1,
    4,
    "legacy-session",
    "event",
    CT,
    N12,
    1234
  );

  const ctx = makeCtx(storageSql);
  const room = new RoomDO(ctx, {}); // 构造模拟 DO 重启（SEC-1 后只建 room_state 哨兵）
  // SEC-1：业务 schema（含 events 的 client_msg_id 守卫式迁移）延到鉴权成功后
  // 才建，构造器不再无条件跑 initSchema——显式调 ensureBusinessSchema 驱动同一
  // 套迁移逻辑，模拟这个旧库在鉴权成功后才真正迁移的时机。
  store.ensureBusinessSchema(room.sql);
  const ws = makeFakeWs();
  // S1ja F2：legacy 无 subject 的 remote 豁免已撤——同上一个测试，改用真实
  // registry-backed 的 remote attachment 才能让 canDeliverOutbound 放行。
  const now = Date.now();
  const subject = "device:33333333-3333-4333-8333-333333333333";
  store.putTokenRegistryEntry(room.sql, {
    subject,
    generation: 1,
    state: "active",
    scope: "remote",
    aliases: [{ token_hash: "2".repeat(64), kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 }],
  }, now);
  ws.serializeAttachment({
    role: "remote",
    scope: "remote",
    subject,
    kind: "current",
    generation: 1,
    alias_generation: 1,
    access_expires: now + 60_000,
    valid_until: now + 120_000,
  });
  room.replayTo(ws, 0);

  assert.deepEqual(ws.sent[0], { t: "replay.head", epoch: 4, headSeq: 1 });
  const replayed = ws.sent[1];
  assert.equal(typeof replayed.client_msg_id, "string");
  assert.notEqual(replayed.client_msg_id, "");
  const validation = validateEnvelope(replayed);
  assert.equal(validation.ok, true, JSON.stringify(validation.errors));
  assert.ok(!validation.errors?.includes("client_msg_id_required_for_kind"));
});

// ============================================================================
// 消息路由：里程碑落库 vs live 只转发 / stale_epoch 拒写
// ============================================================================

test("kind=event 落库盖 seq 并广播给全房间（含发送方）；kind=live 只转发不落库", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const remoteWs = connect(room, ctx, "remote");

  await send(room, desktopWs, envelope({ kind: "event", epoch: 1 }));
  assert.equal(store.headSeq(room.sql), 1); // 真落库了
  assert.equal(lastSent(desktopWs).kind, "event");
  assert.equal(lastSent(desktopWs).seq, 1); // 发送方自己也收到 seq 确认
  assert.equal(lastSent(remoteWs).kind, "event");
  assert.equal(lastSent(remoteWs).seq, 1);

  await send(room, desktopWs, envelope({ kind: "live", epoch: 1 }));
  assert.equal(store.headSeq(room.sql), 1); // live 没有让 seq 往前走，说明没落库
  assert.equal(lastSent(remoteWs).kind, "live"); // 远端收到了转发
  assert.equal(lastSent(remoteWs).seq, null);
  // 发送方自己不应该收到自己发的 live 回声（broadcastRaw 排除了 excludeWs=desktopWs）。
  assert.equal(lastSent(desktopWs).kind, "event"); // 桌面最后一条还是上面那条 event 确认，不是 live
});

test("stale_epoch：epoch 落后于房间当前 epoch 的 event 写入被拒、不消耗 seq", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  store.bumpEpoch(room.sql); // 模拟另一个桌面连接顶替上来，房间 epoch -> 2

  await send(room, desktopWs, envelope({ kind: "event", epoch: 1 })); // 还在用旧 epoch
  assert.equal(lastSent(desktopWs).reason, "stale_epoch");
  assert.equal(lastSent(desktopWs).currentEpoch, 2);
  assert.equal(store.headSeq(room.sql), 0); // 没有真的落库
});

test("kind=event：未来 epoch 回发 stale_epoch、不落库", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1

  await send(room, desktopWs, envelope({ kind: "event", epoch: 2 }));
  assert.equal(lastSent(desktopWs).reason, "stale_epoch");
  assert.equal(lastSent(desktopWs).currentEpoch, 1);
  assert.equal(store.headSeq(room.sql), 0); // 没有真的落库
});

// ============================================================================
// input 通道：在线直转 / 离线暂存 / ack 清队列
// ============================================================================

test("kind=input：桌面在线时直转，不进 pending_input 表", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote");

  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-online", seq: null }));

  assert.equal(lastSent(desktopWs).kind, "input");
  assert.equal(lastSent(desktopWs).command_id, "cmd-online");
  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 0); // 在线直转，没有暂存
});

test("kind=input：未来 epoch 回发 stale_epoch、不转发桌面且不暂存", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const remoteWs = connect(room, ctx, "remote");

  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-future", seq: null, epoch: 2 }));

  assert.deepEqual(lastSent(remoteWs), { t: "error", reason: "stale_epoch", currentEpoch: 1 });
  assert.equal(desktopWs.sent.length, 0);
  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 0);
});

test("kind=input：桌面离线时暂存进 pending_input 表", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote"); // 没有桌面连接

  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-offline", seq: null, epoch: 0 }));

  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 1);
  assert.equal(deliverable[0].command_id, "cmd-offline");
});

// C1-RQ（dogfood 修障第二批·手机发消息桌面离线无反馈）：桌面离线暂存成功后
// 现在要回一条 input.relay_queued（不再一声不吭）——手机端才能知道「消息没
// 丢，正在排队等桌面回来」而不是傻等一个永远不来的确认。
test("kind=input：桌面离线暂存成功后回 input.relay_queued（带 command_id + expires_at）", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote");

  const before = Date.now();
  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-relay-queued", seq: null, epoch: 0 }));

  const reply = lastSent(remoteWs);
  assert.equal(reply.t, "input.relay_queued");
  assert.equal(reply.command_id, "cmd-relay-queued");
  assert.ok(
    Number.isSafeInteger(reply.expires_at) && reply.expires_at > before,
    "expires_at 应是一个真实的、晚于入队时刻的未来到期时刻"
  );
});

test("kind=input：同一 command_id 幂等重发也回 input.relay_queued（既存行的 expires_at，不重插）", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote");

  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-retry", seq: null, epoch: 0 }));
  const first = lastSent(remoteWs);
  assert.equal(first.t, "input.relay_queued");

  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-retry", seq: null, epoch: 0 }));
  const second = lastSent(remoteWs);

  assert.equal(second.t, "input.relay_queued");
  assert.equal(second.command_id, "cmd-retry");
  assert.equal(second.expires_at, first.expires_at, "幂等命中应回既存行原本的 expires_at，不是重新算出的新时刻");

  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 1, "幂等重试不该在表里多插一行");
});

test("kind=input：桌面在线直转时不回 input.relay_queued（只有离线暂存才回）", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote");

  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-online-2", seq: null }));

  assert.equal(desktopWs.sent.length, 1, "桌面应收到直转的 envelope");
  assert.equal(remoteWs.sent.length, 0, "在线直转路径不该给发送方任何 relay_queued/ack 回执");
});

test("input.ack：桌面确认后清空 pending_input 里对应的行，并把回执广播给远端", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote");
  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-ack", seq: null })); // 暂存一条

  const desktopWs = connect(room, ctx, "desktop");
  const ack = { t: "input.ack", command_id: "cmd-ack", outcome: "ok" };
  await send(room, desktopWs, ack);

  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 0);
  assert.deepEqual(lastSent(remoteWs), ack);
});

// ============================================================================
// control 通道：在线直转 / stale_epoch 拒绝
// ============================================================================

test("kind=control：桌面在线且 epoch 为当前值时原样转发", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const remoteWs = connect(room, ctx, "remote");
  const controlEnvelope = envelope({
    kind: "control",
    command_id: "cmd-ctrl-1",
    epoch: 1,
  });

  await send(room, remoteWs, controlEnvelope);

  assert.deepEqual(lastSent(desktopWs), controlEnvelope);
});

test("kind=control：未来 epoch 回发 stale_epoch 给发送端且不转发桌面", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const remoteWs = connect(room, ctx, "remote");
  const futureControl = envelope({
    kind: "control",
    command_id: "cmd-future-ctrl",
    epoch: 2,
  });

  await send(room, remoteWs, futureControl);

  assert.deepEqual(lastSent(remoteWs), { t: "error", reason: "stale_epoch", currentEpoch: 1 });
  assert.equal(desktopWs.sent.length, 0);
});

test("kind=control：旧新 desktop 并存时只投递给当前 epoch 的连接", async () => {
  const { room, ctx } = makeRoom();
  const oldDesktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const newDesktopWs = connect(room, ctx, "desktop"); // epoch -> 2
  const remoteWs = connect(room, ctx, "remote");
  const controlEnvelope = envelope({
    kind: "control",
    command_id: "cmd-newdesk",
    epoch: 2,
  });

  await send(room, remoteWs, controlEnvelope);

  assert.deepEqual(lastSent(newDesktopWs), controlEnvelope);
  assert.equal(oldDesktopWs.sent.length, 0);
});

test("kind=control：陈旧 epoch 回发 stale_epoch 给发送端且不转发桌面", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const remoteWs = connect(room, ctx, "remote");
  store.bumpEpoch(room.sql); // 模拟桌面被顶替，房间 epoch -> 2
  const staleControl = envelope({
    kind: "control",
    command_id: "cmd-ctrl-stale",
    epoch: 1,
  });

  await send(room, remoteWs, staleControl);

  assert.deepEqual(lastSent(remoteWs), { t: "error", reason: "stale_epoch", currentEpoch: 2 });
  assert.equal(desktopWs.sent.length, 0);
});

// ============================================================================
// H1：role 强制方向——违反方向一律拒绝 + 回错误帧，不静默丢弃
// ============================================================================

test("H1：role=remote 发 kind=event 被拒，且不落库", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote");

  await send(room, remoteWs, envelope({ kind: "event", epoch: 1 }));

  assert.equal(lastSent(remoteWs).reason, "role_forbidden");
  assert.equal(lastSent(remoteWs).kind, "event");
  assert.equal(store.headSeq(room.sql), 0);
});

test("H1：role=remote 发 kind=live 被拒，且不转发", async () => {
  const { room, ctx } = makeRoom();
  const attackerRemote = connect(room, ctx, "remote");
  const bystanderRemote = connect(room, ctx, "remote");

  await send(room, attackerRemote, envelope({ kind: "live", seq: null }));

  assert.equal(lastSent(attackerRemote).reason, "role_forbidden");
  assert.equal(lastSent(attackerRemote).kind, "live");
  assert.equal(bystanderRemote.sent.length, 0); // 完全没收到
});

test("H1：role=desktop 发 kind=input 被拒，且不入队", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");

  await send(room, desktopWs, envelope({ kind: "input", command_id: "cmd-x", seq: null }));

  assert.equal(lastSent(desktopWs).reason, "role_forbidden");
  assert.equal(lastSent(desktopWs).kind, "input");
  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 0);
});

test("H1：input.ack 来自非 desktop（remote）被拒，队列不清空", async () => {
  const { room, ctx } = makeRoom();
  const bystanderRemote = connect(room, ctx, "remote");
  await send(room, bystanderRemote, envelope({ kind: "input", command_id: "cmd-guard", seq: null, epoch: 0 }));

  const attackerRemote = connect(room, ctx, "remote");
  await send(room, attackerRemote, { t: "input.ack", command_id: "cmd-guard", outcome: "ok" });

  assert.equal(lastSent(attackerRemote).reason, "role_forbidden");
  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 1); // 没被清掉
});

test("H1：control.notify_hint 来自非 desktop（remote）被拒，不广播给其它远端", async () => {
  const { room, ctx } = makeRoom();
  const attackerRemote = connect(room, ctx, "remote");
  const bystanderRemote = connect(room, ctx, "remote");

  await send(room, attackerRemote, { t: "control.notify_hint", category: "fake-alert" });

  assert.equal(lastSent(attackerRemote).reason, "role_forbidden");
  assert.equal(bystanderRemote.sent.length, 0); // 完全没收到
});

test("R3（返工·硬化）：远端伪造 role:\"desktop\" 的 presence 被拒绝，且不广播给房间内其它连接", async () => {
  const { room, ctx } = makeRoom();
  const attackerRemote = connect(room, ctx, "remote");
  const bystanderRemote = connect(room, ctx, "remote");

  await send(room, attackerRemote, { t: "presence", role: "desktop", event: "offline" });

  assert.equal(lastSent(attackerRemote).reason, "role_forbidden");
  assert.equal(lastSent(attackerRemote).frame, "presence");
  assert.equal(bystanderRemote.sent.length, 0, "伪造帧不该被广播出去——别的手机不该看到这条假的桌面下线横幅");
  // 「含计违规」——不是单纯回错误就完事，同 scope 矩阵外帧那条既有样式一样计入违规计数
  // （攒够 PROTOCOL_VIOLATION_LIMIT 次才踢连接，见 recordProtocolViolation 头注）。
  assert.equal(room.socketProtocolViolations.get(attackerRemote), 1, "伪造 desktop presence 应计入这条 socket 的违规计数");
  assert.equal(room.pendingProtocolViolations, 1, "同一次调用也应计入房间聚合违规计数（尚未攒够批量落库）");
});

test("R3 防误伤：远端广播自己真实角色（role:\"remote\"）的 presence 不受影响，正常放行且不计违规", async () => {
  const { room, ctx } = makeRoom();
  const sender = connect(room, ctx, "remote");
  const observer = connect(room, ctx, "remote");

  await send(room, sender, { t: "presence", role: "remote", event: "online" });

  assert.equal(observer.sent.length, 1, "正常的远端自报 presence 应照旧广播给其它在线连接");
  assert.equal(observer.sent[0].t, "presence");
  assert.equal(room.socketProtocolViolations.get(sender), undefined, "正常路径不应计入违规");
});

test("正向对照：role=desktop 发 kind=event / role=remote 发 kind=input 都正常放行", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote");

  await send(room, desktopWs, envelope({ kind: "event", epoch: 1 }));
  assert.equal(store.headSeq(room.sql), 1);

  await send(room, remoteWs, envelope({ kind: "input", command_id: "cmd-ok", seq: null }));
  assert.equal(lastSent(desktopWs).command_id, "cmd-ok");
});

test("正向对照：role=desktop 发 kind=live 正常转发给 remote", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote");

  await send(room, desktopWs, envelope({ kind: "live", epoch: 1 }));

  assert.equal(lastSent(remoteWs).kind, "live");
  assert.equal(lastSent(remoteWs).seq, null);
});

// ============================================================================
// H3：配额超限——断 live、保里程碑；quota.exceeded 同时广播给远端
// ============================================================================

test("H3：配额超限时 kind=live 被降级丢弃，quota.exceeded 同时广播给桌面和远端", async () => {
  const { room, ctx } = makeRoom({ MONTHLY_MILESTONE_LIMIT: 1 });
  store.incrementQuotaCount(room.sql, currentPeriod(Date.now()), 1); // 已经用满这个月的配额
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote");

  await send(room, desktopWs, envelope({ kind: "live", epoch: 1 }));

  assert.equal(lastSent(desktopWs).t, "quota.exceeded");
  assert.equal(lastSent(desktopWs).channel, "live");
  assert.equal(lastSent(remoteWs).t, "quota.exceeded"); // 远端也收到了，不是只回桌面
  assert.equal(lastSent(remoteWs).channel, "live");
});

test("H3：配额超限时 kind=event（里程碑）仍然正常落库——超限只断 live，不断里程碑", async () => {
  const { room, ctx } = makeRoom({ MONTHLY_MILESTONE_LIMIT: 1 });
  store.incrementQuotaCount(room.sql, currentPeriod(Date.now()), 1); // 已经用满
  const desktopWs = connect(room, ctx, "desktop");

  await send(room, desktopWs, envelope({ kind: "event", epoch: 1 }));

  assert.equal(store.headSeq(room.sql), 1); // 照常落库，没有被配额挡下
  assert.equal(lastSent(desktopWs).kind, "event");
  assert.equal(lastSent(desktopWs).seq, 1);
});

// ============================================================================
// client_msg_id 去重（v1.7.4）：命中不重插、不重播、不重复计数配额
// ============================================================================

test("kind=event：相同 client_msg_id 二次发送命中去重——只回发送方既有 seq 确认、不广播全房间、不重复计数配额", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const remoteWs = connect(room, ctx, "remote");

  await send(room, desktopWs, envelope({ kind: "event", epoch: 1, client_msg_id: "cmid-dedup-1" }));
  assert.equal(store.headSeq(room.sql), 1);
  const firstSeq = lastSent(desktopWs).seq;
  assert.equal(lastSent(remoteWs).seq, firstSeq); // 首次正常广播全房间（含远端）

  const remoteSentBefore = remoteWs.sent.length;
  const period = currentPeriod(Date.now());
  const quotaBefore = store.getQuotaCount(room.sql, period);

  await send(room, desktopWs, envelope({ kind: "event", epoch: 1, client_msg_id: "cmid-dedup-1", ts: Date.now() + 1 }));

  assert.equal(store.headSeq(room.sql), 1); // 没有分配新 seq
  assert.equal(lastSent(desktopWs).seq, firstSeq); // 发送方收到既有 seq 确认
  assert.equal(remoteWs.sent.length, remoteSentBefore); // 远端没有收到第二次广播
  assert.equal(store.getQuotaCount(room.sql, period), quotaBefore); // 配额没有重复累加
});

test("kind=event：不同 client_msg_id 各自正常落库、各自广播", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // epoch -> 1
  const remoteWs = connect(room, ctx, "remote");

  await send(room, desktopWs, envelope({ kind: "event", epoch: 1, client_msg_id: "cmid-a" }));
  await send(room, desktopWs, envelope({ kind: "event", epoch: 1, client_msg_id: "cmid-b" }));

  assert.equal(store.headSeq(room.sql), 2);
  assert.equal(lastSent(remoteWs).seq, 2);
});

// ============================================================================
// pair.*：配对握手三帧转发 + role 方向强制
// S1i3 F1（§9.5 第 235 行）：relay 转发 pair.hello/pair.done 时必须盖章
// origin_connection_id 并持久写路由表；pair.accept/pair.ready 撤回广播、改按路由表
// 定向投递。以下测试全部经 `room.webSocketMessage` 真路径驱动（不是手写校验器），
// 其中两条直接消费 wire fixture 的 pair_hello_forward_origin_valid /
// pair_done_forward_origin_valid 样张，把「relay 转发出的帧长这样」钉死在真实转发
// 代码产出的形状上。
// ============================================================================

test("pair.hello：relay 转发前盖章 origin_connection_id（wire 样张 pair_hello_forward_origin_valid 驱动真路径）", async () => {
  const { room, ctx } = makeRoom();
  // 样张里的 origin_connection_id 是 relay 转发后才有的值——手机实际发出的帧不带这个
  // 字段（relay 才盖），这里剥掉它、用样张里同一个值当这条连接的 connection_id，让
  // relay 盖章后产出的帧应当跟样张逐字节一致。
  const fixture = wireFrame("pair_hello_forward_origin_valid");
  const { origin_connection_id: expectedConnectionId, ...clientSent } = fixture;
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: expectedConnectionId });
  const desktopWs = connect(room, ctx, "desktop");

  await send(room, remoteWs, clientSent);

  assert.deepEqual(lastSent(desktopWs), fixture, "relay 盖出来的帧必须跟 wire 样张逐字节一致");
});

test("pair.hello：手机若自己夹带 origin_connection_id，relay 必须用自己认定的值覆盖（绝不信客户端自报）", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: "real-conn-42" });
  const desktopWs = connect(room, ctx, "desktop");
  const forged = {
    t: "pair.hello",
    room: ROOM,
    remote_pub: "remote-public-key",
    token_ct: CT,
    token_n: N12,
    origin_connection_id: "forged-conn-99",
  };

  await send(room, remoteWs, forged);

  assert.equal(lastSent(desktopWs).origin_connection_id, "real-conn-42");
});

test("pair.hello：desktop 发送被 role_forbidden 拒绝", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");

  await send(room, desktopWs, { t: "pair.hello", room: ROOM, remote_pub: "forged", token_ct: CT, token_n: N12 });

  assert.deepEqual(lastSent(desktopWs), {
    t: "error",
    reason: "role_forbidden",
    frame: "pair.hello",
    role: "desktop",
  });
});

test("pair.hello：desktop 离线时回 desktop_offline 且不暂存", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing" });

  await send(room, remoteWs, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });

  assert.deepEqual(lastSent(remoteWs), { t: "error", reason: "desktop_offline" });
  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.equal(deliverable.length, 0);
});

test("pair.done：relay 转发前盖章 origin_connection_id（wire 样张 pair_done_forward_origin_valid 驱动真路径）", async () => {
  const { room, ctx } = makeRoom();
  const fixture = wireFrame("pair_done_forward_origin_valid");
  const { origin_connection_id: expectedConnectionId, ...clientSent } = fixture;
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: expectedConnectionId });
  const desktopWs = connect(room, ctx, "desktop");

  await send(room, remoteWs, clientSent);

  assert.deepEqual(lastSent(desktopWs), fixture, "relay 盖出来的帧必须跟 wire 样张逐字节一致");
});

test("pair.done：desktop 发送被 role_forbidden 拒绝", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");

  await send(room, desktopWs, { t: "pair.done", room: ROOM, device_id: "device-1" });

  assert.deepEqual(lastSent(desktopWs), {
    t: "error",
    reason: "role_forbidden",
    frame: "pair.done",
    role: "desktop",
  });
});

test("pair.done：手机在 accept→ready 窗口掉线重连，新连接重放 done 后路由随之搬家，ready 投给新连接而不是已断的旧连接（K1 验收：不给 done 补 upsert 这条必红）", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const now = Date.now();
  const conn1 = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-mobile-1", now });

  await send(room, conn1, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });
  assert.equal(lastSent(desktopWs).t, "pair.hello"); // 路由此刻 = conn-mobile-1

  await send(room, desktopWs, wireFrame("pair_accept_encrypted_tokens_valid"));
  assert.deepEqual(lastSent(conn1), wireFrame("pair_accept_encrypted_tokens_valid")); // accept 投给 conn-mobile-1

  // 手机真的掉线：从 relay 的在线连接登记表摘掉（不是只走 webSocketClose 回调——
  // 真实 Durable Object Hibernation API 断线后 getWebSockets() 自然就不会再吐出它）。
  ctx.disconnectWebSocket(conn1);

  // 手机换一条新连接重放 done：真机重连后 connection_id 是握手现生成的新随机值，
  // 跟 conn-mobile-1 不是同一个——§9.5 明写 pair.done 幂等可重放正是为了兜住这种
  // 掉线重连（accept→ready 窗口内断线时唯一的恢复路）。
  const conn2 = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-mobile-2", now });
  await send(room, conn2, { t: "pair.done", room: ROOM, device_id: "device-1", confirm_ct: CT, confirm_n: N12 });
  assert.equal(lastSent(desktopWs).t, "pair.done");
  assert.equal(lastSent(desktopWs).origin_connection_id, "conn-mobile-2", "盖章值必须是重放 done 的新连接");

  // 桌面确认完成后发 ready：deliverToPairingRoute 必须按路由投给 conn-mobile-2。
  // 若 pair.done 没有把路由行搬过来，路由此刻仍指向已断的 conn-mobile-1——
  // getWebSockets() 里已经找不到它，撞进「目标 socket 已断」分支（room-do.js 的
  // `if (!target) return`）被静默丢弃，conn-mobile-2 永远收不到 ready，用
  // deepEqual 而不是 `.t` 取属性断言：路由没搬时 conn2 什么都没收到，
  // lastSent(conn2) 是 undefined，deepEqual 对着 undefined 比较会给出真实的断言
  // 失败（不是 TypeError 崩溃）。
  await send(room, desktopWs, wireFrame("pair_ready_valid"));
  assert.deepEqual(
    lastSent(conn2),
    wireFrame("pair_ready_valid"),
    "ready 必须投给重放 done 的新连接，不能因路由仍指向已断连接被静默丢弃"
  );
  assert.equal(conn1.sent.at(-1).t, "pair.accept", "已断的旧连接停留在收到 accept 那一刻，不该再收到任何新帧");
});

test("pair.done：同窗第二台手机的 hello 抢了路由，第一台重放 done 后路由拉回来，ready 落回第一台、第二台一个字节都收不到（K1 第二条后果验收：不给 done 补 upsert 这条必红）", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const now = Date.now();
  const mobileA = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-mobile-a", now });
  const mobileB = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-mobile-b", now });

  await send(room, mobileA, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });
  assert.equal(lastSent(desktopWs).origin_connection_id, "conn-mobile-a"); // 路由此刻 = A

  await send(room, desktopWs, wireFrame("pair_accept_encrypted_tokens_valid"));
  assert.deepEqual(lastSent(mobileA), wireFrame("pair_accept_encrypted_tokens_valid")); // accept 投给 A

  // 同一窗口内第二台手机也发 hello：relay 这层无条件转发+落路由（真实桌面客户端
  // 在 SentAccept 态会自己忽略这条转发来的 hello，但那是桌面应用层状态，不是
  // relay 的行为——relay 端的路由表已经被这次 hello 无条件搬到了 B，这正是 K1
  // 第二条后果的病灶：真正完成配对的 A 若不能靠 done 把路由拉回来，就会被饿死）。
  await send(room, mobileB, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key-b", token_ct: CT, token_n: N12 });
  assert.equal(lastSent(desktopWs).origin_connection_id, "conn-mobile-b"); // 路由被 B 抢走

  // A（真正完成了配对的那一台）重放 done：若 done 不把路由行写回 A，路由仍然
  // 指着抢路由的 B。
  await send(room, mobileA, { t: "pair.done", room: ROOM, device_id: "device-1", confirm_ct: CT, confirm_n: N12 });
  assert.equal(lastSent(desktopWs).origin_connection_id, "conn-mobile-a", "done 必须把路由拉回真正完成配对的 A");

  await send(room, desktopWs, wireFrame("pair_ready_valid"));
  assert.deepEqual(
    lastSent(mobileA),
    wireFrame("pair_ready_valid"),
    "ready 必须落回重放 done 的 A，不能被 B 那次 hello 抢走的路由带偏"
  );
  assert.equal(mobileB.sent.length, 0, "同窗第二台手机（抢了一次路由但没完成配对）不该收到任何帧，包括 ready");
});

// ---- pair.accept / pair.ready：定向投递（§9.5 第 235 行，撤回 broadcastToRemotes）----
// 路由表只在 pair.hello 转发成功时才会 upsert，所以下面每条测试都先手工驱动一遍
// pair.hello（模拟真实握手顺序），路由行才存在，pair.accept/pair.ready 才投得出去。

test("pair.accept：定向投递给完成了 pair.hello 握手的那个 remote", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-pairing-remote" });
  await send(room, remoteWs, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });
  const payload = wireFrame("pair_accept_encrypted_tokens_valid");

  await send(room, desktopWs, payload);

  assert.deepEqual(lastSent(remoteWs), payload);
});

test("pair.accept：同窗第二个 remote（没做过 pair.hello）收不到别人的配对结果", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  // 两条 pairing 连接共用同一个 now：见 connect() 上的注释，避免各自现取
  // Date.now() 在共享的 "pairing" subject 行上产生计时竞态。
  const now = Date.now();
  const pairingRemoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-pairing-owner", now });
  const bystanderRemoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-pairing-bystander", now });
  await send(room, pairingRemoteWs, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });
  const payload = wireFrame("pair_accept_encrypted_tokens_valid");

  await send(room, desktopWs, payload);

  assert.deepEqual(lastSent(pairingRemoteWs), payload, "发起配对的那个 remote 必须收到");
  assert.equal(bystanderRemoteWs.sent.length, 0, "旁观的第二个 remote 一个字节都不该收到（撤回广播）");
});

test("pair.accept：从未发生过配对（无路由行）时安全丢弃，不报错也不广播", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing" });
  const payload = wireFrame("pair_accept_encrypted_tokens_valid");

  await send(room, desktopWs, payload);

  assert.equal(remoteWs.sent.length, 0);
  assert.equal(desktopWs.sent.length, 0, "没有校验错误，desktop 自己也不该收到任何回执");
});

test("pair.accept：缺 tokens_ct 按 wire 样张拒绝", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  connect(room, ctx, "remote", { scope: "pairing" });

  await send(room, desktopWs, wireFrame("pair_accept_tokens_ct_missing"));

  assert.deepEqual(lastSent(desktopWs), {
    t: "error",
    reason: "invalid_pair_accept",
    errors: ["tokens_ct_required"],
  });
});

test("pair.accept：明文令牌字段按 wire 样张拒绝且不转发", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing" });

  await send(room, desktopWs, wireFrame("pair_accept_plaintext_tokens_forbidden"));

  assert.deepEqual(lastSent(desktopWs), {
    t: "error",
    reason: "invalid_pair_accept",
    errors: ["plaintext_token_forbidden"],
  });
  assert.equal(remoteWs.sent.length, 0);
});

test("pair.ready：定向投递给完成了 pair.hello 握手的那个 remote", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const remoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-ready-remote" });
  await send(room, remoteWs, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });
  const payload = wireFrame("pair_ready_valid");

  await send(room, desktopWs, payload);

  assert.deepEqual(lastSent(remoteWs), payload);
});

test("pair.ready：同窗第二个 remote（没做过 pair.hello）收不到别人的 ready", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop");
  const now = Date.now();
  const pairingRemoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-ready-owner", now });
  const bystanderRemoteWs = connect(room, ctx, "remote", { scope: "pairing", connectionId: "conn-ready-bystander", now });
  await send(room, pairingRemoteWs, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });
  const payload = wireFrame("pair_ready_valid");

  await send(room, desktopWs, payload);

  assert.deepEqual(lastSent(pairingRemoteWs), payload, "发起配对的那个 remote 必须收到");
  assert.equal(bystanderRemoteWs.sent.length, 0, "旁观的第二个 remote 一个字节都不该收到（撤回广播）");
});

test("休眠唤醒安全：pairing_routes 落库跨 RoomDO 实例存活，重建实例后定向投递仍能命中（S1i3 K2-2，不是只证明表被建出来）", async () => {
  // S1i3 F1 那 9 条新测试全在同一个 RoomDO 实例内跑完，只结构性地证明了这张表被建了
  // 出来（见 room-lifecycle-fixture.test.js 的「fresh 构造器」测试）。room-do.js 里
  // 转发 pair.hello 的注释明说这张表存在的理由是「DO 休眠后内存 Map 会丢」——这条补的
  // 正是这个盲区：同一份 storage 上重建 RoomDO 实例（`new RoomDO(ctx, {})` 第二次调用，
  // 手法同上面「旧库重启迁移」那条测试：构造即模拟 DO 被淘汰后再唤醒、构造函数重跑一
  // 遍，不带上一个实例任何 JS 层字段），路由行必须原样还在，定向投递必须还能靠它命中。
  const storageSql = makeStorageSql();
  const ctx = makeCtx(storageSql);
  const roomBeforeHibernation = new RoomDO(ctx, {});
  // SEC-1：构造器不再无条件建业务 schema——下面 connect() 会直接调
  // bumpEpoch/putTokenRegistryEntry，手动建好（同一份 storageSql 持久共享，
  // 之后 roomAfterHibernation 复用同一张物理表，不需要再建一次）。
  store.ensureBusinessSchema(roomBeforeHibernation.sql);
  const now = Date.now();
  // ctx（含 getWebSockets 登记表）在两个 RoomDO 实例间共享：真实 Hibernation API 下
  // 已连接的 socket 不会因为 DO 的 JS 对象被重建就消失——消失的只是 DO 实例自己内存
  // 里的状态（比如以前那个内存 Map，正是这张表要顶替的东西）。
  const desktopWs = connect(roomBeforeHibernation, ctx, "desktop", { now });
  const remoteWs = connect(roomBeforeHibernation, ctx, "remote", { scope: "pairing", connectionId: "conn-hibernate", now });

  await send(roomBeforeHibernation, remoteWs, { t: "pair.hello", room: ROOM, remote_pub: "remote-public-key", token_ct: CT, token_n: N12 });
  assert.equal(lastSent(desktopWs).t, "pair.hello", "唤醒前：hello 正常转发、路由已写入");
  assert.equal(store.getPairingRoute(roomBeforeHibernation.sql), "conn-hibernate");

  const roomAfterHibernation = new RoomDO(ctx, {});

  assert.equal(
    store.getPairingRoute(roomAfterHibernation.sql),
    "conn-hibernate",
    "路由行必须落库、跨实例重建原样存活（不是内存 Map，重建后不会清空）"
  );

  await send(roomAfterHibernation, desktopWs, wireFrame("pair_accept_encrypted_tokens_valid"));

  assert.deepEqual(
    lastSent(remoteWs),
    wireFrame("pair_accept_encrypted_tokens_valid"),
    "重建实例后，定向投递仍必须能靠落库的路由行命中原连接——不能只靠「表存在」的结构性论证"
  );
});

test("pair.accept：remote 发送被 role_forbidden 拒绝", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote");

  await send(room, remoteWs, { t: "pair.accept", room: ROOM, device_id: "device-1", k_room_ct: CT, k_room_n: N12 });

  assert.deepEqual(lastSent(remoteWs), {
    t: "error",
    reason: "role_forbidden",
    frame: "pair.accept",
    role: "remote",
  });
});

// ============================================================================
// P0-d1：epoch.changed——relay 出站广播帧，remote scope 白名单
// （INBOUND_SCOPE_MATRIX.remote，room-do.js:103）里没有它，remote 若把它当
// 入站帧发进来必须落既有「未知帧型/矩阵拒绝」路径（同上面 pair.accept 的
// role_forbidden 一致，不需要新增显式拒绝项）。
// ============================================================================

test("epoch.changed：remote 若把它当入站帧发进来，被矩阵拒绝（不在 remote scope 白名单内）", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote");

  await send(room, remoteWs, { t: "epoch.changed", epoch: 99, ts: Date.now() });

  assert.deepEqual(lastSent(remoteWs), {
    t: "error",
    reason: "role_forbidden",
    frame: "epoch.changed",
    role: "remote",
  });
});

test("epoch.changed：desktop 若把它当入站帧发进来，同样被矩阵拒绝（不在 desktop scope 白名单内）", async () => {
  const { room, ctx } = makeRoom();
  const desktopWs = connect(room, ctx, "desktop"); // connect() 已把 registry_ready 设 true

  await send(room, desktopWs, { t: "epoch.changed", epoch: 99, ts: Date.now() });

  assert.deepEqual(lastSent(desktopWs), {
    t: "error",
    reason: "role_forbidden",
    frame: "epoch.changed",
    role: "desktop",
  });
});

// ============================================================================
// SEC-3：未认领空房经 alarm 自杀回收。room-do.js 没有导出这个常量，这里镜像
// 同一个值（20 分钟）——改动两边任一处务必同步，否则本节测试的固定时刻断言
// 会失真。
// ============================================================================

const UNCLAIMED_RECLAIM_MS = 20 * 60 * 1000;

test("SEC-3：未认领裸房经失败 upgrade fetch() 武装固定回收 alarm；到点 alarm() 自杀回收", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  const initialState = store.getRoomState(room.sql);
  assert.equal(initialState.owner_credential_hash, null);
  assert.ok(Number.isSafeInteger(initialState.created_at), "构造器应已把 created_at 回填成安全整数");

  // 失败 upgrade（无凭据）——brief 原话例子之一，走 rejectUpgradeAuthentication。
  const res = await room.fetch(req(`https://relay.example/room/${ROOM}`, { headers: { Upgrade: "websocket" } }));
  assert.equal(res.status, 401);
  assert.equal(
    ctx.alarmLog.scheduled,
    initialState.created_at + UNCLAIMED_RECLAIM_MS,
    "应排在 created_at + UNCLAIMED_RECLAIM_MS 这个固定时刻"
  );

  // 快进：直接把 created_at 拨到回收窗口之前，不真的等 20 分钟。
  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  await room.alarm();

  // F2-A：到点回收改彻底清房——不再是 tombstone（room_state 保留一行墓碑），
  // 而是整个私有 SQLite 数据库被清空，room_state 表本身也不存在了，不能再
  // 调 store.getRoomState(room.sql)（会撞 "no such table: room_state"）。用
  // sqlite_master 直接证明「一张表都不剩」，不只信一个 mock 标志位。
  const remainingTables = room.sql.exec("SELECT name FROM sqlite_master WHERE type = 'table'");
  assert.deepEqual(remainingTables, [], "deleteAll 后整个私有数据库应清空，一张表都不该剩");
  assert.equal(ctx.alarmLog.deleteAllCalled, true, "alarm() 回收枝应调用 storage.deleteAll");
  assert.equal(ctx.alarmLog.deleted, true, "alarm() 回收枝应调用 deleteAlarm");
});

test("SEC-3：已 claim 空房（owner 非 null、无连接）到点 alarm() 不被回收（误杀防线核心断言）", async () => {
  const { room } = makeRoom();
  const claimResult = store.claimRoom(room.sql, "a".repeat(64), Date.now());
  assert.equal(claimResult, "claimed");

  // 把 created_at 拨到远早于回收窗口之前——若误杀防线失守，这里会被回收。
  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  await room.alarm();

  const state = store.getRoomState(room.sql);
  assert.equal(state.owner_credential_hash, "a".repeat(64));
  assert.equal(state.tombstoned_at, null, "已 claim 的空房永远不该被 SEC-3 回收枝误杀");
});

test("SEC-3：claim 抢在回收 alarm 到点之前提交 → alarm() 实时重查 owner，到点不杀（claim 赢的竞态分支）", async () => {
  const { room, ctx } = makeRoom();
  // 造出「已经过了回收窗口」的未认领房，模拟 alarm 即将到点前一刻。
  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  // 竞态的「claim 先」这一支：DO 单线程、claim 与 alarm 到点之间不交错——
  // 这里用调用顺序直接体现「claim 先提交」。
  const result = store.claimRoom(room.sql, "b".repeat(64), Date.now());
  assert.equal(result, "claimed");

  await room.alarm();

  const state = store.getRoomState(room.sql);
  assert.equal(state.tombstoned_at, null, "claim 先提交，alarm 到点应放过这个房（不信调度快照，实时重查 owner）");
  assert.equal(ctx.alarmLog.deleted, false);
});

test("F2-A：回收 alarm 抢在 claim 之前触发 → 彻底清房后同 id 不是永久 410，而是全新未认领房（reclaim 赢的竞态分支·新语义）", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  await room.alarm();
  assert.equal(ctx.alarmLog.deleteAllCalled, true, "到点应先被彻底清房（不再是墓碑式 tombstone）");

  // 生产语义：deleteAll 后 DO 才真正具备被 CF 回收资格，下一次同 id 请求打
  // 进来会是全新 DO 实例（同一份物理 storage，构造器重新跑一遍）——用同一个
  // ctx（同一份 storageSql/db）手工重建一个新 RoomDO 实例模拟这个过程（同款
  // 手法见上面「休眠唤醒安全」那条 pairing_routes 测试）。
  const reconstructed = new RoomDO(ctx, {});
  const freshState = store.getRoomState(reconstructed.sql);
  assert.equal(freshState.owner_credential_hash, null, "构造器重建的哨兵表应是全新未认领房，不是墓碑");
  assert.equal(freshState.tombstoned_at, null, "全新未认领房不该带着墓碑时间戳");

  // 端到端：真实 fetch()（handleClaim 的 POST 路由）现在应该 200，不再是
  // 410——这正是「墓碑语义损失已论证无害」的行为证据：同 id 收敛成又一个
  // 全新未认领房，可以正常被认领。
  const res = await reconstructed.fetch(
    req(`https://relay.example/room/${ROOM}/claim`, {
      method: "POST",
      body: JSON.stringify({ v: 1, credential_hash: "c".repeat(64) }),
    })
  );
  assert.equal(res.status, 200, "彻底清房后同 id 是全新未认领房，claim 应正常成功，不再是永久 410");
});

test("SEC-3：未认领房与 refresh_requests 候选共存时，scheduleNextTokenAlarm 取更近的那个（min 语义）", async () => {
  const { room, ctx } = makeRoom();
  // Never-authenticated room (no room_meta) that keeps refresh_requests.
  room.sql.exec("DROP TABLE room_meta");
  const state = store.getRoomState(room.sql);
  const reclaimAt = state.created_at + UNCLAIMED_RECLAIM_MS;
  const subject = "device:99999999-9999-4999-8999-999999999999";

  // 案例 A：refresh 候选比回收候选更近 → nearest 应是 refresh 候选。
  const nearRefreshDeadline = Date.now() + 5_000;
  const upsertA = store.upsertRefreshRequest(room.sql, {
    requestId: "req-a",
    subject,
    requestGeneration: 1,
    connectionId: "conn-a",
    deadline: nearRefreshDeadline,
  });
  assert.equal(upsertA.ok, true);
  await room.scheduleNextTokenAlarm();
  assert.equal(ctx.alarmLog.scheduled, nearRefreshDeadline, "更近的 refresh 候选应赢得 min");

  // 案例 B：把 refresh 候选挪到远晚于回收候选之后 → nearest 应回落到固定的
  // 回收截止（第四类候选）。
  store.deleteRefreshRequest(room.sql, "req-a");
  const upsertB = store.upsertRefreshRequest(room.sql, {
    requestId: "req-b",
    subject,
    requestGeneration: 1,
    connectionId: "conn-b",
    deadline: reclaimAt + 999_999,
  });
  assert.equal(upsertB.ok, true);
  // Clear the recorded alarm to simulate the earlier refresh alarm firing.
  // Its deadline precedes the reclaim deadline calculated in this case.
  // Leaving it recorded would make scheduleNextTokenAlarm skip setAlarm,
  // since an existing earlier alarm already satisfies the scheduling check.
  // That would prevent this assertion from observing candidate selection.
  // Clearing the mock models the runtime removing the fired alarm before
  // rescheduling and isolates the minimum-deadline calculation here.
  // Alarm deduplication is covered separately.
  ctx.alarmLog.scheduled = null;
  await room.scheduleNextTokenAlarm();
  assert.equal(ctx.alarmLog.scheduled, reclaimAt, "回收候选更近时应赢得 min");
});

// ============================================================================
// C1-TTL（dogfood 修障第二批·手机发消息桌面离线无反馈）：pending_input TTL
// 到期时刻——scheduleNextTokenAlarm 第五类候选 + alarm() 到点清扫兜底。
// ============================================================================

test("scheduleNextTokenAlarm：pending_input 到期时刻是候选之一，比其它候选更近时赢得 min", async () => {
  const { room, ctx } = makeRoom();
  const now = Date.now();
  const nearExpiry = now + 5_000;
  store.enqueueInput(room.sql, {
    commandId: "cmd-alarm-candidate", session: "s1", envelopeJson: "{}", now, ttlMs: 5_000,
  });

  await room.scheduleNextTokenAlarm(now);

  // R4（返工·TTL 死区）：排的候选时刻是 `expires_at + 1`（不是 `expires_at` 本身）——
  // 见 room-do.js::scheduleNextTokenAlarm 头注，`expires_at` 那一刻恰好落在
  // nextPendingInputExpiry 的 `>` 与 purgeExpiredPendingInput 的 `<` 两条查询的公共
  // 盲区，+1 保证 alarm 触发时稳定被清扫判据判定为"已过去"。
  assert.equal(ctx.alarmLog.scheduled, nearExpiry + 1, "pending_input 的到期时刻+1ms 应赢得 min（房间没有更近的其它候选）");
});

test("R4（返工·TTL 死区）：expires_at 恰好等于 now 的边界时刻——排的候选仍是 expires_at+1，不是 expires_at 本身", () => {
  const { room } = makeRoom();
  const sql = room.sql;
  const now = 10_000;
  store.enqueueInput(sql, { commandId: "cmd-boundary", session: "s1", envelopeJson: "{}", now: now - 1_000, ttlMs: 1_000 });
  // 这一行的 expires_at 恰好等于 now（10_000）——nextPendingInputExpiry 的 `expires_at > now`
  // 判据对它是 false（不算"未来"），purgeExpiredPendingInput 的 `expires_at < now` 判据
  // 对它也是 false（不算"过去"）：这正是 R4 要修的公共盲区，room-store.js 本身两条查询都
  // 不变（brief 明确不改 nextPendingInputExpiry 为 `>=`），盲区靠 room-do.js 调用方
  // 排候选时 +1 规避。
  assert.equal(store.nextPendingInputExpiry(sql, now), null, "expires_at===now 时不被当作未来候选（room-store.js 本身行为不变）");
  const purgedCommandIds = store.deleteExpiredPendingInput(sql, now);
  assert.deepEqual(purgedCommandIds, [], "expires_at===now 时也不被当作已过去清扫（room-store.js 本身行为不变）——盲区真实存在");
});

test("alarm()：到点清扫过期 pending_input、广播 input.expired、并重排下一次 alarm 到剩余行的到期时刻", async () => {
  const { room, ctx } = makeRoom();
  const remoteWs = connect(room, ctx, "remote");
  const now = Date.now();

  // 一条早已过期、一条还有余量——只有过期那条该被清扫广播；alarm() 本身不
  // 是被 handleInput 高频路径触发的（房间此刻没有任何新 input 涌入），这条
  // 测试专门验证 C1-TTL 加的 alarm 驱动兜底，不是既有 handleInput 内联清扫。
  store.enqueueInput(room.sql, {
    commandId: "cmd-alarm-expired", session: "s1", envelopeJson: "{}", now: now - 10_000, ttlMs: 1_000,
  });
  store.enqueueInput(room.sql, {
    commandId: "cmd-alarm-still-fresh", session: "s1", envelopeJson: "{}", now, ttlMs: 60_000,
  });

  await room.alarm();

  const expiredFrames = remoteWs.sent.filter((frame) => frame.t === "input.expired");
  assert.deepEqual(expiredFrames.map((frame) => frame.command_id), ["cmd-alarm-expired"]);

  const { deliverable } = store.drainDeliverableInput(room.sql, Date.now());
  assert.deepEqual(deliverable.map((row) => row.command_id), ["cmd-alarm-still-fresh"], "只清过期的那一行，未过期的原样留着");

  assert.ok(ctx.alarmLog.scheduled != null, "alarm() 收尾应重排下一次 alarm");
  assert.ok(
    // R4（返工·TTL 死区）：排的候选是 `expires_at + 1`（见 room-do.js::
    // scheduleNextTokenAlarm 头注），边界从 `now + 60_000` 松 1ms 到
    // `now + 60_000 + 1`——原断言用的是「不晚于」这条宽松上界，本单只需要把
    // 上界本身也跟着挪 1ms，不改这条测试要验的东西。
    ctx.alarmLog.scheduled <= now + 60_000 + 1,
    "重排的下一次 alarm 应不晚于还剩那条行的到期时刻+1ms（房间没有更近的其它候选）"
  );
});

// ============================================================================
// Repeated failed upgrades, claim requests, and plain GET or DELETE requests
// must not repeatedly write the same fixed reclamation deadline to storage.
// scheduleNextTokenAlarm checks getAlarm() before calling setAlarm and skips
// the write when an alarm is already scheduled for the same time or earlier.
// A newly calculated earlier deadline must still replace the existing alarm.
// ============================================================================

test("F3：未认领房连续多次失败 upgrade 命中同一个回收时刻，只有第一次真正 setAlarm，后续全部去重跳过", async () => {
  const { room, ctx } = makeSchemaLessRoom();

  // 连续 3 次无凭据 WS 升级尝试——每次都会走 rejectUpgradeAuthentication →
  // scheduleNextTokenAlarm，算出的 nearest（回收候选，created_at 全程不变）
  // 每次都一样。
  for (let i = 0; i < 3; i += 1) {
    const res = await room.fetch(req(`https://relay.example/room/${ROOM}`, { headers: { Upgrade: "websocket" } }));
    assert.equal(res.status, 401);
  }

  assert.equal(ctx.alarmLog.setAlarmCalls, 1, "同一个固定回收时刻只应真正 setAlarm 一次，后续命中去重");
});

test("F3：新算出的 nearest 比现有 alarm 更早时仍会重排（去重不吞掉真正需要提前的更新）", async () => {
  const { room, ctx } = makeRoom();
  const subject = "device:88888888-8888-4888-8888-888888888888";
  const farDeadline = Date.now() + 999_999;
  const nearDeadline = Date.now() + 5_000;

  const upsertFar = store.upsertRefreshRequest(room.sql, {
    requestId: "req-far",
    subject,
    requestGeneration: 1,
    connectionId: "conn-far",
    deadline: farDeadline,
  });
  assert.equal(upsertFar.ok, true);
  await room.scheduleNextTokenAlarm();
  assert.equal(ctx.alarmLog.scheduled, farDeadline);
  assert.equal(ctx.alarmLog.setAlarmCalls, 1);

  const upsertNear = store.upsertRefreshRequest(room.sql, {
    requestId: "req-near",
    subject,
    requestGeneration: 1,
    connectionId: "conn-near",
    deadline: nearDeadline,
  });
  assert.equal(upsertNear.ok, true);
  await room.scheduleNextTokenAlarm();
  assert.equal(ctx.alarmLog.scheduled, nearDeadline, "更早的新候选出现时必须重排，不能被去重逻辑吞掉");
  assert.equal(ctx.alarmLog.setAlarmCalls, 2);
});

// ============================================================================
// SEC-3 修复轮（独立 skeptic 复现的真实缺口）：DELETE /room/<hex> 与裸
// GET/错路径请求（426 else 分支）撞中 owner==null 的未认领房时，之前哪个
// scheduleNextTokenAlarm() 调用都够不到——它们既不是失败 upgrade（不走
// rejectUpgradeAuthentication），也不是 claim POST（不走 handleClaim），也
// 从不触达 fetch() 握手成功后的既有调用点（:339 一线，只有真正的 WS
// upgrade 才会走到那一行）。刷房者换用 GET/DELETE（比 WS upgrade 尝试更
// 便宜）就能绕过整个 SEC-3 防刷房回收——本轮在这两条分支各补一次同款武装。
// ============================================================================

test("SEC-3 修复轮：DELETE 到未认领房也武装固定回收 alarm；到点 alarm() 自杀回收", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  const initialState = store.getRoomState(room.sql);
  assert.equal(initialState.owner_credential_hash, null);

  // 未带 Authorization：bearer.provided=false → 401，owner 全程仍 null。
  const res = await room.fetch(req(`https://relay.example/room/${ROOM}`, { method: "DELETE" }));
  assert.equal(res.status, 401);
  assert.equal(
    ctx.alarmLog.scheduled,
    initialState.created_at + UNCLAIMED_RECLAIM_MS,
    "DELETE 打未认领房也应武装在 created_at + UNCLAIMED_RECLAIM_MS 这个固定时刻"
  );

  // 快进：直接把 created_at 拨到回收窗口之前，不真的等 20 分钟。
  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  await room.alarm();

  // F2-A：到点回收改彻底清房，不再是 tombstone——room_state 表本身也不存在
  // 了，用 sqlite_master 直接证明「一张表都不剩」。
  const remainingTables = room.sql.exec("SELECT name FROM sqlite_master WHERE type = 'table'");
  assert.deepEqual(remainingTables, [], "deleteAll 后整个私有数据库应清空，一张表都不该剩");
  assert.equal(ctx.alarmLog.deleteAllCalled, true, "alarm() 回收枝应调用 storage.deleteAll");
  assert.equal(ctx.alarmLog.deleted, true, "alarm() 回收枝应调用 deleteAlarm");
});

test("SEC-3 修复轮：裸 GET（426 else 分支）到未认领房也武装固定回收 alarm；到点 alarm() 自杀回收", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  const initialState = store.getRoomState(room.sql);
  assert.equal(initialState.owner_credential_hash, null);

  // 无 Upgrade 头的裸 GET——既不是 claim POST 也不是 DELETE，落 426 else 分支。
  const res = await room.fetch(req(`https://relay.example/room/${ROOM}`, { method: "GET" }));
  assert.equal(res.status, 426);
  assert.equal(
    ctx.alarmLog.scheduled,
    initialState.created_at + UNCLAIMED_RECLAIM_MS,
    "裸 GET 打未认领房也应武装在 created_at + UNCLAIMED_RECLAIM_MS 这个固定时刻"
  );

  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  await room.alarm();

  // F2-A：到点回收改彻底清房，不再是 tombstone——room_state 表本身也不存在
  // 了，用 sqlite_master 直接证明「一张表都不剩」。
  const remainingTables = room.sql.exec("SELECT name FROM sqlite_master WHERE type = 'table'");
  assert.deepEqual(remainingTables, [], "deleteAll 后整个私有数据库应清空，一张表都不该剩");
  assert.equal(ctx.alarmLog.deleteAllCalled, true, "alarm() 回收枝应调用 storage.deleteAll");
  assert.equal(ctx.alarmLog.deleted, true, "alarm() 回收枝应调用 deleteAlarm");
});

// ============================================================================
// A claim can persist an owner without a successful authenticated session.
// When alarm() sees that owner, it must skip reclamation without creating
// the business schema. Calling ensureBusinessSchema as a fallback here
// would let an unauthenticated claim indirectly create business tables
// and indexes when the alarm fires, violating deferred schema creation.
//
// makeRoom() creates the business schema unconditionally, so fixtures built
// with it cannot detect this regression. These cases use rooms without
// a business schema to verify safe reclamation of unclaimed rooms and
// preservation of claimed rooms without creating business tables.
// ============================================================================

function makeSchemaLessRoom(env = {}) {
  const ctx = makeCtx();
  const room = new RoomDO(ctx, env);
  // 不调 store.ensureBusinessSchema——模拟「从未有过成功鉴权 WS 会话」的房：
  // 构造器（SEC-1）只建/补 room_state 哨兵，11 张业务表全不存在。
  return { room, ctx };
}

test("F1：未认领 schema-less 房到点 alarm() 正常彻底清房、不建业务表", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  assert.equal(store.hasTable(room.sql, "room_meta"), false, "构造完不该有任何业务表");
  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  await room.alarm();

  // F2-A：到点回收改彻底清房，不再是 tombstone——room_state 表本身也不存在
  // 了，用 sqlite_master 直接证明「一张表都不剩」（回收枝只摸 room_state，
  // 不需要业务 schema，这条不变量不受本轮改动影响）。
  const remainingTables = room.sql.exec("SELECT name FROM sqlite_master WHERE type = 'table'");
  assert.deepEqual(remainingTables, [], "deleteAll 后整个私有数据库应清空，一张表都不该剩");
  assert.equal(ctx.alarmLog.deleteAllCalled, true, "alarm() 回收枝应调用 storage.deleteAll");
  assert.equal(ctx.alarmLog.deleted, true, "alarm() 回收枝应调用 deleteAlarm");
  assert.equal(store.hasTable(room.sql, "room_meta"), false, "回收路径不该顺手建出任何业务表");
});

test("R8：已 claim 但从未鉴权成功过的 schema-less 房超过回收时限后被 alarm() 整库回收（room_state 等表一并清空）", async () => {
  const { room, ctx } = makeSchemaLessRoom();
  // Unauthenticated POST /room/<hex>/claim persists an owner without ever
  // creating the business schema (no WS auth happened).
  const claimed = store.claimRoom(room.sql, "e".repeat(64), Date.now());
  assert.equal(claimed, "claimed");
  assert.equal(store.hasTable(room.sql, "room_meta"), false, "claim 本身不该建出任何业务表");

  room.sql.exec("UPDATE room_state SET created_at = ?", Date.now() - UNCLAIMED_RECLAIM_MS - 1);

  await room.alarm();

  const remainingTables = room.sql.exec("SELECT name FROM sqlite_master WHERE type = 'table'");
  assert.deepEqual(remainingTables, [], "已 claim 未鉴权的房到点应整库清空，room_state 也不剩");
  assert.equal(ctx.alarmLog.deleteAllCalled, true);
  assert.equal(ctx.alarmLog.deleted, true);
});
