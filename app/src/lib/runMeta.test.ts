import { describe, it, expect } from "vitest";
import { formatRunMeta } from "./runMeta";

describe("formatRunMeta", () => {
  it("秒 + tokens（<60s·<1000 tok）", () => {
    expect(
      formatRunMeta({ cost_usd: null, output_tokens: 42, elapsed_sec: 28 }),
    ).toBe("28s · 42 tok");
  });
  it("千 token → Xk tok（一位小数）", () => {
    expect(
      formatRunMeta({ cost_usd: null, output_tokens: 12400, elapsed_sec: 28 }),
    ).toBe("28s · 12.4k tok");
  });
  it("≥60s → Xm SSs（秒零填充两位）", () => {
    expect(
      formatRunMeta({ cost_usd: null, output_tokens: 11400, elapsed_sec: 186 }),
    ).toBe("3m 06s · 11.4k tok");
  });
  it("无 elapsed_sec → 只 tokens", () => {
    expect(formatRunMeta({ cost_usd: null, output_tokens: 42 })).toBe("42 tok");
  });
  it("无 tokens 有 elapsed → 只秒", () => {
    expect(
      formatRunMeta({ cost_usd: null, output_tokens: null, elapsed_sec: 5 }),
    ).toBe("5s");
  });
  it("全空 → 空串", () => {
    expect(formatRunMeta({ cost_usd: null, output_tokens: null })).toBe("");
  });
  it("null done → 空串", () => {
    expect(formatRunMeta(null)).toBe("");
  });
});
