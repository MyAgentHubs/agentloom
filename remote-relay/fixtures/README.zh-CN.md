# fixtures

[English](README.md) · **简体中文**

跨端契约样张，桌面端 Rust 测试与 relay 测试共用。每个 JSON 文件是远程控制
线协议某一块的共同真相源：线帧信封、加密已知答案向量、数据面里程碑帧。
`crypto-kat-v1.json` 含 RFC 7748 官方 X25519 测试向量。

这里所有取值都是公开的、非机密的协议样张——不含真实密钥、令牌或凭据。

`wire-v1.9-pending.json` 已删除：`reply` kind 五张信封正反例样张
（依次为：合法带 command_id / 缺 command_id 拒 / 带 client_msg_id 拒 / 带 seq 拒 / session=null 合法；样张名 `reply_with_command_id`/`reply_missing_command_id`/`reply_with_client_msg_id_rejected`/
`reply_with_seq_rejected`/`reply_with_null_session`）已合入 `wire-v1.json` 正式文件（既有条目
零改动，只追加，现共 139 条；`envelope.js` 的 `ENVELOPE_KINDS` 新增 `reply`，relay
route 表 `reply_routes` 落地，见 `remote-relay/src/roomStoreReplyRoutes.js` 与
`remote-relay/test/msg-reply-route.test.js`）——relay 侧 `envelope.js` `ENVELOPE_KINDS`/`validateEnvelope` 与
`remote-web/src/crypto/envelope.test.ts` 的计数断言均已同步。

`data-plane-v1.9-pending.json` 已删除：5 张样张
（`msg_fetch_request`/`msg_chunk`/`msg_fetch_error_stale_revision`/`msg_fetch_error_not_found`/
`msg_completed_with_content_ref`）已合入 `data-plane-v1.json` 正式文件（既有 31 张条目零改动，
只追加，`cases` 现共 36 张·32 valid/4 invalid）——对应
`remote-web/src/events/parseFrame.ts`（`msg.fetch`/`msg.chunk`/`msg.fetch.error` 三型解析 +
`msg.completed`/history row 的可选 `content_ref` 透传）、
`remote-web/src/events/msgFetch.ts`（`MsgChunkReassembler` 重组状态机 + `MsgFetchClient` 发送/
接收编排）、`remote-web/src/events/milestoneProjection.ts`（`revision` 高者胜投影）的真路径消费；
`remote-web/src/events/parseFrame.test.ts` 的硬编码计数断言同步改为 36/32/4。

`client-msg-id-derivation-v1.json` 的 `revision` 派生新用例
已合入正式文件（4+2=6 条向量）：`revision==1` 与现行派生逐字节相同——不追加 `|1`
后缀，这条『存量零扰动』约束由文件里第一条既有向量（`"msg.completed|s-1|dk-1"`）
证明；`revision>1` 在 name 末尾追加 `|<revision>`（如
`"msg.completed|s-1|dk-1|2"`），对应 `remote_gateway.rs` 的
`derive_msg_completed_client_msg_id(session_id, dedup_key, revision)`。原 pending
样张 `client-msg-id-derivation-v1.9-pending.json` 已删除。

relay 服务端实现本身已在本仓库开源，见
https://github.com/MyAgentHubs/agentloom/tree/main/remote-relay

`data-plane-v1.json` 还包含 `input_ack_failed_no_agent`：桌面端异步的终态失败回执，
带可选字段 `reason: "no_agent"`。
