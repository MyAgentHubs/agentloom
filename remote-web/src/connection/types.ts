// types.ts — T6c-refresh · ConnectionSession 依赖端口与共享类型。
//
// 权威参照（只读对照，未改动）：
//   - Web client protocol contract covering the connection lifecycle: refresh-token
//     rotation, stateless close-code classification, foreground resume, single-flight across tabs, log redaction, plus the on-device credential persistence contract.
//   - Protocol contract for the three-phase connection window, the three-frame refresh
//     handshake, the abuse-prevention matrix, and logging discipline.
//   - remote-relay/src/room-do.js（`REAUTH_CLOSE_REASON`/`MESSAGE_RATE_LIMIT_CLOSE_REASON`/
//     `device_revoked` 错误帧·只读对照，relay 是唯一权威 wire 行为来源）。
//
// 本模块只定义类型与端口接口——不含任何浏览器 API 直接调用，方便 connectionSession.test.ts 用纯
// TS 假实现跑全部分支，不依赖 jsdom（本仓 vitest 环境 = node，无 DOM/WebSocket 全局，任务书 §3
// 也不许装新依赖）。

/**
 * WebSocket readyState 数值——照 WHATWG 标准常量，不依赖全局 `WebSocket`（node 测试环境没有它）。
 * 用普通对象而不是 `const enum`——`tsconfig.json` 开了 `isolatedModules`，`const enum` 跨文件引用
 * 在按文件独立转译的工具链下不可靠，项目里其它枚举型常量（如无）也不会用它，这里不开这个先例。
 */
export const ReadyState = {
  CONNECTING: 0,
  OPEN: 1,
  CLOSING: 2,
  CLOSED: 3,
} as const;

export interface WebSocketCloseInfo {
  code: number;
  reason: string;
  wasClean: boolean;
}

/**
 * 真实浏览器 `WebSocket` 的最小子集——`WebSocketFactory` 注入这个形状，测试用假实现，生产用
 * `(url, protocols) => new WebSocket(url, protocols)`。
 */
export interface WebSocketLike {
  readonly readyState: number;
  onopen: (() => void) | null;
  onclose: ((event: WebSocketCloseInfo) => void) | null;
  onerror: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  send(data: string): void;
  close(code?: number, reason?: string): void;
}

/** `agentloom-rc-v1` + `token.<hex64>` 双 offer（M0 §9.1）——不接受自定义 header，浏览器办不到。 */
export type WebSocketFactory = (url: string, protocols: string[]) => WebSocketLike;

// ---------------------------------------------------------------------------
// 凭据（ConnectionSession 的输入）
// ---------------------------------------------------------------------------

/**
 * `ConnectionSession` 需要的凭据面——比 `store/key-store.ts::StoredPairingCredentials` 多一个
 * **必填** `kPair`（refresh 密文体用的长期设备密钥，见 key-store.ts 里 `kPair` 字段注释记的缺口：
 * `PairingSession` 目前不落盘它）。`ConnectionSession` 故意不从 `KeyStorePort.loadKeys()` 静默摸
 * 这个字段——调用方必须显式提供，缺失就是缺失，不假装接好了。
 */
export interface ConnectionCredentials {
  deviceId: string;
  room: string;
  /** `wss://<host>`——无路径/query/hash（qr-payload.ts 的 `relay_url` 校验口径同款）。 */
  relayUrl: string;
  /** 当前 access token（hex64）——WS 子协议 offer 用它，不是 refresh token。 */
  access: string;
  /** 当前 refresh token（hex64）——只用于 `token.refresh` 密文体，从不出现在 URL/子协议里。 */
  refresh: string;
  /** refresh 密文体加解密key（K_pair 原始 32 字节）——见本文件顶注与 key-store.ts 的缺口记录。 */
  kPair: Uint8Array;
  /** `access` 最近一次（重新）签发的本地时钟毫秒时间戳——本地过期估算的唯一输入,见 key-store.ts 同名字段注释。 */
  accessIssuedAtMs: number;
}

/** 一轮成功 refresh 的新凭据——`onCredentialsRotated` 回调与内部落盘都用这个形状。 */
export interface RotatedCredentials {
  access: string;
  refresh: string;
  accessIssuedAtMs: number;
}

// ---------------------------------------------------------------------------
// 前台恢复 / 多标签
// ---------------------------------------------------------------------------

/**
 * 前台恢复钩子（M0 §3 v0.5 块："前台恢复 = visibilitychange/pageshow/online 时把旧 socket 一律
 * 视为可疑"）。生产实现用 `document`/`window`；测试注入假实现，手动调用 `trigger()`。
 * 默认无操作（永不触发）——不装 polyfill，浏览器 API 缺失时静默降级（`connectionSession.ts` 里
 * 有对应的默认工厂）。
 */
export interface ForegroundResumePort {
  /** 订阅"回前台"信号；返回取消订阅函数。 */
  onResume(callback: () => void): () => void;
}

/**
 * Web Locks API 的最小子集——`navigator.locks.request` 签名。测试注入假实现；生产用真
 * `navigator.locks`（若存在）。**不装 polyfill**——`LocksPort | undefined` 是"环境有没有 locks"
 * 这件事本身的类型表达，`undefined` = 环境没有，`connectionSession.ts` 据此走「单标签直通」分支。
 */
export interface LocksPort {
  request<T>(
    name: string,
    options: { mode?: "exclusive" | "shared"; ifAvailable?: boolean; signal?: AbortSignal },
    callback: (lock: unknown | null) => Promise<T>,
  ): Promise<T>;
}

// ---------------------------------------------------------------------------
// 日志
// ---------------------------------------------------------------------------

export type LogLevel = "debug" | "info" | "warn" | "error";

/** 内部日志出口——`connectionSession.ts` 一律先过 `redact()` 再调用它（M0 §9.8 日志纪律）。 */
export type LogSink = (level: LogLevel, message: string, context?: Record<string, unknown>) => void;

// ---------------------------------------------------------------------------
// 分类 / 退避
// ---------------------------------------------------------------------------

export type ConnectionPhase = "first_connect" | "reconnect";

export type LastRefreshOutcome =
  | "none"
  | "ok"
  | "fail_invalid"
  | "fail_in_flight"
  | "fail_rate_limited"
  | "fail_unknown";

export type UpgradeFailureClassification = "needs_refresh" | "needs_repair" | "retry_backoff";

export type ConnectionSessionPhase =
  | "idle"
  | "connecting"
  | "open"
  | "reconnect_scheduled"
  | "needs_repair"
  | "closed";
