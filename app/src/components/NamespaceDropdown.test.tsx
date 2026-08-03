import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { NamespaceDropdown } from "./NamespaceDropdown";
import type { NamespaceMeta, RepoMeta } from "../types/agent";

const localNs: NamespaceMeta = {
  id: "local",
  kind: "local",
  name: "Local",
  is_builtin: 1,
  last_active_repo_id: "local-default",
  added_at: 0,
  last_used_at: null,
};
const orgA: NamespaceMeta = {
  id: "ns-a",
  kind: "github_org",
  name: "myagenthubs",
  is_builtin: 0,
  last_active_repo_id: null,
  added_at: 100,
  last_used_at: 200,
};
const orgB: NamespaceMeta = {
  id: "ns-b",
  kind: "github_org",
  name: "impanda-cookie",
  is_builtin: 0,
  last_active_repo_id: null,
  added_at: 50,
  last_used_at: 80,
};
const allRepos: RepoMeta[] = [
  {
    id: "local-default",
    source: "local",
    owner: null,
    name: "Local 默认",
    path: "/tmp",
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: "local",
  },
  {
    id: "r-a1",
    source: "github",
    owner: "myagenthubs",
    name: "agentloom",
    path: "/tmp/a1",
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: "ns-a",
  },
  {
    id: "r-a2",
    source: "github",
    owner: "myagenthubs",
    name: "my-blog",
    path: "/tmp/a2",
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: "ns-a",
  },
  {
    id: "r-a3",
    source: "github",
    owner: "myagenthubs",
    name: "x",
    path: "/tmp/a3",
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: "ns-a",
  },
];
const base = {
  open: true,
  namespaces: [localNs, orgA, orgB],
  activeNamespaceId: "ns-a",
  allRepos,
  onSelectNamespace: vi.fn(),
  onClose: vi.fn(),
};

describe("NamespaceDropdown · v4 严格保真", () => {
  beforeEach(() => {
    base.onSelectNamespace.mockReset();
    base.onClose.mockReset();
  });

  it("open=false 时不渲染", () => {
    const { container } = render(<NamespaceDropdown {...base} open={false} />);
    expect(container.firstChild).toBeNull();
  });

  it("v4 DOM：.dropdown / .dd-search input · 「内置」section + .builtin-tag 「不可删」 · Local row NamespaceAvatar folder-git + .dd-nm small 「· ~/.agentloom/local/」+ .dd-ct count", () => {
    const { container } = render(<NamespaceDropdown {...base} />);
    expect(container.querySelector(".dropdown")).not.toBeNull();
    expect(container.querySelector(".dropdown.repo")).toBeNull();
    const search = container.querySelector(
      ".dd-search input",
    ) as HTMLInputElement;
    expect(search.placeholder).toMatch(/搜索 namespace/);
    expect(screen.getByText("内置")).toBeInTheDocument();
    expect(container.querySelector(".builtin-tag")).not.toBeNull();
    expect(screen.getByText("不可删")).toBeInTheDocument();
    const localRow = screen.getByText("Local").closest(".dd-row");
    expect(localRow!.querySelector(".ns-av--loc")).not.toBeNull();
    expect(localRow!.querySelector(".ns-av--loc svg")).not.toBeNull();
    expect(localRow!.querySelector("small")!.textContent).toMatch(
      /~\/\.agentloom\/local\//,
    );
    expect(localRow!.querySelector(".dd-ct")!.textContent).toBe("1");
  });

  it("「GitHub」section · active ns 行 ✓ + count = 该 ns repos 数", () => {
    render(<NamespaceDropdown {...base} />);
    expect(screen.getByText("GitHub")).toBeInTheDocument();
    const orgARow = screen.getByText("myagenthubs").closest(".dd-row");
    expect(orgARow!.classList.contains("active")).toBe(true);
    expect(orgARow!.querySelector(".dd-check")!.textContent).toBe("✓");
    expect(orgARow!.querySelector(".ns-av__badge--gh svg")).not.toBeNull();
    expect(orgARow!.querySelector(".dd-ct")!.textContent).toBe("3");
    expect(orgARow!.querySelector("small")!.textContent).toMatch(/github_org/);
  });

  it("无 github_org namespace 时「GitHub」section 整段隐藏", () => {
    render(<NamespaceDropdown {...base} namespaces={[localNs]} />);
    expect(screen.queryByText("GitHub")).not.toBeInTheDocument();
  });

  it("点 ns 行触发 onSelectNamespace + onClose", () => {
    const onSelectNamespace = vi.fn();
    const onClose = vi.fn();
    render(
      <NamespaceDropdown
        {...base}
        onSelectNamespace={onSelectNamespace}
        onClose={onClose}
      />,
    );
    fireEvent.click(screen.getByText("impanda-cookie"));
    expect(onSelectNamespace).toHaveBeenCalledWith("ns-b");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("search input filter rows · 输入 'imp' 只剩 impanda-cookie", () => {
    const { container } = render(<NamespaceDropdown {...base} />);
    const search = container.querySelector(
      ".dd-search input",
    ) as HTMLInputElement;
    fireEvent.change(search, { target: { value: "imp" } });
    expect(screen.queryByText("myagenthubs")).not.toBeInTheDocument();
    expect(screen.queryByText("Local")).not.toBeInTheDocument();
    expect(screen.getByText("impanda-cookie")).toBeInTheDocument();
  });

  it("footer「连接 GitHub repo」可点 · 触发 onConnectGithub 不触发 onSelectNamespace", () => {
    const onConnectGithub = vi.fn();
    const onSelectNamespace = vi.fn();
    const { container } = render(
      <NamespaceDropdown
        {...base}
        onConnectGithub={onConnectGithub}
        onSelectNamespace={onSelectNamespace}
      />,
    );
    const foot = container.querySelector(".dd-foot");
    expect(foot).not.toBeNull();
    expect(foot!.classList.contains("future")).toBe(false);
    expect(foot!.textContent).toMatch(/连接 GitHub repo/);
    fireEvent.click(foot!);
    expect(onConnectGithub).toHaveBeenCalledTimes(1);
    expect(onSelectNamespace).not.toHaveBeenCalled();
  });

  it("点击管理仓库触发 onManageRepos", () => {
    const onManageRepos = vi.fn();
    render(
      <NamespaceDropdown {...base} open={true} onManageRepos={onManageRepos} />,
    );
    fireEvent.click(screen.getByText("管理仓库"));
    expect(onManageRepos).toHaveBeenCalled();
  });

  it("connectError 渲染对应提示", () => {
    const { getByRole } = render(
      <NamespaceDropdown {...base} connectError="NOT_GITHUB" />,
    );
    expect(getByRole("alert").textContent).toMatch(/不是 GitHub repo/);
  });

  // R3
  it("R3 外点关闭：mousedown outside → onClose", () => {
    const onClose = vi.fn();
    render(<NamespaceDropdown {...base} onClose={onClose} />);
    fireEvent.mouseDown(document.body);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("R3 Esc 关闭：keydown Escape → onClose", () => {
    const onClose = vi.fn();
    render(<NamespaceDropdown {...base} onClose={onClose} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
