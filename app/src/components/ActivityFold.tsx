import { useState } from "react";
import type { Segment, Verbosity } from "../lib/streamItems";
import { humanizeToolName } from "../lib/toolLabel";
import { useI18n } from "../i18n";
import { MessageContent } from "./MessageContent";

type FoldSegment = Extract<Segment, { kind: "activity_fold" }>;
type FoldApproval = Extract<
  FoldSegment["blocks"][number],
  { type: "approval" }
>;

type Props = {
  fold: FoldSegment;
  verbosity: Verbosity;
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
};

const LIVE_SUMMARY_MAX_CHARS = 40;

function firstLine(text: string): string {
  const idx = text.indexOf("\n");
  return idx >= 0 ? text.slice(0, idx) : text;
}

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

function ChevronIcon({ open }: { open: boolean }) {
  return (
    <svg
      className="activity-fold__chevron"
      viewBox="0 0 24 24"
      width="11"
      height="11"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      aria-hidden
      style={{
        transform: open ? "rotate(90deg)" : "none",
        transition: "transform 0.15s ease",
      }}
    >
      <path d="M4 17l6-6-6-6" />
    </svg>
  );
}

function ActivityFoldImpl({
  fold,
  verbosity,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
}: Props) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const { counts, live } = fold;

  const countSegments: { text: string; warn: boolean }[] = [];
  if (counts.tools > 0) {
    // P2-1：英文单复数——n===1 用单数键，否则用复数键（zh 两个键文案相同，无影响）。
    countSegments.push({
      text: t(
        counts.tools === 1
          ? "activity.fold.toolCount"
          : "activity.fold.toolsCount",
        { n: counts.tools },
      ),
      warn: false,
    });
  }
  if (counts.failed > 0) {
    countSegments.push({
      text: t("activity.fold.failedCount", { n: counts.failed }),
      warn: true,
    });
  }
  if (counts.interrupted > 0) {
    countSegments.push({
      text: t("activity.fold.interruptedCount", { n: counts.interrupted }),
      warn: true,
    });
  }
  if (counts.rejected > 0) {
    countSegments.push({
      text: t("activity.fold.rejectedCount", { n: counts.rejected }),
      warn: true,
    });
  }
  if (counts.thinking > 0) {
    countSegments.push({
      text: t("activity.fold.thinkingCount", { n: counts.thinking }),
      warn: false,
    });
  }

  const isLive = live !== undefined;
  const runningLabel =
    isLive && live
      ? t("activity.fold.running", {
          tool: humanizeToolName(live.tool, t),
          summary: truncate(firstLine(live.summary), LIVE_SUMMARY_MAX_CHARS),
        })
      : null;
  // approval_resolved 与后续 tool_started 是两个独立事件。两者之间的瞬态里，
  // approved approval 会形成零计数 fold；复用准确的已处理文案，避免空白 chip，
  // 但绝不把 approval 伪计成一次工具调用。
  const approvedApproval = fold.blocks.find(
    (block): block is FoldApproval =>
      block.type === "approval" && block.status === "approved",
  );
  const approvedLabel = approvedApproval
    ? t(
        approvedApproval.request_kind === "criterion"
          ? "approvalCard.approvedCriterion"
          : "approvalCard.approvedCommand",
      )
    : null;

  return (
    <div
      className={`activity-fold${isLive ? " activity-fold--live" : ""}`}
      data-verbosity={verbosity}
    >
      <button
        type="button"
        aria-expanded={open}
        className={`activity-fold__chip${
          isLive ? " activity-fold__chip--live" : ""
        }`}
        onClick={() => setOpen((current) => !current)}
      >
        <ChevronIcon open={open} />
        {isLive && (
          <span className="activity-fold__spinner" aria-hidden="true" />
        )}
        {runningLabel ? (
          <span className="activity-fold__running">{runningLabel}</span>
        ) : countSegments.length === 0 && approvedLabel ? (
          <span className="activity-fold__count">{approvedLabel}</span>
        ) : (
          countSegments.map((segment, index) => (
            <span
              key={index}
              className={`activity-fold__count${
                segment.warn ? " activity-fold__count--warn" : ""
              }`}
            >
              {segment.text}
            </span>
          ))
        )}
      </button>
      {open && (
        <div className="activity-fold__body">
          <MessageContent
            blocks={fold.blocks}
            verbosity="full"
            suppressArtifacts
            sessionId={sessionId}
            onOpenPreview={onOpenPreview}
            onOpenLightbox={onOpenLightbox}
          />
        </div>
      )}
    </div>
  );
}

export const ActivityFold = ActivityFoldImpl;
export type { Props as ActivityFoldProps };
