// RootRouter.tsx — 顶层路由：checking → unpaired（配对屏）/ paired（已配对运行时）/
// repair_failed（needs_repair 清核失败·可重试）。原逻辑在 `main.tsx` 里（T6f1 起），INT1c 审查
// 返工（P1）为了给"清除失败路径"写测试而抽成独立、可注入依赖的组件——`main.tsx` 现在只是拿真实
// 依赖调用它的薄 bootstrap（`createRoot(...).render(...)`），不再自己持有任何状态/逻辑。
//
// **needs_repair 清核（INT1c 审查返工·P1）**：`connection/connectionSession.ts::
// transitionToNeedsRepair` 在认证性终态时已经自己清过一次 key-store（`connection/` 目录本单不
// 碰）——但 Root 不能只是信任"它清过了"就直接切回配对屏。收到 `onNeedsRepair` 回调后，Root
// **自己再执行一次** `key-store.clear()` 并**读回验证**（`attemptRepairClear()`，见该文件），确认
// 真的清空了才切 `unpaired`；清除或读回验证失败时展示`repair_failed`（可重试的 repair 态），不假
// 装已经清干净——`repair_failed` 页面上的重试按钮重新调用同一条清除逻辑。
//
// **msgfix2 U4（收拢 P1-3 + 四触发点③"re-pair/room 切换"）**：`eventStore`/`commandLedger`/
// `bodyCache` 三个按房间构造的存储现在都由本组件统一 `useMemo` 出来（依赖 `deps.createXxx`，root
// 级探测/装配在 `store/idbFactory.ts`，`main.tsx` 是唯一装配点）——不再像旧版那样只有 `eventStore`
// 走这条路、`commandLedger` 散落在 `AppRuntime.tsx` 内部自己默认构造。`handleNeedsRepair` 现在会
// 把这三个实例（经 ref 追踪，见 `eventStoreRef`/`commandLedgerRef`/`bodyCacheRef` 注释）一并传给
// `attemptRepairClear()`，让它们先 `close()` 再删库（`store/cacheManager.ts::purgeRoomData()`）。
// `handleActivated` 新增"换房检测"：重新配对到跟之前不同的房间时，主动清掉**旧**房间的三库（不碰
// key-store——新凭据已经落盘，不能重复清），见该函数内注释。
//
// **msgfix2 U4 修单三（第四轮修单·J2/J3，语义由 Lead 裁死）**：
//   J2——IDB 永久坏不得困死用户。① 启动期 `keyStore.loadKeys()` rejection 之前没人接（见下方
//   `useEffect` 里的 `.catch()`），未捕获的 promise 拒绝会让路由永远停在 `"checking"`（渲染
//   `null`，用户看到白屏）——现在承接住，走既有 `repair_failed` UI 路径（复用同一套"重试"按钮 +
//   `handleNeedsRepair()`）。② `handleNeedsRepair()` 新增 `repairFailureCountRef`：同一轮
//   repair 连续失败达到 2 次（第一次 + 重试一次仍失败）就不再继续展示 `repair_failed` 死循环——
//   视为"本设备存储不可用"，`console.error` 留痕后直接降级切 `unpaired`，引导用户重新配对（反正
//   这套存储已经读不出旧凭据了，困住用户没有意义）。
//   J3——`pendingRoomPurges` 启动补清：room-switch 触发的 purge（`handleActivated` 换房分支）
//   在发起前先把目标房间登记进 `localStorage`（`registerPendingRoomPurge`），purge 成功落地才
//   摘除（`clearPendingRoomPurge`）——`ROOM_SWITCH_PURGE_TIMEOUT_MS` 那道 5s 超时不再是"清理这件
//   事的终点"，只是"这次前台等待"的终点，真正兜底靠下方新增的启动 `useEffect`：每次挂载读一遍
//   这份持久化清单，对每个还挂着的房间补跑一次 purge（幂等——`purgeRoomData()` 删一个已经不存在
//   的库仍然算成功；失败/超时则原样留在清单里，等下一次启动再试）。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyStorePort, StoredPairingCredentials } from "../store/key-store.ts";
import type { EventStorePort } from "../store/port.ts";
import type { CommandLedgerPort } from "../store/commandLedger.ts";
import type { BodyCachePort } from "../store/bodyCache.ts";
import type { WebSocketFactory } from "../connection/types.ts";
import { purgeRoomData, type PurgeRoomDataOutcome, type PurgeRoomDataTargets } from "../store/cacheManager.ts";
import { RealPairingFlow } from "./RealPairingFlow.tsx";
import { AppRuntime } from "./AppRuntime.tsx";
import { attemptRepairClear, describeError } from "./repair.ts";

/** msgfix2 U4 修单三 J3：room-switch 旧房间 purge 的持久化兜底记账清单——`localStorage` 读写全程
 *  try/catch 静默（隐私模式/配额满时直接退化成"没有这份兜底"，不阻塞主流程；这份清单本身就是尽力
 *  而为，不是硬保证）。 */
export const PENDING_ROOM_PURGES_STORAGE_KEY = "agentloom.remote-web.pendingRoomPurges";

/** 读当前挂着的（还没成功 purge 完的）房间清单——读失败/内容不是预期形状一律退化成空清单。 */
export function readPendingRoomPurges(): string[] {
  try {
    const raw = window.localStorage.getItem(PENDING_ROOM_PURGES_STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((entry): entry is string => typeof entry === "string") : [];
  } catch {
    return [];
  }
}

function writePendingRoomPurges(rooms: string[]): void {
  try {
    window.localStorage.setItem(PENDING_ROOM_PURGES_STORAGE_KEY, JSON.stringify(rooms));
  } catch {
    // 静默——见上方 `PENDING_ROOM_PURGES_STORAGE_KEY` 头注。
  }
}

/** purge 发起前登记（幂等——已经登记过不重复追加）。 */
function registerPendingRoomPurge(room: string): void {
  const current = readPendingRoomPurges();
  if (!current.includes(room)) writePendingRoomPurges([...current, room]);
}

/** purge 成功落地后摘除。 */
function clearPendingRoomPurge(room: string): void {
  const current = readPendingRoomPurges();
  const next = current.filter((entry) => entry !== room);
  if (next.length !== current.length) writePendingRoomPurges(next);
}

/** msgfix2 U4 修单二 I3：room-switch 旧房间 purge 的上限——超过这个时长还没落地就不再等，当"失败"
 *  处理（`console.error` 可见，供人工/下次启动排查），不能让一次异常慢的删除（大量历史消息/损坏的
 *  IndexedDB 库）无限期挂着不结束（虽然 `handleActivated()` 早已不等它就切了 `paired`，但挂起的
 *  promise 本身仍然是个资源泄漏/诊断信号永远不落地的问题）。 */
export const ROOM_SWITCH_PURGE_TIMEOUT_MS = 5000;

/** `purgeRoomData()` 加一层超时——`purgeRoomData()` 本身不会真的 reject（内部已经把删库失败收口成
 *  `{ok:false}`），这里只处理"迟迟不 settle"的情况，超时后返回一个失败态供调用方走同一条
 *  `!purgeOutcome.ok` 分支,不需要调用方额外分支处理"超时"这第三种结果。 */
export function purgeRoomDataWithTimeout(targets: PurgeRoomDataTargets, timeoutMs: number): Promise<PurgeRoomDataOutcome> {
  return new Promise((resolve) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      resolve({ ok: false, error: `room-switch purge timed out after ${timeoutMs}ms` });
    }, timeoutMs);
    void purgeRoomData(targets).then((outcome) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(outcome);
    });
  });
}

export interface RootRouterDeps {
  keyStore: KeyStorePort;
  webSocketFactory: WebSocketFactory;
  /** 按房间构造一个新的事件库实例（INT1c P0④：不同房间必须落进不同的 IndexedDB 库，换房不
   *  串库）——调用方（`main.tsx`）注入 `store/idbFactory.ts::createStoreFactory()` 产出的工厂
   *  函数（探测失败时是内存实现的工厂，探测成功时是真实 IndexedDB 实现的工厂）。 */
  createEventStore: (room: string) => EventStorePort;
  /** msgfix2 U4（收拢 P1-3）：同 `createEventStore` 的既有惯例，按房间构造 command_id 账本——
   *  取代旧版由 `AppRuntime.tsx` 内部零散默认构造的做法。 */
  createCommandLedger: (room: string) => CommandLedgerPort;
  /** msgfix2 U4：同上，按房间构造 body cache（设计稿 §4.2）。 */
  createBodyCache: (room: string) => BodyCachePort;
  /** 默认读真实 `window.location.href`（origin 校验需要完整 URL，同 `RealPairingFlow`/
   *  `PairingScreen.tsx` 的既有口径）。 */
  getLocationHref: () => string;
  /** 默认用 `history.replaceState` 清掉 hash（fragment 卫生）。 */
  clearFragment: () => void;
  /** msgfix2 U4 修单 H3：`store/idbFactory.ts::createStoreFactory()` 的探测结果——透传给
   *  `attemptRepairClear()`/`purgeRoomData()`，内存 fallback 态下跳过真实
   *  `deleteIndexedDb()`（否则会再抛一次触发探测失败的同一个错误，把 repair 拖进死循环）。
   *  默认 `true`（省略时保持既有行为，同 `main.tsx` 未来接入前的既有调用点不受影响）。 */
  idbAvailable?: boolean;
}

type RootState =
  | { kind: "checking" }
  | { kind: "unpaired" }
  | { kind: "paired"; stored: StoredPairingCredentials }
  | { kind: "repair_failed"; error: string }
  /**
   * msgfix2 F2 S4：显式解除配对（`AppRuntime` 设置屏"解除配对"按钮）的过渡态——置这个态的同一次
   * render 就不再渲染 `<AppRuntime>`（连同它持有的活跃 WebSocket 一起卸载，`ConnectionSession`
   * 的 effect cleanup 同步调用 `session.stop()` → `socket.close()`），再开始跑 purge。跟
   * `"checking"` 一样渲染 `null`（技术性过渡态，用户停留时间通常很短——purge 走 fake-indexeddb/
   * 真实 IndexedDB 删库，一般是毫秒级），不需要专门的呈现。
   */
  | { kind: "purging" };

function RepairFailedView({ error, onRetry }: { error: string; onRetry: () => void }) {
  return (
    <div className="repair-failed" data-testid="repair-failed">
      <p data-testid="repair-failed-message">{`Unable to fully clear local pairing data (${error}). Please retry.`}</p>
      <button type="button" data-testid="repair-retry" onClick={onRetry}>
        {"Retry"}
      </button>
    </div>
  );
}

export function RootRouter({ deps }: { deps: RootRouterDeps }) {
  const [state, setState] = useState<RootState>({ kind: "checking" });
  // 同 PairingScreen.tsx 的 bootstrapPromiseRef 模式：StrictMode 双调 effect 时只真正读一次
  // IndexedDB，不是每次 setup 都重新查一遍。
  const checkPromiseRef = useRef<Promise<RootState> | null>(null);
  // msgfix2 U4（四触发点③）：重新配对前"旧房间是哪个"的记忆——`#p=` 分支不经过正常的
  // "keyStore.loadKeys() → paired" 路由（直接短路成 unpaired），旧凭据这次读取纯粹是为了记下换房
  // 前的房间号，不影响这里的路由结果。`handleActivated()` 激活后据此判断要不要清旧房间三库。
  const previousRoomRef = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (!checkPromiseRef.current) {
      // `#p=` 是用户刚刚扫码发出的显式重新配对意图，必须先于本地存量凭据判路由。这里只识别
      // marker、不清 fragment：payload 的解析、错误页与一次性清理仍全部留给下方既有的
      // `RealPairingFlow.runBootstrap()`，避免两层各消费一次 bootstrap 捕获值。
      if (deps.getLocationHref().includes("#p=")) {
        checkPromiseRef.current = deps.keyStore
          .loadKeys()
          .catch(() => null)
          .then((existing): RootState => {
            previousRoomRef.current = existing?.room ?? null;
            return { kind: "unpaired" };
          });
      } else {
        checkPromiseRef.current = deps.keyStore
          .loadKeys()
          .then((creds): RootState => (creds ? { kind: "paired", stored: creds } : { kind: "unpaired" }))
          .catch((error): RootState => {
            // msgfix2 U4 修单三 J2：启动读取凭据失败（IDB 永久坏/事务异常）之前没人接这个
            // rejection——未捕获的 promise 拒绝会让路由永远停在 "checking"（渲染 null，用户看到
            // 白屏）。承接住，走既有 repair/re-pair UI 路径：`repair_failed` 页面自带"重试"按钮，
            // 复用同一条 `handleNeedsRepair()`/`attemptRepairClear()`（含下方"重试仍失败则降级"
            // 的兜底），不需要为这条路径另开一套 UI。
            const message = describeError(error);
            console.error(`msgfix2 U4 修单三 J2: boot keyStore.loadKeys() failed: ${message}`);
            return { kind: "repair_failed", error: message };
          });
      }
    }
    checkPromiseRef.current.then((result) => {
      if (!cancelled) setState(result);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // msgfix2 U4 修单三 J3：启动补清——每次挂载读一遍 `pendingRoomPurges` 持久化清单，对每个还挂着
  // 的房间补跑一次 purge（幂等：`purgeRoomData()` 删一个已经不存在的库仍然算成功）。跟上方
  // "checking → paired/unpaired" 的路由判定完全独立并行跑，不影响、不等待路由结果——旧房间的
  // 清理从来就不是"能不能看到当前房间"的前置条件（同 `handleActivated` 换房分支"先激活、purge
  // 异步跑"的既有取向）。
  //
  // msgfix2 F2 S2（Opus 整盘审 P0）：旧版无条件对清单里每个房间发起 purge，完全不比对"这个房间是
  // 不是当前正在用的那个"——一次 room-switch purge 失败/超时留下的标记，若用户后来又配回了**同一个
  // 房间**（或标记本就是自我 purge 的残留），启动补清会把用户正在用的三库当"旧房间"删掉。必须等
  // 路由判定落定（知道"当前房间是谁"）才能安全比较；`state.kind === "checking"` 时还不知道，先
  // 不动（下方依赖数组里 `state.kind`/`state.stored.room` 变化会重新触发这个 effect，路由落定后
  // 自然补跑一次，不会漏）。
  useEffect(() => {
    if (state.kind === "checking") return;
    const currentRoom = state.kind === "paired" ? state.stored.room : null;
    const pendingRooms = readPendingRoomPurges();
    for (const room of pendingRooms) {
      if (room === currentRoom) {
        // 标记指向的正是当前配对回的这个房间——库正被使用，绝不能删；这条"待办"已经不成立了，
        // 直接摘除标记（同既有幂等口径：标记只是"待办"，不是"必须执行"）。
        clearPendingRoomPurge(room);
        continue;
      }
      void purgeRoomDataWithTimeout({ room, idbAvailable: deps.idbAvailable }, ROOM_SWITCH_PURGE_TIMEOUT_MS).then(
        (outcome) => {
          if (outcome.ok) {
            clearPendingRoomPurge(room);
          } else {
            // 失败/超时——原样留在清单里，不摘除，下次启动再补跑一次（同 room-switch 那半的既有
            // 取向：失败可见、不静默）。
            console.error(`msgfix2 U4 修单三 J3: startup pending-purge sweep failed for room ${room}: ${outcome.error}`);
          }
        },
      );
    }
  }, [state.kind === "paired" ? state.stored.room : state.kind]);

  // `eventStore`/`commandLedger`/`bodyCache` 必须按 room 稳定——`AppRuntime.tsx` 内部的
  // `ConnectionSession` 生命周期 effect 把 `eventStore` 列进依赖数组，若这里每次 render 都重新
  // 构造一份，`AppRuntime` 会把它当成"依赖变了"而不必要地断开重连整条连接。`useMemo` 只在房间真的
  // 变化（重新配对到不同房间）时才重新构造。
  const pairedRoom = state.kind === "paired" ? state.stored.room : null;
  const eventStore = useMemo(
    () => (pairedRoom !== null ? deps.createEventStore(pairedRoom) : null),
    [pairedRoom],
  );
  const commandLedger = useMemo(
    () => (pairedRoom !== null ? deps.createCommandLedger(pairedRoom) : null),
    [pairedRoom],
  );
  const bodyCache = useMemo(
    () => (pairedRoom !== null ? deps.createBodyCache(pairedRoom) : null),
    [pairedRoom],
  );

  // msgfix2 U4：三个 ref 各自"记住最近一次非 null 值"——同下方既有 `pairedRoomRef` 的手法。
  // `handleNeedsRepair`/`handleActivated` 在状态已经切离 "paired"（`pairedRoom` 变回 null、上面三个
  // `useMemo` 结果也跟着变回 null）之后仍然需要拿到"清理前那一刻正在用的实例"——ref 跨这次状态切换
  // 依然读得到，render 期间的同步赋值是幂等的，不需要额外的 effect。
  const eventStoreRef = useRef<EventStorePort | null>(null);
  const commandLedgerRef = useRef<CommandLedgerPort | null>(null);
  const bodyCacheRef = useRef<BodyCachePort | null>(null);
  if (eventStore !== null) eventStoreRef.current = eventStore;
  if (commandLedger !== null) commandLedgerRef.current = commandLedger;
  if (bodyCache !== null) bodyCacheRef.current = bodyCache;

  // FIX2 P2-7：`handleNeedsRepair` 也是 `repair_failed` 页面"重试"按钮的处理函数——点重试那一刻
  // `state.kind` 已经是 `"repair_failed"`，不再是 `"paired"`，直接读 `pairedRoom` 会拿到 `null`
  // （房间信息随状态切换丢了），把三库清除的对象派生成错误的库名。用一个 ref 记住"最近一次处于
  // paired 态时的房间"，跨这次状态切换依然读得到——render 期间的同步赋值是幂等的（多次相同赋值
  // 无副作用），不需要额外的 effect。msgfix2 U4：`handleActivated` 的换房检测也复用这个 ref
  // （见下方注释）。
  const pairedRoomRef = useRef<string | null>(null);
  if (pairedRoom !== null) {
    pairedRoomRef.current = pairedRoom;
  }

  const handleActivated = useCallback(() => {
    // `RealPairingFlow` → `RealPairingHost` 只在 `PairingSession.phase === "activated"` 时才调
    // 这个回调，那一刻 `keyStore.saveKeys()` 已经 await 完成（见
    // `pairing/pairing-session.ts::persistActivation`）——这里重新 `loadKeys()` 一次是为了拿到
    // `AppRuntime` 需要的**完整** `StoredPairingCredentials`（`RealPairingHost.onActivated` 只给
    // `deviceId`），不是不信任落盘是否成功。
    void deps.keyStore.loadKeys().then((stored) => {
      if (!stored) return;
      // msgfix2 U4（四触发点③"re-pair/room 切换"）：`previousRoomRef` 是本次挂载期间进入
      // `#p=` 分支时记下的"换房前旧房间"；`pairedRoomRef` 是"本次挂载期间任何时刻曾经处于
      // paired 态的房间"——同一会话内不经过整页刷新、直接再次触发 `handleActivated`（若产品未来
      // 支持这种流程）时靠它兜底。两者取非 null 的那个。新凭据已经落盘（换的是新房间），旧房间的
      // 三库（events/commands/bodyCache）留着没有意义——隐私缺口同 `repair.ts` 的既有论述，且会
      // 混进"上一个房间的归约状态"。key-store 不动：这次要保留的正是刚落盘的新凭据。
      const previousRoom = previousRoomRef.current ?? pairedRoomRef.current;
      previousRoomRef.current = null;
      // msgfix2 U4 修单二 I3：先激活新房间，旧房间的 purge 异步跑、不卡激活——旧版 `await
      // purgeRoomData(...)` 挡在 `setState({kind:"paired", ...})` 之前，慢删除（大量历史消息/
      // IndexedDB 大库）会让用户刚扫码配对成功却卡在白屏，直到清理完才看得到新房间。新凭据已经
      // 落盘，激活新房间不依赖旧房间清没清干净。
      setState({ kind: "paired", stored });
      // msgfix2 U4 修单三 J2：新的一轮配对/激活成功了——上一轮 repair（若有）是另一个故事，别让
      // 那次失败计数拖进这次全新会话（见下方 `repairFailureCountRef` 注释）。
      repairFailureCountRef.current = 0;
      if (previousRoom !== null && previousRoom !== stored.room) {
        const purgeTargets = {
          eventStore: eventStoreRef.current ?? undefined,
          commandLedger: commandLedgerRef.current ?? undefined,
          bodyCache: bodyCacheRef.current ?? undefined,
        };
        // msgfix2 U4 修单三 J3：发起前先登记——`ROOM_SWITCH_PURGE_TIMEOUT_MS` 那道超时只是"这次
        // 前台等待的终点"，不是"清理这件事本身的终点"；登记进 `pendingRoomPurges` 之后，就算这次
        // 超时/失败，下次启动的补清 `useEffect` 也能接着补跑。
        registerPendingRoomPurge(previousRoom);
        void purgeRoomDataWithTimeout(
          { room: previousRoom, ...purgeTargets, idbAvailable: deps.idbAvailable },
          ROOM_SWITCH_PURGE_TIMEOUT_MS,
        ).then((purgeOutcome) => {
          if (!purgeOutcome.ok) {
            // msgfix2 U4 修单二 I3：失败/超时可见——不静默、不假装干净了（同 H2 既有取向），留一条
            // 明确指向 room-switch 清理失败的诊断信息；不重新弹一个会打断当前交互的确认卡。标记
            // 不摘除——留给下次启动补清（J3）。
            console.error(`msgfix2 U4 I3: room-switch purge of previous room ${previousRoom} failed/timed out: ${purgeOutcome.error}`);
          } else {
            clearPendingRoomPurge(previousRoom);
          }
        });
      }
    });
  }, [deps.keyStore, deps.idbAvailable]);

  // msgfix2 U4 修单三 J2：同一轮 repair 连续失败的计数——第一次失败仍然停在可重试的
  // `repair_failed`（既有行为不变，`repair-failed` 页面照常展示、用户可以看清错误信息）；用户点
  // "重试"后如果又失败（累计到 2），就不再继续展示同一个死循环页面——视为"本设备存储已经不可用"
  // （常见诱因：IndexedDB 配额耗尽/连接永久损坏，不是那种重试一下就能自愈的瞬时错误），
  // `console.error` 留痕后直接降级切 `unpaired`，把用户放回配对屏重新配对（反正这套存储已经读不出
  // 旧凭据、旧凭据也清不掉，继续困在 repair_failed 里对用户没有任何帮助）。成功过一次
  // （`handleActivated`）就清零，不让计数跨会话累积。
  const repairFailureCountRef = useRef(0);

  // msgfix2 F2 S4：`handleNeedsRepair`（认证性 needs_repair）与 `handleUnpair`（显式解除配对）
  // 现在共用同一段"跑 purge、按结果决定下一个 UI 态"的逻辑——两者的差别只在于触发这段逻辑*之前*
  // 要不要先卸载 `AppRuntime`（见下方 `handleUnpair`），purge 本身、成功/失败后怎么收口是同一套。
  const runRepairClearAndResolve = useCallback(() => {
    void attemptRepairClear({
      keyStore: deps.keyStore,
      room: pairedRoomRef.current ?? "",
      eventStore: eventStoreRef.current ?? undefined,
      commandLedger: commandLedgerRef.current ?? undefined,
      bodyCache: bodyCacheRef.current ?? undefined,
      idbAvailable: deps.idbAvailable,
    }).then((outcome) => {
      if (outcome.ok) {
        repairFailureCountRef.current = 0;
        setState({ kind: "unpaired" });
        return;
      }
      repairFailureCountRef.current += 1;
      if (repairFailureCountRef.current >= 2) {
        // msgfix2 U4 修单三 J2：重试一次仍失败——降级语义，见上方 `repairFailureCountRef` 头注。
        console.error(
          `msgfix2 U4 修单三 J2: repair clear failed ${repairFailureCountRef.current} times in a row (${outcome.error}); ` +
            "treating local storage as permanently unavailable and routing back to re-pair instead of looping repair_failed.",
        );
        repairFailureCountRef.current = 0;
        setState({ kind: "unpaired" });
        return;
      }
      setState({ kind: "repair_failed", error: outcome.error });
    });
  }, [deps.keyStore, deps.idbAvailable]);

  const handleNeedsRepair = useCallback(() => {
    runRepairClearAndResolve();
  }, [runRepairClearAndResolve]);

  // msgfix2 F2 S4（Opus 整盘审 P1）：显式解除配对时连接可能还活着（不同于 `handleNeedsRepair`
  // 那条路——那条路触发的那一刻连接本就已经在认证性终态失败，`ConnectionSession` 自己已经在拆
  // 连接了）。旧版直接把 `onNeedsRepair` 复用给"解除配对"按钮，purge 跑的时候 `AppRuntime`（连同
  // 它这条活跃的 WebSocket）仍然挂在树上继续收帧写库——撞 `onblocked`（还有连接开着，
  // `deleteDatabase()` 悬挂）时 key-store 已经清了，用户被扔进 `repair_failed`；就算没撞
  // `onblocked`，purge 完成之后若又有一帧姗姗来迟被处理，也会把刚删干净的库重新写出几行数据，
  // 跟 UI 已经宣称的"已清干净"自相矛盾。
  //
  // 最简修法：先置 `purging` 态——这一步本身就会让 `<AppRuntime>` 从渲染结果里消失（下方 render
  // 分支不再命中 `"paired"`），React 同步卸载它，`ConnectionSession` 的 effect cleanup 同步调
  // `session.stop()` → `socket.close()`：连接停了，不会再有新帧被处理。卸载之后 `eventStore`/
  // `commandLedger`/`bodyCache` 三个 `useMemo` 结果变回 `null`，但 `eventStoreRef`/
  // `commandLedgerRef`/`bodyCacheRef`（`handleNeedsRepair` 早已依赖的同一套 ref）仍然保留着卸载
  // 前那一刻的实例——`runRepairClearAndResolve()` 用它们 close 再删库，跟现有 repair 路径完全同一
  // 套机制，不是另起一条。
  const handleUnpair = useCallback(() => {
    setState({ kind: "purging" });
    runRepairClearAndResolve();
  }, [runRepairClearAndResolve]);

  if (state.kind === "checking" || state.kind === "purging") return null;
  if (state.kind === "repair_failed") {
    return <RepairFailedView error={state.error} onRetry={handleNeedsRepair} />;
  }
  if (state.kind === "paired" && eventStore && commandLedger && bodyCache) {
    return (
      <AppRuntime
        stored={state.stored}
        keyStore={deps.keyStore}
        webSocketFactory={deps.webSocketFactory}
        eventStore={eventStore}
        commandLedger={commandLedger}
        bodyCache={bodyCache}
        onNeedsRepair={handleNeedsRepair}
        onUnpair={handleUnpair}
      />
    );
  }
  return (
    <RealPairingFlow
      deps={{
        keyStore: deps.keyStore,
        webSocketFactory: deps.webSocketFactory,
        getLocationHref: deps.getLocationHref,
        clearFragment: deps.clearFragment,
      }}
      onActivated={handleActivated}
    />
  );
}
