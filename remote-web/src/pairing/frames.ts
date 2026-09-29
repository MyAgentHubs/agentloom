// frames.ts — T6b · pair.* 帧族的 wire 形状 + 字段抽取校验。
//
// 权威参照（只读对照，未改动）：
//   - remote-relay/test/s1ja-fake-mobile-e2e.test.js 的 `send(room, ws, {...})` 调用点
//     （pair.hello/pair.accept/pair.done/pair.ready 四帧，逐字段照抄）。
//   - app/src-tauri/src/remote_gateway.rs 的 `PairHelloFrame`/`PairAcceptFrame`/`PairDoneFrame`/
//     `PairReadyFrame` 结构体字段名 + `parse_pair_hello`/`parse_pair_done` 两个解析函数（必填
//     字段口径）。
//   - remote-relay/fixtures/wire-v1.json 的 `pair_hello_forward_origin_valid` /
//     `pair_accept_encrypted_tokens_valid` / `pair_accept_tokens_ct_missing` /
//     `pair_accept_plaintext_tokens_forbidden` / `pair_ready_valid` 五条样张。
//
// **remote_pub 编码裁定（同 qr-payload.ts 头注）**：remote_gateway.rs:4423-4424
// `STANDARD.decode(value.get("remote_pub")...)` + wire-v1.json 样张值（`"ERERE...E="`，带 base64
// padding）两处独立、可测的证据一致确认 `remote_pub` 是标准 base64（32 字节），不是 hex。
// s1ja 测试自己发 hello 时把 `remote_pub` 填成 `mobileKeys.publicHex`（hex）——那是它自封自测的
// 假手机 actor 内部简写：s1ja 全程只靠 RoomDO 转发裸字节，从不解码这个字段，所以两种编码对它的
// 路由测试都"能过"，但只有 base64 那条路径真的会被真实桌面 `parse_pair_hello` 接受。本文件按
// 更强证据（真实解析器 + 独立 fixture 样张）为准，在 ⑥ 偏离说明里如实记录这处分歧，不视为 M0
// 语义矛盾（remote_pairing.rs 本身不描述字节编码，`s1ja` 与它之间没有真正的协议层冲突）。

export interface OutboundPairHelloFrame {
  t: "pair.hello";
  room: string;
  /** 标准 base64，32 字节 X25519 公钥。 */
  remote_pub: string;
  token_ct: string;
  token_n: string;
}

export interface OutboundPairDoneFrame {
  t: "pair.done";
  room: string;
  device_id: string;
  confirm_ct: string;
  confirm_n: string;
}

export type OutboundPairingFrame = OutboundPairHelloFrame | OutboundPairDoneFrame;

/** pair.accept 六字段（M0 §9.5：`{room, device_id, k_room_ct, k_room_n, tokens_ct, tokens_n}`）。 */
export interface PairAcceptFields {
  room: string;
  deviceId: string;
  kRoomCt: string;
  kRoomN: string;
  tokensCt: string;
  tokensN: string;
}

export function extractPairAcceptFields(frame: Record<string, unknown>): PairAcceptFields | null {
  const { room, device_id: deviceId, k_room_ct: kRoomCt, k_room_n: kRoomN, tokens_ct: tokensCt, tokens_n: tokensN } = frame;
  if (
    typeof room !== "string" ||
    typeof deviceId !== "string" ||
    typeof kRoomCt !== "string" ||
    typeof kRoomN !== "string" ||
    typeof tokensCt !== "string" ||
    typeof tokensN !== "string"
  ) {
    return null;
  }
  return { room, deviceId, kRoomCt, kRoomN, tokensCt, tokensN };
}

/** pair.ready 四字段（M0 §9.5：`{room, device_id, ct, n}`）。 */
export interface PairReadyFields {
  room: string;
  deviceId: string;
  ct: string;
  n: string;
}

export function extractPairReadyFields(frame: Record<string, unknown>): PairReadyFields | null {
  const { room, device_id: deviceId, ct, n } = frame;
  if (typeof room !== "string" || typeof deviceId !== "string" || typeof ct !== "string" || typeof n !== "string") {
    return null;
  }
  return { room, deviceId, ct, n };
}

/** pair.accept 令牌密文体明文形状（remote_pairing.rs::PairAcceptTokens·wire-v1.json aad_kat_pair_accept_tokens 字节级钉死）。 */
export interface PairAcceptTokens {
  capability_token: string;
  refresh_token: string;
}

export function parsePairAcceptTokens(plaintext: Uint8Array): PairAcceptTokens {
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(plaintext));
  } catch (cause) {
    throw new Error("pair.accept tokens plaintext is not valid JSON", { cause });
  }
  if (typeof parsed !== "object" || parsed === null) {
    throw new Error("pair.accept tokens plaintext must be a JSON object");
  }
  const record = parsed as Record<string, unknown>;
  if (typeof record.capability_token !== "string" || typeof record.refresh_token !== "string") {
    throw new Error("pair.accept tokens plaintext missing capability_token/refresh_token");
  }
  return { capability_token: record.capability_token, refresh_token: record.refresh_token };
}
