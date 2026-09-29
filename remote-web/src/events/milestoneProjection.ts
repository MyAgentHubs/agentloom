// milestoneProjection.ts — 里程碑帧折进本地可视状态时的"业务层二次去重"
// （brief §2 项 2 后半：message_id/decision_id·密文内业务 id 是端到端真兜底）。
//
// `store/indexeddbEventStore.ts` 的 `client_msg_id` 去重是第一层（防 relay at-least-once 重投
// 造成同一条帧被处理两次）；这里是第二层纵深防御，不是信任"第一层一定拦住了一切"：即便某条
// 边缘路径下同一条业务记录（同一个 message_id / decision_id）真的带着两个不同的 client_msg_id
// 到达（理论上不该发生——relay 幂等已在 M0 §1 处理——但纵深防御不能假设协议其它层永远无懈可
// 击），折算进本地状态时依然按 message_id/decision_id 做 upsert 而不是"追加成两条"。
//
// **范围边界**：这里只做"折算成 Map 条目"这一层最小状态——UI 渲染/排序/分页/持久化是另一层
// 的事，不在本单（C1 事件内核）范围内。`session.index` 的五种变体（full/created/renamed/
// archived|unarchived/deleted）在这里折成一份 `sessions: Map<id, row>`，语义与桌面
// `session_runtime` 表的"当前态"同构，不做历史留痕。

import type {
  CardCreatedFrame,
  CardResolvedFrame,
  ContentRef,
  MsgCompletedFrame,
  RunStatusFrame,
  SessionIndexActiveRepo,
  SessionIndexArchivedFrame,
  SessionIndexCreatedFrame,
  SessionIndexDeletedFrame,
  SessionIndexFrame,
  SessionIndexFullFrame,
  SessionIndexRenamedFrame,
  SessionIndexRow,
  ToolCompletedFrame,
  HistoryResponseFrame,
} from "./parseFrame.ts";

export interface ProjectedMessage {
  messageId: number;
  role: string;
  blocks: unknown[];
  /** optional——见 `MsgCompletedFrame.agent` 头注；history 投影暂不带（MA2 范围边界），消费方缺失时
   *  回退现有 assistant/user 占位。 */
  agent?: string;
  /** The single source of truth for this message's content version — always defined, never optional:
   *  没见过 content_ref 的普通消息按"无 revision 字段视为 1"钉死为 1，不是 undefined——`revision`
   *  高者胜的比较逻辑需要一个总能比的数，不能先判断"有没有"再决定要不要比。 */
  revision: number;
  /** 见 `MsgCompletedFrame.content_ref` 头注；只有超预算降级为 preview 的消息才会有——普通消息
   *  该键不出现（不是 undefined 值语义上的区别，这里用 optional 属性表达"没有 ref"）。 */
  contentRef?: ContentRef;
  /** Full blocks fetched via `msg.fetch`, kept in-memory only and never persisted to the EventStore —
   *  `undefined` = 还没拉过 / 拉到的是比当前 revision 更旧的内容已被丢弃。渲染层用
   *  `fullBlocks ?? blocks` 决定显示全文还是 preview（`streamSource.ts`）。 */
  fullBlocks?: unknown[];
}

/** When `content_ref` is absent, this message's "version" is pinned to 1 for old-desktop compatibility — it never
 *  发过 content_ref 的桌面，等价于永远只有 revision 1 这一个版本）。
 *  Prefer the **top-level** `revision` (`MsgCompletedFrame.revision`/
 *  `HistoryResponseMessage.revision`）——普通尺寸消息也可能带它，不必等降级为 preview、
 *  `content_ref.revision` 才出现才有版本号可比，"高者胜"投影才能覆盖到未降级消息的窄窗口
 *  when `topLevelRevision` is missing (older desktops, or a variant not yet shipped here) fall back in the existing order to
 *  `content_ref.revision`，再无则钉死为 1。 */
function effectiveRevision(topLevelRevision: number | undefined, contentRef: ContentRef | undefined): number {
  return topLevelRevision ?? contentRef?.revision ?? 1;
}

export interface ProjectedDecisionCard {
  decisionId: string;
  /** 只见过 `card.resolved`、没见过 `card.created` 时留空——仍可记录状态变化。 */
  block: Record<string, unknown> | undefined;
  status: string;
  chosenOption: string | null;
}

export interface ProjectedRunStatus {
  status: string;
  runId: string | null;
}

export interface ProjectedToolCompletion {
  id: string;
  tool: string;
  status: string;
  exitCode: number | null;
  output: string | null;
}

export class MilestoneProjection {
  readonly messages = new Map<number, ProjectedMessage>();
  readonly decisionCards = new Map<string, ProjectedDecisionCard>();
  readonly runStatusBySession = new Map<string, ProjectedRunStatus>();
  readonly toolCompletions = new Map<string, ProjectedToolCompletion>();
  readonly sessions = new Map<string, SessionIndexRow>();
  /** M2-4x：全量快照顶层"当前被远程的项目"摘要——每次全量快照原样替换（`?? null` 归一旧
   *  桌面不带这个键的情况，同 `sessions` 的"全量替换"语义，不是增量 op 能改的字段）。 */
  activeRepo: SessionIndexActiveRepo | null = null;
  historyCursor: number | null = null;
  historyExhausted = false;
  historyRequested = false;

  /**
   * 幂等：同一 `message_id` 应用两次（无论是否同一个 client_msg_id 带来的）只留一条。
   *
   * Revision-wins semantics: when the recorded revision is already higher than the incoming one, **ignore** this
   * 应用（拒绝被一条乱序/重放的旧内容覆盖新内容——纵深防御，relay/事件层理论上已经按到达序投递，
   * 但这里不假设上游永远无懈可击，同文件头注"业务层二次去重"的既有取向）；否则照常覆盖，与旧版
   * "无条件覆盖"行为完全一致（`revision` 缺失时 `effectiveRevision` 恒为 1，新旧两次都按 1 比，
   * `1 > 1` 恒假，覆盖照常发生——这正是"旧行为不更坏"：从未见过 content_ref 的流量不受本次改动
   * 影响任何可观察行为）。revision 真的推进（不是"同一版本重复到达"）时清空 `fullBlocks`——旧版本
   * 拉到的全文对新内容已经过期，留着只会展示错误内容,必须让 UI 退回这次到达的 preview（或全文,
   * 视 wire 内容而定）。
   */
  applyMsgCompleted(frame: MsgCompletedFrame): void {
    const revision = effectiveRevision(frame.revision, frame.content_ref);
    const existing = this.messages.get(frame.message_id);
    if (existing !== undefined && existing.revision > revision) return;
    const isNewRevision = existing === undefined || existing.revision !== revision;
    this.messages.set(frame.message_id, {
      messageId: frame.message_id,
      role: frame.role,
      blocks: frame.blocks,
      agent: frame.agent,
      revision,
      contentRef: frame.content_ref,
      fullBlocks: isNewRevision ? undefined : existing?.fullBlocks,
    });
  }

  /** 历史只补缺口；实时里程碑已经写入的同 message_id 永远优先。 */
  applyHistory(frame: HistoryResponseFrame, advanceCursor = true): void {
    for (const message of frame.messages) {
      if (this.messages.has(message.message_id)) continue;
      this.messages.set(message.message_id, {
        messageId: message.message_id,
        role: message.role,
        blocks: message.blocks,
        revision: effectiveRevision(message.revision, message.content_ref),
        contentRef: message.content_ref,
      });
    }
    if (advanceCursor) {
      this.historyRequested = true;
      this.historyCursor = frame.next_before;
      this.historyExhausted = frame.next_before === null;
    }
  }

  /**
   * After `msg.fetch` reassembly completes, attach the full blocks to the matching message — kept in-memory only ("never
   * EventStore"）。`revision` 由调用方传入本次 fetch 针对的版本；只有该消息**眼下仍然**是这个
   * revision 时才应用（`existing === undefined` = 消息已经不在——不太可能但防御；
   * `existing.revision !== revision` = fetch 在飞期间又来了一条更新的 msg.completed，这份全文已经
   * 过期，绝不能把旧版本内容当"全文"展示——同 `applyMsgCompleted` 的"revision 高者胜"是同一条
   * 不变量的两个方向）。返回是否真的应用了，供调用方决定要不要提示用户"内容已更新，全文需重新
   * 加载"。
   */
  applyFullText(messageId: number, revision: number, blocks: unknown[]): boolean {
    const existing = this.messages.get(messageId);
    if (existing === undefined || existing.revision !== revision) return false;
    this.messages.set(messageId, { ...existing, fullBlocks: blocks });
    return true;
  }

  /** 幂等：同一 `decision_id` 的 `card.created` 重复到达只覆盖，不追加。 */
  applyCardCreated(frame: CardCreatedFrame): void {
    const decisionId = typeof frame.block.decision_id === "string" ? frame.block.decision_id : undefined;
    if (decisionId === undefined) return; // 异形块（decision_id 缺失/非字符串）——不崩溃，静默丢弃。
    const status = typeof frame.block.status === "string" ? frame.block.status : "pending";
    const chosenOption = typeof frame.block.chosen_option === "string" ? frame.block.chosen_option : null;
    this.decisionCards.set(decisionId, { decisionId, block: frame.block, status, chosenOption });
  }

  /** 幂等：保留已知的 `block`（若有），只更新 status/chosen_option——晚到的 resolved 不擦除卡片内容。 */
  applyCardResolved(frame: CardResolvedFrame): void {
    const existing = this.decisionCards.get(frame.decision_id);
    this.decisionCards.set(frame.decision_id, {
      decisionId: frame.decision_id,
      block: existing?.block,
      status: frame.status,
      chosenOption: frame.chosen_option,
    });
  }

  /** 幂等：同一 session_id 的 run.status 只留最新一条。 */
  applyRunStatus(frame: RunStatusFrame): void {
    this.runStatusBySession.set(frame.session_id, { status: frame.status, runId: frame.run_id });
  }

  /** 幂等：同一 tool id 只留最新一条（tool.completed 本就是终态，不存在"追加"语义）。 */
  applyToolCompleted(frame: ToolCompletedFrame): void {
    this.toolCompletions.set(frame.id, {
      id: frame.id,
      tool: frame.tool,
      status: frame.status,
      exitCode: frame.exit_code,
      output: frame.output,
    });
  }

  applySessionIndex(frame: SessionIndexFrame): void {
    if (frame.full) {
      this.applySessionIndexFull(frame);
      return;
    }
    switch (frame.op) {
      case "created":
        this.applySessionIndexCreated(frame);
        return;
      case "renamed":
        this.applySessionIndexRenamed(frame);
        return;
      case "archived":
      case "unarchived":
        this.applySessionIndexArchived(frame);
        return;
      case "deleted":
        this.applySessionIndexDeleted(frame);
        return;
    }
  }

  private applySessionIndexFull(frame: SessionIndexFullFrame): void {
    this.sessions.clear();
    for (const row of frame.sessions) {
      this.sessions.set(row.id, row);
    }
    this.activeRepo = frame.repo ?? null;
  }

  private applySessionIndexCreated(frame: SessionIndexCreatedFrame): void {
    this.sessions.set(frame.session.id, {
      id: frame.session.id,
      title: frame.session.title,
      repo_id: frame.session.repo_id,
      archived: frame.session.archived,
      status: null,
      run_id: null,
      updated_at: this.sessions.get(frame.session.id)?.updated_at ?? 0,
      last_msg_preview: null,
      last_activity_at: null,
      repo_name: frame.session.repo_name ?? null,
    });
  }

  private applySessionIndexRenamed(frame: SessionIndexRenamedFrame): void {
    const existing = this.sessions.get(frame.id);
    if (!existing) return; // 增量帧先于全量快照到达——无条目可改时静默丢弃，等 full 快照兜底。
    this.sessions.set(frame.id, { ...existing, title: frame.title });
  }

  private applySessionIndexArchived(frame: SessionIndexArchivedFrame): void {
    const archived = frame.op === "archived";
    for (const id of frame.ids) {
      const existing = this.sessions.get(id);
      if (!existing) continue;
      this.sessions.set(id, { ...existing, archived });
    }
  }

  private applySessionIndexDeleted(frame: SessionIndexDeletedFrame): void {
    this.sessions.delete(frame.id);
  }
}
