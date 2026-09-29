// idbFactory.ts — msgfix2 U4 · root-level factory（设计稿 §4.2「统一 cache manager」段首
// "root-level factory（main.tsx 层）统一构造并持有全部 IDB 对象"）。
//
// 现状病灶（本单要收拢的）：`EventStore` 在 `main.tsx` 按房间构造、`CommandLedger` 在
// `app/AppRuntime.tsx` 内部按房间默认构造、`KeyStore` 在 `main.tsx` 直接 `new
// IndexedDbKeyStore()`——三处各管各的，没有一处在"要不要真的碰 IndexedDB"之前做过探测,任何一个
// 探测失败（隐私模式/iOS 限制/配额耗尽到打不开）在旧代码路径下都是**局部**抛错，不是"整套切内存"。
//
// 本文件是唯一的探测点 + 唯一的装配点：`createStoreFactory()` 先探测一次（`probeIndexedDb()`——
// `indexedDB.open` 试开一次即关闭，不留悬挂连接），成功则返回构造真实 IndexedDB 实现的工厂集合，
// 失败则返回构造纯内存实现（`InMemoryKeyStore`/`InMemoryEventStore`/`InMemoryCommandLedger`/
// `InMemoryBodyCache`）的工厂集合——四者统一降级，调用方（`main.tsx`）拿到的是同一套接口形状，
// 不需要关心走的是哪一路。

import { IndexedDbKeyStore } from "./key-store.indexeddb.ts";
import { InMemoryKeyStore, withMemoryFallback as withKeyStoreMemoryFallback, type KeyStorePort } from "./key-store.ts";
import { deriveEventStoreDbName, IndexedDbEventStore } from "./indexeddbEventStore.ts";
import { InMemoryEventStore, withMemoryFallback as withEventStoreMemoryFallback } from "./inMemoryEventStore.ts";
import type { EventStorePort } from "./port.ts";
import { deriveCommandLedgerDbName, IndexedDbCommandLedger } from "./commandLedger.indexeddb.ts";
import { InMemoryCommandLedger, withMemoryFallback as withCommandLedgerMemoryFallback, type CommandLedgerPort } from "./commandLedger.ts";
import { deriveBodyCacheDbName, IndexedDbBodyCache } from "./bodyCache.indexeddb.ts";
import { InMemoryBodyCache, withMemoryFallback, type BodyCachePort } from "./bodyCache.ts";

export interface StoreFactory {
  /** 单例——不按房间派生（同 `main.tsx` 既有取向："`keyStore` 同时喂配对阶段与已配对运行时…
   *  `IndexedDbKeyStore` 本身无状态，复用无害"）。 */
  keyStore: KeyStorePort;
  createEventStore: (room: string) => EventStorePort;
  createCommandLedger: (room: string) => CommandLedgerPort;
  createBodyCache: (room: string) => BodyCachePort;
  /** 供诊断/测试观测——探测是否成功（`true` = 真 IndexedDB 生效；`false` = 已知整套走内存，
   *  UI 层目前不消费这个字段，留作后续"本设备不支持持久缓存"提示的钩子）。 */
  idbAvailable: boolean;
}

const PROBE_DB_NAME = "agentloom-idb-probe";

/**
 * 启动探测——`indexedDB.open` 试开一次即关闭（不留悬挂连接），成功 = IndexedDB 可用。任何
 * 抛错/`onerror`/`onblocked`/`globalThis.indexedDB` 缺失都判"不可用"，不让探测本身的异常冒泡出去
 * 打断启动——fallback adapter 的核心存在意义就是"探测失败不能变成第二个更大的失败"。
 */
export async function probeIndexedDb(idbFactory?: IDBFactory): Promise<boolean> {
  const factory = idbFactory ?? (globalThis as { indexedDB?: IDBFactory }).indexedDB;
  if (!factory) return false;
  try {
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = factory.open(PROBE_DB_NAME, 1);
      request.onupgradeneeded = () => {
        if (!request.result.objectStoreNames.contains("probe")) {
          request.result.createObjectStore("probe");
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error ?? new Error("probe open failed"));
      request.onblocked = () => reject(new Error("probe open blocked"));
    });
    db.close();
    // msgfix2 F2 S5②：探测库只是"能不能开得开"这个信号的副产品，没有任何长期存在的意义——旧版
    // 探测完只 close() 不删，这个库会永久留在磁盘上；它不按房间派生（跟 events-<room>/
    // commands-<room>/agentloom-body-cache-<room> 三个不一样），天生不会被任何一次房间级
    // `purgeRoomData()` 扫到，成了一个不进任何清理集的孤儿。探测完就地删掉——删除失败不影响这次
    // 探测已经拿到的"可用"结论（吞掉，不让这一步失败把 true 错误地变成 false）。
    try {
      await new Promise<void>((resolve, reject) => {
        const deleteRequest = factory.deleteDatabase(PROBE_DB_NAME);
        deleteRequest.onsuccess = () => resolve();
        deleteRequest.onerror = () => reject(deleteRequest.error ?? new Error("probe delete failed"));
        deleteRequest.onblocked = () => reject(new Error("probe delete blocked"));
      });
    } catch {
      // 见上方注释——删不掉不影响探测结论。
    }
    return true;
  } catch {
    return false;
  }
}

export interface CreateStoreFactoryOptions {
  /** 测试注入——生产不传，走 `globalThis.indexedDB`。 */
  idbFactory?: IDBFactory;
  /** 测试注入——跳过真实探测，直接指定结果（省得每个装配测试都要真的模拟一个会失败的
   *  `indexedDB.open`）。生产不传，总是走真实 `probeIndexedDb()`。 */
  forceIdbAvailable?: boolean;
}

/**
 * 唯一装配点——`main.tsx` 在渲染 `RootRouter` 之前 `await` 这个函数一次。探测在**任何** IDB 对象
 * 构造之前执行：失败分支完全不触碰任何 `Indexed*` 类，直接返回内存实现的工厂集合。
 */
export async function createStoreFactory(options: CreateStoreFactoryOptions = {}): Promise<StoreFactory> {
  const idbAvailable = options.forceIdbAvailable ?? (await probeIndexedDb(options.idbFactory));

  if (!idbAvailable) {
    return {
      keyStore: new InMemoryKeyStore(),
      createEventStore: () => new InMemoryEventStore(),
      createCommandLedger: () => new InMemoryCommandLedger(),
      createBodyCache: () => new InMemoryBodyCache(),
      idbAvailable: false,
    };
  }

  // 运行期事务失败降级内存（设计稿 §4.2「单点包装」）——探测成功之后某次真实事务仍可能失败
  // （配额耗尽/连接损坏）。修单前只有 `createBodyCache` 包了这层（`withMemoryFallback()`），
  // `keyStore`/`createEventStore`/`createCommandLedger` 三个直接返回裸 IndexedDB 实现——一次运行期
  // 事务失败会直接抛给调用方（`RootRouter.tsx` 卡路由 / `AppRuntime.tsx` 丢帧 / `commandChannel.ts`
  // 丢指令，见 msgfix2 U4 修单 H1）。现在四者统一包同一条降级思路（各自文件里的
  // `withMemoryFallback()`——同 body cache 那份的既有设计，只是端口形状不同各自实现，见
  // `store/key-store.ts`/`store/inMemoryEventStore.ts`/`store/commandLedger.ts` 对应函数头注），
  // 失败后同一个引用内部换到内存实现，调用方拿到的端口引用不变。
  return {
    keyStore: withKeyStoreMemoryFallback(new IndexedDbKeyStore(options.idbFactory)),
    createEventStore: (room) => withEventStoreMemoryFallback(new IndexedDbEventStore(deriveEventStoreDbName(room))),
    createCommandLedger: (room) =>
      withCommandLedgerMemoryFallback(new IndexedDbCommandLedger(deriveCommandLedgerDbName(room), options.idbFactory)),
    createBodyCache: (room) => withMemoryFallback(new IndexedDbBodyCache(deriveBodyCacheDbName(room), options.idbFactory)),
    idbAvailable: true,
  };
}
