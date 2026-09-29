// msgFetch.ts — full-content fetch client: `msg.fetch` request handling + `msg.chunk`/
// `msg.fetch.error` 重组状态机（M0 §10.4/§10.5/§10.9）。
//
// 权威参照（只读对照）：
//   - The wire protocol's core reassembly rules —
//     「重组安全 · 滥用闸 · reply 队列语义」，本文件的 `MsgChunkReassembler` 是它在客户端的落点。
//   - `app/src-tauri/src/remote_gateway.rs::build_msg_chunks`（生产切片器，只读对照）：切片按裸
//     字节边界切、`content_sha256`/`total_bytes` 恒对全量内容计算。
//     Synced with v1.9.2 semantics (new wording for `handle_msg_fetch_at` step ③.5):
//     `offset > total_bytes`——协议误用，服务端不再静默钳位到 `total_bytes`，直接回
//     `msg.fetch.error{code:"not_found"}`（本文件的重组状态机走不到这条路径，`msgFetchClient`
//     发起请求恒带 `offset:0`，见下方"续传"一段）；`offset === total_bytes`——合法的"已经拿到
//     全部内容"收尾态，服务端补一片 `chunk_len:0` 的终态空分片（`build_msg_chunks` 内部仍有一层
//     `start_offset.min(total_bytes)` 防御性钳位，但调用方已经把 `offset > total_bytes` 挡在
//     前面，这层钳位在生产路径上不可达，纯纵深防御）——本文件的重组逻辑必须能吃下这个边界
//     （空消息/offset 收尾确认）。
//     **Resume-semantics downgrade** (per the changelog for `
//     m0-protocol.md` changelog v1.9.2）：条文原描述的"同 command_id 断线续传"本批（msgfix1 刀 1）
//     未实现——`msgFetchClient` 恒以 `offset:0` 发起新请求，重连后靠新 command_id 从头重拉，不是
//     从断点续传；桌面 command_id 终态账本（`msg_fetch_command_ledger`）会拒绝同 command_id 复用，
//     旧写法"续传同一个 command_id"在当前实现下反而会被判 `busy`。
//
// **本文件不含 React/WebSocket I/O 细节以外的浏览器依赖**（同 `commandChannel.ts` 头注的既有
// 取向）：`MsgFetchClient` 注入 `getEpoch`/`getSocket`/`trySendControlSlot` 等依赖，可在 vitest
// "logic" project 下纯逻辑驱动；`app/AppRuntime.tsx` 是唯一的生产装配点。
//
// **单飞行范围（任务书 §3 明文："单飞行与桌面侧一致：同 session 同时一个"）**：这是比桌面侧
// M0 §10.4 "同 (session, message_id) 同一时刻一个" 更严格的客户端自选策略——同一 session 下，
// 哪怕是两个不同的 message_id，也只允许一个在飞。理由：这层是纯 UX 简化（避免同时几条"加载全文"
// 都在转圈、抢同一份 relay 出站字节预算），不是协议要求；桌面侧仍按自己的 (session, message_id)
// 粒度独立处理，客户端更严格的本地闸不会违反协议,只会让某些理论上桌面能并发处理的请求在本地被
// 拒（`startFetch` 返回 `{ ok:false, reason:"local_busy" }`，调用方按钮保持"未开始"视觉态）。

import { decodeCanonicalBase64, bytesToHex, utf8Bytes } from "../crypto/bytes.ts";
import { seal } from "../crypto/envelope.ts";
import { envelopeMeta, buildCommandEnvelope } from "../app/wireEnvelope.ts";
import { buildMsgFetchRequest, type ContentRef, type MsgChunkFrame, type MsgFetchErrorCode, type MsgFetchErrorFrame } from "./parseFrame.ts";
import { ReadyState, type WebSocketLike } from "../connection/types.ts";

/** M0 §10.9："total_bytes 上限：4MiB"。客户端侧防御性复核——正常情况下桌面自己会先拒
 *  （`msg.fetch.error{code:"too_large"}`），这里是纵深防御，防一个不遵守协议的对端。 */
export const MSG_FETCH_TOTAL_BYTES_CAP = 4 * 1024 * 1024;

/** 客户端本地超时——与桌面 `MSG_FETCH_INFLIGHT_TIMEOUT_MS`（remote_gateway.rs:191）同一个数值，
 *  不是巧合：超过这个时长桌面自己也已经认为这次 fetch 该释放占用了，客户端没必要等更久。
 *
 *  **滑动窗口（msgfix2 U3·设计稿 v4.1 §4.3 择入 P2「三端超时口径」）**：不是"发起后一次性等
 *  `MSG_FETCH_TIMEOUT_MS`"——`handleReply()` 每收到一片（`in_progress`）就把这个计时器重新起算
 *  一次（`resetTimeout()`），对齐 relay 每帧续期语义（同一份文件头注引用的 M0 §10 精神：只要还在
 *  真的收数据，就不该因为"距离最初发起已经过去太久"而误判超时）。慢链路上一次 4MiB 的多片拉取
 *  不再必死——只要相邻两片之间的间隔不超过这个窗口，传输就能一直续下去；真的卡住不再来片时，仍会
 *  在最近一片到达后的这个窗口内超时，不会无限等待（`msgFetch.test.ts` 的
 *  "chunk arrival slides the timeout window forward" / "no further chunks…still times out" 两条
 *  用例分别锁死这两个方向）。桌面侧 `MSG_FETCH_INFLIGHT_TIMEOUT_MS` 是单飞行占用闸的语义（不是
 *  给客户端下载体验计时），不跟着改——两端各自独立，见设计稿该节"桌面 30s 不变……条文注明差异"。 */
export const MSG_FETCH_TIMEOUT_MS = 30_000;

/** 「视口内最新一条」自动拉取的尺寸阈值（任务书 §3："≤512KiB 自动·超过点开才拉"）。 */
export const MSG_FETCH_AUTO_THRESHOLD_BYTES = 512 * 1024;

// ---------------------------------------------------------------------------
// 重组状态机（M0 §10.9）——纯逻辑，不碰 socket/WS，只吃已解析好的 `MsgChunkFrame`/
// `MsgFetchErrorFrame`，可独立单测。
// ---------------------------------------------------------------------------

export type ReassembleRejectReason =
  /** 首片 offset 不是 0，或后续片 offset 与"已收字节数"不连续（含空洞/重叠）。 */
  | "offset_not_contiguous"
  /** 同一 command_id 的分片之间 revision/content_sha256/total_bytes 不一致——混拼。 */
  | "revision_mismatch"
  /** `chunk_len` 与 `bytes_b64` 解码后的实际字节数不符，或 `bytes_b64` 本身不是合法 base64。 */
  | "chunk_len_mismatch"
  /** 首片 `total_bytes` 超过 4MiB 上限（客户端侧防御性复核，见 `MSG_FETCH_TOTAL_BYTES_CAP`）。 */
  | "too_large"
  /** 累计字节数一旦超过 `total_bytes`（末片把内容撑爆）。 */
  | "overflow"
  /** 拼完全部分片后整体 SHA-256 与声明值不符——弃拉（M0 §10.9："绝不向用户展示部分或损坏内容"）。 */
  | "sha256_mismatch";

export type ReassembleOutcome =
  | { status: "in_progress"; receivedBytes: number; totalBytes: number }
  | { status: "complete"; bytes: Uint8Array; contentRef: ContentRef }
  | { status: "rejected"; reason: ReassembleRejectReason }
  | { status: "error"; code: MsgFetchErrorCode; currentRef?: ContentRef };

interface ExpectedShape {
  revision: number;
  contentSha256: string;
  totalBytes: number;
}

/**
 * 单个 `command_id` 对应的一次拉取的重组状态——`messageId` 由调用方在构造时确定（发起 fetch 时
 * 就知道自己在拉哪条消息，不依赖分片里的字段反推），每片到达先核对 `frame.message_id` 是否与之
 * 一致（防御性——路由已经按 command_id 精确匹配，这层理论上不会撞，纵深防御同文件其它地方的
 * 既有取向）。
 */
export class MsgChunkReassembler {
  private expected: ExpectedShape | null = null;
  private buffer: Uint8Array | null = null;
  private received = 0;

  constructor(private readonly messageId: number) {}

  /** 已收字节数——供 UI 展示拉取进度（可选）。 */
  get receivedBytes(): number {
    return this.received;
  }

  async ingestChunk(frame: MsgChunkFrame): Promise<ReassembleOutcome> {
    if (frame.message_id !== this.messageId) {
      return { status: "rejected", reason: "revision_mismatch" };
    }
    const bytes = decodeCanonicalBase64(frame.bytes_b64);
    if (bytes === null || bytes.length !== frame.chunk_len) {
      return { status: "rejected", reason: "chunk_len_mismatch" };
    }

    if (this.expected === null) {
      if (frame.total_bytes > MSG_FETCH_TOTAL_BYTES_CAP) {
        return { status: "rejected", reason: "too_large" };
      }
      if (frame.offset !== 0) {
        return { status: "rejected", reason: "offset_not_contiguous" };
      }
      this.expected = { revision: frame.revision, contentSha256: frame.content_sha256, totalBytes: frame.total_bytes };
      this.buffer = new Uint8Array(frame.total_bytes);
    } else {
      if (
        frame.revision !== this.expected.revision ||
        frame.content_sha256 !== this.expected.contentSha256 ||
        frame.total_bytes !== this.expected.totalBytes
      ) {
        return { status: "rejected", reason: "revision_mismatch" };
      }
      if (frame.offset !== this.received) {
        return { status: "rejected", reason: "offset_not_contiguous" };
      }
    }

    if (this.received + bytes.length > this.expected.totalBytes) {
      return { status: "rejected", reason: "overflow" };
    }
    // 空分片（`build_msg_chunks` 的"恒非空 Vec"终态收尾，见文件头注）——`buffer.set` 对零长度
    // 输入是安全的 no-op，不需要特判。
    this.buffer!.set(bytes, frame.offset);
    this.received += bytes.length;

    if (this.received < this.expected.totalBytes) {
      return { status: "in_progress", receivedBytes: this.received, totalBytes: this.expected.totalBytes };
    }

    const digest = bytesToHex(new Uint8Array(await requireSubtle().digest("SHA-256", toBufferSource(this.buffer!))));
    if (digest !== this.expected.contentSha256) {
      return { status: "rejected", reason: "sha256_mismatch" };
    }
    return {
      status: "complete",
      bytes: this.buffer!,
      contentRef: {
        message_id: this.messageId,
        revision: this.expected.revision,
        content_sha256: this.expected.contentSha256,
        total_bytes: this.expected.totalBytes,
      },
    };
  }

  ingestError(frame: MsgFetchErrorFrame): ReassembleOutcome {
    return { status: "error", code: frame.code, currentRef: frame.current_ref };
  }
}

function requireSubtle(): SubtleCrypto {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) throw new Error("WebCrypto SubtleCrypto is unavailable in this runtime");
  return subtle;
}

function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}

// ---------------------------------------------------------------------------
// 发送/接收编排——`MsgFetchClient` 是 `sendSnapshotRequest`/`CommandChannel` 那套"seal 前后复核
// epoch/socket"发送姿势在 `msg.fetch` 上的落点，加上按 command_id 路由到期 `MsgChunkReassembler`
// 的接收半边。
// ---------------------------------------------------------------------------

export type MsgFetchUiStatus = "idle" | "loading" | "error";

/** `errorReason` 六个协议 code 之外再加两个客户端本地终态：`"timeout"`（本地超时放弃,见文件头
 *  `MSG_FETCH_TIMEOUT_MS`）与 `"malformed_content"`（重组成功但拼出来的字节不是合法 UTF-8 JSON
 *  blocks 数组——`AppRuntime.tsx` 解码失败时上报,理论上不该发生但不能让 UI 崩）。 */
export type MsgFetchErrorReason = MsgFetchErrorCode | "timeout" | "malformed_content";

export interface MsgFetchState {
  status: MsgFetchUiStatus;
  receivedBytes?: number;
  totalBytes?: number;
  errorReason?: MsgFetchErrorReason;
  /** 仅 `errorReason:"stale_revision"` 有值——引导重拉的新指针。 */
  currentRef?: ContentRef;
}

const IDLE_STATE: MsgFetchState = { status: "idle" };

export interface MsgFetchClientDeps {
  room: string;
  kRoomKey: CryptoKey;
  getEpoch: () => number | null;
  getSocket: () => WebSocketLike | null;
  /** 复用 `CommandChannel.trySendControlSlot()`——本地滑窗 + 记账，同 `control.snapshot`/
   *  `control.history` 共享同一个 `"control"` 桶（`msg.fetch` 走既有 control 通道，M0 §10.4）。 */
  trySendControlSlot: (commandId: string, session: string) => Promise<boolean>;
  /** 拉取重组完成后把全文写回投影——调用方决定"归到哪个会话的哪份 MilestoneProjection"，本类
   *  不持有 `AppRuntimeCore`。返回值供本类判断是否要额外提示"内容已过期"（当前实现不使用返回值，
   *  留给调用方自己接下一步）。 */
  applyFullText: (sessionId: string, messageId: number, revision: number, blocks: unknown[]) => void;
  /**
   * msgfix2 U4（body cache 写入挂钩点，设计稿 §4.2 读写序"fetch 成功 → 内存投影 → 异步写 cache
   * （失败静默）"）：重组完成、SHA-256 校验通过、`applyFullText()` 已经调用之后触发——内存投影
   * 优先落地，缓存写入是锦上添花，不阻塞呈现。**本类不 await 这个钩子、不关心它的结果**（调用方
   * `app/AppRuntime.tsx` 自己决定写哪个 body cache 实例、失败怎么静默降级）——省略时（生产装配点
   * 之外的调用方，如未来别的宿主）完全不影响重组/呈现逻辑，纯粹是可选的旁路通知。 */
  onFetchCached?: (input: {
    room: string;
    sessionId: string;
    messageId: number;
    revision: number;
    contentSha256: string;
    blocks: unknown[];
    bytes: Uint8Array;
  }) => void;
  now?: () => number;
  randomUUID?: () => string;
  onChange?: () => void;
  timeoutMs?: number;
}

interface InFlight {
  commandId: string;
  sessionId: string;
  messageId: number;
  revision: number;
  reassembler: MsgChunkReassembler;
  timeoutHandle: ReturnType<typeof setTimeout>;
}

export class MsgFetchClient {
  /** 每个 session 至多一条在飞——键 = sessionId（见文件头注"单飞行范围"）。 */
  private readonly bySession = new Map<string, InFlight>();
  /** command_id → sessionId，供 `handleReply` 按 command_id 反查该路由到哪条 `InFlight`。 */
  private readonly commandToSession = new Map<string, string>();
  private readonly states = new Map<number, MsgFetchState>();
  /**
   * `startFetch()` 第一行**同步**占位——`bySession` 直到 `seal()`（真异步、跨多个微任务）成功后
   * 才会写入，如果"是否已有在飞请求"这条判断只看 `bySession`，两次几乎同时的 `startFetch()` 调用
   * （生产场景真实会发生：`AppRuntime.tsx` 的自动拉取 `useEffect` 在没有依赖数组的情况下每次
   * 渲染后都会重新判断一次"要不要发起"，`seal()` 完成前的这段窗口里任何别的帧触发的 `forceRender`
   * 都会让 effect 重新跑一遍）都会在各自都还没写 `bySession` 的时候读到"没有在飞"，从而重复发出
   * 两条 `msg.fetch`（各占用一次 relay 字节预算/桌面单飞行占用，且两个 `command_id` 谁的
   * `msg.chunk` 先到会互相踩）。用一个在函数第一行就同步写入的 `Set` 堵死这条竞态——所有提前返回
   * 失败的分支都必须删除对应条目，成功路径上所有权转交给 `finish()` 在拉取终态时统一删除。
   */
  private readonly activeSessions = new Set<string>();

  constructor(private readonly deps: MsgFetchClientDeps) {}

  private now(): number {
    return this.deps.now?.() ?? Date.now();
  }

  private newCommandId(): string {
    return this.deps.randomUUID ? this.deps.randomUUID() : crypto.randomUUID();
  }

  private notify(): void {
    this.deps.onChange?.();
  }

  private setState(messageId: number, state: MsgFetchState): void {
    this.states.set(messageId, state);
    this.notify();
  }

  getState(messageId: number): MsgFetchState {
    return this.states.get(messageId) ?? IDLE_STATE;
  }

  /**
   * 发起一次全文拉取——`offset` 恒 0（本单不做跨重启续传,见文件头"内存态即可"取向）。已有同 session
   * 在飞的 fetch 时返回 `{ok:false, reason:"local_busy"}`，不排队、不打断已有请求。
   */
  async startFetch(sessionId: string, messageId: number, revision: number): Promise<{ ok: boolean; reason?: "local_busy" | "no_connection" }> {
    if (this.activeSessions.has(sessionId)) return { ok: false, reason: "local_busy" };
    this.activeSessions.add(sessionId); // 同步占位——见字段头注,必须是本函数第一个可能提前返回之前的最后一步。

    const epochAtStart = this.deps.getEpoch();
    const socketAtStart = this.deps.getSocket();
    if (epochAtStart === null || !socketAtStart || socketAtStart.readyState !== ReadyState.OPEN) {
      this.activeSessions.delete(sessionId);
      return { ok: false, reason: "no_connection" };
    }
    const commandId = this.newCommandId();
    const allowed = await this.deps.trySendControlSlot(commandId, sessionId);
    if (!allowed) {
      this.activeSessions.delete(sessionId);
      this.setState(messageId, { status: "error", errorReason: "busy" });
      return { ok: false, reason: "local_busy" };
    }

    const plaintext = buildMsgFetchRequest(sessionId, messageId, revision, 0);
    const meta = envelopeMeta({ v: 1, room: this.deps.room, epoch: epochAtStart, kind: "control", session: sessionId, commandId });
    let sealed: { ct: string; n: string };
    try {
      sealed = await seal(this.deps.kRoomKey, meta, utf8Bytes(JSON.stringify(plaintext)));
    } catch {
      this.activeSessions.delete(sessionId);
      this.setState(messageId, { status: "error", errorReason: "timeout" });
      return { ok: false, reason: "no_connection" };
    }
    // 复核：seal 期间 epoch/socket 可能已经变了（同 `AppRuntime.tsx::sendSnapshotRequest` 的既有
    // 竞态防护手法）——旧 epoch 封的密文 AAD 已经过时，不发；本单不做"epoch 变化自动重发"（不像
    // `control.snapshot` 那样有天然的"下一次 epoch.changed 触发时重发"钩子），epoch 变化期间发起
    // 的 fetch 直接判失败，用户可以再点一次"加载全文"。
    const latestEpoch = this.deps.getEpoch();
    const latestSocket = this.deps.getSocket();
    if (latestEpoch === null || latestEpoch !== epochAtStart || !latestSocket || latestSocket.readyState !== ReadyState.OPEN) {
      this.activeSessions.delete(sessionId);
      this.setState(messageId, { status: "error", errorReason: "timeout" });
      return { ok: false, reason: "no_connection" };
    }

    const timeoutHandle = setTimeout(() => this.handleTimeout(sessionId), this.deps.timeoutMs ?? MSG_FETCH_TIMEOUT_MS);
    this.bySession.set(sessionId, {
      commandId,
      sessionId,
      messageId,
      revision,
      reassembler: new MsgChunkReassembler(messageId),
      timeoutHandle,
    });
    this.commandToSession.set(commandId, sessionId);
    this.setState(messageId, { status: "loading", receivedBytes: 0 });

    try {
      latestSocket.send(
        JSON.stringify(
          buildCommandEnvelope({
            kind: "control",
            room: this.deps.room,
            epoch: latestEpoch,
            session: sessionId,
            commandId,
            ct: sealed.ct,
            n: sealed.n,
            now: () => this.now(),
          }),
        ),
      );
    } catch {
      this.finish(sessionId);
      this.setState(messageId, { status: "error", errorReason: "timeout" });
      return { ok: false, reason: "no_connection" };
    }
    return { ok: true };
  }

  /** 收到一条 `reply` kind 的 `msg.chunk`/`msg.fetch.error`——按外层信封 `command_id` 路由；
   *  路由不到任何在飞请求（未知/已终结的 command_id）时静默忽略（M0 §10.1："relay 不落库不广播,
   *  收到即按路由表转发"——客户端这层同样不对陌生 command_id 报错，只是它已经跟自己无关）。 */
  async handleReply(commandId: string, frame: MsgChunkFrame | MsgFetchErrorFrame): Promise<void> {
    const sessionId = this.commandToSession.get(commandId);
    if (sessionId === undefined) return;
    const inFlight = this.bySession.get(sessionId);
    if (inFlight === undefined || inFlight.commandId !== commandId) return;

    const outcome = frame.t === "msg.chunk" ? await inFlight.reassembler.ingestChunk(frame) : inFlight.reassembler.ingestError(frame);

    switch (outcome.status) {
      case "in_progress":
        // 滑动窗口——见 `MSG_FETCH_TIMEOUT_MS` 头注：收到这一片说明链路还活着，把超时计时器从
        // 这一刻重新起算，而不是继续沿着发起时设下的那个原始截止点走。
        this.resetTimeout(inFlight);
        this.setState(inFlight.messageId, { status: "loading", receivedBytes: outcome.receivedBytes, totalBytes: outcome.totalBytes });
        return;
      case "rejected":
        this.finish(sessionId);
        this.setState(inFlight.messageId, { status: "error", errorReason: "malformed_content" });
        return;
      case "error":
        this.finish(sessionId);
        this.setState(inFlight.messageId, { status: "error", errorReason: outcome.code, currentRef: outcome.currentRef });
        return;
      case "complete": {
        this.finish(sessionId);
        let blocks: unknown[] | null = null;
        try {
          const parsed: unknown = JSON.parse(new TextDecoder().decode(outcome.bytes));
          if (Array.isArray(parsed)) blocks = parsed;
        } catch {
          blocks = null;
        }
        if (blocks === null) {
          this.setState(inFlight.messageId, { status: "error", errorReason: "malformed_content" });
          return;
        }
        this.deps.applyFullText(sessionId, inFlight.messageId, outcome.contentRef.revision, blocks);
        // msgfix2 U4：内存投影已经落地——缓存写入挂钩点（可选，见 `MsgFetchClientDeps.onFetchCached`
        // 头注），不 await、不影响下面的状态收尾。
        this.deps.onFetchCached?.({
          room: this.deps.room,
          sessionId,
          messageId: inFlight.messageId,
          revision: outcome.contentRef.revision,
          contentSha256: outcome.contentRef.content_sha256,
          blocks,
          bytes: outcome.bytes,
        });
        this.setState(inFlight.messageId, { status: "idle" });
        return;
      }
    }
  }

  /** 滑动窗口的落点——清掉旧计时器、按 `deps.timeoutMs ?? MSG_FETCH_TIMEOUT_MS` 重新起一个,同一个
   *  到期回调（`handleTimeout`）。只在收到一片真正的数据（`in_progress`）时调用；`error`/
   *  `complete`/`rejected` 终态各自走 `finish()` 直接清计时器,不需要也不该再重置一次。 */
  private resetTimeout(inFlight: InFlight): void {
    clearTimeout(inFlight.timeoutHandle);
    inFlight.timeoutHandle = setTimeout(() => this.handleTimeout(inFlight.sessionId), this.deps.timeoutMs ?? MSG_FETCH_TIMEOUT_MS);
  }

  private handleTimeout(sessionId: string): void {
    const inFlight = this.bySession.get(sessionId);
    if (inFlight === undefined) return;
    this.finish(sessionId);
    this.setState(inFlight.messageId, { status: "error", errorReason: "timeout" });
  }

  private finish(sessionId: string): void {
    this.activeSessions.delete(sessionId);
    const inFlight = this.bySession.get(sessionId);
    if (inFlight === undefined) return;
    clearTimeout(inFlight.timeoutHandle);
    this.bySession.delete(sessionId);
    this.commandToSession.delete(inFlight.commandId);
  }
}
