import type { CodingTaskBlock, CodingPhase } from "../types/agent";
import { useI18n, type I18nKey } from "../i18n";
import { CodingDecisionCard } from "./CodingDecisionCard";

const PHASE_LABEL: Record<CodingPhase, I18nKey> = {
  finalizing: "codingTask.phase.finalizing",
  ask_verify: "codingTask.phase.askVerify",
  verifying: "codingTask.phase.verifying",
  verify_failed: "codingTask.phase.verifyFailed",
  ask_apply: "codingTask.phase.askApply",
  merging: "codingTask.phase.merging",
  applying: "codingTask.phase.applying", // b2b 关自动落地：merge 进 staging 后停隔离区·等用户点改动条·不再自动「应用中」
  applied: "codingTask.phase.applied",
  landing_blocked: "codingTask.phase.landingBlocked",
  shelved: "codingTask.phase.shelved",
  error: "codingTask.phase.error",
};

export function CodingTaskCard({
  block,
  onOpenDetail,
  onConfirmVerify,
  onShelve,
  onRetryVerify,
}: {
  block: CodingTaskBlock;
  onOpenDetail: (runId: string, assignmentId: string) => void;
  onConfirmVerify?: (runId: string, cmd: string) => void;
  onShelve?: (runId: string) => void;
  onRetryVerify?: (runId: string) => void;
}) {
  const { t } = useI18n();
  const { worker_name, phase, step_done, step_total } = block;
  const showProgress =
    typeof step_done === "number" &&
    typeof step_total === "number" &&
    step_total > 0;
  return (
    <div className="coding-task" data-phase={phase}>
      <div className="coding-task__head">
        <span className="coding-task__worker">{worker_name}</span>
        <span className="coding-task__phase">{t(PHASE_LABEL[phase])}</span>
        {showProgress && (
          <span className="coding-task__steps">
            {step_done} / {step_total}
          </span>
        )}
      </div>
      {block.lead_rationale && (
        <details className="coding-task__why">
          <summary>{t("codingTask.why")}</summary>
          {block.lead_rationale}
        </details>
      )}
      {showProgress && (
        <div className="coding-task__bar" aria-hidden>
          <div
            className="coding-task__bar-fill"
            style={{
              width: `${Math.round((step_done! / step_total!) * 100)}%`,
            }}
          />
        </div>
      )}
      <button
        className="coding-task__detail"
        onClick={() => onOpenDetail(block.run_id, block.assignment_id)}
      >
        {t("codingTask.details")}
      </button>
      <CodingDecisionCard
        block={block}
        onConfirmVerify={onConfirmVerify}
        onShelve={onShelve}
        onRetryVerify={onRetryVerify}
        onViewChanges={(rid, aid) => onOpenDetail(rid, aid)}
      />
    </div>
  );
}
