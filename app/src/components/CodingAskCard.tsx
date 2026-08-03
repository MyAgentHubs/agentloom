import { useState } from "react";
import { useI18n } from "../i18n";

type Props = {
  kind: "verify" | "verify_failed";
  recommendedCmd: string | null;
  detail?: string | null;
  onConfirmVerify: (cmd: string) => void;
  onShelve: () => void;
  onViewChanges?: () => void;
  onRetryVerify?: () => void;
};

export function CodingAskCard({
  kind,
  recommendedCmd,
  detail,
  onConfirmVerify,
  onShelve,
  onViewChanges,
  onRetryVerify,
}: Props) {
  const { t } = useI18n();
  const [cmd, setCmd] = useState(recommendedCmd ?? "");
  if (kind === "verify_failed") {
    return (
      <div className="coding-ask" data-failed>
        <p className="coding-ask__q">{t("codingAsk.verifyFailed")}</p>
        <p className="coding-ask__q">
          {t("codingAsk.command")} <code>{detail || "—"}</code>
        </p>
        <div className="coding-ask__actions">
          <button
            className="coding-ask__ghost"
            onClick={() => onRetryVerify?.()}
          >
            {t("codingAsk.retryWithCommand")}
          </button>
          <button
            className="coding-ask__ghost"
            onClick={() => onViewChanges?.()}
          >
            {t("codingAsk.viewChanges")}
          </button>
          <button className="coding-ask__ghost" onClick={onShelve}>
            {t("codingAsk.shelve")}
          </button>
        </div>
      </div>
    );
  }
  if (kind === "verify") {
    return (
      <div className="coding-ask">
        <p className="coding-ask__q">{t("codingAsk.verifyPrompt")}</p>
        <input
          className="coding-ask__cmd"
          value={cmd}
          onChange={(e) => setCmd(e.target.value)}
        />
        <div className="coding-ask__actions">
          <button
            className="coding-ask__primary"
            onClick={() => onConfirmVerify(cmd)}
          >
            {t("codingAsk.startVerify")}
          </button>
        </div>
      </div>
    );
  }
  return null;
}
