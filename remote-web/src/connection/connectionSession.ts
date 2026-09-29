// connectionSession.ts — T6c-refresh · ConnectionSession 状态机(连接 + refresh 轮换 + 无状态码
// 分类 + 前台恢复 + 多标签单飞行 + 日志脱敏)。
//
// 权威参照(只读对照,只 import 消费,一行不改——任务书首段红线,`crypto/`/`pairing/`/`events/` 三目录):
//   - Web client protocol contract: the v0.5 block plus its credential-persistence section.
//   - Protocol contract for the connection lifecycle: the three-phase window, the
//     pair.ready persistence step, the three-frame refresh handshake, and logging discipline.
//   - remote-relay/src/room-do.js(`REAUTH_CLOSE_REASON`/`MESSAGE_RATE_LIMIT_CLOSE_REASON`/
//     `device_revoked` 错误帧·"put_rejected 让手机凭旧 refresh 立刻重试"——只读对照)。
//   - app/src-tauri/src/remote_gateway.rs(refresh 状态机注释·只读对照,桌面是签发方,本模块是
//     消费方,帧形状不逐行照抄实现,照抄的是**协议行为**)。
//
// **本单已知缺口(任务书 §4⑥ 记档,详情见 store/key-store.ts 顶部 `kPair` 字段注释)**:K_pair 由
// 调用方显式注入(`ConnectionCredentials.kPair`),不是本模块自己从 `KeyStorePort.loadKeys()` 静默
// 摸出来的——`PairingSession`(T6b·pairing/ 目录本单不可改)目前不落盘 K_pair,这个字段要么由后续
// 小刀补上持久化,要么由整合 PairingSession 与 ConnectionSession 的上层在内存里直接传递。

import type {
  ConnectionCredentials,
  ConnectionPhase,
  ConnectionSessionPhase,
  ForegroundResumePort,
  LocksPort,
  LogLevel,
  LogSink,
  RotatedCredentials,
  UpgradeFailureClassification,
  WebSocketCloseInfo,
  WebSocketFactory,
  WebSocketLike,
} from "./types.ts";
import { ReadyState } from "./types.ts";
import { BackoffTracker, type BackoffOptions } from "./backoff.ts";
import { classifyCloseReason, classifyUpgradeFailure, DEFAULT_ACCESS_LIFETIME_MS, DEFAULT_REFRESH_UNTIL_WINDOW_MS } from "./upgradeClassifier.ts";
import { connectLockName, refreshLockName, withConnectionLeadership, withRefreshSingleFlight } from "./webLocks.ts";
import { redact } from "./redact.ts";
import {
  buildTokenRefreshFrame,
  isValidRequestId,
  openRefreshOkBody,
  parseRefreshResponseFrame,
  sealRefreshRequestBody,
  type TokenRefreshFailFrame,
  type TokenRefreshOkFrame,
} from "./refreshFrames.ts";
import type { KeyStorePort, StoredPairingCredentials } from "../store/key-store.ts";

/**
 * refresh 能力必备的 KeyStore——审查返工(fail-closed)：`savePendingRefresh`/`loadPendingRefresh`
 * 在 `KeyStorePort` 上是可选方法(理由见 `store/key-store.ts` 该接口注释——`pairing/
 * pairing-session.test.ts` 的 `DeferredKeyStore` 测试替身不需要也不该被强制实现它们)，但
 * `ConnectionSession` 离开这两个方法无法安全运作 refresh(§9.5 落盘契约要求"先落盘再发送"，没有
 * 持久层就没有崩溃/重载安全)。构造函数在这里做一次性 fail-closed 检查——缺失即拒绝构造，不再对
 * 每次调用点各自可选链静默降级(旧实现的隐患：某次调用忘了检查就会悄悄跳过持久化)。
 */
type RefreshCapableKeyStore = KeyStorePort &
  Required<Pick<KeyStorePort, "savePendingRefresh" | "loadPendingRefresh">>;

function requireRefreshCapableKeyStore(keyStore: KeyStorePort): RefreshCapableKeyStore {
  if (!keyStore.savePendingRefresh || !keyStore.loadPendingRefresh) {
    throw new Error(
      "ConnectionSession requires a KeyStore implementing savePendingRefresh()/loadPendingRefresh() — " +
        "refresh cannot be operated safely without durable pending_refresh bookkeeping (fail-closed)",
    );
  }
  return keyStore as RefreshCapableKeyStore;
}

const SUBPROTOCOL_RC_V1 = "agentloom-rc-v1";
/** M0 §3 v0.5 块:"回前台立即评估" + G8-knife "message_rate_limited 不能被误判成认证问题"——
 * 单飞行冲突/桌面自愈重试都走一个短、固定、独立于连接退避的重试节奏。 */
const DEFAULT_REFRESH_RETRY_DELAY_MS = 2_000;
/** access 名义寿命的多大比例算"该主动 refresh 了"——留出富余,不用等到真过期才动手。 */
const DEFAULT_PROACTIVE_REFRESH_RATIO = 0.8;
/**
 * FIX2 P0-1 第三环:open 后这么久还没收到 `replay.head` 就触发一次"本地钟判定"检查。默认 10s——
 * 见 `ConnectionSessionDeps.replayHeadWatchdogMs` 注释。
 */
const DEFAULT_REPLAY_HEAD_WATCHDOG_MS = 10_000;

export interface ConnectionSessionDeps {
  webSocketFactory: WebSocketFactory;
  keyStore: KeyStorePort;
  /** `undefined` = 环境没有 Web Locks API——单标签直通(不装 polyfill,见 webLocks.ts)。 */
  locks?: LocksPort;
  /** 默认:永不触发的空端口(不装 polyfill;生产环境由调用方提供基于 `document`/`window` 的实现)。 */
  foregroundResume?: ForegroundResumePort;
  log?: LogSink;
  now?: () => number;
  scheduleTimer?: (callback: () => void, delayMs: number) => unknown;
  clearTimer?: (handle: unknown) => void;
  requestIdFactory?: () => string;
  /** 断点续传水位——不直接依赖 `store/port.ts::EventStorePort`(避免跨模块耦合),由调用方注入。 */
  getLastSeq?: () => number | Promise<number>;
  backoff?: Partial<BackoffOptions>;
  accessLifetimeMs?: number;
  /** current 别名 refresh_until 宽限窗(默认 30 天,见 upgradeClassifier.ts 顶注)。 */
  refreshUntilWindowMs?: number;
  proactiveRefreshRatio?: number;
  /** 判定"这份 token.refresh.ok 回执像是过期重放"的往返耗时门槛,默认=accessLifetimeMs。 */
  staleReplayThresholdMs?: number;
  refreshRetryDelayMs?: number;
  /**
   * FIX2 P0-1 第三环(open 后静默降级连接兜底):open 后这么久(默认 10s)还没收到 `replay.head`,
   * 且此刻本地钟判定 access 已经过了名义寿命(`accessLifetimeMs`)——主动 `beginRefresh()`。
   *
   * **为什么 `maybeProactiveRefreshOrArmWatchdog()`(open 时那次检查)不够**:那次检查只看"open 这一瞬间"的本地
   * 钟,如果 access 恰好没越过 0.8 倍寿命的阈值就不会动手;但 relay 判定"这条连接是不是配得上完整
   * 数据面"用的是它自己的时钟/校验路径,不保证跟客户端本地钟完全同步——一条 relay 判定为"仅可
   * refresh"的降级连接**不会**用任何专属信号告诉客户端("refresh-scope 降级连接的唯一可观测信号
   * 就是这种静默"——见 `onReplayHeadWatchdogFired()` 实现注释):registry_ready 前静默期本身是正常
   * 现象(见下方 `classifyOutcome` 头注),没法靠"迟迟没收到帧"本身区分"正常静默"与"被静默降级"。
   * 这条 watchdog 把两个独立信号叠加起来判断:静默(没有 `replay.head`)+ 本地钟认为这份 access 早该
   * 过期了——两者同时成立才值得赌一把主动 refresh(单独任一个都不够:纯静默可能只是正常的
   * registry_ready 前等待;纯"本地钟过期"在 open 那一刻已经被 `maybeProactiveRefreshOrArmWatchdog()` 处理过)。
   */
  replayHeadWatchdogMs?: number;
}

export interface ConnectionSessionCallbacks {
  onOpen?: () => void;
  onClose?: (info: WebSocketCloseInfo) => void;
  /** M2 C1 spec §3 连接项:"epoch.changed 处理挂点(转发给 events 层的回调)"——本模块只转发,不处理。 */
  onEpochChanged?: (epoch: number, ts: number) => void;
  /**
   * 审查返工新增:收到 `replay.head` 时的专属挂点(前台恢复分支的一部分)——回前台重连后一旦
   * relay 回了 `replay.head`,接线层应该据此给每个 running 会话补发 `control.snapshot` 请求。
   * 本模块只负责在这里可靠地转发 `{epoch, headSeq}`——**真正发送 `control.snapshot` 帧归接线层**
   * (T6f2/T6f3):本模块不知道"哪些会话在跑",没有资格代它们决定要不要请求快照。
   */
  onReplayHead?: (epoch: number, headSeq: number) => void;
  /** 任何本模块不消费的帧(event/live 密文信封、replay.head、presence、其它 error reason……)一律
   * 原样转发,交给 events 层(T6d1)处理——本模块的职责边界只是连接/refresh,不是完整数据面。
   * (`replay.head` 额外经上面的 `onReplayHead` 单独转发一次——两个回调不互斥,服务不同的下游。) */
  onFrame?: (frame: unknown) => void;
  onCredentialsRotated?: (rotated: RotatedCredentials) => void;
  /** 终态:refresh 路径彻底走死或收到桌面确认的连续无效——本模块已经清空长期凭据,调用方据此
   * 切到"重新配对"UI(M2 C1 spec §3 终态闭环)。 */
  onNeedsRepair?: (reason: string) => void;
  onPhaseChange?: (phase: ConnectionSessionPhase) => void;
}

function noopLog(): void {}
function neverResumingForegroundPort(): ForegroundResumePort {
  return { onResume: () => () => {} };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export class ConnectionSession {
  private phaseValue: ConnectionSessionPhase = "idle";
  private socket: WebSocketLike | null = null;
  private stopped = false;
  private stopWaiters: Array<() => void> = [];
  private connectionPhaseKind: ConnectionPhase = "first_connect";
  private backoffTracker: BackoffTracker;
  /** 前台恢复主动关闭当前 socket 后,下一轮重连要跳过退避(§3 v0.5 块"回前台立即评估")。 */
  private skipNextBackoff = false;

  private pendingRefreshRequestId: string | null = null;
  private refreshInFlight = false;
  private refreshSentAtMs: number | null = null;
  /**
   * 审查返工新增:这一轮 pendingRefreshRequestId 是不是从 KeyStore 恢复的(而不是本会话内新发起
   * 的)——跨页面重载后 `refreshSentAtMs` 即便补上了持久化的发送时刻,仍然不能完全信任"往返耗时"
   * 这把尺子(重载前后经历了什么、真实发送时刻精不精确都不再可控)。收到回执时只要这个标记为
   * true,一律按"保守立即二次轮换"处理,不单纯依赖往返耗时的阈值比较——见 `handleRefreshOk`。
   */
  private resumedPendingRefresh = false;
  private refreshDeferred: { resolve: () => void; reject: (error: unknown) => void } | null = null;
  private forceRefreshOnNextOpen = false;
  private localGeneration: number | undefined = undefined;

  /** FIX2 P0-1 第三环:见 `onConnectionOpened()`/`onReplayHeadWatchdogFired()`。 */
  private replayHeadWatchdogHandle: unknown = null;

  /** 审查返工:fail-closed 检查后的必备引用,见 `requireRefreshCapableKeyStore` 顶注。 */
  private readonly keyStore: RefreshCapableKeyStore;

  private readonly accessLifetimeMs: number;
  private readonly refreshUntilWindowMs: number;
  private readonly proactiveRefreshRatio: number;
  private readonly staleReplayThresholdMs: number;
  private readonly refreshRetryDelayMs: number;
  private readonly replayHeadWatchdogMs: number;

  constructor(
    private readonly credentials: ConnectionCredentials,
    private readonly deps: ConnectionSessionDeps,
    private readonly callbacks: ConnectionSessionCallbacks = {},
  ) {
    this.keyStore = requireRefreshCapableKeyStore(deps.keyStore);
    this.accessLifetimeMs = deps.accessLifetimeMs ?? DEFAULT_ACCESS_LIFETIME_MS;
    this.refreshUntilWindowMs = deps.refreshUntilWindowMs ?? DEFAULT_REFRESH_UNTIL_WINDOW_MS;
    this.proactiveRefreshRatio = deps.proactiveRefreshRatio ?? DEFAULT_PROACTIVE_REFRESH_RATIO;
    this.staleReplayThresholdMs = deps.staleReplayThresholdMs ?? this.accessLifetimeMs;
    this.refreshRetryDelayMs = deps.refreshRetryDelayMs ?? DEFAULT_REFRESH_RETRY_DELAY_MS;
    this.replayHeadWatchdogMs = deps.replayHeadWatchdogMs ?? DEFAULT_REPLAY_HEAD_WATCHDOG_MS;
    this.backoffTracker = new BackoffTracker({
      baseMs: 1_000,
      capMs: 60_000,
      ...deps.backoff,
    });
  }

  get phase(): ConnectionSessionPhase {
    return this.phaseValue;
  }

  private setPhase(phase: ConnectionSessionPhase): void {
    if (this.phaseValue === phase) return;
    this.phaseValue = phase;
    this.callbacks.onPhaseChange?.(phase);
  }

  private now(): number {
    return this.deps.now?.() ?? Date.now();
  }

  private log(level: LogLevel, message: string, context?: Record<string, unknown>): void {
    const sink = this.deps.log ?? noopLog;
    const redactedContext: Record<string, unknown> | undefined = context
      ? Object.fromEntries(Object.entries(context).map(([key, value]) => [key, typeof value === "string" ? redact(value) : value]))
      : undefined;
    sink(level, redact(message), redactedContext);
  }

  // -------------------------------------------------------------------------
  // 生命周期
  // -------------------------------------------------------------------------

  /**
   * 启动连接生命周期——恰好调用一次。**返回时只代表"是否拿到连接锁"已经确定**,不代表连接生命
   * 周期已经结束:拿到锁时 `runLoop()` 在后台持续跑到 `stop()` 为止(这条锁本来就该按连接的整个
   * 生命周期持有,不是按单次连接尝试持有——见 `withConnectionLeadership` 注释);若 `start()` 等
   * 那整条 promise 才返回,调用方会永远拿不到返回值(除非连接从未成功过就立刻 `stop()`)。
   */
  async start(): Promise<void> {
    if (this.phaseValue !== "idle") {
      throw new Error(`ConnectionSession.start() called from phase "${this.phaseValue}", expected "idle"`);
    }
    // 审查返工(锁与恢复顺序):pending_refresh 恢复**不再**在这里做——它现在是 `runLoop()` 的第一
    // 件事,只有真正拿到 connection leadership(下面 `withConnectionLeadership` 的 `task` 被调用)
    // 的那个标签页才会执行到它。被动标签(没拿到连接锁)绝不该去抢 refresh 单飞行锁——那会在真正
    // 的 leader 标签稍后想发起 refresh 时把它锁死(被动标签握着锁却永远等不到一条它能发送请求的
    // 活连接,锁永不释放)。
    const lockName = connectLockName(this.credentials.room);
    const acquisition = new Promise<{ acquired: boolean; viaFallback: boolean }>((resolveAcquisition) => {
      withConnectionLeadership(
        this.deps.locks,
        lockName,
        () => this.runLoop(),
        (acquired, viaFallback) => resolveAcquisition({ acquired, viaFallback }),
      ).catch((error) => this.log("error", "connection lifecycle failed unexpectedly", { error: String(error) }));
    });
    const { acquired, viaFallback } = await acquisition;
    if (viaFallback) {
      this.log("info", "Web Locks API unavailable — single-tab passthrough, no cross-tab connection mutual exclusion");
    }
    if (!acquired) {
      this.log("info", "another tab already holds the connection lock; this tab stays passive (single connection per origin)");
      this.setPhase("idle");
    }
  }

  /** 优雅停止——关掉当前 socket(若有),让重连循环退出、释放任何飞行中的 refresh 单飞行锁。可安全重复调用。 */
  async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    this.socket?.close(1000, "client_stop");
    this.socket = null;
    this.disarmReplayHeadWatchdog();
    for (const waiter of this.stopWaiters.splice(0)) waiter();
    // 飞行中的 refresh 若还没收到回执就被 stop() 打断——释放 `withRefreshSingleFlight` 持有的跨标签
    // 锁(否则真实 Web Locks API 下这把锁会被永久悬空持有,直到页面卸载)。不清 pendingRefreshRequestId/
    // 不清 keyStore 里的 pending_refresh——那是"这次没跑完,下次(下次 start() 或另一个标签页)该继续
    // 跑同一个 request_id"的正常语义,不是错误。
    this.finishRefreshAttempt();
    this.setPhase("closed");
  }

  /**
   * M0 §9.5 落盘契约第②条 + §3.7 重启恢复:启动时若发现上次会话留下的 pending_refresh,复用同一
   * `request_id` 续跑(不生成新 id)——`runRefreshRoundTrip` 会在 `onConnectionOpened()` 里被首个
   * `open` 事件触发实际重发。**只应该被 `runLoop()` 调用**(即只有拿到 connection leadership 的
   * 标签页才会跑到这里——见 `start()`/`runLoop()` 顶注,防止被动标签抢 refresh 单飞行锁)。
   */
  private async resumePendingRefreshFromStore(): Promise<void> {
    const pending = await this.keyStore.loadPendingRefresh();
    if (!pending) return;
    if (!isValidRequestId(pending.requestId)) {
      // fail-closed:存量记录里的 request_id 形状不对(篡改/迁移脏数据)——不能安全复用,直接把这条
      // 坏记录从 KeyStore 里清掉,当作"没有 pending"处理,让后续的正常触发(主动 refresh/首连)重新
      // 走一遍干净流程,而不是拿一个畸形 id 去发请求。
      this.log("error", "resumed pending_refresh has an invalid request_id — discarding it (fail-closed)", {
        requestId: String(pending.requestId),
      });
      try {
        const existing = await this.keyStore.loadKeys();
        if (existing) {
          await this.keyStore.saveKeys({ ...existing, pendingRefresh: null });
        }
      } catch {
        // 清理失败不阻塞启动——下次 loadPendingRefresh 仍会读到这条坏记录并再次走这个分支,
        // 是安全的(幂等),不会比现在更糟。
      }
      return;
    }
    this.log("info", "resuming a pending_refresh left over from a previous session", { requestId: pending.requestId });
    this.pendingRefreshRequestId = pending.requestId;
    this.localGeneration = pending.generation;
    this.refreshSentAtMs = pending.sentAtMs ?? null;
    this.resumedPendingRefresh = true;
    this.refreshInFlight = true;
    const lockName = refreshLockName(this.credentials.room, this.credentials.deviceId);
    void withRefreshSingleFlight(this.deps.locks, lockName, () => this.runRefreshRoundTrip(pending.requestId)).catch(
      (error) => this.log("error", "resumed refresh round trip failed", { error: String(error) }),
    );
  }

  // -------------------------------------------------------------------------
  // 主循环:连接 → (open→close) → 分类 → 退避/终态 → 重连
  // -------------------------------------------------------------------------

  private async runLoop(): Promise<void> {
    // 审查返工(锁与恢复顺序):只有真正拿到 connection leadership 才会执行到这里(`runLoop` 就是
    // `withConnectionLeadership` 的 `task`)——pending_refresh 恢复放在这第一步,被动标签永远不会
    // 调用它,自然也就永远不会去抢 refresh 单飞行锁。
    await this.resumePendingRefreshFromStore();
    while (!this.stopped) {
      this.setPhase("connecting");
      const outcome = await this.attemptOneConnection();
      if (this.stopped) break;

      const classification = this.classifyOutcome(outcome);
      this.log("info", "connection outcome classified", { classification });
      if (classification === "needs_repair") {
        await this.transitionToNeedsRepair(
          outcome.everOpened ? `close reason classified as needs_repair: ${outcome.closeReason}` : "upgrade failure classified as needs_repair by local clock",
        );
        break;
      }
      if (classification === "needs_refresh") {
        this.forceRefreshOnNextOpen = true;
      }
      // 审查返工(前台恢复零退避):`setupForegroundResumeWhileOpen()` 主动关闭"可疑"socket 时会
      // 置位 `skipNextBackoff`——那不是一次失败,是用户主动把 app 带回前台后我们自愿发起的重连,
      // 不该背上正常失败退避的等待时间,也不该推进 `backoffTracker` 的失败计数(否则下一次真正的
      // 失败会从一个被抬高的退避基数开始,不公平地拖慢真正的故障恢复)。
      let delayMs: number;
      if (this.skipNextBackoff) {
        this.skipNextBackoff = false;
        delayMs = 0;
        this.log("info", "skipping backoff for this reconnect (foreground-resume-triggered)");
      } else {
        delayMs = this.backoffTracker.recordFailure();
      }
      this.setPhase("reconnect_scheduled");
      this.connectionPhaseKind = "reconnect";
      await this.waitBackoffOrForegroundInterrupt(delayMs);
    }
  }

  /**
   * **registry_ready 前静默期语义(M2 C1 spec §3 连接项)**:桌面完成 `sync.ack` 之前,relay 不会
   * 发 `replay.head`/presence online/派发 pending input——一条刚 open 的远端连接可能安静好一阵子,
   * 这不是失败信号。本模块**刻意不实现**"open 后 N 秒内没收到任何帧就当连接失败"这类看门狗
   * (那会在真实的静默期里错误触发重连风暴)。沉默期结束后的第一条帧(不管是不是
   * `replay.head`——那由 T6d1 处理)照常经 `onmessage` 走 `handleMessage()`。
   */
  private classifyOutcome(outcome: { everOpened: boolean; closeReason: string }): UpgradeFailureClassification {
    if (outcome.everOpened) {
      const byReason = classifyCloseReason(outcome.closeReason);
      if (byReason !== "unclassified") return byReason;
    }
    return classifyUpgradeFailure({
      nowMs: this.now(),
      accessIssuedAtMs: this.credentials.accessIssuedAtMs,
      accessLifetimeMs: this.accessLifetimeMs,
      refreshUntilWindowMs: this.refreshUntilWindowMs,
      connectionPhase: this.connectionPhaseKind,
    });
  }

  private attemptOneConnection(): Promise<{ everOpened: boolean; closeReason: string }> {
    return new Promise((resolve) => {
      const proceed = (lastSeq: number): void => {
        if (this.stopped) {
          resolve({ everOpened: false, closeReason: "" });
          return;
        }
        const url = this.buildConnectUrl(lastSeq);
        const socket = this.deps.webSocketFactory(url, [SUBPROTOCOL_RC_V1, `token.${this.credentials.access}`]);
        this.socket = socket;
        let everOpened = false;

        socket.onopen = () => {
          everOpened = true;
          this.backoffTracker.reset();
          this.setPhase("open");
          this.log("info", "connection open");
          this.callbacks.onOpen?.();
          this.onConnectionOpened();
        };
        socket.onmessage = (event) => this.handleMessage(event.data);
        socket.onerror = () => {
          this.log("warn", "websocket error (close event will follow)");
        };
        socket.onclose = (info) => {
          this.socket = null;
          // FIX2 P0-1 第三环:这一轮 open 已经结束——挂着的 watchdog(若还没触发)属于这条已经死掉
          // 的连接,不该在下一轮重连后继续用旧连接的语境触发(下一轮 open 会自己重新武装一个)。
          this.disarmReplayHeadWatchdog();
          this.log("info", "connection closed", { code: info.code, reason: info.reason });
          this.callbacks.onClose?.(info);
          resolve({ everOpened, closeReason: info.reason });
        };
      };
      // msgfix2 U4（实锤 bug 修复）：`deps.getLastSeq` 生产装配点是 `eventStore.getWatermark()`
      // （`app/AppRuntime.tsx`）——IndexedDB 探测/打开失败时这个 Promise 会 reject。改之前这里只有
      // `.then(onFulfilled)`一个分支：一旦 reject，下面这条链**永远不会**调用 `resolve()`——这个
      // `attemptOneConnection()` 返回的 Promise 就此悬空，WebSocket 从未被创建，重连循环卡死在这
      // 一步，表现为"IndexedDB 一坏，连接永远建不起来"（fallback adapter 的反面教材：本该是"缓存
      // 不可用就退化"，实际是"缓存不可用就把不相关的传输层也拖死"）。降级契约：拿不到本地水位就
      // 当 0（`?? 0` 那半已有的兜底同款语义——`getLastSeq` 缺省时也是 0），继续建连，不让一次纯
      // 本地存储失败阻塞 WebSocket 建立。**用 `.then(onFulfilled, onRejected)` 单次双分支,不是
      // `.catch().then()` 两段链**——`connectionSession.test.ts` 里几十个既有测试的 `started()`
      // 助手 `await session.start()` 之后立即同步读 `wsFactory.last`，隐含假设"socket 创建落在一个
      // 固定的微任务跳数以内"；额外插一次 `.catch()` 会在（生产环境同样存在、`getLastSeq` 正常
      // resolve 的）主路径上多绕一跳微任务，把 socket 创建推到那些既有断言检查的时间点之后，
      // 实测让 50 个既有测试假报"no socket created yet"——不是"该改测试去多等一拍"，是本次修复
      // 必须保持跟原代码同样的微任务跳数,行为等价(只多出"reject 时不再永久悬空"这一条新增能力)。
      Promise.resolve(this.deps.getLastSeq?.() ?? 0).then(proceed, (error) => {
        this.log("warn", "getLastSeq() rejected — falling back to last_seq=0 (IndexedDB probe/open failure must not block WebSocket setup)", {
          error: String(error),
        });
        proceed(0);
      });
    });
  }

  private buildConnectUrl(lastSeq: number): string {
    const base = this.credentials.relayUrl.replace(/\/+$/, "");
    return `${base}/room/${this.credentials.room}?last_seq=${lastSeq}`;
  }

  private async waitBackoffOrForegroundInterrupt(delayMs: number): Promise<void> {
    await new Promise<void>((resolve) => {
      let settled = false;
      const finish = () => {
        if (settled) return;
        settled = true;
        resolve();
      };
      const handle = this.deps.scheduleTimer
        ? this.deps.scheduleTimer(finish, delayMs)
        : setTimeout(finish, delayMs);
      const clear = () => (this.deps.clearTimer ? this.deps.clearTimer(handle) : clearTimeout(handle as ReturnType<typeof setTimeout>));
      const unsubscribeForeground = (this.deps.foregroundResume ?? neverResumingForegroundPort()).onResume(() => {
        this.log("info", "foreground resume interrupted backoff wait — reconnecting immediately");
        clear();
        finish();
      });
      this.stopWaiters.push(() => {
        clear();
        finish();
      });
      // 一旦 settle,取消订阅(防止延迟到达的前台事件在下一轮循环里意外触发一次多余的 finish——
      // finish() 本身有 settled 闸,这里额外解绑是卫生习惯,不是必需的正确性条件)。
      void Promise.resolve().then(() => {
        if (settled) unsubscribeForeground();
      });
    });
  }

  // -------------------------------------------------------------------------
  // 前台恢复(M2 C1 spec §3 v0.5 块)
  // -------------------------------------------------------------------------

  /**
   * 由 `deps.foregroundResume` 在 open 态时也可能触发——回前台把当前 socket 视为可疑,直接重连。
   * 审查返工:置位 `skipNextBackoff`,让 `runLoop()` 里紧接着这次关闭之后的那一轮重连**零退避**
   * 立即发起(这是用户主动带回前台触发的自愿重连,不是一次失败,不该背正常故障的等待时间)。
   */
  private setupForegroundResumeWhileOpen(): void {
    (this.deps.foregroundResume ?? neverResumingForegroundPort()).onResume(() => {
      if (this.phaseValue !== "open" || !this.socket) return;
      this.log("info", "foreground resume while open — treating current socket as suspect, forcing reconnect");
      this.skipNextBackoff = true;
      this.socket.close(4900, "foreground_resume_reconnect");
    });
  }

  // -------------------------------------------------------------------------
  // 帧分发
  // -------------------------------------------------------------------------

  private onConnectionOpened(): void {
    this.setupForegroundResumeWhileOpen();
    // FIX2 P0-1 第三环:`pendingRefreshRequestId`/`forceRefreshOnNextOpen` 两支已经意味着"这一轮
    // open 会立刻主动折腾一次 refresh"——那条机制自己有重发/退避手段应对"回执迟迟不来",不需要另一
    // 条独立的静默 watchdog 再兜一层(徒增一个必然是无害空转的定时器)。只有落到"什么都不做,纯粹在
    // 等 replay.head"这条路径(`maybeProactiveRefreshOrArmWatchdog()` 判定本地钟还不到主动刷新阈值)时,才是
    // watchdog 真正要防的"静默降级连接"场景——见 `maybeProactiveRefreshOrArmWatchdog()`。
    if (this.pendingRefreshRequestId) {
      this.log("info", "resending in-flight token.refresh after (re)connect", { requestId: this.pendingRefreshRequestId });
      this.sendRefreshRequest(this.pendingRefreshRequestId);
      return;
    }
    if (this.forceRefreshOnNextOpen) {
      this.forceRefreshOnNextOpen = false;
      void this.beginRefresh();
      return;
    }
    void this.maybeProactiveRefreshOrArmWatchdog();
  }

  /**
   * FIX2 P0-1 第三环:只在"open 后本地钟还不到主动 refresh 阈值、这一轮什么都不主动做,纯粹等
   * `replay.head`"这条路径上武装——见 `maybeProactiveRefreshOrArmWatchdog()`/`ConnectionSessionDeps.
   * replayHeadWatchdogMs` 注释。旧的挂表先清一遍(理论上到这里不该已经挂着一个,但幂等无害)。
   */
  private armReplayHeadWatchdog(): void {
    this.disarmReplayHeadWatchdog();
    this.replayHeadWatchdogHandle = this.deps.scheduleTimer
      ? this.deps.scheduleTimer(() => this.onReplayHeadWatchdogFired(), this.replayHeadWatchdogMs)
      : setTimeout(() => this.onReplayHeadWatchdogFired(), this.replayHeadWatchdogMs);
  }

  /** 收到 `replay.head`(连接健康的证明)或连接关闭/会话停止时撤防——不需要再等这个信号了。 */
  private disarmReplayHeadWatchdog(): void {
    if (this.replayHeadWatchdogHandle === null) return;
    if (this.deps.clearTimer) {
      this.deps.clearTimer(this.replayHeadWatchdogHandle);
    } else {
      clearTimeout(this.replayHeadWatchdogHandle as ReturnType<typeof setTimeout>);
    }
    this.replayHeadWatchdogHandle = null;
  }

  /**
   * 到点仍未收到 `replay.head`——单独的静默本身不是问题(见 `classifyOutcome` 头注的 registry_ready
   * 前静默期语义,那条不变量这里没有被打破:本方法从不因为"到点了"就重连/报错)。只有再叠加"本地钟
   * 判定 access 已经过了名义寿命"这第二个独立信号,才值得赌一把主动 `beginRefresh()`——
   * `beginRefresh()` 自身有 `refreshInFlight` 重入闸,这里即便与 `maybeProactiveRefreshOrArmWatchdog()`/
   * `forceRefreshOnNextOpen` 撞了车也不会重复发起。
   */
  private onReplayHeadWatchdogFired(): void {
    this.replayHeadWatchdogHandle = null;
    if (this.stopped || this.phaseValue !== "open") return;
    const elapsed = this.now() - this.credentials.accessIssuedAtMs;
    if (elapsed < this.accessLifetimeMs) return;
    this.log("warn", "no replay.head received within the watchdog window and the local clock judges access already past its nominal lifetime — proactively refreshing (the only observable signal of a silent refresh-scope-downgraded connection)", {
      elapsedMs: elapsed,
    });
    void this.beginRefresh();
  }

  private handleMessage(raw: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      this.log("warn", "received non-JSON frame; ignoring");
      return;
    }
    if (!isRecord(parsed) || typeof parsed.t !== "string") {
      this.callbacks.onFrame?.(parsed);
      return;
    }
    switch (parsed.t) {
      case "epoch.changed": {
        const epoch = typeof parsed.epoch === "number" ? parsed.epoch : null;
        const ts = typeof parsed.ts === "number" ? parsed.ts : this.now();
        if (epoch === null) {
          this.log("warn", "malformed epoch.changed frame; ignoring");
          return;
        }
        this.callbacks.onEpochChanged?.(epoch, ts);
        return;
      }
      case "error": {
        const reason = typeof parsed.reason === "string" ? parsed.reason : "unknown";
        if (reason === "device_revoked") {
          // M0 §9.6 撤销分流:relay 的明文提示不可信——只记日志/转发给上层做 UI 提示,不在这里
          // 直接清凭据。真正的"清凭据→重新配对"只在 refresh 路径认证性地走死后触发(§3.7 终态
          // 闭环),见 `handleRefreshFail`/`classifyUpgradeFailure` 的 needs_repair 分支。
          this.log("warn", "received device_revoked hint (relay claim — not authenticated, not acted on directly)");
        }
        if (reason === "token_refresh_rate_limited" || reason === "token_refresh_resend_rate_limited") {
          // FIX2 P2-6:relay per-socket 限速拒绝(`room-do.js::takeRefreshRequestSlot`/
          // `takeRefreshResendSlot`)——受理阶段之前就被挡下,这条帧不带 `request_id`。
          this.handleRefreshRateLimitedError(reason);
        }
        this.callbacks.onFrame?.(parsed);
        return;
      }
      case "replay.head": {
        // 审查返工(前台恢复分支②):专属挂点——真正"给每个 running 会话发 control.snapshot
        // 请求"归接线层(T6f2/T6f3),本模块只负责可靠地把 {epoch, headSeq} 转发出去,并且仍然照
        // 常经通用 onFrame 再转发一次给 events 层(T6d1 自己也要用 replay.head 做补发合并)。
        const epoch = typeof parsed.epoch === "number" ? parsed.epoch : null;
        const headSeq = typeof parsed.headSeq === "number" ? parsed.headSeq : null;
        if (epoch === null || headSeq === null) {
          this.log("warn", "malformed replay.head frame; ignoring");
          return;
        }
        // FIX2 P0-1 第三环:这条帧本身就是"连接没有被静默降级"的证明——撤防 watchdog,不需要它了。
        this.disarmReplayHeadWatchdog();
        this.callbacks.onReplayHead?.(epoch, headSeq);
        this.callbacks.onFrame?.(parsed);
        return;
      }
      case "token.refresh.ok":
      case "token.refresh.fail": {
        const responseParsed = parseRefreshResponseFrame(parsed);
        if (responseParsed.kind === "ok") {
          void this.handleRefreshOk(responseParsed.frame);
        } else if (responseParsed.kind === "fail") {
          this.handleRefreshFail(responseParsed.frame);
        } else {
          this.log("warn", "malformed token.refresh.ok/fail frame; ignoring");
        }
        return;
      }
      default:
        this.callbacks.onFrame?.(parsed);
    }
  }

  // -------------------------------------------------------------------------
  // refresh 轮换(M0 §9.6)
  // -------------------------------------------------------------------------

  /**
   * FIX2 P0-1 第三环(改名自 `maybeProactiveRefresh`):本地钟在 open 这一刻已经过了 0.8 倍寿命阈值
   * ——照旧立即主动 refresh。否则(本地钟认为此刻还新鲜)——武装 replay.head watchdog,把"会不会
   * 静默被降级"这件事交给它在 `replayHeadWatchdogMs` 之后再判一次(见该方法与
   * `ConnectionSessionDeps.replayHeadWatchdogMs` 注释)。
   */
  private async maybeProactiveRefreshOrArmWatchdog(): Promise<void> {
    const elapsed = this.now() - this.credentials.accessIssuedAtMs;
    if (elapsed >= this.accessLifetimeMs * this.proactiveRefreshRatio) {
      this.log("info", "proactively refreshing before access nears expiry", { elapsedMs: elapsed });
      await this.beginRefresh();
      return;
    }
    this.armReplayHeadWatchdog();
  }

  /**
   * 发起一轮新的 refresh(不会在已有飞行中的 refresh 上重复发起)。
   *
   * **审查返工(fail-closed 落盘顺序)**:`pendingRefreshRequestId`/`refreshSentAtMs` 现在只在
   * `keyStore.savePendingRefresh()` **成功返回之后**才写入——旧实现先设内存态、再落盘,落盘失败时
   * 内存已经留下一份"volatile pending"(从未真正持久化,却已经在内存里表现得像是"飞行中"),重载
   * 或另一个标签页接手时会看到一个从未真正被记录过的 request_id,或者更糟——`onConnectionOpened()`
   * 照常把它当真发出去,而它其实从未通过持久层的崩溃安全检查点。落盘失败时直接放弃这次尝试,不
   * 发送任何东西,让下一次自然触发(下次 open / 下次主动检查)重新走一遍完整流程。
   */
  private async beginRefresh(): Promise<void> {
    if (this.refreshInFlight) {
      this.log("debug", "beginRefresh() called while a refresh is already in flight; ignoring");
      return;
    }
    const requestId = this.deps.requestIdFactory ? this.deps.requestIdFactory() : globalThis.crypto.randomUUID();
    if (!isValidRequestId(requestId)) {
      // fail-closed:生成侧校验——只可能在注入了自定义 requestIdFactory 时触发(默认的
      // crypto.randomUUID() 恒合法),防止一个畸形 id 被落盘/发送出去。
      this.log("error", "generated request_id fails validation — aborting refresh (fail-closed)", {
        requestId: String(requestId),
      });
      return;
    }
    this.refreshInFlight = true;
    const sentAtMs = this.now();
    try {
      // M0 §9.5/§3.7 落盘契约第②条:首次发送前原子落盘,早于任何网络往返、早于把 requestId 记进
      // 内存态"飞行中"字段。
      await this.keyStore.savePendingRefresh({ requestId, generation: this.localGeneration, sentAtMs });
    } catch (error) {
      this.log("error", "failed to persist pending_refresh — aborting this refresh attempt without sending (fail-closed)", {
        error: String(error),
      });
      this.refreshInFlight = false;
      return;
    }
    // 落盘成功之后才允许把这次尝试标记为"飞行中"、才允许真正发送。
    this.pendingRefreshRequestId = requestId;
    this.refreshSentAtMs = sentAtMs;
    this.resumedPendingRefresh = false; // 本会话内新发起的一轮,不是从 KeyStore 恢复的。
    const lockName = refreshLockName(this.credentials.room, this.credentials.deviceId);
    void withRefreshSingleFlight(this.deps.locks, lockName, () => this.runRefreshRoundTrip(requestId))
      .then((result) => {
        if (result.viaFallback) {
          this.log("debug", "Web Locks API unavailable — refresh single-flight not cross-tab guaranteed (single-tab passthrough)");
        }
      })
      .catch((error) => this.log("error", "refresh round trip failed unexpectedly", { error: String(error) }));
  }

  /** 持有跨标签单飞行锁直到这一整轮 refresh(可能跨越若干次重连/重发)彻底结束。 */
  private runRefreshRoundTrip(requestId: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.refreshDeferred = { resolve, reject };
      if (this.phaseValue === "open" && this.socket) {
        this.sendRefreshRequest(requestId);
      }
      // socket 未 open 时不在这里发送——`onConnectionOpened()` 会在下一次 open 时看到
      // `pendingRefreshRequestId` 非空并补发,不需要在这里轮询/等待。
    });
  }

  private finishRefreshAttempt(): void {
    this.refreshInFlight = false;
    this.refreshDeferred?.resolve();
    this.refreshDeferred = null;
  }

  private sendRefreshRequest(requestId: string): void {
    if (!this.socket || this.socket.readyState !== ReadyState.OPEN) return;
    void sealRefreshRequestBody(
      this.credentials.kPair,
      this.credentials.room,
      this.credentials.deviceId,
      requestId,
      this.credentials.refresh,
    ).then(({ ct, n }) => {
      // 竞态防护:密封是异步的,期间连接可能已经关闭或这一轮 refresh 已经被别的路径结束。
      if (this.stopped || this.pendingRefreshRequestId !== requestId || !this.socket || this.socket.readyState !== ReadyState.OPEN) {
        return;
      }
      this.socket.send(JSON.stringify(buildTokenRefreshFrame(requestId, ct, n)));
    });
  }

  /**
   * **审查返工(凭据落盘 fail-closed + 保守二次轮换)**:
   * - `saveKeys()` 失败时**保留旧凭据与 pending**、不更新内存态、不触发 `onCredentialsRotated`——
   *   桌面已经把这份回执缓存在它自己的 journal 里(命中 prev 哈希会原样重放同一份回执,见 M0
   *   §9.6),所以安全的做法是退避后用**同一个** request_id 重试,等桌面重放同一份密文,给自己
   *   再一次持久化的机会,而不是假装这轮成功了。
   * - `resumedPendingRefresh`(跨页面重载恢复的 pending)一律按"保守立即二次轮换"处理,不管往返
   *   耗时测出来是不是"看起来新鲜"——重载后我们对自己发送时刻的记录、桌面那边具体处理到哪一步都
   *   不再有完整把握,这种不确定性下故意选保守的那一侧(多轮一次也没坏处,journal 幂等)。
   */
  private async handleRefreshOk(frame: TokenRefreshOkFrame): Promise<void> {
    if (frame.request_id !== this.pendingRefreshRequestId) {
      this.log("debug", "ignoring token.refresh.ok for an unrelated request_id");
      return;
    }
    let rotated: { capabilityToken: string; refreshToken: string };
    try {
      rotated = await openRefreshOkBody(
        this.credentials.kPair,
        this.credentials.room,
        this.credentials.deviceId,
        frame.request_id,
        frame.ct,
        frame.n,
      );
    } catch (error) {
      this.log("warn", "failed to decrypt token.refresh.ok", { error: String(error) });
      this.pendingRefreshRequestId = null;
      this.resumedPendingRefresh = false;
      this.finishRefreshAttempt();
      return;
    }

    const nowMs = this.now();
    const roundTripMs = nowMs - (this.refreshSentAtMs ?? nowMs);
    const wasResumed = this.resumedPendingRefresh;

    let existing: StoredPairingCredentials | null;
    try {
      existing = await this.keyStore.loadKeys();
    } catch (error) {
      this.log("error", "failed to load existing credentials while persisting token.refresh.ok — keeping OLD credentials & pending, retrying same request_id (fail-closed)", {
        error: String(error),
      });
      this.scheduleRefreshRetry(frame.request_id);
      return;
    }
    if (!existing) {
      this.log("error", "token.refresh.ok succeeded but no base credential record exists to merge into — retrying same request_id (fail-closed)");
      this.scheduleRefreshRetry(frame.request_id);
      return;
    }
    try {
      await this.keyStore.saveKeys({
        ...existing,
        access: rotated.capabilityToken,
        refresh: rotated.refreshToken,
        accessIssuedAtMs: nowMs,
        pendingRefresh: null,
      });
    } catch (error) {
      this.log("error", "failed to persist rotated credentials — keeping OLD credentials & pending_refresh, retrying same request_id (fail-closed)", {
        error: String(error),
      });
      this.scheduleRefreshRetry(frame.request_id);
      return;
    }

    // 落盘成功之后才更新内存态、才触发回调——这是"新令牌"第一次被这个会话真正采纳的时刻。
    this.credentials.access = rotated.capabilityToken;
    this.credentials.refresh = rotated.refreshToken;
    this.credentials.accessIssuedAtMs = nowMs;
    this.localGeneration = frame.generation;
    this.pendingRefreshRequestId = null;
    this.resumedPendingRefresh = false;
    this.finishRefreshAttempt();
    this.callbacks.onCredentialsRotated?.({
      access: rotated.capabilityToken,
      refresh: rotated.refreshToken,
      accessIssuedAtMs: nowMs,
    });

    // §3.7 第③条:过期重放二次轮换——往返耗时长到"新"凭据大概率其实是桌面 journal 缓存的旧回执,
    // 或者这条 pending 本来就是跨页面重载恢复的(对它的"往返耗时"完全不该信任,见上方方法注释)
    // ——两种情况都立即用刚拿到的新 refresh_token 再轮一次,不等下一次自然触发。
    if (wasResumed || roundTripMs > this.staleReplayThresholdMs) {
      this.log("info", "token.refresh.ok round trip looked like a stale replay (or was a reload-resumed pending); immediately rotating again", {
        roundTripMs,
        wasResumed,
      });
      await this.beginRefresh();
    }
  }

  private handleRefreshFail(frame: TokenRefreshFailFrame): void {
    if (frame.request_id !== this.pendingRefreshRequestId) {
      this.log("debug", "ignoring token.refresh.fail for an unrelated request_id");
      return;
    }

    if (frame.close) {
      // **认证终态(唯一权威信号)**:桌面判连续 ≥3 次无效(`record_refresh_invalid` 真数出来的,
      // 不是猜的)——立即终态,不等下一次 upgrade 失败才通过 classifyUpgradeFailure 推断。
      this.pendingRefreshRequestId = null;
      this.resumedPendingRefresh = false;
      void this.transitionToNeedsRepair(`refresh failed with close=true (reason=${frame.reason})`);
      return;
    }

    if (frame.reason === "in_flight" || frame.reason === "put_rejected" || frame.reason === "invalid") {
      // 审查返工(invalid 分类保守化,依据=lib.rs:901-929):三者统一按良性、非终态处理——
      // in_flight = 客户端单飞行保证之外仍撞车的边角情形;put_rejected = 桌面自愈信号("让手机凭旧
      // refresh 立刻重试",remote_gateway.rs 原话);**普通 invalid(不带 close)不是认证终态**——
      // 桌面自身的 registry 锁中毒/DB 不可用/DB 连接锁中毒这类内部基建故障,也会原样发
      // `reason:"invalid", close:false`(见 upgradeClassifier.ts 顶注引用的 lib.rs 证据),跟"你的
      // refresh_token 到底对不对"毫无关系。三者都不清 pendingRefresh、不断连,退避一小段后用
      // **同一个** request_id 重发。
      this.scheduleRefreshRetry(frame.request_id);
      return;
    }

    // 其余(如 rate_limited)——这次尝试结束,不立即重试(立即重试只会再次撞限速),等待下一次
    // 自然触发(下次 open / 下次主动检查)。
    this.pendingRefreshRequestId = null;
    this.resumedPendingRefresh = false;
    this.finishRefreshAttempt();
  }

  /**
   * FIX2 P2-6(refresh 面限速消费):`{t:"error", reason:"token_refresh_rate_limited"|
   * "token_refresh_resend_rate_limited"}`——relay per-socket 限速拒绝(`room-do.js::
   * takeRefreshRequestSlot`/`takeRefreshResendSlot`),受理阶段之前就被挡下,帧本身不带
   * `request_id`(desktop 从没见过这次尝试,没有可关联的痕迹)。relay 紧接着会用
   * `REAUTH_CLOSE_REASON`(`closeSocketForReauthorization`)关掉这条 socket——那个复用的关闭原因
   * 字符串本是给"needs_refresh"分类用的,但这不是认证失败,只是被节流:**不清 `pendingRefreshRequestId`
   * 、不清 KeyStore 里的 pending_refresh、不当认证失败处理**。释放 `refreshInFlight` 闸(同
   * `beginRefresh()` 落盘失败分支的既有先例——"下次仍可正常重试"),按 `refreshRetryDelayMs` 退避后
   * 用同一个 `request_id` 重发(同 `handleRefreshFail()` 良性组的既有手法)。没有正在飞行的 refresh
   * 时(`pendingRefreshRequestId` 为空)——这条帧与本机当前状态无关,忽略。
   */
  private handleRefreshRateLimitedError(reason: string): void {
    const requestId = this.pendingRefreshRequestId;
    if (!requestId) {
      this.log("debug", "ignoring refresh rate-limit error frame — no refresh currently in flight", { reason });
      return;
    }
    this.log("warn", "refresh request rate-limited by relay — retrying the same request_id after backoff (not an auth failure)", { reason });
    this.refreshInFlight = false;
    this.scheduleRefreshRetry(requestId);
  }

  private scheduleRefreshRetry(requestId: string): void {
    const handle = this.deps.scheduleTimer
      ? this.deps.scheduleTimer(() => this.retryRefreshIfStillPending(requestId), this.refreshRetryDelayMs)
      : setTimeout(() => this.retryRefreshIfStillPending(requestId), this.refreshRetryDelayMs);
    this.stopWaiters.push(() => (this.deps.clearTimer ? this.deps.clearTimer(handle) : clearTimeout(handle as ReturnType<typeof setTimeout>)));
  }

  private retryRefreshIfStillPending(requestId: string): void {
    if (this.stopped || this.pendingRefreshRequestId !== requestId) return;
    this.sendRefreshRequest(requestId);
  }

  // -------------------------------------------------------------------------
  // needs_repair 终态(M2 C1 spec §3 终态闭环)
  // -------------------------------------------------------------------------

  /**
   * **审查返工**:① 置 `this.stopped = true`(与 `stop()` 同款"不可逆终态"语义)——不这样做的话,
   * 从 `handleRefreshFail` 的 close:true 分支直接调用本方法时(此时连接很可能仍是 open 态,
   * `runLoop()` 还卡在 `await attemptOneConnection()` 上),下面主动关闭 socket 会让那个 await
   * resolve、`runLoop()` 继续往下走分类+重连,与"已经判定彻底走死"自相矛盾。② 认证终态时主动
   * 关掉可能还活着的连接——不能让一条用"已经判定作废"的凭据认证出来的连接继续存活。
   */
  private async transitionToNeedsRepair(reason: string): Promise<void> {
    if (this.phaseValue === "needs_repair" || this.stopped) return;
    this.stopped = true;
    this.setPhase("needs_repair");
    this.log("warn", "connection entering needs_repair — clearing long-term credentials", { reason });
    this.socket?.close(4901, "needs_repair");
    this.socket = null;
    for (const waiter of this.stopWaiters.splice(0)) waiter();
    try {
      await this.keyStore.clear();
    } catch (error) {
      this.log("error", "failed to clear key store while entering needs_repair", { error: String(error) });
    }
    this.pendingRefreshRequestId = null;
    this.resumedPendingRefresh = false;
    this.finishRefreshAttempt();
    this.callbacks.onNeedsRepair?.(reason);
  }
}
