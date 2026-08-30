import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  __resetForTests,
  getChatVerbosity,
  setChatVerbosity,
} from "../../lib/chatVerbosity";
import { SettingsChat } from "./SettingsChat";

describe("SettingsChat", () => {
  beforeEach(() => {
    localStorage.clear();
    __resetForTests();
  });

  afterEach(() => {
    localStorage.clear();
    __resetForTests();
  });

  it("渲染三项 radio·标题与说明齐全", () => {
    render(<SettingsChat />);

    expect(screen.getByText("对话")).toBeInTheDocument();
    expect(
      screen.getByText("对话流里过程细节的显示级别，仅影响本机。"),
    ).toBeInTheDocument();
    expect(screen.getByRole("radiogroup")).toBeInTheDocument();
    expect(screen.getAllByRole("radio")).toHaveLength(3);
    expect(screen.getByText("详细")).toBeInTheDocument();
    expect(screen.getByText("摘要")).toBeInTheDocument();
    expect(screen.getByText("精简")).toBeInTheDocument();
    expect(
      screen.getByText("显示面向用户的工具活动、命令摘要与思考过程（可折叠）"),
    ).toBeInTheDocument();
  });

  it("默认档 summary 对应「摘要」项被选中", () => {
    render(<SettingsChat />);

    const summaryRadio = screen.getByRole("radio", {
      name: /摘要/,
    }) as HTMLInputElement;
    expect(summaryRadio.checked).toBe(true);
  });

  it("已存 minimal 偏好时「精简」项渲染为选中", () => {
    setChatVerbosity("minimal");
    render(<SettingsChat />);

    const minimalRadio = screen.getByRole("radio", {
      name: /精简/,
    }) as HTMLInputElement;
    expect(minimalRadio.checked).toBe(true);
  });

  it("点另一项后 store 变更·再渲染一个实例同步显示新选中项", () => {
    render(<SettingsChat />);

    const fullRadio = screen.getByRole("radio", { name: /详细/ });
    fireEvent.click(fullRadio);

    expect(getChatVerbosity()).toBe("full");

    const { unmount } = render(<SettingsChat />);
    const allFullRadios = screen.getAllByRole("radio", { name: /详细/ });
    expect(allFullRadios.every((el) => (el as HTMLInputElement).checked)).toBe(
      true,
    );
    unmount();
  });
});
