import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  validateEnvelope,
  validatePairAcceptFrame,
  buildAAD,
  PROTOCOL_VERSION,
  base64DecodedByteLength,
} from "../src/envelope.js";

const ROOM = "a".repeat(32);
const CT = Buffer.from("hello ciphertext").toString("base64");
const N12 = Buffer.alloc(12, 7).toString("base64"); // 12 字节 nonce
const wireFixtures = JSON.parse(
  readFileSync(new URL("../fixtures/wire-v1.json", import.meta.url), "utf8"),
);

for (const name of [
  "pair_accept_encrypted_tokens_valid",
  "pair_accept_tokens_ct_missing",
  "pair_accept_plaintext_tokens_forbidden",
]) {
  test(`pair.accept validator consumes wire sample: ${name}`, () => {
    const fixture = wireFixtures.find((item) => item.name === name);
    assert.ok(fixture, `${name}: fixture exists`);

    const result = validatePairAcceptFrame(fixture.frame);

    assert.equal(result.ok, fixture.expect.valid, name);
    assert.deepEqual(result.errors, fixture.expect.errors, name);
  });
}

function baseEnvelope(overrides = {}) {
  const kind = overrides.kind ?? "event";
  return {
    v: PROTOCOL_VERSION,
    room: ROOM,
    epoch: 1,
    seq: null,
    kind,
    session: "s1",
    client_msg_id: kind === "event" ? "cmid-1" : null,
    ct: CT,
    n: N12,
    ts: 1765430400123,
    ...overrides,
  };
}

test("合法信封通过校验", () => {
  const result = validateEnvelope(baseEnvelope());
  assert.equal(result.ok, true);
  assert.equal(result.envelope.room, ROOM);
  assert.equal(result.envelope.kind, "event");
});

test("缺字段被拒——room 缺失", () => {
  const obj = baseEnvelope();
  delete obj.room;
  const result = validateEnvelope(obj);
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_room"));
});

test("坏 v 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ v: 2 }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("unsupported_version"));
});

test("v 缺失也被拒", () => {
  const obj = baseEnvelope();
  delete obj.v;
  const result = validateEnvelope(obj);
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("unsupported_version"));
});

test("room 不是 32 位 hex 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ room: "not-hex" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_room"));
});

test("kind 不在枚举内被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "bogus" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_kind"));
});

test("kind=live 合法（T4 修复轮新增：只转发不落库，取代旧 milestone 布尔位）", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "live" }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.kind, "live");
});

test("epoch 非整数被拒", () => {
  const result = validateEnvelope(baseEnvelope({ epoch: "seven" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_epoch"));
});

test("epoch 负数被拒", () => {
  const result = validateEnvelope(baseEnvelope({ epoch: -1 }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_epoch"));
});

test("seq 为 null 合法（live/presence 场景）", () => {
  const result = validateEnvelope(baseEnvelope({ seq: null }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.seq, null);
});

test("seq 为负数被拒", () => {
  const result = validateEnvelope(baseEnvelope({ seq: -5 }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_seq"));
});

test("seq 出现在 kind=live 上被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "live", seq: 5 }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("seq_not_allowed_for_kind"));
});

test("seq 出现在 kind=presence 上被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "presence", seq: 5 }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("seq_not_allowed_for_kind"));
});

test("seq 在 kind=event 上仍然合法", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event", seq: 42 }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.seq, 42);
});

test("seq=null 在非 event 的 kind 上仍然合法", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "live", seq: null }));
  assert.equal(result.ok, true);
});

test("ct 不是合法 base64 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ ct: "not base64!!" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_ct"));
});

test("ct 缺失被拒", () => {
  const obj = baseEnvelope();
  delete obj.ct;
  const result = validateEnvelope(obj);
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_ct"));
});

test("nonce 不是 12 字节被拒", () => {
  const badNonce = Buffer.alloc(16, 1).toString("base64"); // 16 字节，不是 12
  const result = validateEnvelope(baseEnvelope({ n: badNonce }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_nonce"));
});

test("ts 非整数被拒", () => {
  const result = validateEnvelope(baseEnvelope({ ts: "not-a-number" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_ts"));
});

test("session 为 null 合法（会话索引流）", () => {
  const result = validateEnvelope(baseEnvelope({ session: null }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.session, null);
});

// ---- session：长度上限 128 UTF-8 字节（G7 收紧·两向语料）----

test("session 恰好 128 字节（纯 ASCII）合法（边界含）", () => {
  const boundary = "a".repeat(128);
  const result = validateEnvelope(baseEnvelope({ session: boundary }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.session, boundary);
});

test("session 恰好 128 字节（非 ASCII 组合边界：多字节字符+单字节字符混合）合法", () => {
  const boundary = "中".repeat(42) + "ab"; // 42*3 + 2 = 128 字节，字符数只有 44
  const result = validateEnvelope(baseEnvelope({ session: boundary }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.session, boundary);
});

test("session 129 字节（纯 ASCII）被拒", () => {
  const overLong = "a".repeat(129);
  const result = validateEnvelope(baseEnvelope({ session: overLong }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("session_too_long"));
});

test("session 129 字节（纯多字节字符，按 UTF-8 字节计非字符数）被拒", () => {
  const overLong = "中".repeat(43); // 43*3=129 字节，字符数只有 43
  const result = validateEnvelope(baseEnvelope({ session: overLong }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("session_too_long"));
});

// ---- command_id：kind=input|control 必填，kind=event|live|presence 禁止携带 ----

test("command_id 缺失合法（event 信封的常态）", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event" }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.command_id, null);
});

test("kind=input 未写 command_id 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "input", seq: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("command_id_required_for_kind"));
});

test("kind=input 的 command_id=null 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "input", command_id: null, seq: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("command_id_required_for_kind"));
});

test("kind=input 的 command_id=undefined 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "input", command_id: undefined, seq: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("command_id_required_for_kind"));
});

test("kind=input 的 command_id 为空字符串时按缺失处理", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "input", command_id: "", seq: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("command_id_required_for_kind"));
});

test("kind=input 带非空 command_id 合法", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "input", command_id: "cmd-1", seq: null }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.command_id, "cmd-1");
});

test("kind=control 缺失 command_id 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "control", seq: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("command_id_required_for_kind"));
});

test("kind=control 的 command_id 为空字符串时按缺失处理", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "control", command_id: "", seq: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("command_id_required_for_kind"));
});

test("kind=control 带非空 command_id 合法", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "control", command_id: "cmd-control-1", seq: null }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.command_id, "cmd-control-1");
});

for (const kind of ["event", "live", "presence"]) {
  test(`kind=${kind} 带 command_id 被拒`, () => {
    const result = validateEnvelope(baseEnvelope({ kind, command_id: "cmd-1", seq: null }));
    assert.equal(result.ok, false);
    assert.ok(result.errors.includes("command_id_not_allowed_for_kind"));
  });
}

test("非字符串 command_id 独立报 bad_command_id", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event", command_id: 123 }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_command_id"));
  assert.ok(!result.errors.includes("command_id_not_allowed_for_kind"));
});

test("command_id 不是字符串被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "input", command_id: 123, seq: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_command_id"));
});

test("多个字段同时出错时全部列出", () => {
  const result = validateEnvelope(baseEnvelope({ v: 9, room: "x", kind: "y" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("unsupported_version"));
  assert.ok(result.errors.includes("bad_room"));
  assert.ok(result.errors.includes("bad_kind"));
});

test("base64DecodedByteLength 计算正确（含 padding）", () => {
  assert.equal(base64DecodedByteLength(N12), 12);
  assert.equal(base64DecodedByteLength("////"), 3); // 4 字符、无 padding = 3 字节
  assert.equal(base64DecodedByteLength("not valid"), -1);
});

// AAD order is v|room|epoch|kind|session|command_id; seq is excluded.

test("AAD 拼接顺序正确——含 command_id、不含 seq", () => {
  const aad = buildAAD({ v: 1, room: ROOM, epoch: 7, kind: "input", session: "sess-1", command_id: "cmd-42" });
  assert.equal(aad, `1|${ROOM}|7|input|sess-1|cmd-42`);
});

test("AAD 里 null 字段序列化为空字符串", () => {
  const aad = buildAAD({ v: 1, room: ROOM, epoch: 3, kind: "presence", session: null, command_id: null });
  assert.equal(aad, `1|${ROOM}|3|presence||`);
});

test("AAD 字段顺序不因传入对象属性顺序而变——恒定按 v/room/epoch/kind/session/command_id", () => {
  // 刻意用与声明顺序不同的属性书写顺序构造输入对象，验证输出仍按固定顺序拼接。
  const aad = buildAAD({ command_id: "c9", session: "s9", kind: "control", epoch: 2, room: ROOM, v: 1 });
  assert.equal(aad, `1|${ROOM}|2|control|s9|c9`);
});

test("AAD 不受 seq 影响——传了 seq 也不出现在结果里（seq 是 relay 盖的、不入 AAD）", () => {
  const withSeq = buildAAD({ v: 1, room: ROOM, epoch: 7, kind: "event", session: "s1", command_id: null, seq: 1042 });
  const withoutSeq = buildAAD({ v: 1, room: ROOM, epoch: 7, kind: "event", session: "s1", command_id: null });
  assert.equal(withSeq, withoutSeq);
  assert.ok(!withSeq.includes("1042"));
});

// ---- client_msg_id：kind=event 必填，其余 kind 禁止携带（v1.7.4） ----

test("client_msg_id 缺失于 kind=event 被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event", client_msg_id: null }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("client_msg_id_required_for_kind"));
});

test("client_msg_id 为空字符串于 kind=event 按缺失处理", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event", client_msg_id: "" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("client_msg_id_required_for_kind"));
});

test("client_msg_id 非空字符串于 kind=event 合法", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event", client_msg_id: "cmid-42" }));
  assert.equal(result.ok, true);
  assert.equal(result.envelope.client_msg_id, "cmid-42");
});

for (const kind of ["live", "input", "control", "presence"]) {
  test(`client_msg_id 出现在 kind=${kind} 上被拒`, () => {
    const overrides = { kind, client_msg_id: "cmid-x", seq: null };
    if (kind === "input" || kind === "control") overrides.command_id = "cmd-required";
    const result = validateEnvelope(baseEnvelope(overrides));
    assert.equal(result.ok, false);
    assert.ok(result.errors.includes("client_msg_id_not_allowed_for_kind"));
  });
}

test("client_msg_id 含竖线被拒", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event", client_msg_id: "cm|id" }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("client_msg_id_must_not_contain_pipe"));
});

test("client_msg_id 超 64 字节被拒（按 UTF-8 字节计，非 UTF-16 码元/字符数）", () => {
  const overLong = "中".repeat(22); // 22*3=66 字节，字符数只有 22
  const result = validateEnvelope(baseEnvelope({ kind: "event", client_msg_id: overLong }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("client_msg_id_too_long"));
});

test("client_msg_id 恰好 64 字节合法（边界含）", () => {
  const boundary = "中".repeat(21) + "a"; // 21*3+1=64 字节
  const result = validateEnvelope(baseEnvelope({ kind: "event", client_msg_id: boundary }));
  assert.equal(result.ok, true);
});

test("非字符串 client_msg_id 独立报 bad_client_msg_id", () => {
  const result = validateEnvelope(baseEnvelope({ kind: "event", client_msg_id: 123 }));
  assert.equal(result.ok, false);
  assert.ok(result.errors.includes("bad_client_msg_id"));
});

test("client_msg_id 不入 AAD——传了也不出现在拼串结果里", () => {
  const withIt = buildAAD({ v: 1, room: ROOM, epoch: 7, kind: "event", session: "s1", command_id: null, client_msg_id: "cmid-should-not-appear" });
  const withoutIt = buildAAD({ v: 1, room: ROOM, epoch: 7, kind: "event", session: "s1", command_id: null });
  assert.equal(withIt, withoutIt);
  assert.ok(!withIt.includes("cmid-should-not-appear"));
});
