import { useEffect, useRef, type ReactNode } from "react";
import { SettingsShell, type SettingsPage } from "./SettingsShell";
import { useI18n } from "../../i18n";
import { SettingsAbout } from "./SettingsAbout";

type Props = {
  open: boolean;
  page: SettingsPage;
  onPageChange: (page: SettingsPage) => void;
  onClose: () => void;
  agentsContent: ReactNode;
  searchContent: ReactNode;
  reposContent: ReactNode;
  archivedProjectsContent: ReactNode;
  languageContent: ReactNode;
};

/**
 * 设置 = 浮层 sheet（spec §2.F）。
 * 暗背景虚化；点背景 / ✕ → onClose（Esc/⌘, 全局键在 App 持有·背景 inert 在 App 套）。
 * 开局 focus 进 sheet（焦点离开背景·配合 App 的 inert 满足「背后不可交互」）。
 * nav 由内嵌的单个 SettingsShell 提供（受控·onNavigate=onPageChange）。
 */
export function SettingsSheet(props: Props) {
  const sheetRef = useRef<HTMLDivElement>(null);
  const { t } = useI18n();
  const appVersion =
    typeof __APP_VERSION__ === "undefined" ? "dev" : __APP_VERSION__;
  useEffect(() => {
    if (props.open) sheetRef.current?.focus();
  }, [props.open]);
  if (!props.open) return null;
  return (
    <div className="settings-backdrop" onClick={props.onClose}>
      <div
        ref={sheetRef}
        className="settings-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-sheet-title"
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="settings-sheet__head">
          <span className="t" id="settings-sheet-title">
            {t("settings.title")}
          </span>
          <span className="kbd">
            <b>Esc</b> {t("settings.close")}
          </span>
          <button
            type="button"
            className="settings-sheet__x"
            aria-label={t("settings.closeSettings")}
            onClick={props.onClose}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.8}
            >
              <path d="M18 6L6 18M6 6l12 12" />
            </svg>
          </button>
        </div>
        <div className="settings-sheet__body">
          <SettingsShell activeKey={props.page} onNavigate={props.onPageChange}>
            {props.page === "agents" ? (
              props.agentsContent
            ) : props.page === "search" ? (
              props.searchContent
            ) : props.page === "language" ? (
              props.languageContent
            ) : props.page === "archivedProjects" ? (
              props.archivedProjectsContent
            ) : props.page === "about" ? (
              <SettingsAbout />
            ) : (
              props.reposContent
            )}
          </SettingsShell>
        </div>
        <div className="settings-sheet__version">
          <span>{t("settings.version")}</span>
          <span>AgentLoom v{appVersion}</span>
        </div>
      </div>
    </div>
  );
}
