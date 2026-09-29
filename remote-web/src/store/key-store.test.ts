// key-store.test.ts — TDD 覆盖 src/store/key-store.ts（InMemoryKeyStore）+
// src/store/key-store.indexeddb.ts（IndexedDbKeyStore，用 `fake-indexeddb` 模拟浏览器 IndexedDB）。
//
// ============================================================================
// 覆盖表 / KeyStore 测到哪层（任务书 §4 ⑤ 要求的"报告里写明测到哪层，不许假绿"——这里就是那份
// 证据）
// ============================================================================
// 本单在写测试前先用一次性脚本核实过（见 worker 报告 ⑤）：Node 26 的 `structuredClone` 原生支持
// CryptoKey（W3C Web Crypto API 序列化步骤），`fake-indexeddb` 的结构化克隆走的正是这条路径——
// 所以下面这批测试**不是"只测接口契约、跳过 CryptoKey"的降级版**，而是完整验证了 M2 C1 spec
// §3.7 的生产语义：non-extractable AES-256-GCM CryptoKey 经 IndexedDB 落储 + 重新打开一个新
// `IndexedDbKeyStore` 实例（模拟 app 重启）+ 取出后仍然是同一把可用的 CryptoKey（`extractable ===
// false` 且真的能拿去 encrypt/decrypt，不是死对象）。**真机遗留**：这只证明了 Node + fake-indexeddb
// 这一种宿主环境的行为；真实 Safari/Chrome/Firefox 各自的 IndexedDB CryptoKey 结构化克隆实现是否
// 逐一吻合（尤其 Safari 18.4 基线），仍归 T6g2 真机矩阵验证，不在本单断言范围内。

import { describe, expect, it } from "vitest";
import { indexedDB as fakeIndexedDB } from "fake-indexeddb";
import { importNonExtractableAesGcmKey, InMemoryKeyStore, withMemoryFallback, type KeyStorePort, type StoredPairingCredentials } from "./key-store.ts";
import { IndexedDbKeyStore } from "./key-store.indexeddb.ts";

async function makeCredentials(overrides: Partial<StoredPairingCredentials> = {}): Promise<StoredPairingCredentials> {
  const kRoomBytes = new Uint8Array(32).fill(9);
  const kRoomKey = await importNonExtractableAesGcmKey(kRoomBytes);
  return {
    deviceId: "11111111-1111-4111-8111-111111111111",
    room: "0123456789abcdef0123456789abcdef",
    relayUrl: "wss://relay.example",
    access: "a".repeat(64),
    refresh: "b".repeat(64),
    kRoomKey,
    ...overrides,
  };
}

describe("importNonExtractableAesGcmKey()", () => {
  it("imports a usable, non-extractable AES-256-GCM key", async () => {
    const key = await importNonExtractableAesGcmKey(new Uint8Array(32).fill(1));
    expect(key.extractable).toBe(false);
    expect(key.algorithm).toMatchObject({ name: "AES-GCM", length: 256 });
    const nonce = new Uint8Array(12);
    const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, key, new TextEncoder().encode("probe"));
    const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce }, key, ct);
    expect(new TextDecoder().decode(pt)).toBe("probe");
  });

  it("rejects a key that is not exactly 32 bytes", async () => {
    await expect(importNonExtractableAesGcmKey(new Uint8Array(16))).rejects.toThrow();
  });
});

describe("InMemoryKeyStore", () => {
  it("saveKeys → loadKeys round trip", async () => {
    const store = new InMemoryKeyStore();
    expect(await store.loadKeys()).toBeNull();
    const creds = await makeCredentials();
    await store.saveKeys(creds);
    expect(await store.loadKeys()).toBe(creds);
  });

  it("clear() removes the stored record", async () => {
    const store = new InMemoryKeyStore();
    await store.saveKeys(await makeCredentials());
    await store.clear();
    expect(await store.loadKeys()).toBeNull();
  });

  it("saveKeys overwrites the previous record (single-device MVP)", async () => {
    const store = new InMemoryKeyStore();
    await store.saveKeys(await makeCredentials({ access: "first" }));
    await store.saveKeys(await makeCredentials({ access: "second" }));
    expect((await store.loadKeys())?.access).toBe("second");
  });
});

describe("IndexedDbKeyStore (fake-indexeddb)", () => {
  it("loadKeys returns null before anything is saved", async () => {
    const store = new IndexedDbKeyStore(fakeIndexedDB);
    expect(await store.loadKeys()).toBeNull();
  });

  it("saveKeys → loadKeys round trip, including a still-usable non-extractable K_room CryptoKey", async () => {
    const dbName = `test-${crypto.randomUUID()}`;
    const store = new IndexedDbKeyStore(fakeIndexedDB, dbName);
    const kRoomBytes = new Uint8Array(32).fill(42);
    const kRoomKey = await importNonExtractableAesGcmKey(kRoomBytes);
    const creds = await makeCredentials({ kRoomKey });
    await store.saveKeys(creds);

    const loaded = await store.loadKeys();
    expect(loaded).not.toBeNull();
    expect(loaded!.deviceId).toBe(creds.deviceId);
    expect(loaded!.room).toBe(creds.room);
    expect(loaded!.relayUrl).toBe(creds.relayUrl);
    expect(loaded!.access).toBe(creds.access);
    expect(loaded!.refresh).toBe(creds.refresh);

    expect(loaded!.kRoomKey.extractable).toBe(false);
    expect(loaded!.kRoomKey).not.toBe(kRoomKey); // 真的经过了一次结构化克隆，不是原对象引用
    const nonce = new Uint8Array(12);
    const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, loaded!.kRoomKey, new TextEncoder().encode("probe"));
    // 用独立导入的、来自同一份原始字节的密钥解密——证明落储/取出的 CryptoKey 与原始 K_room 字节一致。
    const referenceKey = await crypto.subtle.importKey("raw", kRoomBytes, "AES-GCM", false, ["decrypt"]);
    const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce }, referenceKey, ct);
    expect(new TextDecoder().decode(pt)).toBe("probe");
  });

  it("credentials survive a fresh IndexedDbKeyStore instance against the same fake IndexedDB factory (simulates app restart)", async () => {
    const dbName = `test-${crypto.randomUUID()}`;
    const first = new IndexedDbKeyStore(fakeIndexedDB, dbName);
    const creds = await makeCredentials({ access: "restart-probe" });
    await first.saveKeys(creds);

    const second = new IndexedDbKeyStore(fakeIndexedDB, dbName);
    const loaded = await second.loadKeys();
    expect(loaded?.access).toBe("restart-probe");
  });

  it("clear() removes the stored record", async () => {
    const dbName = `test-${crypto.randomUUID()}`;
    const store = new IndexedDbKeyStore(fakeIndexedDB, dbName);
    await store.saveKeys(await makeCredentials());
    await store.clear();
    expect(await store.loadKeys()).toBeNull();
  });

  it("saveKeys uses a single transaction (M0 §9.5: access/refresh must be persisted atomically together)", async () => {
    const dbName = `test-${crypto.randomUUID()}`;
    const store = new IndexedDbKeyStore(fakeIndexedDB, dbName);
    await store.saveKeys(await makeCredentials({ access: "atomic-access", refresh: "atomic-refresh" }));
    const loaded = await store.loadKeys();
    // 两个字段必须同时可见——不存在只写了一半的中间态可观察窗口（单条 IDBObjectStore.put 调用，
    // 事务边界即函数边界）。
    expect(loaded?.access).toBe("atomic-access");
    expect(loaded?.refresh).toBe("atomic-refresh");
  });
});

/** 模拟"探测成功之后某次真实事务失败"（配额耗尽/连接损坏）——同 `bodyCache.test.ts::
 *  FlakyBodyCache` 的既有手法。 */
class FlakyKeyStore implements KeyStorePort {
  saveCalls = 0;
  shouldFail = true;
  async saveKeys(): Promise<void> {
    this.saveCalls += 1;
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
  }
  async loadKeys(): Promise<StoredPairingCredentials | null> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return null;
  }
  async clear(): Promise<void> {}
}

/** 只在 `clear()` 上受控失败——供 I2 红测试专用，不影响 saveKeys/loadKeys。 */
class ClearFailsKeyStore implements KeyStorePort {
  private readonly inner = new InMemoryKeyStore();
  clearCalls = 0;
  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    return this.inner.saveKeys(creds);
  }
  async loadKeys(): Promise<StoredPairingCredentials | null> {
    return this.inner.loadKeys();
  }
  async clear(): Promise<void> {
    this.clearCalls += 1;
    throw new Error("indexeddb clear transaction failed (test)");
  }
}

/**
 * msgfix2 U4 修单二 I1 竞态专用 primary——`saveKeys()` 的行为按调用次序受控：
 * 第一次调用真挂起（测试代码手动 `resolvePending()` 放行），下一次调用可以武装成立即失败一次
 * （`failNextCall()`），模拟"op A 在飞中，op B（同一个 wrapper 上的另一次并发写）先一步触发降级"
 * 这个时序——同 `AppRuntime.bodyCache.e2e.test.tsx::DeferredGetBodyCache` 的既有"真挂起、手动
 * 放行"手法，只是这里额外支持"下一次调用直接失败"。
 */
class RaceKeyStore implements KeyStorePort {
  private readonly inner = new InMemoryKeyStore();
  calls: StoredPairingCredentials[] = [];
  private pendingResolvers: Array<() => void> = [];
  private nextCallShouldFail = false;

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    this.calls.push(creds);
    if (this.nextCallShouldFail) {
      this.nextCallShouldFail = false;
      throw new Error("race primary saveKeys failed (test)");
    }
    await new Promise<void>((resolve) => this.pendingResolvers.push(resolve));
    await this.inner.saveKeys(creds);
  }

  /** 下一次（还没发起的）`saveKeys()` 调用直接失败——用于制造"op B 触发降级"。 */
  failNextCall(): void {
    this.nextCallShouldFail = true;
  }

  /** 放行最早一次仍挂起的 `saveKeys()`。 */
  resolvePending(): void {
    const resolver = this.pendingResolvers.shift();
    if (!resolver) throw new Error("resolvePending() called with no pending saveKeys() to resolve (test setup bug)");
    resolver();
  }

  get pendingCount(): number {
    return this.pendingResolvers.length;
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    return this.inner.loadKeys();
  }
  async clear(): Promise<void> {
    return this.inner.clear();
  }
}

/**
 * msgfix2 U4 修单三 J1 专用——`loadKeys()` 真挂起（测试代码手动 `resolvePendingRead()` 放行），
 * `saveKeys()` 正常写内部存储、可按需武装成下一次立即失败（`failNextSave()`）——同 `RaceKeyStore`
 * 的既有"真挂起、手动放行"手法，只是这里挂起的是读而不是写，用来制造"读在飞中，写先一步触发降级"
 * 这个时序。
 */
class RaceReadKeyStore implements KeyStorePort {
  private readonly inner = new InMemoryKeyStore();
  private pendingReads: Array<() => void> = [];
  private nextSaveShouldFail = false;

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    if (this.nextSaveShouldFail) {
      this.nextSaveShouldFail = false;
      throw new Error("race primary saveKeys failed (test)");
    }
    return this.inner.saveKeys(creds);
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    const snapshot = await this.inner.loadKeys();
    return new Promise<StoredPairingCredentials | null>((resolve) => {
      this.pendingReads.push(() => resolve(snapshot));
    });
  }

  /** 放行最早一次仍挂起的 `loadKeys()`——用它落地时"当时"内部存储的快照（不是放行那一刻的）。 */
  resolvePendingRead(): void {
    const resolver = this.pendingReads.shift();
    if (!resolver) throw new Error("resolvePendingRead() called with no pending loadKeys() to resolve (test setup bug)");
    resolver();
  }

  /** 下一次（还没发起的）`saveKeys()` 调用直接失败——用于制造"读在飞中，写先一步触发降级"。 */
  failNextSave(): void {
    this.nextSaveShouldFail = true;
  }

  async clear(): Promise<void> {
    return this.inner.clear();
  }
}

/**
 * msgfix2 U4 修单三 J1 BACKLOG 局限测试专用——一个"物理上真的持久化"的 primary（内部真存数据，
 * 不像 `FlakyKeyStore` 那样 `loadKeys()` 永远返回 `null`），`shouldFail` 可以随时翻转，模拟
 * "同一块物理存储，跨会话/跨重启在不同时刻可用性不同"。
 */
class TogglableFailureKeyStore implements KeyStorePort {
  private readonly inner = new InMemoryKeyStore();
  shouldFail = false;

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return this.inner.saveKeys(creds);
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    if (this.shouldFail) throw new Error("indexeddb transaction failed (test)");
    return this.inner.loadKeys();
  }

  async clear(): Promise<void> {
    if (this.shouldFail) throw new Error("indexeddb clear transaction failed (test)");
    return this.inner.clear();
  }
}

describe("withMemoryFallback() · msgfix2 U4 修单 H1（运行期事务失败降级内存）", () => {
  it("primary.saveKeys() 抛错——切内存实现重试一次，这次真的存住（不 crash，不丢刚配对成功的凭据）", async () => {
    const primary = new FlakyKeyStore();
    let fallbackBuilds = 0;
    const store = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryKeyStore();
    });
    const creds = await makeCredentials();

    await expect(store.saveKeys(creds)).resolves.toBeUndefined();
    expect(primary.saveCalls).toBe(1);
    expect(fallbackBuilds).toBe(1);

    // 后续调用直接走内存实现——「连接不断」：loadKeys() 能读回刚存的凭据。
    expect((await store.loadKeys())?.deviceId).toBe(creds.deviceId);
  });

  it("primary 正常工作时不会构造 fallback（不需要就不建）", async () => {
    const primary = new FlakyKeyStore();
    primary.shouldFail = false;
    let fallbackBuilds = 0;
    const store = withMemoryFallback(primary, () => {
      fallbackBuilds += 1;
      return new InMemoryKeyStore();
    });
    await store.saveKeys(await makeCredentials());
    await store.loadKeys();
    expect(fallbackBuilds).toBe(0);
  });

  describe("msgfix2 U4 修单二 I2：读取/清除失败必须如实上抛（不适用『静默降级继续跑』）", () => {
    it("primary.loadKeys() 抛错——上抛给调用方，不再静默返回 null 掩盖 primary 里其实还在的旧凭据", async () => {
      const primary = new FlakyKeyStore();
      const store = withMemoryFallback(primary, () => new InMemoryKeyStore());
      await expect(store.loadKeys()).rejects.toThrow("indexeddb transaction failed (test)");
    });

    it("primary.clear() 抛错——repair 收到失败（不误报成功），不能让旧凭据被误判『已经清空』", async () => {
      const primary = new ClearFailsKeyStore();
      const store = withMemoryFallback(primary, () => new InMemoryKeyStore());
      await expect(store.clear()).rejects.toThrow("indexeddb clear transaction failed (test)");
      expect(primary.clearCalls).toBe(1);
    });

    it("降级只影响写入新凭据场景——primary.saveKeys() 已经失败切到 fallback 之后，loadKeys() 正常读 fallback，不因为『降级过』就额外抛错", async () => {
      const primary = new FlakyKeyStore();
      const store = withMemoryFallback(primary, () => new InMemoryKeyStore());
      const creds = await makeCredentials();
      await store.saveKeys(creds); // primary 失败，切到 fallback 重试成功。
      await expect(store.loadKeys()).resolves.toMatchObject({ deviceId: creds.deviceId });
    });
  });

  describe("msgfix2 U4 修单二 I1：fallback 单向闩防双账本——飞行中的 primary 结果不因竞态被误落账", () => {
    it("op A（saveKeys）在飞中，op B（同一个 wrapper 上的另一次并发 saveKeys）先失败触发降级——A 落地时感知到闩已经跳了，换到（此刻已是当前）fallback 重新落一次，不是『一半在 primary 一半在 fallback』的分裂账本；此后读写全部一致地走 fallback", async () => {
      const primary = new RaceKeyStore();
      let fallbackBuilds = 0;
      const store = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryKeyStore();
      });
      const credsA = await makeCredentials({ deviceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" });
      const credsB = await makeCredentials({ deviceId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb" });

      // op A 发起——primary.saveKeys(credsA) 真挂起，闩此刻没跳（target 是 primary）。
      const opA = store.saveKeys(credsA);
      await Promise.resolve();
      await Promise.resolve();
      expect(primary.pendingCount).toBe(1);

      // op B 发起——武装 primary 让这次调用直接失败,触发 wrapper 跳闸（wrapper 捕获失败后自己会
      // 换到 fallback 重试一次成功,这是既有 `saveKeys()` 失败路径的行为,不是本测试要验证的新东西）。
      primary.failNextCall();
      await expect(store.saveKeys(credsB)).resolves.toBeUndefined();
      expect(fallbackBuilds).toBe(1); // 降级已经发生，fallback 已经建好。

      // 放行 op A 挂起的 primary 调用——这次调用是在闩跳之前发起的,物理上仍然落进了 primary
      // （`primary.calls` 会证明这一点），但 wrapper 落地时发现闩状态变了，不能把这次结果当数。
      primary.resolvePending();
      await opA;

      // 核心断言①：op A 确实物理写过 primary（不是没发生过），但这次结果被丢弃不落账——重新
      // 换到 fallback 补落了一次，fallback 侧最终看到的是 A 的凭据（A 在 B 之后完成落地，A 覆盖
      // 了 B 先写进 fallback 的那条记录，单设备 MVP 覆盖写语义不变）。
      expect(primary.calls.map((c) => c.deviceId)).toEqual(["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"]);
      await expect(store.loadKeys()).resolves.toMatchObject({ deviceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" });

      // 核心断言②：无交叉——只构造了一个 fallback 实例（没有因为 A 的补落而再建一个）,后续所有
      // 读写都稳定地落在同一个 fallback 上。
      expect(fallbackBuilds).toBe(1);
    });

    it("干净路径（无竞态）——op A 正常落地，闩全程没跳，不构造 fallback、不发生任何『重新落一次』", async () => {
      const primary = new RaceKeyStore();
      let fallbackBuilds = 0;
      const store = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryKeyStore();
      });
      const creds = await makeCredentials();
      const opA = store.saveKeys(creds);
      await Promise.resolve();
      primary.resolvePending();
      await opA;
      expect(primary.calls).toHaveLength(1); // 只落地了一次，没有因为误判竞态而重复写。
      expect(fallbackBuilds).toBe(0);
    });
  });

  describe("msgfix2 U4 修单三 J1：resilientLatch 飞行期语义收口", () => {
    it("loadKeys() 在飞中——闩在读取途中被另一次并发写触发降级，primary 这次读到的（跳闸前的）快照必须被丢弃，改从（此刻已是当前）fallback 重读一次（读己之写·key-store.ts:181 那个洞）", async () => {
      const primary = new RaceReadKeyStore();
      const oldCreds = await makeCredentials({ deviceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" });
      await primary.saveKeys(oldCreds); // 直接摸 primary 内部垫一条"旧数据"（不经过 wrapper）。

      let fallbackBuilds = 0;
      const store = withMemoryFallback(primary, () => {
        fallbackBuilds += 1;
        return new InMemoryKeyStore();
      });

      // op A：loadKeys() 发起——真挂起，闩此刻没跳（target 是 primary）。
      const readPromise = store.loadKeys();
      await Promise.resolve();
      await Promise.resolve();

      // op B：并发写失败，触发降级；wrapper 换到（此刻已建好的）fallback 重试一次成功，fallback
      // 现在持有"新数据"。
      primary.failNextSave();
      const newCreds = await makeCredentials({ deviceId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb" });
      await store.saveKeys(newCreds);
      expect(fallbackBuilds).toBe(1);

      // 放行 op A 挂起的 primary 读取——这次读到的是 primary 上跳闸前的旧快照,但闩已经在飞行
      // 途中跳了。
      primary.resolvePendingRead();
      const readResult = await readPromise;

      // 核心断言：op A 拿到的不是过期的 primary 快照，而是（重新读的）fallback 上的最新数据。
      expect(readResult?.deviceId).toBe("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
    });

    it("干净路径（无竞态）——loadKeys() 全程闩没跳，不重读、原样返回 primary 那次读到的结果", async () => {
      const primary = new RaceReadKeyStore();
      const creds = await makeCredentials();
      await primary.saveKeys(creds);

      const store = withMemoryFallback(primary, () => new InMemoryKeyStore());
      const readPromise = store.loadKeys();
      await Promise.resolve();
      primary.resolvePendingRead();
      await expect(readPromise).resolves.toMatchObject({ deviceId: creds.deviceId });
    });

    it("BACKLOG 局限：降级后新写只进 fallback，『重启』（全新闩、同一个物理 primary）后一旦 primary 重新可用，读到的是降级前那份旧快照——本刀不做恢复期重同步（见 key-store.ts `withMemoryFallback()` 头注），此处锁住的是这条局限本身，不是锁『不会复活』", async () => {
      const physicalPrimary = new TogglableFailureKeyStore();
      const oldCreds = await makeCredentials({ deviceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" });
      await physicalPrimary.saveKeys(oldCreds); // 降级前，primary 上已经落了这份"旧"凭据。

      // 会话 1：primary 这次运行期不可用（配额耗尽/连接损坏）——闩跳到 fallback，新配对凭据只
      // 落进 fallback。
      physicalPrimary.shouldFail = true;
      const session1 = withMemoryFallback(physicalPrimary, () => new InMemoryKeyStore());
      const newCreds = await makeCredentials({ deviceId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb" });
      await session1.saveKeys(newCreds);
      await expect(session1.loadKeys()).resolves.toMatchObject({ deviceId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb" });

      // "重启"：新一次 probe（`store/idbFactory.ts`,本单不改）判定 primary 这次可用了——新会话
      // 是一个全新的 wrapper 实例，闩状态从零开始（未跳），物理上仍是同一个 primary（同一块磁盘/
      // 同一个 IndexedDB 库），但闩记忆已经随进程一起没了。
      physicalPrimary.shouldFail = false;
      const session2 = withMemoryFallback(physicalPrimary, () => new InMemoryKeyStore());

      // 核心断言（锁住 BACKLOG 声明的局限，不是锁"不会复活"）：新会话读到的是降级前的旧快照，
      // 会话 1 里只落进 fallback 的新凭据不可见——重启期间没有任何重同步机制去补写/覆盖 primary。
      await expect(session2.loadKeys()).resolves.toMatchObject({ deviceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" });
    });
  });
});
