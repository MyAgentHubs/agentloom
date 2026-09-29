// msgFetch.test.ts — TDD coverage for src/events/msgFetch.ts.
//
// 两层：① `MsgChunkReassembler`——纯逻辑重组状态机，直接构造 `MsgChunkFrame`/`MsgFetchErrorFrame`
// 喂给它（M0 §10.9 的每条不变量各自一个测试：乱序 offset 拒/revision 混拼拒/chunk_len 不符拒/
// sha256 不符弃拉/超 4MiB 拒/末片不足整片不提前完成/空消息终态分片）；② `MsgFetchClient`——发送/
// 接收编排，假 socket（同 `commandChannel.test.ts` 的既有取向，真 AES-GCM CryptoKey + 手动解密
// 验证 wire 内容，不自证式地只信任被测代码自己的 seal/open 互逆）。
//
// `msg_chunk` fixture 样张（data-plane-v1.json）用短占位 sha256/bytes_b64，不是真实对应关系
// （fixture 只验"字段能不能被 parseFrame 正确解析"，见 parseFrame.test.ts）——本文件的重组测试
// 需要真实可验证的 sha256（内容与摘要必须对得上，否则测不出"完成后整体校验"这条），用
// `crypto.subtle.digest("SHA-256", ...)` 现算期望摘要（同 `kdf.ts`/`msgFetch.ts` 生产代码用的同一
// WebCrypto 原语——本仓没有 `@types/node`，`node:crypto` 在这条 tsconfig 下类型不全，见任务书报告
// 「关键决定」一节）。"篡改/不符"分支的反例（sha256_mismatch 等）用硬编码错误值，不依赖计算，仍是
// 真正独立的负向断言，不受这个选择影响。

import { describe, expect, it } from "vitest";
import { open } from "../crypto/envelope.ts";
import { bytesToBase64 } from "../crypto/bytes.ts";
import { ReadyState, type WebSocketCloseInfo, type WebSocketLike } from "../connection/types.ts";
import type { ContentRef, MsgChunkFrame, MsgFetchErrorFrame } from "./parseFrame.ts";
import { MSG_FETCH_AUTO_THRESHOLD_BYTES, MSG_FETCH_TOTAL_BYTES_CAP, MsgChunkReassembler, MsgFetchClient } from "./msgFetch.ts";

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", Uint8Array.from(bytes)));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** 构造一批真实可校验的分片（内容真实、sha256 真实对得上）——`chunkSize` 控制切片粒度,模拟
 *  `build_msg_chunks` 的多片切分。 */
async function buildChunks(content: string, messageId: number, revision: number, chunkSize: number): Promise<MsgChunkFrame[]> {
  const bytes = new TextEncoder().encode(content);
  const contentSha256 = await sha256Hex(bytes);
  const chunks: MsgChunkFrame[] = [];
  let offset = 0;
  while (offset < bytes.length) {
    const end = Math.min(offset + chunkSize, bytes.length);
    const slice = bytes.slice(offset, end);
    chunks.push({
      t: "msg.chunk",
      message_id: messageId,
      revision,
      content_sha256: contentSha256,
      total_bytes: bytes.length,
      offset,
      chunk_len: slice.length,
      bytes_b64: bytesToBase64(slice),
    });
    offset = end;
  }
  if (chunks.length === 0) {
    chunks.push({
      t: "msg.chunk",
      message_id: messageId,
      revision,
      content_sha256: contentSha256,
      total_bytes: 0,
      offset: 0,
      chunk_len: 0,
      bytes_b64: "",
    });
  }
  return chunks;
}

describe("msgFetch.ts constants (task brief §3 numeric landing points)", () => {
  it("MSG_FETCH_AUTO_THRESHOLD_BYTES is 512KiB (『视口内最新一条』自动拉取阈值——超过则要求点开手动拉)", () => {
    expect(MSG_FETCH_AUTO_THRESHOLD_BYTES).toBe(512 * 1024);
  });

  it("MSG_FETCH_TOTAL_BYTES_CAP is 4MiB (M0 §10.9 client-side defensive mirror of the desktop's hard cap)", () => {
    expect(MSG_FETCH_TOTAL_BYTES_CAP).toBe(4 * 1024 * 1024);
  });
});

describe("MsgChunkReassembler · M0 §10.9 重组安全不变量", () => {
  it("single chunk completes immediately and verifies sha256", async () => {
    const [chunk] = await buildChunks("hello world", 1, 1, 1024);
    const reassembler = new MsgChunkReassembler(1);
    const outcome = await reassembler.ingestChunk(chunk!);
    expect(outcome.status).toBe("complete");
    if (outcome.status !== "complete") throw new Error("unreachable");
    expect(new TextDecoder().decode(outcome.bytes)).toBe("hello world");
    expect(outcome.contentRef).toEqual<ContentRef>({
      message_id: 1,
      revision: 1,
      content_sha256: await sha256Hex(new TextEncoder().encode("hello world")),
      total_bytes: 11,
    });
  });

  it("multi-chunk transfer stays in_progress until the last byte, then completes (末片不足整片：中途片不提前判完成)", async () => {
    const chunks = await buildChunks("A".repeat(50), 2, 1, 20); // 3 片：20/20/10
    expect(chunks).toHaveLength(3);
    const reassembler = new MsgChunkReassembler(2);
    const first = await reassembler.ingestChunk(chunks[0]!);
    expect(first).toEqual({ status: "in_progress", receivedBytes: 20, totalBytes: 50 });
    const second = await reassembler.ingestChunk(chunks[1]!);
    expect(second).toEqual({ status: "in_progress", receivedBytes: 40, totalBytes: 50 });
    // 第三片（末片，10 字节，不足一整片 chunkSize=20）——收完才是 total_bytes,不能提前判完成。
    const third = await reassembler.ingestChunk(chunks[2]!);
    expect(third.status).toBe("complete");
    if (third.status !== "complete") throw new Error("unreachable");
    expect(third.bytes.length).toBe(50);
  });

  it("empty message (total_bytes=0) completes on the single terminal empty chunk (build_msg_chunks 的恒非空 Vec 边界)", async () => {
    const [chunk] = await buildChunks("", 3, 1, 1024);
    const reassembler = new MsgChunkReassembler(3);
    const outcome = await reassembler.ingestChunk(chunk!);
    expect(outcome.status).toBe("complete");
    if (outcome.status !== "complete") throw new Error("unreachable");
    expect(outcome.bytes.length).toBe(0);
  });

  it("rejects a non-zero first offset (offset_not_contiguous)", async () => {
    const [chunk] = await buildChunks("hello", 1, 1, 1024);
    const malformed: MsgChunkFrame = { ...chunk!, offset: 5 };
    const reassembler = new MsgChunkReassembler(1);
    expect(await reassembler.ingestChunk(malformed)).toEqual({ status: "rejected", reason: "offset_not_contiguous" });
  });

  it("rejects a gap between chunks (offset_not_contiguous)", async () => {
    const chunks = await buildChunks("A".repeat(30), 1, 1, 10); // 3 片
    const reassembler = new MsgChunkReassembler(1);
    await reassembler.ingestChunk(chunks[0]!);
    const gapped: MsgChunkFrame = { ...chunks[2]!, offset: 20 }; // 跳过第二片,留空洞
    expect(await reassembler.ingestChunk(gapped)).toEqual({ status: "rejected", reason: "offset_not_contiguous" });
  });

  it("rejects an overlapping offset (offset_not_contiguous)", async () => {
    const chunks = await buildChunks("A".repeat(30), 1, 1, 10);
    const reassembler = new MsgChunkReassembler(1);
    await reassembler.ingestChunk(chunks[0]!);
    const overlapped: MsgChunkFrame = { ...chunks[1]!, offset: 5 }; // 与已收字节重叠
    expect(await reassembler.ingestChunk(overlapped)).toEqual({ status: "rejected", reason: "offset_not_contiguous" });
  });

  it("rejects a revision switch mid-stream (revision_mismatch)", async () => {
    const chunks = await buildChunks("A".repeat(30), 1, 1, 10);
    const reassembler = new MsgChunkReassembler(1);
    await reassembler.ingestChunk(chunks[0]!);
    const revisionSwitch: MsgChunkFrame = { ...chunks[1]!, revision: 2 };
    expect(await reassembler.ingestChunk(revisionSwitch)).toEqual({ status: "rejected", reason: "revision_mismatch" });
  });

  it("rejects a content_sha256 switch mid-stream (revision_mismatch)", async () => {
    const chunks = await buildChunks("A".repeat(30), 1, 1, 10);
    const reassembler = new MsgChunkReassembler(1);
    await reassembler.ingestChunk(chunks[0]!);
    const shaSwitch: MsgChunkFrame = { ...chunks[1]!, content_sha256: "f".repeat(64) };
    expect(await reassembler.ingestChunk(shaSwitch)).toEqual({ status: "rejected", reason: "revision_mismatch" });
  });

  it("rejects a total_bytes switch mid-stream (revision_mismatch)", async () => {
    const chunks = await buildChunks("A".repeat(30), 1, 1, 10);
    const reassembler = new MsgChunkReassembler(1);
    await reassembler.ingestChunk(chunks[0]!);
    const totalSwitch: MsgChunkFrame = { ...chunks[1]!, total_bytes: 999 };
    expect(await reassembler.ingestChunk(totalSwitch)).toEqual({ status: "rejected", reason: "revision_mismatch" });
  });

  it("rejects chunk_len not matching the decoded bytes_b64 length (chunk_len_mismatch)", async () => {
    const [chunk] = await buildChunks("hello", 1, 1, 1024);
    const malformed: MsgChunkFrame = { ...chunk!, chunk_len: chunk!.chunk_len + 1 };
    const reassembler = new MsgChunkReassembler(1);
    expect(await reassembler.ingestChunk(malformed)).toEqual({ status: "rejected", reason: "chunk_len_mismatch" });
  });

  it("rejects non-canonical base64 bytes_b64 (chunk_len_mismatch: decode fails)", async () => {
    const [chunk] = await buildChunks("hello", 1, 1, 1024);
    const malformed: MsgChunkFrame = { ...chunk!, bytes_b64: "not-valid-base64!!" };
    const reassembler = new MsgChunkReassembler(1);
    expect(await reassembler.ingestChunk(malformed)).toEqual({ status: "rejected", reason: "chunk_len_mismatch" });
  });

  it("rejects a first chunk declaring total_bytes over the 4MiB cap (too_large)", async () => {
    const [chunk] = await buildChunks("hello", 1, 1, 1024);
    const oversized: MsgChunkFrame = { ...chunk!, total_bytes: MSG_FETCH_TOTAL_BYTES_CAP + 1 };
    const reassembler = new MsgChunkReassembler(1);
    expect(await reassembler.ingestChunk(oversized)).toEqual({ status: "rejected", reason: "too_large" });
  });

  it("rejects a chunk that would overflow total_bytes (overflow)", async () => {
    const chunks = await buildChunks("A".repeat(30), 1, 1, 10);
    const reassembler = new MsgChunkReassembler(1);
    await reassembler.ingestChunk(chunks[0]!);
    await reassembler.ingestChunk(chunks[1]!);
    const overflowing: MsgChunkFrame = { ...chunks[2]!, chunk_len: 15, bytes_b64: bytesToBase64(new Uint8Array(15)) };
    expect(await reassembler.ingestChunk(overflowing)).toEqual({ status: "rejected", reason: "overflow" });
  });

  it("discards the reassembled content when the final SHA-256 does not match (sha256_mismatch — 弃拉,绝不展示部分/损坏内容)", async () => {
    const chunks = await buildChunks("hello world", 1, 1, 1024);
    const tampered: MsgChunkFrame = { ...chunks[0]!, content_sha256: "0".repeat(64) };
    const reassembler = new MsgChunkReassembler(1);
    expect(await reassembler.ingestChunk(tampered)).toEqual({ status: "rejected", reason: "sha256_mismatch" });
  });

  it("rejects a chunk whose message_id does not match the reassembler's (defense-in-depth — route already keys by command_id)", async () => {
    const [chunk] = await buildChunks("hello", 1, 1, 1024);
    const reassembler = new MsgChunkReassembler(999);
    expect((await reassembler.ingestChunk(chunk!)).status).toBe("rejected");
  });

  it("ingestError passes through code/current_ref (error 终态)", () => {
    const reassembler = new MsgChunkReassembler(1);
    const frame: MsgFetchErrorFrame = { t: "msg.fetch.error", code: "busy" };
    expect(reassembler.ingestError(frame)).toEqual({ status: "error", code: "busy", currentRef: undefined });

    const staleFrame: MsgFetchErrorFrame = {
      t: "msg.fetch.error",
      code: "stale_revision",
      current_ref: { message_id: 1, revision: 2, content_sha256: "a".repeat(64), total_bytes: 5 },
    };
    expect(reassembler.ingestError(staleFrame)).toEqual({
      status: "error",
      code: "stale_revision",
      currentRef: staleFrame.current_ref,
    });
  });
});

// ---------------------------------------------------------------------------
// MsgFetchClient · 发送/接收编排——假 socket，真 AES-GCM CryptoKey，手动解密验证 wire 内容
// （同 commandChannel.test.ts 的既有取向，不自证式地只信任被测代码自己的 seal/open 互逆）。
// ---------------------------------------------------------------------------

const ROOM = "0123456789abcdef0123456789abcdef";
const SESSION = "sess-1";

class FakeSocket implements WebSocketLike {
  readyState: number = ReadyState.OPEN;
  onopen: (() => void) | null = null;
  onclose: ((event: WebSocketCloseInfo) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  sent: string[] = [];
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    this.readyState = ReadyState.CLOSED;
  }
}

async function makeKRoomKey(fill = 7): Promise<CryptoKey> {
  const raw = new Uint8Array(32).fill(fill);
  return crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]);
}

async function decryptEnvelope(envelope: Record<string, unknown>, kRoomKey: CryptoKey): Promise<Record<string, unknown>> {
  const meta = {
    v: envelope.v as number,
    room: envelope.room as string,
    epoch: envelope.epoch as number,
    kind: envelope.kind as string,
    session: envelope.session as string | null,
    command_id: envelope.command_id as string | null,
  };
  const plaintext = await open(kRoomKey, meta, envelope.ct as string, envelope.n as string);
  return JSON.parse(new TextDecoder().decode(plaintext));
}

async function waitUntil(predicate: () => boolean, timeoutMs = 3000, stepMs = 5): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("waitUntil: condition never became true within timeout");
    await new Promise((resolve) => setTimeout(resolve, stepMs));
  }
}

interface Harness {
  client: MsgFetchClient;
  socket: FakeSocket;
  kRoomKey: CryptoKey;
  applyFullTextCalls: Array<{ sessionId: string; messageId: number; revision: number; blocks: unknown[] }>;
  /** msgfix2 U4：body cache 写入挂钩点的调用记录——见 `MsgFetchClientDeps.onFetchCached`。 */
  onFetchCachedCalls: Array<{
    room: string;
    sessionId: string;
    messageId: number;
    revision: number;
    contentSha256: string;
    blocks: unknown[];
    bytes: Uint8Array;
  }>;
  trySendControlSlotResult: boolean;
}

async function makeHarness(overrides?: { timeoutMs?: number; trySendControlSlotResult?: boolean }): Promise<Harness> {
  const socket = new FakeSocket();
  const kRoomKey = await makeKRoomKey();
  const applyFullTextCalls: Harness["applyFullTextCalls"] = [];
  const onFetchCachedCalls: Harness["onFetchCachedCalls"] = [];
  const trySendControlSlotResult = overrides?.trySendControlSlotResult ?? true;
  const client = new MsgFetchClient({
    room: ROOM,
    kRoomKey,
    getEpoch: () => 1,
    getSocket: () => socket,
    trySendControlSlot: async () => trySendControlSlotResult,
    applyFullText: (sessionId, messageId, revision, blocks) => {
      applyFullTextCalls.push({ sessionId, messageId, revision, blocks });
    },
    onFetchCached: (input) => {
      onFetchCachedCalls.push(input);
    },
    timeoutMs: overrides?.timeoutMs,
  });
  return { client, socket, kRoomKey, applyFullTextCalls, onFetchCachedCalls, trySendControlSlotResult };
}

/** 从 harness 的 socket 里取出（且解密验证）刚发的那条 `msg.fetch` 请求的 `command_id`——`expectPlaintext`
 *  给定时额外核对密文体真解出来的内容（不自证式地只信任被测代码自己的 seal/open 互逆）。 */
async function sentCommandId(h: Harness, expectPlaintext?: Record<string, unknown>): Promise<string> {
  await waitUntil(() => h.socket.sent.length > 0);
  const envelope = JSON.parse(h.socket.sent.at(-1)!);
  expect(envelope.kind).toBe("control");
  if (expectPlaintext) {
    const plaintext = await decryptEnvelope(envelope, h.kRoomKey);
    expect(plaintext).toEqual(expectPlaintext);
  }
  return envelope.command_id as string;
}

describe("MsgFetchClient · 发送姿势 + 单飞行", () => {
  it("startFetch seals and sends a msg.fetch request whose plaintext matches buildMsgFetchRequest()", async () => {
    const h = await makeHarness();
    const result = await h.client.startFetch(SESSION, 4821, 1);
    expect(result).toEqual({ ok: true });
    await sentCommandId(h, { t: "msg.fetch", session: SESSION, message_id: 4821, revision: 1, offset: 0 });
    expect(h.client.getState(4821)).toEqual({ status: "loading", receivedBytes: 0 });
  });

  it("a second startFetch for the same session while one is in flight is rejected as local_busy (single-flight, task brief §3)", async () => {
    const h = await makeHarness();
    const first = h.client.startFetch(SESSION, 4821, 1);
    // 第二次调用必须在第一次的 await 落定之前就发起——这正是回归测试要卡死的竞态窗口（见
    // msgFetch.ts::activeSessions 头注）：同步 Set 占位保证即使这里在 seal() 真异步操作完成前
    // 就发起第二次调用，也会被正确挡下。
    const second = await h.client.startFetch(SESSION, 9999, 1);
    expect(second).toEqual({ ok: false, reason: "local_busy" });
    await first;
  });

  it("startFetch for a different session is not blocked by another session's in-flight fetch", async () => {
    const h = await makeHarness();
    void h.client.startFetch(SESSION, 4821, 1);
    const other = await h.client.startFetch("sess-2", 111, 1);
    expect(other.ok).toBe(true);
  });

  it("no connection (epoch null) fails fast without sending", async () => {
    const socket = new FakeSocket();
    const kRoomKey = await makeKRoomKey();
    const client = new MsgFetchClient({
      room: ROOM,
      kRoomKey,
      getEpoch: () => null,
      getSocket: () => socket,
      trySendControlSlot: async () => true,
      applyFullText: () => {},
    });
    const result = await client.startFetch(SESSION, 1, 1);
    expect(result).toEqual({ ok: false, reason: "no_connection" });
    expect(socket.sent).toHaveLength(0);
  });

  it("local control-slot rejection (rate limited) surfaces as an error state with errorReason busy", async () => {
    const h = await makeHarness({ trySendControlSlotResult: false });
    const result = await h.client.startFetch(SESSION, 4821, 1);
    expect(result).toEqual({ ok: false, reason: "local_busy" });
    expect(h.client.getState(4821)).toEqual({ status: "error", errorReason: "busy" });
  });

  it("times out and frees the session slot when no reply ever arrives", async () => {
    const h = await makeHarness({ timeoutMs: 20 });
    await h.client.startFetch(SESSION, 4821, 1);
    await waitUntil(() => h.client.getState(4821).status === "error");
    expect(h.client.getState(4821).errorReason).toBe("timeout");
    // session 槽位已释放——同一 session 现在可以发起新的 fetch。
    const retry = await h.client.startFetch(SESSION, 4821, 1);
    expect(retry.ok).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 滑动超时（msgfix2 U3·设计稿 v4.1 §4.3 择入 P2："收到任一片即续期"）——`MSG_FETCH_TIMEOUT_MS`
// 头注引用的两个方向：① 收到一片就把窗口从那一刻重新起算，原本一次性窗口本该到期的时刻不再触发
// 超时；② 续期不是无限——真的不再来新片时，仍在"最近一片到达 + 窗口"这个时刻超时,不会永远等下去。
// ---------------------------------------------------------------------------
/** 供滑动超时用例复用——`MsgFetchClient.handleReply()` 的 "complete" 分支要求重组出来的字节
 *  必须是合法 JSON blocks 数组，否则会走 `"malformed_content"` 分支（同 `MsgChunkReassembler` 那组
 *  纯重组测试不同——那组不经 `MsgFetchClient`，用 `"A".repeat(N)` 这种非 JSON 占位内容完全没问题；
 *  这里必须用真的能 `JSON.parse` 成数组的内容）。`desiredChunks` 决定 `buildChunks()` 的
 *  `chunkSize`（按总字节数均分，向上取整,保证真的切出这么多片）。 */
async function buildJsonChunks(messageId: number, revision: number, desiredChunks: number): Promise<{ chunks: Awaited<ReturnType<typeof buildChunks>>; blocks: unknown[] }> {
  const blocks = [{ type: "text", text: `full content for message ${messageId} reassembled across ${desiredChunks} wire chunks` }];
  const content = JSON.stringify(blocks);
  const totalBytes = new TextEncoder().encode(content).length;
  const chunkSize = Math.ceil(totalBytes / desiredChunks);
  const chunks = await buildChunks(content, messageId, revision, chunkSize);
  return { chunks, blocks };
}

describe("MsgFetchClient · 滑动超时（msgfix2 U3：收到任一片即续期）", () => {
  it("a chunk arriving after most of the original one-shot window has elapsed resets the clock — the transfer does not time out at the original deadline", async () => {
    // 数值刻意留足余量（不是紧贴边界的裸 timeoutMs=20 这类写法）——本用例断言的是"经过原始
    // 一次性窗口早该到期的那一刻，传输仍然存活"，真实定时器（Node `setTimeout`）本身就有几毫秒
    // 到十几毫秒的调度抖动，窗口/等待值必须留够裕量，否则会因为计时器抖动而假性失败，不是逻辑错。
    const h = await makeHarness({ timeoutMs: 150 });
    await h.client.startFetch(SESSION, 4821, 1);
    const commandId = await sentCommandId(h);

    // 模拟慢链路：第一片直到接近（但不超过）原始一次性窗口边缘才到达（100ms < 150ms）。
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(h.client.getState(4821).status).toBe("loading");

    const { chunks } = await buildJsonChunks(4821, 1, 2); // 两片
    await h.client.handleReply(commandId, chunks[0]!); // 首片到达——续期起点重新落在这一刻。

    // 再等 80ms——距离 startFetch 已经过去 ~180ms，早就超过原始一次性 150ms 窗口（若超时仍是
    // "一次性从发起那刻算"，这里必然已经转 error）；但距离首片到达只过去 80ms（< 150ms 续期窗口），
    // 仍应是 loading。
    await new Promise((resolve) => setTimeout(resolve, 80));
    expect(h.client.getState(4821).status).toBe("loading"); // 仍未超时——证明计时器已从首片到达那刻重新起算。

    // 第二片按时到达，正常完成——滑动窗口没有意外提前判死这次传输。
    await h.client.handleReply(commandId, chunks[1]!);
    expect(h.client.getState(4821).status).toBe("idle");
  });

  it("bounded, not infinite: if no further chunk ever arrives after the first one, it still times out relative to that last chunk's arrival", async () => {
    const h = await makeHarness({ timeoutMs: 30 });
    await h.client.startFetch(SESSION, 4821, 1);
    const commandId = await sentCommandId(h);

    const chunks = await buildChunks("A".repeat(20), 4821, 1, 10);
    await h.client.handleReply(commandId, chunks[0]!);
    expect(h.client.getState(4821).status).toBe("loading");

    // 再没有第二片到达——最终仍然超时,不是永久 loading。
    await waitUntil(() => h.client.getState(4821).status === "error");
    expect(h.client.getState(4821).errorReason).toBe("timeout");
  });

  it("a multi-chunk transfer where each gap is individually within the window, but the cumulative elapsed time from start exceeds the original one-shot budget, still completes (proves the reset is real, not a coincidence of timing slack)", async () => {
    const h = await makeHarness({ timeoutMs: 150 });
    await h.client.startFetch(SESSION, 4821, 1);
    const commandId = await sentCommandId(h);

    const { chunks } = await buildJsonChunks(4821, 1, 3); // 3 片
    await h.client.handleReply(commandId, chunks[0]!); // T≈0——续期起点。
    await new Promise((resolve) => setTimeout(resolve, 90)); // < 150ms 续期窗口。
    expect(h.client.getState(4821).status).toBe("loading");

    await h.client.handleReply(commandId, chunks[1]!); // T≈90——再次续期起点。
    // 再等 90ms（累计已过去 ~180ms，超过原始一次性 150ms 窗口——若超时仍是"一次性从发起那刻算"，
    // 这里必然已经转 error；但距离上一片到达只过去 90ms < 150ms 续期窗口，仍应是 loading）。
    await new Promise((resolve) => setTimeout(resolve, 90));
    expect(h.client.getState(4821).status).toBe("loading"); // 仍未超时——第二片同样续了期。

    await h.client.handleReply(commandId, chunks[2]!);
    expect(h.client.getState(4821).status).toBe("idle");
  });
});

describe("MsgFetchClient · handleReply routing + applyFullText", () => {
  it("a complete single-chunk reply decodes the JSON blocks array and calls applyFullText", async () => {
    const h = await makeHarness();
    await h.client.startFetch(SESSION, 4821, 3);
    const commandId = await sentCommandId(h);

    const blocks = [{ type: "text", text: "full content" }];
    const bytes = new TextEncoder().encode(JSON.stringify(blocks));
    const chunk: MsgChunkFrame = {
      t: "msg.chunk",
      message_id: 4821,
      revision: 3,
      content_sha256: await sha256Hex(bytes),
      total_bytes: bytes.length,
      offset: 0,
      chunk_len: bytes.length,
      bytes_b64: bytesToBase64(bytes),
    };
    await h.client.handleReply(commandId, chunk);

    expect(h.applyFullTextCalls).toEqual([{ sessionId: SESSION, messageId: 4821, revision: 3, blocks }]);
    expect(h.client.getState(4821)).toEqual({ status: "idle" });

    // msgfix2 U4：`onFetchCached` 挂钩点在 `applyFullText` 之后触发，带上 SHA 校验后的原始 bytes
    // （非再序列化）+ 已解出的 blocks，供调用方（`app/AppRuntime.tsx`）写 body cache。
    expect(h.onFetchCachedCalls).toEqual([
      { room: ROOM, sessionId: SESSION, messageId: 4821, revision: 3, contentSha256: chunk.content_sha256, blocks, bytes },
    ]);
  });

  it("decoded bytes that are not a JSON array surface as malformed_content and never call applyFullText or onFetchCached", async () => {
    const h = await makeHarness();
    await h.client.startFetch(SESSION, 4821, 1);
    const commandId = await sentCommandId(h);

    const bytes = new TextEncoder().encode("not json at all {{{");
    const chunk: MsgChunkFrame = {
      t: "msg.chunk",
      message_id: 4821,
      revision: 1,
      content_sha256: await sha256Hex(bytes),
      total_bytes: bytes.length,
      offset: 0,
      chunk_len: bytes.length,
      bytes_b64: bytesToBase64(bytes),
    };
    await h.client.handleReply(commandId, chunk);

    expect(h.applyFullTextCalls).toHaveLength(0);
    expect(h.onFetchCachedCalls).toHaveLength(0);
    expect(h.client.getState(4821)).toEqual({ status: "error", errorReason: "malformed_content" });
  });

  it("onFetchCached is optional — omitting it does not throw and the rest of the complete path still runs", async () => {
    const socket = new FakeSocket();
    const kRoomKey = await makeKRoomKey();
    const applyFullTextCalls: Array<{ sessionId: string; messageId: number; revision: number; blocks: unknown[] }> = [];
    const client = new MsgFetchClient({
      room: ROOM,
      kRoomKey,
      getEpoch: () => 1,
      getSocket: () => socket,
      trySendControlSlot: async () => true,
      applyFullText: (sessionId, messageId, revision, blocks) => {
        applyFullTextCalls.push({ sessionId, messageId, revision, blocks });
      },
      // onFetchCached 故意省略——`MsgFetchClientDeps.onFetchCached` 是可选字段。
    });
    await client.startFetch(SESSION, 4821, 1);
    await waitUntil(() => socket.sent.length > 0);
    const commandId = JSON.parse(socket.sent.at(-1)!).command_id as string;

    const blocks = [{ type: "text", text: "hi" }];
    const bytes = new TextEncoder().encode(JSON.stringify(blocks));
    const chunk: MsgChunkFrame = {
      t: "msg.chunk",
      message_id: 4821,
      revision: 1,
      content_sha256: await sha256Hex(bytes),
      total_bytes: bytes.length,
      offset: 0,
      chunk_len: bytes.length,
      bytes_b64: bytesToBase64(bytes),
    };
    await expect(client.handleReply(commandId, chunk)).resolves.toBeUndefined();
    expect(applyFullTextCalls).toEqual([{ sessionId: SESSION, messageId: 4821, revision: 1, blocks }]);
  });

  it("an unrelated/unknown command_id is ignored (relay routing already resolved this — no crash, no state change)", async () => {
    const h = await makeHarness();
    await h.client.startFetch(SESSION, 4821, 1);
    await sentCommandId(h);
    const frame: MsgFetchErrorFrame = { t: "msg.fetch.error", code: "not_found" };
    await h.client.handleReply("some-other-command-id", frame);
    // 状态不受影响——仍在 loading（不属于这条 fetch 的帧被忽略）。
    expect(h.client.getState(4821).status).toBe("loading");
  });

  it("a busy error reply frees the session slot and surfaces a retryable error state", async () => {
    const h = await makeHarness();
    await h.client.startFetch(SESSION, 4821, 1);
    const commandId = await sentCommandId(h);
    const frame: MsgFetchErrorFrame = { t: "msg.fetch.error", code: "busy" };
    await h.client.handleReply(commandId, frame);
    expect(h.client.getState(4821)).toEqual({ status: "error", errorReason: "busy", currentRef: undefined });
    // session 槽位已释放。
    const retry = await h.client.startFetch(SESSION, 4821, 1);
    expect(retry.ok).toBe(true);
  });

  it("a stale_revision error reply carries current_ref through to the UI state (M0 §10.5: guides a re-fetch at the new revision)", async () => {
    const h = await makeHarness();
    await h.client.startFetch(SESSION, 4821, 1);
    const commandId = await sentCommandId(h);
    const currentRef: ContentRef = { message_id: 4821, revision: 4, content_sha256: "a".repeat(64), total_bytes: 99 };
    const frame: MsgFetchErrorFrame = { t: "msg.fetch.error", code: "stale_revision", current_ref: currentRef };
    await h.client.handleReply(commandId, frame);
    expect(h.client.getState(4821)).toEqual({ status: "error", errorReason: "stale_revision", currentRef });
  });

  it("progress updates surface receivedBytes/totalBytes while a multi-chunk transfer is in flight", async () => {
    const h = await makeHarness();
    await h.client.startFetch(SESSION, 4821, 1);
    const commandId = await sentCommandId(h);
    const content = "A".repeat(30);
    const bytes = new TextEncoder().encode(content);
    const sha = await sha256Hex(bytes);
    const chunk1: MsgChunkFrame = {
      t: "msg.chunk",
      message_id: 4821,
      revision: 1,
      content_sha256: sha,
      total_bytes: 30,
      offset: 0,
      chunk_len: 20,
      bytes_b64: bytesToBase64(bytes.slice(0, 20)),
    };
    await h.client.handleReply(commandId, chunk1);
    expect(h.client.getState(4821)).toEqual({ status: "loading", receivedBytes: 20, totalBytes: 30 });
  });
});

