// indexeddbEventStore.test.ts — TDD 覆盖 src/store/indexeddbEventStore.ts（replay/去重层 +
// 水位事务层，brief §2 项 2/3；事件本体持久化 = 审查返工新增硬约束）。
//
// 用 `fake-indexeddb/auto`（本单任务书 §0 明确允许新增的唯一 devDependency）在 node 环境驱动
// 真实的 IndexedDB 事务语义——不是手写 in-memory mock 顶替真实的 IndexedDB 行为（那正是 CLAUDE.md
// 记录过的教训：mock 比真实运行时宽容会让 bug 漏到真机才现形）。
//
// 大部分用例的测试对象是 envelope 层的元数据（client_msg_id/seq），跟具体帧 `t` 是什么无关，
// 用合成的 client_msg_id/seq/frame 语料，不是 fixture 消费方——分层边界见 parseFrame.test.ts
// 顶部注释。"重载后从日志重建内容"那组测试例外：为了证明重建出的内容真的能喂回
// `parseFrame()`/`MilestoneProjection` 产出正确结果，那里用了 data-plane-v1.json 的
// `msg_completed` 真样张。

import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { loadFixture } from "../test-support/fixtures.ts";
import { parseFrame } from "../events/parseFrame.ts";
import { MilestoneProjection } from "../events/milestoneProjection.ts";
import { deriveEventStoreDbName, IndexedDbEventStore } from "./indexeddbEventStore.ts";

let dbCounter = 0;
function freshStore(): IndexedDbEventStore {
  dbCounter += 1;
  return new IndexedDbEventStore(`test-db-${dbCounter}-${Date.now()}`);
}

const dummyFrame = (note: string): unknown => ({ t: "presence", role: "remote", event: note });

describe("IndexedDbEventStore · basic dedup + watermark", () => {
  it("getWatermark() is 0 before anything has been applied", async () => {
    const store = freshStore();
    expect(await store.getWatermark()).toBe(0);
  });

  it("hasAppliedClientMsgId() is false before applying, true after", async () => {
    const store = freshStore();
    expect(await store.hasAppliedClientMsgId("cmid-1")).toBe(false);
    await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 1, session: null, frame: dummyFrame("a") });
    expect(await store.hasAppliedClientMsgId("cmid-1")).toBe(true);
  });

  it("first application of a client_msg_id returns applied:true and advances the watermark", async () => {
    const store = freshStore();
    const result = await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 7, session: null, frame: dummyFrame("a") });
    expect(result).toEqual({ applied: true, watermark: 7 });
    expect(await store.getWatermark()).toBe(7);
  });

  it("re-applying the exact same client_msg_id returns applied:false and does not change the watermark (at-least-once redelivery)", async () => {
    const store = freshStore();
    const first = await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 7, session: null, frame: dummyFrame("a") });
    expect(first).toEqual({ applied: true, watermark: 7 });

    const second = await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 7, session: null, frame: dummyFrame("a-redelivered") });
    expect(second).toEqual({ applied: false, watermark: 7 });
    expect(await store.getWatermark()).toBe(7);
  });

  it("watermark advances monotonically via max(), never regressing on an out-of-order lower seq", async () => {
    const store = freshStore();
    await store.applyEventIfNew({ clientMsgId: "cmid-high", seq: 5, session: null, frame: dummyFrame("high") });
    expect(await store.getWatermark()).toBe(5);

    // A different, distinct event arrives with a *lower* seq (out-of-order delivery/replay overlap).
    const result = await store.applyEventIfNew({ clientMsgId: "cmid-low", seq: 3, session: null, frame: dummyFrame("low") });
    expect(result).toEqual({ applied: true, watermark: 5 }); // new event IS applied, watermark stays at the max
    expect(await store.getWatermark()).toBe(5);
  });

  it("multiple distinct events in increasing seq order advance the watermark each time", async () => {
    const store = freshStore();
    expect(await store.applyEventIfNew({ clientMsgId: "a", seq: 1, session: null, frame: dummyFrame("a") })).toEqual({
      applied: true,
      watermark: 1,
    });
    expect(await store.applyEventIfNew({ clientMsgId: "b", seq: 2, session: null, frame: dummyFrame("b") })).toEqual({
      applied: true,
      watermark: 2,
    });
    expect(await store.applyEventIfNew({ clientMsgId: "c", seq: 3, session: null, frame: dummyFrame("c") })).toEqual({
      applied: true,
      watermark: 3,
    });
  });
});

describe("IndexedDbEventStore · event body persistence (审查返工·硬约束：本体必须跟回执/水位同一事务落储)", () => {
  it("applyEventIfNew() persists the frame body, retrievable via listEvents()", async () => {
    const store = freshStore();
    const frame = dummyFrame("persisted");
    await store.applyEventIfNew({ clientMsgId: "cmid-body", seq: 3, session: null, frame });
    expect(await store.listEvents()).toEqual([{ clientMsgId: "cmid-body", seq: 3, session: null, frame }]);
  });

  it("listEvents() returns entries ordered by seq ascending, regardless of application order", async () => {
    const store = freshStore();
    await store.applyEventIfNew({ clientMsgId: "third", seq: 30, session: null, frame: dummyFrame("third") });
    await store.applyEventIfNew({ clientMsgId: "first", seq: 10, session: null, frame: dummyFrame("first") });
    await store.applyEventIfNew({ clientMsgId: "second", seq: 20, session: null, frame: dummyFrame("second") });

    const events = await store.listEvents();
    expect(events.map((e) => e.seq)).toEqual([10, 20, 30]);
    expect(events.map((e) => e.clientMsgId)).toEqual(["first", "second", "third"]);
  });

  it("re-delivery of the same client_msg_id does not duplicate the event log entry (log stays the first-seen body)", async () => {
    const store = freshStore();
    const originalFrame = dummyFrame("original");
    await store.applyEventIfNew({ clientMsgId: "cmid-dup", seq: 5, session: null, frame: originalFrame });
    // Redelivery carries a *different* frame object identity but the same clientMsgId — this must
    // not happen in a well-behaved relay (same client_msg_id ⇒ same content), but the store's job
    // is dedup-by-id, not content diffing: the second call is a no-op, log keeps the first body.
    await store.applyEventIfNew({ clientMsgId: "cmid-dup", seq: 5, session: null, frame: dummyFrame("should-be-ignored") });

    const events = await store.listEvents();
    expect(events).toHaveLength(1);
    expect(events[0]).toEqual({ clientMsgId: "cmid-dup", seq: 5, session: null, frame: originalFrame });
  });
});

describe("IndexedDbEventStore · 重载后从持久事件日志重建内容（往返测试，data-plane-v1.json 真样张）", () => {
  interface DataPlaneCase {
    name: string;
    frame: unknown;
  }
  interface DataPlaneFixture {
    cases: DataPlaneCase[];
  }
  const fixture = loadFixture<DataPlaneFixture>("data-plane-v1.json");
  const msgCompletedCase = fixture.cases.find((entry) => entry.name === "msg_completed");
  if (!msgCompletedCase) throw new Error("data-plane-v1.json missing msg_completed case");

  it("a fresh store instance against the same dbName ('reload') can rebuild a MilestoneProjection purely from listEvents()", async () => {
    dbCounter += 1;
    const dbName = `test-db-rebuild-${dbCounter}-${Date.now()}`;
    const writer = new IndexedDbEventStore(dbName);
    await writer.applyEventIfNew({ clientMsgId: "msg.completed|s-1|dedup-1", seq: 1, session: null, frame: msgCompletedCase.frame });

    // "Reload": a brand-new store instance, no shared in-memory state with `writer` — only the
    // dbName ties them together, exactly like a page reload reopening the same IndexedDB database.
    const reader = new IndexedDbEventStore(dbName);
    const events = await reader.listEvents();
    expect(events).toHaveLength(1);

    const projection = new MilestoneProjection();
    for (const event of events) {
      const parsed = parseFrame(event.frame);
      if (!parsed.ok) throw new Error(`rebuild: unparseable logged frame (${parsed.reason})`);
      if (parsed.frame.t === "msg.completed") projection.applyMsgCompleted(parsed.frame);
    }

    // Content must match what parsing the raw fixture frame directly would produce — proving the
    // round trip (write → persist → reload → read → re-parse → project) preserves the real content,
    // not just an opaque "seen it" receipt.
    const direct = parseFrame(msgCompletedCase.frame);
    if (!direct.ok || direct.frame.t !== "msg.completed") throw new Error("fixture case failed to parse directly");
    expect(projection.messages.get(direct.frame.message_id)).toEqual({
      messageId: direct.frame.message_id,
      role: direct.frame.role,
      blocks: direct.frame.blocks,
      agent: "Claude",
      revision: 1,
    });
  });
});

describe("IndexedDbEventStore · persistence across store instances (same dbName)", () => {
  it("a second store instance opened against the same dbName sees data written by the first", async () => {
    dbCounter += 1;
    const dbName = `test-db-shared-${dbCounter}-${Date.now()}`;
    const first = new IndexedDbEventStore(dbName);
    await first.applyEventIfNew({ clientMsgId: "cmid-persist", seq: 42, session: null, frame: dummyFrame("persist") });

    const second = new IndexedDbEventStore(dbName);
    expect(await second.getWatermark()).toBe(42);
    expect(await second.hasAppliedClientMsgId("cmid-persist")).toBe(true);
    // The second instance's own call for the same id must also see it as a duplicate.
    expect(
      await second.applyEventIfNew({ clientMsgId: "cmid-persist", seq: 42, session: null, frame: dummyFrame("persist") }),
    ).toEqual({
      applied: false,
      watermark: 42,
    });
  });

  it("two independently-named stores do not see each other's data (dbName isolation)", async () => {
    const storeA = freshStore();
    const storeB = freshStore();
    await storeA.applyEventIfNew({ clientMsgId: "only-in-a", seq: 9, session: null, frame: dummyFrame("a") });
    expect(await storeB.getWatermark()).toBe(0);
    expect(await storeB.hasAppliedClientMsgId("only-in-a")).toBe(false);
  });
});

describe("IndexedDbEventStore · msgfix2 F2 S3（onversionchange 必须一并清 dbPromise，不只清 dbHandle）", () => {
  it("versionchange 触发后（另一个上下文对同一个库发起 deleteDatabase()）——下一次操作重新 openDb() 成功，不是死死攥着一个已 close 的 stale 连接恒抛 InvalidStateError", async () => {
    dbCounter += 1;
    const dbName = `test-versionchange-${dbCounter}-${Date.now()}`;
    const store = new IndexedDbEventStore(dbName);
    await store.applyEventIfNew({ clientMsgId: "before-versionchange", seq: 1, session: null, frame: dummyFrame("a") });

    // 模拟"另一个标签页"对同一个库发起 deleteDatabase()——本店的 `db.onversionchange` 处理器会
    // 同步 `db.close()`，deleteDatabase() 才不会卡在 onblocked。旧版只清了 `dbHandle`，没清
    // `dbPromise`——`dbPromise` 仍然缓存着一个已经 resolve 过、但底层连接已经 close 的 stale
    // `IDBDatabase`，下一次 `openDb()` 会直接复用这个死连接（`if (this.dbPromise) return
    // this.dbPromise`），对已关闭连接开事务在真实 IndexedDB 语义下恒抛 `InvalidStateError`。
    await new Promise<void>((resolve, reject) => {
      const req = indexedDB.deleteDatabase(dbName);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error ?? new Error("deleteDatabase failed"));
      req.onblocked = () => reject(new Error("deleteDatabase blocked — onversionchange did not release the connection"));
    });

    // 核心断言：下一次操作重新 openDb() 成功（不是 stale dbPromise 恒坏）——库被删过，watermark
    // 重新从 0 开始，新写入正常生效。
    const result = await store.applyEventIfNew({ clientMsgId: "after-versionchange", seq: 1, session: null, frame: dummyFrame("b") });
    expect(result).toEqual({ applied: true, watermark: 1 });
  });
});

describe("IndexedDbEventStore · `session` 字段随信封同一事务落盘（INT1c 审查返工·P0）", () => {
  it("session.index 类型（外层 session 恒 null）与其余里程碑类型（非 null）都原样往返", async () => {
    const store = freshStore();
    await store.applyEventIfNew({ clientMsgId: "idx-1", seq: 1, session: null, frame: dummyFrame("index") });
    await store.applyEventIfNew({ clientMsgId: "msg-1", seq: 2, session: "sess-a", frame: dummyFrame("msg") });

    const events = await store.listEvents();
    expect(events.find((e) => e.clientMsgId === "idx-1")?.session).toBeNull();
    expect(events.find((e) => e.clientMsgId === "msg-1")?.session).toBe("sess-a");
  });

  it("`deriveEventStoreDbName(room)` 派生的库名按房间彼此独立——换房不串库", async () => {
    const dbA = deriveEventStoreDbName("0123456789abcdef0123456789abcdef");
    const dbB = deriveEventStoreDbName("fedcba9876543210fedcba9876543210");
    expect(dbA).not.toBe(dbB);

    const storeA = new IndexedDbEventStore(dbA);
    const storeB = new IndexedDbEventStore(dbB);
    await storeA.applyEventIfNew({ clientMsgId: "only-in-room-a", seq: 1, session: "sess-a", frame: dummyFrame("a") });

    expect(await storeB.getWatermark()).toBe(0);
    expect(await storeB.hasAppliedClientMsgId("only-in-room-a")).toBe(false);
    expect(await storeB.listEvents()).toEqual([]);

    // 同一个房间、"重新打开"（新实例、同派生库名）必须还能看见旧数据——不是每次都是空库。
    const storeAReopened = new IndexedDbEventStore(dbA);
    expect(await storeAReopened.getWatermark()).toBe(1);
    expect(await storeAReopened.listEvents()).toHaveLength(1);
  });

  it("迁移边界：本字段引入之前写入的存量行（IDB 里物理上没有 `session` 键）读出时 `session` 是 `undefined`，不是 `null`、不崩溃", async () => {
    dbCounter += 1;
    const dbName = `test-db-legacy-${dbCounter}-${Date.now()}`;

    // 模拟"旧版本代码写的行"——绕过 IndexedDbEventStore，直接用裸 IndexedDB API 写一行不带
    // `session` 键的记录到同一个 object store 形状里（旧版本 EventRow 就是 `{clientMsgId, seq,
    // frame}` 三个字段，物理上没有 `session` 属性，不是"值为 undefined"——结构化克隆不会凭空
    // 生出这个键）。
    await new Promise<void>((resolve, reject) => {
      const request = indexedDB.open(dbName, 1);
      request.onupgradeneeded = () => {
        const db = request.result;
        db.createObjectStore("appliedClientMsgIds", { keyPath: "clientMsgId" });
        db.createObjectStore("meta", { keyPath: "key" });
        db.createObjectStore("events", { keyPath: "clientMsgId" });
      };
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction(["appliedClientMsgIds", "meta", "events"], "readwrite");
        tx.objectStore("appliedClientMsgIds").put({ clientMsgId: "legacy-1", seq: 1 });
        tx.objectStore("meta").put({ key: "watermark", value: 1 });
        tx.objectStore("events").put({ clientMsgId: "legacy-1", seq: 1, frame: dummyFrame("legacy, pre-session-field") });
        tx.oncomplete = () => {
          db.close();
          resolve();
        };
        tx.onerror = () => reject(tx.error);
      };
      request.onerror = () => reject(request.error);
    });

    const store = new IndexedDbEventStore(dbName);
    const events = await store.listEvents();
    expect(events).toHaveLength(1);
    expect(events[0]!.session).toBeUndefined(); // 不是 null——真的"不知道"，不是"确定是房间级流"。
    expect(events[0]!.clientMsgId).toBe("legacy-1");

    // 新写入（本单之后）在同一个库里正常带上 session 字段，新旧行共存不互相污染。
    await store.applyEventIfNew({ clientMsgId: "fresh-1", seq: 2, session: "sess-fresh", frame: dummyFrame("fresh") });
    const eventsAfter = await store.listEvents();
    expect(eventsAfter.find((e) => e.clientMsgId === "legacy-1")?.session).toBeUndefined();
    expect(eventsAfter.find((e) => e.clientMsgId === "fresh-1")?.session).toBe("sess-fresh");
  });
});

describe("IndexedDbEventStore · same-transaction atomicity (structural proof)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("applyEventIfNew() opens exactly one IDBTransaction spanning all three object stores (not separate transactions)", async () => {
    const store = freshStore();
    // Prime the DB connection first so the upgrade transaction (a separate, expected transaction)
    // doesn't pollute the count for the call under test.
    await store.getWatermark();

    const transactionSpy = vi.spyOn(IDBDatabase.prototype, "transaction");
    await store.applyEventIfNew({ clientMsgId: "atomicity-check", seq: 1, session: null, frame: dummyFrame("atomicity") });

    expect(transactionSpy).toHaveBeenCalledTimes(1);
    const [storeNames, mode] = transactionSpy.mock.calls[0]!;
    expect(mode).toBe("readwrite");
    expect(Array.from(storeNames as string[]).sort()).toEqual(["appliedClientMsgIds", "events", "meta"]);
  });

  it("a forced abort mid-transaction leaves no partial writes — receipt, event body, and watermark are ALL absent (not just one of them)", async () => {
    const store = freshStore();
    await store.getWatermark(); // prime the DB connection (see note above)

    const originalPut = IDBObjectStore.prototype.put;
    const putSpy = vi
      .spyOn(IDBObjectStore.prototype, "put")
      .mockImplementationOnce(function (this: IDBObjectStore, ...args: Parameters<IDBObjectStore["put"]>) {
        // Let the first `put()` (appliedStore's receipt row) actually get queued on the
        // transaction, then abort the *whole* transaction before it can commit — this proves the
        // rollback is atomic across all three stores, not just "this one write didn't happen".
        const request = originalPut.apply(this, args);
        this.transaction.abort();
        return request;
      });

    await expect(
      store.applyEventIfNew({ clientMsgId: "abort-check", seq: 1, session: null, frame: dummyFrame("should-not-persist") }),
    ).rejects.toThrow();

    putSpy.mockRestore();

    // None of the three must have persisted — proving atomic rollback, not partial commit.
    expect(await store.hasAppliedClientMsgId("abort-check")).toBe(false);
    expect(await store.getWatermark()).toBe(0);
    expect(await store.listEvents()).toEqual([]);
  });
});
