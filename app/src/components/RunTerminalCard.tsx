import { memo } from "react";
import type { Block } from "../types/agent";
import { useI18n } from "../i18n";
import { renderBackendError } from "../lib/backendMsg";
import { humanizeStopReason } from "../lib/stopReason";

type RunTerminalBlock = Extract<Block, { type: "run_terminal" }>;

type Tone = "ok" | "warn" | "error" | "muted";

function RunTerminalCardImpl({ block }: { block: RunTerminalBlock }) {
  const { t } = useI18n();

  // completed 且无 message → 对齐 live「空轮 completed 不渲卡」惯例，不渲染。
  if (block.status === "completed" && !block.message) return null;

  let tone: Tone;
  let label: string;
  // needs_decision 按规格只显示固定文案，不附带 message（范围调整已由 scope_change 卡承载）。
  let showMessage = true;

  switch (block.status) {
    case "completed":
      tone = "ok";
      label = t("runTerminal.completed");
      break;
    case "error":
      tone = "error";
      label = t("runTerminal.error");
      break;
    case "interrupted":
      tone = "warn";
      label = t("runTerminal.interrupted");
      break;
    case "blocked":
      tone = "warn";
      label = t("runTerminal.blocked");
      break;
    case "needs_decision":
      tone = "warn";
      label = t("runTerminal.needsDecision");
      showMessage = false;
      break;
    case "fallback":
      tone = "muted";
      label = t("runTerminal.fallback");
      break;
    default:
      // 未知 status → 灰点 + 原样显示 status 文本（前向兼容·不崩不吞）。
      tone = "muted";
      label = block.status;
      break;
  }

  // 收工 reason 人话化：只对 blocked 状态的裸 reason 码过映射（interrupted/error 的
  // message 已经是完整人话句子，未收录的 reason 原样透传·见 lib/stopReason.ts）。
  const rawMessage = showMessage ? block.message : null;
  const humanized =
    rawMessage && block.status === "blocked"
      ? humanizeStopReason(rawMessage, t)
      : rawMessage;
  const message = humanized ? renderBackendError(humanized, t) : null;
  const full = message ? `${label} · ${message}` : label;

  return (
    <div className="run-terminal" title={full}>
      <span
        className={`run-terminal__dot run-terminal__dot--${tone}`}
        aria-hidden="true"
      />
      <span className="run-terminal__label">{label}</span>
      {message ? <span className="run-terminal__msg">{message}</span> : null}
    </div>
  );
}

export const RunTerminalCard = memo(RunTerminalCardImpl);
