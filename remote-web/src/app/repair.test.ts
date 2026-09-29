// repair.test.ts — INT1c 审查返工（P1）· attemptRepairClear() 覆盖：清除成功、清除抛错、清除
// "没报错但读回仍有数据"、读回本身抛错——四条路径全覆盖，尤其是"清除失败路径"（审查明确要求）。
//
// FIX2 P2-7（三库清除齐套）：新增 events/commands 两个 IndexedDB 库的删除断言——本文件只测控制流
// （注入假 `deleteIndexedDb`，不碰真实 IndexedDB），真实 IndexedDB 端到端删除验证见
// `RootRouter.test.tsx`。

import { describe, expect, it } from "vitest";
import type { KeyStorePort, StoredPairingCredentials } from "../store/key-store.ts";
import { deriveEventStoreDbName } from "../store/indexeddbEventStore.ts";
import { deriveCommandLedgerDbName } from "../store/commandLedger.indexeddb.ts";
import { deriveBodyCacheDbName } from "../store/bodyCache.indexeddb.ts";
import { attemptRepairClear } from "./repair.ts";

const ROOM = "0123456789abcdef0123456789abcdef";

function fakeCredentials(): StoredPairingCredentials {
  return {
    deviceId: "device-1",
    room: ROOM,
    relayUrl: "wss://relay.example",
    access: "a".repeat(64),
    refresh: "b".repeat(64),
    kRoomKey: {} as CryptoKey, // 本文件只测控制流，不做真加密——占位值足够。
  };
}

class FakeKeyStore implements KeyStorePort {
  private record: StoredPairingCredentials | null;
  clearCalls = 0;
  loadCalls = 0;
  clearShouldThrow: Error | null = null;
  loadShouldThrow: Error | null = null;
  /** 模拟"clear() 没抛错，但底层其实没真的清掉"——读回验证的价值就在于能抓住这种情形。 */
  clearIsNoop = false;

  constructor(initial: StoredPairingCredentials | null) {
    this.record = initial;
  }

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    this.record = creds;
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    this.loadCalls += 1;
    if (this.loadShouldThrow) throw this.loadShouldThrow;
    return this.record;
  }

  async clear(): Promise<void> {
    this.clearCalls += 1;
    if (this.clearShouldThrow) throw this.clearShouldThrow;
    if (!this.clearIsNoop) {
      this.record = null;
    }
  }
}

/** 假 `indexedDB.deleteDatabase`——记下每次调用的库名，按名字可控失败。 */
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

describe("attemptRepairClear()", () => {
  it("clear() 成功且读回确实为 null → ok:true", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: true });
    expect(keyStore.clearCalls).toBe(1);
    expect(keyStore.loadCalls).toBe(1); // 真的读回验证了，不是只调了 clear() 就信了。
  });

  it("已经是空的 key-store 上调用也是 ok:true（幂等）", async () => {
    const keyStore = new FakeKeyStore(null);
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: true });
  });

  it("清除失败路径①：clear() 直接抛错 → ok:false，附带可展示的错误信息，不假装已清", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    keyStore.clearShouldThrow = new Error("indexeddb blocked");
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: "indexeddb blocked" });
    // clear() 抛错之后不该去调 loadKeys()——没有"清过"这回事，读回验证没有意义。
    expect(keyStore.loadCalls).toBe(0);
    // 也不该再去删 events/commands 两库——key-store 这一步就已经不干净，不该继续往下走。
    expect(deleter.calls).toEqual([]);
  });

  it("清除失败路径②：clear() 没抛错，但读回验证发现凭据仍在 → ok:false(clear_did_not_take_effect)，不假装已清", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    keyStore.clearIsNoop = true;
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: "clear_did_not_take_effect" });
    expect(keyStore.clearCalls).toBe(1);
    expect(keyStore.loadCalls).toBe(1);
    expect(deleter.calls).toEqual([]);
  });

  it("清除失败路径③：clear() 成功，但读回验证这一步本身抛错 → ok:false，不能假装'没读到=清干净了'", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    keyStore.loadShouldThrow = new Error("indexeddb connection lost");
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: "indexeddb connection lost" });
  });

  it("非 Error 类型的异常也被安全转成字符串，不泄漏为 [object Object] 之外的诡异值", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    keyStore.clearShouldThrow = "not an Error instance" as unknown as Error;
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: "not an Error instance" });
  });
});

describe("attemptRepairClear() · FIX2 P2-7 + msgfix2 U4：三库清除齐套（events/commands/body-cache 三个 IndexedDB 库按房间派生名一并删除）", () => {
  it("key-store 清干净之后，按房间派生名依次删除 events 库、commands 库与 body-cache 库——三库都读回验证过（onsuccess 才算数）才算 ok:true", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: true });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(ROOM), deriveCommandLedgerDbName(ROOM), deriveBodyCacheDbName(ROOM)]);
  });

  it("events 库删除失败 → ok:false，附带错误信息；commands/body-cache 两库不再继续尝试（顺序执行、任一步失败即停）", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    deleter.failNames.add(deriveEventStoreDbName(ROOM));
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: `delete failed (test): ${deriveEventStoreDbName(ROOM)}` });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(ROOM)]); // commands/body-cache 两库从未被尝试。
  });

  it("commands 库删除失败 → ok:false（events 库已经先删成功了，body-cache 库也从未被尝试，但整体仍报失败，不假装『删了两库就够』）", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    deleter.failNames.add(deriveCommandLedgerDbName(ROOM));
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: `delete failed (test): ${deriveCommandLedgerDbName(ROOM)}` });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(ROOM), deriveCommandLedgerDbName(ROOM)]);
  });

  it("body-cache 库删除失败 → ok:false（events/commands 两库已经先删成功了，但整体仍报失败，不假装『三库删了两库就够』）", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    deleter.failNames.add(deriveBodyCacheDbName(ROOM));
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome).toEqual({ ok: false, error: `delete failed (test): ${deriveBodyCacheDbName(ROOM)}` });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(ROOM), deriveCommandLedgerDbName(ROOM), deriveBodyCacheDbName(ROOM)]);
  });

  it("不同房间派生出不同的库名——不会用错房间的库名去删", async () => {
    const otherRoom = "fedcba9876543210fedcba9876543210";
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    await attemptRepairClear({ keyStore, room: otherRoom, deleteIndexedDb: deleter.fn });
    expect(deleter.calls).toEqual([deriveEventStoreDbName(otherRoom), deriveCommandLedgerDbName(otherRoom), deriveBodyCacheDbName(otherRoom)]);
    expect(deleter.calls).not.toContain(deriveEventStoreDbName(ROOM));
  });

  it("传入活跃的 eventStore/commandLedger/bodyCache 实例——先各自 close() 再删库（close-then-delete，避免本标签页自己的连接卡住 onblocked）", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    let eventStoreClosed = false;
    let commandLedgerClosed = false;
    let bodyCacheClosed = false;
    const outcome = await attemptRepairClear({
      keyStore,
      room: ROOM,
      deleteIndexedDb: deleter.fn,
      eventStore: { close: () => (eventStoreClosed = true) },
      commandLedger: { close: () => (commandLedgerClosed = true) },
      bodyCache: { close: () => (bodyCacheClosed = true) },
    });
    expect(outcome).toEqual({ ok: true });
    expect(eventStoreClosed).toBe(true);
    expect(commandLedgerClosed).toBe(true);
    expect(bodyCacheClosed).toBe(true);
  });
});

describe("attemptRepairClear() · msgfix2 U4 修单 H3：内存 fallback 态（idbAvailable:false）下 repair 不再必死", () => {
  it("探测失败/内存态：即使 deleteIndexedDb 必然失败，repair 仍然 ok:true——key-store 清干净就够了，不去调一个必然失败的真实 deleteDatabase", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    deleter.failNames.add(deriveEventStoreDbName(ROOM));
    deleter.failNames.add(deriveCommandLedgerDbName(ROOM));
    deleter.failNames.add(deriveBodyCacheDbName(ROOM));
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn, idbAvailable: false });
    expect(outcome).toEqual({ ok: true });
    expect(deleter.calls).toEqual([]); // 真的跳过了，不是"调用了但错误被吞"。
    expect(keyStore.clearCalls).toBe(1); // key-store 那半仍然照常执行——只跳过三库真实删库这一步。
  });

  it("省略 idbAvailable——保持既有行为，deleteIndexedDb 失败仍然 ok:false", async () => {
    const keyStore = new FakeKeyStore(fakeCredentials());
    const deleter = new FakeIndexedDbDeleter();
    deleter.failNames.add(deriveEventStoreDbName(ROOM));
    const outcome = await attemptRepairClear({ keyStore, room: ROOM, deleteIndexedDb: deleter.fn });
    expect(outcome.ok).toBe(false);
  });
});
