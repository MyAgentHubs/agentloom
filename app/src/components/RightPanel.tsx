import type { ReactElement } from "react";
import { FilesPanel } from "./FilesPanel";
import { MemberDrillIn, type DrillProps } from "./MemberDrillIn";
import { PreviewPanel } from "./PreviewPanel";
import { ReviewPanel } from "./ReviewPanel";
import { UndoReviewPanel } from "./UndoReviewPanel";
import { type RightPanelTab } from "./RightPanelTabs";
import { TaskInspector } from "./TaskInspector";
import { TaskList } from "./TaskList";
import { useI18n, type I18nKey } from "../i18n";
import type { MemberUnit } from "../types/agent";
import type { ReviewResult } from "../types/agent";
import type { UndoResultRecord } from "../types/undo";

export type { RightPanelTab };

type Props = {
  open: boolean;
  tab: RightPanelTab | null;
  review: ReviewResult | null;
  onTab: (t: RightPanelTab | null) => void;
  reviewContext?: "session" | "none";
  sessionId?: string | null;
  repoId?: string | null;
  repoName?: string | null;
  previewPath?: string | null;
  previewSessionId?: string | null;
  onClosePreview?: () => void;
  drill?: DrillProps | null;
  inspectorMember?: MemberUnit | null;
  onCloseInspector?: () => void;
  showTaskList?: boolean;
  taskListWorkers?: MemberUnit[];
  onSelectTask?: (aid: string) => void;
  onStopTask?: (aid: string) => void;
  onBackToList?: () => void;
  undoTarget?: {
    sessionId: string;
    runId: string;
    result?: UndoResultRecord | null;
  } | null;
  onExitUndo?: () => void;
  onUndoComplete?: (result: UndoResultRecord) => void;
};

const ic = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.8,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  width: 14,
  height: 14,
};

const SOON: Record<
  Exclude<RightPanelTab, "review" | "files" | "preview">,
  I18nKey
> = {
  side: "rightPanel.soon.side",
  terminal: "rightPanel.soon.terminal",
  browser: "rightPanel.soon.browser",
};

const PICKER_CARDS: {
  id: RightPanelTab;
  h: string;
  descriptionKey: I18nKey;
  available: boolean;
  icon: (p: typeof ic) => ReactElement;
}[] = [
  {
    id: "files",
    h: "Files",
    descriptionKey: "rightPanel.picker.filesDescription",
    available: true,
    icon: (p) => (
      <svg {...p}>
        <path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z" />
      </svg>
    ),
  },
  {
    id: "review",
    h: "Review",
    descriptionKey: "rightPanel.picker.reviewDescription",
    available: true,
    icon: (p) => (
      <svg {...p}>
        <path d="M12 3v18M5 8l-3 3 3 3M19 8l3 3-3 3" />
      </svg>
    ),
  },
  {
    id: "side",
    h: "Side chat",
    descriptionKey: "rightPanel.picker.sideDescription",
    available: false,
    icon: (p) => (
      <svg {...p}>
        <path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z" />
      </svg>
    ),
  },
  {
    id: "terminal",
    h: "Terminal",
    descriptionKey: "rightPanel.picker.terminalDescription",
    available: false,
    icon: (p) => (
      <svg {...p}>
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M7 9l3 3-3 3M13 15h4" />
      </svg>
    ),
  },
  {
    id: "browser",
    h: "Browser",
    descriptionKey: "rightPanel.picker.browserDescription",
    available: false,
    icon: (p) => (
      <svg {...p}>
        <circle cx="12" cy="12" r="9" />
        <path d="M3 12h18M12 3a14 14 0 010 18M12 3a14 14 0 000 18" />
      </svg>
    ),
  },
];

function basename(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

export function RightPanel({
  open,
  tab,
  review,
  onTab,
  reviewContext = "session",
  sessionId = null,
  repoId = null,
  repoName = null,
  previewPath = null,
  previewSessionId = null,
  onClosePreview,
  drill = null,
  inspectorMember,
  onCloseInspector,
  showTaskList,
  taskListWorkers,
  onSelectTask,
  onStopTask,
  onBackToList,
  undoTarget = null,
  onExitUndo,
  onUndoComplete,
}: Props) {
  const { t } = useI18n();
  if (!open) return null;
  return (
    <aside className="rightpanel">
      <div className="rightpanel__body">
        {inspectorMember ? (
          <TaskInspector
            member={inspectorMember}
            onClose={onCloseInspector ?? (() => {})}
            onBackToList={onBackToList}
          />
        ) : showTaskList ? (
          <TaskList
            workers={taskListWorkers ?? []}
            onSelect={onSelectTask ?? (() => {})}
            onStop={onStopTask ?? (() => {})}
          />
        ) : drill ? (
          <MemberDrillIn
            members={drill.members}
            selectedId={drill.selectedId}
            onSelect={drill.onSelect}
            onBack={drill.onBack}
            onStop={drill.onStop}
            goal={drill.goal}
            criteria={drill.criteria}
          />
        ) : tab === null ? (
          <div className="rppicker">
            <div className="rppicker__hint">{t("rightPanel.picker.hint")}</div>
            {previewPath ? (
              <div className="rppicker__card">
                <button
                  type="button"
                  aria-label={t("rightPanel.picker.open", {
                    name: t("rightPanel.picker.previewLabel"),
                  })}
                  onClick={() => onTab("preview")}
                  style={{
                    flex: 1,
                    minWidth: 0,
                    display: "flex",
                    alignItems: "center",
                    gap: 13,
                    padding: 0,
                    border: 0,
                    background: "none",
                    cursor: "pointer",
                    font: "inherit",
                    textAlign: "left",
                  }}
                >
                  <span className="rppicker__ic">
                    <svg {...ic} width={18} height={18}>
                      <path d="M4 4h11l5 5v11a1 1 0 01-1 1H4a1 1 0 01-1-1V5a1 1 0 011-1z" />
                      <path d="M14 4v5h5" />
                      <circle cx="11.5" cy="14" r="2" />
                    </svg>
                  </span>
                  <span className="rppicker__tx">
                    <span className="rppicker__h">
                      {t("rightPanel.picker.previewLabel")}
                    </span>{" "}
                    <span className="rppicker__s">{basename(previewPath)}</span>
                  </span>
                </button>
                <button
                  type="button"
                  className="rptabs__win"
                  aria-label={t("rightPanel.preview.close")}
                  title={t("rightPanel.preview.close")}
                  onClick={onClosePreview}
                >
                  <svg {...ic}>
                    <path d="M6 6l12 12M18 6L6 18" />
                  </svg>
                </button>
              </div>
            ) : null}
            {PICKER_CARDS.map((c) => (
              <button
                key={c.id}
                className="rppicker__card"
                aria-label={t(
                  c.available
                    ? "rightPanel.picker.open"
                    : "rightPanel.picker.unavailable",
                  { name: c.h },
                )}
                title={
                  c.available
                    ? undefined
                    : t("rightPanel.picker.unavailable", { name: c.h })
                }
                disabled={!c.available}
                onClick={() => {
                  if (c.available) onTab(c.id);
                }}
              >
                <span className="rppicker__ic">
                  {c.icon({ ...ic, width: 18, height: 18 })}
                </span>
                <span className="rppicker__tx">
                  <span className="rppicker__h">{c.h}</span>{" "}
                  <span className="rppicker__s">{t(c.descriptionKey)}</span>
                </span>
                <span className="rppicker__go">
                  {c.available ? (
                    <svg {...ic}>
                      <path d="M9 6l6 6-6 6" />
                    </svg>
                  ) : (
                    <span className="rppicker__soon">
                      {t("rightPanel.picker.soon")}
                    </span>
                  )}
                </span>
              </button>
            ))}
          </div>
        ) : tab === "files" ? (
          <FilesPanel
            sessionId={sessionId}
            repoId={repoId}
            repoName={repoName}
          />
        ) : tab === "review" ? (
          undoTarget ? (
            <UndoReviewPanel
              sessionId={undoTarget.sessionId}
              runId={undoTarget.runId}
              initialResult={undoTarget.result}
              onBack={onExitUndo ?? (() => {})}
              onComplete={onUndoComplete ?? (() => {})}
            />
          ) : (
            <>
              {review && review.has_changes ? (
                <ReviewPanel review={review} onClose={() => onTab(null)} />
              ) : (
                <div className="rp-empty">
                  <svg {...ic} width={30} height={30}>
                    <circle cx="12" cy="12" r="9" />
                    <path d="M8 12h8" />
                  </svg>
                  <div className="rp-empty__h">
                    {reviewContext === "none"
                      ? t("rightPanel.empty.noSessionTitle")
                      : review?.diff_available === false
                        ? t("reviewPanel.unavailableTitle")
                        : t("rightPanel.empty.noChangesTitle")}
                  </div>
                  <div className="rp-empty__s">
                    {reviewContext === "none"
                      ? t("rightPanel.empty.noSessionDescription")
                      : review?.diff_available === false
                        ? t("reviewPanel.unavailableDescription")
                        : t("rightPanel.empty.noChangesDescription")}
                  </div>
                </div>
              )}
              {reviewContext !== "none" &&
              (review?.other_dirty_count ?? 0) > 0 ? (
                <div className="rp-otherdirty">
                  {t("reviewPanel.otherDirty", {
                    count: review?.other_dirty_count ?? 0,
                  })}
                </div>
              ) : null}
            </>
          )
        ) : tab === "preview" ? (
          <div style={{ position: "relative", height: "100%" }}>
            <PreviewPanel path={previewPath} sessionId={previewSessionId} />
            <button
              type="button"
              className="iconbtn"
              aria-label={t("rightPanel.preview.close")}
              title={t("rightPanel.preview.close")}
              onClick={onClosePreview}
              style={{ position: "absolute", top: 6, right: 6 }}
            >
              <svg {...ic}>
                <path d="M6 6l12 12M18 6L6 18" />
              </svg>
            </button>
          </div>
        ) : (
          <p className="rightpanel__empty">{t(SOON[tab])}</p>
        )}
      </div>
    </aside>
  );
}
