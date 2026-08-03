import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SettingsShell } from "./SettingsShell";

describe("SettingsShell", () => {
  it("只渲染已实现 nav·Agent 池 active·仓库 enabled·含 svg 图标", () => {
    const { container } = render(
      <SettingsShell activeKey="agents" onNavigate={() => {}}>
        <div>content</div>
      </SettingsShell>,
    );
    expect(screen.getByText("Agent 池")).toBeInTheDocument();
    expect(screen.getByText("联网搜索")).toBeInTheDocument();
    expect(screen.getByText("语言与区域")).toBeInTheDocument();
    expect(screen.getByText("仓库")).toBeInTheDocument();
    expect(screen.getByText("已归档项目")).toBeInTheDocument();
    expect(screen.getByText("关于")).toBeInTheDocument();
    expect(screen.queryByText("快捷键")).toBeNull();
    expect(screen.queryByText("默认 & 模式")).toBeNull();
    expect(screen.queryByText("namespace 白名单")).toBeNull();
    expect(screen.queryByText("账户 & Git")).toBeNull();
    expect(screen.queryByText("成本 & 预算")).toBeNull();
    expect(screen.getAllByRole("button")).toHaveLength(6);
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
    expect(container.querySelectorAll(".st-nav-item svg").length).toBe(6);
    expect(
      container.querySelector(".st-nav-item:last-child"),
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

  it("activeKey=repos 时 st-content 加 .repo 变体类", () => {
    const { container } = render(
      <SettingsShell activeKey="repos" onNavigate={() => {}}>
        <div>content</div>
      </SettingsShell>,
    );
    expect(container.querySelector(".st-content.repo")).not.toBeNull();
  });
});
