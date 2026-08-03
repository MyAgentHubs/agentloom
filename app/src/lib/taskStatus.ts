import type { MemberUnit, Block, CodingPhase } from "../types/agent";
import type { I18nKey } from "../i18n";
import { memberFailureProgress, memberFailureReason } from "./memberFailure";

export type TaskRowText = {
  key: I18nKey;
  values?: Record<string, string | number>;
};

export type TaskRowView = {
  dotClass: string;
  label: I18nKey;
  name: string | TaskRowText;
  progress: I18nKey | null;
  // 仅运行中的 latestSummary 原文保留在 rawProgress。
  rawProgress: string | null;
  chips: TaskRowText[];
};

const STATUS_MAP: Record<
  MemberUnit["status"],
  { dotClass: string; label: I18nKey }
> = {
  running: { dotClass: "run", label: "memberDrillIn.status.running" },
  needs_input: {
    dotClass: "wait",
    label: "memberDrillIn.status.needsInput",
  },
  done: { dotClass: "done", label: "memberDrillIn.status.done" },
  failed: { dotClass: "fail", label: "memberDrillIn.status.failed" },
  stopped: { dotClass: "cancel", label: "memberDrillIn.status.stopped" },
};
const TERMINAL = new Set(["done", "failed", "stopped"]);

function latestSummary(blocks: Block[]): string | null {
  for (let i = blocks.length - 1; i >= 0; i--) {
    const b = blocks[i] as any;
    if (b.type === "tool" && typeof b.summary === "string" && b.summary.trim())
      return b.summary;
    if (b.type === "text" && typeof b.text === "string" && b.text.trim())
      return b.text.split("\n")[0];
  }
  return null;
}

/** member → 任务行视图（块 B）。5 态映射·执行中只 chip 步进度·终态才 N files/验证条数·进展取最新 summary。 */
export function taskRowView(m: MemberUnit): TaskRowView {
  const map = STATUS_MAP[m.status] ?? STATUS_MAP.running;
  const terminal = TERMINAL.has(m.status);
  const chips: TaskRowText[] = [];
  if (!terminal) {
    if (m.steps_total > 0)
      chips.push({
        key: "taskStatus.chip.steps",
        values: { done: m.steps_done, total: m.steps_total },
      });
  } else {
    const files = m.result?.changed_files ?? [];
    if (files.length)
      chips.push({
        key: "taskStatus.chip.files",
        values: { n: files.length },
      });
    const cmds = m.result?.command_evidence ?? [];
    if (cmds.length)
      chips.push({
        key: "taskStatus.chip.verify",
        values: { n: cmds.length },
      });
  }
  const sub = m.sub.trim();
  let failureProgress: I18nKey | null = null;
  if (m.status === "failed") {
    const failureReason = memberFailureReason(m);
    // 状态行只承载短标签：spawn 有 detail 时继续显示既有分类标签，真实 detail 留给
    // DispatchCard/LeadSummaryBlock；没有 detail 时不显示「worker 调用失败」空话，回退到
    // 此处已有的通用失败状态。
    failureProgress =
      failureReason?.code === "spawn" &&
      (failureReason.detail?.trim() ?? "") === ""
        ? STATUS_MAP.failed.label
        : memberFailureProgress(m);
  } else if (m.status === "stopped") {
    failureProgress = memberFailureProgress(m);
  }
  return {
    dotClass: map.dotClass,
    label: map.label,
    name: sub || { key: "liveStreamCard.preparing" },
    progress: failureProgress,
    rawProgress: m.status === "running" ? latestSummary(m.blocks) : null,
    chips,
  };
}

/** coding 闭环 phase → 任务行视图（块 B v3·替 T5 的 as-any 合成 MemberUnit·codex/opus 折）。
 * phase 精确映射 5 态〔ask_*=等你确认 / 中间步=进行中 / applied=已完成 / shelved=已搁置 / error=失败〕·
 * 进展位显 PHASE_LABEL（spec「coding 细分阶段显进展位」）。 */
const CODING_PHASE_VIEW: Record<
  CodingPhase,
  { dotClass: string; label: I18nKey; progress: I18nKey }
> = {
  finalizing: {
    dotClass: "run",
    label: "memberDrillIn.status.running",
    progress: "codingTask.phase.finalizing",
  },
  ask_verify: {
    dotClass: "wait",
    label: "memberDrillIn.status.needsInput",
    progress: "codingTask.phase.askVerify",
  },
  verifying: {
    dotClass: "run",
    label: "memberDrillIn.status.running",
    progress: "codingTask.phase.verifying",
  },
  verify_failed: {
    dotClass: "fail",
    label: "memberDrillIn.status.needsInput",
    progress: "codingTask.phase.verifyFailed",
  },
  ask_apply: {
    dotClass: "wait",
    label: "memberDrillIn.status.needsInput",
    progress: "taskStatus.phase.askApplyProgress",
  },
  merging: {
    dotClass: "run",
    label: "memberDrillIn.status.running",
    progress: "codingTask.phase.merging",
  },
  applying: {
    dotClass: "wait",
    label: "codingTask.phase.applying",
    progress: "taskStatus.phase.applyingProgress",
  },
  applied: {
    dotClass: "done",
    label: "memberDrillIn.status.done",
    progress: "codingTask.phase.applied",
  },
  landing_blocked: {
    dotClass: "fail",
    label: "codingTask.phase.landingBlocked",
    progress: "taskStatus.phase.landingBlockedProgress",
  },
  shelved: {
    dotClass: "cancel",
    label: "codingTask.phase.shelved",
    progress: "codingTask.phase.shelved",
  },
  error: {
    dotClass: "fail",
    label: "memberDrillIn.status.failed",
    progress: "codingTask.phase.error",
  },
};

export function codingPhaseView(
  phase: CodingPhase,
  workerName: string,
): TaskRowView {
  const v = CODING_PHASE_VIEW[phase];
  return {
    dotClass: v.dotClass,
    label: v.label,
    name: workerName,
    progress: v.progress,
    rawProgress: null,
    chips: [],
  };
}
