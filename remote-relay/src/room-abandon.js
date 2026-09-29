"use strict";

import { getMeta, setMeta } from "./roomStoreMeta.js";
import { hasTable } from "./roomStoreSchema.js";

// Reclaim of rooms that authenticated once but were then abandoned. Claim is
// unauthenticated, so anyone can claim a room and connect once with their own
// credential (which creates the business schema); without this those rooms
// would live forever. A room is only reclaimed when it is provably worthless:
// no live sockets, no phone pairing, no stored data, and idle for a long time.
export const ABANDONED_ROOM_RECLAIM_MS = 7 * 24 * 60 * 60 * 1000;
export const LAST_ACTIVITY_KEY = "last_activity_at";

// Tables where any row means the room holds something a user may still need.
const ANY_ROW_TABLES = [
  "token_subjects", // paired phones, including revoked ones
  "token_aliases",
  "token_put_fingerprints",
  "pairing_routes", // pairing in progress
  "events", // milestone history
];
// Tables whose rows only matter until their deadline (alarm purges them later).
const UNEXPIRED_ROW_TABLES = [
  ["pending_input", "expires_at"], // input waiting for the desktop
  ["refresh_requests", "deadline"], // in-flight token refresh receipts
  ["reply_routes", "deadline"], // desktop replies still routable to a phone
];

// Called on authenticated connect and on disconnect; the idle clock starts when
// the last connection goes away.
export function touchRoomActivity(sql, now = Date.now()) {
  if (hasTable(sql, "room_meta")) setMeta(sql, LAST_ACTIVITY_KEY, now);
}

function roomHoldsData(sql, now) {
  for (const table of ANY_ROW_TABLES) {
    if (hasTable(sql, table) && sql.exec(`SELECT 1 AS present FROM ${table} LIMIT 1`).length > 0) return true;
  }
  for (const [table, deadlineColumn] of UNEXPIRED_ROW_TABLES) {
    if (
      hasTable(sql, table) &&
      sql.exec(`SELECT 1 AS present FROM ${table} WHERE ${deadlineColumn} > ? LIMIT 1`, now).length > 0
    ) {
      return true;
    }
  }
  return false;
}

// Returns the time at which the room becomes reclaimable, or null when it must
// not be reclaimed (never authenticated, live sockets, or holds data).
export function abandonedReclaimAt(sql, liveSocketCount, now = Date.now()) {
  if (liveSocketCount > 0 || !hasTable(sql, "room_meta")) return null;
  if (roomHoldsData(sql, now)) return null;
  const lastActivity = getMeta(sql, LAST_ACTIVITY_KEY);
  if (lastActivity == null) {
    // Room authenticated before activity tracking existed: start the clock now
    // rather than guessing it is old.
    touchRoomActivity(sql, now);
    return now + ABANDONED_ROOM_RECLAIM_MS;
  }
  return Number(lastActivity) + ABANDONED_ROOM_RECLAIM_MS;
}

// alarm() branch: wipe the whole database (no tombstone; the desktop's next
// connect gets 401 and re-claims). Returns true when the room was reclaimed.
export async function reclaimIfAbandoned(ctx, sql, now = Date.now()) {
  const reclaimAt = abandonedReclaimAt(sql, ctx.getWebSockets().length, now);
  if (reclaimAt == null || now < reclaimAt) return false;
  await ctx.storage.deleteAlarm?.();
  await ctx.storage.deleteAll();
  return true;
}

// scheduleNextTokenAlarm candidate: fold the reclaim time into the running minimum.
export function withAbandonedCandidate(sql, liveSocketCount, now, nearest) {
  const reclaimAt = abandonedReclaimAt(sql, liveSocketCount, now);
  return reclaimAt != null && (nearest == null || reclaimAt < nearest) ? reclaimAt : nearest;
}
