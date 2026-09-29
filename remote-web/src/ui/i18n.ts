// i18n.ts — T6f1 · 配对屏薄 i18n hook（T6f2 追加 `stream.*` 前缀：会话流屏自己的壳层文案——
// 「正在输入」指示 / 空态提示 / 运行状态标签，这些是 remote-web 自己的 UI，不在桌面 messages
// 里；桌面叶子组件内部文案仍然经 `@app/i18n` 的 `useI18n()`/messages 真相源，两套 i18n 各管各的
// 命名空间，互不覆盖——见文件下方 t() 的"缺 key 用 key 本身兜底"授权路径）。T6d2 追加
// `sessions.*` 前缀：会话列表屏（空态文案 + 状态点的 aria-label——列表行本身不堆文字，色点即状态，
// aria-label 只服务屏幕阅读器，同 stream.* 的"每屏一个 key 命名空间"既有惯例）。
//
// 设计取舍（如实记录·worker 报告 ⑥ 偏离说明·FIX2 顺手订正）：M2 C1 spec §6 设想"i18n 按前缀裁
// web 子集，经 @app alias 引桌面 messages 对象裁 pairing 相关前缀，共用同一份 messages.zh 作 key
// 真相源防漂移"。**本表落地时（T6f1）桌面 `messages` 还是 `app/src/i18n.tsx` 里的私有 const，没有
// export，字面字符串表拿不到**——那条理由目前已经过时：桌面已经把它抽成零依赖的独立模块
// `app/src/i18n.tsx::messages`（实际定义在 `app/src/i18nMessages.ts`，T6E2），本可共享消费，
// **但本表迁移留给后续单**（不是本单顺手改动范围——迁移涉及把下面这张表的 key 整体搬迁、核对
// 两边字面字符串逐字一致，是一件独立的活，不是这行注释订正顺带能做完的）。
//
// 按任务书"不 fork 文案字符串·缺 key 用 key 本身兜底"授权的降级路径：本文件新开一个 `pairing.*`
// 前缀的小表——跟桌面 `settings.remoteControl.*`/`decisionCard.*` 这类"每屏一个 key 命名空间"的
// 既有惯例一致（桌面自己也不共享"重试"这种通用 key，每屏各自一份），不是新发明规则；文案是原创
// 中英文本，不是复制桌面已有字符串（配对屏这套语义桌面目前也没有对应文案可复制）。等后续那一单
// 把这张表的内容按 `pairing.*` 前缀整体搬进桌面 `messages` 时，调用方签名（`t(key, values)`）
// 不用变。
//
// `t()` 对不存在的 key 落回 key 本身（不是防御式代码，是这批还没挪进桌面 messages 前的真实运行
// 状态——见任务书 §2「缺 key 用 key 本身兜底」）。

export type Locale = "zh" | "en";

const PAIRING_MESSAGES = {
  zh: {
    "pairing.manualEntry.heading": "手动粘贴配对串",
    "pairing.manualEntry.hint": "没有扫码入口？把桌面「设置 → 远程控制」里的配对串粘贴到下面。",
    "pairing.manualEntry.submit": "连接",
    "pairing.manualEntry.error.format": "配对信息格式不正确，请确认完整粘贴。",
    "pairing.manualEntry.error.originMismatch": "配对信息的服务器地址与当前页面不符，请重新扫码获取。",
    "pairing.progress.idle": "准备中…",
    "pairing.progress.awaitingAccept": "正在与电脑握手…",
    "pairing.progress.awaitingReady": "电脑正在确认…",
    "pairing.progress.retryingDesktopOffline": "桌面 App 当前不在线，正在重试（{attempt}/{maxRetries}）…",
    "pairing.progress.retryingConnection": "配对连接中断，正在重试（{attempt}/{maxRetries}）…",
    "pairing.paired.heading": "已连接",
    "pairing.paired.hint": "会话列表施工中。",
    "pairing.paired.deviceId": "设备 ID：{deviceId}",
    "pairing.error.qr.formatHeading": "配对信息无法识别",
    "pairing.error.qr.formatHint":
      "二维码内容不完整或格式不对，请回到电脑重新生成二维码，或手动粘贴配对串。",
    "pairing.error.qr.originHeading": "配对信息来源不符",
    "pairing.error.qr.originHint":
      "这个二维码指向的服务器地址与当前页面不一致，出于安全考虑已拒绝。请回到电脑重新生成二维码。",
    "pairing.error.needsRepair.heading": "需要重新配对",
    "pairing.error.needsRepair.hint": "这台手机的访问权限已被电脑撤销。请回到电脑重新扫码配对。",
    "pairing.error.needsRepair.hintWithReason":
      "这台手机的访问权限已被电脑撤销（{reason}）。请回到电脑重新扫码配对。",
    "pairing.error.desktopOffline.heading": "桌面 App 不在线",
    "pairing.error.desktopOffline.hint":
      "桌面 App 当前不在线。请确认桌面端已打开并保持前台，然后重新生成配对码。",
    "pairing.error.connection.heading": "无法建立配对连接",
    "pairing.error.connection.hint": "配对连接在重试 {count} 次后仍然失败。请检查网络，然后重新生成配对码。",
    "stream.empty": "还没有消息。",
    "stream.typing": "正在输入…",
    "stream.statusRunning": "运行中",
    "stream.statusIdle": "空闲",
    "stream.restrictedHint": "请回桌面处理",
    // msgfix2 F2 S1：消息级 ErrorBoundary 兜底文案（`../ErrorBoundary.tsx`）——一条消息渲染崩溃
    // 时的降级提示，不是"全部消息都不可用"，只是这一条。
    "stream.messageRenderError": "该消息渲染失败",
    "stream.backToSessions": "返回会话列表",
    "stream.msgFetch.load": "加载全文（{size}）",
    "stream.msgFetch.loading": "加载中…",
    "stream.msgFetch.retry": "重试",
    "stream.msgFetch.unavailable": "全文不可用",
    "stream.msgFetch.staleRevision": "内容已更新，请重新加载",
    "stream.activitySummary.collapsed": "活动 · {tools} 次工具调用 · {failed} 次失败",
    "stream.activitySummary.stateRunning": "进行中",
    "stream.activitySummary.stateDone": "已完成",
    "stream.activitySummary.stateFailed": "失败",
    "stream.activitySummary.detailTools": "工具调用 {count} 次",
    "stream.activitySummary.detailMcp": "MCP 调用 {count} 次",
    "stream.activitySummary.detailPermission": "权限请求 {count} 次",
    "stream.activitySummary.detailFailed": "失败 {count} 次",
    "history.loadEarlier": "加载更早",
    "history.loading": "加载中…",
    "history.retry": "重试",
    "history.error.timeout": "加载历史超时，请重试。",
    "history.error.failed": "历史消息加载失败，请重试。",
    "history.error.rateLimited": "请求太频繁，请稍候重试。",
    "history.error.desktopOffline": "桌面当前离线，请稍后重试。",
    "history.error.quota": "本月额度已用完。",
    "history.error.notConnected": "未连接，无法加载历史。",
    "sessions.empty": "暂无会话",
    "sessions.currentProject": "当前项目：{name}",
    "sessions.statusRunning": "运行中",
    "sessions.statusIdle": "空闲",
    "sessions.settings": "设置",
    "connection.disconnected": "未连接",
    "connection.connecting": "连接中… 已持续 {seconds} 秒",
    "connection.reconnecting": "未连接·重连中… 已持续 {seconds} 秒",
    "connection.longDisconnectionHint": "若持续无法连接，请在电脑上重新扫码配对",
    "connection.needsRepair": "配对已失效，请在电脑上重新生成配对码",
    "connection.messageNotSent": "未连接·消息未送出",
    "connection.desktopMaybeOffline": "电脑似乎不在线，消息会排队等它回来",
    "composer.placeholder": "给桌面发消息…",
    "composer.send": "发送",
    "composer.sending": "发送中…",
    "composer.deliveringMaybeOffline": "投递中，桌面可能离线…",
    "composer.queued": "桌面已接收，正在排队",
    "composer.relayQueued": "已排队，等电脑回来",
    "composer.deliveringUncertain": "投递结果未知，可重试",
    "composer.failed": "发送失败",
    "composer.failedNoAgent": "这个会话还没选 agent，请先在桌面上打开它选一个",
    "composer.expired": "已过期，请重新发送",
    "composer.rateLimited": "发送太快，请稍候重试",
    "composer.giveUp": "连接不稳定，请重新发送",
    "composer.retry": "重试",
    "composer.stop": "停止",
    "composer.stopConfirm": "确定要停止当前会话吗？",
    "composer.stopConfirmYes": "确定停止",
    "composer.stopConfirmCancel": "取消",
    "composer.stopSending": "发送停止指令…",
    "composer.stopQueued": "桌面已接收停止指令，正在排队",
    "composer.stopFailed": "停止指令发送失败",
    "composer.stopRateLimited": "指令发送太快，请稍候重试",
    "composer.stopMaybeStillValid": "未收到确认，可能仍在生效窗内",
    "settings.back": "返回",
    "settings.title": "设置",
    "settings.verbose.label": "显示详细活动",
    "settings.verbose.hint": "展开活动摘要为按类别的计数明细",
    "settings.cache.label": "本设备缓存已加载的长消息全文",
    "settings.cache.hint": "关闭后不再写入新的缓存，并立即清空本设备已缓存的内容",
    "settings.unpair.label": "解除配对",
    "settings.unpair.hint": "清除本设备保存的配对凭据与已缓存内容，需要重新扫码连接",
  },
  en: {
    "pairing.manualEntry.heading": "Paste pairing string",
    "pairing.manualEntry.hint":
      "No QR code handy? Paste the pairing string from Desktop → Settings → Remote Control below.",
    "pairing.manualEntry.submit": "Connect",
    "pairing.manualEntry.error.format": "Pairing data is malformed — please paste the full string.",
    "pairing.manualEntry.error.originMismatch":
      "The pairing data's server address doesn't match this page. Please rescan.",
    "pairing.progress.idle": "Preparing…",
    "pairing.progress.awaitingAccept": "Shaking hands with the desktop…",
    "pairing.progress.awaitingReady": "Desktop is confirming…",
    "pairing.progress.retryingDesktopOffline": "The Desktop App is offline. Retrying ({attempt}/{maxRetries})…",
    "pairing.progress.retryingConnection": "Pairing connection interrupted. Retrying ({attempt}/{maxRetries})…",
    "pairing.paired.heading": "Connected",
    "pairing.paired.hint": "Session list is under construction.",
    "pairing.paired.deviceId": "Device ID: {deviceId}",
    "pairing.error.qr.formatHeading": "Pairing data not recognized",
    "pairing.error.qr.formatHint":
      "The QR code is incomplete or malformed. Please regenerate it on the desktop, or paste the pairing string manually.",
    "pairing.error.qr.originHeading": "Pairing source mismatch",
    "pairing.error.qr.originHint":
      "This QR code points to a server address that doesn't match this page, and has been rejected for safety. Please regenerate it on the desktop.",
    "pairing.error.needsRepair.heading": "Re-pairing required",
    "pairing.error.needsRepair.hint": "This phone's access has been revoked from the desktop. Please rescan to re-pair.",
    "pairing.error.needsRepair.hintWithReason":
      "This phone's access has been revoked from the desktop ({reason}). Please rescan to re-pair.",
    "pairing.error.desktopOffline.heading": "Desktop App offline",
    "pairing.error.desktopOffline.hint":
      "The Desktop App is currently offline. Make sure it is open and kept in the foreground, then generate a new pairing code.",
    "pairing.error.connection.heading": "Unable to establish pairing connection",
    "pairing.error.connection.hint":
      "The pairing connection still failed after {count} retries. Check your network, then generate a new pairing code.",
    "stream.empty": "No messages yet.",
    "stream.typing": "Typing…",
    "stream.statusRunning": "Running",
    "stream.statusIdle": "Idle",
    "stream.restrictedHint": "Please handle this on the desktop",
    "stream.messageRenderError": "This message failed to render",
    "stream.backToSessions": "Back to session list",
    "stream.msgFetch.load": "Load full text ({size})",
    "stream.msgFetch.loading": "Loading…",
    "stream.msgFetch.retry": "Retry",
    "stream.msgFetch.unavailable": "Full text unavailable",
    "stream.msgFetch.staleRevision": "Content changed — reload",
    "stream.activitySummary.collapsed": "Activity · {tools} tool calls · {failed} failed",
    "stream.activitySummary.stateRunning": "Running",
    "stream.activitySummary.stateDone": "Done",
    "stream.activitySummary.stateFailed": "Failed",
    "stream.activitySummary.detailTools": "{count} tool calls",
    "stream.activitySummary.detailMcp": "{count} MCP calls",
    "stream.activitySummary.detailPermission": "{count} permission prompts",
    "stream.activitySummary.detailFailed": "{count} failed",
    "history.loadEarlier": "Load earlier",
    "history.loading": "Loading…",
    "history.retry": "Retry",
    "history.error.timeout": "History loading timed out. Please retry.",
    "history.error.failed": "History failed to load. Please retry.",
    "history.error.rateLimited": "Too many requests. Please retry shortly.",
    "history.error.desktopOffline": "The desktop is offline. Please retry later.",
    "history.error.quota": "This month's quota has been used up.",
    "history.error.notConnected": "Not connected. Unable to load history.",
    "sessions.empty": "No sessions yet.",
    "sessions.currentProject": "Current project: {name}",
    "sessions.statusRunning": "Running",
    "sessions.statusIdle": "Idle",
    "sessions.settings": "Settings",
    "connection.disconnected": "Not connected",
    "connection.connecting": "Connecting… {seconds}s elapsed",
    "connection.reconnecting": "Not connected · Reconnecting… {seconds}s elapsed",
    "connection.longDisconnectionHint": "If you still cannot connect, generate a new pairing code on the desktop and scan it again",
    "connection.needsRepair": "Pairing has expired. Generate a new pairing code on the desktop",
    "connection.messageNotSent": "Not connected · Message not sent",
    "connection.desktopMaybeOffline": "Desktop may be offline — messages will queue until it's back",
    "composer.placeholder": "Message the desktop…",
    "composer.send": "Send",
    "composer.sending": "Sending…",
    "composer.deliveringMaybeOffline": "Delivering — desktop may be offline…",
    "composer.queued": "Desktop received it, queued",
    "composer.relayQueued": "Queued, waiting for desktop",
    "composer.deliveringUncertain": "Delivery status unknown — retry available",
    "composer.failed": "Send failed",
    "composer.failedNoAgent": "This session has no agent yet. Open it on the desktop and pick one first.",
    "composer.expired": "Expired — please resend",
    "composer.rateLimited": "Sending too fast — please retry shortly",
    "composer.giveUp": "Connection unstable — please resend",
    "composer.retry": "Retry",
    "composer.stop": "Stop",
    "composer.stopConfirm": "Stop the current session?",
    "composer.stopConfirmYes": "Confirm stop",
    "composer.stopConfirmCancel": "Cancel",
    "composer.stopSending": "Sending stop command…",
    "composer.stopQueued": "Desktop received the stop command, queued",
    "composer.stopFailed": "Stop command failed to send",
    "composer.stopRateLimited": "Sending too fast — please retry shortly",
    "composer.stopMaybeStillValid": "No confirmation yet — may still be within the valid window",
    "settings.back": "Back",
    "settings.title": "Settings",
    "settings.verbose.label": "Show detailed activity",
    "settings.verbose.hint": "Expand activity summaries into per-category counts",
    "settings.cache.label": "Cache loaded long messages on this device",
    "settings.cache.hint": "When off, new content stops being cached and anything already cached on this device is cleared immediately",
    "settings.unpair.label": "Unpair this device",
    "settings.unpair.hint": "Clear the pairing credentials and cached content stored on this device — you'll need to scan the QR code again",
  },
} as const;

export type PairingMessageKey = keyof (typeof PAIRING_MESSAGES)["zh"];

function detectLocale(): Locale {
  try {
    const lang = typeof navigator === "undefined" ? undefined : navigator.language || navigator.languages?.[0];
    if (lang?.toLowerCase().startsWith("zh")) return "zh";
  } catch {
    // navigator 在部分测试/hardened 环境不可用——回落默认（同桌面 detectLocale 的取向：系统语言
    // 优先命中 zh，否则 en）。
  }
  return "en";
}

/** 缺 key 用 key 本身兜底（见文件头注）。允许任意字符串是故意的——不是所有 "pairing." 前缀 /
 *  "stream." 前缀的 key 都已在两个 locale 里配齐文案。 */
function translate(
  locale: Locale,
  key: PairingMessageKey | (string & {}),
  values?: Record<string, string>,
): string {
  const table = PAIRING_MESSAGES[locale] as Record<string, string>;
  const zhTable = PAIRING_MESSAGES.zh as Record<string, string>;
  let template = table[key] ?? zhTable[key] ?? key;
  if (values) {
    for (const [name, value] of Object.entries(values)) {
      template = template.split(`{${name}}`).join(value);
    }
  }
  return template;
}

/** 便捷导出——不接受 locale 覆盖，每次调用都重新自动探测（`useI18n()` 的行为基石）。 */
export function t(key: PairingMessageKey | (string & {}), values?: Record<string, string>): string {
  return translate(detectLocale(), key, values);
}

export interface I18nHookValue {
  locale: Locale;
  t: typeof t;
}

/**
 * 薄 hook——MVP 没有运行时切换 locale 的 UI（那是桌面设置页的活），这里只包一层给组件用统一的
 * `useI18n()` 调用形状，方便以后接 context 也不用改调用方。
 *
 * `overrideLocale` pins the requested locale when supplied and preserves automatic locale detection when omitted.
 * 用法不变）。传了就锁定这个 locale，不再自动探测——`SessionStreamScreen` 需要它：该组件同时挂了
 * `@app/i18n` 的 `I18nProvider`（喂 `initialLocale` 给桌面叶子组件的 `useI18n()`）**和**这份
 * remote-web 自己的 `useI18n()`（喂壳层自己的 `stream.*` 文案）——两套 i18n 各自探测 locale 时，
 * jsdom 测试环境下 `navigator.language` 恒为 `en-US`、跟 `I18nProvider` 显式传的 `initialLocale`
 * 对不上，会出现"桌面叶子显示中文、壳层文案显示英文"这种同屏混语言的真实 bug（不只是测试噪音——
 * 生产环境下如果两套探测逻辑因为任何原因给出不同结果，用户会看到同一屏幕夹杂两种语言）。调用方
 * 把同一个 `locale` 值同时传给两套 provider 就能保证一致，见 `SessionStreamScreen.tsx`。
 */
export function useI18n(overrideLocale?: Locale): I18nHookValue {
  const locale = overrideLocale ?? detectLocale();
  return { locale, t: (key, values) => translate(locale, key, values) };
}
