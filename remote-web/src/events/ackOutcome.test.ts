// ackOutcome.test.ts — TDD 覆盖 src/events/ackOutcome.ts。
//
// 全部自造语料，不是 fixture 消费方——见 ackOutcome.ts 顶部注释：data-plane-v1.json 明确没有
// "未知 outcome" 反样张（fixture 自己的 coverage 表注明了原因），三个已知值（ok/queued/failed）
// 的解析已经在 parseFrame.test.ts 的 input_ack_valid_deletes_pending 正例里覆盖过一次（那是
// t 判别层），这里测的是 outcome 语义规范化这一层，两者不重复。

import { describe, expect, it } from "vitest";
import { normalizeInputAckOutcome, TAKEN_OVER } from "./ackOutcome.ts";

describe("normalizeInputAckOutcome()", () => {
  it.each(["ok", "queued", "failed"] as const)("recognizes known outcome %s and passes it through unchanged", (outcome) => {
    expect(normalizeInputAckOutcome(outcome)).toEqual({ raw: outcome, recognized: true, effective: outcome });
  });

  it("falls back unknown outcomes to the independent neutral state 'taken_over' — NOT 'ok' (审查返工·校准：不得用一个更强的已知态冒充未知态), and flags recognized:false", () => {
    const result = normalizeInputAckOutcome("some_future_outcome");
    expect(result).toEqual({
      raw: "some_future_outcome",
      recognized: false,
      effective: "taken_over",
    });
    expect(result.effective).toBe(TAKEN_OVER);
    // The whole point of this fix: an unrecognized outcome must never be indistinguishable from a
    // genuine "ok" (which is a strong "definitely succeeded" claim we have no evidence for).
    expect(result.effective).not.toBe("ok");
  });

  it("treats an empty string as unrecognized rather than crashing", () => {
    expect(normalizeInputAckOutcome("")).toEqual({ raw: "", recognized: false, effective: "taken_over" });
  });

  it("is case-sensitive (relay forwards outcome verbatim, no normalization upstream) — 'OK' is unrecognized, not coerced to the known lowercase 'ok'", () => {
    expect(normalizeInputAckOutcome("OK")).toEqual({ raw: "OK", recognized: false, effective: "taken_over" });
  });

  it("'taken_over' itself, if it ever showed up as a literal wire outcome, would still be unrecognized (it is not in the protocol's three legal values)", () => {
    expect(normalizeInputAckOutcome("taken_over")).toEqual({
      raw: "taken_over",
      recognized: false,
      effective: "taken_over",
    });
  });
});
