// envelope.test.ts — TDD 覆盖 src/crypto/envelope.ts。
//
// ============================================================================
// 覆盖表（T6A worker 任务书 §2 硬要求：wire-v1.json 139 条样张里哪些被本单消费、哪些不适用、
// 不适用的归属谁——不许静默跳层）
// ============================================================================
// wire-v1.json 139 条里，本文件消费两层，共 14 条不重复样张：
//
// 1)「buildAAD 层」——10 条，凡样张同时有 `envelope` 对象与**非 null 的** `expect.aad` 字符串的都
//    算：input_with_command_id / input_command_id_non_ascii_utf8_bytes_at_126_accepted /
//    control_with_command_id / event_normal / live_normal / presence_normal /
//    event_client_msg_id_at_64_bytes_accepted / kat_control_stop（与第 2 层重叠，见下）/
//    reply_with_command_id / reply_with_null_session are positive reply fixtures added alongside input/control AAD handling to ensure consistent coverage.
//    `reply` 与 input/control 同构走信封顶层 command_id 入 AAD，AAD 拼串规则对它没有任何特殊
//    分支，天然落进本层同一个通用谓词，机械纳入不代表本单主动为 `reply` 写了新逻辑）。
//    只验 `buildAAD(envelope) === expect.aad`。
//
// 2)「aad-kat 真密文」层——5 条，每条都真调 `open()` 解出与 fixture 声明一致的明文，不只比对 AAD
//    字符串：kat_control_stop（与第 1 层重叠）+ aad_kat_pair_ready + aad_kat_pair_accept_tokens +
//    aad_kat_token_refresh + aad_kat_token_refresh_ok（这 4 条 `layer: "aad-kat"`，用 `meta`
//    直接构造 EnvelopeMeta，不走完整 envelope 对象）。
//
// **不适用，且逐类写明归属（125 条 = 139 − 14，本单不碰）**：
//   - **14 条 `expect.aad: null` 的语法-非法样张**（input_missing_command_id /
//     input_session_with_pipe_rejected / input_command_id_with_pipe_rejected /
//     input_command_id_too_long_rejected /
//     input_command_id_non_ascii_utf8_bytes_over_128_rejected / control_missing_command_id /
//     event_with_command_id_rejected / event_missing_client_msg_id_rejected /
//     input_with_client_msg_id_rejected / event_client_msg_id_with_pipe_rejected /
//     event_client_msg_id_over_64_bytes_rejected / reply_missing_command_id /
//     reply_with_client_msg_id_rejected / reply_with_seq_rejected complete the negative reply fixtures, whose syntactically invalid envelopes have no AAD to compute.
//     `reply` kind 反例，同款「语法非法→relay validator 层拒绝→没有 AAD 可算」姿势，机械纳入本
//     canary 名单，不代表本单为 `reply` 新写了校验逻辑）——**这批样张自己声明 `expect.aad` 为 null**
//     （fixture 生成时的态度是：语法非法的信封根本不该走到「算 AAD」这一步，不是「AAD 算出来但
//     不对」）。这是 relay `envelope.js::validateEnvelope()` 的语法层（session/command_id 长度、
//     禁止 `|`、按 kind 决定是否必填……），不是 AAD 拼串或 AEAD 的活——`buildAAD` 是全函数，对
//     任何字段组合都会算出一个确定字符串而不是 null，所以这批样张在本文件里**不能**、也**不该**
//     拿去断言 `buildAAD(...) === null`。归属 T6d1（事件内核）/ T6f3（指令面接线）构造/解析完整
//     信封时按需要复用或对齐 relay 那套语法规则，不在加密内核单重复实现。下方有一条 canary 测试
//     显式断言这 14 条确实是 `expect.aad === null`（防止 fixture 未来改口径却没人发现）。
//   - `token_put_*` / `token_delete_*` / `token_ack_*` / `token_sync*` / `token_reset_*`
//     （管理帧字段语法与 CAS 语义）→ relay §9.2-9.4 令牌注册表，S1 系列（relay 侧）已收官落地，
//     C1 侧不需要重新校验这些帧的语法——C1 只是这些帧密文体的消费方（aad-kat 层已覆盖密文体本身）。
//   - `token_refresh_*`（三帧成套/in-flight 语义）→ relay S1i/S1j 已收官 + C1 侧 T6c-refresh
//     （配对/refresh 状态机单，不是加密内核单）。
//   - `pair_hello_forward_origin_*` / `pair_done_forward_origin_*` / `pair_ready_valid` /
//     `pair_accept_encrypted_tokens_valid` / `pair_accept_tokens_ct_missing` /
//     `pair_accept_plaintext_tokens_forbidden` → 配对帧路由定向转发 + 外层帧语法 → relay S1g 系列
//     已收 + C1 侧 T6b（配对状态机单，密文体本身的 AAD/seal/open 已被上面「aad-kat」层覆盖）。
//   - `subprotocol_*` / `desktop_upgrade_*` / `inbound_*` → wss upgrade 子协议协商、桌面 Bearer
//     准入、scope→入站帧矩阵——全是 relay §9.1 fail-closed 校验，跟 AEAD 内容无关，S1/G8 系列已收。
//   - `http_claim_*` / `http_delete_*` → relay HTTP claim/delete 端点，S1 系列已收，与 C1 无关。
//   - `time_*` / `ttl_*` → relay §9.2/9.4 时间窗与 TTL clamp，注册表语义，S1 系列已收。
//   - `device_access_chain` / `pairing_connect_chain_random_ascii_hex` → relay 端到端链路场景，
//     已用 `s1ja-fake-mobile-e2e.test.js` 覆盖。
//
// ============================================================================
// 变异自证（≥3 条，见 T6A worker 报告 ④——本文件本身不含变异测试，那是开发期手动临时改
// envelope.ts 源码后重跑 `npm test` 观察打红的一次性操作，改完已恢复）
// ============================================================================

import { describe, expect, it } from "vitest";
import { buildAAD, open, seal, EnvelopeDecryptError, type EnvelopeMeta } from "./envelope.ts";
import { loadFixture } from "../test-support/fixtures.ts";

interface WireEnvelope {
  v: number;
  room: string;
  epoch: number;
  kind: string;
  session: string | null;
  command_id: string | null;
  ct: string;
  n: string;
}

interface WireCase {
  name: string;
  layer?: string;
  envelope?: WireEnvelope;
  meta?: EnvelopeMeta;
  expect?: { valid?: boolean; errors?: string[]; aad?: string | null };
  kat?: { k_room_hex?: string; key_hex?: string; n_b64?: string; ct_b64?: string; plaintext: string };
}

const wireCases = loadFixture<WireCase[]>("wire-v1.json");
const byName = new Map(wireCases.map((entry) => [entry.name, entry]));

function envelopeToMeta(envelope: WireEnvelope): EnvelopeMeta {
  return {
    v: envelope.v,
    room: envelope.room,
    epoch: envelope.epoch,
    kind: envelope.kind,
    session: envelope.session,
    command_id: envelope.command_id,
  };
}

describe("wire-v1.json is the shape this test file was written against", () => {
  it("has exactly 139 cases (coverage table above must be re-audited if this changes)", () => {
    expect(wireCases).toHaveLength(139);
  });

  // Canary for the coverage-table headline counts (T6A worker report M2): computed directly from
  // the live fixture via the same predicates the two consuming layers use below — not by re-
  // asserting the hardcoded name lists against themselves — so that adding/removing/retagging a
  // wire-v1 case (a new non-null `expect.aad`, a new `layer: "aad-kat"` entry, ...) turns this red
  // instead of silently leaving the "122 条不适用" prose in the header comment stale.
  it("consumed(12) + untouched(122) === total — coverage-table arithmetic must stay in sync with the fixture", () => {
    const consumedNames = new Set([
      ...wireCases
        .filter((entry) => entry.envelope !== undefined && typeof entry.expect?.aad === "string")
        .map((entry) => entry.name),
      ...wireCases.filter((entry) => entry.layer === "aad-kat").map((entry) => entry.name),
    ]);
    const total = wireCases.length;
    const untouched = total - consumedNames.size;

    expect(consumedNames.size).toBe(14);
    expect(untouched).toBe(125);
    expect(consumedNames.size + untouched).toBe(total);
  });
});

describe("buildAAD() · AAD layer (10 cases with a real, non-null expect.aad)", () => {
  const envelopeAadNames = [
    "input_with_command_id",
    "input_command_id_non_ascii_utf8_bytes_at_126_accepted",
    "control_with_command_id",
    "event_normal",
    "live_normal",
    "presence_normal",
    "kat_control_stop",
    "event_client_msg_id_at_64_bytes_accepted",
    // The reply kind shares envelope-level command_id-in-AAD handling with input/control to preserve consistent authentication semantics.
    // command_id 入 AAD，机械纳入本层同一个通用谓词（见文件头覆盖表说明）。
    "reply_with_command_id",
    "reply_with_null_session",
  ];

  // 与 grammar-invalid 那 11 条的边界（fixture 用 `expect.aad: null` 标记「这份信封语法非法，
  // 没有 AAD 可算」）——canary 测试：防止 fixture 未来改口径却没人发现，导致覆盖表描述过时。
  const grammarInvalidNullAadNames = [
    "input_missing_command_id",
    "input_session_with_pipe_rejected",
    "input_command_id_with_pipe_rejected",
    "input_command_id_too_long_rejected",
    "input_command_id_non_ascii_utf8_bytes_over_128_rejected",
    "control_missing_command_id",
    "event_with_command_id_rejected",
    "event_missing_client_msg_id_rejected",
    "input_with_client_msg_id_rejected",
    "event_client_msg_id_with_pipe_rejected",
    "event_client_msg_id_over_64_bytes_rejected",
    // Negative reply fixtures enforce the same invariant that syntactically invalid envelopes have no AAD to compute.
    "reply_missing_command_id",
    "reply_with_client_msg_id_rejected",
    "reply_with_seq_rejected",
  ];

  it("the named list above is exactly the set of fixture entries carrying `envelope` + a non-null `expect.aad`", () => {
    const actual = wireCases
      .filter((entry) => entry.envelope !== undefined && typeof entry.expect?.aad === "string")
      .map((entry) => entry.name)
      .sort();
    expect(actual).toEqual([...envelopeAadNames].sort());
  });

  it("the grammar-invalid list above is exactly the set of fixture entries carrying `envelope` + expect.aad === null (out of scope, see coverage table)", () => {
    const actual = wireCases
      .filter((entry) => entry.envelope !== undefined && entry.expect && "aad" in entry.expect && entry.expect.aad === null)
      .map((entry) => entry.name)
      .sort();
    expect(actual).toEqual([...grammarInvalidNullAadNames].sort());
  });

  it.each(envelopeAadNames)("%s: buildAAD(envelope) matches expect.aad", (name) => {
    const fixtureCase = byName.get(name);
    if (!fixtureCase?.envelope || typeof fixtureCase.expect?.aad !== "string") {
      throw new Error(`fixture case missing envelope/non-null expect.aad: ${name}`);
    }
    expect(buildAAD(envelopeToMeta(fixtureCase.envelope))).toBe(fixtureCase.expect.aad);
  });

  it("client_msg_id never leaks into the AAD (event_client_msg_id_at_64_bytes_accepted shares the same AAD shape as event_normal)", () => {
    const normal = byName.get("event_normal");
    const withClientMsgId = byName.get("event_client_msg_id_at_64_bytes_accepted");
    if (!normal?.envelope || !withClientMsgId?.envelope) {
      throw new Error("fixture cases missing");
    }
    // Both share v/room/epoch/kind/session/command_id — only client_msg_id differs — so buildAAD
    // must produce byte-identical strings even though client_msg_id itself is present on one and
    // not the other (client_msg_id is deliberately excluded from AAD per M0 §1 v1.7.4).
    expect(buildAAD(envelopeToMeta(normal.envelope))).toBe(buildAAD(envelopeToMeta(withClientMsgId.envelope)));
  });
});

describe("seal()/open() · aad-kat real-ciphertext layer (5 cases)", () => {
  it("kat_control_stop: open() decrypts the real fixture ciphertext to the exact expected plaintext", async () => {
    const fixtureCase = byName.get("kat_control_stop");
    if (!fixtureCase?.envelope || !fixtureCase.kat?.k_room_hex) {
      throw new Error("kat_control_stop fixture missing required fields");
    }
    const key = hexToBytes(fixtureCase.kat.k_room_hex);
    const meta = envelopeToMeta(fixtureCase.envelope);
    const plaintext = await open(key, meta, fixtureCase.envelope.ct, fixtureCase.envelope.n);
    expect(new TextDecoder().decode(plaintext)).toBe(fixtureCase.kat.plaintext);
    expect(buildAAD(meta)).toBe(fixtureCase.expect?.aad);
  });

  const aadKatOnlyNames = [
    "aad_kat_pair_ready",
    "aad_kat_pair_accept_tokens",
    "aad_kat_token_refresh",
    "aad_kat_token_refresh_ok",
  ];

  it("the named list above is exactly the set of fixture entries tagged layer=aad-kat", () => {
    const actual = wireCases
      .filter((entry) => entry.layer === "aad-kat")
      .map((entry) => entry.name)
      .sort();
    expect(actual).toEqual([...aadKatOnlyNames].sort());
  });

  it.each(aadKatOnlyNames)("%s: open() decrypts the real fixture ciphertext to the exact expected plaintext", async (name) => {
    const fixtureCase = byName.get(name);
    if (!fixtureCase?.meta || !fixtureCase.kat?.key_hex || !fixtureCase.kat.n_b64 || !fixtureCase.kat.ct_b64) {
      throw new Error(`fixture case missing required aad-kat fields: ${name}`);
    }
    const key = hexToBytes(fixtureCase.kat.key_hex);
    const plaintext = await open(key, fixtureCase.meta, fixtureCase.kat.ct_b64, fixtureCase.kat.n_b64);
    expect(new TextDecoder().decode(plaintext)).toBe(fixtureCase.kat.plaintext);
    expect(buildAAD(fixtureCase.meta)).toBe(fixtureCase.expect?.aad);
  });

  it("round-trips: sealing the KAT plaintext under the same key/meta as kat_control_stop reproduces a ciphertext open() accepts (not byte-identical — nonce is random each call)", async () => {
    const fixtureCase = byName.get("kat_control_stop");
    if (!fixtureCase?.envelope || !fixtureCase.kat?.k_room_hex) {
      throw new Error("kat_control_stop fixture missing required fields");
    }
    const key = hexToBytes(fixtureCase.kat.k_room_hex);
    const meta = envelopeToMeta(fixtureCase.envelope);
    const plaintextBytes = new TextEncoder().encode(fixtureCase.kat.plaintext);
    const { ct, n } = await seal(key, meta, plaintextBytes);
    expect(ct).not.toBe(fixtureCase.envelope.ct); // fresh random nonce → different ciphertext bytes
    const reopened = await open(key, meta, ct, n);
    expect(new TextDecoder().decode(reopened)).toBe(fixtureCase.kat.plaintext);
  });
});

// ---------------------------------------------------------------------------
// AEAD tamper-rejection ("两向": positive open() above + negative tamper-rejection here) — reuses
// the real fixture keys/ciphertexts rather than self-generated ones, which is a strictly stronger
// check than remote_crypto.rs's self-referential `open_rejects_changed_*` unit tests.
// ---------------------------------------------------------------------------
describe("open() rejects tampering on any AAD-authenticated field (using real aad-kat vectors)", () => {
  it("rejects when epoch is tampered", async () => {
    const fixtureCase = byName.get("aad_kat_pair_ready");
    if (!fixtureCase?.meta || !fixtureCase.kat?.key_hex) throw new Error("fixture missing");
    const key = hexToBytes(fixtureCase.kat.key_hex);
    const tamperedMeta = { ...fixtureCase.meta, epoch: fixtureCase.meta.epoch + 1 };
    await expect(open(key, tamperedMeta, fixtureCase.kat.ct_b64!, fixtureCase.kat.n_b64!)).rejects.toThrow();
  });

  it("rejects when kind is tampered", async () => {
    const fixtureCase = byName.get("aad_kat_token_refresh");
    if (!fixtureCase?.meta || !fixtureCase.kat?.key_hex) throw new Error("fixture missing");
    const key = hexToBytes(fixtureCase.kat.key_hex);
    const tamperedMeta = { ...fixtureCase.meta, kind: "token.refresh.ok" };
    await expect(open(key, tamperedMeta, fixtureCase.kat.ct_b64!, fixtureCase.kat.n_b64!)).rejects.toThrow();
  });

  it("rejects when command_id is tampered", async () => {
    const fixtureCase = byName.get("aad_kat_token_refresh_ok");
    if (!fixtureCase?.meta || !fixtureCase.kat?.key_hex) throw new Error("fixture missing");
    const key = hexToBytes(fixtureCase.kat.key_hex);
    const tamperedMeta = { ...fixtureCase.meta, command_id: "00000000-0000-0000-0000-000000000000" };
    await expect(open(key, tamperedMeta, fixtureCase.kat.ct_b64!, fixtureCase.kat.n_b64!)).rejects.toThrow();
  });

  it("rejects when a single ciphertext byte is flipped", async () => {
    const fixtureCase = byName.get("kat_control_stop");
    if (!fixtureCase?.envelope || !fixtureCase.kat?.k_room_hex) throw new Error("fixture missing");
    const key = hexToBytes(fixtureCase.kat.k_room_hex);
    const meta = envelopeToMeta(fixtureCase.envelope);
    const flipped = flipOneBase64Byte(fixtureCase.envelope.ct);
    await expect(open(key, meta, flipped, fixtureCase.envelope.n)).rejects.toThrow();
  });

  it("rejects when the wrong key is used", async () => {
    const fixtureCase = byName.get("kat_control_stop");
    if (!fixtureCase?.envelope || !fixtureCase.kat?.k_room_hex) throw new Error("fixture missing");
    const wrongKey = hexToBytes(fixtureCase.kat.k_room_hex.split("").reverse().join(""));
    const meta = envelopeToMeta(fixtureCase.envelope);
    await expect(open(wrongKey, meta, fixtureCase.envelope.ct, fixtureCase.envelope.n)).rejects.toThrow();
  });
});

// ---------------------------------------------------------------------------
// M0 hardening (real gap, not hypothetical): `open()` previously fed whatever `atob()` decoded
// straight to `crypto.subtle.decrypt()` without checking (a) that the base64 was the *canonical*
// encoding of its own bytes, or (b) that the decoded nonce was exactly 12 bytes. Desktop Rust
// (`app/src-tauri/src/remote_crypto.rs:176` `decode_ciphertext_and_nonce`) uses the `STANDARD`
// base64 engine (rejects unpadded/non-canonical input outright) and `try_into::<[u8; 12]>()`
// (rejects any nonce that isn't exactly 12 bytes) — both hard rejections at the type/decode level.
// AES-GCM's WebCrypto IV parameter is NOT fixed-length by spec (arbitrary-length IVs are valid
// GCM, just non-standard) and `atob()` is lenient about missing padding (see `bytes.ts`
// `decodeCanonicalBase64` doc comment) — so the JS side was silently *more permissive* than the
// Rust side: a malformed envelope Rust would flatly reject at decode time could still be accepted
// and correctly decrypted here. Fixed by `decodeCanonicalBase64()` (bytes.ts) + an explicit
// `nonceBytes.length !== NONCE_LEN` check in `open()`.
// ---------------------------------------------------------------------------
describe("open() rejects malformed wire encodings (M0 canonical base64 + nonce length parity with remote_crypto.rs:176)", () => {
  it("rejects a syntactically-valid-but-13-byte nonce that would otherwise decrypt successfully", async () => {
    const key = new Uint8Array(32).fill(3);
    const meta: EnvelopeMeta = { v: 1, room: "a".repeat(32), epoch: 1, kind: "control", session: "s", command_id: "c" };
    const plaintext = new TextEncoder().encode("thirteen-byte-nonce payload");
    const iv13 = new Uint8Array(13).fill(9);

    // Manually AEAD-encrypt with a hand-rolled 13-byte IV, bypassing seal() (which only ever
    // generates 12-byte nonces) — this is exactly what a malformed/hostile sender on the wire
    // could produce, and WebCrypto's AES-GCM genuinely accepts it (GCM permits non-96-bit IVs;
    // confirmed against this runtime — encrypt/decrypt round-trips fine with a 13-byte IV outside
    // of `open()`'s guard).
    const cryptoKey = await crypto.subtle.importKey("raw", key, "AES-GCM", false, ["encrypt"]);
    const aad = new TextEncoder().encode(buildAAD(meta));
    const ciphertext = new Uint8Array(
      await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv13, additionalData: aad }, cryptoKey, plaintext),
    );
    const ctBase64 = bytesToBase64Standalone(ciphertext);
    const nonceBase64 = bytesToBase64Standalone(iv13);

    // Sanity: this really is a well-formed AEAD ciphertext under the malformed 13-byte IV, not a
    // garbage input that would fail for an unrelated reason.
    const rawDecrypt = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: iv13, additionalData: aad },
      await crypto.subtle.importKey("raw", key, "AES-GCM", false, ["decrypt"]),
      ciphertext,
    );
    expect(new TextDecoder().decode(rawDecrypt)).toBe("thirteen-byte-nonce payload");

    await expect(open(key, meta, ctBase64, nonceBase64)).rejects.toThrow(EnvelopeDecryptError);
  });

  it("rejects non-canonical (unpadded) base64 ciphertext even though it decodes to the exact same bytes atob() would accept", async () => {
    const key = new Uint8Array(32).fill(4);
    const meta: EnvelopeMeta = { v: 1, room: "a".repeat(32), epoch: 1, kind: "control", session: "s", command_id: "c" };
    // "h" (1 byte of plaintext) + 16-byte GCM tag = 17 ciphertext bytes → 17 % 3 === 2, so
    // canonical base64 has exactly one '=' of padding, giving a real non-canonical variant once
    // that padding character is stripped (an 18-byte ciphertext, e.g. from "hi", would need zero
    // padding to begin with and wouldn't exercise this at all).
    const { ct, n } = await seal(key, meta, new TextEncoder().encode("h"));
    expect(ct.endsWith("=")).toBe(true); // precondition: this fixture really does have padding to strip
    const unpaddedCt = ct.replace(/=+$/, "");
    expect(unpaddedCt).not.toBe(ct);

    // Sanity: atob() alone treats the padded and unpadded forms as identical (this is precisely
    // the leniency that made the old open() accept non-canonical input).
    const paddedBytes = Array.from(atob(ct), (c) => c.charCodeAt(0));
    const unpaddedBytes = Array.from(atob(unpaddedCt), (c) => c.charCodeAt(0));
    expect(unpaddedBytes).toEqual(paddedBytes);

    await expect(open(key, meta, unpaddedCt, n)).rejects.toThrow(EnvelopeDecryptError);
  });

  it("regression: a valid-length (12-byte) nonce paired with truncated ciphertext still fails authentication", async () => {
    const key = new Uint8Array(32).fill(5);
    const meta: EnvelopeMeta = { v: 1, room: "a".repeat(32), epoch: 1, kind: "control", session: "s", command_id: "c" };
    const { ct, n } = await seal(key, meta, new TextEncoder().encode("regression payload"));
    const ciphertextBytes = Array.from(atob(ct), (c) => c.charCodeAt(0));
    const truncated = bytesToBase64Standalone(new Uint8Array(ciphertextBytes.slice(0, -1)));
    await expect(open(key, meta, truncated, n)).rejects.toThrow(EnvelopeDecryptError);
  });
});

function bytesToBase64Standalone(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

describe("seal() produces the documented wire shapes", () => {
  it("nonce is always 12 bytes, ct/n are standard base64", async () => {
    const key = new Uint8Array(32).fill(9);
    const meta: EnvelopeMeta = { v: 1, room: "a".repeat(32), epoch: 1, kind: "input", session: "s", command_id: "c" };
    const { ct, n } = await seal(key, meta, new TextEncoder().encode("hello"));
    const standardBase64 = /^[A-Za-z0-9+/]*={0,2}$/;
    expect(standardBase64.test(ct)).toBe(true);
    expect(standardBase64.test(n)).toBe(true);
    expect(base64DecodedByteLength(n)).toBe(12);
  });

  it("never repeats a nonce across two seals of the same plaintext (CSPRNG sanity)", async () => {
    const key = new Uint8Array(32).fill(9);
    const meta: EnvelopeMeta = { v: 1, room: "a".repeat(32), epoch: 1, kind: "input", session: "s", command_id: "c" };
    const first = await seal(key, meta, new TextEncoder().encode("same plaintext"));
    const second = await seal(key, meta, new TextEncoder().encode("same plaintext"));
    expect(first.n).not.toBe(second.n);
    expect(first.ct).not.toBe(second.ct);
  });

  it("rejects a key that is not 32 bytes", async () => {
    const shortKey = new Uint8Array(16);
    const meta: EnvelopeMeta = { v: 1, room: "a".repeat(32), epoch: 1, kind: "input", session: "s", command_id: "c" };
    await expect(seal(shortKey, meta, new TextEncoder().encode("x"))).rejects.toThrow();
  });
});

// ---------------------------------------------------------------------------
// INT1A — `key` accepts `Uint8Array | CryptoKey` (real gap: `store/key-store.ts` persists K_room
// as a *non-extractable* CryptoKey — raw bytes are never retrievable again after import — while
// seal()/open() previously only accepted raw bytes and re-imported internally on every call. A
// reloaded, already-paired device would hold nothing but a non-extractable CryptoKey and could
// never encrypt/decrypt another frame. Fixed by `resolveAesKey()`: a `CryptoKey` argument is used
// as-is (WebCrypto natively supports encrypt/decrypt on non-extractable keys — no re-import
// needed or possible); a `Uint8Array` argument still goes through `importAesKey()` byte-for-byte,
// unchanged.
// ---------------------------------------------------------------------------
describe("seal()/open() accept a pre-imported CryptoKey in addition to raw Uint8Array bytes", () => {
  const meta: EnvelopeMeta = { v: 1, room: "a".repeat(32), epoch: 7, kind: "control", session: "s", command_id: "c" };
  const plaintext = new TextEncoder().encode("CryptoKey variant payload");

  it("bytes-path seal() → CryptoKey-path open() succeeds and decrypts to the identical plaintext", async () => {
    const rawKey = new Uint8Array(32).fill(11);
    const { ct, n } = await seal(rawKey, meta, plaintext);
    const cryptoKey = await crypto.subtle.importKey("raw", rawKey, "AES-GCM", true, ["decrypt"]);
    const opened = await open(cryptoKey, meta, ct, n);
    expect(new TextDecoder().decode(opened)).toBe("CryptoKey variant payload");
  });

  it("CryptoKey-path seal() → bytes-path open() succeeds and decrypts to the identical plaintext (reverse direction)", async () => {
    const rawKey = new Uint8Array(32).fill(12);
    const cryptoKey = await crypto.subtle.importKey("raw", rawKey, "AES-GCM", true, ["encrypt"]);
    const { ct, n } = await seal(cryptoKey, meta, plaintext);
    const opened = await open(rawKey, meta, ct, n);
    expect(new TextDecoder().decode(opened)).toBe("CryptoKey variant payload");
  });

  it("non-extractable CryptoKey works both directions (the actual key-store.ts shape: extractable:false, usages [encrypt, decrypt])", async () => {
    const rawKey = new Uint8Array(32).fill(13);
    const nonExtractableKey = await crypto.subtle.importKey("raw", rawKey, "AES-GCM", false, [
      "encrypt",
      "decrypt",
    ]);
    expect(nonExtractableKey.extractable).toBe(false);

    // seal() with the non-extractable key, open() with the same non-extractable key.
    const sealed = await seal(nonExtractableKey, meta, plaintext);
    const openedSameKey = await open(nonExtractableKey, meta, sealed.ct, sealed.n);
    expect(new TextDecoder().decode(openedSameKey)).toBe("CryptoKey variant payload");

    // Cross-check against the raw-bytes path in both directions, proving the non-extractable
    // CryptoKey is cryptographically equivalent to the bytes it was imported from — not just
    // "doesn't throw".
    const openedViaBytes = await open(rawKey, meta, sealed.ct, sealed.n);
    expect(new TextDecoder().decode(openedViaBytes)).toBe("CryptoKey variant payload");

    const sealedViaBytes = await seal(rawKey, meta, plaintext);
    const openedNonExtractable = await open(nonExtractableKey, meta, sealedViaBytes.ct, sealedViaBytes.n);
    expect(new TextDecoder().decode(openedNonExtractable)).toBe("CryptoKey variant payload");
  });
});

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function base64DecodedByteLength(value: string): number {
  const padding = value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0;
  return (value.length / 4) * 3 - padding;
}

function flipOneBase64Byte(base64: string): string {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  bytes[0] ^= 1;
  let out = "";
  for (const byte of bytes) out += String.fromCharCode(byte);
  return btoa(out);
}
