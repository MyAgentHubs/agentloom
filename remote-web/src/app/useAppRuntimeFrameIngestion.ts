import { useCallback, useRef, type Dispatch, type RefObject, type SetStateAction } from "react";
import { open } from "../crypto/envelope.ts";
import { parseFrame } from "../events/parseFrame.ts";
import type { MsgFetchClient } from "../events/msgFetch.ts";
import type { EventStorePort } from "../store/port.ts";
import type { HistoryLoadError } from "../ui/stream/SessionStreamScreen.tsx";
import type { DesktopPresence } from "../ui/connection/ConnectionBanner.tsx";
import { envelopeMeta, parseWireEnvelope } from "./wireEnvelope.ts";
import type { CommandChannel } from "./commandChannel.ts";
import type { PendingSnapshotRequest, PendingHistoryRequest } from "./AppRuntime.tsx";
import {
  applyDecryptedLiveFrame,
  applyDecryptedHistoryFrame,
  applyDecryptedMilestoneFrame,
  checkFrameRouting,
  recordFrameApplied,
  recordFrameSeen,
  recordProcessFrameDrop,
  recordRoutingRejection,
  type AppRuntimeCore,
} from "./appRuntimeCore.ts";

// Caller must keep forceRender, currentEpochRef, pendingSnapshotRequestRef, pendingHistoryRequestRef, and setDesktopPresence stable across renders; omitted dependencies assume stable references.
interface FrameIngestionParams {
  commandChannel: CommandChannel;
  core: AppRuntimeCore;
  eventStore: EventStorePort;
  room: string;
  kRoomKey: CryptoKey;
  msgFetchClient: MsgFetchClient;
  forceRender: () => void;
  clearHistoryPending: (sessionId: string, commandId: string, error: HistoryLoadError | null, attempt?: number) => void;
  failHistoryRequests: (error: HistoryLoadError, commandId?: string) => void;
  sendSnapshotRequest: (sessionId: string, commandId: string) => void;
  sendHistoryRequest: (sessionId: string, beforeMessageId: number | null, commandId: string) => void;
  currentEpochRef: RefObject<number | null>;
  pendingSnapshotRequestRef: RefObject<PendingSnapshotRequest | null>;
  pendingHistoryRequestRef: RefObject<Map<string, PendingHistoryRequest>>;
  setDesktopPresence: Dispatch<SetStateAction<DesktopPresence>>;
}

type PlaintextFrameParams = Pick<FrameIngestionParams,
  | "commandChannel"
  | "failHistoryRequests"
  | "sendSnapshotRequest"
  | "sendHistoryRequest"
  | "currentEpochRef"
  | "pendingSnapshotRequestRef"
  | "pendingHistoryRequestRef"
  | "setDesktopPresence">;

type ProcessFrameParams = Pick<FrameIngestionParams,
  | "kRoomKey"
  | "eventStore"
  | "core"
  | "room"
  | "commandChannel"
  | "clearHistoryPending"
  | "msgFetchClient"
  | "forceRender"
  | "pendingSnapshotRequestRef"
  | "pendingHistoryRequestRef"> & {
  handlePlaintextCommandFrame: (raw: unknown) => Promise<void>;
};

  // -------------------------------------------------------------------------
  // Incoming frame handling: strictly serial queue (INT1c P0).
  // -------------------------------------------------------------------------
  /**
   * T6f3: `raw` is not a wire envelope (it lacks `v`/`room`/`epoch`/`kind`).
   * The command-related plaintext exceptions in M0 §1 have this shape:
   * `error{reason:"stale_epoch"|"input_rate_limited"|"control_rate_limited"|
   * "queue_full"|"ip_message_rate_limited"|"desktop_offline"}`,
   * `input.ack`, and `input.expired`. `ConnectionSession.handleMessage` forwards
   * plaintext frames without dedicated branches unchanged to `onFrame`; these
   * cases use the `default:` branch in `connection/connectionSession.ts`.
   *
   * - `error{reason:"stale_epoch", currentEpoch}` (G4 acceptance): it has no
   *   `command_id` (relay rejection frames omit it; see the `room-do.js` reference
   *   in the header comment of `commandChannel.ts::handleStaleEpoch`). First update
   *   `currentEpochRef` to the relay's authoritative epoch so every later send,
   *   including these retries, reads the new value. Then have `CommandChannel`
   *   resend all commands still awaiting results with the new epoch and the same
   *   command_id. FIX2 P1-2: an in-flight `control.snapshot` request may also have
   *   been sealed with the old epoch and rejected; reuse `handleEpochChanged()`'s
   *   existing `pendingSnapshotRequestRef` retry logic to resend it as well.
   * - `error{reason:"input_rate_limited"|"control_rate_limited"|"queue_full"|
   *   "ip_message_rate_limited", command_id, frame}` (G8 rate-limit handling +
   *   FIX2 P1-3): the relevant branches of
   *   `room-do.js::takeSubjectChannelRateSlot`/`takeIpMessageSlot` and the full
   *   pending-input queue in `handleInput` carry `command_id`. They reply only to
   *   the socket whose request was rejected, without broadcasting. Pass all four
   *   to `CommandChannel.handleRateLimited()` so the UI changes from sending to
   *   rate-limited/retryable instead of silently ignoring them.
   * - `error{reason:"desktop_offline"}` (FIX2 P1-3): at `room-do.js:722` and similar
   *   sites, the desktop is offline. `handleInput`/`handleControl` reject directly
   *   without adding to `pending_input` and **without** `command_id`. Pass this to
   *   `CommandChannel.handleDesktopOffline()` to mark all in-flight commands
   *   retryable as a group, as `handleStaleEpoch()` does (see its header comment).
   * - `input.ack`/`input.expired` (G3 mitigation): let `CommandChannel` filter
   *   through its persistent ledger. Silently ignore commands not sent by this
   *   client, and apply state changes only for this client's commands.
   * - `input.relay_queued` (C1-RQ, second dogfood fix batch): a new directed
   *   response from relay `handleInput` to the sender after successfully queuing
   *   an input while the desktop is offline or finding an idempotent match. As
   *   with `input.ack`/`input.expired`, let `CommandChannel` filter through its
   *   ledger, enter the nonterminal `relay_queued` state, and wait for the real
   *   ack/expired frame.
   * - `presence{role:"desktop", event}` (C1-PS, second dogfood fix batch): a
   *   directed snapshot of desktop presence when a new connection joins (see
   *   `room-do.js::sendDesktopPresenceSnapshot`), or a broadcast when the desktop
   *   connects or disconnects (see `broadcastPresence`). Maintain the
   *   `desktopPresence` state for `ConnectionBanner`'s weak-guarantee banner and
   *   `deriveSendBadge`'s queued-send semantics. As before, silently ignore
   *   presence frames with `role!=="desktop"` for other remote devices.
   *
   * `replay.head` also arrives on this path. Its dedicated `onReplayHead`
   * callback already handles the relevant parts (see `handleReplayHead` in `useAppRuntimeHistoryRequests.ts`),
   * so do not consume it twice.
   */
async function processPlaintextCommandFrame(raw: unknown, {
  commandChannel,
  failHistoryRequests,
  sendSnapshotRequest,
  sendHistoryRequest,
  currentEpochRef,
  pendingSnapshotRequestRef,
  pendingHistoryRequestRef,
  setDesktopPresence,
}: PlaintextFrameParams): Promise<void> {
  if (typeof raw !== "object" || raw === null) return;
  const record = raw as Record<string, unknown>;
  if (record.t === "error" && record.reason === "stale_epoch" && typeof record.currentEpoch === "number") {
    currentEpochRef.current = record.currentEpoch;
    commandChannel.handleStaleEpoch();
    // FIX2 P1-2: `stale_epoch` may reject an in-flight `control.snapshot` request
    // sealed with the old epoch, not just composer commands. Reuse the existing
    // `handleEpochChanged()` behavior: reseal and resend with the same command_id
    // and the latest epoch (updated above). This follows the snapshot retry
    // convention in `sendSnapshotRequest`/`pendingSnapshotRequestRef`.
    const pending = pendingSnapshotRequestRef.current;
    if (pending) {
      sendSnapshotRequest(pending.sessionId, pending.commandId);
    }
    for (const pendingHistory of pendingHistoryRequestRef.current.values()) {
      sendHistoryRequest(pendingHistory.sessionId, pendingHistory.beforeMessageId, pendingHistory.commandId);
    }
    return;
  }
  if (
    record.t === "error" &&
    (record.reason === "input_rate_limited" ||
      record.reason === "control_rate_limited" ||
      // FIX2 P1-3: `queue_full` (`room-do.js:694`, pending input queue full) and
      // `ip_message_rate_limited` (`room-do.js:1466/1479`, per-IP rate limit) both
      // carry `command_id`. Handle them as retryable terminal outcomes through
      // `handleRateLimited()`, with wording consistent with input/control_rate_limited.
      record.reason === "queue_full" ||
      record.reason === "ip_message_rate_limited") &&
    typeof record.command_id === "string"
  ) {
    if (record.reason === "control_rate_limited") {
      failHistoryRequests("rateLimited", record.command_id);
    }
    await commandChannel.handleRateLimited(record.command_id);
    return;
  }
  if (record.t === "error" && record.reason === "desktop_offline") {
    // FIX2 P1-3: `desktop_offline` (`room-do.js:722` and similar sites) omits
    // `command_id`; see `CommandChannel.handleDesktopOffline()` for group handling.
    failHistoryRequests(
      "desktopOffline",
      typeof record.command_id === "string" ? record.command_id : undefined,
    );
    await commandChannel.handleDesktopOffline();
    return;
  }
  if (record.t === "quota.exceeded" && record.channel === "live") {
    failHistoryRequests("quota");
    return;
  }
  const parsed = parseFrame(raw);
  if (!parsed.ok) return;
  if (parsed.frame.t === "input.ack") {
    if (parsed.frame.outcome === "failed") {
      failHistoryRequests("failed", parsed.frame.command_id);
    }
    await commandChannel.handleAck(parsed.frame.command_id, parsed.frame.outcome, parsed.frame.reason);
    return;
  }
  if (parsed.frame.t === "input.expired") {
    await commandChannel.handleExpired(parsed.frame.command_id);
    return;
  }
  if (parsed.frame.t === "input.relay_queued") {
    await commandChannel.handleRelayQueued(parsed.frame.command_id, parsed.frame.expires_at);
    return;
  }
  if (parsed.frame.t === "presence" && parsed.frame.role === "desktop") {
    // Weak-guarantee mapping: relay has only sent "online"/"offline" events (see
    // room-do.js sendDesktopPresenceSnapshot/broadcastPresence). Treat any value
    // other than "online" as the more conservative "offline": an extra "possibly
    // offline" banner is preferable to silently treating an unknown value as online.
    setDesktopPresence(parsed.frame.event === "online" ? "online" : "offline");
  }
}

async function processIncomingFrame(raw: unknown, {
  kRoomKey,
  eventStore,
  core,
  room,
  commandChannel,
  clearHistoryPending,
  msgFetchClient,
  forceRender,
  pendingSnapshotRequestRef,
  pendingHistoryRequestRef,
  handlePlaintextCommandFrame,
}: ProcessFrameParams): Promise<void> {
  recordFrameSeen(core);
  const envelope = parseWireEnvelope(raw);
  if (!envelope) {
    recordProcessFrameDrop(core, "plaintextControl", {
      kind: typeof raw === "object" && raw !== null && typeof (raw as { kind?: unknown }).kind === "string" ? (raw as { kind: string }).kind : null,
      t: typeof raw === "object" && raw !== null && typeof (raw as { t?: unknown }).t === "string" ? (raw as { t: string }).t : null,
    });
    forceRender();
    await handlePlaintextCommandFrame(raw);
    return;
  }
  // `reply` (M0 §10.1) is the sixth top-level kind — desktop -> relay -> a directed delivery to
  // a specific remote, using the same AEAD envelope as event/live (`ct`/`n`), just without
  // persistence or broadcast (it's a reply to a command_id, not a frame to reduce into session
  // state). Admitted into this allowlist so the same decrypt -> parse steps run below, then
  // handled in its own branch after parsing (before `checkFrameRouting`/milestone reduction).
  if (envelope.kind !== "event" && envelope.kind !== "live" && envelope.kind !== "reply") {
    recordProcessFrameDrop(core, "kindSkipped", { kind: envelope.kind, t: null });
    forceRender();
    return;
  } // ConnectionSession handles presence/error/... in its own branches, or
  // forwards them here unchanged for harmless dropping. This component only
  // processes the three encrypted data-plane kinds above.

  let plaintext: Uint8Array;
  try {
    plaintext = await open(kRoomKey, envelopeMeta(envelope), envelope.ct, envelope.n);
  } catch {
    recordProcessFrameDrop(core, "decryptFailed", { kind: envelope.kind, t: null });
    forceRender();
    return; // Drop a failed/tampered AEAD authentication without crashing.
  }
  let json: unknown;
  try {
    json = JSON.parse(new TextDecoder().decode(plaintext));
  } catch {
    recordProcessFrameDrop(core, "parseFailed", { kind: envelope.kind, t: null, parseReason: "invalid_json" });
    forceRender();
    return;
  }
  const parsed = parseFrame(json);
  if (!parsed.ok) {
    recordProcessFrameDrop(core, "parseFailed", { kind: envelope.kind, t: parsed.t, parseReason: parsed.reason });
    forceRender();
    return;
  }

  // The `reply` kind (M0 §10.1) is never persisted or broadcast, so it skips the event/live
  // reduction path and `checkFrameRouting` (those checks are specific to
  // session.index/snapshot/history/run.status frames; `msg.chunk`/`msg.fetch.error` match none
  // of them and would just get a meaningless `accepted:true`). Routing to `MsgFetchClient`
  // relies on `envelope.commandId`, required under `kind=reply` (M0 §10.1); if missing
  // (malformed frame / relay contract violation), drop and count it rather than passing null.
  if (envelope.kind === "reply") {
    if ((parsed.frame.t !== "msg.chunk" && parsed.frame.t !== "msg.fetch.error") || envelope.commandId === null) {
      recordProcessFrameDrop(core, "parseFailed", { kind: envelope.kind, t: parsed.frame.t, parseReason: "malformed_fields" });
      forceRender();
      return;
    }
    await msgFetchClient.handleReply(envelope.commandId, parsed.frame);
    forceRender();
    return;
  }

  // Enforce routing before persistence/reduction (INT1c P0): count and drop
  // room mismatches, session.index with a non-null outer session, and snapshot
  // or run.status with mismatched inner/outer sessions. Never let them reach
  // the reduction layer below.
  const routing = checkFrameRouting(room, { room: envelope.room, session: envelope.session }, parsed.frame);
  if (!routing.accepted) {
    recordRoutingRejection(core, routing.reason, { kind: envelope.kind, t: parsed.frame.t });
    forceRender();
    return;
  }

  if (envelope.kind === "event") {
    if (envelope.clientMsgId === null || envelope.seq === null) {
      recordProcessFrameDrop(core, "missingIds", { kind: envelope.kind, t: parsed.frame.t });
      forceRender();
      return;
    } // Wire grammar violation.
    let result;
    try {
      result = await eventStore.applyEventIfNew({
        clientMsgId: envelope.clientMsgId,
        seq: envelope.seq,
        session: envelope.session,
        frame: json,
      });
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      // msgfix2 F2 S3: `recordProcessFrameDrop()` only updates the in-memory
      // `frameDiagnostics` counter, visible only when the user opens DebugPanel
      // with `?debug=1`. Otherwise this degraded state is silent. Before the
      // `onversionchange` fix cleared `dbPromise` in the three IndexedDB
      // implementations, a version change triggered by another tab made this
      // path repeatedly throw InvalidStateError and silently lose history.
      // Keep a console.error visible in devtools without requiring DebugPanel.
      // This is below crash severity: the existing frame-drop path lets later
      // frames continue, but cache/history writes must not fail invisibly.
      console.error(`msgfix2 F2 S3: eventStore.applyEventIfNew() failed (frame dropped): ${errorMessage}`);
      recordProcessFrameDrop(core, "storeError", {
        kind: envelope.kind,
        t: parsed.frame.t,
        errorMessage,
      });
      forceRender();
      return;
    }
    if (!result.applied) {
      recordProcessFrameDrop(core, "storeDuplicate", { kind: envelope.kind, t: parsed.frame.t });
      forceRender();
      return;
    } // Idempotently drop a duplicate from at-least-once redelivery.
    if (applyDecryptedMilestoneFrame(core, envelope.session, parsed.frame)) {
      recordFrameApplied(core);
    }
    // Only a new msg.completed frame that passes decryption, parsing, routing,
    // persistent deduplication, and projection can acknowledge delivery. The
    // outer client_msg_id is available here; pass it to CommandChannel to match
    // precisely against each deterministic ID derived from this session's
    // pending/queued input.send commands.
    if (parsed.frame.t === "msg.completed" && envelope.session !== null) {
      await commandChannel.handleMsgCompleted(envelope.session, envelope.clientMsgId);
    }
    // A session's snapshot response fulfills its in-flight request; later
    // epoch changes must not resend it.
    if (parsed.frame.t === "snapshot" && pendingSnapshotRequestRef.current?.sessionId === parsed.frame.session) {
      pendingSnapshotRequestRef.current = null;
    }
    forceRender();
    return;
  }

  // kind === "live": reduce directly without persisting (M0 §2: live is never replayed).
  if (parsed.frame.t === "history") {
    const pending = pendingHistoryRequestRef.current.get(parsed.frame.session);
    const matchesPending =
      pending !== undefined && pending.beforeMessageId === parsed.frame.before_message_id;
    if (applyDecryptedHistoryFrame(core, envelope.session, parsed.frame, { advanceCursor: matchesPending })) {
      recordFrameApplied(core);
      if (matchesPending) {
        clearHistoryPending(parsed.frame.session, pending.commandId, null);
      }
    }
    forceRender();
    return;
  }
  if (applyDecryptedLiveFrame(core, envelope.session, parsed.frame)) {
    recordFrameApplied(core);
  }
  forceRender();
}

export function useAppRuntimeFrameIngestion({
  commandChannel,
  core,
  eventStore,
  room,
  kRoomKey,
  msgFetchClient,
  forceRender,
  clearHistoryPending,
  failHistoryRequests,
  sendSnapshotRequest,
  sendHistoryRequest,
  currentEpochRef,
  pendingSnapshotRequestRef,
  pendingHistoryRequestRef,
  setDesktopPresence,
}: FrameIngestionParams) {
  const frameQueueRef = useRef<Promise<void>>(Promise.resolve());

  const handlePlaintextCommandFrame = useCallback(
    (raw: unknown): Promise<void> => processPlaintextCommandFrame(raw, {
      commandChannel,
      failHistoryRequests,
      sendSnapshotRequest,
      sendHistoryRequest,
      currentEpochRef,
      pendingSnapshotRequestRef,
      pendingHistoryRequestRef,
      setDesktopPresence,
    }),
    [commandChannel, sendSnapshotRequest, sendHistoryRequest, failHistoryRequests],
  );

  const processFrame = useCallback(
    (raw: unknown): Promise<void> => processIncomingFrame(raw, {
      kRoomKey,
      eventStore,
      core,
      room,
      commandChannel,
      clearHistoryPending,
      msgFetchClient,
      forceRender,
      pendingSnapshotRequestRef,
      pendingHistoryRequestRef,
      handlePlaintextCommandFrame,
    }),
    [kRoomKey, eventStore, core, room, handlePlaintextCommandFrame, commandChannel, clearHistoryPending, msgFetchClient],
  );

  /**
   * Strictly serial processing: `handleFrame` keeps the synchronous signature
   * expected by `ConnectionSessionCallbacks.onFrame`, but queues the actual
   * work on one Promise chain. Each frame's `processFrame()` starts only after
   * the previous frame fully settles (resolves or reaches the `.catch()` below),
   * giving the same arrival-order behavior as a single reader thread.
   * `processFrame` branches normally catch errors and return rather than reject.
   * The `.catch()` is defensive: if a future await inside `processFrame` lacks
   * a catch, its rejection must not break the chain and leave all later frames
   * stuck behind a rejected promise.
   */
  const handleFrame = useCallback(
    (raw: unknown) => {
      frameQueueRef.current = frameQueueRef.current.then(() => processFrame(raw)).catch(() => {});
    },
    [processFrame],
  );

  return { handleFrame };
}
