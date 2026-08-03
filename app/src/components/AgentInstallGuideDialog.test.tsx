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

  it("渲染原因、两种 CLI 说明和操作", () => {
    render(
      <AgentInstallGuideDialog onClose={() => {}} onOpenSettings={() => {}} />,
    );

    const dialog = screen.getByRole("dialog", {
      name: "还没有可用的 agent",
    });
    expect(dialog).toHaveTextContent(
      "AgentLoom 通过本机的 Claude Code 或 Codex CLI 驱动 agent，当前两个都没有检测到。",
    );
    expect(within(dialog).getByText("Claude Code")).toBeInTheDocument();
    expect(within(dialog).getByText("Codex")).toBeInTheDocument();
    expect(dialog).toHaveTextContent("使用 Anthropic 账号运行 Claude agent。");
    expect(dialog).toHaveTextContent("使用 OpenAI 账号运行 Codex agent。");
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
