import { render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InputArea } from "../InputArea";

afterEach(() => {
  vi.useRealTimers();
});

function renderInputArea(extra: Record<string, unknown> = {}) {
  return render(
    <InputArea
      composerBusy={false}
      running={false}
      memberRunning={false}
      agents={[]}
      agentId="a1"
      onAgentChange={() => {}}
      mode="normal"
      onModeChange={() => {}}
      onSend={() => {}}
      onStop={() => {}}
      {...extra}
    />,
  );
}

describe("InputArea 会话累计 token", () => {
  it("静止态把非零累计追加到本轮 runMeta", () => {
    const { container } = renderInputArea({
      runMeta: "28s · 12.4k tok",
      sessionUsage: { input: 7, output: 13 },
    });

    expect(container.querySelector(".composer__hint-cost")).toHaveTextContent(
      "28s · 12.4k tok · 全程 20 tok",
    );
  });

  it("静止态累计为零时不显示累计段", () => {
    const { container } = renderInputArea({
      runMeta: "28s · 12.4k tok",
      sessionUsage: { input: 0, output: 0 },
    });

    expect(container.querySelector(".composer__hint-cost")).toHaveTextContent(
      "28s · 12.4k tok",
    );
    expect(
      container.querySelector(".composer__hint-cost"),
    ).not.toHaveTextContent("全程");
  });

  it("静止态无本轮 runMeta 时只显示累计段", () => {
    const { container } = renderInputArea({
      sessionUsage: { input: 7, output: 13 },
    });

    expect(container.querySelector(".composer__hint-cost")).toHaveTextContent(
      "全程 20 tok",
    );
  });

  it("运行态 hint 只显示 token 成本（不显示秒数、状态词、累计段）", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-18T00:00:00.000Z"));

    const { container } = renderInputArea({
      running: true,
      runStartedAt: Date.now() - 4000,
      runMeta: "28s · 12.4k tok",
      sessionUsage: { input: 7, output: 13 },
    });

    const status = container.querySelector(".composer__hint-cost");
    // 不显示秒数
    expect(status).toHaveTextContent("");
    expect(status).not.toHaveTextContent("工作中");
    expect(status).not.toHaveTextContent("全程");
    expect(status).not.toHaveTextContent("4s");
  });

  it("运行态 workingTokens > 0 时 hint 只显示 ↑ x tok（不显示秒数）", () => {
    const { container } = renderInputArea({
      running: true,
      runStartedAt: Date.now() - 7000,
      workingTokens: 12_100,
    });

    const status = container.querySelector(".composer__hint-cost");
    expect(status).toHaveTextContent("↑ 12.1k tok");
    expect(status).not.toHaveTextContent("7s");
    expect(status).not.toHaveTextContent("工作中");
  });

  it("运行态 workingTokens 为空/null/0 时不渲染", () => {
    const { container } = renderInputArea({
      running: true,
      runStartedAt: Date.now() - 7000,
      workingTokens: null,
    });

    const status = container.querySelector(".composer__hint-cost");
    expect(status).toHaveTextContent("");
  });

  it("累计提示的 title 显示输入输出明细", () => {
    const { container } = renderInputArea({
      sessionUsage: { input: 7, output: 13 },
    });

    expect(container.querySelector(".composer__hint-cost")).toHaveAttribute(
      "title",
      "↑ 7 · ↓ 13",
    );
  });
});
