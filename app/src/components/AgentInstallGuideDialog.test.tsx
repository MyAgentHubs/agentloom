import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AgentInstallGuideDialog } from "./AgentInstallGuideDialog";

const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: openUrlMock }));

describe("AgentInstallGuideDialog", () => {
  beforeEach(() => {
    openUrlMock.mockReset();
    openUrlMock.mockResolvedValue(undefined);
  });

  it("渲染原因、三种 agent 路径和操作，myagent 排在首位", () => {
    const { container } = render(
      <AgentInstallGuideDialog onClose={() => {}} onOpenSettings={() => {}} />,
    );

    const dialog = screen.getByRole("dialog", {
      name: "还没有可用的 agent",
    });
    expect(dialog).toHaveTextContent(
      "AgentLoom 可以用内置引擎 myagent 跑 agent —— 只需要你自己的 API key，不用装任何厂商 CLI；也可以驱动本机的 Claude Code 或 Codex CLI。当前三种都还没配置好。",
    );
    const options = container.querySelectorAll(".agent-install-guide__option");
    expect(options).toHaveLength(3);
    expect(
      within(options[0] as HTMLElement).getByRole("heading"),
    ).toHaveTextContent("myagent");
    expect(dialog).toHaveTextContent("myagent");
    expect(within(dialog).getByText("Claude Code")).toBeInTheDocument();
    expect(within(dialog).getByText("Codex")).toBeInTheDocument();
    expect(dialog).toHaveTextContent(
      "用你自己的 API key 直接跑，不需要安装任何厂商 CLI。",
    );
    expect(dialog).toHaveTextContent("使用 Anthropic 账号运行 Claude agent。");
    expect(dialog).toHaveTextContent("使用 OpenAI 账号运行 Codex agent。");
    expect(
      within(dialog).getByRole("button", { name: "去配置" }),
    ).toBeInTheDocument();
    expect(
      within(dialog).getAllByRole("button", { name: "打开安装指引" }),
    ).toHaveLength(2);
    expect(
      within(dialog).getByRole("button", { name: "打开 Agent 设置" }),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByRole("button", { name: "稍后再说" }),
    ).toBeInTheDocument();
  });

  it("两个安装按钮分别打开 Claude Code 和 Codex 安装 URL", () => {
    render(
      <AgentInstallGuideDialog onClose={() => {}} onOpenSettings={() => {}} />,
    );

    const buttons = screen.getAllByRole("button", { name: "打开安装指引" });
    fireEvent.click(buttons[0]);
    fireEvent.click(buttons[1]);

    expect(openUrlMock).toHaveBeenNthCalledWith(
      1,
      "https://claude.com/claude-code",
    );
    expect(openUrlMock).toHaveBeenNthCalledWith(
      2,
      "https://github.com/openai/codex",
    );
  });

  it("点击 myagent 配置按钮会关闭弹窗并打开设置", () => {
    const onClose = vi.fn();
    const onOpenSettings = vi.fn();
    render(
      <AgentInstallGuideDialog
        onClose={onClose}
        onOpenSettings={onOpenSettings}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "去配置" }));

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
    expect(onClose.mock.invocationCallOrder[0]).toBeLessThan(
      onOpenSettings.mock.invocationCallOrder[0],
    );
  });

  it("Escape、背景和稍后再说都会关闭", () => {
    const onClose = vi.fn();
    const { container } = render(
      <AgentInstallGuideDialog onClose={onClose} onOpenSettings={() => {}} />,
    );

    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.click(container.querySelector(".dialog__backdrop")!);
    fireEvent.click(screen.getByRole("button", { name: "稍后再说" }));

    expect(onClose).toHaveBeenCalledTimes(3);
  });

  it("点击 dialog 内部不关闭，打开设置调用设置回调", () => {
    const onClose = vi.fn();
    const onOpenSettings = vi.fn();
    const { container } = render(
      <AgentInstallGuideDialog
        onClose={onClose}
        onOpenSettings={onOpenSettings}
      />,
    );

    fireEvent.click(container.querySelector(".dialog")!);
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "打开 Agent 设置" }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
