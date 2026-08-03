import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { RepoList } from "./RepoList";
import type { RemoteRepo } from "../types/repoManage";
import { repoKey } from "../types/repoManage";

const cloned: RemoteRepo = {
  owner: "acme",
  name: "done",
  name_with_owner: "acme/done",
  is_private: false,
  is_empty: false,
  updated_at: "x",
  description: null,
  language: "TypeScript",
  language_color: "#3178C6",
  cloned: true,
  repo_id: "r1",
  local_path: "~/code/github.com/acme/done",
};
const remote: RemoteRepo = {
  owner: "acme",
  name: "todo",
  name_with_owner: "acme/todo",
  is_private: true,
  is_empty: false,
  updated_at: "x",
  description: null,
  language: null,
  language_color: null,
  cloned: false,
  repo_id: null,
  local_path: null,
};
const empty: RemoteRepo = {
  owner: "acme",
  name: "blank",
  name_with_owner: "acme/blank",
  is_private: false,
  is_empty: true,
  updated_at: "x",
  description: null,
  language: null,
  language_color: null,
  cloned: false,
  repo_id: null,
  local_path: null,
};

function setup(over: Partial<React.ComponentProps<typeof RepoList>> = {}) {
  const onToggleSelect = vi.fn();
  const props = {
    repos: [cloned, remote, empty],
    selectedLogin: "acme",
    search: "",
    onSearchChange: vi.fn(),
    filter: "all" as const,
    onFilterChange: vi.fn(),
    selected: new Set<string>(),
    onToggleSelect,
    onOpenSession: vi.fn(),
    ...over,
  };
  return { onToggleSelect, props, ...render(<RepoList {...props} />) };
}

describe("RepoList", () => {
  it("分组渲染已克隆/远程，已克隆行显示本地路径", () => {
    setup();
    expect(screen.getByText(/已克隆/)).toBeTruthy();
    expect(screen.getByText("~/code/github.com/acme/done")).toBeTruthy();
    expect(screen.getByText("远程", { exact: false })).toBeTruthy();
  });
  it("空 repo 标空仓库且 checkbox 禁用（点击不触发选中）", () => {
    const { onToggleSelect } = setup();
    expect(screen.getByText(/空仓库/)).toBeTruthy();
    const blankRow = screen.getByText("blank").closest(".ob-repo")!;
    fireEvent.click(blankRow.querySelector(".ob-cb")!);
    expect(onToggleSelect).not.toHaveBeenCalledWith(repoKey(empty));
  });
  it("远程行 checkbox 可选，回调带规范 key", () => {
    const { onToggleSelect } = setup();
    const row = screen.getByText("todo").closest(".ob-repo")!;
    fireEvent.click(row.querySelector(".ob-cb")!);
    expect(onToggleSelect).toHaveBeenCalledWith("github.com/acme/todo");
  });
  it("filter=cloned 只显示已克隆行", () => {
    setup({ filter: "cloned" });
    expect(screen.queryByText("todo")).toBeNull();
    expect(screen.getByText("done")).toBeTruthy();
  });
  it("二层结构：rm-fixed 含搜索 / rm-list 含行", () => {
    const { container } = setup();
    expect(container.querySelector(".rm-fixed .ob-search")).not.toBeNull();
    expect(container.querySelector(".rm-list")).not.toBeNull();
    // clone bar 不再由 RepoList 渲染
    expect(container.querySelector(".ob-batchbar")).toBeNull();
  });
  it("occupied 行按结构化 phase 显示本地化文案", () => {
    setup({
      cloneProgress: {
        [repoKey(remote)]: {
          login: "acme",
          owner: "acme",
          name: "todo",
          order: 0,
          phase: "occupied",
          message: "Error: PATH_OCCUPIED",
        },
      },
    });
    expect(screen.getByText("位置被占用")).toBeTruthy();
    expect(screen.queryByText("Error: PATH_OCCUPIED")).toBeNull();
  });
});
