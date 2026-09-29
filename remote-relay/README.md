**English** · [简体中文](README.zh-CN.md)

# AgentLoom Remote Control relay skeleton

A Cloudflare Workers + Durable Objects relay skeleton covering three areas:
**S1 access and authentication / S2 room DO / S3 quota and abuse protection**
(single-layer envelope, `kind` includes `live`, and `command_id` is a top-level
envelope field — an earlier skeleton guessed at an outer
`{envelope, milestone, command_id}` wrapper, which has been removed; see the
"Protocol shape" section below).

## What this is and is not

The relay is a "message forwarding server": the desktop AgentLoom and the remote
(phone/browser) both connect to it outbound, and it carries **ciphertext
envelopes** plus a little routing metadata between the two sides.

**The relay only sees ciphertext**: `src/envelope.js` only parses/validates the
outer envelope fields (v / room / epoch / seq / kind / session / ct / n / ts) and
never tries to decrypt `ct` — the relay holds no content key (`K_room` lives only
on the desktop and on paired remote devices). This is the E2EE boundary, and the
place in code that backs the promise that "even if the relay is down or
compromised, session content cannot be read".

**Same-origin hosting — an honest statement of this boundary**: if the Web remote
(a separate component, C1/S4, not included in this skeleton) is hosted on the same
origin as the relay, the JS that does the decryption is shipped by the relay
operator — in theory the operator could push a key-stealing version at any time.
In other words, **E2EE for a same-origin-hosted Web client protects against
outsiders and intruders, not against the operator itself**. This is not an
implementation defect of this skeleton; it is a trade-off inherent to this
topology, and a more complete guarantee has to wait for the native iOS client (M5).

## Protocol shape (single-layer envelope, no outer wrapper)

A WS message is either a **plaintext control frame** (top-level `t` field; three
kinds: presence / `control.notify_hint` / `input.ack`; no `ct`/`n`), or is itself
an **envelope** — there is no `{ envelope: ... }` outer wrapper any more:

```
{ v, room, epoch,
  kind,        // "event"(milestone; relay stamps seq + persists) | "live"(forward only; never persisted; seq always null)
               // | "input" | "control" | "presence"
  session,     // sid or null
  command_id,  // optional; only carried by kind=input (and the plaintext frame input.ack); relay-readable; part of AAD
  seq,         // only kind=event; stamped by relay; not part of AAD
  ct, n, ts }

AAD = v | room | epoch | kind | session | command_id
```

An earlier skeleton guessed at an outer `{ envelope, milestone, command_id }`
wrapper, and treated `milestone`/`command_id` as routing handles the protocol did
not specify but the relay needed. This has been corrected following review —
`kind` itself is the routing handle (the two values `event`/`live` correspond to
the two combinations of the old "event + milestone boolean"), and `command_id` is
now formally a top-level envelope field. For the detailed rationale see the file
headers of `src/room-do.js` / `src/envelope.js`.

## What this skeleton does

- **S1 access and authentication** (`src/auth.js` + `fetch` in `src/room-do.js`):
  two credentials, two identities. The desktop presents the room owner credential
  in an `Authorization: Bearer` header; the relay hashes it and compares it in
  constant time with the hash registered when the room was claimed, and on success
  admits it as `desktop`. The remote (phone/browser) presents its credential via
  WebSocket subprotocols; there must be exactly one version entry
  `agentloom-rc-v1` and one `token.<64 hex chars>` entry, and the relay then looks
  that token up in the room's token registry to find the corresponding device, and
  on success admits it as `remote`. Tokens are no longer accepted in the query
  string. **Deny by default**: no credential, an ambiguous subprotocol format, no
  matching token registered for the room, or an expired or revoked token all yield
  401, and too-frequent authentication failures yield 429; there is no anonymous
  "just let me connect and look" path. Token **issuance and registration** is done
  by the desktop after pairing, by writing them into the relay through control
  frames; the relay is only responsible for comparison and expiry handling.
  **Authenticate before persisting**: business tables are only created after
  authentication succeeds, so a failed authentication never leaves any rows behind
  for the room.
- **S2 room DO** (`src/room-do.js` + `src/room-store.js`): one DO = one room. It
  accepts WebSockets using the **Hibernation API** (`ctx.acceptWebSocket` /
  `webSocketMessage` / `webSocketClose`, not `ws.accept()`). SQLite inside the DO
  stores the milestone event log (the `events` table; `seq` is allocated by a
  separate monotonic counter in `room_meta`, not `MAX(events.seq)+1` — so that seq
  cannot go backwards if a retention window trim is added later) plus a small
  amount of per-device connection metadata (`ws.serializeAttachment()`, with a
  ≤16KB guard in the implementation).
- **epoch double-write protection**: each time the desktop connects, the DO bumps
  the room epoch by 1; `insertMilestone` rejects writes whose `epoch` is behind
  the room's current epoch (when an old connection is not fully dead and a new one
  arrives, the old connection's writes are blocked).
- **`kind` itself decides whether to persist**: `kind=event` is always a
  milestone, persisted and stamped with a monotonically increasing `seq`;
  `kind=live` is always forward-only, not persisted, with `seq` always `null`;
  presence is not persisted.
- **Two channels**: `kind=input` goes through a FIFO — forwarded directly if the
  desktop is online, otherwise buffered in the `pending_input` table (30-minute
  TTL; on expiry `input.expired` is broadcast and the item is dropped);
  `kind=control` is delivered immediately — forwarded only if the desktop is
  online, otherwise the sender is told `desktop_offline` right away (not buffered,
  because the semantics of a "queue-jumping channel" are that it takes effect now
  or not at all, never "accumulate and take effect tomorrow").
- **Reconnect replay**: a remote connecting with `?last_seq=N` first gets a
  `{t:"replay.head", epoch, headSeq}` frame from the DO, then the milestones with
  `seq > N` are pushed one by one in ascending seq order, and only then does it
  join the live stream.
- **S3 quota: cut live, keep milestones**: a per-room "milestone writes per month"
  counter (the `quota_counters` table, bucketed by UTC year-month); once
  `MONTHLY_MILESTONE_LIMIT` is exceeded (default value in `src/quota.js`; the
  production number is to be set separately by the product side), only `kind=live`
  is degraded and dropped, and `{t:"quota.exceeded", channel:"live"}` is broadcast
  to all connections in the room (including the remote, not only the desktop);
  `kind=event` (milestones) is **never degraded by quota** — losing a milestone
  leaves an unrecoverable hole in the history the product promises, which is far
  worse than a bit of extra cost; `kind=control` is always let through (the quota
  gate must not stop even Stop from working); anonymous/token-less connections
  never reach this point, since S1 rejects them first.
- **Role-enforced direction** (`webSocketMessage`/`handlePlainFrame` in
  `src/room-do.js`): a connection's role is determined by the credential verified
  at admission (owner credential = `desktop`, subprotocol token = `remote`), not
  self-reported by the client. The relay uses it to police the directions it can
  observe: it rejects `role=remote` sending `kind=event` (otherwise any remote
  holding `K_room` could forge an "agent said this" broadcast, which would also be
  persisted / replayed by a future reconnect as real history); it rejects
  `role=desktop` sending `kind=input`; `input.ack` and `control.notify_hint` are
  only accepted from the desktop connection. Anything violating the direction is
  rejected with `{t:"error", reason:"role_forbidden"}` — not silently dropped.

## S4 same-origin static hosting + security headers (T6g1)

The `remote-web` (C1 mobile Web client) build output is mounted under this
worker's origin; the `[assets]` binding in `wrangler.toml` points at
`../remote-web/dist` — **before deploying / doing `wrangler dev` integration with
static assets you must run a build manually once**:

```bash
cd remote-web
npm run build   # generates/refreshes remote-web/dist/ -- wrangler does not trigger this step automatically
```

`fetch()` in `src/index.js` routes requests (`run_worker_first = true`, so every
request enters here first; see the comment in `wrangler.toml`): `/healthz` and
`/room/*` (WS upgrade / claim rate limiting) continue through the existing
routes with unchanged behavior; all other GET/HEAD requests are forwarded to
`env.ASSETS.fetch()` (`not_found_handling = "single-page-application"` — the
mobile client is an SPA with pure URL-fragment routing, so any path that does not
match a real file is answered with index.html, status 200 rather than 3xx), and
the response goes through `src/security-headers.js::withSecurityHeaders()`, which
applies four kinds of security headers uniformly: `Content-Security-Policy`
(script-src allows only `'self'` plus the SHA-256 hash of the inline fragment
bootstrap script in `remote-web/index.html`, leaving no `'unsafe-inline'` hole) /
`Referrer-Policy: no-referrer` / `X-Content-Type-Options: nosniff` /
`Cache-Control` (content-hashed files under `/assets/` get long-lived immutable
caching, everything else `no-store`).

**Coupling between the CSP hash and the actual inline script in dist (guarding
against drift from hand edits)**: the `BOOTSTRAP_INLINE_SCRIPT_SHA256_BASE64`
constant in `src/security-headers.js` is not a copy-once-and-forget value —
`test/security-headers.test.js` reads `remote-web/dist/index.html` at test time,
computes the SHA-256 of that inline script text, and asserts it equals this
constant; if the fragment bootstrap script in `remote-web/index.html` is edited
without updating the constant, this test goes red first, instead of the problem
only surfacing when the production CSP blocks the entire app into a white screen.

**Honest disclosure of the fragment security boundary (same stance as the E2EE
boundary in the "What this is and is not" section)**: these security headers
defend against external attack surface such as "third-party scripts/fonts getting
in" and "the pairing fragment being inherited by a 3xx or leaking into Referer".
They do not change the already disclosed trade-off that "E2EE for a
same-origin-hosted Web client protects against outsiders and intruders, not
against the relay operator itself" — CSP cannot stop the operator from shipping
malicious page code that reads the pairing material; that is a trade-off inherent
to the topology.

## Not done (the boundary of this skeleton, not omissions)

- Production quota numbers (`DEFAULT_MONTHLY_MILESTONE_LIMIT` is a placeholder
  that lets the skeleton run, not a number settled by the product).
- Staging deployment + a real-phone device matrix smoke test + a first-screen
  budget numeric gate (T6g2, a separate task — this task only covers the code /
  config / security headers for same-origin hosting and does not run
  `wrangler deploy`).
- The complete flow of desktop-side token issuance / pairing handshake is
  implemented in the desktop app; the relay in this directory is only responsible
  for registering tokens, comparing tokens and enforcing expiry.
- **Scope of rate limiting** (a different thing from S3's "monthly milestone
  quota", see the header of `src/quota.js` — quota governs "too many rows written
  this month", rate limiting governs "talking too fast this second"): `RL_CLAIM`/
  `RL_UPGRADE` in `wrangler.toml` cover the claim and WS upgrade entry points at
  the edge, keyed by source IP; inside a room there are additionally per-source-IP
  authentication-failure limiting, remote inbound frame limiting, pairing
  handshake frame limiting, a per-device concurrent connection cap, and per-device
  window limiting of input/control frames (see `src/room-do-ratelimit.js` for the
  implementation). The in-memory counters of these limiters reset after the DO
  hibernates; there is no message rate limit counted per room as a whole.
- **L2**: between "read the counter" and "write back +1" in seq allocation
  (`allocateSeq` in `room-store.js`) there must be no `await` — currently
  everything is synchronous so this holds naturally, and a comment states this
  invariant, but there is no dedicated concurrency test pinning it down.
- **L3**: `onlineDesktop()` currently just takes the first of
  `getWebSockets("desktop")`, and does not handle picking the one with the larger
  epoch when a new and an old desktop connection briefly coexist.
- **L4**: the FIFO order of `pending_input` is currently sorted only by
  `created_at`, with no secondary ordering such as rowid to break the ordering
  nondeterminism of same-millisecond concurrent enqueues.
- **L5**: `validateEnvelope` does not enforce that the `seq` of `kind=live`/
  `presence` envelopes must be `null` — it currently only checks that `seq` is a
  valid integer or null, without tightening by kind.

## Running locally

```bash
cd remote-relay
npm install
npm test               # pure-logic unit tests (envelope / auth / quota / room-store / room-do / S4 static hosting + security headers)

# Before integrating static assets (S4) / starting a local DO environment, build remote-web once (wrangler does not trigger this step automatically):
(cd ../remote-web && npm run build)

npx wrangler dev        # start a local DO environment for integration; needs wrangler to have network access
npx wrangler deploy --dry-run   # compile and validate only; does not actually publish
```

## How the tests work

The SQL logic in `src/room-store.js` does not depend on any Cloudflare-specific
API; it only depends on a minimal adapter interface
`sql.exec(query, ...params) -> Array<row object>`. `room-do.js` wraps
`ctx.storage.sql` (Cloudflare DO's real SQLite) with a thin adapter;
`test/room-store.test.js` wraps Node 20+'s built-in `node:sqlite` (real SQLite,
not a hand-rolled fake) with a thin adapter. Both sides run the **same**
`room-store.js` code, not "a separate piece of logic written for the tests that
merely looks similar" — so seq allocation, epoch rejection, replay queries and
quota counting are genuinely verified rather than self-certified.

`test/room-do.test.js` sets up a mock-ctx stand-in for `room-do.js`:
`ctx.storage.sql` reuses the same node:sqlite adapter; `ctx.acceptWebSocket` /
`ctx.getWebSockets` use an array as the connection registry; the WebSocket itself
implements only the three methods `send`/`serializeAttachment`/
`deserializeAttachment`. It drives `fetch()` (authentication 401/404, M2
authentication-first) and `webSocketMessage()` (message routing / H1 role
direction / H3 quota degradation) directly — these are the two entry points
RoomDO actually exposes to the outside world, and the stand-in only replaces the
runtime infrastructure they depend on; not a single line of business logic is
changed.

The only thing that cannot be run in unit tests is the Hibernation lifecycle
itself (the 101 response after a successful handshake, from
`new WebSocketPair()` to `return new Response(null, {status:101, webSocket:client})`);
the comment at the top of `src/room-do.js` marks it as "integration test pending
manual verification with wrangler dev".
