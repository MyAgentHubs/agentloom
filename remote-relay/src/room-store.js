"use strict";

// room-store.js — SQL persistence logic for the S2 room DO, fully decoupled
// from whether it runs with Cloudflare DO's ctx.storage.sql or node:sqlite.
//
// This module depends only on a minimal sql adapter interface:
//   sql.exec(query, ...params) -> Array<object rows>
// - room-do.js uses a thin adapter around Cloudflare's ctx.storage.sql.exec(...).toArray();
// - test/room-store.test.js uses a thin adapter around Node's built-in node:sqlite.
// Both sides run the same logic code, not “rewrite it once for tests and again for production” — there is no risk
// of the two implementations drifting. This is also why this scaffold can say that the seq/epoch/replay/quota logic
// has been “actually tested” rather than merely “copied as mock logic that looks plausible.”
//
// The DO wiring (the Hibernation API / WebSocketPair / serializeAttachment part)
// has no equivalent substitute that can be unit-tested; it can only be verified on a real device with `wrangler dev` — as also noted
// in the header comment of room-do.js.

export * from "./roomStoreSchema.js";
export * from "./roomStoreClaimLimits.js";
export * from "./roomStoreTokenRegistry.js";
export * from "./roomStoreRefreshRequests.js";
export * from "./roomStoreReplyRoutes.js";
export * from "./roomStoreMeta.js";
export * from "./roomStorePendingInput.js";
