// commandChannel.test.ts — TDD 覆盖 src/app/commandChannel.ts（T6f3 命令发送层）。
//
// 假 socket/账本/timer——不依赖 DOM（`.test.ts` 落 vitest "logic" node project）。真 K_room
// CryptoKey + 真 `crypto/envelope.ts::open()` 解密验证信封确实是"按 M0 §3 明文形状密封的"——不是
// 自证式地只调用被测代码内部用的同一份 `seal()` 再拿它自己解开就当验证过了（那种写法只证明
// seal/open 互逆，证不出"密文体字段名对不对"）：这里手动重建 AAD/解密，跟
// `AppRuntime.e2e.test.tsx` 的既有纪律一致。

import { beforeEach, describe, expect, it, vi } from "vitest";
import { ReadyState, type WebSocketCloseInfo, type WebSocketLike } from "../connection/types.ts";
import { open } from "../crypto/envelope.ts";
import { InMemoryCommandLedger } from "../store/commandLedger.ts";
import { deriveMsgCompletedClientMsgId } from "../events/clientMsgId.ts";
import { parseFrame } from "../events/parseFrame.ts";
import { loadFixture } from "../test-support/fixtures.ts";
import { ACK_WATCHDOG_MS, CommandChannel, type CommandRecord } from "./commandChannel.ts";

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

/**
 * 轮询等到条件成立——`sealAndSend()` 内部 `await seal()` 是真 WebCrypto 异步操作，落定所需的真实
 * 时间在不同机器/负载下不是常数（曾实测：单次 `setTimeout(resolve, 0)` 在本地大多数情况下够用，
 * 但在并行跑全量测试套件、系统负载较高时会偶发不够——flaky 的根因是"猜一个固定的等待量"而不是
 * "等到真正发生"）。轮询到期即真正落地，不依赖等待时长的猜测，消除这类 flake。
 */
async function waitUntil(predicate: () => boolean, timeoutMs = 3000, stepMs = 5): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) {
      throw new Error("waitUntil: condition never became true within timeout");
    }
    await new Promise((resolve) => setTimeout(resolve, stepMs));
  }
}

/** 手动排队的假定时器——`fire(n)` 精确触发排在最前的 n 个待触发回调（G4 退避测试用,不依赖真实时间）。
 *  C1（dogfood 修障第二批）：新增虚拟时钟 + `advance()`——自 C1 起 `CommandChannel` 在每次成功发送
 *  时都会额外排一条 `ACK_WATCHDOG_MS`（30 秒，远长于 G4 退避的毫秒级延迟）看门狗定时器，与既有 G4
 *  重试定时器共用同一份 `scheduleTimer`/`clearTimer` 注入点（不是两套机制）。`flushAll()` 对"延迟"
 *  完全无感——会把还没到期的 30 秒看门狗也一并触发,在 G4 重试测试里造成看门狗抢在真正的重试之前
 *  把 status 判成 "delivering_uncertain"（`sealAndSend()` 的 `isInFlight()` 校验因此拦住重试真正
 *  发送，测试假死等不到第二条消息）。`advance(ms)` 补一个只触发"确实到期"的入口（同 `fireAt` 语义，
 *  近似 `vi.advanceTimersByTime()`，但不用 vitest 假定时器——继续沿用 `fire(n)` 精确触发既有取
 *  向），G4 测试改用它推进"刚够 G4 退避那一档延迟"的量，30 秒看门狗天然还留在队列里不受影响。 */
class ManualTimers {
  private queue: Array<{ handle: number; cb: () => void; fireAt: number }> = [];
  private nextHandle = 1;
  private virtualNow = 0;

  schedule = (cb: () => void, delayMs: number): unknown => {
    const handle = this.nextHandle++;
    this.queue.push({ handle, cb, fireAt: this.virtualNow + delayMs });
    return handle;
  };

  clear = (handle: unknown): void => {
    this.queue = this.queue.filter((entry) => entry.handle !== handle);
  };

  /** 触发全部当前排队的回调（保留新排进来的,不递归展开——每次显式调用一层）。不推进虚拟时钟——
   *  不与"长延迟看门狗+短延迟重试"混排的测试继续用它，语义不变。 */
  flushAll(): void {
    const batch = this.queue;
    this.queue = [];
    for (const entry of batch) entry.cb();
  }

  /** 虚拟时钟前进 `ms`——只触发 `fireAt` 落在新时刻之内的条目（真实 `setTimeout` 的延迟语义），
   *  还没到期的（比如同批次里更长延迟的看门狗）继续留在队列里，不受影响。 */
  advance(ms: number): void {
    this.virtualNow += ms;
    const due = this.queue.filter((entry) => entry.fireAt <= this.virtualNow);
    this.queue = this.queue.filter((entry) => entry.fireAt > this.virtualNow);
    for (const entry of due) entry.cb();
  }

  get pendingCount(): number {
    return this.queue.length;
  }
}

function makeChannel(overrides: Partial<{
  socket: FakeSocket | null;
  epoch: number | null;
  ledger: InMemoryCommandLedger;
  timers: ManualTimers;
  onChange: () => void;
  kRoomKey: CryptoKey;
  maxStaleEpochRetries: number;
  now: () => number;
  localInputRateLimit: number;
  localControlRateLimit: number;
  localRateWindowMs: number;
}> = {}) {
  const socketBox = { current: overrides.socket === undefined ? new FakeSocket() : overrides.socket };
  const epochBox = { current: overrides.epoch === undefined ? 1 : overrides.epoch };
  const ledger = overrides.ledger ?? new InMemoryCommandLedger();
  const timers = overrides.timers ?? new ManualTimers();
  const onChange = overrides.onChange ?? (() => {});
  return {
    socketBox,
    epochBox,
    ledger,
    timers,
    channelPromise: (async () => {
      const kRoomKey = overrides.kRoomKey ?? (await makeKRoomKey());
      const channel = new CommandChannel({
        room: ROOM,
        kRoomKey,
        getEpoch: () => epochBox.current,
        getSocket: () => socketBox.current,
        ledger,
        onChange,
        scheduleTimer: timers.schedule,
        clearTimer: timers.clear,
        maxStaleEpochRetries: overrides.maxStaleEpochRetries,
        now: overrides.now,
        localInputRateLimit: overrides.localInputRateLimit,
        localControlRateLimit: overrides.localControlRateLimit,
        localRateWindowMs: overrides.localRateWindowMs,
      });
      return { channel, kRoomKey };
    })(),
  };
}

describe("CommandChannel: 三条发送入口的 wire 形状（M0 §3 逐字段照 handle_command_envelope 消费面）", () => {
  it("sendInput(): kind=input 顶层信封 + 密文体 {t:'input.send', session, text}", async () => {
    const { socketBox, channelPromise } = makeChannel();
    const { channel, kRoomKey } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hello desktop");

    expect(socketBox.current!.sent).toHaveLength(1);
    const envelope = JSON.parse(socketBox.current!.sent[0]!) as Record<string, unknown>;
    expect(envelope.kind).toBe("input");
    expect(envelope.session).toBe(SESSION);
    expect(envelope.command_id).toBe(commandId);
    expect(envelope.seq).toBeNull();
    expect(Object.prototype.hasOwnProperty.call(envelope, "client_msg_id")).toBe(false);

    const plain = await decryptEnvelope(envelope, kRoomKey);
    expect(plain).toEqual({ t: "input.send", session: SESSION, text: "hello desktop" });
  });

  it("answerCard(): CommandRecord 记录 kind=input.answer 与 decisionId", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.answerCard(SESSION, "dec-1", "继续");

    const record = channel.getRecord(commandId)!;
    expect(record.kind).toBe("input.answer");
    expect(record.decisionId).toBe("dec-1");
  });

  it("answerCard(): 解密后的明文字段名与 M0 §3 完全一致（不是 answer/optionText 等相近但错的字段名）", async () => {
    const { socketBox, channelPromise } = makeChannel();
    const { channel, kRoomKey } = await channelPromise;
    await channel.answerCard(SESSION, "dec-1", "继续");
    const envelope = JSON.parse(socketBox.current!.sent[0]!) as Record<string, unknown>;
    expect(envelope.kind).toBe("input");
    const plain = await decryptEnvelope(envelope, kRoomKey);
    expect(plain).toEqual({ t: "input.answer", session: SESSION, decision_id: "dec-1", option: "继续" });
  });

  it("stopSession(): kind=control 密文体 {t:'control.stop', session, issued_at_ms, expires_at_ms=issued+30000}", async () => {
    const fixedNow = 1_765_430_400_000;
    const ledger = new InMemoryCommandLedger();
    const kRoomKey = await makeKRoomKey();
    const socket = new FakeSocket();
    const channel = new CommandChannel({
      room: ROOM,
      kRoomKey,
      getEpoch: () => 1,
      getSocket: () => socket,
      ledger,
      now: () => fixedNow,
    });
    const commandId = await channel.stopSession(SESSION);
    const envelope = JSON.parse(socket.sent[0]!) as Record<string, unknown>;
    expect(envelope.kind).toBe("control");
    expect(envelope.command_id).toBe(commandId);
    const plain = await decryptEnvelope(envelope, kRoomKey);
    expect(plain).toEqual({
      t: "control.stop",
      session: SESSION,
      issued_at_ms: fixedNow,
      expires_at_ms: fixedNow + 30_000,
    });
  });
});

describe("CommandChannel · G3：发送前先持久化到账本，且账本记录与 wire 上的 command_id 一致", () => {
  it("recordSent() 在 socket.send() 之前完成（sealAndSend 是 dispatch 里 await ledger.recordSent 之后才调用的）", async () => {
    const order: string[] = [];
    const ledger = new InMemoryCommandLedger();
    const originalRecordSent = ledger.recordSent.bind(ledger);
    ledger.recordSent = async (input) => {
      order.push("ledger.recordSent");
      await originalRecordSent(input);
    };
    const socket = new FakeSocket();
    const originalSend = socket.send.bind(socket);
    socket.send = (data) => {
      order.push("socket.send");
      originalSend(data);
    };
    const kRoomKey = await makeKRoomKey();
    const channel = new CommandChannel({ room: ROOM, kRoomKey, getEpoch: () => 1, getSocket: () => socket, ledger });
    await channel.sendInput(SESSION, "hi");
    expect(order).toEqual(["ledger.recordSent", "socket.send"]);
  });

  it("ledger.isOwn() 命中刚发出的 command_id", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    expect(await ledger.isOwn(commandId)).toBe(true);
    expect(await ledger.isOwn("some-other-devices-command-id")).toBe(false);
  });
});

describe("CommandChannel · G3：input.ack/input.expired 按持久账本过滤——不是本机发出的一律忽略", () => {
  it("handleAck(): 不在账本里的 command_id（别的手机发出的广播）被忽略，不产生任何本机记录", async () => {
    const changeSpy = vi.fn();
    const { channelPromise } = makeChannel({ onChange: changeSpy });
    const { channel } = await channelPromise;
    changeSpy.mockClear();
    await channel.handleAck("not-mine-command-id", "ok");
    expect(channel.getRecord("not-mine-command-id")).toBeUndefined();
    expect(changeSpy).not.toHaveBeenCalled();
  });

  it("handleAck(): 过滤判据是账本 isOwn()，不是内存态 records 是否存在——ledger.recordSent 失败后该 command_id 从未真正发出（socket 零调用），即便内存态仍留着一条 give_up 记录，收到一条同 id 的 ack 也被当作『不是我发的』忽略（isOwn 以账本为准，不是拿内存态当权威）", async () => {
    const ledger = new InMemoryCommandLedger();
    ledger.recordSent = async () => {
      throw new Error("ledger write failed");
    };
    const socket = new FakeSocket();
    const kRoomKey = await makeKRoomKey();
    const channel = new CommandChannel({ room: ROOM, kRoomKey, getEpoch: () => 1, getSocket: () => socket, ledger });

    const id = await channel.sendInput(SESSION, "hi");
    expect(socket.sent).toHaveLength(0); // 落账失败——从未真正发出（dispatch() 提前 return）。
    expect(channel.getRecord(id)!.status).toBe("give_up"); // 但内存态仍然留着这条记录。
    expect(await ledger.isOwn(id)).toBe(false); // 账本里确实没有它。

    await channel.handleAck(id, "ok");
    // 如果过滤只看内存态 records 有没有这个 id（而不是真的去问账本），这里会被错误地当"本机的"
    // 处理、状态被 ack 覆盖成 "acked"——正确实现必须维持 "give_up" 不变。
    expect(channel.getRecord(id)!.status).toBe("give_up");
  });

  it("handleAck(): 本机发出的 command_id 收到 outcome=ok/queued/failed 分别落到对应状态", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    const idOk = await channel.sendInput(SESSION, "a");
    const idQueued = await channel.sendInput(SESSION, "b");
    const idFailed = await channel.sendInput(SESSION, "c");

    await channel.handleAck(idOk, "ok");
    await channel.handleAck(idQueued, "queued");
    await channel.handleAck(idFailed, "failed");

    expect(channel.getRecord(idOk)!.status).toBe("acked");
    expect(channel.getRecord(idOk)!.ackOutcome).toBe("ok");
    expect(channel.getRecord(idQueued)!.ackOutcome).toBe("queued");
    expect(channel.getRecord(idFailed)!.ackOutcome).toBe("failed");
    expect(channel.getRecord(idFailed)!.ackReason).toBeUndefined();
    expect((await ledger.get(idFailed))?.status).toBe("failed");
  });

  it("handleAck(): 真样张 no_agent reason 被保留到本机 CommandRecord", async () => {
    const fixture = loadFixture<{ cases: Array<{ name: string; frame: unknown }> }>("data-plane-v1.json");
    const frame = fixture.cases.find((entry) => entry.name === "input_ack_failed_no_agent")?.frame;
    if (!frame) throw new Error("data-plane-v1.json missing case: input_ack_failed_no_agent");
    const parsed = parseFrame(frame);
    expect(parsed.ok).toBe(true);
    if (!parsed.ok || parsed.frame.t !== "input.ack") throw new Error("fixture must be a valid input.ack");
    const ackFrame = parsed.frame;
    expect(ackFrame.reason).toBe("no_agent");

    const kRoomKey = await makeKRoomKey();
    const channel = new CommandChannel({
      room: ROOM,
      kRoomKey,
      getEpoch: () => 1,
      getSocket: () => new FakeSocket(),
      ledger: new InMemoryCommandLedger(),
      randomUUID: () => ackFrame.command_id,
    });
    const commandId = await channel.sendInput(SESSION, "hello");
    await channel.handleAck(commandId, ackFrame.outcome, ackFrame.reason);

    expect(channel.getRecord(commandId)).toMatchObject({
      status: "acked",
      ackOutcome: "failed",
      ackReason: "no_agent",
    });
  });

  it("handleAck(): 未知 outcome 字符串兜底为中性态 taken_over，且 recognized=false（不冒充 ok，见 ackOutcome.ts）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const id = await channel.sendInput(SESSION, "a");
    await channel.handleAck(id, "some-future-outcome-c1-has-never-seen");
    const record = channel.getRecord(id)!;
    expect(record.ackOutcome).toBe("taken_over");
    expect(record.ackRecognized).toBe(false);
  });

  it("handleExpired(): 不在账本里的 command_id 被忽略；本机的 input.send 会转 expired 状态", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const mine = await channel.sendInput(SESSION, "a");
    await channel.handleExpired("someone-elses-command-id");
    expect(channel.getRecord("someone-elses-command-id")).toBeUndefined();

    await channel.handleExpired(mine);
    expect(channel.getRecord(mine)!.status).toBe("expired");
  });

  it("返工②第①点·control.stop 伪 expired 被拒——本机真发出的 control.stop 命令收到一条（伪造/协议误用）input.expired，按命令族拒绝，status 不变", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    const stopId = await channel.stopSession(SESSION);
    expect(channel.getRecord(stopId)!.status).toBe("sent"); // control.stop 即刻投递、不暂存,已发出。

    await channel.handleExpired(stopId);

    // 命令族不对——不接受这个状态转换：内存态与账本都保持原样，不是 "expired"。
    expect(channel.getRecord(stopId)!.status).toBe("sent");
    expect((await ledger.get(stopId))?.status).toBe("sent");
  });

  it("返工②第①点·input.answer 命令族仍然接受 expired（只有 control.stop 被拒，不是把 handleExpired 整体锁死成只认 input.send）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const answerId = await channel.answerCard(SESSION, "dec-1", "继续");
    await channel.handleExpired(answerId);
    expect(channel.getRecord(answerId)!.status).toBe("expired");
  });
});

describe("CommandChannel · C1-RQ（dogfood 修障第二批）：handleRelayQueued()——relay input.relay_queued 帧", () => {
  it("不在账本里的 command_id 被忽略，不产生任何本机记录", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    await channel.handleRelayQueued("not-mine-command-id", 12345);
    expect(channel.getRecord("not-mine-command-id")).toBeUndefined();
  });

  it("本机的 input.send 收到 relay_queued 后转 relay_queued 状态并带上 expires_at，取消挂着的重试与看门狗", async () => {
    const { channelPromise, ledger, timers, epochBox } = makeChannel({ epoch: 1 });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");

    // 顺带验证与 G4 重试机制的交互：先排一条 stale_epoch 重试——relay_queued 到达后必须一并取消
    // （relay 已经确认收下，没必要再重试发送）。
    epochBox.current = 2;
    channel.handleStaleEpoch();
    expect(timers.pendingCount).toBe(2); // 看门狗(1) + 重试(1)。

    await channel.handleRelayQueued(commandId, 1_800_000_000_000);
    const record = channel.getRecord(commandId)!;
    expect(record.status).toBe("relay_queued");
    expect(record.relayQueuedExpiresAt).toBe(1_800_000_000_000);
    expect((await ledger.get(commandId))?.status).toBe("relay_queued");
    expect(timers.pendingCount).toBe(0); // 重试与看门狗都被取消。
  });

  it("本机的 input.answer 也能收到 relay_queued（不只是 input.send）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const answerId = await channel.answerCard(SESSION, "dec-1", "继续");
    await channel.handleRelayQueued(answerId, 999);
    expect(channel.getRecord(answerId)!.status).toBe("relay_queued");
  });

  it("control.stop 收到（伪造/协议误用）relay_queued 按命令族拒绝，status 不变——协议上 control.stop 走 kind=control，relay 从不会真的产生这个组合", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    const stopId = await channel.stopSession(SESSION);
    expect(channel.getRecord(stopId)!.status).toBe("sent");

    await channel.handleRelayQueued(stopId, 999);

    expect(channel.getRecord(stopId)!.status).toBe("sent");
    expect((await ledger.get(stopId))?.status).toBe("sent");
  });

  it("已经收到真 ack 的指令不会被迟到的 relay_queued 倒退（较弱的中间事实不覆盖更强的终态判定）", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    await channel.handleAck(commandId, "ok");
    expect(channel.getRecord(commandId)!.status).toBe("acked");

    await channel.handleRelayQueued(commandId, 999);

    expect(channel.getRecord(commandId)!.status).toBe("acked"); // 没有被倒退成 relay_queued。
    expect((await ledger.get(commandId))?.status).toBe("ok");
  });

  it("迟到的 relay_queued 能把 delivering_uncertain（看门狗超时）拉回来——这是较弱事实里唯一允许覆盖的方向：从不确定变成确定还在排队", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    const record = channel.getRecord(commandId)!;
    record.status = "delivering_uncertain"; // 模拟看门狗已经先触发。

    await channel.handleRelayQueued(commandId, 4242);

    expect(channel.getRecord(commandId)!.status).toBe("relay_queued");
    expect(channel.getRecord(commandId)!.relayQueuedExpiresAt).toBe(4242);
  });
});

describe("CommandChannel · G4 硬验收：stale_epoch → 取最新 epoch、同 command_id 重封重发", () => {
  it("resend 时信封换成最新 epoch，但 command_id 与首次发送完全相同", async () => {
    const { socketBox, epochBox, timers, channelPromise } = makeChannel({ epoch: 5 });
    const { channel, kRoomKey } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    const first = JSON.parse(socketBox.current!.sent[0]!) as Record<string, unknown>;
    expect(first.epoch).toBe(5);

    // relay 拒绝：新 epoch 是 9——调用方（AppRuntime）先把 getEpoch() 会读到的值更新掉。
    epochBox.current = 9;
    channel.handleStaleEpoch();
    // C1：用 advance() 而不是 flushAll()——首发已经额外排了一条 30 秒 ack 看门狗（远晚于这里
    // attempts=0 的 G4 退避延迟），flushAll() 会把它也一并触发，若看门狗抢在这条重试前面把 status
    // 判成 "delivering_uncertain"，sealAndSend() 的 isInFlight() 校验会拦住这次重试真正发送，
    // 让下面的 waitUntil() 永远等不到第二条消息。advance() 只推进到刚好够触发这一档重试延迟。
    timers.advance(300); // DEFAULT_RETRY_BASE_MS（attempts=0 → base*2**0）。
    // sealAndSend 内部有一次 await seal()——真 WebCrypto 异步操作，轮询等到它真正落地。
    await waitUntil(() => socketBox.current!.sent.length >= 2);

    expect(socketBox.current!.sent).toHaveLength(2);
    const second = JSON.parse(socketBox.current!.sent[1]!) as Record<string, unknown>;
    expect(second.epoch).toBe(9);
    expect(second.command_id).toBe(commandId); // 硬验收核心断言：同一个 command_id。
    const plain = await decryptEnvelope(second, kRoomKey);
    expect(plain).toEqual({ t: "input.send", session: SESSION, text: "hi" }); // 业务内容不变。
  });

  it("已经收到 ack 的指令不会被 stale_epoch 重发（只重发仍在等结果的）", async () => {
    const { socketBox, epochBox, timers, channelPromise } = makeChannel({ epoch: 1 });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    await channel.handleAck(commandId, "ok");
    expect(socketBox.current!.sent).toHaveLength(1);

    epochBox.current = 2;
    channel.handleStaleEpoch();
    timers.flushAll();
    // 已 acked 的记录在 handleStaleEpoch() 里同步 `continue`（不进入 sealAndSend，没有任何异步
    // 工作可等）——不需要轮询，直接断言即可确认这条分支没有排出任何重发。
    expect(socketBox.current!.sent).toHaveLength(1); // 没有第二条。
  });

  it("有限次重试：超过上限后转 give_up，不再继续自动重发", async () => {
    const { socketBox, epochBox, timers, channelPromise } = makeChannel({ epoch: 1, maxStaleEpochRetries: 2 });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");

    for (let i = 0; i < 2; i += 1) {
      const expectedCount = i + 2; // 首发(1) + 第 i+1 次重试。
      epochBox.current = (epochBox.current ?? 0) + 1;
      channel.handleStaleEpoch();
      // C1：advance() 而不是 flushAll()——同上一条测试的理由，每次成功（重）发都会重开一条 30 秒
      // 看门狗，与这里毫秒级的 G4 退避延迟混排在同一个队列里；advance() 只推进到刚好够这一档重试。
      timers.advance(300 * 2 ** i); // attempts=i → base*2**i（本测试默认 base=300）。
      await waitUntil(() => socketBox.current!.sent.length >= expectedCount);
    }
    expect(socketBox.current!.sent).toHaveLength(3); // 首发 + 2 次重试。
    expect(channel.getRecord(commandId)!.status).toBe("sent");

    // 第三次 stale_epoch——attempts 已达上限，`give_up` 转换是同步的（handleStaleEpoch 内直接
    // continue，不排 sealAndSend），不需要轮询等待。give_up 分支也会取消挂着的看门狗（见
    // commandChannel.ts），flushAll() 这里已经没有"长延迟未到期"的顾虑，用它清空剩余队列即可。
    epochBox.current = (epochBox.current ?? 0) + 1;
    channel.handleStaleEpoch();
    timers.flushAll();
    expect(socketBox.current!.sent).toHaveLength(3);
    expect(channel.getRecord(commandId)!.status).toBe("give_up");
  });

  it("退避：连续两次 attempts 使用的延迟按指数增长（0 次→base，1 次→base*2，均不超过 cap）", async () => {
    const delays: number[] = [];
    const timers = new ManualTimers();
    const capturingSchedule = (cb: () => void, delayMs: number) => {
      delays.push(delayMs);
      return timers.schedule(cb, delayMs);
    };
    const ledger = new InMemoryCommandLedger();
    const kRoomKey = await makeKRoomKey();
    const socket = new FakeSocket();
    const epochBox = { current: 1 };
    const channel = new CommandChannel({
      room: ROOM,
      kRoomKey,
      getEpoch: () => epochBox.current,
      getSocket: () => socket,
      ledger,
      scheduleTimer: capturingSchedule,
      clearTimer: timers.clear,
      staleEpochRetryBaseMs: 100,
      staleEpochRetryCapMs: 10_000,
    });
    await channel.sendInput(SESSION, "hi");

    epochBox.current = 2;
    channel.handleStaleEpoch();
    // C1：advance() 而不是 flushAll()——首发额外排了一条 30 秒 ack 看门狗（ACK_WATCHDOG_MS，
    // 与这里 base=100 的 G4 退避完全不在一个量级），同上面两条测试的理由，只推进到刚好够这一档
    // 重试延迟，看门狗留在队列里不受影响。
    timers.advance(100);
    await waitUntil(() => socket.sent.length >= 2);

    epochBox.current = 3;
    channel.handleStaleEpoch();
    timers.advance(200);
    await waitUntil(() => socket.sent.length >= 3);

    // delays 现在也会记录每次（重）发送重开的 ACK_WATCHDOG_MS 看门狗——本测试只关心 G4 退避那两条,
    // 过滤掉看门狗的条目再断言。
    expect(delays.filter((delayMs) => delayMs !== ACK_WATCHDOG_MS)).toEqual([100, 200]);
    // 测试强化（返工·Minor）：补一条计数断言——上面那条断言只看"非看门狗"的延迟,如果某次重发
    // 忘记先 cancelAckWatchdog() 就 armAckWatchdog()（"重复武装看门狗"回归），delays 里会多出
    // 额外的 ACK_WATCHDOG_MS 条目,但上面那条断言完全无感。三次真正发送（首发 + 两次重试）应该
    // 恰好三条看门狗延迟,不多不少。
    expect(delays.filter((delayMs) => delayMs === ACK_WATCHDOG_MS)).toHaveLength(3);
  });

  it("返工①硬验收·终态竞态：stale 排定重试 → ack 先到达（取消挂着的定时器）→ 定时器到点触发 → 零发送，status 稳定停在 acked（不被重试改回 sent）", async () => {
    const { socketBox, epochBox, timers, channelPromise } = makeChannel({ epoch: 5 });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    expect(socketBox.current!.sent).toHaveLength(1);

    // stale_epoch 拒绝——排定一次重试（不立即触发，ManualTimers 需要显式 flushAll() 才跑）。
    epochBox.current = 9;
    channel.handleStaleEpoch();
    // C1：首发已经排了一条 30 秒 ack 看门狗，这里的 stale_epoch 又加一条 G4 重试——2 条待触发。
    expect(timers.pendingCount).toBe(2); // 看门狗(1) + 重试(1)。

    // ack 在定时器真正触发之前到达——转终态,同时取消这条挂着的重试与看门狗（两条独立定时器）。
    await channel.handleAck(commandId, "ok");
    expect(channel.getRecord(commandId)!.status).toBe("acked");
    expect(timers.pendingCount).toBe(0); // 返工①第①点后半句 + C1：两条挂着的定时器都被取消。

    // 定时器到点触发（即便这里手动再 flushAll 一次——队列已空，没有任何回调可跑）。
    timers.flushAll();
    await new Promise((resolve) => setTimeout(resolve, 20)); // 给可能遗漏的异步路径一点时间显形。

    expect(socketBox.current!.sent).toHaveLength(1); // 零发送——没有第二条。
    expect(channel.getRecord(commandId)!.status).toBe("acked"); // 稳定停在 acked，没有被重试改回别的状态。
  });

  it("返工①校验点冗余验证：即便『取消定时器』这一步意外失效（clearTimer 是 no-op、条目仍留在队列里），到点真正触发时 sealAndSend() 自己的状态二次校验依然拦住零发送（不是唯一靠取消机制兜底）", async () => {
    const rawTimers = new ManualTimers();
    const ledger = new InMemoryCommandLedger();
    const kRoomKey = await makeKRoomKey();
    const socket = new FakeSocket();
    const epochBox = { current: 5 };
    const channel = new CommandChannel({
      room: ROOM,
      kRoomKey,
      getEpoch: () => epochBox.current,
      getSocket: () => socket,
      ledger,
      scheduleTimer: rawTimers.schedule,
      clearTimer: () => {}, // 故意 no-op——模拟"取消请求没有真正生效"，条目仍会在 flushAll() 时触发。
    });
    const commandId = await channel.sendInput(SESSION, "hi");
    expect(socket.sent).toHaveLength(1);

    epochBox.current = 9;
    channel.handleStaleEpoch();
    // C1：首发已经排了一条看门狗，这里再加一条 G4 重试——2 条待触发。
    expect(rawTimers.pendingCount).toBe(2);

    // cancelPendingRetry()/cancelAckWatchdog() 都会调用 clearTimer()，但它是 no-op——两条都清不掉。
    await channel.handleAck(commandId, "ok");
    expect(rawTimers.pendingCount).toBe(2); // 两条都还在队列里（clearTimer 没有真正移除它们）。

    rawTimers.flushAll(); // 定时器真正触发——sealAndSend() 内部调用。
    await new Promise((resolve) => setTimeout(resolve, 20));

    // 即便定时器真触发了，sealAndSend() 自己在 seal 前查了一次 status（isInFlight()==false，因为
    // 已经是 "acked"）,直接 return——零发送。这是与"取消定时器"完全独立的第二道防线。
    expect(socket.sent).toHaveLength(1);
    expect(channel.getRecord(commandId)!.status).toBe("acked");
  });
});

describe("CommandChannel · C1（dogfood 修障第二批）：ack 看门狗——status:sent 满 ACK_WATCHDOG_MS 未见 ack/relay_queued 转 delivering_uncertain", () => {
  it("首次发送成功后立即武装一条看门狗（不需要等任何拒绝帧触发）", async () => {
    const { timers, channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    await channel.sendInput(SESSION, "hi");
    expect(timers.pendingCount).toBe(1);
  });

  it("到点仍是 sent（未见 ack/relay_queued）→ 翻 delivering_uncertain，并通知 onChange", async () => {
    const changeSpy = vi.fn();
    const { timers, channelPromise } = makeChannel({ onChange: changeSpy });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    changeSpy.mockClear();

    timers.advance(ACK_WATCHDOG_MS);

    expect(channel.getRecord(commandId)!.status).toBe("delivering_uncertain");
    expect(changeSpy).toHaveBeenCalled();
  });

  it("收到 ack 后看门狗被取消——到点不会把 status 改回 delivering_uncertain", async () => {
    const { timers, channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    await channel.handleAck(commandId, "ok");
    expect(timers.pendingCount).toBe(0);

    timers.advance(ACK_WATCHDOG_MS); // 队列已空——没有任何回调可跑。

    expect(channel.getRecord(commandId)!.status).toBe("acked");
  });

  it("收到 relay_queued 后看门狗被取消——到点不会翻 delivering_uncertain", async () => {
    const { timers, channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    await channel.handleRelayQueued(commandId, 999);
    expect(timers.pendingCount).toBe(0);

    timers.advance(ACK_WATCHDOG_MS);

    expect(channel.getRecord(commandId)!.status).toBe("relay_queued");
  });

  it("delivering_uncertain 之后真正的 ack 到达仍能把它推进到 acked（看门狗超时不是永久锁死，可恢复）", async () => {
    const { timers, channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    timers.advance(ACK_WATCHDOG_MS);
    expect(channel.getRecord(commandId)!.status).toBe("delivering_uncertain");

    await channel.handleAck(commandId, "ok");

    expect(channel.getRecord(commandId)!.status).toBe("acked");
  });

  it("G4 重试重发后看门狗随最新一次发送重新计时（不是钉死在首发时刻）", async () => {
    const { timers, epochBox, socketBox, channelPromise } = makeChannel({ epoch: 1 });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");

    timers.advance(1_000); // 首发之后过一小段时间（远小于 ACK_WATCHDOG_MS）。
    epochBox.current = 2;
    channel.handleStaleEpoch(); // 排一条 G4 重试（attempts=0 → 延迟 300ms，fireAt = 1000+300=1300）。
    timers.advance(300);
    await waitUntil(() => socketBox.current!.sent.length >= 2); // 重发真正落地，看门狗随之重开。

    // 重发发生在虚拟时刻 1300——若看门狗正确地从这次重发重新计时，新的 fireAt = 1300 + 30000 =
    // 31300；若错误地仍钉在首发时刻的旧 fireAt=30000（比如取消/重新武装没生效），下面这一大步
    // 推进（总计到 31299）会连带把那条"本该已被取消"的旧看门狗也触发,提前把 status 判成
    // delivering_uncertain——下面第一条断言就会失败,能捕获这个回归。
    timers.advance(29_999); // 虚拟时刻推进到 1300 + 29999 = 31299（比正确的新 fireAt 早 1ms）。
    expect(channel.getRecord(commandId)!.status).toBe("sent"); // 还没到"重发之后 30 秒"。

    timers.advance(1); // 虚拟时刻 31300——正好是重发之后的 30 秒。
    expect(channel.getRecord(commandId)!.status).toBe("delivering_uncertain");
  });
});

describe("CommandChannel · R1（返工·断线看门狗吃掉 G4 重连补发）：cancelAckWatchdogsForDisconnect()", () => {
  it("断线时调用——取消挂着的看门狗，原定到点时刻不再触发，status 停在 sent", async () => {
    const { timers, channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    expect(timers.pendingCount).toBe(1); // 首发已武装一条看门狗。

    channel.cancelAckWatchdogsForDisconnect();
    expect(timers.pendingCount).toBe(0);

    timers.advance(ACK_WATCHDOG_MS); // 原定到点时刻——但看门狗已被取消，没有回调可跑。
    expect(channel.getRecord(commandId)!.status).toBe("sent"); // 没有翻 delivering_uncertain。
  });

  it("断线 >30s 后重连（handleStaleEpoch）：同 command_id 自动补发，未被吃掉的看门狗未提前判定 delivering_uncertain", async () => {
    const { timers, epochBox, socketBox, channelPromise } = makeChannel({ epoch: 1 });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    expect(socketBox.current!.sent).toHaveLength(1);

    // 模拟断线：取消看门狗，再让虚拟时钟前进 40 秒（超过 ACK_WATCHDOG_MS）——若看门狗没有被
    // 暂停，这里会先翻 delivering_uncertain，导致下面的 handleStaleEpoch() 因为
    // isInFlight()===false 被跳过，补发也就不会发生（这正是本单要修的回归）。
    channel.cancelAckWatchdogsForDisconnect();
    timers.advance(40_000);
    expect(channel.getRecord(commandId)!.status).toBe("sent"); // 断线期间不计时——仍是 sent。

    // 重连——同 handleReplayHead() 的既有姿势：更新 epoch，再触发 handleStaleEpoch()。
    epochBox.current = 2;
    channel.handleStaleEpoch();
    timers.advance(300); // attempts=0 → base=300ms（makeChannel 默认参数）。
    await waitUntil(() => socketBox.current!.sent.length >= 2);

    const secondEnvelope = JSON.parse(socketBox.current!.sent[1]!) as Record<string, unknown>;
    expect(secondEnvelope.command_id).toBe(commandId); // 同 command_id 自动补发，不是新铸的。
    expect(channel.getRecord(commandId)!.status).toBe("sent"); // 未翻 delivering_uncertain。
  });

  it("连线状态下 30 秒无 ack 仍翻 delivering_uncertain（既有行为保留，本单不改这条——见同 describe block 之上『C1』一节的既有覆盖）", async () => {
    const { timers, channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    timers.advance(ACK_WATCHDOG_MS); // 全程没有调用 cancelAckWatchdogsForDisconnect()——模拟连线状态。
    expect(channel.getRecord(commandId)!.status).toBe("delivering_uncertain");
  });
});

describe("CommandChannel · R2（返工·重试铸新 id 造重复执行）：retryDeliveringUncertain() 复用原 command_id，其余终态不受影响", () => {
  it("retryDeliveringUncertain()：重发帧的 command_id 与原记录一致（不是新铸的），status 回到 sent", async () => {
    const { timers, socketBox, channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");
    timers.advance(ACK_WATCHDOG_MS);
    expect(channel.getRecord(commandId)!.status).toBe("delivering_uncertain");

    await channel.retryDeliveringUncertain(commandId);
    await waitUntil(() => socketBox.current!.sent.length >= 2);

    expect(channel.getRecord(commandId)!.status).toBe("sent"); // 复用同一条记录重新进入等待态。
    const secondEnvelope = JSON.parse(socketBox.current!.sent[1]!) as Record<string, unknown>;
    expect(secondEnvelope.command_id).toBe(commandId); // 重发帧仍是原 command_id，不是新铸的。
  });

  it("retryDeliveringUncertain()：对非 delivering_uncertain 状态的记录是 no-op（纵深防御，防误用）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi"); // 状态是 "sent"，不是 delivering_uncertain。
    await channel.retryDeliveringUncertain(commandId);
    expect(channel.getRecord(commandId)!.status).toBe("sent"); // 未被改动（既没有重发也没有出错）。
  });

  it("防误伤：expired 终态的重试仍走既有『铸新 command_id』设计（本单只改 delivering_uncertain 这一态，commandChannel.ts:630-631 一带的既有设计不动）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const original = await channel.sendInput(SESSION, "retry me");
    await channel.handleExpired(original);
    expect(channel.getRecord(original)!.status).toBe("expired");

    const retried = await channel.sendInput(SESSION, "retry me"); // UI 层对 expired 的重试仍是再调一次 sendInput()。
    expect(retried).not.toBe(original); // 新 command_id——与 delivering_uncertain 复用旧 id 不同。
    expect(channel.getRecord(retried)!.status).toBe("sent");
  });
});

describe("CommandChannel · TTL/expired：重发用新 command_id（靠调用约定，不是特殊代码路径）", () => {
  it("同一段文本连续两次 sendInput() 产生两个不同的 command_id", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const first = await channel.sendInput(SESSION, "same text");
    const second = await channel.sendInput(SESSION, "same text");
    expect(first).not.toBe(second);
  });

  it("expired 之后由 UI 层发起的重试（再调一次 sendInput）拿到的是新 command_id，旧记录仍留着 expired 终态", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const original = await channel.sendInput(SESSION, "retry me");
    await channel.handleExpired(original);
    expect(channel.getRecord(original)!.status).toBe("expired");

    const retried = await channel.sendInput(SESSION, "retry me");
    expect(retried).not.toBe(original);
    expect(channel.getRecord(retried)!.status).toBe("sent");
  });
});

describe("CommandChannel · getAnswerOverride()：只给 {status:submitting/failed, option}，从不本地臆断 chosen", () => {
  it("发出后未 ack：submitting，option 是本机点的那个选项", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    await channel.answerCard(SESSION, "dec-1", "继续");
    expect(channel.getAnswerOverride("dec-1")).toEqual({ status: "submitting", option: "继续" });
  });

  it("ack outcome=ok/queued：仍是 submitting（真正的终态要等 card.resolved，不是 ack 就地当作完成）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const id = await channel.answerCard(SESSION, "dec-1", "继续");
    await channel.handleAck(id, "queued");
    expect(channel.getAnswerOverride("dec-1")).toEqual({ status: "submitting", option: "继续" });
  });

  it("ack outcome=failed：failed（DecisionCard 自带的重试态会显示重试按钮）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const id = await channel.answerCard(SESSION, "dec-1", "继续");
    await channel.handleAck(id, "failed");
    expect(channel.getAnswerOverride("dec-1")).toEqual({ status: "failed", option: "继续" });
  });

  it("没有任何回答记录的 decisionId：undefined（调用方据此不覆盖，展示服务器原始状态）", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    expect(channel.getAnswerOverride("never-answered")).toBeUndefined();
  });

  it("同一个 decisionId 两次回答（如重试，重试选的还是原来那个选项）：只取最新一条的状态，option 也是最新一条的", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const first = await channel.answerCard(SESSION, "dec-1", "继续");
    await channel.handleAck(first, "failed");
    expect(channel.getAnswerOverride("dec-1")).toEqual({ status: "failed", option: "继续" });

    const second = await channel.answerCard(SESSION, "dec-1", "继续");
    expect(channel.getAnswerOverride("dec-1")).toEqual({ status: "submitting", option: "继续" }); // 新一条覆盖旧一条的 failed。
    void second;
  });

  it("返工②第②点·答卡选第 2 项失败后重试仍是第 2 项——option 精确保留、不会退回第一个选项", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    // 选项列表假想为 ["继续", "停止"]——本机点的是第 2 项"停止"，不是 options[0]。
    const first = await channel.answerCard(SESSION, "dec-1", "停止");
    await channel.handleAck(first, "failed");
    expect(channel.getAnswerOverride("dec-1")).toEqual({ status: "failed", option: "停止" });

    // 重试——UI 层用同一段"再调一次 answerCard()"逻辑，重发的是原来失败的那个选项，不是第一项。
    const retried = await channel.answerCard(SESSION, "dec-1", "停止");
    expect(retried).not.toBe(first); // 新 command_id。
    expect(channel.getAnswerOverride("dec-1")).toEqual({ status: "submitting", option: "停止" });
    // 旧记录终态保留——不因为有了新记录就被改写。
    expect(channel.getRecord(first)!.status).toBe("acked");
    expect(channel.getRecord(first)!.ackOutcome).toBe("failed");
  });
});

describe("CommandChannel · getSendState()/getStopState()：按会话取最新一条对应记录", () => {
  let channel: CommandChannel;
  beforeEach(async () => {
    const { channelPromise } = makeChannel();
    ({ channel } = await channelPromise);
  });

  it("getSendState 只看 input.send，不与 input.answer/control.stop 混淆", async () => {
    await channel.answerCard(SESSION, "dec-1", "x");
    await channel.stopSession(SESSION);
    const sendId = await channel.sendInput(SESSION, "hi");
    const state = channel.getSendState(SESSION);
    expect(state?.commandId).toBe(sendId);
    expect(state?.kind).toBe("input.send");
  });

  it("getStopState 取该会话最新一条 control.stop", async () => {
    await channel.stopSession(SESSION);
    const second = await channel.stopSession(SESSION);
    const state = channel.getStopState(SESSION);
    expect(state?.commandId).toBe(second);
  });

  it("不同会话互不干扰", async () => {
    await channel.sendInput("other-session", "x");
    const state = channel.getSendState(SESSION);
    expect(state).toBeUndefined();
  });
});

describe("CommandChannel · 确定性 msg.completed 回执", () => {
  it("匹配本会话 queued input.send 后转为无徽标终局，并忽略迟到的 ack/expired/rate_limited", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "queued message");
    await channel.handleAck(commandId, "queued");

    await channel.handleMsgCompleted(SESSION, deriveMsgCompletedClientMsgId(SESSION, commandId));

    const delivered = channel.getRecord(commandId)!;
    expect(delivered.status).toBe("acked");
    expect(delivered.ackOutcome).toBe("ok");
    expect(delivered.completedReceipt).toBe(true);
    expect((await ledger.get(commandId))?.status).toBe("ok");

    await channel.handleAck(commandId, "queued");
    await channel.handleExpired(commandId);
    await channel.handleRateLimited(commandId);
    expect(channel.getRecord(commandId)!.ackOutcome).toBe("ok");
    expect(channel.getRecord(commandId)!.status).toBe("acked");
    expect((await ledger.get(commandId))?.status).toBe("ok");
  });

  it("出站账本更新失败也不推翻已经匹配的持久 msg.completed 投递事实", async () => {
    class RejectingStatusLedger extends InMemoryCommandLedger {
      override async updateStatus(): Promise<void> {
        throw new Error("injected ledger failure");
      }
    }
    const ledger = new RejectingStatusLedger();
    const { channelPromise } = makeChannel({ ledger });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "durable event wins");

    await expect(channel.handleMsgCompleted(SESSION, deriveMsgCompletedClientMsgId(SESSION, commandId))).resolves.toBeUndefined();
    expect(channel.getRecord(commandId)!.status).toBe("acked");
    expect(channel.getRecord(commandId)!.ackOutcome).toBe("ok");
    expect(channel.getRecord(commandId)!.completedReceipt).toBe(true);
  });

  it("failed/expired/rate_limited 保持既有可转换语义，可被后续 queued ack 更新", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    const failedId = await channel.sendInput(SESSION, "failed");
    const expiredId = await channel.sendInput(SESSION, "expired");
    const limitedId = await channel.sendInput(SESSION, "limited");
    await channel.handleAck(failedId, "failed");
    await channel.handleExpired(expiredId);
    await channel.handleRateLimited(limitedId);

    await channel.handleAck(failedId, "queued");
    await channel.handleAck(expiredId, "queued");
    await channel.handleAck(limitedId, "queued");

    for (const commandId of [failedId, expiredId, limitedId]) {
      expect(channel.getRecord(commandId)!.status).toBe("acked");
      expect(channel.getRecord(commandId)!.ackOutcome).toBe("queued");
      expect((await ledger.get(commandId))?.status).toBe("queued");
    }
  });

  it("重载后仅剩持久 ok 账本时，迟到 ack/expired/rate_limited 仍不能覆盖投递终局", async () => {
    const ledger = new InMemoryCommandLedger();
    const commandId = "pre-reload-command";
    await ledger.recordSent({ commandId, kind: "input.send", session: SESSION, createdAt: 1 });
    await ledger.updateStatus(commandId, "ok");
    const { channelPromise } = makeChannel({ ledger });
    const { channel } = await channelPromise;
    expect(channel.getRecord(commandId)).toBeUndefined(); // 模拟页面重载：内存态已清空。

    await channel.handleAck(commandId, "queued");
    await channel.handleExpired(commandId);
    await channel.handleRateLimited(commandId);

    expect((await ledger.get(commandId))?.status).toBe("ok");
  });

  it("只接受同会话、确定性 id 匹配且仍 pending/queued 的 input.send；failed 不受影响", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const pendingId = await channel.sendInput(SESSION, "pending");
    const queuedId = await channel.sendInput(SESSION, "queued");
    const failedId = await channel.sendInput(SESSION, "failed");
    await channel.handleAck(queuedId, "queued");
    await channel.handleAck(failedId, "failed");

    await channel.handleMsgCompleted("other-session", deriveMsgCompletedClientMsgId(SESSION, pendingId));
    await channel.handleMsgCompleted(SESSION, deriveMsgCompletedClientMsgId(SESSION, "not-mine"));
    await channel.handleMsgCompleted(SESSION, deriveMsgCompletedClientMsgId(SESSION, failedId));
    expect(channel.getRecord(pendingId)!.status).toBe("sent");
    expect(channel.getRecord(queuedId)!.ackOutcome).toBe("queued");
    expect(channel.getRecord(failedId)!.ackOutcome).toBe("failed");

    await channel.handleMsgCompleted(SESSION, deriveMsgCompletedClientMsgId(SESSION, pendingId));
    await channel.handleMsgCompleted(SESSION, deriveMsgCompletedClientMsgId(SESSION, queuedId));
    expect(channel.getRecord(pendingId)!.completedReceipt).toBe(true);
    expect(channel.getRecord(queuedId)!.completedReceipt).toBe(true);
    expect(channel.getRecord(failedId)!.completedReceipt).toBeUndefined();
  });
});

describe("CommandChannel · 没有可用连接时不发送但不崩溃（状态保持 sending）", () => {
  it("epoch 为 null 时不 send()", async () => {
    const { socketBox, channelPromise } = makeChannel({ epoch: null });
    const { channel } = await channelPromise;
    const id = await channel.sendInput(SESSION, "hi");
    expect(socketBox.current!.sent).toHaveLength(0);
    expect(channel.getRecord(id)!.status).toBe("sending");
  });

  it("socket 为 null 时不 send()", async () => {
    const { socketBox, channelPromise } = makeChannel({ socket: null });
    const { channel } = await channelPromise;
    const id = await channel.sendInput(SESSION, "hi");
    expect(socketBox.current).toBeNull();
    expect(channel.getRecord(id)!.status).toBe("sending");
  });
});

describe("CommandChannel · 返工③第②点：handleRateLimited()——relay input_rate_limited/control_rate_limited 错误帧转可重试终态", () => {
  it("本机发出的 command_id 收到限速错误帧——转 rate_limited，取消挂着的重试定时器", async () => {
    const { channelPromise, ledger, timers, epochBox } = makeChannel({ epoch: 1 });
    const { channel } = await channelPromise;
    const commandId = await channel.sendInput(SESSION, "hi");

    // 顺带验证与 G4 重试机制的交互：先排一条 stale_epoch 重试，再被限速拒绝——重试必须一并取消。
    epochBox.current = 2;
    channel.handleStaleEpoch();
    // C1：首发已经排了一条 30 秒 ack 看门狗——2 条待触发。
    expect(timers.pendingCount).toBe(2);

    await channel.handleRateLimited(commandId);
    expect(channel.getRecord(commandId)!.status).toBe("rate_limited");
    expect((await ledger.get(commandId))?.status).toBe("rate_limited");
    expect(timers.pendingCount).toBe(0); // 重试与看门狗都被取消。
  });

  it("不在账本里的 command_id（别的手机自己的限速错误——理论不该广播到这里，纵深防御）被忽略", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    await channel.handleRateLimited("not-mine-command-id");
    expect(channel.getRecord("not-mine-command-id")).toBeUndefined();
  });
});

describe("CommandChannel · FIX2 P1-3：handleDesktopOffline()——relay desktop_offline 错误帧（不带 command_id）粗粒度转可重试终态", () => {
  it("当前所有在飞指令（sending/sent）整体标 rate_limited，取消各自挂着的重试定时器", async () => {
    const { channelPromise, ledger, timers, epochBox } = makeChannel({ epoch: 1 });
    const { channel } = await channelPromise;
    const inputId = await channel.sendInput(SESSION, "hi");
    const stopId = await channel.stopSession(SESSION);

    // 顺带验证与 G4 重试机制的交互：先给两条都排一条 stale_epoch 重试。
    epochBox.current = 2;
    channel.handleStaleEpoch();
    // C1：两条命令首发时各自排了一条 30 秒 ack 看门狗，这里再各加一条 G4 重试——4 条待触发。
    expect(timers.pendingCount).toBe(4);

    await channel.handleDesktopOffline();
    expect(channel.getRecord(inputId)!.status).toBe("rate_limited");
    expect(channel.getRecord(stopId)!.status).toBe("rate_limited");
    expect((await ledger.get(inputId))?.status).toBe("rate_limited");
    expect((await ledger.get(stopId))?.status).toBe("rate_limited");
    expect(timers.pendingCount).toBe(0); // 两条记录各自的重试与看门狗都被取消。
  });

  it("已经进终态（acked/rate_limited）的记录不受影响——只动 sending/sent", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    const ackedId = await channel.sendInput(SESSION, "already acked");
    await channel.handleAck(ackedId, "ok");
    const sendingId = await channel.sendInput(SESSION, "still in flight");

    await channel.handleDesktopOffline();
    expect(channel.getRecord(ackedId)!.status).toBe("acked"); // 没有被覆盖
    expect(channel.getRecord(sendingId)!.status).toBe("rate_limited");
  });

  it("没有任何在飞指令时调用——空操作，不崩溃", async () => {
    const { channelPromise } = makeChannel();
    const { channel } = await channelPromise;
    await expect(channel.handleDesktopOffline()).resolves.toBeUndefined();
  });
});

describe("CommandChannel · 返工③第①点：本地分通道滑动窗——超窗『稍候重试』而不是发出去挨拒", () => {
  it("input 通道连发超过本地上限——第 N+1 条不占 socket/账本，直接标 rate_limited", async () => {
    let nowValue = 1_000_000;
    const socket = new FakeSocket();
    const ledger = new InMemoryCommandLedger();
    const kRoomKey = await makeKRoomKey();
    const channel = new CommandChannel({
      room: ROOM,
      kRoomKey,
      getEpoch: () => 1,
      getSocket: () => socket,
      ledger,
      now: () => nowValue,
      localInputRateLimit: 3,
      localRateWindowMs: 60_000,
    });

    const ids: string[] = [];
    for (let i = 0; i < 3; i += 1) {
      nowValue += 100;
      ids.push(await channel.sendInput(SESSION, `msg-${i}`));
    }
    expect(socket.sent).toHaveLength(3); // 前 3 条都在窗口内，正常发出。

    nowValue += 100;
    const blockedId = await channel.sendInput(SESSION, "msg-blocked");
    expect(socket.sent).toHaveLength(3); // 第 4 条——本地窗口拦下，没有第 4 次 send()。
    expect(channel.getRecord(blockedId)!.status).toBe("rate_limited");
    expect(await ledger.isOwn(blockedId)).toBe(false); // 从未真正发出——不写账本（见 dispatch() 注释）。

    // 时间推移超过窗口——名额恢复，新的发送恢复正常。
    nowValue += 60_000;
    const recoveredId = await channel.sendInput(SESSION, "msg-recovered");
    expect(socket.sent).toHaveLength(4);
    expect(channel.getRecord(recoveredId)!.status).not.toBe("rate_limited");
    void ids;
  });

  it("input 与 control 各自独立的桶——刷爆 input 不影响 control.stop 照常发出", async () => {
    let nowValue = 1_000_000;
    const socket = new FakeSocket();
    const ledger = new InMemoryCommandLedger();
    const kRoomKey = await makeKRoomKey();
    const channel = new CommandChannel({
      room: ROOM,
      kRoomKey,
      getEpoch: () => 1,
      getSocket: () => socket,
      ledger,
      now: () => nowValue,
      localInputRateLimit: 2,
      localControlRateLimit: 2,
      localRateWindowMs: 60_000,
    });

    for (let i = 0; i < 2; i += 1) {
      nowValue += 10;
      await channel.sendInput(SESSION, `msg-${i}`);
    }
    nowValue += 10;
    const blockedInputId = await channel.sendInput(SESSION, "blocked");
    expect(channel.getRecord(blockedInputId)!.status).toBe("rate_limited");

    // input 桶已经耗尽，但 control 桶是独立的——stopSession() 照常发出。
    nowValue += 10;
    const stopId = await channel.stopSession(SESSION);
    expect(channel.getRecord(stopId)!.status).not.toBe("rate_limited");
    expect(socket.sent.some((raw) => (JSON.parse(raw) as { kind: string }).kind === "control")).toBe(true);
  });

  it("默认本地上限低于 relay 硬闸（30/60s）——不传自定义值时默认值本身 < 30，起到『留余量』作用", async () => {
    const socket = new FakeSocket();
    const ledger = new InMemoryCommandLedger();
    const kRoomKey = await makeKRoomKey();
    let nowValue = 0;
    const channel = new CommandChannel({
      room: ROOM,
      kRoomKey,
      getEpoch: () => 1,
      getSocket: () => socket,
      ledger,
      now: () => nowValue,
    });
    // 默认值下连发 30 条（relay 硬闸的确切阈值）——本地必须已经先一步拦截，不能全部放行到 30。
    let blockedCount = 0;
    for (let i = 0; i < 30; i += 1) {
      nowValue += 1;
      const id = await channel.sendInput(SESSION, `m${i}`);
      if (channel.getRecord(id)!.status === "rate_limited") blockedCount += 1;
    }
    expect(blockedCount).toBeGreaterThan(0); // 30 条里必然有被本地拦下的——默认上限 < 30。
    expect(socket.sent.length).toBeLessThan(30);
  });
});

describe("CommandChannel · FIX2 P2-5：trySendControlSlot()——control.snapshot 请求并入命令通道", () => {
  it("占用与 control.stop 相同的本地滑窗——刷爆 control 桶后 snapshot 也被拦，且 control.stop 也被拦（同一条桶）", async () => {
    const { channelPromise } = makeChannel({ localControlRateLimit: 1 });
    const { channel } = await channelPromise;

    expect(await channel.trySendControlSlot("snap-1", SESSION)).toBe(true); // 第一枚——占满桶。
    const stopId = await channel.stopSession(SESSION); // control.stop 走同一个 "control" 桶。
    expect(channel.getRecord(stopId)!.status).toBe("rate_limited"); // 桶已经被 snapshot 占满。

    expect(await channel.trySendControlSlot("snap-2", SESSION)).toBe(false); // 桶满——第二枚也被拦。
  });

  it("成功占位时代 AppRuntime 记账——commandId 之后能被 ledger.isOwn() 认领（control_rate_limited 拒绝帧不会因为查无此单被静默忽略）", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;

    const allowed = await channel.trySendControlSlot("snap-abc", SESSION);
    expect(allowed).toBe(true);
    expect(await ledger.isOwn("snap-abc")).toBe(true);

    // 之后一条带这个 command_id 的 control_rate_limited 拒绝帧可以被正常认领、翻可重试态——
    // 不会因为查无此单被 handleRateLimited() 的 isOwn() 过滤掉。
    await channel.handleRateLimited("snap-abc");
    expect((await ledger.get("snap-abc"))?.status).toBe("rate_limited");
  });

  it("control.history 共享同一滑窗，并以自己的 kind 入账", async () => {
    const { channelPromise, ledger } = makeChannel();
    const { channel } = await channelPromise;
    expect(await channel.trySendControlSlot("hist-abc", SESSION, "control.history")).toBe(true);
    expect((await ledger.get("hist-abc"))?.kind).toBe("control.history");
  });

  it("本地窗口已满时返回 false，不占用网络往返，也不写账本", async () => {
    const { channelPromise, ledger } = makeChannel({ localControlRateLimit: 1 });
    const { channel } = await channelPromise;

    expect(await channel.trySendControlSlot("snap-1", SESSION)).toBe(true);
    expect(await channel.trySendControlSlot("snap-2", SESSION)).toBe(false);
    expect(await ledger.isOwn("snap-2")).toBe(false); // 被拦下的这次尝试没有留下任何账本痕迹。
  });
});

// 类型引用（避免 `CommandRecord` 仅作类型导入被 tree-shake 掉时的未使用告警——部分断言里直接用到
// 了字段但没有显式标注类型时,保留一个哨兵引用)。
void (null as unknown as CommandRecord);
