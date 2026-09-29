import { useEffect, useMemo, type RefObject } from "react";
import { MSG_FETCH_AUTO_THRESHOLD_BYTES, type MsgFetchClient } from "../events/msgFetch.ts";
import type { ProjectedMessage } from "../events/milestoneProjection.ts";
import type { HistoryLoadError } from "../ui/stream/SessionStreamScreen.tsx";
import { DebugPanel, isDebugPanelEnabled, type ConnectionDiagnosticsSnapshot } from "../ui/debug/DebugPanel.tsx";
import { getFrameDiagnostics, type AppRuntimeCore } from "./appRuntimeCore.ts";
import type { PendingHistoryRequest } from "./AppRuntime.tsx";
import type { useAppRuntimeMsgFetch } from "./useAppRuntimeMsgFetch.ts";

interface AppRuntimeDerivedStateParams {
  core: AppRuntimeCore;
  connectionDiagnostics: ConnectionDiagnosticsSnapshot;
  selectedSessionId: string | null;
  historyRevision: number;
  pendingHistoryRequestRef: RefObject<Map<string, PendingHistoryRequest>>;
  historyErrorRef: RefObject<Map<string, HistoryLoadError>>;
  msgFetchClient: MsgFetchClient;
  loadFullTextViaCacheOrFetch: ReturnType<typeof useAppRuntimeMsgFetch>["loadFullTextViaCacheOrFetch"];
}

export function useAppRuntimeDerivedState({
  core,
  connectionDiagnostics,
  selectedSessionId,
  historyRevision,
  pendingHistoryRequestRef,
  historyErrorRef,
  msgFetchClient,
  loadFullTextViaCacheOrFetch,
}: AppRuntimeDerivedStateParams) {
  // Read the current Map contents directly, without useMemo: every render triggered by
  // `forceRender()` must inspect the Map again. It is mutated in place, so useMemo would
  // see the same reference and leave the initial empty list frozen in place.
  // Archived sessions stay out of the list, matching the desktop `App.tsx` filter
  // (`sessions.filter((s) => !s.archived)`). Filter in the frontend without changing the
  // backend snapshot SQL. `MilestoneProjection.applySessionIndex` updates each row's
  // `archived` flag in place for session.index archived/unarchived changes (see
  // `events/milestoneProjection.ts`), so this always reads the latest value.
  const sessions = Array.from(core.indexProjection.sessions.values()).filter((s) => !s.archived);
  // The full snapshot's top-level summary of the currently remote-controlled project:
  // read it directly like `sessions`, without useMemo, for the same reason.
  const activeRepo = core.indexProjection.activeRepo;
  const debugPanel = isDebugPanelEnabled() ? (
    <DebugPanel diagnostics={getFrameDiagnostics(core)} connectionDiagnostics={connectionDiagnostics} />
  ) : null;

  // `historyRevision` gates historyLoading/historyError. Instead of relying on a
  // render-time ref read to coincide with some `forceRender()` call, these values
  // explicitly depend on React state. Changing `historyRevision` guarantees both
  // values are recomputed. The two refs remain the sole sources of truth (their Maps
  // are mutated in place, as noted where they are defined); this state dependency
  // explicitly determines when to read them again. Call before any early return
  // to satisfy the Rules of Hooks.
  const historyLoading = useMemo(
    () => selectedSessionId !== null && pendingHistoryRequestRef.current.has(selectedSessionId),
    [selectedSessionId, historyRevision],
  );
  const historyError = useMemo(
    () => (selectedSessionId !== null ? historyErrorRef.current.get(selectedSessionId) ?? null : null),
    [selectedSessionId, historyRevision],
  );

  // Auto-fetch the newest ref-bearing message in the viewport. `SessionStreamScreen` has a single
  // scroll container with no virtualization/visibility observation (`useStickToBottom` only keeps
  // the view pinned to bottom, it doesn't track which message is actually inside the viewport), so
  // the best proxy for "newest in viewport" here is "largest messageId in the current session" —
  // normal stick-to-bottom scrolling keeps it visible anyway. No dependency array (runs every
  // render) is intentional: `projection.messages` is a Map mutated in place, not React state, so
  // there's no cheap "did it change" signal; the guards below are idempotent (already
  // loading/fetched/over threshold all short-circuit), so repeated runs never double-fetch.
  //
  // The candidate must be found by scanning **all** messages for the largest messageId first
  // (not pre-filtered by contentRef/fullBlocks), then checked for an unfetched content_ref under
  // the size threshold — filtering out already-fetched messages before finding the max instead
  // answers "largest messageId among the unfetched", so once the true latest message finishes
  // fetching it drops out of the candidate set and the next-newest gets misfetched as "latest".
  useEffect(() => {
    if (selectedSessionId === null) return;
    const projection = core.sessionProjections.get(selectedSessionId);
    if (!projection) return;
    let latestMessage: ProjectedMessage | null = null;
    for (const message of projection.messages.values()) {
      if (latestMessage === null || message.messageId > latestMessage.messageId) {
        latestMessage = message;
      }
    }
    if (latestMessage === null) return;
    if (latestMessage.contentRef === undefined || latestMessage.fullBlocks !== undefined) return;
    if (latestMessage.contentRef.total_bytes > MSG_FETCH_AUTO_THRESHOLD_BYTES) return;
    if (msgFetchClient.getState(latestMessage.messageId).status !== "idle") return;
    // Check the body cache before deciding whether to auto-fetch; a hit avoids the
    // fetch. `loadFullTextViaCacheOrFetch()` uses `cacheLookupPendingRef` to block
    // duplicate lookups when this effect reruns on every render. This preserves the
    // existing "repeated runs never double-fetch" behavior described above, extending
    // the pending gate from `msgFetchClient.activeSessions` to both cache lookup and fetch.
    loadFullTextViaCacheOrFetch(
      selectedSessionId,
      latestMessage.messageId,
      latestMessage.contentRef.revision,
      latestMessage.contentRef.content_sha256,
    );
  });

  return { sessions, activeRepo, debugPanel, historyLoading, historyError };
}
