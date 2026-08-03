import { describe, expect, it, vi } from "vitest";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";

vi.mock("shiki/core", () => ({
  createHighlighterCore: vi.fn(() => Promise.resolve({ ok: true })),
}));

vi.mock("shiki/engine/oniguruma", () => ({
  createOnigurumaEngine: vi.fn(() => Promise.resolve({ engine: "oniguruma" })),
}));

vi.mock("shiki/wasm", () => ({}));

import { createHighlighterCore } from "shiki/core";
import { CODE_THEME, getHighlighter, normalizeLang } from "./highlighter";

describe("highlighter singleton", () => {
  it("归一化首批语言别名，未知语言 fallback text", () => {
    expect(normalizeLang("ts")).toBe("typescript");
    expect(normalizeLang("tsx")).toBe("tsx");
    expect(normalizeLang("js")).toBe("javascript");
    expect(normalizeLang("shell")).toBe("bash");
    expect(normalizeLang("rs")).toBe("rust");
    expect(normalizeLang("unknown")).toBe("text");
    expect(normalizeLang()).toBe("text");
  });

  it("复用 Promise 级 highlighter singleton", async () => {
    const first = getHighlighter();
    const second = getHighlighter();

    expect(first).toBe(second);
    await first;
    expect(createHighlighterCore).toHaveBeenCalledTimes(1);
  });

  it("视觉迭代 Fix A：代码块使用 AgentLoom 浅暖底主题（调浅自暖深褐）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");

    expect(CODE_THEME).toBe("agentloom-warm-dark");
    expect(css).toMatch(/\.mm-code\s*\{[^}]*background:\s*#efe7d6/);
    expect(css).toMatch(/\.mm-code-head\s*\{[^}]*rgba\(53,\s*45,\s*37/);
  });

  it("单例首次失败不永久缓存：下次调用重新尝试并可成功", async () => {
    // 独立模块实例（vi.resetModules + 动态 import），避免与上面已经把
    // 顶层静态导入的 highlighterPromise 变成 resolved 的测试互相污染。
    vi.resetModules();
    vi.doMock("shiki/core", () => ({
      createHighlighterCore: vi
        .fn()
        .mockRejectedValueOnce(new Error("boom"))
        .mockResolvedValueOnce({ ok: true }),
    }));
    vi.doMock("shiki/engine/oniguruma", () => ({
      createOnigurumaEngine: vi.fn(() =>
        Promise.resolve({ engine: "oniguruma" }),
      ),
    }));
    vi.doMock("shiki/wasm", () => ({}));

    const fresh = await import("./highlighter");
    const { createHighlighterCore: freshCreate } = await import("shiki/core");

    await expect(fresh.getHighlighter()).rejects.toThrow("boom");
    const retry = await fresh.getHighlighter();

    expect(retry).toEqual({ ok: true });
    expect(freshCreate).toHaveBeenCalledTimes(2);

    vi.doUnmock("shiki/core");
    vi.doUnmock("shiki/engine/oniguruma");
    vi.doUnmock("shiki/wasm");
    vi.resetModules();
  });
});
