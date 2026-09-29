# fixtures

**English** · [简体中文](README.zh-CN.md)

Cross-endpoint contract fixtures shared by the desktop Rust test suite and the
relay test suite. Each JSON file is a shared source of truth for one slice of
the remote-control wire protocol: wire-frame envelopes, encryption known-answer
vectors, and data-plane milestone frames. `crypto-kat-v1.json` includes the
official RFC 7748 X25519 test vectors.

All values here are public, non-secret protocol fixtures — no live keys,
tokens, or credentials.

`wire-v1.9-pending.json` has been deleted: the five positive/negative `reply` kind
envelope samples (valid with command_id / rejected without command_id / rejected with
client_msg_id / rejected with seq / valid with session=null, in that order: `reply_with_command_id`/`reply_missing_command_id`/`reply_with_client_msg_id_rejected`/
`reply_with_seq_rejected`/`reply_with_null_session`) have been merged into the
official `wire-v1.json` file (existing entries unchanged, append-only, now 139
entries in total; `ENVELOPE_KINDS` in `envelope.js` gains `reply`, and the relay
route table `reply_routes` is in place, see `remote-relay/src/roomStoreReplyRoutes.js`
and `remote-relay/test/msg-reply-route.test.js`) — the count assertions in the relay-side `envelope.js`
`ENVELOPE_KINDS`/`validateEnvelope` and in `remote-web/src/crypto/envelope.test.ts`
have both been updated accordingly.

`data-plane-v1.9-pending.json` has been deleted: its 5
samples (`msg_fetch_request`/`msg_chunk`/`msg_fetch_error_stale_revision`/`msg_fetch_error_not_found`/
`msg_completed_with_content_ref`) have been merged into the official
`data-plane-v1.json` file (the existing 31 entries unchanged, append-only;
`cases` now totals 36 · 32 valid / 4 invalid) — they are consumed by real code
paths in `remote-web/src/events/parseFrame.ts` (parsing of the three types
`msg.fetch`/`msg.chunk`/`msg.fetch.error`, plus pass-through of the optional
`content_ref` on `msg.completed`/history rows),
`remote-web/src/events/msgFetch.ts` (the `MsgChunkReassembler` reassembly state
machine + `MsgFetchClient` send/receive orchestration), and
`remote-web/src/events/milestoneProjection.ts` (projection where the higher
`revision` wins); the hard-coded count assertions in
`remote-web/src/events/parseFrame.test.ts` were updated to 36/32/4.

The new `revision` derivation cases in `client-msg-id-derivation-v1.json`
have been merged into the official file (4+2=6 vectors): `revision==1` is
byte-for-byte identical to the current derivation — no `|1` suffix is appended,
and this "zero disturbance to existing data" constraint is proven by the first
existing vector in the file (`"msg.completed|s-1|dk-1"`); for `revision>1`,
`|<revision>` is appended to the end of the name (e.g.
`"msg.completed|s-1|dk-1|2"`), corresponding to
`derive_msg_completed_client_msg_id(session_id, dedup_key, revision)` in
`remote_gateway.rs`. The original pending sample
`client-msg-id-derivation-v1.9-pending.json` has been deleted.

The relay server implementation itself is open-sourced in this repository, at
https://github.com/MyAgentHubs/agentloom/tree/main/remote-relay

`data-plane-v1.json` also includes `input_ack_failed_no_agent`, the desktop's asynchronous
terminal failure receipt with the optional `reason: "no_agent"` field.
