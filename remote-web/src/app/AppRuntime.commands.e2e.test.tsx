// AppRuntime.commands.e2e.test.tsx — T6f3 · 端到端：composer 发送/答卡/Stop 接线，假 relay 帧序
// 驱动 `AppRuntime` 的命令面——覆盖任务书 §4 要求的六条场景：
//   ① 发送 → ack 翻面
//   ② stale_epoch → 同 command_id、新 epoch 重封重发（G4 硬验收）
//   ③ input.expired → 用户点重试 → 新 command_id 重发（TTL 语义）
//   ④ 他机 ack 广播（本机从未发过的 command_id）被持久账本过滤，不污染本机状态（G3 缓解）
//   ⑤ Stop 二次确认流
//   ⑥ 答卡 → 服务器 card.resolved 到达后翻终态（不本地臆断赢家）
//
// 复用 `AppRuntime.e2e.test.tsx` 头注记录的既有纪律：假 relay 侧加密独立实现（不 import
// `crypto/envelope.ts` 的 `seal`/`open`/`buildAAD`），验证的是"生产代码产出的信封，一个完全独立的
// 实现也能正确解密"，不是自证式的"用被测代码自己解开自己加密的东西"。

import "fake-indexeddb/auto";
import { describe, expect, it } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

// ============================================================================
// 假 WebSocket（同 AppRuntime.e2e.test.tsx 的姊妹实现）
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

/** 解密本机（被测 AppRuntime）发出的一条指令面信封——独立实现，元数据从信封自身字段读出。 */
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

/** 在 `sent[]` 里找第一条明文 `t` 匹配的指令信封——不假设发送顺序（`control.snapshot` 自动请求
 *  可能穿插在中间）。 */
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

/** 装配到"已选中会话 s-1、epoch 已知（=5）"的公共前置——六个场景都从这一步开始。 */
async function setupSelectedSessionAtEpoch5() {
  const { kRoomRaw, stored } = await makeStoredCredentials();
  const keyStore = new InMemoryKeyStore();
  await keyStore.saveKeys(stored);
  const eventStore = new IndexedDbEventStore(`apprt-cmd-test-${crypto.randomUUID()}`);
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
  // replay.head 触发的自动 control.snapshot 请求先让它落地，避免污染后续"第一条 sent"之类的假设
  // （本文件全部改用 findSentCommand 按明文 t 查找，这里等待只是为了让后续 seq 计数更好读）。
  await waitFor(() => expect(factory.last.sent.length).toBeGreaterThanOrEqual(1));

  let seq = 1;
  return { kRoomRaw, stored, keyStore, eventStore, ledger, factory, nextSeq: () => (seq += 1) };
}

// ============================================================================
// ① 发送 → ack 翻面
// ============================================================================

describe("AppRuntime commands e2e · ① input.send → ack 翻面", () => {
  it("composer 发送后先显示发送中，ack outcome=queued 翻成弱担保措辞，随后 outcome=ok 翻成无徽标", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    const input = screen.getByTestId("composer-input");
    await user.type(input, "hello desktop");
    await user.click(screen.getByTestId("composer-send"));

    const sent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("input.send not sent yet");
      return found;
    });
    expect(sent.envelope.kind).toBe("input");
    expect(sent.envelope.session).toBe("s-1");
    expect(sent.plaintext).toEqual({ t: "input.send", session: "s-1", text: "hello desktop" });
    const commandId = sent.envelope.command_id as string;

    await screen.findByTestId("composer-send-badge");
    // data-status 是稳定的技术属性，不受 locale 自动探测（jsdom 恒 en-US）影响——文案本身的 i18n
    // 覆盖已经在 Composer.test.tsx 里用显式 locale="zh" 断言过。
    expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("sending");

    await act(async () => {
      factory.last.simulateMessage({ t: "input.ack", command_id: commandId, outcome: "queued" });
    });
    await waitFor(() => expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("queued"));

    await act(async () => {
      factory.last.simulateMessage({ t: "input.ack", command_id: commandId, outcome: "ok" });
    });
    await waitFor(() => expect(screen.queryByTestId("composer-send-badge")).toBeNull());
  });
});

// ============================================================================
// ② stale_epoch → 同 command_id、新 epoch 重封重发（G4 硬验收）
// ============================================================================

describe("AppRuntime commands e2e · ② stale_epoch → 同 command_id 新 epoch 重封重发", () => {
  it("relay 拒绝 stale_epoch 后，AppRuntime 用信封里 currentEpoch 重新密封同一个 command_id 再发一次", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    const input = screen.getByTestId("composer-input");
    await user.type(input, "stale epoch probe");
    await user.click(screen.getByTestId("composer-send"));

    const first = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("not sent yet");
      return found;
    });
    expect(first.envelope.epoch).toBe(5);
    const commandId = first.envelope.command_id as string;

    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "stale_epoch", currentEpoch: 9 });
    });

    await waitFor(async () => {
      const resent = await findResentInputSend(factory.last.sent, kRoomRaw, commandId, 9);
      expect(resent).toBeDefined();
    });
    const resent = await findResentInputSend(factory.last.sent, kRoomRaw, commandId, 9);
    expect(resent!.envelope.command_id).toBe(commandId); // 硬验收核心：同一个 command_id。
    expect(resent!.envelope.epoch).toBe(9);
    expect(resent!.plaintext).toEqual({ t: "input.send", session: "s-1", text: "stale epoch probe" });
  });

  async function findResentInputSend(sent: string[], kRoomRaw: Uint8Array, commandId: string, epoch: number) {
    for (const raw of sent) {
      const decoded = await decryptSentEnvelope(raw, kRoomRaw);
      if (decoded.plaintext.t === "input.send" && decoded.envelope.command_id === commandId && decoded.envelope.epoch === epoch) {
        return decoded;
      }
    }
    return undefined;
  }

  it("返工①第②点·epoch.changed 广播（不是 stale_epoch 拒绝）→ 在飞命令同样按同 command_id、新 epoch 重发（与 control.snapshot 请求同待遇）", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    await user.type(screen.getByTestId("composer-input"), "epoch changed probe");
    await user.click(screen.getByTestId("composer-send"));

    const first = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("not sent yet");
      return found;
    });
    expect(first.envelope.epoch).toBe(5);
    const commandId = first.envelope.command_id as string;

    // 桌面重连、relay 广播 epoch.changed——这条命令还没等到 ack。
    await act(async () => {
      factory.last.simulateMessage({ t: "epoch.changed", epoch: 11, ts: Date.now() });
    });

    const resent = await waitFor(async () => {
      const found = await findResentInputSend(factory.last.sent, kRoomRaw, commandId, 11);
      if (!found) throw new Error("not resent yet");
      return found;
    });
    expect(resent.envelope.command_id).toBe(commandId); // 同一个 command_id。
    expect(resent.envelope.epoch).toBe(11);
    expect(resent.plaintext).toEqual({ t: "input.send", session: "s-1", text: "epoch changed probe" });
  });
});

// ============================================================================
// ③ input.expired → 用户点重试 → 新 command_id 重发（TTL 语义）
// ============================================================================

describe("AppRuntime commands e2e · ③ input.expired → 新 command_id 重发", () => {
  it("relay 回 input.expired 后徽标转过期态，点重试用新 command_id 重发同一段文本", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    await user.type(screen.getByTestId("composer-input"), "left in the pending queue");
    await user.click(screen.getByTestId("composer-send"));

    const original = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("not sent yet");
      return found;
    });
    const originalCommandId = original.envelope.command_id as string;

    await act(async () => {
      factory.last.simulateMessage({ t: "input.expired", command_id: originalCommandId });
    });
    await waitFor(() => expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("expired"));

    await user.click(screen.getByTestId("composer-send-retry"));

    await waitFor(async () => {
      const sentCount = (
        await Promise.all(factory.last.sent.map((raw) => decryptSentEnvelope(raw, kRoomRaw)))
      ).filter((d) => d.plaintext.t === "input.send").length;
      expect(sentCount).toBe(2);
    });
    const allInputSends = (await Promise.all(factory.last.sent.map((raw) => decryptSentEnvelope(raw, kRoomRaw)))).filter(
      (d) => d.plaintext.t === "input.send",
    );
    const retried = allInputSends.find((d) => d.envelope.command_id !== originalCommandId)!;
    expect(retried.envelope.command_id).not.toBe(originalCommandId); // 硬验收核心：新 command_id。
    expect(retried.plaintext).toEqual({ t: "input.send", session: "s-1", text: "left in the pending queue" });
  });
});

// ============================================================================
// ④ 他机 ack 广播（本机从未发出的 command_id）被持久账本过滤（G3 缓解）
// ============================================================================

describe("AppRuntime commands e2e · ④ 同窗多手机互收 ack 广播 → 按持久账本过滤", () => {
  it("relay 广播一条别的设备发出的 command_id 的 ack——本机账本没有这一行，静默忽略，不产生徽标/不落幽灵行", async () => {
    const { factory, ledger } = await setupSelectedSessionAtEpoch5();

    await act(async () => {
      factory.last.simulateMessage({ t: "input.ack", command_id: "other-devices-command-id", outcome: "ok" });
    });
    // 给异步 handleAck() 一点时间跑完（本机账本 isOwn() 返回 false，静默返回）。
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(screen.queryByTestId("composer-send-badge")).toBeNull(); // 没有被污染出一条本机徽标。
    expect(await ledger.get("other-devices-command-id")).toBeNull(); // 也没有在本机账本里留下幽灵行。
  });

  it("同样地，别的设备的 input.expired 广播也被过滤", async () => {
    const { factory, ledger } = await setupSelectedSessionAtEpoch5();
    await act(async () => {
      factory.last.simulateMessage({ t: "input.expired", command_id: "other-devices-command-id-2" });
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.queryByTestId("composer-send-badge")).toBeNull();
    expect(await ledger.get("other-devices-command-id-2")).toBeNull();
  });
});

// ============================================================================
// ⑤ Stop 二次确认流
// ============================================================================

describe("AppRuntime commands e2e · ⑤ Stop 二次确认流", () => {
  it("running 时出现 Stop 按钮；点击先要确认，确认后才发 control.stop；run.status 转 idle 后整行收起", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory, nextSeq } = await setupSelectedSessionAtEpoch5();

    const runningFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: nextSeq(),
      clientMsgId: "run-1",
      epoch: 5,
      payload: { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" },
    });
    await act(async () => {
      factory.last.simulateMessage(runningFrame);
    });
    await waitFor(() => expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Running|运行中/));

    await user.click(screen.getByTestId("stream-stop-button"));
    expect(screen.getByTestId("stream-stop-confirm")).toBeTruthy();

    await user.click(screen.getByTestId("stream-stop-confirm-yes"));

    const stopSent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "control.stop");
      if (!found) throw new Error("control.stop not sent yet");
      return found;
    });
    expect(stopSent.envelope.kind).toBe("control");
    expect(stopSent.envelope.session).toBe("s-1");
    const plain = stopSent.plaintext as { t: string; session: string; issued_at_ms: number; expires_at_ms: number };
    expect(plain.t).toBe("control.stop");
    expect(plain.session).toBe("s-1");
    expect(plain.expires_at_ms - plain.issued_at_ms).toBe(30_000);

    await screen.findByTestId("stream-stop-badge");

    const idleFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: nextSeq(),
      clientMsgId: "run-2",
      epoch: 5,
      payload: { t: "run.status", session_id: "s-1", status: "idle", run_id: null },
    });
    await act(async () => {
      factory.last.simulateMessage(idleFrame);
    });
    await waitFor(() => expect(screen.queryByTestId("stream-stop-row")).toBeNull());
  });
});

// ============================================================================
// ⑥ 答卡 → 服务器 card.resolved 到达后翻终态（不本地臆断赢家）
// ============================================================================

describe("AppRuntime commands e2e · ⑥ 答卡：点选项 → 发 input.answer → submitting → card.resolved 翻终态", () => {
  it("点选项后按钮禁用（submitting 本地覆盖）；card.resolved 到达后展示服务器真正的赢家（可能不是本机点的那个）", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory, nextSeq } = await setupSelectedSessionAtEpoch5();

    const cardCreatedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: nextSeq(),
      clientMsgId: "card-1",
      epoch: 5,
      payload: {
        t: "card.created",
        block: {
          type: "decision_card",
          decision_id: "dec-1",
          kind: "ask",
          question: "继续执行下一步吗？",
          options: ["继续", "停止"],
          recommended: "继续",
          rationale: null,
          payload: null,
          source_run_id: "run-1",
          status: "pending",
          chosen_option: null,
          created_at: Date.now(),
        },
      },
    });
    await act(async () => {
      factory.last.simulateMessage(cardCreatedFrame);
    });
    const section = await screen.findByTestId("stream-decision-cards");
    const optionButton = within(section).getByRole("button", { name: /继续/ });
    expect((optionButton as HTMLButtonElement).disabled).toBe(false); // T6f3：答卡已激活，不再是 T6f2 的禁用态。

    await user.click(optionButton);

    const answerSent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.answer");
      if (!found) throw new Error("input.answer not sent yet");
      return found;
    });
    expect(answerSent.plaintext).toEqual({ t: "input.answer", session: "s-1", decision_id: "dec-1", option: "继续" });

    // submitting 本地覆盖生效——按钮变禁用（DecisionCard.tsx 自带的 status==="submitting" 禁用规则）。
    await waitFor(() => expect((within(section).getByRole("button", { name: /继续/ }) as HTMLButtonElement).disabled).toBe(true));

    // 服务器最终判给了"停止"（比如另一台手机抢先答对——CAS 输家场景）——本机不本地臆断自己点的
    // 那个赢了，card.resolved 到达后展示的是服务器真相。
    const cardResolvedFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: nextSeq(),
      clientMsgId: "card-1-resolved",
      epoch: 5,
      payload: { t: "card.resolved", decision_id: "dec-1", status: "chosen", chosen_option: "停止" },
    });
    await act(async () => {
      factory.last.simulateMessage(cardResolvedFrame);
    });

    // DecisionCard.tsx 的紧凑回执行——jsdom 下 remote-web 没有显式传 locale，`@app/i18n` 走系统
    // 语言自动探测（jsdom 恒 en-US）——`decisionCard.chosen` 的 zh/en 文案本身不是本单改动范围。
    await waitFor(() => expect(within(section).getByText("Chose: 停止")).toBeTruthy());
    expect(within(section).queryByRole("button", { name: /继续/ })).toBeNull();
  });
});

// ============================================================================
// ⑦（返工③第②点）relay input_rate_limited/control_rate_limited 错误帧 → 可重试终态，重试新 id
// ============================================================================

describe("AppRuntime commands e2e · ⑦ rate_limited 错误帧翻可重试态", () => {
  it("relay 回 {t:'error',reason:'input_rate_limited',command_id} 后，composer 徽标从 sending 翻成 rate_limited；点重试用新 command_id 重发同一段文本", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    await user.type(screen.getByTestId("composer-input"), "too fast");
    await user.click(screen.getByTestId("composer-send"));

    const original = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("not sent yet");
      return found;
    });
    const originalCommandId = original.envelope.command_id as string;

    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "input_rate_limited", frame: "input", command_id: originalCommandId });
    });
    await waitFor(() => expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("rate_limited"));

    await user.click(screen.getByTestId("composer-send-retry"));

    const allInputSends = await waitFor(async () => {
      const decoded = await Promise.all(factory.last.sent.map((raw) => decryptSentEnvelope(raw, kRoomRaw)));
      const sends = decoded.filter((d) => d.plaintext.t === "input.send");
      if (sends.length < 2) throw new Error("retry not sent yet");
      return sends;
    });
    const retried = allInputSends.find((d) => d.envelope.command_id !== originalCommandId)!;
    expect(retried.envelope.command_id).not.toBe(originalCommandId); // 新 command_id，不复用旧的。
    expect(retried.plaintext).toEqual({ t: "input.send", session: "s-1", text: "too fast" });
  });

  it("relay 回 control_rate_limited——composer 的 Stop 徽标同样翻成 rate_limited，可重试", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory, nextSeq } = await setupSelectedSessionAtEpoch5();

    const runningFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: nextSeq(),
      clientMsgId: "run-1",
      epoch: 5,
      payload: { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" },
    });
    await act(async () => {
      factory.last.simulateMessage(runningFrame);
    });
    await waitFor(() => expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Running|运行中/));

    await user.click(screen.getByTestId("stream-stop-button"));
    await user.click(screen.getByTestId("stream-stop-confirm-yes"));

    const stopSent = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "control.stop");
      if (!found) throw new Error("control.stop not sent yet");
      return found;
    });
    const stopCommandId = stopSent.envelope.command_id as string;

    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "control_rate_limited", frame: "control", command_id: stopCommandId });
    });
    await waitFor(() => expect(screen.getByTestId("stream-stop-badge").getAttribute("data-status")).toBe("rate_limited"));

    // 被限速后 Stop 按钮不再被"发送中"锁死——可以再次点击重试（新一轮确认 → 新 command_id）。
    await user.click(screen.getByTestId("stream-stop-retry"));
    await waitFor(async () => {
      const decoded = await Promise.all(factory.last.sent.map((raw) => decryptSentEnvelope(raw, kRoomRaw)));
      const stops = decoded.filter((d) => d.plaintext.t === "control.stop");
      expect(stops.some((d) => d.envelope.command_id !== stopCommandId)).toBe(true);
    });
  });
});

// ============================================================================
// ⑧ FIX2 P1-3：queue_full / ip_message_rate_limited / desktop_offline 三类拒绝帧消费
// ============================================================================

describe("AppRuntime commands e2e · ⑧ FIX2 P1-3：queue_full/ip_message_rate_limited/desktop_offline 三类拒绝帧翻可重试态", () => {
  it("relay 回 {t:'error',reason:'queue_full',command_id}（input 暂存队列已满）——同 input_rate_limited 待遇：翻 rate_limited，可重试", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    await user.type(screen.getByTestId("composer-input"), "queue is full");
    await user.click(screen.getByTestId("composer-send"));

    const original = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("not sent yet");
      return found;
    });
    const originalCommandId = original.envelope.command_id as string;

    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "queue_full", frame: "input", command_id: originalCommandId });
    });
    await waitFor(() => expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("rate_limited"));

    await user.click(screen.getByTestId("composer-send-retry"));
    await waitFor(async () => {
      const decoded = await Promise.all(factory.last.sent.map((raw) => decryptSentEnvelope(raw, kRoomRaw)));
      const sends = decoded.filter((d) => d.plaintext.t === "input.send");
      expect(sends.some((d) => d.envelope.command_id !== originalCommandId)).toBe(true);
    });
  });

  it("relay 回 {t:'error',reason:'ip_message_rate_limited',command_id}（IP 维度限速）——同 rate_limited 待遇：翻 rate_limited", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory } = await setupSelectedSessionAtEpoch5();

    await user.type(screen.getByTestId("composer-input"), "ip throttled");
    await user.click(screen.getByTestId("composer-send"));

    const original = await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("not sent yet");
      return found;
    });
    const originalCommandId = original.envelope.command_id as string;

    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "ip_message_rate_limited", frame: "input", command_id: originalCommandId });
    });
    await waitFor(() => expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("rate_limited"));
  });

  it("relay 回 {t:'error',reason:'desktop_offline'}（不带 command_id，桌面此刻不在线）——当前所有在飞指令（composer 发送 + Stop）粗粒度整体翻可重试，不悬死", async () => {
    const user = userEvent.setup();
    const { kRoomRaw, factory, nextSeq } = await setupSelectedSessionAtEpoch5();

    const runningFrame = await encryptEventFrame(kRoomRaw, {
      session: "s-1",
      seq: nextSeq(),
      clientMsgId: "run-1",
      epoch: 5,
      payload: { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" },
    });
    await act(async () => {
      factory.last.simulateMessage(runningFrame);
    });
    await waitFor(() => expect(screen.getByTestId("stream-status-label").textContent).toMatch(/Running|运行中/));

    await user.type(screen.getByTestId("composer-input"), "desktop is offline");
    await user.click(screen.getByTestId("composer-send"));
    await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "input.send");
      if (!found) throw new Error("input.send not sent yet");
    });

    await user.click(screen.getByTestId("stream-stop-button"));
    await user.click(screen.getByTestId("stream-stop-confirm-yes"));
    await waitFor(async () => {
      const found = await findSentCommand(factory.last.sent, kRoomRaw, "control.stop");
      if (!found) throw new Error("control.stop not sent yet");
    });

    // 不带 command_id 的粗粒度拒绝——当前所有在飞指令（发送 + Stop）一起翻可重试，不是只影响其中一个。
    await act(async () => {
      factory.last.simulateMessage({ t: "error", reason: "desktop_offline" });
    });
    await waitFor(() => expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("rate_limited"));
    await waitFor(() => expect(screen.getByTestId("stream-stop-badge").getAttribute("data-status")).toBe("rate_limited"));
  });
});
