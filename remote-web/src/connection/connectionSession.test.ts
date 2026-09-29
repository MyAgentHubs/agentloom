// connectionSession.test.ts — T6c-refresh · ConnectionSession 全分支状态表驱动测试。
//
// ============================================================================
// 分支覆盖表(任务书 §4③ 要求;逐条 M0 v1.8.4-1.8.7 + M2 C1 spec §3 v0.5 块对照,见文件内各
// describe 块标题引用的版本号)
// ============================================================================
// 见本文件末尾的覆盖表 describe 块列表汇总(每个分支至少一条测试,不堆砌相似用例)。

import { afterEach, describe, expect, it, vi } from "vitest";
import { ConnectionSession, type ConnectionSessionCallbacks, type ConnectionSessionDeps } from "./connectionSession.ts";
import { ReadyState, type ConnectionCredentials, type ForegroundResumePort, type LocksPort, type WebSocketCloseInfo, type WebSocketLike } from "./types.ts";
import { InMemoryKeyStore, importNonExtractableAesGcmKey, type KeyStorePort, type StoredPairingCredentials } from "../store/key-store.ts";
import { refreshOkMeta } from "./refreshFrames.ts";
import { seal } from "../crypto/envelope.ts";
import { utf8Bytes } from "../crypto/bytes.ts";

// ---------------------------------------------------------------------------
// 测试基础设施:假 WebSocket / 假 locks / 手动调度器 / 凭据与 KeyStore 种子
// ---------------------------------------------------------------------------

class FakeSocket implements WebSocketLike {
  readyState: number = ReadyState.CONNECTING;
  onopen: (() => void) | null = null;
  onclose: ((event: WebSocketCloseInfo) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  sent: string[] = [];
  closeCalls: Array<{ code?: number; reason?: string }> = [];

  constructor(
    public readonly url: string,
    public readonly protocols: string[],
  ) {}

  send(data: string): void {
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    this.closeCalls.push({ code, reason });
    if (this.readyState === ReadyState.CLOSED) return;
    this.readyState = ReadyState.CLOSING;
    // 真实 WebSocket.close() 最终总会异步触发一次 close 事件(WHATWG 关闭握手)——本假实现照做,
    // 避免测试里"调用 close() 之后 attemptOneConnection() 的 promise 永远不 resolve"这种失真。
    // 服务端发起的关闭(帧/掉线)仍用 `simulateClose`/`simulateUpgradeFailure` 单独驱动。
    queueMicrotask(() => {
      this.readyState = ReadyState.CLOSED;
      this.onclose?.({ code: code ?? 1000, reason: reason ?? "", wasClean: (code ?? 1000) === 1000 });
    });
  }

  /** 测试驱动:真正把 socket 推进到 open 态并触发 onopen。 */
  simulateOpen(): void {
    this.readyState = ReadyState.OPEN;
    this.onopen?.();
  }

  simulateMessage(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }

  /** 已 open 过的连接被(服务端或本地 close() 调用)关闭。 */
  simulateClose(code: number, reason: string): void {
    this.readyState = ReadyState.CLOSED;
    this.onclose?.({ code, reason, wasClean: code === 1000 });
  }

  /** 从未 open 过就失败——浏览器真实行为:没有状态码,close 事件 code=1006/reason="" 这类通用值。 */
  simulateUpgradeFailure(): void {
    this.readyState = ReadyState.CLOSED;
    this.onerror?.();
    this.onclose?.({ code: 1006, reason: "", wasClean: false });
  }
}

class FakeWebSocketFactory {
  sockets: FakeSocket[] = [];
  factory = (url: string, protocols: string[]): WebSocketLike => {
    const socket = new FakeSocket(url, protocols);
    this.sockets.push(socket);
    return socket;
  };
  get last(): FakeSocket {
    const socket = this.sockets.at(-1);
    if (!socket) throw new Error("no socket created yet");
    return socket;
  }
}

/** 手动调度器——测试自己决定"时间前进",不依赖真实 setTimeout。 */
class ManualScheduler {
  private nextId = 1;
  tasks: Array<{ id: number; callback: () => void; delayMs: number }> = [];

  schedule = (callback: () => void, delayMs: number): unknown => {
    const id = this.nextId++;
    this.tasks.push({ id, callback, delayMs });
    return id;
  };

  clear = (handle: unknown): void => {
    this.tasks = this.tasks.filter((task) => task.id !== handle);
  };

  /** 触发最早入队的一个任务(不管 delayMs——测试只关心"这个任务最终会被触发一次")。 */
  runNext(): void {
    const task = this.tasks.shift();
    task?.callback();
  }

  get pendingCount(): number {
    return this.tasks.length;
  }
}

/** 立即批准的假 LocksPort——单一虚拟标签页,记录每次 `request` 的 name/options 供断言。 */
function recordingGrantingLocks(): LocksPort & { calls: Array<{ name: string; options: unknown }> } {
  const calls: Array<{ name: string; options: unknown }> = [];
  return {
    calls,
    async request(name, options, callback) {
      calls.push({ name, options });
      return callback({});
    },
  };
}

/** 假装连接锁已被别的标签页占着(非阻塞探测立即拿到 null)。 */
function unavailableConnectLocks(): LocksPort {
  return {
    async request(_name, options, callback) {
      if (options.ifAvailable) return callback(null);
      return callback({});
    },
  };
}

/**
 * 审查返工新增:真正做互斥的假 LocksPort(不是"always grant"/"always deny"那种单标签快捷方式)——
 * 用于"双 session 共享同一把假锁"竞争测试,模拟同源两个真实标签页共享同一个 `navigator.locks`。
 * `ifAvailable:true` 探测式:锁被占用时立即拿 `null`;`mode:"exclusive"`(无 `ifAvailable`)走简单
 * 轮询等待(测试场景不需要公平排队,只需要正确的互斥语义)。
 */
class SharedFakeLocks implements LocksPort {
  calls: Array<{ name: string; options: { mode?: "exclusive" | "shared"; ifAvailable?: boolean } }> = [];

  /** `held` 可以在构造时注入同一个 Set 实例,让两个"标签页"各自的 `calls` 记录分开、但互斥状态
   * 真正共享——这样既能验证"锁确实互斥"又能验证"这次调用具体是谁发起的"。 */
  constructor(private readonly held: Set<string> = new Set()) {}

  async request<T>(
    name: string,
    options: { mode?: "exclusive" | "shared"; ifAvailable?: boolean },
    callback: (lock: unknown | null) => Promise<T>,
  ): Promise<T> {
    this.calls.push({ name, options });
    if (options.ifAvailable) {
      if (this.held.has(name)) return callback(null);
      this.held.add(name);
      try {
        return await callback({});
      } finally {
        this.held.delete(name);
      }
    }
    while (this.held.has(name)) {
      await new Promise((resolve) => setTimeout(resolve, 2));
    }
    this.held.add(name);
    try {
      return await callback({});
    } finally {
      this.held.delete(name);
    }
  }
}

/**
 * 审查返工新增:可控故障注入的 KeyStore 装饰器——包一层 `InMemoryKeyStore`,让测试能精确控制
 * `savePendingRefresh()`/`saveKeys()` 在指定时刻抛错,验证 fail-closed 行为(item 2)。
 */
class FaultInjectableKeyStore implements KeyStorePort {
  failSavePendingRefresh = false;
  failSaveKeys = false;

  constructor(private readonly inner: InMemoryKeyStore) {}

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    if (this.failSaveKeys) throw new Error("forced saveKeys() failure (test fault injection)");
    return this.inner.saveKeys(creds);
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    return this.inner.loadKeys();
  }

  async clear(): Promise<void> {
    return this.inner.clear();
  }

  async savePendingRefresh(pending: { requestId: string; generation?: number; sentAtMs?: number }): Promise<void> {
    if (this.failSavePendingRefresh) throw new Error("forced savePendingRefresh() failure (test fault injection)");
    return this.inner.savePendingRefresh(pending);
  }

  async loadPendingRefresh() {
    return this.inner.loadPendingRefresh();
  }
}

/** 手动可触发的前台恢复端口。 */
function manualForegroundResumePort(): ForegroundResumePort & { trigger: () => void } {
  let listener: (() => void) | null = null;
  return {
    onResume(callback) {
      listener = callback;
      return () => {
        if (listener === callback) listener = null;
      };
    },
    trigger() {
      listener?.();
    },
  };
}

const ROOM = "0123456789abcdef0123456789abcdef";
const DEVICE_ID = "11111111-1111-4111-8111-111111111111";
const K_PAIR = new Uint8Array(32).fill(4);
const ACCESS = "a".repeat(64);
const REFRESH = "b".repeat(64);

function makeCredentials(overrides: Partial<ConnectionCredentials> = {}): ConnectionCredentials {
  return {
    deviceId: DEVICE_ID,
    room: ROOM,
    relayUrl: "wss://relay.example",
    access: ACCESS,
    refresh: REFRESH,
    kPair: K_PAIR,
    accessIssuedAtMs: 0,
    ...overrides,
  };
}

async function seededKeyStore(overrides: Partial<StoredPairingCredentials> = {}): Promise<InMemoryKeyStore> {
  const store = new InMemoryKeyStore();
  const kRoomKey = await importNonExtractableAesGcmKey(new Uint8Array(32).fill(9));
  await store.saveKeys({
    deviceId: DEVICE_ID,
    room: ROOM,
    relayUrl: "wss://relay.example",
    access: ACCESS,
    refresh: REFRESH,
    kRoomKey,
    kPair: K_PAIR,
    accessIssuedAtMs: 0,
    ...overrides,
  });
  return store;
}

async function sealDesktopRefreshOk(requestId: string, capabilityToken: string, refreshToken: string): Promise<{ ct: string; n: string }> {
  return seal(
    K_PAIR,
    refreshOkMeta(ROOM, DEVICE_ID, requestId),
    utf8Bytes(JSON.stringify({ capability_token: capabilityToken, refresh_token: refreshToken })),
  );
}

async function waitUntil(predicate: () => boolean, timeoutMs = 2_000): Promise<void> {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) {
      throw new Error("waitUntil() timed out waiting for condition");
    }
    await new Promise((resolve) => setTimeout(resolve, 4));
  }
}

interface Harness {
  session: ConnectionSession;
  wsFactory: FakeWebSocketFactory;
  keyStore: InMemoryKeyStore;
  scheduler: ManualScheduler;
  foreground: ReturnType<typeof manualForegroundResumePort>;
  clock: { value: number };
  callbacks: {
    onOpen: ReturnType<typeof vi.fn>;
    onClose: ReturnType<typeof vi.fn>;
    onEpochChanged: ReturnType<typeof vi.fn>;
    onReplayHead: ReturnType<typeof vi.fn>;
    onFrame: ReturnType<typeof vi.fn>;
    onCredentialsRotated: ReturnType<typeof vi.fn>;
    onNeedsRepair: ReturnType<typeof vi.fn>;
    onPhaseChange: ReturnType<typeof vi.fn>;
  };
  logs: Array<{ level: string; message: string; context?: Record<string, unknown> }>;
}

async function makeHarness(
  options: {
    credentials?: Partial<ConnectionCredentials>;
    deps?: Partial<ConnectionSessionDeps>;
    keyStore?: InMemoryKeyStore;
    locks?: LocksPort;
  } = {},
): Promise<Harness> {
  const wsFactory = new FakeWebSocketFactory();
  const keyStore = options.keyStore ?? (await seededKeyStore());
  const scheduler = new ManualScheduler();
  const foreground = manualForegroundResumePort();
  const clock = { value: 0 };
  const logs: Array<{ level: string; message: string; context?: Record<string, unknown> }> = [];
  const callbacks: ConnectionSessionCallbacks = {
    onOpen: vi.fn(),
    onClose: vi.fn(),
    onEpochChanged: vi.fn(),
    onReplayHead: vi.fn(),
    onFrame: vi.fn(),
    onCredentialsRotated: vi.fn(),
    onNeedsRepair: vi.fn(),
    onPhaseChange: vi.fn(),
  };
  const session = new ConnectionSession(
    makeCredentials(options.credentials),
    {
      webSocketFactory: wsFactory.factory,
      keyStore,
      locks: options.locks,
      foregroundResume: foreground,
      log: (level, message, context) => logs.push({ level, message, context }),
      now: () => clock.value,
      scheduleTimer: scheduler.schedule,
      clearTimer: scheduler.clear,
      accessLifetimeMs: 1_000,
      refreshUntilWindowMs: 2_000,
      proactiveRefreshRatio: 0.8,
      refreshRetryDelayMs: 50,
      backoff: { baseMs: 100, capMs: 1_000, jitterRatio: 0 },
      ...options.deps,
    },
    callbacks,
  );
  return {
    session,
    wsFactory,
    keyStore,
    scheduler,
    foreground,
    clock,
    callbacks: callbacks as Harness["callbacks"],
    logs,
  };
}

const activeSessions: ConnectionSession[] = [];
afterEach(async () => {
  for (const session of activeSessions.splice(0)) {
    await session.stop();
  }
});

async function started(harness: Harness): Promise<Harness> {
  activeSessions.push(harness.session);
  await harness.session.start();
  return harness;
}

// ---------------------------------------------------------------------------
// 连接契约:双 offer / URL last_seq / registry_ready 静默期 / epoch.changed 挂点
// ---------------------------------------------------------------------------

describe("连接契约(M0 §9.1)", () => {
  it("offers both agentloom-rc-v1 and token.<access hex> subprotocols", async () => {
    const harness = await started(await makeHarness());
    expect(harness.wsFactory.last.protocols).toEqual(["agentloom-rc-v1", `token.${ACCESS}`]);
  });

  it("connects to /room/<room>?last_seq=<n> using the injected getLastSeq()", async () => {
    const harness = await started(
      await makeHarness({ deps: { getLastSeq: () => 42 } }),
    );
    expect(harness.wsFactory.last.url).toBe(`wss://relay.example/room/${ROOM}?last_seq=42`);
  });

  it("defaults last_seq to 0 when getLastSeq is not injected", async () => {
    const harness = await started(await makeHarness());
    expect(harness.wsFactory.last.url).toBe(`wss://relay.example/room/${ROOM}?last_seq=0`);
  });

  it("msgfix2 U4（实锤 bug 修复）：getLastSeq() 返回被 reject 的 Promise（IndexedDB 探测/打开失败的生产装配点 eventStore.getWatermark()）不阻塞 WebSocket 建立——降级 last_seq=0 继续连，不是永久悬空", async () => {
    const harness = await started(
      await makeHarness({ deps: { getLastSeq: () => Promise.reject(new Error("IndexedDB open failed")) } }),
    );
    // 改之前：`attemptOneConnection()` 内部只有 `.then(...)` 没有 `.catch(...)`，reject 之后
    // `resolve()` 永远不会被调用——`session.start()` 悬空，socket 从未被创建。改之后必须真的建立
    // 一条连接，且 last_seq 降级为 0（同"未注入 getLastSeq"的既有兜底同一个值）。
    expect(harness.wsFactory.sockets).toHaveLength(1);
    expect(harness.wsFactory.last.url).toBe(`wss://relay.example/room/${ROOM}?last_seq=0`);
    expect(harness.logs.some((entry) => entry.level === "warn" && entry.message.includes("getLastSeq() rejected"))).toBe(true);
  });

  it("phase transitions idle → connecting → open on successful handshake", async () => {
    const harness = await makeHarness();
    expect(harness.session.phase).toBe("idle");
    activeSessions.push(harness.session);
    const startPromise = harness.session.start();
    await waitUntil(() => harness.wsFactory.sockets.length > 0);
    expect(harness.session.phase).toBe("connecting");
    harness.wsFactory.last.simulateOpen();
    expect(harness.session.phase).toBe("open");
    await startPromise;
  });
});

describe("registry_ready 前静默期语义(M2 C1 spec §3 连接项)——open 后长时间无消息不是连接失败信号", () => {
  it("does not reconnect or emit any error just because no frames arrive after open (FIX2 P0-1 差量:唯一被安排的计时器是 replay.head watchdog,它只可能触发『主动 refresh』,从不触发重连/报错)", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    expect(harness.wsFactory.sockets).toHaveLength(1);
    // 本模块没有"N 秒内无消息即失败"的看门狗——没有任何计时器被安排用于"判连接失败"这个目的。
    // 唯一被安排的计时器是 FIX2 P0-1 新增的 replay.head watchdog(此处 accessIssuedAtMs 与 clock
    // 都是 0,本地钟还没到主动 refresh 阈值,`maybeProactiveRefreshOrArmWatchdog()` 走的是"武装
    // watchdog"分支,不是立即 refresh 分支——watchdog 本身到点后触发的也只会是"主动 refresh"，
    // 见下方专属 describe 块的详细覆盖,不是重连/报错)。
    expect(harness.scheduler.pendingCount).toBe(1);
    expect(harness.session.phase).toBe("open");
    expect(harness.wsFactory.sockets).toHaveLength(1); // 没有因为"沉默"而重连出第二条 socket

    // 触发这枚 watchdog——本地钟此刻仍未过 access 全寿命,不该发起 refresh(不发送任何东西)。
    harness.scheduler.runNext();
    expect(harness.wsFactory.last.sent).toHaveLength(0);
    expect(harness.session.phase).toBe("open"); // 仍未重连/未报错
  });
});

describe("FIX2 P0-1 第三环:open 后 N 秒未收到 replay.head 且本地钟判 access 已过寿命 → 主动 beginRefresh()(refresh-scope 降级连接的唯一可观测信号)", () => {
  it("watchdog 到点时本地钟判 access 已过全寿命——发起 beginRefresh()(送出 token.refresh)", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.wsFactory.last.simulateOpen();
    // open 那一刻 elapsed=0,不到 0.8 倍寿命阈值(800ms)——不会立即触发 refresh,只武装 watchdog。
    expect(harness.wsFactory.last.sent).toHaveLength(0);
    expect(harness.scheduler.pendingCount).toBe(1);

    // 本地钟拨过 access 全寿命(accessLifetimeMs=1000)——一直没收到 replay.head。
    harness.clock.value = 1_500;
    harness.scheduler.runNext(); // 唯一排队的计时器——watchdog。

    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const sent = JSON.parse(harness.wsFactory.last.sent[0]) as { t: string };
    expect(sent.t).toBe("token.refresh");
  });

  it("watchdog 到点但本地钟仍判 access 未过寿命——不发起 refresh", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.wsFactory.last.simulateOpen();
    harness.clock.value = 200; // 远低于 accessLifetimeMs=1000
    harness.scheduler.runNext();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(harness.wsFactory.last.sent).toHaveLength(0);
  });

  it("watchdog 触发前先收到 replay.head——定时器被取消,不会因为随后本地钟判过期而误发 refresh", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.wsFactory.last.simulateOpen();
    expect(harness.scheduler.pendingCount).toBe(1);
    harness.wsFactory.last.simulateMessage({ t: "replay.head", epoch: 1, headSeq: 0 });
    expect(harness.scheduler.pendingCount).toBe(0); // watchdog 被撤防

    harness.clock.value = 5_000; // 即便现在时钟远超寿命
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(harness.wsFactory.last.sent).toHaveLength(0); // 没有定时器可触发,自然不会调用
  });

  it("本地钟在 open 时已经过了主动 refresh 阈值——直接 beginRefresh(),不武装 watchdog(那条机制自己有重发手段,不需要叠加一条独立定时器)", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    // 唯一排队的是 refresh 失败重试才会用到的定时器——目前还没有(还没收到任何回执)。
    expect(harness.scheduler.pendingCount).toBe(0);
  });
});

describe("epoch.changed 挂点(转发给 events 层的回调,不在本模块处理)", () => {
  it("forwards {t:'epoch.changed', epoch, ts} via onEpochChanged and does not touch onFrame", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    harness.wsFactory.last.simulateMessage({ t: "epoch.changed", epoch: 7, ts: 12345 });
    expect(harness.callbacks.onEpochChanged).toHaveBeenCalledWith(7, 12345);
    expect(harness.callbacks.onFrame).not.toHaveBeenCalled();
  });

  it("any other frame type (e.g. presence) is forwarded via onFrame, unhandled by this module", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    harness.wsFactory.last.simulateMessage({ t: "presence", role: "desktop", event: "online" });
    expect(harness.callbacks.onFrame).toHaveBeenCalledWith({ t: "presence", role: "desktop", event: "online" });
    expect(harness.callbacks.onEpochChanged).not.toHaveBeenCalled();
  });
});

describe("replay.head 挂点(审查返工新增·前台恢复分支②:真发 control.snapshot 归接线层)", () => {
  it("fires onReplayHead(epoch, headSeq) AND still forwards the frame via onFrame (both callbacks serve different downstream consumers)", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    harness.wsFactory.last.simulateMessage({ t: "replay.head", epoch: 3, headSeq: 77 });
    expect(harness.callbacks.onReplayHead).toHaveBeenCalledWith(3, 77);
    expect(harness.callbacks.onFrame).toHaveBeenCalledWith({ t: "replay.head", epoch: 3, headSeq: 77 });
  });

  it("ignores a malformed replay.head frame (missing headSeq) without throwing or firing either callback", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    harness.wsFactory.last.simulateMessage({ t: "replay.head", epoch: 3 });
    expect(harness.callbacks.onReplayHead).not.toHaveBeenCalled();
    expect(harness.session.phase).toBe("open"); // 没有因为畸形帧崩溃/断连
  });
});

describe("device_revoked 提示(M0 §9.6 撤销分流)——不可信,只记录不直接清凭据", () => {
  it("logs and forwards the hint via onFrame, but does not clear credentials or enter needs_repair", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    harness.wsFactory.last.simulateMessage({ t: "error", reason: "device_revoked" });
    expect(harness.callbacks.onFrame).toHaveBeenCalledWith({ t: "error", reason: "device_revoked" });
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    expect(harness.session.phase).toBe("open");
    expect(await harness.keyStore.loadKeys()).not.toBeNull();
    expect(harness.logs.some((entry) => entry.message.includes("device_revoked"))).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// refresh 轮换全分支(M0 §9.6·v1.8.4-1.8.7)
// ---------------------------------------------------------------------------

describe("refresh 轮换 — 主动触发(M2 C1 spec §3 refresh 轮换项)", () => {
  it("proactively sends token.refresh when access is near its nominal lifetime on connect", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 }, deps: {} }));
    harness.clock.value = 0; // elapsed since issuance = 900ms >= 0.8*1000ms threshold
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const sent = JSON.parse(harness.wsFactory.last.sent[0]) as { t: string; request_id: string };
    expect(sent.t).toBe("token.refresh");
    expect(typeof sent.request_id).toBe("string");
  });

  it("does not send token.refresh when access was just issued (fresh)", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.clock.value = 0;
    harness.wsFactory.last.simulateOpen();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(harness.wsFactory.last.sent).toHaveLength(0);
  });

  it("persists pending_refresh to the KeyStore before the request is sent (M0 §9.5/§3.7 atomicity contract)", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const pending = await harness.keyStore.loadPendingRefresh();
    expect(pending).not.toBeNull();
    const sentFrame = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };
    expect(pending?.requestId).toBe(sentFrame.request_id);
  });
});

describe("refresh 轮换 — 成功回执(v1.8.5/9.5 落盘契约)", () => {
  it("token.refresh.ok: rotates credentials, persists them, clears pending_refresh, and fires onCredentialsRotated", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const sentFrame = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    const newAccess = "c".repeat(64);
    const newRefresh = "d".repeat(64);
    const { ct, n } = await sealDesktopRefreshOk(sentFrame.request_id, newAccess, newRefresh);
    harness.clock.value = 500; // 往返 500ms < staleReplayThresholdMs(=accessLifetimeMs=1000) — 不触发二次轮换
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: sentFrame.request_id, subject: `device:${DEVICE_ID}`, generation: 2, ct, n });
    await waitUntil(() => harness.callbacks.onCredentialsRotated.mock.calls.length > 0);

    expect(harness.callbacks.onCredentialsRotated).toHaveBeenCalledWith({ access: newAccess, refresh: newRefresh, accessIssuedAtMs: 500 });
    const stored = await harness.keyStore.loadKeys();
    expect(stored?.access).toBe(newAccess);
    expect(stored?.refresh).toBe(newRefresh);
    expect(stored?.pendingRefresh ?? null).toBeNull();
    expect(await harness.keyStore.loadPendingRefresh()).toBeNull();
    // 底层 kRoomKey 等既有字段必须原样保留(read-modify-write,不是"重建一份新记录丢字段")。
    expect(stored?.deviceId).toBe(DEVICE_ID);
    expect(stored?.kRoomKey).toBeDefined();
  });

  it("v1.8.12 过期重放二次轮换(§3.7 第③条): a suspiciously slow round trip immediately triggers a second rotation with the newly received refresh token", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const firstFrame = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    const staleAccess = "e".repeat(64);
    const staleRefresh = "f".repeat(64);
    const { ct, n } = await sealDesktopRefreshOk(firstFrame.request_id, staleAccess, staleRefresh);
    harness.clock.value = 5_000; // 往返 5000ms > staleReplayThresholdMs(1000ms) — 判定像过期重放
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: firstFrame.request_id, subject: `device:${DEVICE_ID}`, generation: 2, ct, n });

    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const secondFrame = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };
    expect(secondFrame.request_id).not.toBe(firstFrame.request_id); // 新一轮:新 request_id
    // 第二轮必须是用刚收到的 staleRefresh 密封的——用真 K_pair 从密文反解出来断言明文用的是新 refresh。
    const secondSealed = JSON.parse(harness.wsFactory.last.sent[1]) as { ct: string; n: string };
    const { open } = await import("../crypto/envelope.ts");
    const { refreshRequestMeta } = await import("./refreshFrames.ts");
    const plaintext = await open(K_PAIR, refreshRequestMeta(ROOM, DEVICE_ID, secondFrame.request_id), secondSealed.ct, secondSealed.n);
    expect(JSON.parse(new TextDecoder().decode(plaintext))).toEqual({ refresh_token: staleRefresh });
  });
});

describe("refresh 轮换 — 失败分支(v1.8.4-1.8.6)", () => {
  it("v1.8.4/9.6 in_flight: benign, no close — retries the SAME request_id after a short delay, does not enter needs_repair", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    harness.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: first.request_id, subject: `device:${DEVICE_ID}`, reason: "in_flight" });
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    expect(harness.session.phase).toBe("open");
    expect(harness.scheduler.pendingCount).toBeGreaterThan(0);

    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const retry = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };
    expect(retry.request_id).toBe(first.request_id); // 同一个 request_id,不是新的
  });

  it("put_rejected: same benign-retry treatment as in_flight (desktop self-heal signal, 'retry with old refresh')", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    harness.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: first.request_id, subject: `device:${DEVICE_ID}`, reason: "put_rejected" });
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const retry = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };
    expect(retry.request_id).toBe(first.request_id);
  });

  it("审查返工:invalid without close is now benign-retry (same group as in_flight/put_rejected) — desktop-internal DB/lock faults (lib.rs:901-929) also produce this shape and are not an auth signal", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    harness.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: first.request_id, subject: `device:${DEVICE_ID}`, reason: "invalid" });
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    expect(harness.session.phase).toBe("open");
    expect(await harness.keyStore.loadKeys()).not.toBeNull(); // 长期凭据还在——没有被清

    // 新行为:不清 pendingRefreshRequestId、安排同 request_id 的短延迟重试(不再是"记录后终止本轮")。
    expect(harness.scheduler.pendingCount).toBeGreaterThan(0);
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const retry = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };
    expect(retry.request_id).toBe(first.request_id);
  });

  it("invalid WITH close=true: authenticated dead-end — immediately enters needs_repair and clears the KeyStore", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    harness.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: first.request_id, subject: `device:${DEVICE_ID}`, reason: "invalid", close: true });
    await waitUntil(() => harness.callbacks.onNeedsRepair.mock.calls.length > 0);
    expect(harness.session.phase).toBe("needs_repair");
    expect(await harness.keyStore.loadKeys()).toBeNull();
  });

  it("rate_limited: recorded as a non-fatal outcome, no retry storm, no needs_repair", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    harness.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: first.request_id, subject: `device:${DEVICE_ID}`, reason: "rate_limited" });
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    expect(harness.scheduler.pendingCount).toBe(0); // 没有安排立即重试(不同于 in_flight/put_rejected)
  });
});

describe("FIX2 P2-6:refresh 面限速消费——token_refresh_rate_limited/token_refresh_resend_rate_limited 释放 refreshInFlight 闸 + 按退避重试(不清 pending、不判认证失败)", () => {
  it("token_refresh_rate_limited:refreshInFlight 闸被释放(不再永久锁死)、pendingRequestId 不清,退避后用同一个 request_id 重发", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { t: string; request_id: string };
    expect(first.t).toBe("token.refresh");

    harness.wsFactory.last.simulateMessage({ t: "error", reason: "token_refresh_rate_limited" });

    // 不当认证失败——没有立即清空/终态化。
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    // 退避后用同一个 request_id 重发——`refreshInFlight` 没有永久锁死这次重试。
    expect(harness.scheduler.pendingCount).toBeGreaterThan(0);
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const retry = JSON.parse(harness.wsFactory.last.sent[1]) as { t: string; request_id: string };
    expect(retry.t).toBe("token.refresh");
    expect(retry.request_id).toBe(first.request_id); // 同一个 request_id,不是新的。

    // 这一轮最终仍能正常轮换成功——闸没有被卡死,不是"表面重发、其实再也发不出第三次"。
    const { ct, n } = await sealDesktopRefreshOk(first.request_id, "c".repeat(64), "d".repeat(64));
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: first.request_id, subject: `device:${DEVICE_ID}`, generation: 1, ct, n });
    await waitUntil(() => harness.callbacks.onCredentialsRotated.mock.calls.length > 0);
  });

  it("token_refresh_resend_rate_limited:同款处理——不当认证失败,退避后重发", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    harness.wsFactory.last.simulateMessage({ t: "error", reason: "token_refresh_resend_rate_limited" });
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    expect(await harness.keyStore.loadKeys()).not.toBeNull(); // 长期凭据没有被清

    expect(harness.scheduler.pendingCount).toBeGreaterThan(0);
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const retry = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };
    expect(retry.request_id).toBe(first.request_id);
  });

  it("没有正在飞行的 refresh 时收到这条错误帧——与本机无关,忽略,不崩溃、不误发", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.wsFactory.last.simulateOpen();
    // accessIssuedAtMs=0——本地钟未到主动 refresh 阈值,此刻没有任何 refresh 在飞行。
    expect(harness.wsFactory.last.sent).toHaveLength(0);

    harness.wsFactory.last.simulateMessage({ t: "error", reason: "token_refresh_rate_limited" });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(harness.wsFactory.last.sent).toHaveLength(0);
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
  });
});

describe("refresh 轮换 — 重连场景(v1.8.7:CLOSING socket 丢帧靠重连重放收敛)", () => {
  it("resends the SAME pending request_id after a reconnect (no response ever arrived on the dropped socket)", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    // socket 掉线,从未收到回执(模拟 v1.8.7:回执落在 CLOSING socket 时被 relay 丢弃)。
    harness.wsFactory.last.simulateClose(1006, "");
    await waitUntil(() => harness.scheduler.pendingCount > 0); // 退避计时器已安排(异步链路)
    harness.scheduler.runNext(); // 触发重连

    await waitUntil(() => harness.wsFactory.sockets.length === 2);
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const resent = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };
    expect(resent.request_id).toBe(first.request_id); // 重连后复用同一个 request_id,不是新的
  });

  it("resumes a pending_refresh persisted by a *previous* ConnectionSession instance (simulates a page reload mid-flight)", async () => {
    const keyStore = await seededKeyStore();
    await keyStore.savePendingRefresh({ requestId: "resumed-request-id" });

    const harness = await started(await makeHarness({ keyStore, credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const resent = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };
    expect(resent.request_id).toBe("resumed-request-id");
  });
});

// ---------------------------------------------------------------------------
// 凭据落盘 fail-closed(审查返工 item 2)
// ---------------------------------------------------------------------------

describe("凭据落盘 fail-closed", () => {
  it("constructor fail-closed: a KeyStore lacking savePendingRefresh()/loadPendingRefresh() is rejected at construction time (no silent optional-chain degrade)", async () => {
    const bareKeyStore: KeyStorePort = {
      async saveKeys() {},
      async loadKeys() {
        return null;
      },
      async clear() {},
      // savePendingRefresh/loadPendingRefresh 故意不实现——模拟一个不支持 refresh 持久化的 KeyStore。
    };
    expect(
      () =>
        new ConnectionSession(makeCredentials(), {
          webSocketFactory: () => {
            throw new Error("should never be called");
          },
          keyStore: bareKeyStore,
        }),
    ).toThrow(/savePendingRefresh.*loadPendingRefresh/);
  });

  it("store 拒写①:savePendingRefresh() 写失败——不发送 token.refresh,refreshInFlight 复位,下次仍可正常重试", async () => {
    const inner = await seededKeyStore();
    const faulty = new FaultInjectableKeyStore(inner);
    faulty.failSavePendingRefresh = true;
    const harness = await started(
      await makeHarness({ keyStore: inner, deps: { keyStore: faulty }, credentials: { accessIssuedAtMs: -900 } }),
    );
    // FaultInjectableKeyStore 比直接用 InMemoryKeyStore 多一层 async 委托,首个 socket 出现前的
    // microtask 跳数比其它测试多——显式等一下,不能假设 started() 返回时 socket 已经存在。
    await waitUntil(() => harness.wsFactory.sockets.length > 0);
    harness.wsFactory.last.simulateOpen();
    // 落盘一直失败——等一小段真实时间,确认确实没有发送任何东西(不是"还没来得及发",是"永远不发")。
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(harness.wsFactory.last.sent).toHaveLength(0);
    expect(await inner.loadPendingRefresh()).toBeNull(); // 底层真实存储从未写入过(fail-closed)

    // 恢复正常落盘后,下一次触发(这里手动再触发一次 open 事件模拟重连)应该能正常发送——
    // 证明失败只是"这次尝试中止",不是"从此卡死"。
    faulty.failSavePendingRefresh = false;
    harness.wsFactory.last.simulateClose(1006, "");
    await waitUntil(() => harness.scheduler.pendingCount > 0);
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.sockets.length === 2);
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
  });

  it("store 拒写②:token.refresh.ok 到达后 saveKeys() 写失败——保留旧凭据与 pending,不更新内存/不触发 onCredentialsRotated,退避后重试同一个 request_id", async () => {
    const inner = await seededKeyStore();
    const faulty = new FaultInjectableKeyStore(inner);
    const harness = await started(
      await makeHarness({ keyStore: inner, deps: { keyStore: faulty }, credentials: { accessIssuedAtMs: -900 } }),
    );
    await waitUntil(() => harness.wsFactory.sockets.length > 0);
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    faulty.failSaveKeys = true;
    const newAccess = "c".repeat(64);
    const newRefresh = "d".repeat(64);
    const { ct, n } = await sealDesktopRefreshOk(first.request_id, newAccess, newRefresh);
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: first.request_id, subject: `device:${DEVICE_ID}`, generation: 2, ct, n });

    // 给失败路径一点真实时间跑完(loadKeys/saveKeys 都是 async)。
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(harness.callbacks.onCredentialsRotated).not.toHaveBeenCalled();
    const stillStored = await inner.loadKeys();
    expect(stillStored?.access).toBe(ACCESS); // 旧凭据原封不动
    expect(stillStored?.refresh).toBe(REFRESH);

    // 退避后应该用同一个 request_id 重试(desktop journal 会原样重放同一份回执,给我们再一次
    // 持久化的机会)。
    expect(harness.scheduler.pendingCount).toBeGreaterThan(0);
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const retry = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };
    expect(retry.request_id).toBe(first.request_id);

    // 恢复正常落盘后,重放这份回执应该能成功采纳。
    faulty.failSaveKeys = false;
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: first.request_id, subject: `device:${DEVICE_ID}`, generation: 2, ct, n });
    await waitUntil(() => harness.callbacks.onCredentialsRotated.mock.calls.length > 0);
    expect((await inner.loadKeys())?.access).toBe(newAccess);
  });
});

// ---------------------------------------------------------------------------
// 重载后过期重放(审查返工 item 5)
// ---------------------------------------------------------------------------

describe("重载后过期重放", () => {
  it("reload 后陈旧 ok:恢复的 pending 收到回执时按保守立即二次轮换处理,即便测得的往返耗时看起来'新鲜'(refreshSentAtMs ?? now 恒 0 的洞已堵死)", async () => {
    const keyStore = await seededKeyStore();
    // 模拟"很久以前发出去的请求"——sentAtMs 是一个远早于当前 clock 的时刻,但因为
    // staleReplayThresholdMs 默认=accessLifetimeMs=1000,而这里 now-sentAtMs 也的确会超过阈值,
    // 单靠往返耗时判断本来就应该二次轮换——这条用例反而要验证:即使我们把 sentAtMs 设成"看起来
    // 刚发出去"(往返耗时很短、按纯阈值比较不会二次轮换),`resumedPendingRefresh` 标记依然强制
    // 二次轮换,不依赖那把不可信的尺子。
    await keyStore.savePendingRefresh({ requestId: "reload-resumed-id", sentAtMs: 0 });

    const harness = await started(await makeHarness({ keyStore, credentials: { accessIssuedAtMs: -900 } }));
    harness.clock.value = 0; // now - sentAtMs(=0) = 0ms,往返耗时"看起来"完全新鲜,远低于阈值(1000ms)
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };
    expect(first.request_id).toBe("reload-resumed-id");

    const staleAccess = "e".repeat(64);
    const staleRefresh = "f".repeat(64);
    const { ct, n } = await sealDesktopRefreshOk(first.request_id, staleAccess, staleRefresh);
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: first.request_id, subject: `device:${DEVICE_ID}`, generation: 2, ct, n });
    await waitUntil(() => harness.callbacks.onCredentialsRotated.mock.calls.length > 0);

    // 尽管"往返耗时"测出来是 0(远低于 staleReplayThresholdMs),仍然必须立即发起第二轮 refresh——
    // 因为这条 pending 是重载恢复的,`resumedPendingRefresh` 标记强制保守二次轮换。
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const second = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };
    expect(second.request_id).not.toBe(first.request_id);
  });

  it("sentAtMs 落盘补发送时刻:beginRefresh() 新发起的一轮把当前时钟写进 pending_refresh.sentAtMs", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.clock.value = 12_345;
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const pending = await harness.keyStore.loadPendingRefresh();
    expect(pending?.sentAtMs).toBe(12_345);
  });

  it("resumedPendingRefresh 标记只作用于被恢复的这一轮——重载恢复的请求成功轮换后,后续自然触发的新一轮不再被强制二次轮换", async () => {
    const keyStore = await seededKeyStore();
    await keyStore.savePendingRefresh({ requestId: "reload-resumed-id-2", sentAtMs: 0 });
    const harness = await started(await makeHarness({ keyStore, credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };

    const { ct, n } = await sealDesktopRefreshOk(first.request_id, "c".repeat(64), "d".repeat(64));
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: first.request_id, subject: `device:${DEVICE_ID}`, generation: 2, ct, n });
    // 恢复轮的强制二次轮换会自动发起第二轮。
    await waitUntil(() => harness.wsFactory.last.sent.length > 1);
    const second = JSON.parse(harness.wsFactory.last.sent[1]) as { request_id: string };

    // 第二轮不是"恢复"来的(是 beginRefresh() 正常发起的)——它的回执即便往返耗时很短也不该再触发
    // 第三轮。
    const { ct: ct2, n: n2 } = await sealDesktopRefreshOk(second.request_id, "1".repeat(64), "2".repeat(64));
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: second.request_id, subject: `device:${DEVICE_ID}`, generation: 3, ct: ct2, n: n2 });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(harness.wsFactory.last.sent).toHaveLength(2); // 没有第三轮
  });
});

// ---------------------------------------------------------------------------
// 无状态码分类(M0 §3 v0.5 块)
// ---------------------------------------------------------------------------

describe("无状态码分类 — retry_backoff(本地钟判定 access 仍在有效期内)", () => {
  it("logs the close code/reason followed by the explicit classification for injected diagnostics", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    harness.wsFactory.last.simulateClose(1013, "message_rate_limited");
    await waitUntil(() =>
      harness.logs.some((entry) => entry.message === "connection outcome classified"),
    );

    expect(harness.logs).toContainEqual({
      level: "info",
      message: "connection closed",
      context: { code: 1013, reason: "message_rate_limited" },
    });
    expect(harness.logs).toContainEqual({
      level: "info",
      message: "connection outcome classified",
      context: { classification: "retry_backoff" },
    });
    expect(harness.logs.findIndex((entry) => entry.message === "connection closed")).toBeLessThan(
      harness.logs.findIndex((entry) => entry.message === "connection outcome classified"),
    );
  });

  it("an upgrade failure with a fresh access token schedules a backoff reconnect, not needs_repair", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.clock.value = 0; // elapsed=0 < accessLifetimeMs(1000)
    harness.wsFactory.last.simulateUpgradeFailure();
    await waitUntil(() => harness.session.phase === "reconnect_scheduled");
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    expect(harness.scheduler.pendingCount).toBeGreaterThan(0);
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.sockets.length === 2);
  });
});

describe("无状态码分类 — needs_refresh(本地钟判定处于宽限窗,重连后立即 refresh)", () => {
  it("an upgrade failure while elapsed is within (lifetime, lifetime+prevWindow] forces an immediate refresh on the next successful open", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.clock.value = 1_500; // > accessLifetimeMs(1000), <= 1000+2000
    harness.wsFactory.last.simulateUpgradeFailure();
    await waitUntil(() => harness.session.phase === "reconnect_scheduled");
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.sockets.length === 2);
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const sent = JSON.parse(harness.wsFactory.last.sent[0]) as { t: string };
    expect(sent.t).toBe("token.refresh");
  });
});

describe("无状态码分类 — needs_repair(本地钟判定彻底过期)", () => {
  it("an upgrade failure with elapsed far beyond lifetime+prevWindow enters needs_repair and stops reconnecting", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.clock.value = 10_000; // >> 1000+2000
    harness.wsFactory.last.simulateUpgradeFailure();
    await waitUntil(() => harness.callbacks.onNeedsRepair.mock.calls.length > 0);
    expect(harness.session.phase).toBe("needs_repair");
    expect(await harness.keyStore.loadKeys()).toBeNull();
    const socketCountAfterRepair = harness.wsFactory.sockets.length;
    if (harness.scheduler.pendingCount > 0) harness.scheduler.runNext();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(harness.wsFactory.sockets.length).toBe(socketCountAfterRepair); // 不再尝试重连
  });

  it("审查返工:a prior plain-invalid (no close) fail does NOT poison a later classification — with a fresh clock, a subsequent unclassified close is still retry_backoff, not needs_repair", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: first.request_id, subject: `device:${DEVICE_ID}`, reason: "invalid" });
    // 上面这条 fail 会安排一次 retry(见"benign-retry"测试)——这里不管它,直接让底层 socket 掉线,
    // 模拟"invalid 之后连接也断了"这个组合场景。clock 保持在 0 不动:accessIssuedAtMs=-900,
    // elapsed 恒为 900ms(< accessLifetimeMs=1000)——"仍然新鲜"。

    harness.wsFactory.last.simulateClose(1006, "");
    // 这条断连是"曾经 open 过"的分支——reason="" 不认识,退回纯时钟的 classifyUpgradeFailure。
    // 审查返工后:分类表不再消费 lastRefreshOutcome,纯时钟判定在 900ms<1000ms(accessLifetimeMs)
    // 下必须是 retry_backoff,不是 needs_repair——即便之前收到过一次 reason="invalid" 的 fail。
    await waitUntil(() => harness.session.phase === "reconnect_scheduled");
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    expect(harness.session.phase).not.toBe("needs_repair");
  });
});

describe("close reason 快捷分类(有信息时不必退化成纯时钟猜测)", () => {
  it("token_reauthorization_failed → needs_refresh: forces an immediate refresh on next open regardless of a fresh clock", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.wsFactory.last.simulateOpen();
    harness.clock.value = 10; // 仍然很"新鲜"
    harness.wsFactory.last.simulateClose(1008, "token_reauthorization_failed");
    await waitUntil(() => harness.session.phase === "reconnect_scheduled");
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.sockets.length === 2);
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const sent = JSON.parse(harness.wsFactory.last.sent[0]) as { t: string };
    expect(sent.t).toBe("token.refresh");
  });

  it("message_rate_limited → retry_backoff: must never be treated as an auth failure (G8-knife requirement)", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.wsFactory.last.simulateOpen();
    harness.wsFactory.last.simulateClose(1013, "message_rate_limited");
    await waitUntil(() => harness.session.phase === "reconnect_scheduled");
    expect(harness.callbacks.onNeedsRepair).not.toHaveBeenCalled();
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.sockets.length === 2);
    harness.wsFactory.last.simulateOpen();
    await new Promise((resolve) => setTimeout(resolve, 20));
    // 没有被强制 refresh(不同于 token_reauthorization_failed)——access 仍新鲜,不该主动 refresh。
    expect(harness.wsFactory.last.sent).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// 前台恢复(M2 C1 spec §3 v0.5 块)
// ---------------------------------------------------------------------------

describe("前台恢复", () => {
  it("interrupts an in-progress backoff wait and reconnects immediately, without waiting out the full delay", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: 0 } }));
    harness.wsFactory.last.simulateUpgradeFailure();
    await waitUntil(() => harness.session.phase === "reconnect_scheduled");
    expect(harness.wsFactory.sockets).toHaveLength(1);

    harness.foreground.trigger(); // 不调用 scheduler.runNext() —— 证明不是靠计时器触发的
    await waitUntil(() => harness.wsFactory.sockets.length === 2);
  });

  it("while open, treats the current socket as suspect and closes it, triggering the normal reconnect path", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    // FIX2 P0-1:open 时本地钟(accessIssuedAtMs=0)还没到主动 refresh 阈值,`onConnectionOpened()`
    // 走的是"武装 replay.head watchdog"分支——那是与本测试要等的"前台恢复触发的重连计时器"完全
    // 独立的另一枚定时器,先确定性地把它排出去(到点时本地钟仍判 access 未过期,零副作用),不然下面
    // "计时器已安排"这个 predicate 会被 watchdog 提前满足,`runNext()` 就打偏了。
    expect(harness.scheduler.pendingCount).toBe(1);
    harness.scheduler.runNext();
    expect(harness.wsFactory.last.sent).toHaveLength(0); // 确认那一枚确实是无害的 watchdog

    const firstSocket = harness.wsFactory.last;
    harness.foreground.trigger();
    expect(firstSocket.closeCalls.length).toBeGreaterThan(0);
    // 不手动 simulateClose——`close()` 本身已经会异步自触发一次 onclose(见 FakeSocket 注释),
    // 这里只等那条真实链路(attemptOneConnection → classifyOutcome → 退避)跑到"计时器已安排"。
    await waitUntil(() => harness.scheduler.pendingCount > 0);
    harness.scheduler.runNext();
    await waitUntil(() => harness.wsFactory.sockets.length === 2);
  });

  it("审查返工:关闭旧连接后下一轮重连零退避——不背正常故障的等待时间(与真正的失败退避区分)", async () => {
    const harness = await started(await makeHarness());
    harness.wsFactory.last.simulateOpen();
    // FIX2 P0-1:同上一条用例——先确定性地清掉 open 时武装的 watchdog(与要等的重连计时器是两个
    // 独立的定时器),否则下面 `pendingCount > 0` 这个 predicate 会被 watchdog 提前满足，
    // `tasks[0]` 拿到的就不是重连计时器了。
    expect(harness.scheduler.pendingCount).toBe(1);
    harness.scheduler.runNext();

    // 前台恢复触发前先记下退避基线:backoffTracker 从未失败过,若真的按正常失败路径走,
    // 第一次退避应该是 baseMs=100ms(harness 配置),不是 0。
    harness.foreground.trigger();
    await waitUntil(() => harness.scheduler.pendingCount > 0);
    // 断言:这次被安排的重连计时器 delayMs 恰好是 0——不是 backoffTracker 算出来的 100ms 基准值。
    expect(harness.scheduler.tasks[0]?.delayMs).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// 多标签 Web Locks 单飞行
// ---------------------------------------------------------------------------

describe("多标签 Web Locks 单飞行", () => {
  it("a tab that cannot acquire the connect lock stays idle and never opens a socket", async () => {
    const harness = await makeHarness({ locks: unavailableConnectLocks() });
    activeSessions.push(harness.session);
    await harness.session.start();
    expect(harness.wsFactory.sockets).toHaveLength(0);
    expect(harness.session.phase).toBe("idle");
  });

  it("acquires the connect lock with {ifAvailable:true} (non-blocking probe)", async () => {
    const locks = recordingGrantingLocks();
    const harness = await started(await makeHarness({ locks }));
    harness.wsFactory.last.simulateOpen();
    const connectCall = locks.calls.find((call) => call.name.startsWith("agentloom-remote-connect:"));
    expect(connectCall?.options).toEqual({ ifAvailable: true });
  });

  it("acquires the refresh lock in exclusive mode when a refresh begins", async () => {
    const locks = recordingGrantingLocks();
    const harness = await started(await makeHarness({ locks, credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => locks.calls.some((call) => call.name.startsWith("agentloom-remote-refresh:")));
    const refreshCall = locks.calls.find((call) => call.name.startsWith("agentloom-remote-refresh:"));
    expect(refreshCall?.options).toEqual({ mode: "exclusive" });
  });

  it("falls back to single-tab passthrough (still connects normally) when locks is undefined — no polyfill", async () => {
    const harness = await started(await makeHarness({ locks: undefined }));
    harness.wsFactory.last.simulateOpen();
    expect(harness.session.phase).toBe("open");
    expect(harness.logs.some((entry) => entry.message.includes("Web Locks API unavailable"))).toBe(true);
  });

  it("审查返工:双 session 共享同一把假锁竞争——被动标签绝不拿 refresh 独占锁,也绝不恢复 pending_refresh(防锁死 leader)", async () => {
    const held = new Set<string>();
    const sharedKeyStore = await seededKeyStore();
    // 预先塞一条 pending_refresh——如果被动标签也去恢复它,它就会去抢 refresh 单飞行锁,
    // 这正是本测试要证明"绝不发生"的行为。
    await sharedKeyStore.savePendingRefresh({ requestId: "shared-pending-request-id", sentAtMs: 0 });

    const locksA = new SharedFakeLocks(held);
    const locksB = new SharedFakeLocks(held);
    const harnessA = await makeHarness({ keyStore: sharedKeyStore, locks: locksA, credentials: { accessIssuedAtMs: -900 } });
    const harnessB = await makeHarness({ keyStore: sharedKeyStore, locks: locksB, credentials: { accessIssuedAtMs: -900 } });
    activeSessions.push(harnessA.session, harnessB.session);

    // 两个"标签页"同时 start()——共享的 connect 锁(ifAvailable:true 非阻塞探测)保证只有一个真正
    // 拿到 leadership、只有一个的 task(runLoop)会被调用。
    await Promise.all([harnessA.session.start(), harnessB.session.start()]);

    const aIsLeader = harnessA.session.phase !== "idle";
    const leader = aIsLeader ? harnessA : harnessB;
    const leaderLocks = aIsLeader ? locksA : locksB;
    const follower = aIsLeader ? harnessB : harnessA;
    const followerLocks = aIsLeader ? locksB : locksA;

    // 被动标签:从未开过 socket(runLoop 从未被调用)、phase 仍是 idle。
    expect(follower.session.phase).toBe("idle");
    expect(follower.wsFactory.sockets).toHaveLength(0);
    // 被动标签绝不该调用 refresh 单飞行锁——它自己的 SharedFakeLocks 实例上一条 refresh 锁记录
    // 都不该有(resumePendingRefreshFromStore 只在 runLoop() 里调用,被动标签从未进入 runLoop)。
    expect(followerLocks.calls.some((call) => call.name.startsWith("agentloom-remote-refresh:"))).toBe(false);

    // leader:正常连接,并且确实去恢复了那条共享的 pending_refresh(用同一个 request_id 重发)。
    expect(leader.wsFactory.sockets.length).toBeGreaterThan(0);
    leader.wsFactory.last.simulateOpen();
    await waitUntil(() => leader.wsFactory.last.sent.length > 0);
    const resent = JSON.parse(leader.wsFactory.last.sent[0]) as { request_id: string };
    expect(resent.request_id).toBe("shared-pending-request-id");
    expect(leaderLocks.calls.some((call) => call.name.startsWith("agentloom-remote-refresh:"))).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 日志脱敏(M0 §9.8)
// ---------------------------------------------------------------------------

describe("日志脱敏 — access/refresh 永不出现在日志出口", () => {
  it("no log entry's message or context ever contains the raw access or refresh token value", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    const first = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };
    const { ct, n } = await sealDesktopRefreshOk(first.request_id, "c".repeat(64), "d".repeat(64));
    harness.wsFactory.last.simulateMessage({ t: "token.refresh.ok", request_id: first.request_id, subject: `device:${DEVICE_ID}`, generation: 2, ct, n });
    await waitUntil(() => harness.callbacks.onCredentialsRotated.mock.calls.length > 0);

    for (const entry of harness.logs) {
      expect(entry.message).not.toContain(ACCESS);
      expect(entry.message).not.toContain(REFRESH);
      for (const value of Object.values(entry.context ?? {})) {
        if (typeof value === "string") {
          expect(value).not.toContain(ACCESS);
          expect(value).not.toContain(REFRESH);
        }
      }
    }
  });
});

// ---------------------------------------------------------------------------
// 变异自证(任务书 §4⑤ 要求 ≥3 条:改分类判定表一格 / 去掉 pending_refresh 先落盘屏障 / 去掉单飞行锁)
// ---------------------------------------------------------------------------

describe("变异自证", () => {
  it("mutation proof: if the 'invalid without close' branch wrongly transitioned to needs_repair immediately, this test's own assertion above would already have caught it — this test isolates the exact boundary condition (close undefined vs close:true) that a one-line mutation could blur", async () => {
    const harnessNoClose = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harnessNoClose.wsFactory.last.simulateOpen();
    await waitUntil(() => harnessNoClose.wsFactory.last.sent.length > 0);
    const noCloseReq = JSON.parse(harnessNoClose.wsFactory.last.sent[0]) as { request_id: string };
    harnessNoClose.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: noCloseReq.request_id, subject: `device:${DEVICE_ID}`, reason: "invalid" });
    expect(harnessNoClose.callbacks.onNeedsRepair).not.toHaveBeenCalled(); // 真实实现:不带 close 不终态

    const harnessWithClose = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harnessWithClose.wsFactory.last.simulateOpen();
    await waitUntil(() => harnessWithClose.wsFactory.last.sent.length > 0);
    const closeReq = JSON.parse(harnessWithClose.wsFactory.last.sent[0]) as { request_id: string };
    harnessWithClose.wsFactory.last.simulateMessage({ t: "token.refresh.fail", request_id: closeReq.request_id, subject: `device:${DEVICE_ID}`, reason: "invalid", close: true });
    await waitUntil(() => harnessWithClose.callbacks.onNeedsRepair.mock.calls.length > 0); // 真实实现:带 close 立即终态

    // 一个"忽略 close 字段、见 reason=invalid 就终态"的变异版本会让上面两个断言都指向同一个结果——
    // 这里显式对照两条路径的最终 phase 确实不同,证明 close 字段真的在被读取、真的驱动分支。
    expect(harnessNoClose.session.phase).not.toBe("needs_repair");
    expect(harnessWithClose.session.phase).toBe("needs_repair");
  });

  it("mutation proof: removing the 'persist pending_refresh before send' ordering would let a crash between send and persist lose the request_id — this asserts persistence happens even before any response, proving the write is unconditional, not response-gated", async () => {
    const harness = await started(await makeHarness({ credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => harness.wsFactory.last.sent.length > 0);
    // 断言:此刻(回执从未到达、甚至没模拟服务端响应)pending_refresh 已经落盘——如果实现把落盘挪到
    // "收到回执之后"才做(去掉先落盘屏障的变异),这条断言会因为"还没收到任何回执"而失败(pending
    // 会是 null,因为变异版本压根没在这个时间点写过)。
    const pending = await harness.keyStore.loadPendingRefresh();
    expect(pending).not.toBeNull();
    const sentFrame = JSON.parse(harness.wsFactory.last.sent[0]) as { request_id: string };
    expect(pending?.requestId).toBe(sentFrame.request_id);
  });

  it("mutation proof: removing the refresh single-flight lock acquisition would mean locks.request() is never called for the refresh lock name — this asserts the exact call happened, which a deleted withRefreshSingleFlight() call would fail", async () => {
    const locks = recordingGrantingLocks();
    const harness = await started(await makeHarness({ locks, credentials: { accessIssuedAtMs: -900 } }));
    harness.wsFactory.last.simulateOpen();
    await waitUntil(() => locks.calls.some((call) => call.name.startsWith("agentloom-remote-refresh:")));
    const refreshLockCalls = locks.calls.filter((call) => call.name.startsWith("agentloom-remote-refresh:"));
    expect(refreshLockCalls.length).toBeGreaterThan(0); // 真实实现:锁调用确实发生了
    // 变异版本(去掉 withRefreshSingleFlight 包裹、直接调用 runRefreshRoundTrip)会让这个数组永远
    // 是空的,因为 recordingGrantingLocks 只在真的调用 locks.request() 时才 push。
  });
});
