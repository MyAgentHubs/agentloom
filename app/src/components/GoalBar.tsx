import { useEffect, useRef } from "react";
import { useI18n } from "../i18n";
import type { Criterion, GoalContract } from "../types/agent";

type Props = {
  goal: GoalContract;
  expanded: boolean;
  onToggle: () => void;
  /** T10 的 GoalCriteriaPanel·原位向下 accordion 展开（非飘浮浮层）。折叠时不渲染。 */
  expandedSlot: React.ReactNode;
  topbar?: boolean;
  running?: boolean;
  runComplete?: boolean;
  runHasMemberFailure?: boolean;
};

// criterion status → 状态点 class（M1a 5 值；checking 留 M3 真验证时区分）
const DOT: Record<Criterion["status"], string> = {
  passed: "pass",
  failed: "fail",
  pending: "todo",
  waived: "waived",
  uncertain: "uncertain",
};

export function GoalBar({
  goal,
  expanded,
  onToggle,
  expandedSlot,
  topbar = false,
  running = false,
  runComplete = false,
  runHasMemberFailure = false,
}: Props) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const total = goal.criteria.length;
  // polish-b：Local scratch 任务无验收标准（criteria=[]）——目标条仍要显示
  // 目标文本 + 状态（执行中转圈），但不渲计数/状态点/查看验收 CTA。
  const hasCriteria = total > 0;
  // opus P1-A：n = 已了结（passed + waived·waived=特批达成）·m = total（含 waived）。
  // 定稿原型屏⑥「5 通过+1 跳过 = 6/6 已完成」是 done 态真相 → n 必须含 waived，否则有 waived 的 run 永到不了绿。
  const resolved = goal.criteria.filter(
    (c) => c.status === "passed" || c.status === "waived",
  ).length;
  const pendingCount = goal.criteria.filter(
    (c) => c.status === "pending",
  ).length;
  const hasFailed = goal.criteria.some((c) => c.status === "failed");
  const hasWaived = goal.criteria.some((c) => c.status === "waived");
  const allDone = total > 0 && resolved === total; // 全了结（无 pending/failed）
  const settledUnverified =
    runComplete &&
    !runHasMemberFailure &&
    total > 0 &&
    pendingCount === total &&
    !hasFailed &&
    !hasWaived;
  const barCls = [
    "goal-bar",
    hasFailed ? "has-fail" : "",
    allDone ? "is-done" : "",
    settledUnverified ? "is-settled-unverified" : "",
  ]
    .filter(Boolean)
    .join(" ");
  useEffect(() => {
    if (!expanded) return;
    const onDown = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        onToggle();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onToggle();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [expanded, onToggle]);
  const { t } = useI18n();
  const isDone = runComplete === true && !running;

  return (
    <div
      ref={wrapRef}
      className={`goal-wrap${topbar ? " goal-wrap--topbar" : ""}`}
    >
      <div className={barCls}>
        <button
          type="button"
          className="goal-bar__row"
          aria-expanded={expanded}
          onClick={onToggle}
        >
          <span
            className={`goal-bar__ic${isDone ? " is-done" : ""}`}
            aria-hidden
          >
            ◇
          </span>
          {isDone && !hasCriteria && (
            <span className="goal-bar__done" aria-label={t("goalBar.done")}>
              <svg
                viewBox="0 0 24 24"
                width={12}
                height={12}
                fill="none"
                stroke="currentColor"
                strokeWidth={2.4}
              >
                <path d="M5 13l4 4L19 7" />
              </svg>
            </span>
          )}
          <span className="goal-bar__lab">{t("goalBar.label")}</span>
          {/* P2-A：✓ 常驻；settled-unverified 例外只报总数「目标 N 条」，
              待核计数由右侧「N 条验收待复核」承载、不重复表达（去冗余）。
              polish-b：无验收标准（criteria=[]）→ 不渲计数/状态点。 */}
          {hasCriteria && (
            <span className="goal-bar__count">
              {settledUnverified
                ? t("goalBar.criteriaCount", { total })
                : `${resolved}/${total} ✓`}
            </span>
          )}
          {hasCriteria && (
            <span className="goal-bar__dots" aria-hidden>
              {goal.criteria.map((c) => (
                <i key={c.id} className={DOT[c.status]} />
              ))}
            </span>
          )}
          <span className="goal-bar__goal">
            {settledUnverified
              ? t("goalBar.pendingReview", { count: pendingCount })
              : topbar && goal.goal_title
                ? goal.goal_title
                : goal.goal}
          </span>
          {running && (
            <span className="goal-bar__st run" aria-live="polite">
              <span className="goal-bar__spin" aria-hidden>
                <svg viewBox="0 0 24 24">
                  <path d="M12 3a9 9 0 109 9" />
                </svg>
              </span>
            </span>
          )}
          {/* polish-b：无验收标准 → 不渲查看验收 CTA（无悬空入口）。 */}
          {hasCriteria && (
            <span className="goal-bar__cta">
              {t("goalBar.viewCriteria")} <span aria-hidden>▾</span>
            </span>
          )}
        </button>
        {expanded && expandedSlot}
      </div>
    </div>
  );
}
