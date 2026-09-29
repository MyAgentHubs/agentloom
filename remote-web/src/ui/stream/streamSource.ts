// streamSource.ts — T6f2 · 把 T6d1 事件内核（`MilestoneProjection` + `LiveBlockReducer`）折成
// `SessionStreamScreen` 消费的纯 props 形状。
//
// **边界（任务书 §3「数据入口 = 注入的 stream source……真 WS 接线不在本单」）**：这里只做「内核
// 状态 → 屏幕 props」这一步纯函数折算，不连 relay、不管 wss/K_room——调用方（真 WS 接线单 /
// 本单测试）负责把解密后的帧喂进 `MilestoneProjection`/`LiveBlockReducer`，本文件只读它们已经
// 归约好的状态。
//
// `MilestoneProjection.messages` 本身不按 session 分——这是刻意的：一个 `MilestoneProjection`
// 实例的生命周期就对应"当前正在看的这一个会话"（同构 M0 §1 的 AAD `session` 字段——envelope 层面
// 早就把帧路由到了正确的会话，见 `src/crypto/envelope.ts` 头注"`session`/`command_id` 的 wire
// grammar 校验……不在这里重复实现"）。`sessionId` 参数只用来查 `runStatusBySession`（`run.status`
// 帧的 payload 里自带 `session_id` 字段，这是三张 Map 里唯一按 session 存的）。

import type { MilestoneProjection } from "../../events/milestoneProjection.ts";
import type { LiveBlockReducer } from "../../events/liveReducer.ts";
import type { ReducedBlock } from "../../events/blocks.ts";
import type { ContentRef } from "../../events/parseFrame.ts";

export interface SessionStreamMessage {
  messageId: number;
  role: string;
  /** db::Block JSON 数组——透传，深层 schema 校验不在本层（同 parseFrame.ts 的范围边界）。
   *  Once full text has been fetched, `blocks` holds the full-text blocks via `ProjectedMessage.fullBlocks`; otherwise it holds the arriving preview/normal blocks.
   *  到达的 preview/正常 blocks——调用方不需要自己判断该展示哪个,这里已经折算好。 */
  blocks: unknown[];
  /** optional——见 `ProjectedMessage.agent` 头注；缺失时消费方（`SessionStreamScreen`）回退现有
   *  assistant/user 占位。 */
  agent?: string;
  /** A non-undefined contentRef identifies a message downgraded to a preview because it exceeded the budget.
   *  preview（还没拉过全文，或拉到的全文已经因为新 revision 到达而失效）还是全文（已成功拉取且
   *  revision 未变）由调用方结合 `ProjectedMessage.fullBlocks` 是否存在自行判定——这里只透传
   *  ref 本身，供渲染层决定要不要显示"加载全文"入口。 */
  contentRef?: ContentRef;
  /** hasFullText tells the renderer whether blocks currently contains fetched full text or a preview so it can control full-text loading.
   *  再显示"加载全文"按钮。`contentRef` 为 undefined 时本字段恒 false（普通消息没有 preview/全文
   *  的区分）。 */
  hasFullText: boolean;
}

/**
 * `card.created`（`block`，整块透传）与 `card.resolved`（`status`/`chosen_option`）按 `decisionId`
 * 合并后的结果——`block` 字段本身携带的 `status`/`chosen_option` 可能是创建时的旧值（`card.created`
 * 帧里默认是 `pending`/`null`），真正的最新值以 `MilestoneProjection.decisionCards` 顶层的
 * `status`/`chosenOption` 为准（`applyCardResolved` 只更新这两个顶层字段，不改 `block` 本身，见
 * `milestoneProjection.ts` 头注）。消费方（`decisionCardView.ts::coerceDecisionCardBlock`）用
 * `block` 里的 `status`/`chosen_option` 覆盖值渲染，就是靠这里已经合并好的结果——本类型的字段命名
 * 特意保持 snake_case 贴合 wire 形状，跟 `block` 剩余字段一致。
 */
export interface SessionStreamDecisionCard {
  decisionId: string;
  /** 已把最新 `status`/`chosen_option` 合并回 `block` 顶层；只见过 `card.resolved`、没见过
   *  `card.created` 时（`MilestoneProjection` 记录了状态变化但没有卡片内容）此字段为 `undefined`
   *  ——调用方跳过渲染（没有 question/options 无法渲染，见已知缺口记档）。 */
  block: Record<string, unknown> | undefined;
}

export interface SessionStreamProps {
  sessionId: string | null;
  /** 按 messageId 升序排列——聊天流「旧的在上、新的在下」的既有心智模型。 */
  messages: SessionStreamMessage[];
  running: boolean;
  /** `null` = 当前没有进行中的 live 消息；非 null（含空数组）= 有一条正在打字的尾部消息。 */
  liveBlocks: ReducedBlock[] | null;
  /** 按到达顺序（`MilestoneProjection.decisionCards` 的 Map 插入序）——DecisionCard/交付确认
   *  （M0 协议里两者共用同一对 card.created/card.resolved 帧，kind 字段区分）合并后的卡片。 */
  decisionCards: SessionStreamDecisionCard[];
  historyCursor?: number | null;
  historyExhausted?: boolean;
}

/**
 * 折算 `MilestoneProjection` + 可选的 `LiveBlockReducer` → `SessionStreamProps`。
 *
 * `liveReducer` 传 `null` 或省略 = 当前没有活跃的 live 归约器（例如会话本就 idle，或调用方还没
 * 收到过 `control.snapshot` 应答）——`liveBlocks` 相应地是 `null`，不是空数组（空数组语义上是
 * "有一条进行中消息、但它还没有任何可见内容"，两者不同，调用方不该混淆）。
 *
 * `sessionIndexStatus` provides a fallback signal from the `session.index` row status that callers must pass explicitly.
 * 调用方显式传入**，不能靠这里读 `projection.sessions`。缘由（独立审核出的接线坑）：生产调用链
 * 里真正喂进来的 `projection` 是 `AppRuntime.tsx` 的 `core.sessionProjections.get(sessionId)`
 * ——一个"每会话一份"的 `MilestoneProjection` 实例，只会应用 `msg.completed`/`card.*`/
 * `run.status`（见 `appRuntimeCore.ts::applyDecryptedMilestoneFrame`），`session.index` 帧
 * 只应用到另一个完全独立的房间级实例 `core.indexProjection` 上——两者的 `.sessions` Map 从不
 * 同步。旧版在这里读 `projection.sessions.get(sessionId)`，在生产路径上永远查到空 Map，等效于
 * 没修（单测能过是因为测试手搓的 `projection` 把两类帧都灌进了同一个实例）。改成显式参数后，
 * 调用方必须自己从 `core.indexProjection.sessions` 取值再传进来——`AppRuntime.tsx` 的接线级
 * 测试断言的正是这条真实调用链。
 */
export function deriveSessionStreamProps(
  projection: MilestoneProjection,
  sessionId: string | null,
  liveReducer?: LiveBlockReducer | null,
  sessionIndexStatus?: string | null,
): SessionStreamProps {
  const messages = Array.from(projection.messages.values())
    .slice()
    .sort((a, b) => a.messageId - b.messageId)
    .map((m) => ({
      messageId: m.messageId,
      role: m.role,
      blocks: m.fullBlocks ?? m.blocks,
      agent: m.agent,
      contentRef: m.contentRef,
      hasFullText: m.fullBlocks !== undefined,
    }));

  const runStatus = sessionId ? projection.runStatusBySession.get(sessionId) : undefined;
  // Once any `run.status` entry exists for the session, it takes precedence over the fallback instead of combining signals with a bare OR.
  // （哪怕值是 idle），就是唯一真相——`run.status` 是里程碑事件，天然比"连接快照里的旧状态"
  // 新鲜；只有这个会话从没收到过 run.status（`runStatus === undefined`，包括压根没选过、或
  // 中途接入还没轮到它的 replay 帧）时，才退回 `sessionIndexStatus` 兜底。裸 OR（旧写法：
  // `runStatus?.status==="running" || sessionIndexRunning`）的问题是——一旦 sessionIndexStatus
  // 是 running（例如连接快照里的旧值），即便随后收到 run.status=idle，OR 仍然为 true，顶栏
  // 会黏在 running 上不放；改成"存在即唯一真相"后，run.status=idle 到达的那一刻就必须让顶栏
  // 转 idle，不再被 session.index 的旧值拖住。
  const running = runStatus !== undefined ? runStatus.status === "running" : sessionIndexStatus === "running";
  const liveBlocks = liveReducer ? liveReducer.snapshotBlocks() : null;

  const decisionCards: SessionStreamDecisionCard[] = Array.from(projection.decisionCards.values()).map((card) => ({
    decisionId: card.decisionId,
    block: card.block ? { ...card.block, status: card.status, chosen_option: card.chosenOption } : undefined,
  }));

  return {
    sessionId,
    messages,
    running,
    liveBlocks,
    decisionCards,
    historyCursor: projection.historyCursor,
    historyExhausted: projection.historyExhausted,
  };
}
