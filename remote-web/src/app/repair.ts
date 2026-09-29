// repair.ts — INT1c 审查返工（P1）· Root 侧"needs_repair 清核"纯逻辑。
//
// `connection/connectionSession.ts::transitionToNeedsRepair` 已经在认证性终态时自己清了一次
// key-store（`connection/` 目录本单不碰，也不需要——那条清除本身没问题）。这里要解决的是另一件
// 事：**Root 不能只是信任"它清过了"就直接切 UI 态**——本函数独立地再清一次、再读回验证，只有确认
// "真的空了"才向调用方报告 `ok: true`；清除调用本身没抛错但读回仍看到旧凭据（例如底层实现有
// 写入未真正提交这类边界），或清除调用直接抛错，都不能被静默吞掉伪装成"已清"。
//
// FIX2 P2-7（三库清除齐套·隐私）：只清 key-store 不够——`store/indexeddbEventStore.ts`/
// `store/commandLedger.indexeddb.ts` 按房间派生的两个 IndexedDB 库（`events-<room>`/
// `commands-<room>`）装着这个房间完整的历史会话内容与指令记录。重新配对/换设备/设备被撤销后，
// 旧房间这两库的内容仍然原样留在磁盘上——凭据虽然清了，内容没清，是隐私缺口。
//
// msgfix2 U4（收拢 P1-3）：三库清除下沉到 `store/cacheManager.ts::purgeRoomData()`（events/
// commands/body-cache 统一 close-then-delete，四触发点共用，见该文件头注）——本文件只保留
// key-store 自己的 clear + 读回验证（这半跟"是不是 IndexedDB"无关，独立成立），key-store 清干净
// 之后把三库那半整体委托出去。调用方（`app/RootRouter.tsx`）现在可以传入当前持有的
// `eventStore`/`commandLedger`/`bodyCache` 实例（供 `purgeRoomData()` 先 `close()` 再删库，避免
// 本标签页自己的活跃连接卡住 `deleteDatabase()` 的 `onblocked`）——三者都是可选的，省略时
// `purgeRoomData()` 仍能正常工作（没有连接可先关，直接删库）。

import type { KeyStorePort } from "../store/key-store.ts";
import { purgeRoomData, type CloseableStore } from "../store/cacheManager.ts";

export type RepairClearOutcome = { ok: true } | { ok: false; error: string };

export interface RepairTargets {
  keyStore: KeyStorePort;
  /** 当前房间——用于派生要一并删除的 events/commands/body-cache 三个 IndexedDB 库名。 */
  room: string;
  /** 调用方（`RootRouter.tsx`）当前持有的三个活跃实例（若有）——传入以便 `purgeRoomData()` 先
   *  `close()` 它们再删库。 */
  eventStore?: CloseableStore;
  commandLedger?: CloseableStore;
  bodyCache?: CloseableStore;
  /**
   * 注入点（测试用假实现；生产默认走真实 `globalThis.indexedDB.deleteDatabase`）——同本仓
   * `store/*.indexeddb.ts` 一贯"只在生产装配点摸全局 indexedDB"的手法，纯逻辑测试不依赖真实
   * IndexedDB。透传给 `purgeRoomData()`。
   */
  deleteIndexedDb?: (name: string) => Promise<void>;
  /** msgfix2 U4 修单 H3：透传给 `purgeRoomData()`——内存 fallback 态下跳过真实
   *  `deleteIndexedDb()`，见该函数 `idbAvailable` 字段头注。默认 `true`（保持既有行为）。 */
  idbAvailable?: boolean;
}

/**
 * 清除 key-store 里的配对凭据，读回确认真的清空了；再委托 `purgeRoomData()` 清当前房间的
 * events/commands/body-cache 三个 IndexedDB 库。任一步不满足都返回 `ok:false`，携带一条可展示的
 * 错误描述（调用方据此渲染"可重试的 repair 态"，不是直接切回 unpaired 假装成功）。顺序执行、任一
 * 步失败即停——key-store 先行：这一步没过，三库那半根本不该继续尝试。
 */
export async function attemptRepairClear(targets: RepairTargets): Promise<RepairClearOutcome> {
  const { keyStore, room } = targets;

  try {
    await keyStore.clear();
  } catch (error) {
    return { ok: false, error: describeError(error) };
  }
  let remaining: Awaited<ReturnType<KeyStorePort["loadKeys"]>>;
  try {
    remaining = await keyStore.loadKeys();
  } catch (error) {
    // 清除调用本身没报错，但读回验证这一步失败——同样不能假装"清干净了"，安全默认是"不确定=没清"。
    return { ok: false, error: describeError(error) };
  }
  if (remaining !== null) {
    return { ok: false, error: "clear_did_not_take_effect" };
  }

  return purgeRoomData({
    room,
    eventStore: targets.eventStore,
    commandLedger: targets.commandLedger,
    bodyCache: targets.bodyCache,
    deleteIndexedDb: targets.deleteIndexedDb,
    idbAvailable: targets.idbAvailable,
  });
}

/** msgfix2 U4 修单三 J2：导出给 `app/RootRouter.tsx` 复用——启动期 `loadKeys()` rejection 兜底走
 *  repair_failed 展示时，需要同一套"错误怎么变成一句可展示文案"的口径，不该在两个文件各写一份。 */
export function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
