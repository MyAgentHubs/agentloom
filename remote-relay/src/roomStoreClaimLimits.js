"use strict";
import { hasTable, withTransaction, getRoomState, isRoomLive } from "./roomStoreSchema.js";

const CLAIM_LIMIT = 30;
const CLAIM_WINDOW_MS = 60 * 60 * 1000;

// ---- Room ownership, claim bucket, and tombstone ----

export function isClaimRateLimited(sql, now = Date.now()) {
  if (!hasTable(sql, "claim_rate_limits")) return false;
  const rows = sql.exec("SELECT window_started_at, attempts FROM claim_rate_limits WHERE id = 1");
  if (rows.length === 0) return false;
  const row = rows[0];
  return now < Number(row.window_started_at) + CLAIM_WINDOW_MS && Number(row.attempts) >= CLAIM_LIMIT;
}

// SEC-1: The claim path (POST /room/<hex>/claim) needs only room_state plus this small table;
// it should not pull up the entire business schema — lazily create it on first use, with claim_rate_limits structured identically to the table in
// SCHEMA_STATEMENTS (CREATE TABLE IF NOT EXISTS is idempotent, so repeated calls are safe).
function recordClaimAttempt(sql, now) {
  sql.exec(`CREATE TABLE IF NOT EXISTS claim_rate_limits (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    window_started_at INTEGER NOT NULL,
    attempts          INTEGER NOT NULL DEFAULT 0
  )`);
  const rows = sql.exec("SELECT window_started_at, attempts FROM claim_rate_limits WHERE id = 1");
  if (rows.length === 0 || now >= Number(rows[0].window_started_at) + CLAIM_WINDOW_MS) {
    sql.exec(
      "INSERT INTO claim_rate_limits (id, window_started_at, attempts) VALUES (1, ?, 1) " +
        "ON CONFLICT(id) DO UPDATE SET window_started_at = excluded.window_started_at, attempts = 1",
      now
    );
    return;
  }
  sql.exec("UPDATE claim_rate_limits SET attempts = attempts + 1 WHERE id = 1");
}

export function claimRoom(sql, credentialHash, now = Date.now()) {
  return withTransaction(sql, () => {
    // edge 10/min is already enforced at the Worker layer; the tombstone is still rechecked inside the transaction to prevent
    // the room from being terminated while the body is read. The DO 30/h limit is a business bucket, evaluated after 410, and idempotent retries with the same hash
    // are neither affected by it nor counted.
    const state = getRoomState(sql);
    if (state.tombstoned_at != null) return "tombstoned";
    if (state.owner_credential_hash === credentialHash) return "same";
    if (isClaimRateLimited(sql, now)) return "rate_limited";

    recordClaimAttempt(sql, now);
    if (state.owner_credential_hash != null) return "conflict";
    sql.exec(
      "UPDATE room_state SET owner_credential_hash = ? WHERE owner_credential_hash IS NULL",
      credentialHash
    );
    return "claimed";
  });
}

// G8 R1 (dual-review fix_required① · message rate limiting): fixed per-subject windows for input/control —
// one call = one attempt; returns whether it is allowed and whether this is the first exceedance in this window.
//
// Unlike the two-stage isClaimRateLimited/recordClaimAttempt flow above in claimRoom (read-only
// decision first, then a conditional write), this intentionally combines them into one step; however, the way attempts is written was revised in
// G8's third-review fix_required (codex xhigh delta review) R2 — **the first exceedance must still
// write to the database** (moving attempts from limit to limit+1; that exact limit+1 value itself
// signals “the first exceedance in this window,” and room-do.js uses it to decide whether to record a protocol violation,
// see G8 R3), but **starting with the second exceedance, attempts saturates and remains fixed at limit+1, followed by read-only
// processing with no writes**: if the read attempts is already > limit, reject directly, with firstExceedThisWindow
// always false, without even touching SQL. The old version unconditionally UPDATEd on every exceedance, which meant an attacker could obtain unlimited database writes for free
// just by frantically sending frames that would be rejected anyway — precisely violating the principle stated in the G8 constant comment in room-do.js:
// “do not turn indiscriminate frame sending into free writes” (echoing the same §6b discipline against blowups; the PROTOCOL_VIOLATION_LIMIT
// header comment follows the same rationale).
// There is no await between read-modify-write (the same L2 invariant as in allocateSeq's header comment); this is inherently atomic
// within a single JS call, so no additional transaction wrapper is needed.
export function takeMessageRateSlot(sql, { subject, channel, limit, windowMs, now }) {
  const rows = sql.exec(
    "SELECT window_started_at, attempts FROM message_rate_limits WHERE subject = ? AND channel = ?",
    subject, channel
  );
  const existing = rows.length ? rows[0] : null;
  const windowExpired = !existing || now >= Number(existing.window_started_at) + windowMs;

  if (windowExpired) {
    sql.exec(
      "INSERT INTO message_rate_limits (subject, channel, window_started_at, attempts) VALUES (?, ?, ?, 1) " +
        "ON CONFLICT(subject, channel) DO UPDATE SET window_started_at = excluded.window_started_at, attempts = 1",
      subject, channel, now
    );
    return { allowed: true, firstExceedThisWindow: false };
  }

  const priorAttempts = Number(existing.attempts);
  if (priorAttempts > limit) {
    // Already saturated at limit+1 — this is not the first exceedance in this window; make a purely read-only decision and never write to the database again.
    return { allowed: false, firstExceedThisWindow: false };
  }

  // priorAttempts <= limit: either it is still within budget (after this write it remains <= limit, so allow it),
  // or it is exactly the call that pushes the quota from limit past limit+1 (after this write it is > limit, making it the
  // first exceedance in this window) — both cases require an actual database write; after the write, attempts will never
  // exceed limit+1, because calls above limit return directly from the branch above and never
  // reach here to continue incrementing.
  const nextAttempts = priorAttempts + 1;
  sql.exec(
    "UPDATE message_rate_limits SET attempts = ? WHERE subject = ? AND channel = ?",
    nextAttempts, subject, channel
  );
  return { allowed: nextAttempts <= limit, firstExceedThisWindow: nextAttempts === limit + 1 };
}

// Check the projected byte total before writing so a rejected frame cannot
// inflate the recorded usage and cause later frames to be rejected early.

/**
 * Fixed-window byte-budget decision: it shares takeMessageRateSlot's “(key) → window start + accumulated amount”
 * structure, except that it accumulates bytes rather than counts, so its decision/write approach differs — a single reply
 * frame's byte size can push the budget at once from “far below the limit” to “far above the limit” (unlike attempts,
 * which only increases by +1 each time), so it must **first calculate “will this write exceed the limit?” and then decide whether to write**, rather than copying
 * the attempts order of “write first, then see whether it exceeded the limit afterward” (which would make the over-limit frame itself
 * count toward bytes_sent, contaminating the actual used amount within the window and causing the next decision to trigger an earlier
 * false-positive). When the window expires, the new window starts from 0 and excludes only the rejected bytesDelta
 * — it does not block the window itself from rolling over.
 * @returns {{allowed:boolean}}
 */
export function takeReplyByteBudget(sql, { subject, limitBytes, windowMs, bytesDelta, now }) {
  const rows = sql.exec(
    "SELECT window_started_at, bytes_sent FROM reply_byte_limits WHERE subject = ? LIMIT 1",
    subject
  );
  const existing = rows.length ? rows[0] : null;
  const windowExpired = !existing || now >= Number(existing.window_started_at) + windowMs;
  const priorBytes = windowExpired ? 0 : Number(existing.bytes_sent);
  const windowStartedAt = windowExpired ? now : Number(existing.window_started_at);
  const nextBytes = priorBytes + bytesDelta;

  if (nextBytes > limitBytes) {
    // Reject this frame, but the window rollover (if it occurs) must still be persisted — otherwise the next frame will again use the stale
    // window_started_at to determine whether it has expired; the rejected bytesDelta itself
    // is not counted toward bytes_sent.
    sql.exec(
      "INSERT INTO reply_byte_limits (subject, window_started_at, bytes_sent) VALUES (?, ?, ?) " +
        "ON CONFLICT(subject) DO UPDATE SET window_started_at = excluded.window_started_at, bytes_sent = excluded.bytes_sent",
      subject, windowStartedAt, priorBytes
    );
    return { allowed: false };
  }

  sql.exec(
    "INSERT INTO reply_byte_limits (subject, window_started_at, bytes_sent) VALUES (?, ?, ?) " +
      "ON CONFLICT(subject) DO UPDATE SET window_started_at = excluded.window_started_at, bytes_sent = excluded.bytes_sent",
    subject, windowStartedAt, nextBytes
  );
  return { allowed: true };
}

export function tombstoneRoom(sql, now = Date.now()) {
  return withTransaction(sql, () => {
    if (!isRoomLive(sql)) return false;
    const tables = sql.exec(
      "SELECT name FROM sqlite_master " +
        "WHERE type = 'table' AND name <> 'room_state' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    );
    for (const { name } of tables) {
      // name comes from sqlite_master rather than the request; nevertheless, fully escape double quotes to prevent future business
      // table names with special characters from causing purge drift. room_state is the only retained table.
      sql.exec(`DELETE FROM "${String(name).replaceAll('"', '""')}"`);
    }
    // room_state is the sole source of truth untouched by purge; the tombstone must be the transaction's final write.
    sql.exec("UPDATE room_state SET tombstoned_at = ?", now);
    return true;
  });
}
