// bodyCache.indexeddb.test.ts — TDD 覆盖 src/store/bodyCache.indexeddb.ts（`IndexedDbBodyCache`，
// 用 `fake-indexeddb` 在 node 环境驱动真实的 IndexedDB 事务语义——同 `commandLedger.test.ts`/
// `indexeddbEventStore.test.ts` 的既有惯例，不是手写 in-memory mock 顶替真实行为）。

import { describe, expect, it, vi } from "vitest";
import { indexedDB as fakeIndexedDB, IDBDatabase } from "fake-indexeddb";
import { BODY_CACHE_LRU_CAP_BYTES, deriveBodyCacheDbName, IndexedDbBodyCache } from "./bodyCache.indexeddb.ts";
import type { BodyCacheKey } from "./bodyCache.ts";

const ROOM = "0123456789abcdef0123456789abcdef";
const KEY: BodyCacheKey = { room: ROOM, session: "sess-1", messageId: 1, contentSha256: "a".repeat(64) };

let dbCounter = 0;
function freshCache(capBytes = BODY_CACHE_LRU_CAP_BYTES): IndexedDbBodyCache {
  dbCounter += 1;
  return new IndexedDbBodyCache(`test-body-cache-${dbCounter}-${Date.now()}`, fakeIndexedDB, capBytes);
}

describe("deriveBodyCacheDbName()", () => {
  it("按房间派生不同库名——换房不串库（同既有约定）", () => {
    expect(deriveBodyCacheDbName(ROOM)).not.toBe(deriveBodyCacheDbName("f".repeat(32)));
    expect(deriveBodyCacheDbName(ROOM)).toContain(ROOM);
  });
});

describe("IndexedDbBodyCache · 基本读写", () => {
  it("未写入时 get() 返回 null", async () => {
    const cache = freshCache();
    expect(await cache.get(KEY)).toBeNull();
  });

  it("put() 之后 get() 命中，blocks/bytes 原样拿回", async () => {
    const cache = freshCache();
    const blocks = [{ type: "text", text: "hello world" }];
    const bytes = new Uint8Array([10, 20, 30]);
    await cache.put(KEY, blocks, bytes);
    const got = await cache.get(KEY);
    expect(got?.blocks).toEqual(blocks);
    expect(got?.bytes).toEqual(bytes);
  });

  it("get() 命中会 touch（cachedAt 更新为最近一次读取时间）", async () => {
    let now = 1_000;
    const cache = new IndexedDbBodyCache(`test-body-cache-touch-${Date.now()}`, fakeIndexedDB, BODY_CACHE_LRU_CAP_BYTES, () => now);
    await cache.put(KEY, [], new Uint8Array([1]));
    const first = await cache.get(KEY);
    expect(first?.cachedAt).toBe(1_000);
    now = 5_000;
    const second = await cache.get(KEY);
    expect(second?.cachedAt).toBe(5_000);
  });

  it("clear() 清空 bodies 与 meta 两个 object store（清空后总量归零，不会误触发下一次 put() 的淘汰）", async () => {
    const cache = freshCache();
    await cache.put(KEY, [], new Uint8Array(10));
    await cache.clear();
    expect(await cache.get(KEY)).toBeNull();
  });

  it("同 key 重复 put() 覆盖内容，且总量计费不重复累加旧的大小", async () => {
    const cache = freshCache(1000);
    await cache.put(KEY, [{ v: 1 }], new Uint8Array(100));
    await cache.put(KEY, [{ v: 2 }], new Uint8Array(50));
    const got = await cache.get(KEY);
    expect(got?.blocks).toEqual([{ v: 2 }]);
    expect(got?.bytes.length).toBe(50);
  });

  it("msgfix2 U4 修单 H4：delete() 精确删除一条——不影响其它 key，总量正确扣减", async () => {
    const cache = freshCache(1000);
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 };
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    await cache.put(keyA, [{ v: 1 }], new Uint8Array(10));
    await cache.put(keyB, [{ v: 2 }], new Uint8Array(4));
    await cache.delete(keyA);
    expect(await cache.get(keyA)).toBeNull();
    expect((await cache.get(keyB))?.blocks).toEqual([{ v: 2 }]);
    // 扣账正确——之后再写入一条撑到刚好卡上限不该触发淘汰（若 delete() 没扣掉 A 那 20
    // sizeBytes，这里会被误判超限提前淘汰掉 B）。msgfix2 F2 S5③：sizeBytes 计量按
    // `bytes.length*2` 记账（不是原始字节数）——B 现存 sizeBytes=4*2=8，最后这条写 496 字节
    // （sizeBytes=992），992+8=1000 恰好卡满 cap=1000。
    await cache.put({ ...KEY, messageId: 3 }, [], new Uint8Array(496));
    expect((await cache.get(keyB))?.bytes.length).toBe(4);
  });

  it("delete() 删一个本就不存在的 key——no-op，不抛错", async () => {
    const cache = freshCache();
    await expect(cache.delete(KEY)).resolves.toBeUndefined();
  });

  it("msgfix2 F2 S5③：计量按 bytes.length*2 保守估算（不是只算 bytes，正文在内存里还留着一份 blocks 副本，见 bodyCache.indexeddb.ts:198 头注）——同样的正文现在占的计量份额是原来的两倍，更早触发淘汰", async () => {
    const cap = 10;
    const cache = new IndexedDbBodyCache(`test-body-cache-sizebytes-${Date.now()}`, fakeIndexedDB, cap);
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 };
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    // 旧公式（sizeBytes=bytes.length）：A(4)+B(3)=7，不超 cap=10，两条都该活着——不淘汰。
    // 新公式（sizeBytes=bytes.length*2）：A 记 8，B 记 6，总量 14>10，必须淘汰最旧的 A 才能回到
    // 限内（8+6-8=6<=10）。用这个门槛差异直接证明计量公式真的翻倍了，不是只测"能不能淘汰"这种
    // 旧测试本来就覆盖过的行为。
    await cache.put(keyA, [], new Uint8Array(4));
    await cache.put(keyB, [], new Uint8Array(3));
    expect(await cache.get(keyA)).toBeNull();
    expect((await cache.get(keyB))?.bytes.length).toBe(3);
  });
});

describe("IndexedDbBodyCache · msgfix2 U4 修单 H6（get() 读+touch 合并单事务）", () => {
  it("get() 命中时只开一个 IndexedDB 事务（不是先 readonly 读一次、再另开一个 readwrite 写回 touch）", async () => {
    const cache = freshCache();
    await cache.put(KEY, [], new Uint8Array(1));

    const transactionSpy = vi.spyOn(IDBDatabase.prototype, "transaction");
    transactionSpy.mockClear();
    await cache.get(KEY);
    // openDb() 已经在 put() 时建立过连接，这次 get() 期间新开的事务只应该有 get() 自己那一个
    // （合并前是 2 个：一个 readonly get + 一个 readwrite touch）。
    expect(transactionSpy).toHaveBeenCalledTimes(1);
    transactionSpy.mockRestore();
  });
});

describe("IndexedDbBodyCache · LRU 淘汰（总量超限时按 cachedAt 升序丢最久未用）", () => {
  it("超过上限时淘汰最久未用的条目，touch 过的条目优先保留", async () => {
    let now = 0;
    // msgfix2 F2 S5③：sizeBytes 计量改按 `bytes.length*2` 记账——cap 同步翻倍（20→40），保持
    // 跟下面三条 `new Uint8Array(10)`（sizeBytes 各 20）的相对触发关系跟改动前完全一致，只是
    // 记账口径整体翻了一倍（cap 翻倍抵消了 sizeBytes 翻倍，触发淘汰的时间点不变）。
    const cache = new IndexedDbBodyCache(`test-body-cache-lru-${Date.now()}`, fakeIndexedDB, 40, () => now);
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 };
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    const keyC: BodyCacheKey = { ...KEY, messageId: 3 };

    now = 1;
    await cache.put(keyA, [], new Uint8Array(10));
    now = 2;
    await cache.put(keyB, [], new Uint8Array(10));
    // touch A——让它比 B "更新"。
    now = 3;
    await cache.get(keyA);
    // 写入 C（sizeBytes 20）把总量推到 60，超过 40 的上限——B 最久未 touch，先被淘汰。
    now = 4;
    await cache.put(keyC, [], new Uint8Array(10));

    expect(await cache.get(keyB)).toBeNull();
    expect((await cache.get(keyA))?.bytes.length).toBe(10);
    expect((await cache.get(keyC))?.bytes.length).toBe(10);
    // msgfix2 U4 修单 H7：总量真的回到限内（不是只信任"B 被淘汰了"这一件事）——按 sizeBytes
    // 口径换算回原始字节数就是 cap/2=20（两条各 10 字节的正文）。
    expect(((await cache.get(keyA))?.bytes.length ?? 0) + ((await cache.get(keyC))?.bytes.length ?? 0)).toBeLessThanOrEqual(20);
  });

  it("msgfix2 U4 修单 H7：一次写入需要连续淘汰多条才能回到限内——按 cachedAt 升序（最旧先走）依次丢，够了就停，不多丢", async () => {
    let now = 0;
    // msgfix2 F2 S5③：cap 同步翻倍（15→30），道理同上一条用例头注。
    const cache = new IndexedDbBodyCache(`test-body-cache-lru-multi-${Date.now()}`, fakeIndexedDB, 30, () => now);
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 }; // 最旧。
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    const keyC: BodyCacheKey = { ...KEY, messageId: 3 }; // 最新，不该被淘汰。
    const keyD: BodyCacheKey = { ...KEY, messageId: 4 }; // 触发淘汰的这次写入本身。

    now = 1;
    await cache.put(keyA, [], new Uint8Array(5)); // sizeBytes 10，总量 10。
    now = 2;
    await cache.put(keyB, [], new Uint8Array(5)); // sizeBytes 10，总量 20。
    now = 3;
    await cache.put(keyC, [], new Uint8Array(5)); // sizeBytes 10，总量 30，正好打满上限，不触发淘汰。
    // 写入 D（sizeBytes 16）把总量推到 46，超过 30 的上限——必须连续淘汰两轮：先丢最旧的 A
    // （10，总量 36 仍超），再丢 B（10，总量 26<=30，停）；C 是三者里最新的一个，全程不该被碰。
    now = 4;
    await cache.put(keyD, [], new Uint8Array(8));

    expect(await cache.get(keyA)).toBeNull(); // 第一轮淘汰。
    expect(await cache.get(keyB)).toBeNull(); // 第二轮淘汰。
    const survivorC = await cache.get(keyC);
    const survivorD = await cache.get(keyD);
    expect(survivorC?.bytes.length).toBe(5);
    expect(survivorD?.bytes.length).toBe(8);
    // 总量回到限内——按 sizeBytes 口径换算回原始字节数就是 cap/2=15。
    expect((survivorC?.bytes.length ?? 0) + (survivorD?.bytes.length ?? 0)).toBeLessThanOrEqual(15);
  });

  it("淘汰只删正文行——不影响其它 key 的独立读写（`meta.totalBytes` 正确反映剩余总量）", async () => {
    // msgfix2 F2 S5③：cap 同步翻倍（15→30），道理同上面两条用例头注。
    const cache = freshCache(30);
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 };
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    await cache.put(keyA, [], new Uint8Array(10));
    await cache.put(keyB, [], new Uint8Array(10)); // 总量 40 > 30，淘汰 A（最旧）。
    expect(await cache.get(keyA)).toBeNull();
    expect((await cache.get(keyB))?.bytes.length).toBe(10);
    // 再写一条 4 字节的（sizeBytes 8）——加上 B 现存的 sizeBytes 20 = 28，仍在 30 的上限内，
    // 不该触发任何淘汰。
    const keyC: BodyCacheKey = { ...KEY, messageId: 3 };
    await cache.put(keyC, [], new Uint8Array(4));
    expect((await cache.get(keyB))?.bytes.length).toBe(10);
    expect((await cache.get(keyC))?.bytes.length).toBe(4);
  });
});

describe("IndexedDbBodyCache · 单一串行队列（写入/计量/淘汰原子化）", () => {
  it("并发发起多个 put()——全部落地，总量与最终可读内容都正确（不会因为交错读到中间态而算错总量）", async () => {
    const cache = freshCache(1_000_000);
    const puts = Array.from({ length: 8 }, (_, i) =>
      cache.put({ ...KEY, messageId: i }, [{ i }], new Uint8Array(100).fill(i)),
    );
    await Promise.all(puts);
    for (let i = 0; i < 8; i++) {
      const got = await cache.get({ ...KEY, messageId: i });
      expect(got?.blocks).toEqual([{ i }]);
      expect(got?.bytes.length).toBe(100);
    }
  });
});

describe("IndexedDbBodyCache · close-then-delete（收拢 P1-3）", () => {
  it("close() 之后立即 deleteDatabase() 不会卡在 onblocked（连接已经主动放手）", async () => {
    const dbName = `test-body-cache-close-${Date.now()}`;
    const cache = new IndexedDbBodyCache(dbName, fakeIndexedDB);
    await cache.put(KEY, [], new Uint8Array([1, 2, 3])); // 建立一次真实连接。
    cache.close();
    await new Promise<void>((resolve, reject) => {
      const request = fakeIndexedDB.deleteDatabase(dbName);
      request.onsuccess = () => resolve();
      request.onerror = () => reject(request.error);
      request.onblocked = () => reject(new Error("deleteDatabase blocked — close() did not release the connection"));
    });
  });

  it("close() 在从未建立过连接时是 no-op（不抛错）", () => {
    const cache = new IndexedDbBodyCache(`test-body-cache-never-opened-${Date.now()}`, fakeIndexedDB);
    expect(() => cache.close()).not.toThrow();
  });

  it("close() 之后再操作会重新建立连接（不是永久失效）", async () => {
    const dbName = `test-body-cache-reopen-${Date.now()}`;
    const cache = new IndexedDbBodyCache(dbName, fakeIndexedDB);
    await cache.put(KEY, [], new Uint8Array([1]));
    cache.close();
    await cache.put({ ...KEY, messageId: 2 }, [], new Uint8Array([2]));
    expect((await cache.get({ ...KEY, messageId: 2 }))?.bytes.length).toBe(1);
  });

  it("msgfix2 F2 S3：versionchange 触发后（另一个上下文对同一个库发起 deleteDatabase()）——下一次操作重新 openDb() 成功，不是攥着已 close 的 stale 连接恒抛 InvalidStateError", async () => {
    const dbName = `test-body-cache-versionchange-${Date.now()}`;
    const cache = new IndexedDbBodyCache(dbName, fakeIndexedDB);
    await cache.put(KEY, [], new Uint8Array([1]));

    // 同 `indexeddbEventStore.test.ts`/`commandLedger.test.ts` 同名用例——模拟"另一个标签页"对
    // 同一个库发起 deleteDatabase()，本店的 `onversionchange` 处理器 close() 已建立的连接，
    // deleteDatabase() 才不会卡在 onblocked。旧版只清 `dbHandle` 不清 `dbPromise`，下一次
    // `openDb()` 会直接复用那个已经 close 的 stale 连接，对已关闭连接开事务恒抛 InvalidStateError
    // ——且这条路径的降级闩是"静默切内存"（`bodyCache.ts::withMemoryFallback`），不会像
    // `AppRuntime.tsx` 的 eventStore 那样至少留一条 console.error，问题会在没有任何信号的情况下
    // 悄悄发生（历史清空零信号）。
    await new Promise<void>((resolve, reject) => {
      const req = fakeIndexedDB.deleteDatabase(dbName);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error ?? new Error("deleteDatabase failed"));
      req.onblocked = () => reject(new Error("deleteDatabase blocked — onversionchange did not release the connection"));
    });

    // 核心断言：下一次操作重新 openDb() 成功——库被删过，重新写入正常生效、读得到。
    await cache.put({ ...KEY, messageId: 3 }, [{ v: 1 }], new Uint8Array([9, 9]));
    const got = await cache.get({ ...KEY, messageId: 3 });
    expect(got?.bytes.length).toBe(2);
  });
});
