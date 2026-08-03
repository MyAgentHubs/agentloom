import { RightPanelTabs, type RightPanelTab } from "./RightPanelTabs";
import { GoalBar } from "./GoalBar";
import type { GoalContract } from "../types/agent";
import { useI18n } from "../i18n";

type Props = {
  view: "overview" | "session" | "intro";
  sidebarCollapsed: boolean;
  onToggleSidebar?: () => void;
  onHome?: () => void; // 收起态总览入口（左栏 sb-top 总览被收掉时归位到此）
  canGoBack?: boolean;
  canGoForward?: boolean;
  onBack?: () => void;
  onForward?: () => void;
  // 注：repoName/onSessionMenu 本刀 session 视图不再渲染（目标条替代）·暂留兼容·待后续清理
  // sessionTitle/status：无 goal 时 topbar 兜底显会话标题（+ 运行中 spinner）
  sessionTitle?: string;
  repoName?: string;
  status?: "idle" | "working";
  onSessionMenu?: () => void;
  // 非 session 视图轻量 context（如 "总览 · acme" / "web · 项目简介"）
  contextLabel?: string;
  // 右面板 tabs（搬入·常驻所有视图；最大化控件常驻所有视图）
  rightPanelOpen: boolean;
  rightPanelTab: RightPanelTab | null;
  previewPath?: string | null;
  tabBeforePreview?: RightPanelTab | null;
  rightPanelExpanded: boolean;
  reviewBadge?: number;
  onTab: (t: RightPanelTab | null) => void;
  onExpand: () => void;
  onUserCollapse: () => void;
  onExpandPanel: () => void;
  onRestorePanel: () => void;
  // 目标条（②a·从 SessionMain 挪入·session 视图 __main）
  goal?: GoalContract | null;
  goalExpanded?: boolean;
  onToggleGoal?: () => void;
  goalPanel?: import("react").ReactNode;
  goalRunComplete?: boolean;
  goalRunHasMemberFailure?: boolean;
  goalRunning?: boolean;
  orchestratedTaskCount?: number;
  orchestratedAnyRunning?: boolean;
  onOpenTaskList?: () => void;
};

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

/**
 * surface 自带 header（spec §2.C / §2.I）。
 * 左段(.sf-head__main)：session→目标条(GoalBar)；非 session→轻量 context。
 * 右：RightPanelTabs 常驻所有视图；最大化控件常驻所有视图。
 * 左栏收起：sf-head 加 .inset（让位真红绿灯）+ 左侧 .sf-collapsed-ctrls（折叠/← →/总览）。
 * P2: repo/project switching remains in the expanded sidebar footer.
 */
export function SurfaceHeader(props: Props) {
  const isSession = props.view === "session";
  const { t } = useI18n();
  const openTabs: RightPanelTab[] = [];
  if (props.previewPath && props.tabBeforePreview) {
    openTabs.push(props.tabBeforePreview);
  }
  if (props.rightPanelTab !== null && !openTabs.includes(props.rightPanelTab)) {
    openTabs.push(props.rightPanelTab);
  }
  if (props.previewPath && !openTabs.includes("preview")) {
    openTabs.push("preview");
  }
  // ③已接 lead 产 goal_title·重启用——topbar 有 goal 时渲 GoalBar（优先显 goal_title·fallback goal.goal）。
  const ctlMod = props.rightPanelExpanded
    ? " sf-head__ctl--max"
    : props.rightPanelOpen
      ? " sf-head__ctl--open"
      : "";
  return (
    <div className={`sf-head${props.sidebarCollapsed ? " inset" : ""}`}>
      <div
        className={`sf-head__main${props.rightPanelExpanded ? " sf-head__main--compact" : ""}`}
      >
        {props.sidebarCollapsed && (
          <div className="sf-collapsed-ctrls">
            <button
              className="iconbtn"
              aria-label={t("surfaceHeader.expandSidebar")}
              title={t("surfaceHeader.expandSidebar")}
              onClick={props.onToggleSidebar}
            >
              <svg {...ic}>
                <rect x="3" y="3" width="18" height="18" rx="2" />
                <path d="M9 3v18" />
              </svg>
            </button>
            <button
              className="iconbtn"
              aria-label={t("surfaceHeader.back")}
              title={t("surfaceHeader.back")}
              disabled={!props.canGoBack}
              onClick={props.onBack}
            >
              <svg {...ic}>
                <path d="M15 18l-6-6 6-6" />
              </svg>
            </button>
            <button
              className="iconbtn"
              aria-label={t("surfaceHeader.forward")}
              title={t("surfaceHeader.forward")}
              disabled={!props.canGoForward}
              onClick={props.onForward}
            >
              <svg {...ic}>
                <path d="M9 18l6-6-6-6" />
              </svg>
            </button>
            <button
              className="iconbtn"
              aria-label={t("surfaceHeader.overview")}
              title={t("surfaceHeader.overviewTitle")}
              onClick={props.onHome}
            >
              <svg {...ic}>
                <path d="M3 12l9-9 9 9M5 10v10h14V10" />
              </svg>
            </button>
          </div>
        )}
        <div className="sf-head__main-content">
          {isSession ? (
            props.goal && props.goal.goal && !props.rightPanelExpanded ? (
              <GoalBar
                topbar
                goal={props.goal}
                expanded={props.goalExpanded ?? false}
                onToggle={props.onToggleGoal ?? (() => {})}
                expandedSlot={props.goalPanel ?? null}
                running={props.goalRunning}
                runComplete={props.goalRunComplete}
                runHasMemberFailure={props.goalRunHasMemberFailure}
              />
            ) : !props.rightPanelExpanded && props.sessionTitle ? (
              // Solo 会话无 goal（Team goal 契约不存在）时兜底：安静显会话标题，运行中带 spinner
              <div className="sf-session-title">
                {props.status === "working" && (
                  <span className="sf-session-title__spin" aria-hidden>
                    <svg viewBox="0 0 24 24">
                      <path d="M12 3a9 9 0 109 9" />
                    </svg>
                  </span>
                )}
                <span className="sf-session-title__text">
                  {props.sessionTitle}
                </span>
              </div>
            ) : null
          ) : props.rightPanelExpanded ? null : (
            <div className="sf-ctx">
              <span>{props.contextLabel ?? ""}</span>
            </div>
          )}
        </div>
      </div>
      <div className={`sf-head__ctl${ctlMod}`}>
        <div
          className={`sf-tabs${props.rightPanelExpanded ? " expanded" : ""}`}
        >
          {(props.orchestratedTaskCount ?? 0) > 0 && (
            <button
              type="button"
              className="taskbtn"
              title={t("topbar.tasks.count", {
                n: props.orchestratedTaskCount ?? 0,
              })}
              aria-label={t("topbar.tasks.view")}
              onClick={props.onOpenTaskList}
            >
              <svg
                viewBox="0 0 24 24"
                width={15}
                height={15}
                fill="none"
                stroke="currentColor"
                strokeWidth={1.8}
              >
                <rect x="3" y="4" width="7" height="7" rx="1.5" />
                <rect x="14" y="4" width="7" height="7" rx="1.5" />
                <rect x="3" y="14" width="7" height="7" rx="1.5" />
                <path d="M14 17.5h7M17.5 14v7" />
              </svg>
              <span className="taskbtn__ct">{props.orchestratedTaskCount}</span>
              {props.orchestratedAnyRunning && (
                <span className="taskbtn__dot" aria-hidden />
              )}
            </button>
          )}
          <RightPanelTabs
            open={props.rightPanelOpen}
            tab={props.rightPanelTab}
            openTabs={openTabs}
            expanded={props.rightPanelExpanded}
            canMaximize={true}
            reviewBadge={props.reviewBadge}
            onTab={props.onTab}
            onExpand={props.onExpand}
            onUserCollapse={props.onUserCollapse}
            onExpandPanel={props.onExpandPanel}
            onRestorePanel={props.onRestorePanel}
          />
        </div>
      </div>
    </div>
  );
}
