// RoomDO rate-limiting logic lives in this module.
// Each exported handler takes the RoomDO instance as its first argument.
// In-memory buckets intentionally reset when the Durable Object hibernates.

import * as store from "./room-store.js";
import { safeAttachment } from "./room-do-attachment.js";

// Use a distinct close reason so a throttled client backs off instead of
// treating the close as a bad credential and starting the pairing flow.
const MESSAGE_RATE_LIMIT_CLOSE_CODE = 1008;
const MESSAGE_RATE_LIMIT_CLOSE_REASON = "message_rate_limited";
const UPGRADE_FINE_LIMIT = 60;
const UPGRADE_COARSE_LIMIT = 120;
const UPGRADE_FAILURE_WINDOW_MS = 60_000;
const PAIR_HELLO_LIMIT = 6;
const PAIR_HELLO_WINDOW_MS = 60_000;
const SUBJECT_SOCKET_LIMIT = 4;

// Charge every inbound remote frame after the scope check, before plaintext
// and envelope routing. This includes malformed envelopes that would never
// reach input/control handlers and should not get a free validation probe.
// A single subject using four sockets can send at most 240 valid input and
// control frames per minute, so the shared IP threshold leaves twice that
// headroom. Shared IPs may see errors but no disconnections.
// Without CF-Connecting-IP, local connections share one bucket.
const IP_MESSAGE_RATE_LIMIT = 480;
const IP_MESSAGE_RATE_WINDOW_MS = 60_000;

// Cap distinct IP keys in long-lived isolates. Sweep expired buckets only
// when a new key arrives at capacity; reject the frame if capacity remains
// full, without evicting an active bucket or resetting its rate limit.
// Existing keys continue to be counted normally even when the map is full.
const IP_MESSAGE_BUCKET_MAX_ENTRIES = 4096;

export async function rejectUpgradeAuthentication(room, ipBucketKey, hashPrefix, now) {
  const fineKey = `${ipBucketKey}:${hashPrefix}`;
  const limited = bucketAtLimit(room.upgradeFineBuckets, fineKey, UPGRADE_FINE_LIMIT, now) ||
    bucketAtLimit(room.upgradeCoarseBuckets, ipBucketKey, UPGRADE_COARSE_LIMIT, now);
  recordBucketHit(room.upgradeFineBuckets, fineKey, now);
  recordBucketHit(room.upgradeCoarseBuckets, ipBucketKey, now);
  // A failed upgrade can instantiate an unclaimed room. This path never reaches
  // the post-handshake alarm scheduling in fetch(), so arm or recompute its
  // fixed reclamation alarm here. Claimed rooms must not be rescheduled for
  // reclamation. The error response is returned normally afterward.
  if (store.getRoomState(room.sql).owner_credential_hash == null) {
    await room.scheduleNextTokenAlarm(now);
  }
  return new Response(limited ? "rate limited" : "unauthorized", { status: limited ? 429 : 401 });
}

export function takePairHelloSlot(room, ws, now) {
  const attachment = safeAttachment(ws);
  if (!Number.isSafeInteger(attachment.pair_hello_window_started_at) ||
      now >= attachment.pair_hello_window_started_at + PAIR_HELLO_WINDOW_MS) {
    attachment.pair_hello_window_started_at = now;
    attachment.pair_hello_attempts = 0;
  }
  if (Number(attachment.pair_hello_attempts) >= PAIR_HELLO_LIMIT) {
    room.recordProtocolViolation(ws);
    try {
      ws.send(JSON.stringify({ t: "error", reason: "pair_hello_rate_limited" }));
    } finally {
      room.closeSocketForReauthorization(ws);
    }
    return false;
  }
  attachment.pair_hello_attempts = Number(attachment.pair_hello_attempts) + 1;
  room.assertAttachmentBudget(attachment);
  ws.serializeAttachment(attachment);
  return true;
}

// A per-subject SQL window replaces socket attachment counters: a paired
// device can reconnect at will, but its rate budget must survive reconnects,
// parallel sockets, and hibernation. Input and control share this logic with
// distinct channel, limit, and reason values; separate attachment handlers
// previously had to read and write different attachment fields.
export function takeSubjectChannelRateSlot(room, ws, envelope, { channel, limit, windowMs, reason }, now) {
  const subject = safeAttachment(ws).subject;
  // authorizeInboundSocket rejects sockets without a subject, so this should
  // be unreachable. If that guard changes, leave message routing available
  // rather than denying it because the rate limiter's premise no longer holds.
  if (!subject) return true;

  const result = store.takeMessageRateSlot(room.sql, { subject, channel, limit, windowMs, now });
  if (result.allowed) return true;

  // Count only the first over-limit frame in a window as one protocol
  // violation. A burst of rejected frames must not alone reach the socket's
  // eight-violation disconnect threshold within the same window.
  let exceeded = false;
  if (result.firstExceedThisWindow) {
    exceeded = room.recordProtocolViolation(ws);
  }
  const error = { t: "error", reason, frame: channel };
  // Include the validated input/control command_id so the client can match
  // this rejection to the frame it sent.
  if (envelope && typeof envelope.command_id === "string") error.command_id = envelope.command_id;
  ws.send(JSON.stringify(error));
  // Use the dedicated close reason for throttling. Reauthorization's reason
  // would make the client treat excess traffic as a bad credential and pair
  // again; normal backoff and reconnection should recover from this limit.
  if (exceeded) room.closeSocketForMessageRateLimit(ws);
  return false;
}

// Reuse the upgrade bucket primitives for a coarse per-IP limit. The handshake
// writes ip_bucket_key; an older socket missing it bypasses this guard so a
// missing attachment field cannot disable all message routing. Shared-IP
// traffic is rejected without disconnecting sockets. Aggregate violations
// are recorded without consuming any individual socket's violation budget.
export function takeIpMessageSlot(room, ws, frameType, commandId, now) {
  const ipBucketKey = safeAttachment(ws).ip_bucket_key;
  if (typeof ipBucketKey !== "string" || ipBucketKey.length === 0) return true;

  const isNewKey = !room.ipMessageBuckets.has(ipBucketKey);
  if (isNewKey && room.ipMessageBuckets.size >= IP_MESSAGE_BUCKET_MAX_ENTRIES) {
    evictExpiredBuckets(room.ipMessageBuckets, now, IP_MESSAGE_RATE_WINDOW_MS);
    if (room.ipMessageBuckets.size >= IP_MESSAGE_BUCKET_MAX_ENTRIES) {
      room.recordProtocolViolation(null);
      const capError = { t: "error", reason: "ip_message_rate_limited", frame: frameType };
      if (typeof commandId === "string") capError.command_id = commandId;
      ws.send(JSON.stringify(capError));
      return false;
    }
  }

  const limited = bucketAtLimit(
    room.ipMessageBuckets, ipBucketKey, IP_MESSAGE_RATE_LIMIT, now, IP_MESSAGE_RATE_WINDOW_MS
  );
  recordBucketHit(room.ipMessageBuckets, ipBucketKey, now, IP_MESSAGE_RATE_WINDOW_MS);
  if (limited) {
    room.recordProtocolViolation(null);
    const error = { t: "error", reason: "ip_message_rate_limited", frame: frameType };
    // This check runs before validateEnvelope, so commandId comes from raw
    // payload. Echo it only when it is a string; presence, pair.hello, and
    // token.refresh frames normally have no command_id to echo.
    if (typeof commandId === "string") error.command_id = commandId;
    ws.send(JSON.stringify(error));
    return false;
  }
  return true;
}

// A subject that reaches the protocol violation threshold gets the dedicated
// message-rate close reason rather than the reauthorization close reason.
export function closeSocketForMessageRateLimit(room, ws) {
  try {
    ws.close(MESSAGE_RATE_LIMIT_CLOSE_CODE, MESSAGE_RATE_LIMIT_CLOSE_REASON);
  } catch {
    // As with closeSocketForReauthorization, close/alarm may race delivery.
  } finally {
    room.flushProtocolViolations();
  }
}

export function enforceSubjectSocketLimit(room, subject) {
  const sockets = room.ctx.getWebSockets()
    .filter((ws) => {
      const attachment = safeAttachment(ws);
      return attachment.subject === subject &&
        attachment.subject_limit_closed !== true &&
        room.isOfficialAttachmentLive(attachment, Date.now());
    })
    .sort((left, right) =>
      Number(safeAttachment(left).connectedAt || 0) - Number(safeAttachment(right).connectedAt || 0)
    );
  if (sockets.length < SUBJECT_SOCKET_LIMIT) return;
  const oldest = sockets[0];
  const attachment = safeAttachment(oldest);
  attachment.subject_limit_closed = true;
  try {
    oldest.serializeAttachment(attachment);
    oldest.send(JSON.stringify({ t: "error", reason: "subject_socket_limit" }));
  } finally {
    room.closeSocketForReauthorization(oldest);
  }
}

// The optional window keeps the upgrade-failure default. The per-IP message
// bucket passes its own window explicitly even though both currently equal
// one minute, so the two policies remain independent.
function bucketAtLimit(buckets, key, limit, now, windowMs = UPGRADE_FAILURE_WINDOW_MS) {
  const bucket = buckets.get(key);
  if (!bucket || now >= bucket.startedAt + windowMs) return false;
  return bucket.attempts >= limit;
}

function recordBucketHit(buckets, key, now, windowMs = UPGRADE_FAILURE_WINDOW_MS) {
  const bucket = buckets.get(key);
  if (!bucket || now >= bucket.startedAt + windowMs) {
    buckets.set(key, { startedAt: now, attempts: 1 });
    return;
  }
  bucket.attempts += 1;
}

// A long-lived isolate may encounter many more IP keys because this bucket
// covers all inbound remote frames, not just input and control. The Map has no
// TTL of its own: remove expired entries only when capacity is reached. This
// is not an exact LRU and avoids an O(n) scan on ordinary traffic.
function evictExpiredBuckets(buckets, now, windowMs) {
  for (const [key, bucket] of buckets) {
    if (now >= bucket.startedAt + windowMs) buckets.delete(key);
  }
}
