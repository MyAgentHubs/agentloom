// AppRuntime.frameIngestion.e2e.test.tsx — end-to-end coverage for serial frame ingestion
// and rejection of frames routed to the wrong session.
// As in sibling AppRuntime suites, duplicate local fake-relay and crypto helpers instead
// of importing from AppRuntime.e2e.test.tsx; relay encryption uses independent WebCrypto.

import "fake-indexeddb/auto";
import { describe, expect, it, afterEach } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import {
  ReadyState,
  type WebSocketCloseInfo,
  type WebSocketFactory,
  type WebSocketLike,
} from "../connection/types.ts";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import {
  InMemoryKeyStore,
  importNonExtractableAesGcmKey,
} from "../store/key-store.ts";
import { IndexedDbEventStore } from "../store/indexeddbEventStore.ts";
import type { EventStorePort } from "../store/port.ts";
import { AppRuntime } from "./AppRuntime.tsx";

afterEach(() => {
  cleanup();
  window.history.replaceState({}, "", "/");
});

const ROOM = "0123456789abcdef0123456789abcdef";

class FakeSocket implements WebSocketLike {
  readyState: number = ReadyState.CONNECTING;
  onopen: (() => void) | null = null;
  onclose: ((event: WebSocketCloseInfo) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  sent: string[] = [];

  constructor(
    public readonly url: string,
    public readonly protocols: string[],
  ) {}

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

  simulateMessage(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }

  simulateClose(code = 1006, reason = ""): void {
    this.readyState = ReadyState.CLOSED;
    this.onclose?.({ code, reason, wasClean: false });
  }
}

class FakeWebSocketFactory {
  sockets: FakeSocket[] = [];
  factory: WebSocketFactory = (url, protocols) => {
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

// ============================================================================
// Independent AES-256-GCM with manually constructed AAD.
// ============================================================================

interface Meta {
  v: number;
  room: string;
  epoch: number;
  kind: string;
  session: string | null;
  command_id: string | null;
}

function buildAadIndependent(meta: Meta): string {
  const part = (v: unknown) => (v === null || v === undefined ? "" : String(v));
  return [
    part(meta.v),
    part(meta.room),
    part(meta.epoch),
    part(meta.kind),
    part(meta.session),
    part(meta.command_id),
  ].join("|");
}

function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}

async function importAesKey(
  raw: Uint8Array,
  usages: KeyUsage[],
): Promise<CryptoKey> {
  return crypto.subtle.importKey(
    "raw",
    toBufferSource(raw),
    "AES-GCM",
    false,
    usages,
  );
}

async function sealIndependent(
  rawKey: Uint8Array,
  meta: Meta,
  plaintext: Uint8Array,
): Promise<{ ct: string; n: string }> {
  const key = await importAesKey(rawKey, ["encrypt"]);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ciphertext = await crypto.subtle.encrypt(
    {
      name: "AES-GCM",
      iv: toBufferSource(nonce),
      additionalData: toBufferSource(utf8Bytes(buildAadIndependent(meta))),
    },
    key,
    toBufferSource(plaintext),
  );
  return {
    ct: bytesToBase64(new Uint8Array(ciphertext)),
    n: bytesToBase64(nonce),
  };
}

/** Fake relay event frame: independently encrypts payload; seq and client_msg_id stay outside ciphertext. */
async function encryptEventFrame(
  kRoomRaw: Uint8Array,
  params: {
    session: string | null;
    seq: number;
    clientMsgId: string;
    epoch: number;
    payload: unknown;
  },
): Promise<Record<string, unknown>> {
  const meta: Meta = {
    v: 1,
    room: ROOM,
    epoch: params.epoch,
    kind: "event",
    session: params.session,
    command_id: null,
  };
  const { ct, n } = await sealIndependent(
    kRoomRaw,
    meta,
    utf8Bytes(JSON.stringify(params.payload)),
  );
  return {
    v: 1,
    room: ROOM,
    epoch: params.epoch,
    kind: "event",
    session: params.session,
    command_id: null,
    seq: params.seq,
    client_msg_id: params.clientMsgId,
    ct,
    n,
    ts: Date.now(),
  };
}

async function makeStoredCredentials() {
  const kRoomRaw = new Uint8Array(32);
  crypto.getRandomValues(kRoomRaw);
  const kRoomKey = await importNonExtractableAesGcmKey(kRoomRaw);
  const kPair = new Uint8Array(32);
  crypto.getRandomValues(kPair);
  const stored = {
    deviceId: "device-1",
    room: ROOM,
    relayUrl: "wss://relay.example",
    access: "a".repeat(64),
    refresh: "b".repeat(64),
    kRoomKey,
    kPair,
    accessIssuedAtMs: Date.now(),
  };
  return { kRoomRaw, stored };
}

describe("AppRuntime · 帧摄入", () => {
  it("帧 1 卡住入库时换 eventStore 触发重连——新 socket 的帧仍须排在帧 1 之后", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const backingStore = new IndexedDbEventStore(`apprt-reconnect-${crypto.randomUUID()}`);
    const steps: string[] = [];
    let releaseFirst!: () => void;
    const firstGate = new Promise<void>((resolve) => { releaseFirst = resolve; });
    const makeEventStore = (): EventStorePort => ({
      applyEventIfNew: async (input) => {
        steps.push(`start${input.seq}`);
        if (input.seq === 1) await firstGate;
        const result = await backingStore.applyEventIfNew(input);
        steps.push(`end${input.seq}`);
        return result;
      },
      getWatermark: () => backingStore.getWatermark(),
      hasAppliedClientMsgId: (id) => backingStore.hasAppliedClientMsgId(id),
      listEvents: () => backingStore.listEvents(),
    });
    const factory = new FakeWebSocketFactory();
    const renderRuntime = (eventStore: EventStorePort) => (
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory}
        eventStore={eventStore} onNeedsRepair={() => {}} />
    );
    const view = render(renderRuntime(makeEventStore()));
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());

    const frame1 = await encryptEventFrame(kRoomRaw, {
      session: null, seq: 1, clientMsgId: "reconnect-1", epoch: 0,
      payload: { t: "session.index", full: true, sessions: [] },
    });
    const frame2 = await encryptEventFrame(kRoomRaw, {
      session: null, seq: 2, clientMsgId: "reconnect-2", epoch: 0,
      payload: { t: "session.index", full: true, sessions: [] },
    });
    act(() => factory.last.simulateMessage(frame1));
    await waitFor(() => expect(steps).toEqual(["start1"]));
    try {
      view.rerender(renderRuntime(makeEventStore()));
      await waitFor(() => expect(factory.sockets).toHaveLength(2));
      await act(async () => factory.last.simulateOpen());
      act(() => factory.last.simulateMessage(frame2));
      await act(async () => { await new Promise((resolve) => setTimeout(resolve, 30)); });
      expect(steps).toEqual(["start1"]);
    } finally {
      releaseFirst();
    }
    await waitFor(() => expect(steps).toEqual(["start1", "end1", "start2", "end2"]));
    expect((await backingStore.listEvents()).map((row) => row.clientMsgId)).toEqual(["reconnect-1", "reconnect-2"]);
  });

  it("连续 3 帧须等前一帧入库完成且拒绝错误路由", async () => {
    window.history.replaceState({}, "", "/?debug=1");
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const backingStore = new IndexedDbEventStore(
      `apprt-serial-${crypto.randomUUID()}`,
    );
    const steps: string[] = [];
    let releaseFirst!: () => void;
    const firstGate = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const eventStore: EventStorePort = {
      applyEventIfNew: async (input) => {
        steps.push(`start${input.seq}`);
        if (input.seq === 1) await firstGate;
        const result = await backingStore.applyEventIfNew(input);
        steps.push(`end${input.seq}`);
        return result;
      },
      getWatermark: () => backingStore.getWatermark(),
      hasAppliedClientMsgId: (id) => backingStore.hasAppliedClientMsgId(id),
      listEvents: () => backingStore.listEvents(),
    };
    const factory = new FakeWebSocketFactory();
    render(
      <AppRuntime
        stored={stored}
        keyStore={keyStore}
        webSocketFactory={factory.factory}
        eventStore={eventStore}
        onNeedsRepair={() => {}}
      />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());
    await screen.findByTestId("debug-panel");

    const frames = await Promise.all(
      [1, 2, 3].map((seq) =>
        encryptEventFrame(kRoomRaw, {
          session: null,
          seq,
          clientMsgId: `serial-${seq}`,
          epoch: 0,
          payload: { t: "session.index", full: true, sessions: [] },
        }),
      ),
    );
    act(() => {
      for (const frame of frames) factory.last.simulateMessage(frame);
    });
    await waitFor(() => expect(steps).toContain("start1"));
    try {
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 30));
      });
      expect(steps).toEqual(["start1"]);
    } finally {
      releaseFirst();
    }
    await waitFor(() =>
      expect(steps).toEqual([
        "start1",
        "end1",
        "start2",
        "end2",
        "start3",
        "end3",
      ]),
    );
    expect(
      (await backingStore.listEvents()).map((row) => row.clientMsgId),
    ).toEqual(["serial-1", "serial-2", "serial-3"]);

    const misrouted = await encryptEventFrame(kRoomRaw, {
      session: "s-wrong",
      seq: 4,
      clientMsgId: "route-invalid",
      epoch: 0,
      payload: { t: "session.index", full: true, sessions: [] },
    });
    await act(async () => factory.last.simulateMessage(misrouted));
    await waitFor(() =>
      expect(screen.getByTestId("debug-routingRejected").textContent).toBe("1"),
    );
    expect(
      (await backingStore.listEvents()).map((row) => row.clientMsgId),
    ).toEqual(["serial-1", "serial-2", "serial-3"]);
  });
});
