import { CodingAskCard } from "./CodingAskCard";
import type { CodingTaskBlock } from "../types/agent";

/** coding 闭环决策卡（从 CodingTaskCard 体内外提·块 B）。仅 ask_verify/verify_failed 渲卡·其余 null。
 * 动作回调只吃 runId（换父容器不丢·§7.1-B 功能不破底线）。 */
export function CodingDecisionCard({
  block,
  onConfirmVerify,
  onShelve,
  onRetryVerify,
  onViewChanges,
}: {
  block: CodingTaskBlock;
  onConfirmVerify?: (runId: string, cmd: string) => void;
  onShelve?: (runId: string) => void;
  onRetryVerify?: (runId: string) => void;
  onViewChanges?: (runId: string, assignmentId: string) => void;
}) {
  const { phase, run_id, assignment_id } = block;
  if (phase === "ask_verify")
    return (
      <CodingAskCard
        key={phase}
        kind="verify"
        recommendedCmd={block.verify_cmd ?? null}
        onConfirmVerify={(cmd) => onConfirmVerify?.(run_id, cmd)}
        onShelve={() => onShelve?.(run_id)}
      />
    );
  if (phase === "verify_failed")
    return (
      <CodingAskCard
        key={phase}
        kind="verify_failed"
        recommendedCmd={null}
        detail={block.detail}
        onConfirmVerify={(cmd) => onConfirmVerify?.(run_id, cmd)}
        onShelve={() => onShelve?.(run_id)}
        onViewChanges={() => onViewChanges?.(run_id, assignment_id)}
        onRetryVerify={() => onRetryVerify?.(run_id)}
      />
    );
  return null;
}
