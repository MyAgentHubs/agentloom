// redact.test.ts — 两向语料（必打码 / 必不打码）+ 变异自证。

import { describe, expect, it } from "vitest";
import { redact, redactedLog } from "./redact.ts";

const HEX64 = "a".repeat(64);
const HEX32 = "b".repeat(32);
const HEX31 = "c".repeat(31);

describe("redact() — 正例：必须打码", () => {
  it("masks a bare hex64 token value with no marker prefix", () => {
    expect(redact(`access=${HEX64}`)).toBe("access=***");
  });

  it("masks a token embedded after 'Bearer '", () => {
    expect(redact(`Authorization: Bearer ${HEX64}`)).toBe("Authorization: Bearer ***");
  });

  it("masks a token embedded after 'token.' subprotocol marker", () => {
    expect(redact(`Sec-WebSocket-Protocol: agentloom-rc-v1, token.${HEX64}`)).toBe(
      "Sec-WebSocket-Protocol: agentloom-rc-v1, token.***",
    );
  });

  it("masks a query-string token= value", () => {
    expect(redact(`wss://relay/room/x?token=${HEX64}`)).toBe("wss://relay/room/x?token=***");
  });

  it("masks the exact 32-char boundary (>=32 is masked)", () => {
    expect(redact(`k=${HEX32}`)).toBe("k=***");
  });

  it("masks multiple independent hex runs in one message", () => {
    const message = `access=${HEX64} refresh=${HEX64}`;
    expect(redact(message)).toBe("access=*** refresh=***");
  });
});

describe("redact() — 反例：必须原样保留", () => {
  it("leaves a 31-char hex run untouched (below the floor)", () => {
    expect(redact(`k=${HEX31}`)).toBe(`k=${HEX31}`);
  });

  it("leaves short diagnostic identifiers like 'token.ack' alone", () => {
    expect(redact("t=token.ack")).toBe("t=token.ack");
  });

  it("leaves 'token.refresh_failed' alone (not a credential shape)", () => {
    expect(redact("reason=token.refresh_failed")).toBe("reason=token.refresh_failed");
  });

  it("leaves non-hex prose untouched", () => {
    expect(redact("connection closed: message_rate_limited")).toBe(
      "connection closed: message_rate_limited",
    );
  });

  it("leaves UUIDs (which contain '-' breaking the hex run) readable", () => {
    // UUID 里的连字符打断了连续 hex 游程,单段最长 12 字符,不会撞到 32 门槛。
    const uuid = "11111111-1111-4111-8111-111111111111";
    expect(redact(`device_id=${uuid}`)).toBe(`device_id=${uuid}`);
  });
});

describe("redact() — 变异自证：把门槛从 32 改成 0（相当于删掉门槛判断）必须让上面的反例转红", () => {
  function redactWithoutFloor(input: string): string {
    return input.replace(/[0-9a-fA-F]+/g, () => "***");
  }

  it("mutation proof: floor-less version masks the 31-char run that the real impl preserves", () => {
    const message = `k=${HEX31}`;
    expect(redact(message)).toBe(message); // 真实实现:不打码
    expect(redactWithoutFloor(message)).not.toBe(message); // 去掉门槛后:错误地打码了
  });

  it("mutation proof: floor-less version mangles the short hex-letter runs inside 'token.ack' ('e' in token, 'ac' in ack)", () => {
    expect(redact("t=token.ack")).toBe("t=token.ack");
    // 去掉门槛后,任何长度(哪怕 1 个字符)的十六进制字母游程都会被打码——"token" 里的 'e'、
    // "ack" 里的 'ac' 各自独立成一段十六进制游程,被分别打码,把这条诊断串弄得面目全非。这正是
    // 32 字符门槛存在的理由:真凭据永远是长游程(hex64),短游程绝不该被误伤。
    expect(redactWithoutFloor("t=token.ack")).toBe("t=tok***n.***k");
  });
});

describe("redactedLog()", () => {
  it("redacts both the message and string context values before handing off to the sink", () => {
    const calls: Array<{ level: string; message: string; context?: Record<string, unknown> }> = [];
    redactedLog(
      (level, message, context) => calls.push({ level, message, context }),
      "debug",
      `sending token.refresh with refresh=${HEX64}`,
      { requestId: "r1", access: HEX64, attempts: 2 },
    );
    expect(calls).toHaveLength(1);
    expect(calls[0].message).toBe("sending token.refresh with refresh=***");
    expect(calls[0].context).toEqual({ requestId: "r1", access: "***", attempts: 2 });
  });

  it("mutation proof: a sink that receives the raw (un-redacted) message would leak the token — asserting the redacted form makes that regression visible", () => {
    let seenMessage = "";
    redactedLog((_level, message) => (seenMessage = message), "info", `access=${HEX64}`);
    expect(seenMessage).not.toContain(HEX64);
  });
});
