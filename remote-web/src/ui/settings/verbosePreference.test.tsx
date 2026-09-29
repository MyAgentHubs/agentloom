// verbosePreference.test.tsx — TDD 覆盖 src/ui/settings/verbosePreference.ts（msgfix2 U3）。
//
// **`.test.tsx` 后缀（不是 `.ts`）是必需的，不是随手写错**：`vitest.config.ts` 按文件后缀分流
// project——`src/**/*.test.ts`（不分目录）恒落 "logic" project 的 node 环境（没有 `window`/
// `localStorage`），只有 `src/ui/**/*.test.tsx`/`src/app/**/*.test.tsx` 落 "ui" project 的 jsdom
// 环境。本文件测的是真实 `window.localStorage` 读写行为，必须在 jsdom 下跑；文件本身不含任何 JSX，
// `.tsx` 只是为了命中正确的 vitest project，不是这份测试真的需要渲染组件。
//
// **`window.localStorage` escape hatch（本仓踩出的 Node 26 + 这版 vitest 环境坑，跟本单业务逻辑
// 无关）**：这台环境的 Node（26.x）自带一个原生 `globalThis.localStorage` getter（未传
// `--localstorage-file` 时恒解出 `undefined`，见启动时的 `ExperimentalWarning: localStorage is
// not available ...`）；vitest 的 jsdom 环境搭建（`populateGlobal()`）只在某个 key **不在** Node
// 全局上时才会用 jsdom 真实实现覆盖它——`localStorage` 撞了这条例外，于是 jsdom 测试里
// `window.localStorage` 实际读到的是 Node 那个恒 `undefined` 的原生 getter，不是 jsdom 的真实
// `Storage` 实例（哪怕环境是 jsdom、`window.location.href` 也正常是 `http://localhost:3000/`）。
// **不是本文件业务逻辑的问题**：生产代码 `verbosePreference.ts` 本来就把每次存取包在 try/catch
// 里——在这个坏境（`window.localStorage` 实际是 `undefined`）下访问 `.getItem`/`.setItem` 会抛
// `TypeError`，一样被吞掉、优雅降级，不会崩；真实浏览器/生产环境不会有这个 Node 全局冲突。这里
// 只是测试需要一个**真的能读写**的 `localStorage` 才能验证"读写确实往返正确"这条主张，所以在每个
// 测试前把 jsdom 内部真正持有的那份实例（`window.jsdom` 是 vitest jsdom 环境自己挂的 `JSDOM`
// 实例句柄，`.window.localStorage` 是它真正的 `Storage` 对象）显式覆盖回 `window.localStorage`
// 这个属性（Node 原生 getter 描述符是 `configurable:true`，可以覆盖，已验证）。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { loadVerbosePreference, saveVerbosePreference } from "./verbosePreference.ts";

const STORAGE_KEY = "agentloom.remote-web.settings.verbose";

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

describe("loadVerbosePreference", () => {
  it("defaults to false when nothing has ever been stored (读不到默认关)", () => {
    expect(loadVerbosePreference()).toBe(false);
  });

  it("returns true after saveVerbosePreference(true)", () => {
    saveVerbosePreference(true);
    expect(loadVerbosePreference()).toBe(true);
  });

  it("returns false after saveVerbosePreference(false), even if it was previously true", () => {
    saveVerbosePreference(true);
    saveVerbosePreference(false);
    expect(loadVerbosePreference()).toBe(false);
  });

  it("any stored value other than the literal '1' reads as false (defensive: not just !== '0')", () => {
    window.localStorage.setItem(STORAGE_KEY, "true");
    expect(loadVerbosePreference()).toBe(false);
    window.localStorage.setItem(STORAGE_KEY, "garbage");
    expect(loadVerbosePreference()).toBe(false);
  });

  it("a getItem() that throws (privacy mode / quota) is swallowed and falls back to false", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("SecurityError: localStorage disabled");
    });
    expect(loadVerbosePreference()).toBe(false);
  });
});

describe("saveVerbosePreference", () => {
  it("a setItem() that throws is swallowed silently (does not propagate)", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceededError");
    });
    expect(() => saveVerbosePreference(true)).not.toThrow();
  });
});
