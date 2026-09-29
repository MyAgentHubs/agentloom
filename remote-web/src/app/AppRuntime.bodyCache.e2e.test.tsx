// AppRuntime.bodyCache.e2e.test.tsx — end-to-end: verifies the body-cache race gate (design doc
// §4.2 "read/write ordering" item c) and the cache toggle (item e) under `AppRuntime.tsx`'s real
// wiring.
//
// `AppRuntime.msgFetch.e2e.test.tsx` covers the `msg.fetch`/`msg.chunk` round trip itself, but
// never injects a `bodyCache` prop — `loadFullTextViaCacheOrFetch()` (the race gate/cache lookup
// inside `AppRuntime.tsx`) and `toggleCacheEnabled()` (clear immediately on disable, stop writing)
// are entirely unreachable in that file (when the `bodyCache` prop is omitted, the component
// derives a real `IndexedDbBodyCache` per room internally, and the test has no reference to assert
// against). This file explicitly injects an observable `InMemoryBodyCache` (see
// `store/bodyCache.ts`) to fill in that missing test coverage:
//   ① 缓存命中——打开会话前 body cache 里已经有对应 `content_sha256` 的正文 → 不发 `msg.fetch`，
//      直接用缓存内容替换预览。
//   ② 缓存未命中——正常走 `msg.fetch`/`msg.chunk`，完成后正文被写回 `bodyCache`（`put()` 收到的
//      key/blocks/bytes 与 wire 上收到的一致）。
//   ③ 缓存开关关闭——`toggleCacheEnabled(false)` 立即清空 `bodyCache`；关闭之后再完成一次
//      `msg.fetch`，`bodyCache` 不会被写入新内容。
//   ④ 竞跑闸——同一条消息的自动拉取 effect 重复触发（同一渲染帧内 `latestMessage` 没变）不会
//      发出第二次 `msg.fetch`（`cacheLookupPendingRef`/`msgFetchClient.getState().status` 双重
//      挂起闸,既有单元测试测过 `msgFetchClient` 自己的幂等,这里补的是"缓存查询挂起期间"这一段）。
//
// 假 relay 侧加密独立实现（不 import `crypto/envelope.ts` 的 `seal`/`open`/`buildAAD`）——同
// `AppRuntime.msgFetch.e2e.test.tsx` 头注纪律，本文件复制同一份独立实现（不跨测试文件共享,避免
// 两个文件的"假桌面"共用同一处潜在 bug）。

import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach } from "vitest";
import { ReadyState, type WebSocketCloseInfo, type WebSocketFactory, type WebSocketLike } from "../connection/types.ts";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import { InMemoryKeyStore, importNonExtractableAesGcmKey } from "../store/key-store.ts";
import { IndexedDbEventStore } from "../store/indexeddbEventStore.ts";
import { InMemoryCommandLedger } from "../store/commandLedger.ts";
import { InMemoryBodyCache, type BodyCacheKey, type BodyCachePort, type CachedBody } from "../store/bodyCache.ts";
import { MsgFetchClient } from "../events/msgFetch.ts";
import { AppRuntime } from "./AppRuntime.tsx";

afterEach(() => {
  cleanup();
});

/**
 * Drains outstanding async work (microtask chains plus one real macrotask round) before a
 * negative assertion ("this must not have happened by now"), without racing a fixed wall-clock
 * threshold. Replaces a fixed `setTimeout(resolve, 20)` that raced CI load and was flaky (~50%
 * under load, see BACKLOG.md R1②). The microtask loop settles any promise-chain continuation
 * (cache get/put/delete, retry chains) regardless of machine speed; the trailing zero-delay
 * `setTimeout` only lets one already-queued macrotask (if any) run its course — it does not race
 * a duration, so it stays deterministic under load the same way the microtask loop does.
 */
async function flushPendingWork(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 50; i += 1) {
      await Promise.resolve();
    }
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    for (let i = 0; i < 50; i += 1) {
      await Promise.resolve();
    }
  });
}

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

function base64Decode(value: string): Uint8Array {
  const binary = atob(value);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", toBufferSource(bytes)));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

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

async function encryptReplyFrame(
  kRoomRaw: Uint8Array,
  params: { session: string | null; commandId: string; epoch: number; payload: unknown },
): Promise<Record<string, unknown>> {
  const meta: Meta = { v: 1, room: ROOM, epoch: params.epoch, kind: "reply", session: params.session, command_id: params.commandId };
  const { ct, n } = await sealIndependent(kRoomRaw, meta, utf8Bytes(JSON.stringify(params.payload)));
  return {
    v: 1,
    room: ROOM,
    epoch: params.epoch,
    kind: "reply",
    session: params.session,
    command_id: params.commandId,
    seq: null,
    ct,
    n,
    ts: Date.now(),
  };
}

async function decryptSentEnvelope(
  raw: string,
  kRoomRaw: Uint8Array,
): Promise<{ envelope: Record<string, unknown>; plaintext: Record<string, unknown> }> {
  const envelope = JSON.parse(raw) as Record<string, unknown>;
  const meta: Meta = {
    v: envelope.v as number,
    room: envelope.room as string,
    epoch: envelope.epoch as number,
    kind: envelope.kind as string,
    session: envelope.session as string | null,
    command_id: envelope.command_id as string | null,
  };
  const plaintextBytes = await openIndependent(kRoomRaw, meta, envelope.ct as string, envelope.n as string);
  return { envelope, plaintext: JSON.parse(new TextDecoder().decode(plaintextBytes)) };
}

async function findSentCommands(sent: string[], kRoomRaw: Uint8Array, t: string): Promise<Record<string, unknown>[]> {
  const out: Record<string, unknown>[] = [];
  for (const raw of sent) {
    const decoded = await decryptSentEnvelope(raw, kRoomRaw);
    if (decoded.plaintext.t === t) out.push(decoded.plaintext);
  }
  return out;
}

async function findSentCommand(
  sent: string[],
  kRoomRaw: Uint8Array,
  t: string,
): Promise<{ envelope: Record<string, unknown>; plaintext: Record<string, unknown> } | undefined> {
  for (const raw of sent) {
    const decoded = await decryptSentEnvelope(raw, kRoomRaw);
    if (decoded.plaintext.t === t) return decoded;
  }
  return undefined;
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

/** 同 `AppRuntime.msgFetch.e2e.test.tsx::setupSelectedSessionAtEpoch5`——额外接受一个 `bodyCache`
 *  注入点（省略时该文件走组件默认构造，本文件恒显式传入以便断言）。 */
async function setupSelectedSessionAtEpoch5(bodyCache: BodyCachePort) {
  const { kRoomRaw, stored } = await makeStoredCredentials();
  const keyStore = new InMemoryKeyStore();
  await keyStore.saveKeys(stored);
  const eventStore = new IndexedDbEventStore(`apprt-bodycache-test-${crypto.randomUUID()}`);
  const ledger = new InMemoryCommandLedger();
  const factory = new FakeWebSocketFactory();

  render(
    <AppRuntime
      stored={stored}
      keyStore={keyStore}
      webSocketFactory={factory.factory}
      eventStore={eventStore}
      commandLedger={ledger}
      bodyCache={bodyCache}
      onNeedsRepair={() => {}}
    />,
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
  await waitFor(() => expect(factory.last.sent.length).toBeGreaterThanOrEqual(1));

  return { kRoomRaw, stored, keyStore, eventStore, ledger, factory };
}

async function contentRefFor(messageId: number, revision: number, blocks: unknown[]) {
  const bytes = new TextEncoder().encode(JSON.stringify(blocks));
  return {
    message_id: messageId,
    revision,
    content_sha256: await sha256Hex(bytes),
    total_bytes: bytes.length,
    bytes,
  };
}

async function pushMsgCompleted(
  kRoomRaw: Uint8Array,
  factory: FakeWebSocketFactory,
  params: {
    messageId: number;
    seq: number;
    totalBytesOverride?: number;
    ref: Awaited<ReturnType<typeof contentRefFor>>;
    /** 默认 `msg-${messageId}`——同一 messageId 两次 `msg.completed`（例如模拟 revision 从 1
     *  推进到 2）必须传不同的值，否则 `EventStorePort::applyEventIfNew()` 的 `client_msg_id`
     *  去重会把第二条当"已经见过的重投"直接丢弃（`storeDuplicate`），projection 永远不会真的
     *  推进到新 revision——这不是 bug，是既有幂等契约，调用方必须显式给第二条不同的 id。 */
    clientMsgId?: string;
  },
) {
  const frame = await encryptEventFrame(kRoomRaw, {
    session: "s-1",
    seq: params.seq,
    clientMsgId: params.clientMsgId ?? `msg-${params.messageId}`,
    epoch: 5,
    payload: {
      t: "msg.completed",
      message_id: params.messageId,
      role: "assistant",
      blocks: [{ type: "text", text: "preview" }],
      content_ref: {
        message_id: params.ref.message_id,
        revision: params.ref.revision,
        content_sha256: params.ref.content_sha256,
        total_bytes: params.totalBytesOverride ?? params.ref.total_bytes,
      },
    },
  });
  await act(async () => {
    factory.last.simulateMessage(frame);
  });
}

describe("AppRuntime body cache e2e · ① 缓存命中——不发 msg.fetch，直接用缓存内容替换预览", () => {
  it("打开会话前 body cache 已有该 content_sha256 的正文 → 自动拉取路径直接命中，never sends msg.fetch", async () => {
    const bodyCache = new InMemoryBodyCache();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    const fullBlocks = [{ type: "text", text: "cached full content — should never hit the network" }];
    const ref = await contentRefFor(701, 1, fullBlocks);
    // 预先把这条消息的正文种进 body cache（key 必须跟 AppRuntime 内部拼的一致：
    // room|session|messageId|contentSha256，见 `store/bodyCache.ts::bodyCacheKeyString`）。
    await bodyCache.put({ room: ROOM, session: "s-1", messageId: 701, contentSha256: ref.content_sha256 }, fullBlocks, ref.bytes);

    // 消息一到达（≤512KiB 自动阈值内）——自动拉取 effect 会先查缓存,命中就不发 msg.fetch。
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 701, seq: 2, ref });

    await screen.findByText("cached full content — should never hit the network");
    expect(screen.queryByTestId("msg-fetch-load")).toBeNull(); // 已经是全文——不展示"加载全文"按钮。

    // 给一点时间让"万一真的发了"的异步 fetch 有机会落地,再确认真的一次都没发过。
    await flushPendingWork();
    expect(await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch")).toHaveLength(0);
  });

  it("手动点击「加载全文」也走同一条缓存查询——命中同样不发 msg.fetch", async () => {
    const bodyCache = new InMemoryBodyCache();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);
    const user = userEvent.setup();

    const fullBlocks = [{ type: "text", text: "cached content for manual load button" }];
    const ref = await contentRefFor(702, 1, fullBlocks);
    await bodyCache.put({ room: ROOM, session: "s-1", messageId: 702, contentSha256: ref.content_sha256 }, fullBlocks, ref.bytes);

    // 超 512KiB 自动阈值——不自动拉,走点击路径（同既有 msgFetch e2e 用例的既有取向）。
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 702, seq: 2, ref, totalBytesOverride: 999_999 });

    const loadButton = await screen.findByTestId("msg-fetch-load");
    await user.click(loadButton);

    await screen.findByText("cached content for manual load button");
    await flushPendingWork();
    expect(await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch")).toHaveLength(0);
  });
});

describe("AppRuntime body cache e2e · ② 缓存未命中——正常走 msg.fetch/msg.chunk，完成后写回 bodyCache", () => {
  it("fetch 成功之后 bodyCache.get() 能读到刚拉到的正文（AppRuntime 接线,不是只测 msgFetch.ts 单元）", async () => {
    const bodyCache = new InMemoryBodyCache();
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    const fullBlocks = [{ type: "text", text: "freshly fetched — written to body cache after completion" }];
    const ref = await contentRefFor(703, 1, fullBlocks);
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 703, seq: 2, ref, totalBytesOverride: 999_999 });

    const loadButton = await screen.findByTestId("msg-fetch-load");
    await user.click(loadButton);

    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    const commandId = sent.envelope.command_id as string;

    const replyFrame = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId,
      epoch: 5,
      payload: {
        t: "msg.chunk",
        message_id: 703,
        revision: 1,
        content_sha256: ref.content_sha256,
        total_bytes: ref.bytes.length,
        offset: 0,
        chunk_len: ref.bytes.length,
        bytes_b64: bytesToBase64(ref.bytes),
      },
    });
    await act(async () => {
      factory.last.simulateMessage(replyFrame);
    });

    await screen.findByText("freshly fetched — written to body cache after completion");

    // 核心断言：AppRuntime 的 `onFetchCached` 挂钩真的把正文写进了注入的 `bodyCache` 实例——不是
    // 只验证 UI 呈现,是验证 U4 新增的"缓存写入"这一半接线真的发生了。
    const cached = await waitFor(async () => {
      const got = await bodyCache.get({ room: ROOM, session: "s-1", messageId: 703, contentSha256: ref.content_sha256 });
      if (!got) throw new Error("not cached yet");
      return got;
    });
    expect(cached.blocks).toEqual(fullBlocks);
    // `toEqual()` 直接比两个 `Uint8Array` 在本文件所在的 jsdom project 下会报"no visual
    // difference"的假失败（实测：内容逐字节相同,只是 vitest 在 jsdom 环境比对 TypedArray 结构
    // 时的已知怪癖——`store/bodyCache.indexeddb.test.ts` 那类 `*.test.ts` 文件走 node project,
    // 不受影响,不是本仓通用问题）；按普通数组比对绕开。
    expect(Array.from(cached.bytes)).toEqual(Array.from(ref.bytes));
  });
});

describe("AppRuntime body cache e2e · ③ 缓存开关——关闭立即清 + 停止新写", () => {
  it("在 Settings 屏点击缓存开关关闭 → bodyCache 立即被清空；关闭之后完成的新 fetch 不再写入", async () => {
    const bodyCache = new InMemoryBodyCache();
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    // 先塞一条已有缓存内容——验证"关闭 = 立即清"，不是只对"以后的写入"生效。
    const preExisting = [{ type: "text", text: "should be purged the moment the toggle flips off" }];
    const preRef = await contentRefFor(704, 1, preExisting);
    await bodyCache.put({ room: ROOM, session: "s-1", messageId: 704, contentSha256: preRef.content_sha256 }, preExisting, preRef.bytes);
    expect(bodyCache.size).toBeGreaterThan(0);

    // 设置入口只挂在会话列表屏（`ui/sessions/SessionListScreen.tsx::session-list-settings-button`）
    // ——先退回列表屏,打开设置,点掉缓存开关（第二行,见 `ui/settings/SettingsScreen.tsx`）。
    fireEvent.click(screen.getByTestId("app-runtime-back-to-sessions"));
    fireEvent.click(await screen.findByTestId("session-list-settings-button"));
    const cacheToggle = await screen.findByTestId("settings-cache-toggle");
    expect(cacheToggle.getAttribute("aria-checked")).toBe("true"); // 设计稿默认开。
    await user.click(cacheToggle);
    await waitFor(() => expect(cacheToggle.getAttribute("aria-checked")).toBe("false"));

    // 核心断言①：立即清——不需要等下一次操作。
    await waitFor(async () => {
      expect(await bodyCache.get({ room: ROOM, session: "s-1", messageId: 704, contentSha256: preRef.content_sha256 })).toBeNull();
    });

    // 回到会话流,完成一次全新的 msg.fetch——关闭状态下不应该再写进 bodyCache。
    fireEvent.click(screen.getByTestId("settings-back"));
    fireEvent.click(await screen.findByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    const fullBlocks = [{ type: "text", text: "fetched while cache toggle is off — must not be written" }];
    const ref = await contentRefFor(705, 1, fullBlocks);
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 705, seq: 3, ref, totalBytesOverride: 999_999 });

    const loadButton = await screen.findByTestId("msg-fetch-load");
    await user.click(loadButton);
    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    const commandId = sent.envelope.command_id as string;
    const replyFrame = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId,
      epoch: 5,
      payload: {
        t: "msg.chunk",
        message_id: 705,
        revision: 1,
        content_sha256: ref.content_sha256,
        total_bytes: ref.bytes.length,
        offset: 0,
        chunk_len: ref.bytes.length,
        bytes_b64: bytesToBase64(ref.bytes),
      },
    });
    await act(async () => {
      factory.last.simulateMessage(replyFrame);
    });
    await screen.findByText("fetched while cache toggle is off — must not be written");

    // 核心断言②：关闭状态下 fetch 成功也不再写入——给异步写入一点时间落地再确认它没发生。
    await flushPendingWork();
    expect(await bodyCache.get({ room: ROOM, session: "s-1", messageId: 705, contentSha256: ref.content_sha256 })).toBeNull();
    expect(bodyCache.size).toBe(0);
  });
});

describe("AppRuntime body cache e2e · ④ 竞跑闸——缓存查询挂起期间不会重复发起 msg.fetch", () => {
  it("同一条消息触发两次自动拉取路径（模拟 effect 重跑）——只发一次 msg.fetch,不因为缓存查询是异步的就重复发起", async () => {
    const bodyCache = new InMemoryBodyCache();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    const fullBlocks = [{ type: "text", text: "auto fetched exactly once despite re-render churn" }];
    const ref = await contentRefFor(706, 1, fullBlocks);
    // ≤512KiB——自动拉取路径。到达时会先查一次（未命中的）缓存,查询挂起期间
    // `cacheLookupPendingRef` 阻断同一 messageId 的重复发起；查询落地后走 `msgFetchClient`
    // 自己的 `status !== "idle"` 幂等闸（既有单元测试测过），两道闸叠加应当仍然只发一次。
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 706, seq: 2, ref });

    // 触发一次不影响该消息的重渲染（连接状态变化会让 AppRuntime 重跑一轮"自动拉取"useEffect 的
    // 依赖判断路径）——同一条消息、同一个 revision，缓存闸应该继续挡住重复发起。
    await act(async () => {
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });

    const sent = await waitFor(async () => {
      const found = await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch");
      if (found.length === 0) throw new Error("msg.fetch not auto-sent yet");
      return found;
    });
    expect(sent).toHaveLength(1);
    expect(sent[0]).toEqual({ t: "msg.fetch", session: "s-1", message_id: 706, revision: 1, offset: 0 });

    // 完成这次 fetch,再等一轮——确认后续也不会因为竞跑闸状态没清干净而补发第二次。
    const commandId = (await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch"))!.envelope.command_id as string;
    const replyFrame = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId,
      epoch: 5,
      payload: {
        t: "msg.chunk",
        message_id: 706,
        revision: 1,
        content_sha256: ref.content_sha256,
        total_bytes: ref.bytes.length,
        offset: 0,
        chunk_len: ref.bytes.length,
        bytes_b64: bytesToBase64(ref.bytes),
      },
    });
    await act(async () => {
      factory.last.simulateMessage(replyFrame);
    });
    await screen.findByText("auto fetched exactly once despite re-render churn");
    await flushPendingWork();
    expect(await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch")).toHaveLength(1);
  });
});

/**
 * msgfix2 U4 修单 H7：`get()` **真挂起**（不立即完成的内存 get）——包一个真实 `InMemoryBodyCache`，
 * `get()` 调用被记下来但故意不 resolve，测试代码手动 `resolveNextGet()` 才放行。这是任务书 H7
 * 明确要求的"用真挂起的 deferred get 证明挂起期间自动 fetch 被阻断、完成后恢复"——不是"立即完成的
 * 内存 get 凑巧没触发第二次调用"那种弱证据。`put()`/`delete()`/`clear()` 直接透传给内部真实
 * `InMemoryBodyCache`（这几个方法不是本单要控制时序的对象）。
 */
class DeferredGetBodyCache implements BodyCachePort {
  private readonly inner: InMemoryBodyCache;
  getCalls: BodyCacheKey[] = [];
  deleteCalls: BodyCacheKey[] = [];
  private resolvers: Array<(value: CachedBody | null) => void> = [];

  constructor(inner: InMemoryBodyCache = new InMemoryBodyCache()) {
    this.inner = inner;
  }

  async get(key: BodyCacheKey): Promise<CachedBody | null> {
    this.getCalls.push(key);
    return new Promise<CachedBody | null>((resolve) => {
      this.resolvers.push(resolve);
    });
  }

  /** 放行最早一次还没 resolve 的 `get()` 调用——先进先出，同任何真实异步队列的直觉顺序。 */
  resolveNextGet(value: CachedBody | null): void {
    const resolver = this.resolvers.shift();
    if (!resolver) throw new Error("resolveNextGet() called with no pending get() to resolve (test setup bug)");
    resolver(value);
  }

  get pendingGetCount(): number {
    return this.resolvers.length;
  }

  async put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void> {
    return this.inner.put(key, blocks, bytes);
  }

  async delete(key: BodyCacheKey): Promise<void> {
    this.deleteCalls.push(key);
    return this.inner.delete(key);
  }

  async clear(): Promise<void> {
    return this.inner.clear();
  }
}

describe("AppRuntime body cache e2e · ④b 竞跑闸——真挂起 deferred get（msgfix2 U4 修单 H7）", () => {
  it("get() 真挂起期间——同一条消息的重复自动拉取尝试被阻断（不发第二次 msg.fetch）；get() 落地（未命中）后正常恢复，只发一次", async () => {
    const bodyCache = new DeferredGetBodyCache();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    const fullBlocks = [{ type: "text", text: "recovers once the deferred get() finally settles" }];
    const ref = await contentRefFor(750, 1, fullBlocks);
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 750, seq: 2, ref });

    // 缓存查询真的挂起了——还没 resolve。
    await waitFor(() => expect(bodyCache.pendingGetCount).toBe(1));
    expect(await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch")).toHaveLength(0); // 还没查完，不该先斩后奏发 fetch。

    // 挂起期间触发一次不影响该消息内容的重渲染——竞跑闸应该继续挡住第二次缓存查询/第二次 fetch。
    await act(async () => {
      factory.last.simulateMessage({ t: "replay.head", epoch: 5, headSeq: 1 });
    });
    expect(bodyCache.pendingGetCount).toBe(1); // 仍然只有那一次挂起的查询，没有叠加第二次。
    expect(await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch")).toHaveLength(0);

    // 放行——真实未命中（这条消息从未真的缓存过）。
    await act(async () => {
      bodyCache.resolveNextGet(null);
    });

    // 核心断言：挂起解除之后自动恢复，且只发一次 msg.fetch（不因为之前被阻断的那次重渲染尝试而
    // 补发第二次）。
    const sent = await waitFor(async () => {
      const found = await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch");
      if (found.length === 0) throw new Error("msg.fetch not sent yet");
      return found;
    });
    expect(sent).toHaveLength(1);
    expect(sent[0]).toEqual({ t: "msg.fetch", session: "s-1", message_id: 750, revision: 1, offset: 0 });
  });
});

describe("AppRuntime body cache e2e · ⑤ stale-revision 缓存拒绝回落（msgfix2 U4 修单 H4）", () => {
  it("缓存查询挂起期间消息已升级到新 revision——命中的旧缓存被投影拒绝后，清掉这条孤儿条目并自动回落到网络拉取新 revision，不永久空转", async () => {
    const bodyCache = new DeferredGetBodyCache();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    const oldBlocks = [{ type: "text", text: "stale revision content — must not resurrect" }];
    const oldRef = await contentRefFor(801, 1, oldBlocks);
    // revision 1 到达——total_bytes 在自动拉取阈值内，触发自动拉取路径先查一次缓存（挂起）。
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 801, seq: 2, ref: oldRef });
    await waitFor(() => expect(bodyCache.pendingGetCount).toBe(1));
    expect(bodyCache.getCalls[0]).toEqual({ room: ROOM, session: "s-1", messageId: 801, contentSha256: oldRef.content_sha256 });

    // 缓存查询挂起期间——一条更新的 msg.completed 帧先到达，把这条消息推进到 revision 2（不同的
    // `clientMsgId`——同一 messageId 复用第一条的 id 会被去重丢弃，见 `pushMsgCompleted` 参数注释）。
    // 故意让新内容明显更大（撑出不同的显示大小）——下面用按钮标签变化等它真的落地（`act()` 包完
    // `simulateMessage()` 只保证同步部分跑完，解密/路由/归约是真异步 WebCrypto 调用，不这样等就会
    // 在这条帧真正应用之前提前 resolve 挂起的 get()，把整条测试的时序基础搞错——写这条测试时踩过
    // 的真实坑）。
    const newBlocks = [{ type: "text", text: "fresh revision content — must be what actually loads ".repeat(60) }];
    const newRef = await contentRefFor(801, 2, newBlocks);
    const oldSizeLabel = `Load full text (${Math.ceil(oldRef.bytes.length / 1024) || 1} KB)`;
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 801, seq: 3, ref: newRef, clientMsgId: "msg-801-rev2" });
    await waitFor(() => expect(screen.getByTestId("msg-fetch-load").textContent).not.toBe(oldSizeLabel));

    // 现在放行挂起的查询——拿到的是 revision 1 的旧内容（真实场景：查询发起那一刻这条缓存确实
    // 命中过，只是这一刻投影已经不认这个 revision 了）。
    await act(async () => {
      bodyCache.resolveNextGet({ blocks: oldBlocks, bytes: oldRef.bytes, cachedAt: Date.now() });
    });

    // 核心断言①：孤儿缓存条目被清掉了——不是留着占位等 LRU 慢慢淘汰。
    await waitFor(() => expect(bodyCache.deleteCalls).toContainEqual({ room: ROOM, session: "s-1", messageId: 801, contentSha256: oldRef.content_sha256 }));

    // 回落重新查了一次缓存（这次是新 revision 的 sha）——真实未命中，放行让它落到网络拉取。
    await waitFor(() => expect(bodyCache.pendingGetCount).toBe(1));
    expect(bodyCache.getCalls.at(-1)).toEqual({ room: ROOM, session: "s-1", messageId: 801, contentSha256: newRef.content_sha256 });
    await act(async () => {
      bodyCache.resolveNextGet(null);
    });

    // 核心断言②：自动回落到网络——不需要用户再点一次，且发的是新 revision（不是死循环重试旧的）。
    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    expect(sent.plaintext).toEqual({ t: "msg.fetch", session: "s-1", message_id: 801, revision: 2, offset: 0 });
  });
});

describe("AppRuntime body cache e2e · ⑤b 连环 stale-revision 回落上限（msgfix2 U4 修单二 I5）", () => {
  it("连续两轮缓存命中都被投影拒绝（消息在两次查询挂起期间各推进了一次 revision）——只回落重试一次，第二次不符直接跳过缓存走网络，不会无限 get/delete 循环", async () => {
    const bodyCache = new DeferredGetBodyCache();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    const rev1Blocks = [{ type: "text", text: "rev1 stale content — must not resurrect" }];
    const rev1Ref = await contentRefFor(901, 1, rev1Blocks);
    // revision 1 到达——触发自动拉取路径先查一次缓存（挂起，第一次 get）。
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 901, seq: 2, ref: rev1Ref });
    await waitFor(() => expect(bodyCache.pendingGetCount).toBe(1));
    expect(bodyCache.getCalls[0]).toEqual({ room: ROOM, session: "s-1", messageId: 901, contentSha256: rev1Ref.content_sha256 });

    // 第一次查询挂起期间——消息推进到 revision 2。
    const rev2Blocks = [{ type: "text", text: "rev2 stale content — also must not resurrect ".repeat(40) }];
    const rev2Ref = await contentRefFor(901, 2, rev2Blocks);
    const rev1SizeLabel = `Load full text (${Math.ceil(rev1Ref.bytes.length / 1024) || 1} KB)`;
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 901, seq: 3, ref: rev2Ref, clientMsgId: "msg-901-rev2" });
    await waitFor(() => expect(screen.getByTestId("msg-fetch-load").textContent).not.toBe(rev1SizeLabel));

    // 放行第一次查询——拿到的是 revision 1 的旧内容，投影拒绝（当前已经是 revision 2）。这是第一次
    // 回落——按 I5 修单，最多允许这一次。
    await act(async () => {
      bodyCache.resolveNextGet({ blocks: rev1Blocks, bytes: rev1Ref.bytes, cachedAt: Date.now() });
    });

    // 孤儿条目（revision 1 那条）被清掉。
    await waitFor(() => expect(bodyCache.deleteCalls).toContainEqual({ room: ROOM, session: "s-1", messageId: 901, contentSha256: rev1Ref.content_sha256 }));

    // 回落发起了第二次查询（revision 2 的 sha）——这是 I5 允许的唯一一次回落重试。
    await waitFor(() => expect(bodyCache.pendingGetCount).toBe(1));
    expect(bodyCache.getCalls.at(-1)).toEqual({ room: ROOM, session: "s-1", messageId: 901, contentSha256: rev2Ref.content_sha256 });

    // 第二次查询挂起期间——消息再推进到 revision 3（连环 stale 的场景：两次查询挂起期间各推进
    // 一次）。
    const rev3Blocks = [{ type: "text", text: "rev3 final content — this is what should actually be fetched ".repeat(40) }];
    const rev3Ref = await contentRefFor(901, 3, rev3Blocks);
    const rev2SizeLabel = `Load full text (${Math.ceil(rev2Ref.bytes.length / 1024) || 1} KB)`;
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 901, seq: 4, ref: rev3Ref, clientMsgId: "msg-901-rev3" });
    await waitFor(() => expect(screen.getByTestId("msg-fetch-load").textContent).not.toBe(rev2SizeLabel));

    // 放行第二次查询——拿到的是 revision 2 的内容，投影再次拒绝（当前已经是 revision 3）。按 I5
    // 修单，这次已经用掉了唯一一次回落重试名额，不该再发起第三次缓存查询——直接跳过缓存走网络。
    await act(async () => {
      bodyCache.resolveNextGet({ blocks: rev2Blocks, bytes: rev2Ref.bytes, cachedAt: Date.now() });
    });

    // 核心断言①：孤儿条目（revision 2 那条）也被清掉了——即使不再回落查缓存，孤儿清理仍然要做。
    await waitFor(() => expect(bodyCache.deleteCalls).toContainEqual({ room: ROOM, session: "s-1", messageId: 901, contentSha256: rev2Ref.content_sha256 }));

    // 核心断言②：恰好只发过两次 get()——没有第三次缓存查询（不是无限 get/delete 循环）。给一点
    // 时间让"如果错误地又发起了第三次查询"的话它有机会真的发生，再断言。
    await flushPendingWork();
    expect(bodyCache.getCalls).toHaveLength(2);
    expect(bodyCache.pendingGetCount).toBe(0); // 没有第三次挂起的查询。

    // 核心断言③：恰好一次网络 fetch，且是最终真正的 revision 3（不是重试 revision 1/2）。
    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    expect(sent.plaintext).toEqual({ t: "msg.fetch", session: "s-1", message_id: 901, revision: 3, offset: 0 });
    expect(await findSentCommands(factory.last.sent, kRoomRaw, "msg.fetch")).toHaveLength(1); // 恰好一次。

    // Reverse-regression proof (msgfix2 U4 I5b): I5b re-locks cacheLookupPendingRef until the
    // fallback fetch's dispatch lands — prove that lock is actually released afterward, not stuck
    // forever. Complete this revision 3 fetch, then advance the same message to revision 4: the
    // auto-fetch path must still be able to issue a fresh, unblocked cache lookup for it, or this
    // message would never refresh again after falling back once.
    const replyFrame = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId: sent.envelope.command_id as string,
      epoch: 5,
      payload: {
        t: "msg.chunk",
        message_id: 901,
        revision: 3,
        content_sha256: rev3Ref.content_sha256,
        total_bytes: rev3Ref.bytes.length,
        offset: 0,
        chunk_len: rev3Ref.bytes.length,
        bytes_b64: bytesToBase64(rev3Ref.bytes),
      },
    });
    await act(async () => {
      factory.last.simulateMessage(replyFrame);
    });
    await waitFor(() => expect(screen.queryByTestId("msg-fetch-load")).toBeNull()); // revision 3 is now full text — button gone.
    const rev4Blocks = [{ type: "text", text: "rev4 content — proves the I5b pending lock was released, not stuck forever" }];
    const rev4Ref = await contentRefFor(901, 4, rev4Blocks);
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 901, seq: 5, ref: rev4Ref, clientMsgId: "msg-901-rev4" });
    await waitFor(() => expect(bodyCache.getCalls).toHaveLength(3));
    expect(bodyCache.getCalls.at(-1)).toEqual({ room: ROOM, session: "s-1", messageId: 901, contentSha256: rev4Ref.content_sha256 });
  });
});

describe("AppRuntime body cache e2e · ⑤c 回落分支 startFetch reject 时锁仍会释放（P2）", () => {
  it("回落分支的 startFetch 拒绝——finally 仍释放 cacheLookupPendingRef，之后同一条消息的新 revision 还能再次触发 get()", async () => {
    const bodyCache = new DeferredGetBodyCache();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    const rev1Blocks = [{ type: "text", text: "rev1 stale content — must not resurrect" }];
    const rev1Ref = await contentRefFor(950, 1, rev1Blocks);
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 950, seq: 2, ref: rev1Ref });
    await waitFor(() => expect(bodyCache.pendingGetCount).toBe(1));

    // Advance to revision 2 while the first query is pending, so its rejection recurses
    // (staleRetried false -> true) instead of landing in the fallback branch yet.
    const rev2Blocks = [{ type: "text", text: "rev2 stale content — also must not resurrect ".repeat(40) }];
    const rev2Ref = await contentRefFor(950, 2, rev2Blocks);
    const rev1SizeLabel = `Load full text (${Math.ceil(rev1Ref.bytes.length / 1024) || 1} KB)`;
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 950, seq: 3, ref: rev2Ref, clientMsgId: "msg-950-rev2" });
    await waitFor(() => expect(screen.getByTestId("msg-fetch-load").textContent).not.toBe(rev1SizeLabel));
    await act(async () => {
      bodyCache.resolveNextGet({ blocks: rev1Blocks, bytes: rev1Ref.bytes, cachedAt: Date.now() });
    });
    await waitFor(() => expect(bodyCache.pendingGetCount).toBe(1));

    // Advance to revision 3 while the second (recursive, staleRetried=true) query is pending, so
    // its rejection lands in the fallback branch this test actually targets.
    const rev3Blocks = [{ type: "text", text: "rev3 final content — this is what should actually be fetched ".repeat(40) }];
    const rev3Ref = await contentRefFor(950, 3, rev3Blocks);
    const rev2SizeLabel = `Load full text (${Math.ceil(rev2Ref.bytes.length / 1024) || 1} KB)`;
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 950, seq: 4, ref: rev3Ref, clientMsgId: "msg-950-rev3" });
    await waitFor(() => expect(screen.getByTestId("msg-fetch-load").textContent).not.toBe(rev2SizeLabel));

    // Force the fallback branch's own startFetch dispatch to reject once. The production code
    // intentionally does not attach a .catch() at that call site (see the P2 comment on
    // loadFullTextViaCacheOrFetch's fallback branch in AppRuntime.tsx), so this genuinely produces
    // a real, uncaught rejection — swap out Node's unhandledRejection listeners for the duration
    // of this call so the expected rejection doesn't get reported as a test-run failure, then
    // restore them (same technique the describe block "⑥" above uses to suppress an expected
    // console.error, just at the process level instead of console).
    const startFetchSpy = vi
      .spyOn(MsgFetchClient.prototype, "startFetch")
      .mockRejectedValueOnce(new Error("simulated startFetch rejection (P2 test)"));
    type RejectionListener = (reason: unknown) => void;
    const nodeProcess = (globalThis as { process?: { listeners: (event: string) => RejectionListener[]; removeAllListeners: (event: string) => void; on: (event: string, listener: RejectionListener) => void } }).process;
    if (!nodeProcess) throw new Error("global `process` (Node's unhandledRejection emitter) not found — has the test runtime changed?");
    const savedRejectionListeners = nodeProcess.listeners("unhandledRejection");
    nodeProcess.removeAllListeners("unhandledRejection");
    let observedRejection: unknown;
    nodeProcess.on("unhandledRejection", (reason) => {
      observedRejection = reason;
    });
    try {
      await act(async () => {
        bodyCache.resolveNextGet({ blocks: rev2Blocks, bytes: rev2Ref.bytes, cachedAt: Date.now() });
      });
      await flushPendingWork();
    } finally {
      nodeProcess.removeAllListeners("unhandledRejection");
      for (const listener of savedRejectionListeners) {
        nodeProcess.on("unhandledRejection", listener);
      }
      startFetchSpy.mockRestore();
    }
    // Confirm the rejection actually happened as expected (not silently absorbed elsewhere).
    expect(observedRejection).toBeInstanceOf(Error);
    expect((observedRejection as Error).message).toBe("simulated startFetch rejection (P2 test)");

    // Core assertion: even though startFetch rejected, the lock was released — a later revision on
    // the same message can still trigger a fresh, unblocked cache lookup (not stuck forever).
    const rev4Blocks = [{ type: "text", text: "rev4 content — proves the lock releases even when startFetch rejects" }];
    const rev4Ref = await contentRefFor(950, 4, rev4Blocks);
    await pushMsgCompleted(kRoomRaw, factory, { messageId: 950, seq: 5, ref: rev4Ref, clientMsgId: "msg-950-rev4" });
    await waitFor(() => expect(bodyCache.getCalls).toHaveLength(3));
    expect(bodyCache.getCalls.at(-1)).toEqual({ room: ROOM, session: "s-1", messageId: 950, contentSha256: rev4Ref.content_sha256 });
  });
});

/** 同 `ui/settings/cachePreference.test.tsx` 头注的既有手法——这个 jsdom project 里
 *  `window.localStorage` 默认不可用（Node 26 自带的 `localStorage` escape hatch 跟 jsdom 自己那份
 *  打架），需要显式把 jsdom 内部真正的 `Window.localStorage` 接回 `window.localStorage`。 */
function installRealLocalStorage(): void {
  const dom = (window as unknown as { jsdom?: { window: Window } }).jsdom;
  if (!dom) throw new Error("window.jsdom (vitest jsdom environment handle) not found — has the environment changed?");
  Object.defineProperty(window, "localStorage", { value: dom.window.localStorage, configurable: true, writable: true });
}

describe("AppRuntime body cache e2e · ⑥ 关闭缓存开关的清理可靠性（msgfix2 U4 修单 H5）", () => {
  const CACHE_PREFERENCE_KEY = "agentloom.remote-web.settings.bodyCache";

  beforeEach(() => {
    installRealLocalStorage();
    window.localStorage.clear();
  });

  afterEach(() => {
    window.localStorage.clear();
  });

  it("启动时偏好已经是『关』——即使上一次关闭时清理未必落地，挂载时会补跑一次幂等清理，最终把库清空（兜底幂等）", async () => {
    window.localStorage.setItem(CACHE_PREFERENCE_KEY, "0");
    const bodyCache = new InMemoryBodyCache();
    const leftover = [{ type: "text", text: "leftover from a previous session that never finished clearing" }];
    const leftoverRef = await contentRefFor(901, 1, leftover);
    await bodyCache.put({ room: ROOM, session: "s-1", messageId: 901, contentSha256: leftoverRef.content_sha256 }, leftover, leftoverRef.bytes);
    expect(bodyCache.size).toBeGreaterThan(0);

    await setupSelectedSessionAtEpoch5(bodyCache);

    await waitFor(async () => {
      expect(await bodyCache.get({ room: ROOM, session: "s-1", messageId: 901, contentSha256: leftoverRef.content_sha256 })).toBeNull();
    });
    expect(bodyCache.size).toBe(0);
  });

  it("关闭缓存开关时清理失败——不再像旧版那样静默吞掉（`.catch(() => {})`），console.error 可见", async () => {
    class FlakyClearBodyCache implements BodyCachePort {
      constructor(private readonly inner: InMemoryBodyCache = new InMemoryBodyCache()) {}
      async get(key: BodyCacheKey): Promise<CachedBody | null> {
        return this.inner.get(key);
      }
      async put(key: BodyCacheKey, blocks: unknown[], bytes: Uint8Array): Promise<void> {
        return this.inner.put(key, blocks, bytes);
      }
      async delete(key: BodyCacheKey): Promise<void> {
        return this.inner.delete(key);
      }
      async clear(): Promise<void> {
        throw new Error("indexeddb clear transaction failed (test)");
      }
    }
    const bodyCache = new FlakyClearBodyCache();
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    await setupSelectedSessionAtEpoch5(bodyCache);

    fireEvent.click(screen.getByTestId("app-runtime-back-to-sessions"));
    fireEvent.click(await screen.findByTestId("session-list-settings-button"));
    const cacheToggle = await screen.findByTestId("settings-cache-toggle");
    await act(async () => {
      fireEvent.click(cacheToggle);
    });

    await waitFor(() => expect(consoleErrorSpy).toHaveBeenCalled());
    consoleErrorSpy.mockRestore();
  });
});

describe("AppRuntime body cache e2e · ⑦ 缓存开关关闭后不再查缓存（msgfix2 F2 S5④，不是只对新写生效）", () => {
  it("关闭缓存开关后——即使库里还有残留命中（模拟 H5 兜底清理还没来得及跑完的窗口期），loadFullTextViaCacheOrFetch 也不会读它，照样发出 msg.fetch 走网络", async () => {
    const bodyCache = new InMemoryBodyCache();
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5(bodyCache);

    // 关闭缓存开关。
    fireEvent.click(screen.getByTestId("app-runtime-back-to-sessions"));
    fireEvent.click(await screen.findByTestId("session-list-settings-button"));
    const cacheToggle = await screen.findByTestId("settings-cache-toggle");
    await user.click(cacheToggle);
    await waitFor(() => expect(cacheToggle.getAttribute("aria-checked")).toBe("false"));
    fireEvent.click(screen.getByTestId("settings-back"));
    fireEvent.click(await screen.findByTestId("session-row"));
    await screen.findByTestId("session-stream-screen");

    // 关闭之后，直接往库里塞一条"残留"命中——模拟 H5 兜底清理还没来得及跑完的窗口期（`clear()`
    // 是异步的，理论上存在一小段窗口库里仍有旧数据）。若 `loadFullTextViaCacheOrFetch()` 仍然去
    // 查缓存，会命中这条并直接从缓存渲染，永远不会发 `msg.fetch`——这正是本用例要防的：关闭缓存
    // 不应该只是"不再写新的"，还应该"不再读旧的"。
    const staleBlocks = [{ type: "text", text: "residual cached content — must never be served with cache toggle off" }];
    const ref = await contentRefFor(706, 1, staleBlocks);
    await bodyCache.put({ room: ROOM, session: "s-1", messageId: 706, contentSha256: ref.content_sha256 }, staleBlocks, ref.bytes);

    await pushMsgCompleted(kRoomRaw, factory, { messageId: 706, seq: 3, ref, totalBytesOverride: 999_999 });

    const loadButton = await screen.findByTestId("msg-fetch-load");
    await user.click(loadButton);

    // 核心断言：即使缓存里有命中，也真的发出了 msg.fetch——证明没有走缓存短路，缓存查询本身被
    // 跳过了（不是"查了但没用上这次命中"）。
    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    expect(sent).toBeTruthy();
    // 且没有把那条残留内容误渲到屏幕上。
    expect(screen.queryByText("residual cached content — must never be served with cache toggle off")).toBeNull();
  });
});
