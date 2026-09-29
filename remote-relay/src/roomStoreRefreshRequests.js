"use strict";
import { hasTable, withTransaction } from "./roomStoreSchema.js";

// ---- refresh_requests (§9.6 line 246 · relay-side delivery accounting) ----

/**
 * Persist/renew a row when a phone's token.refresh arrives. Resent from a new connection with the same request_id → after confirming the
 * same subject, safely rebind connection_id/request_generation/deadline/
 * ip_bucket_key (a resend may have changed connection or IP, so the row must copy the latest one); a different
 * subject is always rejected (preventing a request_id collision from hijacking another subject's receipt pending delivery).
 * @returns {{ok:true}|{ok:false, reason:"refresh_request_subject_conflict"}}
 */
export function upsertRefreshRequest(sql, { requestId, subject, requestGeneration, connectionId, deadline, ipBucketKey = null }) {
  return withTransaction(sql, () => {
    const existing = sql.exec(
      "SELECT subject FROM refresh_requests WHERE request_id = ? LIMIT 1",
      requestId
    );
    if (existing.length > 0 && existing[0].subject !== subject) {
      return { ok: false, reason: "refresh_request_subject_conflict" };
    }
    sql.exec(
      "INSERT INTO refresh_requests (request_id, subject, request_generation, connection_id, deadline, ip_bucket_key) " +
        "VALUES (?, ?, ?, ?, ?, ?) " +
        "ON CONFLICT(request_id) DO UPDATE SET " +
        "request_generation = excluded.request_generation, " +
        "connection_id = excluded.connection_id, " +
        "deadline = excluded.deadline, " +
        "ip_bucket_key = excluded.ip_bucket_key",
      requestId,
      subject,
      requestGeneration,
      connectionId,
      deadline,
      ipBucketKey
    );
    return { ok: true };
  });
}

export function getRefreshRequest(sql, requestId) {
  const rows = sql.exec(
    "SELECT request_id, subject, request_generation, connection_id, deadline, ip_bucket_key " +
      "FROM refresh_requests WHERE request_id = ? LIMIT 1",
    requestId
  );
  return rows.length ? rows[0] : null;
}

export function deleteRefreshRequest(sql, requestId) {
  sql.exec("DELETE FROM refresh_requests WHERE request_id = ?", requestId);
}

// R1 (§9.6 line 249 · v1.8.5): Expiration means truly deleting rows, not filtering them at query time —
// nextRefreshRequestDeadline filters only deadline>now in SELECT; if rows themselves are never
// deleted, they remain forever: ① alarm scheduling spins on them doing nothing when it wakes; ② rows whose receipts never arrived
// remain occupied permanently; ③ lingering rows make isResend permanently true for that request_id, making it forever immune to the
// §9.8 6/min main bucket (a real abuse surface). alarm() calls this function to scan on every wake.
export function deleteExpiredRefreshRequests(sql, now) {
  sql.exec("DELETE FROM refresh_requests WHERE deadline <= ?", now);
}

// P2-3: One candidate for scheduleNextTokenAlarm — consider only rows in the future (deadline > now);
// expired rows must not pin alarm in the past and cause a busy loop that immediately uses the same expired time to
// call setAlarm again.
// SEC-3: hasTable guard (following the isClaimRateLimited/resolveTokenAdmission precedent) —
// scheduleNextTokenAlarm is now also called from new trigger points before fetch() authentication/business-schema creation
// (an unclaimed room's claim POST / a failed upgrade); on that path,
// the 11 business tables including refresh_requests may not exist at all; treat a miss as no candidate,
// do not create tables or throw a SQL exception.
export function nextRefreshRequestDeadline(sql, now) {
  if (!hasTable(sql, "refresh_requests")) return null;
  const rows = sql.exec("SELECT MIN(deadline) AS next FROM refresh_requests WHERE deadline > ?", now);
  const value = rows.length ? rows[0].next : null;
  return value == null ? null : Number(value);
}
