import { memo } from "react";
import type { MemberUnit } from "../types/agent";
import { AgentAvatar } from "./AgentAvatar";
import { useI18n, type TFn } from "../i18n";
import {
  memberFailureReason,
  memberFailureReasonText,
} from "../lib/memberFailure";

// stopped ≠ 失败：worker 因超时/被停而中断（结果可能已落库），渲中性徽标「已中断/STOPPED」而非红
// FAILED（对齐 taskStatus.ts 里 stopped=cancel 的语义）；只有真 failed 才保留红色 FAILED。
function workerBadge(
  status: MemberUnit["status"],
  t: TFn,
): {
  cls: string;
  label: string;
} {
  switch (status) {
    case "running":
      return { cls: "run", label: "RUNNING" };
    case "done":
      return { cls: "done", label: "DONE" };
    case "failed":
      return { cls: "fail", label: "FAILED" };
    case "stopped":
      return { cls: "intr", label: t("stream.worker.badge.stopped") };
    case "needs_input":
      return { cls: "intr", label: "QUEUED" };
  }
}

function DispatchCardImpl({
  member,
  onOpenInspector,
}: {
  member: MemberUnit;
  onOpenInspector?: (assignmentId: string) => void;
}) {
  const { t } = useI18n();
  const open = () => onOpenInspector?.(member.assignment_id);
  const b = workerBadge(member.status, t);
  const sub = member.sub?.trim() || null;
  // failed/stopped 时徽标下补一行有信息量的人话。没有分类证据仍保留 no_final_text
  // 兜底；唯独 spawn 没有 detail 时不复述「FAILED / worker 调用失败」。
  const failureReason =
    member.status === "failed" || member.status === "stopped"
      ? (memberFailureReason(member) ?? {
          code: member.status === "stopped" ? "stopped" : "no_final_text",
        })
      : null;
  const failureText =
    failureReason == null ? null : memberFailureReasonText(failureReason, t);

  return (
    <div
      className="workerrow"
      role="button"
      tabIndex={0}
      onClick={open}
      onKeyDown={(e) => {
        if (e.key === "Enter") open();
      }}
    >
      {/* P2-9（opus 对抗审）：失败原因另起一行靠这层内嵌的 .wr-main 行容器，不靠给
          共享的 .workerrow 加 flex-wrap:wrap——那样会改到所有 running/done 行的换行行为
          （即便实际视觉上多半不触发，也不该让一个只服务 failed/stopped 的功能悄悄改一个
          全局共享类的布局属性）。这里纯加法：多一层 div，互不影响。 */}
      <div className="wr-main">
        <AgentAvatar kind={member.name} />
        <span className="wr-nm">{member.name}</span>
        {sub && <span className="wr-sub">{sub}</span>}
        <span className={"toolcard__badge toolcard__badge--" + b.cls}>
          {b.label}
        </span>
        <span
          className="wr-view"
          onClick={(e) => {
            e.stopPropagation();
            open();
          }}
        >
          {t("stream.task.view")}
        </span>
      </div>
      {failureText && <span className="wr-fail-reason">{failureText}</span>}
    </div>
  );
}

export const DispatchCard = memo(DispatchCardImpl);
