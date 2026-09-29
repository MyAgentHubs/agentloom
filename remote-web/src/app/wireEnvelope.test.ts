// wireEnvelope.test.ts — INT1b · parseWireEnvelope()/envelopeMeta()/buildControlEnvelope() 覆盖。

import { describe, expect, it } from "vitest";
import { buildAAD } from "../crypto/envelope.ts";
import { buildCommandEnvelope, buildControlEnvelope, envelopeMeta, parseWireEnvelope } from "./wireEnvelope.ts";

describe("parseWireEnvelope()", () => {
  it("parses a full kind=event envelope (all optional fields present)", () => {
    const raw = {
      v: 1,
      room: "0123456789abcdef0123456789abcdef",
      epoch: 3,
      kind: "event",
      session: null,
      command_id: null,
      seq: 42,
      client_msg_id: "msg.completed|sess-1|dedup-key",
      ct: "AAAA",
      n: "BBBB",
      ts: 1765430400123,
    };
    expect(parseWireEnvelope(raw)).toEqual({
      v: 1,
      room: "0123456789abcdef0123456789abcdef",
      epoch: 3,
      kind: "event",
      session: null,
      commandId: null,
      seq: 42,
      clientMsgId: "msg.completed|sess-1|dedup-key",
      ts: 1765430400123,
      ct: "AAAA",
      n: "BBBB",
    });
  });

  it("parses a kind=control envelope missing seq/client_msg_id/ts (defaults to null, not required)", () => {
    const raw = {
      v: 1,
      room: "0123456789abcdef0123456789abcdef",
      epoch: 1,
      kind: "control",
      session: "sess-1",
      command_id: "cmd-1",
      ct: "AAAA",
      n: "BBBB",
    };
    const parsed = parseWireEnvelope(raw);
    expect(parsed?.seq).toBeNull();
    expect(parsed?.clientMsgId).toBeNull();
    expect(parsed?.ts).toBeNull();
    expect(parsed?.session).toBe("sess-1");
    expect(parsed?.commandId).toBe("cmd-1");
  });

  it("command_id omitted entirely (kind=event/live legit shape) parses to null, not rejected", () => {
    const raw = { v: 1, room: "r", epoch: 0, kind: "event", session: null, ct: "A", n: "B" };
    expect(parseWireEnvelope(raw)?.commandId).toBeNull();
  });

  for (const bad of [
    null,
    undefined,
    "not an object",
    42,
    { room: "r", epoch: 0, kind: "event", ct: "A", n: "B" }, // missing v
    { v: 1, epoch: 0, kind: "event", ct: "A", n: "B" }, // missing room
    { v: 1, room: "r", kind: "event", ct: "A", n: "B" }, // missing epoch
    { v: 1, room: "r", epoch: 0, ct: "A", n: "B" }, // missing kind
    { v: 1, room: "r", epoch: 0, kind: "event", session: 42, ct: "A", n: "B" }, // session wrong type
    { v: 1, room: "r", epoch: 0, kind: "event", command_id: 42, ct: "A", n: "B" }, // command_id wrong type
    { v: 1, room: "r", epoch: 0, kind: "event", n: "B" }, // missing ct
    { v: 1, room: "r", epoch: 0, kind: "event", ct: "A" }, // missing n
  ]) {
    it(`rejects malformed input: ${JSON.stringify(bad)}`, () => {
      expect(parseWireEnvelope(bad)).toBeNull();
    });
  }
});

describe("envelopeMeta()", () => {
  it("picks exactly the 6 fields crypto/envelope.ts::buildAAD needs, and buildAAD() on it matches the manual formula", () => {
    const envelope = parseWireEnvelope({
      v: 1,
      room: "0123456789abcdef0123456789abcdef",
      epoch: 7,
      kind: "control",
      session: "sess-1",
      command_id: "cmd-1",
      ct: "AAAA",
      n: "BBBB",
    })!;
    const meta = envelopeMeta(envelope);
    expect(meta).toEqual({
      v: 1,
      room: "0123456789abcdef0123456789abcdef",
      epoch: 7,
      kind: "control",
      session: "sess-1",
      command_id: "cmd-1",
    });
    expect(buildAAD(meta)).toBe("1|0123456789abcdef0123456789abcdef|7|control|sess-1|cmd-1");
  });
});

describe("buildControlEnvelope()", () => {
  it("shapes a kind=control outbound envelope with seq:null and no client_msg_id key at all (M0 §1 wire grammar)", () => {
    const envelope = buildControlEnvelope({
      room: "0123456789abcdef0123456789abcdef",
      epoch: 2,
      session: "sess-1",
      commandId: "cmd-1",
      ct: "CT",
      n: "N",
      now: () => 1765430400123,
    });
    expect(envelope).toEqual({
      v: 1,
      room: "0123456789abcdef0123456789abcdef",
      epoch: 2,
      kind: "control",
      session: "sess-1",
      command_id: "cmd-1",
      seq: null,
      ct: "CT",
      n: "N",
      ts: 1765430400123,
    });
    expect(Object.prototype.hasOwnProperty.call(envelope, "client_msg_id")).toBe(false);
  });
});

describe("buildCommandEnvelope()", () => {
  it("kind='input' shapes a kind=input outbound envelope (T6f3 · M0 §3 input.send/input.answer)", () => {
    const envelope = buildCommandEnvelope({
      kind: "input",
      room: "0123456789abcdef0123456789abcdef",
      epoch: 4,
      session: "sess-1",
      commandId: "cmd-2",
      ct: "CT",
      n: "N",
      now: () => 1765430400123,
    });
    expect(envelope).toEqual({
      v: 1,
      room: "0123456789abcdef0123456789abcdef",
      epoch: 4,
      kind: "input",
      session: "sess-1",
      command_id: "cmd-2",
      seq: null,
      ct: "CT",
      n: "N",
      ts: 1765430400123,
    });
  });

  it("kind='control' produces the exact same shape buildControlEnvelope() produces (buildControlEnvelope is a thin alias)", () => {
    const params = { room: "0123456789abcdef0123456789abcdef", epoch: 2, session: "sess-1", commandId: "cmd-1", ct: "CT", n: "N", now: () => 1765430400123 };
    expect(buildCommandEnvelope({ kind: "control", ...params })).toEqual(buildControlEnvelope(params));
  });
});
