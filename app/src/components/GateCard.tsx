import { useState } from "react";
import { useI18n } from "../i18n";
import type { GateDraft, GateAction } from "../lib/gateReducer";
import type { AgentProfile } from "../types/agent";
import { AssignmentEditor } from "./AssignmentEditor";

const LONG_LIST = 8; // A3：>8 条折后前 8 + 展开剩余

type Props = {
  draft: GateDraft;
  leadName: string;
  enabledAgents: AgentProfile[];
  onAction: (a: GateAction) => void;
  onFreeze: () => void;
  /** 让队长重拟（丢 draft 再 propose）。砍「带批注重拟」→ 无 note 参数。 */
  onRedraft: () => void;
  /** 冻结发起链 in-flight 时为 true·禁用主按钮防双击困惑（P2-1）。 */
  freezing?: boolean;
  readonlyReason?: string | null;
};

const TIER_LABEL: Record<GateDraft["tier"], string> = {
  tier0: "Tier 0",
  tier1: "Tier 1",
  tier2: "Tier 2",
};

export function GateCard({
  draft,
  leadName,
  enabledAgents,
  onAction,
  onFreeze,
  onRedraft,
  freezing = false,
  readonlyReason = null,
}: Props) {
  const { t } = useI18n();
  const [editingGoal, setEditingGoal] = useState(false);
  const [goalDraft, setGoalDraft] = useState(draft.goal);
  const [teamOpen, setTeamOpen] = useState(false);
  const [showAll, setShowAll] = useState(false);

  const shown =
    showAll || draft.criteria.length <= LONG_LIST
      ? draft.criteria
      : draft.criteria.slice(0, LONG_LIST);
  const hiddenCount = draft.criteria.length - shown.length;
  const readonly = readonlyReason != null;

  const assignees = draft.assignments
    .map((a) => a.assignee)
    .filter((x): x is NonNullable<typeof x> => x != null);

  function renderReadonlyAssignments() {
    return (
      <div
        className="assign assign--readonly"
        data-testid="gate-readonly-assignments"
      >
        <div className="assign__head">
          <span className="assign__t">{t("gateCard.readonlyAssignments")}</span>
          <span className="assign__lead">
            {draft.autoDispatch
              ? t("gateCard.autoDispatch")
              : t("gateCard.manualDispatch")}
          </span>
        </div>
        {draft.assignments.map((s) => (
          <div className="assign__row" key={s.subtaskId}>
            <div className="assign__task">
              <div className="assign__d">{s.subtask}</div>
              {s.scopeFiles.length > 0 && (
                <div className="assign__f">{s.scopeFiles.join(" · ")}</div>
              )}
            </div>
            <div className="assign__chipwrap">
              <span className="assign__chip" data-readonly="true">
                {s.assignee ? (
                  <>
                    <span className="assign__a" aria-hidden>
                      {s.assignee.provider.slice(0, 1).toUpperCase()}
                    </span>
                    <span className="assign__nm">{s.assignee.provider}</span>
                  </>
                ) : (
                  <span className="assign__nm assign__nm--none">
                    {t("gateCard.unassigned")}
                  </span>
                )}
              </span>
            </div>
          </div>
        ))}
      </div>
    );
  }

  return (
    <div className="gate-card">
      <div className="gate-card__au">
        <span className="gate-card__av" aria-hidden>
          {leadName.slice(0, 1).toUpperCase()}
        </span>
        <span className="gate-card__w">{leadName}</span>
        <span className="gate-card__r">· Lead</span>
      </div>
      <div className="gate-card__say">
        {draft.manual
          ? t("gateCard.manualIntro")
          : t("gateCard.autoIntro", { count: draft.criteria.length })}
      </div>

      <div className="gate-card__card">
        <div className="gate-card__head">
          <span className="gate-card__draft">{t("gateCard.draft")}</span>
          <span className="gate-card__ht">{t("gateCard.headerTitle")}</span>
          <span className="gate-card__tier">{TIER_LABEL[draft.tier]}</span>
        </div>

        {draft.tier === "tier1" && (
          <div className="gate-card__tier-note">{t("gateCard.tierNote")}</div>
        )}

        <div className="gate-card__goal">
          <div className="gate-card__lab">{t("gateCard.goalLabel")}</div>
          {editingGoal ? (
            <input
              className="gate-card__goal-input"
              aria-label={t("gateCard.editGoalAria")}
              autoFocus
              value={goalDraft}
              onChange={(e) => setGoalDraft(e.currentTarget.value)}
              onBlur={() => {
                setEditingGoal(false);
                if (readonly) return;
                if (goalDraft !== draft.goal)
                  onAction({ type: "editGoal", goal: goalDraft });
              }}
              disabled={readonly}
            />
          ) : (
            <div className="gate-card__gline">
              <span className="gate-card__gtxt">
                {draft.goal || t("gateCard.emptyGoal")}
              </span>
              <button
                type="button"
                className="gate-card__edit"
                disabled={readonly}
                onClick={() => {
                  if (readonly) return;
                  setGoalDraft(draft.goal);
                  setEditingGoal(true);
                }}
              >
                {t("gateCard.edit")}
              </button>
            </div>
          )}
        </div>

        <div className="gate-card__acc">
          <div className="gate-card__acc-lab">
            {t("gateCard.acceptanceTitle")}
            <span className="gate-card__hint">
              {t("gateCard.acceptanceHint")}
            </span>
          </div>
          {shown.map((c, i) => (
            <div className="gate-card__acc-row" key={c.id}>
              <span className="gate-card__num">{i + 1}</span>
              <input
                className="gate-card__ctx"
                aria-label={t("gateCard.criterionAria", { index: i + 1 })}
                value={c.claim}
                placeholder={t("gateCard.criterionPlaceholder")}
                disabled={readonly}
                onChange={(e) =>
                  readonly
                    ? undefined
                    : onAction({
                        type: "editCriterion",
                        id: c.id,
                        claim: e.currentTarget.value,
                      })
                }
              />
              <span hidden>{c.claim}</span>
              <button
                type="button"
                className="gate-card__ib"
                aria-label={t("gateCard.deleteCriterionAria")}
                disabled={readonly}
                onClick={() =>
                  readonly
                    ? undefined
                    : onAction({ type: "removeCriterion", id: c.id })
                }
              >
                ✕
              </button>
            </div>
          ))}
          {hiddenCount > 0 && (
            <button
              type="button"
              className="gate-card__more"
              onClick={() => setShowAll(true)}
            >
              {t("gateCard.showRemaining", { count: hiddenCount })}
            </button>
          )}
          <button
            type="button"
            className="gate-card__acc-add"
            disabled={readonly}
            onClick={() =>
              readonly ? undefined : onAction({ type: "addCriterion" })
            }
          >
            {t("gateCard.addCriterion")}
          </button>
        </div>

        <div className="gate-card__team">
          <button
            type="button"
            className="gate-card__trow"
            aria-expanded={teamOpen}
            onClick={() => {
              setTeamOpen((p) => !p);
            }}
          >
            <span className="gate-card__chev" aria-hidden>
              {teamOpen ? "▾" : "▸"}
            </span>
            <span className="gate-card__tl">
              <b>{t("gateCard.assignments")}</b>
              {t("gateCard.assignmentHint")}
            </span>
            <span className="gate-card__stack" aria-hidden>
              {assignees.map((a, i) => (
                <span className="gate-card__a" key={i}>
                  {a.provider.slice(0, 1).toUpperCase()}
                </span>
              ))}
            </span>
          </button>
          {teamOpen && !readonly && (
            <AssignmentEditor
              assignments={draft.assignments}
              autoDispatch={draft.autoDispatch}
              enabledAgents={enabledAgents}
              onAction={onAction}
            />
          )}
          {teamOpen && readonly && renderReadonlyAssignments()}
        </div>

        <div className="gate-card__foot">
          <button
            type="button"
            className="gate-card__gbtn is-primary"
            onClick={() => {
              if (!readonly) onFreeze();
            }}
            disabled={freezing || readonly}
          >
            {freezing
              ? t("gateCard.freezing")
              : draft.tier === "tier2"
                ? t("gateCard.confirmAndStart")
                : t("gateCard.start")}
          </button>
          {!draft.manual && (
            <button
              type="button"
              className="gate-card__gbtn is-ghost"
              disabled={readonly}
              onClick={() => {
                if (!readonly) onRedraft();
              }}
            >
              {t("gateCard.redraft")}
            </button>
          )}
          <span className="gate-card__fnote">
            {readonly
              ? t("gateCard.readonlyCannotStart")
              : t("gateCard.freezeHint")}
          </span>
        </div>
      </div>
    </div>
  );
}
