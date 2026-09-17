import type { Locale } from "../i18n";

export const sessionLifecycleCopy = {
  zh: {
    nav: "通用",
    title: "通用",
    subtitle: "管理会话的自动归档和永久清理策略。",
    groupLabel: "会话生命周期",
    archiveLabel: "自动归档未活动会话",
    archiveDesc: "会话连续未活动达到此天数后自动归档。",
    purgeLabel: "永久删除已归档会话",
    purgeDesc: "会话归档达到此天数后永久删除并清理其持久化资源。此操作不可恢复。",
    days: "天",
    minHint: "最少 1 天",
  },
  en: {
    nav: "General",
    title: "General",
    subtitle: "Manage automatic session archiving and permanent cleanup.",
    groupLabel: "Session lifecycle",
    archiveLabel: "Auto-archive inactive sessions",
    archiveDesc: "Archive sessions after they have been inactive for this many days.",
    purgeLabel: "Permanently delete archived sessions",
    purgeDesc: "Permanently delete archived sessions and clean up their persistent resources after this many days. This cannot be undone.",
    days: "days",
    minHint: "Minimum 1 day",
  },
} satisfies Record<Locale, Record<string, string>>;

export function getSessionLifecycleCopy(locale: Locale) {
  return sessionLifecycleCopy[locale];
}
