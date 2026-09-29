"use strict";
import { createHash } from "node:crypto";
import { getMeta } from "./roomStoreMeta.js";

const CLIENT_MSG_ID_NAMESPACE = "fe4e51ad-468c-4c11-85c2-f15f0c22f030";
// SEC-1: Move ip_bucket_salt from room_meta (the business schema, created only after authentication succeeds) to
// room_state (the sentinel table, which always exists) — deriveIpBucketKey needs to
// derive a per-IP bucket key using the salt before authentication (S1i2 §9.8); if the salt remained in the business schema, creating the salt before authentication would pull business tables
// back into the old issue of unconditional creation before authentication (see this item's background).
// SEC-3: created_at provides the fixed reclaim deadline for unclaimed empty rooms (created_at +
// UNCLAIMED_RECLAIM_MS; see room-do.js scheduleNextTokenAlarm/alarm) — it is fixed
// rather than sliding, following the same “sentinel table always exists” discipline, for the same reason as the ip_bucket_salt comment above.
const ROOM_STATE_SCHEMA = `CREATE TABLE IF NOT EXISTS room_state (
  owner_credential_hash TEXT,
  tombstoned_at INTEGER,
  registry_floor INTEGER NOT NULL DEFAULT 0,
  ip_bucket_salt TEXT,
  created_at INTEGER
)`;

const SCHEMA_STATEMENTS = [
  // Milestone event log (parent document §2): seq is allocated by the DO and monotonically increases within the room;
  // do not use AUTOINCREMENT — seq allocation must first pass the epoch gate, so insertMilestone
  // allocates it explicitly. The source of seq is the independent counter in room_meta (see allocateSeq below),
  // A persistent counter avoids deriving the next sequence from MAX(seq),
  // which can decrease when retained event rows are pruned or deleted.
  // Keeping the counter independent of event retention prevents new sequences
  // from reusing or falling below values already delivered to remote peers.
  `CREATE TABLE IF NOT EXISTS events (
    seq     INTEGER PRIMARY KEY,
    epoch   INTEGER NOT NULL,
    session TEXT,
    kind    TEXT NOT NULL,
    ct      TEXT NOT NULL,
    n       TEXT NOT NULL,
    ts      INTEGER NOT NULL,
    client_msg_id TEXT
  )`,
  // Miscellaneous room-level state (current epoch, room ID, valid token set, …) — KV form, sufficient without overdesign.
  `CREATE TABLE IF NOT EXISTS room_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
  )`,
  // input channel staging (parent document §3): rows exist only while the desktop is offline; TTL is 30 minutes;
  // delete after the desktop replies input.ack; if it expires without an ack, passively remove it and reply input.expired.
  `CREATE TABLE IF NOT EXISTS pending_input (
    command_id TEXT PRIMARY KEY,
    session    TEXT,
    envelope   TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    subject    TEXT,
    generation INTEGER
  )`,
  // G8 third-review fix_required (codex xhigh delta review) R3: deleteExpiredPendingInput
  // now queries by expires_at before every handleInput enqueue — without this index, it would be
  // a full-table scan; this is a guarded migration (CREATE INDEX IF NOT EXISTS is safe for both new and old databases).
  `CREATE INDEX IF NOT EXISTS idx_pending_input_expires ON pending_input(expires_at)`,
  // Quota counter (parent document §3.5 S3): bucketed by UTC year-month.
  `CREATE TABLE IF NOT EXISTS quota_counters (
    period          TEXT PRIMARY KEY,
    milestone_count INTEGER NOT NULL DEFAULT 0
  )`,
  // In-room claim-attempt bucket: fixed one-hour window, persisted across DO hibernation.
  `CREATE TABLE IF NOT EXISTS claim_rate_limits (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    window_started_at INTEGER NOT NULL,
    attempts          INTEGER NOT NULL DEFAULT 0
  )`,
  // G8 R1 (dual-review fix_required① · message rate limiting): per-subject fixed windows for input/control,
  // with the same structure as claim_rate_limits above (persisted across DO hibernation),
  // except the key becomes the composite primary key (subject, channel) — regardless of how many concurrent
  // sockets a subject opens, input/control each has only one row and shares one quota, rather than
  // resetting on disconnect/reconnect as in the previously mistaken attachment version. CREATE TABLE IF NOT EXISTS is safe for DOs of both new and old
  // rooms (a guarded migration, following the same discipline as the initSchema header comment).
  `CREATE TABLE IF NOT EXISTS message_rate_limits (
    subject           TEXT NOT NULL,
    channel           TEXT NOT NULL CHECK (channel IN ('input','control')),
    window_started_at INTEGER NOT NULL,
    attempts          INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (subject, channel)
  )`,
  `CREATE TABLE IF NOT EXISTS token_subjects (
    subject TEXT PRIMARY KEY,
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL CHECK (state IN ('active','revoked')),
    scope TEXT CHECK (scope IN ('remote','pairing')),
    CHECK (state = 'revoked' OR scope IS NOT NULL)
  )`,
  `CREATE TABLE IF NOT EXISTS token_aliases (
    token_hash TEXT PRIMARY KEY,
    subject TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('current','prev')),
    generation INTEGER NOT NULL,
    access_expires INTEGER,
    valid_until INTEGER NOT NULL,
    UNIQUE (subject, kind)
  )`,
  `CREATE TABLE IF NOT EXISTS token_put_fingerprints (
    subject TEXT PRIMARY KEY,
    generation INTEGER NOT NULL CHECK (generation > 0),
    fingerprint TEXT NOT NULL CHECK (length(fingerprint) = 64)
  )`,
  // S1i2 §9.6 line 246: relay-side delivery accounting for the refresh three-frame set. One row = one
  // in-flight request whose receipt has not yet been delivered; a matched delivery predicate or expired deadline both delete the row
  // (see room-do.js deliverRefreshReceipt). connection_id records “the connection that most recently
  // sent this request_id” — safely rebind when the same request_id is resent from a new connection
  // (see upsertRefreshRequest); always reject a different subject to prevent a request_id collision
  // from being used to hijack another subject's receipt pending delivery.
  // ip_bucket_key (S1i2 rework R3 · §9.8 line 263): webSocketMessage after hibernation wakeup
  // cannot obtain the original Request, so all per-IP accounting for message periods uses this key copied from attachment when the row is created,
  // rather than retrieving Request on demand. The column permits NULL (legacy/direct-row test paths are not required to
  // carry it), but once present it must be deriveIpBucketKey's fixed-length output (the hex of a 16-byte truncated hash
  // = 32 characters).
  `CREATE TABLE IF NOT EXISTS refresh_requests (
    request_id TEXT PRIMARY KEY,
    subject TEXT NOT NULL,
    request_generation INTEGER NOT NULL CHECK (request_generation > 0),
    connection_id TEXT NOT NULL,
    deadline INTEGER NOT NULL,
    ip_bucket_key TEXT CHECK (ip_bucket_key IS NULL OR length(ip_bucket_key) = 32)
  )`,
  // R5.3: deadline is the sole query key for alarm cleanup and nextRefreshRequestDeadline scheduling;
  // add an index.
  `CREATE INDEX IF NOT EXISTS idx_refresh_requests_deadline ON refresh_requests(deadline)`,
  // S1i3 F1 (§9.5 line 235 · pairing-route persistence): a one-row table — subject is always "pairing", recording the connection_id of “the remote connection that most
  // recently sent pair.hello.” relay stamps this value onto the frame when forwarding pair.hello /
  // pair.done to the desktop (never trust the phone's self-report); when the desktop replies pair.accept / pair.ready,
  // it delivers directionally based on this row instead of broadcasting with broadcastToRemotes (multiple phones in the same window do not receive each other's accept).
  // It must be persisted (not retained only in an in-memory Map): memory state is lost after DO hibernation wakeup, but the route row must remain.
  `CREATE TABLE IF NOT EXISTS pairing_routes (
    subject TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL
  )`,
  // Persist command_id routes so desktop replies reach the requesting connection
  // even after hibernation. The top-level identifier is visible to the relay,
  // while the encrypted control payload remains opaque.
  // Track bytes_sent using full forwarded frame sizes because the relay cannot
  // inspect ciphertext to measure payload content.
  // Keep each route across multiple reply chunks rather than deleting it
  // after the first delivery: the relay cannot identify the terminal frame.
  // Each successful delivery increments bytes_sent and extends the deadline
  // so active transfers remain routable; expiration and alarm cleanup
  // eventually remove routes once forwarding activity stops.
  `CREATE TABLE IF NOT EXISTS reply_routes (
    command_id TEXT PRIMARY KEY,
    subject TEXT NOT NULL,
    generation INTEGER,
    connection_id TEXT NOT NULL,
    deadline INTEGER NOT NULL,
    bytes_sent INTEGER NOT NULL DEFAULT 0
  )`,
  // deadline is the sole query key for alarm cleanup and nextReplyRouteDeadline scheduling; add an index
  // (following the idx_refresh_requests_deadline precedent).
  `CREATE INDEX IF NOT EXISTS idx_reply_routes_deadline ON reply_routes(deadline)`,
  // Persist a fixed-window byte budget per subject so all sockets and
  // reconnections share the same allowance, including across hibernation.
  // Resetting this state on reconnect would allow the budget to be bypassed.
  // Count full forwarded frame sizes because encrypted payloads are opaque.
  // Store the window start and cumulative bytes together so each subject
  // has one consistent allowance for the current window.
  `CREATE TABLE IF NOT EXISTS reply_byte_limits (
    subject           TEXT PRIMARY KEY,
    window_started_at INTEGER NOT NULL,
    bytes_sent        INTEGER NOT NULL DEFAULT 0
  )`,
];

export function hasTable(sql, tableName) {
  const rows = sql.exec("SELECT 1 AS present FROM sqlite_master WHERE type = 'table' AND name = ?", tableName);
  return rows.length > 0;
}

export function initRoomStateSentinel(sql, now = Date.now()) {
  sql.exec(ROOM_STATE_SCHEMA);
  ensureRoomStateIpBucketSaltColumn(sql);
  ensureRoomStateCreatedAtColumn(sql);
  sql.exec(
    "INSERT INTO room_state (owner_credential_hash, tombstoned_at, registry_floor, created_at) " +
      "SELECT NULL, NULL, 0, ? WHERE NOT EXISTS (SELECT 1 FROM room_state)",
    now
  );
  // SEC-3: created_at added by the guarded ALTER above for existing rooms (deployed before SEC-3) is
  // NULL — backfill it with the time of “first touch” (this construction) as the starting point, without guessing the real room-creation time
  // (rotating the starting point does not alter the reclaim-deadline semantics: it merely lets existing unclaimed rooms start counting again from this deployment
  // for a full UNCLAIMED_RECLAIM_MS grace window, without retrospectively deciding that they “had already expired”).
  // It affects only the row where created_at is still NULL; subsequent calls are no-ops (idempotent, following the same
  // migration-family discipline as ip_bucket_salt).
  sql.exec("UPDATE room_state SET created_at = ? WHERE created_at IS NULL", now);
}

// Guarded migration (same approach as ensureClientMsgIdColumn :783): CREATE TABLE IF NOT
// EXISTS is a no-op for an existing old room_state and cannot add this column, so run a separate ALTER once;
// newly created room_state already has this column, so this encounters “column already exists” and suppresses it with try/catch (other
// errors are still thrown normally).
function ensureRoomStateIpBucketSaltColumn(sql) {
  try {
    sql.exec("ALTER TABLE room_state ADD COLUMN ip_bucket_salt TEXT");
  } catch (err) {
    const message = String((err && err.message) || err);
    if (!/duplicate column/i.test(message)) {
      throw err;
    }
  }
}

// SEC-3: As above, guarded ALTER for the created_at column — this must be placed in the unconditional
// initRoomStateSentinel (called unconditionally by the constructor; see the room-do.js constructor comment), rather than
// run only when “room_state is found not to exist”; otherwise it reproduces SEC-1's pitfall where an existing room is blocked by
// `!hasTable("room_state")` and its column can never be added (see this item's brief, “the pitfall SEC-1 just
// encountered”).
function ensureRoomStateCreatedAtColumn(sql) {
  try {
    sql.exec("ALTER TABLE room_state ADD COLUMN created_at INTEGER");
  } catch (err) {
    const message = String((err && err.message) || err);
    if (!/duplicate column/i.test(message)) {
      throw err;
    }
  }
}

export function getRoomState(sql) {
  const rows = sql.exec(
    "SELECT owner_credential_hash, tombstoned_at, registry_floor, created_at FROM room_state LIMIT 1"
  );
  return rows.length
    ? rows[0]
    : { owner_credential_hash: null, tombstoned_at: null, registry_floor: 0, created_at: null };
}

export function isRoomLive(sql) {
  return getRoomState(sql).tombstoned_at == null;
}

// ---- room_state.ip_bucket_salt (SEC-1: migrated from room_meta and held by the always-existing sentinel table,
// so deriveIpBucketKey can safely read/write it before authentication) ----

export function getRoomIpBucketSalt(sql) {
  const rows = sql.exec("SELECT ip_bucket_salt FROM room_state LIMIT 1");
  return rows.length ? rows[0].ip_bucket_salt : null;
}

export function setRoomIpBucketSalt(sql, salt) {
  sql.exec("UPDATE room_state SET ip_bucket_salt = ?", salt);
}

export function withTransaction(sql, callback) {
  if (typeof sql.transactionSync !== "function") {
    throw new Error("SQL adapter does not provide transactionSync");
  }
  return sql.transactionSync(callback);
}

export function initSchema(sql) {
  for (const stmt of SCHEMA_STATEMENTS) {
    sql.exec(stmt);
  }
  // Guarded migration (v1.7.4): the old events table in deployed DOs lacks the client_msg_id column —
  // CREATE TABLE IF NOT EXISTS is a no-op for an existing old table and cannot add this column, so separately
  // run ALTER TABLE once; the new table already includes the column in the CREATE TABLE above, so this encounters
  // "column already exists" and suppresses only that error with try/catch (other errors are thrown normally and real faults are not silently
  // swallowed). initSchema itself can be called repeatedly (idempotently) — this migration step is likewise reentrant.
  ensureClientMsgIdColumn(sql);
  ensurePendingInputAuthorizationColumns(sql);
  // First backfill old rows with complete data, then create the unique index: this makes index creation during the first migration perform a final integrity check on all
  // client_msg_id values, rather than temporarily letting historical NULL rows bypass the index.
  backfillLegacyClientMsgIds(sql);
  sql.exec(
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_events_client_msg_id ON events(client_msg_id) WHERE client_msg_id IS NOT NULL"
  );
  // S1ja F2 (guarded migration · reentrant): retire the __admin/register-token backdoor together with the
  // room_meta['valid_tokens'] row it wrote — if only the route/ADMIN_TOKEN is removed without clearing this row,
  // existing dev tokens can still use the old authorizeInboundSocket/canDeliverOutbound legacy
  // exemption (both locations below have been changed to fail closed) to bypass the entire registry gate, never expire, and cannot be revoked,
  // making a permanent backdoor. DELETE on a nonexistent row is inherently a no-op and idempotent, requiring no
  // try/catch guard.
  purgeLegacyValidTokensMeta(sql);
}

// SEC-1: Delay business-schema creation until after authentication succeeds (at the convergence point of room-do.js fetch() authentication branches +
// a defensive fallback call from alarm()). initSchema consists entirely of CREATE TABLE IF NOT EXISTS +
// idempotent ALTER/backfill and is already reentrant; this merely gives that use a more appropriate name,
// not a separate implementation.
export function ensureBusinessSchema(sql) {
  initSchema(sql);
}

function ensureClientMsgIdColumn(sql) {
  try {
    sql.exec("ALTER TABLE events ADD COLUMN client_msg_id TEXT");
  } catch (err) {
    const message = String((err && err.message) || err);
    if (!/duplicate column/i.test(message)) {
      throw err;
    }
  }
}

function ensurePendingInputAuthorizationColumns(sql) {
  const columns = new Set(sql.exec("PRAGMA table_info(pending_input)").map((row) => row.name));
  if (!columns.has("subject")) sql.exec("ALTER TABLE pending_input ADD COLUMN subject TEXT");
  if (!columns.has("generation")) sql.exec("ALTER TABLE pending_input ADD COLUMN generation INTEGER");
}

// S1ja F2: Clear the room_meta['valid_tokens'] row written by the backdoor — DELETE on a nonexistent key
// is inherently a no-op and idempotent, requiring no try/catch guard like ensureClientMsgIdColumn's.
function purgeLegacyValidTokensMeta(sql) {
  sql.exec("DELETE FROM room_meta WHERE key = 'valid_tokens'");
}

// Standard UUIDv5 (RFC 4122, namespace + SHA-1). The algorithm matches the independent reference implementation in
// test/client-msg-id-derivation.test.js, and the namespace also reuses the
// repository-wide constant; this is used only for relay-private legacy backfill and does not participate in cross-endpoint derivation KATs.
function uuidv5(name, namespaceUuid) {
  const nsBytes = Buffer.from(namespaceUuid.replace(/-/g, ""), "hex");
  const nameBytes = Buffer.from(name, "utf8");
  const hash = createHash("sha1").update(Buffer.concat([nsBytes, nameBytes])).digest();
  const bytes = Buffer.from(hash.subarray(0, 16));
  bytes[6] = (bytes[6] & 0x0f) | 0x50; // version 5
  bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC4122 variant
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function backfillLegacyClientMsgIds(sql) {
  const rows = sql.exec("SELECT seq FROM events WHERE client_msg_id IS NULL ORDER BY seq ASC");
  if (rows.length === 0) return;

  // On the production path, a room that can retain old events must already have called
  // ensureRoomId in an earlier authenticated fetch; room_meta and events belong to the same DO's persistent SQLite and remain after restart/deployment.
  // The empty string is only a fallback for manually corrupted/non-production old databases; seq still ensures values generated within one DO are distinct.
  const roomId = getMeta(sql, "room_id", "");
  for (const row of rows) {
    const clientMsgId = uuidv5(`legacy|${roomId}|${String(row.seq)}`, CLIENT_MSG_ID_NAMESPACE);
    sql.exec("UPDATE events SET client_msg_id = ? WHERE seq = ?", clientMsgId, row.seq);
  }
}
