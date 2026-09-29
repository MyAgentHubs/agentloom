// bytes.ts — dependency-free hex/base64 codecs shared by the crypto kernel.
//
// Deliberately hand-rolled on top of `atob`/`btoa` (both Web-standard globals, present in every
// browser and in Node 16+) instead of `Buffer` (Node-only, would break the browser build) or the
// newer `Uint8Array.prototype.toBase64()`/`fromBase64()` (too recent to assume on the Safari
// 18.4 baseline this package targets — see M2 C1 spec §2). Conversion is chunked to avoid
// `String.fromCharCode(...bytes)` blowing the call stack on large inputs.

const HEX_CHARS = "0123456789abcdef";

export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) {
    out += HEX_CHARS[(byte >> 4) & 0xf];
    out += HEX_CHARS[byte & 0xf];
  }
  return out;
}

export function hexToBytes(hex: string): Uint8Array {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) {
    throw new Error(`invalid hex string: ${hex}`);
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

const BASE64_CHUNK = 0x8000;

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += BASE64_CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + BASE64_CHUNK));
  }
  return btoa(binary);
}

export function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}

const BASE64_SHAPE_RE = /^[A-Za-z0-9+/]*={0,2}$/;

/**
 * Strict **canonical** base64 decode — rejects anything `atob()` would silently accept beyond
 * the single canonical encoding of its own decoded bytes: missing/extra `=` padding, non-zero
 * padding bits in the final group, or plain garbage characters. `atob()` on its own is lenient
 * (e.g. Node/browsers both happily decode `"QQ"` — no padding — the same as canonical `"QQ=="`),
 * which lets a malformed wire envelope smuggle a byte length atob() "means" but the sender never
 * canonically encoded.
 *
 * Technique: decode, then re-encode with `bytesToBase64()` (which always emits the one canonical
 * form for a given byte sequence) and require the two strings to match exactly. This is complete,
 * not just an approximation — any non-canonical input decodes to *some* fixed byte sequence, and
 * that sequence's canonical re-encoding can only equal the original string if the input already
 * was canonical.
 *
 * Returns `null` (never throws) so callers — see `envelope.ts::open()` — can fold "bad base64"
 * into the same error shape as "bad ciphertext"/"bad AAD" without a branch that could leak which
 * check failed to a network attacker.
 */
export function decodeCanonicalBase64(value: string): Uint8Array | null {
  if (!BASE64_SHAPE_RE.test(value) || value.length % 4 !== 0) {
    return null;
  }
  let bytes: Uint8Array;
  try {
    bytes = base64ToBytes(value);
  } catch {
    return null;
  }
  return bytesToBase64(bytes) === value ? bytes : null;
}

/** URL-safe, unpadded base64 — only used to build the JWK `d`/`x` fields in x25519.ts. */
export function bytesToBase64Url(bytes: Uint8Array): string {
  return bytesToBase64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function utf8Bytes(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}
