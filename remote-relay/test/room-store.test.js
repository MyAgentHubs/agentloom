import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import * as store from "../src/room-store.js";

const CLIENT_MSG_ID_NAMESPACE = "fe4e51ad-468c-4c11-85c2-f15f0c22f030";

// 独立 UUIDv5 参考实现：不复用生产 helper，避免「实现算错、测试跟着算错」。
function uuidv5(name, namespaceUuid) {
  const nsBytes = Buffer.from(namespaceUuid.replace(/-/g, ""), "hex");
  const nameBytes = Buffer.from(name, "utf8");
  const hash = createHash("sha1").update(Buffer.concat([nsBytes, nameBytes])).digest();
  const bytes = Buffer.from(hash.subarray(0, 16));
  bytes[6] = (bytes[6] & 0x0f) | 0x50;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

// 薄适配器：把 Node 内建的 node:sqlite（真实 SQLite）包成 room-store.js 期望的
// `sql.exec(query, ...params) -> Array<行对象>` 形状。room-do.js 里包的是
// Cloudflare DO 的 ctx.storage.sql（也是真实 SQLite），两边跑同一份
// room-store.js 逻辑——这里不是重新实现一遍产品逻辑再测那份假的。
function makeSql() {
  const db = new DatabaseSync(":memory:");
  return {
    exec(query, ...params) {
      const isSelect = /^\s*(SELECT|PRAGMA)/i.test(query);
      const stmt = db.prepare(query);
      if (isSelect) {
        return stmt.all(...params);
      }
      stmt.run(...params);
      return [];
    },
    // withTransaction(room-store.js) 要求适配器提供 transactionSync——同
    // room-do.test.js/room-lifecycle-fixture.test.js 的薄适配器同款姿势，
    // 真实 node:sqlite 事务，不是假的 no-op。之前这个文件没有测试直接调用
    // claimRoom/putTokenRegistryEntry 这类要事务的函数，所以一直没需要过。
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
  };
}

function freshSql() {
  const sql = makeSql();
  store.initSchema(sql);
  return sql;
}

function milestone(sql, overrides = {}) {
  return store.insertMilestone(sql, {
    epoch: 1,
    session: "s1",
    kind: "event",
    ct: "Y2lwaGVy",
    n: "bm9uY2UxMjM0NTY=",
    ts: Date.now(),
    ...overrides,
  });
}

test("initSchema 可重复调用（幂等，不报错）", () => {
  const sql = makeSql();
  store.initSchema(sql);
  assert.doesNotThrow(() => store.initSchema(sql));
});

test("initSchema 清掉后门写过的 room_meta['valid_tokens'] 存量行（S1ja F2 迁移）", () => {
  // __admin/register-token 后门（连同它写的 room_meta.valid_tokens 行）已随
  // S1ja F1 整个删除——但只删路由/代码不清已写入的行，存量 dev token 仍能靠
  // legacy 豁免（已在 room-do.js 一并撤掉）绕开整个注册表闸、永不过期不可
  // 吊销，是一条永久后门。这条迁移必须幂等（跟 ensureClientMsgIdColumn 那批
  // 守卫式迁移同一纪律）：initSchema 可能在同一个 DO 实例生命周期内被多次
  // 触发（fresh 构造 vs 唤醒重跑）。
  const sql = makeSql();
  store.initSchema(sql);
  store.setMeta(sql, "valid_tokens", JSON.stringify(["stale-dev-token"]));
  assert.equal(store.getMeta(sql, "valid_tokens"), JSON.stringify(["stale-dev-token"]));

  store.initSchema(sql);
  assert.equal(store.getMeta(sql, "valid_tokens"), null);

  // 幂等：房间里从没写过这一行时，第二次 initSchema 调用（唤醒重跑）也不报错。
  assert.doesNotThrow(() => store.initSchema(sql));
  assert.equal(store.getMeta(sql, "valid_tokens"), null);
});

test("initSchema 为旧 pending_input 幂等补 subject/generation，存量行保持 NULL", () => {
  const sql = makeSql();
  sql.exec(`CREATE TABLE pending_input (
    command_id TEXT PRIMARY KEY,
    session TEXT,
    envelope TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
  )`);
  sql.exec(
    "INSERT INTO pending_input (command_id, session, envelope, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
    "legacy", null, "{}", 1, 2
  );
  store.initSchema(sql);
  assert.doesNotThrow(() => store.initSchema(sql));
  const row = sql.exec("SELECT subject, generation FROM pending_input WHERE command_id = ?", "legacy")[0];
  assert.equal(row.subject, null);
  assert.equal(row.generation, null);
});

// ---- epoch ----

test("getCurrentEpoch 初始为 0", () => {
  const sql = freshSql();
  assert.equal(store.getCurrentEpoch(sql), 0);
});

test("bumpEpoch 递增且持久", () => {
  const sql = freshSql();
  assert.equal(store.bumpEpoch(sql), 1);
  assert.equal(store.bumpEpoch(sql), 2);
  assert.equal(store.getCurrentEpoch(sql), 2);
});

// ---- seq 单调 ----

test("连续里程碑写入 seq 从 1 递增、无跳号", () => {
  const sql = freshSql();
  const r1 = milestone(sql);
  const r2 = milestone(sql);
  const r3 = milestone(sql);
  assert.deepEqual([r1.seq, r2.seq, r3.seq], [1, 2, 3]);
  assert.equal(store.headSeq(sql), 3);
});

test("不同 session 共用同一房间级 seq 序列（不按 session 各自计数）", () => {
  const sql = freshSql();
  const a = milestone(sql, { session: "sess-a" });
  const b = milestone(sql, { session: "sess-b" });
  assert.deepEqual([a.seq, b.seq], [1, 2]);
});

// ---- epoch 防双写 ----

test("旧 epoch 的里程碑写入被拒", () => {
  const sql = freshSql();
  store.bumpEpoch(sql); // epoch -> 1
  store.bumpEpoch(sql); // epoch -> 2（模拟桌面重连）

  const stale = milestone(sql, { epoch: 1 }); // 旧连接还在用 epoch=1 发写入
  assert.equal(stale.ok, false);
  assert.equal(stale.reason, "stale_epoch");
  assert.equal(stale.currentEpoch, 2);
  assert.equal(store.headSeq(sql), 0); // 没有真的落库
});

test("当前 epoch 的写入被接受", () => {
  const sql = freshSql();
  store.bumpEpoch(sql); // epoch -> 1
  const ok = milestone(sql, { epoch: 1 });
  assert.equal(ok.ok, true);
  assert.equal(ok.seq, 1);
});

test("epoch 恰好等于当前 epoch 视为有效（不是必须严格大于）", () => {
  const sql = freshSql();
  store.bumpEpoch(sql); // -> 1
  const first = milestone(sql, { epoch: 1 });
  const second = milestone(sql, { epoch: 1 });
  assert.equal(first.ok, true);
  assert.equal(second.ok, true);
});

// ---- 重连回放 ----

test("回放只返回 last_seq 之后的事件，按 seq 升序", () => {
  const sql = freshSql();
  for (let i = 0; i < 5; i++) milestone(sql, { session: `s${i}` });

  const rows = store.replaySince(sql, 2);
  assert.deepEqual(
    rows.map((r) => r.seq),
    [3, 4, 5]
  );
});

test("last_seq=0（或缺省）回放全部历史", () => {
  const sql = freshSql();
  milestone(sql);
  milestone(sql);
  const rows = store.replaySince(sql, 0);
  assert.equal(rows.length, 2);
});

test("last_seq 大于等于 headSeq 时回放为空", () => {
  const sql = freshSql();
  milestone(sql);
  assert.deepEqual(store.replaySince(sql, 999), []);
});

// ---- client_msg_id 去重（v1.7.4） ----

test("insertMilestone：相同 client_msg_id 二次写入命中去重——不落新行、不消耗新 seq、返回既有 seq", () => {
  const sql = freshSql();
  const first = milestone(sql, { client_msg_id: "cmid-a" });
  const second = milestone(sql, { client_msg_id: "cmid-a" });
  assert.equal(first.ok, true);
  assert.equal(second.ok, true);
  assert.equal(second.dedup, true);
  assert.equal(second.seq, first.seq);
  assert.equal(store.headSeq(sql), first.seq); // 没有分配新 seq
  const rows = store.replaySince(sql, 0);
  assert.equal(rows.length, 1); // 没有真的插入第二行
});

test("insertMilestone：client_msg_id 为 null 时不去重（历史行天然豁免唯一索引）", () => {
  const sql = freshSql();
  const a = milestone(sql, { client_msg_id: null });
  const b = milestone(sql, { client_msg_id: null });
  assert.notEqual(a.seq, b.seq);
  assert.equal(b.dedup, undefined);
});

test("insertMilestone：不同 client_msg_id 各自正常分配新 seq", () => {
  const sql = freshSql();
  const a = milestone(sql, { client_msg_id: "cmid-x" });
  const b = milestone(sql, { client_msg_id: "cmid-y" });
  assert.notEqual(a.seq, b.seq);
});

test("replaySince 返回的行带 client_msg_id 字段", () => {
  const sql = freshSql();
  milestone(sql, { client_msg_id: "cmid-replay-1" });
  const rows = store.replaySince(sql, 0);
  assert.equal(rows[0].client_msg_id, "cmid-replay-1");
});

test("initSchema 对已存在但缺 client_msg_id 列的旧 events 表可安全迁移（守卫式 ALTER，模拟已部署 DO 的存量表）", () => {
  const sql = makeSql();
  sql.exec(`CREATE TABLE events (
    seq     INTEGER PRIMARY KEY,
    epoch   INTEGER NOT NULL,
    session TEXT,
    kind    TEXT NOT NULL,
    ct      TEXT NOT NULL,
    n       TEXT NOT NULL,
    ts      INTEGER NOT NULL
  )`);
  assert.doesNotThrow(() => store.initSchema(sql));
  assert.doesNotThrow(() => store.initSchema(sql)); // 可重入
  const first = store.insertMilestone(sql, {
    epoch: 0,
    session: "s1",
    kind: "event",
    ct: "Y2lwaGVy",
    n: "bm9uY2UxMjM0NTY=",
    ts: 1,
    client_msg_id: "cmid-migrate-1",
  });
  assert.equal(first.ok, true);
  const dup = store.insertMilestone(sql, {
    epoch: 0,
    session: "s1",
    kind: "event",
    ct: "Y2lwaGVy",
    n: "bm9uY2UxMjM0NTY=",
    ts: 2,
    client_msg_id: "cmid-migrate-1",
  });
  assert.equal(dup.dedup, true);
  assert.equal(dup.seq, first.seq);
});

test("initSchema 用持久 room_id + seq 为旧 events 确定性回填互不相同的 legacy client_msg_id，且可重入", () => {
  const sql = makeSql();
  const roomId = "c".repeat(32);
  sql.exec(`CREATE TABLE events (
    seq     INTEGER PRIMARY KEY,
    epoch   INTEGER NOT NULL,
    session TEXT,
    kind    TEXT NOT NULL,
    ct      TEXT NOT NULL,
    n       TEXT NOT NULL,
    ts      INTEGER NOT NULL
  )`);
  sql.exec(`CREATE TABLE room_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
  )`);
  store.ensureRoomId(sql, roomId); // 模拟旧房间此前已通过 fetch 持久化 room_id
  for (const seq of [1, 2, 7]) {
    sql.exec(
      "INSERT INTO events (seq, epoch, session, kind, ct, n, ts) VALUES (?, ?, ?, ?, ?, ?, ?)",
      seq,
      3,
      `s${seq}`,
      "event",
      "Y2lwaGVy",
      "BwcHBwcHBwcHBwcH",
      1000 + seq
    );
  }

  store.initSchema(sql);
  const firstPass = sql.exec("SELECT seq, client_msg_id FROM events ORDER BY seq ASC");
  assert.deepEqual(
    firstPass.map((row) => row.client_msg_id),
    firstPass.map((row) => uuidv5(`legacy|${roomId}|${row.seq}`, CLIENT_MSG_ID_NAMESPACE))
  );
  assert.ok(firstPass.every((row) => typeof row.client_msg_id === "string" && row.client_msg_id.length > 0));
  assert.equal(new Set(firstPass.map((row) => row.client_msg_id)).size, firstPass.length);

  store.initSchema(sql);
  const secondPass = sql.exec("SELECT seq, client_msg_id FROM events ORDER BY seq ASC");
  assert.deepEqual(secondPass, firstPass);
});

// ---- input 暂存队列 ----

test("input 在 TTL 内可投递", () => {
  const sql = freshSql();
  const now = 1_000_000;
  store.enqueueInput(sql, { commandId: "c1", session: "s1", envelopeJson: "{}", now, ttlMs: 30 * 60 * 1000 });

  const { deliverable, expired } = store.drainDeliverableInput(sql, now + 60_000); // 1 分钟后查
  assert.equal(deliverable.length, 1);
  assert.equal(expired.length, 0);
  assert.equal(deliverable[0].command_id, "c1");
});

test("input 超过 TTL 被判定过期", () => {
  const sql = freshSql();
  const now = 1_000_000;
  const ttlMs = 30 * 60 * 1000;
  store.enqueueInput(sql, { commandId: "c1", session: "s1", envelopeJson: "{}", now, ttlMs });

  const { deliverable, expired } = store.drainDeliverableInput(sql, now + ttlMs + 1);
  assert.equal(deliverable.length, 0);
  assert.equal(expired.length, 1);
  assert.equal(expired[0].command_id, "c1");
});

test("input 按 created_at 升序返回（FIFO）", () => {
  const sql = freshSql();
  store.enqueueInput(sql, { commandId: "second", session: "s1", envelopeJson: "{}", now: 2000 });
  store.enqueueInput(sql, { commandId: "first", session: "s1", envelopeJson: "{}", now: 1000 });

  const { deliverable } = store.drainDeliverableInput(sql, 3000);
  assert.deepEqual(
    deliverable.map((r) => r.command_id),
    ["first", "second"]
  );
});

test("同一毫秒入队的多条 input 按插入顺序（rowid）排列", () => {
  const sql = freshSql();
  const now = 5000;
  store.enqueueInput(sql, { commandId: "a", session: "s1", envelopeJson: "{}", now });
  store.enqueueInput(sql, { commandId: "b", session: "s1", envelopeJson: "{}", now });
  store.enqueueInput(sql, { commandId: "c", session: "s1", envelopeJson: "{}", now });

  const { deliverable } = store.drainDeliverableInput(sql, now);
  assert.deepEqual(
    deliverable.map((r) => r.command_id),
    ["a", "b", "c"]
  );
});

test("removePendingInput 删除对应行", () => {
  const sql = freshSql();
  store.enqueueInput(sql, { commandId: "c1", session: "s1", envelopeJson: "{}", now: 1000 });
  store.removePendingInput(sql, "c1");
  const { deliverable, expired } = store.drainDeliverableInput(sql, 999_999);
  assert.equal(deliverable.length, 0);
  assert.equal(expired.length, 0);
});

test("同一 command_id 重复入队不产生第二行（去重）", () => {
  const sql = freshSql();
  store.enqueueInput(sql, { commandId: "dup", session: "s1", envelopeJson: "{}", now: 1000 });
  store.enqueueInput(sql, { commandId: "dup", session: "s1", envelopeJson: "{}", now: 2000 });
  const { deliverable } = store.drainDeliverableInput(sql, 999_999);
  assert.equal(deliverable.length, 1);
});

// C1-RQ（dogfood 修障第二批）：enqueueInput 现在带回本次入队实际生效的
// expires_at——room-do.js handleInput 拿它拼 input.relay_queued 确认帧。
test("enqueueInput 返回 { expiresAt: now + ttlMs }", () => {
  const sql = freshSql();
  const result = store.enqueueInput(sql, {
    commandId: "c1", session: "s1", envelopeJson: "{}", now: 1000, ttlMs: 5000,
  });
  assert.deepEqual(result, { expiresAt: 6000 });
});

// ---- getPendingInputExpiry（C1-RQ：幂等命中回执要用的既存行 expires_at） ----

test("getPendingInputExpiry 对已存在的行返回其 expires_at", () => {
  const sql = freshSql();
  store.enqueueInput(sql, { commandId: "c1", session: "s1", envelopeJson: "{}", now: 1000, ttlMs: 5000 });
  assert.equal(store.getPendingInputExpiry(sql, "c1"), 6000);
});

test("getPendingInputExpiry 对不存在的 command_id 返回 null", () => {
  const sql = freshSql();
  assert.equal(store.getPendingInputExpiry(sql, "never-enqueued"), null);
});

// ---- nextPendingInputExpiry（C1-TTL：scheduleNextTokenAlarm 第五类候选） ----

test("nextPendingInputExpiry 返回未来到期时刻里最早的一个", () => {
  const sql = freshSql();
  store.enqueueInput(sql, { commandId: "later", session: "s1", envelopeJson: "{}", now: 1000, ttlMs: 9000 });
  store.enqueueInput(sql, { commandId: "earlier", session: "s1", envelopeJson: "{}", now: 1000, ttlMs: 3000 });
  assert.equal(store.nextPendingInputExpiry(sql, 500), 4000); // "earlier" 的 expires_at
});

test("nextPendingInputExpiry 忽略已过期的行，只看未来", () => {
  const sql = freshSql();
  store.enqueueInput(sql, { commandId: "already-expired", session: "s1", envelopeJson: "{}", now: 1000, ttlMs: 1000 });
  store.enqueueInput(sql, { commandId: "still-future", session: "s1", envelopeJson: "{}", now: 1000, ttlMs: 9000 });
  assert.equal(store.nextPendingInputExpiry(sql, 2500), 10000); // 只看 expires_at > 2500 的行
});

test("nextPendingInputExpiry 队列为空时返回 null", () => {
  const sql = freshSql();
  assert.equal(store.nextPendingInputExpiry(sql, Date.now()), null);
});

// ---- 配额计数器 ----

test("getQuotaCount 未记录时为 0", () => {
  const sql = freshSql();
  assert.equal(store.getQuotaCount(sql, "2026-08"), 0);
});

test("incrementQuotaCount 累加", () => {
  const sql = freshSql();
  store.incrementQuotaCount(sql, "2026-08", 1);
  store.incrementQuotaCount(sql, "2026-08", 1);
  store.incrementQuotaCount(sql, "2026-08", 3);
  assert.equal(store.getQuotaCount(sql, "2026-08"), 5);
});

test("不同月份的配额计数互相独立", () => {
  const sql = freshSql();
  store.incrementQuotaCount(sql, "2026-08", 10);
  store.incrementQuotaCount(sql, "2026-09", 1);
  assert.equal(store.getQuotaCount(sql, "2026-08"), 10);
  assert.equal(store.getQuotaCount(sql, "2026-09"), 1);
});

// ---- room_meta / room_id ----

test("ensureRoomId 首次设置、之后幂等返回同一个值", () => {
  const sql = freshSql();
  const first = store.ensureRoomId(sql, "a".repeat(32));
  const second = store.ensureRoomId(sql, "b".repeat(32)); // 试图换成另一个 id
  assert.equal(first, "a".repeat(32));
  assert.equal(second, "a".repeat(32)); // 不会被后来的调用覆盖
});

// ---- SEC-1：业务 schema 延到鉴权成功后才建 ----

test("resolveTokenAdmission：token_aliases/token_subjects 未建（空 schema 房）时返回 null，不抛 SQL 异常", () => {
  const sql = makeSql();
  store.initRoomStateSentinel(sql); // 只有哨兵表，业务 schema 尚未建
  assert.equal(store.hasTable(sql, "token_aliases"), false);
  assert.equal(store.hasTable(sql, "token_subjects"), false);
  assert.equal(store.resolveTokenAdmission(sql, "a".repeat(64), Date.now()), null);
});

test("resolveTokenAdmission：业务 schema 建好后按原逻辑正常解析合法令牌", () => {
  const sql = freshSql();
  const now = Date.now();
  const tokenHash = "b".repeat(64);
  store.putTokenRegistryEntry(sql, {
    subject: "device:11111111-1111-4111-8111-111111111111",
    generation: 1,
    state: "active",
    scope: "remote",
    aliases: [{ token_hash: tokenHash, kind: "current", generation: 1, access_expires: now + 60_000, valid_until: now + 120_000 }],
  }, now);
  const admission = store.resolveTokenAdmission(sql, tokenHash, now);
  assert.ok(admission);
  assert.equal(admission.scope, "remote");
});

test("claimRoom：不依赖业务 schema，只需 room_state + 懒建的 claim_rate_limits 这一张小表", () => {
  const sql = makeSql();
  store.initRoomStateSentinel(sql); // 只建哨兵表，模拟 SEC-1 之后鉴权前的裸房
  assert.equal(store.hasTable(sql, "claim_rate_limits"), false);
  const first = store.claimRoom(sql, "c".repeat(64), 1_000);
  assert.equal(first, "claimed");
  // recordClaimAttempt 首行懒建，claim 一次后表应已存在，且没有连带建出任何
  // 业务表（events/token_subjects 等 11 张都不该出现）。
  assert.equal(store.hasTable(sql, "claim_rate_limits"), true);
  const tableNames = sql
    .exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
    .map((row) => row.name);
  assert.deepEqual(tableNames, ["claim_rate_limits", "room_state"]);

  const same = store.claimRoom(sql, "c".repeat(64), 2_000); // 同哈希幂等重试
  assert.equal(same, "same");
});

test("initRoomStateSentinel：对存量（预 SEC-1）room_state 守卫式补 ip_bucket_salt 列，幂等可重入", () => {
  const sql = makeSql();
  // 手工搭一张「预 SEC-1」的旧 room_state——没有 ip_bucket_salt 列，模拟已部署
  // DO 的存量哨兵表，验证 initRoomStateSentinel 的守卫式 ALTER 迁移路径。
  sql.exec(`CREATE TABLE room_state (
    owner_credential_hash TEXT,
    tombstoned_at INTEGER,
    registry_floor INTEGER NOT NULL DEFAULT 0
  )`);
  sql.exec(
    "INSERT INTO room_state (owner_credential_hash, tombstoned_at, registry_floor) VALUES (NULL, NULL, 0)"
  );
  assert.doesNotThrow(() => store.initRoomStateSentinel(sql));
  assert.doesNotThrow(() => store.initRoomStateSentinel(sql)); // 可重入
  assert.equal(store.getRoomIpBucketSalt(sql), null); // 列已在，值仍是 NULL
  // 迁移没有把已有的 sentinel 行重复插入第二行。
  assert.equal(sql.exec("SELECT COUNT(*) AS n FROM room_state")[0].n, 1);
});

test("getRoomIpBucketSalt/setRoomIpBucketSalt：room_state 承载的 IP 盐读写往返，且只有这一行", () => {
  const sql = makeSql();
  store.initRoomStateSentinel(sql);
  assert.equal(store.getRoomIpBucketSalt(sql), null);
  store.setRoomIpBucketSalt(sql, "d".repeat(64));
  assert.equal(store.getRoomIpBucketSalt(sql), "d".repeat(64));
  assert.equal(sql.exec("SELECT COUNT(*) AS n FROM room_state")[0].n, 1);
});

test("ensureBusinessSchema：与 initSchema 等价（幂等建全部 13 张业务表），只是给鉴权后建表这个用途起的名字", () => {
  const sql = makeSql();
  store.ensureBusinessSchema(sql);
  const tableNames = sql
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
    "token_aliases",
    "token_put_fingerprints",
    "token_subjects",
  ]);
  assert.doesNotThrow(() => store.ensureBusinessSchema(sql)); // 可重入
});
