import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SettingsShell } from "./SettingsShell";

describe("SettingsShell", () => {
  it("按五组渲染目标 nav 顺序，并仅在四个组边界插入分隔线", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <SettingsShell activeKey="agents" onNavigate={() => {}}>
        <div>content</div>
      </SettingsShell>,
    );
    const groups = Array.from(container.querySelectorAll(".st-nav-group"));

    expect(groups).toHaveLength(5);
    expect(
      groups.map((group) =>
        Array.from(group.querySelectorAll(".st-nav-item"), (item) =>
          item.textContent?.trim(),
        ),
      ),
    ).toEqual([
      ["Agent 池", "联网搜索"],
      ["仓库", "已归档项目"],
      ["对话", "语言与区域"],
      ["远程控制"],
      ["关于"],
    ]);
    expect(
      Array.from(container.querySelectorAll(".st-nav-item"), (item) =>
        item.textContent?.trim(),
      ),
    ).toEqual([
      "Agent 池",
      "联网搜索",
      "仓库",
      "已归档项目",
      "对话",
      "语言与区域",
      "远程控制",
      "关于",
    ]);

    const separators = screen.getAllByRole("separator");
    expect(separators).toHaveLength(4);
    for (const [index, separator] of separators.entries()) {
      expect(separator.previousElementSibling).toBe(groups[index]);
      expect(separator.nextElementSibling).toBe(groups[index + 1]);
    }
    for (const separator of separators) {
      expect(separator).not.toHaveAttribute("tabindex");
      expect(separator).not.toHaveAttribute("aria-disabled");
    }

    for (const label of [
      "Agent 池",
      "联网搜索",
      "仓库",
      "已归档项目",
      "对话",
      "语言与区域",
      "远程控制",
      "关于",
    ]) {
      await user.tab();
      expect(screen.getByRole("button", { name: label })).toHaveFocus();
    }
  });

  it("只渲染已实现 nav·Agent 池 active·仓库 enabled·含 svg 图标", () => {
    const { container } = render(
      <SettingsShell activeKey="agents" onNavigate={() => {}}>
        <div>content</div>
      </SettingsShell>,
    );
    expect(screen.getByText("Agent 池")).toBeInTheDocument();
    expect(screen.getByText("联网搜索")).toBeInTheDocument();
    expect(screen.getByText("语言与区域")).toBeInTheDocument();
    expect(screen.getByText("对话")).toBeInTheDocument();
    expect(screen.getByText("远程控制")).toBeInTheDocument();
    expect(screen.getByText("仓库")).toBeInTheDocument();
    expect(screen.getByText("已归档项目")).toBeInTheDocument();
    expect(screen.getByText("关于")).toBeInTheDocument();
    expect(screen.queryByText("快捷键")).toBeNull();
    expect(screen.queryByText("默认 & 模式")).toBeNull();
    expect(screen.queryByText("namespace 白名单")).toBeNull();
    expect(screen.queryByText("账户 & Git")).toBeNull();
    expect(screen.queryByText("成本 & 预算")).toBeNull();
    expect(screen.getAllByRole("button")).toHaveLength(8);
    expect(screen.getByText("Agent 池").closest("button")).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(screen.getByText("仓库").closest("button")).not.toHaveAttribute(
      "tabindex",
      "-1",
    );
    const disabled = screen
      .getAllByRole("button")
      .filter((b) => b.getAttribute("aria-disabled") === "true");
    expect(disabled.length).toBe(0);
    expect(container.querySelectorAll(".st-nav-item svg").length).toBe(8);
    expect(
      container.querySelector(
        ".st-nav-group:last-child .st-nav-item:last-child",
      ),
    ).toHaveTextContent("关于");
    expect(screen.getByText("content")).toBeInTheDocument();
  });

  it("点已实现 nav 触发 onNavigate", () => {
    const onNavigate = vi.fn();
    render(
      <SettingsShell activeKey="agents" onNavigate={onNavigate}>
        <div>content</div>
      </SettingsShell>,
    );
    fireEvent.click(screen.getByText("仓库"));
    expect(onNavigate).toHaveBeenCalledWith("repos");
  });

  it("点「联网搜索」nav 触发 onNavigate(search)·可切换到该页", () => {
    const onNavigate = vi.fn();
    render(
      <SettingsShell activeKey="agents" onNavigate={onNavigate}>
        <div>content</div>
      </SettingsShell>,
    );
    fireEvent.click(screen.getByText("联网搜索"));
    expect(onNavigate).toHaveBeenCalledWith("search");
  });

  it("点「远程控制」nav 触发 onNavigate(remoteControl)", () => {
    const onNavigate = vi.fn();
    render(
      <SettingsShell activeKey="agents" onNavigate={onNavigate}>
        <div>content</div>
      </SettingsShell>,
    );
    fireEvent.click(screen.getByText("远程控制"));
    expect(onNavigate).toHaveBeenCalledWith("remoteControl");
  });

  it("activeKey=repos 时 st-content 加 .repo 变体类", () => {
    const { container } = render(
      <SettingsShell activeKey="repos" onNavigate={() => {}}>
        <div>content</div>
      </SettingsShell>,
    );
    expect(container.querySelector(".st-content.repo")).not.toBeNull();
  });
});
