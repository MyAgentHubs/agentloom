import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { CLIENT_MSG_ID_NAMESPACE, deriveClientMsgId, deriveMsgCompletedClientMsgId } from "./clientMsgId.ts";

interface DerivationFixture {
  namespace: string;
  vectors: Array<{ name: string; expect: string }>;
}

const fixture = JSON.parse(
  readFileSync(
    path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../remote-relay/fixtures/client-msg-id-derivation-v1.json"),
    "utf8",
  ),
) as DerivationFixture;

describe("client_msg_id UUIDv5 派生", () => {
  it("固定 namespace 与共享样张一致", () => {
    expect(CLIENT_MSG_ID_NAMESPACE).toBe(fixture.namespace);
  });

  for (const vector of fixture.vectors) {
    it(`命中共享样张：${vector.name}`, () => {
      expect(deriveClientMsgId(vector.name)).toBe(vector.expect);
    });
  }

  it("msg.completed + remote_input 组合与 Rust derive_msg_completed_client_msg_id 对齐", () => {
    // 期望值由同一 name 串
    // `msg.completed|session-42|remote_input:123e4567-e89b-12d3-a456-426614174000`
    // 经标准 UUIDv5 独立算出；拼串形状对应 Rust
    // `remote_gateway.rs::derive_msg_completed_client_msg_id` +
    // `display_reduce.rs::remote_input_key`。
    expect(deriveMsgCompletedClientMsgId("session-42", "123e4567-e89b-12d3-a456-426614174000")).toBe(
      "14e1c0c6-8d12-5896-8429-16aa840f82d8",
    );
  });

  it("按 UTF-8 派生含中文与 emoji 的 name", () => {
    // 期望值由 remote-relay/test/client-msg-id-derivation.test.js 同款的独立 Node
    // createHash("sha1") 参考实现离线算出，防止 TextEncoder 被误改成 Latin-1 路径。
    expect(deriveClientMsgId("msg.completed|会话-东京🚀|remote_input:命令-你好👋")).toBe(
      "7f21432f-b826-5668-9d31-43937d6d423e",
    );
  });
});
