// bodyCache.test.ts — TDD 覆盖 src/store/bodyCache.ts（`InMemoryBodyCache` + `bodyCacheKeyString` +
// `withMemoryFallback` 单点包装）。IndexedDB 实现的等价契约见 `bodyCache.indexeddb.test.ts`。

import { describe, expect, it } from "vitest";
import { InMemoryBodyCache, bodyCacheKeyString, withMemoryFallback, type BodyCacheKey, type BodyCachePort, type CachedBody } from "./bodyCache.ts";

const KEY: BodyCacheKey = { room: "room-1", session: "sess-1", messageId: 42, contentSha256: "a".repeat(64) };

describe("bodyCacheKeyString()", () => {
  it("拼接四段——room|session|messageId|contentSha256", () => {
    expect(bodyCacheKeyString(KEY)).toBe("room-1|sess-1|42|" + "a".repeat(64));
  });

  it("任一段不同都产生不同的 key（revision 变化天然换 key——不同 contentSha256）", () => {
    const other = { ...KEY, contentSha256: "b".repeat(64) };
    expect(bodyCacheKeyString(KEY)).not.toBe(bodyCacheKeyString(other));
  });
});

describe("InMemoryBodyCache", () => {
  it("未写入时 get() 返回 null", async () => {
    const cache = new InMemoryBodyCache();
    expect(await cache.get(KEY)).toBeNull();
  });

  it("put() 之后 get() 命中，拿到相同的 blocks/bytes", async () => {
    const cache = new InMemoryBodyCache();
    const blocks = [{ type: "text", text: "hello" }];
    const bytes = new Uint8Array([1, 2, 3]);
    await cache.put(KEY, blocks, bytes);
    const got = await cache.get(KEY);
    expect(got?.blocks).toEqual(blocks);
    expect(got?.bytes).toEqual(bytes);
  });

  it("get() 命中会 touch（更新 cachedAt）", async () => {
    let now = 1000;
    const cache = new InMemoryBodyCache(8 * 1024 * 1024, () => now);
    await cache.put(KEY, [], new Uint8Array([1]));
    const first = await cache.get(KEY);
    expect(first?.cachedAt).toBe(1000);
    now = 2000;
    const second = await cache.get(KEY);
    expect(second?.cachedAt).toBe(2000);
  });

  it("clear() 清空全部条目", async () => {
    const cache = new InMemoryBodyCache();
    await cache.put(KEY, [], new Uint8Array([1, 2, 3]));
    await cache.clear();
    expect(await cache.get(KEY)).toBeNull();
    expect(cache.size).toBe(0);
  });

  it("同 key 重复 put() 覆盖，不重复计入总量", async () => {
    const cache = new InMemoryBodyCache();
    await cache.put(KEY, [], new Uint8Array(10));
    await cache.put(KEY, [], new Uint8Array(20));
    expect(cache.size).toBe(20);
  });

  it("LRU 淘汰：总量超过上限时丢最久未用的条目，命中过的（touch 过的）优先保留", async () => {
    // 上限设成正好放下两条 10 字节的记录。
    const cache = new InMemoryBodyCache(20);
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 };
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    const keyC: BodyCacheKey = { ...KEY, messageId: 3 };
    await cache.put(keyA, [], new Uint8Array(10));
    await cache.put(keyB, [], new Uint8Array(10));
    // touch A——让它比 B 更"新"。
    await cache.get(keyA);
    // 写入 C（10 字节）会把总量推到 30，超过 20 的上限——必须淘汰到 <=20：B 最久未用，先丢。
    await cache.put(keyC, [], new Uint8Array(10));
    expect(await cache.get(keyB)).toBeNull(); // 被淘汰。
    expect(await cache.get(keyA)).not.toBeNull(); // touch 过的活下来。
    expect(await cache.get(keyC)).not.toBeNull(); // 最新写入的活下来。
    expect(cache.size).toBeLessThanOrEqual(20); // 总量真的回到限内。
  });

  it("msgfix2 U4 修单 H7：一次写入需要连续淘汰多条才能回到限内——按 cachedAt 升序（最旧先走）依次丢，够了就停，不多丢", async () => {
    const cache = new InMemoryBodyCache(15);
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 }; // 最旧。
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    const keyC: BodyCacheKey = { ...KEY, messageId: 3 }; // 最新，不该被淘汰。
    const keyD: BodyCacheKey = { ...KEY, messageId: 4 }; // 触发淘汰的这次写入本身。
    await cache.put(keyA, [], new Uint8Array(5)); // 总量 5。
    await cache.put(keyB, [], new Uint8Array(5)); // 总量 10。
    await cache.put(keyC, [], new Uint8Array(5)); // 总量 15，正好打满上限，不触发淘汰。
    // 写入 D（8 字节）把总量推到 23，超过 15 的上限——必须连续淘汰两轮：先丢 A（5，总量 18 仍超）
    // 再丢 B（5，总量 13<=15，停）；C 是三者里最新的一个，全程不该被碰。
    await cache.put(keyD, [], new Uint8Array(8));
    expect(await cache.get(keyA)).toBeNull(); // 第一轮淘汰。
    expect(await cache.get(keyB)).toBeNull(); // 第二轮淘汰。
    expect(await cache.get(keyC)).not.toBeNull(); // 全程未被碰。
    expect(await cache.get(keyD)).not.toBeNull(); // 这次写入本身留下。
    expect(cache.size).toBeLessThanOrEqual(15);
    expect(cache.size).toBe(13); // C(5) + D(8)。
  });

  it("msgfix2 U4 修单 H4：delete() 精确删除一条——不影响其它 key，总量正确扣减", async () => {
    const cache = new InMemoryBodyCache();
    const keyA: BodyCacheKey = { ...KEY, messageId: 1 };
    const keyB: BodyCacheKey = { ...KEY, messageId: 2 };
    await cache.put(keyA, [{ text: "a" }], new Uint8Array(10));
    await cache.put(keyB, [{ text: "b" }], new Uint8Array(4));
    expect(cache.size).toBe(14);
    await cache.delete(keyA);
    expect(await cache.get(keyA)).toBeNull();
    expect((await cache.get(keyB))?.blocks).toEqual([{ text: "b" }]);
    expect(cache.size).toBe(4);
  });

  it("delete() 删一个本就不存在的 key——no-op，不抛错", async () => {
    const cache = new InMemoryBodyCache();
    await expect(cache.delete(KEY)).resolves.toBeUndefined();
  });

  it("单条记录本身就超过上限——仍然写入（不拒绝单条超大写入），随后立刻被自己触发的淘汰清空", async () => {
    const cache = new InMemoryBodyCache(5);
    await cache.put(KEY, [], new Uint8Array(10));
    // 淘汰循环会把这条自己刚写的记录也丢掉（它是队列里唯一的一条，也是"最旧"的一条）——
    // 这是设计上可接受的边界（body cache 只是加速层，丢了下次重拉）。
    expect(await cache.get(KEY)).toBeNull();
  });
});

describe("withMemoryFallback()", () => {
  class FlakyBodyCache implements BodyCachePort {
    getCalls = 0;
    putCalls = 0;
    shouldFail = true;
    async get(): Promise<null> {
      this.getCalls += 1;
      if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
      return null;
    }
    async put(): Promise<void> {
      this.putCalls += 1;
      if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    }
    deleteCalls = 0;
    async delete(): Promise<void> {
      this.deleteCalls += 1;
      if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    }
    async clear(): Promise<void> {}
  }

  it("primary.put() 抛错——降级到内存实现重试一次，这次成功；之后的调用直接走内存实现，不再碰 primary", async () => {
    const primary = new FlakyBodyCache();
    let fallbackBuilds = 0;
    const cache = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryBodyCache();
    });

    await cache.put(KEY, [{ text: "hi" }], new Uint8Array([1, 2, 3]));
    expect(primary.putCalls).toBe(1); // 试过 primary 一次，失败了。
    expect(fallbackBuilds).toBe(1); // 换到内存实现，只建一次。

    const got = await cache.get(KEY);
    expect(got?.blocks).toEqual([{ text: "hi" }]); // 内存版本重试确实把内容存住了。
    expect(primary.getCalls).toBe(0); // 已经降级，不再去碰 primary。

    // 第二次 put() 直接走内存版本，不再尝试 primary（不会再累加 primary.putCalls）。
    await cache.put({ ...KEY, messageId: 99 }, [], new Uint8Array([9]));
    expect(primary.putCalls).toBe(1);
    expect(fallbackBuilds).toBe(1); // 内存实例只建一次，不重复构造。
  });

  it("primary.get() 抛错——降级为未命中（不是让异常冒泡打断呈现主流程）", async () => {
    const primary = new FlakyBodyCache();
    const cache = withMemoryFallback(primary, () => new InMemoryBodyCache());
    await expect(cache.get(KEY)).resolves.toBeNull();
  });

  it("primary 正常工作时不会构造 fallback（不需要就不建）", async () => {
    const primary = new FlakyBodyCache();
    primary.shouldFail = false;
    let fallbackBuilds = 0;
    const cache = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryBodyCache();
    });
    await cache.put(KEY, [], new Uint8Array([1]));
    await cache.get(KEY);
    expect(fallbackBuilds).toBe(0);
  });

  it("msgfix2 U4 修单 H4：primary.delete() 抛错——降级到内存实现重试一次", async () => {
    const primary = new FlakyBodyCache();
    const cache = withMemoryFallback(primary, () => new InMemoryBodyCache());
    await expect(cache.delete(KEY)).resolves.toBeUndefined();
    expect(primary.deleteCalls).toBe(1);
  });

  it("close() 透传给 primary 与已构造的 fallback（若有）", async () => {
    let primaryClosed = false;
    let fallbackClosed = false;
    const primary: BodyCachePort = {
      async get() {
        throw new Error("fail");
      },
      async put() {
        throw new Error("fail");
      },
      async delete() {},
      async clear() {},
      close: () => (primaryClosed = true),
    };
    const cache = withMemoryFallback(primary, () => ({
      async get() {
        return null;
      },
      async put() {},
      async delete() {},
      async clear() {},
      close: () => (fallbackClosed = true),
    }));
    await cache.put(KEY, [], new Uint8Array([1])); // 触发降级，构造 fallback。
    cache.close?.();
    expect(primaryClosed).toBe(true);
    expect(fallbackClosed).toBe(true);
  });

  /**
   * msgfix2 U4 修单二 I1 竞态专用 primary——`put()` 的行为按调用次序受控：第一次调用真挂起（测试
   * 代码手动 `resolvePending()` 放行），下一次调用可以武装成立即失败一次（`failNextCall()`），
   * 模拟"op A 在飞中，op B（同一个 wrapper 上的另一次并发写）先一步触发降级"这个时序。
   */
  class RaceBodyCache implements BodyCachePort {
    private readonly inner = new InMemoryBodyCache();
    putCalls: BodyCacheKey[] = [];
    private pendingResolvers: Array<() => void> = [];
    private nextCallShouldFail = false;

    async get(key: BodyCacheKey): Promise<CachedBody | null> {
      return this.inner.get(key);
    }
    async put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void> {
      this.putCalls.push(key);
      if (this.nextCallShouldFail) {
        this.nextCallShouldFail = false;
        throw new Error("race primary put failed (test)");
      }
      await new Promise<void>((resolve) => this.pendingResolvers.push(resolve));
      await this.inner.put(key, blocks, bytes);
    }
    async delete(key: BodyCacheKey): Promise<void> {
      return this.inner.delete(key);
    }
    async clear(): Promise<void> {
      return this.inner.clear();
    }
    failNextCall(): void {
      this.nextCallShouldFail = true;
    }
    resolvePending(): void {
      const resolver = this.pendingResolvers.shift();
      if (!resolver) throw new Error("resolvePending() called with no pending put() to resolve (test setup bug)");
      resolver();
    }
    get pendingCount(): number {
      return this.pendingResolvers.length;
    }
  }

  describe("msgfix2 U4 修单二 I1：fallback 单向闩防双账本——飞行中的 primary 结果不因竞态被误落账", () => {
    it("op A（put）在飞中，op B（同一个 wrapper 上的另一次并发 put）先失败触发降级——A 落地时感知到闩已经跳了，这次 primary 结果丢弃不落账（不追加写 fallback），A 这条 key 之后读不到；B 那条 key 正常在 fallback 里，无交叉", async () => {
      const primary = new RaceBodyCache();
      let fallbackBuilds = 0;
      const cache = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryBodyCache();
      });
      const keyA: BodyCacheKey = { ...KEY, messageId: 1 };
      const keyB: BodyCacheKey = { ...KEY, messageId: 2 };

      // op A 发起——primary.put(keyA) 真挂起，闩此刻没跳（target 是 primary）。
      const opA = cache.put(keyA, [{ text: "A" }], new Uint8Array([1]));
      await Promise.resolve();
      await Promise.resolve();
      expect(primary.pendingCount).toBe(1);

      // op B 发起——武装 primary 让这次调用直接失败，触发 wrapper 跳闸（wrapper 捕获失败后自己
      // 会换到 fallback 重试一次成功——既有 `put()` 失败路径的行为，不是本测试要验证的新东西）。
      primary.failNextCall();
      await cache.put(keyB, [{ text: "B" }], new Uint8Array([2]));
      expect(fallbackBuilds).toBe(1); // 降级已经发生，fallback 已经建好。

      // 放行 op A 挂起的 primary 调用——这次调用是在闩跳之前发起的，物理上仍然落进了 primary
      // （`primary.putCalls` 会证明这一点），但 wrapper 落地时发现闩状态变了，这次结果丢弃不落账。
      primary.resolvePending();
      await opA;

      // 核心断言①：op A 确实物理写过 primary（不是没发生过），但丢弃不落账——不追加写 fallback,
      // A 这条 key 现在读不到（读走的是 fallback,没有这条）。
      expect(primary.putCalls.map((k) => k.messageId)).toEqual([1, 2]);
      await expect(cache.get(keyA)).resolves.toBeNull();

      // 核心断言②：B 那条 key 正常在 fallback 里——两条账本没有交叉污染。
      const gotB = await cache.get(keyB);
      expect(gotB?.blocks).toEqual([{ text: "B" }]);

      // 核心断言③：无交叉——只构造了一个 fallback 实例。
      expect(fallbackBuilds).toBe(1);
    });

    it("干净路径（无竞态）——op A 正常落地，闩全程没跳，不构造 fallback、数据正常写入 primary", async () => {
      const primary = new RaceBodyCache();
      let fallbackBuilds = 0;
      const cache = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryBodyCache();
      });
      const opA = cache.put(KEY, [{ text: "clean" }], new Uint8Array([7]));
      await Promise.resolve();
      primary.resolvePending();
      await opA;
      expect(primary.putCalls).toHaveLength(1);
      expect(fallbackBuilds).toBe(0);
      expect((await primary.get(KEY))?.blocks).toEqual([{ text: "clean" }]);
    });
  });

  describe("msgfix2 U4 修单二 I6：clear 竞态与吞错（bodyCache.ts:199-209 锚点）", () => {
    it("primary.clear() 抛错——错误上抛给调用方（旧版无论成败恒 resolve，上层可见性形同虚设）", async () => {
      const primary: BodyCachePort = {
        async get() {
          return null;
        },
        async put() {},
        async delete() {},
        async clear() {
          throw new Error("indexeddb clear transaction failed (test)");
        },
      };
      const cache = withMemoryFallback(primary, () => new InMemoryBodyCache());
      await expect(cache.clear()).rejects.toThrow("indexeddb clear transaction failed (test)");
    });

    it("off→clear 慢→on→写入 → 迟到的 clear 不会删掉 clear 发起之后才写入的新数据", async () => {
      class SlowClearBodyCache implements BodyCachePort {
        private readonly inner = new InMemoryBodyCache();
        clearCalls = 0;
        private clearResolvers: Array<() => void> = [];
        async get(key: BodyCacheKey): Promise<CachedBody | null> {
          return this.inner.get(key);
        }
        async put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void> {
          return this.inner.put(key, blocks, bytes);
        }
        async delete(key: BodyCacheKey): Promise<void> {
          return this.inner.delete(key);
        }
        async clear(): Promise<void> {
          this.clearCalls += 1;
          await new Promise<void>((resolve) => this.clearResolvers.push(resolve));
          return this.inner.clear();
        }
        resolveClear(): void {
          const resolver = this.clearResolvers.shift();
          if (!resolver) throw new Error("resolveClear() called with no pending clear() (test setup bug)");
          resolver();
        }
      }
      const primary = new SlowClearBodyCache();
      const cache = withMemoryFallback(primary, () => new InMemoryBodyCache());

      // off：触发 clear()——primary 侧真挂起（模拟一次耗时的 IndexedDB 清空事务）。
      const clearPromise = cache.clear();
      await Promise.resolve();
      expect(primary.clearCalls).toBe(1);

      // on→写入：这次 put() 必须排在旧 clear 真正完成之后才落盘——不能被迟到的 clear 顺手清掉。
      const putPromise = cache.put(KEY, [{ text: "fresh after on" }], new Uint8Array([9, 9]));

      // 放行慢 clear()，它完成之后 put() 才应该真正落盘。
      primary.resolveClear();
      await clearPromise;
      await putPromise;

      const got = await cache.get(KEY);
      expect(got?.blocks).toEqual([{ text: "fresh after on" }]); // 新写入没被迟到的 clear 删掉。
    });
  });
});
