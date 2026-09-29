// verbosePreference.ts — msgfix2 U3 · 「显示详细活动」（verbose）开关的 localStorage 偏好读写。
//
// per-device 偏好（设计稿 v4.1 §4.1："设置项 per-device localStorage"）——不经协议、不落 EventStore，
// 只影响本设备呈现层怎么折叠/展开 L1 活动摘要 chip（`ui/stream/SessionStreamScreen.tsx`）。
//
// **读写 try/catch，读不到默认关**（brief §3b）：隐私模式/存储配额耗尽/未来 iOS fallback 等场景下
// `localStorage` 存取会抛异常——读失败按"默认关"降级（同本仓一贯的"没拿到就当没开过"取向，见
// `ui/i18n.ts::detectLocale` 的同款 try/catch 兜底）；写失败静默吞掉（用户这次切换没有持久化，下次
// 刷新会回到旧值，但不能让一次存储异常打断当前这次交互）。
//
// 调用方（`app/AppRuntime.tsx`）负责把读出的值喂给 `SessionStreamScreen`/`SettingsScreen` 的
// `verboseEnabled` prop——本文件不碰 React state，只做纯粹的存取。

const STORAGE_KEY = "agentloom.remote-web.settings.verbose";

export function loadVerbosePreference(): boolean {
  try {
    return window.localStorage.getItem(STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveVerbosePreference(next: boolean): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, next ? "1" : "0");
  } catch {
    // 静默降级——见文件头注，不能让存储异常打断当前交互。
  }
}
