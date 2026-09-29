// backoff.test.ts

import { describe, expect, it } from "vitest";
import { BackoffTracker, computeBackoffDelayMs } from "./backoff.ts";

describe("computeBackoffDelayMs()", () => {
  it("with jitterRatio=0, grows exponentially by `factor` per attempt", () => {
    const options = { baseMs: 100, capMs: 100_000, jitterRatio: 0 };
    expect(computeBackoffDelayMs(0, options)).toBe(100);
    expect(computeBackoffDelayMs(1, options)).toBe(200);
    expect(computeBackoffDelayMs(2, options)).toBe(400);
    expect(computeBackoffDelayMs(3, options)).toBe(800);
  });

  it("caps growth at capMs — never exceeds it regardless of attempt count", () => {
    const options = { baseMs: 100, capMs: 500, jitterRatio: 0 };
    expect(computeBackoffDelayMs(10, options)).toBe(500);
    expect(computeBackoffDelayMs(50, options)).toBe(500);
  });

  it("full-jitter mode (default jitterRatio=1) stays within [0, theoretical]", () => {
    const options = { baseMs: 100, capMs: 100_000 };
    for (let attempt = 0; attempt < 6; attempt += 1) {
      const theoretical = Math.min(100_000, 100 * 2 ** attempt);
      const delay = computeBackoffDelayMs(attempt, options);
      expect(delay).toBeGreaterThanOrEqual(0);
      expect(delay).toBeLessThanOrEqual(theoretical);
    }
  });

  it("injected random source is used deterministically", () => {
    const options = { baseMs: 100, capMs: 100_000, random: () => 0.5 };
    // attempt 0: theoretical=100, floor=0, span=100 → 0 + 0.5*100 = 50
    expect(computeBackoffDelayMs(0, options)).toBe(50);
  });

  it("rejects a negative or non-integer attempt", () => {
    expect(() => computeBackoffDelayMs(-1, { baseMs: 10, capMs: 100 })).toThrow();
    expect(() => computeBackoffDelayMs(1.5, { baseMs: 10, capMs: 100 })).toThrow();
  });

  it("mutation proof: removing the cap (min()) would let delay grow past capMs at high attempt counts", () => {
    const options = { baseMs: 100, capMs: 500, jitterRatio: 0 };
    const delay = computeBackoffDelayMs(20, options);
    expect(delay).toBe(500); // 真实实现:封顶
    const uncapped = options.baseMs * 2 ** 20; // 去掉 min() 之后会是这个天文数字
    expect(delay).toBeLessThan(uncapped);
  });
});

describe("BackoffTracker", () => {
  it("tracks attempt count across recordFailure() calls and resets on reset()", () => {
    const tracker = new BackoffTracker({ baseMs: 10, capMs: 1000, jitterRatio: 0 });
    expect(tracker.attemptCount).toBe(0);
    expect(tracker.recordFailure()).toBe(10);
    expect(tracker.attemptCount).toBe(1);
    expect(tracker.recordFailure()).toBe(20);
    expect(tracker.attemptCount).toBe(2);
    tracker.reset();
    expect(tracker.attemptCount).toBe(0);
    expect(tracker.recordFailure()).toBe(10);
  });
});
