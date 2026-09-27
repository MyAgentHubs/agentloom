import type { RemotePairingPayload } from "./remoteControlTypes";

// These values are pinned deliberately; do not re-derive them locally.
export const QR_MAX_URL_BYTES = 1024;
export const QR_ERROR_CORRECTION_LEVEL = "M";
export const QR_MARGIN = 4;
// Without width, qrcode emits an SVG with only a viewBox and no explicit width/height.
// In the Tauri WKWebView flex container (qrWrap), it collapses to 0×0; jsdom cannot
// catch this because it has no real layout. Pin the render size so the SVG gets
// width/height attributes. The pinned 240px container minus 8px padding per side is 224px.
export const QR_RENDER_SIZE_PX = 224;

/** Validate relay_url beyond its `wss://` prefix: it must parse, have no userinfo,
 *  have a nonempty host, use only the root or empty path, and have no query or fragment.
 *  Return the parsed `URL` for deriving the origin, or `null` on failure. */
export function parseValidRelayUrl(value: string): URL | null {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.protocol !== "wss:") return null;
  if (url.username !== "" || url.password !== "") return null;
  if (!url.host) return null;
  if (url.pathname !== "/" && url.pathname !== "") return null;
  if (url.search !== "") return null;
  if (url.hash !== "") return null;
  // Property checks alone miss input that WHATWG normalization silently removes, such as
  // empty userinfo, query, or fragment, and forgiving extra or backslashes. Require the
  // canonical href to match the input, allowing only an added trailing root slash. This
  // deliberately rejects uppercase schemes or hosts because normalization lowercases them.
  if (url.href !== value && url.href !== `${value}/`) return null;
  // `wss://host/?` and `wss://host/#` retain their empty delimiters in href, while
  // search and hash getters return empty strings. Check the original input directly:
  // a valid root-only relay URL cannot contain `?` or `#`.
  if (value.includes("?") || value.includes("#")) return null;
  return url;
}

export function isValidRelayUrl(value: string): boolean {
  return parseValidRelayUrl(value) !== null;
}

/** Allow an empty relay address to use the official public relay through the backend
 *  `effective_relay_url` fallback; nonempty values still require strict `wss://` validation. */
export function isValidOrEmptyRelayUrl(value: string): boolean {
  return value === "" || isValidRelayUrl(value);
}

/** Derive HTTPS from WSS by appending the host (including port) to `https://` exactly,
 *  without forgiving mappings such as special handling for default ports. */
export function relayHttpsOrigin(url: URL): string {
  return `https://${url.host}`;
}

/** base64url (RFC 4648 §5, without padding). Encode UTF-8 bytes first; the payload may contain non-ASCII text. */
export function base64UrlEncode(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

/** QR content is `https://<relay host>/#p=<base64url(payload)>`. Return `null`
 *  for an invalid relay_url or a URL over 1024 bytes, so the caller shows a pairing
 *  error instead of falling back to raw JSON encoding.
 *
 *  Derive the origin solely from `payload.relay_url`, which is encoded in the QR code.
 *  The locally stored relay URL and the backend pairing payload currently share a source,
 *  but that coincidence is not guaranteed; the phone connects using the encoded value. */
export function buildPairingQrUrl(
  payload: RemotePairingPayload,
): string | null {
  const parsedRelay = parseValidRelayUrl(payload.relay_url);
  if (!parsedRelay) return null;
  const origin = relayHttpsOrigin(parsedRelay);
  const encoded = base64UrlEncode(JSON.stringify(payload));
  const url = `${origin}/#p=${encoded}`;
  const byteLength = new TextEncoder().encode(url).length;
  if (byteLength > QR_MAX_URL_BYTES) return null;
  return url;
}
