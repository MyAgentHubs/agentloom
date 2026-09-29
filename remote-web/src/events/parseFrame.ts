// parseFrame.ts — Frame parsing layer: discriminates the type of data-plane plaintext frames (the `t` field) and performs minimal structural validation.
//
// Authoritative references (read-only comparisons, unmodified):
//   - M0 §2 (milestone event catalog) / §3 (command channel + snapshot response watermark · v1.8.12) / §6 (replay after reconnect):
//   - remote-relay/fixtures/data-plane-v1.json (28 real sample frames—this file and parseFrame.test.ts are
//     the implementation of their "C1-side real-path consumers"; see the coverage table at the top of the test file).
//   - Desktop production builders (read-only comparisons; the authoritative source for field naming):
//     the `build_*_payload` series + `classify` in app/src-tauri/src/remote_gateway.rs (four live variants).
//   - Relay plaintext-frame behavior (read-only comparison): remote-relay/src/room-do.js
//     (forwarding/cleanup logic for presence/input.ack/input.expired/replay.head).
//
// **Design boundary (intentionally narrowed)**: this layer only performs "t discrimination + structural/shape validation of the minimally required fields for that t"; it does not deeply inspect
// the exact schema of each block in blocks[] (that belongs to the rendering layer; see the header comment in blocks.ts). Frames with an unknown `t`, or a known
// `t` whose field shapes are incorrect, **do not throw**—they always return `{ ok:false, reason, t }` for the caller to count/ignore
// (brief §2: "unknown t or malformed frames do not crash; count and ignore them").

export * from "./frameTypes";
export * from "./frameBuilders";

import { isFiniteNumber, isRecord, isSha256Hex, isString, isStringArray, isStringOrNull, parseContentRef, reject } from "./frameHelpers";

import type {
  MsgCompletedFrame,
  HistoryResponseMessage,
  ParseResult,
  SessionIndexActiveRepo,
  SessionIndexRow,
  SnapshotResponsePartialMsg,
} from "./frameTypes";

/**
 * Discrimination + parsing entry point—`raw` is a decrypted (or inherently plaintext; see the M0 §1 plaintext exception) JSON value.
 * Does not throw: every rejection path uses `{ ok:false, reason, t }`.
 */
export function parseFrame(raw: unknown): ParseResult {
  if (!isRecord(raw)) {
    return reject("not_an_object", null);
  }
  const t = raw.t;
  if (!isString(t)) {
    return reject("missing_t", null);
  }

  switch (t) {
    case "session.index":
      return parseSessionIndex(raw, t);
    case "msg.completed":
      return parseMsgCompleted(raw, t);
    case "card.created":
      return parseCardCreated(raw, t);
    case "card.resolved":
      return parseCardResolved(raw, t);
    case "run.status":
      return parseRunStatus(raw, t);
    case "tool.completed":
      return parseToolCompleted(raw, t);
    case "text_delta":
    case "thinking_delta":
      return parseTextLikeDelta(raw, t);
    case "tool_output_delta":
      return parseToolOutputDelta(raw, t);
    case "usage_delta":
      return parseUsageDelta(raw, t);
    case "control.snapshot":
      return parseControlSnapshotRequest(raw, t);
    case "snapshot":
      return parseSnapshotResponse(raw, t);
    case "control.history":
      return parseControlHistoryRequest(raw, t);
    case "history":
      return parseHistoryResponse(raw, t);
    case "msg.fetch":
      return parseMsgFetchRequest(raw, t);
    case "msg.chunk":
      return parseMsgChunk(raw, t);
    case "msg.fetch.error":
      return parseMsgFetchError(raw, t);
    case "presence":
      return parsePresence(raw, t);
    case "input.ack":
      return parseInputAck(raw, t);
    case "input.expired":
      return parseInputExpired(raw, t);
    case "input.relay_queued":
      return parseInputRelayQueued(raw, t);
    case "replay.head":
      return parseReplayHead(raw, t);
    default:
      return reject("unknown_t", t);
  }
}

function parseSessionIndex(raw: Record<string, unknown>, t: "session.index"): ParseResult {
  if (raw.full === true) {
    if (!Array.isArray(raw.sessions)) return reject("malformed_fields", t);
    const sessions: SessionIndexRow[] = [];
    for (const entry of raw.sessions) {
      if (!isRecord(entry)) return reject("malformed_fields", t);
      const lastMsgPreview = entry.last_msg_preview;
      const lastActivityAt = entry.last_activity_at;
      const repoName = entry.repo_name;
      if (
        !isString(entry.id) ||
        !isString(entry.title) ||
        !isString(entry.repo_id) ||
        typeof entry.archived !== "boolean" ||
        !isStringOrNull(entry.status) ||
        !isStringOrNull(entry.run_id) ||
        !isFiniteNumber(entry.updated_at) ||
        (lastMsgPreview !== undefined && !isStringOrNull(lastMsgPreview)) ||
        (lastActivityAt !== undefined && lastActivityAt !== null && !isFiniteNumber(lastActivityAt)) ||
        (repoName !== undefined && !isStringOrNull(repoName))
      ) {
        return reject("malformed_fields", t);
      }
      sessions.push({
        id: entry.id,
        title: entry.title,
        repo_id: entry.repo_id,
        archived: entry.archived,
        status: entry.status,
        run_id: entry.run_id,
        updated_at: entry.updated_at,
        ...(lastMsgPreview !== undefined ? { last_msg_preview: lastMsgPreview } : {}),
        ...(lastActivityAt !== undefined ? { last_activity_at: lastActivityAt } : {}),
        ...(repoName !== undefined ? { repo_name: repoName } : {}),
      });
    }
    // Top-level repo summary—M2-4x; see the header comment on SessionIndexActiveRepo. Three states: key absent (older desktop) leaves
    // `repo` undefined; explicit `null` (fail-closed; no determinable active repo) is preserved as-is;
    // `{id, name}` is passed through after its shape is validated; an incorrect shape (the key exists but is not null/object, or the object lacks id/name
    // or has incorrect types) is rejected as a malformed frame rather than silently discarding this one field (which would cause the mobile client to display an incorrect "previously seen
    // project" without realizing it).
    const repoField = raw.repo;
    let repo: SessionIndexActiveRepo | null | undefined;
    if (repoField === undefined) {
      repo = undefined;
    } else if (repoField === null) {
      repo = null;
    } else if (isRecord(repoField) && isString(repoField.id) && isStringOrNull(repoField.name)) {
      repo = { id: repoField.id, name: repoField.name };
    } else {
      return reject("malformed_fields", t);
    }
    return {
      ok: true,
      frame: { t, full: true, sessions, ...(repo !== undefined ? { repo } : {}) },
    };
  }
  if (raw.full !== false) return reject("malformed_fields", t);

  const op = raw.op;
  switch (op) {
    case "created": {
      const session = raw.session;
      if (!isRecord(session)) return reject("malformed_fields", t);
      const repoName = session.repo_name;
      if (
        !isString(session.id) ||
        !isString(session.title) ||
        !isString(session.repo_id) ||
        !isString(session.namespace_id) ||
        typeof session.archived !== "boolean" ||
        (repoName !== undefined && !isStringOrNull(repoName))
      ) {
        return reject("malformed_fields", t);
      }
      return {
        ok: true,
        frame: {
          t,
          op: "created",
          full: false,
          session: {
            id: session.id,
            title: session.title,
            repo_id: session.repo_id,
            namespace_id: session.namespace_id,
            archived: session.archived,
            ...(repoName !== undefined ? { repo_name: repoName } : {}),
          },
        },
      };
    }
    case "renamed": {
      if (!isString(raw.id) || !isString(raw.title)) return reject("malformed_fields", t);
      return { ok: true, frame: { t, op: "renamed", full: false, id: raw.id, title: raw.title } };
    }
    case "archived":
    case "unarchived": {
      if (!isStringArray(raw.ids)) return reject("malformed_fields", t);
      return { ok: true, frame: { t, op, full: false, ids: raw.ids } };
    }
    case "deleted": {
      if (!isString(raw.id)) return reject("malformed_fields", t);
      return { ok: true, frame: { t, op: "deleted", full: false, id: raw.id } };
    }
    default:
      return reject("malformed_fields", t);
  }
}

function parseMsgCompleted(raw: Record<string, unknown>, t: "msg.completed"): ParseResult {
  if (!isFiniteNumber(raw.message_id) || !isString(raw.role) || !Array.isArray(raw.blocks)) {
    return reject("malformed_fields", t);
  }
  // agent is an optional key—when omitted, it is not validated; when present but not a string, it is treated as a malformed frame (as with the strict validation of other fields).
  if (raw.agent !== undefined && !isString(raw.agent)) {
    return reject("malformed_fields", t);
  }
  // An omitted content_ref leaves no parsed key; an invalid pointer rejects the frame to prevent broken fetch requests.
  // When omitted, this key does not appear at all on the frame; if present but incorrectly shaped, reject the entire frame (rather than silently discarding this one field—a partial `content_ref`
  // would cause the remote to issue msg.fetch with a bad pointer, which is more dangerous than "not having this field").
  if (raw.content_ref !== undefined && parseContentRef(raw.content_ref) === null) {
    return reject("malformed_fields", t);
  }
  // Optional revision is validated only when supplied and must be finite to keep version comparisons valid.
  // When it is not a finite number, it is treated as a malformed frame (as with the strict validation of other fields; do not silently discard this one field).
  if (raw.revision !== undefined && !isFiniteNumber(raw.revision)) {
    return reject("malformed_fields", t);
  }
  const frame: MsgCompletedFrame = { t, message_id: raw.message_id, role: raw.role, blocks: raw.blocks };
  if (isString(raw.agent)) frame.agent = raw.agent;
  if (raw.content_ref !== undefined) {
    const contentRef = parseContentRef(raw.content_ref);
    if (contentRef) frame.content_ref = contentRef;
  }
  if (isFiniteNumber(raw.revision)) frame.revision = raw.revision;
  return { ok: true, frame };
}

function parseCardCreated(raw: Record<string, unknown>, t: "card.created"): ParseResult {
  if (!isRecord(raw.block)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, block: raw.block } };
}

function parseCardResolved(raw: Record<string, unknown>, t: "card.resolved"): ParseResult {
  if (!isString(raw.decision_id) || !isString(raw.status) || !isStringOrNull(raw.chosen_option)) {
    return reject("malformed_fields", t);
  }
  return { ok: true, frame: { t, decision_id: raw.decision_id, status: raw.status, chosen_option: raw.chosen_option } };
}

function parseRunStatus(raw: Record<string, unknown>, t: "run.status"): ParseResult {
  if (!isString(raw.session_id) || !isString(raw.status) || !isStringOrNull(raw.run_id)) {
    return reject("malformed_fields", t);
  }
  return { ok: true, frame: { t, session_id: raw.session_id, status: raw.status, run_id: raw.run_id } };
}

function parseToolCompleted(raw: Record<string, unknown>, t: "tool.completed"): ParseResult {
  const exitCodeOk = raw.exit_code === null || isFiniteNumber(raw.exit_code);
  const outputOk = raw.output === null || isString(raw.output);
  if (!isString(raw.id) || !isString(raw.tool) || !isString(raw.status) || !exitCodeOk || !outputOk) {
    return reject("malformed_fields", t);
  }
  return {
    ok: true,
    frame: {
      t,
      id: raw.id,
      tool: raw.tool,
      status: raw.status,
      exit_code: (raw.exit_code as number | null) ?? null,
      output: (raw.output as string | null) ?? null,
    },
  };
}

function parseTextLikeDelta(raw: Record<string, unknown>, t: "text_delta" | "thinking_delta"): ParseResult {
  if (!isFiniteNumber(raw.seq) || !isString(raw.text)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, seq: raw.seq, text: raw.text } };
}

function parseToolOutputDelta(raw: Record<string, unknown>, t: "tool_output_delta"): ParseResult {
  if (!isFiniteNumber(raw.seq) || !isString(raw.id) || !isString(raw.text)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, seq: raw.seq, id: raw.id, text: raw.text } };
}

function parseUsageDelta(raw: Record<string, unknown>, t: "usage_delta"): ParseResult {
  if (!isFiniteNumber(raw.seq) || !isFiniteNumber(raw.input_tokens) || !isFiniteNumber(raw.output_tokens)) {
    return reject("malformed_fields", t);
  }
  return { ok: true, frame: { t, seq: raw.seq, input_tokens: raw.input_tokens, output_tokens: raw.output_tokens } };
}

function parseControlSnapshotRequest(raw: Record<string, unknown>, t: "control.snapshot"): ParseResult {
  if (!isString(raw.session)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, session: raw.session } };
}

function parseControlHistoryRequest(raw: Record<string, unknown>, t: "control.history"): ParseResult {
  if (!isString(raw.session) || (raw.before_message_id !== null && !isFiniteNumber(raw.before_message_id))) {
    return reject("malformed_fields", t);
  }
  return { ok: true, frame: { t, session: raw.session, before_message_id: raw.before_message_id as number | null } };
}

function parseHistoryResponse(raw: Record<string, unknown>, t: "history"): ParseResult {
  if (
    !isString(raw.session) ||
    (raw.before_message_id !== null && !isFiniteNumber(raw.before_message_id)) ||
    !Array.isArray(raw.messages) ||
    (raw.next_before !== null && !isFiniteNumber(raw.next_before))
  ) {
    return reject("malformed_fields", t);
  }
  const messages: HistoryResponseMessage[] = [];
  for (const message of raw.messages) {
    if (!isRecord(message) || !isFiniteNumber(message.message_id) || !isString(message.role) || !Array.isArray(message.blocks)) {
      return reject("malformed_fields", t);
    }
    // History preserves the completed-message invariant: omitted content_ref leaves no key; invalid pointers reject the frame.
    // When present but incorrectly shaped, reject the entire history frame (as above; do not "silently discard some fields").
    if (message.content_ref !== undefined && parseContentRef(message.content_ref) === null) {
      return reject("malformed_fields", t);
    }
    // History revisions are optional, but a supplied non-finite revision invalidates the entire history frame.
    // When it is not a finite number, reject the entire history frame.
    if (message.revision !== undefined && !isFiniteNumber(message.revision)) {
      return reject("malformed_fields", t);
    }
    const row: HistoryResponseMessage = { message_id: message.message_id, role: message.role, blocks: message.blocks };
    if (message.content_ref !== undefined) {
      const contentRef = parseContentRef(message.content_ref);
      if (contentRef) row.content_ref = contentRef;
    }
    if (isFiniteNumber(message.revision)) row.revision = message.revision;
    messages.push(row);
  }
  return {
    ok: true,
    frame: {
      t,
      session: raw.session,
      before_message_id: raw.before_message_id as number | null,
      messages,
      next_before: raw.next_before as number | null,
    },
  };
}

/**
 * Snapshot response (M0 §3 v1.8.12)—the shape invariants are fixed here, not "best-effort compatibility" at runtime:
 *   - `run_id === null` ⟺ `through_run_seq === null` ⟺ `partial_msg === null` (three nulls when idle).
 *   - `run_id !== null` ⟹ `through_run_seq` is a finite number `>= 1`—the value `through_run_seq === 0`
 *     was deprecated as of v1.8.12 (the production sequencer increments before returning; the first event is seq=1) and is rejected as a malformed frame.
 *   - When `run_id !== null`, `partial_msg` may be null (events have been included but there is no displayable content, such as usage-only
 *     events) or `{role, blocks}`—both are valid; this is not a hard constraint "bound to run_id".
 */
function parseSnapshotResponse(raw: Record<string, unknown>, t: "snapshot"): ParseResult {
  if (!isString(raw.session)) return reject("malformed_fields", t);
  const runId = raw.run_id;
  const throughRunSeq = raw.through_run_seq;
  const partialMsgRaw = raw.partial_msg;

  if (runId === null) {
    if (throughRunSeq !== null || partialMsgRaw !== null) return reject("malformed_fields", t);
    return { ok: true, frame: { t, session: raw.session, run_id: null, through_run_seq: null, partial_msg: null } };
  }

  if (!isString(runId)) return reject("malformed_fields", t);
  if (!isFiniteNumber(throughRunSeq) || throughRunSeq < 1 || !Number.isInteger(throughRunSeq)) {
    return reject("malformed_fields", t);
  }

  let partialMsg: SnapshotResponsePartialMsg | null = null;
  if (partialMsgRaw !== null) {
    if (!isRecord(partialMsgRaw) || !isString(partialMsgRaw.role) || !Array.isArray(partialMsgRaw.blocks)) {
      return reject("malformed_fields", t);
    }
    partialMsg = { role: partialMsgRaw.role, blocks: partialMsgRaw.blocks };
  }

  return {
    ok: true,
    frame: { t, session: raw.session, run_id: runId, through_run_seq: throughRunSeq, partial_msg: partialMsg },
  };
}

function parseMsgFetchRequest(raw: Record<string, unknown>, t: "msg.fetch"): ParseResult {
  if (
    !isString(raw.session) ||
    !isFiniteNumber(raw.message_id) ||
    !isFiniteNumber(raw.revision) ||
    !isFiniteNumber(raw.offset)
  ) {
    return reject("malformed_fields", t);
  }
  return {
    ok: true,
    frame: { t, session: raw.session, message_id: raw.message_id, revision: raw.revision, offset: raw.offset },
  };
}

function parseMsgChunk(raw: Record<string, unknown>, t: "msg.chunk"): ParseResult {
  if (
    !isFiniteNumber(raw.message_id) ||
    !isFiniteNumber(raw.revision) ||
    !isSha256Hex(raw.content_sha256) ||
    !isFiniteNumber(raw.total_bytes) ||
    !isFiniteNumber(raw.offset) ||
    !isFiniteNumber(raw.chunk_len) ||
    !isString(raw.bytes_b64)
  ) {
    return reject("malformed_fields", t);
  }
  return {
    ok: true,
    frame: {
      t,
      message_id: raw.message_id,
      revision: raw.revision,
      content_sha256: raw.content_sha256,
      total_bytes: raw.total_bytes,
      offset: raw.offset,
      chunk_len: raw.chunk_len,
      bytes_b64: raw.bytes_b64,
    },
  };
}

function parseMsgFetchError(raw: Record<string, unknown>, t: "msg.fetch.error"): ParseResult {
  if (!isString(raw.code)) return reject("malformed_fields", t);
  if (raw.code === "stale_revision") {
    const currentRef = parseContentRef(raw.current_ref);
    if (currentRef === null) return reject("malformed_fields", t);
    return { ok: true, frame: { t, code: raw.code, current_ref: currentRef } };
  }
  // · Unknown codes (not in the six-value `MSG_FETCH_ERROR_CODES` enumeration) no longer reject the entire frame—the old behavior caused the entire
  //   msg.fetch.error frame to be deemed malformed_fields at the `parseFrame()` layer, so the UI could not see this
  //   error at all (not even an "unavailable" prompt), which is worse than "render as an unknown error and allow the user to retry". Future protocol additions of
  //   error codes are an expected evolution (consistent with the existing tolerance for unknown fields in msg.completed/history), and old clients
  //   should degrade gracefully rather than immediately deem the entire frame invalid.
  // · If a non-stale_revision code (whether one of the other five known codes or an unknown code) unexpectedly carries `current_ref`—
  //   tolerate and ignore this extra field; no longer reject the entire frame because of it (M0 §10.5's "required only for stale_revision" still holds;
  //   this merely downgrades the consequence of "violating this rule" from "the entire frame is invalidated" to "ignore this field that should not be present").
  return { ok: true, frame: { t, code: raw.code } };
}

function parsePresence(raw: Record<string, unknown>, t: "presence"): ParseResult {
  if (!isString(raw.role) || !isString(raw.event)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, role: raw.role, event: raw.event } };
}

function parseInputAck(raw: Record<string, unknown>, t: "input.ack"): ParseResult {
  if (!isString(raw.command_id) || !isString(raw.outcome)) return reject("malformed_fields", t);
  return {
    ok: true,
    frame: {
      t,
      command_id: raw.command_id,
      outcome: raw.outcome,
      ...(isString(raw.reason) ? { reason: raw.reason } : {}),
    },
  };
}

function parseInputExpired(raw: Record<string, unknown>, t: "input.expired"): ParseResult {
  if (!isString(raw.command_id)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, command_id: raw.command_id } };
}

function parseInputRelayQueued(raw: Record<string, unknown>, t: "input.relay_queued"): ParseResult {
  if (!isString(raw.command_id) || !isFiniteNumber(raw.expires_at)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, command_id: raw.command_id, expires_at: raw.expires_at } };
}

function parseReplayHead(raw: Record<string, unknown>, t: "replay.head"): ParseResult {
  if (!isFiniteNumber(raw.epoch) || !isFiniteNumber(raw.headSeq)) return reject("malformed_fields", t);
  return { ok: true, frame: { t, epoch: raw.epoch, headSeq: raw.headSeq } };
}
