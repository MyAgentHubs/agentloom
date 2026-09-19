// 桌面与 Web 共享的纯数据入口；各功能域保留字面量类型。
import { messages as zhSettings } from "./i18nMessages/zh/settings";
import { messages as zhWorkspace } from "./i18nMessages/zh/workspace";
import { messages as zhConversation } from "./i18nMessages/zh/conversation";
import { messages as zhCollaboration } from "./i18nMessages/zh/collaboration";
import { messages as zhSessions } from "./i18nMessages/zh/sessions";
import { messages as zhBackend } from "./i18nMessages/zh/backend";
import { messages as zhProjects } from "./i18nMessages/zh/projects";
import { messages as zhUpdater } from "./i18nMessages/zh/updater";
import { messages as enSettings } from "./i18nMessages/en/settings";
import { messages as enWorkspace } from "./i18nMessages/en/workspace";
import { messages as enConversation } from "./i18nMessages/en/conversation";
import { messages as enCollaboration } from "./i18nMessages/en/collaboration";
import { messages as enSessions } from "./i18nMessages/en/sessions";
import { messages as enBackend } from "./i18nMessages/en/backend";
import { messages as enProjects } from "./i18nMessages/en/projects";
import { messages as enUpdater } from "./i18nMessages/en/updater";

// 按原表顺序聚合，保留各语言的 key 枚举顺序。
export const messages = {
  zh: {
    ...zhSettings,
    ...zhWorkspace,
    ...zhConversation,
    ...zhCollaboration,
    ...zhSessions,
    ...zhBackend,
    ...zhProjects,
    ...zhUpdater,
  },
  en: {
    ...enSettings,
    ...enWorkspace,
    ...enConversation,
    ...enCollaboration,
    ...enSessions,
    ...enBackend,
    ...enProjects,
    ...enUpdater,
  },
} as const;
