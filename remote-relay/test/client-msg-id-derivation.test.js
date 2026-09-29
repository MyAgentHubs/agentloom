import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const fixture = JSON.parse(
  readFileSync(new URL("../fixtures/client-msg-id-derivation-v1.json", import.meta.url), "utf8"),
);

// 标准 UUIDv5（RFC 4122，命名空间+SHA-1）参考实现，纯本文件内自包含，不依赖
// 任何第三方 uuid 库——relay 本身不派生 client_msg_id，这份实现只用来当
// fixture 的独立校验证据。
function uuidv5(name, namespace) {
  const nsBytes = Buffer.from(namespace.replace(/-/g, ""), "hex");
  const nameBytes = Buffer.from(name, "utf8");
  const hash = createHash("sha1").update(Buffer.concat([nsBytes, nameBytes])).digest();
  const bytes = Buffer.from(hash.subarray(0, 16));
  bytes[6] = (bytes[6] & 0x0f) | 0x50; // version 5
  bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC4122 variant
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

test("uuidv5 参考实现对齐 RFC 4122 官方已知测试向量（NAMESPACE_DNS + python.org）", () => {
  assert.equal(
    uuidv5("python.org", "6ba7b810-9dad-11d1-80b4-00c04fd430c8"),
    "886313e1-3b8a-5372-9b90-0c9aee199e5d",
  );
});

for (const vector of fixture.vectors) {
  test(`client_msg_id 派生向量：${vector.name}`, () => {
    assert.equal(uuidv5(vector.name, fixture.namespace), vector.expect);
  });
}
