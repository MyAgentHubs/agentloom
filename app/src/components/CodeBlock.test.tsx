import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../lib/highlighter", () => ({
  CODE_THEME: "agentloom-warm-dark",
  SUPPORTED_LANGS: ["typescript"],
  getHighlighter: vi.fn(async () => ({
    codeToTokens: (code: string) => ({
      tokens: code.split("\n").map((line) => [
        {
          color: "#d8d2c4",
          content: line,
        },
      ]),
    }),
    getLoadedLanguages: () => ["typescript"],
  })),
  normalizeLang: (lang?: string) => (lang === "ts" ? "typescript" : "text"),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openPath: vi.fn() }));

import { getHighlighter } from "../lib/highlighter";
import { CodeBlock } from "./CodeBlock";

describe("CodeBlock", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("初始渲染纯 pre fallback（高亮前）+ lang 标签", async () => {
    const { container } = render(<CodeBlock code="const x = 1;" lang="ts" />);

    expect(container.querySelector("pre")).not.toBeNull();
    expect(screen.getByText("typescript")).toBeInTheDocument();
    await waitFor(() => {
      expect(container.querySelector('[style*="color"]')).not.toBeNull();
    });
  });

  it("高亮完成后渲染 token spans（异步）", async () => {
    const { container } = render(<CodeBlock code={"a\nb"} lang="ts" />);

    await waitFor(() => {
      expect(container.querySelectorAll(".mm-code .ln").length).toBeGreaterThan(
        0,
      );
      expect(container.querySelector('[style*="color"]')).not.toBeNull();
    });
  });

  it("复制按钮调 clipboard", async () => {
    const writeText = vi.fn();
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    const { container } = render(<CodeBlock code="x" lang="ts" />);
    await waitFor(() => {
      expect(container.querySelector('[style*="color"]')).not.toBeNull();
    });

    fireEvent.click(screen.getByRole("button", { name: "复制" }));

    expect(writeText).toHaveBeenCalledWith("x");
  });

  it("超长（>30 行）默认折叠显示前 30 + 展开按钮", async () => {
    const code = Array.from({ length: 50 }, (_, i) => `line${i}`).join("\n");

    render(<CodeBlock code={code} lang="ts" />);

    await waitFor(() => {
      expect(screen.getByText("展开 +20 行")).toBeInTheDocument();
    });
    expect(screen.getByText("line0")).toBeInTheDocument();
    expect(screen.queryByText("line49")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("展开 +20 行"));
    expect(await screen.findByText("line49")).toBeInTheDocument();
  });

  it("raw lang=html → 显「在浏览器打开」（即使 normalize 成 text）", () => {
    render(<CodeBlock code="<div>x</div>" lang="html" />);
    expect(
      screen.getByRole("button", { name: "在浏览器打开" }),
    ).toBeInTheDocument();
  });

  it("内容起始 <!doctype html → 显", () => {
    render(<CodeBlock code="<!DOCTYPE html><html></html>" lang="text" />);
    expect(
      screen.getByRole("button", { name: "在浏览器打开" }),
    ).toBeInTheDocument();
  });

  it("普通代码不显", () => {
    render(<CodeBlock code="const x = 1;" lang="javascript" />);
    expect(screen.queryByRole("button", { name: "在浏览器打开" })).toBeNull();
  });

  it("高亮引擎失败时静默退化为纯文本、不抛未捕获 rejection", async () => {
    vi.mocked(getHighlighter).mockRejectedValueOnce(new Error("engine boom"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    // 用未在其它用例出现过的代码内容，避开模块级 tokenCache 命中
    // （否则会直接读缓存跳过 getHighlighter，测不到失败路径）。
    const code = "const highlightFailureProbe = 42;";
    const { container } = render(<CodeBlock code={code} lang="ts" />);

    await waitFor(() => {
      expect(screen.getByText(code)).toBeInTheDocument();
    });
    // 退化路径：没有走高亮 token 着色
    expect(container.querySelector('[style*="color"]')).toBeNull();
    expect(warn).toHaveBeenCalled();

    warn.mockRestore();
  });

  it("点击 → write_temp_html + openPath", async () => {
    vi.mocked(invoke).mockResolvedValue("/tmp/x.html");

    render(<CodeBlock code="<html></html>" lang="html" />);
    fireEvent.click(screen.getByRole("button", { name: "在浏览器打开" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("write_temp_html", {
        content: "<html></html>",
      }),
    );
    await waitFor(() => expect(openPath).toHaveBeenCalledWith("/tmp/x.html"));
  });
});
