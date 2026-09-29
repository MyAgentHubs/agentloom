// browserWebSocketFactory.ts — INT1 · 真实浏览器 `WebSocket` → `connection/types.ts::WebSocketLike`
// 适配器。`WebSocketFactory` 是 `PairingTransportPort`/`ConnectionSession` 两处都要的注入点
// （连接层顶注："生产用 `(url, protocols) => new WebSocket(url, protocols)`"）——本文件就是那句
// 话的落地实现，供 `main.tsx` 同时喂给配对 transport 与（未来）已配对运行时的 ConnectionSession。
//
// 原生 `WebSocket.onclose` 签名是 `(ev: CloseEvent) => void`（比 `WebSocketCloseInfo` 多很多字段）、
// `onmessage` 签名是 `(ev: MessageEvent) => void`（`data` 可能是 string/Blob/ArrayBuffer）——本适配
// 器只做「窄化成 `WebSocketLike` 需要的最小形状」这一层结构转换，不改变任何字节内容、不做协议
// 判断。

import type { WebSocketCloseInfo, WebSocketFactory, WebSocketLike } from "../connection/types.ts";

class BrowserWebSocketAdapter implements WebSocketLike {
  private readonly socket: WebSocket;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  private closeHandler: ((event: WebSocketCloseInfo) => void) | null = null;
  private messageHandler: ((event: { data: string }) => void) | null = null;

  constructor(url: string, protocols: string[]) {
    this.socket = new WebSocket(url, protocols);
    this.socket.onopen = () => this.onopen?.();
    this.socket.onerror = () => this.onerror?.();
    this.socket.onclose = (event) => {
      this.closeHandler?.({ code: event.code, reason: event.reason, wasClean: event.wasClean });
    };
    this.socket.onmessage = (event) => {
      // relay 协议只发 JSON 文本帧（M0 §1「wire format·JSON」）——非 string payload 结构上不可能
      // 是本协议帧，静默丢弃（不是"容忍垃圾输入"，是"这条根本不是本协议消息，不属于要处理的
      // 输入域"）。
      if (typeof event.data === "string") {
        this.messageHandler?.({ data: event.data });
      }
    };
  }

  get readyState(): number {
    return this.socket.readyState;
  }

  get onclose(): ((event: WebSocketCloseInfo) => void) | null {
    return this.closeHandler;
  }
  set onclose(handler: ((event: WebSocketCloseInfo) => void) | null) {
    this.closeHandler = handler;
  }

  get onmessage(): ((event: { data: string }) => void) | null {
    return this.messageHandler;
  }
  set onmessage(handler: ((event: { data: string }) => void) | null) {
    this.messageHandler = handler;
  }

  send(data: string): void {
    this.socket.send(data);
  }

  close(code?: number, reason?: string): void {
    this.socket.close(code, reason);
  }
}

export function createBrowserWebSocketFactory(): WebSocketFactory {
  return (url, protocols) => new BrowserWebSocketAdapter(url, protocols);
}
