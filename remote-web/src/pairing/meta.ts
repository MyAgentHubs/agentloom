// meta.ts — T6b · 配对四个密文体的 AAD meta 构造（M0 §9.5「AAD 口径统一」段）。
//
// 权威参照（只读对照，未改动）：app/src-tauri/src/remote_pairing.rs 的
// `pairing_envelope_meta`/`pair_done_confirm_meta`/`pair_ready_meta`/`pair_accept_tokens_meta`
// ——kind 字面量、session 装什么、command_id 恒 null，逐字段照抄，不自创。
// wire-v1.json `aad_kat_pair_ready`/`aad_kat_pair_accept_tokens` 两条样张核对过 AAD 拼串本身
// （`expect.aad`）与这里的字段组合一致（这两条样张的 plaintext 按 note 明写"非本单契约"，本文件
// 不依赖它们的 plaintext 形状）。

import type { EnvelopeMeta } from "../crypto/envelope.ts";

export const PAIRING_PROTOCOL_VERSION = 1;

/** pair.hello 密文体（token 证明）AAD meta。kind="control"（remote_pairing.rs 现状实现·M0 §5 标注 provisional，待 relay/Web 端对表——本单按桌面现状实现，不自创新 kind）。 */
export function helloEnvelopeMeta(room: string): EnvelopeMeta {
  return { v: PAIRING_PROTOCOL_VERSION, room, epoch: 0, kind: "control", session: null, command_id: null };
}

/** pair.done 确认体 AAD meta（remote_pairing.rs::pair_done_confirm_meta）。session 装 device_id。 */
export function pairDoneConfirmMeta(room: string, deviceId: string): EnvelopeMeta {
  return { v: PAIRING_PROTOCOL_VERSION, room, epoch: 0, kind: "pair-confirm", session: deviceId, command_id: null };
}

/** pair.ready 密文体 AAD meta（remote_pairing.rs::pair_ready_meta）。 */
export function pairReadyMeta(room: string, deviceId: string): EnvelopeMeta {
  return { v: PAIRING_PROTOCOL_VERSION, room, epoch: 0, kind: "pair-ready", session: deviceId, command_id: null };
}

/** pair.accept 令牌密文体 AAD meta（remote_pairing.rs::pair_accept_tokens_meta）。 */
export function pairAcceptTokensMeta(room: string, deviceId: string): EnvelopeMeta {
  return { v: PAIRING_PROTOCOL_VERSION, room, epoch: 0, kind: "pair-accept-tokens", session: deviceId, command_id: null };
}
