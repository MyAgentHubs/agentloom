// cachePreference.test.tsx — TDD 覆盖 src/ui/settings/cachePreference.ts（msgfix2 U4）。
//
// `.test.tsx` 后缀是必需的（同 `verbosePreference.test.tsx` 头注——命中 jsdom project）；`window.
// localStorage` 的 Node 26 escape hatch 同该文件同一份手法，见其头注全文,这里不重复。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { loadCacheEnabledPreference, saveCacheEnabledPreference } from "./cachePreference.ts";

const STORAGE_KEY = "agentloom.remote-web.settings.bodyCache";

function installRealLocalStorage(): void {
  const dom = (window as unknown as { jsdom?: { window: Window } }).jsdom;
  if (!dom) throw new Error("window.jsdom (vitest jsdom environment handle) not found — has the environment changed?");
  Object.defineProperty(window, "localStorage", { value: dom.window.localStorage, configurable: true, writable: true });
}

beforeEach(() => {
  installRealLocalStorage();
  window.localStorage.clear();
});

afterEach(() => {
  window.localStorage.clear();
  vi.restoreAllMocks();
});

describe("loadCacheEnabledPreference", () => {
  it("defaults to true when nothing has ever been stored（设计稿默认开——同「本设备缓存已加载的长消息全文」的产品判断）", () => {
    expect(loadCacheEnabledPreference()).toBe(true);
  });

  it("returns false after saveCacheEnabledPreference(false)", () => {
    saveCacheEnabledPreference(false);
    expect(loadCacheEnabledPreference()).toBe(false);
  });

  it("returns true after saveCacheEnabledPreference(true), even if it was previously false", () => {
    saveCacheEnabledPreference(false);
    saveCacheEnabledPreference(true);
    expect(loadCacheEnabledPreference()).toBe(true);
  });

  it("any stored value other than the literal '1' reads as false (defensive: not just !== '0')", () => {
    window.localStorage.setItem(STORAGE_KEY, "true");
    expect(loadCacheEnabledPreference()).toBe(false);
    window.localStorage.setItem(STORAGE_KEY, "garbage");
    expect(loadCacheEnabledPreference()).toBe(false);
  });

  it("a getItem() that throws (privacy mode / quota) is swallowed and falls back to true (默认开，不是默认关)", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("SecurityError: localStorage disabled");
    });
    expect(loadCacheEnabledPreference()).toBe(true);
  });
});

describe("saveCacheEnabledPreference", () => {
  it("a setItem() that throws is swallowed silently (does not propagate)", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceededError");
    });
    expect(() => saveCacheEnabledPreference(false)).not.toThrow();
  });
});
