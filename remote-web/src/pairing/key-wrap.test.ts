// key-wrap.test.ts — TDD 覆盖 src/pairing/key-wrap.ts（K_room 包裹，AES-256-GCM 无 AAD）。
//
// 刻意不用 Node `Buffer`（项目未装 @types/node·任务书依赖清单只许新增 fake-indexeddb devDep）——
// 一律走 `crypto/bytes.ts` 的 base64 编解码 + `TextEncoder`/`TextDecoder`，跟生产代码同一套工具。

import { describe, expect, it } from "vitest";
import { base64ToBytes, bytesToBase64 } from "../crypto/bytes.ts";
import { KeyWrapError, unwrapKey, wrapKey } from "./key-wrap.ts";

function randomKey(): Uint8Array {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return bytes;
}

// TS DOM lib 的 BufferSource 要求 `Uint8Array<ArrayBuffer>`（不是更宽的 ArrayBufferLike）——
// 与 envelope.ts/key-wrap.ts 生产代码同款 workaround。
function toBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(bytes);
}

describe("wrapKey()/unwrapKey() · round trip", () => {
  it("unwraps exactly what was wrapped", async () => {
    const kek = randomKey();
    const kRoom = randomKey();
    const { ct, n } = await wrapKey(kek, kRoom);
    const unwrapped = await unwrapKey(kek, ct, n);
    expect(Array.from(unwrapped)).toEqual(Array.from(kRoom));
  });

  it("produces a fresh random nonce every call (no nonce reuse)", async () => {
    const kek = randomKey();
    const kRoom = randomKey();
    const a = await wrapKey(kek, kRoom);
    const b = await wrapKey(kek, kRoom);
    expect(a.n).not.toBe(b.n);
    expect(a.ct).not.toBe(b.ct);
  });

  it("wrong kek fails to unwrap (AEAD auth failure, not garbage output)", async () => {
    const { ct, n } = await wrapKey(randomKey(), randomKey());
    await expect(unwrapKey(randomKey(), ct, n)).rejects.toThrow(KeyWrapError);
  });

  it("tampered ciphertext fails to unwrap", async () => {
    const kek = randomKey();
    const { ct, n } = await wrapKey(kek, randomKey());
    const bytes = base64ToBytes(ct);
    bytes[0] ^= 0xff;
    const tamperedCt = bytesToBase64(bytes);
    await expect(unwrapKey(kek, tamperedCt, n)).rejects.toThrow(KeyWrapError);
  });

  it("rejects a nonce that decodes to something other than 12 bytes", async () => {
    const kek = randomKey();
    const { ct } = await wrapKey(kek, randomKey());
    const shortNonce = bytesToBase64(new Uint8Array(8));
    await expect(unwrapKey(kek, ct, shortNonce)).rejects.toThrow(KeyWrapError);
  });

  it("rejects non-canonical base64 input (mirrors envelope.ts::open's canonical-decode guard)", async () => {
    const kek = randomKey();
    const { n } = await wrapKey(kek, randomKey());
    await expect(unwrapKey(kek, "not-valid-base64!!", n)).rejects.toThrow(KeyWrapError);
  });
});

describe("wrapKey() no-AAD contract (distinguishes this from envelope.ts::seal)", () => {
  it("an independently hand-rolled crypto.subtle.encrypt with NO additionalData decrypts fine with unwrapKey — proves wrapKey is genuinely AAD-less, not just empty-AAD", async () => {
    const kek = randomKey();
    const kRoom = randomKey();
    const cryptoKey = await crypto.subtle.importKey("raw", toBufferSource(kek), "AES-GCM", false, ["encrypt"]);
    const nonce = new Uint8Array(12);
    crypto.getRandomValues(nonce);
    const ciphertext = await crypto.subtle.encrypt({ name: "AES-GCM", iv: toBufferSource(nonce) }, cryptoKey, toBufferSource(kRoom));
    const ctB64 = bytesToBase64(new Uint8Array(ciphertext));
    const nB64 = bytesToBase64(nonce);
    const unwrapped = await unwrapKey(kek, ctB64, nB64);
    expect(Array.from(unwrapped)).toEqual(Array.from(kRoom));
  });
});

// ============================================================================
// Enforce 32-byte key lengths (remote_crypto.rs:97/115).
// ============================================================================
// Rust's remote_crypto.rs::wrap_key/unwrap_key reject invalid lengths through `&[u8; 32]`;
// TypeScript needs equivalent runtime guards in key-wrap.ts because arrays have no fixed size.
// unwrapKey tests independently build no-AAD ciphertext: wrapKey rejects invalid keyBytes
// lengths, so it cannot produce ciphertext that authenticates but decrypts to an invalid length.
// Use the same raw crypto.subtle approach as the "no-AAD contract" test for arbitrary plaintext.

async function encryptRawNoAad(kek: Uint8Array, plaintext: Uint8Array): Promise<{ ct: string; n: string }> {
  const cryptoKey = await crypto.subtle.importKey("raw", toBufferSource(kek), "AES-GCM", false, ["encrypt"]);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ciphertext = await crypto.subtle.encrypt({ name: "AES-GCM", iv: toBufferSource(nonce) }, cryptoKey, toBufferSource(plaintext));
  return { ct: bytesToBase64(new Uint8Array(ciphertext)), n: bytesToBase64(nonce) };
}

describe("wrapKey() 拒绝非 32B keyBytes", () => {
  it.each([16, 31, 33])("rejects a %dB keyBytes input before ever touching crypto.subtle", async (length) => {
    await expect(wrapKey(randomKey(), new Uint8Array(length))).rejects.toThrow(KeyWrapError);
  });

  it("accepts exactly 32B (boundary sanity, not just off-by-one rejects)", async () => {
    await expect(wrapKey(randomKey(), new Uint8Array(32))).resolves.toBeDefined();
  });
});

describe("unwrapKey() 解密成功后仍强制明文恰 32B", () => {
  it.each([16, 31, 33])(
    "an AEAD-valid ciphertext whose plaintext is %dB (not 32B) is rejected with KeyWrapError, not returned",
    async (length) => {
      const kek = randomKey();
      const { ct, n } = await encryptRawNoAad(kek, new Uint8Array(length).fill(1));
      // 先确认这不是"解密本身失败"——AEAD 认证必须通过，只是长度不对，两种失败原因不能混淆着测。
      await expect(unwrapKey(kek, ct, n)).rejects.toThrow(KeyWrapError);
    },
  );

  it("a 32B plaintext (boundary sanity) round-trips through the same encryptRawNoAad path", async () => {
    const kek = randomKey();
    const plaintext = new Uint8Array(32).fill(1);
    const { ct, n } = await encryptRawNoAad(kek, plaintext);
    const unwrapped = await unwrapKey(kek, ct, n);
    expect(Array.from(unwrapped)).toEqual(Array.from(plaintext));
  });
});
