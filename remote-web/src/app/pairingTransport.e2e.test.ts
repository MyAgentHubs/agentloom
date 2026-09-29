// pairingTransport.e2e.test.ts — INT1 · 端到端：真 `PairingSession`（`pairing/pairing-session.ts`，
// 只读 import 消费，不改）经 `createRealPairingTransport()` 建立的（假）WebSocket 完整跑通
// pair.hello/accept/done/ready → `activated` → 凭据落盘 → 配对连接关闭。
//
// **不经 React 渲染**——`RealPairingHost.tsx`/`RealPairingFlow.tsx` 那层薄 React 接线（`useMemo`
// 建 transport / `dispatchRef` 转发 / `useEffect` 在 `activated` 时调 `onActivated`+`transport
// .close()`）目前**没有**被自动化测试覆盖：本仓 `vitest.config.ts` 的 `test.projects` 只给
// `src/ui/**/*.test.tsx` 配了 jsdom 环境（"ui" project），本单新增的 `src/app/**` 目录不在任何
// 现有 project 的 include glob 里——本单 SCOPE（`remote-web/src/app/**` + `main.tsx`）不含
// `vite/vitest 配置`（任务书 §3「其余一律不碰（含 vite/vitest 配置——若测试环境需要调整先停下
// 报告）」），已在报告 ⑤/⑥ 如实记录，不在此绕过硬约束自行改配置。本文件改为**直接驱动
// `PairingSession` 类本体**（不经 `usePairingSession` hook / 不经 React）验证
// `createRealPairingTransport()` 的真实承载能力——`RealPairingSessionHost.tsx` 对这里已验证过的
// `transport`/`PairingSession` 只做薄封装（`useMemo` 创建 + `dispatchRef` 单行转发 + phase 变化
// 时调用两个副作用），改动面小、逻辑已人工复核，但如实记录为残余风险，不假装等价于真做过 jsdom
// 集成测试。
//
// **假桌面独立实现**（不 import `pairing/key-wrap.ts`/`pairing/meta.ts`/`crypto/kdf.ts`）——同
// `pairing/pairing-session.test.ts` 头注纪律（避免假桌面与生产代码共享 bug、测试全绿但协议不通，
// T5d-b 的教训）。

import { describe, expect, it } from "vitest";
import { hkdfSync, randomUUID } from "node:crypto";
import { ReadyState, type WebSocketCloseInfo, type WebSocketLike } from "../connection/types.ts";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import { deriveConnectTokenHex } from "../crypto/kdf.ts";
import type { QrPayload } from "../pairing/qr-payload.ts";
import { PairingSession } from "../pairing/pairing-session.ts";
import { InMemoryKeyStore } from "../store/key-store.ts";
import { createRealPairingTransport } from "./pairingTransport.ts";

const ROOM = "0123456789abcdef0123456789abcdef";
const K_PAIR_INFO = utf8Bytes("agentloom-rc-v1");
const CONNECT_INFO = utf8Bytes("agentloom-rc-connect-v1");

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
  closeCalls: Array<{ code?: number; reason?: string }> = [];

  constructor(
    public readonly url: string,
    public readonly protocols: string[],
  ) {}

  send(data: string): void {
    if (this.readyState !== ReadyState.OPEN) {
      throw new DOMException("still in CONNECTING state", "InvalidStateError");
    }
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    this.closeCalls.push({ code, reason });
    if (this.readyState === ReadyState.CLOSED) return;
    this.readyState = ReadyState.CLOSING;
    queueMicrotask(() => {
      this.readyState = ReadyState.CLOSED;
      this.onclose?.({ code: code ?? 1000, reason: reason ?? "", wasClean: (code ?? 1000) === 1000 });
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
  factory = (url: string, protocols: string[]): WebSocketLike => {
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

/** 等待条件成立，超时报错——替代 `@testing-library`'s `waitFor()`（本文件不引入 jsdom 依赖）。 */
async function waitForCondition(check: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (!check()) {
    if (Date.now() - start > timeoutMs) {
      throw new Error("waitForCondition() timed out");
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

// ============================================================================
// 独立加密工具（node:crypto hkdfSync，不是 src/crypto/kdf.ts 的 WebCrypto subtle 路径；
// AAD/密钥包裹全部手搓，不 import pairing/meta.ts、pairing/key-wrap.ts）——照抄
// pairing-session.test.ts 头注的既有纪律。
// ============================================================================

function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}

function randomHex64(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
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

async function importAesKey(raw: Uint8Array, usages: KeyUsage[]): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", toBufferSource(raw), "AES-GCM", false, usages);
}

async function sealAadIndependent(rawKey: Uint8Array, meta: Meta, plaintext: Uint8Array): Promise<{ ct: string; n: string }> {
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

async function openAadIndependent(rawKey: Uint8Array, meta: Meta, ctB64: string, nB64: string): Promise<Uint8Array> {
  const key = await importAesKey(rawKey, ["decrypt"]);
  const plaintext = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: toBufferSource(base64Decode(nB64)),
      additionalData: toBufferSource(utf8Bytes(buildAadIndependent(meta))),
    },
    key,
    toBufferSource(base64Decode(ctB64)),
  );
  return new Uint8Array(plaintext);
}

async function wrapKeyNoAadIndependent(kek: Uint8Array, keyBytes: Uint8Array): Promise<{ ct: string; n: string }> {
  const key = await importAesKey(kek, ["encrypt"]);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ciphertext = await crypto.subtle.encrypt({ name: "AES-GCM", iv: toBufferSource(nonce) }, key, toBufferSource(keyBytes));
  return { ct: bytesToBase64(new Uint8Array(ciphertext)), n: bytesToBase64(nonce) };
}

function base64Decode(value: string): Uint8Array {
  const binary = atob(value);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

function deriveKPairIndependent(sharedSecret: Uint8Array, pairingTokenHex: string): Uint8Array {
  return new Uint8Array(hkdfSync("sha256", sharedSecret, utf8Bytes(pairingTokenHex), K_PAIR_INFO, 32));
}

function deriveConnectTokenHexIndependent(pairingTokenHex: string): string {
  const raw = new Uint8Array(hkdfSync("sha256", utf8Bytes(pairingTokenHex), new Uint8Array(0), CONNECT_INFO, 32));
  return Array.from(raw)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

async function generateDesktopX25519(): Promise<{ privateKey: CryptoKey; publicKeyBase64: string }> {
  const keyPair = await crypto.subtle.generateKey({ name: "X25519" }, true, ["deriveBits"]);
  const publicKeyBytes = new Uint8Array(await crypto.subtle.exportKey("raw", keyPair.publicKey));
  return { privateKey: keyPair.privateKey, publicKeyBase64: bytesToBase64(publicKeyBytes) };
}

async function importRemotePublicKey(base64: string): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", toBufferSource(base64Decode(base64)), { name: "X25519" }, true, []);
}

interface FakeDesktop {
  qrPayload: QrPayload;
  pairingTokenHex: string;
  verifyHello(helloFrame: Record<string, unknown>): Promise<Uint8Array>;
  buildAccept(
    kPair: Uint8Array,
    deviceId: string,
    kRoom: Uint8Array,
    capabilityTokenHex: string,
    refreshTokenHex: string,
  ): Promise<Record<string, unknown>>;
  verifyDone(kRoom: Uint8Array, deviceId: string, doneFrame: Record<string, unknown>): Promise<void>;
  buildReady(kPair: Uint8Array, deviceId: string): Promise<Record<string, unknown>>;
}

async function makeFakeDesktop(relayUrl = "wss://relay.example"): Promise<FakeDesktop> {
  const { privateKey, publicKeyBase64 } = await generateDesktopX25519();
  const pairingTokenHex = randomHex64();
  const qrPayload: QrPayload = {
    v: 1,
    relay_url: relayUrl,
    room: ROOM,
    pairing_token: pairingTokenHex,
    desktop_pub: publicKeyBase64,
  };

  return {
    qrPayload,
    pairingTokenHex,

    async verifyHello(helloFrame) {
      const remotePub = String(helloFrame.remote_pub);
      const remotePublicKey = await importRemotePublicKey(remotePub);
      const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "X25519", public: remotePublicKey }, privateKey, 256));
      const kPair = deriveKPairIndependent(shared, pairingTokenHex);
      const helloMeta: Meta = { v: 1, room: ROOM, epoch: 0, kind: "control", session: null, command_id: null };
      const plaintext = await openAadIndependent(kPair, helloMeta, String(helloFrame.token_ct), String(helloFrame.token_n));
      if (new TextDecoder().decode(plaintext) !== pairingTokenHex) {
        throw new Error("hello token proof mismatch");
      }
      return kPair;
    },

    async buildAccept(kPair, deviceId, kRoom, capabilityTokenHex, refreshTokenHex) {
      const { ct: kRoomCt, n: kRoomN } = await wrapKeyNoAadIndependent(kPair, kRoom);
      const tokensMeta: Meta = { v: 1, room: ROOM, epoch: 0, kind: "pair-accept-tokens", session: deviceId, command_id: null };
      const tokensPlain = utf8Bytes(JSON.stringify({ capability_token: capabilityTokenHex, refresh_token: refreshTokenHex }));
      const { ct: tokensCt, n: tokensN } = await sealAadIndependent(kPair, tokensMeta, tokensPlain);
      return {
        t: "pair.accept",
        room: ROOM,
        device_id: deviceId,
        k_room_ct: kRoomCt,
        k_room_n: kRoomN,
        tokens_ct: tokensCt,
        tokens_n: tokensN,
      };
    },

    async verifyDone(kRoom, deviceId, doneFrame) {
      const confirmMeta: Meta = { v: 1, room: ROOM, epoch: 0, kind: "pair-confirm", session: deviceId, command_id: null };
      const plaintext = await openAadIndependent(kRoom, confirmMeta, String(doneFrame.confirm_ct), String(doneFrame.confirm_n));
      if (new TextDecoder().decode(plaintext) !== deviceId) {
        throw new Error("done confirm mismatch");
      }
    },

    async buildReady(kPair, deviceId) {
      const readyMeta: Meta = { v: 1, room: ROOM, epoch: 0, kind: "pair-ready", session: deviceId, command_id: null };
      const { ct, n } = await sealAadIndependent(kPair, readyMeta, utf8Bytes(deviceId));
      return { t: "pair.ready", room: ROOM, device_id: deviceId, ct, n };
    },
  };
}

describe("createRealPairingTransport() + PairingSession · 端到端配对（真 WebSocket 帧序，独立假桌面）", () => {
  it("QR -> hello -> accept -> done -> ready -> activated：凭据落盘 + 配对连接关闭", async () => {
    const desktop = await makeFakeDesktop();
    const factory = new FakeWebSocketFactory();
    const keyStore = new InMemoryKeyStore();

    const transport = createRealPairingTransport(desktop.qrPayload, factory.factory, {
      onFrame: (raw) => {
        void session.handleFrame(raw);
      },
    });
    const session = new PairingSession(desktop.qrPayload, transport, keyStore);

    await waitForCondition(() => factory.sockets.length === 1);
    expect(factory.last.url).toBe(`wss://relay.example/room/${ROOM}`);
    const expectedToken = deriveConnectTokenHexIndependent(desktop.pairingTokenHex);
    expect(factory.last.protocols).toEqual(["agentloom-rc-v1", `token.${expectedToken}`]);
    // 双重交叉验证：production 的 deriveConnectTokenHex()（WebCrypto 路径）与本文件独立实现
    // （node:crypto hkdfSync 路径）必须算出同一个值。
    expect(await deriveConnectTokenHex(desktop.pairingTokenHex)).toBe(expectedToken);

    await session.start(); // 此刻 socket 仍 CONNECTING——pair.hello 应该被 transport 缓冲住
    expect(factory.last.sent).toHaveLength(0);
    expect(session.state).toBe("awaiting_accept");

    factory.last.simulateOpen();
    await waitForCondition(() => factory.last.sent.length === 1);
    const helloFrame = JSON.parse(factory.last.sent[0]!) as Record<string, unknown>;
    expect(helloFrame.t).toBe("pair.hello");
    expect(helloFrame.room).toBe(ROOM);

    const kPair = await desktop.verifyHello(helloFrame);

    const deviceId = randomUUID();
    const kRoom = new Uint8Array(32);
    crypto.getRandomValues(kRoom);
    const capabilityTokenHex = randomHex64();
    const refreshTokenHex = randomHex64();
    const acceptFrame = await desktop.buildAccept(kPair, deviceId, kRoom, capabilityTokenHex, refreshTokenHex);

    factory.last.simulateMessage(acceptFrame);
    await waitForCondition(() => session.state === "awaiting_ready");
    expect(factory.last.sent).toHaveLength(2);
    const doneFrame = JSON.parse(factory.last.sent[1]!) as Record<string, unknown>;
    expect(doneFrame.t).toBe("pair.done");
    await desktop.verifyDone(kRoom, deviceId, doneFrame); // 抛异常即测试失败——AEAD 认证必须真的过

    const readyFrame = await desktop.buildReady(kPair, deviceId);
    factory.last.simulateMessage(readyFrame);
    await waitForCondition(() => session.state === "activated");

    const stored = await keyStore.loadKeys();
    expect(stored?.deviceId).toBe(deviceId);
    expect(stored?.room).toBe(ROOM);
    expect(stored?.access).toBe(capabilityTokenHex);
    expect(stored?.refresh).toBe(refreshTokenHex);
    expect(stored?.kPair).toEqual(kPair);
    expect(stored?.kRoomKey).toBeDefined();
    expect(stored!.kRoomKey.extractable).toBe(false);

    // 落盘的 kRoomKey 确实是同一把 K_room——用它加密一段探针明文，独立导入原始 kRoom 字节解出来
    // 必须一致（同 store/key-store.test.ts 的验证姿势）。
    const probeNonce = new Uint8Array(12);
    crypto.getRandomValues(probeNonce);
    const probeCt = await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: toBufferSource(probeNonce) },
      stored!.kRoomKey,
      toBufferSource(utf8Bytes("probe")),
    );
    const independentKRoomKey = await importAesKey(kRoom, ["decrypt"]);
    const probePlain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: toBufferSource(probeNonce) },
      independentKRoomKey,
      probeCt,
    );
    expect(new TextDecoder().decode(probePlain)).toBe("probe");

    // 「配对完成 → 关配对连接」——本单在此手动模拟 RealPairingSessionHost 的 activated 副作用
    // （它做的正是这一行）：
    transport.close();
    await waitForCondition(() => factory.last.closeCalls.length === 1);
    expect(factory.last.closeCalls).toEqual([{ code: 1000, reason: "pairing_done" }]);
  });

  it("篡改的 pair.accept 密文（AEAD 认证失败）不会让协议往前走，也不会发出 pair.done", async () => {
    const desktop = await makeFakeDesktop();
    const factory = new FakeWebSocketFactory();
    const keyStore = new InMemoryKeyStore();

    const transport = createRealPairingTransport(desktop.qrPayload, factory.factory, {
      onFrame: (raw) => {
        void session.handleFrame(raw);
      },
    });
    const session = new PairingSession(desktop.qrPayload, transport, keyStore);

    await waitForCondition(() => factory.sockets.length === 1);
    await session.start();
    factory.last.simulateOpen();
    await waitForCondition(() => factory.last.sent.length === 1);
    const helloFrame = JSON.parse(factory.last.sent[0]!) as Record<string, unknown>;
    const kPair = await desktop.verifyHello(helloFrame);

    const deviceId = randomUUID();
    const kRoom = new Uint8Array(32);
    crypto.getRandomValues(kRoom);
    const acceptFrame = await desktop.buildAccept(kPair, deviceId, kRoom, randomHex64(), randomHex64());
    const tamperedCt = base64Decode(String(acceptFrame.k_room_ct));
    tamperedCt[0] = tamperedCt[0]! ^ 0xff;
    acceptFrame.k_room_ct = bytesToBase64(tamperedCt);

    const outcome = await session.handleFrame(acceptFrame);
    expect(outcome).toEqual({ status: "rejected", reason: "decrypt_failed" });
    expect(session.state).toBe("awaiting_accept"); // 没有前进
    expect(factory.last.sent).toHaveLength(1); // 仍只有 pair.hello，没有多发 pair.done
  });
});
