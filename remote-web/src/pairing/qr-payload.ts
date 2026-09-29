// qr-payload.ts: parse QR links (`#p=<base64url(JSON)>`) into QrPayload.
//
// Field names and desktop_pub encoding follow app/src-tauri/src/remote_pairing.rs::QrPayload.
// Its test `qr_payload_uses_protocol_field_names_and_public_key_shape`
// (remote_pairing.rs:1846-1865) fixes desktop_pub as standard base64 encoding 32 bytes
// and pairing_token as 64 lowercase hex characters. A QR code contains an openable link;
// the payload lives in its URL fragment `#p=`, with bare JSON parsing as a legacy fallback.
//
// Strict relay_url validation and origin matching: `parseValidRelayUrl`/`relayHttpsOrigin`
// mirror every check in app/src/components/settings/SettingsRemoteControl.tsx:229-268.
// That desktop QR implementation is duplicated rather than imported because its React/TSX
// code and the mobile SPA are separate builds. Require wss, no userinfo, a nonempty host,
// a root path, no query/hash, and canonical href equality except a WHATWG-added trailing `/`.
// Also reject raw `?`/`#` to catch `wss://host/?` and `wss://host/#` despite canonical equality.
// For a full QR URL (`https://<host>/#p=<payload>`), require the outer https origin to match
// the https origin derived from payload.relay_url's wss origin. Bare fragment values and
// bare JSON have no independent outer origin, so this additional check does not apply.
//
// **desktop_pub 编码裁定（偏离 s1ja 测试自身写法，见 pairing-session.ts 头注同款说明的简版）**：
// remote-relay/test/s1ja-fake-mobile-e2e.test.js 自己构造的假 QR payload 用 `desktopKeys.publicHex`
// （hex）——那是它自封自测的假桌面/假手机两个 actor 的内部简写，从未走过
// remote_pairing.rs::QrPayload 的真实序列化路径。真实桌面产出的 desktop_pub 字段 = 标准 base64
// （remote_pairing.rs:64 `STANDARD.encode(desktop_public)` + 上述测试锁死），本文件按此为准。

import { decodeCanonicalBase64 } from "../crypto/bytes.ts";

export interface QrPayload {
  v: number;
  relay_url: string;
  room: string;
  pairing_token: string;
  /** 标准 base64（非 hex）编码的 32 字节 X25519 公钥。 */
  desktop_pub: string;
}

export class QrPayloadError extends Error {
  constructor(
    public readonly code: QrPayloadErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "QrPayloadError";
  }
}

export type QrPayloadErrorCode =
  | "fragment_decode_failed"
  | "json_parse_failed"
  | "shape_invalid";

const ROOM_RE = /^[0-9a-f]{32}$/;
const PAIRING_TOKEN_RE = /^[0-9a-f]{64}$/;

const FRAGMENT_MARKER = "#p=";

/**
 * 从二维码承载的字符串解析出 QrPayload。接受三种输入形状：
 * ① 完整 URL（含 `#p=<base64url>` fragment）——额外校验外层 https origin 与 payload 的 wss
 *   origin 一一对应；② 已提取出的裸 `#p=` 值本身（base64url，无需前缀，没有外层 origin 可比对）；
 * ③ 裸 JSON 文本（老格式兜底·M2 C1 spec §3 第 1 条「粘贴配对串」）。
 */
export function parseQrPayload(input: string): QrPayload {
  const trimmed = input.trim();
  if (trimmed.startsWith("{")) {
    return validateShape(safeJsonParse(trimmed));
  }

  const markerIndex = trimmed.indexOf(FRAGMENT_MARKER);
  if (markerIndex === -1) {
    // 裸 base64url fragment 值——没有外层 URL，不做 origin 比对。
    return decodeFragmentPayload(trimmed);
  }

  const outerUrlString = trimmed.slice(0, markerIndex);
  const fragmentValue = trimmed.slice(markerIndex + FRAGMENT_MARKER.length);

  let outerUrl: URL;
  try {
    outerUrl = new URL(outerUrlString);
  } catch {
    throw new QrPayloadError("shape_invalid", "QR URL prefix before #p= is not a valid URL");
  }
  if (outerUrl.protocol !== "https:") {
    throw new QrPayloadError("shape_invalid", "QR URL must use https");
  }

  const payload = decodeFragmentPayload(fragmentValue);
  // validateShape 已经拒绝过 relay_url 形态非法的情形，这里的 parseValidRelayUrl 必不为 null。
  const parsedRelay = parseValidRelayUrl(payload.relay_url);
  if (!parsedRelay || relayHttpsOrigin(parsedRelay) !== `https://${outerUrl.host}`) {
    throw new QrPayloadError("shape_invalid", "QR URL https origin does not match payload's wss origin");
  }
  return payload;
}

function decodeFragmentPayload(fragmentValue: string): QrPayload {
  const decodedBytes = decodeBase64Url(fragmentValue);
  if (decodedBytes === null) {
    throw new QrPayloadError("fragment_decode_failed", "QR payload fragment is not valid base64url");
  }
  const json = new TextDecoder().decode(decodedBytes);
  return validateShape(safeJsonParse(json));
}

/** 从完整 URL 里摘出 `#p=` 后面的原始值；找不到 marker 时返回 null（调用方回落成整串当值）。 */
export function extractFragmentValue(url: string): string | null {
  const index = url.indexOf(FRAGMENT_MARKER);
  if (index === -1) {
    return null;
  }
  return url.slice(index + FRAGMENT_MARKER.length);
}

/**
 * relay_url 严格校验（逐条复刻桌面 `SettingsRemoteControl.tsx::parseValidRelayUrl`，见文件头
 * 注）：wss 协议 + 无 userinfo + host 非空 + 根路径 + 无 query/hash + canonical href 等值（唯一
 * 允许差异=WHATWG 给根路径补的尾随 `/`）+ 原始串不含 `?`/`#`（双保险，堵 canonical 比对测不出的
 * 漏网形态）。
 */
export function parseValidRelayUrl(value: string): URL | null {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.protocol !== "wss:") return null;
  if (url.username !== "" || url.password !== "") return null;
  if (!url.host) return null;
  if (url.pathname !== "/" && url.pathname !== "") return null;
  if (url.search !== "") return null;
  if (url.hash !== "") return null;
  if (url.href !== value && url.href !== `${value}/`) return null;
  if (value.includes("?") || value.includes("#")) return null;
  return url;
}

/** wss host（含端口）派生的对应 https origin（桌面 `relayHttpsOrigin` 同款：host 原样接到 `https://` 后，不做默认端口宽容映射）。 */
export function relayHttpsOrigin(url: URL): string {
  return `https://${url.host}`;
}

/** QrPayload.desktop_pub 解码成裸 32 字节公钥（标准 base64，见文件头注）。 */
export function desktopPublicKeyBytes(payload: QrPayload): Uint8Array {
  const decoded = decodeCanonicalBase64(payload.desktop_pub);
  if (decoded === null || decoded.length !== 32) {
    throw new QrPayloadError("shape_invalid", "desktop_pub must decode to exactly 32 bytes of standard base64");
  }
  return decoded;
}

function safeJsonParse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new QrPayloadError("json_parse_failed", "QR payload is not valid JSON");
  }
}

function validateShape(candidate: unknown): QrPayload {
  if (typeof candidate !== "object" || candidate === null) {
    throw new QrPayloadError("shape_invalid", "QR payload must be a JSON object");
  }
  const record = candidate as Record<string, unknown>;
  if (record.v !== 1) {
    throw new QrPayloadError("shape_invalid", "QR payload v must be 1");
  }
  if (typeof record.relay_url !== "string" || parseValidRelayUrl(record.relay_url) === null) {
    throw new QrPayloadError("shape_invalid", "QR payload relay_url failed strict wss validation");
  }
  if (typeof record.room !== "string" || !ROOM_RE.test(record.room)) {
    throw new QrPayloadError("shape_invalid", "QR payload room must be 32 lowercase hex chars");
  }
  if (typeof record.pairing_token !== "string" || !PAIRING_TOKEN_RE.test(record.pairing_token)) {
    throw new QrPayloadError("shape_invalid", "QR payload pairing_token must be 64 lowercase hex chars");
  }
  if (typeof record.desktop_pub !== "string" || decodeCanonicalBase64(record.desktop_pub)?.length !== 32) {
    throw new QrPayloadError("shape_invalid", "QR payload desktop_pub must be standard base64 of 32 bytes");
  }
  return {
    v: record.v,
    relay_url: record.relay_url,
    room: record.room,
    pairing_token: record.pairing_token,
    desktop_pub: record.desktop_pub,
  };
}

/** base64url（RFC 4648 §5，无 padding）解码——QR fragment 专用；base64 字母表本身不含需要转义的字符，故不做 URL 百分号解码。 */
function decodeBase64Url(value: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(value)) {
    return null;
  }
  const withPadding = value.replace(/-/g, "+").replace(/_/g, "/");
  const paddedLength = Math.ceil(withPadding.length / 4) * 4;
  const padded = withPadding.padEnd(paddedLength, "=");
  return decodeCanonicalBase64WithoutPaddingCheck(padded);
}

// decodeCanonicalBase64 (crypto/bytes.ts) 要求输入本身就是 canonical padded base64——base64url 在
// 转换成标准字母表 + 补 padding 后已经是这个形状，直接复用即可，不需要单独的宽松解码路径。
function decodeCanonicalBase64WithoutPaddingCheck(padded: string): Uint8Array | null {
  return decodeCanonicalBase64(padded);
}
