// wireEnvelope.ts — INT1b · 外层 wire 信封（M0 §1）的解析/构造。
//
// `crypto/envelope.ts` 只做 AAD 拼串 + AEAD seal/open，明确把"构造/解析完整信封"这层留给
// "T6d1 事件内核 / T6f3 指令面接线"（该文件头注原话）——本文件就是这层，在 `src/app/` 内新写，
// 不碰 `crypto/`/`events/`。
//
// Authoritative reference (read-only):
// The wire envelope contract (M0 §1: envelope fields, AAD concatenation order, plaintext
// exceptions); `crypto/envelope.test.ts`'s `WireEnvelope`/`envelopeToMeta` (a test-only helper with
// the same field layout as this file — this file is its production-side counterpart; it does not
// import the test file, they are independent).

import type { EnvelopeMeta } from "../crypto/envelope.ts";

/**
 * 解析后的外层信封——`seq`/`client_msg_id`/`ts` 仅 `kind=event` 非 null（M0 §1：其余 kind 恒
 * null/禁止携带），本类型统一用 `null` 表达"此 kind 下不适用或缺省"，不强制调用方按 kind 分支
 * 判断字段是否存在。
 */
export interface WireEnvelope {
  v: number;
  room: string;
  epoch: number;
  kind: string;
  session: string | null;
  commandId: string | null;
  /** relay 盖的房间级 seq——仅 `kind=event` 非 null。 */
  seq: number | null;
  /** 去重手柄——仅 `kind=event` 非 null（M0 §1 v1.7.4，不入 AAD）。 */
  clientMsgId: string | null;
  /** 未认证字段，仅供显示，禁止参与任何安全判断（M0 §1）——这里原样保留，不用于任何决策。 */
  ts: number | null;
  ct: string;
  n: string;
}

/**
 * 最小结构校验——只保证下游 `open()`/`buildAAD()` 需要的字段类型对得上，不重复 relay
 * `envelope.js::validateEnvelope()` 的完整 wire grammar（长度上限/`|` 禁令等，`crypto/envelope.ts`
 * 头注已经把这条边界画清楚：语法校验不在加密内核，也不该在这里悄悄重开一套）。结构不对 → `null`，
 * 调用方按"畸形帧，丢弃"处理，不抛异常。
 */
export function parseWireEnvelope(raw: unknown): WireEnvelope | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.v !== "number") return null;
  if (typeof r.room !== "string") return null;
  if (typeof r.epoch !== "number") return null;
  if (typeof r.kind !== "string") return null;
  if (r.session !== null && typeof r.session !== "string") return null;
  if (r.command_id !== undefined && r.command_id !== null && typeof r.command_id !== "string") return null;
  if (typeof r.ct !== "string") return null;
  if (typeof r.n !== "string") return null;

  return {
    v: r.v,
    room: r.room,
    epoch: r.epoch,
    kind: r.kind,
    session: (r.session as string | null | undefined) ?? null,
    commandId: (r.command_id as string | null | undefined) ?? null,
    seq: typeof r.seq === "number" ? r.seq : null,
    clientMsgId: typeof r.client_msg_id === "string" ? r.client_msg_id : null,
    ts: typeof r.ts === "number" ? r.ts : null,
    ct: r.ct,
    n: r.n,
  };
}

/** 挑出 `crypto/envelope.ts::buildAAD`/`open`/`seal` 需要的那 6 个字段。 */
export function envelopeMeta(envelope: Pick<WireEnvelope, "v" | "room" | "epoch" | "kind" | "session" | "commandId">): EnvelopeMeta {
  return {
    v: envelope.v,
    room: envelope.room,
    epoch: envelope.epoch,
    kind: envelope.kind,
    session: envelope.session,
    command_id: envelope.commandId,
  };
}

/** 指令通道的两种信封 `kind`（M0 §3）——`input.send`/`input.answer` 走 `"input"`，
 *  `control.stop`/`control.snapshot` 走 `"control"`。 */
export type CommandEnvelopeKind = "input" | "control";

/**
 * 构造一条待发送的指令面信封（T6f3 起覆盖 `kind=input`/`kind=control` 两种——`ct`/`n` 由调用方先经
 * `crypto/envelope.ts::seal()` 算好再传入，本函数只负责拼出完整 JSON 形状（M0 §1：`seq` 恒 null·
 * `client_msg_id` 该 kind 下禁止携带，不出现在输出对象里）。
 */
export function buildCommandEnvelope(params: {
  kind: CommandEnvelopeKind;
  room: string;
  epoch: number;
  session: string;
  commandId: string;
  ct: string;
  n: string;
  now: () => number;
}): Record<string, unknown> {
  return {
    v: 1,
    room: params.room,
    epoch: params.epoch,
    kind: params.kind,
    session: params.session,
    command_id: params.commandId,
    seq: null,
    ct: params.ct,
    n: params.n,
    ts: params.now(),
  };
}

/**
 * `buildCommandEnvelope({kind:"control", ...})` 的薄别名——INT1c 起 `AppRuntime.tsx` 的
 * `control.snapshot` 请求路径在用，T6f3 把它收窄成 `buildCommandEnvelope` 的特化版而不是删掉，
 * 保持既有调用点/既有测试（`wireEnvelope.test.ts`）零改动。
 */
export function buildControlEnvelope(params: {
  room: string;
  epoch: number;
  session: string;
  commandId: string;
  ct: string;
  n: string;
  now: () => number;
}): Record<string, unknown> {
  return buildCommandEnvelope({ kind: "control", ...params });
}
