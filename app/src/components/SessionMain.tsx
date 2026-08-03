import React from "react";
import { useCallback, useState } from "react";
import { MessageStream } from "./MessageStream";
import { InputArea } from "./InputArea";
import { LeadAskCard } from "./LeadAskCard";
import type {
  AgentProfile,
  ChatMessage,
  ComposerRuntimeConfig,
  CodingTaskBlock,
  DecisionCardBlock,
  TeamRun,
} from "../types/agent";
import { canQuote, quoteBlock } from "../lib/quoteMessage";
import { formatRunMeta } from "../lib/runMeta";
import type { LeadView } from "../lib/leadView";
import type { SessionUsage } from "../lib/sessionUsage";
import type { Mode } from "./ModeDropdown";
import {
  ContinuationBriefPanel,
  type ContinuationDraftState,
  type ContinuationStartPayload,
} from "./ContinuationBriefPanel";
import { useI18n } from "../i18n";
import { summarizeLastStep } from "../lib/runningStatus";
import { activeDispatchWorker } from "../lib/dispatchCards";

type Done = {
  cost_usd: number | null;
  output_tokens: number | null;
  elapsed_sec: number | null;
};

const EMPTY_CALLBACK = () => {};

type Props = {
  messages: ChatMessage[];
  busy: boolean;
  composerBusy: boolean;
  memberRunning: boolean;
  runStartedAt: number | null;
  workingTokens?: number | null;
  agents?: AgentProfile[];
  agentId: string;
  done: Done | null;
  sessionUsage?: SessionUsage;
  sessionId: string | null;
  onAgentChange: (agentId: string) => void;
  onMenuAgents?: () => void;
  mode: Mode;
  onModeChange: (m: Mode) => void;
  onSend: (text: string, mode: Mode, config?: ComposerRuntimeConfig) => void;
  onMemberIdle?: () => void;
  onStop: () => void;
  onViewRun?: (runId?: string) => void;
  onUndoRun?: (runId: string) => void;
  onOpenMember?: (runId: string, assignmentId: string) => void;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  onOpenInspector?: (assignmentId: string) => void;
  gateView?: import("../lib/gateView").GateView | null;
  leadName?: string;
  enabledAgents?: AgentProfile[];
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
  pendingDecision?: DecisionCardBlock | null;
  gateFreezing?: boolean;
  onTakeOver?: () => void;
  onCleanRedispatch?: (runId: string) => void;
  canSend?: boolean;
  loading?: boolean;
  teamSaving?: boolean;
  teamLeadId?: string | null;
  rosterIds?: string[] | null;
  onSetLead?: (id: string | null, memberIds?: string[]) => void;
  onToggleRoster?: (id: string, allEnabledIds: string[]) => void;
  interruptedBanner?: React.ReactNode;
  leadView?: LeadView | null;
  onLeadChoose?: (opt: string) => void;
  liveRunsByRun?: Record<string, TeamRun>;
  liveCodingByRun?: Record<string, CodingTaskBlock>;
  continuationParentId?: string | null;
  continuationParentTitle?: string;
  continuationDraftState?: ContinuationDraftState;
  continuationStarting?: boolean;
  onRetryContinuation?: () => void;
  onCancelContinuation?: () => void;
  onStartContinuation?: (payload: ContinuationStartPayload) => void;
  readonlyReason?: string | null;
};

/**
 * cluster L Phase 3 plan C1 Task 3 · SessionMain
 *  - 删 session 标题（sidebar 已高亮 · 不重复）
 *  - 删冗余 meta 行：mode / engine 信息归 composer，main 顶部不重复
 * 引用功能（2026-05-30）：quoteRef 状态提升至此 · {sessionId,index} 守卫派生（消除切会话误指）。
 */
export const SessionMain = React.memo(function SessionMain({
  messages,
  busy,
  composerBusy,
  memberRunning,
  runStartedAt,
  workingTokens = null,
  agents,
  agentId,
  done,
  sessionUsage = { input: 0, output: 0 },
  sessionId,
  onAgentChange,
  onMenuAgents,
  mode,
  onModeChange,
  onSend,
  onMemberIdle,
  onStop,
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
  onDecisionChoose,
  pendingDecision = null,
  gateFreezing,
  onTakeOver,
  onCleanRedispatch,
  canSend,
  loading,
  teamSaving,
  teamLeadId,
  rosterIds,
  onSetLead,
  onToggleRoster,
  interruptedBanner,
  leadView,
  onLeadChoose,
  liveRunsByRun,
  liveCodingByRun,
  continuationParentId,
  continuationParentTitle,
  continuationDraftState,
  continuationStarting = false,
  onRetryContinuation,
  onCancelContinuation,
  onStartContinuation,
  readonlyReason = null,
}: Props) {
  const { t } = useI18n();
  const [quoteRef, setQuoteRef] = useState<{
    sessionId: string;
    index: number;
  } | null>(null);
  const lastStepSummary = busy
    ? summarizeLastStep(messages, t("stream.status.thinking"))
    : null;
  // UX②：状态行归因——lead 同步阻塞等 worker 时，别显得像卡死，指认在等谁。
  const activeWorker = busy ? activeDispatchWorker(messages) : null;

  // 守卫派生：要求 ref.sessionId === 当前 sessionId。
  //  - 切走（A→B）时 B !== A → quoted=null → chip 隐（不跨会话泄漏）。
  //  - 回到 A 时守卫重新成立 → chip 自动恢复（与 draft 保存一致；不主动清 ref）。
  //  - 单引用槽：在别的会话再点引用会覆盖（last-wins）。
  const quoted =
    quoteRef && quoteRef.sessionId === sessionId
      ? (messages[quoteRef.index] ?? null)
      : null;
  const quoteKey = quoted ? `${quoteRef!.sessionId}:${quoteRef!.index}` : null;
  const quoteActive = quoted != null;

  const handleQuote = useCallback(
    (i: number) => {
      if (sessionId && canQuote(messages[i])) {
        setQuoteRef({ sessionId, index: i });
      }
    },
    [messages, sessionId],
  );

  const handleSend = useCallback(
    (text: string, mode: Mode, config?: ComposerRuntimeConfig) => {
      const nextText = quoted ? quoteBlock(quoted) + text : text;
      if (config) {
        onSend(nextText, mode, config);
      } else {
        onSend(nextText, mode);
      }
      setQuoteRef(null);
    },
    [onSend, quoted],
  );
  const handleLeadChoose = useCallback(
    (opt: string) => onLeadChoose?.(opt),
    [onLeadChoose],
  );
  const handleClearQuote = useCallback(() => setQuoteRef(null), []);

  return (
    <div className="session">
      {interruptedBanner}
      <MessageStream
        messages={messages}
        busy={busy}
        sessionId={sessionId}
        onQuote={handleQuote}
        quoteActive={quoteActive}
        onViewRun={onViewRun}
        onUndoRun={onUndoRun}
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
        onOpenMember={onOpenMember}
        onOpenInspector={onOpenInspector}
        gateView={gateView}
        leadName={leadName}
        teamLeadId={teamLeadId}
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
        onDecisionChoose={onDecisionChoose}
        gateFreezing={gateFreezing}
        onTakeOver={onTakeOver}
        onCleanRedispatch={onCleanRedispatch}
        liveRunsByRun={liveRunsByRun}
        liveCodingByRun={liveCodingByRun}
        readonlyReason={readonlyReason}
      />
      {leadView &&
        (leadView.kind === "ask" || leadView.kind === "dispatch_confirm") && (
          <LeadAskCard
            view={leadView}
            onChoose={handleLeadChoose}
            disabled={readonlyReason != null}
          />
        )}
      {leadView && leadView.kind === "finish" && (
        <div className="lead-ask">
          <p className="lead-ask__q">{t("sessionMain.finished")}</p>
          {leadView.rationale && (
            <p className="lead-ask__why">{leadView.rationale}</p>
          )}
        </div>
      )}
      {continuationParentId &&
        continuationDraftState &&
        onStartContinuation && (
          <div className="cc-brief-wrap">
            <ContinuationBriefPanel
              parentSessionId={continuationParentId}
              parentTitle={continuationParentTitle}
              draftState={continuationDraftState}
              starting={continuationStarting}
              onRetry={onRetryContinuation ?? EMPTY_CALLBACK}
              onCancel={onCancelContinuation ?? EMPTY_CALLBACK}
              onStart={onStartContinuation}
            />
          </div>
        )}
      <InputArea
        composerBusy={composerBusy}
        memberRunning={memberRunning}
        running={busy}
        agents={agents}
        agentId={agentId}
        onAgentChange={onAgentChange}
        onMenuAgents={onMenuAgents}
        sessionId={sessionId}
        mode={mode}
        onModeChange={onModeChange}
        onSend={handleSend}
        onMemberIdle={onMemberIdle}
        onStop={onStop}
        quoted={quoted}
        quoteKey={quoteKey}
        onClearQuote={handleClearQuote}
        pendingDecision={pendingDecision}
        onDecisionChoose={onDecisionChoose}
        canSend={canSend}
        readonlyReason={readonlyReason}
        loading={loading}
        teamSaving={teamSaving}
        teamLeadId={teamLeadId}
        rosterIds={rosterIds}
        onSetLead={onSetLead}
        onToggleRoster={onToggleRoster}
        runMeta={formatRunMeta(done)}
        sessionUsage={sessionUsage}
        runStartedAt={runStartedAt}
        workingTokens={workingTokens}
        lastStepSummary={lastStepSummary}
        streamMessages={messages}
        activeWorker={activeWorker}
      />
    </div>
  );
});
