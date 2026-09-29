// kdf.test.ts — TDD 覆盖 src/crypto/kdf.ts。
//
// ============================================================================
// 覆盖表
// ============================================================================
// - `remote-relay/fixtures/connect-kdf-v1.json`：3/3 条全打真函数 `deriveConnectTokenHex()`
//   （connect_token_hex 输出逐条比对；token_hash_hex 作为额外交叉校验——sha256(connect_token_hex
//   的 ASCII 字节)，不是 kdf.ts 导出的功能，只是顺手验证整条向量内部自洽，见下方
//   `sha256HexOfAscii` 私有 helper）。
// - `deriveKPair()`（T6A2 起）经共享 fixture `remote-relay/fixtures/crypto-kat-v1.json` 的
//   `k_pair_hkdf` 段消费——与 remote_crypto.rs 的 `derive_k_pair_matches_independent_kat_vector`
//   读同一份文件（RFC 7748 §5.2 官方向量接续 HKDF-SHA256，与 x25519.test.ts 的 `x25519_dh` 段共享
//   同一条 shared secret），两端不再各自维护内联复制。

import { describe, expect, it } from "vitest";
import { deriveConnectTokenHex, deriveKPair, hkdfSha256 } from "./kdf.ts";
import { loadFixture } from "../test-support/fixtures.ts";

interface ConnectKdfCase {
  name: string;
  pairing_token_hex: string;
  expect: { connect_token_hex: string; token_hash_hex: string };
}

async function sha256HexOfAscii(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

describe("deriveConnectTokenHex() · connect-kdf-v1 KAT (3/3)", () => {
  const cases = loadFixture<ConnectKdfCase[]>("connect-kdf-v1.json");

  it("fixture has exactly 3 cases (all required, none may be silently dropped)", () => {
    expect(cases).toHaveLength(3);
  });

  it.each(cases)("$name", async (fixtureCase) => {
    const derived = await deriveConnectTokenHex(fixtureCase.pairing_token_hex);
    expect(derived).toBe(fixtureCase.expect.connect_token_hex);

    // 额外自洽校验：token_hash_hex = sha256(connect_token_hex 的 ASCII 字节)（remote_crypto.rs 同款
    // 口径：relay 只存哈希，connect_token 本身不落库）。
    const hash = await sha256HexOfAscii(derived);
    expect(hash).toBe(fixtureCase.expect.token_hash_hex);
  });

  it("output never equals the input and does not preserve its prefix (KDF sanity, mirrors remote_crypto.rs)", async () => {
    for (const fixtureCase of cases) {
      const derived = await deriveConnectTokenHex(fixtureCase.pairing_token_hex);
      expect(derived).not.toBe(fixtureCase.pairing_token_hex);
      expect(derived.startsWith(fixtureCase.pairing_token_hex.slice(0, 16))).toBe(false);
    }
  });
});

interface KPairHkdfFixture {
  k_pair_hkdf: {
    shared_secret_hex: string;
    pairing_code: string;
    expected_k_pair_hex: string;
  };
}

// 来源：`remote-relay/fixtures/crypto-kat-v1.json` 的 `k_pair_hkdf` 段——`sharedSecretHex` =
// 同 fixture `x25519_dh.expected_shared_hex`（RFC 7748 §5.2 官方向量）；`expectedKPairHex` =
// 接续做 HKDF-SHA256 派生的输出（出处见该 fixture 的 `source` 字段：Node `crypto.hkdfSync` 与独立
// Python HMAC-SHA256 交叉复算过）。与 remote_crypto.rs 的
// `derive_k_pair_matches_independent_kat_vector` 读同一份文件，不是本单新造向量。
const kPairFixture = loadFixture<KPairHkdfFixture>("crypto-kat-v1.json");
if (
  !kPairFixture.k_pair_hkdf?.shared_secret_hex ||
  !kPairFixture.k_pair_hkdf?.pairing_code ||
  !kPairFixture.k_pair_hkdf?.expected_k_pair_hex
) {
  throw new Error("crypto-kat-v1.json fixture missing required k_pair_hkdf fields");
}
const K_PAIR_KAT = {
  sharedSecretHex: kPairFixture.k_pair_hkdf.shared_secret_hex,
  pairingToken: kPairFixture.k_pair_hkdf.pairing_code,
  expectedKPairHex: kPairFixture.k_pair_hkdf.expected_k_pair_hex,
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

describe("deriveKPair()", () => {
  it("matches the RFC 7748 + HKDF KAT vector cross-checked against remote_crypto.rs", async () => {
    const kPair = await deriveKPair(hexToBytes(K_PAIR_KAT.sharedSecretHex), K_PAIR_KAT.pairingToken);
    expect(bytesToHex(kPair)).toBe(K_PAIR_KAT.expectedKPairHex);
  });

  it("is salt-bound: a different pairing token yields a different K_pair for the same shared secret", async () => {
    const a = await deriveKPair(hexToBytes(K_PAIR_KAT.sharedSecretHex), "123456");
    const b = await deriveKPair(hexToBytes(K_PAIR_KAT.sharedSecretHex), "654321");
    expect(bytesToHex(a)).not.toBe(bytesToHex(b));
  });
});

describe("hkdfSha256() variant self-证：改 info 串会让输出偏离预期(反证 K_pair/connect KDF 互不可替换)", () => {
  it("K_pair info string produces a different output than the connect-token info string for identical IKM/salt", async () => {
    const ikm = hexToBytes(K_PAIR_KAT.sharedSecretHex);
    const salt = new TextEncoder().encode(K_PAIR_KAT.pairingToken);
    const withKPairInfo = await hkdfSha256(ikm, salt, new TextEncoder().encode("agentloom-rc-v1"), 32);
    const withConnectInfo = await hkdfSha256(ikm, salt, new TextEncoder().encode("agentloom-rc-connect-v1"), 32);
    expect(bytesToHex(withKPairInfo)).not.toBe(bytesToHex(withConnectInfo));
    // 且 withKPairInfo 必须仍然等于真正的 K_pair KAT 期望值——证明改 info 串确实是唯一变量。
    expect(bytesToHex(withKPairInfo)).toBe(K_PAIR_KAT.expectedKPairHex);
  });
});
