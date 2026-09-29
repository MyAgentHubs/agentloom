// AppRuntime.e2e.test.tsx — INT1b · 端到端：真 ConnectionSession + 真 IndexedDbEventStore
// （fake-indexeddb）+ 假 relay 帧序（真 AES-256-GCM，独立于生产 crypto/envelope.ts 实现）驱动
// `AppRuntime`：session.index → 会话列表 → 选中 → replay.head 触发 control.snapshot 请求 →
// snapshot 应答 + run.status + live delta 归约 → msg.completed 落库入屏 → 篡改帧被拒。
//
// **假 relay 侧加密独立实现**（不 import `crypto/envelope.ts` 的 `seal`/`open`/`buildAAD`）——同
// `pairing/pairing-session.test.ts`/`pairingTransport.e2e.test.ts` 头注的既有纪律：如果这边也调用
// 被测代码内部用的同一份加密 helper，两边共享的 bug 会互相抵消。这里直接用 WebCrypto
// `subtle.encrypt`/`decrypt` + 手拼 AAD 字符串。

import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach } from "vitest";
import {
  ReadyState,
  type ConnectionSessionPhase,
  type WebSocketCloseInfo,
  type WebSocketFactory,
  type WebSocketLike,
} from "../connection/types.ts";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import { InMemoryKeyStore, importNonExtractableAesGcmKey } from "../store/key-store.ts";
import { InMemoryCommandLedger } from "../store/commandLedger.ts";
import { IndexedDbEventStore } from "../store/indexeddbEventStore.ts";
import type { EventStorePort } from "../store/port.ts";
import { deriveMsgCompletedClientMsgId } from "../events/clientMsgId.ts";
import { Composer } from "../ui/composer/Composer.tsx";
import { AppRuntime } from "./AppRuntime.tsx";
import type { CommandRecord } from "./commandChannel.ts";
import { deriveSendBadge } from "./sendBadge.ts";

afterEach(() => {
  cleanup();
  window.history.replaceState({}, "", "/");
});

const ROOM = "0123456789abcdef0123456789abcdef";

// ============================================================================
// 假 WebSocket（同 pairingTransport.test.ts 的姊妹实现）
// ============================================================================

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
// 独立加密（AES-256-GCM，AAD 手拼——不 import crypto/envelope.ts）
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

/** 假 relay 一条 kind=event 里程碑帧——用假桌面的独立密钥+AAD 加密，`seq`/`client_msg_id` 顶层明文。 */
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

/** 假 relay 一条 kind=live 帧——session 顶层明文，`seq` 在密文体内（帧自己的字段），不是顶层 seq。 */
async function encryptLiveFrame(
  kRoomRaw: Uint8Array,
  params: { session: string; epoch: number; payload: unknown },
): Promise<Record<string, unknown>> {
  const meta: Meta = { v: 1, room: ROOM, epoch: params.epoch, kind: "live", session: params.session, command_id: null };
  const { ct, n } = await sealIndependent(kRoomRaw, meta, utf8Bytes(JSON.stringify(params.payload)));
  return { v: 1, room: ROOM, epoch: params.epoch, kind: "live", session: params.session, command_id: null, seq: null, ct, n, ts: Date.now() };
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

describe("deriveSendBadge · 连接状态诚实化", () => {
  const inFlightRecord: CommandRecord = {
    commandId: "command-1", kind: "input.send", session: "s-1", text: "hello",
    plaintext: { t: "input.send", session: "s-1", text: "hello" },
    createdAt: 123, attempts: 0, retryScheduled: false, status: "sending",
  };

  it.each<ConnectionSessionPhase>(["idle", "connecting", "reconnect_scheduled", "needs_repair", "closed"])(
    "sending/sent 在 phase=%s 时立即派生未连接徽标，open 后恢复原逻辑",
    (phase) => {
      expect(deriveSendBadge(inFlightRecord, phase, "s-1", null as never, "unknown")?.status).toBe("not_connected");
      expect(
        deriveSendBadge({ ...inFlightRecord, status: "sent" }, phase, "s-1", null as never, "unknown")?.status,
      ).toBe("not_connected");
      expect(deriveSendBadge(inFlightRecord, "open", "s-1", null as never, "unknown")?.status).toBe("sending");
    },
  );

  it("未连接派生态在 composer 立即显示消息未送出文案", () => {
    const badge = deriveSendBadge(inFlightRecord, "reconnect_scheduled", "s-1", null as never, "unknown");
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("未连接·消息未送出");
  });

  it("failed ack 的 no_agent reason 被折算进 Composer 徽标", () => {
    const record = { ...inFlightRecord, status: "acked", ackOutcome: "failed", ackReason: "no_agent" } as const;
    const badge = deriveSendBadge(record, "open", "s-1", null as never, "unknown");
    expect(badge).toMatchObject({ status: "failed", reason: "no_agent" });
  });
});
describe("AppRuntime · 端到端（真 ConnectionSession + 真 IndexedDbEventStore + 假 relay 帧序）", () => {
  it("连接 phase 变化驱动列表顶部横幅出现与消失", async () => {
    const { stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-banner-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    expect(screen.getByTestId("connection-banner").getAttribute("data-phase")).toBe("connecting");

    await act(async () => factory.last.simulateOpen());
    expect(screen.queryByTestId("connection-banner")).toBeNull();

    await act(async () => factory.last.simulateClose());
    await waitFor(() => {
      const phase = screen.getByTestId("connection-banner").getAttribute("data-phase");
      // 默认退避仅 1 秒；全量并发测试较慢时可能已经从 scheduled 进入下一轮 connecting。
      expect(["reconnect_scheduled", "connecting"]).toContain(phase);
    });
  });

  // C1-PS（dogfood 修障第二批·手机发消息桌面离线无反馈）：relay 定向回的 presence 快照/广播
  // （`{t:"presence", role:"desktop", event}`）驱动 `desktopPresence` state，phase=open 时额外
  // 渲染一条弱担保横幅——不是连接态本身（连接一直是 open，只是桌面这一端可能不在线）。
  it("C1-PS：presence online→offline→online 横幅出没（phase 全程 open，只是桌面在线态变化）", async () => {
    const { stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-presence-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());
    expect(screen.queryByTestId("connection-banner")).toBeNull(); // phase=open，桌面在线态还是初始 unknown。

    await act(async () => {
      factory.last.simulateMessage({ t: "presence", role: "desktop", event: "online" });
    });
    expect(screen.queryByTestId("connection-banner")).toBeNull(); // 明确 online——仍不渲染。

    await act(async () => {
      factory.last.simulateMessage({ t: "presence", role: "desktop", event: "offline" });
    });
    const offlineBanner = screen.getByTestId("connection-banner");
    expect(offlineBanner.getAttribute("data-desktop-presence")).toBe("offline");
    // 本文件其它端到端用例都不锁定 locale（走 jsdom 默认 navigator.language=en-US 的自动探测），
    // 弱担保横幅同样断言其英文措辞——语言本身不是本单硬验收项，data-desktop-presence 属性已经
    // 精确核对了状态；i18n key parity 由 i18n.test.ts 单独守住。
    expect(offlineBanner.textContent).toContain("Desktop may be offline");

    await act(async () => {
      factory.last.simulateMessage({ t: "presence", role: "desktop", event: "online" });
    });
    expect(screen.queryByTestId("connection-banner")).toBeNull(); // 回到 online——横幅消失。
  });

  it("C1-PS：presence role!=='desktop' 的帧不冒充桌面上下线（不消费别的远端设备的 presence）", async () => {
    const { stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-presence-other-role-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());

    await act(async () => {
      factory.last.simulateMessage({ t: "presence", role: "remote", event: "offline" });
    });
    expect(screen.queryByTestId("connection-banner")).toBeNull();
  });

  it("C1-PS：新连接刚打通后收到的第一条快照就是 offline——横幅立即显示（冷启动即显，不需要先等别的帧）", async () => {
    const { stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-presence-snapshot-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());

    // 模拟 relay 的 sendDesktopPresenceSnapshot：新连接接入后几乎立即定向回一条快照——这里就是
    // 这条连接收到的第一条消息，在任何 session.index/replay.head 之前。
    await act(async () => {
      factory.last.simulateMessage({ t: "presence", role: "desktop", event: "offline" });
    });
    const banner = screen.getByTestId("connection-banner");
    expect(banner.getAttribute("data-desktop-presence")).toBe("offline");
  });

  it("C1-PS：断线后桌面在线态回 unknown——重连相位横幅不再带着上一条连接的旧 presence 判定", async () => {
    const { stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-presence-reset-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());
    await act(async () => {
      factory.last.simulateMessage({ t: "presence", role: "desktop", event: "offline" });
    });
    expect(screen.getByTestId("connection-banner").getAttribute("data-desktop-presence")).toBe("offline");

    await act(async () => factory.last.simulateClose());
    await waitFor(() => {
      const banner = screen.getByTestId("connection-banner");
      // 断线后横幅回落到既有的连接相位提示（data-phase），不再带 data-desktop-presence="offline"
      // 那条弱担保提示——旧连接的 presence 判定对新连接不再权威，等新连接的快照重新确认。
      expect(["reconnect_scheduled", "connecting"]).toContain(banner.getAttribute("data-phase"));
      expect(banner.getAttribute("data-desktop-presence")).not.toBe("offline");
    });
  });

  // C1-RQ（dogfood 修障第二批）：relay `input.relay_queued` 明文帧——composer 徽标应翻成排队语义
  // （不是失败，不带重试按钮，见 Composer.tsx/i18n.ts）。
  it("C1-RQ：发送后收到 relay_queued 明文帧——composer 徽标翻成排队语义，不带重试按钮", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-relay-queued-${crypto.randomUUID()}`);
    const commandLedger = new InMemoryCommandLedger();
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime
        stored={stored}
        keyStore={keyStore}
        webSocketFactory={factory.factory}
        eventStore={eventStore}
        commandLedger={commandLedger}
        onNeedsRepair={() => {}}
      />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-relay-queued",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [
          { id: "s-1", title: "Relay queued session", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 },
        ],
      },
    });
    await act(async () => factory.last.simulateMessage(indexFrame));
    await screen.findByText("Relay queued session");
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");
    await act(async () => factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 }));

    fireEvent.change(screen.getByTestId("composer-input"), { target: { value: "hi while desktop is offline" } });
    fireEvent.click(screen.getByTestId("composer-send"));
    const commandId = await waitFor(() => {
      const inputs = factory.last.sent.map((raw) => JSON.parse(raw) as Record<string, unknown>).filter((row) => row.kind === "input");
      expect(inputs).toHaveLength(1);
      return inputs[0]!.command_id as string;
    });

    await act(async () => {
      factory.last.simulateMessage({ t: "input.relay_queued", command_id: commandId, expires_at: Date.now() + 1_800_000 });
    });

    await waitFor(() => {
      expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("relay_queued");
    });
    // 同上——本文件端到端用例走 jsdom 默认 en-US 自动探测，断言英文措辞；data-status 已精确核对。
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("Queued");
    expect(screen.queryByTestId("composer-send-retry")).toBeNull(); // 消息没丢——不该出现重试按钮。
  });

  it("?debug=1 面板按 processFrame 实际闸门累计 kind/decrypt/parse/missingIds/duplicate/applied", async () => {
    window.history.replaceState({}, "", "/?debug=1#p=untouched-pairing-fragment");
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-debug-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });
    await screen.findByTestId("debug-panel");

    const parseRejected = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "parse-rejected",
      epoch: 0,
      payload: { t: "future.frame" },
    });
    const missingIds = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "missing-ids",
      epoch: 0,
      payload: { t: "session.index", full: true, sessions: [] },
    });
    delete missingIds.seq;
    delete missingIds.client_msg_id;
    const applied = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-debug-1",
      epoch: 0,
      payload: { t: "session.index", full: true, sessions: [] },
    });

    act(() => {
      factory.last.simulateMessage({
        v: 1,
        room: ROOM,
        epoch: 0,
        kind: "presence",
        session: null,
        command_id: null,
        ct: "unused",
        n: "unused",
      });
      factory.last.simulateMessage({
        v: 1,
        room: ROOM,
        epoch: 0,
        kind: "event",
        session: null,
        command_id: null,
        seq: 1,
        client_msg_id: "decrypt-failed",
        ct: "not-base64",
        n: "not-base64",
      });
      factory.last.simulateMessage(parseRejected);
      factory.last.simulateMessage(missingIds);
      factory.last.simulateMessage(applied);
      factory.last.simulateMessage(applied);
    });

    await waitFor(() => expect(screen.getByTestId("debug-storeDuplicate").textContent).toBe("1"));
    expect(screen.getByTestId("debug-framesSeen").textContent).toBe("6");
    expect(screen.getByTestId("debug-kindSkipped").textContent).toBe("1");
    expect(screen.getByTestId("debug-decryptFailed").textContent).toBe("1");
    expect(screen.getByTestId("debug-parseFailed").textContent).toBe("1");
    expect(screen.getByTestId("debug-parseReason").textContent).toBe("unknown_t");
    expect(screen.getByTestId("debug-missingIds").textContent).toBe("1");
    expect(screen.getByTestId("debug-applied").textContent).toBe("1");
    expect(screen.getByTestId("debug-lastDrop").textContent).toBe("session.index / event");
    expect(window.location.hash).toBe("#p=untouched-pairing-fragment");
  });

  it("eventStore 写入抛错时计入 storeError、保留错误文本并 forceRender 到 DebugPanel；msgfix2 F2 S3：还要留一条 console.error 可见信号（不是只更新一个只有开着 DebugPanel 才看得到的计数器——同一单三个 IndexedDB 实现 onversionchange 触发的 InvalidStateError 也走这条路径，旧版完全静默，历史清空零信号）", async () => {
    window.history.replaceState({}, "", "/?debug=1");
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore: EventStorePort = {
      applyEventIfNew: async () => {
        throw new Error("transaction aborted: quota exceeded");
      },
      getWatermark: async () => 0,
      hasAppliedClientMsgId: async () => false,
      listEvents: async () => [],
    };
    const factory = new FakeWebSocketFactory();
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());
    expect(screen.getByTestId("debug-storeError").textContent).toBe("0");
    expect(consoleErrorSpy).not.toHaveBeenCalled();

    const frame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "store-failure",
      epoch: 0,
      payload: { t: "session.index", full: true, sessions: [] },
    });
    await act(async () => factory.last.simulateMessage(frame));

    await waitFor(() => expect(screen.getByTestId("debug-storeError").textContent).toBe("1"));
    expect(screen.getByTestId("debug-applied").textContent).toBe("0");
    expect(screen.getByTestId("debug-lastDrop").textContent).toBe("session.index / event / transaction aborted: quota exceeded");
    // 核心断言：不依赖用户开着 `?debug=1` DebugPanel 才能发现——console.error 里带着可辨认的错误
    // 文本，随时能在真机 devtools console 里看到。
    const loggedStoreError = consoleErrorSpy.mock.calls.some((call) =>
      call.some((arg) => typeof arg === "string" && arg.includes("transaction aborted: quota exceeded")),
    );
    expect(loggedStoreError).toBe(true);

    consoleErrorSpy.mockRestore();
  });

  it("session.index → 选会话 → replay.head 触发 control.snapshot → snapshot/run.status/live/msg.completed 归约入屏", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-test-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );

    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    expect(factory.last.url).toBe(`wss://relay.example/room/${ROOM}?last_seq=0`);
    await act(async () => {
      factory.last.simulateOpen();
    });

    // ---- session.index full：会话列表出现 ----
    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-1",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-1", title: "Fix login bug", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Fix login bug");

    // ---- 选中会话 ----
    const row = screen.getByTestId("session-row");
    fireEvent.click(row);
    await screen.findByTestId("session-stream-screen");

    // ---- replay.head → 应该发出一条 control.snapshot 请求（真 K_room seal，command_id 顶层 AAD）----
    await act(async () => {
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });
    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    const controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const outEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot")!.envelope;
    expect(outEnvelope.kind).toBe("control");
    expect(outEnvelope.session).toBe("s-1");
    expect(outEnvelope.epoch).toBe(5);
    expect(outEnvelope.seq).toBeNull();
    expect(Object.prototype.hasOwnProperty.call(outEnvelope, "client_msg_id")).toBe(false);
    const commandId = outEnvelope.command_id as string;
    expect(commandId).toMatch(/^[0-9a-f-]{36}$/);
    const outMeta: Meta = { v: 1, room: ROOM, epoch: 5, kind: "control", session: "s-1", command_id: commandId };
    const outPlain = JSON.parse(
      new TextDecoder().decode(await openIndependent(kRoomRaw, outMeta, outEnvelope.ct as string, outEnvelope.n as string)),
    );
    expect(outPlain).toEqual({ t: "control.snapshot", session: "s-1" });

    // ---- snapshot 应答 + run.status + live delta + msg.completed：连续无等待投递（INT1c P0①
    // 入站严格串行队列的直接验证——不在中间插一个 `waitFor`/`findBy*` 手动等前一条处理完再发下一
    // 条，全部 `simulateMessage()` 背靠背同步触发。若 `handleFrame` 各自并发处理（旧实现），
    // run.status 与它之后紧跟的 live 帧几乎必然交错——live 帧到达时 `runStatusBySession` 还没被
    // run.status 那条异步链写入，会被"查不到 runId 就丢弃、不瞎猜"规则吃掉，"partial seed more
    // text"永远拼不出来。有了串行队列，不管发送方等不等、发多快，处理顺序恒等于到达顺序。----
    const snapshotFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "snap-1",
      epoch: 5,
      payload: { t: "snapshot", session: "s-1", run_id: "run-1", through_run_seq: 1, partial_msg: { role: "assistant", blocks: [{ type: "text", text: "partial seed" }] } },
    });
    const runStatusFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 3,
      clientMsgId: "run-1",
      epoch: 5,
      payload: { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" },
    });
    const liveFrame = await encryptLiveFrame(kRoomRaw, {
      session: "s-1",
      epoch: 5,
      payload: { t: "text_delta", seq: 2, text: " more text" },
    });
    const msgFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 4,
      clientMsgId: "msg-1",
      epoch: 5,
      payload: { t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "Final message from s-1" }] },
    });
    act(() => {
      // 故意不 await、不在四条之间穿插任何等待——四次 simulateMessage() 同步背靠背触发。
      factory.last.simulateMessage(snapshotFrame);
      factory.last.simulateMessage(runStatusFrame);
      factory.last.simulateMessage(liveFrame);
      factory.last.simulateMessage(msgFrame);
    });

    // 最终状态一次性核验——顺序与完整性：live 归约正确接上了 snapshot 的种子文本（证明 snapshot
    // 先于 live 落地）、状态条显示 running（证明 run.status 先于 live 落地）、msg.completed 正常
    // 渲染（证明它在 live 之后仍被正确处理，串行不等于"后来的被饿死"）。
    await waitFor(() => expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Running|运行中/));
    const liveRow = await screen.findByTestId("stream-live-message");
    await within(liveRow).findByText("partial seed more text");
    await screen.findByText("Final message from s-1");

    // ---- 真的落库了，不是只在内存里"看起来"生效 ----
    const persisted = await eventStore.listEvents();
    const clientMsgIds = persisted.map((row) => row.clientMsgId).sort();
    expect(clientMsgIds).toEqual(["idx-1", "msg-1", "run-1", "snap-1"]);
    expect(await eventStore.getWatermark()).toBe(4);

    // ---- 重复投递（at-least-once 重投）幂等：同一条 msg.completed 再来一次不重复渲染/不二次落库 ----
    await act(async () => {
      factory.last.simulateMessage(msgFrame);
    });
    await waitFor(() => expect(screen.getAllByText("Final message from s-1")).toHaveLength(1));
    expect((await eventStore.listEvents()).length).toBe(4);

    // ---- 篡改的 msg.completed 密文（AEAD 认证失败）被丢弃，不崩溃、不落库 ----
    const tamperedMsgFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 5,
      clientMsgId: "msg-2",
      epoch: 5,
      payload: { t: "msg.completed", message_id: 2, role: "assistant", blocks: [{ type: "text", text: "should never appear" }] },
    });
    const tamperedCt = base64Decode(tamperedMsgFrame.ct as string);
    tamperedCt[0] = tamperedCt[0]! ^ 0xff;
    tamperedMsgFrame.ct = bytesToBase64(tamperedCt);
    await act(async () => {
      factory.last.simulateMessage(tamperedMsgFrame);
    });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(screen.queryByText("should never appear")).toBeNull();
    expect((await eventStore.listEvents()).length).toBe(4); // 未新增
  });

  // Wiring-level test: proves the production call chain threads a `session.index` row's status
  // into the header, rather than reading `sessionProjection.sessions` (always an empty Map, see
  // `streamSource.ts`'s header: `session.index` only applies to the room-level
  // `core.indexProjection`, never per-session `core.sessionProjections`). This test only sends
  // `session.index` and never `run.status`/`msg.completed`/`card.*`/`tool.completed` —
  // `core.sessionProjections` has no entry for this session, so if the header still shows Running,
  // it can only come from the real `core.indexProjection.sessions` data path.
  it("idlefix-T1 补针 A：session.index 行 status=running 且该会话从未收到过 run.status → 顶栏仍显示 Running（真接线，不是只有 streamSource 单测才过）", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-test-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );

    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    // ---- session.index full：这条会话行本身携带 status="running"（连接快照里的现状），不是
    // run.status 里程碑。----
    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-running-1",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [
          {
            id: "s-1",
            title: "Running from index",
            repo_id: "repo-a",
            archived: false,
            status: "running",
            run_id: "run-1",
            updated_at: 1000,
          },
        ],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Running from index");

    // ---- 选中会话——故意不发 run.status/msg.completed/card.*/tool.completed 帧，这个会话在
    // `core.sessionProjections` 里没有条目。----
    const row = screen.getByTestId("session-row");
    fireEvent.click(row);
    await screen.findByTestId("session-stream-screen");

    await waitFor(() => expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Running|运行中/));
  });

  it("U2 根修（症状①手机端 typing 恒显）：idle snapshot 到达不显示 typing；run 活跃时显示；msg.completed 终态清残留后 typing 立即消失", async () => {
    // `?debug=1` 的 `debug-applied` 计数器（`recordFrameApplied`，`applyDecryptedMilestoneFrame`/
    // `applyDecryptedLiveFrame` 成功时同步递增、随即 `forceRender()`）是本测试唯一的同步点——每发
    // 一条帧后 `waitFor` 这个计数器涨到预期值，保证断言发生在"这条帧的归约 + React 重渲染都已提交"
    // 之后，不靠 `act(async…)` 的隐式微任务刷新去赌时序（那对"元素应当消失"这类否定式断言不可靠：
    // 如果处理还没发生，`queryByTestId` 找不到元素并不能证明修复生效，只能证明还没处理完）。
    window.history.replaceState({}, "", "/?debug=1");
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-u2-typing-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-u2-typing",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-1", title: "Typing regression", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    act(() => {
      factory.last.simulateMessage(indexFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("1"));
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    // ---- 症状①核心复现：idle 快照（三 null）到达——旧实现的判据是"runTracks 有没有这个会话的
    // 条目"，`applySnapshot` 的 idle 分支会写入一条带全新空 `LiveBlockReducer` 的记录，
    // `snapshotBlocks()` 返回 `[]`（非 null），typing 气泡会永久出现。修复后 `liveReducer` 的判据
    // 是 `runId !== null`，idle 快照令 `runId` 保持 null，不应该渲染 live message row。----
    const idleSnapshotFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "snap-idle",
      epoch: 0,
      payload: { t: "snapshot", session: "s-1", run_id: null, through_run_seq: null, partial_msg: null },
    });
    act(() => {
      factory.last.simulateMessage(idleSnapshotFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("2"));
    expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Idle|空闲/);
    expect(screen.queryByTestId("stream-live-message")).toBeNull();
    expect(screen.queryByTestId("stream-typing-indicator")).toBeNull();

    // ---- run 变为活跃：run.status running → typing 仍不该显示（还没有任何 live delta 内容），
    // 紧接一条 live text_delta → typing 正常出现 ----
    const runStatusFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 3,
      clientMsgId: "run-u2-1",
      epoch: 0,
      payload: { t: "run.status", session_id: "s-1", status: "running", run_id: "run-u2-1" },
    });
    act(() => {
      factory.last.simulateMessage(runStatusFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("3"));
    expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Running|运行中/);

    const liveFrame = await encryptLiveFrame(kRoomRaw, {
      session: "s-1",
      epoch: 0,
      payload: { t: "text_delta", seq: 1, text: "working on it" },
    });
    act(() => {
      factory.last.simulateMessage(liveFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("4"));
    const liveRow = screen.getByTestId("stream-live-message");
    await within(liveRow).findByText("working on it");
    expect(within(liveRow).getByTestId("stream-typing-indicator")).toBeTruthy();

    // ---- run 终态：msg.completed 落成一条真正消息 → 残留 partial 被清、typing 立即消失 ----
    const msgCompletedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 4,
      clientMsgId: "msg-u2-1",
      epoch: 0,
      payload: { t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "Done working on it" }] },
    });
    act(() => {
      factory.last.simulateMessage(msgCompletedFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("5"));
    expect(screen.getByText("Done working on it")).toBeTruthy();
    expect(screen.queryByTestId("stream-live-message")).toBeNull();
    expect(screen.queryByTestId("stream-typing-indicator")).toBeNull();
  });

  it("U2 修复轮（独立审查定罪·判据钉反）：无 msg.completed，仅收到 run.status 转 idle 且沿用旧 run_id（生产真实终态帧形态）→ typing 立即消失", async () => {
    // 定罪依据：桌面 Rust 侧 refresh_session_runtime → upsert_session_runtime_status 这条"run 结束/
    // 摘槽"路径刻意沿用写前读到的旧 run_id（db.rs:13168），发布形如 `{status:"idle", run_id:"run-1"}`
    // ——从不是 `run_id:null`。唯一发 `run_id:null` 的生产帧是 solo 开跑帧 `{status:"running",
    // run_id:null}`。旧判据 `frame.run_id === null` 在这条真实终态帧上永不触发，中断/报错这类没有
    // msg.completed 收尾的 run 会让 typing 永久挂着；本用例钉死修复后的正确行为。
    window.history.replaceState({}, "", "/?debug=1");
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-u2-terminal-status-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-u2-terminal",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-1", title: "Terminal status regression", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    act(() => {
      factory.last.simulateMessage(indexFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("1"));
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    // ---- run 变为活跃 + 一条 live delta → typing 出现 ----
    const runStatusRunningFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "run-term-1",
      epoch: 0,
      payload: { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" },
    });
    act(() => {
      factory.last.simulateMessage(runStatusRunningFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("2"));

    const liveFrame = await encryptLiveFrame(kRoomRaw, {
      session: "s-1",
      epoch: 0,
      payload: { t: "text_delta", seq: 1, text: "still working" },
    });
    act(() => {
      factory.last.simulateMessage(liveFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("3"));
    const liveRow = screen.getByTestId("stream-live-message");
    await within(liveRow).findByText("still working");
    expect(within(liveRow).getByTestId("stream-typing-indicator")).toBeTruthy();

    // ---- 终态：无 msg.completed，只收到 run.status 转 idle、沿用旧 run_id（生产真实形态）----
    const runStatusIdleFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 3,
      clientMsgId: "run-term-2",
      epoch: 0,
      payload: { t: "run.status", session_id: "s-1", status: "idle", run_id: "run-1" },
    });
    act(() => {
      factory.last.simulateMessage(runStatusIdleFrame);
    });
    await waitFor(() => expect(screen.getByTestId("debug-applied").textContent).toBe("4"));
    expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Idle|空闲/);
    expect(screen.queryByTestId("stream-live-message")).toBeNull();
    expect(screen.queryByTestId("stream-typing-indicator")).toBeNull();
  });

  it("queued 徽标按实时 msg.completed 撤销；真正重载时 records 为空、持久事件只重建投影", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const dbName = `apprt-delivery-${crypto.randomUUID()}`;
    const eventStore = new IndexedDbEventStore(dbName);
    const commandLedger = new InMemoryCommandLedger();
    const factory = new FakeWebSocketFactory();

    const first = render(
      <AppRuntime
        stored={stored}
        keyStore={keyStore}
        webSocketFactory={factory.factory}
        eventStore={eventStore}
        commandLedger={commandLedger}
        onNeedsRepair={() => {}}
      />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());

    const indexPayload = {
      t: "session.index",
      full: true,
      sessions: [{ id: "s-1", title: "Delivery receipt", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
    };
    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-delivery",
      epoch: 0,
      payload: indexPayload,
    });
    await act(async () => factory.last.simulateMessage(indexFrame));
    await screen.findByText("Delivery receipt");
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");
    await act(async () => factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 }));
    await waitFor(() => expect(factory.last.sent.some((raw) => JSON.parse(raw).kind === "control")).toBe(true));

    const sendAndQueue = async (text: string, expectedInputCount: number): Promise<string> => {
      fireEvent.change(screen.getByTestId("composer-input"), { target: { value: text } });
      fireEvent.click(screen.getByTestId("composer-send"));
      const commandId = await waitFor(() => {
        const inputs = factory.last.sent.map((raw) => JSON.parse(raw) as Record<string, unknown>).filter((row) => row.kind === "input");
        expect(inputs).toHaveLength(expectedInputCount);
        return inputs.at(-1)!.command_id as string;
      });
      await act(async () => factory.last.simulateMessage({ t: "input.ack", command_id: commandId, outcome: "queued" }));
      await waitFor(() => expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("queued"));
      return commandId;
    };

    // 实时接受路径：事件先持久化并进入投影，再用外层 client_msg_id 兑付 queued 命令。
    const liveCommandId = await sendAndQueue("live receipt", 1);
    const liveCompletedPayload = {
      t: "msg.completed",
      message_id: 1,
      role: "user",
      blocks: [{ type: "text", text: "Live receipt persisted" }],
    };
    const liveCompletedId = deriveMsgCompletedClientMsgId("s-1", liveCommandId);
    const liveCompletedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: liveCompletedId,
      epoch: 5,
      payload: liveCompletedPayload,
    });
    await act(async () => factory.last.simulateMessage(liveCompletedFrame));
    await screen.findByText("Live receipt persisted");
    await waitFor(() => expect(screen.queryByTestId("composer-send-badge")).toBeNull());

    // 再把一条完成事件直接写进同一 IndexedDB，但不经当前 websocket；真正卸载后，新建
    // AppRuntime/CommandChannel 的 records 恒为空，冷启动只从事件日志重建消息投影。
    const replayCommandId = "command-from-before-reload";
    const replayCompletedPayload = {
      t: "msg.completed",
      message_id: 2,
      role: "user",
      blocks: [{ type: "text", text: "Replay receipt persisted" }],
    };
    await eventStore.applyEventIfNew({
      clientMsgId: deriveMsgCompletedClientMsgId("s-1", replayCommandId),
      seq: 3,
      session: "s-1",
      frame: replayCompletedPayload,
    });

    first.unmount();
    cleanup();
    const reloadedFactory = new FakeWebSocketFactory();
    render(
      <AppRuntime
        stored={stored}
        keyStore={keyStore}
        webSocketFactory={reloadedFactory.factory}
        eventStore={new IndexedDbEventStore(dbName)}
        commandLedger={commandLedger}
        onNeedsRepair={() => {}}
      />,
    );
    await screen.findByText("Delivery receipt");
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByText("Live receipt persisted");
    await screen.findByText("Replay receipt persisted");
    expect(screen.queryByTestId("composer-send-badge")).toBeNull();

    // 只有实时观察点兑付过的命令会写成 ok；冷启动重放不会臆造或恢复 CommandChannel records。
    expect((await commandLedger.get(liveCommandId))?.status).toBe("ok");
    expect(await commandLedger.get(replayCommandId)).toBeNull();

    // 重载后的 CommandChannel records 为空；再到一条新的、确定性形状的 msg.completed 时，观察点
    // 安全 no-op、不崩溃，同时事件仍正常落库入投影，也不会凭空产生徽标。
    await waitFor(() => expect(reloadedFactory.sockets).toHaveLength(1));
    await act(async () => reloadedFactory.last.simulateOpen());
    const lateCompletedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 4,
      clientMsgId: deriveMsgCompletedClientMsgId("s-1", "another-command-from-before-reload"),
      epoch: 5,
      payload: {
        t: "msg.completed",
        message_id: 3,
        role: "user",
        blocks: [{ type: "text", text: "Late receipt after reload" }],
      },
    });
    await act(async () => reloadedFactory.last.simulateMessage(lateCompletedFrame));
    await screen.findByText("Late receipt after reload");
    expect(screen.queryByTestId("composer-send-badge")).toBeNull();
    expect((await commandLedger.get(liveCommandId))?.status).toBe("ok");
  });

  it("replay.head 先于任何会话选中到达 → 不发请求；随后选中会话 → 立即补发（INT1c P0②「待补发」标记）", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-test-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-1",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-1", title: "Fix login bug", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Fix login bug");

    // replay.head 到达——此刻还没有选中任何会话。
    await act(async () => {
      factory.last.simulateMessage({ t: "replay.head", epoch: 9, headSeq: 1 });
    });
    // 给尚未落地的异步处理留一个 tick——即便如此也不该发出任何 control.snapshot 请求（没有对象）。
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(factory.last.sent).toHaveLength(0);

    // 现在选中会话——「待补发」标记生效，立即补发一条 control.snapshot 请求。
    const row = screen.getByTestId("session-row");
    fireEvent.click(row);
    await screen.findByTestId("session-stream-screen");

    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    const controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const outEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot")!.envelope;
    expect(outEnvelope.kind).toBe("control");
    expect(outEnvelope.session).toBe("s-1");
    expect(outEnvelope.epoch).toBe(9); // 用的是先前 replay.head 带来的 epoch，不是某个默认值。
    const commandId = outEnvelope.command_id as string;
    const outMeta: Meta = { v: 1, room: ROOM, epoch: 9, kind: "control", session: "s-1", command_id: commandId };
    const outPlain = JSON.parse(
      new TextDecoder().decode(await openIndependent(kRoomRaw, outMeta, outEnvelope.ct as string, outEnvelope.n as string)),
    );
    expect(outPlain).toEqual({ t: "control.snapshot", session: "s-1" });
  });

  it("发出 control.snapshot 请求后收到 epoch.changed → 用同一个 command_id 按新 epoch 重封重发（INT1c P0② onEpochChanged 挂点）", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-test-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-1",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-1", title: "Fix login bug", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Fix login bug");
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    // 首次 replay.head（epoch=5）→ 发出第一条请求。
    await act(async () => {
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });
    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    let controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const firstEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot" && entry.envelope.epoch === 5)!.envelope;
    expect(firstEnvelope.epoch).toBe(5);
    const commandId = firstEnvelope.command_id as string;

    // 桌面重连、relay 广播 epoch.changed——在这条请求还没收到 snapshot 应答之前 epoch 变了。
    await act(async () => {
      factory.last.simulateMessage({ t: "epoch.changed", epoch: 8, ts: Date.now() });
    });

    await waitFor(() => expect(factory.last.sent).toHaveLength(4));
    controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const secondEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot" && entry.envelope.epoch === 8)!.envelope;
    expect(secondEnvelope.epoch).toBe(8);
    expect(secondEnvelope.session).toBe("s-1");
    // 同一个 command_id——这是"重封重发同一个未完成的请求"，不是发起了第二个独立请求。
    expect(secondEnvelope.command_id).toBe(commandId);
    const secondMeta: Meta = { v: 1, room: ROOM, epoch: 8, kind: "control", session: "s-1", command_id: commandId };
    const secondPlain = JSON.parse(
      new TextDecoder().decode(await openIndependent(kRoomRaw, secondMeta, secondEnvelope.ct as string, secondEnvelope.n as string)),
    );
    expect(secondPlain).toEqual({ t: "control.snapshot", session: "s-1" });

    // 收到该会话的 snapshot 应答后，再来一次 epoch.changed 不应该再重发（在飞请求已被兑付、清空）。
    const snapshotFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "snap-1",
      epoch: 8,
      payload: { t: "snapshot", session: "s-1", run_id: "run-1", through_run_seq: 1, partial_msg: null },
    });
    await act(async () => {
      factory.last.simulateMessage(snapshotFrame);
    });
    // `partial_msg: null` 仍然会让 `liveBlocks` 变成非 null 的空数组（"已纳入事件但暂无可显示
    // 内容"，见 runWatermark.ts 头注）——`stream-live-message` 会渲染出来，用它确认这条 snapshot
    // 真的已经跑完串行队列、落地生效，不是靠一个恒真断言假装等到了。
    await screen.findByTestId("stream-live-message");

    await act(async () => {
      factory.last.simulateMessage({ t: "epoch.changed", epoch: 11, ts: Date.now() });
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    expect(controls.filter((entry) => entry.payload.t === "control.snapshot")).toHaveLength(2);
  });

  it("FIX2 P1-2：发出 control.snapshot 请求后收到 stale_epoch 拒绝 → 用信封里 currentEpoch 重新密封同一个 command_id 再发一次（不是只有 composer 指令才补发，snapshot 请求同待遇）", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-test-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-1",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-1", title: "Fix login bug", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Fix login bug");
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    // 首次 replay.head（epoch=5）→ 发出第一条请求。
    await act(async () => {
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });
    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    let controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const firstEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot" && entry.envelope.epoch === 5)!.envelope;
    expect(firstEnvelope.epoch).toBe(5);
    const commandId = firstEnvelope.command_id as string;

    // relay 拒绝：stale_epoch（没有 command_id，权威新 epoch 靠 currentEpoch 字段）——这条请求
    // 还没收到 snapshot 应答之前 epoch 就被判过时了。
    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "stale_epoch", currentEpoch: 9 });
    });

    await waitFor(() => expect(factory.last.sent).toHaveLength(4));
    controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const secondEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot" && entry.envelope.epoch === 9)!.envelope;
    expect(secondEnvelope.epoch).toBe(9);
    expect(secondEnvelope.session).toBe("s-1");
    // 同一个 command_id——重封重发同一个未完成的请求，不是发起了第二个独立请求。
    expect(secondEnvelope.command_id).toBe(commandId);
    const secondMeta: Meta = { v: 1, room: ROOM, epoch: 9, kind: "control", session: "s-1", command_id: commandId };
    const secondPlain = JSON.parse(
      new TextDecoder().decode(await openIndependent(kRoomRaw, secondMeta, secondEnvelope.ct as string, secondEnvelope.n as string)),
    );
    expect(secondPlain).toEqual({ t: "control.snapshot", session: "s-1" });
  });

  it("FIX2 P2-5：snapshot 请求被 control_rate_limited 拒绝后不悬死——pendingSnapshotRequestRef 保留同一个 command_id，下一次 epoch.changed 触发时仍能正常重发", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-test-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-1",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-1", title: "Fix login bug", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Fix login bug");
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    await act(async () => {
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });
    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    let controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const firstEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot" && entry.envelope.epoch === 5)!.envelope;
    const commandId = firstEnvelope.command_id as string;

    // relay 拒绝：control_rate_limited（带 command_id——占用了本地/relay 同一条 "control" 桶的
    // snapshot 请求撞上了限速）。这条帧本身不该让 AppRuntime 崩溃，也不该让 pendingSnapshotRequestRef
    // 悬死（悬死 = 以为还在等回应，其实永远等不到，也永远不会再重发）。
    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "control_rate_limited", frame: "control", command_id: commandId });
    });
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(factory.last.sent).toHaveLength(2); // 没有因为处理这条拒绝帧而误发任何东西。

    // 下一次自然触发（桌面重连广播 epoch.changed）——pendingSnapshotRequestRef 仍然指向同一个
    // command_id，照常按新 epoch 重封重发，证明没有卡死。
    await act(async () => {
      factory.last.simulateMessage({ t: "epoch.changed", epoch: 7, ts: Date.now() });
    });
    await waitFor(() => expect(factory.last.sent).toHaveLength(4));
    controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const secondEnvelope = controls.find((entry) => entry.payload.t === "control.snapshot" && entry.envelope.epoch === 7)!.envelope;
    expect(secondEnvelope.epoch).toBe(7);
    expect(secondEnvelope.command_id).toBe(commandId);
  });

  it("空会话自动拉最新历史；live history 无 runId 入屏；加载更早沿游标请求；stale_epoch 同 command_id 重封", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-history-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-history",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [{ id: "s-history", title: "History session", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 }],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });
    await screen.findByText("History session");
    fireEvent.click(screen.getByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    let controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const firstHistory = controls.find((entry) => entry.payload.t === "control.history")!;
    expect(firstHistory.payload).toEqual({ t: "control.history", session: "s-history", before_message_id: null });
    const historyCommandId = firstHistory.envelope.command_id;

    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "stale_epoch", currentEpoch: 8 });
    });
    await waitFor(() => expect(factory.last.sent).toHaveLength(4));
    controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const resealedHistory = controls.find(
      (entry) => entry.payload.t === "control.history" && entry.envelope.epoch === 8,
    )!;
    expect(resealedHistory.envelope.command_id).toBe(historyCommandId);
    expect(resealedHistory.payload.before_message_id).toBeNull();

    const newestPage = await encryptLiveFrame(kRoomRaw, {
      session: "s-history",
      epoch: 8,
      payload: {
        t: "history",
        session: "s-history",
        before_message_id: null,
        messages: [{ message_id: 119, role: "assistant", blocks: [{ type: "text", text: "Newest history message" }] }],
        next_before: 101,
      },
    });
    await act(async () => factory.last.simulateMessage(newestPage));
    await screen.findByText("Newest history message");
    const loadEarlier = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(loadEarlier.disabled).toBe(false);

    fireEvent.click(loadEarlier);
    expect((screen.getByTestId("history-load-earlier") as HTMLButtonElement).disabled).toBe(true);
    await waitFor(() => expect(factory.last.sent).toHaveLength(5));
    controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const olderRequest = controls.find(
      (entry) => entry.payload.t === "control.history" && entry.payload.before_message_id === 101,
    );
    expect(olderRequest?.envelope.command_id).not.toBe(historyCommandId);
  });

  it("已有至少 5 条消息的会话可手动加载更早页，首次请求以本地最小 message_id 为游标；断线点击有明确反馈", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-existing-history-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-existing-history",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [
          {
            id: "s-existing-history",
            title: "Existing history session",
            repo_id: "repo-a",
            archived: false,
            status: null,
            run_id: null,
            updated_at: 1000,
          },
        ],
      },
    });
    const knownMessageIds = [110, 106, 109, 108, 107];
    const messageFrames = await Promise.all(
      knownMessageIds.map((messageId, index) =>
        encryptEventFrame(kRoomRaw, {
          session: "s-existing-history",
          seq: index + 2,
          clientMsgId: `existing-message-${messageId}`,
          epoch: 0,
          payload: {
            t: "msg.completed",
            message_id: messageId,
            role: "assistant",
            blocks: [{ type: "text", text: `Known message ${messageId}` }],
          },
        }),
      ),
    );
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
      for (const frame of messageFrames) factory.last.simulateMessage(frame);
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 6 });
    });
    await waitFor(async () => expect(await eventStore.listEvents()).toHaveLength(6));

    fireEvent.click(await screen.findByText("Existing history session"));
    await screen.findByText("Known message 106");
    await waitFor(() => expect(factory.last.sent).toHaveLength(1));
    const loadEarlier = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(loadEarlier.disabled).toBe(false);

    fireEvent.click(loadEarlier);
    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    let controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const firstManualHistory = controls.find((entry) => entry.payload.t === "control.history")!;
    expect(firstManualHistory.payload.before_message_id).toBe(106);

    const olderPage = await encryptLiveFrame(kRoomRaw, {
      session: "s-existing-history",
      epoch: 5,
      payload: {
        t: "history",
        session: "s-existing-history",
        before_message_id: 106,
        messages: [{ message_id: 105, role: "assistant", blocks: [{ type: "text", text: "Older page message" }] }],
        next_before: 100,
      },
    });
    await act(async () => factory.last.simulateMessage(olderPage));
    await screen.findByText("Older page message");

    const connectedSocket = factory.last;
    fireEvent.click(screen.getByTestId("history-load-earlier"));
    expect((screen.getByTestId("history-load-earlier") as HTMLButtonElement).disabled).toBe(true);
    await act(async () => connectedSocket.simulateClose());
    expect((await screen.findByTestId("history-load-error")).textContent).toBe(
      "Not connected. Unable to load history.",
    );
    expect((screen.getByTestId("history-load-earlier") as HTMLButtonElement).textContent).toBe("Retry");
    controls = await decryptSentControlFrames(kRoomRaw, connectedSocket.sent);
    expect(controls.filter((entry) => entry.payload.t === "control.history")).toHaveLength(1);

    fireEvent.click(screen.getByTestId("history-load-earlier"));
    expect(screen.getByTestId("history-load-error").textContent).toBe("Not connected. Unable to load history.");
    controls = await decryptSentControlFrames(kRoomRaw, connectedSocket.sent);
    expect(controls.filter((entry) => entry.payload.t === "control.history")).toHaveLength(1);
  });

  it("control.history 15s 有界超时后清 pending、显示错误并允许首屏手动重试", async () => {
    const { factory, kRoomRaw, historyCommandId } = await setupPendingHistoryRequest(30);

    await screen.findByTestId("history-load-error");
    expect(screen.getByTestId("history-load-error").textContent).toContain("timed out");
    const retry = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(retry.disabled).toBe(false);
    expect(retry.textContent).toBe("Retry");

    fireEvent.click(retry);
    await waitFor(() => expect(factory.last.sent).toHaveLength(3));
    const controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const retried = controls.filter((entry) => entry.payload.t === "control.history").at(-1)!;
    expect(retried.payload.before_message_id).toBeNull();
    expect(retried.envelope.command_id).not.toBe(historyCommandId);
  });

  it("U3：loading 态经 historyRevision state 立即反映在 DOM 上；连续多次 resend（stale_epoch ×2 + epoch.changed）不丢失原始 timeout——到点仍清 pending 回 Retry", async () => {
    // 250ms——比"发出首个 control.history 请求"实际耗费的真实时间（`setupPendingHistoryRequest`
    // 内部靠 `waitFor` 轮询确认，通常个位数毫秒到几十毫秒量级）留足余量，避免测试本身在
    // "还没来得及断言 Loading… 就已经先超时"上出现假红；同时仍然远小于 `findByTestId` 默认的
    // 1000ms 轮询上限，保证下面等超时触发那段不会真的等到超时。
    const { factory } = await setupPendingHistoryRequest(250);

    // 请求已发出、还悬着等回应——按钮此刻必须已经显示 "Loading…"（`historyLoading` 现在经
    // `historyRevision` state 门控，不是"读一次 ref 侥幸对上 forceRender 时机"；`requestHistory`
    // 创建 pending 时同步 bump 过，这次渲染必然已经反映出来）。
    const loadingButton = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(loadingButton.disabled).toBe(true);
    expect(loadingButton.textContent).toBe("Loading…");

    const sentBefore = factory.last.sent.length;
    // 在原超时到期前连续触发三次"重发"信号——`sendHistoryRequest` 每次重入都会各自起一条独立的
    // fire-and-forget 异步链（trySendControlSlot/seal 都是真异步），彼此交错完成的顺序不保证。
    // U3 修复前，timeout 的武装责任散落在每次重发尝试自己身上（先清旧、后武装新），一旦某次尝试
    // 在"更新的一次重发已经超车"分支静默 return，就再也没人替它武装新计时器——pending 从此无超时
    // 兜底，Loading… 永久挂着。U3 修复后 timeout 只在 `requestHistory` 创建 pending 时武装一次、
    // 只在 `clearHistoryPending` 清——不管这里重发几次、以什么顺序完成，原计时器岿然不动，到点
    // 必触发。
    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "stale_epoch", currentEpoch: 6 });
      factory.last.simulateMessage({ t: "epoch.changed", epoch: 7, ts: Date.now() });
      factory.last.simulateMessage({ t: "error", reason: "stale_epoch", currentEpoch: 8 });
    });
    // 断言真的发生过重发（不是因为守卫全部拦截、这次测试其实什么也没验证到）。
    await waitFor(() => expect(factory.last.sent.length).toBeGreaterThan(sentBefore));

    await screen.findByTestId("history-load-error");
    expect(screen.getByTestId("history-load-error").textContent).toContain("timed out");
    const retry = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(retry.disabled).toBe(false);
    expect(retry.textContent).toBe("Retry");
  });

  it("U3：control.history 已发出、静候响应期间断线——立即经 onPhaseChange 转错误态，不必等原超时", async () => {
    // 超时故意设得很长（60s，远超真实测试等待时间）——如果断线后错误态很快出现，只能是新增的
    // "非 open 相位即败" 路径（`onPhaseChange` → `failHistoryRequests`）生效了，不可能是等到了这个
    // 60 秒的原始超时。此刻 `sendHistoryRequest` 的异步续体早已跑完（`sent` 已经到 2——控制帧真的
    // 发出去了），不存在"post-seal 检查恰好也在这时捕获到断线"这条旧有捷径可以蒙混过关。
    const { factory } = await setupPendingHistoryRequest(60_000);

    await act(async () => factory.last.simulateClose());

    await screen.findByTestId("history-load-error");
    expect(screen.getByTestId("history-load-error").textContent).toBe(
      "Not connected. Unable to load history.",
    );
    const retry = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(retry.disabled).toBe(false);
    expect(retry.textContent).toBe("Retry");
  });

  it("匹配 command_id 的 failed ack 清 history pending，错误态恢复按钮并可重试", async () => {
    const { factory, kRoomRaw, historyCommandId } = await setupPendingHistoryRequest();

    await act(async () => {
      factory.last.simulateMessage({ t: "input.ack", command_id: historyCommandId, outcome: "failed" });
    });
    await screen.findByTestId("history-load-error");
    expect(screen.getByTestId("history-load-error").textContent).toContain("failed to load");
    const retry = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(retry.disabled).toBe(false);

    fireEvent.click(retry);
    await waitFor(() => expect(factory.last.sent).toHaveLength(3));
    const controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const historyIds = controls
      .filter((entry) => entry.payload.t === "control.history")
      .map((entry) => entry.envelope.command_id);
    expect(historyIds).toHaveLength(2);
    expect(historyIds[1]).not.toBe(historyCommandId);
  });

  it("quota.exceeded live 广播清全部 history pending，并显示明确额度错误、恢复重试按钮", async () => {
    const { factory } = await setupPendingHistoryRequest();

    await act(async () => {
      factory.last.simulateMessage({ t: "quota.exceeded", channel: "live" });
    });
    await screen.findByTestId("history-load-error");
    expect(screen.getByTestId("history-load-error").textContent).toBe("This month's quota has been used up.");
    expect((screen.getByTestId("history-load-earlier") as HTMLButtonElement).disabled).toBe(false);
  });

  it("切换会话不会覆盖另一会话的 history pending；stale_epoch 会逐会话保留原 command_id 重封", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-history-sessions-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();
    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => factory.last.simulateOpen());
    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-history-sessions",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [
          { id: "s-a", title: "Session A", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 2 },
          { id: "s-b", title: "Session B", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1 },
        ],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });
    await screen.findByText("Session A");
    fireEvent.click(screen.getByText("Session A"));
    await waitFor(() => expect(factory.last.sent).toHaveLength(2));
    fireEvent.click(screen.getByTestId("app-runtime-back-to-sessions"));
    fireEvent.click(await screen.findByText("Session B"));
    await waitFor(() => expect(factory.last.sent).toHaveLength(3));

    let controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const initialHistoryIds = new Map(
      controls
        .filter((entry) => entry.payload.t === "control.history")
        .map((entry) => [entry.payload.session as string, entry.envelope.command_id]),
    );
    expect(Array.from(initialHistoryIds.keys()).sort()).toEqual(["s-a", "s-b"]);

    await act(async () => factory.last.simulateMessage({ t: "error", reason: "stale_epoch", currentEpoch: 8 }));
    await waitFor(() => expect(factory.last.sent).toHaveLength(6));
    controls = await decryptSentControlFrames(kRoomRaw, factory.last.sent);
    const resealedHistory = controls.filter(
      (entry) => entry.payload.t === "control.history" && entry.envelope.epoch === 8,
    );
    expect(resealedHistory).toHaveLength(2);
    for (const entry of resealedHistory) {
      expect(entry.envelope.command_id).toBe(initialHistoryIds.get(entry.payload.session as string));
    }
  });

  it("冷启动全量重放往返（INT1c P0④：不再只重建 session.index）——第二个 AppRuntime 实例接同一个持久事件库，session.index/msg.completed/run.status 全部正确重建且会话归属不串号", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const dbName = `apprt-replay-${crypto.randomUUID()}`;
    const eventStoreA = new IndexedDbEventStore(dbName);
    const factoryA = new FakeWebSocketFactory();

    // ---- "第一次会话"：真实收帧，落库两个会话的里程碑（含跨会话内容，验证不串号）----
    const first = render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factoryA.factory} eventStore={eventStoreA} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factoryA.sockets).toHaveLength(1));
    await act(async () => {
      factoryA.last.simulateOpen();
    });

    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-1",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [
          { id: "s-1", title: "Session One", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 },
          { id: "s-2", title: "Session Two", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 900 },
        ],
      },
    });
    const msg1Frame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "msg-s1",
      epoch: 0,
      payload: { t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "Hello from session one" }] },
    });
    const msg2Frame = await encryptEventFrame(kRoomRaw, {
      session: "s-2",
      seq: 3,
      clientMsgId: "msg-s2",
      epoch: 0,
      payload: { t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "Hello from session two" }] },
    });
    const runStatusFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 4,
      clientMsgId: "run-s1",
      epoch: 0,
      payload: { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" },
    });
    act(() => {
      factoryA.last.simulateMessage(indexFrame);
      factoryA.last.simulateMessage(msg1Frame);
      factoryA.last.simulateMessage(msg2Frame);
      factoryA.last.simulateMessage(runStatusFrame);
    });
    await waitFor(async () => expect(await eventStoreA.getWatermark()).toBe(4));
    first.unmount();
    cleanup();

    // ---- "重载"：全新 AppRuntime 实例 + 全新（从未 open 过的）socket，只接同一个持久事件库 ----
    const factoryB = new FakeWebSocketFactory();
    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factoryB.factory} eventStore={eventStoreA} onNeedsRepair={() => {}} />,
    );

    // session.index 重建——两个会话都在列表里（不是只有旧实现能安全重建的那一种）。
    await screen.findByText("Session One");
    await screen.findByText("Session Two");

    // 选会话一——它的 msg.completed 被正确重建，且状态条显示 running（run.status 也重建了）。
    fireEvent.click(screen.getByText("Session One").closest('[data-testid="session-row"]')!);
    await screen.findByTestId("session-stream-screen");
    await screen.findByText("Hello from session one");
    await waitFor(() => expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Running|运行中/));
    // 会话一的重建不应该带出会话二的内容——证明重放没有串号。
    expect(screen.queryByText("Hello from session two")).toBeNull();

    // 返回列表、选会话二——它的 msg.completed 也被正确重建，且不显示 running（没收到过它的
    // run.status），也看不到会话一的内容。
    fireEvent.click(screen.getByTestId("app-runtime-back-to-sessions"));
    fireEvent.click(screen.getByText("Session Two").closest('[data-testid="session-row"]')!);
    await screen.findByTestId("session-stream-screen");
    await screen.findByText("Hello from session two");
    expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Idle|空闲/);
    expect(screen.queryByText("Hello from session one")).toBeNull();
  });

  it("已归档会话不进入会话列表——全量快照即滤掉，archived 增量令可见行消失，unarchived 增量令其恢复", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-archived-filter-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    // ---- 全量快照：一条已归档 + 一条未归档——列表只应渲染未归档那条。 ----
    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-archived-filter",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [
          { id: "s-visible", title: "Visible session", repo_id: "repo-a", archived: false, status: null, run_id: null, updated_at: 1000 },
          { id: "s-archived", title: "Already archived", repo_id: "repo-a", archived: true, status: null, run_id: null, updated_at: 900 },
        ],
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Visible session");
    expect(screen.queryByText("Already archived")).toBeNull();
    expect(screen.getAllByTestId("session-row")).toHaveLength(1);

    // ---- session.index 增量 archived：唯一可见的那条被归档——列表应回落空态。 ----
    const archivedFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 2,
      clientMsgId: "idx-archived-filter-op",
      epoch: 0,
      payload: { t: "session.index", op: "archived", full: false, ids: ["s-visible"] },
    });
    await act(async () => {
      factory.last.simulateMessage(archivedFrame);
    });
    await waitFor(() => expect(screen.queryByText("Visible session")).toBeNull());
    expect(screen.queryByTestId("session-row")).toBeNull();
    expect(screen.getByTestId("session-list-empty")).toBeTruthy();

    // ---- session.index 增量 unarchived：同一条恢复——重新出现在列表里。 ----
    const unarchivedFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 3,
      clientMsgId: "idx-archived-filter-op-2",
      epoch: 0,
      payload: { t: "session.index", op: "unarchived", full: false, ids: ["s-visible"] },
    });
    await act(async () => {
      factory.last.simulateMessage(unarchivedFrame);
    });
    await screen.findByText("Visible session");
    expect(screen.getAllByTestId("session-row")).toHaveLength(1);
  });

  it("M2-4x：会话行副标题显示人类可读项目名（回退裸 repo_id），列表顶部显示当前项目", async () => {
    const { kRoomRaw, stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-repo-name-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();

    render(
      <AppRuntime stored={stored} keyStore={keyStore} webSocketFactory={factory.factory} eventStore={eventStore} onNeedsRepair={() => {}} />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    // 一条带 repo_name，一条没有（旧桌面兼容/repo 被删的降级路径）——各自的副标题必须分别是
    // 人类可读名字 / 裸 repo_id；顶部当前项目名读自顶层 repo.name。
    const indexFrame = await encryptEventFrame(kRoomRaw, {
      session: null,
      seq: 1,
      clientMsgId: "idx-repo-name",
      epoch: 0,
      payload: {
        t: "session.index",
        full: true,
        sessions: [
          {
            id: "s-1", title: "Has repo name", repo_id: "repo-18c3527a", archived: false,
            status: null, run_id: null, updated_at: 1000, repo_name: "Acme Corp",
          },
          {
            id: "s-2", title: "No repo name", repo_id: "repo-18c3527b", archived: false,
            status: null, run_id: null, updated_at: 900, repo_name: null,
          },
        ],
        repo: { id: "repo-18c3527a", name: "Acme Corp" },
      },
    });
    await act(async () => {
      factory.last.simulateMessage(indexFrame);
    });
    await screen.findByText("Has repo name");

    const rows = screen.getAllByTestId("session-row");
    const row1 = rows.find((r) => r.dataset.sessionId === "s-1")!;
    const row2 = rows.find((r) => r.dataset.sessionId === "s-2")!;
    expect(within(row1).getByTestId("session-row-repo").textContent).toBe("Acme Corp");
    expect(within(row2).getByTestId("session-row-repo").textContent).toBe("repo-18c3527b");
    expect(screen.getByTestId("session-list-active-repo").textContent).toBe("Current project: Acme Corp");
  });
});

describe("AppRuntime · 设置屏解除配对按钮（msgfix2 U4 修单 H2，四触发点②唯一入口接线）", () => {
  it("从会话列表进设置屏，点『解除配对』——调用外层 onNeedsRepair（同 repair_failed 页面『重试』走的同一条路径，不是另开一条独立的清除逻辑）", async () => {
    const { stored } = await makeStoredCredentials();
    const keyStore = new InMemoryKeyStore();
    await keyStore.saveKeys(stored);
    const eventStore = new IndexedDbEventStore(`apprt-unpair-${crypto.randomUUID()}`);
    const factory = new FakeWebSocketFactory();
    const onNeedsRepair = vi.fn();

    render(
      <AppRuntime
        stored={stored}
        keyStore={keyStore}
        webSocketFactory={factory.factory}
        eventStore={eventStore}
        onNeedsRepair={onNeedsRepair}
      />,
    );
    await waitFor(() => expect(factory.sockets).toHaveLength(1));
    await act(async () => {
      factory.last.simulateOpen();
    });

    fireEvent.click(await screen.findByTestId("session-list-settings-button"));
    fireEvent.click(await screen.findByTestId("settings-unpair-button"));

    expect(onNeedsRepair).toHaveBeenCalledTimes(1);
  });
});
