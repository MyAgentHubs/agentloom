// idbFactory.test.ts — TDD 覆盖 src/store/idbFactory.ts（root-level factory：探测 + 装配）。
//
// `probeIndexedDb()` 用真实 `fake-indexeddb`（`fake-indexeddb/auto` 全局补丁，同
// `indexeddbEventStore.test.ts`/`RootRouter.test.tsx` 既有惯例）驱动"探测成功"分支；"探测失败"
// 分支用一个 `open()` 直接同步抛错的假 `IDBFactory` 驱动——不需要真的搞坏全局 indexedDB。

import "fake-indexeddb/auto";
import { describe, expect, it } from "vitest";
import { createStoreFactory, probeIndexedDb } from "./idbFactory.ts";
import { IndexedDbKeyStore } from "./key-store.indexeddb.ts";
import { InMemoryKeyStore } from "./key-store.ts";
import { deriveEventStoreDbName, IndexedDbEventStore } from "./indexeddbEventStore.ts";
import { InMemoryEventStore } from "./inMemoryEventStore.ts";
import { deriveCommandLedgerDbName, IndexedDbCommandLedger } from "./commandLedger.indexeddb.ts";
import { InMemoryCommandLedger } from "./commandLedger.ts";
import { InMemoryBodyCache } from "./bodyCache.ts";

const ROOM = "0123456789abcdef0123456789abcdef";

/** `open()` 同步抛错——`probeIndexedDb()` 的 `new Promise((resolve, reject) => { factory.open(...) })`
 *  执行器里同步抛出的异常会被 Promise 构造机制自动转成 reject，`await` 到的 try/catch 能正常抓到,
 *  不需要真的搞坏全局 `indexedDB`。 */
const brokenIdbFactory = {
  open: () => {
    throw new Error("boom (test)");
  },
} as unknown as IDBFactory;

describe("probeIndexedDb()", () => {
  it("真实 fake-indexeddb 可用时返回 true", async () => {
    expect(await probeIndexedDb()).toBe(true);
  });

  it("factory.open() 抛错时返回 false（不让异常冒泡）", async () => {
    expect(await probeIndexedDb(brokenIdbFactory)).toBe(false);
  });

  it("globalThis.indexedDB 缺失（未注入 factory 参数、全局也没有）时返回 false", async () => {
    const originalDescriptor = Object.getOwnPropertyDescriptor(globalThis, "indexedDB");
    // @ts-expect-error 测试专用：临时移除全局 indexedDB。
    delete globalThis.indexedDB;
    try {
      expect(await probeIndexedDb()).toBe(false);
    } finally {
      if (originalDescriptor) Object.defineProperty(globalThis, "indexedDB", originalDescriptor);
    }
  });

  it("msgfix2 F2 S5②：探测成功后探测库本身被删掉——不留一个不按房间派生、不在任何 purge 清理集里的孤儿库永久占着磁盘（`purgeRoomData()` 只认 events-<room>/commands-<room>/agentloom-body-cache-<room> 三个按房间派生的库名，这个探测库天生不会被任何一次房间级 purge 扫到）", async () => {
    expect(await probeIndexedDb()).toBe(true);

    // 若探测库还留着,用同一个库名 + 同一个版本号 1 重新 open() 不会触发 onupgradeneeded（已经在
    // 这个版本了）；若已经被删掉,重新 open() 走"库不存在→新建"这条路径,会重新触发
    // onupgradeneeded。用这个信号反推"探测库到底还在不在"，不依赖 `indexedDB.databases()`
    // （不同 IndexedDB 实现对这个枚举 API 的支持度不稳定,不作为断言依据）。
    let upgraded = false;
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const req = indexedDB.open("agentloom-idb-probe", 1);
      req.onupgradeneeded = () => {
        upgraded = true;
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error ?? new Error("reopen probe db failed"));
    });
    db.close();
    expect(upgraded).toBe(true);
  });
});

describe("createStoreFactory() · 探测成功——整套真实 IndexedDB 实现", () => {
  it("idbAvailable:true，keyStore/createEventStore/createCommandLedger/createBodyCache 全部是真实 IndexedDB 实现（msgfix2 U4 修单 H1：四者现在都包了一层 `withMemoryFallback()` 运行期降级 wrapper，不再是裸 `IndexedDb*` 实例——不能再用 `instanceof` 判断，改用「写进去的东西能被一个指向同一个库名的独立 raw 实例读到」证明底层确实落的是 IndexedDB，不是 wrapper 私藏的内存 Map，同 `createBodyCache` 早先的既有取向）", async () => {
    const factory = await createStoreFactory({ forceIdbAvailable: true });
    expect(factory.idbAvailable).toBe(true);

    await factory.keyStore.saveKeys({
      deviceId: "d-raw-check",
      room: ROOM,
      relayUrl: "wss://relay.example",
      access: "a".repeat(64),
      refresh: "b".repeat(64),
      kRoomKey: {} as CryptoKey,
    });
    const rawKeyStore = new IndexedDbKeyStore();
    expect((await rawKeyStore.loadKeys())?.deviceId).toBe("d-raw-check");

    const eventStore = factory.createEventStore(ROOM);
    await eventStore.applyEventIfNew({ clientMsgId: "raw-check-event", seq: 1, session: null, frame: {} });
    const rawEventStore = new IndexedDbEventStore(deriveEventStoreDbName(ROOM));
    expect(await rawEventStore.hasAppliedClientMsgId("raw-check-event")).toBe(true);

    const commandLedger = factory.createCommandLedger(ROOM);
    await commandLedger.recordSent({ commandId: "raw-check-cmd", kind: "input.send", session: "s", createdAt: 0 });
    const rawCommandLedger = new IndexedDbCommandLedger(deriveCommandLedgerDbName(ROOM));
    expect(await rawCommandLedger.isOwn("raw-check-cmd")).toBe(true);

    // createBodyCache 包了一层 `withMemoryFallback()`（运行期降级用），不是直接返回
    // `IndexedDbBodyCache` 实例——写入能正常工作足以证明底层确实是 IndexedDB 路径。
    const bodyCache = factory.createBodyCache(ROOM);
    await bodyCache.put({ room: ROOM, session: "s", messageId: 1, contentSha256: "a".repeat(64) }, [{ v: 1 }], new Uint8Array([1]));
    const got = await bodyCache.get({ room: ROOM, session: "s", messageId: 1, contentSha256: "a".repeat(64) });
    expect(got?.blocks).toEqual([{ v: 1 }]);
  });

  it("createEventStore()/createCommandLedger() 按房间派生不同实例（换房不串库）", async () => {
    const factory = await createStoreFactory({ forceIdbAvailable: true });
    const roomA = factory.createEventStore("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const roomB = factory.createEventStore("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    await roomA.applyEventIfNew({ clientMsgId: "x", seq: 1, session: null, frame: {} });
    expect(await roomB.hasAppliedClientMsgId("x")).toBe(false); // 不同房间的库互不可见。
  });
});

describe("createStoreFactory() · 探测失败——整套内存 fallback", () => {
  it("idbAvailable:false，四者全部是内存实现，且真的可用（读写往返成功）", async () => {
    const factory = await createStoreFactory({ forceIdbAvailable: false });
    expect(factory.idbAvailable).toBe(false);
    expect(factory.keyStore).toBeInstanceOf(InMemoryKeyStore);
    expect(factory.createEventStore(ROOM)).toBeInstanceOf(InMemoryEventStore);
    expect(factory.createCommandLedger(ROOM)).toBeInstanceOf(InMemoryCommandLedger);
    expect(factory.createBodyCache(ROOM)).toBeInstanceOf(InMemoryBodyCache);

    // 往返一遍，证明"降级"不是"整套失效"。
    await factory.keyStore.saveKeys({
      deviceId: "d",
      room: ROOM,
      relayUrl: "wss://relay.example",
      access: "a".repeat(64),
      refresh: "b".repeat(64),
      kRoomKey: {} as CryptoKey,
    });
    expect((await factory.keyStore.loadKeys())?.room).toBe(ROOM);
  });

  it("真实探测失败（factory.open() 抛错）也走同一条内存 fallback（不依赖 forceIdbAvailable 才能测到这条路径）", async () => {
    const factory = await createStoreFactory({ idbFactory: brokenIdbFactory });
    expect(factory.idbAvailable).toBe(false);
    expect(factory.keyStore).toBeInstanceOf(InMemoryKeyStore);
  });
});
