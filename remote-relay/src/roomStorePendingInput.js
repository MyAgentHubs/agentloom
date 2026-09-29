"use strict";
import { hasTable } from "./roomStoreSchema.js";
import { getTokenSubject } from "./roomStoreTokenRegistry.js";

// C1-TTL (second batch of dogfood fixes · no feedback when a phone sends a message while the desktop is offline): pending_input's stored
// TTL expiration time — one candidate for scheduleNextTokenAlarm, using the same “calculate in one place, set in one place” structure
// (as nextRefreshRequestDeadline). Consider only future rows (expires_at > now); expired
// rows must not pin alarm in the past; the hasTable guard has the same rationale — this function may also be called at a trigger point
// before the business schema is created.
export function nextPendingInputExpiry(sql, now) {
  if (!hasTable(sql, "pending_input")) return null;
  const rows = sql.exec("SELECT MIN(expires_at) AS next FROM pending_input WHERE expires_at > ?", now);
  const value = rows.length ? rows[0].next : null;
  return value == null ? null : Number(value);
}

// ---- input staging queue ----

const DEFAULT_INPUT_TTL_MS = 30 * 60 * 1000; // 30 minutes (parent document §3)

// C1-RQ (second batch of dogfood fixes · no feedback when a phone sends a message while the desktop is offline): return the expires_at actually
// applied by this enqueue — after a successful enqueue, the caller (room-do.js handleInput) needs this
// time to return an input.relay_queued frame telling the sender “staged, and when it expires.” The caller
// guarantees that, before reaching here, it has confirmed command_id is not in the table (see the idempotent branch in
// getPendingInputExpiry), so ON CONFLICT DO NOTHING is never triggered on this path — the return value is
// the now + ttlMs actually written this time, not “possibly the original value of an old row.”
export function enqueueInput(sql, {
  commandId,
  session,
  envelopeJson,
  now,
  subject = null,
  generation = null,
  ttlMs = DEFAULT_INPUT_TTL_MS,
}) {
  const expiresAt = now + ttlMs;
  sql.exec(
    "INSERT INTO pending_input " +
      "(command_id, session, envelope, created_at, expires_at, subject, generation) VALUES (?, ?, ?, ?, ?, ?, ?) " +
      "ON CONFLICT(command_id) DO NOTHING",
    commandId,
    session ?? null,
    envelopeJson,
    now,
    expiresAt,
    subject,
    generation
  );
  return { expiresAt };
}

/**
 * Fetch all staged input in ascending created_at order (FIFO), dividing it into “still deliverable within TTL”
 * and “expired and should be discarded” groups. The caller is responsible for: removePendingInput each expired item +
 * reply input.expired; send each deliverable item to the desktop connection that has just come online.
 */
export function drainDeliverableInput(sql, now) {
  const rows = sql.exec("SELECT * FROM pending_input ORDER BY created_at ASC, rowid ASC");
  const deliverable = [];
  const expired = [];
  for (const row of rows) {
    if (row.expires_at < now) {
      expired.push(row);
    } else {
      deliverable.push(row);
    }
  }
  return { deliverable, expired };
}

// G8 third-review fix_required (codex xhigh delta review) R3: before every enqueue,
// handleInput must first clear expired rows (see room-do.js purgeExpiredPendingInput); this path is now
// high-frequency (every offline input passes through it), so it cannot reuse drainDeliverableInput's
// approach of “SELECT * ... moving the entire table (including a possibly nearly 4 MB envelope large field) into JS”
// — drainDeliverableInput serves the low-frequency path where the desktop has just come online and already needs to forward
// the full contents of all staged input; the read costs of the two paths should be accounted separately, and this new path must not pull
// the old path into reading an unused large field too (drainDeliverableInput itself remains unchanged). Here it queries command_id only via the expires_at index, never touching the envelope column;
// SELECT and DELETE use the same `expires_at < now` predicate, and single-threaded synchronous execution
// without await ensures no other write can intervene between the two statements, so the sets of matching rows are byte-for-byte identical,
// equivalent to “first select this batch of IDs,
// then batch-delete by those IDs.”
export function deleteExpiredPendingInput(sql, now) {
  const rows = sql.exec("SELECT command_id FROM pending_input WHERE expires_at < ?", now);
  const commandIds = rows.map((row) => row.command_id);
  if (commandIds.length > 0) {
    sql.exec("DELETE FROM pending_input WHERE expires_at < ?", now);
  }
  return commandIds;
}

export function isPendingInputAuthorized(sql, row) {
  if (typeof row?.subject !== "string") return false;
  // S1i2 batch review 3③: a row whose generation is NULL must explicitly fail closed. The old implementation relied on
  // the arithmetic coincidence of Number(null) === 0 and token_subjects.generation always being > 0 (schema CHECK) to produce
  // the same result — if either side changes one day (for example, a future relaxation of CHECK), that coincidence
  // silently fails; here, “without generation, it does not count” is written as a visible branch.
  if (row?.generation === null || row?.generation === undefined) return false;
  if (!Number.isSafeInteger(Number(row.generation))) return false;
  const subject = getTokenSubject(sql, row.subject);
  return subject?.state === "active" && Number(row.generation) === Number(subject.generation);
}

export function removePendingInput(sql, commandId) {
  sql.exec("DELETE FROM pending_input WHERE command_id = ?", commandId);
}

// G8 R4 (dual-review fix_required④ · message rate limiting): before enqueueing, first determine whether this command_id is already
// in pending_input — if so, allow it directly with idempotent semantics: neither reinsert nor reject it.
// enqueueInput itself uses ON CONFLICT(command_id) DO NOTHING, so even without this
// check, passing directly through the capacity gate and inserting again would not create a second row; but if the queue is exactly full,
// the capacity gate would block this retry before it reaches enqueueInput and return a misleading
// queue_full — this function lets the caller distinguish “this is actually the same retry that has already succeeded” before capacity evaluation,
// excluding it from that capacity evaluation.
// C1-RQ (second batch of dogfood fixes · no feedback when a phone sends a message while the desktop is offline): change from a Boolean existence check to
// directly returning expires_at — on an idempotent hit, room-do.js handleInput now needs to use the existing row's
// expires_at to return an input.relay_queued confirmation frame (rather than silently returning as before);
// one lookup saves one SQL trip over “first use hasPendingInputCommandId to check existence, then on a hit query
// expires_at again.”
export function getPendingInputExpiry(sql, commandId) {
  const rows = sql.exec("SELECT expires_at FROM pending_input WHERE command_id = ? LIMIT 1", commandId);
  return rows.length ? Number(rows[0].expires_at) : null;
}

// G8 (C1 design v0.5 §8 · message rate limiting): before enqueueing, query the existing pending_input row count and
// total bytes for this room, so room-do.js handleInput can determine whether it exceeds PENDING_INPUT_ROW_LIMIT /
// PENDING_INPUT_BYTE_LIMIT. One DO = one room; pending_input is already this room's
// staging queue, so no room filter is needed. For byte count, use LENGTH(CAST(envelope AS BLOB))
// rather than bare LENGTH(envelope) — SQLite's LENGTH() on a TEXT column returns character count,
// and non-ASCII characters in ciphertext/session fields carried by envelope would make that character count smaller than the
// actual UTF-8 byte count; only CAST AS BLOB counts by bytes, consistently with the byte convention used by TextEncoder
// elsewhere in room-do.js (the same 128-byte limit convention for command_id/session).
export function pendingInputStats(sql) {
  const rows = sql.exec(
    "SELECT COUNT(*) AS row_count, COALESCE(SUM(LENGTH(CAST(envelope AS BLOB))), 0) AS total_bytes " +
      "FROM pending_input"
  );
  const row = rows.length ? rows[0] : { row_count: 0, total_bytes: 0 };
  return { rowCount: Number(row.row_count), totalBytes: Number(row.total_bytes) };
}
