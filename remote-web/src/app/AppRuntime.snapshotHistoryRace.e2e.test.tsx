// AppRuntime.snapshotHistoryRace.e2e.test.tsx — end-to-end coverage for epoch changes
// while sealing control.snapshot and socket closure while sealing control.history.
// As in sibling AppRuntime suites, duplicate local fake-relay and crypto helpers instead
// of importing from AppRuntime.e2e.test.tsx; relay encryption uses independent WebCrypto.

import "fake-indexeddb/auto";
import { describe, expect, it, vi, afterEach } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ReadyState, type WebSocketCloseInfo, type WebSocketFactory, type WebSocketLike } from "../connection/types.ts";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import { seal } from "../crypto/envelope.ts";
import { InMemoryKeyStore, importNonExtractableAesGcmKey } from "../store/key-store.ts";
import { IndexedDbEventStore } from "../store/indexeddbEventStore.ts";
import { AppRuntime } from "./AppRuntime.tsx";

vi.mock("../crypto/envelope.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../crypto/envelope.ts")>();
  return { ...actual, seal: vi.fn(actual.seal) };
});

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
  return [part(meta.v), part(meta.room), part(meta.epoch), part(meta.kind), part(meta.session), part(meta.command_id)].join("|");
}

function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}

async function importAesKey(raw: Uint8Array, usages: KeyUsage[]): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", toBufferSource(raw), "AES-GCM", false, usages);
}

async function sealIndependent(rawKey: Uint8Array, meta: Meta, plaintext: Uint8Array): Promise<{ ct: string; n: string }> {
  const key = await importAesKey(rawKey, ["encrypt"]);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ciphertext = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: toBufferSource(nonce), additionalData: toBufferSource(utf8Bytes(buildAadIndependent(meta))) },
    key,
    toBufferSource(plaintext),
  );
  return { ct: bytesToBase64(new Uint8Array(ciphertext)), n: bytesToBase64(nonce) };
}

async function openIndependent(rawKey: Uint8Array, meta: Meta, ctB64: string, nB64: string): Promise<Uint8Array> {
  const key = await importAesKey(rawKey, ["decrypt"]);
  const plaintext = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv: toBufferSource(base64Decode(nB64)), additionalData: toBufferSource(utf8Bytes(buildAadIndependent(meta))) },
    key,
    toBufferSource(base64Decode(ctB64)),
  );
  return new Uint8Array(plaintext);
}

async function decryptSentControlFrames(rawKey: Uint8Array, sent: string[]) {
  return Promise.all(sent.map(async (raw) => {
    const envelope = JSON.parse(raw) as Record<string, unknown>;
    const meta: Meta = {
      v: envelope.v as number,
      room: envelope.room as string,
      epoch: envelope.epoch as number,
      kind: envelope.kind as string,
      session: envelope.session as string | null,
      command_id: envelope.command_id as string | null,
    };
    const payload = JSON.parse(new TextDecoder().decode(
      await openIndependent(rawKey, meta, envelope.ct as string, envelope.n as string),
    )) as Record<string, unknown>;
    return { envelope, payload };
  }));
}

function base64Decode(value: string): Uint8Array {
  const binary = atob(value);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

/** Fake relay event frame: independently encrypts payload; seq and client_msg_id stay outside ciphertext. */
async function encryptEventFrame(
  kRoomRaw: Uint8Array,
  params: { session: string | null; seq: number; clientMsgId: string; epoch: number; payload: unknown },
): Promise<Record<string, unknown>> {
  const meta: Meta = { v: 1, room: ROOM, epoch: params.epoch, kind: "event", session: params.session, command_id: null };
  const { ct, n } = await sealIndependent(kRoomRaw, meta, utf8Bytes(JSON.stringify(params.payload)));
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

async function setupPendingHistoryRequest(historyRequestTimeoutMs?: number) {
  const { kRoomRaw, stored } = await makeStoredCredentials();
  const keyStore = new InMemoryKeyStore();
  await keyStore.saveKeys(stored);
  const eventStore = new IndexedDbEventStore(`apprt-history-retry-${crypto.randomUUID()}`);
  const factory = new FakeWebSocketFactory();
  render(
    <AppRuntime
      stored={stored}
      keyStore={keyStore}
      webSocketFactory={factory.factory}
      eventStore={eventStore}
      onNeedsRepair={() => {}}
      historyRequestTimeoutMs={historyRequestTimeoutMs}
    />,
  );
  await waitFor(() => expect(factory.sockets).toHaveLength(1));
  await act(async () => factory.last.simulateOpen());
  const indexFrame = await encryptEventFrame(kRoomRaw, {
    session: null,
    seq: 1,
    clientMsgId: "idx-history-retry",
    epoch: 0,
    payload: {
      t: "session.index",
      full: true,
      sessions: [
        {
          id: "s-history-retry",
          title: "Retry history",
          repo_id: "repo-a",
          archived: false,
          status: null,
          run_id: null,
          updated_at: 1,
        },
      ],
    },
  });
  await act(async () => {
    factory.last.simulateMessage(indexFrame);
    factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
  });
  fireEvent.click(await screen.findByText("Retry history"));
  await screen.findByTestId("session-stream-screen");
  await waitFor(() => expect(factory.last.sent).toHaveLength(2));
  const controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
  const history = controls.find((entry) => entry.payload.t === "control.history")!;
  return { factory, kRoomRaw, historyCommandId: history.envelope.command_id as string };
}

describe("AppRuntime · snapshot/history 密封竞态", () => {
  it("control.snapshot 密封期间 epoch 变更——发送前复核拦截旧 epoch 信封", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-snapshot-race-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();
    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());
    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null, seq: 1, clientMsgId: "idx-snapshot-race", epoch: 0,
      payload: {
        t: "session.index", full: true,
        sessions: [{ id: "s-snapshot-race", title: "Snapshot race", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1 }],
      },
    });
    await act(async () => factory.last.simulateMessage(indexFrame));
    fireEvent.click(await screen.findByText("Snapshot race"));
    await screen.findByTestId("session-stream-screen");

    const actualSeal = (await vi.importActual<typeof import("../crypto/envelope.ts")>("../crypto/envelope.ts")).seal;
    let signalSealStarted!: () => void;
    const sealStarted = new Promise<void>((resolve) => { signalSealStarted = resolve; });
    let releaseSeal!: () => void;
    const sealGate = new Promise<void>((resolve) => { releaseSeal = resolve; });
    let signalSealFinished!: () => void;
    const sealFinished = new Promise<void>((resolve) => { signalSealFinished = resolve; });
    vi.mocked(seal).mockImplementationOnce(async (...args) => {
      expect(JSON.parse(new TextDecoder().decode(args[2])).t).toBe("control.snapshot");
      signalSealStarted();
      await sealGate;
      const result = await actualSeal(...args);
      signalSealFinished();
      return result;
    });

    await act(async () => factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 }));
    await sealStarted;
    await act(async () => factory.last.simulateMessage({ t: "epoch.changed", epoch: 8, ts: Date.now() }));
    await waitFor(async () => {
      const controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
      expect(controls.some((entry) => entry.payload.t === "control.snapshot" && entry.envelope.epoch === 8)).toBe(true);
    });
    await act(async () => {
      releaseSeal();
      await sealFinished;
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    expect(controls.filter((entry) => entry.payload.t === "control.snapshot").map((entry) => entry.envelope.epoch)).toEqual([8]);
  });

  it("control.history 密封期间 socket 关闭——发送前复核拦截旧连接上的信封", async () => {
    const { factory, kRoomRaw, historyCommandId } = await setupPendingHistoryRequest(60_000);
    await act(async () => factory.last.simulateMessage({ t: "input.ack", command_id: historyCommandId, outcome: "failed" }));
    const retry = await screen.findByTestId("history-load-earlier");
    await screen.findByTestId("history-load-error");

    const actualSeal = (await vi.importActual<typeof import("../crypto/envelope.ts")>("../crypto/envelope.ts")).seal;
    let signalSealStarted!: () => void;
    const sealStarted = new Promise<void>((resolve) => { signalSealStarted = resolve; });
    let releaseSeal!: () => void;
    const sealGate = new Promise<void>((resolve) => { releaseSeal = resolve; });
    vi.mocked(seal).mockImplementationOnce(async (...args) => {
      expect(JSON.parse(new TextDecoder().decode(args[2])).t).toBe("control.history");
      signalSealStarted();
      await sealGate;
      return actualSeal(...args);
    });

    fireEvent.click(retry);
    await sealStarted;
    // Socket becomes CLOSED before the disconnect callback runs, exposing the pre-send validation window.
    factory.last.readyState = ReadyState.CLOSED;
    await act(async () => { releaseSeal(); await sealGate; });

    expect(factory.last.sent).toHaveLength(2);
    expect((await decryptSentControlFrames(kRoomRaw, factory.last.sent)).filter((entry) => entry.payload.t === "control.history")).toHaveLength(1);
    expect(screen.getByTestId("history-load-error").textContent).toBe("Not connected. Unable to load history.");
  });

});
