import { useCallback, type Dispatch, type RefObject, type SetStateAction } from "react";
import { ReadyState, type WebSocketLike } from "../connection/types.ts";
import { seal } from "../crypto/envelope.ts";
import { utf8Bytes } from "../crypto/bytes.ts";
import { buildControlHistoryRequest } from "../events/parseFrame.ts";
import type { HistoryLoadError } from "../ui/stream/SessionStreamScreen.tsx";
import { getOrCreateSessionProjection, type AppRuntimeCore } from "./appRuntimeCore.ts";
import type { PendingSnapshotRequest, PendingHistoryRequest } from "./AppRuntime.tsx";
import type { CommandChannel } from "./commandChannel.ts";
import { buildControlEnvelope, envelopeMeta } from "./wireEnvelope.ts";

interface HistoryRequestParams {
  kRoomKey: CryptoKey;
  room: string;
  commandChannel: CommandChannel;
  core: AppRuntimeCore;
  forceRender: () => void;
  currentEpochRef: RefObject<number | null>;
  currentSocketRef: RefObject<WebSocketLike | null>;
  pendingSnapshotRequestRef: RefObject<PendingSnapshotRequest | null>;
  pendingHistoryRequestRef: RefObject<Map<string, PendingHistoryRequest>>;
  historyErrorRef: RefObject<Map<string, HistoryLoadError>>;
  awaitingSelectionForSnapshotRef: RefObject<boolean>;
  selectedSessionIdRef: RefObject<string | null>;
  setSelectedSessionIdState: Dispatch<SetStateAction<string | null>>;
  bumpHistoryRevision: () => void;
  historyRequestTimeoutMs: number;
  sendSnapshotRequest: (sessionId: string, commandId: string) => void;
}

function sendHistoryRequestBody(
  sessionId: string,
  beforeMessageId: number | null,
  commandId: string,
  kRoomKey: CryptoKey,
  room: string,
  commandChannel: CommandChannel,
  core: AppRuntimeCore,
  forceRender: () => void,
  currentEpochRef: RefObject<number | null>,
  currentSocketRef: RefObject<WebSocketLike | null>,
  pendingHistoryRequestRef: RefObject<Map<string, PendingHistoryRequest>>,
  clearHistoryPending: (sessionId: string, commandId: string, error: HistoryLoadError | null, attempt?: number) => void,
) {
  const epochAtStart = currentEpochRef.current;
  const socketAtStart = currentSocketRef.current;
  if (epochAtStart === null || !socketAtStart || socketAtStart.readyState !== ReadyState.OPEN) return;
  const pending = pendingHistoryRequestRef.current.get(sessionId);
  if (!pending || pending.commandId !== commandId || pending.beforeMessageId !== beforeMessageId) return;
  // Resends only advance the attempt. The original timeout remains live for
  // the full pending entry lifetime, even if an attempt exits early.
  pending.attempt += 1;
  const attempt = pending.attempt;
  void (async () => {
    const allowed = await commandChannel.trySendControlSlot(commandId, sessionId, "control.history");
    if (!allowed) {
      clearHistoryPending(sessionId, commandId, "rateLimited", attempt);
      return;
    }
    const plaintext = buildControlHistoryRequest(sessionId, beforeMessageId);
    const meta = envelopeMeta({ v: 1, room, epoch: epochAtStart, kind: "control", session: sessionId, commandId });
    let sealed: { ct: string; n: string };
    try {
      sealed = await seal(kRoomKey, meta, utf8Bytes(JSON.stringify(plaintext)));
    } catch {
      clearHistoryPending(sessionId, commandId, "failed", attempt);
      return;
    }
    // Recheck the latest epoch and socket after async sealing, before sending.
    const latestEpoch = currentEpochRef.current;
    const latestSocket = currentSocketRef.current;
    if (latestEpoch === null || latestEpoch !== epochAtStart || !latestSocket || latestSocket.readyState !== ReadyState.OPEN) {
      clearHistoryPending(sessionId, commandId, "notConnected", attempt);
      return;
    }
    const latestPending = pendingHistoryRequestRef.current.get(sessionId);
    if (!latestPending || latestPending.commandId !== commandId || latestPending.attempt !== attempt) return;
    try {
      latestSocket.send(JSON.stringify(buildControlEnvelope({
        room,
        epoch: latestEpoch,
        session: sessionId,
        commandId,
        ct: sealed.ct,
        n: sealed.n,
        now: () => Date.now(),
      })));
    } catch {
      clearHistoryPending(sessionId, commandId, "failed", attempt);
      return;
    }
    getOrCreateSessionProjection(core, sessionId).historyRequested = true;
    forceRender();
  })();
}

function clearHistoryPendingBody(
  sessionId: string,
  commandId: string,
  error: HistoryLoadError | null,
  attempt: number | undefined,
  pendingHistoryRequestRef: RefObject<Map<string, PendingHistoryRequest>>,
  historyErrorRef: RefObject<Map<string, HistoryLoadError>>,
  core: AppRuntimeCore,
  bumpHistoryRevision: () => void,
  forceRender: () => void,
) {
  const current = pendingHistoryRequestRef.current.get(sessionId);
  if (
    !current ||
    current.commandId !== commandId ||
    (attempt !== undefined && current.attempt !== attempt)
  ) {
    return;
  }
  // This is the only path that disarms a pending request timeout.
  if (current.timeoutHandle !== null) clearTimeout(current.timeoutHandle);
  pendingHistoryRequestRef.current.delete(sessionId);
  if (error === null) {
    historyErrorRef.current.delete(sessionId);
  } else {
    historyErrorRef.current.set(sessionId, error);
    getOrCreateSessionProjection(core, sessionId).historyRequested = false;
  }
  // Write the error before bumping the revision so the next render sees it.
  bumpHistoryRevision();
  forceRender();
}

export function useAppRuntimeHistoryRequests({
  kRoomKey,
  room,
  commandChannel,
  core,
  forceRender,
  currentEpochRef,
  currentSocketRef,
  pendingSnapshotRequestRef,
  pendingHistoryRequestRef,
  historyErrorRef,
  awaitingSelectionForSnapshotRef,
  selectedSessionIdRef,
  setSelectedSessionIdState,
  bumpHistoryRevision,
  historyRequestTimeoutMs,
  sendSnapshotRequest,
}: HistoryRequestParams) {
  const clearHistoryPending = useCallback(
    (sessionId: string, commandId: string, error: HistoryLoadError | null, attempt?: number) => {
      clearHistoryPendingBody(
        sessionId, commandId, error, attempt, pendingHistoryRequestRef,
        historyErrorRef, core, bumpHistoryRevision, forceRender,
      );
    },
    [core, bumpHistoryRevision],
  );

  const failHistoryRequests = useCallback(
    (error: HistoryLoadError, commandId?: string) => {
      for (const pending of Array.from(pendingHistoryRequestRef.current.values())) {
        if (commandId !== undefined && pending.commandId !== commandId) continue;
        clearHistoryPending(pending.sessionId, pending.commandId, error);
      }
    },
    [clearHistoryPending],
  );

  const sendHistoryRequest = useCallback(
    (sessionId: string, beforeMessageId: number | null, commandId: string) => {
      sendHistoryRequestBody(
        sessionId, beforeMessageId, commandId, kRoomKey, room, commandChannel, core, forceRender,
        currentEpochRef, currentSocketRef, pendingHistoryRequestRef, clearHistoryPending,
      );
    },
    [kRoomKey, room, commandChannel, core, clearHistoryPending],
  );

  const requestHistory = useCallback(
    (sessionId: string, beforeMessageId: number | null): boolean => {
      const socket = currentSocketRef.current;
      if (currentEpochRef.current === null || !socket || socket.readyState !== ReadyState.OPEN) {
        historyErrorRef.current.set(sessionId, "notConnected");
        bumpHistoryRevision();
        forceRender();
        return false;
      }
      if (pendingHistoryRequestRef.current.has(sessionId)) return false;
      historyErrorRef.current.delete(sessionId);
      const commandId = crypto.randomUUID();
      // Arm one timeout for this pending entry. Its command ID stays stable across
      // resends; clearHistoryPending is the only path that disarms it. Thus a
      // pending entry always has a live timeout.
      const timeoutHandle = setTimeout(() => {
        clearHistoryPending(sessionId, commandId, "timeout");
      }, historyRequestTimeoutMs);
      pendingHistoryRequestRef.current.set(sessionId, {
        sessionId,
        beforeMessageId,
        commandId,
        attempt: 0,
        timeoutHandle,
      });
      bumpHistoryRevision();
      forceRender();
      sendHistoryRequest(sessionId, beforeMessageId, commandId);
      return true;
    },
    [sendHistoryRequest, clearHistoryPending, historyRequestTimeoutMs, bumpHistoryRevision],
  );

  const requestLatestHistoryIfNeeded = useCallback(
    (sessionId: string) => {
      const projection = getOrCreateSessionProjection(core, sessionId);
      if (projection.messages.size >= 5 || projection.historyRequested || pendingHistoryRequestRef.current.has(sessionId)) return;
      requestHistory(sessionId, null);
    },
    [core, requestHistory],
  );

  const setSelectedSessionId = useCallback(
    (id: string | null) => {
      selectedSessionIdRef.current = id;
      setSelectedSessionIdState(id);
      if (id !== null && awaitingSelectionForSnapshotRef.current) {
        // Replay arrived before selection; send the deferred snapshot now.
        awaitingSelectionForSnapshotRef.current = false;
        const commandId = crypto.randomUUID();
        pendingSnapshotRequestRef.current = { sessionId: id, commandId };
        sendSnapshotRequest(id, commandId);
      }
      if (id !== null) requestLatestHistoryIfNeeded(id);
    },
    [sendSnapshotRequest, requestLatestHistoryIfNeeded],
  );

  const handleReplayHead = useCallback(
    (epoch: number, _headSeq: number) => {
      currentEpochRef.current = epoch;
      // Retry unacknowledged composer commands on the new epoch after reconnect.
      commandChannel.handleStaleEpoch();
      for (const pendingHistory of pendingHistoryRequestRef.current.values()) {
        sendHistoryRequest(pendingHistory.sessionId, pendingHistory.beforeMessageId, pendingHistory.commandId);
      }
      const sessionId = selectedSessionIdRef.current;
      if (!sessionId) {
        // Defer the snapshot until a session is selected.
        awaitingSelectionForSnapshotRef.current = true;
        return;
      }
      const commandId = crypto.randomUUID();
      pendingSnapshotRequestRef.current = { sessionId, commandId };
      sendSnapshotRequest(sessionId, commandId);
      if (!pendingHistoryRequestRef.current.has(sessionId)) {
        requestLatestHistoryIfNeeded(sessionId);
      }
    },
    [sendSnapshotRequest, sendHistoryRequest, commandChannel, requestLatestHistoryIfNeeded],
  );

  const handleEpochChanged = useCallback(
    (epoch: number, _ts: number) => {
      currentEpochRef.current = epoch;
      // Reseal in-flight composer commands with the same command ID on epoch change.
      commandChannel.handleStaleEpoch();
      const pending = pendingSnapshotRequestRef.current;
      if (pending) {
        sendSnapshotRequest(pending.sessionId, pending.commandId);
      }
      for (const pendingHistory of pendingHistoryRequestRef.current.values()) {
        sendHistoryRequest(pendingHistory.sessionId, pendingHistory.beforeMessageId, pendingHistory.commandId);
      }
    },
    [sendSnapshotRequest, sendHistoryRequest, commandChannel],
  );

  return {
    clearHistoryPending,
    failHistoryRequests,
    sendHistoryRequest,
    requestHistory,
    requestLatestHistoryIfNeeded,
    setSelectedSessionId,
    handleReplayHead,
    handleEpochChanged,
  };
}
