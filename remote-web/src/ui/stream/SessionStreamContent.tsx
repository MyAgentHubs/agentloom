import { useEffect, useMemo, useReducer, useState } from "react";
import { useI18n as useAppI18n, type Locale as AppLocale } from "@app/i18n";
import type { AttachmentPort } from "@app/lib/remoteSessionPort";
import { RunLeadTurn } from "@app/components/RunLeadTurn";
import type { Block } from "@app/types/agent";
import type { SessionStreamProps } from "./streamSource.ts";
import type { MsgFetchState } from "../../events/msgFetch.ts";
import { groupDecisionCardsIntoTurns, type LocalAnswerOverride } from "./decisionCardView.ts";
import { useStickToBottom } from "./useStickToBottom.ts";
import { useI18n } from "../i18n.ts";
import { ErrorBoundary } from "../ErrorBoundary.tsx";
import { MessageRow, LiveMessageRow, StopIcon, StopBadgeView, STOP_UNCERTAIN_THRESHOLD_MS, type StreamStopBadge } from "./sessionStreamMessageRows.tsx";
import "./SessionStreamScreen.css";

/** Trigger exactly one re-render at the moment the threshold is reached (a one-shot timer, with no continuous polling) — the same existing approach as
 *  `useThresholdTick` in `Composer.tsx` (each file keeps its own small utility function to avoid cross-file coupling; see the U4 delivery notes). */
function useThresholdTick(sentAtMs: number | undefined, thresholdMs: number, active: boolean, now: () => number): void {
  const [, forceTick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!active || sentAtMs === undefined) return;
    const remaining = sentAtMs + thresholdMs - now();
    if (remaining <= 0) return;
    const timer = setTimeout(forceTick, remaining);
    return () => clearTimeout(timer);
  }, [sentAtMs, thresholdMs, active]);
}

export interface SessionStreamScreenProps extends SessionStreamProps {
  /** Inject an alternative implementation for tests / future real WS wiring; when omitted, use the default fallback from `createWebAttachmentPort()`. */
  attachmentPort?: AttachmentPort;
  /** Initial locale for `I18nProvider`; omitted = use the system-language auto-detection from `@app/i18n` (the same detection
   *  logic as the desktop). If the `I18nProvider` locale changes after mounting (such as through a future language-switching UI), the shell copy follows it rather than
   *  taking effect only once at mount time — see the "shared locale source" section in the file header comment. */
  initialLocale?: AppLocale;
  /** T6f3: Callback that actually sends `input.answer` — when omitted, `DecisionCard` retains the existing disabled state from T6f2 (see the
   *  "answer card activation" section in the file header comment). */
  onDecisionChoose?: (decisionId: string, option: string) => void;
  /** T6f3: Local display-state overrides for in-flight answers on this machine — passed through to `decisionCardView.ts::groupDecisionCardsIntoTurns`. */
  decisionAnswerOverrides?: ReadonlyMap<string, LocalAnswerOverride>;
  historyLoading?: boolean;
  historyError?: HistoryLoadError | null;
  onLoadEarlier?: () => void;
  /** U4: Called after clicking "confirm stop" on the Stop button on the right side of the header while running — when omitted, the confirmation state can still be shown/hidden,
   *  but clicking confirm has no effect (the same graceful-degradation approach as existing optional callbacks such as `onLoadEarlier`). */
  onStop?: () => void;
  stopBadge?: StreamStopBadge | null;
  /** Full-text fetch state indexed by messageId — `undefined` (prop omitted, or missing from the
   *  Map) means idle (same fallback as `MsgFetchClient.getState()`). When omitted, the preview
   *  card's "load full text" button still renders but never leaves the not-started state, since
   *  nothing calls `onLoadFullText` (same degrade-gracefully posture as `onLoadEarlier`). */
  msgFetchStates?: ReadonlyMap<number, MsgFetchState>;
  /** Called on a "load full text" click. Auto-triggering for "the newest message in the viewport"
   *  is the caller's job (`AppRuntime.tsx`'s orchestration) — this pure-presentation component has
   *  no built-in auto-trigger logic. */
  onLoadFullText?: (messageId: number) => void;
  /** Expansion-state toggle for the L1 activity-summary chip — the current value of the "Show detailed activity" setting
   *  (`ui/settings/SettingsScreen.tsx`), read from the localStorage
   *  preference by the caller (`AppRuntime.tsx`) and passed through unchanged. Omitted/`false` = collapsed (the default; see `MessageRow`/`ActivitySummaryChip`). */
  verboseEnabled?: boolean;
  /** Return to the session list — rendered as the first child of `.stream-screen__head` (it was previously a bare button in `AppRuntime.tsx` that
   *  occupied an entire row by itself inside `.app-runtime-stream`, stacking with this screen's own header to form two top bars).
   *  When omitted, the back button is not rendered (the same graceful-degradation approach as existing optional callbacks such as `onLoadEarlier`; tests/future standalone usage scenarios are not
   *  forced to have the concept of "back"). */
  onBack?: () => void;
  /** Clock injected by tests — when omitted, use the real `Date.now()` (the same existing approach as `Composer.tsx`). */
  now?: () => number;
}

export type HistoryLoadError = "timeout" | "failed" | "rateLimited" | "desktopOffline" | "quota" | "notConnected";

/**
 * Content body — exported for direct composition in tests (the testability requirement in incremental rework item 3): tests can wrap this component in an `I18nProvider`
 * from `@app/i18n` (with a probe component that can call its `setLocale`) and verify that shell
 * copy follows locale switches, without having to split out another layer of top-level component provider-mounting logic. The production path (`SessionStreamScreen`) and
 * the test path share the same component, so their behavior cannot drift apart.
 */
export function SessionStreamContent({
  sessionId,
  messages,
  running,
  liveBlocks,
  decisionCards,
  onDecisionChoose,
  decisionAnswerOverrides,
  historyCursor = null,
  historyExhausted = false,
  historyLoading = false,
  historyError = null,
  onLoadEarlier,
  onStop,
  stopBadge = null,
  onBack,
  now = Date.now,
  msgFetchStates,
  onLoadFullText,
  verboseEnabled = false,
}: SessionStreamProps &
  Pick<
    SessionStreamScreenProps,
    | "onDecisionChoose"
    | "decisionAnswerOverrides"
    | "historyLoading"
    | "historyError"
    | "onLoadEarlier"
    | "onStop"
    | "stopBadge"
    | "onBack"
    | "now"
    | "msgFetchStates"
    | "onLoadFullText"
    | "verboseEnabled"
  >) {
  // Shared locale source (the "shared locale source" section in the file header comment): read the current value from the mounted desktop I18nProvider rather than detecting it independently.
  const { locale } = useAppI18n();
  const { t } = useI18n(locale);
  const [confirmingStop, setConfirmingStop] = useState(false);
  useThresholdTick(stopBadge?.sentAtMs, STOP_UNCERTAIN_THRESHOLD_MS, stopBadge?.status === "sending", now);
  // After running flips to false (Stop actually takes effect and the run.status milestone arrives), the confirmation state is no longer meaningful — hide it (the same existing behavior moved out of the old
  // `Composer.tsx`).
  useEffect(() => {
    if (!running) setConfirmingStop(false);
  }, [running]);
  // Trigger signals for the stick-to-bottom decision: a change in message count + the "content fingerprint" of live blocks (length alone is insufficient — while text accumulates within the same live message,
  // the array length may remain unchanged even though its content changes, so whether to stick to the bottom must still be reevaluated).
  const liveFingerprint = liveBlocks === null ? "none" : JSON.stringify(liveBlocks);
  const scrollRef = useStickToBottom<HTMLDivElement>(`${messages.length}:${liveFingerprint}`);
  const isEmpty = messages.length === 0 && liveBlocks === null && decisionCards.length === 0;
  const decisionTurns = useMemo(
    () => groupDecisionCardsIntoTurns(decisionCards, decisionAnswerOverrides),
    [decisionCards, decisionAnswerOverrides],
  );
  let historyButtonLabel = t("history.loadEarlier");
  if (historyError) historyButtonLabel = t("history.retry");
  if (historyLoading) historyButtonLabel = t("history.loading");
  // U3 do not keep the button present for an empty session: when there is not yet any "meaningful history signal" (no loaded messages and no cursor — meaning the desktop
  // has never returned a history frame, is not loading, and has never produced an error), showing a button that does nothing when clicked would only mislead the user.
  // As soon as any of the following signals appears — messages already exist (even if merely waiting to page backward), a cursor has been received, loading is in progress, or the last attempt errored and the user should be allowed
  // to retry — the button should be present (the existing outer condition `!historyExhausted` remains unchanged; the two conditions have an AND relationship).
  const hasHistorySignal =
    messages.length > 0 || historyCursor !== null || historyLoading || historyError !== null;

  return (
    <div className="stream-screen" data-testid="session-stream-screen">
      <header className="stream-screen__head">
        {/* Return to the session list — it was previously a bare button in `AppRuntime.tsx` that occupied an entire row by itself and stacked with the header here to form
            two top bars; it is now the first child of this header itself (retaining the `.app-runtime-back` style class +
            the existing data-testid, so the caller only needs to pass `onBack` instead and the DOM anchor remains unchanged). */}
        {onBack && (
          <button
            type="button"
            className="app-runtime-back"
            data-testid="app-runtime-back-to-sessions"
            aria-label={t("stream.backToSessions")}
            onClick={onBack}
          >
            {"‹"}
          </button>
        )}
        <span
          className={`stream-status-dot stream-status-dot--${running ? "running" : "idle"}`}
          aria-hidden="true"
        />
        <span className="stream-screen__status-label" data-testid="stream-status-label">
          {running ? t("stream.statusRunning") : t("stream.statusIdle")}
        </span>
        {running && (
          <div className="stream-screen__stop-wrap" data-testid="stream-stop-row">
            {!confirmingStop ? (
              <button
                type="button"
                className="stream-screen__stop"
                data-testid="stream-stop-button"
                aria-label={t("composer.stop")}
                // Rework item ③, point ③, moved here: "disable repeated confirmation while Stop is in flight after confirmation" — after it has been confirmed once and before the command has received a
                // result (`stopBadge.status==="sending"`), clicking Stop no longer opens the confirmation popover again.
                disabled={stopBadge?.status === "sending"}
                onClick={() => setConfirmingStop(true)}
              >
                <StopIcon />
              </button>
            ) : (
              <div className="stream-screen__stop-confirm" data-testid="stream-stop-confirm">
                <span>{t("composer.stopConfirm")}</span>
                <button
                  type="button"
                  data-testid="stream-stop-confirm-yes"
                  onClick={() => {
                    setConfirmingStop(false);
                    onStop?.();
                  }}
                >
                  {t("composer.stopConfirmYes")}
                </button>
                <button
                  type="button"
                  data-testid="stream-stop-confirm-cancel"
                  onClick={() => setConfirmingStop(false)}
                >
                  {t("composer.stopConfirmCancel")}
                </button>
              </div>
            )}
          </div>
        )}
      </header>
      {running && stopBadge && (
        <div className="stream-screen__stop-badge-row">
          <StopBadgeView badge={stopBadge} now={now} t={t} />
        </div>
      )}
      <div className="stream-scroll" ref={scrollRef} data-testid="stream-scroll">
        {!historyExhausted && hasHistorySignal && (
          <>
            <button
              type="button"
              className="stream-history-load"
              data-testid="history-load-earlier"
              disabled={historyLoading}
              onClick={onLoadEarlier}
            >
              {historyButtonLabel}
            </button>
            {historyError !== null && (
              <p className="stream-history-error" data-testid="history-load-error" role="alert">
                {t(`history.error.${historyError}`)}
              </p>
            )}
          </>
        )}
        {isEmpty && (
          <p className="stream-empty" data-testid="stream-empty">
            {t("stream.empty")}
          </p>
        )}
        {messages.map((msg) => (
          // Message-level ErrorBoundary — if rendering one message crashes, only that message is lost (degrading to a one-line notice),
          // without affecting sibling messages or the outer shell (see the header comment in `../ErrorBoundary.tsx`). `key` uses messageId, the same as the original key on
          // `MessageRow` above, so a boundary that has crashed once will not be unexpectedly remounted and reset because of other list changes.
          <ErrorBoundary
            key={msg.messageId}
            fallback={
              <div className="stream-msg stream-msg--error" data-testid="stream-msg-error">
                {t("stream.messageRenderError")}
              </div>
            }
          >
            <MessageRow
              sessionId={sessionId}
              msg={msg}
              t={t}
              fetchState={msgFetchStates?.get(msg.messageId)}
              onLoadFullText={onLoadFullText}
              verboseEnabled={verboseEnabled}
            />
          </ErrorBoundary>
        ))}
        {decisionTurns.length > 0 && (
          <div className="stream-decisions" data-testid="stream-decision-cards">
            {decisionTurns.map((turn) => (
              // T6f3: When `onDecisionChoose` has a value, DecisionCard.tsx's `disabled = ... || !onChoose`
              // naturally becomes clickable; when the caller does not pass it (the existing T6f2 behavior), it naturally remains disabled — this line itself does not make the decision; the switch is on
              // the desktop component's side (see the "answer card activation" section in the file header comment).
              <RunLeadTurn key={turn.runId} turn={turn} sessionId={sessionId} onDecisionChoose={onDecisionChoose} />
            ))}
          </div>
        )}
        {/* U2 evaluation conclusion (the defense-in-depth `running &&` was not adopted; see this task's delivery notes): `running` (derived from the
            `run.status` milestone) and `liveBlocks !== null` (derived from `runTrack.runId`; see
            the U2 root-fix comment in `AppRuntime.tsx`) are two independent signals. The "snapshot responses may
            arrive before any milestone" section in the `AppRuntime.tsx` header comment explicitly documents the legitimate race window in which they diverge — for a newly selected running session,
            a `control.snapshot` response carrying a nonempty `run_id` arrives first and the `run.status` milestone arrives later. During this window,
            `running` is still false while `liveBlocks` is already legitimately non-null (the
            "receives epoch.changed after sending the control.snapshot request" case in `AppRuntime.e2e.test.tsx` uses the visible typing bubble in precisely this window
            to assert that "the snapshot has landed"). Adding `running &&` here would incorrectly swallow the typing that should be shown
            during this legitimate window — it would not be a fix, but another layer of the same class of error already explicitly rejected in the file header comment. A second signal genuinely independent of `runTrack`
            can only come from how `running` itself is calculated (`streamSource.ts::deriveSessionStreamProps`),
            and that file is outside this task's SCOPE; see the delivery notes. */}
        {liveBlocks !== null && (
          <ErrorBoundary
            fallback={
              <div className="stream-msg stream-msg--error" data-testid="stream-msg-error">
                {t("stream.messageRenderError")}
              </div>
            }
          >
            <LiveMessageRow sessionId={sessionId} blocks={liveBlocks as Block[]} typingLabel={t("stream.typing")} />
          </ErrorBoundary>
        )}
      </div>
    </div>
  );
}
