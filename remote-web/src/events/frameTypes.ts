export type FrameRejectReason = "not_an_object" | "missing_t" | "unknown_t" | "malformed_fields";

// ---------------------------------------------------------------------------
// Milestone frames (kind=event·relay supplies the seq·client_msg_id deduplication handle)
// ---------------------------------------------------------------------------

export interface SessionIndexRow {
  id: string;
  title: string;
  repo_id: string;
  archived: boolean;
  status: string | null;
  run_id: string | null;
  updated_at: number;
  /** Optional while older desktop builds can still emit a full row without the nullable extension. */
  last_msg_preview?: string | null;
  /** Unix timestamp from messages.created_at; optional for old-desktop compatibility. */
  last_activity_at?: number | null;
  /** Human-readable project name (M2-4x)——optional for old-desktop compatibility; consumers fall
   *  back to rendering the bare `repo_id` when missing/null (see SessionListScreen.tsx). */
  repo_name?: string | null;
}

/** M2-4x: a top-level full-snapshot summary of the "project currently being remoted"—`name` may be `null` (the active repo is known, but that
 *  project currently has zero sessions and its name cannot be obtained; see the desktop `active_repo_summary_for_snapshot` documentation). */
export interface SessionIndexActiveRepo {
  id: string;
  name: string | null;
}

export interface SessionIndexFullFrame {
  t: "session.index";
  full: true;
  sessions: SessionIndexRow[];
  /** Optional/nullable for old-desktop compatibility——older builds don't send this key at all. */
  repo?: SessionIndexActiveRepo | null;
}

export interface SessionIndexCreatedFrame {
  t: "session.index";
  op: "created";
  full: false;
  session: {
    id: string;
    title: string;
    repo_id: string;
    namespace_id: string;
    archived: boolean;
    repo_name?: string | null;
  };
}

export interface SessionIndexRenamedFrame {
  t: "session.index";
  op: "renamed";
  full: false;
  id: string;
  title: string;
}

export interface SessionIndexArchivedFrame {
  t: "session.index";
  op: "archived" | "unarchived";
  full: false;
  ids: string[];
}

export interface SessionIndexDeletedFrame {
  t: "session.index";
  op: "deleted";
  full: false;
  id: string;
}

export type SessionIndexFrame =
  | SessionIndexFullFrame
  | SessionIndexCreatedFrame
  | SessionIndexRenamedFrame
  | SessionIndexArchivedFrame
  | SessionIndexDeletedFrame;

export interface MsgCompletedFrame {
  t: "msg.completed";
  message_id: number;
  role: string;
  /** Array of already-formed blocks—passed through; the deep schema is not validated at this layer (see the file header comment). */
  blocks: unknown[];
  /** agent_name_snapshot (e.g. "Claude"/"Codex"/"DeepSeek")—this key is omitted for user messages / when there is no agent;
   *  optional: consumers fall back to the existing assistant/user placeholder when absent (MA2 displays the current agent-completed message). */
  agent?: string;
  /** An optional pointer allows over-budget messages to carry a preview without changing required frame fields.
   *  It is purely additive to the existing required field set. When omitted, this key does not appear at all on the parsed frame (following the existing
   *  optional convention for `agent`), rather than having an `undefined` value. */
  content_ref?: ContentRef;
  /** Optional top-level revision enables version comparisons independently of preview pointers.
   * Forward-compatible parsing preserves the revision whenever the desktop sends it.
   *  `MilestoneProjection` uses it preferentially to determine "the higher one wins", without needing to wait for a message to be downgraded to a preview and for
   *  `content_ref.revision` to appear before a version number can be compared. When omitted, this key does not appear at all (following the existing optional convention for `agent`/`content_ref`). */
  revision?: number;
}

/** The optional content_ref uses the same four-field shape for completed messages and history rows. */
export interface ContentRef {
  message_id: number;
  revision: number;
  content_sha256: string;
  total_bytes: number;
}

export interface CardCreatedFrame {
  t: "card.created";
  /** DecisionCard block, passed through as a whole. */
  block: Record<string, unknown>;
}

export interface CardResolvedFrame {
  t: "card.resolved";
  decision_id: string;
  status: string;
  chosen_option: string | null;
}

export interface RunStatusFrame {
  t: "run.status";
  session_id: string;
  status: string;
  run_id: string | null;
}

export interface ToolCompletedFrame {
  t: "tool.completed";
  id: string;
  tool: string;
  status: string;
  exit_code: number | null;
  output: string | null;
}

// ---------------------------------------------------------------------------
// Live frames (kind=live·not persisted·never replayed)
// ---------------------------------------------------------------------------

export interface TextDeltaFrame {
  t: "text_delta";
  seq: number;
  text: string;
}

export interface ThinkingDeltaFrame {
  t: "thinking_delta";
  seq: number;
  text: string;
}

export interface ToolOutputDeltaFrame {
  t: "tool_output_delta";
  seq: number;
  id: string;
  text: string;
}

export interface UsageDeltaFrame {
  t: "usage_delta";
  seq: number;
  input_tokens: number;
  output_tokens: number;
}

export type LiveFrame = TextDeltaFrame | ThinkingDeltaFrame | ToolOutputDeltaFrame | UsageDeltaFrame;

// ---------------------------------------------------------------------------
// Command plane · control.snapshot request (remote → desktop) and snapshot response (desktop → remote, via the kind=event
// channel in versions v1.8.11/v1.8.12)
// ---------------------------------------------------------------------------

export interface ControlSnapshotRequestFrame {
  t: "control.snapshot";
  session: string;
}

export interface ControlHistoryRequestFrame {
  t: "control.history";
  session: string;
  before_message_id: number | null;
}

export interface HistoryResponseMessage {
  message_id: number;
  role: string;
  blocks: unknown[];
  /** Like MsgCompletedFrame.content_ref, this optional pointer leaves no key when omitted. */
  content_ref?: ContentRef;
  /** Like MsgCompletedFrame.revision, this optional version leaves no key when omitted. */
  revision?: number;
}

export interface HistoryResponseFrame {
  t: "history";
  session: string;
  before_message_id: number | null;
  messages: HistoryResponseMessage[];
  next_before: number | null;
}

export interface SnapshotResponsePartialMsg {
  role: string;
  blocks: unknown[];
}

export interface SnapshotResponseFrame {
  t: "snapshot";
  session: string;
  run_id: string | null;
  through_run_seq: number | null;
  partial_msg: SnapshotResponsePartialMsg | null;
}

// ---------------------------------------------------------------------------
// Message retrieval uses the existing control channel for remote-to-desktop msg.fetch requests.
// `msg.chunk`/`msg.fetch.error` are the two ciphertext body forms of the new outer `kind` `reply` (desktop → remote, directed delivery, not persisted
// or broadcast). All three belong to the same "command plane" as `control.snapshot`/`control.history`/`history` above,
// and are placed here adjacent to them rather than in the relay plaintext frame list below (these three still use the AEAD envelope; only the outer kind differs).
// ---------------------------------------------------------------------------

export interface MsgFetchRequestFrame {
  t: "msg.fetch";
  session: string;
  message_id: number;
  revision: number;
  offset: number;
}

export interface MsgChunkFrame {
  t: "msg.chunk";
  message_id: number;
  revision: number;
  content_sha256: string;
  total_bytes: number;
  offset: number;
  chunk_len: number;
  /** Base64 of this fragment's raw content—the deep validation (decoding/length checking) is not performed at this layer; see the reassembly state machine in `msgFetch.ts`. */
  bytes_b64: string;
}

/** a six-value enumeration; `current_ref` is required only for `"stale_revision"`.
 * The `| (string & {})` union accepts known codes and future string codes for protocol compatibility.
 *  any other string that future versions may add"—`parseMsgFetchError` no longer rejects the entire frame for a code outside this six-value enumeration;
 *  it renders it as a retryable error and preserves the original code for UI display (see the `retryable` determination and
 *  `data-error-reason` attribute in `SessionStreamScreen.tsx`). The `string & {}` form merely prevents TS from collapsing the entire union type into bare
 *  `string`—the six known literals still have editor autocomplete, while any other string value is also accepted. */
export type MsgFetchErrorCode =
  | "soft_deleted"
  | "forbidden"
  | "too_large"
  | "stale_revision"
  | "busy"
  | "not_found"
  | (string & {});

export interface MsgFetchErrorFrame {
  t: "msg.fetch.error";
  code: MsgFetchErrorCode;
  /** Present only when `code:"stale_revision"`; for all other codes, this key does not appear at all (not `undefined`/`null`). */
  current_ref?: ContentRef;
}

// ---------------------------------------------------------------------------
// Relay plaintext frames (do not use the AEAD envelope; part of the plaintext exception list)
// ---------------------------------------------------------------------------

export interface PresenceFrame {
  t: "presence";
  role: string;
  event: string;
}

export interface InputAckFrame {
  t: "input.ack";
  command_id: string;
  /** Valid values are ok/queued/failed; the relay forwards them as-is without validation—see ackOutcome.ts for fallback display semantics of unknown values. */
  outcome: string;
  reason?: string;
}

export interface InputExpiredFrame {
  t: "input.expired";
  command_id: string;
}

/**
 * (second batch of dogfood bug fixes · no feedback when a mobile message is sent while the desktop is offline)—the relay's `handleInput`, when offline storage succeeds /
 * an idempotency hit occurs, sends this back only to the sender (only the remote socket that triggered this input, not broadcast to the whole room); it is neither
 * `input.ack` (which means "desktop has received it") nor `error` (the message did not fail). See
 * `remote-relay/src/room-do.js::handleInput`. `expires_at` is the TTL expiration time of this stored row (milliseconds since the
 * epoch), allowing the UI to show roughly when it should give up waiting—it is not an exact protocol guarantee; authoritative expiration cleanup on the relay side still
 * follows the `pending_input` table itself.
 */
export interface InputRelayQueuedFrame {
  t: "input.relay_queued";
  command_id: string;
  expires_at: number;
}

export interface ReplayHeadFrame {
  t: "replay.head";
  epoch: number;
  headSeq: number;
}

// ---------------------------------------------------------------------------
// Discrimination + parsing
// ---------------------------------------------------------------------------

export type ParsedFrame =
  | SessionIndexFrame
  | MsgCompletedFrame
  | CardCreatedFrame
  | CardResolvedFrame
  | RunStatusFrame
  | ToolCompletedFrame
  | LiveFrame
  | ControlSnapshotRequestFrame
  | SnapshotResponseFrame
  | ControlHistoryRequestFrame
  | HistoryResponseFrame
  | MsgFetchRequestFrame
  | MsgChunkFrame
  | MsgFetchErrorFrame
  | PresenceFrame
  | InputAckFrame
  | InputExpiredFrame
  | InputRelayQueuedFrame
  | ReplayHeadFrame;

export type ParseResult =
  | { ok: true; frame: ParsedFrame }
  | { ok: false; reason: FrameRejectReason; t: string | null };
