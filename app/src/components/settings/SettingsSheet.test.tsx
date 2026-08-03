import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import pkg from "../../../package.json";
import { SettingsSheet } from "./SettingsSheet";

const base = {
  page: "agents" as const,
  onPageChange: () => {},
  onClose: () => {},
  agentsContent: <div>AGENTS_CONTENT</div>,
  searchContent: <div>SEARCH_CONTENT</div>,
  reposContent: <div>REPOS_CONTENT</div>,
  archivedProjectsContent: <div>ARCHIVED_PROJECTS_CONTENT</div>,
  languageContent: <div>LANGUAGE_CONTENT</div>,
};

describe("SettingsSheet", () => {
  it("open=false 返回 null", () => {
    const { container } = render(<SettingsSheet open={false} {...base} />);
    expect(container.firstChild).toBeNull();
  });

  it("open=true 渲染 backdrop + sheet + 当前页内容 + 唯一 nav", () => {
    const { container } = render(<SettingsSheet open={true} {...base} />);
    expect(container.querySelector(".settings-backdrop")).not.toBeNull();
    expect(container.querySelector(".settings-sheet")).not.toBeNull();
    expect(screen.getByText("AGENTS_CONTENT")).toBeInTheDocument();
    expect(screen.queryByText("REPOS_CONTENT")).toBeNull();
    expect(container.querySelectorAll(".st-nav").length).toBe(1);
    expect(screen.getByText("版本")).toBeInTheDocument();
    expect(screen.getByText(`AgentLoom v${pkg.version}`)).toBeInTheDocument();
  });

  it("page=repos 渲染 repos 内容", () => {
    render(<SettingsSheet open={true} {...base} page="repos" />);
    expect(screen.getByText("REPOS_CONTENT")).toBeInTheDocument();
    expect(screen.queryByText("AGENTS_CONTENT")).toBeNull();
  });

  it("page=archivedProjects 渲染已归档项目内容", () => {
    render(<SettingsSheet open={true} {...base} page="archivedProjects" />);
    expect(screen.getByText("ARCHIVED_PROJECTS_CONTENT")).toBeInTheDocument();
    expect(screen.queryByText("REPOS_CONTENT")).toBeNull();
  });

  it("page=language 渲染语言内容", () => {
    render(<SettingsSheet open={true} {...base} page="language" />);
    expect(screen.getByText("LANGUAGE_CONTENT")).toBeInTheDocument();
    expect(screen.queryByText("AGENTS_CONTENT")).toBeNull();
  });

  it("page=search 渲染联网搜索内容", () => {
    render(<SettingsSheet open={true} {...base} page="search" />);
    expect(screen.getByText("SEARCH_CONTENT")).toBeInTheDocument();
    expect(screen.queryByText("AGENTS_CONTENT")).toBeNull();
  });

  it("点 nav「联网搜索」触发 onPageChange(search)", () => {
    const onPageChange = vi.fn();
    render(<SettingsSheet open={true} {...base} onPageChange={onPageChange} />);
    fireEvent.click(screen.getByText("联网搜索"));
    expect(onPageChange).toHaveBeenCalledWith("search");
  });

  it("点 nav「仓库」触发 onPageChange(repos)", () => {
    const onPageChange = vi.fn();
    render(<SettingsSheet open={true} {...base} onPageChange={onPageChange} />);
    fireEvent.click(screen.getByText("仓库"));
    expect(onPageChange).toHaveBeenCalledWith("repos");
  });

  it("点背景关·点 sheet 不关·点 ✕ 关", () => {
    const onClose = vi.fn();
    const { container } = render(
      <SettingsSheet open={true} {...base} onClose={onClose} />,
    );
    fireEvent.click(container.querySelector(".settings-sheet")!);
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByLabelText("关闭设置"));
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(container.querySelector(".settings-backdrop")!);
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});
