// x25519.test.ts — TDD 覆盖 src/crypto/x25519.ts。
//
// ============================================================================
// 覆盖表（T6A worker 任务书 §2 硬要求：wire-v1 消费哪些层、不适用的层归属谁）
// ============================================================================
// 本文件不直接消费 wire-v1.json（那份 fixture 只描述信封字段/AAD，见 envelope.test.ts）；
// X25519 的跨端 KAT 向量（T6A2 起）经共享 fixture `remote-relay/fixtures/crypto-kat-v1.json` 的
// `x25519_dh` 段消费——与 remote_crypto.rs 的 `derive_k_pair_matches_independent_kat_vector`
// 读同一份文件，不再各自维护内联复制。connect-kdf-v1.json 的 3 条 KAT 在 kdf.test.ts 里 consumed
// （那是 HKDF 层，不是 DH 层）。
//
// 双路一致性（native WebCrypto + noble）在本文件对同一批断言重复跑两遍：`describe.each(["noble",
// "native"])`。native 路在 Node 26 上真跑通（已实机探测确认，见下方 probe 断言与 worker 报告
// ⑤）；理论上仍保留 native-unsupported 时的 skip 分支，避免这份测试在未来某个不支持 X25519
// native 的运行时里假红。

import { describe, expect, it, afterEach } from "vitest";
import {
  NonContributoryError,
  deriveSharedSecret,
  generateKeyPair,
  probe,
  resolvePath,
  setNativeSupportOverride,
  type X25519Path,
} from "./x25519.ts";
import { loadFixture } from "../test-support/fixtures.ts";

interface CryptoKatFixture {
  x25519_dh: {
    my_secret_hex: string;
    their_public_hex: string;
    expected_shared_hex: string;
  };
}

// 来源：`remote-relay/fixtures/crypto-kat-v1.json` 的 `x25519_dh` 段（RFC 7748 §5.2 官方
// published X25519 DH 测试向量，出处见该 fixture 的 `source` 字段）——与
// `app/src-tauri/src/remote_crypto.rs` 的 `derive_k_pair_matches_independent_kat_vector` 读同一份
// 文件，两端不再各自维护内联复制（T6A2）。
const cryptoKat = loadFixture<CryptoKatFixture>("crypto-kat-v1.json");
if (
  !cryptoKat.x25519_dh?.my_secret_hex ||
  !cryptoKat.x25519_dh?.their_public_hex ||
  !cryptoKat.x25519_dh?.expected_shared_hex
) {
  throw new Error("crypto-kat-v1.json fixture missing required x25519_dh fields");
}
const RFC7748_KAT = {
  mySecretHex: cryptoKat.x25519_dh.my_secret_hex,
  theirPublicHex: cryptoKat.x25519_dh.their_public_hex,
  expectedSharedHex: cryptoKat.x25519_dh.expected_shared_hex,
};

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

afterEach(() => {
  setNativeSupportOverride(null);
});

describe("probe()", () => {
  it("actually probes crypto.subtle.generateKey (real result on this machine, not a UA guess)", async () => {
    setNativeSupportOverride(null);
    const supported = await probe();
    // 本机(vitest node 环境) node 26 已实测支持 native X25519——断言必须是 true，false 会说明
    // probe() 本身坏了(比如 catch 吞掉了非探测相关的错误)。
    expect(supported).toBe(true);
  });

  it("caches the result until overridden", async () => {
    setNativeSupportOverride(true);
    expect(await probe()).toBe(true);
    setNativeSupportOverride(false);
    expect(await probe()).toBe(false);
  });
});

describe("resolvePath()", () => {
  it("honors an explicit forcePath without consulting probe()", async () => {
    setNativeSupportOverride(false);
    expect(await resolvePath("native")).toBe("native");
    setNativeSupportOverride(true);
    expect(await resolvePath("noble")).toBe("noble");
  });

  it("falls back to probe() when no path is forced", async () => {
    setNativeSupportOverride(true);
    expect(await resolvePath()).toBe("native");
    setNativeSupportOverride(false);
    expect(await resolvePath()).toBe("noble");
  });
});

describe("generateKeyPair()", () => {
  it("produces 32-byte secret/public keys that are not trivially zero", () => {
    const { secretKey, publicKey } = generateKeyPair();
    expect(secretKey).toHaveLength(32);
    expect(publicKey).toHaveLength(32);
    expect(secretKey.every((b) => b === 0)).toBe(false);
    expect(publicKey.every((b) => b === 0)).toBe(false);
  });

  it("never repeats a keypair across calls (CSPRNG sanity)", () => {
    const a = generateKeyPair();
    const b = generateKeyPair();
    expect(bytesToHex(a.secretKey)).not.toBe(bytesToHex(b.secretKey));
    expect(bytesToHex(a.publicKey)).not.toBe(bytesToHex(b.publicKey));
  });
});

// ---------------------------------------------------------------------------
// 双路一致性：同一组断言在 native / noble 各跑一遍。
// ---------------------------------------------------------------------------
const paths: X25519Path[] = ["noble", "native"];

describe.each(paths)("deriveSharedSecret() · path=%s", (path) => {
  // 硬要求(worker 任务书 §2):本机不支持 native 时必须显式 skip、不能静默假绿——用 vitest 的
  // 动态 `ctx.skip(condition, note)`（真跑一次 probe() 判定，而不是硬编码的 `describe.skipIf`）。
  async function skipIfNativeUnsupported(ctx: { skip: (condition: boolean, note?: string) => void }): Promise<void> {
    if (path !== "native") return;
    ctx.skip(!(await probe()), "native X25519 (crypto.subtle) unsupported on this runtime");
  }

  it("matches the RFC 7748 §5.2 KAT vector (also cross-checked against remote_crypto.rs)", async (ctx) => {
    await skipIfNativeUnsupported(ctx);
    const shared = await deriveSharedSecret(
      hexToBytes(RFC7748_KAT.mySecretHex),
      hexToBytes(RFC7748_KAT.theirPublicHex),
      { path },
    );
    expect(bytesToHex(shared)).toBe(RFC7748_KAT.expectedSharedHex);
  });

  it("is symmetric between two freshly generated keypairs", async (ctx) => {
    await skipIfNativeUnsupported(ctx);
    const alice = generateKeyPair();
    const bob = generateKeyPair();
    const aliceShared = await deriveSharedSecret(alice.secretKey, bob.publicKey, { path });
    const bobShared = await deriveSharedSecret(bob.secretKey, alice.publicKey, { path });
    expect(bytesToHex(aliceShared)).toBe(bytesToHex(bobShared));
  });

  it("rejects an all-zero peer public key as non-contributory (RFC 7748 §6.1)", async (ctx) => {
    await skipIfNativeUnsupported(ctx);
    const me = generateKeyPair();
    const allZeroPeerPublic = new Uint8Array(32);
    await expect(deriveSharedSecret(me.secretKey, allZeroPeerPublic, { path })).rejects.toThrow(
      NonContributoryError,
    );
  });

  it("rejects a known low-order point (u=1) as non-contributory", async (ctx) => {
    await skipIfNativeUnsupported(ctx);
    const me = generateKeyPair();
    const lowOrderPeerPublic = new Uint8Array(32);
    lowOrderPeerPublic[0] = 1;
    await expect(deriveSharedSecret(me.secretKey, lowOrderPeerPublic, { path })).rejects.toThrow(
      NonContributoryError,
    );
  });
});

describe("deriveSharedSecret() default path selection", () => {
  it("uses native when probe() resolves true and noble when it resolves false, without changing the output for a fixed KAT vector", async () => {
    setNativeSupportOverride(true);
    const native = await deriveSharedSecret(hexToBytes(RFC7748_KAT.mySecretHex), hexToBytes(RFC7748_KAT.theirPublicHex));
    setNativeSupportOverride(false);
    const noble = await deriveSharedSecret(hexToBytes(RFC7748_KAT.mySecretHex), hexToBytes(RFC7748_KAT.theirPublicHex));
    expect(bytesToHex(native)).toBe(RFC7748_KAT.expectedSharedHex);
    expect(bytesToHex(noble)).toBe(RFC7748_KAT.expectedSharedHex);
  });
});
