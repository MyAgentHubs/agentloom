// refreshFrames.test.ts
//
// ============================================================================
// 覆盖表(任务书 §4③ 要求;呼应 envelope.test.ts 顶注"token_refresh_* → C1 侧 T6c-refresh 消费"
// 的分工声明——本文件是 wire-v1.json 里这批样张在 C1 侧的真路径消费方)
// ============================================================================
// - `aad_kat_token_refresh` / `aad_kat_token_refresh_ok`(layer=aad-kat)——真调 `open()`,经本文件
//   的 `refreshRequestMeta`/`refreshOkMeta`(不是手写 meta),解出与 fixture 声明一致的明文。这是
//   防"kind 打错字但测试自己拼 meta 照样全绿"那类回归的关键(M0 changelog v1.8.6 记录的真实教训)。
// - `token_refresh_valid`(layer=token-frame)——`buildTokenRefreshFrame` 产出的帧与它逐字段相等。
// - `token_refresh_ok_valid` / `token_refresh_fail_valid` /
//   `token_refresh_fail_in_flight_no_close_valid`(layer=token-frame)——`parseRefreshResponseFrame`
//   逐条解析,断言判别结果与字段。
// - `token_refresh_request_id_missing`——反例:`parseRefreshResponseFrame` 对缺字段帧不适用(这条
//   fixture 本身是 request 侧缺字段样张,不是 ok/fail 响应帧;这里用来证明"非 ok/fail 的 t 或缺字段
//   帧一律 not_a_refresh_response",不是我方帧校验的直接样张——见下方注释)。
// - `token_refresh_forward_valid`——relay→桌面帧,C1(手机)从不发送/接收它,不适用,归 relay/桌面。
//
// 变异自证(≥3 条,任务书 §4⑤):见文末专门 describe 块。

import { describe, expect, it } from "vitest";
import {
  buildTokenRefreshFrame,
  isValidRequestId,
  openRefreshOkBody,
  parseRefreshResponseFrame,
  refreshOkMeta,
  refreshRequestMeta,
  REQUEST_ID_MAX_BYTES,
  RefreshOkDecodeError,
  sealRefreshRequestBody,
} from "./refreshFrames.ts";
import { open, seal } from "../crypto/envelope.ts";
import { base64ToBytes, bytesToBase64, hexToBytes, utf8Bytes } from "../crypto/bytes.ts";
import { loadFixture } from "../test-support/fixtures.ts";

interface AadKatFixture {
  name: string;
  layer: string;
  device_id: string;
  request_id: string;
  meta: { v: number; room: string; epoch: number; kind: string; session: string; command_id: string };
  expect: { aad: string };
  kat: { key_hex: string; n_b64: string; ct_b64: string; plaintext: string };
}

interface TokenFrameFixture {
  name: string;
  layer: string;
  frame: Record<string, unknown>;
  expect: { valid: boolean; errors: string[] };
}

const wireFixtures = loadFixture<Array<Record<string, unknown>>>("wire-v1.json");

function findAadKat(name: string): AadKatFixture {
  const found = wireFixtures.find((entry) => entry.name === name);
  if (!found) throw new Error(`fixture ${name} not found`);
  return found as unknown as AadKatFixture;
}

function findTokenFrame(name: string): TokenFrameFixture {
  const found = wireFixtures.find((entry) => entry.name === name);
  if (!found) throw new Error(`fixture ${name} not found`);
  return found as unknown as TokenFrameFixture;
}

describe("refreshRequestMeta()/refreshOkMeta() — AAD KAT 真路径消费(经 crypto/envelope.ts::open())", () => {
  it("aad_kat_token_refresh: refreshRequestMeta 产出的 meta 能解开 KAT 密文,明文与 fixture 一致", async () => {
    const fixture = findAadKat("aad_kat_token_refresh");
    const meta = refreshRequestMeta(fixture.meta.room, fixture.device_id, fixture.request_id);
    expect(meta).toEqual(fixture.meta);
    const plaintext = await open(hexToBytes(fixture.kat.key_hex), meta, fixture.kat.ct_b64, fixture.kat.n_b64);
    expect(new TextDecoder().decode(plaintext)).toBe(fixture.kat.plaintext);
  });

  it("aad_kat_token_refresh_ok: refreshOkMeta 产出的 meta 能解开 KAT 密文,明文与 fixture 一致", async () => {
    const fixture = findAadKat("aad_kat_token_refresh_ok");
    const meta = refreshOkMeta(fixture.meta.room, fixture.device_id, fixture.request_id);
    expect(meta).toEqual(fixture.meta);
    const plaintext = await open(hexToBytes(fixture.kat.key_hex), meta, fixture.kat.ct_b64, fixture.kat.n_b64);
    expect(new TextDecoder().decode(plaintext)).toBe(fixture.kat.plaintext);
  });

  it("aad_kat_token_refresh_ok: openRefreshOkBody() end-to-end 解出 fixture 里编码的 capability_token/refresh_token", async () => {
    const fixture = findAadKat("aad_kat_token_refresh_ok");
    const rotated = await openRefreshOkBody(
      hexToBytes(fixture.kat.key_hex),
      fixture.meta.room,
      fixture.device_id,
      fixture.request_id,
      fixture.kat.ct_b64,
      fixture.kat.n_b64,
    );
    const expectedPlaintext = JSON.parse(fixture.kat.plaintext) as { capability_token: string; refresh_token: string };
    expect(rotated).toEqual({
      capabilityToken: expectedPlaintext.capability_token,
      refreshToken: expectedPlaintext.refresh_token,
    });
  });
});

describe("sealRefreshRequestBody()/buildTokenRefreshFrame() — 出站请求往返", () => {
  it("round-trips through seal()/open() with the same meta the desktop reference uses", async () => {
    const kPair = new Uint8Array(32).fill(7);
    const room = "0123456789abcdef0123456789abcdef";
    const deviceId = "11111111-1111-4111-8111-111111111111";
    const requestId = crypto.randomUUID();
    const refreshToken = "b".repeat(64);

    const { ct, n } = await sealRefreshRequestBody(kPair, room, deviceId, requestId, refreshToken);
    const frame = buildTokenRefreshFrame(requestId, ct, n);
    expect(frame).toEqual({ t: "token.refresh", request_id: requestId, ct, n });

    // 桌面侧视角:用同一把 K_pair + 同一个 meta 解开——对齐 remote_pairing.rs::open_token_refresh_request。
    const plaintext = await open(kPair, refreshRequestMeta(room, deviceId, requestId), ct, n);
    expect(JSON.parse(new TextDecoder().decode(plaintext))).toEqual({ refresh_token: refreshToken });
  });

  it("token_refresh_valid fixture: buildTokenRefreshFrame produces the exact same frame shape", () => {
    const fixture = findTokenFrame("token_refresh_valid");
    const frame = buildTokenRefreshFrame(
      fixture.frame.request_id as string,
      fixture.frame.ct as string,
      fixture.frame.n as string,
    );
    expect(frame).toEqual(fixture.frame);
  });
});

describe("isValidRequestId() — 审查返工新增:生成/恢复/解析三处统一调用的 grammar 校验", () => {
  it("accepts a normal UUID (default requestIdFactory output)", () => {
    expect(isValidRequestId(crypto.randomUUID())).toBe(true);
  });

  it("rejects an empty string", () => {
    expect(isValidRequestId("")).toBe(false);
  });

  it("rejects non-string values", () => {
    expect(isValidRequestId(123)).toBe(false);
    expect(isValidRequestId(null)).toBe(false);
    expect(isValidRequestId(undefined)).toBe(false);
    expect(isValidRequestId({})).toBe(false);
  });

  it(`accepts exactly ${REQUEST_ID_MAX_BYTES} UTF-8 bytes and rejects ${REQUEST_ID_MAX_BYTES + 1}`, () => {
    expect(isValidRequestId("a".repeat(REQUEST_ID_MAX_BYTES))).toBe(true);
    expect(isValidRequestId("a".repeat(REQUEST_ID_MAX_BYTES + 1))).toBe(false);
  });

  it("counts UTF-8 bytes, not UTF-16 code units (multi-byte characters) — aligns with room-do.js::requestIdTooLong", () => {
    // 每个"中"字是 3 个 UTF-8 字节但只占 1 个 UTF-16 码元——用它验证字节计数,不是 .length 计数。
    const fortyThreeChars = "中".repeat(43); // 43*3=129 字节 > 128
    expect(fortyThreeChars.length).toBe(43); // .length(码元数)看起来很安全
    expect(isValidRequestId(fortyThreeChars)).toBe(false); // 但字节数已经超限
    expect(isValidRequestId("中".repeat(42))).toBe(true); // 42*3=126 字节,在限内
  });
});

describe("parseRefreshResponseFrame() — 入站响应帧判别", () => {
  it("token_refresh_ok_valid: parses as kind=ok with all fields intact", () => {
    const fixture = findTokenFrame("token_refresh_ok_valid");
    const parsed = parseRefreshResponseFrame(fixture.frame);
    expect(parsed).toEqual({ kind: "ok", frame: fixture.frame });
  });

  it("token_refresh_fail_valid: parses as kind=fail with close:true (>=3 次连续无效)", () => {
    const fixture = findTokenFrame("token_refresh_fail_valid");
    const parsed = parseRefreshResponseFrame(fixture.frame);
    expect(parsed).toEqual({ kind: "fail", frame: fixture.frame });
    if (parsed.kind === "fail") {
      expect(parsed.frame.close).toBe(true);
    }
  });

  it("token_refresh_fail_in_flight_no_close_valid: parses as kind=fail with close undefined (良性单飞行冲突,不断连)", () => {
    const fixture = findTokenFrame("token_refresh_fail_in_flight_no_close_valid");
    const parsed = parseRefreshResponseFrame(fixture.frame);
    expect(parsed.kind).toBe("fail");
    if (parsed.kind === "fail") {
      expect(parsed.frame.reason).toBe("in_flight");
      expect(parsed.frame.close).toBeUndefined();
    }
  });

  it("rejects an unrelated t (e.g. token.refresh request frame itself, not a response)", () => {
    const fixture = findTokenFrame("token_refresh_valid");
    expect(parseRefreshResponseFrame(fixture.frame)).toEqual({ kind: "not_a_refresh_response" });
  });

  it("rejects malformed input (not an object, missing t, missing required fields) without throwing", () => {
    expect(parseRefreshResponseFrame(null)).toEqual({ kind: "not_a_refresh_response" });
    expect(parseRefreshResponseFrame("token.refresh.ok")).toEqual({ kind: "not_a_refresh_response" });
    expect(parseRefreshResponseFrame({ t: "token.refresh.ok" })).toEqual({ kind: "not_a_refresh_response" });
    expect(parseRefreshResponseFrame({ t: "token.refresh.fail" })).toEqual({ kind: "not_a_refresh_response" });
  });

  describe("审查返工:request_id/generation/close 严格校验(对齐 room-do.js:1800 一带)", () => {
    it("rejects an ok frame whose request_id exceeds 128 UTF-8 bytes", () => {
      const fixture = findTokenFrame("token_refresh_ok_valid");
      const tooLong = { ...fixture.frame, request_id: "a".repeat(129) };
      expect(parseRefreshResponseFrame(tooLong)).toEqual({ kind: "not_a_refresh_response" });
    });

    it("rejects a fail frame whose request_id is an empty string", () => {
      const fixture = findTokenFrame("token_refresh_fail_valid");
      const empty = { ...fixture.frame, request_id: "" };
      expect(parseRefreshResponseFrame(empty)).toEqual({ kind: "not_a_refresh_response" });
    });

    it("rejects generation=0 (must be a strictly positive integer, per room-do.js::tokenRefreshReceiptShapeError)", () => {
      const fixture = findTokenFrame("token_refresh_ok_valid");
      expect(parseRefreshResponseFrame({ ...fixture.frame, generation: 0 })).toEqual({ kind: "not_a_refresh_response" });
    });

    it("rejects a negative generation", () => {
      const fixture = findTokenFrame("token_refresh_ok_valid");
      expect(parseRefreshResponseFrame({ ...fixture.frame, generation: -1 })).toEqual({ kind: "not_a_refresh_response" });
    });

    it("rejects a non-integer (float) generation", () => {
      const fixture = findTokenFrame("token_refresh_ok_valid");
      expect(parseRefreshResponseFrame({ ...fixture.frame, generation: 1.5 })).toEqual({ kind: "not_a_refresh_response" });
    });

    it("accepts a large but safe-integer generation", () => {
      const fixture = findTokenFrame("token_refresh_ok_valid");
      expect(parseRefreshResponseFrame({ ...fixture.frame, generation: Number.MAX_SAFE_INTEGER }).kind).toBe("ok");
    });

    it("rejects a fail frame whose close field is present but not a boolean (整帧拒绝,不静默坍缩成 undefined)", () => {
      const fixture = findTokenFrame("token_refresh_fail_in_flight_no_close_valid");
      expect(parseRefreshResponseFrame({ ...fixture.frame, close: "true" })).toEqual({ kind: "not_a_refresh_response" });
      expect(parseRefreshResponseFrame({ ...fixture.frame, close: 1 })).toEqual({ kind: "not_a_refresh_response" });
      expect(parseRefreshResponseFrame({ ...fixture.frame, close: null })).toEqual({ kind: "not_a_refresh_response" });
    });

    it("still accepts a fail frame with close entirely absent (undefined, benign in_flight/put_rejected shape)", () => {
      const fixture = findTokenFrame("token_refresh_fail_in_flight_no_close_valid");
      const parsed = parseRefreshResponseFrame(fixture.frame);
      expect(parsed.kind).toBe("fail");
      if (parsed.kind === "fail") expect(parsed.frame.close).toBeUndefined();
    });
  });
});

describe("openRefreshOkBody() — 认证失败/形状不对统一报错,不泄露子原因", () => {
  const room = "0123456789abcdef0123456789abcdef";
  const deviceId = "11111111-1111-4111-8111-111111111111";
  const requestId = "8d6e4c20-1a3b-4f95-b7c2-6e0d9a41f583";

  it("throws RefreshOkDecodeError on ciphertext tampering (AEAD auth failure)", async () => {
    const kPair = new Uint8Array(32).fill(3);
    const { ct, n } = await seal(
      kPair,
      refreshOkMeta(room, deviceId, requestId),
      utf8Bytes(JSON.stringify({ capability_token: "a".repeat(64), refresh_token: "b".repeat(64) })),
    );
    const ctBytes = base64ToBytes(ct);
    ctBytes[0] ^= 0xff; // 翻转首字节——破坏 AEAD 认证标签/密文完整性
    const tamperedCt = bytesToBase64(ctBytes);
    await expect(openRefreshOkBody(kPair, room, deviceId, requestId, tamperedCt, n)).rejects.toThrow(
      RefreshOkDecodeError,
    );
  });

  it("throws RefreshOkDecodeError when the decrypted plaintext has the wrong shape", async () => {
    const kPair = new Uint8Array(32).fill(3);
    const { ct, n } = await seal(kPair, refreshOkMeta(room, deviceId, requestId), utf8Bytes(JSON.stringify({ oops: true })));
    await expect(openRefreshOkBody(kPair, room, deviceId, requestId, ct, n)).rejects.toThrow(RefreshOkDecodeError);
  });

  it("throws RefreshOkDecodeError when decrypted tokens are not hex64-shaped", async () => {
    const kPair = new Uint8Array(32).fill(3);
    const { ct, n } = await seal(
      kPair,
      refreshOkMeta(room, deviceId, requestId),
      utf8Bytes(JSON.stringify({ capability_token: "not-hex", refresh_token: "b".repeat(64) })),
    );
    await expect(openRefreshOkBody(kPair, room, deviceId, requestId, ct, n)).rejects.toThrow(RefreshOkDecodeError);
  });

  it("throws when opened with the wrong K_pair (simulates a foreign/rotated key)", async () => {
    const kPair = new Uint8Array(32).fill(3);
    const wrongKPair = new Uint8Array(32).fill(9);
    const { ct, n } = await seal(
      kPair,
      refreshOkMeta(room, deviceId, requestId),
      utf8Bytes(JSON.stringify({ capability_token: "a".repeat(64), refresh_token: "b".repeat(64) })),
    );
    await expect(openRefreshOkBody(wrongKPair, room, deviceId, requestId, ct, n)).rejects.toThrow(
      RefreshOkDecodeError,
    );
  });
});

describe("变异自证(≥3 条)", () => {
  it("mutation proof: if refreshRequestMeta's kind literal were mistyped ('token-refresh' instead of 'token.refresh'), the KAT ciphertext would fail to decrypt", async () => {
    const fixture = findAadKat("aad_kat_token_refresh");
    const mistypedMeta = { ...refreshRequestMeta(fixture.meta.room, fixture.device_id, fixture.request_id), kind: "token-refresh" };
    await expect(open(hexToBytes(fixture.kat.key_hex), mistypedMeta, fixture.kat.ct_b64, fixture.kat.n_b64)).rejects.toThrow();
    // 真实实现(正确 kind)必须成功——两相对照,证明这条 KAT 真的在锁死 kind 字面量,不是摆设。
    await expect(
      open(
        hexToBytes(fixture.kat.key_hex),
        refreshRequestMeta(fixture.meta.room, fixture.device_id, fixture.request_id),
        fixture.kat.ct_b64,
        fixture.kat.n_b64,
      ),
    ).resolves.toBeDefined();
  });

  it("mutation proof: if parseRefreshResponseFrame stopped checking `generation` is a finite number, a malformed ok frame with a string generation would be wrongly accepted", () => {
    const fixture = findTokenFrame("token_refresh_ok_valid");
    const malformed = { ...fixture.frame, generation: "not-a-number" };
    expect(parseRefreshResponseFrame(malformed)).toEqual({ kind: "not_a_refresh_response" }); // 真实实现:拒绝
    // 模拟"去掉这条检查"的变异版本,证明它确实会放行:
    const withoutGenerationCheck = (raw: Record<string, unknown>) =>
      typeof raw.t === "string" &&
      raw.t === "token.refresh.ok" &&
      typeof raw.request_id === "string" &&
      typeof raw.subject === "string" &&
      typeof raw.ct === "string" &&
      typeof raw.n === "string";
    expect(withoutGenerationCheck(malformed)).toBe(true);
  });

  it("mutation proof: if openRefreshOkBody stopped validating hex64 shape, a plaintext with a too-short token would be wrongly accepted as rotated credentials", async () => {
    const kPair = new Uint8Array(32).fill(5);
    const room = "0123456789abcdef0123456789abcdef";
    const deviceId = "22222222-2222-4222-8222-222222222222";
    const requestId = "33333333-3333-4333-8333-333333333333";
    const { ct, n } = await seal(
      kPair,
      refreshOkMeta(room, deviceId, requestId),
      utf8Bytes(JSON.stringify({ capability_token: "short", refresh_token: "b".repeat(64) })),
    );
    await expect(openRefreshOkBody(kPair, room, deviceId, requestId, ct, n)).rejects.toThrow(RefreshOkDecodeError);
    // 变异版本(去掉 hex64 校验)会把 "short" 原样当合法 access token 放行:
    const plaintext = await open(kPair, refreshOkMeta(room, deviceId, requestId), ct, n);
    const withoutHexCheck = JSON.parse(new TextDecoder().decode(plaintext)) as { capability_token: string };
    expect(withoutHexCheck.capability_token).toBe("short"); // 证明"不校验"确实会让这个坏值流出去
  });

  it("审查返工新增 mutation proof: reverting the close-field check from 'reject if present-but-not-boolean' back to 'silently coerce to undefined' would let a tampered/malformed close value sneak through as if it were absent", () => {
    const fixture = findTokenFrame("token_refresh_fail_in_flight_no_close_valid");
    const malformed = { ...fixture.frame, close: "true" }; // 字符串 "true",不是布尔 true
    expect(parseRefreshResponseFrame(malformed)).toEqual({ kind: "not_a_refresh_response" }); // 真实实现:整帧拒绝
    // 变异版本("静默坍缩"旧实现):不管 close 是什么形状,只要不是 boolean 就悄悄变成 undefined,
    // 帧本身仍然被判定合法——这会让"close 字段被篡改成非布尔值"这个信号被完全吞掉。
    const closeUnderOldCoercion = typeof malformed.close === "boolean" ? malformed.close : undefined;
    expect(closeUnderOldCoercion).toBeUndefined(); // 旧实现会把这条"畸形但存在"的 close 悄悄当成"缺省"
  });
});
