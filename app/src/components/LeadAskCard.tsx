import type { LeadView } from "../lib/leadView";
import { useI18n } from "../i18n";

type Props = {
  view: Extract<LeadView, { kind: "ask" | "dispatch_confirm" }>;
  onChoose: (option: string) => void;
  disabled?: boolean;
};

export function LeadAskCard({ view, onChoose, disabled = false }: Props) {
  const { t } = useI18n();

  return (
    <div className="lead-ask">
      <p className="lead-ask__q">{view.question}</p>
      <div className="lead-ask__actions">
        {view.options.map((opt) => (
          <button
            key={opt}
            type="button"
            className={
              opt === view.recommended ? "lead-ask__primary" : "lead-ask__ghost"
            }
            disabled={disabled}
            onClick={() => {
              if (!disabled) onChoose(opt);
            }}
          >
            {opt}
          </button>
        ))}
      </div>
      {view.rationale && (
        <p className="lead-ask__why" title={view.rationale}>
          {t("leadAsk.rationale", { rationale: view.rationale })}
        </p>
      )}
    </div>
  );
}
