import { getCurrentWindow } from "@tauri-apps/api/window";

/** 窗口以 visible:false 创建（消启动白屏）：前端首个 JS 时机把窗口亮出来。
 *  成功（窗口已 show）返回 null；失败返回错误描述字符串——调用方负责上报，
 *  绝不静默吞错。Rust 侧另有 3s 兜底 show（lib.rs setup）。 */
export async function showAppWindow(): Promise<string | null> {
  let w;
  try {
    w = getCurrentWindow();
    await w.show();
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
  try {
    await w.setFocus();
  } catch {
    // best-effort：窗口已显示即算成功，聚焦失败不上报为失败。
  }
  return null;
}
