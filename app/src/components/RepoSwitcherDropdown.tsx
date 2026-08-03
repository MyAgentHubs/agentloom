import { useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { localProjectDisplayName, useI18n } from "../i18n";
import type { NamespaceMeta, RepoMeta, Session } from "../types/agent";
import { NamespaceAvatar } from "./NamespaceAvatar";

type Props = {
  open: boolean;
  namespaces: NamespaceMeta[];
  /** 所有 active repos（跨 namespace · 分组数据源 = list_repos 结果） */
  allRepos: RepoMeta[];
  sessions: Session[];
  activeNamespaceId: string;
  activeRepoId: string | null;
  /** 一步切：跨 namespace 原子切换（App.onSelectRepoInNamespace） */
  onSelectRepoInNamespace: (nsId: string, repoId: string) => void;
  onClose: () => void;
  onNewProject?: () => void;
  onEditRepo?: (repo: RepoMeta) => void;
  onManageRepos?: () => void;
};

/**
 * 导航 IA（spec §2.A.3）· 按 namespace/owner 分组的 repo 下拉。
 * - 「项目」组置顶（本地项目）+ 各 GitHub owner namespace 段（圆头像 + gh 角标）。
 * - 顶部 combined filter：过滤 repo 名 + namespace/owner 名（NamespaceDropdown 只搜 ns、RepoDropdown 只搜当前 ns repo·都不够·codex NIT9）。
 * - 当前 repo 行 .on + ✓。底部两入口：新建项目 / 管理 GitHub 仓库。
 * - 个人/组织 pill defer（spec §2.A.5·缺后端 user.type）。
 * - root class .repo-switcher（区别于 NamespaceDropdown 的 .dropdown·避共存撞）。outside-close 复用 NamespaceDropdown 同款 document mousedown + ref。
 */
export function RepoSwitcherDropdown({
  open,
  namespaces,
  allRepos,
  sessions,
  activeNamespaceId,
  activeRepoId,
  onSelectRepoInNamespace,
  onClose,
  onNewProject,
  onEditRepo,
  onManageRepos,
}: Props) {
  const { t } = useI18n();
  const panelRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (!open) return;
    function onMouseDown(e: MouseEvent) {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        onClose();
      }
    }
    document.addEventListener("mousedown", onMouseDown);
    return () => document.removeEventListener("mousedown", onMouseDown);
  }, [open, onClose]);

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  useEffect(() => {
    if (open) {
      setQuery("");
    }
  }, [open]);

  const reposByNs = useMemo(() => {
    const m = new Map<string, RepoMeta[]>();
    for (const r of allRepos) {
      const list = m.get(r.namespace_id) ?? [];
      list.push(r);
      m.set(r.namespace_id, list);
    }
    return m;
  }, [allRepos]);

  if (!open) return null;

  const q = query.trim().toLowerCase();
  // combined filter：section 名匹配 → 该段全部 repo；否则只留 repo 名匹配的。
  function visibleRepos(ns: NamespaceMeta): RepoMeta[] {
    const list = reposByNs.get(ns.id) ?? [];
    if (q === "") return list;
    if (ns.name.toLowerCase().includes(q)) return list;
    return list.filter((r) => r.name.toLowerCase().includes(q));
  }

  const local = namespaces.find((n) => n.kind === "local") ?? null;
  const orgs = namespaces.filter((n) => n.kind === "github_org");

  function pick(ns: NamespaceMeta, r: RepoMeta) {
    onSelectRepoInNamespace(ns.id, r.id);
    onClose();
  }

  function renderSection(ns: NamespaceMeta) {
    const repos = visibleRepos(ns);
    if (repos.length === 0) return null;
    return (
      <div key={ns.id} className="rsw-group">
        <div className="dd-sec">
          {ns.kind === "local" ? (
            <span className="dd-sec-nm">{t("repoSwitcher.projectsGroup")}</span>
          ) : (
            <>
              <NamespaceAvatar namespace={ns} size={17} />
              <span className="dd-sec-nm">{ns.name}</span>
            </>
          )}
        </div>
        {repos.map((r) => {
          const active = r.id === activeRepoId && ns.id === activeNamespaceId;
          const sessionCount = sessions.filter(
            (session) => session.repo_id === r.id && !session.archived,
          ).length;
          return (
            <div
              key={r.id}
              className={`dd-row${active ? " on" : ""}`}
              role="button"
              tabIndex={0}
              onClick={() => pick(ns, r)}
            >
              {ns.kind === "local" || r.icon ? (
                <span className="project-icon" aria-hidden="true">
                  {r.icon || "📁"}
                </span>
              ) : (
                <NamespaceAvatar namespace={ns} size={22} />
              )}
              <span className="dd-row-nm">{localProjectDisplayName(r, t)}</span>
              {sessionCount > 0 && (
                <span className="dd-row-count">
                  {t("projectSwitcher.sessionCount", { n: sessionCount })}
                </span>
              )}
              <button
                type="button"
                className="repo-switcher__edit"
                aria-label={t("repoSwitcher.editProject")}
                title={t("repoSwitcher.editProject")}
                onClick={(e) => {
                  e.stopPropagation();
                  onEditRepo?.(r);
                }}
              >
                ✎
              </button>
              {active && <span className="ck">✓</span>}
            </div>
          );
        })}
      </div>
    );
  }

  // codex P2：先 filter(Boolean) 再按 index 插 divider·避 local 为 null 时前置 divider。
  const orderedNs = local ? [local, ...orgs] : orgs;
  const visibleSections = orderedNs
    .map((ns) => ({ ns, node: renderSection(ns) }))
    .filter(
      (s): s is { ns: NamespaceMeta; node: ReactElement } => s.node !== null,
    );
  const sections: ReactElement[] = [];
  visibleSections.forEach((s, i) => {
    if (i > 0) sections.push(<div key={`d${s.ns.id}`} className="dd-div" />);
    sections.push(s.node);
  });
  const hasAnyRepo = visibleSections.length > 0;

  return (
    <div className="repo-switcher" ref={panelRef}>
      <div className="rsw-search">
        <input
          placeholder={t("repoSwitcher.searchPlaceholder")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>
      {hasAnyRepo ? (
        sections
      ) : (
        <div className="rsw-empty">{t("repoSwitcher.empty")}</div>
      )}
      <div className="dd-div" />
      <div
        className="dd-foot"
        role="button"
        tabIndex={0}
        onClick={() => onNewProject?.()}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") onNewProject?.();
        }}
      >
        <span className="dd-foot__ic">＋</span>
        {t("repoSwitcher.newProject")}
      </div>
      <div
        className="dd-foot muted"
        role="button"
        tabIndex={0}
        onClick={() => onManageRepos?.()}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") onManageRepos?.();
        }}
      >
        {t("repoSwitcher.manageRepos")}
      </div>
    </div>
  );
}
