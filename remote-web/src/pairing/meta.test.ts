// meta.test.ts — TDD 覆盖 src/pairing/meta.ts + frames.ts::parsePairAcceptTokens。
//
// ============================================================================
// 覆盖表
// ============================================================================
// - `pairReadyMeta`/`pairAcceptTokensMeta` 的 AAD 拼串对 remote-relay/fixtures/wire-v1.json 的
//   `aad_kat_pair_ready`/`aad_kat_pair_accept_tokens` 两条样张（`buildAAD` 来自 src/crypto/
//   envelope.ts，T6a 已 KAT 验证过——这里只验证本单新写的 meta 字段组合喂给它产出正确结果）。
// - `pair_accept_tokens` 字节级契约：直接用 fixture 的 key_hex/n_b64/ct_b64 走生产
//   `open()`+`parsePairAcceptTokens()`，断言解出恰好等于 fixture 记录的明文
//   `{capability_token, refresh_token}`（`aad_kat_pair_ready` 的 plaintext 按 fixture 自己的 note
//   明写"非本单契约"，不用它验证 pair.ready 的明文形状——那个契约来自
//   remote_pairing.rs::seal_pair_ready，在 pairing-session.test.ts 里用真实 K_pair 往返验证）。

import { describe, expect, it } from "vitest";
import { buildAAD, open } from "../crypto/envelope.ts";
import { loadFixture } from "../test-support/fixtures.ts";
import { parsePairAcceptTokens } from "./frames.ts";
import { helloEnvelopeMeta, pairAcceptTokensMeta, pairDoneConfirmMeta, pairReadyMeta } from "./meta.ts";

interface AadKatCase {
  name: string;
  layer: string;
  device_id: string;
  meta: { v: number; room: string; epoch: number; kind: string; session: string | null; command_id: string | null };
  expect: { aad: string };
  kat: { key_hex: string; n_b64: string; ct_b64: string; plaintext: string };
}

const wireFixture = loadFixture<AadKatCase[]>("wire-v1.json");
const ROOM = "0123456789abcdef0123456789abcdef";

function findCase(name: string): AadKatCase {
  const found = wireFixture.find((c) => c.name === name);
  if (!found) throw new Error(`fixture case "${name}" not found in wire-v1.json`);
  return found;
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

describe("pairReadyMeta() · AAD 拼串对齐 wire-v1.json aad_kat_pair_ready", () => {
  it("matches fixture's expected AAD string", () => {
    const c = findCase("aad_kat_pair_ready");
    const meta = pairReadyMeta(c.meta.room, c.device_id);
    expect(meta).toEqual(c.meta);
    expect(buildAAD(meta)).toBe(c.expect.aad);
  });
});

describe("pairAcceptTokensMeta() · AAD 拼串 + 字节级明文契约对齐 wire-v1.json aad_kat_pair_accept_tokens", () => {
  const c = findCase("aad_kat_pair_accept_tokens");

  it("AAD string matches fixture", () => {
    const meta = pairAcceptTokensMeta(c.meta.room, c.device_id);
    expect(meta).toEqual(c.meta);
    expect(buildAAD(meta)).toBe(c.expect.aad);
  });

  it("open() + parsePairAcceptTokens() decrypt the fixture ciphertext into the exact recorded plaintext", async () => {
    const key = hexToBytes(c.kat.key_hex);
    const meta = pairAcceptTokensMeta(c.meta.room, c.device_id);
    const plaintext = await open(key, meta, c.kat.ct_b64, c.kat.n_b64);
    expect(new TextDecoder().decode(plaintext)).toBe(c.kat.plaintext);
    const tokens = parsePairAcceptTokens(plaintext);
    const expected = JSON.parse(c.kat.plaintext);
    expect(tokens).toEqual(expected);
  });

  it("mutation self-证：AAD kind 改一个字符会让 fixture 密文解不开（证明 kind 字面量确实被校验进 AAD，不是摆设）", async () => {
    const key = hexToBytes(c.kat.key_hex);
    const tamperedMeta = { ...pairAcceptTokensMeta(c.meta.room, c.device_id), kind: "pair-accept-tokenz" };
    await expect(open(key, tamperedMeta, c.kat.ct_b64, c.kat.n_b64)).rejects.toThrow();
  });
});

describe("helloEnvelopeMeta() / pairDoneConfirmMeta() · 字段组合(remote_pairing.rs 对照，无独立 fixture 样张——由 pairing-session.test.ts 端到端验证)", () => {
  it("helloEnvelopeMeta uses kind='control', session=null, command_id=null (remote_pairing.rs::pairing_envelope_meta)", () => {
    expect(helloEnvelopeMeta(ROOM)).toEqual({ v: 1, room: ROOM, epoch: 0, kind: "control", session: null, command_id: null });
  });

  it("pairDoneConfirmMeta uses kind='pair-confirm', session=deviceId (remote_pairing.rs::pair_done_confirm_meta)", () => {
    expect(pairDoneConfirmMeta(ROOM, "device-x")).toEqual({
      v: 1,
      room: ROOM,
      epoch: 0,
      kind: "pair-confirm",
      session: "device-x",
      command_id: null,
    });
  });

  it("all four meta kinds are pairwise distinct (AAD kind label is the only thing separating the four ciphertexts under related keys)", () => {
    const kinds = [
      helloEnvelopeMeta(ROOM).kind,
      pairDoneConfirmMeta(ROOM, "d").kind,
      pairReadyMeta(ROOM, "d").kind,
      pairAcceptTokensMeta(ROOM, "d").kind,
    ];
    expect(new Set(kinds).size).toBe(kinds.length);
  });
});
