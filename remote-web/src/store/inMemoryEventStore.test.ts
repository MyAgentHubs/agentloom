// inMemoryEventStore.test.ts — TDD 覆盖 src/store/inMemoryEventStore.ts（`InMemoryEventStore`，
// fallback adapter 的一环）。同 `indexeddbEventStore.test.ts` 的契约用例集（applyEventIfNew 幂等 +
// 水位 max() 语义 + listEvents 按 seq 升序），证明两个实现同构——`store/idbFactory.ts` 的探测失败
// 分支切到这个实现时,`app/AppRuntime.tsx` 的冷启动重放/去重逻辑不需要区分走的是哪一路。

import { describe, expect, it } from "vitest";
import { InMemoryEventStore, withMemoryFallback } from "./inMemoryEventStore.ts";
import type { ApplyEventInput, ApplyEventResult, EventStorePort, StoredEvent } from "./port.ts";

const dummyFrame = (note: string): unknown => ({ t: "presence", role: "remote", event: note });

describe("InMemoryEventStore · basic dedup + watermark", () => {
  it("getWatermark() is 0 before anything has been applied", async () => {
    const store = new InMemoryEventStore();
    expect(await store.getWatermark()).toBe(0);
  });

  it("hasAppliedClientMsgId() is false before applying, true after", async () => {
    const store = new InMemoryEventStore();
    expect(await store.hasAppliedClientMsgId("cmid-1")).toBe(false);
    await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 1, session: null, frame: dummyFrame("a") });
    expect(await store.hasAppliedClientMsgId("cmid-1")).toBe(true);
  });

  it("first application of a client_msg_id returns applied:true and advances the watermark", async () => {
    const store = new InMemoryEventStore();
    const result = await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 7, session: null, frame: dummyFrame("a") });
    expect(result).toEqual({ applied: true, watermark: 7 });
    expect(await store.getWatermark()).toBe(7);
  });

  it("re-applying the exact same client_msg_id returns applied:false and does not change the watermark (at-least-once redelivery)", async () => {
    const store = new InMemoryEventStore();
    const first = await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 7, session: null, frame: dummyFrame("a") });
    expect(first).toEqual({ applied: true, watermark: 7 });

    const second = await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 7, session: null, frame: dummyFrame("a-redelivered") });
    expect(second).toEqual({ applied: false, watermark: 7 });
    expect(await store.getWatermark()).toBe(7);
  });

  it("watermark advances monotonically via max(), never regressing on an out-of-order lower seq", async () => {
    const store = new InMemoryEventStore();
    await store.applyEventIfNew({ clientMsgId: "cmid-high", seq: 5, session: null, frame: dummyFrame("high") });
    expect(await store.getWatermark()).toBe(5);

    const result = await store.applyEventIfNew({ clientMsgId: "cmid-low", seq: 3, session: null, frame: dummyFrame("low") });
    expect(result).toEqual({ applied: true, watermark: 5 });
    expect(await store.getWatermark()).toBe(5);
  });
});

describe("InMemoryEventStore · event body + listEvents", () => {
  it("applyEventIfNew() persists the frame body, retrievable via listEvents()", async () => {
    const store = new InMemoryEventStore();
    const frame = dummyFrame("persisted");
    await store.applyEventIfNew({ clientMsgId: "cmid-body", seq: 3, session: null, frame });
    expect(await store.listEvents()).toEqual([{ clientMsgId: "cmid-body", seq: 3, session: null, frame }]);
  });

  it("listEvents() returns entries ordered by seq ascending, regardless of application order", async () => {
    const store = new InMemoryEventStore();
    await store.applyEventIfNew({ clientMsgId: "third", seq: 30, session: null, frame: dummyFrame("third") });
    await store.applyEventIfNew({ clientMsgId: "first", seq: 10, session: null, frame: dummyFrame("first") });
    await store.applyEventIfNew({ clientMsgId: "second", seq: 20, session: null, frame: dummyFrame("second") });

    const events = await store.listEvents();
    expect(events.map((e) => e.seq)).toEqual([10, 20, 30]);
    expect(events.map((e) => e.clientMsgId)).toEqual(["first", "second", "third"]);
  });

  it("re-delivery of the same client_msg_id does not duplicate the event log entry", async () => {
    const store = new InMemoryEventStore();
    const originalFrame = dummyFrame("original");
    await store.applyEventIfNew({ clientMsgId: "cmid-dup", seq: 5, session: null, frame: originalFrame });
    await store.applyEventIfNew({ clientMsgId: "cmid-dup", seq: 5, session: null, frame: dummyFrame("should-be-ignored") });

    const events = await store.listEvents();
    expect(events).toHaveLength(1);
    expect(events[0]).toEqual({ clientMsgId: "cmid-dup", seq: 5, session: null, frame: originalFrame });
  });

  it("session 字段随事件同一存原样保留（三态语义——null/字符串两种真实取值，本实现不产生 undefined 存量态）", async () => {
    const store = new InMemoryEventStore();
    await store.applyEventIfNew({ clientMsgId: "a", seq: 1, session: null, frame: dummyFrame("index") });
    await store.applyEventIfNew({ clientMsgId: "b", seq: 2, session: "sess-1", frame: dummyFrame("msg") });
    const events = await store.listEvents();
    expect(events.find((e) => e.clientMsgId === "a")?.session).toBeNull();
    expect(events.find((e) => e.clientMsgId === "b")?.session).toBe("sess-1");
  });

  it("close() 是 no-op（无持久连接可言），不抛错", () => {
    const store = new InMemoryEventStore();
    expect(() => store.close()).not.toThrow();
  });

  it("msgfix2 U4 修单二 I4：clear() 真的清空——applied/watermark 都归零，不是 close() 那种 no-op", async () => {
    const store = new InMemoryEventStore();
    await store.applyEventIfNew({ clientMsgId: "cmid-1", seq: 7, session: null, frame: dummyFrame("a") });
    expect(await store.getWatermark()).toBe(7);
    await store.clear();
    expect(await store.getWatermark()).toBe(0);
    expect(await store.hasAppliedClientMsgId("cmid-1")).toBe(false);
    expect(await store.listEvents()).toEqual([]);
  });
});

/** 模拟"探测成功之后某次真实事务失败"（配额耗尽/连接损坏）——同 `bodyCache.test.ts::
 *  FlakyBodyCache` 的既有手法。 */
class FlakyEventStore implements EventStorePort {
  applyCalls = 0;
  shouldFail = true;
  async applyEventIfNew(): Promise<ApplyEventResult> {
    this.applyCalls += 1;
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return { applied: true, watermark: 1 };
  }
  async getWatermark(): Promise<number> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return 0;
  }
  async hasAppliedClientMsgId(): Promise<boolean> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return false;
  }
  async listEvents(): Promise<StoredEvent[]> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return [];
  }
}

describe("withMemoryFallback() · msgfix2 U4 修单 H1（运行期事务失败降级内存）", () => {
  const INPUT: ApplyEventInput = { clientMsgId: "cmid-1", seq: 1, session: null, frame: { t: "x" } };

  it("primary.applyEventIfNew() 抛错——切内存实现重试一次，这次真的应用成功（不 crash、不丢这条事件）", async () => {
    const primary = new FlakyEventStore();
    let fallbackBuilds = 0;
    const store = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryEventStore();
    });

    const result = await store.applyEventIfNew(INPUT);
    expect(result).toEqual({ applied: true, watermark: 1 });
    expect(primary.applyCalls).toBe(1); // 试过 primary 一次，失败了。
    expect(fallbackBuilds).toBe(1); // 换到内存实现，只建一次。

    // 后续调用直接走内存实现——「连接不断」：不是每次都重新报错/重新抛异常。
    expect(await store.hasAppliedClientMsgId("cmid-1")).toBe(true);
    const second = await store.applyEventIfNew({ ...INPUT, clientMsgId: "cmid-2", seq: 2 });
    expect(second).toEqual({ applied: true, watermark: 2 });
    expect(primary.applyCalls).toBe(1); // 不再尝试 primary。
  });

  it("primary.getWatermark()/listEvents() 抛错——降级为安全默认值（0/[]），不让异常冒泡", async () => {
    const primary = new FlakyEventStore();
    const store = withMemoryFallback(primary, () => new InMemoryEventStore());
    await expect(store.getWatermark()).resolves.toBe(0);
    await expect(store.listEvents()).resolves.toEqual([]);
  });

  it("primary 正常工作时不会构造 fallback（不需要就不建）", async () => {
    const primary = new FlakyEventStore();
    primary.shouldFail = false;
    let fallbackBuilds = 0;
    const store = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryEventStore();
    });
    await store.applyEventIfNew(INPUT);
    await store.getWatermark();
    expect(fallbackBuilds).toBe(0);
  });

  it("close() 透传给 primary 与已构造的 fallback（若有）", async () => {
    let primaryClosed = false;
    let fallbackClosed = false;
    const primary: EventStorePort = {
      async applyEventIfNew() {
        throw new Error("fail");
      },
      async getWatermark() {
        return 0;
      },
      async hasAppliedClientMsgId() {
        return false;
      },
      async listEvents() {
        return [];
      },
      close: () => (primaryClosed = true),
    };
    const store = withMemoryFallback(primary, () => ({
      async applyEventIfNew() {
        return { applied: true, watermark: 1 };
      },
      async getWatermark() {
        return 0;
      },
      async hasAppliedClientMsgId() {
        return false;
      },
      async listEvents() {
        return [];
      },
      close: () => (fallbackClosed = true),
    }));
    await store.applyEventIfNew(INPUT); // 触发降级，构造 fallback。
    store.close?.();
    expect(primaryClosed).toBe(true);
    expect(fallbackClosed).toBe(true);
  });

  /**
   * msgfix2 U4 修单二 I1 竞态专用 primary——`applyEventIfNew()` 的行为按调用次序受控：第一次调用
   * 真挂起（测试代码手动 `resolvePending()` 放行），下一次调用可以武装成立即失败一次
   * （`failNextCall()`），模拟"op A 在飞中，op B（同一个 wrapper 上的另一次并发应用）先一步触发
   * 降级"这个时序。
   */
  class RaceEventStore implements EventStorePort {
    private readonly inner = new InMemoryEventStore();
    applyCalls: string[] = [];
    private pendingResolvers: Array<() => void> = [];
    private nextCallShouldFail = false;

    async applyEventIfNew(input: ApplyEventInput): Promise<ApplyEventResult> {
      this.applyCalls.push(input.clientMsgId);
      if (this.nextCallShouldFail) {
        this.nextCallShouldFail = false;
        throw new Error("race primary applyEventIfNew failed (test)");
      }
      await new Promise<void>((resolve) => this.pendingResolvers.push(resolve));
      return this.inner.applyEventIfNew(input);
    }
    async getWatermark(): Promise<number> {
      return this.inner.getWatermark();
    }
    async hasAppliedClientMsgId(clientMsgId: string): Promise<boolean> {
      return this.inner.hasAppliedClientMsgId(clientMsgId);
    }
    async listEvents(): Promise<StoredEvent[]> {
      return this.inner.listEvents();
    }
    failNextCall(): void {
      this.nextCallShouldFail = true;
    }
    resolvePending(): void {
      const resolver = this.pendingResolvers.shift();
      if (!resolver) throw new Error("resolvePending() called with no pending applyEventIfNew() to resolve (test setup bug)");
      resolver();
    }
    get pendingCount(): number {
      return this.pendingResolvers.length;
    }
  }

  describe("msgfix2 U4 修单二 I1：fallback 单向闩防双账本——飞行中的 primary 结果不因竞态被误落账", () => {
    it("op A（applyEventIfNew）在飞中，op B（同一个 wrapper 上的另一次并发应用）先失败触发降级——A 落地时感知到闩已经跳了，换到（此刻已是当前）fallback 重新应用一次（事件本体不能真的丢），不是『一半在 primary 一半在 fallback』的分裂账本；此后 hasAppliedClientMsgId()/listEvents() 全部一致地走 fallback", async () => {
      const primary = new RaceEventStore();
      let fallbackBuilds = 0;
      const store = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryEventStore();
      });

      // op A 发起——primary.applyEventIfNew("evt-a") 真挂起，闩此刻没跳（target 是 primary）。
      const opA = store.applyEventIfNew({ clientMsgId: "evt-a", seq: 1, session: null, frame: dummyFrame("a") });
      await Promise.resolve();
      await Promise.resolve();
      expect(primary.pendingCount).toBe(1);

      // op B 发起——武装 primary 让这次调用直接失败，触发 wrapper 跳闸（wrapper 捕获失败后自己会
      // 换到 fallback 重试一次成功——既有 `applyEventIfNew()` 失败路径的行为）。
      primary.failNextCall();
      const resultB = await store.applyEventIfNew({ clientMsgId: "evt-b", seq: 2, session: null, frame: dummyFrame("b") });
      expect(resultB).toEqual({ applied: true, watermark: 2 });
      expect(fallbackBuilds).toBe(1);

      // 放行 op A 挂起的 primary 调用——物理上仍然落进了 primary（`primary.applyCalls` 证明这一
      // 点），但 wrapper 落地时发现闩状态变了，换到 fallback 重新应用一次（事件本体不能真的丢）。
      primary.resolvePending();
      await opA;

      expect(primary.applyCalls).toEqual(["evt-a", "evt-b"]);
      // 核心断言：A、B 两条事件都能在 fallback 里查到——A 没有因为竞态被真的丢掉，也没有产生
      // "一部分账本查得到、一部分查不到"的分裂状态。
      await expect(store.hasAppliedClientMsgId("evt-a")).resolves.toBe(true);
      await expect(store.hasAppliedClientMsgId("evt-b")).resolves.toBe(true);
      expect(fallbackBuilds).toBe(1); // 无交叉——只构造了一个 fallback 实例。
    });

    it("干净路径（无竞态）——op A 正常落地，闩全程没跳，不构造 fallback、不发生任何『重新应用一次』", async () => {
      const primary = new RaceEventStore();
      let fallbackBuilds = 0;
      const store = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryEventStore();
      });
      const opA = store.applyEventIfNew({ clientMsgId: "evt-clean", seq: 1, session: null, frame: dummyFrame("clean") });
      await Promise.resolve();
      primary.resolvePending();
      await opA;
      expect(primary.applyCalls).toHaveLength(1);
      expect(fallbackBuilds).toBe(0);
      await expect(primary.hasAppliedClientMsgId("evt-clean")).resolves.toBe(true);
    });
  });
});
