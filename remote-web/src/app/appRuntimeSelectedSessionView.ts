import type { ConnectionSessionPhase } from "../connection/types.ts";
import type { MsgFetchClient } from "../events/msgFetch.ts";
import type { LocalAnswerOverride } from "../ui/stream/decisionCardView.ts";
import type { DesktopPresence } from "../ui/connection/ConnectionBanner.tsx";
import { deriveSessionStreamProps } from "../ui/stream/streamSource.ts";
import { deriveSendBadge } from "./sendBadge.ts";
import { deriveStopBadge } from "./stopBadge.ts";
import type { AppRuntimeCore } from "./appRuntimeCore.ts";
import type { CommandChannel } from "./commandChannel.ts";
import type { useAppRuntimeMsgFetch } from "./useAppRuntimeMsgFetch.ts";

interface SelectedSessionViewParams {
  selectedSessionId: string;
  core: AppRuntimeCore;
  commandChannel: CommandChannel;
  connectionPhase: ConnectionSessionPhase;
  desktopPresence: DesktopPresence;
  msgFetchClient: MsgFetchClient;
  loadFullTextViaCacheOrFetch: ReturnType<typeof useAppRuntimeMsgFetch>["loadFullTextViaCacheOrFetch"];
}

export function deriveSelectedSessionView({
  selectedSessionId,
  core,
  commandChannel,
  connectionPhase,
  desktopPresence,
  msgFetchClient,
  loadFullTextViaCacheOrFetch,
}: SelectedSessionViewParams) {
  const sessionProjection = core.sessionProjections.get(selectedSessionId);
  // The criterion must be whether this run is still active (`track.runId !== null`),
  // rather than whether runTracks has an entry for the session. Even an idle snapshot
  // causes `runWatermark.ts::idleRunTrackState()` to write an entry with a fresh
  // `LiveBlockReducer` and `runId === null`. That reducer's `snapshotBlocks()` returns
  // `[]`, not `null`; the `streamSource.ts` contract is `null` for no active message
  // and `[]` for a typing tail message. Checking only whether the entry exists with
  // `?? null` would leave typing visible forever after any idle snapshot response.
  const runTrack = core.runTracks.get(selectedSessionId);
  const liveReducer = runTrack && runTrack.runId !== null ? runTrack.reducer : null;
  // `session.index` frames only apply to the room-level `core.indexProjection` (see `sessions`
  // above), never to per-session `core.sessionProjections` — its `.sessions` Map is always empty.
  // The header's fallback status must be explicitly read from `core.indexProjection.sessions` and
  // passed to `deriveSessionStreamProps` (it no longer reads `projection.sessions` itself, see
  // `streamSource.ts`'s header) — this is where that fallback gets wired to real data.
  const sessionIndexStatus = core.indexProjection.sessions.get(selectedSessionId)?.status ?? null;
  // `sessionProjection` exists only after a msg.completed/card.*/run.status/tool.completed
  // milestone (`appRuntimeCore.ts::getOrCreateSessionProjection` is produced in those
  // branches). A control.snapshot response (a "snapshot" frame with `kind=event`)
  // writes only runTracks and can arrive before any milestone (M0 §6 step 2 sends
  // control.snapshot for every running session; the desktop's partial reduction may
  // be the session's first actual content). Do not discard `liveReducer` when
  // `sessionProjection` is absent: selecting a running session before msg.completed
  // arrives is a normal ordering window and must not appear falsely empty. Likewise,
  // do not hardcode `running` to false: session.index may already say the session is
  // running before any milestone arrives. Apply the same `sessionIndexStatus` fallback
  // here so that window does not falsely display Idle.
  const streamProps = sessionProjection
    ? deriveSessionStreamProps(sessionProjection, selectedSessionId, liveReducer, sessionIndexStatus)
    : {
        sessionId: selectedSessionId,
        messages: [],
        running: sessionIndexStatus === "running",
        liveBlocks: liveReducer ? liveReducer.snapshotBlocks() : null,
        decisionCards: [],
        historyCursor: null,
        historyExhausted: false,
      };

  // Local decision-card answer overrides: look up only decisionIds on cards actually
  // shown on this screen. Recompute on every render because the count is only the
  // number of currently visible decision cards, so a separate memo is not worthwhile.
  // `getAnswerOverride()` now returns the selected `option` as well; pass it through
  // unchanged to `decisionCardView.ts`.
  const decisionAnswerOverrides = new Map<string, LocalAnswerOverride>();
  for (const card of streamProps.decisionCards) {
    const override = commandChannel.getAnswerOverride(card.decisionId);
    if (override) decisionAnswerOverrides.set(card.decisionId, override);
  }

  const sendBadge = deriveSendBadge(
    commandChannel.getSendState(selectedSessionId),
    connectionPhase,
    selectedSessionId,
    commandChannel,
    desktopPresence,
  );
  const stopBadge = deriveStopBadge(commandChannel.getStopState(selectedSessionId), selectedSessionId, commandChannel);

  // Only look up state for messages currently on screen that carry a `content_ref` (same approach
  // as `decisionAnswerOverrides` above — recomputed every render, not worth a separate memo).
  const msgFetchStates = new Map<number, ReturnType<MsgFetchClient["getState"]>>();
  for (const message of streamProps.messages) {
    if (message.contentRef === undefined) continue;
    msgFetchStates.set(message.messageId, msgFetchClient.getState(message.messageId));
  }
  const onLoadFullText = (messageId: number) => {
    const message = streamProps.messages.find((m) => m.messageId === messageId);
    // In the stale_revision terminal state, retry using the new version indicated by
    // `current_ref` (M0 §10.5). `message.contentRef` still has the old revision until
    // a new msg.completed arrives; once that happens, the "load full text" entry is
    // no longer shown. The error frame's `currentRef` is therefore required to avoid
    // retrying the same revision that will inevitably be stale again.
    const staleCurrentRef = msgFetchStates.get(messageId)?.currentRef;
    const revision = staleCurrentRef?.revision ?? message?.contentRef?.revision;
    if (revision === undefined) return;
    // The manual "load full text" button also checks the body cache first; a hit
    // avoids fetching. It shares `loadFullTextViaCacheOrFetch()` with auto-fetch (see
    // that function's header). Prefer the new pointer's `contentSha256` from
    // `staleCurrentRef` in the stale_revision terminal state, then fall back to the
    // message's current `contentRef`.
    const contentSha256 = staleCurrentRef?.content_sha256 ?? message?.contentRef?.content_sha256;
    loadFullTextViaCacheOrFetch(selectedSessionId, messageId, revision, contentSha256);
  };

  return {
    streamProps,
    decisionAnswerOverrides,
    sendBadge,
    stopBadge,
    msgFetchStates,
    onLoadFullText,
  };
}
