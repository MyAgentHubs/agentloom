import "fake-indexeddb/auto";
import { StrictMode } from "react";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ReadyState,
  type WebSocketCloseInfo,
  type WebSocketFactory,
  type WebSocketLike,
} from "../connection/types.ts";
import {
  InMemoryKeyStore,
  importNonExtractableAesGcmKey,
} from "../store/key-store.ts";
import { InMemoryEventStore } from "../store/inMemoryEventStore.ts";
import { AppRuntime } from "./AppRuntime.tsx";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

class FakeSocket implements WebSocketLike {
  readyState: number = ReadyState.CONNECTING;
  onopen: (() => void) | null = null;
  onclose: ((event: WebSocketCloseInfo) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  sent: string[] = [];

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    if (this.readyState === ReadyState.CLOSED) return;
    this.readyState = ReadyState.CLOSING;
    queueMicrotask(() => {
      this.readyState = ReadyState.CLOSED;
      this.onclose?.({ code: 1000, reason: "", wasClean: true });
    });
  }

  simulateOpen(): void {
    this.readyState = ReadyState.OPEN;
    this.onopen?.();
  }

  simulateClose(): void {
    this.readyState = ReadyState.CLOSED;
    this.onclose?.({ code: 1006, reason: "", wasClean: false });
  }
}

class FakeWebSocketFactory {
  sockets: FakeSocket[] = [];
  factory: WebSocketFactory = () => {
    const socket = new FakeSocket();
    this.sockets.push(socket);
    return socket;
  };
}

async function makeStoredCredentials() {
  const kRoomRaw = crypto.getRandomValues(new Uint8Array(32));
  const kRoomKey = await importNonExtractableAesGcmKey(kRoomRaw);
  const kPair = crypto.getRandomValues(new Uint8Array(32));
  return {
    deviceId: "device-1",
    room: "0123456789abcdef0123456789abcdef",
    relayUrl: "wss://relay.example",
    access: "a".repeat(64),
    refresh: "b".repeat(64),
    kRoomKey,
    kPair,
    accessIssuedAtMs: Date.now(),
  };
}

function bannerSeconds(): number {
  const match = screen
    .getByTestId("connection-banner")
    .textContent?.match(/(\d+)(?:s elapsed| 秒)/);
  if (!match) throw new Error("connection banner has no elapsed seconds");
  return Number(match[1]);
}

describe("AppRuntime · 连接生命周期", () => {
  it("StrictMode 双挂载只留一个活连接，cleanup 停掉的是自己那个 session", async () => {
    const stored = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new InMemoryEventStore();
    const factory = new FakeWebSocketFactory();
    const renderRuntime = (webSocketFactory: WebSocketFactory) => (
      <StrictMode>
        <AppRuntime
          stored={stored}
          keyStore={keyStore}
          webSocketFactory={webSocketFactory}
          eventStore={eventStore}
          onNeedsRepair={() => {}}
        />
      </StrictMode>
    );

    const { rerender } = render(renderRuntime(factory.factory));
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    const oldSocket = factory.sockets[0];
    rerender(
      renderRuntime((url, protocols) => factory.factory(url, protocols)),
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(2));
    await waitFor(() => expect(oldSocket.readyState).toBe(ReadyState.CLOSED));
    expect(factory.sockets[1].readyState).toBe(ReadyState.CONNECTING);

    await act(async () => oldSocket.simulateOpen());
    expect(
      screen.getByTestId("connection-banner").getAttribute("data-phase"),
    ).toBe("connecting");
    await act(async () => factory.sockets[1].simulateOpen());
    expect(screen.queryByTestId("connection-banner")).toBeNull();
  });

  it("断线后多次重试相位切换，横幅已持续时长不从零重新计时", async () => {
    const stored = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const factory = new FakeWebSocketFactory();
    const baseMs = Date.now();
    let elapsedMs = 0;
    vi.spyOn(Date, "now").mockImplementation(() => baseMs + elapsedMs);

    render(
      <AppRuntime
        stored={stored}
        keyStore={keyStore}
        webSocketFactory={factory.factory}
        eventStore={new InMemoryEventStore()}
        onNeedsRepair={() => {}}
      />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.sockets[0].simulateOpen());
    await act(async () => factory.sockets[0].simulateClose());
    await waitFor(() =>
      expect(screen.getByTestId("connection-banner")).toBeTruthy(),
    );
    elapsedMs = 11_000;
    await waitFor(() => expect(bannerSeconds()).toBe(11), { timeout: 2_000 });

    await waitFor(() => expect(factory.sockets).toHaveLength(2), {
      timeout: 3_000,
    });
    await act(async () => factory.sockets[1].simulateClose());
    elapsedMs = 14_000;
    await waitFor(() => expect(bannerSeconds()).toBe(14), { timeout: 2_000 });
  });
});
