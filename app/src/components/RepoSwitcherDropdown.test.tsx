import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { RepoSwitcherDropdown } from "./RepoSwitcherDropdown";
import type { NamespaceMeta, RepoMeta, Session } from "../types/agent";

const localNs: NamespaceMeta = {
  id: "local",
  name: "本机",
  kind: "local",
  is_builtin: 1,
  last_active_repo_id: null,
  added_at: 0,
  last_used_at: null,
};
const ghNs: NamespaceMeta = {
  id: "gh:acme",
  name: "acme",
  kind: "github_org",
  is_builtin: 0,
  last_active_repo_id: "r2",
  added_at: 0,
  last_used_at: null,
};
function repo(id: string, name: string, nsId: string, owner: string): RepoMeta {
  return {
    id,
    name,
    source: nsId === "local" ? "local" : "github",
    owner,
    path: `/tmp/${name}`,
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: nsId,
    icon: nsId === "local" ? "📝" : null,
  };
}
const rLocal = repo("rl", "scratch", "local", "local");
const r1 = repo("r1", "web", "gh:acme", "acme");
const r2 = repo("r2", "api", "gh:acme", "acme");

function baseProps() {
  return {
    open: true,
    namespaces: [localNs, ghNs],
    allRepos: [rLocal, r1, r2],
    sessions: [] as Session[],
    activeNamespaceId: "gh:acme",
    activeRepoId: "r2",
    onSelectRepoInNamespace: vi.fn(),
    onClose: vi.fn(),
    onNewProject: vi.fn(),
    onEditRepo: vi.fn(),
    onManageRepos: vi.fn(),
  };
}

describe("RepoSwitcherDropdown · 分组下拉", () => {
  it("open=false 不渲染", () => {
    const { container } = render(
      <RepoSwitcherDropdown {...baseProps()} open={false} />,
    );
    expect(container.querySelector(".repo-switcher")).toBeNull();
  });

  it("「项目」组置顶（DOM 顺序在 GitHub owner 段之前）", () => {
    const { container } = render(<RepoSwitcherDropdown {...baseProps()} />);
    const secs = Array.from(container.querySelectorAll(".dd-sec .dd-sec-nm"));
    expect(secs.map((s) => s.textContent)).toEqual(["项目", "acme"]);
  });

  it("当前 repo 行高亮 .on + ✓", () => {
    const { container } = render(<RepoSwitcherDropdown {...baseProps()} />);
    const onRow = container.querySelector(".dd-row.on");
    expect(onRow?.textContent).toContain("api");
    expect(onRow?.querySelector(".ck")?.textContent).toBe("✓");
  });

  it("点 repo 行触发 onSelectRepoInNamespace(nsId, repoId)", () => {
    const p = baseProps();
    render(<RepoSwitcherDropdown {...p} />);
    fireEvent.click(screen.getByText("web"));
    expect(p.onSelectRepoInNamespace).toHaveBeenCalledWith("gh:acme", "r1");
  });

  it("combined filter：搜 owner 名留该 owner 全部 repo", () => {
    const { container } = render(<RepoSwitcherDropdown {...baseProps()} />);
    fireEvent.change(container.querySelector(".rsw-search input")!, {
      target: { value: "acme" },
    });
    expect(screen.getByText("web")).not.toBeNull();
    expect(screen.getByText("api")).not.toBeNull();
    expect(screen.queryByText("scratch")).toBeNull();
  });

  it("combined filter：搜 repo 名只留匹配 repo + 其所在段", () => {
    const { container } = render(<RepoSwitcherDropdown {...baseProps()} />);
    fireEvent.change(container.querySelector(".rsw-search input")!, {
      target: { value: "web" },
    });
    expect(screen.getByText("web")).not.toBeNull();
    expect(screen.queryByText("api")).toBeNull();
    expect(screen.queryByText("scratch")).toBeNull();
  });

  it("底部只保留新建项目、管理 GitHub 仓库，并触发对应回调", () => {
    const p = baseProps();
    const { container } = render(<RepoSwitcherDropdown {...p} />);
    const footers = Array.from(container.querySelectorAll(".dd-foot"));
    expect(footers.map((node) => node.textContent)).toEqual([
      "＋新建项目",
      "管理 GitHub 仓库",
    ]);
    expect(screen.queryByText("连接 GitHub 仓库")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("新建项目"));
    expect(p.onNewProject).toHaveBeenCalled();
    fireEvent.click(screen.getByText("管理 GitHub 仓库"));
    expect(p.onManageRepos).toHaveBeenCalled();
  });

  it("点组件外 mousedown 触发 onClose", () => {
    const p = baseProps();
    render(<RepoSwitcherDropdown {...p} />);
    fireEvent.mouseDown(document.body);
    expect(p.onClose).toHaveBeenCalled();
  });

  it("零 GitHub namespace 只显项目组 + 两入口·不崩", () => {
    const { container } = render(
      <RepoSwitcherDropdown
        {...baseProps()}
        namespaces={[localNs]}
        allRepos={[rLocal]}
        activeNamespaceId="local"
        activeRepoId="rl"
      />,
    );
    const secs = Array.from(container.querySelectorAll(".dd-sec .dd-sec-nm"));
    expect(secs.map((s) => s.textContent)).toEqual(["项目"]);
    expect(screen.getByText("新建项目")).not.toBeNull();
    expect(screen.queryByText("连接 GitHub 仓库")).toBeNull();
    expect(screen.getByText("管理 GitHub 仓库")).not.toBeNull();
  });

  it("本机段 header 不再显示额外副文案", () => {
    const { container } = render(<RepoSwitcherDropdown {...baseProps()} />);
    expect(container.querySelector(".dd-sec .dd-sec-sub")).toBeNull();
    expect(container).not.toHaveTextContent("无远程");
  });

  it("combined filter 仅命中 GitHub repo 时·无前置 divider（codex P2 边界）", () => {
    // 搜 "api"（只命中 gh:acme 的 api·本机段 0 命中被隐）→ 第一个可见节点不是 .dd-div
    const { container } = render(<RepoSwitcherDropdown {...baseProps()} />);
    fireEvent.change(container.querySelector(".rsw-search input")!, {
      target: { value: "api" },
    });
    expect(screen.queryByText("scratch")).toBeNull(); // 本机段隐
    // 搜索框后第一个 group/divider：应是 .rsw-group（acme 段）·非 .dd-div
    const search = container.querySelector(".rsw-search")!;
    const firstAfter = search.nextElementSibling;
    expect(firstAfter?.classList.contains("dd-div")).toBe(false);
    expect(firstAfter?.classList.contains("rsw-group")).toBe(true);
  });

  it("零已克隆 repo（allRepos=[]）显空态引导·不崩（spec §2.A.10）", () => {
    const { container } = render(
      <RepoSwitcherDropdown {...baseProps()} allRepos={[]} />,
    );
    expect(container.querySelector(".rsw-empty")).not.toBeNull();
    expect(container.querySelector(".rsw-group")).toBeNull();
    expect(screen.getByText("新建项目")).not.toBeNull();
  });

  it("local-default 哨兵名称在列表显示为本地化的「我的项目」", () => {
    const defaultRepo = repo("local-default", "我的项目", "local", "local");
    render(
      <RepoSwitcherDropdown
        {...baseProps()}
        namespaces={[localNs]}
        allRepos={[defaultRepo]}
        activeNamespaceId="local"
        activeRepoId="local-default"
      />,
    );
    expect(screen.getByText("我的项目")).toBeInTheDocument();
    expect(screen.getByText("我的项目")).toBeInTheDocument();
  });

  it("本地项目显示 emoji，未设置时回退 📁", () => {
    const noIconRepo = {
      ...rLocal,
      id: "no-icon",
      name: "no icon",
      icon: null,
    };
    const { container } = render(
      <RepoSwitcherDropdown
        {...baseProps()}
        namespaces={[localNs]}
        allRepos={[rLocal, noIconRepo]}
      />,
    );

    expect(screen.getByText("scratch").closest(".dd-row")).toHaveTextContent(
      "📝",
    );
    expect(screen.getByText("no icon").closest(".dd-row")).toHaveTextContent(
      "📁",
    );
    expect(container.querySelectorAll(".project-icon")).toHaveLength(2);
  });

  it("本地项目 ✎ 调 onEditRepo(repo)，不进入行内输入态", () => {
    const p = baseProps();
    render(<RepoSwitcherDropdown {...p} />);

    const localRow = screen.getByText("scratch").closest(".dd-row")!;
    fireEvent.click(localRow.querySelector('button[aria-label="编辑项目"]')!);

    expect(p.onEditRepo).toHaveBeenCalledWith(rLocal);
    expect(p.onSelectRepoInNamespace).not.toHaveBeenCalled();
    expect(screen.queryByRole("textbox", { name: "编辑项目" })).toBeNull();
  });

  it("GitHub 项目 ✎ 调 onEditRepo(repo)，不触发行选择", () => {
    const p = baseProps();
    render(<RepoSwitcherDropdown {...p} />);

    const githubRow = screen.getByText("web").closest(".dd-row")!;
    fireEvent.click(githubRow.querySelector('button[aria-label="编辑项目"]')!);

    expect(p.onEditRepo).toHaveBeenCalledWith(r1);
    expect(p.onSelectRepoInNamespace).not.toHaveBeenCalled();
  });

  it("GitHub 项目有自定义 icon 时显示 emoji，否则显示 NamespaceAvatar", () => {
    const customIconRepo = { ...r1, icon: "🚀" };
    const { container } = render(
      <RepoSwitcherDropdown
        {...baseProps()}
        allRepos={[rLocal, customIconRepo, r2]}
      />,
    );

    const customIconRow = screen.getByText("web").closest(".dd-row")!;
    const defaultIconRow = screen.getByText("api").closest(".dd-row")!;
    expect(customIconRow.querySelector(".project-icon")).toHaveTextContent(
      "🚀",
    );
    expect(customIconRow.querySelector(".ns-av")).toBeNull();
    expect(defaultIconRow.querySelector(".project-icon")).toBeNull();
    expect(defaultIconRow.querySelector(".ns-av")).not.toBeNull();
    expect(container.querySelectorAll(".project-icon")).toHaveLength(2);
  });
});
