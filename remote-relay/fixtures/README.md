# fixtures

Cross-endpoint contract fixtures shared by the desktop Rust test suite and the
relay test suite. Each JSON file is a shared source of truth for one slice of
the remote-control wire protocol: wire-frame envelopes, encryption known-answer
vectors, and data-plane milestone frames. `crypto-kat-v1.json` includes the
official RFC 7748 X25519 test vectors.

All values here are public, non-secret protocol fixtures — no live keys,
tokens, or credentials.

`wire-v1.9-pending.json` 已删除（msgfix1 T1·M0 §10.1/§10.2）：`reply` kind 五张信封样张
（`reply_with_command_id`/`reply_missing_command_id`/`reply_with_client_msg_id_rejected`/
`reply_with_seq_rejected`/`reply_with_null_session`）已合入 `wire-v1.json` 正式文件（既有条目
零改动，只追加，现共 139 条）——relay 侧 `envelope.js` `ENVELOPE_KINDS`/`validateEnvelope` 与
`remote-web/src/crypto/envelope.test.ts` 的计数断言均已同步。

`data-plane-v1.9-pending.json` 已删除（msgfix1 T6·M0 §10.4/§10.5/§10.6）：5 张样张
（`msg_fetch_request`/`msg_chunk`/`msg_fetch_error_stale_revision`/`msg_fetch_error_not_found`/
`msg_completed_with_content_ref`）已合入 `data-plane-v1.json` 正式文件（既有 31 张条目零改动，
只追加，`cases` 现共 36 张·32 valid/4 invalid）——对应
`remote-web/src/events/parseFrame.ts`（`msg.fetch`/`msg.chunk`/`msg.fetch.error` 三型解析 +
`msg.completed`/history row 的可选 `content_ref` 透传）、
`remote-web/src/events/msgFetch.ts`（`MsgChunkReassembler` 重组状态机 + `MsgFetchClient` 发送/
接收编排）、`remote-web/src/events/milestoneProjection.ts`（`revision` 高者胜投影）的真路径消费；
`remote-web/src/events/parseFrame.test.ts` 的硬编码计数断言同步改为 36/32/4。

`client-msg-id-derivation-v1.json` 的 `revision` 派生新用例（msgfix1 T5·M0 §10.7）
已合入正式文件（4+2=6 条向量）：`revision==1` 与现行派生逐字节相同——不追加 `|1`
后缀，这条『存量零扰动』约束由文件里第一条既有向量（`"msg.completed|s-1|dk-1"`）
证明；`revision>1` 在 name 末尾追加 `|<revision>`（如
`"msg.completed|s-1|dk-1|2"`），对应 `remote_gateway.rs` 的
`derive_msg_completed_client_msg_id(session_id, dedup_key, revision)`。原 pending
样张 `client-msg-id-derivation-v1.9-pending.json` 已删除。


`wire-v1.9-pending.json`（`reply` kind 信封 5 条正反例：合法带 command_id/缺
command_id 拒/带 client_msg_id 拒/带 seq 拒/session=null 合法）已随 msgfix1 T1
合入正式 `wire-v1.json`（`envelope.js` 的 `ENVELOPE_KINDS` 新增 `reply` + relay
route 表 `reply_routes` 落地，详 M0 协议文档 §10.3）——pending 文件已删除，不再
单独列出。

The relay server implementation itself is open-sourced separately at
https://github.com/MyAgentHubs/agentloom-remote-control-server
