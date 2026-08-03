import { describe, expect, it } from "vitest";

import {
  accumulateSessionUsage,
  accumulateWorkingTokens,
  formatTokenCount,
  sessionUsageDetail,
  sessionUsageFromSession,
  sessionUsageTotal,
} from "./sessionUsage";

describe("accumulateWorkingTokens", () => {
  it("adds both input and output deltas regardless of their relative size", () => {
    expect(accumulateWorkingTokens(1_000, 100, 25)).toBe(1_125);
    expect(accumulateWorkingTokens(1_000, 25, 100)).toBe(1_125);
  });

  it("treats null and undefined fields as zero", () => {
    expect(accumulateWorkingTokens(10, null, 5)).toBe(15);
    expect(accumulateWorkingTokens(10, 5, undefined)).toBe(15);
  });
});

describe("sessionUsageFromSession", () => {
  it("reads the accumulated input and output totals", () => {
    expect(
      sessionUsageFromSession({
        total_input_tokens: 74_200,
        total_output_tokens: 12_100,
      }),
    ).toEqual({ input: 74_200, output: 12_100 });
  });

  it("normalizes NaN and negative values to zero", () => {
    expect(
      sessionUsageFromSession({
        total_input_tokens: Number.NaN,
        total_output_tokens: -1,
      }),
    ).toEqual({ input: 0, output: 0 });
  });
});

describe("accumulateSessionUsage", () => {
  it("adds distinct completed-event counts without mutating the previous value", () => {
    const prev = { input: 100, output: 200 };

    expect(accumulateSessionUsage(prev, 7, 13)).toEqual({
      input: 107,
      output: 213,
    });
    expect(prev).toEqual({ input: 100, output: 200 });
  });

  it("treats undefined and null counts as zero", () => {
    expect(
      accumulateSessionUsage({ input: 3, output: 5 }, undefined, null),
    ).toEqual({ input: 3, output: 5 });
  });
});

describe("formatTokenCount", () => {
  it.each([
    [0, "0"],
    [999, "999"],
    [1_000, "1k"],
    [1_049, "1k"],
    [12_400, "12.4k"],
    [99_949, "99.9k"],
    [100_000, "100k"],
    [999_499, "999k"],
    [999_500, "1.0M"],
    [999_999, "1.0M"],
    [1_000_000, "1.0M"],
    [1_240_000, "1.2M"],
  ])("formats %i as %s", (value, expected) => {
    expect(formatTokenCount(value)).toBe(expected);
  });
});

describe("session usage derived values", () => {
  it("sums input and output", () => {
    expect(sessionUsageTotal({ input: 74_200, output: 12_100 })).toBe(86_300);
  });

  it("builds the hover detail", () => {
    expect(sessionUsageDetail({ input: 74_200, output: 12_100 })).toBe(
      "↑ 74.2k · ↓ 12.1k",
    );
  });
});
