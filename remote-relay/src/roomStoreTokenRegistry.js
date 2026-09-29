"use strict";
import { createHash } from "node:crypto";
import { hasTable, withTransaction, getRoomState } from "./roomStoreSchema.js";
import { isClaimRateLimited } from "./roomStoreClaimLimits.js";

const TOKEN_TTL_SKEW_MS = 120_000;
const TOKEN_TTL_CAPS_MS = Object.freeze({
  pairing: 330_000,
  access: 3_900_000,
  prev: 172_800_000,
  refresh_until: 2_592_000_000,
});
const TOKEN_HASH_RE = /^[0-9a-f]{64}$/;
const DEVICE_SUBJECT_RE = /^device:[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

// ---- Canonical token registry (§9.2) ----

export function clampTokenExpiry(cap, inputMs, relayNow = Date.now()) {
  const capMs = TOKEN_TTL_CAPS_MS[cap];
  if (capMs == null) throw new TypeError(`unknown token TTL cap: ${cap}`);
  if (Number.isInteger(inputMs) && inputMs > Number.MAX_SAFE_INTEGER) {
    throw new TypeError("token expiry exceeds JSON safe integer");
  }
  if (Number.isSafeInteger(inputMs) && inputMs <= 0) {
    throw new TypeError("token expiry must be positive");
  }
  if (!Number.isSafeInteger(inputMs)) throw new TypeError("invalid token expiry timestamp");
  if (inputMs < 1_000_000_000_000) {
    throw new TypeError("token expiry must use unix milliseconds");
  }
  if (!Number.isSafeInteger(relayNow) || relayNow <= 0) {
    throw new TypeError("relayNow must be a positive JSON-safe unix millisecond timestamp");
  }
  return Math.min(inputMs, relayNow + capMs + TOKEN_TTL_SKEW_MS);
}

function assertTokenSubject(subject, scope, state) {
  if (subject !== "pairing" && !DEVICE_SUBJECT_RE.test(subject || "")) {
    throw new TypeError("invalid token subject");
  }
  if (!["active", "revoked"].includes(state)) throw new TypeError("invalid token subject state");
  if (scope != null && !["remote", "pairing"].includes(scope)) throw new TypeError("invalid token scope");
  if (state === "active" && scope == null) throw new TypeError("active token subject requires scope");
  if (scope != null && subject === "pairing" && scope !== "pairing") {
    throw new TypeError("pairing subject requires pairing scope");
  }
  if (scope != null && subject !== "pairing" && scope === "pairing") {
    throw new TypeError("device subject cannot use pairing scope");
  }
}

function normalizeTokenAlias(alias, subject, subjectScope, relayNow) {
  if (!TOKEN_HASH_RE.test(alias?.token_hash || "")) throw new TypeError("invalid token hash");
  if (!["current", "prev"].includes(alias.kind)) throw new TypeError("invalid token alias kind");
  if (!Number.isSafeInteger(alias.generation) || alias.generation <= 0) {
    throw new TypeError("invalid token alias generation");
  }
  if (subjectScope === "pairing" && alias.kind !== "current") {
    throw new TypeError("pairing subject only supports a current alias");
  }

  if (alias.kind === "current") {
    const cap = subject === "pairing" ? "pairing" : "access";
    const accessExpires = clampTokenExpiry(cap, alias.access_expires, relayNow);
    const validUntil = subject === "pairing"
      ? accessExpires
      : clampTokenExpiry("refresh_until", alias.valid_until, relayNow);
    if (accessExpires > validUntil) throw new TypeError("access_expires must not exceed valid_until");
    return { ...alias, access_expires: accessExpires, valid_until: validUntil };
  }

  return {
    ...alias,
    access_expires: null,
    valid_until: clampTokenExpiry("prev", alias.valid_until, relayNow),
  };
}

/**
 * The sole write entry point shared by test seed rows and S1e token.put. It performs the §9.2 grammar and time
 * unit/cap/field-relation validation, and rebuilds that subject's alias set within a single transaction.
 */
export function putTokenRegistryEntry(sql, entry, relayNow = Date.now(), options = {}) {
  return withTransaction(sql, () =>
    putTokenRegistryEntryInTransaction(sql, entry, relayNow, options)
  );
}

/**
 * Identical CAS/fingerprint write semantics to putTokenRegistryEntry, but the caller must already
 * hold a transaction. sync/reset uses it to keep reconciliation of the entire batch within one SQL transaction; ordinary token.put
 * still relies on the compatibility entry point above to start the transaction.
 */
export function putTokenRegistryEntryInTransaction(sql, entry, relayNow = Date.now(), options = {}) {
  const normalized = normalizeTokenRegistryEntry(entry, relayNow);
  const putFingerprint = fingerprintNormalizedOriginalPut(entry, normalized);
  if (options.cas === true) {
    const currentRows = sql.exec(
      "SELECT subject, generation, state, scope FROM token_subjects WHERE subject = ? LIMIT 1",
      normalized.subject
    );
    const current = currentRows.length ? currentRows[0] : null;
    const floor = Number(getRoomState(sql).registry_floor);
    if (normalized.generation <= floor) {
      return { result: "rejected", reason: "generation_at_or_below_floor" };
    }
    if (current && normalized.generation < Number(current.generation)) {
      return { result: "rejected", reason: "generation_too_low" };
    }
    if (current && normalized.generation === Number(current.generation)) {
      const idempotent = putFingerprint == null
        ? tokenRegistryEntryEquals(sql, normalized, current)
        : tokenPutFingerprintEquals(sql, normalized.subject, normalized.generation, putFingerprint);
      return idempotent
        ? { result: "idempotent", entry: normalized }
        : { result: "rejected", reason: "generation_content_mismatch" };
    }
  }

  writeNormalizedTokenRegistryEntry(sql, normalized, putFingerprint);
  return options.cas === true ? { result: "ok", entry: normalized } : normalized;
}

export function reconcileTokenRegistry(sql, { revision, entries, reset = false }, relayNow = Date.now()) {
  return withTransaction(sql, () => {
    let floor = Number(getRoomState(sql).registry_floor);
    const revokedSubjects = [];
    if (reset) {
      revokedSubjects.push(...sql.exec("SELECT subject FROM token_subjects").map((row) => row.subject));
      const tableHighWater = tokenRegistryTableHighWater(sql);
      floor = Math.max(floor, tableHighWater);
      sql.exec("UPDATE room_state SET registry_floor = ?", floor);
      clearTokenRegistryInTransaction(sql);
    }

    const listedSubjects = new Set();
    const results = [];
    for (const entry of entries) {
      if (typeof entry?.subject === "string") listedSubjects.add(entry.subject);
      try {
        if (entry?.invalid === true) throw new TypeError("invalid sync entry");
        results.push(putTokenRegistryEntryInTransaction(sql, entry, relayNow, { cas: true }));
      } catch (error) {
        if (!(error instanceof TypeError) && !/token hash already belongs to another subject/.test(String(error?.message))) {
          throw error;
        }
        // Consistent with a single-frame put: malformed format/collision rejects that entry; the other reset/sync entries and
        // the final ack continue, rather than escalating one bad entry into a rollback of the entire frame.
        results.push({ result: "rejected", error });
      }
    }

    if (!reset) {
      const current = sql.exec("SELECT subject, generation, state FROM token_subjects");
      for (const row of current) {
        if (listedSubjects.has(row.subject) || row.state === "revoked" || Number(row.generation) >= revision) {
          continue;
        }
        sql.exec(
          "UPDATE token_subjects SET generation = ?, state = 'revoked', scope = NULL WHERE subject = ?",
          revision,
          row.subject
        );
        sql.exec("DELETE FROM token_aliases WHERE subject = ?", row.subject);
        sql.exec("DELETE FROM token_put_fingerprints WHERE subject = ?", row.subject);
        revokedSubjects.push(row.subject);
      }
    }

    const relayHighWater = Math.max(tokenRegistryTableHighWater(sql), floor, revision);
    return { results, revokedSubjects, relayHighWater };
  });
}

function tokenRegistryTableHighWater(sql) {
  const rows = sql.exec("SELECT COALESCE(MAX(generation), 0) AS high_water FROM token_subjects");
  return rows.length ? Number(rows[0].high_water) : 0;
}

function normalizeTokenRegistryEntry(entry, relayNow = Date.now()) {
  const state = entry?.state ?? "active";
  const scope = entry?.scope ?? null;
  assertTokenSubject(entry?.subject, scope, state);
  if (Number.isSafeInteger(entry?.generation) && entry.generation <= 0) {
    throw new TypeError("token subject generation must be positive");
  }
  if (!Number.isSafeInteger(entry?.generation)) {
    throw new TypeError("invalid token subject generation");
  }
  const aliases = (entry.aliases ?? []).map((alias) =>
    normalizeTokenAlias(alias, entry.subject, scope, relayNow)
  );
  if (aliases.length > 2 || new Set(aliases.map((alias) => alias.kind)).size !== aliases.length) {
    throw new TypeError("token subject aliases must have unique current/prev kinds");
  }
  if (state === "revoked" && aliases.length > 0) {
    throw new TypeError("revoked token subject cannot retain aliases");
  }
  const current = aliases.find((alias) => alias.kind === "current");
  const prev = aliases.find((alias) => alias.kind === "prev");
  if (current && prev) {
    // §9.2: prev=min(prev_expires, refresh_until). First cap both values individually, then use
    // current.valid_until (namely refresh_until) to bound the old token's maximum catch-up window.
    prev.valid_until = Math.min(prev.valid_until, current.valid_until);
  }
  return { subject: entry.subject, generation: entry.generation, state, scope, aliases };
}

function writeNormalizedTokenRegistryEntry(sql, entry, putFingerprint) {
  for (const alias of entry.aliases) {
    const collision = sql.exec(
      "SELECT subject FROM token_aliases WHERE token_hash = ? AND subject <> ? LIMIT 1",
      alias.token_hash,
      entry.subject
    );
    if (collision.length > 0) throw new Error("token hash already belongs to another subject");
  }
  sql.exec(
    "INSERT INTO token_subjects (subject, generation, state, scope) VALUES (?, ?, ?, ?) " +
      "ON CONFLICT(subject) DO UPDATE SET generation=excluded.generation, state=excluded.state, scope=excluded.scope",
    entry.subject,
    entry.generation,
    entry.state,
    entry.scope
  );
  sql.exec("DELETE FROM token_aliases WHERE subject = ?", entry.subject);
  for (const alias of entry.aliases) {
    sql.exec(
      "INSERT INTO token_aliases " +
        "(token_hash, subject, kind, generation, access_expires, valid_until) VALUES (?, ?, ?, ?, ?, ?)",
      alias.token_hash,
      entry.subject,
      alias.kind,
      alias.generation,
      alias.access_expires,
      alias.valid_until
    );
  }
  if (putFingerprint == null) {
    sql.exec("DELETE FROM token_put_fingerprints WHERE subject = ?", entry.subject);
  } else {
    sql.exec(
      "INSERT INTO token_put_fingerprints (subject, generation, fingerprint) VALUES (?, ?, ?) " +
        "ON CONFLICT(subject) DO UPDATE SET generation=excluded.generation, fingerprint=excluded.fingerprint",
      entry.subject,
      entry.generation,
      putFingerprint
    );
  }
}

function fingerprintNormalizedOriginalPut(rawEntry, normalized) {
  if (normalized.state !== "active") return null;
  const current = (rawEntry.aliases ?? []).find((alias) => alias.kind === "current");
  if (!current) return null;
  const prev = (rawEntry.aliases ?? []).find((alias) => alias.kind === "prev");
  const canonical = {
    subject: normalized.subject,
    generation: normalized.generation,
    scope: normalized.scope,
    current: normalized.scope === "pairing"
      ? {
          token_hash: current.token_hash,
          access_expires: current.access_expires,
        }
      : {
          token_hash: current.token_hash,
          access_expires: current.access_expires,
          refresh_until: current.valid_until,
        },
  };
  if (prev) {
    canonical.prev = {
      token_hash: prev.token_hash,
      generation: prev.generation,
      prev_expires: prev.valid_until,
    };
  }
  return createHash("sha256").update(JSON.stringify(canonical), "utf8").digest("hex");
}

function tokenPutFingerprintEquals(sql, subject, generation, fingerprint) {
  const rows = sql.exec(
    "SELECT generation, fingerprint FROM token_put_fingerprints WHERE subject = ? LIMIT 1",
    subject
  );
  return rows.length === 1 &&
    Number(rows[0].generation) === generation &&
    rows[0].fingerprint === fingerprint;
}

function tokenRegistryEntryEquals(sql, expected, current) {
  if (current.subject !== expected.subject ||
      Number(current.generation) !== expected.generation ||
      current.state !== expected.state ||
      current.scope !== expected.scope) {
    return false;
  }
  const aliases = sql.exec(
    "SELECT token_hash, kind, generation, access_expires, valid_until " +
      "FROM token_aliases WHERE subject = ? ORDER BY kind",
    expected.subject
  );
  const wanted = [...expected.aliases].sort((left, right) => left.kind.localeCompare(right.kind));
  if (aliases.length !== wanted.length) return false;
  return aliases.every((alias, index) => {
    const other = wanted[index];
    return alias.token_hash === other.token_hash &&
      alias.kind === other.kind &&
      Number(alias.generation) === other.generation &&
      (alias.access_expires == null ? null : Number(alias.access_expires)) === other.access_expires &&
      Number(alias.valid_until) === other.valid_until;
  });
}

export function clearTokenRegistry(sql) {
  return withTransaction(sql, () => clearTokenRegistryInTransaction(sql));
}

function clearTokenRegistryInTransaction(sql) {
  sql.exec("DELETE FROM token_put_fingerprints");
  sql.exec("DELETE FROM token_aliases");
  sql.exec("DELETE FROM token_subjects");
}

export function getTokenSubject(sql, subject) {
  const rows = sql.exec(
    "SELECT subject, generation, state, scope FROM token_subjects WHERE subject = ? LIMIT 1",
    subject
  );
  return rows.length ? rows[0] : null;
}

export function getTokenAlias(sql, subject, kind) {
  const rows = sql.exec(
    "SELECT token_hash, subject, kind, generation, access_expires, valid_until " +
      "FROM token_aliases WHERE subject = ? AND kind = ? LIMIT 1",
    subject,
    kind
  );
  return rows.length ? rows[0] : null;
}

export function resolveTokenAdmission(sql, tokenHash, now = Date.now()) {
  // SEC-1: When a remote hits a room with an empty schema (the business schema has not yet been created), token_aliases/
  // token_subjects do not exist — following the hasTable guard precedent of isClaimRateLimited(:216),
  // a miss is simply a clean 401: do not create tables or throw a SQL exception.
  if (!hasTable(sql, "token_aliases") || !hasTable(sql, "token_subjects")) return null;
  if (!TOKEN_HASH_RE.test(tokenHash || "")) return null;
  const rows = sql.exec(
    "SELECT a.token_hash, a.subject, a.kind, a.generation, a.access_expires, a.valid_until, " +
      "s.generation AS subject_generation, s.state AS subject_state, s.scope " +
      "FROM token_aliases a JOIN token_subjects s ON s.subject = a.subject " +
      "WHERE a.token_hash = ? LIMIT 1",
    tokenHash
  );
  if (rows.length === 0) return null;
  const row = rows[0];
  if (row.subject_state !== "active") return null;
  if (row.kind === "current" && Number(row.generation) !== Number(row.subject_generation)) return null;

  let admittedScope = null;
  if (row.kind === "current" && now < Number(row.access_expires)) admittedScope = row.scope;
  else if (row.kind === "current" && now < Number(row.valid_until) && row.scope !== "pairing") admittedScope = "refresh";
  else if (row.kind === "prev" && now < Number(row.valid_until)) admittedScope = "refresh";
  if (!admittedScope) return null;
  return {
    scope: admittedScope,
    subject: row.subject,
    kind: row.kind,
    generation: Number(row.subject_generation),
    alias_generation: Number(row.generation),
    access_expires: row.access_expires == null ? null : Number(row.access_expires),
    valid_until: Number(row.valid_until),
  };
}
