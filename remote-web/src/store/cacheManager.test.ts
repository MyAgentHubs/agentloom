// cacheManager.test.ts — TDD 覆盖 src/store/cacheManager.ts（`purgeRoomData()`：close-then-delete
// 的房间级三库清除）。控制流测试，注入假 `deleteIndexedDb`，不碰真实 IndexedDB（真实 IndexedDB
// 端到端删除验证见 `app/RootRouter.test.tsx`，同 `app/repair.test.ts` 的既有分层取向）。

import { describe, expect, it } from "vitest";
import { deriveEventStoreDbName } from "./indexeddbEventStore.ts";
import { deriveCommandLedgerDbName } from "./commandLedger.indexeddb.ts";
import { deriveBodyCacheDbName } from "./bodyCache.indexeddb.ts";
import { purgeRoomData } from "./cacheManager.ts";

const ROOM = "0123456789abcdef0123456789abcdef";

class FakeIndexedDbDeleter {
  calls: string[] = [];
  failNames = new Set<string>();
  fn = async (name: string): Promise<void> => {
    this.calls.push(name);
    if (this.failNames.has(name)) {
      throw new Error(`delete failed (test): ${name}`);
    }
  };
}

describe("purgeRoomData()", () => {
  it("按 events → commands → bodyCache 固定顺序删除，三者都成功才 ok:true", async () => {
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: true });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(ROOM), deriveCommandLedgerDbName(ROOM), deriveBodyCacheDbName(ROOM)]);
  });

  it("先 close 全部再删库——即使没有任何一步失败，close() 也必须先被调用（close-then-delete 顺序，不是事后清理）", async () => {
    const deleter = new FakeIndexedDbDeleter();
    const closeOrder: string[] = [];
    const deleteOrder: string[] = [];
    const trackedFn = async (name: string): Promise<void> => {
      deleteOrder.push(name);
      await deleter.fn(name);
    };
    await purgeRoomData({
      room: ROOM,
      deleteIndexedDb: trackedFn,
      eventStore: { close: () => closeOrder.push("eventStore") },
      commandLedger: { close: () => closeOrder.push("commandLedger") },
      bodyCache: { close: () => closeOrder.push("bodyCache") },
    });
    expect(closeOrder).toEqual(["eventStore", "commandLedger", "bodyCache"]);
    // close 全部发生在第一次 delete 之前——不是"删一个关一个"的交错顺序。
    expect(deleteOrder[0]).toBe(deriveEventStoreDbName(ROOM));
  });

  it("省略 eventStore/commandLedger/bodyCache（没有活跃实例可关）——不抛错，直接进入删库阶段", async () => {
    const deleter = new FakeIndexedDbDeleter();
    await expect(purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn })).resolves.toEqual({ ok: true });
  });

  it("close 方法本身缺失（`{}` 而不是 `{close: fn}`）——用可选链跳过，不当错误处理", async () => {
    const deleter = new FakeIndexedDbDeleter();
    await expect(
      purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn, eventStore: {}, commandLedger: {}, bodyCache: {} }),
    ).resolves.toEqual({ ok: true });
  });

  it("events 库删除失败 → ok:false，commands/bodyCache 两库不再继续尝试", async () => {
    const deleter = new FakeIndexedDbDeleter();
    deleter.failNames.add(deriveEventStoreDbName(ROOM));
    const outcome = await purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: `delete failed (test): ${deriveEventStoreDbName(ROOM)}` });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(ROOM)]);
  });

  it("bodyCache 库删除失败 → ok:false（events/commands 两库已经先删成功了，但整体仍报失败）", async () => {
    const deleter = new FakeIndexedDbDeleter();
    deleter.failNames.add(deriveBodyCacheDbName(ROOM));
    const outcome = await purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: `delete failed (test): ${deriveBodyCacheDbName(ROOM)}` });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(ROOM), deriveCommandLedgerDbName(ROOM), deriveBodyCacheDbName(ROOM)]);
  });

  it("不同房间派生出不同的库名——不会用错房间的库名去删", async () => {
    const otherRoom = "fedcba9876543210fedcba9876543210";
    const deleter = new FakeIndexedDbDeleter();
    await purgeRoomData({ room: otherRoom, deleteIndexedDb: deleter.fn });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(otherRoom), deriveCommandLedgerDbName(otherRoom), deriveBodyCacheDbName(otherRoom)]);
    expect(deleter.calls).not.toContain(deriveEventStoreDbName(ROOM));
  });

  it("非 Error 类型的异常也被安全转成字符串", async () => {
    const deleteIndexedDb = async (name: string): Promise<void> => {
      if (name === deriveCommandLedgerDbName(ROOM)) throw "not an Error instance";
    };
    const outcome = await purgeRoomData({ room: ROOM, deleteIndexedDb });
    expect(outcome).toEqual({ ok: false, error: "not an Error instance" });
  });

  describe("msgfix2 U4 修单 H3：idbAvailable:false（内存 fallback 态）跳过真实删库", () => {
    it("idbAvailable:false——即使传入一个必然失败的 deleteIndexedDb，也不会被调用，直接 ok:true（close() 已经是内存态『删库』的全部语义）", async () => {
      const deleter = new FakeIndexedDbDeleter();
      deleter.failNames.add(deriveEventStoreDbName(ROOM));
      deleter.failNames.add(deriveCommandLedgerDbName(ROOM));
      deleter.failNames.add(deriveBodyCacheDbName(ROOM));
      const outcome = await purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn, idbAvailable: false });
      expect(outcome).toEqual({ ok: true });
      expect(deleter.calls).toEqual([]); // 真的没被调用——不是"调用了但被吞掉了错误"。
    });

    it("idbAvailable:false——仍然先 close() 传入的活跃实例（内存实现的 close() 通常是 no-op，但契约不变）", async () => {
      const deleter = new FakeIndexedDbDeleter();
      const closeOrder: string[] = [];
      const outcome = await purgeRoomData({
        room: ROOM,
        deleteIndexedDb: deleter.fn,
        idbAvailable: false,
        eventStore: { close: () => closeOrder.push("eventStore") },
        commandLedger: { close: () => closeOrder.push("commandLedger") },
        bodyCache: { close: () => closeOrder.push("bodyCache") },
      });
      expect(outcome).toEqual({ ok: true });
      expect(closeOrder).toEqual(["eventStore", "commandLedger", "bodyCache"]);
    });

    it("省略 idbAvailable（未显式声明）——保持既有行为，真的会调用 deleteIndexedDb", async () => {
      const deleter = new FakeIndexedDbDeleter();
      await purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn });
      expect(deleter.calls.length).toBeGreaterThan(0);
    });
  });

  describe("msgfix2 U4 修单二 I4：内存模式 purge 真的清空内存实现（不再只 close() 就判定成功）", () => {
    class FakeMemoryStore {
      cleared = false;
      closed = false;
      close(): void {
        this.closed = true;
      }
      async clear(): Promise<void> {
        this.cleared = true;
      }
    }

    it("idbAvailable:false——传入的三个内存实例都真的被 clear() 了（不只是 close()）", async () => {
      const deleter = new FakeIndexedDbDeleter();
      const eventStore = new FakeMemoryStore();
      const commandLedger = new FakeMemoryStore();
      const bodyCache = new FakeMemoryStore();
      const outcome = await purgeRoomData({
        room: ROOM,
        deleteIndexedDb: deleter.fn,
        idbAvailable: false,
        eventStore,
        commandLedger,
        bodyCache,
      });
      expect(outcome).toEqual({ ok: true });
      expect(eventStore.closed).toBe(true);
      expect(commandLedger.closed).toBe(true);
      expect(bodyCache.closed).toBe(true);
      expect(eventStore.cleared).toBe(true);
      expect(commandLedger.cleared).toBe(true);
      expect(bodyCache.cleared).toBe(true);
    });

    it("idbAvailable:false——clear() 抛错 → ok:false（不吞错，让调用方知道内存态清理失败了）", async () => {
      const deleter = new FakeIndexedDbDeleter();
      const bodyCache = {
        clear: async () => {
          throw new Error("in-memory clear failed (test)");
        },
      };
      const outcome = await purgeRoomData({ room: ROOM, deleteIndexedDb: deleter.fn, idbAvailable: false, bodyCache });
      expect(outcome).toEqual({ ok: false, error: "in-memory clear failed (test)" });
    });

    it("端到端：InMemoryEventStore/InMemoryCommandLedger/InMemoryBodyCache 三个真实内存实现——purge 之后读回全部为空", async () => {
      const { InMemoryEventStore } = await import("./inMemoryEventStore.ts");
      const { InMemoryCommandLedger } = await import("./commandLedger.ts");
      const { InMemoryBodyCache } = await import("./bodyCache.ts");

      const eventStore = new InMemoryEventStore();
      await eventStore.applyEventIfNew({ clientMsgId: "cmid-1", seq: 5, session: null, frame: { t: "x" } });

      const commandLedger = new InMemoryCommandLedger();
      await commandLedger.recordSent({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1 });

      const bodyCache = new InMemoryBodyCache();
      await bodyCache.put({ room: ROOM, session: "sess-1", messageId: 1, contentSha256: "a".repeat(64) }, [{ text: "hi" }], new Uint8Array([1, 2, 3]));

      const deleter = new FakeIndexedDbDeleter();
      const outcome = await purgeRoomData({
        room: ROOM,
        deleteIndexedDb: deleter.fn,
        idbAvailable: false,
        eventStore,
        commandLedger,
        bodyCache,
      });
      expect(outcome).toEqual({ ok: true });
      expect(deleter.calls).toEqual([]); // 真的没碰真实 deleteIndexedDb。

      // 核心断言：读回全部为空——不是"close() 了但数据还在"。
      expect(await eventStore.getWatermark()).toBe(0);
      expect(await eventStore.hasAppliedClientMsgId("cmid-1")).toBe(false);
      expect(await commandLedger.isOwn("cmd-1")).toBe(false);
      expect(await bodyCache.get({ room: ROOM, session: "sess-1", messageId: 1, contentSha256: "a".repeat(64) })).toBeNull();
    });
  });
});
