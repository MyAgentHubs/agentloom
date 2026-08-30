// V2：桌面 chat 过程细节显示级别偏好——本机 localStorage 外部 store。
// 存储语义对齐 `i18n.tsx:101-121` 的既有样板：先更新内存快照并同步通知订阅者，
// 再 best-effort 写 localStorage（失败静默吞掉，不影响本次切档立即生效）。
// V1 会在 `lib/streamItems.ts` 另行导出 `Verbosity` 类型（渲染折算用）；本文件
// 独立声明 `ChatVerbosity`，避免并行改动冲突——两者语义相同，V3b 合并时统一。

import { useSyncExternalStore } from "react";

export type ChatVerbosity = "full" | "summary" | "minimal";

export const STORAGE_KEY = "agentloom.chatVerbosity.v1";
export const DEFAULT_VERBOSITY: ChatVerbosity = "summary";

export function isChatVerbosity(v: unknown): v is ChatVerbosity {
  return v === "full" || v === "summary" || v === "minimal";
}

function hasLocalStorage(): boolean {
  return typeof localStorage !== "undefined";
}

function readInitial(): ChatVerbosity {
  if (!hasLocalStorage()) return DEFAULT_VERBOSITY;
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    return isChatVerbosity(stored) ? stored : DEFAULT_VERBOSITY;
  } catch {
    // localStorage can be unavailable in tests or hardened webviews.
    return DEFAULT_VERBOSITY;
  }
}

let current: ChatVerbosity = readInitial();
const listeners = new Set<() => void>();

export function getChatVerbosity(): ChatVerbosity {
  return current;
}

export function setChatVerbosity(v: ChatVerbosity): void {
  current = v;
  for (const listener of listeners) listener();
  if (!hasLocalStorage()) return;
  try {
    localStorage.setItem(STORAGE_KEY, v);
  } catch {
    // Keep verbosity switching functional even when persistence is unavailable.
  }
}

export function subscribeChatVerbosity(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** 仅供测试使用：重置模块级内存态，不触碰 localStorage（调用方按需自行清理）。 */
export function __resetForTests(): void {
  current = readInitial();
  listeners.clear();
}

export function useChatVerbosity(): [
  ChatVerbosity,
  (v: ChatVerbosity) => void,
] {
  const value = useSyncExternalStore(
    subscribeChatVerbosity,
    getChatVerbosity,
    () => DEFAULT_VERBOSITY,
  );
  return [value, setChatVerbosity];
}
