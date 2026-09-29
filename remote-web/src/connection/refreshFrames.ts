// refreshFrames.ts — T6c-refresh · token.refresh 三帧的密文体构造/解析(M0 §9.6)。
//
// 权威参照(只读对照,只 import 消费,一行不改——任务书首段红线):
//   - crypto/envelope.ts 的 `seal`/`open`/`EnvelopeMeta`(AES-256-GCM + AAD 拼串)。
//   - crypto/bytes.ts 的 `utf8Bytes`。
//   - app/src-tauri/src/remote_pairing.rs 的 `token_refresh_meta`/`token_refresh_ok_meta`
//     (526-550 行·kind 字面量与字段装法)+ `open_token_refresh_request`/`seal_token_refresh_ok`
//     (561-611 行·明文形状 `{"refresh_token"}`/`{"capability_token","refresh_token"}`)。
//   - remote-relay/fixtures/wire-v1.json 的 `token_refresh_valid`/`token_refresh_ok_valid`/
//     `token_refresh_fail_valid`/`token_refresh_fail_in_flight_no_close_valid`/
//     `aad_kat_token_refresh`/`aad_kat_token_refresh_ok`(帧形状与 AAD/KAT 权威样张)。
//
// **本文件不修改 `pairing/meta.ts`**(那个文件只有配对四帧的 meta,没有 refresh 的——refresh 帧
// 族不属于 `PairingSession` 的职责域,`pairing/` 目录本单不可改)。这里按同一套 `EnvelopeMeta`
// 拼串约定(M0 §1)独立构造 refresh 专用的两个 meta,字段值逐一对齐上面列的 Rust 权威实现与
// AAD KAT 样张(见本文件测试)。
//
// **审查返工(request_id 严格校验)**:`request_id` 的 grammar 权威在 `remote-relay/src/room-do.js`
// (`REQUEST_ID_MAX_BYTES = 128`·非空字符串·UTF-8 字节计——与 envelope.js 的 command_id 上限同口径)
// ——relay 拒绝超限/空字符串的 request_id(`tokenRefreshShapeError`)。本单在**三处**都过同一条
// `isValidRequestId()`:① 生成(`connectionSession.ts::beginRefresh` 新生成的 id)——防注入的自定义
// `requestIdFactory` 产出畸形 id;② 恢复(`resumePendingRefreshFromStore` 从 KeyStore 读回的
// pending)——防存量脏数据/篡改；③ 解析(`parseRefreshResponseFrame` 收到的响应帧)——防恶意/损坏
// relay 转发出畸形 id。三处任一校验失败都是 fail-closed(拒绝该 id,不静默放行)。

import { open, seal, type EnvelopeMeta } from "../crypto/envelope.ts";
import { utf8Bytes } from "../crypto/bytes.ts";

const REFRESH_PROTOCOL_VERSION = 1;
const HEX64_RE = /^[0-9a-f]{64}$/;
/** 对齐 `remote-relay/src/room-do.js::REQUEST_ID_MAX_BYTES`。 */
export const REQUEST_ID_MAX_BYTES = 128;

function isHex64(value: string): boolean {
  return HEX64_RE.test(value);
}

/**
 * `request_id` grammar——非空字符串,UTF-8 字节数 ≤128(`room-do.js::requestIdTooLong`/
 * `requiredNonEmptyString` 同口径,按字节不按 `.length` 码元数)。生成/恢复/解析三处统一调用这个
 * 唯一实现,不各自重新判断。
 */
export function isValidRequestId(value: unknown): value is string {
  if (typeof value !== "string" || value.length === 0) return false;
  return new TextEncoder().encode(value).length <= REQUEST_ID_MAX_BYTES;
}

/** `token.refresh` 请求密文体 AAD meta——对齐 `remote_pairing.rs::token_refresh_meta`。 */
export function refreshRequestMeta(room: string, deviceId: string, requestId: string): EnvelopeMeta {
  return {
    v: REFRESH_PROTOCOL_VERSION,
    room,
    epoch: 0,
    kind: "token.refresh",
    session: deviceId,
    command_id: requestId,
  };
}

/** `token.refresh.ok` 回执密文体 AAD meta——对齐 `remote_pairing.rs::token_refresh_ok_meta`。 */
export function refreshOkMeta(room: string, deviceId: string, requestId: string): EnvelopeMeta {
  return {
    v: REFRESH_PROTOCOL_VERSION,
    room,
    epoch: 0,
    kind: "token.refresh.ok",
    session: deviceId,
    command_id: requestId,
  };
}

// ---------------------------------------------------------------------------
// 出站:token.refresh 请求
// ---------------------------------------------------------------------------

export interface TokenRefreshRequestFrame {
  t: "token.refresh";
  request_id: string;
  ct: string;
  n: string;
}

/** 密封请求密文体——明文形状 `{"refresh_token":"<hex64>"}`,与 `aad_kat_token_refresh` KAT 一致。 */
export async function sealRefreshRequestBody(
  kPair: Uint8Array,
  room: string,
  deviceId: string,
  requestId: string,
  refreshTokenHex: string,
): Promise<{ ct: string; n: string }> {
  const plaintext = utf8Bytes(JSON.stringify({ refresh_token: refreshTokenHex }));
  return seal(kPair, refreshRequestMeta(room, deviceId, requestId), plaintext);
}

export function buildTokenRefreshFrame(requestId: string, ct: string, n: string): TokenRefreshRequestFrame {
  return { t: "token.refresh", request_id: requestId, ct, n };
}

// ---------------------------------------------------------------------------
// 入站:token.refresh.ok / token.refresh.fail
// ---------------------------------------------------------------------------

export interface TokenRefreshOkFrame {
  t: "token.refresh.ok";
  request_id: string;
  subject: string;
  generation: number;
  ct: string;
  n: string;
}

export interface TokenRefreshFailFrame {
  t: "token.refresh.fail";
  request_id: string;
  subject: string;
  reason: string;
  /** 仅当桌面判连续 ≥3 次无效才带 true;省略 = 良性单飞行冲突等,不断连(M0 §9.6)。 */
  close?: boolean;
}

export type ParsedRefreshResponse =
  | { kind: "ok"; frame: TokenRefreshOkFrame }
  | { kind: "fail"; frame: TokenRefreshFailFrame }
  | { kind: "not_a_refresh_response" };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function isString(value: unknown): value is string {
  return typeof value === "string";
}

/**
 * `generation` grammar——对齐 `room-do.js::tokenRefreshReceiptShapeError`:
 * `Number.isSafeInteger(payload.generation) && payload.generation > 0`(正整数,不接受 0/负数/
 * 浮点/非 safe-integer)。
 */
function isValidGeneration(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

/**
 * 判别 + 结构校验一条已解析的 JSON 值是不是 `token.refresh.ok`/`token.refresh.fail`——不是这两
 * 种 `t`,或字段形状不对,一律 `not_a_refresh_response`(调用方按"不认识/忽略"处理,不抛异常,
 * 呼应 `pairing-session.ts::handleFrame` 同款"结构不合法不抛异常"约定)。
 *
 * **审查返工**:`request_id` 额外过 `isValidRequestId()`(非空·UTF-8 ≤128B,对齐 relay 权威);
 * `generation`(仅 ok 帧)改用 `isValidGeneration()`(正整数,不再接受 0/负数/浮点);`close`
 * (仅 fail 帧)若字段**存在**但不是 boolean 直接判整帧不合法(不再静默把非法值坍缩成
 * `undefined`——静默坍缩会让"畸形 close 值"这个信号被吞掉,呼应 relay `close_invalid` 的拒绝态度)。
 */
export function parseRefreshResponseFrame(raw: unknown): ParsedRefreshResponse {
  if (!isRecord(raw) || !isString(raw.t)) {
    return { kind: "not_a_refresh_response" };
  }
  if (raw.t === "token.refresh.ok") {
    if (
      !isValidRequestId(raw.request_id) ||
      !isString(raw.subject) ||
      !isValidGeneration(raw.generation) ||
      !isString(raw.ct) ||
      !isString(raw.n)
    ) {
      return { kind: "not_a_refresh_response" };
    }
    return {
      kind: "ok",
      frame: {
        t: "token.refresh.ok",
        request_id: raw.request_id,
        subject: raw.subject,
        generation: raw.generation,
        ct: raw.ct,
        n: raw.n,
      },
    };
  }
  if (raw.t === "token.refresh.fail") {
    if (!isValidRequestId(raw.request_id) || !isString(raw.subject) || !isString(raw.reason)) {
      return { kind: "not_a_refresh_response" };
    }
    if (Object.hasOwn(raw, "close") && typeof raw.close !== "boolean") {
      // close 字段存在但形状不对——按 relay `close_invalid` 同款态度整帧拒绝,不静默坍缩。
      return { kind: "not_a_refresh_response" };
    }
    const close = typeof raw.close === "boolean" ? raw.close : undefined;
    return {
      kind: "fail",
      frame: { t: "token.refresh.fail", request_id: raw.request_id, subject: raw.subject, reason: raw.reason, close },
    };
  }
  return { kind: "not_a_refresh_response" };
}

export interface RotatedTokenPair {
  capabilityToken: string;
  refreshToken: string;
}

export class RefreshOkDecodeError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "RefreshOkDecodeError";
  }
}

/**
 * 解密并校验 `token.refresh.ok` 密文体——明文形状 `{"capability_token","refresh_token"}`,对齐
 * `aad_kat_token_refresh_ok` KAT。AEAD 认证失败(密钥/AAD 不对、密文被篡改)与明文形状不对
 * (JSON 解析失败/字段缺失/不是 hex64)统一走同一个错误类型,不向调用方泄露"是密文坏还是明文形状
 * 坏"(同 `remote_pairing.rs::open_token_refresh_request` 的"不额外区分子原因"设计意图)。
 */
export async function openRefreshOkBody(
  kPair: Uint8Array,
  room: string,
  deviceId: string,
  requestId: string,
  ct: string,
  n: string,
): Promise<RotatedTokenPair> {
  let plaintext: Uint8Array;
  try {
    plaintext = await open(kPair, refreshOkMeta(room, deviceId, requestId), ct, n);
  } catch (cause) {
    throw new RefreshOkDecodeError("token.refresh.ok ciphertext failed to decrypt/authenticate", { cause });
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(plaintext));
  } catch (cause) {
    throw new RefreshOkDecodeError("token.refresh.ok plaintext is not valid JSON", { cause });
  }
  if (!isRecord(parsed) || !isString(parsed.capability_token) || !isString(parsed.refresh_token)) {
    throw new RefreshOkDecodeError("token.refresh.ok plaintext has unexpected shape");
  }
  if (!isHex64(parsed.capability_token) || !isHex64(parsed.refresh_token)) {
    throw new RefreshOkDecodeError("token.refresh.ok plaintext tokens are not hex64");
  }
  return { capabilityToken: parsed.capability_token, refreshToken: parsed.refresh_token };
}
