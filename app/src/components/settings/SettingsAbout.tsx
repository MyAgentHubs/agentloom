import { openUrl } from "@tauri-apps/plugin-opener";
import type { CSSProperties, MouseEvent } from "react";
import { useI18n } from "../../i18n";
import {
  COPYRIGHT,
  GITHUB_ISSUES_LABEL,
  GITHUB_ISSUES_URL,
  SUPPORT_EMAIL,
  WEBSITE_LABEL,
  WEBSITE_URL,
} from "../../constants/about";

const styles: Record<string, CSSProperties> = {
  header: {
    marginBottom: 18,
  },
  version: {
    color: "var(--ink-3)",
    fontSize: 12,
    marginTop: 4,
  },
  card: {
    display: "flex",
    flexDirection: "column",
    gap: 0,
    marginTop: 24,
  },
  row: {
    alignItems: "center",
    display: "grid",
    gridTemplateColumns: "96px minmax(0, 1fr)",
    minHeight: 40,
  },
  label: {
    margin: 0,
  },
  value: {
    color: "var(--ink-2)",
    fontSize: 13,
  },
  copyright: {
    borderTop: "1px solid var(--line-soft)",
    color: "var(--ink-3)",
    fontSize: 11,
    marginTop: 28,
    paddingTop: 14,
  },
};

export function SettingsAbout() {
  const { t } = useI18n();
  const appVersion =
    typeof __APP_VERSION__ === "undefined" ? "dev" : __APP_VERSION__;

  const openLink = (event: MouseEvent<HTMLAnchorElement>, url: string) => {
    event.preventDefault();
    void openUrl(url);
  };

  return (
    <div className="st-lang">
      <div className="st-lang__head" style={styles.header}>
        <h2>AgentLoom</h2>
        <div style={styles.version}>v{appVersion}</div>
      </div>
      <div style={styles.card}>
        <div style={styles.row}>
          <div className="st-lang__label" style={styles.label}>
            {t("settings.about.support")}
          </div>
          <div style={styles.value}>
            <a
              className="ob-link"
              href={`mailto:${SUPPORT_EMAIL}`}
              onClick={(event) => openLink(event, `mailto:${SUPPORT_EMAIL}`)}
            >
              {SUPPORT_EMAIL}
            </a>
          </div>
        </div>
        <div style={styles.row}>
          <div className="st-lang__label" style={styles.label}>
            {t("settings.about.feedback")}
          </div>
          <div style={styles.value}>
            <a
              className="ob-link"
              href={GITHUB_ISSUES_URL}
              onClick={(event) => openLink(event, GITHUB_ISSUES_URL)}
            >
              {GITHUB_ISSUES_LABEL}
            </a>
          </div>
        </div>
        <div style={styles.row}>
          <div className="st-lang__label" style={styles.label}>
            {t("settings.about.website")}
          </div>
          <div style={styles.value}>
            <a
              className="ob-link"
              href={WEBSITE_URL}
              onClick={(event) => openLink(event, WEBSITE_URL)}
            >
              {WEBSITE_LABEL}
            </a>
          </div>
        </div>
        <div style={styles.copyright}>{COPYRIGHT}</div>
      </div>
    </div>
  );
}
