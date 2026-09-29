// AppRuntime.msgFetch.e2e.test.tsx — end-to-end: verifies AppRuntime's real wiring for the `reply`
// kind route (M0 §10.1) — msg.fetch send posture, msg.chunk reassembly, preview-card-to-full-text
// replacement, and viewport-triggered auto fetch.
//
// Reuses the discipline documented in `AppRuntime.commands.e2e.test.tsx`'s header: the fake relay
// side encrypts with an independent implementation (it does not import `seal`/`open`/`buildAAD`
// from `crypto/envelope.ts`), proving "an envelope produced by production code decrypts correctly
// under a completely independent implementation" — this file additionally verifies the reverse
// direction: an envelope encrypted by an independent implementation with `kind=reply` also
// decrypts and routes correctly through AppRuntime (previously `processFrame` only recognized the
// event/live kinds, and `reply` would be silently dropped by the `kindSkipped` branch — this is the
// highest-risk wiring point for this feature and must be verified end-to-end, not just trusted to
// unit tests).
//
// Covers:
//   1. Clicking "load full text" sends msg.fetch (verified via real decryption) -> a single-chunk
//      msg.chunk reply (kind=reply) -> the preview is replaced by the full text, button disappears.
//   2. The newest ref-bearing message in the viewport, if <=512KiB, auto-fetches without a click.
//   3. msg.fetch.error (busy) -> a retryable error state; retrying succeeds.
//   4. msg.fetch.error (soft_deleted/forbidden/too_large) -> terminal, non-retryable, no hang.

import "fake-indexeddb/auto";
import { describe, expect, it } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach } from "vitest";
import { ReadyState, type WebSocketCloseInfo, type WebSocketFactory, type WebSocketLike } from "../connection/types.ts";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import { InMemoryKeyStore, importNonExtractableAesGcmKey } from "../store/key-store.ts";
import { IndexedDbEventStore } from "../store/indexeddbEventStore.ts";
import { InMemoryCommandLedger } from "../store/commandLedger.ts";
import { AppRuntime } from "./AppRuntime.tsx";

afterEach(() => {
  cleanup();
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

/** 假桌面按 M0 §10.1 加密一条 `kind=reply` 帧——`command_id` 必填、`seq`/`client_msg_id` 恒不带。 */
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

/** 装配到"已选中会话 s-1、epoch 已知（=5）"——同 `AppRuntime.commands.e2e.test.tsx` 的既有前置。 */
async function setupSelectedSessionAtEpoch5() {
  const { kRoomRaw, stored } = await makeStoredCredentials();
  const keyStore = new InMemoryKeyStore();
  await keyStore.saveKeys(stored);
  const eventStore = new IndexedDbEventStore(`apprt-msgfetch-test-${crypto.randomUUID()}`);
  const ledger = new InMemoryCommandLedger();
  const factory = new FakeWebSocketFactory();

  render(
    <AppRuntime
      stored={stored}
      keyStore={keyStore}
      webSocketFactory={factory.factory}
      eventStore={eventStore}
      commandLedger={ledger}
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

/** 把一份 JSON blocks 数组的真实 UTF-8 字节，算出 content_ref 需要的 sha256/total_bytes。 */
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

describe("AppRuntime msgFetch e2e · ① 点击加载全文 → msg.fetch/msg.chunk 往返 → 预览替换为全文", () => {
  it("发出的 msg.fetch 明文与真实 msg.chunk 回复正确解密、重组、替换预览", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    const fullBlocks = [{ type: "text", text: "the complete investigation report, much longer than the preview" }];
    const ref = await contentRefFor(101, 1, fullBlocks);
    const msgCompletedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "msg-101",
      epoch: 5,
      payload: {
        t: "msg.completed",
        message_id: 101,
        role: "assistant",
        blocks: [{ type: "text", text: "preview…点击加载全文" }],
        content_ref: { message_id: ref.message_id, revision: ref.revision, content_sha256: ref.content_sha256, total_bytes: ref.total_bytes },
      },
    });
    await act(async () => {
      factory.last.simulateMessage(msgCompletedFrame);
    });

    const loadButton = await screen.findByTestId("msg-fetch-load");
    await user.click(loadButton);

    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    expect(sent.envelope.kind).toBe("control");
    expect(sent.envelope.session).toBe("s-1");
    expect(sent.plaintext).toEqual({ t: "msg.fetch", session: "s-1", message_id: 101, revision: 1, offset: 0 });
    const commandId = sent.envelope.command_id as string;

    const replyFrame = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId,
      epoch: 5,
      payload: {
        t: "msg.chunk",
        message_id: 101,
        revision: 1,
        content_sha256: ref.content_sha256,
        total_bytes: ref.total_bytes,
        offset: 0,
        chunk_len: ref.bytes.length,
        bytes_b64: bytesToBase64(ref.bytes),
      },
    });
    await act(async () => {
      factory.last.simulateMessage(replyFrame);
    });

    await screen.findByText("the complete investigation report, much longer than the preview");
    await waitFor(() => expect(screen.queryByTestId("msg-fetch-load")).toBeNull());
    expect(screen.queryByText("preview…点击加载全文")).toBeNull();
  });
});

describe("AppRuntime msgFetch e2e · ② 视口内最新一条 ≤512KiB 自动发起 fetch（不点按钮）", () => {
  it("消息一到达就自动发出 msg.fetch，不需要用户点击", async () => {
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    const fullBlocks = [{ type: "text", text: "auto-fetched full content" }];
    const ref = await contentRefFor(202, 1, fullBlocks);
    const msgCompletedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "msg-202",
      epoch: 5,
      payload: {
        t: "msg.completed",
        message_id: 202,
        role: "assistant",
        blocks: [{ type: "text", text: "preview" }],
        content_ref: { message_id: ref.message_id, revision: ref.revision, content_sha256: ref.content_sha256, total_bytes: ref.total_bytes },
      },
    });
    await act(async () => {
      factory.last.simulateMessage(msgCompletedFrame);
    });

    // 没有任何用户点击——直接等自动发出的 msg.fetch。
    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not auto-sent yet");
      return found;
    });
    expect(sent.plaintext).toEqual({ t: "msg.fetch", session: "s-1", message_id: 202, revision: 1, offset: 0 });
  });
});

describe("AppRuntime msgFetch e2e · ③ msg.fetch.error(busy) 可重试，重试后成功", () => {
  it("busy 终态展示可重试提示；点击重试后新一轮 msg.fetch 由真实 msg.chunk 完成", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    const fullBlocks = [{ type: "text", text: "retried content" }];
    const ref = await contentRefFor(303, 1, fullBlocks);
    const msgCompletedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: "msg-303",
      epoch: 5,
      payload: {
        t: "msg.completed",
        message_id: 303,
        role: "assistant",
        blocks: [{ type: "text", text: "preview" }],
        content_ref: { message_id: ref.message_id, revision: ref.revision, content_sha256: ref.content_sha256, total_bytes: 999_999 }, // 超 512KiB 自动阈值——不自动拉，测试专注点击路径。
      },
    });
    await act(async () => {
      factory.last.simulateMessage(msgCompletedFrame);
    });

    const loadButton = await screen.findByTestId("msg-fetch-load");
    await user.click(loadButton);
    const firstSent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    const firstCommandId = firstSent.envelope.command_id as string;

    const busyReply = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId: firstCommandId,
      epoch: 5,
      payload: { t: "msg.fetch.error", code: "busy" },
    });
    await act(async () => {
      factory.last.simulateMessage(busyReply);
    });

    const retryButton = await screen.findByTestId("msg-fetch-retry");
    await user.click(retryButton);

    const secondCommandId = await waitFor(async () => {
      for (const raw of factory.last.sent) {
        const decoded = await decryptSentEnvelope(raw, kRoomRaw);
        if (decoded.plaintext.t === "msg.fetch" && decoded.envelope.command_id !== firstCommandId) {
          return decoded.envelope.command_id as string;
        }
      }
      throw new Error("retry msg.fetch not sent yet");
    });

    const successReply = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId: secondCommandId,
      epoch: 5,
      payload: {
        t: "msg.chunk",
        message_id: 303,
        revision: 1,
        content_sha256: ref.content_sha256,
        total_bytes: ref.bytes.length,
        offset: 0,
        chunk_len: ref.bytes.length,
        bytes_b64: bytesToBase64(ref.bytes),
      },
    });
    await act(async () => {
      factory.last.simulateMessage(successReply);
    });

    await screen.findByText("retried content");
  });
});

// ============================================================================
// msgfix2 U3（设计稿 v4.1 §4.3 明确列出的择入 P2）：soft_deleted/forbidden/too_large 三个
// wire code——从（假 relay 加密的）真实 `msg.fetch.error` 帧出发，走 `AppRuntime` 的真接线
// （`kind=reply` 解密路由 → `MsgFetchClient.handleReply` → `SessionStreamScreen` 渲染），断言
// 三者各自展示"全文不可用"、不给重试按钮、`data-error-reason` 对应各自的 code——终态落地，不是
// 卡在 loading 悬挂。三个 code 在 `parseFrame.ts::parseMsgFetchError` 走的是同一条解析分支（跟
// `not_found` 一样都不是 `stale_revision`），data-plane-v1.json 没有专门给这三个 code 各出一张
// fixture 样张（跟 `msgFetch.test.ts` 里 `ingestError` 已经用过的 "busy"/"stale_revision" 自造
// 语料同一惯例）——这里同样直接构造 `msg.fetch.error` payload，走真实加密/解密/路由。
// ============================================================================
describe.each([
  ["soft_deleted", 401],
  ["forbidden", 402],
  ["too_large", 403],
] as const)("AppRuntime msgFetch e2e · ④ msg.fetch.error(%s) → 终态不可重试、不悬挂", (code, messageId) => {
  it(`shows the 'unavailable' label with no retry button and errorReason=${code}`, async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    const ref = await contentRefFor(messageId, 1, [{ type: "text", text: "irrelevant — never fetched" }]);
    const msgCompletedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: 2,
      clientMsgId: `msg-${messageId}`,
      epoch: 5,
      payload: {
        t: "msg.completed",
        message_id: messageId,
        role: "assistant",
        blocks: [{ type: "text", text: "preview" }],
        // 超 512KiB 自动阈值——不自动拉，测试专注点击路径（同 ③ busy 用例的既有取向）。
        content_ref: { message_id: ref.message_id, revision: ref.revision, content_sha256: ref.content_sha256, total_bytes: 999_999 },
      },
    });
    await act(async () => {
      factory.last.simulateMessage(msgCompletedFrame);
    });

    const loadButton = await screen.findByTestId("msg-fetch-load");
    await user.click(loadButton);
    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "msg.fetch");
      if (!found) throw new Error("msg.fetch not sent yet");
      return found;
    });
    const commandId = sent.envelope.command_id as string;

    const errorReply = await encryptReplyFrame(kRoomRaw, {
      session: "s-1",
      commandId,
      epoch: 5,
      payload: { t: "msg.fetch.error", code },
    });
    await act(async () => {
      factory.last.simulateMessage(errorReply);
    });

    const errorFooter = await screen.findByTestId("msg-fetch-error");
    expect(errorFooter.getAttribute("data-error-reason")).toBe(code);
    expect(errorFooter.textContent).toBe("Full text unavailable"); // 终态——用户可见文案正确。
    expect(screen.queryByTestId("msg-fetch-retry")).toBeNull(); // 不可重试——不误导用户再点一次必然再失败的按钮。
    // 不悬挂：loading footer 也不在了（不是卡在"正在加载"）。
    expect(screen.queryByTestId("msg-fetch-loading")).toBeNull();
    expect(screen.queryByTestId("msg-fetch-load")).toBeNull();
  });
});
