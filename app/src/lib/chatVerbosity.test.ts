import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_VERBOSITY,
  STORAGE_KEY,
  getChatVerbosity,
  isChatVerbosity,
  setChatVerbosity,
  subscribeChatVerbosity,
} from "./chatVerbosity";

// 模块级 store 跨测试共享内存态——每条测试前显式清 localStorage +
// 动态 re-import 拿一份新鲜的模块实例，避免测试间互相污染 `current`/`listeners`。
async function freshModule() {
  vi.resetModules();
  return import("./chatVerbosity");
}

describe("chatVerbosity", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("isChatVerbosity 只认三个合法值", () => {
    expect(isChatVerbosity("full")).toBe(true);
    expect(isChatVerbosity("summary")).toBe(true);
    expect(isChatVerbosity("minimal")).toBe(true);
    expect(isChatVerbosity("verbose")).toBe(false);
    expect(isChatVerbosity(undefined)).toBe(false);
    expect(isChatVerbosity(null)).toBe(false);
    expect(isChatVerbosity(1)).toBe(false);
  });

  it("非法值/缺值时模块加载读出默认档 summary", async () => {
    localStorage.setItem(STORAGE_KEY, "not-a-real-level");
    const mod = await freshModule();
    expect(mod.getChatVerbosity()).toBe("summary");
    expect(mod.DEFAULT_VERBOSITY).toBe("summary");

    localStorage.removeItem(STORAGE_KEY);
    const mod2 = await freshModule();
    expect(mod2.getChatVerbosity()).toBe(DEFAULT_VERBOSITY);
  });

  it("localStorage 里已有合法值时模块加载读出该值", async () => {
    localStorage.setItem(STORAGE_KEY, "minimal");
    const mod = await freshModule();
    expect(mod.getChatVerbosity()).toBe("minimal");
  });

  it("setChatVerbosity 后 getChatVerbosity 立即反映新值，且落盘 localStorage", () => {
    setChatVerbosity("full");
    expect(getChatVerbosity()).toBe("full");
    expect(localStorage.getItem(STORAGE_KEY)).toBe("full");
  });

  it("localStorage.setItem 抛错时 set 仍即时生效、订阅者仍被通知、不抛", () => {
    const fn = vi.fn();
    const unsubscribe = subscribeChatVerbosity(fn);
    vi.spyOn(globalThis.localStorage, "setItem").mockImplementation(() => {
      throw new Error("quota exceeded");
    });

    expect(() => setChatVerbosity("minimal")).not.toThrow();
    expect(getChatVerbosity()).toBe("minimal");
    expect(fn).toHaveBeenCalledTimes(1);
    unsubscribe();
  });

  it("同窗口多订阅者同步收到通知", () => {
    const a = vi.fn();
    const b = vi.fn();
    const unsubA = subscribeChatVerbosity(a);
    const unsubB = subscribeChatVerbosity(b);

    setChatVerbosity("summary");

    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
    unsubA();
    unsubB();
  });

  it("退订后不再收到通知", () => {
    const fn = vi.fn();
    const unsubscribe = subscribeChatVerbosity(fn);
    setChatVerbosity("full");
    expect(fn).toHaveBeenCalledTimes(1);

    unsubscribe();
    setChatVerbosity("minimal");
    expect(fn).toHaveBeenCalledTimes(1);
  });
});
