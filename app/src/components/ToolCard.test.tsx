import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ToolCard } from "./ToolCard";
import type { Block } from "../types/agent";
import { I18nProvider } from "../i18n";

type ToolBlock = Extract<Block, { type: "tool" }>;

function tool(over: Partial<ToolBlock>): ToolBlock {
  return {
    type: "tool",
    id: "t1",
    tool: "Bash",
    summary: "ls -la",
    card: "command",
    status: "running",
    exit_code: null,
    output: null,
    ...over,
  };
}

function renderZh(block: ToolBlock, compact?: boolean) {
  return render(
    <I18nProvider initialLocale="zh">
      <ToolCard block={block} compact={compact} />
    </I18nProvider>,
  );
}

describe("ToolCard", () => {
  it("command running 显命令 + 运行中徽章、不显 exit", () => {
    renderZh(tool({ status: "running" }));
    expect(screen.getByText("ls -la")).toBeInTheDocument();
    expect(screen.getByText("运行中")).toBeInTheDocument();
    expect(screen.queryByText(/exit/i)).not.toBeInTheDocument();
  });

  it("command done exit 0 显 exit 0", () => {
    renderZh(tool({ status: "ok", exit_code: 0, output: "done" }));
    expect(screen.getByText("完成")).toBeInTheDocument();
    expect(screen.getByText(/exit 0/)).toBeInTheDocument();
  });

  it("exit_code null 不渲染 exit 段", () => {
    renderZh(tool({ status: "ok", exit_code: null, output: "x" }));
    expect(screen.queryByText(/exit/i)).not.toBeInTheDocument();
  });

  it("failed 自动展开且只露尾部 30 行 + 顶部 +N 行展开器", () => {
    const lines = Array.from({ length: 50 }, (_, i) => `line${i + 1}`).join(
      "\n",
    );
    renderZh(tool({ status: "failed", exit_code: 1, output: lines }));

    expect(screen.getByText("失败")).toBeInTheDocument();
    expect(screen.getByText(/line50/)).toBeInTheDocument();
    expect(screen.queryByText(/^line1$/)).not.toBeInTheDocument();
    expect(screen.getByText(/\+\s*\d+\s*行/)).toBeInTheDocument();
  });

  it("interrupted 显已中断徽章、折叠", () => {
    renderZh(tool({ status: "interrupted", output: "partial" }));
    expect(screen.getByText("已中断")).toBeInTheDocument();
  });

  it("compact 档一行、无展开体", () => {
    renderZh(
      tool({
        card: "compact",
        tool: "Read",
        summary: "a.rs",
        status: "ok",
      }),
    );
    expect(screen.getByText("a.rs")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /展开/ }),
    ).not.toBeInTheDocument();
  });

  it("toolcard_compact_prop_forces_single_line", () => {
    renderZh(
      {
        type: "tool",
        id: "b1",
        tool: "Bash",
        card: "command",
        summary: "npm test",
        status: "ok",
        exit_code: 0,
        output: "very long output that should not be shown",
      },
      true,
    );
    const el = document.querySelector(".toolcard--compact");
    expect(el).not.toBeNull();
    expect(screen.getByText("运行命令")).toBeInTheDocument();
    expect(screen.getByText("npm test")).toBeInTheDocument();
    expect(screen.getByText("完成")).toBeInTheDocument();
    expect(document.querySelector(".toolcard__head")).toBeNull();
    expect(document.querySelector(".toolcard__out")).toBeNull();
  });

  it("compact 档 summary 与 tool 原始名相同时不重复渲染 summary（后端回落坑）", () => {
    renderZh(
      tool({
        card: "compact",
        tool: "mcp__agentloom__commit",
        summary: "mcp__agentloom__commit",
        status: "ok",
      }),
    );
    expect(screen.getByText("提交代码")).toBeInTheDocument();
    // summary 与原始 tool 名相同 → 不应再额外渲染一份 "mcp__agentloom__commit"
    expect(
      screen.queryByText("mcp__agentloom__commit"),
    ).not.toBeInTheDocument();
    expect(document.querySelector(".toolcard__summary")).toBeNull();
  });
});
