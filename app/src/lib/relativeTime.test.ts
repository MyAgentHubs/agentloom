import { describe, expect, it } from "vitest";
import {
  formatLocalDayLabel,
  formatRelativeTime,
  relativeTime,
} from "./relativeTime";

describe("relativeTime", () => {
  const from = 1_000_000_000_000;

  it("returns just now within 60 seconds", () => {
    expect(relativeTime(from, from + 30_000)).toEqual({
      key: "time.justNow",
      n: 0,
    });
  });

  it("returns minutes ago", () => {
    expect(relativeTime(from, from + 5 * 60 * 1000)).toEqual({
      key: "time.minAgo",
      n: 5,
    });
  });

  it("returns one minute at exactly 60 seconds", () => {
    expect(relativeTime(from, from + 60_000)).toEqual({
      key: "time.minAgo",
      n: 1,
    });
  });

  it("returns hours ago", () => {
    expect(relativeTime(from, from + 3 * 60 * 60 * 1000)).toEqual({
      key: "time.hourAgo",
      n: 3,
    });
  });

  it("returns days ago", () => {
    expect(relativeTime(from, from + 2 * 24 * 60 * 60 * 1000)).toEqual({
      key: "time.dayAgo",
      n: 2,
    });
  });

  it("clamps future timestamps to just now", () => {
    expect(relativeTime(from, from - 1000)).toEqual({
      key: "time.justNow",
      n: 0,
    });
  });
});

const NOW_MS = new Date(2026, 6, 18, 12, 0, 0).getTime();
const nowSeconds = NOW_MS / 1000;

describe("formatRelativeTime", () => {
  it.each([
    ["zh", 0, "刚刚"],
    ["zh", 59, "刚刚"],
    ["zh", 60, "1 分钟"],
    ["zh", 59 * 60 + 59, "59 分钟"],
    ["zh", 60 * 60, "1 小时"],
    ["zh", 23 * 60 * 60 + 59 * 60 + 59, "23 小时"],
    ["zh", 24 * 60 * 60, "1 天"],
    ["zh", 7 * 24 * 60 * 60 - 1, "6 天"],
    ["zh", 7 * 24 * 60 * 60, "7月11日"],
    ["en", 0, "now"],
    ["en", 59, "now"],
    ["en", 60, "1m"],
    ["en", 59 * 60 + 59, "59m"],
    ["en", 60 * 60, "1h"],
    ["en", 23 * 60 * 60 + 59 * 60 + 59, "23h"],
    ["en", 24 * 60 * 60, "1d"],
    ["en", 7 * 24 * 60 * 60 - 1, "6d"],
    ["en", 7 * 24 * 60 * 60, "Jul 11"],
  ] as const)("%s locale · %i 秒前 → %s", (locale, ageSeconds, expected) => {
    expect(formatRelativeTime(nowSeconds - ageSeconds, locale, NOW_MS)).toBe(
      expected,
    );
  });

  it("未来时间按刚刚处理", () => {
    expect(formatRelativeTime(nowSeconds + 60, "zh", NOW_MS)).toBe("刚刚");
    expect(formatRelativeTime(nowSeconds + 60, "en", NOW_MS)).toBe("now");
  });

  it("满 7 天后按 locale 显示创建日期", () => {
    const createdAt = new Date(2026, 5, 9, 8, 30, 0).getTime() / 1000;
    expect(formatRelativeTime(createdAt, "zh", NOW_MS)).toBe("6月9日");
    expect(formatRelativeTime(createdAt, "en", NOW_MS)).toBe("Jun 9");
  });
});

describe("formatLocalDayLabel", () => {
  const now = new Date(2026, 6, 18, 23, 0, 0); // 2026-07-18 23:00 本地时间

  it("今天 / 昨天用人话，不用日期数字", () => {
    expect(formatLocalDayLabel("2026-07-18", "zh", now)).toBe("今天");
    expect(formatLocalDayLabel("2026-07-18", "en", now)).toBe("Today");
    expect(formatLocalDayLabel("2026-07-17", "zh", now)).toBe("昨天");
    expect(formatLocalDayLabel("2026-07-17", "en", now)).toBe("Yesterday");
  });

  it("更早的日期按 locale 格式化为 N月D日 / Mon D", () => {
    expect(formatLocalDayLabel("2026-07-11", "zh", now)).toBe("7月11日");
    expect(formatLocalDayLabel("2026-07-11", "en", now)).toBe("Jul 11");
  });

  it("不二次按 UTC 解析 · 跨月边界不错位", () => {
    expect(formatLocalDayLabel("2026-06-30", "zh", now)).toBe("6月30日");
    expect(formatLocalDayLabel("2026-06-30", "en", now)).toBe("Jun 30");
  });
});
