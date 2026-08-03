import type { DraftFailure } from "../types/gate";
import { useI18n } from "../i18n";

type Props = {
  failure: DraftFailure;
  onRetry: () => void;
  onManual: () => void;
  onBackToNormal: () => void;
  disabled?: boolean;
};

function describe(f: DraftFailure, t: ReturnType<typeof useI18n>["t"]): string {
  if (f.kind === "parseExhausted")
    return t("draftFailed.parseExhausted", {
      attempts: f.attempts,
      lastError: f.lastError,
    });
  return t("draftFailed.invokeFailed", { reason: f.reason });
}

export function DraftFailedCard({
  failure,
  onRetry,
  onManual,
  onBackToNormal,
  disabled = false,
}: Props) {
  const { t } = useI18n();

  return (
    <div className="draft-failed">
      <div className="draft-failed__head">{t("draftFailed.title")}</div>
      <div className="draft-failed__msg">{describe(failure, t)}</div>
      <div className="draft-failed__acts">
        <button
          type="button"
          className="draft-failed__btn is-primary"
          disabled={disabled}
          onClick={() => {
            if (!disabled) onRetry();
          }}
        >
          {t("draftFailed.retry")}
        </button>
        <button
          type="button"
          className="draft-failed__btn"
          disabled={disabled}
          onClick={() => {
            if (!disabled) onManual();
          }}
        >
          {t("draftFailed.manual")}
        </button>
        <button
          type="button"
          className="draft-failed__btn"
          disabled={disabled}
          onClick={() => {
            if (!disabled) onBackToNormal();
          }}
        >
          {t("draftFailed.backToNormal")}
        </button>
      </div>
    </div>
  );
}
