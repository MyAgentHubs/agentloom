// x25519.ts — C1 加密内核 · X25519 ECDH 双路实现（native WebCrypto + @noble/curves 回落）。
//
// 权威参照（只读对照，未改动）：app/src-tauri/src/remote_crypto.rs（`generate_x25519_keypair` /
// `derive_k_pair` 的 DH 前半段）；跨端 wire 形状照 remote-relay/test/s1ja-fake-mobile-e2e.test.js
// 的 `generateX25519KeyPair`/`deriveKPair` 参考实现（native WebCrypto 用法逐行对齐）。
//
// 双路选择依据 M2 C1 spec §2（v0.2 订正）：X25519 是 Safari 18.4 才原生支持的曲线，早于该版本
// 或不支持的运行时必须回落到纯 JS 的 @noble/curves——**运行时 feature probe**（真调
// `crypto.subtle.generateKey({name:"X25519"})`），不做 UA 嗅探。HKDF-SHA256 与 AES-256-GCM
// 均为广泛支持的 WebCrypto 原生算法，不需要双路（见 kdf.ts / envelope.ts）。
//
// 两路对外统一使用「裸 32 字节」形状（同 remote_crypto.rs），而不是把 native 路的私钥留成不透明
// CryptoKey——本模块产出的 secretKey 是配对握手用的临时 `remote_secret`，协议要求「用后即弃」
// （M2 C1 spec §3 步骤 7），不是需要长期不可导出存储的 K_room/rewrap key（那是 T6b 的活）；统一
// 裸字节形状换来两路可以对同一组 KAT 向量做逐字节比较（双路一致性测试的前提）。

import { x25519 as nobleX25519 } from "@noble/curves/ed25519.js";
import { bytesToBase64Url } from "./bytes.ts";

export type X25519Path = "native" | "noble";

export interface X25519KeyPair {
  /** 32 raw bytes · RFC 7748 clamped scalar. */
  secretKey: Uint8Array;
  /** 32 raw bytes. */
  publicKey: Uint8Array;
}

/**
 * RFC 7748 §6.1 非贡献性 DH 结果（全零共享秘密 / 低阶点）拒绝错误。native 与 noble 两路的底层
 * 库各自用不同方式报错（native 路 `crypto.subtle.deriveBits` 抛 `OperationError`；noble 路
 * `getSharedSecret` 抛普通 `Error`）——两路都被本模块统一 wrap 成这一个类型，调用方不需要
 * 关心走的是哪一路。
 */
export class NonContributoryError extends Error {
  constructor(options?: { cause?: unknown }) {
    super(
      "non-contributory Diffie-Hellman result (all-zero shared secret / low-order point) — refusing to derive a key from it",
      options,
    );
    this.name = "NonContributoryError";
  }
}

let cachedProbe: boolean | null = null;

/**
 * 运行时 feature probe：真调 `crypto.subtle.generateKey({name:"X25519"}, ...)` 探测原生支持，
 * 不做 UA 推断（M2 C1 spec §2 v0.2 订正的硬要求）。结果按进程缓存；测试需要强制走某一路时用
 * `setNativeSupportOverride()` 覆盖，不要指望每次都真的重新探测。
 */
export async function probe(): Promise<boolean> {
  if (cachedProbe !== null) {
    return cachedProbe;
  }
  cachedProbe = await probeUncached();
  return cachedProbe;
}

async function probeUncached(): Promise<boolean> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    return false;
  }
  try {
    await subtle.generateKey({ name: "X25519" }, true, ["deriveBits"]);
    return true;
  } catch {
    return false;
  }
}

/**
 * 测试专用：强制 `probe()`/`resolvePath()` 后续解析结果，绕过真实探测（或传 `null` 清缓存、
 * 恢复真探测）。生产代码不应调用这个函数。
 */
export function setNativeSupportOverride(value: boolean | null): void {
  cachedProbe = value;
}

export async function resolvePath(forcePath?: X25519Path): Promise<X25519Path> {
  if (forcePath) {
    return forcePath;
  }
  return (await probe()) ? "native" : "noble";
}

/**
 * 生成一对 X25519 密钥（CSPRNG + RFC 7748 clamp，@noble/curves `keygen()` 与桌面
 * `x25519_dalek::StaticSecret` clamp 规则一致）。密钥生成本身两路结果等价（都是裸随机字节 +
 * 同一套 clamp 规则），不需要按 probe 结果分叉实现。
 */
export function generateKeyPair(): X25519KeyPair {
  const { secretKey, publicKey } = nobleX25519.keygen();
  return { secretKey, publicKey };
}

/**
 * DH 共享秘密（`derive_k_pair` 的 DH 前半段，HKDF 部分留给 kdf.ts）。
 * @param options.path 强制走哪一路（测试用）；缺省按 `probe()` 结果自动选择。
 * @throws {NonContributoryError} 共享秘密全零或对端给的是已知低阶点。
 */
export async function deriveSharedSecret(
  secretKey: Uint8Array,
  publicKey: Uint8Array,
  options?: { path?: X25519Path },
): Promise<Uint8Array> {
  const path = await resolvePath(options?.path);
  const raw =
    path === "native"
      ? await deriveSharedSecretNative(secretKey, publicKey)
      : deriveSharedSecretNoble(secretKey, publicKey);
  if (isAllZero(raw)) {
    throw new NonContributoryError();
  }
  return raw;
}

function deriveSharedSecretNoble(secretKey: Uint8Array, publicKey: Uint8Array): Uint8Array {
  try {
    return nobleX25519.getSharedSecret(secretKey, publicKey);
  } catch (cause) {
    // noble 自己已经会拒绝全零/已知低阶点（v2.3.0 起 getSharedSecret 内置低阶点黑名单），这里只是
    // 把它的错误统一成本模块的 NonContributoryError，不重复实现黑名单。
    throw new NonContributoryError({ cause });
  }
}

async function deriveSharedSecretNative(
  secretKey: Uint8Array,
  publicKey: Uint8Array,
): Promise<Uint8Array> {
  const subtle = requireSubtle();
  const privateCryptoKey = await importNativePrivateKey(secretKey);
  const publicCryptoKey = await subtle.importKey("raw", toBufferSource(publicKey), { name: "X25519" }, false, []);
  try {
    const bits = await subtle.deriveBits({ name: "X25519", public: publicCryptoKey }, privateCryptoKey, 256);
    return new Uint8Array(bits);
  } catch (cause) {
    // native 路对全零/已知低阶点对端公钥的行为是 `deriveBits` 直接抛 OperationError（Node 26 +
    // Chromium/WebKit 的 X25519 实现均已实测验证——见 T6A worker 报告),同样统一成 NonContributoryError。
    throw new NonContributoryError({ cause });
  }
}

/**
 * WebCrypto 没有给 X25519 私钥定义 "raw" 导入/导出格式（规范只允许公钥用 raw；私钥只能走
 * pkcs8/jwk）——为了让 native 路吃下一个外部给定的裸 32 字节标量（KAT 向量、或复用
 * `generateKeyPair()` 产出的 noble 路密钥），这里用 noble 算出配套公钥（RFC 7748 clamp 与
 * native/x25519_dalek 逐字节一致，已用共享 KAT 向量交叉验证），拼成 JWK {d, x} 再导入——
 * WebCrypto 导入 JWK 时会自己校验 d 算出的公钥是否等于 x，两者不匹配会直接报错，所以这里传入
 * 拼错的 x 不会被静默接受。
 */
async function importNativePrivateKey(secretKey: Uint8Array): Promise<CryptoKey> {
  const subtle = requireSubtle();
  const publicKey = nobleX25519.getPublicKey(secretKey);
  const jwk: JsonWebKey = {
    kty: "OKP",
    crv: "X25519",
    d: bytesToBase64Url(secretKey),
    x: bytesToBase64Url(publicKey),
    ext: true,
  };
  return subtle.importKey("jwk", jwk, { name: "X25519" }, false, ["deriveBits"]);
}

function requireSubtle(): SubtleCrypto {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    throw new Error("WebCrypto SubtleCrypto is unavailable in this runtime");
  }
  return subtle;
}

function isAllZero(bytes: Uint8Array): boolean {
  return bytes.every((byte) => byte === 0);
}

// TS's DOM lib types `BufferSource` parameters more strictly about the underlying ArrayBuffer
// than a plain `Uint8Array` guarantees (it may be backed by a SharedArrayBuffer/resizable
// buffer); narrowing through a copy keeps this file honest under `strict` without `any`.
function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}
