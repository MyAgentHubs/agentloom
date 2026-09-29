// AppRuntime.tsx — INT1b · 已配对运行时：真 ConnectionSession + key-store 凭据 → 入站帧解密入库
// →（session.index/msg.completed/card.*/run.status/tool.completed → 逐会话 projection；live delta
// → 逐会话 runWatermark）→ 喂 SessionListScreen/SessionStreamScreen；`replay.head` → 对"当前选中
// 会话"发 `control.snapshot` 请求（M0 §3）。INT1c 审查返工（P0 ×3）：入站帧改严格串行队列、
// replay.head/epoch.changed 的待补发与重封、入库前强制路由校验。
//
// 复用（import 消费，不改）：`connection/connectionSession.ts::ConnectionSession`（真连接+refresh
// 状态机）、`crypto/envelope.ts::seal/open`（INT1A 起已接受 `CryptoKey`，K_room 直传落储的
// non-extractable key）、`events/parseFrame.ts::parseFrame/buildControlSnapshotRequest`、
// `store/indexeddbEventStore.ts`（经 `EventStorePort` 注入，不在本文件构造具体类）、
// `ui/sessions/SessionListScreen.tsx`/`ui/stream/SessionStreamScreen.tsx`（纯 props 组件）。
//
// **如何拿到"发送 control.snapshot 请求"所需的 socket 句柄（`ConnectionSession` 不暴露 send()）**
// ——同 INT1「配对阶段真 transport」用过的同一招：`ConnectionSessionDeps.webSocketFactory` 本就是
// 官方留的注入点，这里包一层 `observableFactory`，每次 `ConnectionSession` 内部建连时把创建出的
// 那个（同一个）socket 存进一个 ref；`onReplayHead`/`onEpochChanged` 触发时直接对这个 ref 里的
// socket 发送。不碰 `connection/connectionSession.ts` 一行。
//
// **StrictMode 双调安全**：`ConnectionSession` 在 `useEffect` 内部现场 `new`（不是组件级 ref 只建
// 一次）——`.start()`/`.stop()` 是一次性状态机（`stop()` 后 phase 变 "closed"，`start()` 只接受
// "idle"，复用同一个实例第二次 `start()` 会抛），StrictMode 的 setup→cleanup→setup 因此必须让每次
// setup 拿到一个全新实例，cleanup 里 `stop()` 的正是那次 setup 自己建的那个——这是 React 官方给
// "effect 内建资源、cleanup 销资源"场景的标准写法，不是本文件独创。
//
// **入站帧严格串行（INT1c P0）**：`handleFrame` 不再各自独立起一个 `void (async () => {...})()`
// ——那样多帧并发到达时"解密→解析→事务提交→归约"四步会在不同帧之间交错执行，谁先到达谁先处理
// 完全不保证（WebCrypto 解密、IndexedDB 事务都是真异步、不止一个微任务），会破坏"里程碑必须按到达
// 序归约"这条隐含前提（比如 run.status 后到但先归约完，live 帧反而先处理导致 runId 查不到被误
// 丢弃）。改用一条 `Promise` 链式队列——每帧的完整处理串行排队，后一帧的处理**要等前一帧完全落地
// （含 forceRender）才开始**，语义上等价于"一个单线程读线程按到达序处理"。

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import {
  type ConnectionSessionPhase,
  type WebSocketFactory,
  type WebSocketLike,
} from "../connection/types.ts";
import type { KeyStorePort, StoredPairingCredentials } from "../store/key-store.ts";
import type { EventStorePort } from "../store/port.ts";
import type { CommandLedgerPort } from "../store/commandLedger.ts";
import { deriveCommandLedgerDbName, IndexedDbCommandLedger } from "../store/commandLedger.indexeddb.ts";
import type { BodyCachePort } from "../store/bodyCache.ts";
import { type HistoryLoadError } from "../ui/stream/SessionStreamScreen.tsx";
import { loadVerbosePreference, saveVerbosePreference } from "../ui/settings/verbosePreference.ts";
import { PairingErrorView } from "../ui/pairing/PairingErrorView.tsx";
import { type DesktopPresence } from "../ui/connection/ConnectionBanner.tsx";
import { createConnectionDiagnostics } from "../ui/debug/DebugPanel.tsx";
import { CommandChannel } from "./commandChannel.ts";
import { useAppRuntimeMsgFetch } from "./useAppRuntimeMsgFetch.ts";
import { useAppRuntimeSnapshotRequests } from "./useAppRuntimeSnapshotRequests.ts";
import { useAppRuntimeHistoryRequests } from "./useAppRuntimeHistoryRequests.ts";
import { useAppRuntimeColdReplay } from "./useAppRuntimeColdReplay.ts";
import { useAppRuntimeFrameIngestion } from "./useAppRuntimeFrameIngestion.ts";
import { useAppRuntimeConnection } from "./useAppRuntimeConnection.ts";
import { createAppRuntimeCore, type AppRuntimeCore } from "./appRuntimeCore.ts";
import { useAppRuntimeDerivedState } from "./useAppRuntimeDerivedState.tsx";
import { deriveSelectedSessionView } from "./appRuntimeSelectedSessionView.ts";
import { AppRuntimeUnselectedView } from "./AppRuntimeUnselectedView.tsx";
import { AppRuntimeSessionView } from "./AppRuntimeSessionView.tsx";

export interface AppRuntimeProps {
  /** 配对激活后（或冷启动读到）的完整凭据——必须已经过 `kPair` 存在性保证（本组件自己再兜底
   *  一次，理由见下方 `AppRuntime` 的 guard 分支）。 */
  stored: StoredPairingCredentials;
  /** 必须实现 `savePendingRefresh`/`loadPendingRefresh`（`ConnectionSession` 的 fail-closed
   *  构造期检查）——`IndexedDbKeyStore` 两个都实现了，调用方（`main.tsx`）传它就够。 */
  keyStore: KeyStorePort;
  webSocketFactory: WebSocketFactory;
  eventStore: EventStorePort;
  /** refresh 路径认证性走死（`ConnectionSession` 已清空 key-store）——调用方据此把 UI 切回配对屏。 */
  onNeedsRepair: () => void;
  /**
   * msgfix2 F2 S4：设置屏"解除配对"按钮的专属回调——省略时退回 `onNeedsRepair`（旧行为，向后
   * 兼容既有调用点/测试）。不能像旧版那样让"显式解除配对"跟"认证性 needs_repair"共用同一个回调：
   * 后者触发时连接本就已经在认证终态失败（`ConnectionSession` 自己已经在拆连接），而前者触发时
   * 连接可能还活着、还在正常收帧写库——调用方（`RootRouter.tsx`）必须能区分这两条路径，才能在
   * "解除配对"时先卸载本组件（停连接）再删库，不让 purge 跟一个仍在写库的活跃连接撞车。
   */
  onUnpair?: () => void;
  /** T6f3：command_id 持久账本（G3 缓解，见 `store/commandLedger.ts` 头注）——省略时按房间派生
   *  一个真实 `IndexedDbCommandLedger`（同 `eventStore` 的既有"调用方按房间构造"惯例，但这里默认
   *  内部构造而不强制调用方传入，因为它是本单新增的内部装配细节，不像 `eventStore` 那样早已是
   *  `RootRouter`/`main.tsx` 装配图的一部分）；测试可注入 `InMemoryCommandLedger` 或另一个真实
   *  IndexedDB 实例来模拟"另一台手机"（不同账本 = 不同设备的持久记账）。 */
  commandLedger?: CommandLedgerPort;
  /** msgfix2 U4：body cache（设计稿 §4.2）——同 `commandLedger` 的既有取向，省略时按房间派生一个
   *  真实 `IndexedDbBodyCache`（包一层 `withMemoryFallback()`，运行期事务失败降级内存）。生产装配
   *  点是 `app/RootRouter.tsx`（经 `store/idbFactory.ts` 探测后的工厂函数），这里的默认值只服务于
   *  测试/直接实例化 `AppRuntime` 的调用点。 */
  bodyCache?: BodyCachePort;
  /** 测试可缩短；生产默认 15 秒，避免 control.history 永久占住分页 UI。 */
  historyRequestTimeoutMs?: number;
}

/**
 * 顶层导出——guard `stored.kPair` 缺失这个理论上不可达但结构上可能的存量态（详见函数体注释），
 * 缺失时不能安全构造 `ConnectionCredentials`（`kPair` 是必填字段），引导重新配对而不是硬塞一个假
 * 值假装能连。真正的连接/归约/渲染逻辑全在 `AppRuntimeConnected`（拆开是因为 React hooks 不能在
 * "可能提前 return"的组件里无条件调用）。
 */
export function AppRuntime({
  stored,
  keyStore,
  webSocketFactory,
  eventStore,
  onNeedsRepair,
  onUnpair,
  commandLedger,
  bodyCache,
  historyRequestTimeoutMs,
}: AppRuntimeProps) {
  if (!stored.kPair) {
    // INT1 起 `pairing/pairing-session.ts::persistActivation` 已经把 kPair 随 saveKeys() 一并落盘
    // （`store/key-store.ts` 顶注记录的"已知缺口"在本单开工前已由上游修好）——只有更早的存量记录
    // 才可能缺它，且 K_pair 是配对握手期的临时派生密钥，早已从内存丢失，没有安全的办法补算，只能
    // 引导用户回配对屏重新扫码。
    return <PairingErrorView kind="needs-repair" reason={null} />;
  }
  return (
    <AppRuntimeConnected
      stored={stored}
      kPair={stored.kPair}
      keyStore={keyStore}
      webSocketFactory={webSocketFactory}
      eventStore={eventStore}
      onNeedsRepair={onNeedsRepair}
      onUnpair={onUnpair}
      commandLedger={commandLedger}
      bodyCache={bodyCache}
      historyRequestTimeoutMs={historyRequestTimeoutMs}
    />
  );
}

/** `handleReplayHead`/`handleEpochChanged`/`setSelectedSessionId` 共享的"当前是否有一条待处理的
 *  snapshot 请求"状态——不是 React state（不需要触发渲染，纯粹是发送逻辑的簿记）。 */
export interface PendingSnapshotRequest {
  sessionId: string;
  commandId: string;
}

export interface PendingHistoryRequest {
  sessionId: string;
  beforeMessageId: number | null;
  commandId: string;
  attempt: number;
  timeoutHandle: ReturnType<typeof setTimeout> | null;
}

const DEFAULT_HISTORY_REQUEST_TIMEOUT_MS = 15_000;

function AppRuntimeConnected({
  stored,
  kPair,
  keyStore,
  webSocketFactory,
  eventStore,
  onNeedsRepair,
  onUnpair,
  commandLedger,
  bodyCache,
  historyRequestTimeoutMs = DEFAULT_HISTORY_REQUEST_TIMEOUT_MS,
}: AppRuntimeProps & { kPair: Uint8Array }) {
  const coreRef = useRef<AppRuntimeCore | null>(null);
  if (!coreRef.current) {
    coreRef.current = createAppRuntimeCore();
  }
  const core = coreRef.current;

  // 归约层是可变引用（Map 就地改写），不是 React state——用一个递增计数器强制重渲染，同
  // `usePairingSession.ts`/既有配对屏对"命令式状态机 + React 反应式薄封装"的处理手法。
  const [, forceRender] = useReducer((n: number) => n + 1, 0);
  const [connectionDiagnostics, setConnectionDiagnostics] = useState(createConnectionDiagnostics);

  const [selectedSessionId, setSelectedSessionIdState] = useState<string | null>(null);
  const selectedSessionIdRef = useRef<string | null>(null);
  // msgfix2 U3（设计稿 v4.1 §4.1/§4.4）：设置屏——只从"未选中会话"的会话列表页可达（低频入口，
  // 同 CLAUDE.md 项目"repo 切换"这类低频功能的既有取向）；`verboseEnabled` 是「显示详细活动」
  // 开关的当前值，挂载时读一次 localStorage 偏好（`loadVerbosePreference()` 读失败已在该文件内部
  // try/catch 降级为 false，这里不需要再包一层）。
  const [showSettings, setShowSettings] = useState(false);
  const [verboseEnabled, setVerboseEnabledState] = useState(() => loadVerbosePreference());
  const setVerboseEnabled = useCallback((next: boolean) => {
    saveVerbosePreference(next);
    setVerboseEnabledState(next);
  }, []);
  const [connectionState, setConnectionState] = useState<{
    phase: ConnectionSessionPhase;
    phaseChangedAtMs: number;
    disconnectedSinceMs: number | null;
  }>(() => {
    const nowMs = Date.now();
    return { phase: "idle", phaseChangedAtMs: nowMs, disconnectedSinceMs: nowMs };
  });
  // C1-PS（dogfood 修障第二批·手机发消息桌面离线无反馈）：relay 定向回的 presence 快照/广播
  // （`{t:"presence", role:"desktop", event}`，见 `handlePlaintextCommandFrame`）折算出的桌面在线
  // 态——初始 `"unknown"`（还没收到任何快照）；连接断开时也回 `"unknown"`（旧快照对新连接不再
  // 权威，见下方 `onPhaseChange`），等下一次连上后新快照重新确认。
  const [desktopPresence, setDesktopPresence] = useState<DesktopPresence>("unknown");

  const currentSocketRef = useRef<WebSocketLike | null>(null);
  const observableFactory = useMemo<WebSocketFactory>(
    () => (url, protocols) => {
      const socket = webSocketFactory(url, protocols);
      currentSocketRef.current = socket;
      return socket;
    },
    [webSocketFactory],
  );

  const kRoomKey = stored.kRoomKey;
  const room = stored.room;

  // -------------------------------------------------------------------------
  // control.snapshot 请求：待补发标记 + epoch 追踪 + 在飞请求簿记（INT1c P0）
  // -------------------------------------------------------------------------
  /** 目前已知的最新 epoch——`onReplayHead`/`onEpochChanged` 都会更新它；发送前一律读这个 ref 的
   *  "此刻最新值"，不复用某次回调触发时捕获的旧值（"任何发送前复核当前 epoch"）。 */
  const currentEpochRef = useRef<number | null>(null);
  /** `replay.head` 到达时还没有选中会话——记一个"待补发"标记，选中会话后立即补发。 */
  const awaitingSelectionForSnapshotRef = useRef(false);
  /** 当前"在飞"（已发送、尚未确认收到对应 snapshot 应答）的请求——`onEpochChanged` 据此用同一个
   *  `command_id` 按新 epoch 重封重发；收到该会话的 snapshot 应答后清空（不再需要因为后续 epoch
   *  变更而重发一个已经有回应的请求）。 */
  const pendingSnapshotRequestRef = useRef<PendingSnapshotRequest | null>(null);
  const pendingHistoryRequestRef = useRef<Map<string, PendingHistoryRequest>>(new Map());
  const historyErrorRef = useRef<Map<string, HistoryLoadError>>(new Map());
  // U3：`pendingHistoryRequestRef`/`historyErrorRef` 是就地改写的 ref（不是 React state）——历史
  // 加载按钮的 loading/error 呈现不再在 render 期直读它们，改经这个显式计数 state 门控（凡是这两个
  // ref 的"存在性"发生变化——新建/清空一条 pending、error 增删——就 bump 一次）：下方渲染只信任
  // `historyRevision` 触发的重渲染，不依赖别处调用 `forceRender()` 是否恰好覆盖到了这次变化。
  const [historyRevision, setHistoryRevision] = useState(0);
  const bumpHistoryRevision = useCallback(() => setHistoryRevision((n) => n + 1), []);

  useEffect(() => () => {
    for (const pending of pendingHistoryRequestRef.current.values()) {
      if (pending.timeoutHandle !== null) clearTimeout(pending.timeoutHandle);
    }
    pendingHistoryRequestRef.current.clear();
  }, []);

  // -------------------------------------------------------------------------
  // T6f3：命令面发送层（input.send / input.answer / control.stop）——`CommandChannel` 复用
  // 上面同一份 `currentEpochRef`/`currentSocketRef`（G4 硬语义"任何发送前复核当前 epoch/socket"
  // 与 `sendSnapshotRequest` 是同一条既有纪律，不是另起一套）。`commandLedger` 省略时按房间构造
  // 一个真实 `IndexedDbCommandLedger`（G3：先持久化再发送）。
  // -------------------------------------------------------------------------
  const commandLedgerRef = useRef<CommandLedgerPort | null>(null);
  if (!commandLedgerRef.current) {
    commandLedgerRef.current = commandLedger ?? new IndexedDbCommandLedger(deriveCommandLedgerDbName(room));
  }
  const commandChannelRef = useRef<CommandChannel | null>(null);
  if (!commandChannelRef.current) {
    commandChannelRef.current = new CommandChannel({
      room,
      kRoomKey,
      getEpoch: () => currentEpochRef.current,
      getSocket: () => currentSocketRef.current,
      ledger: commandLedgerRef.current,
      onChange: forceRender,
    });
  }
  const commandChannel = commandChannelRef.current;

  const { cacheEnabled, toggleCacheEnabled, msgFetchClient, loadFullTextViaCacheOrFetch } = useAppRuntimeMsgFetch({
    room,
    kRoomKey,
    bodyCache,
    commandChannel,
    core,
    forceRender,
    currentEpochRef,
    currentSocketRef,
  });

  const { sendSnapshotRequest } = useAppRuntimeSnapshotRequests({
    kRoomKey,
    room,
    commandChannel,
    currentEpochRef,
    currentSocketRef,
  });
  const {
    clearHistoryPending,
    failHistoryRequests,
    sendHistoryRequest,
    requestHistory,
    setSelectedSessionId,
    handleReplayHead,
    handleEpochChanged,
  } = useAppRuntimeHistoryRequests({
    kRoomKey,
    room,
    commandChannel,
    core,
    forceRender,
    currentEpochRef,
    currentSocketRef,
    pendingSnapshotRequestRef,
    pendingHistoryRequestRef,
    historyErrorRef,
    awaitingSelectionForSnapshotRef,
    selectedSessionIdRef,
    setSelectedSessionIdState,
    bumpHistoryRevision,
    historyRequestTimeoutMs,
    sendSnapshotRequest,
  });

  useAppRuntimeColdReplay({ eventStore, core, forceRender });
  const { handleFrame } = useAppRuntimeFrameIngestion({
    commandChannel,
    core,
    eventStore,
    room,
    kRoomKey,
    msgFetchClient,
    forceRender,
    clearHistoryPending,
    failHistoryRequests,
    sendSnapshotRequest,
    sendHistoryRequest,
    currentEpochRef,
    pendingSnapshotRequestRef,
    pendingHistoryRequestRef,
    setDesktopPresence,
  });

  useAppRuntimeConnection({
    stored,
    kPair,
    observableFactory,
    keyStore,
    eventStore,
    onNeedsRepair,
    handleFrame,
    handleReplayHead,
    handleEpochChanged,
    failHistoryRequests,
    commandChannel,
    setConnectionDiagnostics,
    setConnectionState,
    setDesktopPresence,
  });

  const { sessions, activeRepo, debugPanel, historyLoading, historyError } = useAppRuntimeDerivedState({
    core,
    connectionDiagnostics,
    selectedSessionId,
    historyRevision,
    pendingHistoryRequestRef,
    historyErrorRef,
    msgFetchClient,
    loadFullTextViaCacheOrFetch,
  });

  if (selectedSessionId === null) {
    return (
      <AppRuntimeUnselectedView
        showSettings={showSettings}
        connectionState={connectionState}
        desktopPresence={desktopPresence}
        verboseEnabled={verboseEnabled}
        onToggleVerbose={setVerboseEnabled}
        cacheEnabled={cacheEnabled}
        onToggleCache={toggleCacheEnabled}
        onUnpair={onUnpair}
        onNeedsRepair={onNeedsRepair}
        onSettingsBack={() => setShowSettings(false)}
        sessions={sessions}
        activeRepo={activeRepo}
        onSelect={setSelectedSessionId}
        onOpenSettings={() => setShowSettings(true)}
        debugPanel={debugPanel}
      />
    );
  }

  const {
    streamProps,
    decisionAnswerOverrides,
    sendBadge,
    stopBadge,
    msgFetchStates,
    onLoadFullText,
  } = deriveSelectedSessionView({
    selectedSessionId,
    core,
    commandChannel,
    connectionPhase: connectionState.phase,
    desktopPresence,
    msgFetchClient,
    loadFullTextViaCacheOrFetch,
  });
  const onBack = () => setSelectedSessionId(null);
  const onLoadEarlier = () => {
    if (!streamProps.historyExhausted) {
      const oldestKnownMessageId = streamProps.messages[0]?.messageId ?? null;
      requestHistory(selectedSessionId, streamProps.historyCursor ?? oldestKnownMessageId);
    }
  };
  const onDecisionChoose = (decisionId: string, option: string) => {
    void commandChannel.answerCard(selectedSessionId, decisionId, option);
  };
  const onStop = () => {
    void commandChannel.stopSession(selectedSessionId);
  };
  const onSend = (text: string) => {
    void commandChannel.sendInput(selectedSessionId, text);
  };
  return (
    <AppRuntimeSessionView
      connectionState={connectionState}
      desktopPresence={desktopPresence}
      streamProps={streamProps}
      onBack={onBack}
      historyLoading={historyLoading}
      historyError={historyError}
      onLoadEarlier={onLoadEarlier}
      onDecisionChoose={onDecisionChoose}
      decisionAnswerOverrides={decisionAnswerOverrides}
      onStop={onStop}
      stopBadge={stopBadge}
      msgFetchStates={msgFetchStates}
      onLoadFullText={onLoadFullText}
      verboseEnabled={verboseEnabled}
      onSend={onSend}
      sendBadge={sendBadge}
      debugPanel={debugPanel}
    />
  );
}
