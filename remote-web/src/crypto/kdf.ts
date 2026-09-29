// kdf.ts — C1 加密内核 · HKDF-SHA256（含 K_pair 与 connect-token 两个具体推导）。
//
// 权威参照（只读对照，未改动）：app/src-tauri/src/remote_crypto.rs 的 `derive_k_pair` /
// `derive_connect_token`；KAT 向量 = remote-relay/fixtures/connect-kdf-v1.json（3 条·§9.5）。
// info/salt 字符串逐字节照抄该文件，不许自创。
//
// HKDF-SHA256 是广泛支持的 WebCrypto 原生算法（M2 C1 spec §2：「HKDF-SHA256、AES-256-GCM
// 均为 WebCrypto 原生」）——不像 X25519 需要 native/noble 双路，这里只有一条实现路径。

import { utf8Bytes, bytesToHex } from "./bytes.ts";

/** M0 §5：K_pair 推导用的 HKDF info 字符串。 */
const K_PAIR_INFO = utf8Bytes("agentloom-rc-v1");

/** M0 §9.5：connect_token 推导用的 HKDF info 字符串（与 K_pair 的 info 刻意不同/不复用）。 */
const CONNECT_TOKEN_INFO = utf8Bytes("agentloom-rc-connect-v1");

const EMPTY_SALT = new Uint8Array(0);

/** 通用 HKDF-SHA256（RFC 5869），`length` 单位为字节。 */
export async function hkdfSha256(
  ikm: Uint8Array,
  salt: Uint8Array,
  info: Uint8Array,
  length: number,
): Promise<Uint8Array> {
  const subtle = requireSubtle();
  const key = await subtle.importKey("raw", toBufferSource(ikm), "HKDF", false, ["deriveBits"]);
  const bits = await subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt: toBufferSource(salt), info: toBufferSource(info) },
    key,
    length * 8,
  );
  return new Uint8Array(bits);
}

/**
 * K_pair = HKDF-SHA256(IKM=X25519 共享秘密, salt=pairing_token 的 ASCII 字节, info="agentloom-rc-v1")。
 * `pairingToken` 就是 M0 §5「配对码歧义钉死」条目里那个唯一的秘密——二维码 payload 里的
 * `pairing_token`，同一个字符串既是这里的 HKDF salt，也是 `deriveConnectTokenHex` 的 IKM。
 */
export async function deriveKPair(sharedSecret: Uint8Array, pairingToken: string): Promise<Uint8Array> {
  return hkdfSha256(sharedSecret, utf8Bytes(pairingToken), K_PAIR_INFO, 32);
}

/**
 * connect_token = HKDF-SHA256(IKM=pairing_token 的 64 字节 ASCII hex 原文, salt=空,
 * info="agentloom-rc-connect-v1")，64 位小写 hex 编码（M0 §9.5）。
 */
export async function deriveConnectTokenHex(pairingTokenHex: string): Promise<string> {
  const raw = await hkdfSha256(utf8Bytes(pairingTokenHex), EMPTY_SALT, CONNECT_TOKEN_INFO, 32);
  return bytesToHex(raw);
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
