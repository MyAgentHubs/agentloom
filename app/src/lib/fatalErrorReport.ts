import { invoke } from "@tauri-apps/api/core";

/**
 * 复用现成的 boot_trace 上报通道（app/src-tauri/src/lib.rs 的 `boot_trace` 命令，
 * 只接受 label: string 与 ms: number 两个字段）上报致命错误。
 *
 * 尽力而为：非 Tauri 环境（例如测试）没有可用的 invoke，或通道本身失败，一律吞掉——
 * 上报通道挂了绝不能影响报错页照常显示（这是三层兜底里最不能失败的一环）。
 */
export function reportFatalError(context: string, message: string): void {
  const label = `fatal-error[${context}] ${message}`.slice(0, 500);
  try {
    void invoke("boot_trace", { label, ms: performance.now() }).catch(() => {});
  } catch {
    // 非 Tauri 环境（例如测试）没有可用的 invoke。
  }
}
