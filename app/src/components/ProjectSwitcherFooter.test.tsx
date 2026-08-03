import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { I18nProvider } from "../i18n";
import type { NamespaceMeta, RepoMeta, Session } from "../types/agent";
import { ProjectSwitcherFooter } from "./ProjectSwitcherFooter";

const localNs: NamespaceMeta = {
  id: "local",
  name: "Local",
  kind: "local",
  is_builtin: 1,
  last_active_repo_id: "local-default",
  added_at: 0,
  last_used_at: null,
};

const ghNs: NamespaceMeta = {
  id: "gh:acme",
  name: "acme",
  kind: "github_org",
  is_builtin: 0,
  last_active_repo_id: "r-web",
  added_at: 0,
  last_used_at: null,
};

function repo(
  id: string,
  name: string,
  ns: string,
  source = "github",
): RepoMeta {
  return {
    id,
    name,
    source,
    owner: source === "github" ? "acme" : null,
    path: `/tmp/${name}`,
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: ns,
    icon: source === "local" ? "📊" : null,
  };
}

const localRepo = repo("local-default", "我的项目", "local", "local");
const webRepo = repo("r-web", "web", "gh:acme");
const apiRepo = repo("r-api", "api", "gh:acme");

function baseProps() {
  return {
    activeNamespace: ghNs,
    activeRepo: webRepo,
    namespaces: [localNs, ghNs],
    allRepos: [localRepo, webRepo, apiRepo],
    sessions: [] as Session[],
    activeNamespaceId: "gh:acme",
    activeRepoId: "r-web",
    onSelectRepoInNamespace: vi.fn(),
    onManageRepos: vi.fn(),
    onSettings: vi.fn(),
    settingsActive: false,
  };
}

describe("ProjectSwitcherFooter", () => {
  it("显示当前 GitHub repo 名和设置按钮", () => {
    const { container } = render(<ProjectSwitcherFooter {...baseProps()} />);
    expect(screen.getByLabelText("项目切换器")).toHaveTextContent("web");
    expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument();
    expect(container.querySelector(".project-switcher")).not.toBeNull();
  });

  it("local-default 哨兵名称在中文显示为「我的项目」", () => {
    render(
      <ProjectSwitcherFooter
        {...baseProps()}
        activeNamespace={localNs}
        activeRepo={localRepo}
        activeNamespaceId="local"
        activeRepoId="local-default"
      />,
    );
    expect(screen.getByLabelText("项目切换器")).toHaveTextContent("我的项目");
  });

  it("local-default 哨兵名称在英文显示为 My Project", () => {
    render(
      <I18nProvider initialLocale="en">
        <ProjectSwitcherFooter
          {...baseProps()}
          activeNamespace={localNs}
          activeRepo={localRepo}
          activeNamespaceId="local"
          activeRepoId="local-default"
        />
      </I18nProvider>,
    );
    expect(screen.getByLabelText("Project switcher")).toHaveTextContent(
      "My Project",
    );
  });

  it("local-default 用户改名后显示真实名称", () => {
    const renamedRepo = { ...localRepo, name: "长篇小说" };
    render(
      <ProjectSwitcherFooter
        {...baseProps()}
        activeNamespace={localNs}
        activeRepo={renamedRepo}
        activeNamespaceId="local"
        activeRepoId="local-default"
      />,
    );
    expect(screen.getByLabelText("项目切换器")).toHaveTextContent("长篇小说");
  });

  it("本地项目显示 repo.icon emoji，不显示 namespace 头像", () => {
    const { container } = render(
      <ProjectSwitcherFooter
        {...baseProps()}
        activeNamespace={localNs}
        activeRepo={localRepo}
        activeNamespaceId="local"
        activeRepoId="local-default"
      />,
    );
    expect(screen.getByTestId("local-project-icon")).toHaveTextContent("📊");
    expect(container.querySelector(".projsw .ns-av")).toBeNull();
  });

  it("本地项目未设置 icon 时显示默认 📁", () => {
    render(
      <ProjectSwitcherFooter
        {...baseProps()}
        activeNamespace={localNs}
        activeRepo={{ ...localRepo, icon: null }}
        activeNamespaceId="local"
        activeRepoId="local-default"
      />,
    );
    expect(screen.getByTestId("local-project-icon")).toHaveTextContent("📁");
  });

  it("点击项目切换器向上打开 RepoSwitcherDropdown", () => {
    const { container } = render(<ProjectSwitcherFooter {...baseProps()} />);
    fireEvent.click(screen.getByLabelText("项目切换器"));
    expect(container.querySelector(".project-switcher.open")).not.toBeNull();
    expect(
      container.querySelector(".project-switcher .repo-switcher"),
    ).not.toBeNull();
    expect(
      screen.getByPlaceholderText("搜索项目或 owner…"),
    ).toBeInTheDocument();
  });

  it("选择 repo 透传 onSelectRepoInNamespace 并关闭", () => {
    const p = baseProps();
    const { container } = render(<ProjectSwitcherFooter {...p} />);
    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("api"));
    expect(p.onSelectRepoInNamespace).toHaveBeenCalledWith("gh:acme", "r-api");
    expect(container.querySelector(".repo-switcher")).toBeNull();
  });

  it("底部管理 GitHub 仓库入口打开仓库管理页", () => {
    const p = baseProps();
    render(<ProjectSwitcherFooter {...p} />);
    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("管理 GitHub 仓库"));
    expect(p.onManageRepos).toHaveBeenCalledTimes(1);
  });

  it("设置齿轮触发 onSettings", () => {
    const p = baseProps();
    render(<ProjectSwitcherFooter {...p} />);
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    expect(p.onSettings).toHaveBeenCalledTimes(1);
  });
});
