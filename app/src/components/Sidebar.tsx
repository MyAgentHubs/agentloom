import React from "react";
import { useEffect, useRef, useState } from "react";
import type {
  GroupMeta,
  Session,
  RepoMeta,
  NamespaceMeta,
} from "../types/agent";
import { useI18n } from "../i18n";
import { ProjectSwitcherFooter } from "./ProjectSwitcherFooter";
import { SessionGroupSection } from "./SessionGroupSection";
import { SessionRow, type SessionDotStatus } from "./SessionRow";

type Props = {
  sessions: Session[];
  currentId: string | null;
  busy: boolean;
  runningSessionIds?: ReadonlySet<string>;
  /** 左栏行状态点三态：切走后仍知 agent 死活（running 时以 runningSessionIds 为准·此 map 只需 attention/done） */
  sessionStatusById?: ReadonlyMap<string, SessionDotStatus>;
  continuationReadySessionIds?: ReadonlySet<string>;
  activeMenu: "intro" | "session";
  settingsActive?: boolean;
  activeNamespace: NamespaceMeta | null;
  activeRepo: RepoMeta | null;
  namespaces: NamespaceMeta[];
  allRepos: RepoMeta[];
  activeRepoId: string | null;
  reposInActiveNs: RepoMeta[];
  repoGroupExpanded: Record<string, boolean>;
  /** B3：0 repo namespace 时 disable「+ 新会话」+ hover tip */
  newDisabled: boolean;
  onSelect: (id: string) => void;
  onNew: () => void;
  onRequestDelete: (session: Session) => void;
  /** session-hover-menu：动作回调（可选·默认 no-op·上游 Task 7 接线） */
  activeNamespaceId?: string | null;
  onRename?: (id: string, title: string) => void;
  onTogglePin?: (id: string, next: boolean) => void;
  onToggleUnread?: (id: string, next: boolean) => void;
  onToggleArchive?: (id: string, next: boolean) => void;
  onHandover?: (id: string) => void;
  handoverAssemblingId?: string | null;
  /** session 分组：repo 级分组渲染 + CRUD + 移动 */
  groups?: GroupMeta[];
  groupExpanded?: Record<string, boolean>;
  onToggleGroup?: (id: string) => void;
  onCreateGroup?: (name: string) => Promise<string> | void;
  onMoveSessionToGroup?: (sessionId: string, groupId: string | null) => void;
  onRenameGroup?: (id: string, name: string) => void;
  onRequestDeleteGroup?: (g: GroupMeta) => void;
  onMenuIntro: () => void;
  onMenuAgents?: () => void;
  onToggleRepoGroup: (repoId: string) => void;
  onSelectRepoInNamespace: (nsId: string, repoId: string) => void;
  onNewProject?: () => void;
  onEditRepo?: (repo: RepoMeta) => void;
  onManageRepos?: () => void;
  canGoBack?: boolean;
  canGoForward?: boolean;
  onBack?: () => void;
  onForward?: () => void;
  // 阶段1 Task1.3：全高列 chrome
  onToggleSidebar?: () => void;
  onHome?: () => void; // 总览入口（删 TopBar 后归位·不能 backlog·既有 App.test 断言「总览」）
};

const emptyRunningSessionIds = new Set<string>();
const emptySessionStatusById = new Map<string, SessionDotStatus>();

const introIcon = (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={2}
    strokeLinecap="round"
    strokeLinejoin="round"
    width={14}
    height={14}
  >
    <path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z" />
    <path d="M14 2v6h6" />
  </svg>
);

function arrangeContinuationThreads(list: Session[]): Session[] {
  const byId = new Map(list.map((s) => [s.id, s]));
  const childrenByParent = continuationChildrenByParent(list);
  const emitted = new Set<string>();
  const arranged: Session[] = [];

  function emitThread(root: Session) {
    let current: Session | undefined = root;
    while (current && !emitted.has(current.id)) {
      arranged.push(current);
      emitted.add(current.id);
      const pointedChild: Session | undefined = current.continued_to_session_id
        ? byId.get(current.continued_to_session_id)
        : undefined;
      const fallbackChildren: Session[] =
        childrenByParent.get(current.id) ?? [];
      current =
        pointedChild ??
        (fallbackChildren.length === 1 ? fallbackChildren[0] : undefined);
    }
  }

  for (const session of list) {
    if (emitted.has(session.id)) continue;
    if (session.parent_session_id && byId.has(session.parent_session_id))
      continue;
    emitThread(session);
  }
  for (const session of list) {
    if (!emitted.has(session.id)) emitThread(session);
  }
  return arranged;
}

function continuationChildrenByParent(list: Session[]): Map<string, Session[]> {
  const childrenByParent = new Map<string, Session[]>();
  for (const session of list) {
    if (!session.parent_session_id) continue;
    const children = childrenByParent.get(session.parent_session_id) ?? [];
    children.push(session);
    childrenByParent.set(session.parent_session_id, children);
  }
  return childrenByParent;
}

function sessionHasContinuationThread(
  session: Session,
  childrenByParent: Map<string, Session[]>,
): boolean {
  return (
    session.parent_session_id !== null ||
    session.continued_to_session_id !== null ||
    (childrenByParent.get(session.id)?.length ?? 0) > 0
  );
}

export const Sidebar = React.memo(function Sidebar({
  sessions,
  currentId,
  runningSessionIds = emptyRunningSessionIds,
  sessionStatusById = emptySessionStatusById,
  continuationReadySessionIds = emptyRunningSessionIds,
  activeMenu,
  settingsActive = false,
  activeNamespace,
  activeRepo,
  namespaces,
  allRepos,
  activeRepoId,
  newDisabled,
  onSelect,
  onNew,
  onRequestDelete,
  onMenuIntro,
  onRename = () => {},
  onTogglePin = () => {},
  onToggleUnread = () => {},
  onToggleArchive = () => {},
  onHandover = undefined,
  handoverAssemblingId = null,
  groups = [] as GroupMeta[],
  groupExpanded = {} as Record<string, boolean>,
  onToggleGroup = undefined,
  onCreateGroup = undefined,
  onMoveSessionToGroup = undefined,
  onRenameGroup = undefined,
  onRequestDeleteGroup = undefined,
  onMenuAgents = () => {},
  activeNamespaceId,
  onSelectRepoInNamespace,
  onNewProject,
  onEditRepo,
  onManageRepos,
  canGoBack = false,
  canGoForward = false,
  onBack,
  onForward,
  onToggleSidebar,
  onHome,
}: Props) {
  const { t } = useI18n();
  const [archOpen, setArchOpen] = useState(false);
  const [newGrouping, setNewGrouping] = useState(false);
  const [newGroupName, setNewGroupName] = useState("");
  const [, setRelativeTimeTick] = useState(0);
  const newGroupInputRef = useRef<HTMLInputElement>(null);
  // 会话栏可拖宽（默认 230·夹 [200,360]·localStorage 持久）
  const SIDEBAR_MIN = 200;
  const SIDEBAR_MAX = 360;
  const SIDEBAR_DEFAULT = 230;
  const [sidebarWidth, setSidebarWidth] = useState<number>(() => {
    try {
      const raw = localStorage.getItem("agentloom.sidebarWidth");
      const n = raw ? parseInt(raw, 10) : NaN;
      if (!Number.isNaN(n))
        return Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, n));
    } catch {
      /* localStorage 不可用·用默认 */
    }
    return SIDEBAR_DEFAULT;
  });
  const startSidebarResize = (e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = sidebarWidth;
    const onMove = (ev: MouseEvent) => {
      const next = Math.min(
        SIDEBAR_MAX,
        Math.max(SIDEBAR_MIN, startW + (ev.clientX - startX)),
      );
      setSidebarWidth(next);
    };
    const onUp = (ev: MouseEvent) => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      const final = Math.min(
        SIDEBAR_MAX,
        Math.max(SIDEBAR_MIN, startW + (ev.clientX - startX)),
      );
      try {
        localStorage.setItem("agentloom.sidebarWidth", String(final));
      } catch {
        /* 持久化失败·静默 */
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };
  const addTitle = newDisabled
    ? t("sidebar.newSessionDisabledTitle")
    : t("sidebar.newSessionTitle");
  const ic = {
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    width: 18,
    height: 18,
  };

  useEffect(() => {
    if (newGrouping) {
      const el = newGroupInputRef.current;
      if (el) {
        el.focus();
        el.select();
      }
    }
  }, [newGrouping]);

  useEffect(() => {
    const intervalId = window.setInterval(() => {
      setRelativeTimeTick((tick) => tick + 1);
    }, 60_000);
    return () => window.clearInterval(intervalId);
  }, []);

  function submitNewGroup() {
    const name = newGroupName.trim();
    if (name) void onCreateGroup?.(name);
    setNewGroupName("");
    setNewGrouping(false);
  }

  function cancelNewGroup() {
    setNewGroupName("");
    setNewGrouping(false);
  }

  const continuationChildren = continuationChildrenByParent(sessions);

  function renderSess(s: Session) {
    const active = activeMenu === "session" && s.id === currentId;
    const parentTitle =
      s.parent_session_id != null
        ? (sessions.find((candidate) => candidate.id === s.parent_session_id)
            ?.title ?? null)
        : null;
    const running = runningSessionIds.has(s.id);
    // running 恒以 runningSessionIds 为准（贯通已在）；done/attention 来自 sessionStatusById——
    // 当前打开的会话（active）不显 done/attention（切进去即清·App 层已清该 map 项·这里再兜一层不靠时序）。
    const dotStatus: SessionDotStatus | null = running
      ? "running"
      : active
        ? null
        : (sessionStatusById.get(s.id) ??
          (continuationReadySessionIds.has(s.id) ? "done" : null));
    return (
      <SessionRow
        key={s.id}
        session={s}
        active={active}
        running={running}
        dotStatus={dotStatus}
        isArchived={s.archived}
        onSelect={onSelect}
        onRename={onRename}
        onRequestDelete={onRequestDelete}
        onTogglePin={onTogglePin}
        onToggleUnread={onToggleUnread}
        onToggleArchive={onToggleArchive}
        onHandover={onHandover}
        handoverBusy={handoverAssemblingId === s.id}
        parentTitle={parentTitle}
        hasContinuationThread={sessionHasContinuationThread(
          s,
          continuationChildren,
        )}
        groups={groups}
        onMoveSessionToGroup={onMoveSessionToGroup}
        onCreateGroup={onCreateGroup}
      />
    );
  }

  // session-hover-menu：活动列表（已按 pinned DESC 排）在置顶组与非置顶组之间插一条分隔线。
  // 仅当既有置顶又有非置顶时插（全置顶 / 全非置顶不插）。
  function renderSessList(list: Session[]) {
    const firstUnpinned = list.findIndex((s) => !s.pinned);
    return list.flatMap((s, i) =>
      i === firstUnpinned && firstUnpinned > 0
        ? [<div key="__pin-div" className="sb-pin-div" />, renderSess(s)]
        : [renderSess(s)],
    );
  }

  return (
    <aside
      className="sidebar"
      style={{ flexBasis: `${sidebarWidth}px`, width: `${sidebarWidth}px` }}
    >
      <div className="sb-top">
        <button
          type="button"
          className="iconbtn"
          aria-label={t("sidebar.collapse")}
          title={t("sidebar.collapse")}
          onClick={onToggleSidebar}
        >
          <svg {...ic}>
            <rect x="3" y="3" width="18" height="18" rx="2" />
            <path d="M9 3v18" />
          </svg>
        </button>
        <button
          type="button"
          className="iconbtn"
          aria-label={t("sidebar.back")}
          title={t("sidebar.back")}
          disabled={!canGoBack}
          onClick={onBack}
        >
          <svg {...ic}>
            <path d="M15 18l-6-6 6-6" />
          </svg>
        </button>
        <button
          type="button"
          className="iconbtn"
          aria-label={t("sidebar.forward")}
          title={t("sidebar.forward")}
          disabled={!canGoForward}
          onClick={onForward}
        >
          <svg {...ic}>
            <path d="M9 18l6-6-6-6" />
          </svg>
        </button>
        <button
          type="button"
          className="iconbtn"
          aria-label={t("sidebar.overview")}
          title={t("sidebar.overviewTitle")}
          onClick={onHome}
        >
          <svg {...ic}>
            <path d="M3 12l9-9 9 9M5 10v10h14V10" />
          </svg>
        </button>
        <button
          type="button"
          className="iconbtn"
          aria-label={t("sidebar.search")}
          title={t("sidebar.searchTitle")}
          disabled
        >
          <svg {...ic}>
            <circle cx="11" cy="11" r="7" />
            <path d="M21 21l-4.3-4.3" />
          </svg>
        </button>
      </div>
      <div className="sessions__scroll">
        <div
          className={`menu-item${activeMenu === "intro" ? " active" : ""}`}
          onClick={onMenuIntro}
        >
          {introIcon}
          <span>{t("sidebar.projectIntro")}</span>
        </div>
        <div className="sb-div" />
        <div className="sb-grp">
          <span>{t("sidebar.sessions")}</span>
          <button
            type="button"
            className="sb-grp__add"
            disabled={newDisabled}
            title={addTitle}
            onClick={onNew}
          >
            {t("sidebar.newSession")}
          </button>
        </div>
        {renderSessList(
          arrangeContinuationThreads(
            sessions.filter(
              (s) =>
                !s.archived &&
                (s.repo_id ?? null) === activeRepoId &&
                !s.group_id,
            ),
          ),
        )}
        {groups.map((g) => (
          <SessionGroupSection
            key={g.id}
            group={g}
            sessions={arrangeContinuationThreads(
              sessions.filter((s) => !s.archived && s.group_id === g.id),
            )}
            expanded={groupExpanded[g.id] ?? true}
            onToggle={() => onToggleGroup?.(g.id)}
            onRename={onRenameGroup}
            onRequestDelete={onRequestDeleteGroup}
            renderSess={renderSess}
          />
        ))}
        {newGrouping ? (
          <div className="sb-new-group-input">
            <input
              ref={newGroupInputRef}
              type="text"
              value={newGroupName}
              placeholder={t("sidebar.groupNamePlaceholder")}
              onChange={(e) => setNewGroupName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.nativeEvent.isComposing)
                  submitNewGroup();
                else if (e.key === "Escape") cancelNewGroup();
              }}
              onBlur={cancelNewGroup}
            />
          </div>
        ) : (
          <button
            type="button"
            data-action="new-group"
            className="sb-new-group"
            onClick={() => setNewGrouping(true)}
          >
            {t("sidebar.newGroup")}
          </button>
        )}
        {(() => {
          const archived = arrangeContinuationThreads(
            sessions.filter((s) => {
              if (!s.archived) return false;
              return (s.repo_id ?? null) === activeRepoId;
            }),
          );
          if (archived.length === 0) return null;
          return (
            <>
              {/* 系统归档区与上方活跃内容（会话 / 用户分组）划开 · 复用 .sb-div 细线 */}
              <div className="sb-div" />
              <div className={`sb-arch${archOpen ? "" : " collapsed"}`}>
                <div
                  className="sb-arch__head"
                  onClick={() => setArchOpen((v) => !v)}
                >
                  <span className="sb-arch__chev">▾</span>
                  <span>{t("sidebar.archived", { n: archived.length })}</span>
                </div>
                <div className="sb-arch__body">{archived.map(renderSess)}</div>
              </div>
            </>
          );
        })()}
      </div>
      <div className="sb-foot">
        <ProjectSwitcherFooter
          activeNamespace={activeNamespace}
          activeRepo={activeRepo}
          namespaces={namespaces}
          allRepos={allRepos}
          sessions={sessions}
          activeNamespaceId={activeNamespaceId ?? "local"}
          activeRepoId={activeRepoId}
          onSelectRepoInNamespace={onSelectRepoInNamespace}
          onNewProject={onNewProject}
          onEditRepo={onEditRepo}
          onManageRepos={onManageRepos}
          onSettings={onMenuAgents}
          settingsActive={settingsActive}
        />
      </div>
      <div
        className="sidebar__resizer"
        onMouseDown={startSidebarResize}
        role="separator"
        aria-orientation="vertical"
        aria-label={t("sidebar.resize")}
      />
    </aside>
  );
});
