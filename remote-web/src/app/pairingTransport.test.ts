// pairingTransport.test.ts — INT1 · `createRealPairingTransport()` 单元覆盖：URL/子协议构造、
// 未 open 前缓冲 + open 后按序 flush、入站 JSON 解析、连接失败/关闭回调、`close()` 幂等。
//
// 假 WebSocket 沿用 `connection/connectionSession.test.ts::FakeSocket` 同款模式（同一个
// `WebSocketLike` 接口，测试驱动 `simulateOpen()`/`simulateMessage()`/`simulateClose()`）——不是
// 另起一套假实现，只是本文件独立持有一份（每个测试文件各自持有测试替身是本仓既有惯例，
// `connectionSession.test.ts`/`pairing-session.test.ts` 均如此，不跨测试文件共享可变状态）。

import { describe, expect, it } from "vitest";
import { ReadyState, type WebSocketCloseInfo, type WebSocketLike } from "../connection/types.ts";
import type { QrPayload } from "../pairing/qr-payload.ts";
import { generateKeyPair } from "../crypto/x25519.ts";
import { bytesToBase64 } from "../crypto/bytes.ts";
import { deriveConnectTokenHex } from "../crypto/kdf.ts";
import { createRealPairingTransport } from "./pairingTransport.ts";

class ManualRetryClock {
  private nowMs = 0;
  private nextId = 1;
  private readonly timers = new Map<number, { at: number; callback: () => void }>();

  setTimeout = (callback: () => void, delayMs: number): number => {
    const id = this.nextId++;
    this.timers.set(id, { at: this.nowMs + delayMs, callback });
    return id;
  };

  clearTimeout = (id: unknown): void => {
    this.timers.delete(Number(id));
  };

  advanceBy(ms: number): void {
    this.nowMs += ms;
    const due = [...this.timers.entries()]
      .filter(([, timer]) => timer.at <= this.nowMs)
      .sort((left, right) => left[1].at - right[1].at);
    for (const [id, timer] of due) {
      this.timers.delete(id);
      timer.callback();
    }
  }
}

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
    if (this.readyState !== ReadyState.OPEN) {
      throw new DOMException("still in CONNECTING state", "InvalidStateError");
    }
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    this.closeCalls.push({ code, reason });
    if (this.readyState === ReadyState.CLOSED) return;
    this.readyState = ReadyState.CLOSING;
    queueMicrotask(() => {
      this.readyState = ReadyState.CLOSED;
      this.onclose?.({ code: code ?? 1000, reason: reason ?? "", wasClean: (code ?? 1000) === 1000 });
    });
  }

  simulateOpen(): void {
    this.readyState = ReadyState.OPEN;
    this.onopen?.();
  }

  simulateMessage(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }

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

function makeQr(overrides: Partial<QrPayload> = {}): QrPayload {
  return {
    v: 1,
    relay_url: "wss://relay.example",
    room: "0123456789abcdef0123456789abcdef",
    pairing_token: "a".repeat(64),
    desktop_pub: bytesToBase64(generateKeyPair().publicKey),
    ...overrides,
  };
}

/** 等 `deriveConnectTokenHex()` 那次异步 HKDF 推导跑完——transport 构造函数本身同步返回，真正建
 *  socket 是它内部 `.then()` 里的事，HKDF 走 WebCrypto 原生实现（`subtle.importKey` +
 *  `subtle.deriveBits` 两跳 await），不保证一个宏任务就能推完——照 `pairing-session.test.ts` 同款
 *  `flushAsync` 的既有教训，多跑几轮 `setTimeout(0)` 而不是赌一轮够用。 */
async function flushAsync(): Promise<void> {
  for (let i = 0; i < 5; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

describe("createRealPairingTransport() · URL 与子协议构造（M0 §9.1/§9.5）", () => {
  it("connects to `${relay_url}/room/${room}` with no query string (pairing 阶段没有 ?last_seq)", async () => {
    const factory = new FakeWebSocketFactory();
    const qr = makeQr({ relay_url: "wss://relay.example", room: "0123456789abcdef0123456789abcdef" });
    createRealPairingTransport(qr, factory.factory, { onFrame: () => {} });
    await flushAsync();

    expect(factory.last.url).toBe("wss://relay.example/room/0123456789abcdef0123456789abcdef");
  });

  it("strips a trailing slash on relay_url before appending /room/<room> (double-slash guard)", async () => {
    const factory = new FakeWebSocketFactory();
    const qr = makeQr({ relay_url: "wss://relay.example/" });
    createRealPairingTransport(qr, factory.factory, { onFrame: () => {} });
    await flushAsync();

    expect(factory.last.url).toBe("wss://relay.example/room/0123456789abcdef0123456789abcdef");
  });

  it("offers exactly [agentloom-rc-v1, token.<connect_token>] in that order, connect_token independently re-derivable", async () => {
    const factory = new FakeWebSocketFactory();
    const qr = makeQr({ pairing_token: "b".repeat(64) });
    createRealPairingTransport(qr, factory.factory, { onFrame: () => {} });
    await flushAsync();

    const expectedToken = await deriveConnectTokenHex(qr.pairing_token);
    expect(factory.last.protocols).toEqual(["agentloom-rc-v1", `token.${expectedToken}`]);
  });
});

describe("createRealPairingTransport() · 未 open 前缓冲、open 后按序 flush", () => {
  it("send() before the socket ever opens does not touch the underlying socket (would throw on a real WebSocket)", async () => {
    const factory = new FakeWebSocketFactory();
    const transport = createRealPairingTransport(makeQr(), factory.factory, { onFrame: () => {} });
    await flushAsync();

    // 此刻 factory.last 已经建好（socket 存在），但仍是 CONNECTING——send() 必须走缓冲，不能碰
    // 底层 socket（真实 WebSocket 在 CONNECTING 态调 send() 会抛 InvalidStateError，见上面
    // FakeSocket.send() 的同款模拟）。
    expect(factory.last.readyState).toBe(ReadyState.CONNECTING);
    expect(() =>
      transport.send({ t: "pair.hello", room: makeQr().room, remote_pub: "x", token_ct: "y", token_n: "z" }),
    ).not.toThrow();
    expect(factory.last.sent).toHaveLength(0);
  });

  it("buffered frames flush in send() order once the socket opens", async () => {
    const factory = new FakeWebSocketFactory();
    const qr = makeQr();
    const transport = createRealPairingTransport(qr, factory.factory, { onFrame: () => {} });
    await flushAsync();

    transport.send({ t: "pair.hello", room: qr.room, remote_pub: "pub1", token_ct: "ct1", token_n: "n1" });
    transport.send({ t: "pair.done", room: qr.room, device_id: "dev1", confirm_ct: "ct2", confirm_n: "n2" });
    expect(factory.last.sent).toHaveLength(0);

    factory.last.simulateOpen();

    expect(factory.last.sent).toHaveLength(2);
    expect(JSON.parse(factory.last.sent[0]!)).toEqual({ t: "pair.hello", room: qr.room, remote_pub: "pub1", token_ct: "ct1", token_n: "n1" });
    expect(JSON.parse(factory.last.sent[1]!)).toEqual({ t: "pair.done", room: qr.room, device_id: "dev1", confirm_ct: "ct2", confirm_n: "n2" });
  });

  it("send() after open goes straight to the socket (no buffering delay)", async () => {
    const factory = new FakeWebSocketFactory();
    const qr = makeQr();
    const transport = createRealPairingTransport(qr, factory.factory, { onFrame: () => {} });
    await flushAsync();
    factory.last.simulateOpen();

    transport.send({ t: "pair.hello", room: qr.room, remote_pub: "pub", token_ct: "ct", token_n: "n" });

    expect(factory.last.sent).toHaveLength(1);
  });

  it("fires onOpen after the flush, not before", async () => {
    const factory = new FakeWebSocketFactory();
    const qr = makeQr();
    const events: string[] = [];
    const transport = createRealPairingTransport(qr, factory.factory, {
      onFrame: () => {},
      onOpen: () => events.push("open"),
    });
    await flushAsync();
    transport.send({ t: "pair.hello", room: qr.room, remote_pub: "p", token_ct: "c", token_n: "n" });

    factory.last.simulateOpen();

    expect(factory.last.sent).toHaveLength(1); // flush 已经发生
    expect(events).toEqual(["open"]);
  });
});

describe("createRealPairingTransport() · 入站帧解析", () => {
  it("onFrame receives the JSON.parse()'d value of an incoming message", async () => {
    const factory = new FakeWebSocketFactory();
    const received: unknown[] = [];
    const transport = createRealPairingTransport(makeQr(), factory.factory, {
      onFrame: (raw) => received.push(raw),
    });
    await flushAsync();
    factory.last.simulateOpen();

    factory.last.simulateMessage({ t: "pair.accept", room: "r", device_id: "d" });

    expect(received).toEqual([{ t: "pair.accept", room: "r", device_id: "d" }]);
    void transport; // transport 未被 close()，仅用于持有连接生命周期
  });

  it("a non-JSON message is dropped silently, not forwarded as garbage", async () => {
    const factory = new FakeWebSocketFactory();
    const received: unknown[] = [];
    createRealPairingTransport(makeQr(), factory.factory, { onFrame: (raw) => received.push(raw) });
    await flushAsync();
    factory.last.simulateOpen();

    factory.last.onmessage?.({ data: "not json {" });

    expect(received).toEqual([]);
  });
});

describe("createRealPairingTransport() · 连接失败 / 关闭 / close() 幂等", () => {
  it("onConnectError fires when a pre-open failure has no retries available", async () => {
    const factory = new FakeWebSocketFactory();
    const errors: Error[] = [];
    const terminalErrors: Array<{ reason: string; maxRetries: number }> = [];
    createRealPairingTransport(
      makeQr(),
      factory.factory,
      {
        onFrame: () => {},
        onConnectError: (error) => errors.push(error),
        onPairingError: ({ reason, maxRetries }) => terminalErrors.push({ reason, maxRetries }),
      },
      { maxRetries: 0 },
    );
    await flushAsync();

    factory.last.simulateUpgradeFailure();

    expect(errors).toHaveLength(1);
    expect(terminalErrors).toEqual([{ reason: "connect_failed", maxRetries: 0 }]);
  });

  it("onConnectError does NOT fire for a post-open error (that path is onClose's job)", async () => {
    const factory = new FakeWebSocketFactory();
    const errors: Error[] = [];
    const closes: WebSocketCloseInfo[] = [];
    createRealPairingTransport(makeQr(), factory.factory, {
      onFrame: () => {},
      onConnectError: (error) => errors.push(error),
      onClose: (info) => closes.push(info),
    });
    await flushAsync();
    factory.last.simulateOpen();

    factory.last.onerror?.();
    factory.last.simulateUpgradeFailure(); // close 事件仍会走 onClose

    expect(errors).toHaveLength(0);
    expect(closes).toHaveLength(1);
  });

  it("close() closes the underlying socket with code 1000 by default and is idempotent", async () => {
    const factory = new FakeWebSocketFactory();
    const transport = createRealPairingTransport(makeQr(), factory.factory, { onFrame: () => {} });
    await flushAsync();
    factory.last.simulateOpen();

    transport.close();
    transport.close(); // 第二次调用安全，不重复抛错/不重复关闭已关闭的 socket

    expect(factory.last.closeCalls).toEqual([{ code: 1000, reason: "pairing_done" }]);
  });

  it("close() before the socket is even created (still awaiting HKDF) suppresses the pending connect entirely", async () => {
    const factory = new FakeWebSocketFactory();
    const transport = createRealPairingTransport(makeQr(), factory.factory, { onFrame: () => {} });
    // 故意不 await flushAsync()——在 HKDF 推导那次微任务落地前就 close()：`.then()` 回调里的
    // `if (closed) return;` 必须挡住后续任何 socket 创建，不是"创建了但立刻关掉"。
    expect(() => transport.close()).not.toThrow();
    await flushAsync();

    expect(factory.sockets).toHaveLength(0);

    // 之后再 send() 也不该抛错、不该在未来某个时刻悄悄冒出一个 socket。
    transport.send({ t: "pair.hello", room: makeQr().room, remote_pub: "p", token_ct: "c", token_n: "n" });
    await flushAsync();
    expect(factory.sockets).toHaveLength(0);
  });
});

describe("createRealPairingTransport() · pair.hello 有限自动重试", () => {
  it("desktop_offline schedules a 3-second reconnect, resends the identical hello, and accepts success", async () => {
    const factory = new FakeWebSocketFactory();
    const clock = new ManualRetryClock();
    const retries: Array<{ reason: string; attempt: number; maxRetries: number }> = [];
    const terminalReasons: string[] = [];
    const qr = makeQr();
    const transport = createRealPairingTransport(
      qr,
      factory.factory,
      {
        onFrame: () => {},
        onRetry: (info) => retries.push(info),
        onPairingError: (info) => terminalReasons.push(info.reason),
      },
      { clock },
    );
    await flushAsync();
    const hello = { t: "pair.hello" as const, room: qr.room, remote_pub: "pub", token_ct: "ct", token_n: "n" };
    transport.send(hello);
    factory.last.simulateOpen();

    factory.last.simulateMessage({ t: "error", reason: "desktop_offline" });
    expect(retries).toEqual([{ reason: "desktop_offline", attempt: 1, maxRetries: 3 }]);
    clock.advanceBy(2_999);
    expect(factory.sockets).toHaveLength(1);
    clock.advanceBy(1);
    expect(factory.sockets).toHaveLength(2);

    factory.last.simulateOpen();
    expect(factory.last.sent.map((raw) => JSON.parse(raw))).toEqual([hello]);
    factory.last.simulateMessage({ t: "pair.accept", room: qr.room, device_id: "dev-1" });
    expect(terminalReasons).toEqual([]);
  });

  it("desktop_offline exhausts exactly 3 retries and then reports the concrete reason", async () => {
    const factory = new FakeWebSocketFactory();
    const clock = new ManualRetryClock();
    const terminalReasons: string[] = [];
    const qr = makeQr();
    const transport = createRealPairingTransport(
      qr,
      factory.factory,
      {
        onFrame: () => {},
        onPairingError: (info) => terminalReasons.push(info.reason),
      },
      { clock },
    );
    await flushAsync();
    transport.send({ t: "pair.hello", room: qr.room, remote_pub: "pub", token_ct: "ct", token_n: "n" });

    for (let retry = 0; retry < 3; retry += 1) {
      factory.last.simulateOpen();
      factory.last.simulateMessage({ t: "error", reason: "desktop_offline" });
      clock.advanceBy(3_000);
    }
    factory.last.simulateOpen();
    factory.last.simulateMessage({ t: "error", reason: "desktop_offline" });

    expect(factory.sockets).toHaveLength(4);
    expect(factory.sockets.every((socket) => socket.sent.length === 1)).toBe(true);
    expect(terminalReasons).toEqual(["desktop_offline"]);
    clock.advanceBy(30_000);
    expect(factory.sockets).toHaveLength(4);
  });

  it("a failure before open retries after 3 seconds and then flushes hello", async () => {
    const factory = new FakeWebSocketFactory();
    const clock = new ManualRetryClock();
    const qr = makeQr();
    const transport = createRealPairingTransport(qr, factory.factory, { onFrame: () => {} }, { clock });
    await flushAsync();
    const hello = { t: "pair.hello" as const, room: qr.room, remote_pub: "pub", token_ct: "ct", token_n: "n" };
    transport.send(hello);

    factory.last.simulateUpgradeFailure();
    clock.advanceBy(2_999);
    expect(factory.sockets).toHaveLength(1);
    clock.advanceBy(1);
    expect(factory.sockets).toHaveLength(2);
    factory.last.simulateOpen();

    expect(factory.last.sent.map((raw) => JSON.parse(raw))).toEqual([hello]);
  });

  it("a close before open retries even when the browser emits no preceding error event", async () => {
    const factory = new FakeWebSocketFactory();
    const clock = new ManualRetryClock();
    const qr = makeQr();
    const transport = createRealPairingTransport(qr, factory.factory, { onFrame: () => {} }, { clock });
    await flushAsync();
    const hello = { t: "pair.hello" as const, room: qr.room, remote_pub: "pub", token_ct: "ct", token_n: "n" };
    transport.send(hello);

    factory.last.readyState = ReadyState.CLOSED;
    factory.last.onclose?.({ code: 1006, reason: "", wasClean: false });
    clock.advanceBy(3_000);
    factory.last.simulateOpen();

    expect(factory.sockets).toHaveLength(2);
    expect(factory.last.sent.map((raw) => JSON.parse(raw))).toEqual([hello]);
  });

  it("after retry exhaustion, late frames and outbound sends cannot continue pairing behind the error screen", async () => {
    const factory = new FakeWebSocketFactory();
    const clock = new ManualRetryClock();
    const received: unknown[] = [];
    const qr = makeQr();
    const transport = createRealPairingTransport(
      qr,
      factory.factory,
      { onFrame: (frame) => received.push(frame) },
      { clock, maxRetries: 0 },
    );
    await flushAsync();
    transport.send({ t: "pair.hello", room: qr.room, remote_pub: "pub", token_ct: "ct", token_n: "n" });
    factory.last.simulateOpen();
    factory.last.simulateMessage({ t: "error", reason: "desktop_offline" });

    factory.last.simulateMessage({ t: "pair.accept", room: qr.room, device_id: "late-device" });
    transport.send({
      t: "pair.done",
      room: qr.room,
      device_id: "late-device",
      confirm_ct: "late-ct",
      confirm_n: "late-n",
    });

    expect(received).toEqual([{ t: "error", reason: "desktop_offline" }]);
    expect(factory.last.sent).toHaveLength(1);
    expect(factory.last.closeCalls).toEqual([{ code: 1000, reason: "pairing_failed" }]);
  });

  it("after pair.accept, desktop_offline is terminal and never reconnects or resends hello", async () => {
    const factory = new FakeWebSocketFactory();
    const clock = new ManualRetryClock();
    const terminalReasons: string[] = [];
    const qr = makeQr();
    const transport = createRealPairingTransport(
      qr,
      factory.factory,
      {
        onFrame: () => {},
        onPairingError: (info) => terminalReasons.push(info.reason),
      },
      { clock },
    );
    await flushAsync();
    transport.send({ t: "pair.hello", room: qr.room, remote_pub: "pub", token_ct: "ct", token_n: "n" });
    factory.last.simulateOpen();
    factory.last.simulateMessage({ t: "pair.accept", room: qr.room, device_id: "dev-1" });

    factory.last.simulateMessage({ t: "error", reason: "desktop_offline" });
    clock.advanceBy(30_000);

    expect(factory.sockets).toHaveLength(1);
    expect(factory.sockets[0]!.sent).toHaveLength(1);
    expect(terminalReasons).toEqual(["desktop_offline"]);
  });
});
