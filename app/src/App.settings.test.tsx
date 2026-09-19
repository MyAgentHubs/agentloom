import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import App from "./App";
import { setupAppTests } from "./__tests__/helpers/appTestSetup";

const { invokeMock, listenMock, openMock, sessionMainProps } = vi.hoisted(
  () => ({
    invokeMock: vi.fn(),
    listenMock: vi.fn(),
    openMock: vi.fn(),
    sessionMainProps: [] as Array<{
      onOpenPreview?: (path: string) => void;
      busy?: boolean;
      messages?: Array<{
        role: "user" | "assistant";
        content: unknown[];
        engine?: string;
        agent_id?: string | null;
        agent_name_snapshot?: string | null;
        // V3a：活尾唯一不变量断言需要读这个字段。
        stream_live?: boolean;
      }>;
    }>,
  }),
);

// VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
// deterministically exposes assertions that read state landing from a *different*
// async source than the one they awaited. CI runners are ~12x slower than a dev
// machine and lose those races for real; this switch reproduces it on purpose.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) =>
    process.env.VITEST_DEFER_INVOKE
      ? new Promise((r) => setTimeout(r, 0)).then(() =>
          (invokeMock as (...a: unknown[]) => unknown)(...args),
        )
      : (invokeMock as (...a: unknown[]) => unknown)(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openMock }));
vi.mock("./components/SessionMain", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./components/SessionMain")>();
  const OriginalSessionMain = actual.SessionMain;
  return {
    ...actual,
    SessionMain: (props: ComponentProps<typeof OriginalSessionMain>) => {
      sessionMainProps.push(props);
      return <OriginalSessionMain {...props} />;
    },
  };
});

declare const process: { env: Record<string, string | undefined> };

describe("App", () => {
  const {
    localNamespace,
    localRepo,
    emptyReview,
    agentProfiles,
    mockBasicApp,
    mockRemovableProjectApp,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("点齿轮打开设置 sheet（Agent 池页）", async () => {
    mockBasicApp();
    const { container } = render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "设置" }));

    expect(container.querySelector(".settings-sheet")).not.toBeNull();
    expect(container.querySelector(".shell-bg")?.hasAttribute("inert")).toBe(
      true,
    );
    expect(
      container.querySelector(".project-switcher__gear.active"),
    ).not.toBeNull();
    expect(
      await screen.findByRole("button", { name: "＋ 添加 agent" }),
    ).toBeInTheDocument();
  });

  it("App footer cutover：不再渲染 FooterRepoSelector 使用", async () => {
    mockBasicApp();
    const { container } = render(<App />);

    await screen.findByRole("button", { name: "设置" });
    expect(container.querySelector(".sb-foot .foot-repo")).toBeNull();
    expect(container.querySelector(".sb-foot .repo-btn")).toBeNull();
    expect(container.querySelector(".sb-foot .foot-sys")).toBeNull();
    expect(
      container.querySelector(".sb-foot .project-switcher"),
    ).not.toBeNull();
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "Local 默认",
      ),
    );
    expect(screen.getByRole("button", { name: "设置" })).not.toBeNull();
  });

  it("编辑 local-default 项目时不显示移除项目按钮", async () => {
    mockBasicApp();
    render(<App />);

    fireEvent.click(await screen.findByLabelText("项目切换器"));
    fireEvent.click(await screen.findByRole("button", { name: "编辑项目" }));

    expect(
      await screen.findByRole("dialog", { name: "编辑项目" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "移除项目" }),
    ).not.toBeInTheDocument();
  });

  it("编辑项目点移除先显示含项目名和会话数的确认框，确认后才归档", async () => {
    const projectRepo = mockRemovableProjectApp();
    render(<App />);

    fireEvent.click(await screen.findByLabelText("项目切换器"));
    fireEvent.click(await screen.findByRole("button", { name: "编辑项目" }));
    fireEvent.click(await screen.findByRole("button", { name: "移除项目" }));

    expect(invokeMock).not.toHaveBeenCalledWith("archive_repo", {
      id: projectRepo.id,
    });
    expect(
      screen.getByRole("heading", { name: "移除项目？" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toHaveTextContent(
      "移除「我的小说」？它的 2 个会话会一起隐藏（数据保留·磁盘代码不动），可在 设置 › 已归档项目 恢复。",
    );

    fireEvent.click(screen.getByRole("button", { name: "移除" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("archive_repo", {
        id: projectRepo.id,
      }),
    );
    expect(
      await screen.findByText("Local 默认 · 项目简介"),
    ).toBeInTheDocument();
  });

  it("移除项目确认框点取消不归档", async () => {
    const projectRepo = mockRemovableProjectApp();
    render(<App />);

    fireEvent.click(await screen.findByLabelText("项目切换器"));
    fireEvent.click(await screen.findByRole("button", { name: "编辑项目" }));
    fireEvent.click(await screen.findByRole("button", { name: "移除项目" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));

    await waitFor(() =>
      expect(
        screen.queryByRole("heading", { name: "移除项目？" }),
      ).not.toBeInTheDocument(),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("archive_repo", {
      id: projectRepo.id,
    });
  });

  it("⌘, 打开设置 sheet", async () => {
    mockBasicApp();
    const { container } = render(<App />);

    await screen.findByRole("button", { name: "设置" });
    fireEvent.keyDown(window, { key: ",", metaKey: true });

    await waitFor(() =>
      expect(container.querySelector(".settings-sheet")).not.toBeNull(),
    );
  });

  it("Esc 与点背景关闭设置 sheet", async () => {
    mockBasicApp();
    const { container } = render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "设置" }));
    expect(container.querySelector(".settings-sheet")).not.toBeNull();
    expect(container.querySelector(".shell-bg")?.hasAttribute("inert")).toBe(
      true,
    );
    expect(
      container.querySelector(".project-switcher__gear.active"),
    ).not.toBeNull();

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() =>
      expect(container.querySelector(".settings-sheet")).toBeNull(),
    );
    expect(container.querySelector(".shell-bg")?.hasAttribute("inert")).toBe(
      false,
    );
    expect(
      container.querySelector(".project-switcher__gear.active"),
    ).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    fireEvent.click(container.querySelector(".settings-backdrop")!);
    await waitFor(() =>
      expect(container.querySelector(".settings-sheet")).toBeNull(),
    );
  });

  it("项目切换器管理仓库入口打开设置 sheet 仓库页", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "detect_git")
        return Promise.resolve({
          available: true,
          version: "git",
          path: "/git",
        });
      if (cmd === "detect_gh")
        return Promise.resolve({ available: true, version: "gh", path: "/gh" });
      if (cmd === "gh_accounts")
        return Promise.resolve([{ login: "acme", active: true }]);
      if (cmd === "gh_repo_list") return Promise.resolve([]);
      return Promise.resolve();
    });
    const { container } = render(<App />);

    const trigger = await screen.findByLabelText("项目切换器");
    expect(trigger.closest(".sb-foot")).not.toBeNull();
    fireEvent.click(trigger);
    fireEvent.click(await screen.findByText(/管理 GitHub 仓库/));
    await waitFor(() =>
      expect(container.querySelector(".settings-sheet")).not.toBeNull(),
    );
    expect(screen.getByLabelText("切换账户")).toBeInTheDocument();
  });

  it("打开设置仓库页触发 gh_repo_list·Agent 页不触发（cache 迁移）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "detect_git")
        return Promise.resolve({
          available: true,
          version: "git",
          path: "/git",
        });
      if (cmd === "detect_gh")
        return Promise.resolve({ available: true, version: "gh", path: "/gh" });
      if (cmd === "gh_accounts")
        return Promise.resolve([{ login: "acme", active: true }]);
      if (cmd === "gh_repo_list") return Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));

    fireEvent.click(await screen.findByLabelText("设置"));
    await screen.findByRole("button", { name: "仓库" });
    const ghCallsOnAgents = invokeMock.mock.calls.filter(
      (c) => c[0] === "gh_repo_list",
    ).length;
    expect(ghCallsOnAgents).toBe(0);
    expect(invokeMock.mock.calls.some((c) => c[0] === "detect_gh")).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "仓库" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("gh_repo_list", {
        login: "acme",
      }),
    );
    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "gh_repo_list").length,
    ).toBeGreaterThan(ghCallsOnAgents);
  });

  it("仓库页检测到无 gh 时显示安装引导且不显示读取仓库 spinner", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "detect_git")
        return Promise.resolve({
          available: true,
          version: "git",
          path: "/git",
        });
      if (cmd === "detect_gh")
        return Promise.resolve({ available: false, version: null, path: null });
      if (cmd === "detect_brew") return Promise.resolve(false);
      return Promise.resolve();
    });
    render(<App />);

    fireEvent.click(await screen.findByLabelText("设置"));
    fireEvent.click(screen.getByRole("button", { name: "仓库" }));

    expect(await screen.findByText("需要 GitHub CLI (gh)")).toBeInTheDocument();
    expect(screen.queryByLabelText("正在读取仓库")).toBeNull();
    expect(invokeMock.mock.calls.some((c) => c[0] === "gh_accounts")).toBe(
      false,
    );
    expect(invokeMock.mock.calls.some((c) => c[0] === "gh_repo_list")).toBe(
      false,
    );
  });

  it("仓库页检测到无 Git 时显示依赖提示且不调用 gh", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "detect_git")
        return Promise.resolve({ available: false, version: null, path: null });
      if (cmd === "detect_gh")
        return Promise.resolve({ available: true, version: "gh", path: "/gh" });
      return Promise.resolve();
    });
    render(<App />);

    fireEvent.click(await screen.findByLabelText("设置"));
    fireEvent.click(screen.getByRole("button", { name: "仓库" }));

    expect(await screen.findByText("需要 Git")).toBeInTheDocument();
    expect(screen.queryByLabelText("正在读取仓库")).toBeNull();
    expect(invokeMock.mock.calls.some((c) => c[0] === "gh_accounts")).toBe(
      false,
    );
    expect(invokeMock.mock.calls.some((c) => c[0] === "gh_repo_list")).toBe(
      false,
    );
  });

  it("点设置「联网搜索」nav 渲染 SettingsSearch", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "get_active_backend") return Promise.resolve("brave");
      if (cmd === "get_search_key") return Promise.resolve(false);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));

    fireEvent.click(await screen.findByLabelText("设置"));
    fireEvent.click(await screen.findByRole("button", { name: "联网搜索" }));
    expect(
      await screen.findByRole("form", { name: "搜索服务设置" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("搜索服务")).toBeInTheDocument();
  });
});
