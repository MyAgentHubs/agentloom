import type { ContentRef, FrameRejectReason, ParseResult } from "./frameTypes";

function reject(reason: FrameRejectReason, t: string | null): ParseResult {
  return { ok: false, reason, t };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isStringOrNull(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every(isString);
}

/** `content_sha256` is production lowercase hex (`sha256_hex_lower`)—64 `[0-9a-f]` characters. */
const HEX64_RE = /^[0-9a-f]{64}$/;

function isSha256Hex(value: unknown): value is string {
  return isString(value) && HEX64_RE.test(value);
}

/**
 * Shared validation for content_ref and current_ref rejects non-record values and incomplete four-field pointers.
 * Non-record values are directly invalid; if the shape of any of the four fields is incorrect, reject the whole thing (do not "partially pass through"—this is a complete opaque pointer and may not be incomplete).
 */
function parseContentRef(value: unknown): ContentRef | null {
  if (!isRecord(value)) return null;
  if (
    !isFiniteNumber(value.message_id) ||
    !isFiniteNumber(value.revision) ||
    !isSha256Hex(value.content_sha256) ||
    !isFiniteNumber(value.total_bytes)
  ) {
    return null;
  }
  return {
    message_id: value.message_id,
    revision: value.revision,
    content_sha256: value.content_sha256,
    total_bytes: value.total_bytes,
  };
}

export { isFiniteNumber, isRecord, isSha256Hex, isString, isStringArray, isStringOrNull, parseContentRef, reject };
