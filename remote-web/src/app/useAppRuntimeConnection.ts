import { useEffect, useMemo, type Dispatch, type SetStateAction } from "react";
import {
  ConnectionSession,
  type ConnectionSessionCallbacks,
} from "../connection/connectionSession.ts";
import type {
  ConnectionCredentials,
  ConnectionSessionPhase,
  WebSocketFactory,
} from "../connection/types.ts";
import type {
  KeyStorePort,
  StoredPairingCredentials,
} from "../store/key-store.ts";
import type { EventStorePort } from "../store/port.ts";
import type { DesktopPresence } from "../ui/connection/ConnectionBanner.tsx";
import type { HistoryLoadError } from "../ui/stream/SessionStreamScreen.tsx";
import {
  isDebugPanelEnabled,
  recordConnectionLog,
  type ConnectionDiagnosticsSnapshot,
} from "../ui/debug/DebugPanel.tsx";
import type { CommandChannel } from "./commandChannel.ts";

interface ConnectionState {
  phase: ConnectionSessionPhase;
  phaseChangedAtMs: number;
  disconnectedSinceMs: number | null;
}

// Caller must keep the three React setters stable; they are intentionally omitted from effect dependencies.
interface Params {
  stored: StoredPairingCredentials;
  kPair: Uint8Array;
  observableFactory: WebSocketFactory;
  keyStore: KeyStorePort;
  eventStore: EventStorePort;
  onNeedsRepair: () => void;
  handleFrame: NonNullable<ConnectionSessionCallbacks["onFrame"]>;
  handleReplayHead: NonNullable<ConnectionSessionCallbacks["onReplayHead"]>;
  handleEpochChanged: NonNullable<ConnectionSessionCallbacks["onEpochChanged"]>;
  failHistoryRequests: (error: HistoryLoadError, commandId?: string) => void;
  commandChannel: CommandChannel;
  // React state setters are stable and do not belong in the effect dependency array.
  setConnectionDiagnostics: Dispatch<
    SetStateAction<ConnectionDiagnosticsSnapshot>
  >;
  setConnectionState: Dispatch<SetStateAction<ConnectionState>>;
  setDesktopPresence: Dispatch<SetStateAction<DesktopPresence>>;
}

export function useAppRuntimeConnection({
  stored,
  kPair,
  observableFactory,
  keyStore,
  eventStore,
  onNeedsRepair,
  handleFrame,
  handleReplayHead,
  handleEpochChanged,
  failHistoryRequests,
  commandChannel,
  setConnectionDiagnostics,
  setConnectionState,
  setDesktopPresence,
}: Params) {
  const credentials = useMemo<ConnectionCredentials>(
    () => ({
      deviceId: stored.deviceId,
      room: stored.room,
      relayUrl: stored.relayUrl,
      access: stored.access,
      refresh: stored.refresh,
      kPair,
      // FIX2 P0-1, second link (make the fallback conservative, reversing the old fallback):
      // `pairing-session.ts::persistActivation` now always writes this field (see its matching
      // comment). A missing field can only come from an existing record persisted before the
      // field was introduced (the migration boundary). In that case we do not know whether
      // access was just issued or has been stored for a long time. The old `Date.now()` fallback
      // silently pretended that "unknown" meant "just issued":
      // `ConnectionSession.maybeProactiveRefreshOrArmWatchdog()` computes
      // `elapsed = now - accessIssuedAtMs` from it. A falsely fresh timestamp makes
      // `elapsed≈0`, so it never concludes that a refresh is due. If this existing access
      // actually expired long ago, the connection can silently stall after a cold start:
      // the relay treats it as a refresh-scope downgraded connection and sends no
      // `replay.head` (see the new watchdog header comment in
      // `connection/connectionSession.ts`). The UI stays on an empty session list with no
      // signal telling the user it is waiting for a reconnect that will never arrive.
      // For a missing field, fall back instead to an ancient epoch timestamp (`0`):
      // `elapsed` immediately appears to be "Date.now() has elapsed", necessarily crossing
      // the `0.8 times nominal lifetime` threshold. As soon as `onConnectionOpened()`
      // opens the connection, it proactively calls `beginRefresh()`. An extra refresh of
      // still-valid access is preferable to treating existing credentials of unknown
      // validity as freshly issued credentials.
      accessIssuedAtMs: stored.accessIssuedAtMs ?? 0,
    }),
    [stored, kPair],
  );

  useEffect(() => {
    let active = true;
    const session = new ConnectionSession(
      credentials,
      {
        webSocketFactory: observableFactory,
        keyStore,
        getLastSeq: () => eventStore.getWatermark(),
        log: isDebugPanelEnabled()
          ? (level, message, context) => {
              if (!active) return;
              setConnectionDiagnostics((current) => recordConnectionLog(current, level, message, context));
            }
          : undefined,
      },
      {
        onFrame: handleFrame,
        onReplayHead: handleReplayHead,
        onEpochChanged: handleEpochChanged,
        onNeedsRepair,
        onPhaseChange: (phase) => {
          if (!active) return;
          if (phase !== "open") {
            // U3: A disconnect is an immediate failure. Any non-open phase means this
            // connection cannot currently send or receive anything, so in-flight history
            // requests must move to Retry immediately instead of waiting for a timeout
            // that may be far away. `failHistoryRequests` calls `clearHistoryPending` for
            // each commandId with the same idempotent semantics: repeating the call on
            // an empty Map or a pending request cleared elsewhere is a safe no-op.
            failHistoryRequests("notConnected");
            // C1-PS: The desktop presence value held at disconnect is no longer
            // authoritative. It was the last snapshot or broadcast received on this
            // socket; after disconnection, no one is watching for desktop changes on
            // our behalf. Return to "unknown" until a fresh snapshot confirms presence
            // after reconnect, instead of rendering the banner or send badge with a
            // possibly stale value.
            setDesktopPresence("unknown");
            // R1 (rework: the disconnect watchdog consumes G4 reconnect resend):
            // In every non-open phase, pause the ack watchdog timers for all in-flight
            // commands without changing their status; time does not run during the
            // disconnect. After reconnect, `handleReplayHead` calls
            // `commandChannel.handleStaleEpoch()` to resend records still marked
            // sending/sent with the same command_id. The watchdog is naturally armed
            // again on actual resend; see the header comment on
            // `commandChannel.ts::cancelAckWatchdogsForDisconnect`.
            commandChannel.cancelAckWatchdogsForDisconnect();
          }
          setConnectionState((current) => {
            if (current.phase === phase) return current;
            const phaseChangedAtMs = Date.now();
            return {
              phase,
              phaseChangedAtMs,
              // connecting ↔ reconnect_scheduled can switch repeatedly during one
              // continuous disconnection. The banner's elapsed duration must start
              // at the beginning of the whole outage rather than reset on each retry.
              // open is the only state that clears the disconnection start time.
              disconnectedSinceMs:
                phase === "open"
                  ? null
                  : current.disconnectedSinceMs ?? phaseChangedAtMs,
            };
          });
        },
      },
    );
    void session.start();
    return () => {
      active = false;
      void session.stop();
    };
  }, [
    credentials,
    observableFactory,
    keyStore,
    eventStore,
    handleFrame,
    handleReplayHead,
    handleEpochChanged,
    onNeedsRepair,
    failHistoryRequests,
    commandChannel,
  ]);
}
