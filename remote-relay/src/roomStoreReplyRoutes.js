"use strict";
import { hasTable, withTransaction } from "./roomStoreSchema.js";

// Persist reply routing so transfers remain addressable across hibernation.

/**
 * Register/renew a row when a phone sends a `control` frame (relay cannot see, and does not need to
 * distinguish, whether the encrypted body is `msg.fetch`) carrying a top-level command_id. The same command_id
 * from the same subject reconnecting → safely rebind connection_id (§10.3 “same subject reconnect: rebinding allowed”),
 * **preserving deadline/bytes_sent unchanged rather than resetting them** (the literal basis is the same line: “in coordination with §10.4
 * offset resumption” — the continuity of those two fields is what resumption concatenation relies on; a reconnect is not
 * a new fetch); the same command_id from a different subject → reject (§10.3 “command_id across subjects:
 * reject … routing must not be hijacked”), following the same precedent pattern as
 * upsertRefreshRequest.
 * @returns {{ok:true}|{ok:false, reason:"reply_route_subject_conflict"}}
 */
export function upsertReplyRoute(sql, { commandId, subject, generation = null, connectionId, deadline }) {
  return withTransaction(sql, () => {
    const existing = sql.exec(
      "SELECT subject FROM reply_routes WHERE command_id = ? LIMIT 1",
      commandId
    );
    if (existing.length > 0 && existing[0].subject !== subject) {
      return { ok: false, reason: "reply_route_subject_conflict" };
    }
    sql.exec(
      "INSERT INTO reply_routes (command_id, subject, generation, connection_id, deadline, bytes_sent) " +
        "VALUES (?, ?, ?, ?, ?, 0) " +
        "ON CONFLICT(command_id) DO UPDATE SET " +
        "connection_id = excluded.connection_id, " +
        "generation = excluded.generation",
      commandId,
      subject,
      generation,
      connectionId,
      deadline
    );
    return { ok: true };
  });
}

export function getReplyRoute(sql, commandId) {
  const rows = sql.exec(
    "SELECT command_id, subject, generation, connection_id, deadline, bytes_sent " +
      "FROM reply_routes WHERE command_id = ? LIMIT 1",
    commandId
  );
  return rows.length ? rows[0] : null;
}

// Recheck the route deadline on delivery because alarm cleanup may lag
// behind expiration. A late reply must not revive an expired route
// or be delivered through it. Remove expired routes on the delivery path
// immediately instead of waiting for the next alarm.
export function deleteReplyRoute(sql, commandId) {
  sql.exec("DELETE FROM reply_routes WHERE command_id = ?", commandId);
}

// Count stored routes before admitting another to enforce the room limit.
// Routes already removed by expiration checks or cleanup no longer count
// toward capacity, allowing their slots to be reused.
export function countReplyRoutes(sql) {
  const rows = sql.exec("SELECT COUNT(*) AS n FROM reply_routes");
  return rows.length ? Number(rows[0].n) : 0;
}

/**
 * Called after the desktop successfully forwards each `reply` frame: add the full frame byte count to bytes_sent (§10.3
 * line 295, “the billing unit must be an amount observable by relay itself … count its full-frame byte count in
 * bytes_sent”), and extend deadline (§10.3, “keep alive during multi-chunk transmission … the row cannot be deleted
 * as soon as the first chunk is sent” — relay does not decrypt ct and cannot determine whether this is the terminal frame
 * for this fetch, so it can only extend lifetime through continued forwarding activity, ultimately letting TTL/alarm naturally finish it).
 */
export function bumpReplyRouteDelivery(sql, commandId, { bytesDelta, deadline }) {
  sql.exec(
    "UPDATE reply_routes SET bytes_sent = bytes_sent + ?, deadline = ? WHERE command_id = ?",
    bytesDelta,
    deadline,
    commandId
  );
}

// Expiration means truly deleting rows, not filtering them at query time — the same
// lesson as the deleteExpiredRefreshRequests header comment (v1.8.5): lingering rows permanently distort replay decisions/resource consumption. alarm() calls
// this function to scan on every wake.
export function deleteExpiredReplyRoutes(sql, now) {
  sql.exec("DELETE FROM reply_routes WHERE deadline <= ?", now);
}

// One candidate for scheduleNextTokenAlarm, with the same structure as nextRefreshRequestDeadline
// (hasTable guard + consider only the future).
export function nextReplyRouteDeadline(sql, now) {
  if (!hasTable(sql, "reply_routes")) return null;
  const rows = sql.exec("SELECT MIN(deadline) AS next FROM reply_routes WHERE deadline > ?", now);
  const value = rows.length ? rows[0].next : null;
  return value == null ? null : Number(value);
}

// ---- pairing_routes (S1i3 F1 · §9.5 line 235 · relay-side pairing-route persistence) ----

/**
 * Upsert on both pair.hello / pair.done forwarding (S1i3 F1 rework · §9.5 line 235 literally requires both frame types to
 * “write persistently”): subject is always "pairing", and the value is the
 * connection_id of the remote connection that initiated/completed this pairing round (determined by relay itself; do not trust the phone's self-report). A one-row table naturally overwrites the route left by the previous pairing round.
 * done must also write; do not rely only on the row written at hello still pointing at the correct connection — the phone may disconnect and reconnect in the accept→ready
 * window (after reconnection, connection_id is a new random value), or the hello from a second phone in the same window may have moved
 * the route first; in both cases, done must rewrite the route row to the connection that actually sent this done, otherwise ready will be delivered
 * nowhere or to the wrong target (see the comment on the pair.done branch in room-do.js for details). The only write entry point is room-do.js's
 * recordPairingRoute(), which first checks assertRoomLive() before calling it.
 */
export function setPairingRoute(sql, connectionId) {
  sql.exec(
    "INSERT INTO pairing_routes (subject, connection_id) VALUES ('pairing', ?) " +
      "ON CONFLICT(subject) DO UPDATE SET connection_id = excluded.connection_id",
    connectionId
  );
}

/**
 * Read for directed delivery of pair.accept / pair.ready: returns the connection_id recorded by the most recent pair.hello;
 * returns null when no pairing has ever occurred (the room-do.js caller safely discards based on this; it is not an error).
 */
export function getPairingRoute(sql) {
  const rows = sql.exec(
    "SELECT connection_id FROM pairing_routes WHERE subject = 'pairing' LIMIT 1"
  );
  return rows.length ? rows[0].connection_id : null;
}
