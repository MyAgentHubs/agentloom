// pairingTransport.ts — INT1 · 配对阶段真 WebSocket transport（把 `ui/pairing/stubTransport.ts`
// 换成真的）。实现 `pairing/pairing-session.ts::PairingTransportPort`——收 QrPayload，建到
// `qr.relay_url` 房间的真 WebSocket（pairing scope 连接契约，M0 §9.1/§9.5），把出站帧序列化发
// 出去、入站帧解出来交给调用方（`PairingSession.handleFrame` 归调用方驱动，这里不直接 import
// `PairingSession` 本体——纯 transport 关注点）。
//
// Authoritative reference (read-only, not modified here):
//   - Remote connection contract (M0 §9.1/§9.5): subprotocol
//     `["agentloom-rc-v1", "token.<64-char lowercase hex>"]`, relay's 101 response echoes only
//     `agentloom-rc-v1`; the pairing window's connect_token is HKDF(pairing_token)-derived, and the
//     phone independently derives the same value after scanning.
//   - `remote-relay/test/s1ja-fake-mobile-e2e.test.js`'s `connectWithToken()` (the real upgrade
//     path `Sec-WebSocket-Protocol: agentloom-rc-v1, token.<hex>`) + `pairDevice()` phase 1 (the
//     phone independently derives the same connect_token as desktop before connecting).
//   - `connection/connectionSession.ts`'s `buildConnectUrl()` (same `${relayUrl}/room/${room}`
//     path shape; the pairing phase has no `?last_seq` — that's reconnect-only for the connected
//     remote scope, M0 §6/§9.1).
//
// **同步可构造·异步才连接**：`PairingSession.start()` 内 `transport.send()` 是同步调用（发完
// `pair.hello` 立刻把 phase 推进到 `awaiting_accept`，不等待任何回执）。真实 WebSocket 建连
// （含这里额外的 connect_token HKDF 推导，`crypto/kdf.ts::deriveConnectTokenHex` 是 async）无法
// 同步完成——若在这里 `await` 完连接再返回 transport，调用方就要在 `usePairingSession()`（React
// hook，不能在 effect 外等 promise）之前先解出一个 async 结果，产生"hook 调用时机依赖运行时状态"
// 的反模式。改用标准的「先入队、连上再按序 flush」缓冲模式：`send()` 保持同步返回 void（不改变
// `PairingSession` 观察到的"帧已发出"语义），socket 未 open 时排入本地队列，`onopen` 时按入队
// 顺序原样 flush。

import type { QrPayload } from "../pairing/qr-payload.ts";
import type { PairingTransportPort } from "../pairing/pairing-session.ts";
import type { OutboundPairingFrame } from "../pairing/frames.ts";
import { deriveConnectTokenHex } from "../crypto/kdf.ts";
import { ReadyState, type WebSocketCloseInfo, type WebSocketFactory, type WebSocketLike } from "../connection/types.ts";

const SUBPROTOCOL_RC_V1 = "agentloom-rc-v1";

export interface PairingTransportEvents {
  /** 入站帧（JSON.parse 后的原始值）——调用方喂给 `PairingSession.handleFrame`；本模块不 import
   *  `PairingSession`，纯 transport 关注点，解析失败的帧直接丢弃（不是本层的校验职责）。 */
  onFrame: (raw: unknown) => void;
  /** WebSocket 进入 OPEN 态（队列已 flush 完）。 */
  onOpen?: () => void;
  onClose?: (info: WebSocketCloseInfo) => void;
  /** 每次有限重试排定时触发；attempt 从 1 开始，最大为 maxRetries。 */
  onRetry?: (info: PairingRetryInfo) => void;
  /** 已不能重试（次数耗尽，或 accept 后收到 desktop_offline）时触发，供 UI 展示具体原因。 */
  onPairingError?: (info: PairingTerminalError) => void;
  /** 建连失败的兼容回调：现在只在有限重试耗尽（或 HKDF 推导异常）后触发。正常 WebSocket close
   *  仍走 `onClose`，不重复经这里。 */
  onConnectError?: (error: Error) => void;
}

export type PairingRetryReason = "connect_failed" | "desktop_offline";

export interface PairingRetryInfo {
  reason: PairingRetryReason;
  attempt: number;
  maxRetries: number;
}

export interface PairingTerminalError {
  reason: PairingRetryReason;
  error: Error;
  maxRetries: number;
}

export interface PairingRetryClock {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface PairingTransportOptions {
  maxRetries?: number;
  retryDelayMs?: number;
  clock?: PairingRetryClock;
}

export interface RealPairingTransport extends PairingTransportPort {
  /** 主动关闭底层连接（M0 §9.5「配对完成 → 关配对连接」；调用方在 phase 转 `activated` 后调用）。
   *  幂等——多次调用/socket 尚未建立时调用都安全。 */
  close(code?: number, reason?: string): void;
}

/**
 * 建真 WebSocket 并实现 `PairingTransportPort`。子协议 offer 固定两项、顺序固定
 * `[agentloom-rc-v1, token.<connect_token>]`（M0 §9.1 grammar；relay 101 响应只回显前者，顺序本身
 * 不受 relay 校验，但两端一致的顺序是唯一有 KAT/e2e 覆盖过的形状——变异自证覆盖了调换顺序）。
 * URL = `${relay_url 去尾斜杠}/room/${room}`，无 query（配对阶段没有 `?last_seq`）。
 */
export function createRealPairingTransport(
  qr: QrPayload,
  webSocketFactory: WebSocketFactory,
  events: PairingTransportEvents,
  options: PairingTransportOptions = {},
): RealPairingTransport {
  const queue: OutboundPairingFrame[] = [];
  const maxRetries = options.maxRetries ?? 3;
  const retryDelayMs = options.retryDelayMs ?? 3_000;
  const clock: PairingRetryClock = options.clock ?? {
    setTimeout: (callback, delayMs) => globalThis.setTimeout(callback, delayMs),
    clearTimeout: (handle) => globalThis.clearTimeout(handle as number),
  };
  let socket: WebSocketLike | null = null;
  let connectToken: string | null = null;
  let helloFrame: OutboundPairingFrame | null = null;
  let pairAccepted = false;
  let retryCount = 0;
  let retryTimer: unknown = null;
  let terminalErrorReported = false;
  let closed = false;

  const reportTerminalError = (reason: PairingRetryReason, error: Error): void => {
    if (terminalErrorReported || closed) return;
    terminalErrorReported = true;
    if (retryTimer !== null) {
      clock.clearTimeout(retryTimer);
      retryTimer = null;
    }
    queue.length = 0;
    const terminalSocket = socket;
    socket = null;
    if (
      terminalSocket &&
      terminalSocket.readyState !== ReadyState.CLOSED &&
      terminalSocket.readyState !== ReadyState.CLOSING
    ) {
      terminalSocket.close(1000, "pairing_failed");
    }
    events.onPairingError?.({ reason, error, maxRetries });
    if (reason === "connect_failed") {
      events.onConnectError?.(error);
    }
  };

  const connect = (): void => {
    if (closed || terminalErrorReported || !connectToken) return;
    let opened = false;
    let retryScheduledForSocket = false;

    const scheduleRetry = (reason: PairingRetryReason, ws: WebSocketLike): void => {
      if (retryScheduledForSocket || closed || terminalErrorReported) return;
      if (pairAccepted) {
        reportTerminalError(reason, new Error(`pairing failed after pair.accept: ${reason}`));
        return;
      }
      if (retryCount >= maxRetries) {
        reportTerminalError(reason, new Error(`pairing retry exhausted: ${reason}`));
        return;
      }

      retryScheduledForSocket = true;
      retryCount += 1;
      queue.length = 0;
      if (helloFrame) queue.push(helloFrame);
      events.onRetry?.({ reason, attempt: retryCount, maxRetries });

      if (ws.readyState !== ReadyState.CLOSED && ws.readyState !== ReadyState.CLOSING) {
        ws.close(1000, "pairing_retry");
      }
      if (socket === ws) socket = null;
      retryTimer = clock.setTimeout(() => {
        retryTimer = null;
        connect();
      }, retryDelayMs);
    };

    try {
      const base = qr.relay_url.replace(/\/+$/, "");
      const url = `${base}/room/${qr.room}`;
      const ws = webSocketFactory(url, [SUBPROTOCOL_RC_V1, `token.${connectToken}`]);
      socket = ws;
      ws.onopen = () => {
        if (closed || socket !== ws) return;
        opened = true;
        for (const frame of queue.splice(0)) {
          ws.send(JSON.stringify(frame));
        }
        events.onOpen?.();
      };
      ws.onmessage = (event) => {
        if (closed || terminalErrorReported || socket !== ws) return;
        let parsed: unknown;
        try {
          parsed = JSON.parse(event.data);
        } catch {
          return; // 畸形帧——不是本层职责，`PairingSession.handleFrame` 本就要能容忍垃圾输入，
          // 但连 JSON 都不是的帧连喂给它的资格都没有，直接丢弃。
        }
        const record = asRecord(parsed);
        if (record?.t === "pair.accept") {
          // 必须在把帧交给上层前关闸：同一个事件循环内紧随其后的任何失败都绝不能再排定 hello。
          pairAccepted = true;
        }
        events.onFrame(parsed);
        if (record?.t === "error" && record.reason === "desktop_offline") {
          scheduleRetry("desktop_offline", ws);
        }
      };
      ws.onerror = () => {
        if (!opened) {
          scheduleRetry("connect_failed", ws);
        }
        // 已 open 之后的 error 后面必然跟一次 close 事件（WHATWG 关闭握手）——那条走 onClose，
        // 不在这里重复上报。
      };
      ws.onclose = (info) => {
        if (retryScheduledForSocket || terminalErrorReported || socket !== ws) return;
        if (!opened) {
          scheduleRetry("connect_failed", ws);
          return;
        }
        events.onClose?.(info);
      };
    } catch (error: unknown) {
      reportTerminalError("connect_failed", error instanceof Error ? error : new Error(String(error)));
    }
  };

  void deriveConnectTokenHex(qr.pairing_token).then(
    (derivedConnectToken) => {
      if (closed) return;
      connectToken = derivedConnectToken;
      connect();
    },
    (error: unknown) => {
      reportTerminalError("connect_failed", error instanceof Error ? error : new Error(String(error)));
    },
  );

  return {
    send(frame: OutboundPairingFrame) {
      if (closed || terminalErrorReported) return;
      if (frame.t === "pair.hello") {
        helloFrame = frame;
      }
      if (socket && socket.readyState === ReadyState.OPEN) {
        socket.send(JSON.stringify(frame));
      } else {
        queue.push(frame);
      }
    },
    close(code = 1000, reason = "pairing_done") {
      if (closed) return; // 幂等——第二次调用不重复触发底层 socket.close()（真实 WebSocket 在
      // spec 层面容忍重复 close()，但这里显式短路，不依赖那条隐含行为）。
      closed = true;
      if (retryTimer !== null) {
        clock.clearTimeout(retryTimer);
        retryTimer = null;
      }
      queue.length = 0;
      socket?.close(code, reason);
    },
  };
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : null;
}
