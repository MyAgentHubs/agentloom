// upgradeClassifier.test.ts — 分类判定表逐格测(任务书 §4④ 要求)+ 变异自证。
//
// Narrowed the close-code classification table from 6 rules to 4 (dropped the lastRefreshOutcome=fail_invalid rule,
// 依据=lib.rs:901-929 桌面内部故障也发 invalid,close:false——不是认证信号);49h 阈值改名
// `refreshUntilWindowMs`、默认值改 30 天;两处等值边界改 `>=`(与 relay §9.1 严格 `<` 对齐)。

import { describe, expect, it } from "vitest";
import {
  classifyCloseReason,
  classifyUpgradeFailure,
  DEFAULT_ACCESS_LIFETIME_MS,
  DEFAULT_REFRESH_UNTIL_WINDOW_MS,
} from "./upgradeClassifier.ts";

const NOW = 10_000_000;

describe("classifyUpgradeFailure() — 判定表逐格(4 条规则)", () => {
  it("row 1: accessIssuedAtMs=null → needs_repair (defensive — should never happen post-pairing)", () => {
    expect(
      classifyUpgradeFailure({ nowMs: NOW, accessIssuedAtMs: null, connectionPhase: "first_connect" }),
    ).toBe("needs_repair");
  });

  it("row 2: accessIssuedAtMs in the future (clock skew) → retry_backoff, not a stronger conclusion", () => {
    expect(
      classifyUpgradeFailure({ nowMs: NOW, accessIssuedAtMs: NOW + 5_000, connectionPhase: "reconnect" }),
    ).toBe("retry_backoff");
  });

  it("row 3: elapsed >= refreshUntilWindowMs → needs_repair (current alias's refresh scope window is over)", () => {
    expect(
      classifyUpgradeFailure({
        nowMs: NOW,
        accessIssuedAtMs: NOW - DEFAULT_REFRESH_UNTIL_WINDOW_MS,
        connectionPhase: "reconnect",
      }),
    ).toBe("needs_repair");
  });

  it("row 3 boundary: elapsed exactly at refreshUntilWindowMs is needs_repair (relay's `now < valid_until` excludes equality)", () => {
    expect(
      classifyUpgradeFailure({
        nowMs: NOW,
        accessIssuedAtMs: NOW - DEFAULT_REFRESH_UNTIL_WINDOW_MS,
        connectionPhase: "reconnect",
      }),
    ).toBe("needs_repair");
    // 差一毫秒仍未到边界 — 应该还是 needs_refresh(row 4),不是 needs_repair。
    expect(
      classifyUpgradeFailure({
        nowMs: NOW,
        accessIssuedAtMs: NOW - (DEFAULT_REFRESH_UNTIL_WINDOW_MS - 1),
        connectionPhase: "reconnect",
      }),
    ).toBe("needs_refresh");
  });

  it("row 4: accessLifetimeMs <= elapsed < refreshUntilWindowMs → needs_refresh (still within the 30-day current refresh window)", () => {
    expect(
      classifyUpgradeFailure({
        nowMs: NOW,
        accessIssuedAtMs: NOW - (DEFAULT_ACCESS_LIFETIME_MS + 1),
        connectionPhase: "reconnect",
      }),
    ).toBe("needs_refresh");
  });

  it("row 4 boundary: elapsed exactly at accessLifetimeMs is needs_refresh (relay's `now < access_expires` excludes equality — access 恰到期=needs_refresh 不是 retry_backoff)", () => {
    expect(
      classifyUpgradeFailure({
        nowMs: NOW,
        accessIssuedAtMs: NOW - DEFAULT_ACCESS_LIFETIME_MS,
        connectionPhase: "reconnect",
      }),
    ).toBe("needs_refresh");
    // 差一毫秒仍未到边界 — 应该还是 retry_backoff(row 5)。
    expect(
      classifyUpgradeFailure({
        nowMs: NOW,
        accessIssuedAtMs: NOW - (DEFAULT_ACCESS_LIFETIME_MS - 1),
        connectionPhase: "reconnect",
      }),
    ).toBe("retry_backoff");
  });

  it("row 5: elapsed < accessLifetimeMs → retry_backoff (no reason to suspect the credential itself)", () => {
    expect(
      classifyUpgradeFailure({ nowMs: NOW, accessIssuedAtMs: NOW - 5_000, connectionPhase: "first_connect" }),
    ).toBe("retry_backoff");
  });

  it("custom accessLifetimeMs/refreshUntilWindowMs override the defaults", () => {
    expect(
      classifyUpgradeFailure({
        nowMs: NOW,
        accessIssuedAtMs: NOW - 2_000,
        accessLifetimeMs: 1_000,
        refreshUntilWindowMs: 1_500,
        connectionPhase: "reconnect",
      }),
    ).toBe("needs_repair"); // 2000 >= 1500
  });

  it("does not treat a plain refresh failure history specially — the function no longer accepts/consumes lastRefreshOutcome at all (type-level: field removed from UpgradeFailureContext)", () => {
    // 传入的 context 对象里根本没有 lastRefreshOutcome 字段——这本身就是最强的"不再消费"证明
    // (类型层面已经不存在这个入参)。这里只是复跑一次 row 5,确认没有任何隐藏分支绕过。
    expect(
      classifyUpgradeFailure({ nowMs: NOW, accessIssuedAtMs: NOW - 5_000, connectionPhase: "reconnect" }),
    ).toBe("retry_backoff");
  });
});

describe("classifyCloseReason()", () => {
  it("token_reauthorization_failed → needs_refresh", () => {
    expect(classifyCloseReason("token_reauthorization_failed")).toBe("needs_refresh");
  });

  it("message_rate_limited → retry_backoff (must never be mistaken for an auth failure)", () => {
    expect(classifyCloseReason("message_rate_limited")).toBe("retry_backoff");
  });

  it("unrecognized reasons fall back to 'unclassified' so the caller uses the clock-based table", () => {
    expect(classifyCloseReason("")).toBe("unclassified");
    expect(classifyCloseReason("sync_timeout")).toBe("unclassified");
    expect(classifyCloseReason("subject_socket_limit")).toBe("unclassified");
  });
});

describe("变异自证", () => {
  it("mutation proof: reverting the row-3 boundary from >= to > would let a token exactly at the 30-day refresh_until edge wrongly stay classified as needs_refresh (relay would already reject it — `now < valid_until` fails at equality)", () => {
    const context = {
      nowMs: NOW,
      accessIssuedAtMs: NOW - DEFAULT_REFRESH_UNTIL_WINDOW_MS,
      connectionPhase: "reconnect" as const,
    };
    expect(classifyUpgradeFailure(context)).toBe("needs_repair"); // 真实实现(>=)
    const elapsed = context.nowMs - context.accessIssuedAtMs;
    const mutatedWithStrictGreaterThan = elapsed > DEFAULT_REFRESH_UNTIL_WINDOW_MS ? "needs_repair" : "needs_refresh";
    expect(mutatedWithStrictGreaterThan).toBe("needs_refresh"); // 变异版本(>)会在边界上判错
    expect(mutatedWithStrictGreaterThan).not.toBe(classifyUpgradeFailure(context));
  });

  it("mutation proof: reverting the row-4 boundary from >= to > would let a token exactly at access_expires wrongly stay classified as retry_backoff (relay's scope=remote already excludes this instant)", () => {
    const context = { nowMs: NOW, accessIssuedAtMs: NOW - DEFAULT_ACCESS_LIFETIME_MS, connectionPhase: "reconnect" as const };
    expect(classifyUpgradeFailure(context)).toBe("needs_refresh"); // 真实实现(>=)
    const elapsed = context.nowMs - context.accessIssuedAtMs;
    const mutatedWithStrictGreaterThan = elapsed > DEFAULT_ACCESS_LIFETIME_MS ? "needs_refresh" : "retry_backoff";
    expect(mutatedWithStrictGreaterThan).toBe("retry_backoff"); // 变异版本(>)会在边界上判错
    expect(mutatedWithStrictGreaterThan).not.toBe(classifyUpgradeFailure(context));
  });

  it("mutation proof: message_rate_limited must never classify as needs_refresh (swapping its classification would make a rate-limited client wrongly distrust its own valid credential)", () => {
    expect(classifyCloseReason("message_rate_limited")).toBe("retry_backoff"); // 真实实现
    expect(classifyCloseReason("message_rate_limited")).not.toBe("needs_refresh"); // 被禁止的错误分类
  });
});
