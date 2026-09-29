// S1ja F5：假手机端到端脚本（离线可跑，无需联网打 staging）。
//
// Public keys use padded standard base64 to match the desktop parser.
// This includes desktop_pub and remote_pub.
// The peer public key passed to deriveKPair uses the same encoding.
// Pairing tokens retain their hexadecimal encoding.
// Capability and refresh tokens also retain their hexadecimal encoding.
// Device identifiers remain hexadecimal.
// These encodings make the simulated payloads compatible with the desktop.
//
// Minimal mobile and desktop actors exercise the encrypted protocol.
// They call the real RoomDO.fetch() and webSocketMessage() entry points.
// The flow starts with the scanned payload and connect_token derivation.
// It connects in pairing scope and exchanges pair.hello, pair.accept, and pair.done.
// The ready barrier precedes access and refresh credential delivery.
// The mobile connects in remote scope, sends input, and receives an acknowledgement.
// Expired access triggers a refresh-scope connection and token.refresh.
// Forwarding, rotation, put/ack, and receipt delivery precede reconnection.
//
// The relay never decrypts pairing or token.refresh ciphertext.
// It routes bytes using t, subject, connection_id, and generation.
// Both simulated endpoints perform real key exchange and authenticated encryption.
// Successful decryption verifies that routing preserves encrypted payloads.
// The complete flow verifies that refreshed access permits reconnection.
//
// Cryptographic operations mirror the desktop implementation.
// Pairing and connection tokens use distinct key-derivation parameters.
// deriveKPair uses the ASCII hexadecimal pairing token as salt and the shared secret as input.
// deriveConnectTokenHex uses empty salt and the ASCII hexadecimal pairing token as input.
// seal/open authenticate the metadata using buildAAD(meta).
// wrapKey/unwrapKey omit AAD when wrapping the room key for pair.accept.
//
// Pairing AAD labels follow the desktop: control for pair.hello and pair-confirm for pair.done.
// Both endpoints must agree on these labels for authentication to succeed.
// The relay forwards ciphertext without decrypting it, exercising routing, admission, and registry behavior.

import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash, hkdfSync, randomBytes, randomUUID, webcrypto } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { RoomDO } from "../src/room-do.js";
import * as store from "../src/room-store.js";
import { buildAAD, PROTOCOL_VERSION } from "../src/envelope.js";

const ROOM = "0123456789abcdef0123456789abcdef";
const K_PAIR_INFO = "agentloom-rc-v1";
const CONNECT_INFO = "agentloom-rc-connect-v1";

// ============================================================================
// 加密原语（与 remote_crypto.rs 一一对应，见文件头注释）
// ============================================================================

function sha256Hex(asciiValue) {
  return createHash("sha256").update(asciiValue, "ascii").digest("hex");
}

function randomHex64() {
  return randomBytes(32).toString("hex");
}

// device_id：UUID v4 形状（remote_pairing.rs generate_device_id 同款：version
// 半字节=4·variant 半字节=8-b），registry subject = `device:${deviceId}`。
function randomDeviceId() {
  const bytes = randomBytes(16);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function generateX25519KeyPair() {
  const keyPair = await webcrypto.subtle.generateKey({ name: "X25519" }, true, ["deriveBits"]);
  const publicB64 = Buffer.from(await webcrypto.subtle.exportKey("raw", keyPair.publicKey)).toString("base64");
  return { privateKey: keyPair.privateKey, publicB64 };
}

async function importX25519PublicKey(publicB64) {
  return webcrypto.subtle.importKey("raw", Buffer.from(publicB64, "base64"), { name: "X25519" }, true, []);
}

// derive_k_pair: HKDF-SHA256(salt=pairing_code 的 ASCII 字节, IKM=X25519 shared secret, info="agentloom-rc-v1", L=32)
async function deriveKPair(myPrivateKey, theirPublicB64, pairingTokenHex) {
  const theirPub = await importX25519PublicKey(theirPublicB64);
  const shared = Buffer.from(await webcrypto.subtle.deriveBits({ name: "X25519", public: theirPub }, myPrivateKey, 256));
  return Buffer.from(hkdfSync(
    "sha256",
    shared,
    Buffer.from(pairingTokenHex, "ascii"),
    Buffer.from(K_PAIR_INFO, "ascii"),
    32,
  ));
}

// derive_connect_token / connect-kdf.test.js 同一公式。
function deriveConnectTokenHex(pairingTokenHex) {
  return Buffer.from(hkdfSync(
    "sha256",
    Buffer.from(pairingTokenHex, "ascii"),
    Buffer.alloc(0),
    Buffer.from(CONNECT_INFO, "ascii"),
    32,
  )).toString("hex");
}

async function importAesKey(rawKeyBytes) {
  return webcrypto.subtle.importKey("raw", rawKeyBytes, "AES-GCM", false, ["encrypt", "decrypt"]);
}

// seal/open：AES-256-GCM 带 AAD = buildAAD(meta)（envelope.js 同一函数）。
async function seal(rawKeyBytes, meta, plaintextBytes) {
  const key = await importAesKey(rawKeyBytes);
  const nonce = randomBytes(12);
  const ciphertext = Buffer.from(await webcrypto.subtle.encrypt(
    { name: "AES-GCM", iv: nonce, additionalData: new TextEncoder().encode(buildAAD(meta)) },
    key,
    plaintextBytes,
  ));
  return { ct: ciphertext.toString("base64"), n: nonce.toString("base64") };
}

async function open(rawKeyBytes, meta, ctB64, nB64) {
  const key = await importAesKey(rawKeyBytes);
  const plaintext = await webcrypto.subtle.decrypt(
    { name: "AES-GCM", iv: Buffer.from(nB64, "base64"), additionalData: new TextEncoder().encode(buildAAD(meta)) },
    key,
    Buffer.from(ctB64, "base64"),
  );
  return Buffer.from(plaintext);
}

// wrap_key/unwrap_key：AES-256-GCM 无 AAD——只用于 pair.accept 的 k_room_ct/n。
async function wrapKey(kekBytes, keyBytes) {
  const key = await importAesKey(kekBytes);
  const nonce = randomBytes(12);
  const ciphertext = Buffer.from(await webcrypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, key, keyBytes));
  return { ct: ciphertext.toString("base64"), n: nonce.toString("base64") };
}

async function unwrapKey(kekBytes, ctB64, nB64) {
  const key = await importAesKey(kekBytes);
  const plaintext = await webcrypto.subtle.decrypt(
    { name: "AES-GCM", iv: Buffer.from(nB64, "base64") },
    key,
    Buffer.from(ctB64, "base64"),
  );
  return Buffer.from(plaintext);
}

function sealJson(rawKeyBytes, meta, obj) {
  return seal(rawKeyBytes, meta, new TextEncoder().encode(JSON.stringify(obj)));
}

async function openJson(rawKeyBytes, meta, ctB64, nB64) {
  return JSON.parse(new TextDecoder().decode(await open(rawKeyBytes, meta, ctB64, nB64)));
}

// ============================================================================
// 运行时/连接夹具（与 s1d/s1f1-fixture.test.js 同款薄适配器——真实 node:sqlite，
// mock 只顶掉 Cloudflare Hibernation API 运行时全局，业务逻辑一行未改）
// ============================================================================

function makeRuntime() {
  const db = new DatabaseSync(":memory:");
  const sql = {
    exec(query, ...params) {
      const stmt = db.prepare(query);
      if (/^\s*(SELECT|PRAGMA)/i.test(query)) return stmt.all(...params);
      stmt.run(...params);
      return [];
    },
  };
  const registry = [];
  const storage = {
    sql,
    transactionSync(callback) {
      db.exec("BEGIN IMMEDIATE");
      try {
        const result = callback();
        db.exec("COMMIT");
        return result;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
    async setAlarm() {},
  };
  const ctx = {
    storage,
    acceptWebSocket(ws, tags = []) {
      registry.push({ ws, tags });
    },
    getWebSockets(tag) {
      // R1（双路审）：真实 Cloudflare Hibernation API 里，close() 只是**发起**
      // 关闭握手——socket 立刻进 CLOSING，但在握手完成、真正变 CLOSED 之前，
      // getWebSockets() 仍然会吐出它。之前这里按 `closed.length===0` 过滤、
      // 一 close() 就立刻消失，等于让 mock 自己把"已断连接收不到东西"这条
      // 不变量藏起来了——真正该证明这条不变量的是 relay 自己在
      // deliverRefreshReceipt/canDeliverOutbound 里核 readyState，不是 mock
      // 帮它作弊。这里只在真正 CLOSED（readyState===3）时才从结果里摘除；
      // CLOSING（2）期间仍可见，直到测试显式调用 finishClose() 推进收尾。
      const live = registry.filter((item) => item.ws.readyState !== 3);
      if (!tag) return live.map((item) => item.ws);
      return live.filter((item) => item.tags.includes(tag)).map((item) => item.ws);
    },
  };
  return { ctx };
}

function fakeWs(attachment = null) {
  let currentAttachment = attachment;
  return {
    sent: [],
    closed: [],
    // R1：OPEN(1) → CLOSING(2) → CLOSED(3) 三态建模（标准 WebSocket.readyState
    // 取值），不是「close() 一调用连接就彻底消失」的两态简化——真实运行时
    // close() 只是发起握手，读到 CLOSING 期间 relay 的投递代码必须自己查
    // readyState 挡下来，不能指望连接已经从 getWebSockets() 里消失。
    readyState: 1, // WebSocket.OPEN
    send(text) {
      this.sent.push(typeof text === "string" ? JSON.parse(text) : text);
    },
    close(code, reason) {
      if (this.readyState >= 2) return; // 已在 CLOSING/CLOSED：真实 close() 对此是空操作
      this.closed.push({ code, reason });
      this.readyState = 2; // WebSocket.CLOSING——仍会被 getWebSockets() 吐出
    },
    // 测试专用：把一条处于 CLOSING 的连接推进到真正 CLOSED（握手完成），
    // 之后才会从 getWebSockets() 里消失。不是真实 API 的一部分，只是这个
    // mock 显式收尾 CLOSING 中间态的手柄——真实运行时里这一步由 workerd
    // 自己在关闭握手完成时做。
    finishClose() {
      if (this.readyState !== 2) {
        throw new Error(`finishClose() called on a socket that isn't CLOSING (readyState=${this.readyState})`);
      }
      this.readyState = 3; // WebSocket.CLOSED
    },
    serializeAttachment(value) {
      currentAttachment = value;
    },
    deserializeAttachment() {
      return currentAttachment;
    },
  };
}

async function withUpgradeRuntime(callback) {
  const NativeResponse = globalThis.Response;
  const NativeWebSocketPair = globalThis.WebSocketPair;
  globalThis.Response = class TestResponse {
    constructor(body, init = {}) {
      this.body = body;
      this.status = init.status ?? 200;
      this.webSocket = init.webSocket ?? null;
      this.headers = new Headers(init.headers || {});
    }
  };
  globalThis.WebSocketPair = class TestWebSocketPair {
    constructor() {
      this[0] = fakeWs();
      this[1] = fakeWs();
    }
  };
  try {
    return await callback();
  } finally {
    globalThis.Response = NativeResponse;
    if (NativeWebSocketPair === undefined) delete globalThis.WebSocketPair;
    else globalThis.WebSocketPair = NativeWebSocketPair;
  }
}

function desktopSocket(room, ctx, { ready = true } = {}) {
  const epoch = store.bumpEpoch(room.sql);
  const ws = fakeWs({
    role: "desktop", scope: "desktop", epoch,
    registry_ready: ready, registry_sync_deadline: Date.now() + 30_000,
    connectedAt: Date.now(),
  });
  ctx.acceptWebSocket(ws, ["desktop"]);
  return ws;
}

async function send(room, ws, frameOrText) {
  const before = ws.sent.length;
  await room.webSocketMessage(ws, typeof frameOrText === "string" ? frameOrText : JSON.stringify(frameOrText));
  return ws.sent[before];
}

// 经真实 fetch() 子协议路径连接（scope 由 relay 按 resolveTokenAdmission 判
// 定，不是调用方指定）；返回 relay 侧接受的那个 socket（`ctx.getWebSockets`
// 里刚追加的最后一条 "remote" tag 连接——桌面用 "desktop" tag，不会撞）。
async function connectWithToken(room, ctx, tokenHex) {
  return withUpgradeRuntime(async () => {
    const response = await room.fetch(new Request(`https://relay.example/room/${ROOM}`, {
      headers: { Upgrade: "websocket", "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${tokenHex}` },
    }));
    assert.equal(response.status, 101, "connectWithToken 期望 101（令牌必须仍被 relay 接受）");
    return ctx.getWebSockets("remote").at(-1);
  });
}

// ============================================================================
// 假桌面 actor：只实现本脚本需要驱动 relay 的那部分桌面协议行为（token 注册表
// 读写 + pairing 三帧 + refresh 三帧），不是 remote_gateway.rs 的重实现——relay
// 侧被测的是路由/准入/注册表状态机，不是桌面自己的业务逻辑。
// ============================================================================

function makeFakeDesktop(room, desktopWs) {
  let generationCounter = 0;
  // refresh journal：{subject -> {currentRefreshHash, prevRefreshHash, prevReceipt}}
  // 对应母文档 §9.6「桌面：命中当前 refresh_hash → 领代…单事务写新 hash…；命中
  // prev_refresh_hash 且 journal 未过期 → 原样重放同一份回执」——F5 点名的
  // 「慢路径轮换」场景必须有这个 journal 才能收敛，relay 自己不存这个。
  const journals = new Map();

  async function tokenPut(frame) {
    const ack = await send(room, desktopWs, { t: "token.put", ...frame });
    assert.equal(ack.t, "token.ack", `token.put 必须收到 token.ack，实际 ${JSON.stringify(ack)}`);
    assert.equal(ack.result, "ok", `token.put 必须被接受，实际 ${JSON.stringify(ack)}`);
    return ack;
  }

  return {
    nextGeneration() {
      generationCounter += 1;
      return generationCounter;
    },
    tokenPut,
    journals,
    async putRegistryEntry({ subject, generation, scope, current, prev }) {
      return tokenPut({ subject, generation, scope, current, ...(prev ? { prev } : {}) });
    },
  };
}

// ============================================================================
// 完整配对流程（phases 0-6）：扫码载荷 → connect_token 派生 → pairing scope
// 连接 → pair.hello/accept/done → ready 屏障。三个测试共用，避免每条测试重
// 敲一遍握手。返回配对完成后的完整状态，供各测试从「已拿到 access+refresh」
// 处继续往后走。
// ============================================================================

async function pairDevice(room, ctx, desktop, fakeDesktop) {
  // ---- phase 0：扫码载荷（光学带外·QR payload）----
  const desktopKeys = await generateX25519KeyPair();
  const pairingTokenHex = randomHex64();
  const qrPayload = {
    v: 1, relay_url: "wss://relay.example", room: ROOM,
    pairing_token: pairingTokenHex, desktop_pub: desktopKeys.publicB64,
  };
  assert.equal(qrPayload.pairing_token.length, 64, "pairing_token 必须是 64 位 hex（256bit CSPRNG）");

  // ---- 桌面「开始配对」：注册 connect_token 到 relay 的 pairing 行（§9.5）----
  const connectTokenHex = deriveConnectTokenHex(qrPayload.pairing_token);
  const pairingGeneration = fakeDesktop.nextGeneration();
  await fakeDesktop.putRegistryEntry({
    subject: "pairing", generation: pairingGeneration, scope: "pairing",
    current: { token_hash: sha256Hex(connectTokenHex), access_expires: Date.now() + 300_000 },
  });

  // ---- phase 1：手机扫码 → connect_token 派生（同一公式，必须与桌面一致）→ pairing scope 连接 ----
  const mobileConnectTokenHex = deriveConnectTokenHex(qrPayload.pairing_token);
  assert.equal(mobileConnectTokenHex, connectTokenHex, "手机与桌面必须独立推出同一个 connect_token");
  const mobileWs = await connectWithToken(room, ctx, mobileConnectTokenHex);
  assert.equal(mobileWs.deserializeAttachment().scope, "pairing", "手机首连必须落在 pairing scope");

  // ---- phase 2：pair.hello（手机 K_pair 材料 + K_pair 加密 pairing_token 证明）----
  const mobileKeys = await generateX25519KeyPair();
  const mobileKPair = await deriveKPair(mobileKeys.privateKey, qrPayload.desktop_pub, qrPayload.pairing_token);
  const helloMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "control", session: null, command_id: null };
  const { ct: helloCt, n: helloN } = await seal(mobileKPair, helloMeta, new TextEncoder().encode(qrPayload.pairing_token));
  await send(room, mobileWs, { t: "pair.hello", room: ROOM, remote_pub: mobileKeys.publicB64, token_ct: helloCt, token_n: helloN });

  const helloOnDesktop = desktop.sent.at(-1);
  assert.equal(helloOnDesktop.t, "pair.hello", "relay 必须原样把 pair.hello 转发给桌面");
  // 不只查类型——必须精确等于发起 hello 的那台手机连接的真实 connection_id
  // （§9.5 pair.accept/pair.ready 的定向路由全靠这个字段回指）；只查
  // typeof==="string" 查不出「盖错了别的连接的 id」这类路由错误。
  assert.equal(
    helloOnDesktop.origin_connection_id,
    mobileWs.deserializeAttachment().connection_id,
    "relay 盖的 origin_connection_id 必须精确等于发起 hello 的手机连接的真实 connection_id",
  );

  // ---- 桌面验证 hello：独立推出同一个 K_pair、解出 pairing_token 明文，两侧材料互证 ----
  const desktopKPair = await deriveKPair(desktopKeys.privateKey, helloOnDesktop.remote_pub, qrPayload.pairing_token);
  assert.deepEqual(desktopKPair, mobileKPair, "桌面/手机两侧 X25519+HKDF 必须推出同一把 K_pair");
  const helloPlain = new TextDecoder().decode(await open(desktopKPair, helloMeta, helloOnDesktop.token_ct, helloOnDesktop.token_n));
  assert.equal(helloPlain, qrPayload.pairing_token, "桌面必须能用 K_pair 解出手机证明的 pairing_token 明文");

  // ---- phase 3：pair.accept（K_room 用 K_pair wrap；capability/refresh token 用 K_pair seal）----
  const deviceId = randomDeviceId();
  const subject = `device:${deviceId}`;
  const kRoom = randomBytes(32);
  const capabilityTokenHex = randomHex64();
  const refreshTokenHex = randomHex64();
  const { ct: kRoomCt, n: kRoomN } = await wrapKey(desktopKPair, kRoom);
  const tokensMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "pair-accept-tokens", session: deviceId, command_id: null };
  const { ct: tokensCt, n: tokensN } = await sealJson(desktopKPair, tokensMeta, {
    capability_token: capabilityTokenHex, refresh_token: refreshTokenHex,
  });
  await send(room, desktop, {
    t: "pair.accept", room: ROOM, device_id: deviceId,
    k_room_ct: kRoomCt, k_room_n: kRoomN, tokens_ct: tokensCt, tokens_n: tokensN,
  });

  const acceptOnMobile = mobileWs.sent.at(-1);
  assert.equal(acceptOnMobile.t, "pair.accept", "relay 必须把 pair.accept 定向投给发起 hello 的那台手机");
  const mobileKRoom = await unwrapKey(mobileKPair, acceptOnMobile.k_room_ct, acceptOnMobile.k_room_n);
  assert.deepEqual(mobileKRoom, kRoom, "手机 unwrap 出的 K_room 必须与桌面生成的一致");
  const mobileTokens = await openJson(mobileKPair, tokensMeta, acceptOnMobile.tokens_ct, acceptOnMobile.tokens_n);
  assert.deepEqual(mobileTokens, { capability_token: capabilityTokenHex, refresh_token: refreshTokenHex });

  // ---- phase 4：pair.done（手机用刚解出的 K_room 密封确认体，证明真已解出
  // K_room）——R2 修正：plaintext = device_id 原始字节，与
  // remote_pairing.rs::seal_pair_done_confirm 逐字节一致（该函数注释明写
  // "plaintext 就是 device_id 自身，AAD 已经把 room+device_id 绑死，plaintext
  // 校验是防御性冗余"），不是 JSON {room,device_id}。wire-v1.json 没有专门的
  // aad-kat 样张钉这个 plaintext 形状——唯一依据是 remote_pairing.rs 源码本身
  // （pairing_envelope_meta/pair_done_confirm_meta 的文档注释也把这类协议
  // 自称 provisional，relay 侧本就不解密，不影响 relay 行为正确性）。----
  const confirmMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "pair-confirm", session: deviceId, command_id: null };
  const { ct: confirmCt, n: confirmN } = await seal(mobileKRoom, confirmMeta, new TextEncoder().encode(deviceId));
  await send(room, mobileWs, { t: "pair.done", room: ROOM, device_id: deviceId, confirm_ct: confirmCt, confirm_n: confirmN });

  const doneOnDesktop = desktop.sent.at(-1);
  assert.equal(doneOnDesktop.t, "pair.done");
  const confirmPlain = new TextDecoder().decode(await open(kRoom, confirmMeta, doneOnDesktop.confirm_ct, doneOnDesktop.confirm_n));
  assert.equal(confirmPlain, deviceId, "恶意 relay 伪造不出这份确认体——它没有 K_room");

  // ---- phase 5：桌面注册设备令牌 + phase 6：ready 屏障——R2 修正：pair.ready
  // 密文体 plaintext = device_id 原始字节（remote_pairing.rs::seal_pair_ready），
  // **不携带 tokens**——手机的 access/refresh 只来自上面 phase 3 已经解出的
  // mobileTokens（pair.accept 的 tokens_ct，唯一的令牌投递点）。ready 纯粹是
  // "桌面已经把设备注册进 relay 注册表、access 现在真的能连得上"这一激活
  // 屏障信号，不是又一份令牌投递（原实现误密封了 {access,refresh}）。----
  const generation = fakeDesktop.nextGeneration();
  const now = Date.now();
  const accessExpires = now + 3_600_000; // 1h
  const refreshUntil = now + 30 * 24 * 3_600_000; // 30d
  await fakeDesktop.putRegistryEntry({
    subject, generation, scope: "remote",
    current: { token_hash: sha256Hex(capabilityTokenHex), access_expires: accessExpires, refresh_until: refreshUntil },
  });
  fakeDesktop.journals.set(subject, { currentRefreshHash: sha256Hex(refreshTokenHex), prevRefreshHash: null, prevReceipt: null });

  const readyMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "pair-ready", session: deviceId, command_id: null };
  const { ct: readyCt, n: readyN } = await seal(desktopKPair, readyMeta, new TextEncoder().encode(deviceId));
  await send(room, desktop, { t: "pair.ready", room: ROOM, device_id: deviceId, ct: readyCt, n: readyN });

  const readyOnMobile = mobileWs.sent.at(-1);
  assert.equal(readyOnMobile.t, "pair.ready");
  const readyPlain = new TextDecoder().decode(await open(mobileKPair, readyMeta, readyOnMobile.ct, readyOnMobile.n));
  assert.equal(readyPlain, deviceId, "pair.ready 密文体解出的必须是 device_id 本身——它是屏障信号，不装令牌");
  // 手机：收 ready（激活屏障）→ 一个 IndexedDB 事务落盘 {access, refresh}
  // 后才许拿 access 重连（§9.5）——凭据本身在 phase 3 解 pair.accept 时就已经
  // 拿到手（mobileTokens），ready 只是告诉手机"现在可以真的去用它了"；测试
  // 里没有真实存储，这里的落盘就是把 mobileTokens 的值收进下面这个对象，
  // 从这一步之后的所有重连都只用这里"持久化"下来的值，象征"落盘后才可用"。
  const persisted = { access: mobileTokens.capability_token, refresh: mobileTokens.refresh_token };

  return {
    mobileKeys, mobileKPair, kRoom, deviceId, subject, generation,
    persisted, desktopKPair, desktopKeys,
  };
}

// ============================================================================
// F5 主线：完整 happy path 端到端
// ============================================================================

test("S1ja F5：假手机端到端——扫码→pairing→ready→remote→input→refresh→新access重连", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const desktop = desktopSocket(room, ctx);
  const fakeDesktop = makeFakeDesktop(room, desktop);

  const paired = await pairDevice(room, ctx, desktop, fakeDesktop);
  const { mobileKPair, kRoom, deviceId, subject } = paired;
  let { persisted } = paired;

  // ---- phase 7：remote scope 连接（用配对拿到的 access）----
  const remoteWs = await connectWithToken(room, ctx, persisted.access);
  assert.equal(remoteWs.deserializeAttachment().scope, "remote");
  assert.equal(remoteWs.deserializeAttachment().subject, subject);

  // ---- phase 8：发一条 input 并收 ack ----
  // W0-fix：密文体明文改按 M0 协议 §3 生产形状 {"t":"input.send","session":<sid>,"text":...}
  // （权威参照 = app/src-tauri/src/remote_gateway.rs::handle_command_envelope :3517，
  // The desktop parser expects input.send with session and text fields.
  // The encrypted payload must preserve this shape for desktop interoperability.
  // command_id is a generated UUID, matching the production remote endpoint.
  // Decryption and acknowledgement verify the command round trip.
  const epoch = store.getCurrentEpoch(room.sql);
  const inputSessionId = "s1";
  const inputCommandId = randomUUID();
  const inputMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch, kind: "input", session: inputSessionId, command_id: inputCommandId };
  const { ct: inputCt, n: inputN } = await seal(kRoom, inputMeta, new TextEncoder().encode(JSON.stringify({ t: "input.send", session: inputSessionId, text: "hello" })));
  await send(room, remoteWs, {
    v: PROTOCOL_VERSION, room: ROOM, epoch, kind: "input", session: inputSessionId,
    command_id: inputCommandId, seq: null, client_msg_id: null, ct: inputCt, n: inputN, ts: Date.now(),
  });

  const inputOnDesktop = desktop.sent.at(-1);
  assert.equal(inputOnDesktop.kind, "input");
  assert.equal(inputOnDesktop.command_id, inputCommandId);
  const inputPlain = JSON.parse(new TextDecoder().decode(await open(kRoom, inputMeta, inputOnDesktop.ct, inputOnDesktop.n)));
  assert.deepEqual(inputPlain, { t: "input.send", session: inputSessionId, text: "hello" }, "K_room 端到端往返必须解出手机原样发的内容（生产 input.send 形状）");

  await send(room, desktop, { t: "input.ack", command_id: inputCommandId, outcome: "ok" });
  const ackOnMobile = remoteWs.sent.at(-1);
  assert.deepEqual(ackOnMobile, { t: "input.ack", command_id: inputCommandId, outcome: "ok" });

  // ---- phase 9：access 过期 → refresh scope 连接 ----
  // 直接把这个 subject 当前别名的 access_expires 拨到过去（模拟时间流逝到 1h
  // 后）——跟 s1d/s1f1-fixture 已有测试同款手法，不必真的等 1 小时。
  room.sql.exec(
    "UPDATE token_aliases SET access_expires = ? WHERE subject = ? AND kind = 'current'",
    Date.now() - 1_000, subject,
  );
  const refreshScopeWs = await connectWithToken(room, ctx, persisted.access);
  assert.equal(refreshScopeWs.deserializeAttachment().scope, "refresh", "access 过期但 valid_until 未到——admission 必须降级成 refresh scope");

  // ---- phase 10：token.refresh → forward → 轮换 → put/ack → 回执——R2 修正：
  // token.refresh 请求明文 = JSON {"refresh_token":"<hex64>"}
  // （remote_pairing.rs::open_token_refresh_request/RefreshRequestPlaintext，
  // 与 fixtures/wire-v1.json 的 aad_kat_token_refresh KAT 逐字节钉死的
  // plaintext 一致），不是裸 hex 字节。----
  const requestId = randomUUID();
  const refreshReqMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "token.refresh", session: deviceId, command_id: requestId };
  const { ct: reqCt, n: reqN } = await sealJson(mobileKPair, refreshReqMeta, { refresh_token: persisted.refresh });
  await send(room, refreshScopeWs, { t: "token.refresh", request_id: requestId, ct: reqCt, n: reqN });

  const forwardOnDesktop = desktop.sent.at(-1);
  assert.equal(forwardOnDesktop.t, "token.refresh.forward");
  assert.equal(forwardOnDesktop.subject, subject);
  const forwardedRefreshPlain = (await openJson(mobileKPair, refreshReqMeta, forwardOnDesktop.ct, forwardOnDesktop.n)).refresh_token;
  assert.equal(forwardedRefreshPlain, persisted.refresh, "desktop 必须能用同一把 K_pair 解出手机送来的 refresh_token 原文");

  const journal = fakeDesktop.journals.get(subject);
  assert.equal(sha256Hex(forwardedRefreshPlain), journal.currentRefreshHash, "命中当前 refresh_hash——走正常轮换分支");

  const newCapabilityTokenHex = randomHex64();
  const newRefreshTokenHex = randomHex64();
  const newGeneration = fakeDesktop.nextGeneration();
  const now2 = Date.now();
  await fakeDesktop.putRegistryEntry({
    subject, generation: newGeneration, scope: "remote",
    current: { token_hash: sha256Hex(newCapabilityTokenHex), access_expires: now2 + 3_600_000, refresh_until: now2 + 30 * 24 * 3_600_000 },
    prev: { token_hash: sha256Hex(persisted.access), generation: paired.generation, prev_expires: now2 + 48 * 3_600_000 },
  });
  // R2 修正：token.refresh.ok 明文 = JSON {"capability_token","refresh_token"}
  // （remote_pairing.rs::seal_token_refresh_ok/RefreshResponseTokens，与
  // aad_kat_token_refresh_ok KAT 逐字节钉死的 plaintext 一致），不是
  // {access,refresh}。
  const okMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "token.refresh.ok", session: deviceId, command_id: requestId };
  const { ct: okCt, n: okN } = await sealJson(paired.desktopKPair, okMeta, {
    capability_token: newCapabilityTokenHex, refresh_token: newRefreshTokenHex,
  });
  journal.prevRefreshHash = journal.currentRefreshHash;
  journal.currentRefreshHash = sha256Hex(newRefreshTokenHex);
  journal.prevReceipt = { ct: okCt, n: okN, meta: okMeta };
  await send(room, desktop, { t: "token.refresh.ok", request_id: requestId, subject, generation: newGeneration, ct: okCt, n: okN });

  const okOnMobile = refreshScopeWs.sent.at(-1);
  assert.equal(okOnMobile.t, "token.refresh.ok", `回执必须送达发起请求的那条连接，实际收到 ${JSON.stringify(okOnMobile)}`);
  const newTokens = await openJson(mobileKPair, okMeta, okOnMobile.ct, okOnMobile.n);
  assert.deepEqual(newTokens, { capability_token: newCapabilityTokenHex, refresh_token: newRefreshTokenHex });
  persisted = { access: newTokens.capability_token, refresh: newTokens.refresh_token };

  // ---- phase 11：用新 access 重连 ----
  const reconnectedWs = await connectWithToken(room, ctx, persisted.access);
  assert.equal(reconnectedWs.deserializeAttachment().scope, "remote", "新 access 必须立刻可用——不需要再等一轮 sync/ready");
  assert.equal(reconnectedWs.deserializeAttachment().subject, subject);
  assert.equal(reconnectedWs.deserializeAttachment().generation, newGeneration);
});

// ============================================================================
// F5 必覆盖场景 ①：慢路径轮换——put 升代后、回执到达前插一帧广播；目标 socket
// 被 canDeliverOutbound 关掉；断言仍能靠 prev 别名 + 重连 + journal 重放收敛。
// ============================================================================

test("S1ja F5 场景①：慢路径轮换——回执到达前的广播关闭旧连接，靠 prev 别名+重连+journal 重放收敛", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const desktop = desktopSocket(room, ctx);
  const fakeDesktop = makeFakeDesktop(room, desktop);
  const paired = await pairDevice(room, ctx, desktop, fakeDesktop);
  const { mobileKPair, deviceId, subject } = paired;
  const persisted = paired.persisted;

  const staleWs = await connectWithToken(room, ctx, persisted.access);
  assert.equal(staleWs.deserializeAttachment().generation, paired.generation);

  // 手机发起 refresh（用当前 remote-scope 连接发·矩阵允许 remote scope 发
  // token.refresh，见 §9.1 入站矩阵）。
  const requestId = randomUUID();
  const refreshReqMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "token.refresh", session: deviceId, command_id: requestId };
  // R2 修正：token.refresh 请求明文 = JSON {"refresh_token":...}（aad_kat_token_refresh KAT），不是裸字节。
  const { ct: reqCt, n: reqN } = await sealJson(mobileKPair, refreshReqMeta, { refresh_token: persisted.refresh });
  await send(room, staleWs, { t: "token.refresh", request_id: requestId, ct: reqCt, n: reqN });
  const forwardOnDesktop = desktop.sent.at(-1);
  assert.equal(forwardOnDesktop.t, "token.refresh.forward");

  // 桌面完成轮换：put 升代（generation 1 → 2），但回执还没发出去。
  const journal = fakeDesktop.journals.get(subject);
  const newCapabilityTokenHex = randomHex64();
  const newRefreshTokenHex = randomHex64();
  const newGeneration = fakeDesktop.nextGeneration();
  const now = Date.now();
  await fakeDesktop.putRegistryEntry({
    subject, generation: newGeneration, scope: "remote",
    current: { token_hash: sha256Hex(newCapabilityTokenHex), access_expires: now + 3_600_000, refresh_until: now + 30 * 24 * 3_600_000 },
    prev: { token_hash: sha256Hex(persisted.access), generation: paired.generation, prev_expires: now + 48 * 3_600_000 },
  });

  // ---- 关键一步：put 升代后、回执到达前，插一帧广播 ----
  // staleWs 的 attachment.generation 还停在旧代（1），canDeliverOutbound 的
  // isOfficialAttachmentLive 逐字段核对 subject 当前 generation（已经是 2）
  // 时会判它「不是当前权威 attachment」→ 直接 close，这正是母文档 F5 点名要
  // 验的「慢路径」触发点。
  room.broadcastRaw({ v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "presence" });
  assert.equal(staleWs.closed.length, 1, "回执到达前的广播必须把旧代 attachment 的 socket 关掉——这是慢路径的触发条件，不是本测试要修的 bug");
  // R1（双路审）：close() 只是发起关闭握手——staleWs 现在是 CLOSING(2)，不是
  // 立刻消失。mock 的 getWebSockets() 在真正 CLOSED(3) 之前仍然吐出它（同
  // 真实 Cloudflare 运行时），下面这条断言就是钉死这一点：接下来
  // deliverRefreshReceipt 挡住投递，靠的必须是 relay 自己核 readyState，
  // 不是 mock 替它把连接藏起来。
  assert.equal(staleWs.readyState, 2, "close() 后应处于 CLOSING，不是立刻 CLOSED");
  assert.ok(
    ctx.getWebSockets("remote").includes(staleWs),
    "CLOSING 期间 getWebSockets() 仍必须吐出这条连接（同真实 Hibernation API）——relay 的投递闸得自己查 readyState 挡它，不能靠 mock 藏起来",
  );

  // 桌面现在才真正发回执——但 relay 的 deliverRefreshReceipt 谓词⑥要求
  // 目标=请求行 connection_id 且 readyState===OPEN，staleWs 处于 CLOSING，
  // 回执必须被丢弃（不是投给别的 socket、不是报错）。connectWithToken 建连时
  // replayTo 已经往 staleWs 送过一次 replay.head（scope=remote 连接的既有
  // 行为，跟本场景无关）——这里要断言的是「广播关闭之后，不会再凭空多收到
  // 东西」，用关闭前的帧数量做基线。
  const staleFramesBeforeReceipt = staleWs.sent.length;
  // R2 修正：token.refresh.ok 明文 = JSON {"capability_token","refresh_token"}（aad_kat_token_refresh_ok KAT），不是 {access,refresh}。
  const okMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "token.refresh.ok", session: deviceId, command_id: requestId };
  const { ct: okCt, n: okN } = await sealJson(paired.desktopKPair, okMeta, {
    capability_token: newCapabilityTokenHex, refresh_token: newRefreshTokenHex,
  });
  journal.prevRefreshHash = journal.currentRefreshHash;
  journal.currentRefreshHash = sha256Hex(newRefreshTokenHex);
  journal.prevReceipt = { ct: okCt, n: okN, meta: okMeta };
  await send(room, desktop, { t: "token.refresh.ok", request_id: requestId, subject, generation: newGeneration, ct: okCt, n: okN });
  assert.equal(staleWs.sent.length, staleFramesBeforeReceipt, "旧连接处于 CLOSING——迟到的回执不会凭空多送一帧给它（靠 relay 的 readyState 闸，不是 mock 藏起来）");
  // R1：deliverRefreshReceipt 命中 readyState 闸时按 ⑥ 同款语义丢弃且**不删
  // 行**——如果这一行被误删，手机走 prev 别名重连后重发同一个 request_id
  // 会被当成"未知 request_id"的全新请求，而不是"resend"（isResend 判定
  // 依赖这一行还在），journal 重放收敛的前提就塌了。
  assert.ok(
    store.getRefreshRequest(room.sql, requestId),
    "readyState 闸拦下投递后，refresh_requests 那一行必须还在——不能被「投完删行」误删",
  );
  // 测试显式推进/收尾：staleWs 的关闭握手在这里才算真正完成，之后它会从
  // getWebSockets() 消失——不留一个永远卡在 CLOSING 的假连接。
  staleWs.finishClose();
  assert.equal(staleWs.readyState, 3, "finishClose 后必须是 CLOSED");
  assert.ok(!ctx.getWebSockets("remote").includes(staleWs), "CLOSED 之后 getWebSockets() 不应再吐出它");

  // ---- 手机走 prev 别名重连恢复：旧的 capability token 现在是 prev kind，
  // now < valid_until(prev_expires) → admission 降级成 refresh scope。 ----
  const prevAliasWs = await connectWithToken(room, ctx, persisted.access);
  assert.equal(prevAliasWs.deserializeAttachment().scope, "refresh", "旧 access（现为 prev 别名）必须仍可连上、降级成 refresh scope——这正是「靠 prev 别名恢复」");
  assert.equal(prevAliasWs.deserializeAttachment().kind, "prev");

  // ---- 手机用同一个 request_id 重发 refresh（§9.6：重启/重连复用同一个
  // pending_refresh id）——命中 journal 的 prevRefreshHash，桌面原样重放同一
  // 份回执（不重新轮换），relay 按新连接的 connection_id 重绑投递。 ----
  const { ct: retryCt, n: retryN } = await sealJson(mobileKPair, refreshReqMeta, { refresh_token: persisted.refresh });
  await send(room, prevAliasWs, { t: "token.refresh", request_id: requestId, ct: retryCt, n: retryN });
  const secondForward = desktop.sent.at(-1);
  assert.equal(secondForward.t, "token.refresh.forward");
  const secondForwardPlain = (await openJson(mobileKPair, refreshReqMeta, secondForward.ct, secondForward.n)).refresh_token;
  assert.equal(sha256Hex(secondForwardPlain), journal.prevRefreshHash, "重放请求命中的必须是 prev_refresh_hash（旧 refresh_token），不是当前代");

  // 假桌面：命中 prev_refresh_hash 且 journal 未过期 → 原样重放缓存的回执
  // （§9.6），不重新领代/不重新轮换。
  const replay = journal.prevReceipt;
  await send(room, desktop, { t: "token.refresh.ok", request_id: requestId, subject, generation: newGeneration, ct: replay.ct, n: replay.n });

  const replayedOnMobile = prevAliasWs.sent.at(-1);
  assert.equal(replayedOnMobile.t, "token.refresh.ok");
  const replayedTokens = await openJson(mobileKPair, okMeta, replayedOnMobile.ct, replayedOnMobile.n);
  assert.deepEqual(
    replayedTokens,
    { capability_token: newCapabilityTokenHex, refresh_token: newRefreshTokenHex },
    "journal 重放必须给出跟第一次轮换完全相同的新 access/refresh——这就是「收敛」：不管走的是原连接还是慢路径重连，手机最终拿到的是同一份新凭据",
  );

  // 收敛之后，新 access 立即可用（不需要再等一轮 sync/ready）。
  const finalWs = await connectWithToken(room, ctx, replayedTokens.capability_token);
  assert.equal(finalWs.deserializeAttachment().scope, "remote");
  assert.equal(finalWs.deserializeAttachment().generation, newGeneration);
});

// ============================================================================
// F5 必覆盖场景 ②：撤销正在飞行中的 refresh——手机的 token.refresh 已转发给
// 桌面、桌面还没回执之前，设备被撤销；断言飞行中的回执被安全丢弃、不会让一个
// 已撤销的设备复活或让 relay 状态错乱。
// ============================================================================

test("S1ja F5 场景②：撤销正在飞行中的 refresh——回执必须被安全丢弃，不复活已撤销设备", async () => {
  const { ctx } = makeRuntime();
  const room = new RoomDO(ctx, {});
  store.ensureBusinessSchema(room.sql);
  const desktop = desktopSocket(room, ctx);
  const fakeDesktop = makeFakeDesktop(room, desktop);
  const paired = await pairDevice(room, ctx, desktop, fakeDesktop);
  const { mobileKPair, deviceId, subject } = paired;
  const persisted = paired.persisted;

  const remoteWs = await connectWithToken(room, ctx, persisted.access);

  // 手机发起 refresh——relay 转发给桌面，还没等桌面回执。
  const requestId = randomUUID();
  const refreshReqMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "token.refresh", session: deviceId, command_id: requestId };
  // R2 修正：token.refresh 请求明文 = JSON {"refresh_token":...}，不是裸字节。
  const { ct: reqCt, n: reqN } = await sealJson(mobileKPair, refreshReqMeta, { refresh_token: persisted.refresh });
  await send(room, remoteWs, { t: "token.refresh", request_id: requestId, ct: reqCt, n: reqN });
  assert.equal(desktop.sent.at(-1).t, "token.refresh.forward");
  assert.ok(store.getRefreshRequest(room.sql, requestId), "转发已登记 refresh_requests 行——回执飞行中");

  // ---- 关键一步：飞行中撤销该设备（用户在桌面点了"移除此设备"）----
  const revokeGeneration = fakeDesktop.nextGeneration();
  const revokeAck = await send(room, desktop, { t: "token.delete", subject, generation: revokeGeneration, close: true });
  assert.equal(revokeAck.t, "token.ack");
  assert.equal(revokeAck.result, "ok");
  assert.equal(store.getTokenSubject(room.sql, subject).state, "revoked");
  assert.equal(remoteWs.closed.length, 1, "close:true 必须强断该 subject 的全部在线 socket");

  // 桌面这时候才把（已经过时的）refresh 回执发出来——模拟"撤销指令与已经在
  // 处理中的旧请求赛跑"的真实时序（桌面判定撤销与处理中的 refresh 请求可能
  // 是并发的两件事）。
  const staleCapabilityTokenHex = randomHex64();
  const staleRefreshTokenHex = randomHex64();
  const okMeta = { v: PROTOCOL_VERSION, room: ROOM, epoch: 0, kind: "token.refresh.ok", session: deviceId, command_id: requestId };
  // R2 修正：token.refresh.ok 明文字段名是 capability_token/refresh_token，不是 access/refresh。
  const { ct: okCt, n: okN } = await sealJson(paired.desktopKPair, okMeta, {
    capability_token: staleCapabilityTokenHex, refresh_token: staleRefreshTokenHex,
  });
  // R5（双路审）：不满足于「send() 没抛异常」这种弱证据——逐一快照房间里
  // 每一条在线连接（desktop + 仍挂着的 pairing 连接 mobileWs + 已撤销但还
  // 没物理消失的 remoteWs）当前收到的帧数，回执发出后逐一核对没有任何一条
  // 多收到东西。
  const socketsBeforeStaleReceipt = ctx.getWebSockets().map((ws) => ({ ws, sentLength: ws.sent.length }));
  await send(room, desktop, { t: "token.refresh.ok", request_id: requestId, subject, generation: revokeGeneration, ct: okCt, n: okN });

  // deliverRefreshReceipt 谓词②「subject active」不成立——飞行中的回执必须被
  // 安全丢弃。remoteWs 仍是 close 状态——回执没有让它借尸还魂。
  assert.equal(remoteWs.closed.length, 1, "迟到的回执不能让已撤销设备的连接重新变回活跃");
  for (const { ws, sentLength } of socketsBeforeStaleReceipt) {
    assert.equal(ws.sent.length, sentLength, `房间里的每条连接在迟到回执发出前后收到的帧数必须逐一相等——没有任何连接凭空多收一帧（connection_id=${ws.deserializeAttachment()?.connection_id}）`);
  }
  // token.delete 本身不清 refresh_requests 行（源码实勘：handleTokenDelete
  // 只改 token_subjects/token_aliases，refresh_requests 的删除只发生在①
  // deliverRefreshReceipt 投递成功之后「投完删行」、②行 deadline 到期后的
  // 周期清理——两条路径这里都没走到：投递被谓词②拦在「投完删行」之前，
  // deadline 也还没到）。这行必须**依然存在**，不是被撤销顺手清掉——它会
  // 一直挂到 deadline 过期，由 §9.6 R1 的到期清理回收。
  assert.ok(
    store.getRefreshRequest(room.sql, requestId),
    "迟到回执被谓词②拦下（不到「投完删行」那一步）——refresh_requests 行必须仍然存在，不是被 token.delete 或这次失败投递清掉",
  );

  // 用飞行中回执里那份"新"凭据去连——必须连不上：它对应的 subject 已撤销，
  // 注册表里那个 token_hash 对应的行也一并被清空（§9.3 delete 语义：别名清空）。
  await withUpgradeRuntime(async () => {
    const response = await room.fetch(new Request(`https://relay.example/room/${ROOM}`, {
      headers: { Upgrade: "websocket", "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${staleCapabilityTokenHex}` },
    }));
    assert.equal(response.status, 401, "已撤销设备的（哪怕是飞行中回执里那份从未真正生效的）新凭据必须连不上");
  });

  // 用配对时拿到的旧 access 也必须连不上——设备已被彻底撤销，不是「只降级」。
  await withUpgradeRuntime(async () => {
    const response = await room.fetch(new Request(`https://relay.example/room/${ROOM}`, {
      headers: { Upgrade: "websocket", "Sec-WebSocket-Protocol": `agentloom-rc-v1, token.${persisted.access}` },
    }));
    assert.equal(response.status, 401, "撤销必须是终态——旧 access 不能continue 连上（哪怕它本来还没过期）");
  });
});
