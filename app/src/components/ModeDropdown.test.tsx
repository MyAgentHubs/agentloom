import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";
import { ModeDropdown } from "./ModeDropdown";

describe("ModeDropdown", () => {
  it("选 Normal 调 onModeChange 并关闭", () => {
    const onModeChange = vi.fn();
    render(<ModeDropdown mode="normal" onModeChange={onModeChange} />);
    fireEvent.click(screen.getByRole("button", { name: /Normal/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Normal/ }));
    expect(onModeChange).toHaveBeenCalledWith("normal");
    expect(screen.queryByRole("menu")).toBeNull();
  });
});

describe("ModeDropdown 默认+召唤", () => {
  it("展开后含「当前」区 Normal（menuitemradio·打勾·desc）", () => {
    render(<ModeDropdown mode="normal" onModeChange={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /Normal/ }));
    const menu = screen.getByRole("menu");
    // 分区标题（防后续误删分区结构）
    expect(within(menu).getByText("当前")).toBeInTheDocument();
    expect(within(menu).getByText("多 agent 协作")).toBeInTheDocument();
    const normal = screen.getByRole("menuitemradio", { name: /Normal/ });
    expect(normal).toHaveAttribute("aria-checked", "true");
    expect(
      screen.getByText("你和一个选对的搭档，专注当前这件事"),
    ).toBeInTheDocument();
  });

  it("Team 可选中，Round 仍是 menuitem aria-disabled +「即将」", () => {
    render(<ModeDropdown mode="normal" onModeChange={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /Normal/ }));
    const team = screen.getByRole("menuitemradio", { name: /Agent Team/ });
    const round = screen.getByRole("menuitem", { name: /Round Table/ });
    expect(team).not.toHaveAttribute("aria-disabled", "true");
    expect(round).toHaveAttribute("aria-disabled", "true");
    expect(within(team).queryByText("即将")).not.toBeInTheDocument();
    expect(within(round).getByText("即将")).toBeInTheDocument();
  });

  it("点 disabled 的 Round 不触发 onModeChange", () => {
    const onModeChange = vi.fn();
    render(<ModeDropdown mode="normal" onModeChange={onModeChange} />);
    fireEvent.click(screen.getByRole("button", { name: /Normal/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: /Round Table/ }));
    expect(onModeChange).not.toHaveBeenCalled();
  });

  it("点 Team 触发 onModeChange('team')", () => {
    const onModeChange = vi.fn();
    render(<ModeDropdown mode="normal" onModeChange={onModeChange} />);
    fireEvent.click(screen.getByRole("button", { name: /Normal/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Agent Team/ }));
    expect(onModeChange).toHaveBeenCalledWith("team");
  });
});
