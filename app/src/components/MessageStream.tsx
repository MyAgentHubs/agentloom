import React from "react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type {
  Block,
  ChatMessage,
  CodingTaskBlock,
  TeamRun,
} from "../types/agent";
import { useAutoScroll } from "../hooks/useAutoScroll";
import { AgentAvatar } from "./AgentAvatar";
import { MessageContent } from "./MessageContent";
import { MessageActions } from "./MessageActions";
import { ScrollButtons } from "./ScrollButtons";
import { canQuote } from "../lib/quoteMessage";
import { buildLeadTurns, type LeadTurnView } from "../lib/leadTurns";
import { RunLeadTurn } from "./RunLeadTurn";
import { useI18n } from "../i18n";

type Props = {
  messages: ChatMessage[];
  busy: boolean;
  sessionId?: string | null;
  onQuote?: (index: number) => void;
  quoteActive?: boolean;
  onViewRun?: (runId?: string) => void;
  onUndoRun?: (runId: string) => void;
  onOpenMember?: (runId: string, assignmentId: string) => void;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  onOpenInspector?: (assignmentId: string) => void;
  gateView?: import("../lib/gateView").GateView | null;
  leadName?: string;
  teamLeadId?: string | null;
  enabledAgents?: import("../types/agent").AgentProfile[];
  onGateAction?: (a: import("../lib/gateReducer").GateAction) => void;
  onGateFreeze?: () => void;
  onGateRedraft?: () => void;
  onGateRetry?: () => void;
  onGateManual?: () => void;
  onGateBackToNormal?: () => void;
  onConfirmVerify?: (runId: string, cmd: string) => void;
  onRetryVerify?: (runId: string) => void;
  onShelve?: (runId: string) => void;
  onDecisionChoose?: (decisionId: string, option: string) => void;
  gateFreezing?: boolean;
  onTakeOver?: () => void;
  onCleanRedispatch?: (runId: string) => void;
  liveRunsByRun?: Record<string, TeamRun>;
  liveCodingByRun?: Record<string, CodingTaskBlock>;
  readonlyReason?: string | null;
};

const EMPTY_LIVE_RUNS: Record<string, TeamRun> = {};
const EMPTY_LIVE_CODING: Record<string, CodingTaskBlock> = {};
const INITIAL_RENDER_COUNT = 30;
const RENDER_CHUNK_SIZE = 20;

function initialVisibleStart(messageCount: number): number {
  return Math.max(0, messageCount - INITIAL_RENDER_COUNT);
}

function nonEmpty(value?: string | null): string | undefined {
  return value && value.trim() !== "" ? value : undefined;
}

function assistantName(message: ChatMessage): string {
  return (
    nonEmpty(message.agent_name_snapshot) ??
    nonEmpty(message.agent_id) ??
    nonEmpty(message.engine) ??
    "?"
  );
}

function assistantAvatarKind(message: ChatMessage): string {
  return nonEmpty(message.agent_id) ?? nonEmpty(message.engine) ?? "?";
}

function messageId(message: ChatMessage): string | null {
  const id = (message as { id?: unknown }).id;
  return typeof id === "string" && id.length > 0 ? id : null;
}

function messageHasLeadTurnBlock(message: ChatMessage): boolean {
  return message.content.some(
    (block) =>
      block.type === "team_run" ||
      block.type === "coding_task" ||
      block.type === "lead_summary" ||
      block.type === "decision_card",
  );
}

type RunBlock =
  | { type: "team_run" | "coding_task"; run_id: string }
  | { type: "decision_card"; source_run_id: string };

function runBlockForMessage(message: ChatMessage): RunBlock | undefined {
  if (message.role !== "assistant") return undefined;
  return message.content.find(
    (block) =>
      block.type === "team_run" ||
      block.type === "coding_task" ||
      block.type === "decision_card",
  ) as RunBlock | undefined;
}

function runBlockKey(runBlock: RunBlock): string {
  const runId =
    runBlock.type === "decision_card"
      ? runBlock.source_run_id
      : runBlock.run_id;
  return `${runBlock.type}:${runId}`;
}

function hashString(value: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < value.length; i++) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36);
}

function stableMessageKeys(messages: ChatMessage[]): string[] {
  const occurrences = new Map<string, number>();
  return messages.map((message) => {
    const id = messageId(message);
    if (id) return `message:id:${id}`;

    const firstBlock = message.content[0];
    const fingerprint = `${message.role}:${hashString(
      JSON.stringify(firstBlock ?? null),
    )}`;
    const occurrence = occurrences.get(fingerprint) ?? 0;
    occurrences.set(fingerprint, occurrence + 1);
    return `message:${fingerprint}:${occurrence}`;
  });
}

function messageTurnKey(message: ChatMessage, fallbackKey: string): string {
  const runBlock = runBlockForMessage(message);
  return runBlock ? runBlockKey(runBlock) : fallbackKey;
}

function leadTurnOrder(messages: ChatMessage[], runId: string): number | null {
  for (let i = 0; i < messages.length; i++) {
    if (
      messages[i].content.some(
        (block) =>
          ((block.type === "team_run" ||
            block.type === "coding_task" ||
            block.type === "lead_summary") &&
            block.run_id === runId) ||
          (block.type === "decision_card" && block.source_run_id === runId),
      )
    ) {
      return i;
    }
  }
  return null;
}

function blockWeight(block: Block): number {
  if (block.type === "text" || block.type === "thinking") {
    return block.text.length;
  }
  if (block.type === "tool") {
    return block.id.length + block.status.length + (block.output?.length ?? 0);
  }
  if (block.type === "gate_card" || block.type === "draft_failed") {
    return block.type.length;
  }
  if (block.type === "team_run") {
    return (
      block.run_id.length +
      (block.lead?.length ?? 0) +
      block.members.reduce(
        (sum, member) =>
          sum +
          member.assignment_id.length +
          member.name.length +
          member.status.length +
          member.steps_done +
          member.steps_total +
          member.blocks.length,
        0,
      )
    );
  }
  if (block.type === "coding_task") {
    return (
      block.run_id.length +
      block.assignment_id.length +
      block.worker_name.length +
      block.phase.length +
      (block.detail?.length ?? 0) +
      (block.verify_cmd?.length ?? 0)
    );
  }
  if (block.type === "lead_summary") {
    return (
      block.run_id.length +
      block.summary_source.length +
      block.status.kind.length +
      block.sections.reduce(
        (sum, section) =>
          sum + section.heading.length + (section.body_richtext?.length ?? 0),
        0,
      ) +
      block.findings.length +
      block.artifact_refs.length
    );
  }
  if (block.type === "dispatch_card") {
    return (
      block.run_id.length +
      block.member.assignment_id.length +
      block.member.status.length +
      block.member.steps_done +
      block.member.steps_total +
      block.member.blocks.length
    );
  }
  return block.type.length;
}

function teamRunWeight(run: TeamRun): number {
  return blockWeight({
    type: "team_run",
    run_id: run.run_id,
    goal: run.goal,
    lead: run.lead,
    members: run.members,
  });
}

type MessageTurnProps = {
  turnKey: string;
  message: ChatMessage;
  hovered: boolean;
  streaming: boolean;
  showAuthorWorking: boolean;
  sessionId: string | null;
  teamLeadId?: string | null;
  onHoverChange: (turnKey: string, hovered: boolean) => void;
  onQuoteTurn: (turnKey: string) => void;
  onViewRun?: (runId?: string) => void;
  onUndoRun?: (runId: string) => void;
  onOpenMember?: (runId: string, assignmentId: string) => void;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  onOpenInspector?: (assignmentId: string) => void;
  gateView?: import("../lib/gateView").GateView | null;
  leadName?: string;
  enabledAgents?: import("../types/agent").AgentProfile[];
  onGateAction?: (action: import("../lib/gateReducer").GateAction) => void;
  onGateFreeze?: () => void;
  onGateRedraft?: () => void;
  onGateRetry?: () => void;
  onGateManual?: () => void;
  onGateBackToNormal?: () => void;
  onConfirmVerify?: (runId: string, cmd: string) => void;
  onRetryVerify?: (runId: string) => void;
  onShelve?: (runId: string) => void;
  gateFreezing?: boolean;
  onTakeOver?: () => void;
  onCleanRedispatch?: (runId: string) => void;
  readonlyReason?: string | null;
};

function sameMessage(a: ChatMessage, b: ChatMessage): boolean {
  if (a === b) return true;
  if (
    a.role !== b.role ||
    a.engine !== b.engine ||
    a.agent_id !== b.agent_id ||
    a.agent_name_snapshot !== b.agent_name_snapshot ||
    a.created_at !== b.created_at ||
    messageId(a) !== messageId(b) ||
    a.content.length !== b.content.length
  ) {
    return false;
  }
  // App.displayMessages 会浅克隆每条消息和 content 数组，但未变化的 block 仍保留引用；
  // run_card 等派生块才做结构比较，确保状态真变化时不会被 memo 吞掉。
  return a.content.every((block, index) => {
    const nextBlock = b.content[index];
    return (
      block === nextBlock || JSON.stringify(block) === JSON.stringify(nextBlock)
    );
  });
}

function sameMessageTurnProps(
  previous: MessageTurnProps,
  next: MessageTurnProps,
): boolean {
  for (const key of Object.keys(previous) as (keyof MessageTurnProps)[]) {
    if (key === "message") continue;
    if (!Object.is(previous[key], next[key])) return false;
  }
  return sameMessage(previous.message, next.message);
}

const MessageTurn = React.memo(function MessageTurn({
  turnKey,
  message,
  hovered,
  streaming,
  showAuthorWorking,
  sessionId,
  teamLeadId,
  onHoverChange,
  onQuoteTurn,
  onViewRun,
  onUndoRun,
  onOpenMember,
  onOpenPreview,
  onOpenLightbox,
  onOpenInspector,
  gateView,
  leadName,
  enabledAgents,
  onGateAction,
  onGateFreeze,
  onGateRedraft,
  onGateRetry,
  onGateManual,
  onGateBackToNormal,
  onConfirmVerify,
  onRetryVerify,
  onShelve,
  gateFreezing,
  onTakeOver,
  onCleanRedispatch,
  readonlyReason,
}: MessageTurnProps) {
  const { t } = useI18n();
  const runBlock = runBlockForMessage(message);
  const teamLead =
    message.role === "assistant"
      ? message.content.find((block) => block.type === "team_run" && block.lead)
      : undefined;

  return (
    <div
      className={`turn turn--${message.role}${
        hovered ? " turn--hovered" : ""
      }${runBlock ? " turn--runbar" : ""}`}
      onMouseEnter={() => onHoverChange(turnKey, true)}
      onMouseLeave={() => onHoverChange(turnKey, false)}
    >
      {!teamLead && !runBlock && (
        <div className="turn__author">
          <AgentAvatar
            kind={
              message.role === "user" ? "user" : assistantAvatarKind(message)
            }
          />
          <span className="turn__name">
            {message.role === "user"
              ? t("stream.role.user")
              : assistantName(message)}
          </span>
          {message.role === "assistant" &&
            teamLeadId != null &&
            message.agent_id === teamLeadId && (
              <span className="turn__role">{t("stream.role.lead")}</span>
            )}
          {showAuthorWorking && (
            <span
              className="turn__working"
              role="status"
              aria-label={t("stream.status.workingAria", {
                name: assistantName(message),
              })}
            >
              <span className="turn__working-dot" aria-hidden="true" />
              {t("stream.status.working")}
            </span>
          )}
        </div>
      )}
      <MessageContent
        blocks={message.content}
        streaming={streaming}
        sessionId={sessionId}
        onViewRun={onViewRun}
        onUndoRun={onUndoRun}
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
        onOpenMember={onOpenMember}
        gateView={gateView}
        leadName={leadName}
        enabledAgents={enabledAgents}
        onGateAction={onGateAction}
        onGateFreeze={onGateFreeze}
        onGateRedraft={onGateRedraft}
        onGateRetry={onGateRetry}
        onGateManual={onGateManual}
        onGateBackToNormal={onGateBackToNormal}
        onConfirmVerify={onConfirmVerify}
        onRetryVerify={onRetryVerify}
        onShelve={onShelve}
        gateFreezing={gateFreezing}
        onTakeOver={onTakeOver}
        onCleanRedispatch={onCleanRedispatch}
        onOpenInspector={onOpenInspector}
        readonlyReason={readonlyReason}
      />
      <MessageActions
        message={message}
        canQuote={canQuote(message)}
        onQuote={() => onQuoteTurn(turnKey)}
      />
    </div>
  );
}, sameMessageTurnProps);

export const MessageStream = React.memo(function MessageStream({
  messages,
  busy,
  sessionId = null,
  onQuote,
  quoteActive = false,
  onViewRun,
  onUndoRun,
  onOpenMember,
  onOpenPreview,
  onOpenLightbox,
  onOpenInspector,
  gateView,
  leadName,
  teamLeadId,
  enabledAgents,
  onGateAction,
  onGateFreeze,
  onGateRedraft,
  onGateRetry,
  onGateManual,
  onGateBackToNormal,
  onConfirmVerify,
  onRetryVerify,
  onShelve,
  onDecisionChoose,
  gateFreezing,
  onTakeOver,
  onCleanRedispatch,
  liveRunsByRun,
  liveCodingByRun,
  readonlyReason,
}: Props) {
  const streamRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const pendingPrependScrollRef = useRef<{
    scrollHeight: number;
    scrollTop: number;
    wasAtBottom: boolean;
  } | null>(null);
  // WKWebView 的 CSS :hover 伪类会「粘住」不清除，故用 JS 事件切 class 做 actions 显隐。
  const [hoveredTurnKey, setHoveredTurnKey] = useState<string | null>(null);
  const [renderWindow, setRenderWindow] = useState(() => ({
    sessionId,
    start: initialVisibleStart(messages.length),
  }));
  let visibleStart = Math.min(
    renderWindow.start,
    initialVisibleStart(messages.length),
  );
  if (renderWindow.sessionId !== sessionId) {
    visibleStart = initialVisibleStart(messages.length);
    pendingPrependScrollRef.current = null;
    setRenderWindow({ sessionId, start: visibleStart });
  }
  const liveRuns = liveRunsByRun ?? EMPTY_LIVE_RUNS;
  const liveCoding = liveCodingByRun ?? EMPTY_LIVE_CODING;
  const fallbackMessageKeys = useMemo(
    () => stableMessageKeys(messages),
    [messages],
  );
  const messageTurnKeys = useMemo(
    () =>
      messages.map((message, index) =>
        messageTurnKey(message, fallbackMessageKeys[index]),
      ),
    [fallbackMessageKeys, messages],
  );
  const quoteIndexByTurnKey = useMemo(
    () => new Map(messageTurnKeys.map((turnKey, index) => [turnKey, index])),
    [messageTurnKeys],
  );
  const quoteIndexByTurnKeyRef = useRef(quoteIndexByTurnKey);
  quoteIndexByTurnKeyRef.current = quoteIndexByTurnKey;
  const onQuoteRef = useRef(onQuote);
  onQuoteRef.current = onQuote;
  const handleQuoteTurn = useCallback((turnKey: string) => {
    const index = quoteIndexByTurnKeyRef.current.get(turnKey);
    if (index !== undefined) onQuoteRef.current?.(index);
  }, []);
  const handleHoverChange = useCallback((turnKey: string, hovered: boolean) => {
    setHoveredTurnKey((current) => {
      if (hovered) return turnKey;
      return current === turnKey ? null : current;
    });
  }, []);
  const { turns, consumedMessageIds } = useMemo(
    () => buildLeadTurns(messages, liveRuns, liveCoding),
    [messages, liveRuns, liveCoding],
  );
  const turnsByOrder = useMemo(() => {
    const byOrder = new Map<number, LeadTurnView[]>();
    let liveOrder = messages.length;
    for (const turn of turns) {
      const order = leadTurnOrder(messages, turn.runId) ?? liveOrder++;
      const list = byOrder.get(order) ?? [];
      list.push(turn);
      byOrder.set(order, list);
    }
    return byOrder;
  }, [messages, turns]);
  const contentKey =
    messages.length +
    messages.reduce(
      (total, message) =>
        total +
        message.content.reduce((sum, block) => sum + blockWeight(block), 0),
      0,
    ) +
    Object.values(liveRuns).reduce((sum, run) => sum + teamRunWeight(run), 0) +
    Object.values(liveCoding).reduce(
      (sum, block) => sum + blockWeight(block),
      0,
    );
  // quoteActive 折进 contentKey：引用 chip 出现/消失会改变 composer 高度→stream 可视高度，
  // 把它纳入 auto-scroll 触发键，使原本贴底者在 chip 开合后仍贴底。
  // 用 *2+bit 无碰撞编码（contentKey 是非负整数，会随 sweepRunning 增减，+bit 会碰撞）。
  const { stickRef, scrollToBottom } = useAutoScroll(
    streamRef,
    contentKey * 2 + Number(quoteActive),
    contentRef,
  );
  useLayoutEffect(() => {
    const pending = pendingPrependScrollRef.current;
    const target = streamRef.current;
    if (!pending || !target) return;
    pendingPrependScrollRef.current = null;

    if (pending.wasAtBottom) {
      target.scrollTop = target.scrollHeight;
      stickRef.current = true;
      return;
    }
    target.scrollTop =
      pending.scrollTop + (target.scrollHeight - pending.scrollHeight);
  }, [visibleStart, stickRef]);

  useEffect(() => {
    if (visibleStart === 0) return;

    const prependChunk = () => {
      const target = streamRef.current;
      if (target) {
        pendingPrependScrollRef.current = {
          scrollHeight: target.scrollHeight,
          scrollTop: target.scrollTop,
          wasAtBottom: stickRef.current,
        };
      }
      setRenderWindow((current) => {
        if (current.sessionId !== sessionId) return current;
        return {
          sessionId,
          start: Math.max(0, visibleStart - RENDER_CHUNK_SIZE),
        };
      });
    };

    if (typeof window.requestIdleCallback === "function") {
      const cancelIdleCallback = window.cancelIdleCallback;
      const handle = window.requestIdleCallback(prependChunk);
      return () => cancelIdleCallback(handle);
    }
    const handle = window.setTimeout(prependChunk, 0);
    return () => window.clearTimeout(handle);
  }, [sessionId, stickRef, visibleStart]);
  const lastAssistantIdx = (() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === "assistant") return i;
    }
    return -1;
  })();

  const renderRunTurn = (turn: LeadTurnView) => (
    <RunLeadTurn
      key={`run:${turn.runId}`}
      turn={turn}
      sessionId={sessionId}
      onViewProcess={(runId) => onViewRun?.(runId)}
      onOpenMember={onOpenMember}
      onUndoRun={onUndoRun}
      onConfirmVerify={readonlyReason ? undefined : onConfirmVerify}
      onShelve={readonlyReason ? undefined : onShelve}
      onRetryVerify={readonlyReason ? undefined : onRetryVerify}
      onDecisionChoose={readonlyReason ? undefined : onDecisionChoose}
    />
  );

  const renderItems = [];
  for (let i = visibleStart; i < messages.length; i++) {
    const message = messages[i];
    if (message.engine === "verifier-result") continue;
    for (const turn of turnsByOrder.get(i) ?? []) {
      renderItems.push(renderRunTurn(turn));
    }
    const id = messageId(message);
    const consumed =
      (id != null && consumedMessageIds.has(id)) ||
      messageHasLeadTurnBlock(message);
    if (!consumed) {
      const turnKey = messageTurnKeys[i];
      const streaming = busy && i === lastAssistantIdx;
      // 块 B（T5·BLOCK-4·P1-3）：含 team_run/coding_task/decision_card 的消息继续用
      // 块内 run id 派 key；普通消息优先用 client id，否则用 role + 首块内容指纹 +
      // 同指纹序号。displayMessages 会克隆消息对象，故不能用 WeakMap 对象身份。
      renderItems.push(
        <MessageTurn
          key={turnKey}
          turnKey={turnKey}
          message={message}
          hovered={hoveredTurnKey === turnKey}
          streaming={streaming}
          showAuthorWorking={streaming && message.role === "assistant"}
          sessionId={sessionId}
          teamLeadId={teamLeadId}
          onHoverChange={handleHoverChange}
          onQuoteTurn={handleQuoteTurn}
          onViewRun={onViewRun}
          onUndoRun={onUndoRun}
          onOpenMember={onOpenMember}
          onOpenPreview={onOpenPreview}
          onOpenLightbox={onOpenLightbox}
          onOpenInspector={onOpenInspector}
          gateView={gateView}
          leadName={leadName}
          enabledAgents={enabledAgents}
          onGateAction={onGateAction}
          onGateFreeze={onGateFreeze}
          onGateRedraft={onGateRedraft}
          onGateRetry={onGateRetry}
          onGateManual={onGateManual}
          onGateBackToNormal={onGateBackToNormal}
          onConfirmVerify={onConfirmVerify}
          onRetryVerify={onRetryVerify}
          onShelve={onShelve}
          gateFreezing={gateFreezing}
          onTakeOver={onTakeOver}
          onCleanRedispatch={onCleanRedispatch}
          readonlyReason={readonlyReason}
        />,
      );
    }
  }
  for (let order = messages.length; turnsByOrder.has(order); order++) {
    for (const turn of turnsByOrder.get(order) ?? []) {
      renderItems.push(renderRunTurn(turn));
    }
  }

  return (
    <div className="stream-wrap">
      <div className="stream" ref={streamRef}>
        <div className="stream-content" ref={contentRef}>
          {renderItems}
        </div>
      </div>
      <ScrollButtons scrollRef={streamRef} scrollToBottom={scrollToBottom} />
    </div>
  );
});
