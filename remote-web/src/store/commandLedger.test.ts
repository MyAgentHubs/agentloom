// commandLedger.test.ts — TDD 覆盖 src/store/commandLedger.ts（InMemoryCommandLedger）+
// src/store/commandLedger.indexeddb.ts（IndexedDbCommandLedger，用 `fake-indexeddb`）。
//
// G3 缓解的核心不变量：①「先持久化再发送」——recordSent() 必须在 send() 之前完成（这条在
// `commandChannel.test.ts` 里断言，本文件只测账本本身的 CRUD 契约）；②`isOwn()` 是唯一的过滤
// 判据——本机没记过账的 command_id 一律 false，不管它长得多像一个合法 UUID。

import { describe, expect, it } from "vitest";
import { indexedDB as fakeIndexedDB } from "fake-indexeddb";
import { InMemoryCommandLedger, withMemoryFallback, type CommandLedgerPort } from "./commandLedger.ts";
import { deriveCommandLedgerDbName, IndexedDbCommandLedger } from "./commandLedger.indexeddb.ts";

const ROOM = "0123456789abcdef0123456789abcdef";

describe("deriveCommandLedgerDbName()", () => {
  it("按房间派生不同库名——换房不串库（同 indexeddbEventStore.ts::deriveEventStoreDbName 的既有约定）", () => {
    expect(deriveCommandLedgerDbName(ROOM)).not.toBe(deriveCommandLedgerDbName("f".repeat(32)));
    expect(deriveCommandLedgerDbName(ROOM)).toContain(ROOM);
  });
});

function ledgerContractTests(makeLedger: () => {
  isOwn(id: string): Promise<boolean>;
  recordSent(input: { commandId: string; kind: string; session: string; createdAt: number; decisionId?: string; option?: string }): Promise<void>;
  updateStatus(id: string, status: "ok" | "queued" | "failed" | "sent" | "taken_over" | "expired" | "rate_limited"): Promise<void>;
  get(id: string): Promise<{ commandId: string; kind: string; session: string; createdAt: number; status: string; decisionId?: string; option?: string } | null>;
}) {
  it("未记账的 command_id：isOwn()=false，get()=null", async () => {
    const ledger = makeLedger();
    expect(await ledger.isOwn("cmd-unknown")).toBe(false);
    expect(await ledger.get("cmd-unknown")).toBeNull();
  });

  it("recordSent() 之后 isOwn()=true，初始 status='sent'", async () => {
    const ledger = makeLedger();
    await ledger.recordSent({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1000 });
    expect(await ledger.isOwn("cmd-1")).toBe(true);
    const record = await ledger.get("cmd-1");
    expect(record).toEqual({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1000, status: "sent" });
  });

  it("updateStatus() 更新已记账行的状态，不影响 commandId/kind/session/createdAt", async () => {
    const ledger = makeLedger();
    await ledger.recordSent({ commandId: "cmd-1", kind: "input.answer", session: "sess-1", createdAt: 1000 });
    await ledger.updateStatus("cmd-1", "failed");
    const record = await ledger.get("cmd-1");
    expect(record?.status).toBe("failed");
    expect(record?.kind).toBe("input.answer");
    expect(record?.session).toBe("sess-1");
    expect(record?.createdAt).toBe(1000);
  });

  it("updateStatus() 对未记账的 command_id 静默不做任何事（不抛异常，不产生幽灵行）", async () => {
    const ledger = makeLedger();
    await expect(ledger.updateStatus("never-recorded", "ok")).resolves.toBeUndefined();
    expect(await ledger.get("never-recorded")).toBeNull();
  });

  it("两条不同 command_id 互不干扰", async () => {
    const ledger = makeLedger();
    await ledger.recordSent({ commandId: "cmd-a", kind: "input.send", session: "sess-1", createdAt: 1 });
    await ledger.recordSent({ commandId: "cmd-b", kind: "control.stop", session: "sess-2", createdAt: 2 });
    await ledger.updateStatus("cmd-a", "ok");
    expect((await ledger.get("cmd-a"))?.status).toBe("ok");
    expect((await ledger.get("cmd-b"))?.status).toBe("sent");
  });

  it("返工②第①点·updateStatus() 接受 'rate_limited'（G8 限速消费的可重试终态）", async () => {
    const ledger = makeLedger();
    await ledger.recordSent({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1 });
    await ledger.updateStatus("cmd-1", "rate_limited");
    expect((await ledger.get("cmd-1"))?.status).toBe("rate_limited");
  });

  it("返工②第①点·input.answer 记录持久保存 decisionId 与 option（重试原选择的唯一真相源——不是靠内存态）", async () => {
    const ledger = makeLedger();
    await ledger.recordSent({
      commandId: "cmd-answer-1",
      kind: "input.answer",
      session: "sess-1",
      createdAt: 1,
      decisionId: "dec-1",
      option: "停止", // 故意选第 2 项（不是 options[0]）——核实精确保存的是这个值，不是随便一个字符串。
    });
    const record = await ledger.get("cmd-answer-1");
    expect(record?.decisionId).toBe("dec-1");
    expect(record?.option).toBe("停止");
  });

  it("input.send/control.stop 记录不带 decisionId/option（可选字段，非 input.answer 命令不应凭空长出这两个字段）", async () => {
    const ledger = makeLedger();
    await ledger.recordSent({ commandId: "cmd-send-1", kind: "input.send", session: "sess-1", createdAt: 1 });
    const record = await ledger.get("cmd-send-1");
    expect(record?.decisionId).toBeUndefined();
    expect(record?.option).toBeUndefined();
  });
}

describe("InMemoryCommandLedger", () => {
  ledgerContractTests(() => new InMemoryCommandLedger());
});

describe("IndexedDbCommandLedger (fake-indexeddb)", () => {
  ledgerContractTests(() => new IndexedDbCommandLedger(`test-cmd-ledger-${crypto.randomUUID()}`, fakeIndexedDB));

  it("记录跨重启存活——新的 IndexedDbCommandLedger 实例接同一个库名/工厂仍能查到（模拟 app 重启，见 key-store.test.ts 同名先例）", async () => {
    const dbName = `test-cmd-ledger-${crypto.randomUUID()}`;
    const first = new IndexedDbCommandLedger(dbName, fakeIndexedDB);
    await first.recordSent({ commandId: "cmd-restart", kind: "input.send", session: "sess-1", createdAt: 42 });

    const second = new IndexedDbCommandLedger(dbName, fakeIndexedDB);
    expect(await second.isOwn("cmd-restart")).toBe(true);
    const record = await second.get("cmd-restart");
    expect(record?.createdAt).toBe(42);
  });

  it("不同库名（不同房间）互不串账——房间 A 记的 command_id 在房间 B 的账本里查不到", async () => {
    const ledgerA = new IndexedDbCommandLedger(`test-room-a-${crypto.randomUUID()}`, fakeIndexedDB);
    const ledgerB = new IndexedDbCommandLedger(`test-room-b-${crypto.randomUUID()}`, fakeIndexedDB);
    await ledgerA.recordSent({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1 });
    expect(await ledgerA.isOwn("cmd-1")).toBe(true);
    expect(await ledgerB.isOwn("cmd-1")).toBe(false);
  });

  it("msgfix2 F2 S3：versionchange 触发后（另一个上下文对同一个库发起 deleteDatabase()）——下一次操作重新 openDb() 成功，不是攥着已 close 的 stale 连接恒抛 InvalidStateError", async () => {
    const dbName = `test-cmd-ledger-versionchange-${crypto.randomUUID()}`;
    const ledger = new IndexedDbCommandLedger(dbName, fakeIndexedDB);
    await ledger.recordSent({ commandId: "before-versionchange", kind: "input.send", session: "sess-1", createdAt: 1 });

    // 同 `indexeddbEventStore.test.ts` 同名用例——模拟"另一个标签页"对同一个库发起
    // deleteDatabase()，本店的 `onversionchange` 处理器 close() 已建立的连接，deleteDatabase()
    // 才不会卡在 onblocked。旧版只清 `dbHandle` 不清 `dbPromise`，下一次 `openDb()` 会直接复用
    // 那个已经 close 的 stale 连接，对已关闭连接开事务恒抛 InvalidStateError。
    await new Promise<void>((resolve, reject) => {
      const req = fakeIndexedDB.deleteDatabase(dbName);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error ?? new Error("deleteDatabase failed"));
      req.onblocked = () => reject(new Error("deleteDatabase blocked — onversionchange did not release the connection"));
    });

    // 核心断言：下一次操作重新 openDb() 成功——库被删过，重新写入正常生效、查得到。
    await ledger.recordSent({ commandId: "after-versionchange", kind: "input.send", session: "sess-1", createdAt: 2 });
    expect(await ledger.isOwn("after-versionchange")).toBe(true);
  });
});

/** 模拟"探测成功之后某次真实事务失败"（配额耗尽/连接损坏）——同 `bodyCache.test.ts::
 *  FlakyBodyCache` 的既有手法。 */
class FlakyCommandLedger implements CommandLedgerPort {
  recordSentCalls = 0;
  shouldFail = true;
  async recordSent(): Promise<void> {
    this.recordSentCalls += 1;
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
  }
  async isOwn(): Promise<boolean> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return false;
  }
  async updateStatus(): Promise<void> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
  }
  async get() {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return null;
  }
}

describe("withMemoryFallback() · msgfix2 U4 修单 H1（运行期事务失败降级内存）", () => {
  it("primary.recordSent() 抛错——切内存实现重试一次，这次真的记账成功（不 crash、指令不会被误判 give_up）", async () => {
    const primary = new FlakyCommandLedger();
    let fallbackBuilds = 0;
    const ledger = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryCommandLedger();
    });

    await expect(ledger.recordSent({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1 })).resolves.toBeUndefined();
    expect(primary.recordSentCalls).toBe(1);
    expect(fallbackBuilds).toBe(1);

    // 后续调用直接走内存实现——「连接不断」：isOwn() 能查到刚记的这条账。
    expect(await ledger.isOwn("cmd-1")).toBe(true);
    await ledger.recordSent({ commandId: "cmd-2", kind: "input.send", session: "sess-1", createdAt: 2 });
    expect(primary.recordSentCalls).toBe(1); // 不再尝试 primary。
  });

  it("primary.isOwn()/get() 抛错——降级为安全默认值（false/null），不让异常冒泡", async () => {
    const primary = new FlakyCommandLedger();
    const ledger = withMemoryFallback(primary, () => new InMemoryCommandLedger());
    await expect(ledger.isOwn("cmd-x")).resolves.toBe(false);
    await expect(ledger.get("cmd-x")).resolves.toBeNull();
  });

  it("primary 正常工作时不会构造 fallback（不需要就不建）", async () => {
    const primary = new FlakyCommandLedger();
    primary.shouldFail = false;
    let fallbackBuilds = 0;
    const ledger = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryCommandLedger();
    });
    await ledger.recordSent({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1 });
    await ledger.isOwn("cmd-1");
    expect(fallbackBuilds).toBe(0);
  });

  it("close() 透传给 primary 与已构造的 fallback（若有）", async () => {
    let primaryClosed = false;
    let fallbackClosed = false;
    const primary: CommandLedgerPort = {
      async recordSent() {
        throw new Error("fail");
      },
      async isOwn() {
        return false;
      },
      async updateStatus() {},
      async get() {
        return null;
      },
      close: () => (primaryClosed = true),
    };
    const ledger = withMemoryFallback(primary, () => ({
      async recordSent() {},
      async isOwn() {
        return true;
      },
      async updateStatus() {},
      async get() {
        return null;
      },
      close: () => (fallbackClosed = true),
    }));
    await ledger.recordSent({ commandId: "cmd-1", kind: "input.send", session: "sess-1", createdAt: 1 }); // 触发降级。
    ledger.close?.();
    expect(primaryClosed).toBe(true);
    expect(fallbackClosed).toBe(true);
  });

  /**
   * msgfix2 U4 修单二 I1 竞态专用 primary——`recordSent()` 的行为按调用次序受控：第一次调用真挂起
   * （测试代码手动 `resolvePending()` 放行），下一次调用可以武装成立即失败一次（`failNextCall()`），
   * 模拟"op A 在飞中，op B（同一个 wrapper 上的另一次并发记账）先一步触发降级"这个时序。
   */
  class RaceCommandLedger implements CommandLedgerPort {
    private readonly inner = new InMemoryCommandLedger();
    recordCalls: string[] = [];
    private pendingResolvers: Array<() => void> = [];
    private nextCallShouldFail = false;

    async recordSent(input: Parameters<CommandLedgerPort["recordSent"]>[0]): Promise<void> {
      this.recordCalls.push(input.commandId);
      if (this.nextCallShouldFail) {
        this.nextCallShouldFail = false;
        throw new Error("race primary recordSent failed (test)");
      }
      await new Promise<void>((resolve) => this.pendingResolvers.push(resolve));
      await this.inner.recordSent(input);
    }
    async isOwn(commandId: string): Promise<boolean> {
      return this.inner.isOwn(commandId);
    }
    async updateStatus(commandId: string, status: Parameters<CommandLedgerPort["updateStatus"]>[1]): Promise<void> {
      return this.inner.updateStatus(commandId, status);
    }
    async get(commandId: string) {
      return this.inner.get(commandId);
    }
    failNextCall(): void {
      this.nextCallShouldFail = true;
    }
    resolvePending(): void {
      const resolver = this.pendingResolvers.shift();
      if (!resolver) throw new Error("resolvePending() called with no pending recordSent() to resolve (test setup bug)");
      resolver();
    }
    get pendingCount(): number {
      return this.pendingResolvers.length;
    }
  }

  describe("msgfix2 U4 修单二 I1：fallback 单向闩防双账本——飞行中的 primary 结果不因竞态被误落账", () => {
    it("op A（recordSent）在飞中，op B（同一个 wrapper 上的另一次并发 recordSent）先失败触发降级——A 落地时感知到闩已经跳了，换到（此刻已是当前）fallback 补记一次（G3『先持久化再发送』不能因为竞态就丢账），不是『一半在 primary 一半在 fallback』的分裂账本；此后 isOwn()/get() 全部一致地走 fallback", async () => {
      const primary = new RaceCommandLedger();
      let fallbackBuilds = 0;
      const ledger = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryCommandLedger();
      });

      // op A 发起——primary.recordSent("cmd-a") 真挂起，闩此刻没跳（target 是 primary）。
      const opA = ledger.recordSent({ commandId: "cmd-a", kind: "input.send", session: "sess-1", createdAt: 1 });
      await Promise.resolve();
      await Promise.resolve();
      expect(primary.pendingCount).toBe(1);

      // op B 发起——武装 primary 让这次调用直接失败，触发 wrapper 跳闸（wrapper 捕获失败后自己会
      // 换到 fallback 重试一次成功——既有 `recordSent()` 失败路径的行为，不是本测试要验证的新东西）。
      primary.failNextCall();
      await expect(
        ledger.recordSent({ commandId: "cmd-b", kind: "input.send", session: "sess-1", createdAt: 2 }),
      ).resolves.toBeUndefined();
      expect(fallbackBuilds).toBe(1);

      // 放行 op A 挂起的 primary 调用——物理上仍然落进了 primary（`primary.recordCalls` 证明这一
      // 点），但 wrapper 落地时发现闩状态变了，换到 fallback 补记一次（不能真的丢账——G3 硬语义）。
      primary.resolvePending();
      await opA;

      expect(primary.recordCalls).toEqual(["cmd-a", "cmd-b"]);
      // 核心断言：A、B 两条命令都能在 fallback 里查到——A 没有因为竞态被真的丢掉，也没有产生
      // "一部分账本查得到、一部分查不到"的分裂状态。
      await expect(ledger.isOwn("cmd-a")).resolves.toBe(true);
      await expect(ledger.isOwn("cmd-b")).resolves.toBe(true);
      expect(fallbackBuilds).toBe(1); // 无交叉——只构造了一个 fallback 实例。
    });

    it("干净路径（无竞态）——op A 正常落地，闩全程没跳，不构造 fallback、不发生任何『重新落一次』", async () => {
      const primary = new RaceCommandLedger();
      let fallbackBuilds = 0;
      const ledger = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryCommandLedger();
      });
      const opA = ledger.recordSent({ commandId: "cmd-clean", kind: "input.send", session: "sess-1", createdAt: 1 });
      await Promise.resolve();
      primary.resolvePending();
      await opA;
      expect(primary.recordCalls).toHaveLength(1);
      expect(fallbackBuilds).toBe(0);
      await expect(primary.isOwn("cmd-clean")).resolves.toBe(true);
    });
  });
});
