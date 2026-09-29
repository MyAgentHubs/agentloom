// envelope.ts — C1 加密内核 · AES-256-GCM 信封 seal/open + AAD 拼串。
//
// 权威参照（只读对照，未改动）：
//   - AAD 拼接顺序 = M0 §1「v|room|epoch|kind|session|command_id」，逐字节照
//     remote-relay/src/envelope.js 的 `buildAAD()` 与 app/src-tauri/src/remote_crypto.rs 的
//     `build_aad()`（三端同一公式，null/undefined 字段拼空串）。
//   - seal/open = 同文件 remote_crypto.rs 的 `seal`/`open`：AES-256-GCM，12 字节随机 nonce，
//     AAD 认证但不加密。
//
// **本文件的范围边界（有意收窄，见 T6A worker 报告「偏离说明」）**：只做 AAD 拼串 + AEAD
// seal/open。`session`/`command_id` 的 wire grammar 校验（长度上限、禁止 `|`、按 kind 决定是否
// 必填等——relay `envelope.js::validateEnvelope` 那一层)不在这里重复实现：AAD 拼串是纯字符串
// 运算，对语法非法的字段一样能算出确定的 AAD（这正是下面测试用 wire-v1 里那些
// `expect.valid:false` 样张的方式——只验它们的 `expect.aad`，不代表本模块会「拒绝」它们）；
// 真正的语法校验属于构造/解析完整信封的那一层（T6d1 事件内核 / T6f3 指令面），要避免和 relay
// 各写一套、悄悄产生分歧。

import { bytesToBase64, decodeCanonicalBase64 } from "./bytes.ts";

export interface EnvelopeMeta {
  v: number;
  room: string;
  epoch: number;
  kind: string;
  /** 会话索引流（session=null）用 `null`；AAD 拼串里 null/undefined 都编码成空串。 */
  session?: string | null;
  /** kind=event/live/presence 必须是 null；kind=input/control 必须非空（wire grammar，本文件不校验）。 */
  command_id?: string | null;
}

const NONCE_LEN = 12;

/** 解密/认证失败（密钥错、AAD 不匹配、密文被篡改、nonce 错……AEAD 不区分具体原因）。 */
export class EnvelopeDecryptError extends Error {
  constructor(options?: { cause?: unknown }) {
    super("AES-256-GCM decryption/authentication failed", options);
    this.name = "EnvelopeDecryptError";
  }
}

/**
 * AAD = `v|room|epoch|kind|session|command_id` 拼接，null/undefined 字段编码为空字符串
 * （M0 §1 v0.2 订正版·三端统一公式）。纯字符串运算，不做任何字段合法性校验。
 */
export function buildAAD(meta: EnvelopeMeta): string {
  const part = (value: string | number | null | undefined): string =>
    value === null || value === undefined ? "" : String(value);
  return [
    part(meta.v),
    part(meta.room),
    part(meta.epoch),
    part(meta.kind),
    part(meta.session),
    part(meta.command_id),
  ].join("|");
}

/**
 * AES-256-GCM 加密；`key` 是 `Uint8Array`（必须 32 字节，走 `importAesKey` 原路径）或已导入的
 * `CryptoKey`（直接用，跳过导入——支持 non-extractable key，见下方 `resolveAesKey`）。返回
 * base64 编码的密文与 12 字节随机 nonce。
 */
export async function seal(
  key: Uint8Array | CryptoKey,
  meta: EnvelopeMeta,
  plaintext: Uint8Array,
): Promise<{ ct: string; n: string }> {
  const cryptoKey = await resolveAesKey(key, ["encrypt"]);
  const nonce = randomNonce();
  const aad = new TextEncoder().encode(buildAAD(meta));
  const ciphertext = await requireSubtle().encrypt(
    { name: "AES-GCM", iv: nonce, additionalData: aad },
    cryptoKey,
    toBufferSource(plaintext),
  );
  return { ct: bytesToBase64(new Uint8Array(ciphertext)), n: bytesToBase64(nonce) };
}

/**
 * AES-256-GCM 解密；AAD 由 `meta` 按 `buildAAD` 重建——`meta` 与加密时不一致会导致认证失败。
 *
 * **M0 加固（remote_crypto.rs:176 `decode_ciphertext_and_nonce` 同款口径）**：`ctBase64`/
 * `nonceBase64` 必须是**严格 canonical** 的 base64（见 `bytes.ts::decodeCanonicalBase64` 的
 * 「decode→re-encode 逐字符相等」判据），且解出的 nonce 必须恰好 12 字节——桌面 Rust 端用
 * `try_into::<[u8; 12]>()` 强制同样的长度；这里如果不强制，AES-GCM 的 WebCrypto 实现会接受任意
 * 长度的 IV（GCM 规范允许变长 IV），导致 native 路能悄悄解出一个桌面端会直接拒绝的畸形信封——
 * 两端口径分叉是真实的安全洞（截断/膨胀 nonce 改变有效 IV，破坏「每消息唯一 nonce」的前提）。
 * 两类拒绝都不 `decrypt()` 直接返回，走跟认证失败**同一个错误路径**（`EnvelopeDecryptError`，
 * 不带更细的原因）——不能让网络攻击者从错误形状里探测出「是 base64 不对还是密钥不对」。
 *
 * `key` 同 `seal`：`Uint8Array` 走原路径，`CryptoKey`（含 non-extractable，如 `key-store.ts` 落储
 * 的 K_room）直接用，跳过导入。
 */
export async function open(
  key: Uint8Array | CryptoKey,
  meta: EnvelopeMeta,
  ctBase64: string,
  nonceBase64: string,
): Promise<Uint8Array> {
  const cryptoKey = await resolveAesKey(key, ["decrypt"]);
  const ciphertextBytes = decodeCanonicalBase64(ctBase64);
  const nonceBytes = decodeCanonicalBase64(nonceBase64);
  if (ciphertextBytes === null || nonceBytes === null || nonceBytes.length !== NONCE_LEN) {
    throw new EnvelopeDecryptError();
  }
  const aad = new TextEncoder().encode(buildAAD(meta));
  try {
    const plaintext = await requireSubtle().decrypt(
      { name: "AES-GCM", iv: toBufferSource(nonceBytes), additionalData: aad },
      cryptoKey,
      toBufferSource(ciphertextBytes),
    );
    return new Uint8Array(plaintext);
  } catch (cause) {
    throw new EnvelopeDecryptError({ cause });
  }
}

/**
 * `key` 是 `CryptoKey`（如 `store/key-store.ts` 落储的 non-extractable K_room）就直接返回——原始
 * 字节永久不可取回，WebCrypto 原生支持用 non-extractable key 做 encrypt/decrypt，无需（也无法）
 * 重新 `importKey`。`key` 是 `Uint8Array` 走原 `importAesKey` 路径，逐字节行为不变。若 `CryptoKey`
 * 的 `usages` 缺对应操作，交由底层 `subtle.encrypt`/`decrypt` 自然报错（不在这里吞或转译）。
 */
async function resolveAesKey(key: Uint8Array | CryptoKey, usages: KeyUsage[]): Promise<CryptoKey> {
  if (key instanceof CryptoKey) {
    return key;
  }
  return importAesKey(key, usages);
}

async function importAesKey(key: Uint8Array, usages: KeyUsage[]): Promise<CryptoKey> {
  if (key.length !== 32) {
    throw new Error(`AES-256-GCM key must be 32 bytes, got ${key.length}`);
  }
  return requireSubtle().importKey("raw", toBufferSource(key), "AES-GCM", false, usages);
}

function randomNonce(): Uint8Array<ArrayBuffer> {
  const nonce = new Uint8Array(NONCE_LEN);
  requireCrypto().getRandomValues(nonce);
  return nonce;
}

function requireCrypto(): Crypto {
  if (!globalThis.crypto) {
    throw new Error("WebCrypto `crypto` global is unavailable in this runtime");
  }
  return globalThis.crypto;
}

function requireSubtle(): SubtleCrypto {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    throw new Error("WebCrypto SubtleCrypto is unavailable in this runtime");
  }
  return subtle;
}

function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}
