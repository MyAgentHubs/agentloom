// key-wrap.ts — T6b · pair.accept 的 K_room 包裹（AES-256-GCM，**无 AAD**）。
//
// 权威参照（只读对照，未改动）：app/src-tauri/src/remote_crypto.rs 的 `wrap_key`/`unwrap_key`
// （仅用于 pair.accept 的 k_room_ct/n）；remote-relay/test/s1ja-fake-mobile-e2e.test.js 的
// `wrapKey()`/`unwrapKey()`（124-139 行，同一 native WebCrypto 用法，逐行对应）。
//
// **不放进 src/crypto/、不复用 envelope.ts::seal/open（本单 forbidden：crypto/ 只读消费，一行不许
// 改）**：envelope.ts 的 seal/open 恒定传 `additionalData: buildAAD(meta)`，即使 meta 六个字段全
// 是 null/undefined，`buildAAD` 也会拼出非空字符串 `"|||||"`（六个空段落 join `|`），不是「没有
// AAD」。remote_crypto.rs::wrap_key 对 crypto.subtle.encrypt 完全不传 additionalData 参数——两者是
// 不同的 AEAD 输入，复用 seal/open 会让本模块悄悄偏离桌面的真实密文（decrypt 会失败或者产出错误
// 密文），故独立实现，不共用 envelope.ts 的 AAD 拼串路径。

import { bytesToBase64, decodeCanonicalBase64 } from "../crypto/bytes.ts";

const NONCE_LEN = 12;
const KEY_LEN = 32;

/**
 * All wrapping/unwrapping failures use one error type: non-32-byte kek, keyBytes, or
 * decrypted plaintext, wrong keys, tampered ciphertext, and invalid nonces. As in
 * envelope.ts, AEAD failures and length checks share this contract instead of bare Error.
 * pairing-session.ts::handleAccept catches failures from this module in one try block.
 * Rejecting invalid plaintext length here prevents it from reaching seal(kRoom, ...)
 * outside that try block and leaking an uncaught exception after successful decryption.
 * A shared error type keeps this contract explicit; validation must stay inside it.
 */
export class KeyWrapError extends Error {
  constructor(options?: { cause?: unknown }) {
    super("AES-256-GCM key wrap/unwrap failed", options);
    this.name = "KeyWrapError";
  }
}

/** 用 `kek`（32 字节）包裹 `keyBytes`（K_room，必须恰 32 字节——对齐 remote_crypto.rs::wrap_key
 *  的 `key: &[u8; 32]` 类型级约束，TS 没有定长数组类型，这里补运行时闸）；返回 base64 密文与 12
 *  字节随机 nonce。 */
export async function wrapKey(kek: Uint8Array, keyBytes: Uint8Array): Promise<{ ct: string; n: string }> {
  if (keyBytes.length !== KEY_LEN) {
    throw new KeyWrapError();
  }
  const cryptoKey = await importKek(kek, ["encrypt"]);
  const nonce = randomNonce();
  const ciphertext = await requireSubtle().encrypt({ name: "AES-GCM", iv: nonce }, cryptoKey, toBufferSource(keyBytes));
  return { ct: bytesToBase64(new Uint8Array(ciphertext)), n: bytesToBase64(nonce) };
}

/**
 * 解包裹 `kek` 包裹的密钥。同 envelope.ts::open 的加固口径：ct/n 必须是严格 canonical base64，
 * 解出的 nonce 必须恰好 12 字节，两类拒绝都并入同一个 `KeyWrapError`（不泄露失败原因分类）。
 * **解密成功后再强制明文恰 32 字节**（对齐 remote_crypto.rs::unwrap_key 的
 * `plaintext.as_slice().try_into().map_err(|_| CryptoError::BadLength)?`）——AEAD 认证通过只保证
 * "密文没被篡改"，不保证"明文长度是调用方期待的那个"；这里不检查的话，一个解密成功但长度错的
 * K_room 会被当作合法值一路传回调用方，等下游拿它去 `seal`/`importKey` 时才在这个函数的调用栈之
 * 外炸出去。
 */
export async function unwrapKey(kek: Uint8Array, ctBase64: string, nonceBase64: string): Promise<Uint8Array> {
  const cryptoKey = await importKek(kek, ["decrypt"]);
  const ciphertextBytes = decodeCanonicalBase64(ctBase64);
  const nonceBytes = decodeCanonicalBase64(nonceBase64);
  if (ciphertextBytes === null || nonceBytes === null || nonceBytes.length !== NONCE_LEN) {
    throw new KeyWrapError();
  }
  let plaintext: Uint8Array;
  try {
    const decrypted = await requireSubtle().decrypt({ name: "AES-GCM", iv: toBufferSource(nonceBytes) }, cryptoKey, toBufferSource(ciphertextBytes));
    plaintext = new Uint8Array(decrypted);
  } catch (cause) {
    throw new KeyWrapError({ cause });
  }
  if (plaintext.length !== KEY_LEN) {
    throw new KeyWrapError();
  }
  return plaintext;
}

async function importKek(kek: Uint8Array, usages: KeyUsage[]): Promise<CryptoKey> {
  if (kek.length !== KEY_LEN) {
    throw new KeyWrapError();
  }
  return requireSubtle().importKey("raw", toBufferSource(kek), "AES-GCM", false, usages);
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
