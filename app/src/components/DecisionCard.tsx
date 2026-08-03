import { useState } from "react";
import type { ChatMessage } from "../types/agent";
import { useI18n } from "../i18n";
import { splitDecisionOption } from "../lib/decisionOption";

type DecisionCardBlock = Extract<
  ChatMessage["content"][number],
  { type: "decision_card" }
>;

export function DecisionCard({
  block,
  onChoose,
}: {
  block: DecisionCardBlock;
  onChoose?: (decisionId: string, option: string) => void;
}) {
  const { t } = useI18n();
  const [rationaleOpen, setRationaleOpen] = useState(false);
  const [questionOpen, setQuestionOpen] = useState(false);
  const disabled = block.status === "submitting" || !onChoose;

  // 决策打扰收敛刀 T1·症状 B：chosen 态不再整条消失（原来 return null 让点击像石沉大海）——
  // 渲一行紧凑回执，与 App.tsx onDecisionChoose 落地的 "submitting"→"chosen" 状态机对齐。
  if (block.status === "chosen") {
    return (
      <div className="decision-chosen">
        {t("decisionCard.chosen", { option: block.chosen_option ?? "" })}
      </div>
    );
  }

  return (
    <div className="decision-card">
      <div className="dc-head">
        <div className="dc-head-hint">{t("decisionCard.hint")}</div>
        <div
          className={
            "dc-head-question" + (questionOpen ? " dc-head-question--open" : "")
          }
        >
          {block.question}
        </div>
        <button
          type="button"
          className="pf-view dc-question-toggle"
          aria-expanded={questionOpen}
          onClick={() => setQuestionOpen((open) => !open)}
        >
          {questionOpen
            ? t("decisionCard.questionCollapse")
            : t("decisionCard.questionExpand")}
        </button>
      </div>
      {block.options.map((opt, i) => {
        const recommended = opt === block.recommended;
        const { label, desc } = splitDecisionOption(opt);
        return (
          <button
            key={opt}
            type="button"
            className={"decision-option" + (recommended ? " rec" : "")}
            disabled={disabled}
            onClick={() => onChoose?.(block.decision_id, opt)}
          >
            <span className="di-n">{i + 1}</span>
            <span className="di-tx">
              <b>
                {label}
                {recommended && (
                  <span className="rec-pill">
                    {t("decisionCard.recommended")}
                  </span>
                )}
              </b>
              {desc && <span>{desc}</span>}
            </span>
          </button>
        );
      })}
      {block.rationale && (
        <div className="dec-foot dec-foot--rationale">
          <button
            type="button"
            className="pf-view dec-rationale-toggle"
            aria-expanded={rationaleOpen}
            onClick={() => setRationaleOpen((open) => !open)}
          >
            {t("decisionCard.rationaleToggle", {
              indicator: rationaleOpen ? "▴" : "▾",
            })}
          </button>
          {rationaleOpen && (
            <div className="dec-rationale">{block.rationale}</div>
          )}
        </div>
      )}
      {block.status === "failed" && (
        <div className="dec-foot">
          <button
            type="button"
            disabled={!onChoose}
            onClick={() =>
              onChoose?.(
                block.decision_id,
                block.chosen_option ?? block.options[0],
              )
            }
          >
            {t("decisionCard.retry")}
          </button>
        </div>
      )}
    </div>
  );
}
