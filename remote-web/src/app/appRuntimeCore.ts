// appRuntimeCore.ts — INT1b · 已配对运行时的纯状态层（不含 React/WebSocket/crypto I/O，方便独立
// 单测）。把"解密后的一帧要落到哪个会话的哪份状态里"这条路由规则从 React 组件里拆出来。
//
// **为什么是"每会话一个 MilestoneProjection"，不是全房间共用一份**（依据 `ui/stream/
// streamSource.ts` 头注原话："一个 MilestoneProjection 实例的生命周期就对应'当前正在看的这一个
// 会话'（同构 M0 §1 的 AAD session 字段——envelope 层面早就把帧路由到了正确的会话）"）：
// `MilestoneProjection.messages`/`decisionCards`/`toolCompletions` 不按 session 分——如果所有会话
// 共用同一份实例，不同会话的 `message_id`/`decision_id`/工具 `id` 可能撞号（这些 id 通常按会话各
// 自计数，不是全房间唯一），会把两个会话的内容混进同一张 Map。本文件维护
// `Map<sessionId, MilestoneProjection>`，逐会话独立——`session.index`（envelope.session 恒
// null·M0 §1）例外，单独落一份房间级 `indexProjection`，只消费它的 `.sessions` 字段。
//
// **live 帧的 run 归属**：live delta（text_delta/thinking_delta/tool_output_delta/usage_delta）
// 本身不带 run_id（M0 §2 wire 目录），`events/runWatermark.ts::applyLiveFrame` 要求调用方显式传
// runId——这里用该会话 `MilestoneProjection.runStatusBySession`（由 `run.status` 里程碑维护）当前
// 记录的 runId；M0 §2"drain 每轮先排里程碑后 live"保证同一批次里 run.status 先于该 run 产生的
// live 帧到达，稳态下这个查找可靠；查不到（真出现的竞态：连接刚建立、run.status 尚未到达）时
// 该条 live 帧被丢弃而不是瞎猜，不静默造出错误的会话归属。

import { applyLiveFrame, applySnapshot, idleRunTrackState, type RunTrackState } from "../events/runWatermark.ts";
import { MilestoneProjection } from "../events/milestoneProjection.ts";
import type { FrameRejectReason, ParsedFrame } from "../events/parseFrame.ts";

export interface FrameDiagnostics {
  framesSeen: number;
  plaintextControl: number;
  kindSkipped: number;
  decryptFailed: number;
  parseFailed: number;
  lastParseFailedReason: FrameRejectReason | "invalid_json" | null;
  missingIds: number;
  storeError: number;
  storeDuplicate: number;
  applied: number;
  milestoneSessionNull: number;
  snapshotRejected: number;
  defensiveDefault: number;
  liveSessionNull: number;
  liveUnknownDelta: number;
  liveDroppedNoRun: number;
  liveWatermarkRejected: number;
  historyApplied?: number;
  historyRejected?: number;
  lastDroppedFrameT: string | null;
  lastDroppedKind: string | null;
  lastDroppedErrorMessage: string | null;
}

export interface FrameDiagnosticsSnapshot extends FrameDiagnostics {
  /** 复用 `routingRejections` 分类 Map 的合计值，不维护第二份可漂移的计数。 */
  routingRejected: number;
}

export type ProcessFrameDiagnosticCounter =
  | "plaintextControl"
  | "kindSkipped"
  | "decryptFailed"
  | "parseFailed"
  | "missingIds"
  | "storeError"
  | "storeDuplicate";

function createFrameDiagnostics(): FrameDiagnostics {
  return {
    framesSeen: 0,
    plaintextControl: 0,
    kindSkipped: 0,
    decryptFailed: 0,
    parseFailed: 0,
    lastParseFailedReason: null,
    missingIds: 0,
    storeError: 0,
    storeDuplicate: 0,
    applied: 0,
    milestoneSessionNull: 0,
    snapshotRejected: 0,
    defensiveDefault: 0,
    liveSessionNull: 0,
    liveUnknownDelta: 0,
    liveDroppedNoRun: 0,
    liveWatermarkRejected: 0,
    historyApplied: 0,
    historyRejected: 0,
    lastDroppedFrameT: null,
    lastDroppedKind: null,
    lastDroppedErrorMessage: null,
  };
}

export interface AppRuntimeCore {
  /** 房间级会话索引——只消费 `.sessions`；其余字段（messages/decisionCards/...）不使用。 */
  indexProjection: MilestoneProjection;
  /** 逐会话独立的消息/卡片/工具/运行态时间线。 */
  sessionProjections: Map<string, MilestoneProjection>;
  /** 逐会话独立的 live 归约水位。 */
  runTracks: Map<string, RunTrackState>;
  /** `checkFrameRouting()` 拒绝的帧计数，按 `RouteRejectionReason` 分类——不是静默丢弃，供诊断
   *  可见（INT1c 审查返工·P0）。 */
  routingRejections: Map<RouteRejectionReason, number>;
  /** 手机端入站帧逐闸诊断，只驻留于当前 AppRuntime 生命周期的内存。 */
  frameDiagnostics: FrameDiagnostics;
}

export function createAppRuntimeCore(): AppRuntimeCore {
  return {
    indexProjection: new MilestoneProjection(),
    sessionProjections: new Map(),
    runTracks: new Map(),
    routingRejections: new Map(),
    frameDiagnostics: createFrameDiagnostics(),
  };
}

export function recordFrameSeen(core: AppRuntimeCore): void {
  core.frameDiagnostics.framesSeen += 1;
}

export function recordFrameApplied(core: AppRuntimeCore): void {
  core.frameDiagnostics.applied += 1;
}

export function recordProcessFrameDrop(
  core: AppRuntimeCore,
  counter: ProcessFrameDiagnosticCounter,
  context: {
    kind: string | null;
    t: string | null;
    parseReason?: FrameRejectReason | "invalid_json";
    errorMessage?: string;
  },
): void {
  core.frameDiagnostics[counter] += 1;
  // 明文指令面是另一条受支持的处理路径，不等同于丢弃；只计数，不污染“最后一次丢弃”。
  if (counter !== "plaintextControl") {
    core.frameDiagnostics.lastDroppedKind = context.kind;
    core.frameDiagnostics.lastDroppedFrameT = context.t;
    core.frameDiagnostics.lastDroppedErrorMessage = context.errorMessage ?? null;
  }
  if (counter === "parseFailed") {
    core.frameDiagnostics.lastParseFailedReason = context.parseReason ?? null;
  }
}

export function getFrameDiagnostics(core: AppRuntimeCore): FrameDiagnosticsSnapshot {
  let routingRejected = 0;
  for (const count of core.routingRejections.values()) routingRejected += count;
  return { ...core.frameDiagnostics, routingRejected };
}

// ============================================================================
// 入库/归约前的强制路由校验（INT1c 审查返工·P0）
// ============================================================================

export type RouteRejectionReason =
  /** 信封 `room` 与当前配对房间不符——正常协议下 AEAD 认证会先一步拒绝这类篡改帧（AAD 含
   *  `room`，解密对不上会先炸），这里是纵深防御第二层：防御的是"本进程自身接错房间/凭据错配"
   *  这类实现层错误，不是只防恶意 relay。 */
  | "room_mismatch"
  /** `session.index` 帧的外层信封 `session` 非 null（M0 §1："会话索引流固定 session=null"）。 */
  | "session_index_outer_session_not_null"
  /** `snapshot`/`run.status` 帧自带的内层 session 字段（`SnapshotResponseFrame.session`/
   *  `RunStatusFrame.session_id`）与外层信封 `session` 字段不一致——两者本该恒等，不等意味着
   *  内容与路由信封被错误拼接（无论因为谁的 bug），不能假装能安全按任一个去归约。 */
  | "inner_outer_session_mismatch";

export type RouteCheckResult = { accepted: true } | { accepted: false; reason: RouteRejectionReason };

/**
 * 入库/归约前的强制校验闸门——`useAppRuntimeFrameIngestion.ts::processIncomingFrame` 在 `parseFrame()` 成功后、调用
 * `applyDecryptedMilestoneFrame`/`applyDecryptedLiveFrame` 之前必须先过这一关；不满足任何一条
 * 直接丢弃（`recordRoutingRejection` 计数），绝不放行到归约层。**epoch 不是这里的校验维度**——
 * 同一房间内 epoch 随连接生命周期正常变化，本函数只认 room/session，不比对 epoch（回归测试见
 * `appRuntimeCore.test.ts` 的"合法新 epoch 正例"）。
 */
export function checkFrameRouting(
  expectedRoom: string,
  envelope: { room: string; session: string | null },
  frame: ParsedFrame,
): RouteCheckResult {
  if (envelope.room !== expectedRoom) {
    return { accepted: false, reason: "room_mismatch" };
  }
  if (frame.t === "session.index" && envelope.session !== null) {
    return { accepted: false, reason: "session_index_outer_session_not_null" };
  }
  if (frame.t === "snapshot" && frame.session !== envelope.session) {
    return { accepted: false, reason: "inner_outer_session_mismatch" };
  }
  if (frame.t === "history" && frame.session !== envelope.session) {
    return { accepted: false, reason: "inner_outer_session_mismatch" };
  }
  if (frame.t === "run.status" && frame.session_id !== envelope.session) {
    return { accepted: false, reason: "inner_outer_session_mismatch" };
  }
  return { accepted: true };
}

export function recordRoutingRejection(
  core: AppRuntimeCore,
  reason: RouteRejectionReason,
  context?: { kind: "event" | "live"; t: string },
): void {
  core.routingRejections.set(reason, (core.routingRejections.get(reason) ?? 0) + 1);
  if (context) {
    core.frameDiagnostics.lastDroppedKind = context.kind;
    core.frameDiagnostics.lastDroppedFrameT = context.t;
    core.frameDiagnostics.lastDroppedErrorMessage = null;
  }
}

type CoreDropCounter =
  | "milestoneSessionNull"
  | "snapshotRejected"
  | "defensiveDefault"
  | "liveSessionNull"
  | "liveUnknownDelta"
  | "liveDroppedNoRun"
  | "liveWatermarkRejected";

function recordCoreDrop(core: AppRuntimeCore, counter: CoreDropCounter, kind: "event" | "live", t: string): void {
  core.frameDiagnostics[counter] += 1;
  core.frameDiagnostics.lastDroppedKind = kind;
  core.frameDiagnostics.lastDroppedFrameT = t;
  core.frameDiagnostics.lastDroppedErrorMessage = null;
}

export function getOrCreateSessionProjection(core: AppRuntimeCore, sessionId: string): MilestoneProjection {
  let projection = core.sessionProjections.get(sessionId);
  if (!projection) {
    projection = new MilestoneProjection();
    core.sessionProjections.set(sessionId, projection);
  }
  return projection;
}

/** history 虽走 live 信封，但不依赖 runId/run watermark，必须在 live delta 闸之前调用。 */
export function applyDecryptedHistoryFrame(
  core: AppRuntimeCore,
  envelopeSession: string | null,
  frame: ParsedFrame,
  options: { advanceCursor?: boolean } = {},
): boolean {
  if (envelopeSession === null || frame.t !== "history" || frame.session !== envelopeSession) {
    core.frameDiagnostics.historyRejected = (core.frameDiagnostics.historyRejected ?? 0) + 1;
    core.frameDiagnostics.lastDroppedKind = "live";
    core.frameDiagnostics.lastDroppedFrameT = frame.t;
    core.frameDiagnostics.lastDroppedErrorMessage = null;
    return false;
  }
  getOrCreateSessionProjection(core, envelopeSession).applyHistory(frame, options.advanceCursor ?? true);
  core.frameDiagnostics.historyApplied = (core.frameDiagnostics.historyApplied ?? 0) + 1;
  return true;
}

function getOrCreateRunTrack(core: AppRuntimeCore, sessionId: string): RunTrackState {
  let track = core.runTracks.get(sessionId);
  if (!track) {
    track = idleRunTrackState();
    core.runTracks.set(sessionId, track);
  }
  return track;
}

/**
 * U2 根修（症状①手机端 typing 恒显 · 成因 2）：run 终态清残留——`msg.completed`（这条 partial
 * 已经落成一条真正的消息）与 `run.status` 转非 running（`status!=="running"`，这个 run 彻底结束或
 * 尚未开始）都是"这条 runTrack 携带的 partial 已经过期"的信号，必须把它重置回
 * `idleRunTrackState()`，否则 `AppRuntime.tsx` 选 `liveReducer` 时看到的 `runTrack.runId` 会一直停
 * 在最后一个已结束 run 的 id 上，typing 气泡永久挂着。
 *
 * **只重置已存在的条目，不凭空创建**：一个从未收到过 snapshot/live 帧的会话不该被这两类里程碑帧
 * 造出一条 idle 记录——`core.runTracks` 的既有不变量是"只有 snapshot/live 路径会创建条目"（见
 * `appRuntimeCore.test.ts` "无 runId 也能应用……"一例：`msg.completed` 帧不该让 `runTracks.has()`
 * 变 true）。
 *
 * **已知残余（U2 修复轮独立审查第 4 条·接受）**：重置后 `throughRunSeq` 归 `null`
 * （`idleRunTrackState()`），下一条到达的 live 帧只要 runId 匹配就会被当作"该 run 的第一条"接收
 * （watermark 不再比较 seq）。如果同一个 run 的旧 live 帧因为乱序在这之后才姗姗来迟，会被短暂重新
 * 接纳、typing 气泡短暂重新点亮，直到更新的帧覆盖它。这是一个短暂的错显窗口，不是数据损坏（不会
 * 落进 messages，只影响瞬时 live 显示），本轮不处理。
 */
function resetRunTrackToIdleIfPresent(core: AppRuntimeCore, sessionId: string): void {
  if (core.runTracks.has(sessionId)) {
    core.runTracks.set(sessionId, idleRunTrackState());
  }
}

/**
 * `kind=event` 里程碑帧的路由（M0 §2 六种 + v1.8.11 起 snapshot 应答同样走 event 通道）。
 * `envelopeSession` = 外层信封的 `session` 字段（AAD 认证过，真相源）；`session.index` 该字段恒
 * null（会话索引流），其余五种里程碑 + snapshot 应答该字段必须非 null 才能路由（缺失时丢弃，不
 * 崩溃、不误归属到"当前选中会话"这类猜测）。
 */
export function applyDecryptedMilestoneFrame(core: AppRuntimeCore, envelopeSession: string | null, frame: ParsedFrame): boolean {
  switch (frame.t) {
    case "session.index":
      core.indexProjection.applySessionIndex(frame);
      return true;
    case "msg.completed":
      if (envelopeSession === null) {
        recordCoreDrop(core, "milestoneSessionNull", "event", frame.t);
        return false;
      }
      getOrCreateSessionProjection(core, envelopeSession).applyMsgCompleted(frame);
      resetRunTrackToIdleIfPresent(core, envelopeSession);
      return true;
    case "card.created":
      if (envelopeSession === null) {
        recordCoreDrop(core, "milestoneSessionNull", "event", frame.t);
        return false;
      }
      getOrCreateSessionProjection(core, envelopeSession).applyCardCreated(frame);
      return true;
    case "card.resolved":
      if (envelopeSession === null) {
        recordCoreDrop(core, "milestoneSessionNull", "event", frame.t);
        return false;
      }
      getOrCreateSessionProjection(core, envelopeSession).applyCardResolved(frame);
      return true;
    case "run.status":
      if (envelopeSession === null) {
        recordCoreDrop(core, "milestoneSessionNull", "event", frame.t);
        return false;
      }
      getOrCreateSessionProjection(core, envelopeSession).applyRunStatus(frame);
      // U2 修复轮（独立审查定罪·判据钉反）：判据必须是 `status`，不是 `run_id`——生产所有
      // "run 结束/摘槽"路径（桌面 Rust 侧 refresh_session_runtime → upsert_session_runtime_status）
      // 刻意保留旧 run_id，发布形如 `{status:"idle", run_id:"run-1"}`（桌面侧契约见
      // `db.rs:13168`："refresh 发布必须沿用写前读到的 run_id"）；唯一发 `run_id:null` 的生产帧是
      // solo 开跑帧 `{status:"running", run_id:null}`。旧判据 `run_id===null` 在这两条路径上全反：
      // 该重置的终态帧（run_id 非 null）不重置、不该重置的开跑帧（run_id 为 null）反而误重置。改用
      // `status !== "running"`，与 `ui/stream/streamSource.ts` 的 `running` 派生口径
      // （`runStatus?.status === "running"`）对齐——run.status 帧只要不是 running 就代表这条
      // runTrack 携带的 partial 已经过期，必须重置。
      if (frame.status !== "running") {
        resetRunTrackToIdleIfPresent(core, envelopeSession);
      }
      return true;
    case "tool.completed":
      if (envelopeSession === null) {
        recordCoreDrop(core, "milestoneSessionNull", "event", frame.t);
        return false;
      }
      getOrCreateSessionProjection(core, envelopeSession).applyToolCompleted(frame);
      return true;
    case "snapshot": {
      const track = getOrCreateRunTrack(core, frame.session);
      const outcome = applySnapshot(track, frame);
      if (outcome.accepted) {
        core.runTracks.set(frame.session, outcome.next);
        return true;
      }
      recordCoreDrop(core, "snapshotRejected", "event", frame.t);
      return false;
    }
    default:
      // control.snapshot 请求回显 / presence / input.ack / input.expired / replay.head——这些不
      // 会（或不该）以 kind=event 到达；防御性丢弃，不抛异常（同 parseFrame.ts 的一贯边界）。
      recordCoreDrop(core, "defensiveDefault", "event", frame.t);
      return false;
  }
}

/** `kind=live` 帧的路由——`envelopeSession` 缺失，或帧本身不是四种 live delta 之一（防御性）时丢弃。 */
export function applyDecryptedLiveFrame(core: AppRuntimeCore, envelopeSession: string | null, frame: ParsedFrame): boolean {
  if (envelopeSession === null) {
    recordCoreDrop(core, "liveSessionNull", "live", frame.t);
    return false;
  }
  if (frame.t !== "text_delta" && frame.t !== "thinking_delta" && frame.t !== "tool_output_delta" && frame.t !== "usage_delta") {
    recordCoreDrop(core, "liveUnknownDelta", "live", frame.t);
    return false;
  }
  const sessionProjection = getOrCreateSessionProjection(core, envelopeSession);
  const runId = sessionProjection.runStatusBySession.get(envelopeSession)?.runId ?? null;
  if (runId === null) {
    recordCoreDrop(core, "liveDroppedNoRun", "live", frame.t);
    return false; // 尚不知道这个会话当前在跑哪个 run——见文件头注"live 帧的 run 归属"。
  }
  const track = getOrCreateRunTrack(core, envelopeSession);
  const outcome = applyLiveFrame(track, runId, frame);
  if (outcome.accepted) {
    core.runTracks.set(envelopeSession, outcome.next);
    return true;
  }
  recordCoreDrop(core, "liveWatermarkRejected", "live", frame.t);
  return false;
}
