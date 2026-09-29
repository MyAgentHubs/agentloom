"use strict";
// envelope.js — 信封 parse/validate（S1/S2 共用）。
//
// relay 只解析/校验信封的「外层字段」，绝不尝试解密 `ct`（relay 没有任何内容钥匙——
// 这是 E2EE 的边界，见 remote-relay/README.md「relay 只见密文」一节）。
//
// Keep routing fields in a single envelope so nested metadata cannot disagree with them:
//   { v, room, epoch, kind, session, command_id, seq, ct, n, ts }
// - kind："event"（里程碑·落库盖 seq）| "live"（只转发·永不落库·seq 恒 null）|
//   "input" | "control" | "presence" | "reply"（v1.9 §10.1·桌面→relay→定向远端·
//   不落库不广播·seq 恒 null）——milestone 布尔位已废弃，改由 kind 本身
//   区分 event/live，不再需要外层旗标。
// - command_id 从外层包装升级为信封顶层字段，kind=input|control|reply 均必填；
//   kind=event|live|presence 禁止携带，应为 null/undefined。
// - AAD 拼接顺序相应调整：删 seq（relay 盖的、不该入密）、加 command_id
//   （发送方给的、relay 要读的路由手柄）——见下方 buildAAD。

export const PROTOCOL_VERSION = 1;

// v1.9（M0 §10.1）：新增 `reply`——桌面对某条 control 帧（msg.fetch）的定向
// 应答，relay 按顶层 command_id 路由投递给指定远端，不落库不广播、seq 恒
// null。与 input/control 同构：command_id 必填非空、入 AAD；client_msg_id 与
// 其它非 event kind 一样禁止携带。
export const ENVELOPE_KINDS = Object.freeze(["event", "live", "input", "control", "presence", "reply"]);

// 房间 id = 128 位随机数的十六进制表示 = 32 个 hex 字符（母文档 §0）。
export const ROOM_ID_RE = /^[0-9a-f]{32}$/;

const BASE64_RE = /^[A-Za-z0-9+/]*={0,2}$/;

function isBase64(str) {
  if (typeof str !== "string" || str.length === 0) return false;
  if (str.length % 4 !== 0) return false;
  return BASE64_RE.test(str);
}

// base64 解码后的字节数，不实际解码（relay 不需要、也不应该摸 ct 的内容）。
export function base64DecodedByteLength(str) {
  if (!isBase64(str)) return -1;
  const padding = str.endsWith("==") ? 2 : str.endsWith("=") ? 1 : 0;
  return (str.length / 4) * 3 - padding;
}

function isNonNegativeInt(n) {
  return Number.isInteger(n) && n >= 0;
}

function isNullableString(v) {
  return v === null || v === undefined || typeof v === "string";
}

/**
 * pair.accept 是配对控制帧，不是 §1 内容信封；relay 只校验并转发外层形状，
 * 不读取 K_pair 下的 tokens_ct 密文体。
 */
export function validatePairAcceptFrame(frame) {
  if (!frame || typeof frame !== "object" || Array.isArray(frame)) {
    return { ok: false, errors: ["pair_accept_not_object"] };
  }
  if (Object.hasOwn(frame, "capability_token") || Object.hasOwn(frame, "refresh_token")) {
    return { ok: false, errors: ["plaintext_token_forbidden"] };
  }

  for (const field of ["room", "device_id", "k_room_ct", "k_room_n", "tokens_ct", "tokens_n"]) {
    if (typeof frame[field] !== "string" || frame[field].length === 0) {
      return { ok: false, errors: [`${field}_required`] };
    }
  }
  if (!ROOM_ID_RE.test(frame.room)) {
    return { ok: false, errors: ["room_invalid"] };
  }
  return { ok: true, errors: [] };
}

/**
 * 校验一个信封对象是否符合 §1 wire format。
 * 返回 { ok: true, envelope } 或 { ok: false, errors: string[] }。
 * 不对 ct 做任何解密尝试；只校验 ct/n 是否是形状合法的 base64、n 解码后是否恰好 12 字节
 * （AES-256-GCM 标准 nonce 长度，§5 定稿参数）。
 */
export function validateEnvelope(obj) {
  const errors = [];

  if (!obj || typeof obj !== "object") {
    return { ok: false, errors: ["envelope_not_object"] };
  }

  if (obj.v !== PROTOCOL_VERSION) {
    // 母文档 §1：「不认识的 v 直接拒连」——version 不对不是普通字段错误，单独标注原因
    errors.push("unsupported_version");
  }

  if (typeof obj.room !== "string" || !ROOM_ID_RE.test(obj.room)) {
    errors.push("bad_room");
  }

  if (!isNonNegativeInt(obj.epoch)) {
    errors.push("bad_epoch");
  }

  if (typeof obj.kind !== "string" || !ENVELOPE_KINDS.includes(obj.kind)) {
    errors.push("bad_kind");
  }

  // seq：仅 kind=event 且里程碑时由 relay 盖；其余场景应为 null/undefined。
  // 入站信封（客户端发来的）通常没有 seq——relay 负责盖；出站补发/回放信封会带 seq。
  if (obj.seq !== null && obj.seq !== undefined && !isNonNegativeInt(obj.seq)) {
    errors.push("bad_seq");
  } else if (obj.seq !== null && obj.seq !== undefined && obj.kind !== "event") {
    errors.push("seq_not_allowed_for_kind");
  }

  if (!isNullableString(obj.session)) {
    errors.push("bad_session");
  }
  if (typeof obj.session === "string" && obj.session.includes("|")) {
    errors.push("session_must_not_contain_pipe");
  }
  // 按 UTF-8 字节计数，与 command_id 同一口径同算法（G7 收紧·session grammar 残余项）。
  if (typeof obj.session === "string" && new TextEncoder().encode(obj.session).length > 128) {
    errors.push("session_too_long");
  }

  // command_id：kind=input|control|reply 必须携带非空字符串；
  // kind=event|live|presence 禁止携带（母文档 v1.7 §1/§3；v1.9 §10.1 把
  // reply 并入「与 input/control 同构」的必填集合）。顶层认证版是唯一真相，
  // 密文内不重复。
  if (!isNullableString(obj.command_id)) {
    errors.push("bad_command_id");
  } else if (
    (obj.kind === "input" || obj.kind === "control" || obj.kind === "reply") &&
    (obj.command_id === null || obj.command_id === undefined || obj.command_id === "")
  ) {
    errors.push("command_id_required_for_kind");
  } else if (
    (obj.kind === "event" || obj.kind === "live" || obj.kind === "presence") &&
    obj.command_id !== null &&
    obj.command_id !== undefined
  ) {
    errors.push("command_id_not_allowed_for_kind");
  }
  if (typeof obj.command_id === "string" && obj.command_id.includes("|")) {
    errors.push("command_id_must_not_contain_pipe");
  }
  // 按 UTF-8 字节计数，与桌面 Rust `command_id.len()` 对齐（M0 v1.7.3 附注 N-6）。
  if (typeof obj.command_id === "string" && new TextEncoder().encode(obj.command_id).length > 128) {
    errors.push("command_id_too_long");
  }

  // client_msg_id：kind=event 必须携带非空字符串；kind=live|input|control|presence
  // 禁止携带（母文档 v1.7.4 §1）。relay 用它做里程碑落库去重手柄，不入 AAD。
  if (!isNullableString(obj.client_msg_id)) {
    errors.push("bad_client_msg_id");
  } else if (
    obj.kind === "event" &&
    (obj.client_msg_id === null || obj.client_msg_id === undefined || obj.client_msg_id === "")
  ) {
    errors.push("client_msg_id_required_for_kind");
  } else if (
    obj.kind !== "event" &&
    obj.client_msg_id !== null &&
    obj.client_msg_id !== undefined
  ) {
    errors.push("client_msg_id_not_allowed_for_kind");
  }
  if (typeof obj.client_msg_id === "string" && obj.client_msg_id.includes("|")) {
    errors.push("client_msg_id_must_not_contain_pipe");
  }
  // 按 UTF-8 字节计数，与 command_id 同一口径（N-6 教训：禁止用 .length 代替）。
  if (typeof obj.client_msg_id === "string" && new TextEncoder().encode(obj.client_msg_id).length > 64) {
    errors.push("client_msg_id_too_long");
  }

  if (!isBase64(obj.ct)) {
    errors.push("bad_ct");
  }

  if (!isBase64(obj.n) || base64DecodedByteLength(obj.n) !== 12) {
    errors.push("bad_nonce"); // AES-256-GCM nonce 必须是 12 字节
  }

  if (!isNonNegativeInt(obj.ts)) {
    errors.push("bad_ts");
  }

  if (errors.length > 0) {
    return { ok: false, errors };
  }

  return {
    ok: true,
    envelope: {
      v: obj.v,
      room: obj.room,
      epoch: obj.epoch,
      kind: obj.kind,
      session: obj.session ?? null,
      command_id: obj.command_id ?? null,
      client_msg_id: obj.client_msg_id ?? null,
      seq: obj.seq ?? null,
      ct: obj.ct,
      n: obj.n,
      ts: obj.ts,
    },
  };
}

/**
 * AEAD 附加数据（AAD）= v|room|epoch|kind|session|command_id 拼接（母文档订正版 §1）。
 * relay 本身没有钥匙、不做实际的 AEAD 校验——这个函数存在的意义是给整条协议线
 * （relay / 桌面 D1 / Web C1）一个唯一的、可单测锁定的参考实现，防止三端对
 * 「null 字段怎么拼」各写各的、悄悄产生解密失败。
 *
 * Exclude seq from authenticated data because the relay assigns it after encryption,
 * so the sender cannot know its value when authenticating the envelope.
 * Include command_id to bind the sender-provided routing handle to the ciphertext.
 *
 * 约定（母文档未明确处的补充，供实现者对齐）：null/undefined 字段序列化为空
 * 字符串——例如 event/live/presence 信封 command_id=null → 该段落为 ""。
 */
export function buildAAD({ v, room, epoch, kind, session, command_id }) {
  const part = (x) => (x === null || x === undefined ? "" : String(x));
  return [part(v), part(room), part(epoch), part(kind), part(session), part(command_id)].join("|");
}

// Treat kind as the sole authority for milestone classification so routing and persistence
// cannot disagree with a separate milestone flag on an outer wrapper.
// Event envelopes represent milestones; live envelopes are transient and never persisted.
// This helper provides a shared classification rule for callers that need this distinction.
// Keeping that rule explicit prevents separate callers from inventing conflicting checks.
export function isMilestone(envelope) {
  return envelope.kind === "event";
}
