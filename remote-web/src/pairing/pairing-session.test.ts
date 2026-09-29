// pairing-session.test.ts — TDD 覆盖 src/pairing/pairing-session.ts。
//
// **假桌面参考实现故意手搓**（不 import 生产代码里的 key-wrap.ts/meta.ts）——独立复算 K_pair
// （HKDF 走 node:crypto `hkdfSync`，不是 WebCrypto subtle 那条路径）、独立拼 AAD 字符串、独立实现
// 无 AAD 的密钥包裹。理由（同 remote-relay/test/s1ja-fake-mobile-e2e.test.js 头注）：如果假桌面复
// 用 PairingSession 自己内部调用的同一份 meta.ts/key-wrap.ts，两边共享的 bug 会互相抵消、测试全绿
// 但协议其实对不上真实桌面——这正是 T5d-b 的教训（"6 组测试全绿但 control 通道端到端 0% 可用"）。
// 假桌面的握手步骤照抄 s1ja 的 `pairDevice()`（remote-relay/test/s1ja-fake-mobile-e2e.test.js:
// 300-459），只把 remote_pub/desktop_pub 编码从 hex 改成 base64（见 qr-payload.ts/frames.ts 头注
// ⑥ 偏离说明）。
//
// ============================================================================
// Coverage matrix: state x frame -> behavior [test group].
// ============================================================================
// idle            x pair.accept -> rejected(wrong_phase) [out-of-order frames]
// idle            x pair.ready -> rejected(wrong_phase) [out-of-order frames]
// awaiting_accept x pair.hello(outbound) -> covered by start() asserting transport.sent[0] [golden path]
// awaiting_accept x pair.ready -> rejected(wrong_phase) [out-of-order frames]
// awaiting_accept x pair.accept(valid) -> applied -> awaiting_ready [golden path]
// awaiting_accept x pair.accept(tampered) -> rejected(decrypt_failed) [tampering]
// awaiting_accept x pair.accept(missing fields) -> rejected(malformed_frame) [malformed frames]
// awaiting_accept x pair.accept(duplicate) -> rejected(wrong_phase); second call is in awaiting_ready [out-of-order frames]
// awaiting_ready  x pair.done(outbound) + resendDone() idempotent replay -> applied twice, identical bytes [idempotent replay]
// awaiting_ready  x pair.ready(valid) -> applied -> activated + KeyStore persistence [golden path]
// awaiting_ready  x pair.ready(device_id field mismatch) -> rejected(device_id_mismatch) [tampering]
// awaiting_ready  x pair.ready(tampered AAD/ciphertext) -> rejected(decrypt_failed) [tampering]
// awaiting_ready  x pair.ready(plaintext mismatches device_id, valid AAD) -> rejected(device_id_mismatch) [tampering]
// activated       x pair.ready(valid duplicate after done replay) -> applied, idempotent, no new write [idempotent replay]
// activated       x error{reason:"device_revoked"} -> applied -> needs_repair, credentials retained [revocation]
// needs_repair    x error{reason:"device_revoked"}(duplicate) -> ignored, terminal state is idempotent [revocation]
// awaiting_accept x error{reason:"desktop_offline"} -> applied -> desktop_offline [offline]
// desktop_offline x pair.accept(valid after retry) -> applied -> awaiting_ready [offline]
// awaiting_ready  x error{reason:"desktop_offline"} -> applied -> desktop_offline [offline]
// any state       x error{reason:other} -> ignored [revocation]
// any state       x unknown t / non-object raw -> ignored / rejected(malformed_frame), no crash [malformed frames]
// awaiting_ready  x two concurrent pair.ready(valid) -> exactly one write, one saveKeys call [concurrent write protection]
// awaiting_ready(write in flight) x device_revoked -> eventually needs_repair, revocation wins over late persistence [concurrent write protection]
//
// ============================================================================
// Mutation checks: five manual production-code mutations were verified to make these tests fail:
// change hello meta kind; change accept AAD kind; skip the ready barrier; remove unwrapKey's
// 32-byte output guard; remove concurrent activation serialization. All mutations were reverted.
// The assertions retained here verify the correct implementation, not the mutation experiments.
// ============================================================================

// node:crypto 的 hkdfSync/randomUUID 类型声明见同目录 node-crypto.test-support.d.ts（项目未装
// @types/node，任务书依赖清单只许新增 fake-indexeddb devDep）。

import { describe, expect, it } from "vitest";
import { hkdfSync, randomUUID } from "node:crypto";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import { InMemoryKeyStore, type KeyStorePort, type StoredPairingCredentials } from "../store/key-store.ts";
import type { OutboundPairingFrame } from "./frames.ts";
import type { QrPayload } from "./qr-payload.ts";
import { PairingSession, type PairingFrameOutcome, type PairingTransportPort } from "./pairing-session.ts";

const ROOM = "0123456789abcdef0123456789abcdef";
const K_PAIR_INFO = utf8Bytes("agentloom-rc-v1");
const CONNECT_INFO = utf8Bytes("agentloom-rc-connect-v1");

// ============================================================================
// 独立工具（不 import 生产 crypto 帮助函数，除了 x25519.ts/kdf.ts/envelope.ts 已在 T6a 被 KAT 单独
// 验证过的底层原语——那些不是本单要证明的对象；这里手搓的是"桌面怎么用它们"这条逻辑本身）。
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

/** 独立 HKDF（node:crypto `hkdfSync`，与 src/crypto/kdf.ts 的 WebCrypto subtle 路径完全不同的实现）。 */
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

// ============================================================================
// 测试传输 port——记录发出的帧，不做真 WebSocket。
// ============================================================================

class RecordingTransport implements PairingTransportPort {
  readonly sent: Record<string, unknown>[] = [];
  send(frame: OutboundPairingFrame): void {
    this.sent.push(frame as unknown as Record<string, unknown>);
  }
  get last(): Record<string, unknown> {
    const frame = this.sent.at(-1);
    if (!frame) throw new Error("no frame sent yet");
    return frame;
  }
}

class CountingKeyStore extends InMemoryKeyStore {
  saveCount = 0;
  override async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    this.saveCount += 1;
    await super.saveKeys(creds);
  }
}

/**
 * KeyStore test double for real concurrency (pairing-session.ts:219): `saveKeys` waits
 * on an externally controlled promise until the test calls `releaseAll()`.
 * Unlike sequentially awaited calls, both `session.handleFrame(readyFrame)` calls start
 * without awaiting either to completion, allowing both to reach the persistence boundary.
 * Multiple `saveKeys` calls can suspend together; the old code issued two, each awaiting release.
 */
class DeferredKeyStore implements KeyStorePort {
  saveCount = 0;
  readonly saveCalls: StoredPairingCredentials[] = [];
  private record: StoredPairingCredentials | null = null;
  private releasers: Array<() => void> = [];

  async saveKeys(creds: StoredPairingCredentials): Promise<void> {
    this.saveCount += 1;
    this.saveCalls.push(creds);
    await new Promise<void>((resolve) => {
      this.releasers.push(resolve);
    });
    this.record = creds;
  }

  /** 放行当前所有已经在等待的 saveKeys 调用。 */
  releaseAll(): void {
    const pending = this.releasers.splice(0);
    for (const release of pending) release();
  }

  async loadKeys(): Promise<StoredPairingCredentials | null> {
    return this.record;
  }

  async clear(): Promise<void> {
    this.record = null;
  }
}

/** 真正让待处理的微任务/宏任务队列跑一轮——比固定次数的 `Promise.resolve()` 链更贴近"等所有已经
 *  在飞行中的异步工作（包括 WebCrypto 的 AES-GCM 解密，走的是原生实现，不保证只需一个微任务）都
 *  有机会推进"。用于并发测试里"确保两条 handleFrame 调用都已经跑到 saveKeys 内部挂起"这一步，
 *  不依赖脆弱的精确计数。 */
function flushAsync(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

// ============================================================================
// 假桌面 actor——照 s1ja pairDevice() 复刻。
// ============================================================================

interface FakeDesktop {
  qrPayload: QrPayload;
  pairingTokenHex: string;
  desktopPrivateKey: CryptoKey;
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

async function makeFakeDesktop(): Promise<FakeDesktop> {
  const { privateKey, publicKeyBase64 } = await generateDesktopX25519();
  const pairingTokenHex = randomHex64();
  const qrPayload: QrPayload = {
    v: 1,
    relay_url: "wss://relay.example",
    room: ROOM,
    pairing_token: pairingTokenHex,
    desktop_pub: publicKeyBase64,
  };

  return {
    qrPayload,
    pairingTokenHex,
    desktopPrivateKey: privateKey,

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

/** 走完 hello→accept→done 三步，停在 awaiting_ready（供多条测试复用，避免每条重敲一遍）。 */
async function pairUpToAwaitingReady() {
  return pairUpToAwaitingReadyWith(new CountingKeyStore());
}

/** 同上，但可以注入任意 `KeyStorePort` 实现——并发测试需要换成 `DeferredKeyStore`。 */
async function pairUpToAwaitingReadyWith<K extends KeyStorePort>(keyStore: K) {
  const desktop = await makeFakeDesktop();
  const transport = new RecordingTransport();
  const session = new PairingSession(desktop.qrPayload, transport, keyStore);

  await session.start();
  const kPair = await desktop.verifyHello(transport.last);

  const deviceId = randomUUID();
  const kRoom = new Uint8Array(32);
  crypto.getRandomValues(kRoom);
  const capabilityTokenHex = randomHex64();
  const refreshTokenHex = randomHex64();
  const acceptFrame = await desktop.buildAccept(kPair, deviceId, kRoom, capabilityTokenHex, refreshTokenHex);

  const outcome = await session.handleFrame(acceptFrame);
  return { desktop, transport, keyStore, session, kPair, deviceId, kRoom, capabilityTokenHex, refreshTokenHex, acceptOutcome: outcome };
}

// ============================================================================
// Golden path
// ============================================================================

describe("PairingSession · golden path (M0 §9.5 扫码→pairing→ready→activated)", () => {
  it("drives the full frame sequence and persists credentials only after the ready barrier", async () => {
    const desktop = await makeFakeDesktop();
    const transport = new RecordingTransport();
    const keyStore = new InMemoryKeyStore();
    const session = new PairingSession(desktop.qrPayload, transport, keyStore);

    expect(session.state).toBe("idle");
    await session.start();
    expect(session.state).toBe("awaiting_accept");
    expect(session.connectTokenHex).toBe(deriveConnectTokenHexIndependent(desktop.pairingTokenHex));

    const helloFrame = transport.last;
    expect(helloFrame.t).toBe("pair.hello");
    expect(helloFrame.room).toBe(ROOM);
    const kPair = await desktop.verifyHello(helloFrame);

    const deviceId = randomUUID();
    const kRoom = new Uint8Array(32);
    crypto.getRandomValues(kRoom);
    const capabilityTokenHex = randomHex64();
    const refreshTokenHex = randomHex64();
    const acceptFrame = await desktop.buildAccept(kPair, deviceId, kRoom, capabilityTokenHex, refreshTokenHex);

    const acceptOutcome = await session.handleFrame(acceptFrame);
    expect(acceptOutcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("awaiting_ready");
    expect(session.pairedDeviceId).toBe(deviceId);

    // 激活屏障（mutation ③ 的正面断言）：accept 处理完、ready 还没到——凭据必须还没落盘。
    expect(await keyStore.loadKeys()).toBeNull();

    const doneFrame = transport.last;
    expect(doneFrame.t).toBe("pair.done");
    expect(doneFrame.device_id).toBe(deviceId);
    await desktop.verifyDone(kRoom, deviceId, doneFrame);

    const readyFrame = await desktop.buildReady(kPair, deviceId);
    const readyOutcome = await session.handleFrame(readyFrame);
    expect(readyOutcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("activated");

    const stored = await keyStore.loadKeys();
    expect(stored).not.toBeNull();
    expect(stored!.deviceId).toBe(deviceId);
    expect(stored!.room).toBe(ROOM);
    expect(stored!.relayUrl).toBe(desktop.qrPayload.relay_url);
    expect(stored!.access).toBe(capabilityTokenHex);
    expect(stored!.refresh).toBe(refreshTokenHex);
    expect(stored!.kRoomKey.extractable).toBe(false);

    // K_room CryptoKey 落储的确实是桌面生成的那把——用独立导入的同一份原始字节交叉验证。
    const nonce = new Uint8Array(12);
    const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv: toBufferSource(nonce) }, stored!.kRoomKey, toBufferSource(utf8Bytes("probe")));
    const referenceKey = await importAesKey(kRoom, ["decrypt"]);
    const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: toBufferSource(nonce) }, referenceKey, ct);
    expect(new TextDecoder().decode(pt)).toBe("probe");
  });

  // T6b2 补刀：persistActivation 落储必须带 kPair（refreshFrames.ts:64 的 refresh 请求/回执密文体
  // 用 K_pair，不是 K_room——跨页面重载后若 kPair 没落盘，refresh 必死）。变异自证：把
  // pairing-session.ts::persistActivation 里 `saveKeys({...})` 的 `kPair,` 那行删掉，本条测试转红
  // （`stored!.kPair` 变成 `undefined`，`toEqual` 断言失败）。
  it("persists kPair byte-for-byte identical to the pairing-derived K_pair (跨重载 refresh 打通)", async () => {
    const { desktop, session, keyStore, kPair, deviceId } = await pairUpToAwaitingReady();
    const readyFrame = await desktop.buildReady(kPair, deviceId);
    expect(await session.handleFrame(readyFrame)).toEqual<PairingFrameOutcome>({ status: "applied" });

    const stored = await keyStore.loadKeys();
    expect(stored).not.toBeNull();
    expect(stored!.kPair).toEqual(kPair);
  });

  // FIX2 P0-1 第一环：persistActivation 落储必须带 accessIssuedAtMs（本地过期钟的落盘起点——
  // `AppRuntime.tsx` 冷启动读到这个字段，若缺失会被消费侧的保守兜底当"已过期"，白白多刷一次
  // refresh；反过来，落了这个字段就该是"此刻"，不是某个更早/更晚的时间戳）。变异自证：把
  // pairing-session.ts::persistActivation 里 `accessIssuedAtMs: Date.now(),` 那行删掉，本条测试
  // 转红（`stored!.accessIssuedAtMs` 变成 `undefined`，`toBeGreaterThanOrEqual`/`toBeLessThanOrEqual`
  // 断言失败）。
  it("persists accessIssuedAtMs as the current clock time at the moment pair.ready is accepted (P0-1 冷启动过期钟起点)", async () => {
    const before = Date.now();
    const { desktop, session, keyStore, kPair, deviceId } = await pairUpToAwaitingReady();
    const readyFrame = await desktop.buildReady(kPair, deviceId);
    expect(await session.handleFrame(readyFrame)).toEqual<PairingFrameOutcome>({ status: "applied" });
    const after = Date.now();

    const stored = await keyStore.loadKeys();
    expect(stored).not.toBeNull();
    expect(typeof stored!.accessIssuedAtMs).toBe("number");
    expect(stored!.accessIssuedAtMs!).toBeGreaterThanOrEqual(before);
    expect(stored!.accessIssuedAtMs!).toBeLessThanOrEqual(after);
  });

  it("connectTokenHex is a 64-char lowercase hex string derived independently by both sides", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    await session.start();
    expect(session.connectTokenHex).toMatch(/^[0-9a-f]{64}$/);
  });
});

// ============================================================================
// done 幂等重放（M0 §9.5：「ready 丢失可重发 done 重得 ready」）
// ============================================================================

describe("PairingSession · pair.done 幂等重放", () => {
  it("resendDone() re-sends byte-identical confirm ct/n while still awaiting_ready", async () => {
    const { transport, session } = await pairUpToAwaitingReady();
    const firstDone = transport.last;
    const outcome = session.resendDone();
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    const secondDone = transport.last;
    expect(secondDone).not.toBe(firstDone);
    expect(secondDone.confirm_ct).toBe(firstDone.confirm_ct);
    expect(secondDone.confirm_n).toBe(firstDone.confirm_n);
    expect(secondDone.device_id).toBe(firstDone.device_id);
  });

  it("resendDone() from any other phase is rejected(wrong_phase), not a crash", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    expect(session.resendDone()).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "wrong_phase" });
  });

  it("a duplicate pair.ready received after already activated (triggered by a done resend racing a slow first ready) is idempotent: applied, no re-save", async () => {
    const { desktop, session, keyStore, kPair, deviceId } = await pairUpToAwaitingReady();
    const readyFrame1 = await desktop.buildReady(kPair, deviceId);
    expect(await session.handleFrame(readyFrame1)).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("activated");
    expect(keyStore.saveCount).toBe(1);

    const readyFrame2 = await desktop.buildReady(kPair, deviceId); // 新密文（新 nonce），语义等价的第二份 ready
    expect(await session.handleFrame(readyFrame2)).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("activated");
    expect(keyStore.saveCount).toBe(1); // 没有第二次落盘
  });
});

// ============================================================================
// Prevent duplicate writes for concurrent ready frames (pairing-session.ts:219).
// ============================================================================
// The duplicate-ready test above awaits each call: the first resolves and reaches activated
// before the second starts, hiding the race where both handleFrame calls see awaiting_ready
// and independently call keyStore.saveKeys. DeferredKeyStore holds persistence pending here
// while both handleFrame calls run; the second starts without awaiting the first.

describe("PairingSession · 并发重复 ready 防双写（真并发，非顺序 await 假并发）", () => {
  it("two ready frames delivered without awaiting the first before starting the second still save exactly once", async () => {
    const deferredStore = new DeferredKeyStore();
    const { desktop, session, kPair, deviceId } = await pairUpToAwaitingReadyWith(deferredStore);

    const readyFrame1 = await desktop.buildReady(kPair, deviceId);
    const readyFrame2 = await desktop.buildReady(kPair, deviceId); // 独立密文（新 nonce），同一 device_id，语义等价的第二份 ready

    // 关键：不 await 第一个就发第二个——制造真并发，不是顺序 await 的假并发。
    const p1 = session.handleFrame(readyFrame1);
    const p2 = session.handleFrame(readyFrame2);

    // 让两条帧各自的解密/校验（只读操作）都有机会跑完、推进到"准备落盘"这一步——不管旧实现（会
    // 各自独立调用 saveKeys）还是新实现（只有一个真正调用 saveKeys，另一个原样 await 同一个
    // promise），此时该发生的调用都应该已经发生、卡在 DeferredKeyStore 内部的 pending promise 上。
    await flushAsync();
    await flushAsync();
    deferredStore.releaseAll();

    const [outcome1, outcome2] = await Promise.all([p1, p2]);
    expect(outcome1).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(outcome2).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(deferredStore.saveCount).toBe(1);
    expect(session.state).toBe("activated");

    const stored = await deferredStore.loadKeys();
    expect(stored?.deviceId).toBe(deviceId);
  });

  it("device_revoked arriving while an activation save is in flight still ends in needs_repair (revocation wins the race)", async () => {
    const deferredStore = new DeferredKeyStore();
    const { desktop, session, kPair, deviceId } = await pairUpToAwaitingReadyWith(deferredStore);
    const readyFrame = await desktop.buildReady(kPair, deviceId);

    const readyPromise = session.handleFrame(readyFrame);
    // 让 handleReady 推进到 saveKeys 内部挂起（此时 phase 仍是 awaiting_ready——还没轮到
    // persistActivation 的 await 之后那行把它翻成 activated）。
    await flushAsync();
    await flushAsync();
    expect(session.state).toBe("awaiting_ready");

    const revokedOutcome = await session.handleFrame({ t: "error", reason: "device_revoked" });
    expect(revokedOutcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("needs_repair");

    deferredStore.releaseAll();
    const readyOutcome = await readyPromise;

    // ready 帧本身仍然"applied"（凭据确实落盘了，帧处理没有失败）；但会话的最终状态是撤销赢——
    // 不能因为落盘完成得晚，就把 needs_repair 覆盖回 activated（M0 §9.6/v0.3.1：明文撤销信号不
    // 清凭据，但也不该让一次滞后的落盘把"已撤销"悄悄抹掉）。
    expect(readyOutcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("needs_repair");
    expect(session.revocationReason).toBe("device_revoked");

    const stored = await deferredStore.loadKeys();
    expect(stored).not.toBeNull(); // 凭据仍然落盘了——v0.3.1「不自动清凭据」
  });
});

// ============================================================================
// 乱序/异形帧——不崩溃，按语义拒绝或忽略
// ============================================================================

describe("PairingSession · 乱序帧（wrong_phase，不崩溃）", () => {
  it("pair.accept before start() (idle phase) is rejected", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    const outcome = await session.handleFrame({ t: "pair.accept", room: ROOM, device_id: "x" });
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "wrong_phase" });
  });

  it("pair.ready before start() (idle phase) is rejected", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    const outcome = await session.handleFrame({ t: "pair.ready", room: ROOM, device_id: "x", ct: "a", n: "b" });
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "wrong_phase" });
  });

  it("pair.ready arriving before pair.accept (awaiting_accept phase) is rejected", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    await session.start();
    const outcome = await session.handleFrame({ t: "pair.ready", room: ROOM, device_id: "x", ct: "a", n: "b" });
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "wrong_phase" });
  });

  it("a second pair.accept after the first already advanced to awaiting_ready is rejected", async () => {
    const { desktop, session, kPair, deviceId, kRoom, capabilityTokenHex, refreshTokenHex, acceptOutcome } =
      await pairUpToAwaitingReady();
    expect(acceptOutcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    const secondAccept = await desktop.buildAccept(kPair, deviceId, kRoom, capabilityTokenHex, refreshTokenHex);
    const outcome = await session.handleFrame(secondAccept);
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "wrong_phase" });
  });
});

describe("PairingSession · 畸形帧（不崩溃）", () => {
  it("non-object raw input is rejected(malformed_frame)", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    await session.start();
    for (const raw of [null, undefined, "a string", 42, ["array"]]) {
      const outcome = await session.handleFrame(raw);
      expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "malformed_frame" });
    }
  });

  it("frame missing t is rejected(malformed_frame)", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    await session.start();
    expect(await session.handleFrame({ room: ROOM })).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "malformed_frame" });
  });

  it("unknown frame type is ignored, not rejected", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    await session.start();
    const outcome = await session.handleFrame({ t: "presence", online: true });
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "ignored", reason: 'unhandled frame type "presence"' });
    expect(session.state).toBe("awaiting_accept"); // 没有被未知帧打乱状态
  });

  it("pair.accept missing tokens_ct is rejected(malformed_frame), state/store/outbound frames all unchanged", async () => {
    const desktop = await makeFakeDesktop();
    const transport = new RecordingTransport();
    const keyStore = new InMemoryKeyStore();
    const session = new PairingSession(desktop.qrPayload, transport, keyStore);
    await session.start();
    const kPair = await desktop.verifyHello(transport.last);
    const fullAccept = await desktop.buildAccept(kPair, randomUUID(), new Uint8Array(32), randomHex64(), randomHex64());
    delete fullAccept.tokens_ct;
    const sentBeforeCount = transport.sent.length;

    const outcome = await session.handleFrame(fullAccept);

    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "malformed_frame" });
    expect(session.state).toBe("awaiting_accept"); // 拒绝不推进状态
    expect(session.pairedDeviceId).toBeNull();
    expect(await keyStore.loadKeys()).toBeNull(); // 没有任何落盘
    expect(transport.sent.length).toBe(sentBeforeCount); // 没有发出 pair.done
  });

  it("pair.accept with room mismatch is rejected(malformed_frame), state/store/outbound frames all unchanged", async () => {
    const desktop = await makeFakeDesktop();
    const transport = new RecordingTransport();
    const keyStore = new InMemoryKeyStore();
    const session = new PairingSession(desktop.qrPayload, transport, keyStore);
    await session.start();
    const kPair = await desktop.verifyHello(transport.last);
    const accept = await desktop.buildAccept(kPair, randomUUID(), new Uint8Array(32), randomHex64(), randomHex64());
    accept.room = "ffffffffffffffffffffffffffffffff";
    const sentBeforeCount = transport.sent.length;

    const outcome = await session.handleFrame(accept);

    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "malformed_frame" });
    expect(session.state).toBe("awaiting_accept");
    expect(session.pairedDeviceId).toBeNull();
    expect(await keyStore.loadKeys()).toBeNull();
    expect(transport.sent.length).toBe(sentBeforeCount);
  });
});

describe("PairingSession · 篡改帧（AEAD 认证必须挡住）", () => {
  it("pair.accept with a bit-flipped k_room_ct fails to decrypt", async () => {
    const desktop = await makeFakeDesktop();
    const transport = new RecordingTransport();
    const session = new PairingSession(desktop.qrPayload, transport, new InMemoryKeyStore());
    await session.start();
    const kPair = await desktop.verifyHello(transport.last);
    const accept = await desktop.buildAccept(kPair, randomUUID(), new Uint8Array(32), randomHex64(), randomHex64());
    const bytes = base64Decode(String(accept.k_room_ct));
    bytes[0] ^= 0xff;
    accept.k_room_ct = bytesToBase64(bytes);
    const outcome = await session.handleFrame(accept);
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "decrypt_failed" });
  });

  it("pair.accept with a bit-flipped tokens_ct fails to decrypt", async () => {
    const desktop = await makeFakeDesktop();
    const transport = new RecordingTransport();
    const session = new PairingSession(desktop.qrPayload, transport, new InMemoryKeyStore());
    await session.start();
    const kPair = await desktop.verifyHello(transport.last);
    const accept = await desktop.buildAccept(kPair, randomUUID(), new Uint8Array(32), randomHex64(), randomHex64());
    const bytes = base64Decode(String(accept.tokens_ct));
    bytes[0] ^= 0xff;
    accept.tokens_ct = bytesToBase64(bytes);
    const outcome = await session.handleFrame(accept);
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "decrypt_failed" });
  });

  // Exercise key-wrap.ts's 32-byte guard with independently encrypted no-AAD k_room_ct.
  // Production wrapKey rejects invalid lengths, so raw encryption supplies non-32-byte plaintext
  // that passes AEAD authentication with a valid key/nonce. key-wrap.ts must reject this result:
  // an invalid-length K_room must never escape unwrapKey and reach the downstream seal() call
  // outside handleAccept's try block, where it would cause an uncaught exception.
  it.each([16, 31, 33])(
    "pair.accept whose k_room_ct AEAD-decrypts fine but to a %dB (not 32B) plaintext is rejected(decrypt_failed), state/store/outbound frames all unchanged",
    async (wrongLength) => {
      const desktop = await makeFakeDesktop();
      const transport = new RecordingTransport();
      const keyStore = new InMemoryKeyStore();
      const session = new PairingSession(desktop.qrPayload, transport, keyStore);
      await session.start();
      const kPair = await desktop.verifyHello(transport.last);
      const deviceId = randomUUID();
      const accept = await desktop.buildAccept(kPair, deviceId, new Uint8Array(32), randomHex64(), randomHex64());
      const { ct: wrongLengthCt, n: wrongLengthN } = await wrapKeyNoAadIndependent(kPair, new Uint8Array(wrongLength).fill(3));
      accept.k_room_ct = wrongLengthCt;
      accept.k_room_n = wrongLengthN;

      const sentBeforeCount = transport.sent.length; // 只有 pair.hello，1 帧

      const outcome = await session.handleFrame(accept);

      expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "decrypt_failed" });
      expect(session.state).toBe("awaiting_accept"); // 没有推进到 awaiting_ready
      expect(session.pairedDeviceId).toBeNull(); // deviceId 没有被提前采纳
      expect(await keyStore.loadKeys()).toBeNull(); // 没有任何落盘
      expect(transport.sent.length).toBe(sentBeforeCount); // 没有发出 pair.done
    },
  );

  it("pair.ready whose device_id field doesn't match the paired device is rejected(device_id_mismatch)", async () => {
    const { desktop, session, kPair } = await pairUpToAwaitingReady();
    const readyFrame = await desktop.buildReady(kPair, randomUUID()); // 另一个 device_id
    const outcome = await session.handleFrame(readyFrame);
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "device_id_mismatch" });
  });

  it("pair.ready with tampered ciphertext fails to decrypt", async () => {
    const { desktop, session, kPair, deviceId } = await pairUpToAwaitingReady();
    const readyFrame = await desktop.buildReady(kPair, deviceId);
    const bytes = base64Decode(String(readyFrame.ct));
    bytes[0] ^= 0xff;
    readyFrame.ct = bytesToBase64(bytes);
    const outcome = await session.handleFrame(readyFrame);
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "decrypt_failed" });
  });

  it("pair.ready whose decrypted plaintext isn't the device_id (AAD correct, content wrong) is rejected(device_id_mismatch)", async () => {
    const { session, kPair, deviceId } = await pairUpToAwaitingReady();
    // 正确 AAD/kind，但明文塞的是别的字符串——模拟一个实现有 bug 的桌面（R2 修正前的旧 bug 类）。
    const readyMeta: Meta = { v: 1, room: ROOM, epoch: 0, kind: "pair-ready", session: deviceId, command_id: null };
    const { ct, n } = await sealAadIndependent(kPair, readyMeta, utf8Bytes("not-the-device-id"));
    const outcome = await session.handleFrame({ t: "pair.ready", room: ROOM, device_id: deviceId, ct, n });
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "rejected", reason: "device_id_mismatch" });
  });
});

// ============================================================================
// 失效 → 重配对终态（device_revoked，明文提示不清凭据）
// ============================================================================

describe("PairingSession · device_revoked → needs_repair（不清凭据）", () => {
  it("error{reason:device_revoked} after activation transitions to needs_repair without clearing KeyStore", async () => {
    const { desktop, session, keyStore, kPair, deviceId } = await pairUpToAwaitingReady();
    const readyFrame = await desktop.buildReady(kPair, deviceId);
    await session.handleFrame(readyFrame);
    expect(session.state).toBe("activated");

    const outcome = await session.handleFrame({ t: "error", reason: "device_revoked" });
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("needs_repair");
    expect(session.revocationReason).toBe("device_revoked");

    // v0.3.1 收紧：relay 的明文提示不可信，只改 UI 文案，不自动清凭据。
    const stored = await keyStore.loadKeys();
    expect(stored).not.toBeNull();
    expect(stored!.access).toBeTruthy();
  });

  it("device_revoked repeated after already in needs_repair is idempotently ignored", async () => {
    const { desktop, session, kPair, deviceId } = await pairUpToAwaitingReady();
    await session.handleFrame(await desktop.buildReady(kPair, deviceId));
    await session.handleFrame({ t: "error", reason: "device_revoked" });
    const outcome = await session.handleFrame({ t: "error", reason: "device_revoked" });
    expect(outcome).toEqual<PairingFrameOutcome>({ status: "ignored", reason: "already in needs_repair" });
    expect(session.state).toBe("needs_repair");
  });

  it("other error reasons (e.g. stale_epoch) are ignored, out of PairingSession's scope", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    await session.start();
    const outcome = await session.handleFrame({ t: "error", reason: "stale_epoch", currentEpoch: 3 });
    expect(outcome).toEqual<PairingFrameOutcome>({
      status: "ignored",
      reason: 'error frame reason "stale_epoch" out of PairingSession scope',
    });
    expect(session.state).toBe("awaiting_accept");
  });

  it("error frame with non-string reason doesn't crash and is ignored", async () => {
    const desktop = await makeFakeDesktop();
    const session = new PairingSession(desktop.qrPayload, new RecordingTransport(), new InMemoryKeyStore());
    await session.start();
    const outcome = await session.handleFrame({ t: "error" });
    expect(outcome).toEqual<PairingFrameOutcome>({
      status: "ignored",
      reason: 'error frame reason "unknown" out of PairingSession scope',
    });
  });
});

describe("PairingSession · desktop_offline 可见化", () => {
  it("desktop_offline while awaiting accept becomes visible and a later accept from the retried socket can continue pairing", async () => {
    const desktop = await makeFakeDesktop();
    const transport = new RecordingTransport();
    const session = new PairingSession(desktop.qrPayload, transport, new InMemoryKeyStore());
    await session.start();
    const kPair = await desktop.verifyHello(transport.last);
    const deviceId = randomUUID();
    const accept = await desktop.buildAccept(kPair, deviceId, new Uint8Array(32), randomHex64(), randomHex64());

    const offlineOutcome = await session.handleFrame({ t: "error", reason: "desktop_offline" });

    expect(offlineOutcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("desktop_offline");

    const acceptOutcome = await session.handleFrame(accept);
    expect(acceptOutcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("awaiting_ready");
    expect(transport.sent.filter((frame) => frame.t === "pair.hello")).toHaveLength(1);
  });

  it("desktop_offline after pair.accept is also a visible terminal phase", async () => {
    const { session } = await pairUpToAwaitingReady();

    const outcome = await session.handleFrame({ t: "error", reason: "desktop_offline" });

    expect(outcome).toEqual<PairingFrameOutcome>({ status: "applied" });
    expect(session.state).toBe("desktop_offline");
  });

  it("desktop_offline after activation is ignored and cannot overwrite the idempotent terminal phase", async () => {
    const { desktop, session, kPair, deviceId } = await pairUpToAwaitingReady();
    await session.handleFrame(await desktop.buildReady(kPair, deviceId));

    const outcome = await session.handleFrame({ t: "error", reason: "desktop_offline" });

    expect(outcome).toEqual<PairingFrameOutcome>({
      status: "ignored",
      reason: 'desktop_offline ignored in phase "activated"',
    });
    expect(session.state).toBe("activated");
  });

  it("desktop_offline after revocation is ignored and cannot overwrite the needs_repair terminal phase", async () => {
    const { session } = await pairUpToAwaitingReady();
    await session.handleFrame({ t: "error", reason: "device_revoked" });

    const outcome = await session.handleFrame({ t: "error", reason: "desktop_offline" });

    expect(outcome).toEqual<PairingFrameOutcome>({
      status: "ignored",
      reason: 'desktop_offline ignored in phase "needs_repair"',
    });
    expect(session.state).toBe("needs_repair");
  });
});
