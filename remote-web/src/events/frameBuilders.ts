import type { ControlHistoryRequestFrame, ControlSnapshotRequestFrame, MsgFetchRequestFrame } from "./frameTypes";

/** Export the six known error codes so the UI can distinguish protocol-defined errors from retryable unknown codes.
 *  Codes that are not among the six values known to the protocol" are all rendered as retryable (see the `retryable` determination in that file). */
export const MSG_FETCH_ERROR_CODES: readonly string[] = [
  "soft_deleted",
  "forbidden",
  "too_large",
  "stale_revision",
  "busy",
  "not_found",
];

// ---------------------------------------------------------------------------
// Construction helpers (the second real-path consumer: control.snapshot requests are not only "parsed upon receipt"; before sending them itself, the client
// also runs the same `parseFrame` self-check—not a separate handwritten validator; see parseFrame.test.ts for round-trip tests)
// ---------------------------------------------------------------------------

export function buildControlSnapshotRequest(session: string): ControlSnapshotRequestFrame {
  return { t: "control.snapshot", session };
}

export function buildControlHistoryRequest(session: string, beforeMessageId: number | null): ControlHistoryRequestFrame {
  return { t: "control.history", session, before_message_id: beforeMessageId };
}

/** Message fetch requests use the existing control channel and the same parser round-trip validation as other requests.
 *  existing round-trip convention of `buildControlSnapshotRequest`/`buildControlHistoryRequest` (the second real-path consumer:
 *  constructed outbound requests also pass the same `parseFrame()` self-check). */
export function buildMsgFetchRequest(
  session: string,
  messageId: number,
  revision: number,
  offset: number,
): MsgFetchRequestFrame {
  return { t: "msg.fetch", session, message_id: messageId, revision, offset };
}
