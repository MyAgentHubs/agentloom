// v3 形态：picker 默认（tab=null 时 tab 行只显示末尾 +）+ 平铺 tab 行（已开 tab 平铺、
// 无分组分隔线、无门符号前缀（旧 model A）、固定宽截断 + 横滚不换行）+ 顶栏右段双窗口控件
// （展开 占用 main 区域 + 单收起，两枚不同图标、同 app topbar 中性、同 26×26 不变小）。
// 对原型 right-panel-v3.html 态 ②（picker tab 行只有 +）/ ⑤（展开态按钮变恢复分栏）。
import type { ReactElement } from "react";
import { useI18n } from "../i18n";

export type RightPanelTab =
  | "files"
  | "review"
  | "preview"
  | "side"
  | "terminal"
  | "browser";

type Props = {
  open: boolean;
  tab: RightPanelTab | null;
  openTabs: RightPanelTab[];
  expanded: boolean;
  canMaximize?: boolean;
  /** plan B3：Review tab 角标——变更文件数 > 0 时显数字。 */
  reviewBadge?: number;
  onTab: (tab: RightPanelTab | null) => void;
  onExpand: () => void;
  onUserCollapse: () => void;
  onExpandPanel: () => void;
  onRestorePanel: () => void;
};

const ic = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  width: 14,
  height: 14,
};

const TAB_META: Record<
  RightPanelTab,
  { label: string; icon: (props: typeof ic) => ReactElement }
> = {
  files: {
    label: "Files",
    icon: (p) => (
      <svg {...p}>
        <path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z" />
      </svg>
    ),
  },
  review: {
    label: "Review",
    icon: (p) => (
      <svg {...p}>
        <path d="M12 3v18M5 8l-3 3 3 3M19 8l3 3-3 3" />
      </svg>
    ),
  },
  preview: {
    label: "Preview",
    icon: (p) => (
      <svg {...p}>
        <path d="M4 4h11l5 5v11a1 1 0 01-1 1H4a1 1 0 01-1-1V5a1 1 0 011-1z" />
        <path d="M14 4v5h5" />
        <circle cx="11.5" cy="14" r="2" />
      </svg>
    ),
  },
  side: {
    label: "Side chat",
    icon: (p) => (
      <svg {...p}>
        <path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z" />
      </svg>
    ),
  },
  terminal: {
    label: "Terminal",
    icon: (p) => (
      <svg {...p}>
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M7 9l3 3-3 3M13 15h4" />
      </svg>
    ),
  },
  browser: {
    label: "Browser",
    icon: (p) => (
      <svg {...p}>
        <circle cx="12" cy="12" r="9" />
        <path d="M3 12h18M12 3a14 14 0 010 18M12 3a14 14 0 000 18" />
      </svg>
    ),
  },
};

function TabBtn({
  id,
  active,
  onTab,
  badge,
}: {
  id: RightPanelTab;
  active: boolean;
  onTab: (t: RightPanelTab) => void;
  badge?: number;
}) {
  const meta = TAB_META[id];
  return (
    <button
      role="tab"
      aria-selected={active}
      aria-label={meta.label}
      className={`rptab rptab--v3${active ? " rptab--on" : ""}`}
      onClick={() => onTab(id)}
      title={meta.label}
    >
      {meta.icon({ ...ic, width: 13, height: 13 })}
      <span className="rptab__nm">{meta.label}</span>
      {badge != null && badge > 0 && (
        <span className="rptab__badge">{badge}</span>
      )}
    </button>
  );
}

export function RightPanelTabs({
  open,
  tab,
  openTabs,
  expanded,
  canMaximize = true,
  reviewBadge,
  onTab,
  onExpand,
  onUserCollapse,
  onExpandPanel,
  onRestorePanel,
}: Props) {
  const { t } = useI18n();
  if (!open) {
    return (
      <div className="topbar__panel topbar__panel--collapsed">
        <button
          className="rptabs__expand"
          aria-label={t("rightPanelTabs.expand")}
          title={t("rightPanelTabs.expandTitle")}
          onClick={onExpand}
        >
          <svg {...ic} width={18} height={18}>
            <path d="M15 3h6v6M21 3l-7 7M9 21H3v-6M3 21l7-7" />
          </svg>
        </button>
      </div>
    );
  }
  return (
    <div className={`topbar__panel${expanded ? " topbar__panel--expand" : ""}`}>
      <div className="rptabs">
        <div className="rptabs__tabrow" role="tablist">
          {openTabs.map((t) => (
            <TabBtn
              key={t}
              id={t}
              active={tab === t}
              onTab={onTab}
              badge={t === "review" ? reviewBadge : undefined}
            />
          ))}
          <button
            className="rptabs__add"
            aria-label={t("rightPanelTabs.newTab")}
            title={t("rightPanelTabs.newTab")}
            onClick={() => onTab(null)}
          >
            <svg {...ic}>
              <path d="M12 5v14M5 12h14" />
            </svg>
          </button>
        </div>
        <div className="rptabs__wins">
          {canMaximize &&
            (expanded ? (
              <button
                className="rptabs__win"
                aria-label={t("rightPanelTabs.restore")}
                title={t("rightPanelTabs.restoreTitle")}
                onClick={onRestorePanel}
              >
                <svg {...ic}>
                  <path d="M3 8h5V3M16 3v5h5M21 16h-5v5M8 21v-5H3" />
                </svg>
              </button>
            ) : (
              <button
                className="rptabs__win"
                aria-label={t("rightPanelTabs.maximize")}
                title={t("rightPanelTabs.maximizeTitle")}
                onClick={onExpandPanel}
              >
                <svg {...ic}>
                  <path d="M8 3H3v5M16 3h5v5M3 16v5h5M21 16v5h-5" />
                </svg>
              </button>
            ))}
          <button
            className="rptabs__win"
            aria-label={t("rightPanelTabs.collapse")}
            title={t("rightPanelTabs.collapseTitle")}
            onClick={onUserCollapse}
          >
            <svg {...ic}>
              <rect x="3" y="3" width="18" height="18" rx="2" />
              <path d="M15 3v18" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
