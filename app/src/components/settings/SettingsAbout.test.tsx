import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { openUrl } from "@tauri-apps/plugin-opener";
import pkg from "../../../package.json";
import { SettingsAbout } from "./SettingsAbout";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

describe("SettingsAbout", () => {
  beforeEach(() => {
    vi.mocked(openUrl).mockReset();
  });

  it("渲染应用名称、完整版本与固定落款信息", () => {
    render(<SettingsAbout />);

    expect(screen.getByText("AgentLoom")).toBeInTheDocument();
    expect(screen.getByText(`v${pkg.version}`)).toBeInTheDocument();
    expect(screen.getByText("panda@myagenthubs.com")).toBeInTheDocument();
    expect(screen.getByText("www.myagenthubs.com")).toBeInTheDocument();
    expect(screen.getByText("© 2026 MyAgentHubs")).toBeInTheDocument();
  });

  it("点击支持邮箱用 mailto 链接打开", () => {
    render(<SettingsAbout />);

    fireEvent.click(
      screen.getByRole("link", { name: "panda@myagenthubs.com" }),
    );

    expect(openUrl).toHaveBeenCalledWith("mailto:panda@myagenthubs.com");
  });

  it("点击官网用 HTTPS 链接打开", () => {
    render(<SettingsAbout />);

    fireEvent.click(screen.getByRole("link", { name: "www.myagenthubs.com" }));

    expect(openUrl).toHaveBeenCalledWith("https://www.myagenthubs.com");
  });

  it("渲染 GitHub issues 问题反馈链接", () => {
    render(<SettingsAbout />);

    expect(
      screen.getByText("github.com/MyAgentHubs/agentloom/issues"),
    ).toBeInTheDocument();
  });

  it("点击问题反馈用 HTTPS 链接打开 GitHub issues", () => {
    render(<SettingsAbout />);

    fireEvent.click(
      screen.getByRole("link", {
        name: "github.com/MyAgentHubs/agentloom/issues",
      }),
    );

    expect(openUrl).toHaveBeenCalledWith(
      "https://github.com/MyAgentHubs/agentloom/issues",
    );
  });
});
