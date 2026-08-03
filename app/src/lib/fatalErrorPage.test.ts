import { describe, expect, it } from "vitest";
import { buildFatalErrorElement } from "./fatalErrorPage";

describe("buildFatalErrorElement", () => {
  it("renders the app version and the error message/stack", () => {
    const el = buildFatalErrorElement({
      message: "boom: something broke",
      stack: "Error: boom\n    at foo (bar.ts:1:1)",
    });

    expect(el.getAttribute("data-testid")).toBe("fatal-error-page");
    expect(el.textContent).toContain("boom: something broke");
    expect(el.textContent).toContain("at foo (bar.ts:1:1)");
    // vitest.config 未在测试模式下注入 __APP_VERSION__ 时应退化为 "dev"，
    // 否则应为 package.json 里的真实版本号——两种情况都必须显示版本行，不能为空。
    const versionEl = el.querySelector('[data-testid="fatal-error-version"]');
    expect(versionEl?.textContent).toMatch(/AgentLoom v\S+/);
  });

  it("falls back to only the message when stack is absent", () => {
    const el = buildFatalErrorElement({ message: "no stack here" });
    expect(el.textContent).toContain("no stack here");
  });

  it("shows the bilingual plain-language notice selected by navigator.language", () => {
    const originalLanguage = navigator.language;
    try {
      Object.defineProperty(navigator, "language", {
        value: "zh-CN",
        configurable: true,
      });
      const zhEl = buildFatalErrorElement({ message: "x" });
      expect(zhEl.textContent).toContain("AgentLoom 遇到了一个问题");

      Object.defineProperty(navigator, "language", {
        value: "en-US",
        configurable: true,
      });
      const enEl = buildFatalErrorElement({ message: "x" });
      expect(enEl.textContent).toContain("AgentLoom ran into a problem");
    } finally {
      Object.defineProperty(navigator, "language", {
        value: originalLanguage,
        configurable: true,
      });
    }
  });

  it("is inline-styled (no dependency on external CSS)", () => {
    const el = buildFatalErrorElement({ message: "x" });
    expect(el.getAttribute("style")).toBeTruthy();
    const card = el.firstElementChild as HTMLElement | null;
    expect(card?.getAttribute("style")).toBeTruthy();
  });
});
