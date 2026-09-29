"use strict";

// ---- room_meta（kv） ----

export function getMeta(sql, key, fallback = null) {
  const rows = sql.exec("SELECT value FROM room_meta WHERE key = ?", key);
  return rows.length ? rows[0].value : fallback;
}

export function setMeta(sql, key, value) {
  sql.exec(
    "INSERT INTO room_meta (key, value) VALUES (?, ?) " +
      "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    key,
    String(value)
  );
}

export function ensureRoomId(sql, roomId) {
  const existing = getMeta(sql, "room_id");
  if (!existing) {
    setMeta(sql, "room_id", roomId);
    return roomId;
  }
  return existing;
}

// ---- epoch (prevents double writes, parent document §0/§6 step 4) ----

export function getCurrentEpoch(sql) {
  return Number(getMeta(sql, "current_epoch", "0"));
}

// Each time the desktop connects, relay increments the room epoch by 1; writes using an old epoch are thereafter always rejected.
export function bumpEpoch(sql) {
  const next = getCurrentEpoch(sql) + 1;
  setMeta(sql, "current_epoch", next);
  return next;
}

// ---- Milestone events (events) + seq counter ----

// The authoritative source of seq = the independent counter in room_meta (key "seq_counter"), not
// A persistent counter preserves the highest sequence allocated in the room.
// Deriving that value from MAX(seq) could move it backward after event
// deletion, allowing subsequent allocations to reuse previously issued values.
export function headSeq(sql) {
  return Number(getMeta(sql, "seq_counter", "0"));
}

// 【L2 invariant】No await may be inserted in allocateSeq between “read the counter” and “write back +1”:
// Cloudflare DO's ctx.storage.sql is a synchronous API, and all functions in this file also
// remain fully synchronous, so this code inherently ensures that “read-modify-write” cannot be interrupted; if anyone later changes it to
// asynchronous (for example, inserting a network call in the middle), they must first determine how to preserve this section's atomicity, or else
// two concurrent calls could read the same old value and allocate duplicate seq values.
function allocateSeq(sql) {
  const next = headSeq(sql) + 1;
  setMeta(sql, "seq_counter", next);
  return next;
}

/**
 * Insert one milestone event; seq is allocated by this function (a room-level monotonic counter +1; see allocateSeq).
 * If the supplied epoch lags behind the room's current epoch, reject the write (preventing double writes) — note that this rejection
 * occurs before seq allocation, so a rejected write does not consume a seq number.
 * @returns {{ok:true, seq:number}|{ok:true, seq:number, dedup:true}|{ok:false, reason:"stale_epoch", currentEpoch:number}}
 */
export function insertMilestone(sql, { epoch, session, kind, ct, n, ts, client_msg_id = null }) {
  const currentEpoch = getCurrentEpoch(sql);
  if (epoch < currentEpoch) {
    return { ok: false, reason: "stale_epoch", currentEpoch };
  }

  // client_msg_id deduplication (parent document v1.7.4): an existing-row hit → idempotent success, returning the existing
  // seq without consuming a new seq or reinserting. All sql.exec calls in a DO are inherently single-threaded and serial (the same L2
  // invariant), so this “check then insert” needs no additional lock.
  if (client_msg_id) {
    const existing = sql.exec("SELECT seq FROM events WHERE client_msg_id = ?", client_msg_id);
    if (existing.length > 0) {
      return { ok: true, seq: existing[0].seq, dedup: true };
    }
  }

  const seq = allocateSeq(sql);
  sql.exec(
    "INSERT INTO events (seq, epoch, session, kind, ct, n, ts, client_msg_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    seq,
    epoch,
    session ?? null,
    kind,
    ct,
    n,
    ts,
    client_msg_id ?? null
  );
  return { ok: true, seq };
}

// Resend after reconnection (parent document §6 step 1): milestones after lastSeq, in ascending seq order.
export function replaySince(sql, lastSeq) {
  const n = Number(lastSeq) || 0;
  return sql.exec(
    "SELECT seq, epoch, session, kind, ct, n, ts, client_msg_id FROM events WHERE seq > ? ORDER BY seq ASC",
    n
  );
}

// ---- Quota counter ----

export function getQuotaCount(sql, period) {
  const rows = sql.exec("SELECT milestone_count FROM quota_counters WHERE period = ?", period);
  return rows.length ? Number(rows[0].milestone_count) : 0;
}

export function incrementQuotaCount(sql, period, by = 1) {
  sql.exec(
    "INSERT INTO quota_counters (period, milestone_count) VALUES (?, ?) " +
      "ON CONFLICT(period) DO UPDATE SET milestone_count = milestone_count + excluded.milestone_count",
    period,
    by
  );
}
