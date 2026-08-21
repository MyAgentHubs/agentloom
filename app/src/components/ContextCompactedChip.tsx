import { memo } from "react";
import { useI18n } from "../i18n";
import type { Block } from "../types/agent";

/** T7a：一行上下文提示块的两种形态——压实（无损）与截断（有损告警）。 */
export type ContextChipType = Extract<
  Block["type"],
  "context_compacted" | "context_truncated"
>;

/** 压实 = 静默的 ink-3 + 半透明琥珀箭头；截断 = 有损，用琥珀警示墨色 + 实心不透明图标。 */
const TONE: Record<
  ContextChipType,
  { className: string; color: string; iconOpacity: number; path: string }
> = {
  context_compacted: {
    className: "context-compacted-chip",
    color: "var(--ink-3)",
    iconOpacity: 0.72,
    path: "M2 3.5h8M4 1.5 2 3.5l2 2M10 8.5H2M8 6.5l2 2-2 2",
  },
  context_truncated: {
    className: "context-truncated-chip",
    color: "var(--amber-ink)",
    iconOpacity: 1,
    // 警示三角 + 感叹号：与压实的「双向箭头」在形状上一眼可分
    path: "M6 1.5 11 10.5H1L6 1.5ZM6 5v2.5M6 9v0.01",
  },
};

function ContextCompactedChipImpl({
  blockType = "context_compacted",
}: {
  blockType?: ContextChipType;
}) {
  const { t } = useI18n();
  const tone = TONE[blockType];
  const label =
    blockType === "context_truncated"
      ? t("contextTruncated.label")
      : t("contextCompacted.label");

  return (
    <div
      className={tone.className}
      role="status"
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        gap: 5,
        margin: "5px 0",
        color: tone.color,
        fontSize: 11,
        lineHeight: 1.4,
      }}
    >
      <svg
        aria-hidden="true"
        width="12"
        height="12"
        viewBox="0 0 12 12"
        fill="none"
        stroke="var(--amber)"
        strokeWidth="1.25"
        strokeLinecap="round"
        strokeLinejoin="round"
        style={{ opacity: tone.iconOpacity, flexShrink: 0 }}
      >
        <path d={tone.path} />
      </svg>
      <span>{label}</span>
    </div>
  );
}

export const ContextCompactedChip = memo(ContextCompactedChipImpl);
