import type {
  AcceptanceCriterion,
  Criterion,
  GoalContract,
} from "../types/agent";
import { useI18n } from "../i18n";

type Props = {
  goal: GoalContract;
  totalTokens: number;
  /** 以下 prop 暂留接口（App 仍传入）·KISS 简化后不渲染·细节走 lead 会话。 */
  totalCostUsd?: number | null;
  criteria?: AcceptanceCriterion[];
  runId?: string;
  onWaive?: (criterionId: string, reason: string) => void;
};

// 二态映射（KISS·用户只关心做到没）：passed/waived = 已达成 ✓·其余 = 未达成 ○
function isDone(status: Criterion["status"]): boolean {
  return status === "passed" || status === "waived";
}

export function GoalCriteriaPanel({ goal, criteria }: Props) {
  const { t } = useI18n();
  const rows: (AcceptanceCriterion | Criterion)[] = criteria ?? goal.criteria;
  const hasCriteria = rows.length > 0;
  const doneCount = rows.filter((c) => isDone(c.status)).length;

  return (
    <div className="goal-body">
      <div className="goal-body__block">
        <div className="goal-body__lab">{t("goalCriteriaPanel.goal")}</div>
        <div className="goal-body__goal">{goal.goal}</div>
      </div>
      <div className="goal-body__block">
        <div className="goal-body__lab">
          {t("goalCriteriaPanel.criteria")}
          {hasCriteria && (
            <span className="goal-body__ct">
              {doneCount}/{rows.length}
            </span>
          )}
        </div>
        {hasCriteria ? (
          <div className="goal-acc">
            {rows.map((c) => {
              const done = isDone(c.status);
              return (
                <div
                  key={c.id}
                  className={`goal-acc__row ${done ? "is-done" : "is-todo"}`}
                >
                  <span className="goal-acc__mk" aria-hidden>
                    <svg viewBox="0 0 24 24">
                      {done ? (
                        <path d="M5 13l4 4L19 7" />
                      ) : (
                        <circle cx="12" cy="12" r="8.5" />
                      )}
                    </svg>
                  </span>
                  <span className="goal-acc__tx">{c.claim}</span>
                </div>
              );
            })}
          </div>
        ) : (
          <div className="goal-acc__empty">{t("goalCriteriaPanel.empty")}</div>
        )}
      </div>
    </div>
  );
}
