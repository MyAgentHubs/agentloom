import { useState } from "react";
import { useI18n } from "../i18n";
import type { LeadTurnView } from "../lib/leadTurns";
import { AgentAvatar } from "./AgentAvatar";
import { BackgroundTaskStack } from "./BackgroundTaskStack";
import { CodingTaskBar } from "./CodingTaskBar";
import { DecisionCard } from "./DecisionCard";
import { LeadSummaryBlock } from "./LeadSummaryBlock";

type Props = {
  turn: LeadTurnView;
  sessionId?: string | null;
  onViewProcess?: (runId: string) => void;
  onOpenMember?: (runId: string, assignmentId: string) => void;
  onUndoRun?: (runId: string) => void;
  onConfirmVerify?: (runId: string, cmd: string) => void;
  onShelve?: (runId: string) => void;
  onRetryVerify?: (runId: string) => void;
  onDecisionChoose?: (decisionId: string, option: string) => void;
};

export function RunLeadTurn({
  turn,
  sessionId,
  onViewProcess,
  onOpenMember,
  onUndoRun,
  onConfirmVerify,
  onShelve,
  onRetryVerify,
  onDecisionChoose,
}: Props) {
  const { t } = useI18n();
  const [processOpen, setProcessOpen] = useState(false);
  const showWorkerTaskStack = turn.codingTask == null;
  const taskCount =
    (showWorkerTaskStack ? turn.members.length : 0) + (turn.codingTask ? 1 : 0);
  const hasStoppedMember = turn.members.some((m) => m.status === "stopped");
  const hasFailedMember = turn.members.some((m) => m.status === "failed");
  const showStoppedNotice = hasStoppedMember && !hasFailedMember;
  const leadName = turn.lead?.trim() || t("runLeadTurn.fallbackLeadName");

  const taskBars = (
    <>
      {showWorkerTaskStack && (
        <BackgroundTaskStack
          runId={turn.runId}
          members={turn.members}
          onOpenMember={onOpenMember}
          onUndoRun={onUndoRun}
        />
      )}
      {turn.codingTask && (
        <CodingTaskBar
          block={turn.codingTask}
          onOpenMember={onOpenMember}
          onConfirmVerify={onConfirmVerify}
          onShelve={onShelve}
          onRetryVerify={onRetryVerify}
        />
      )}
    </>
  );

  return (
    <div className="turn">
      <div className="turn__author">
        <AgentAvatar kind={leadName} />
        <span className="turn__name">{leadName}</span>
        <span className="turn__name">{t("runLeadTurn.captain")}</span>
      </div>

      {turn.verdict === null ? (
        taskBars
      ) : (
        <>
          <LeadSummaryBlock
            block={turn.verdict}
            sessionId={sessionId}
            stopNotice={showStoppedNotice}
          />
          {turn.showProcessFold && (
            <div className={`proc-fold${processOpen ? " open" : ""}`}>
              <div className="pf-row">
                <button
                  type="button"
                  className="pf-tog"
                  aria-expanded={processOpen}
                  onClick={() => setProcessOpen((open) => !open)}
                >
                  <span className="pf-tri" aria-hidden="true">
                    <svg viewBox="0 0 24 24">
                      <path d="M9 6l6 6-6 6" />
                    </svg>
                  </span>
                  <span className="pf-lab">
                    {t("runLeadTurn.processSummary", { count: taskCount })}
                  </span>
                </button>
                <span className="pf-sp" />
                <button
                  type="button"
                  className="pf-view"
                  onClick={() => onViewProcess?.(turn.runId)}
                >
                  {t("runLeadTurn.viewProcess")}
                </button>
              </div>
              <div className="pf-list">{taskBars}</div>
            </div>
          )}
        </>
      )}

      {turn.decisionCards.map((card) => (
        <DecisionCard
          key={card.decision_id}
          block={card}
          onChoose={onDecisionChoose}
        />
      ))}
    </div>
  );
}
