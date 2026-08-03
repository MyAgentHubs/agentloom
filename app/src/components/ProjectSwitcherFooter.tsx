import { useState, type MouseEvent } from "react";
import { localProjectDisplayName, useI18n } from "../i18n";
import type { NamespaceMeta, RepoMeta, Session } from "../types/agent";
import { NamespaceAvatar } from "./NamespaceAvatar";
import { RepoSwitcherDropdown } from "./RepoSwitcherDropdown";

type Props = {
  activeNamespace: NamespaceMeta | null;
  activeRepo: RepoMeta | null;
  namespaces: NamespaceMeta[];
  allRepos: RepoMeta[];
  sessions: Session[];
  activeNamespaceId: string;
  activeRepoId: string | null;
  onSelectRepoInNamespace: (nsId: string, repoId: string) => void;
  onNewProject?: () => void;
  onEditRepo?: (repo: RepoMeta) => void;
  onManageRepos?: () => void;
  onSettings?: () => void;
  settingsActive?: boolean;
};

export function ProjectSwitcherFooter({
  activeNamespace,
  activeRepo,
  namespaces,
  allRepos,
  sessions,
  activeNamespaceId,
  activeRepoId,
  onSelectRepoInNamespace,
  onNewProject,
  onEditRepo,
  onManageRepos,
  onSettings,
  settingsActive = false,
}: Props) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const label = !activeRepo
    ? t("projectSwitcher.selectProject")
    : localProjectDisplayName(activeRepo, t);
  const isLocalProject =
    activeRepo !== null &&
    (activeNamespace?.kind === "local" || activeRepo.source === "local");
  const stopMouseDown = (event: MouseEvent) => event.stopPropagation();

  return (
    <div className={`project-switcher${open ? " open" : ""}`}>
      <div className="project-switcher__row">
        <button
          type="button"
          className="projsw"
          aria-label={t("projectSwitcher.ariaLabel")}
          aria-expanded={open}
          onMouseDown={stopMouseDown}
          onClick={() => setOpen((v) => !v)}
        >
          {isLocalProject ? (
            <span
              className="project-icon"
              data-testid="local-project-icon"
              aria-hidden="true"
            >
              {activeRepo.icon || "📁"}
            </span>
          ) : (
            <NamespaceAvatar namespace={activeNamespace} size={24} />
          )}
          <span className="projsw__name">{label}</span>
          <span className="projsw__chev" aria-hidden="true">
            {open ? "▴" : "▾"}
          </span>
        </button>
        <button
          type="button"
          className={`project-switcher__gear${settingsActive ? " active" : ""}`}
          aria-label={t("projectSwitcher.settings")}
          title={t("projectSwitcher.settings")}
          onClick={onSettings}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={1.8}
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>
      <RepoSwitcherDropdown
        open={open}
        namespaces={namespaces}
        allRepos={allRepos}
        sessions={sessions}
        activeNamespaceId={activeNamespaceId}
        activeRepoId={activeRepoId}
        onSelectRepoInNamespace={onSelectRepoInNamespace}
        onClose={() => setOpen(false)}
        onNewProject={() => {
          setOpen(false);
          onNewProject?.();
        }}
        onEditRepo={(repo) => {
          setOpen(false);
          onEditRepo?.(repo);
        }}
        onManageRepos={onManageRepos}
      />
    </div>
  );
}
