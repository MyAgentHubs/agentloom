import { createPortal } from "react-dom";
import { useEffect, useState, type MouseEvent } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useI18n } from "../i18n";
import {
  GITHUB_ISSUES_LABEL,
  GITHUB_ISSUES_URL,
  SUPPORT_EMAIL,
  WEBSITE_LABEL,
  WEBSITE_URL,
} from "../constants/about";
import agentloomIcon from "../assets/agentloom-icon.svg";

type Props = {
  open: boolean;
  onClose: () => void;
};

export function AboutDialog({ open, onClose }: Props) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const appVersion =
    typeof __APP_VERSION__ === "undefined" ? "dev" : __APP_VERSION__;

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open, onClose]);

  const handleCopyVersion = async () => {
    try {
      await navigator.clipboard.writeText(`v${appVersion}`);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // clipboard write failed — silently ignore
    }
  };

  const openLink = (event: MouseEvent<HTMLAnchorElement>, url: string) => {
    event.preventDefault();
    void openUrl(url);
  };

  const rows = [
    {
      href: WEBSITE_URL,
      label: t("settings.about.website"),
      text: WEBSITE_LABEL,
    },
    {
      href: GITHUB_ISSUES_URL,
      label: t("settings.about.feedback"),
      text: GITHUB_ISSUES_LABEL,
    },
    {
      href: `mailto:${SUPPORT_EMAIL}`,
      label: t("settings.about.support"),
      text: SUPPORT_EMAIL,
    },
  ];

  if (!open) return null;

  return createPortal(
    <div className="dialog__backdrop" onClick={onClose}>
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-label="About AgentLoom"
        onClick={(e) => e.stopPropagation()}
        style={{ maxWidth: 420 }}
      >
        <div
          style={{
            alignItems: "center",
            display: "flex",
            gap: 14,
            marginBottom: 20,
          }}
        >
          <img
            src={agentloomIcon}
            alt="AgentLoom"
            style={{ height: 48, width: 48 }}
          />
          <div>
            <h2 className="dialog__title" style={{ margin: 0 }}>
              AgentLoom
            </h2>
            <div
              role="button"
              tabIndex={0}
              onClick={handleCopyVersion}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  void handleCopyVersion();
                }
              }}
              title={t("aboutDialog.copyVersion")}
              style={{
                color: copied ? "var(--accent)" : "var(--ink-3)",
                cursor: "pointer",
                fontSize: 12,
                marginTop: 2,
                userSelect: "none",
                width: "fit-content",
              }}
            >
              {copied ? t("aboutDialog.copied") : `v${appVersion}`}
            </div>
          </div>
        </div>

        <div style={{ display: "flex", flexDirection: "column" }}>
          {rows.map((row) => (
            <div
              key={row.href}
              style={{
                alignItems: "center",
                display: "grid",
                gridTemplateColumns: "96px minmax(0, 1fr)",
                minHeight: 34,
              }}
            >
              <div style={{ color: "var(--ink-3)", fontSize: 12 }}>
                {row.label}
              </div>
              <div
                style={{
                  fontSize: 13,
                  minWidth: 0,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                <a
                  className="ob-link"
                  href={row.href}
                  onClick={(event) => openLink(event, row.href)}
                >
                  {row.text}
                </a>
              </div>
            </div>
          ))}
        </div>

        <div
          style={{
            borderTop: "1px solid var(--line-soft)",
            color: "var(--ink-3)",
            fontSize: 11,
            marginTop: 16,
            paddingTop: 12,
          }}
        >
          {t("aboutDialog.copyright")}
        </div>
      </div>
    </div>,
    document.body,
  );
}
