import { test } from "node:test";
import assert from "node:assert/strict";
import { currentPeriod, evaluateQuota, shouldDegrade, DEFAULT_MONTHLY_MILESTONE_LIMIT } from "../src/quota.js";

test("currentPeriod 格式为 UTC 年-月", () => {
  const ts = Date.UTC(2026, 7, 11, 3, 4, 5); // 月份 0-based：7 = 8 月
  assert.equal(currentPeriod(ts), "2026-08");
});

test("currentPeriod 月份补零", () => {
  const ts = Date.UTC(2026, 0, 1); // 1 月
  assert.equal(currentPeriod(ts), "2026-01");
});

test("evaluateQuota：未超限", () => {
  const q = evaluateQuota({ count: 10, limit: 100 });
  assert.equal(q.allowed, true);
  assert.equal(q.exceeded, false);
  assert.equal(q.remaining, 90);
});

test("evaluateQuota：恰好用完（count === limit）算超限", () => {
  const q = evaluateQuota({ count: 100, limit: 100 });
  assert.equal(q.allowed, false);
  assert.equal(q.exceeded, true);
  assert.equal(q.remaining, 0);
});

test("evaluateQuota：超过上限", () => {
  const q = evaluateQuota({ count: 150, limit: 100 });
  assert.equal(q.exceeded, true);
  assert.equal(q.remaining, 0); // 不出现负数
});

test("DEFAULT_MONTHLY_MILESTONE_LIMIT 是正数占位值", () => {
  assert.ok(DEFAULT_MONTHLY_MILESTONE_LIMIT > 0);
});

test("shouldDegrade：control 永远放行，即使超限", () => {
  const exceeded = evaluateQuota({ count: 999, limit: 100 });
  assert.equal(shouldDegrade("control", exceeded), false);
});

test("shouldDegrade：live 未超限时放行", () => {
  const ok = evaluateQuota({ count: 1, limit: 100 });
  assert.equal(shouldDegrade("live", ok), false);
});

test("shouldDegrade：live 超限时降级（H3：断 live）", () => {
  const exceeded = evaluateQuota({ count: 100, limit: 100 });
  assert.equal(shouldDegrade("live", exceeded), true);
});

test("shouldDegrade：event（里程碑）永不降级，即使超限（H3：保里程碑，母文档 §6b）", () => {
  const exceeded = evaluateQuota({ count: 999, limit: 100 });
  assert.equal(shouldDegrade("event", exceeded), false);
});

test("shouldDegrade：input/presence 不受配额闸管辖", () => {
  const exceeded = evaluateQuota({ count: 999, limit: 100 });
  assert.equal(shouldDegrade("input", exceeded), false);
  assert.equal(shouldDegrade("presence", exceeded), false);
});
