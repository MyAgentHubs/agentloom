export type RelativeTime = { key: string; n: number };

export function relativeTime(fromMs: number, nowMs: number): RelativeTime {
  const diff = Math.max(0, nowMs - fromMs);
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return { key: "time.justNow", n: 0 };
  const min = Math.floor(sec / 60);
  if (min < 60) return { key: "time.minAgo", n: min };
  const hr = Math.floor(min / 60);
  if (hr < 24) return { key: "time.hourAgo", n: hr };
  return { key: "time.dayAgo", n: Math.floor(hr / 24) };
}

export const ENGLISH_MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
] as const;

export function formatRelativeTime(
  epochSeconds: number,
  locale: "zh" | "en",
  nowMs = Date.now(),
): string {
  const elapsedSeconds = Math.max(
    0,
    Math.floor((nowMs - epochSeconds * 1000) / 1000),
  );

  if (elapsedSeconds < 60) return locale === "zh" ? "刚刚" : "now";
  if (elapsedSeconds < 60 * 60) {
    const minutes = Math.floor(elapsedSeconds / 60);
    return locale === "zh" ? `${minutes} 分钟` : `${minutes}m`;
  }
  if (elapsedSeconds < 24 * 60 * 60) {
    const hours = Math.floor(elapsedSeconds / (60 * 60));
    return locale === "zh" ? `${hours} 小时` : `${hours}h`;
  }
  if (elapsedSeconds < 7 * 24 * 60 * 60) {
    const days = Math.floor(elapsedSeconds / (24 * 60 * 60));
    return locale === "zh" ? `${days} 天` : `${days}d`;
  }

  const createdAt = new Date(epochSeconds * 1000);
  const month = createdAt.getMonth();
  const day = createdAt.getDate();
  return locale === "zh"
    ? `${month + 1}月${day}日`
    : `${ENGLISH_MONTHS[month]} ${day}`;
}

/**
 * G3-B Overview「最近活动」：把后端已按本地日历日分好桶的 "YYYY-MM-DD" 字符串
 * 转成人话标签（今天 / 昨天 / N月D日）。
 *
 * 注意：故意手拆 y/m/d 再用 `new Date(y, m-1, d)` 构造——不能直接
 * `new Date(isoDate)`，那会按 UTC 解析 "YYYY-MM-DD"，本地时区一转换
 * 就可能整体错位一天（尤其 UTC 负偏移地区）。后端已经用调用方传入的
 * tz_offset_minutes 把日期分到正确的本地日，前端只管原样显示，不再二次转时区。
 */
export function formatLocalDayLabel(
  isoDate: string,
  locale: "zh" | "en",
  now = new Date(),
): string {
  const [y, m, d] = isoDate.split("-").map(Number);
  if (!y || !m || !d) return isoDate;
  const target = new Date(y, m - 1, d);
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const diffDays = Math.round(
    (today.getTime() - target.getTime()) / (24 * 60 * 60 * 1000),
  );
  if (diffDays === 0) return locale === "zh" ? "今天" : "Today";
  if (diffDays === 1) return locale === "zh" ? "昨天" : "Yesterday";
  return locale === "zh"
    ? `${target.getMonth() + 1}月${target.getDate()}日`
    : `${ENGLISH_MONTHS[target.getMonth()]} ${target.getDate()}`;
}
