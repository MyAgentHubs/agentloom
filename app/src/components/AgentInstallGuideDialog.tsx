import { useEffect, useId, useRef } from "react";
import { useI18n } from "../i18n";
import { openInstallGuide } from "../lib/installGuide";

type Props = {
  onClose: () => void;
  onOpenSettings: () => void;
};

export function AgentInstallGuideDialog({ onClose, onOpenSettings }: Props) {
  const { t } = useI18n();
  const titleId = useId();
  const reasonId = useId();
  const dismissRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    dismissRef.current?.focus();
  }, []);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [onClose]);

  const handleOpenSettings = () => {
    onClose();
    onOpenSettings();
  };

  return (
    <div className="dialog__backdrop" onClick={onClose}>
      <div
        className="dialog agent-install-guide"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={reasonId}
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="dialog__title" id={titleId}>
          {t("onboarding.installGuide.title")}
        </h2>
        <p className="dialog__body" id={reasonId}>
          {t("onboarding.installGuide.reason")}
        </p>
        <div className="agent-install-guide__options">
          <div className="agent-install-guide__option">
            <div>
              <h3 className="agent-install-guide__name">myagent</h3>
              <p className="agent-install-guide__description">
                {t("onboarding.installGuide.harnessDescription")}
              </p>
            </div>
            <button
              type="button"
              className="dialog__btn"
              onClick={handleOpenSettings}
            >
              {t("onboarding.installGuide.configureHarness")}
            </button>
          </div>
          <div className="agent-install-guide__option">
            <div>
              <h3 className="agent-install-guide__name">Claude Code</h3>
              <p className="agent-install-guide__description">
                {t("onboarding.installGuide.claudeDescription")}
              </p>
            </div>
            <button
              type="button"
              className="dialog__btn"
              onClick={() => void openInstallGuide("claude")}
            >
              {t("onboarding.installGuide.openInstallGuide")}
            </button>
          </div>
          <div className="agent-install-guide__option">
            <div>
              <h3 className="agent-install-guide__name">Codex</h3>
              <p className="agent-install-guide__description">
                {t("onboarding.installGuide.codexDescription")}
              </p>
            </div>
            <button
              type="button"
              className="dialog__btn"
              onClick={() => void openInstallGuide("codex")}
            >
              {t("onboarding.installGuide.openInstallGuide")}
            </button>
          </div>
        </div>
        <div className="dialog__actions">
          <button
            ref={dismissRef}
            type="button"
            className="dialog__btn"
            onClick={onClose}
          >
            {t("onboarding.installGuide.dismiss")}
          </button>
          <button
            type="button"
            className="dialog__btn dialog__btn--primary"
            onClick={handleOpenSettings}
          >
            {t("onboarding.installGuide.openSettings")}
          </button>
        </div>
      </div>
    </div>
  );
}
