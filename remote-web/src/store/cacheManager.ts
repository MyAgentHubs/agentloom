// cacheManager.ts — msgfix2 U4 · 统一 cache manager（设计稿 §4.2）：房间级三库（events/commands/
// body-cache）close-then-delete，四触发点共用底层实现——
//   ① repair（`app/repair.ts::attemptRepairClear` 内部改调 `purgeRoomData()` 处理这三库那半，
//      key-store 的 clear + 读回验证仍留在 `repair.ts` 自己那）；
//   ② 显式解除配对（同 repair_failed 页面"重试"走的是同一条 `attemptRepairClear` 路径——needs_repair
//      从用户视角就是"被强制解除配对，请重新扫码"，不是另一条独立代码路径）；
//   ③ re-pair/room 切换（`app/RootRouter.tsx::handleActivated` 检测到新激活的凭据换了房间时，
//      直接调 `purgeRoomData()` 清旧房间三库——不动 key-store，新凭据已经落盘）；
//   ④ device_revoked 后 refresh 走死（C1 判据）——`connection/connectionSession.ts::
//      transitionToNeedsRepair` 判定的 needs_repair 终态，回调路径与①同一个 `onNeedsRepair`。
//
// **先 close 全部再 deleteDatabase，不是顺序偏好**：浏览器规范下，只要还有连接开着，
// `deleteDatabase()` 请求会悬挂在 `onblocked` 直到那些连接全部关闭——本标签页自己持有的活跃连接
// （`eventStore`/`commandLedger`/`bodyCache` 三个实例，若调用方当前正持有）必须先主动 `close()`
// 放手；跨标签页的连接靠各自实现里注册的 `onversionchange → close()`（`indexeddbEventStore.ts`/
// `commandLedger.indexeddb.ts`/`bodyCache.indexeddb.ts` 三处收拢 P1-3）在收到这次 `deleteDatabase()`
// 触发的 versionchange 事件后自动放手，不需要这里协调。

import { deriveEventStoreDbName } from "./indexeddbEventStore.ts";
import { deriveCommandLedgerDbName } from "./commandLedger.indexeddb.ts";
import { deriveBodyCacheDbName } from "./bodyCache.indexeddb.ts";

export interface CloseableStore {
  close?(): void;
  /**
   * msgfix2 U4 修单二 I4：内存 fallback 态（`idbAvailable:false`）下"删库"的真正语义——`close()`
   * 对内存实现本就是 no-op，之前只调 `close()` 就直接判定 `purgeRoomData()` 成功，三个内存 store
   * 实例本体压根没被清空，`Root` 侧持有的引用继续读得到旧数据（`InMemoryEventStore`/
   * `InMemoryCommandLedger`/`InMemoryBodyCache` 现在都实现了这个方法）。可选：真实 IndexedDB
   * 实现不需要提供——那半走 `deleteIndexedDb()` 真删库,不经过这个方法。
   */
  clear?(): Promise<void>;
}

export interface PurgeRoomDataTargets {
  room: string;
  /** 调用方当前持有的活跃实例（若有）——传入是为了先 `close()` 它们；不传也不是错误（比如
   *  刚重新加载页面还没来得及为旧房间建立任何连接的场景），只是少了"抢在删库前主动放手"这一步的
   *  加速，最终仍会删成功（靠 `onblocked` 之外的路径——没有连接开着，`deleteDatabase()` 直接成功）。 */
  eventStore?: CloseableStore;
  commandLedger?: CloseableStore;
  bodyCache?: CloseableStore;
  /** 注入点（测试用假实现；生产默认走真实 `globalThis.indexedDB.deleteDatabase`）——同
   *  `app/repair.ts` 既有的 `deleteIndexedDb` 注入惯例。 */
  deleteIndexedDb?: (name: string) => Promise<void>;
  /**
   * msgfix2 U4 修单 H3：探测阶段已判定 IndexedDB 整体不可用（`store/idbFactory.ts::
   * createStoreFactory()` 的 `idbAvailable:false` 分支）——传进来的三个 store 本就是内存实现
   * （`InMemoryEventStore`/`InMemoryCommandLedger`/`InMemoryBodyCache`），没有真实 IndexedDB 库
   * 可删；跳过 `deleteIndexedDb()`，`close()`（no-op）就是内存态"清库"的全部语义。不跳过的话，
   * 默认的 `defaultDeleteIndexedDb()` 会去调用真实 `globalThis.indexedDB.deleteDatabase()`——那
   * 正是引发探测失败的同一个不可用面（隐私模式/配额耗尽到打不开），调用它只会再抛一次同样的
   * 错误，把 repair/purge 拖进 `repair_failed` 死循环（本单修单 H3：一次真实"无 IDB"环境下 repair
   * 必死）。默认 `true`（未显式声明时假定走真实 IndexedDB，保持既有调用方行为不变）。
   */
  idbAvailable?: boolean;
}

export type PurgeRoomDataOutcome = { ok: true } | { ok: false; error: string };

/**
 * 按 events → commands → bodyCache 的固定顺序删除，任一步失败即停（不是"能删多少删多少"再汇总——
 * 调用方按同一套"整体成功/失败"的 UI 简单收口，同 `app/repair.ts::attemptRepairClear` 的既有取向）。
 */
export async function purgeRoomData(targets: PurgeRoomDataTargets): Promise<PurgeRoomDataOutcome> {
  targets.eventStore?.close?.();
  targets.commandLedger?.close?.();
  targets.bodyCache?.close?.();

  if (targets.idbAvailable === false) {
    // msgfix2 U4 修单二 I4：内存态没有真实 IndexedDB 库可删——close() 不等于"清空"，真正的清空要
    // 靠各内存实现自己的 `clear()`（Map.clear() / 归零水位）。close() 仍然先调过了（幂等、无副
    // 作用地放手连接），clear() 才是这条分支下"删库"真正对应的动作；任一步失败即报失败（同下面
    // 真实删库分支"任一步失败即停"的取向一致，不吞错）。
    try {
      await targets.eventStore?.clear?.();
      await targets.commandLedger?.clear?.();
      await targets.bodyCache?.clear?.();
      return { ok: true };
    } catch (error) {
      return { ok: false, error: describeError(error) };
    }
  }

  const deleteIndexedDb = targets.deleteIndexedDb ?? defaultDeleteIndexedDb;

  try {
    await deleteIndexedDb(deriveEventStoreDbName(targets.room));
    await deleteIndexedDb(deriveCommandLedgerDbName(targets.room));
    await deleteIndexedDb(deriveBodyCacheDbName(targets.room));
    return { ok: true };
  } catch (error) {
    return { ok: false, error: describeError(error) };
  }
}

/**
 * 真实 IndexedDB 删库——`onsuccess` 才算数（浏览器保证它只在库真的被删掉之后才触发）。`onblocked`
 * （先 close 那一步理论上已经堵死这条路，纵深防御：万一还有本函数不知道的连接开着）按错误处理，
 * 不无限期悬挂。
 */
function defaultDeleteIndexedDb(name: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const idb = globalThis.indexedDB;
    if (!idb) {
      reject(new Error("IndexedDB is unavailable in this runtime"));
      return;
    }
    const request = idb.deleteDatabase(name);
    request.onsuccess = () => resolve();
    request.onerror = () => reject(request.error ?? new Error(`failed to delete IndexedDB database "${name}"`));
    request.onblocked = () => reject(new Error(`delete IndexedDB database "${name}" is blocked by an open connection`));
  });
}

function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
