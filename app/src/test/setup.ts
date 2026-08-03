import "@testing-library/jest-dom";
import { beforeAll } from "vitest";
import { preloadMarkdown } from "../lib/useMarkdown";

beforeAll(async () => {
  await preloadMarkdown();
});

// jsdom 在本仓 vitest 环境下未配 url → window.localStorage 不可用（Web Storage 需非 opaque origin）。
// 提供内存实现，供模型列表缓存等依赖 localStorage 的测试使用。
let hasLocalStorage = false;
try {
  hasLocalStorage =
    typeof globalThis.localStorage !== "undefined" &&
    globalThis.localStorage !== null;
} catch {
  hasLocalStorage = false;
}

if (!hasLocalStorage) {
  class MemoryStorage implements Storage {
    private store = new Map<string, string>();
    get length(): number {
      return this.store.size;
    }
    clear(): void {
      this.store.clear();
    }
    getItem(key: string): string | null {
      return this.store.has(key) ? (this.store.get(key) as string) : null;
    }
    key(index: number): string | null {
      return Array.from(this.store.keys())[index] ?? null;
    }
    removeItem(key: string): void {
      this.store.delete(key);
    }
    setItem(key: string, value: string): void {
      this.store.set(key, String(value));
    }
  }
  Object.defineProperty(globalThis, "localStorage", {
    value: new MemoryStorage(),
    configurable: true,
  });
}
