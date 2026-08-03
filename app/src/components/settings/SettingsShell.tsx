import type { ReactNode } from "react";
import { useI18n, type I18nKey } from "../../i18n";

export type SettingsPage =
  | "agents"
  | "repos"
  | "archivedProjects"
  | "language"
  | "search"
  | "about";

type NavKey =
  | "agents"
  | "search"
  | "language"
  | "defaults"
  | "repos"
  | "archivedProjects"
  | "allowlist"
  | "accounts"
  | "budget"
  | "shortcuts"
  | "about";

const ICONS: Record<NavKey, ReactNode> = {
  agents: (
    <path d="M12 2a4 4 0 100 8 4 4 0 000-8zM5 21v-2a4 4 0 014-4h6a4 4 0 014 4v2" />
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="7" />
      <path d="M21 21l-4.3-4.3" />
    </>
  ),
  language: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18M12 3a14 14 0 010 18M12 3a14 14 0 000 18" />
    </>
  ),
  defaults: (
    <>
      <path d="M12 2v4M12 18v4M2 12h4M18 12h4" />
      <circle cx="12" cy="12" r="4" />
    </>
  ),
  repos: (
    <>
      <path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z" />
      <circle cx="15.5" cy="13.5" r="2" />
      <path d="M15.5 9v2.5" />
    </>
  ),
  archivedProjects: (
    <>
      <path d="M4 7h16v13H4zM3 3h18v4H3z" />
      <path d="M9 11h6" />
    </>
  ),
  allowlist: (
    <>
      <rect x="3" y="11" width="18" height="11" rx="2" />
      <path d="M7 11V7a5 5 0 0110 0v4" />
    </>
  ),
  accounts: (
    <>
      <circle cx="12" cy="8" r="5" />
      <path d="M3 21v-1a7 7 0 0114 0v1" />
    </>
  ),
  budget: <path d="M12 1v22M17 5H9.5a3.5 3.5 0 000 7h5a3.5 3.5 0 010 7H6" />,
  shortcuts: (
    <>
      <rect x="2" y="4" width="20" height="16" rx="2" />
      <path d="M6 8h.01M10 8h.01M7 12h10M8 16h8" />
    </>
  ),
  about: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v6M12 7h.01" />
    </>
  ),
};

const NAV: {
  key: NavKey;
  labelKey: I18nKey;
  enabled: boolean;
}[] = [
  {
    key: "agents",
    labelKey: "settings.nav.agents",
    enabled: true,
  },
  {
    key: "search",
    labelKey: "settings.nav.search",
    enabled: true,
  },
  {
    key: "language",
    labelKey: "settings.nav.language",
    enabled: true,
  },
  {
    key: "defaults",
    labelKey: "settings.nav.defaults",
    enabled: false,
  },
  {
    key: "repos",
    labelKey: "settings.nav.repos",
    enabled: true,
  },
  {
    key: "archivedProjects",
    labelKey: "settings.nav.archivedProjects",
    enabled: true,
  },
  {
    key: "allowlist",
    labelKey: "settings.nav.allowlist",
    enabled: false,
  },
  {
    key: "accounts",
    labelKey: "settings.nav.accounts",
    enabled: false,
  },
  {
    key: "budget",
    labelKey: "settings.nav.budget",
    enabled: false,
  },
  {
    key: "shortcuts",
    labelKey: "settings.nav.shortcuts",
    enabled: false,
  },
  {
    key: "about",
    labelKey: "settings.nav.about",
    enabled: true,
  },
];

export function SettingsShell({
  activeKey,
  onNavigate,
  children,
}: {
  activeKey: SettingsPage;
  onNavigate?: (key: SettingsPage) => void;
  children: ReactNode;
}) {
  const { t } = useI18n();
  const visibleNav = NAV.filter((item) => item.enabled);

  return (
    <div className="st-app">
      <div className="st-nav">
        <div className="st-nav-title">{t("settings.title")}</div>
        {visibleNav.map((item) => (
          <button
            key={item.key}
            type="button"
            className={`st-nav-item${item.key === activeKey ? " active" : ""}`}
            aria-disabled={!item.enabled}
            aria-current={item.key === activeKey ? "page" : undefined}
            tabIndex={item.enabled ? undefined : -1}
            onClick={() => {
              if (!item.enabled) return;
              onNavigate?.(item.key as SettingsPage);
            }}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth={2}
            >
              {ICONS[item.key]}
            </svg>
            {t(item.labelKey)}
          </button>
        ))}
      </div>
      <div className={`st-content${activeKey === "repos" ? " repo" : ""}`}>
        {children}
      </div>
    </div>
  );
}
