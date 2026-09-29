// commandChannel.ts — T6f3 · 指令面发送层：input.send / input.answer / control.stop（M0 §3）。
//
// Authoritative reference (read-only):
//   - Command-plane contract (M0 §1 envelope/AAD, §3 command semantics): three plaintext command
//     shapes, ack mapping, busy-time enqueue, and the control.stop time window.
//   - Remote retry/probing semantics for outbound commands (G4 hard acceptance / G3 mitigation),
//     plus the multi-phone-in-one-room ack broadcast note.
//   - `app/src-tauri/src/remote_gateway.rs::handle_command_envelope` (desktop consumption side,
//     read-only reference — pins down each command's payload field names and `session` ownership).
//   - `remote-relay/src/room-do.js`: `handleInput`/`handleControl` (`stale_epoch` rejections carry
//     no `command_id`, so retries resend all in-flight commands as a batch) + `input.ack`/
//     `input.expired` broadcast to every online remote in the room (the source of the G3 gap).
//
// **本文件不含 React/WebSocket I/O 细节以外的浏览器依赖**——`CommandChannel` 是一个可在 vitest
// "logic"（node）project 下纯逻辑驱动的类：注入 `getEpoch`/`getSocket`/`ledger`/`now`/
// `randomUUID`/`scheduleTimer` 等依赖（同 `connection/connectionSession.ts` 的既有依赖注入手法），
// `commandChannel.test.ts` 用假实现覆盖 G4/G3/TTL 三条硬语义；`app/AppRuntime.tsx` 是唯一的生产
// 装配点（真 K_room CryptoKey + 真 socket 引用 + 真 IndexedDB 账本）。
//
// **"重发用新 command_id" 是靠调用约定自然成立的，不是靠一段专门代码实现**：`sendInput`/
// `answerCard`/`stopSession` 每次调用都 `newCommandId()` 现铸一个全新 UUID——UI 层的"重试"按钮
// 就是再调一次同一个方法（`Composer`/`DecisionCard` 的 onChoose 重新点一次），天然拿到新 id；
// 反过来，`handleStaleEpoch()` 的"同 command_id 重封重发"是唯一需要专门代码复用旧 id 的地方
// （见该方法注释），这条不对称正是 M0 §3 与 §4d 两处措辞差异的根源："failed/expired 终态重试用
// 新 ID"vs"stale_epoch 用同一个未完成请求的 ID 重试"——本质是"这条指令到底有没有指令结果"的区别：
// stale_epoch 时 relay 压根没受理这条指令（不存在"重复受理"的风险），failed/expired 时 relay/
// 桌面已经对这个 command_id 有了终态记录，复用旧 id 只会拿到台账里那条稳定终态、绝不会二次投递
// （M0 §3 原文）。

import { seal } from "../crypto/envelope.ts";
import { utf8Bytes } from "../crypto/bytes.ts";
import { ReadyState, type WebSocketLike } from "../connection/types.ts";
import { envelopeMeta, buildCommandEnvelope, type CommandEnvelopeKind } from "./wireEnvelope.ts";
import { normalizeInputAckOutcome, type AckDisplayOutcome } from "../events/ackOutcome.ts";
import { deriveMsgCompletedClientMsgId } from "../events/clientMsgId.ts";
import type { CommandLedgerPort } from "../store/commandLedger.ts";
import { isPersistedCompleted } from "./sendBadge.ts";

export type CommandKind = "input.send" | "input.answer" | "control.stop";

export type CommandStatus =
  | "sending" // seal() 进行中，尚未真正 send() 到 socket。
  | "sent" // 已经 send() 出去，等待 ack/expired。
  /** C1-RQ（dogfood 修障第二批）：relay `input.relay_queued`——消息已安全落在 relay 的
   *  `pending_input` 表里（桌面离线），但真正投递结果仍要等桌面回来后的真 `input.ack`/
   *  `input.expired`。非终态；`isInFlight()` 不把它算在内——relay 已经确认收下，不需要因为
   *  epoch 变化/desktop_offline 广播就被自动重发（重发只会拿到同一份 relay_queued 确认，白费一趟
   *  网络往返）,但 handleAck/handleExpired/handleRateLimited 仍能把它推进到真正的终态。 */
  | "relay_queued"
  /** C1（dogfood 修障第二批）：`status:"sent"` 满 `ACK_WATCHDOG_MS` 仍未见 ack/relay_queued——
   *  投递结果未知，UI 展示"可重试"（同 expired/rate_limited/give_up 一族，重试用新 command_id）。
   *  见 `armAckWatchdog()`。 */
  | "delivering_uncertain"
  | "acked" // 收到 input.ack，或确定性 msg.completed 回执（`ackOutcome` 记录 UI 有效终局）。
  | "expired" // 收到 input.expired（仅 kind=input 可能出现——relay 30 分钟离线暂存到期）。
  | "rate_limited" // 返工②：relay 限速拒绝，或本机滑动窗预判超限、从未真正发出。
  | "give_up"; // stale_epoch 重试次数耗尽，本机不再自动重发（用户可另行发起一次新指令）。

export interface CommandRecord {
  commandId: string;
  kind: CommandKind;
  session: string;
  /** 仅 `kind:"input.answer"` 有值——`getAnswerOverride()` 按它索引。 */
  decisionId?: string;
  /** 仅 `kind:"input.send"` 有值——UI 展示"发的是什么"用，不参与协议判定。 */
  text?: string;
  /** 仅 `kind:"input.answer"` 有值——本机真正点的那个选项，见 `store/commandLedger.ts::
   *  CommandLedgerRecord.option` 注释（不许失败重试退回 `options[0]`）。 */
  option?: string;
  /** 密文体的明文——`handleStaleEpoch()` 用新 epoch 重新 `seal()` 时原样复用（同一份业务内容，
   *  不是重新构造一份新的）。 */
  plaintext: Record<string, unknown>;
  createdAt: number;
  /** stale_epoch 已经触发过几次重发——见 `handleStaleEpoch()`。 */
  attempts: number;
  /** 已经排了一次重试定时器、尚未真正触发——防止同一批 stale_epoch 拒绝帧（relay 对每条被拒的
   *  原始帧各回一条，见文件头注）把同一条记录重复排队。 */
  retryScheduled: boolean;
  /** 返工①：`retryScheduled` 为真时，这是 `scheduleTimer`/`setTimeout` 返回的句柄——命令进终态
   *  （ack/expired/rate_limited）时用它主动取消挂着的重试定时器（`cancelPendingRetry()`），不必
   *  等定时器空转触发后再被 `sealAndSend()` 的状态二次校验拦下。`retryScheduled=false` 时恒为
   *  `undefined`。 */
  retryTimerHandle?: unknown;
  /** C1：`status` 首次翻到 `"sent"` 时排的 ack 看门狗句柄（`armAckWatchdog()`）——命令进任何终态
   *  （或收到 `relay_queued`）时用它取消（`cancelAckWatchdog()`），同 `retryTimerHandle` 的既有
   *  纪律，只是管的是另一条独立定时器（两者可能同时挂着——G4 重试与 C1 看门狗是互不相关的两件
   *  事，不能合用一个句柄字段）。 */
  ackWatchdogHandle?: unknown;
  status: CommandStatus;
  ackOutcome?: AckDisplayOutcome;
  ackReason?: string; // Only set for outcome=failed when the desktop supplies a reason.
  ackRecognized?: boolean;
  /** 已观察到与本命令确定性匹配的 msg.completed；这是比首发 queued ack 更强的投递事实。 */
  completedReceipt?: boolean;
  /** C1-RQ：仅 `status==="relay_queued"` 有值——relay 这条暂存行的 TTL 到期时刻（毫秒 epoch），
   *  供 UI 展示。 */
  relayQueuedExpiresAt?: number;
}

export interface CommandChannelDeps {
  room: string;
  kRoomKey: CryptoKey;
  /** 读"此刻认为最新"的 epoch——`AppRuntime.tsx` 传入的是与 `control.snapshot` 请求共享的同一个
   *  `currentEpochRef`，语义一致："发送前一律复核当前值，不用调用发起时刻捕获的旧值"。 */
  getEpoch: () => number | null;
  getSocket: () => WebSocketLike | null;
  /** G3：发送前必须先持久化——见 `store/commandLedger.ts` 头注。 */
  ledger: CommandLedgerPort;
  now?: () => number;
  randomUUID?: () => string;
  scheduleTimer?: (callback: () => void, delayMs: number) => unknown;
  clearTimer?: (handle: unknown) => void;
  /** 任何会改变可观察状态（状态机 status/ackOutcome）的操作之后调用一次——`AppRuntime.tsx` 传
   *  `forceRender`；测试可以传 `() => {}` 或断言用的探针。 */
  onChange?: () => void;
  /** stale_epoch 重试上限（默认 3 次）——超过后转 `give_up`，不再自动重发，避免恶意/异常 relay
   *  反复回 stale_epoch 时无限重试打爆 G8 限速桶（`remote-relay/src/room-do.js` 每 subject 每
   *  channel 每分钟 30 帧的硬闸——见文件头注）。 */
  maxStaleEpochRetries?: number;
  staleEpochRetryBaseMs?: number;
  staleEpochRetryCapMs?: number;
  /** 返工③（G8 限速消费）：本地分通道滑动窗——input（覆盖 `input.send`+`input.answer`，两者都走
   *  `kind:"input"` 信封，对齐 relay `room-do.js::handleInput` 的 per-subject 桶口径）/control
   *  （`control.stop`，对齐 `handleControl`）各自一条。数值默认 25/60s——relay 硬闸是
   *  `INPUT_RATE_LIMIT=30`/`CONTROL_RATE_LIMIT=30` 每 60s（`room-do.js` 顶部常量），本地留 5 帧
   *  余量：本地窗口只是"劝退明显会被拒的发送"，不是安全边界（真正的闸在 relay），留余量是为了不
   *  在临界值附近因为本地/远端计时窗口对不齐而误伤正常用户。 */
  localInputRateLimit?: number;
  localControlRateLimit?: number;
  localRateWindowMs?: number;
}

const DEFAULT_MAX_STALE_EPOCH_RETRIES = 3;
const DEFAULT_RETRY_BASE_MS = 300;
const DEFAULT_RETRY_CAP_MS = 3_000;
/** control.stop 的固定生命周期——M0 §3："远端生成时 expires_at_ms = issued_at_ms + 30_000"。 */
const CONTROL_STOP_LIFETIME_MS = 30_000;
/** 见 `CommandChannelDeps.localInputRateLimit` 注释——relay 硬闸 30/60s 的本地留余量版本。 */
const DEFAULT_LOCAL_RATE_LIMIT = 25;
const DEFAULT_LOCAL_RATE_WINDOW_MS = 60_000;
/** C1（dogfood 修障第二批）：`status:"sent"` 的 ack 看门狗超时——与 `ui/composer/Composer.tsx`
 *  展示"发送中"文案升级为"投递中,桌面可能离线"的那个 30 秒阈值共用同一个数字来源（该文件从这里
 *  导入，不再自己另开一份字面量 `30_000`）。见 `armAckWatchdog()`。 */
export const ACK_WATCHDOG_MS = 30_000;

function envelopeKindFor(kind: CommandKind): CommandEnvelopeKind {
  return kind === "control.stop" ? "control" : "input";
}

export class CommandChannel {
  private readonly records = new Map<string, CommandRecord>();

  constructor(private readonly deps: CommandChannelDeps) {}

  private now(): number {
    return this.deps.now?.() ?? Date.now();
  }

  private newCommandId(): string {
    return this.deps.randomUUID ? this.deps.randomUUID() : crypto.randomUUID();
  }

  private notify(): void {
    this.deps.onChange?.();
  }

  // ---------------------------------------------------------------------------
  // 三条发送入口——M0 §3 明文形状逐字段照抄 handle_command_envelope 的消费面。
  // ---------------------------------------------------------------------------

  /** `{t:"input.send", session, text}`（kind=input）。返回新铸的 `command_id`。 */
  async sendInput(session: string, text: string): Promise<string> {
    const commandId = this.newCommandId();
    await this.dispatch({
      commandId,
      kind: "input.send",
      session,
      text,
      plaintext: { t: "input.send", session, text },
    });
    return commandId;
  }

  /** `{t:"input.answer", session, decision_id, option}`（kind=input）。返回新铸的 `command_id`。 */
  async answerCard(session: string, decisionId: string, option: string): Promise<string> {
    const commandId = this.newCommandId();
    await this.dispatch({
      commandId,
      kind: "input.answer",
      session,
      decisionId,
      option,
      plaintext: { t: "input.answer", session, decision_id: decisionId, option },
    });
    return commandId;
  }

  /** `{t:"control.stop", session, issued_at_ms, expires_at_ms}`（kind=control）。返回新铸的
   *  `command_id`——`expires_at_ms = issued_at_ms + 30_000`（M0 §3 远端生成侧定值,与桌面
   *  `CONTROL_STOP_MAX_LIFETIME_MS` 硬上限相等,不是任取一个 <= 30s 的值)。 */
  async stopSession(session: string): Promise<string> {
    const commandId = this.newCommandId();
    const issuedAtMs = this.now();
    await this.dispatch({
      commandId,
      kind: "control.stop",
      session,
      plaintext: {
        t: "control.stop",
        session,
        issued_at_ms: issuedAtMs,
        expires_at_ms: issuedAtMs + CONTROL_STOP_LIFETIME_MS,
      },
    });
    return commandId;
  }

  private async dispatch(input: {
    commandId: string;
    kind: CommandKind;
    session: string;
    decisionId?: string;
    text?: string;
    option?: string;
    plaintext: Record<string, unknown>;
  }): Promise<void> {
    const record: CommandRecord = {
      commandId: input.commandId,
      kind: input.kind,
      session: input.session,
      decisionId: input.decisionId,
      text: input.text,
      option: input.option,
      plaintext: input.plaintext,
      createdAt: this.now(),
      attempts: 0,
      retryScheduled: false,
      status: "sending",
    };
    this.records.set(record.commandId, record);

    // 返工③（G8 限速消费）：本地滑动窗预判——超窗就"稍候重试"，不真的发出去挨 relay 拒（省一趟
    // 网络往返，也不占 relay per-subject 桶里本已紧张的名额）。从未真正发出，不写账本（`ledger`
    // 是"这个 command_id 是不是我发的"membership 判据，relay 从没见过这个 id，未来也不可能有任何
    // ack/expired/rate_limited 帧引用它，记账没有意义，只会留一条永远等不到回应的死行）。
    if (!this.takeLocalRateSlot(envelopeKindFor(record.kind))) {
      record.status = "rate_limited";
      this.notify();
      return;
    }
    this.notify();

    // G3 硬语义：先持久化再发送（`store/commandLedger.ts` 头注）——落账失败就不发送，宁可让用户
    // 看到"发送失败"重试，也不能让一条从未记账的 command_id 飞出去（万一它的 ack 回来时，账本
    // 查不到它，会被 G3 过滤误判成"别人的"而永远静默丢弃，用户对着一个查无此单的失败指令干等）。
    try {
      await this.deps.ledger.recordSent({
        commandId: record.commandId,
        kind: record.kind,
        session: record.session,
        createdAt: record.createdAt,
        decisionId: record.decisionId,
        option: record.option,
      });
    } catch {
      record.status = "give_up";
      this.notify();
      return;
    }
    await this.sealAndSend(record);
  }

  // ---------------------------------------------------------------------------
  // 返工③：本地分通道滑动窗（G8 消费半——见 CommandChannelDeps.localInputRateLimit 注释）。
  // ---------------------------------------------------------------------------

  private readonly sendTimestampsByChannel: Record<CommandEnvelopeKind, number[]> = { input: [], control: [] };

  /** 占一个本地发送名额——`channel` 对齐信封 `kind`（`"input"` 覆盖 input.send+input.answer 两种
   *  命令，同 relay `handleInput` 的 per-subject 桶口径；`"control"` 对齐 `handleControl`）。窗口
   *  内已达上限返回 `false`（不占名额）；否则记一次时间戳、返回 `true`。 */
  private takeLocalRateSlot(channel: CommandEnvelopeKind): boolean {
    const limit = channel === "input" ? (this.deps.localInputRateLimit ?? DEFAULT_LOCAL_RATE_LIMIT) : (this.deps.localControlRateLimit ?? DEFAULT_LOCAL_RATE_LIMIT);
    const windowMs = this.deps.localRateWindowMs ?? DEFAULT_LOCAL_RATE_WINDOW_MS;
    const now = this.now();
    const bucket = this.sendTimestampsByChannel[channel];
    while (bucket.length > 0 && now - bucket[0]! >= windowMs) {
      bucket.shift();
    }
    if (bucket.length >= limit) return false;
    bucket.push(now);
    return true;
  }

  /**
   * FIX2 P2-5（snapshot 请求并入命令通道）：`control.snapshot` 请求不是 `sendInput`/`answerCard`/
   * `stopSession` 那三条正式指令入口——它没有 UI 可见状态、不走 `give_up`/`attempts` 那套状态机，
   * resend 仍由 `AppRuntime.tsx` 自己的 `pendingSnapshotRequestRef` 机制驱动（`sendSnapshotRequest`/
   * `handleEpochChanged`/`handleReplayHead`/stale_epoch 分支头注）。但它与 `control.stop` 共享
   * relay 同一个 per-subject `"control"` 信封桶（`room-do.js::handleControl` 对 `kind:"control"`
   * 的两种命令一视同仁），必须占用同一条本地滑窗——不能各转各的、让二者合计悄悄超过本地"留余量"
   * 设计的初衷。`control_rate_limited` 拒绝帧带的 `command_id` 要能被 `handleRateLimited()` 的
   * `ledger.isOwn()` 认领（否则那条拒绝帧会因为查无此单被当"别人的"静默忽略——snapshot 请求就
   * 悬死了：AppRuntime 以为还在等回应，其实 relay 已经拒了），所以这里也代 AppRuntime 记一笔账。
   *
   * 返回 `false` = 本地窗口已满，调用方不该真的发送这次尝试（不占用一次网络往返，也不占 relay
   * per-subject 桶里紧张的名额——同 `dispatch()` 既有的预判取舍）；调用方保留
   * `pendingSnapshotRequestRef` 不清，等下一次自然触发（epoch 变化/重选会话）再试。
   */
  async trySendControlSlot(
    commandId: string,
    session: string,
    // `msg.fetch` (M0 §10.4) shares the existing `control` channel with `control.snapshot`/
    // `control.history` — same local sliding window and bookkeeping (relay's
    // `room-do.js::handleControl` treats all three as one per-subject bucket by design).
    // `CommandLedgerPort.kind` is already a plain `string` in `store/commandLedger.ts`; the
    // narrower literal here is just a caller-facing hint, no ledger shape change needed.
    kind: "control.snapshot" | "control.history" | "msg.fetch" = "control.snapshot",
  ): Promise<boolean> {
    if (!this.takeLocalRateSlot("control")) return false;
    try {
      await this.deps.ledger.recordSent({ commandId, kind, session, createdAt: this.now() });
    } catch {
      // 记账失败不阻止发送——snapshot 请求没有 G3"先持久化再发"那条硬语义（那是给 composer 指令
      // 保证 ack/expired 广播能正确按账本过滤设的；snapshot 应答走 event 通道自己的
      // `client_msg_id` 去重，不依赖这本账）；记账失败只影响"日后一条 control_rate_limited 错误帧
      // 能不能认领到这个 commandId"这一件事，不影响请求本身能不能发出去。
    }
    return true;
  }

  /** 命令是否仍处于"可以（重新）发送"的非终态——`sealAndSend()` 在 seal 前后各查一次这条（返工①）。
   *  C1-RQ：`"relay_queued"` 故意不算在内——relay 已经确认收下这条消息，`handleStaleEpoch()`/
   *  `handleDesktopOffline()` 不该因为 epoch 变化/别的指令撞上 desktop_offline 就把它当"还没发出"
   *  重新走一遍发送流程（那只会拿到同一份 relay_queued 确认，白费一趟网络往返）。C1 同理：
   *  `"delivering_uncertain"` 也不算——那是"该由用户手动决定要不要重试"的可重试终态,不是自动
   *  重发的候选（同 expired/rate_limited/give_up 一族）。 */
  private isInFlight(record: CommandRecord): boolean {
    return record.status === "sending" || record.status === "sent";
  }

  private async sealAndSend(record: CommandRecord): Promise<void> {
    // 返工①校验点 A（seal 前）：命令可能在这次调用发起之前就已经进了终态——`handleStaleEpoch()`
    // 排的重试定时器触发时最典型：排定之后、真正 fire 之前，ack/expired/rate_limited 已经到达。
    // 已进终态则零发送，不浪费一次 seal()，也不会把终态状态"改回" sending/sent。
    if (!this.isInFlight(record)) return;
    const epochAtStart = this.deps.getEpoch();
    const socketAtStart = this.deps.getSocket();
    if (epochAtStart === null || !socketAtStart || socketAtStart.readyState !== ReadyState.OPEN) {
      // 没有可用连接——保持当前 status（首次发送时是 "sending"），不是错误,只是此刻发不出去。
      // 后续连接恢复不会自动补发（本单不做"离线补发在飞指令"这层,与 §4a`control.snapshot`
      // 请求的既有取舍一致——那条路径同样只在有 socket 时才发,离线时静默不发,不在这里另开）。
      return;
    }
    const meta = envelopeMeta({
      v: 1,
      room: this.deps.room,
      epoch: epochAtStart,
      kind: envelopeKindFor(record.kind),
      session: record.session,
      commandId: record.commandId,
    });
    let sealed: { ct: string; n: string };
    try {
      sealed = await seal(this.deps.kRoomKey, meta, utf8Bytes(JSON.stringify(record.plaintext)));
    } catch {
      return;
    }
    // 返工①校验点 B（seal 后）：seal() 是真异步操作——密封期间终态可能刚好到达（同一条 race：
    // ack/expired/rate_limited 在 await 期间落地）。连同既有的 epoch/socket 复核一起，用最新状态
    // 判定要不要真的发出去，过时的密封结果直接放弃。
    if (!this.isInFlight(record)) return;
    const latestEpoch = this.deps.getEpoch();
    const latestSocket = this.deps.getSocket();
    if (latestEpoch === null || latestEpoch !== epochAtStart || !latestSocket || latestSocket.readyState !== ReadyState.OPEN) {
      return;
    }
    const outbound = buildCommandEnvelope({
      kind: envelopeKindFor(record.kind),
      room: this.deps.room,
      epoch: latestEpoch,
      session: record.session,
      commandId: record.commandId,
      ct: sealed.ct,
      n: sealed.n,
      now: () => this.now(),
    });
    latestSocket.send(JSON.stringify(outbound));
    const current = this.records.get(record.commandId);
    if (current) {
      if (current.status === "sending") {
        current.status = "sent";
        this.notify();
      }
      // C1（dogfood 修障第二批）：每次真正把字节发上线都重开一段看门狗——不只是首发那一次
      // （"sending→sent"）；G4 stale_epoch 重试到这里时 status 已经是 "sent"（不进上面那个 if），
      // 但确实又发送了一次，"30 秒内没消息"这条不变量该从这次最新发送重新计时,不是钉死在最早那
      // 次尝试。先取消旧的（如果还挂着）再武装新的，避免同一条记录并行挂两个看门狗定时器互相打架
      // ——校验点 B（上面 `if (!this.isInFlight(record)) return;`）已经保证走到这里时 `current`
      // 恒是 "sending"/"sent" 之一，不会误伤别的状态。
      this.cancelAckWatchdog(current);
      this.armAckWatchdog(current);
    }
  }

  // ---------------------------------------------------------------------------
  // G4 硬语义：stale_epoch 拒绝 → 取最新 epoch、同 command_id 重封重发（有限次 + 退避）。
  // ---------------------------------------------------------------------------

  /**
   * 三处触发（返工①第②点起，`AppRuntime.tsx` 三个调用点）：① relay 的 `{t:"error",
   * reason:"stale_epoch", currentEpoch}` 拒绝帧——没有 `command_id`（`room-do.js` 的
   * `handleInput`/`handleControl` 拒绝分支原样——见文件头注），没法精确关联"是哪条指令过时了"；
   * ② `{t:"epoch.changed", epoch, ts}` 广播（桌面重连、relay 主动通知在线远端）；③ 重连恢复时的
   * `replay.head`（`handleReplayHead` 回调——旧连接上"已发出但还没等到 ack"的指令，旧 socket 很
   * 可能压根没把它们真正投递到 relay，重连后按新 epoch 补发一次）。三者统一走同一套保守策略：把
   * 所有仍在等待结果的指令（`status` 为 `"sending"`/`"sent"`）全部重试一遍——对没有真正过时/未真正
   * 丢失的指令而言,这次重发是无害的（`command_id` 不变,桌面侧 `remote_inbox` 表
   * `UNIQUE(command_id)` 幂等——见 M0 §3"忙时入队"段）,不会造成二次投递；方法名保留
   * `handleStaleEpoch`（历史上第一个触发源），不是三个源各写一份的意思——与 `sendSnapshotRequest`/
   * `pendingSnapshotRequestRef` 那条既有的 snapshot 重发路径待遇对齐（"与 snapshot 同待遇"）。
   *
   * 调用方（`AppRuntime.tsx`）负责在调用本方法前先把 `getEpoch()` 会读到的引用更新成最新 epoch
   * ——本方法自己不碰那个引用（依赖注入的 `getEpoch` 只读,不该由发送层反向写调用方的状态）,
   * `sealAndSend()` 重新调用时会读到新值。
   */
  handleStaleEpoch(): void {
    const maxRetries = this.deps.maxStaleEpochRetries ?? DEFAULT_MAX_STALE_EPOCH_RETRIES;
    for (const record of this.records.values()) {
      if (!this.isInFlight(record)) continue;
      if (record.retryScheduled) continue; // 同一批 stale_epoch 帧不重复排队（见字段注释）。
      if (record.attempts >= maxRetries) {
        this.cancelAckWatchdog(record); // C1：give_up 是终态——不留一条空转到 30s 后才自己 no-op 的看门狗。
        record.status = "give_up";
        this.notify();
        continue;
      }
      record.retryScheduled = true;
      const attemptIndex = record.attempts;
      record.attempts += 1;
      this.notify();
      const base = this.deps.staleEpochRetryBaseMs ?? DEFAULT_RETRY_BASE_MS;
      const cap = this.deps.staleEpochRetryCapMs ?? DEFAULT_RETRY_CAP_MS;
      const delayMs = Math.min(cap, base * 2 ** attemptIndex);
      const fire = () => {
        record.retryScheduled = false;
        record.retryTimerHandle = undefined;
        void this.sealAndSend(record);
      };
      record.retryTimerHandle = this.deps.scheduleTimer ? this.deps.scheduleTimer(fire, delayMs) : setTimeout(fire, delayMs);
    }
  }

  /**
   * 返工①第②点："命令进终态时取消其挂着的重封定时器"——`handleAck`/`handleExpired`/
   * `handleRateLimited` 把记录判进终态前调用。`sealAndSend()` 的 seal 前/后二次状态校验（校验点
   * A/B）已经能保证"零发送"这条正确性不变量，本方法是在那之上再做的资源清理：定时器真取消掉，
   * 不必等它自己空转触发一次（省一次无意义的 `seal()` 调用），且清掉 `retryScheduled`/
   * `retryTimerHandle`，保持这两个字段"是否有一条挂着的重试"的语义准确。
   */
  private cancelPendingRetry(record: CommandRecord): void {
    if (record.retryTimerHandle !== undefined) {
      if (this.deps.clearTimer) {
        this.deps.clearTimer(record.retryTimerHandle);
      } else {
        clearTimeout(record.retryTimerHandle as ReturnType<typeof setTimeout>);
      }
      record.retryTimerHandle = undefined;
    }
    record.retryScheduled = false;
  }

  // ---------------------------------------------------------------------------
  // C1（dogfood 修障第二批）：`status:"sent"` 的 ack 看门狗——`sealAndSend()` 首次转 `"sent"` 时
  // 武装（见该方法调用点），`ACK_WATCHDOG_MS` 后仍未见 ack/relay_queued 就翻 `"delivering_uncertain"`
  // （可重试，不是自动重发）。命令进任何终态或收到 `relay_queued` 时必须取消——同
  // `cancelPendingRetry()` 的既有纪律，只是管另一条独立定时器。
  // ---------------------------------------------------------------------------

  private armAckWatchdog(record: CommandRecord): void {
    const fire = () => {
      record.ackWatchdogHandle = undefined;
      // 校验点（同 sealAndSend 的既有纪律）：定时器真正触发时状态可能已经在等待期间被别的分支
      // 推进（正常情况下应该已经被 cancelAckWatchdog() 提前拦下——这里是独立的第二道防线,不是
      // 唯一防线）。只有仍稳稳停在 "sent" 才翻看门狗态,不倒退任何已经拿到的更强判定。
      if (record.status !== "sent") return;
      record.status = "delivering_uncertain";
      this.notify();
    };
    record.ackWatchdogHandle = this.deps.scheduleTimer
      ? this.deps.scheduleTimer(fire, ACK_WATCHDOG_MS)
      : setTimeout(fire, ACK_WATCHDOG_MS);
  }

  private cancelAckWatchdog(record: CommandRecord): void {
    if (record.ackWatchdogHandle !== undefined) {
      if (this.deps.clearTimer) {
        this.deps.clearTimer(record.ackWatchdogHandle);
      } else {
        clearTimeout(record.ackWatchdogHandle as ReturnType<typeof setTimeout>);
      }
      record.ackWatchdogHandle = undefined;
    }
  }

  /**
   * R1（返工·断线看门狗吃掉 G4 重连补发）：`ACK_WATCHDOG_MS` 度量的是"已经发上线、桌面理应能收到
   * 却迟迟没有回音"这段异常窗口——连接本就不 open 的这段时间里收不到 ack 是必然,不是异常,不该被
   * 计入。调用方（`AppRuntime.tsx` 的 `onPhaseChange`）在任何非 open 相位立即调用本方法,把当前所有
   * 在飞记录（`"sending"`/`"sent"`）挂着的看门狗定时器原样取消——**不改变 `status`**,不产生任何
   * 判定,纯粹是"暂停计时"。这样断线期间 `status` 会稳稳停在 `"sent"`,不会被看门狗提前判成
   * `"delivering_uncertain"`；重连后 `handleStaleEpoch()`（`AppRuntime.tsx::handleReplayHead` 既有
   * 调用点）能照常把这些仍是 `"sending"`/`"sent"` 的记录用同一个 `command_id` 重新走一遍
   * `sealAndSend()`——真正把字节发出去时,该方法已有的"每次真正发送都重开一段全新看门狗"逻辑（见
   * `armAckWatchdog()` 调用点注释）自然重新武装,不需要额外的"暂停/恢复剩余时长"这套更复杂的机制。
   */
  cancelAckWatchdogsForDisconnect(): void {
    for (const record of this.records.values()) {
      if (!this.isInFlight(record)) continue;
      this.cancelAckWatchdog(record);
    }
  }

  // ---------------------------------------------------------------------------
  // G3 缓解：input.ack / input.expired / rate_limited 按持久化账本过滤——不是本机发的一律忽略。
  // ---------------------------------------------------------------------------

  /** `{t:"input.ack", command_id, outcome}`（明文帧,relay 广播给全房间在线远端——文件头注）。 */
  async handleAck(commandId: string, outcome: string, reason?: string): Promise<void> {
    const owned = await this.deps.ledger.get(commandId);
    if (!owned) return; // 别的手机发出的指令的 ack 广播——按账本过滤,不打扰本机 UI（v0.4 记档 ③）。
    const record = this.records.get(commandId);
    // msg.completed 是确定性投递回执；它先到时，迟到的首发 queued ack 不能把终局倒退回排队态。
    if (record?.completedReceipt) return;
    // 重载后内存 record 会清空，但持久账本仍保留最后终局；不得被迟到 ack 覆盖。
    if (isPersistedCompleted(owned.status)) return;
    const normalized = normalizeInputAckOutcome(outcome);
    await this.deps.ledger.updateStatus(commandId, normalized.effective);
    if (record) {
      this.cancelPendingRetry(record);
      this.cancelAckWatchdog(record);
      record.status = "acked";
      record.ackOutcome = normalized.effective;
      record.ackReason = reason;
      record.ackRecognized = normalized.recognized;
      this.notify();
    }
  }

  /**
   * 桌面为远程注入消息发布 `msg.completed` 时，把 `remote_input:<command_id>` 纳入 UUIDv5 name。
   * 手机端对本会话仍在等待或显示 queued 的 input.send 逐条重算；只在 client_msg_id 精确相等时
   * 接受为更强的“已投递”事实。failed/expired/rate_limited 等既有终态不参与，避免回执改写失败语义。
   */
  async handleMsgCompleted(session: string, clientMsgId: string): Promise<void> {
    for (const record of this.records.values()) {
      if (record.kind !== "input.send" || record.session !== session) continue;
      const eligible =
        record.status === "sending" ||
        record.status === "sent" ||
        (record.status === "acked" && (record.ackOutcome === "queued" || record.ackOutcome === "taken_over"));
      if (!eligible) continue;
      if (deriveMsgCompletedClientMsgId(session, record.commandId) !== clientMsgId) continue;

      try {
        await this.deps.ledger.updateStatus(record.commandId, "ok");
      } catch {
        // 入站 msg.completed 已由 EventStore 持久化且会在冷启动重放；出站账本只是展示缓存，更新
        // 失败不能推翻已经通过确定性 id 证明的投递事实，也不能让徽标继续悬挂。
      }
      this.cancelPendingRetry(record);
      this.cancelAckWatchdog(record);
      record.status = "acked";
      record.ackOutcome = "ok";
      record.ackRecognized = true;
      record.completedReceipt = true;
      this.notify();
      return;
    }
  }

  /**
   * `{t:"input.expired", command_id}`——仅 relay 的 `pending_input` 30 分钟离线暂存到期清扫会产生
   * 这个帧，而 `pending_input` 只暂存 `kind=input` 的两种命令（`input.send`/`input.answer`——M0
   * §3："kind=input（FIFO·排队）...relay 仅在桌面离线时暂存"）；`control.stop` 走 `kind=control`，
   * `handleControl` 即刻投递、不暂存、离线直接回 `desktop_offline`，**协议上不存在** `control.stop`
   * 的 command_id 收到 `input.expired` 这回事。
   *
   * 返工②第①点：改用 `ledger.get()`（而不是只查 `isOwn()`）取出记录连同 `kind`——本机的
   * `control.stop` id 若收到一条（伪造/协议误用/未来 bug）`input.expired`，按族拒绝，不接受这个
   * 状态转换（**不**转 `"expired"`，`CommandRecord.status` 保持原样）。
   */
  async handleExpired(commandId: string): Promise<void> {
    const owned = await this.deps.ledger.get(commandId);
    if (!owned) return; // 不是本机发的——按账本过滤（G3）。
    if (owned.kind !== "input.send" && owned.kind !== "input.answer") return; // 命令族不对，拒绝。
    if (isPersistedCompleted(owned.status)) return;
    const record = this.records.get(commandId);
    if (record?.completedReceipt) return; // 确定性已投递终局不可被迟到的过期帧倒退。
    await this.deps.ledger.updateStatus(commandId, "expired");
    if (record) {
      this.cancelPendingRetry(record);
      this.cancelAckWatchdog(record);
      record.status = "expired";
      this.notify();
    }
  }

  /**
   * C1-RQ（dogfood 修障第二批·手机发消息桌面离线无反馈）：`{t:"input.relay_queued", command_id,
   * expires_at}`——relay `handleInput` 在桌面离线成功暂存/幂等命中时定向回给发送方（只有触发这次
   * input 的那个 remote socket 会收到，不是广播，不需要像 ack/expired 那样担心"别的手机的"，但
   * 仍按同一套账本过滤纪律核验一次，纵深防御不搞例外）。非终态——记录翻到 `"relay_queued"` 后仍
   * 继续等真正的 `input.ack`/`input.expired`。
   *
   * 只接受从 `"sending"`/`"sent"`/`"delivering_uncertain"`/`"relay_queued"` 这几个"还没有更强判定"
   * 的状态转入——`relay_queued` 只是"消息安全躺在 relay 队列里"这一较弱的中间事实，不该倒退已经
   * 进了 `acked`/`expired`/`rate_limited`/`give_up` 的更强终态判定（正常时序下 relay 总在任何真
   * ack/expired 之前就先回这条帧，理论上不会真的撞上这个倒退场景，这里是纵深防御）。
   */
  async handleRelayQueued(commandId: string, expiresAt: number): Promise<void> {
    const owned = await this.deps.ledger.get(commandId);
    if (!owned) return;
    if (owned.kind !== "input.send" && owned.kind !== "input.answer") return; // 命令族不对（同 handleExpired）。
    if (isPersistedCompleted(owned.status)) return;
    const record = this.records.get(commandId);
    if (record?.completedReceipt) return; // 确定性已投递终局不可被这条较弱的中间事实倒退。
    if (
      record &&
      record.status !== "sending" &&
      record.status !== "sent" &&
      record.status !== "delivering_uncertain" &&
      record.status !== "relay_queued"
    ) {
      return; // 已经进了别的终态——不倒退。
    }
    await this.deps.ledger.updateStatus(commandId, "relay_queued");
    if (record) {
      this.cancelPendingRetry(record); // relay 已确认收下,没必要再重试发送。
      this.cancelAckWatchdog(record);
      record.status = "relay_queued";
      record.relayQueuedExpiresAt = expiresAt;
      this.notify();
    }
  }

  /**
   * 返工③第②点：`{t:"error", reason:"input_rate_limited"|"control_rate_limited", command_id,
   * frame}`（`remote-relay/src/room-do.js::takeSubjectChannelRateSlot` 只回给触发限速的那个 socket
   * 自己，不广播全房间——但仍按账本核验一次，纵深防御，与 ack/expired 同一套过滤纪律不搞例外）。
   * 转可重试终态：composer 从 sending 翻成"被限速"，提示稍候重试，重试用新 command_id（同
   * failed/expired 语义，不复用旧 id——旧 id 在 relay 眼里可能还占着这一窗口的名额）。
   */
  async handleRateLimited(commandId: string): Promise<void> {
    const owned = await this.deps.ledger.get(commandId);
    if (!owned) return;
    if (isPersistedCompleted(owned.status)) return;
    const record = this.records.get(commandId);
    if (record?.completedReceipt) return; // 确定性已投递终局不可被迟到的限速拒绝倒退。
    await this.deps.ledger.updateStatus(commandId, "rate_limited");
    if (record) {
      this.cancelPendingRetry(record);
      this.cancelAckWatchdog(record);
      record.status = "rate_limited";
      this.notify();
    }
  }

  /**
   * FIX2 P1-3：`{t:"error", reason:"desktop_offline"}`（`remote-relay/src/room-do.js:722` 等——
   * 桌面此刻不在线，`handleInput`/`handleControl` 直接拒绝，不进 `pending_input` 暂存）不带
   * `command_id`——relay 没法（也没打算）告诉我们具体是哪一条指令撞上了这次拒绝。粗粒度处理，同
   * `handleStaleEpoch()` 既有手法："当前所有在飞指令"（`status` 为 `"sending"`/`"sent"`）一视同仁，
   * 整体标成 `handleRateLimited()` 同款的可重试终态——不需要像那两个带 `command_id` 的拒绝帧一样先
   * 过 `ledger.isOwn()`：这里遍历的就是本机内存里正在跟踪的记录，天然都是本机自己发出的。
   */
  async handleDesktopOffline(): Promise<void> {
    for (const record of this.records.values()) {
      if (!this.isInFlight(record)) continue;
      this.cancelPendingRetry(record);
      this.cancelAckWatchdog(record);
      try {
        await this.deps.ledger.updateStatus(record.commandId, "rate_limited");
      } catch {
        // 记账失败不阻止内存态转可重试——UI 状态优先保证不悬死，账本只是展示缓存（同文件头注
        // "本机最后已知的 outcome，供 UI 展示"）。
      }
      record.status = "rate_limited";
    }
    this.notify();
  }

  /**
   * R2（返工·重试铸新 id 造重复执行）：`"delivering_uncertain"` 的语义是"可能已经送到,只是没等到
   * ack"（见 `armAckWatchdog()` 头注）——重试不该像 `expired`/`rate_limited`/`give_up` 那三种真正
   * 终态一样铸一个全新 `command_id`（M0 §3 原文那条不对称：那三种是 relay/桌面已经对这个
   * `command_id` 有了终态记录,复用旧 id 只会拿到台账里那条稳定终态;而 `delivering_uncertain` 恰恰
   * 相反——relay/桌面很可能**已经**受理了这条指令,只是 ack 没能送回来,铸新 id 重发会让桌面把同一句
   * 话当成两条不同指令各自执行一遍）。改为复用原 `command_id` 原样重发——语义上就是又发生了一次
   * `sealAndSend()`（走 relay/桌面两层既有幂等：relay 按 `command_id` 主键幂等、桌面
   * `remote_inbox UNIQUE(command_id)` 幂等,双层兜底,绝不会二次投递）。只接受从
   * `"delivering_uncertain"` 发起——调用方（`AppRuntime.tsx::deriveSendBadge` 的 `onRetry`）已经按
   * `record.status` 分派,这里再核验一次防误用（同文件其余 handle* 方法的既有纵深防御纪律）,对其余
   * 状态是 no-op。
   */
  async retryDeliveringUncertain(commandId: string): Promise<void> {
    const record = this.records.get(commandId);
    if (!record || record.status !== "delivering_uncertain") return;
    record.status = "sending"; // 回到"发起中,等待结果"这一等待态——同 dispatch() 首发前的既有姿势。
    this.notify();
    await this.sealAndSend(record);
  }

  // ---------------------------------------------------------------------------
  // UI 查询——纯读取,不产生副作用。
  // ---------------------------------------------------------------------------

  getRecord(commandId: string): CommandRecord | undefined {
    return this.records.get(commandId);
  }

  private latestFor(predicate: (record: CommandRecord) => boolean): CommandRecord | undefined {
    let latest: CommandRecord | undefined;
    for (const record of this.records.values()) {
      if (!predicate(record)) continue;
      if (!latest || record.createdAt >= latest.createdAt) latest = record;
    }
    return latest;
  }

  /** 某个会话当前最新的一条 `input.send` 记录——`Composer` 用它渲染"发送中/已排队/失败"提示。 */
  getSendState(session: string): CommandRecord | undefined {
    return this.latestFor((r) => r.kind === "input.send" && r.session === session);
  }

  /** 某个会话当前最新的一条 `control.stop` 记录——`Composer` 的 Stop 按钮用它渲染状态。 */
  getStopState(session: string): CommandRecord | undefined {
    return this.latestFor((r) => r.kind === "control.stop" && r.session === session);
  }

  /**
   * 某个 decisionId 当前是否有一条尚未被服务器 `card.resolved` 取代的在飞/已确认回答——用于
   * `decisionCardView.ts::groupDecisionCardsIntoTurns` 的本地覆盖显示。**`status` 只返回
   * "submitting"/"failed" 两种,绝不返回 "chosen"**——CAS 输家显示赢家由桌面 `card.resolved` 里程碑
   * 自然到达,本层永远不本地臆断哪个选项赢了（调用方 `decisionCardView.ts` 还有一层独立保险：只在
   * 服务器 `status` 仍是 `"pending"` 时才采用这个覆盖值,一旦服务器状态翻到非 pending,覆盖值天然
   * 被忽略）。
   *
   * 返工②第②点：连同 `option`（本机真正点的那个选项）一起返回——`decisionCardView.ts` 用它覆盖
   * `chosen_option`，防止 `DecisionCard.tsx` 自带的失败重试按钮（`onChoose(decision_id,
   * chosen_option ?? options[0])`）在服务器 `chosen_option` 仍是 `null`（还没真正 resolve）时退回
   * `options[0]`——那会在本机点的不是第一个选项时，重试静默发错指令。`latest.option` 理论上恒有
   * 值（`answerCard()` 是这条记录唯一的构造入口，恒传 `option`）；`undefined` 分支只是类型系统
   * 意义上的穷尽,不代表真实可达路径。
   */
  getAnswerOverride(decisionId: string): { status: "submitting" | "failed"; option: string } | undefined {
    const latest = this.latestFor((r) => r.kind === "input.answer" && r.decisionId === decisionId);
    if (!latest || latest.option === undefined) return undefined;
    const option = latest.option;
    // C1-RQ：relay_queued 是"消息已安全排队,等桌面回来"，不是失败——同 sending/sent 一样按
    // submitting 展示，不提前吓用户以为要重试。
    if (latest.status === "sending" || latest.status === "sent" || latest.status === "relay_queued") {
      return { status: "submitting", option };
    }
    if (latest.status === "acked") {
      return { status: latest.ackOutcome === "failed" ? "failed" : "submitting", option };
    }
    // expired / give_up / rate_limited / delivering_uncertain（C1：看门狗超时,投递结果未知）：
    // 都视同需要用户重试的失败态。
    return { status: "failed", option };
  }
}
