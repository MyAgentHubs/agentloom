// pairing-session.ts — T6b · PairingSession 状态机（手机侧配对远端半边）。
//
// 覆盖 M0 §9.5 的完整配对窗口令牌流：扫码 payload → connect_token 派生 → pair.hello 构造 → 收
// pair.accept（解 K_room + 令牌）→ 发 pair.done（K_room 密封确认体）→ 收 pair.ready（激活屏障，
// 落盘 access/refresh）。**不做真 WebSocket**——收发帧经注入的 `PairingTransportPort`，连接本身
// 与 token.refresh 轮换归 T6c-refresh 单（见任务书 §2 首段）。
//
// Protocol references and invariants:
//   - remote-relay/test/s1ja-fake-mobile-e2e.test.js `pairDevice()` (lines 300-459):
//     this state machine follows its frame sequence and key-material flow step by step.
//   - app/src-tauri/src/remote_pairing.rs: `handle_hello`/`finalize_hello` define hello
//     validation and K_room wrapping; `seal_pair_done_confirm`/`verify_pair_done_confirm`
//     use raw device_id bytes as the done plaintext. `seal_pair_ready` also uses raw
//     device_id bytes, without tokens; tokens are delivered once in pair.accept tokens_ct.
//   - pair.done replay is idempotent; a plaintext device_revoked notification must
//     not clear stored credentials.

import { deriveSharedSecret, generateKeyPair } from "../crypto/x25519.ts";
import { deriveConnectTokenHex, deriveKPair } from "../crypto/kdf.ts";
import { bytesToBase64, utf8Bytes } from "../crypto/bytes.ts";
import { open, seal } from "../crypto/envelope.ts";
import { desktopPublicKeyBytes, type QrPayload } from "./qr-payload.ts";
import { unwrapKey } from "./key-wrap.ts";
import { helloEnvelopeMeta, pairAcceptTokensMeta, pairDoneConfirmMeta, pairReadyMeta } from "./meta.ts";
import {
  extractPairAcceptFields,
  extractPairReadyFields,
  parsePairAcceptTokens,
  type OutboundPairingFrame,
  type PairAcceptTokens,
} from "./frames.ts";
import { importNonExtractableAesGcmKey, type KeyStorePort } from "../store/key-store.ts";

export interface PairingTransportPort {
  send(frame: OutboundPairingFrame): void;
}

export type PairingPhase =
  | "idle"
  | "awaiting_accept"
  | "desktop_offline"
  | "awaiting_ready"
  | "activated"
  | "needs_repair";

export type PairingRejectReason =
  | "wrong_phase"
  | "malformed_frame"
  | "decrypt_failed"
  | "device_id_mismatch";

/**
 * `applied`  = 帧被当前态接受并处理（含状态迁移）。
 * `ignored`  = 帧结构合法但与本状态机无关（如 device_revoked 之外的 error 帧、未知 `t`）——按
 *              §9.5「乱序/异形帧…按语义拒绝或忽略」的忽略分支，不算协议违例。
 * `rejected` = 帧是本状态机认识的类型，但当前态不接受，或字段/解密未通过——按语义拒绝分支。
 */
export type PairingFrameOutcome =
  | { status: "applied" }
  | { status: "ignored"; reason: string }
  | { status: "rejected"; reason: PairingRejectReason };

export class PairingSession {
  private phase: PairingPhase = "idle";
  private kPair: Uint8Array | null = null;
  private kRoom: Uint8Array | null = null;
  private deviceId: string | null = null;
  private pendingTokens: PairAcceptTokens | null = null;
  private lastDoneConfirm: { ct: string; n: string } | null = null;
  private connectToken: string | null = null;
  private revokedReason: string | null = null;
  /**
   * Serialize activation writes by storing the first `handleReady` call's
   * `persistActivation()` promise here. Later concurrent calls await that same
   * promise instead of calling `keyStore.saveKeys` again. See `handleReady`/`persistActivation`.
   */
  private activationInFlight: Promise<void> | null = null;

  constructor(
    private readonly qr: QrPayload,
    private readonly transport: PairingTransportPort,
    private readonly keyStore: KeyStorePort,
  ) {}

  get state(): PairingPhase {
    return this.phase;
  }

  get pairedDeviceId(): string | null {
    return this.deviceId;
  }

  /** 手机独立推出的 connect_token（hex64）——T6c-refresh 用它建立 pairing scope 连接，本状态机不连接。 */
  get connectTokenHex(): string | null {
    return this.connectToken;
  }

  get revocationReason(): string | null {
    return this.revokedReason;
  }

  /** idle → awaiting_accept：派生连接令牌与 K_pair，构造并发送 pair.hello。 */
  async start(): Promise<void> {
    if (this.phase !== "idle") {
      throw new Error(`PairingSession.start() called from phase "${this.phase}", expected "idle"`);
    }
    const connectToken = await deriveConnectTokenHex(this.qr.pairing_token);

    const desktopPub = desktopPublicKeyBytes(this.qr);
    const mobileKeys = generateKeyPair();
    // 非贡献性 DH（全零共享秘密/低阶点）直接抛出、绝不发 pair.hello（M0 §5 硬约束）——不在这里
    // catch，让调用方把它当致命错误处理。
    const shared = await deriveSharedSecret(mobileKeys.secretKey, desktopPub);
    const kPair = await deriveKPair(shared, this.qr.pairing_token);

    const { ct, n } = await seal(kPair, helloEnvelopeMeta(this.qr.room), utf8Bytes(this.qr.pairing_token));

    this.connectToken = connectToken;
    this.kPair = kPair;
    this.transport.send({
      t: "pair.hello",
      room: this.qr.room,
      remote_pub: bytesToBase64(mobileKeys.publicKey),
      token_ct: ct,
      token_n: n,
    });
    this.phase = "awaiting_accept";
  }

  /** 收到一帧（pair.accept / pair.ready / error 等）时调用；结构不合法或态不匹配都不抛异常。 */
  async handleFrame(raw: unknown): Promise<PairingFrameOutcome> {
    const frame = asRecord(raw);
    if (!frame || typeof frame.t !== "string") {
      return { status: "rejected", reason: "malformed_frame" };
    }
    switch (frame.t) {
      case "pair.accept":
        return this.handleAccept(frame);
      case "pair.ready":
        return this.handleReady(frame);
      case "error":
        return this.handleError(frame);
      default:
        return { status: "ignored", reason: `unhandled frame type "${frame.t}"` };
    }
  }

  /**
   * done 幂等重放（M0 §9.5：「ready 丢失可重发 done 重得 ready·状态机允许 Done 态重入」）——
   * awaiting_ready 态下用同一份已经产出过的确认体重发 pair.done，不重新加密（复用即是幂等的一部
   * 分：两次密文字节相同，桌面侧 done 处理本身也是幂等的）。
   */
  resendDone(): PairingFrameOutcome {
    if (this.phase !== "awaiting_ready" || !this.lastDoneConfirm || !this.deviceId) {
      return { status: "rejected", reason: "wrong_phase" };
    }
    this.transport.send({
      t: "pair.done",
      room: this.qr.room,
      device_id: this.deviceId,
      confirm_ct: this.lastDoneConfirm.ct,
      confirm_n: this.lastDoneConfirm.n,
    });
    return { status: "applied" };
  }

  private async handleAccept(frame: Record<string, unknown>): Promise<PairingFrameOutcome> {
    // desktop_offline 是 transport 重建 WS、重发同一份 hello 期间的可见进度态；重试后的合法 accept
    // 必须能从该态继续。是否允许重发 hello 的硬闸由真正持有 socket/入站顺序的 transport 执行，
    // PairingSession 自己不发送第二份 hello。
    if (this.phase !== "awaiting_accept" && this.phase !== "desktop_offline") {
      return { status: "rejected", reason: "wrong_phase" };
    }
    if (!this.kPair) {
      return { status: "rejected", reason: "malformed_frame" };
    }
    const fields = extractPairAcceptFields(frame);
    if (!fields || fields.room !== this.qr.room) {
      return { status: "rejected", reason: "malformed_frame" };
    }

    let kRoom: Uint8Array;
    let tokens: PairAcceptTokens;
    try {
      kRoom = await unwrapKey(this.kPair, fields.kRoomCt, fields.kRoomN);
      const tokensPlaintext = await open(
        this.kPair,
        pairAcceptTokensMeta(this.qr.room, fields.deviceId),
        fields.tokensCt,
        fields.tokensN,
      );
      tokens = parsePairAcceptTokens(tokensPlaintext);
    } catch {
      return { status: "rejected", reason: "decrypt_failed" };
    }

    this.kRoom = kRoom;
    this.deviceId = fields.deviceId;
    this.pendingTokens = tokens;

    const { ct, n } = await seal(kRoom, pairDoneConfirmMeta(this.qr.room, fields.deviceId), utf8Bytes(fields.deviceId));
    this.lastDoneConfirm = { ct, n };
    this.transport.send({ t: "pair.done", room: this.qr.room, device_id: fields.deviceId, confirm_ct: ct, confirm_n: n });
    this.phase = "awaiting_ready";
    return { status: "applied" };
  }

  private async handleReady(frame: Record<string, unknown>): Promise<PairingFrameOutcome> {
    // 激活屏障：awaiting_ready 首次通过 → activated；activated 态再收到 ready（done 重放触发的
    // 重复投递）视为幂等重入，不重新落盘、原样返回 applied（M0 §9.5「状态机允许 Done 态重入」）。
    if (this.phase !== "awaiting_ready" && this.phase !== "activated") {
      return { status: "rejected", reason: "wrong_phase" };
    }
    if (!this.kPair || !this.kRoom || !this.deviceId || !this.pendingTokens) {
      return { status: "rejected", reason: "malformed_frame" };
    }
    // 局部捕获——避免 `this.*` 在下面的多个 await 之间被并发调用改写后重新读到不一致的值（同
    // start() 里的教训：TS 不会跨 await narrow `this.field`，这里额外用局部变量把"这次调用认定的
    // 材料"锁死，不管并发的另一次调用后续把 `this.*` 改成什么）。
    const kPair = this.kPair;
    const kRoom = this.kRoom;
    const deviceId = this.deviceId;
    const pendingTokens = this.pendingTokens;

    const fields = extractPairReadyFields(frame);
    if (!fields || fields.room !== this.qr.room || fields.deviceId !== deviceId) {
      return { status: "rejected", reason: "device_id_mismatch" };
    }

    let plaintext: Uint8Array;
    try {
      plaintext = await open(kPair, pairReadyMeta(this.qr.room, deviceId), fields.ct, fields.n);
    } catch {
      return { status: "rejected", reason: "decrypt_failed" };
    }
    // R2（remote_pairing.rs::seal_pair_ready 注释）：密文体明文就是 device_id 原始字节，不是 JSON、
    // 不装令牌——令牌已经在 pair.accept 阶段拿到手（pendingTokens）。
    if (new TextDecoder().decode(plaintext) !== deviceId) {
      return { status: "rejected", reason: "device_id_mismatch" };
    }

    // Serialize activation writes: two valid ready frames arriving concurrently may both
    // finish the read-only decryption and validation above, but must persist only once.
    // No await separates the check and assignment below, so JavaScript runs them atomically
    // relative to other continuations. The first continuation stores the `persistActivation()`
    // promise in `activationInFlight`; every other concurrent call awaits that shared promise
    // instead of independently calling `keyStore.saveKeys`, regardless of arrival order.
    // Previously, checking phase === "awaiting_ready", awaiting saveKeys, then setting phase
    // to "activated" let both calls observe "awaiting_ready" and each start a saveKeys write.
    if (!this.activationInFlight) {
      this.activationInFlight = this.persistActivation(kPair, kRoom, deviceId, pendingTokens);
    }
    await this.activationInFlight;
    return { status: "applied" };
  }

  private async persistActivation(
    kPair: Uint8Array,
    kRoom: Uint8Array,
    deviceId: string,
    tokens: PairAcceptTokens,
  ): Promise<void> {
    const kRoomKey = await importNonExtractableAesGcmKey(kRoom);
    await this.keyStore.saveKeys({
      deviceId,
      room: this.qr.room,
      relayUrl: this.qr.relay_url,
      access: tokens.capability_token,
      refresh: tokens.refresh_token,
      kRoomKey,
      kPair,
      // FIX2 P0-1：这次 pair.ready 激活拿到的 access 就是此刻刚签发的——落盘"此刻"作为本地过期钟
      // 的起点（`key-store.ts::StoredPairingCredentials.accessIssuedAtMs` 字段注释/`AppRuntime.tsx`
      // 消费侧同款语义）。落这个字段是三处协同修复的第一环：没有它，`AppRuntime.tsx` 冷启动读到
      // 的存量记录会缺这个字段，消费侧的保守兜底会把它当"已过期"（宁可多刷一次 refresh，也不能
      // 假装刚签发——见该文件 `credentials` 那段注释），不是本环节该吞的静默假设。
      accessIssuedAtMs: Date.now(),
    });
    // 落盘这段 await 期间，一次并发到达的 device_revoked（handleError 是同步的，可能正好插在这
    // 中间）必须赢——绝不能让"凭据合法拿到手、只是激活时序慢了半拍"的 save 完成后，把已经翻成
    // needs_repair 的 phase 覆盖回 activated。凭据本身仍然落盘（它是合法材料，落盘不是错误，
    // v0.3.1「不自动清凭据」原则同样适用于这条时序），只是这里不再把 phase 拉回 activated。
    if (this.phase !== "needs_repair") {
      this.phase = "activated";
    }
  }

  /**
   * 失效→重配对终态（M0 §9.6 撤销分流 + M2 C1 spec §3 终态闭环·v0.3.1 收紧）：relay 明文
   * `{t:"error", reason:"device_revoked"}`（room-do.js::closeSocketsForRevokedSubject 的真实产出
   * 形状）只供 UI 提示，不可信、**不自动清凭据**——转终态是唯一动作。`desktop_offline` 是配对期
   * 的另一条已知、可见错误：transport 会在尚未收到 accept 时有限重试，这里把 phase 翻成 UI 可
   * 观察的进度/终态；其余 error reason（stale_epoch / bad_json / role_forbidden 等）仍按 ignored。
   */
  private handleError(frame: Record<string, unknown>): PairingFrameOutcome {
    const reason = typeof frame.reason === "string" ? frame.reason : "unknown";
    if (reason === "desktop_offline") {
      if (this.phase !== "awaiting_accept" && this.phase !== "awaiting_ready") {
        return { status: "ignored", reason: `desktop_offline ignored in phase "${this.phase}"` };
      }
      this.phase = "desktop_offline";
      return { status: "applied" };
    }
    if (reason !== "device_revoked") {
      return { status: "ignored", reason: `error frame reason "${reason}" out of PairingSession scope` };
    }
    if (this.phase === "needs_repair") {
      return { status: "ignored", reason: "already in needs_repair" };
    }
    this.revokedReason = reason;
    this.phase = "needs_repair";
    return { status: "applied" };
  }
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  return value as Record<string, unknown>;
}
