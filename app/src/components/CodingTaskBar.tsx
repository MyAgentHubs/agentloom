import type { CodingTaskBlock } from "../types/agent";
import { useI18n } from "../i18n";
import { codingPhaseView } from "../lib/taskStatus";
import { CodingDecisionCard } from "./CodingDecisionCard";

type Props = {
  block: CodingTaskBlock;
  onOpenMember?: (runId: string, assignmentId: string) => void;
  onConfirmVerify?: (runId: string, cmd: string) => void;
  onShelve?: (runId: string) => void;
  onRetryVerify?: (runId: string) => void;
};

export function CodingTaskBar({
  block,
  onOpenMember,
  onConfirmVerify,
  onShelve,
  onRetryVerify,
}: Props) {
  const { t } = useI18n();
  const v = codingPhaseView(block.phase, block.worker_name);
  const progress =
    block.phase === "applied" && block.detail
      ? block.detail
      : (v.rawProgress ?? (v.progress ? t(v.progress) : null));
  const name =
    typeof v.name === "string" ? v.name : t(v.name.key, v.name.values);
  const label = t(v.label);
  return (
    <div className="taskstack">
      <div
        className={`task-row st-${v.dotClass}`}
        role="button"
        tabIndex={0}
        title={label}
        onClick={() => onOpenMember?.(block.run_id, block.assignment_id)}
        onKeyDown={(e) => {
          if (e.key === "Enter")
            onOpenMember?.(block.run_id, block.assignment_id);
        }}
      >
        <div className="tbody">
          <span className="tnm">{name}</span>
          {progress && <span className="tprog">{progress}</span>}
        </div>
        <span className={`task-badge st-${v.dotClass}`}>{label}</span>
      </div>
      <CodingDecisionCard
        block={block}
        onConfirmVerify={onConfirmVerify}
        onShelve={onShelve}
        onRetryVerify={onRetryVerify}
        onViewChanges={(rid, aid) => onOpenMember?.(rid, aid)}
      />
    </div>
  );
}
