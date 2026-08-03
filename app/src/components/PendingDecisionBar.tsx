import { useI18n } from "../i18n";
import { splitDecisionOption } from "../lib/decisionOption";
import type { DecisionCardBlock } from "../types/agent";

type Props = {
  block: DecisionCardBlock;
  onChoose?: (decisionId: string, option: string) => void;
};

export function PendingDecisionBar({ block, onChoose }: Props) {
  const { t } = useI18n();

  return (
    <div className="composer__pending" role="status" aria-live="polite">
      <span className="composer__pending-label">
        {t("composer.pendingDecision.label")}
      </span>
      <span className="composer__pending-question" title={block.question}>
        {block.question}
      </span>
      <div className="composer__pending-actions">
        {block.options.map((option) => {
          const recommended = option === block.recommended;
          const { label } = splitDecisionOption(option);

          return (
            <button
              key={option}
              type="button"
              className={
                "composer__pending-option" + (recommended ? " rec" : "")
              }
              disabled={!onChoose}
              onClick={() => onChoose?.(block.decision_id, option)}
            >
              <span>{label}</span>
              {recommended && (
                <span className="rec-pill">
                  {t("decisionCard.recommended")}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
