import { MessageContent } from "@app/components/MessageContent";
import { AgentAvatar } from "@app/components/AgentAvatar";
import type { Block } from "@app/types/agent";
import type { SessionStreamMessage } from "./streamSource.ts";
import type { MsgFetchState } from "../../events/msgFetch.ts";
import { MSG_FETCH_ERROR_CODES } from "../../events/parseFrame.ts";
import { extractActivitySummary, type ActivitySummaryBlock } from "./activitySummary.ts";
import type { I18nHookValue } from "../i18n.ts";

// ---------------------------------------------------------------------------
// Mobile composer and shell layout rearrangement: The Stop button moved from the bottom of `ui/composer/Composer.tsx` to the right side of the
// header here — the original specification (the old Composer.tsx header comment) already said "right side of the top session row = Stop
// button (visible while running, with two-step confirmation)"; the previous implementation put it in the wrong place (symptom on a real device: Send/Stop stacked in two right-aligned layers).
// The two-step confirmation interaction + `stopBadge`'s "truthful timing" display logic were moved verbatim (the 150s truthful-clock threshold is the same), not rewritten.
// ---------------------------------------------------------------------------

export type StreamStopStatus = "sending" | "queued" | "failed" | "rate_limited";

export interface StreamStopBadge {
  commandId: string;
  status: StreamStopStatus;
  sentAtMs: number;
  onRetry?: () => void;
}

/** Truthful declaration: "maximum receive window under a truthful clock ≈ 30s + 120s ≈ 150s." */
export const STOP_UNCERTAIN_THRESHOLD_MS = 150_000;

/**
 * `MessageContent`/`GateCard` use this only as a boolean switch meaning "non-null is read-only" (`readonly = readonlyReason
 * != null`); the string content is never rendered to the user — it does not need to go through i18n, and a stable technical sentinel string is sufficient (see the file header comment).
 */
const READONLY_REASON = "remote-web-readonly-stream";

/** Blocks of type approval / scope_change / decision_card — they have no built-in disabled mechanism, so the shell must wrap them in a read-only
 *  presentation (see the file header comment). Add `decision_card` so all three clients (and all three actionable block types) use the same
 *  read-only policy of "visible, not clickable, and clear about where to act" — the old version omitted this type, and the reasoning about lacking a disabled mechanism applies equally to
 *  `decision_card`; it should not cover only the first two types. */
function hasRestrictedBlock(blocks: unknown[]): boolean {
  return blocks.some((block) => {
    if (typeof block !== "object" || block === null) return false;
    const type = (block as { type?: unknown }).type;
    return type === "approval" || type === "scope_change" || type === "decision_card";
  });
}

export function MessageRow({
  sessionId,
  msg,
  t,
  fetchState,
  onLoadFullText,
  verboseEnabled,
}: {
  sessionId: string | null;
  msg: SessionStreamMessage;
  t: I18nHookValue["t"];
  fetchState?: MsgFetchState;
  onLoadFullText?: (messageId: number) => void;
  verboseEnabled?: boolean;
}) {
  const isUser = msg.role === "user";
  const restricted = hasRestrictedBlock(msg.blocks);
  // L1 activity summary — not a new frame type, but a msg.completed containing exactly one
  // `activity_summary` block (the "single-point definition" in the `activitySummary.ts` header comment). `restricted`
  // is naturally mutually exclusive with it (the L0 guard function in `extractActivitySummary` already excludes messages containing actionable blocks),
  // so the two decisions are read independently and do not need to guard each other with &&.
  const activitySummary = extractActivitySummary(msg.blocks);
  return (
    <div
      className={`stream-msg stream-msg--${isUser ? "user" : "agent"}`}
      data-testid="stream-message"
      data-role={msg.role}
      data-message-id={msg.messageId}
    >
      {!isUser && <AgentAvatar kind={msg.agent ?? msg.role} />}
      <div className="stream-msg__bubble">
        {activitySummary ? (
          <ActivitySummaryChip summary={activitySummary} verbose={verboseEnabled ?? false} t={t} />
        ) : (
          <div
            data-testid={restricted ? "stream-restricted-content" : undefined}
            // pointer-events:none is the mechanism that actually makes this "not clickable" (in a real browser, it completely blocks every
            // onClick inside this layer, including ApprovalCard's Allow/Deny and ScopeChangeCard's "Continue"); opacity is
            // the visual signal that it is "visible, but known not to be in its normal interactive state" — use an inline style instead of an external CSS class so
            // jsdom/RTL tests can assert directly on `style.pointerEvents` without depending on jsdom's parsing of external stylesheets.
            style={restricted ? { pointerEvents: "none", opacity: 0.55 } : undefined}
          >
            <MessageContent blocks={msg.blocks as Block[]} sessionId={sessionId} readonlyReason={READONLY_REASON} />
          </div>
        )}
        {restricted && (
          <div className="stream-restricted-hint" data-testid="stream-restricted-hint">
            {t("stream.restrictedHint")}
          </div>
        )}
        {msg.contentRef && !msg.hasFullText && (
          <MsgFetchFooter
            messageId={msg.messageId}
            totalBytes={msg.contentRef.total_bytes}
            fetchState={fetchState}
            onLoadFullText={onLoadFullText}
            t={t}
          />
        )}
      </div>
      {isUser && <AgentAvatar kind="user" />}
    </div>
  );
}

/** Collapse/expand the L1 activity summary — when `verbose` is false, collapse it into
 *  a one-line chip (tool-call count + failure count + status); when true, expand it into per-category count details (tool calls/MCP
 *  calls/permission requests/failures — the `activity_summary` block contains only aggregate counts, not actual per-tool details; see the
 *  "no promise of per-tool details" section in the `activitySummary.ts` header comment; the "per-category expansion" here is the finest granularity available from the protocol's existing fields).
 *  When a revision update arrives, `msg.blocks` already contains the latest content from that arrival (the higher-revision-wins projection in `MilestoneProjection`;
 *  see that file's header comment). This component simply re-renders from the latest props, so "refreshing the chip in place" requires no additional state,
 *  and the same messageId corresponds to the same DOM position. */
function ActivitySummaryChip({
  summary,
  verbose,
  t,
}: {
  summary: ActivitySummaryBlock;
  verbose: boolean;
  t: I18nHookValue["t"];
}) {
  const stateLabel =
    summary.state === "running"
      ? t("stream.activitySummary.stateRunning")
      : summary.state === "failed"
        ? t("stream.activitySummary.stateFailed")
        : t("stream.activitySummary.stateDone");

  if (!verbose) {
    return (
      <div className="stream-activity-chip" data-testid="activity-summary-chip" data-state={summary.state}>
        <span data-testid="activity-summary-collapsed">
          {t("stream.activitySummary.collapsed", { tools: String(summary.tool_calls), failed: String(summary.failed) })}
        </span>
        <span className="stream-activity-chip__state" data-testid="activity-summary-state">
          {stateLabel}
        </span>
      </div>
    );
  }

  return (
    <div className="stream-activity-chip stream-activity-chip--expanded" data-testid="activity-summary-chip" data-state={summary.state}>
      <span className="stream-activity-chip__state" data-testid="activity-summary-state">
        {stateLabel}
      </span>
      <ul className="stream-activity-chip__detail" data-testid="activity-summary-detail">
        <li data-testid="activity-summary-detail-tools">{t("stream.activitySummary.detailTools", { count: String(summary.tool_calls) })}</li>
        <li data-testid="activity-summary-detail-mcp">{t("stream.activitySummary.detailMcp", { count: String(summary.mcp_calls) })}</li>
        <li data-testid="activity-summary-detail-permission">
          {t("stream.activitySummary.detailPermission", { count: String(summary.permission_prompts) })}
        </li>
        <li data-testid="activity-summary-detail-failed">{t("stream.activitySummary.detailFailed", { count: String(summary.failed) })}</li>
      </ul>
    </div>
  );
}

/** Preview card UI for a message with `content_ref` but no full text yet — "load full text (N KB)"
 *  button / loading / terminal error state. Disappears once fetched, since `msg.blocks` already
 *  folds in the full text (`streamSource.ts::deriveSessionStreamProps`'s
 *  `blocks: m.fullBlocks ?? m.blocks`). */
function MsgFetchFooter({
  messageId,
  totalBytes,
  fetchState,
  onLoadFullText,
  t,
}: {
  messageId: number;
  totalBytes: number;
  fetchState?: MsgFetchState;
  onLoadFullText?: (messageId: number) => void;
  t: I18nHookValue["t"];
}) {
  const status = fetchState?.status ?? "idle";
  const sizeLabel = `${Math.max(1, Math.ceil(totalBytes / 1024))} KB`;

  if (status === "loading") {
    return (
      <div className="stream-msg__fetch-footer" data-testid="msg-fetch-loading">
        {t("stream.msgFetch.loading")}
      </div>
    );
  }

  if (status === "error") {
    // The three cases busy/timeout/stale_revision — retryable (busy/timeout are retryable; stale_revision directs the user
    // to fetch again using the new current_ref, still through the same "click again" entry point, and `onLoadFullText` rereads the latest contentRef).
    // soft_deleted/forbidden/too_large/not_found — terminally unavailable, so no retry button is provided ("show unavailable for soft_deleted/forbidden";
    // too_large/not_found belong to the same category; see the `msgFetch.ts` header comment for the reasoning).
    // If `errorReason` is none of the three retryable values above, none of the other four known
    // terminal values in the protocol's six-value enum, and not `msgFetch.ts`'s own synthesized
    // `"malformed_content"` (also terminal), it's a wire code this client doesn't recognize yet —
    // `parseFrame.ts` no longer rejects unrecognized codes outright (see `parseMsgFetchError`), so
    // this renders it as retryable too rather than silently treating it as a dead end.
    const errorReason = fetchState?.errorReason;
    const isKnownWireCode = errorReason !== undefined && MSG_FETCH_ERROR_CODES.includes(errorReason);
    const isKnownTerminalClientReason = errorReason === "malformed_content";
    const retryable =
      errorReason === "busy" ||
      errorReason === "timeout" ||
      errorReason === "stale_revision" ||
      (errorReason !== undefined && !isKnownWireCode && !isKnownTerminalClientReason);
    const label = fetchState?.errorReason === "stale_revision" ? t("stream.msgFetch.staleRevision") : t("stream.msgFetch.unavailable");
    return (
      <div className="stream-msg__fetch-footer" data-testid="msg-fetch-error" data-error-reason={fetchState?.errorReason}>
        <span>{label}</span>
        {retryable && (
          <button type="button" data-testid="msg-fetch-retry" onClick={() => onLoadFullText?.(messageId)}>
            {t("stream.msgFetch.retry")}
          </button>
        )}
      </div>
    );
  }

  return (
    <button
      type="button"
      className="stream-msg__fetch-load"
      data-testid="msg-fetch-load"
      onClick={() => onLoadFullText?.(messageId)}
    >
      {t("stream.msgFetch.load", { size: sizeLabel })}
    </button>
  );
}

export function LiveMessageRow({
  sessionId,
  blocks,
  typingLabel,
}: {
  sessionId: string | null;
  blocks: Block[];
  typingLabel: string;
}) {
  return (
    <div className="stream-msg stream-msg--agent stream-msg--live" data-testid="stream-live-message">
      <AgentAvatar kind="assistant" />
      <div className="stream-msg__bubble">
        <MessageContent blocks={blocks} sessionId={sessionId} streaming readonlyReason={READONLY_REASON} />
        <span className="stream-typing" data-testid="stream-typing-indicator">
          {typingLabel}
        </span>
      </div>
    </div>
  );
}

/** Linear square icon — U4 iconification (the Stop button no longer shows text and retains accessibility through `aria-label`). Explicit width/height
 *  attributes (a hard-earned lesson in this repository: an unsized SVG can blow out the layout in WKWebView). */
export function StopIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <rect x="5" y="5" width="14" height="14" rx="2" />
    </svg>
  );
}

/** The "truthful timing" display for stopBadge — moved verbatim from `Composer.tsx` (a 150s truthful-clock receive window; after the threshold it does not
 *  declare "expired," only that it "may still be within the effective window"); see the U4 delivery notes. */
export function StopBadgeView({ badge, now, t }: { badge: StreamStopBadge; now: () => number; t: I18nHookValue["t"] }) {
  const elapsed = now() - badge.sentAtMs;
  let label: string;
  let variant: "neutral" | "error" = "neutral";
  switch (badge.status) {
    case "sending":
      label = elapsed >= STOP_UNCERTAIN_THRESHOLD_MS ? t("composer.stopMaybeStillValid") : t("composer.stopSending");
      break;
    case "queued":
      label = t("composer.stopQueued");
      break;
    case "failed":
      label = t("composer.stopFailed");
      variant = "error";
      break;
    case "rate_limited":
      label = t("composer.stopRateLimited");
      variant = "error";
      break;
  }
  return (
    <div className={`stream-screen__stop-badge stream-screen__stop-badge--${variant}`} data-testid="stream-stop-badge" data-status={badge.status}>
      <span>{label}</span>
      {badge.onRetry && (
        <button type="button" data-testid="stream-stop-retry" onClick={badge.onRetry}>
          {t("composer.retry")}
        </button>
      )}
    </div>
  );
}
