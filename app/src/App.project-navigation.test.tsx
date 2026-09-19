import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
} from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import type { Session } from "./types/agent";
import { makeSession } from "./test/factories";
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
    githubNamespace,
    githubRepo,
    agentProfiles,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("send_message 返 PROJECT_INVALID:<id> → 弹 InvalidProjectDialog 不污染消息流", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "r-inv",
              source: "local",
              owner: null,
              name: "moved-proj",
              path: "/nowhere",
              status: "active",
              added_at: 1,
              last_used_at: null,
              namespace_id: "local",
            },
          ],
        });
      if (cmd === "send_message")
        return Promise.reject("PROJECT_INVALID:r-inv");
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() =>
      expect(screen.getByText(/路径已无效/)).toBeInTheDocument(),
    );
    expect(screen.queryByText(/\[启动失败\]/)).not.toBeInTheDocument();

    // 首条消息发送会触发后台 rename_session → refreshSessions 的
    // fire-and-forget 链路（onSend 不 await 它，产品上是有意的非阻塞行为，
    // 与 send_message 是否失败无关——标题先按输入内容改名）。测试须等它落定，
    // 否则 unmount 后才 resolve 的 setSessions 会打出 act() 警告（偶发升级
    // 成 AggregateError、曾把下一个用例的 cleanup 一并带崩）。
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("rename_session", {
        id: "s1",
        title: expect.any(String),
      }),
    );
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "list_sessions").length,
      ).toBeGreaterThanOrEqual(2),
    );
  });

  it("send_message 返 ALREADY_ADDED:<id> → toast 提示 + 自动切到 intro view", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "r-x",
              source: "local",
              owner: null,
              name: "existing",
              path: "/x",
              status: "active",
              added_at: 1,
              last_used_at: null,
              namespace_id: "local",
            },
          ],
        });
      if (cmd === "send_message") return Promise.reject("ALREADY_ADDED:r-x");
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() =>
      expect(screen.getByText("已在列表 · 已切到该项目")).toBeInTheDocument(),
    );

    // 首条消息发送会触发后台 rename_session → refreshSessions 的
    // fire-and-forget 链路（onSend 不 await 它，产品上是有意的非阻塞行为，
    // 与 send_message 是否失败无关——标题先按输入内容改名）。测试须等它落定，
    // 否则 unmount 后才 resolve 的 setSessions 会打出 act() 警告（偶发升级
    // 成 AggregateError）。
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("rename_session", {
        id: "s1",
        title: expect.any(String),
      }),
    );
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "list_sessions").length,
      ).toBeGreaterThanOrEqual(2),
    );
  });

  it("Task 6：点项目切换器展开 repo 列表但不点击不触发切换", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "默认会话",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "r-pick",
              source: "local",
              owner: null,
              name: "20260527",
              path: "/x/20260527",
              status: "active",
              added_at: 1,
              last_used_at: null,
              namespace_id: "local",
            },
          ],
        });
      if (cmd === "list_repos")
        return Promise.resolve([
          {
            id: "r-pick",
            source: "local",
            owner: null,
            name: "20260527",
            path: "/x/20260527",
            status: "active",
            added_at: 1,
            last_used_at: null,
            namespace_id: "local",
          },
        ]);
      if (cmd === "list_repos_by_status") return Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByLabelText("项目切换器"));
    await waitFor(() =>
      expect(document.querySelector(".repo-switcher")).not.toBeNull(),
    );
    expect(screen.getAllByText("项目").length).toBeGreaterThanOrEqual(1);
    const rows = document.querySelectorAll(".repo-switcher .dd-row");
    expect(rows.length).toBeGreaterThan(0);
    expect(
      Array.from(rows).some((r) => r.textContent?.includes("20260527")),
    ).toBe(true);

    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "set_active_namespace"),
    ).toHaveLength(0);
    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "set_last_active_repo"),
    ).toHaveLength(0);
  });

  it("connect github：选目录 → IPC → 切到新 ns 并选中新 repo", async () => {
    openMock.mockReset();
    openMock.mockResolvedValue("/code/foo");
    const localNs = {
      id: "local",
      kind: "local",
      name: "Local",
      is_builtin: 1,
      last_active_repo_id: null,
      added_at: 0,
      last_used_at: null,
    };
    const ghNs = {
      id: "gh:acme",
      kind: "github_org",
      name: "acme",
      is_builtin: 0,
      last_active_repo_id: "r-new",
      added_at: 0,
      last_used_at: null,
    };
    const localRepo = {
      id: "local-default",
      namespace_id: "local",
      source: "local",
      owner: null,
      name: "Local",
      path: "/tmp/x",
      status: "active",
      added_at: 0,
      last_used_at: null,
    };
    const ghRepo = {
      id: "r-new",
      namespace_id: "gh:acme",
      source: "github",
      owner: "acme",
      name: "foo",
      path: "/code/foo",
      status: "active",
      added_at: 0,
      last_used_at: null,
    };
    let reposCall = 0;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNs],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "list_namespaces") return Promise.resolve([localNs, ghNs]);
      if (cmd === "gh_accounts")
        return Promise.resolve([{ login: "acme", active: true }]);
      if (cmd === "gh_repo_list") return Promise.resolve([]);
      if (cmd === "list_repos") {
        reposCall++;
        return Promise.resolve(
          reposCall <= 1 ? [localRepo] : [localRepo, ghRepo],
        );
      }
      if (cmd === "connect_github_repo")
        return Promise.resolve({ namespace_id: "gh:acme", repo_id: "r-new" });
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));

    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText(/管理 GitHub 仓库/));
    fireEvent.click(
      await screen.findByRole("button", { name: "添加本地已克隆的仓库" }),
    );
    await waitFor(() =>
      expect(openMock).toHaveBeenCalledWith(
        expect.objectContaining({ directory: true }),
      ),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("connect_github_repo", {
        path: "/code/foo",
      }),
    );
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("foo"),
    );
    await waitFor(() => {
      const activeRow = document.querySelector(".repo-switcher .dd-row.on");
      expect(activeRow?.querySelector(".dd-row-nm")?.textContent).toBe("foo");
      expect(
        activeRow?.closest(".rsw-group")?.querySelector(".dd-sec-nm")
          ?.textContent,
      ).toContain("acme");
    });
  });

  it("Task 9：activeRepoId 为 null 时新建按钮 disabled", async () => {
    let sessionsState: Session[] = [
      makeSession({
        id: "s1",
        title: "默认会话",
        repo_id: null,
        namespace_id: "local",
      }),
    ];

    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([...sessionsState]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: null,
          repos: [],
        });
      if (cmd === "list_repos") return Promise.resolve([]);
      if (cmd === "list_repos_by_status") return Promise.resolve([]);
      if (cmd === "create_session") {
        sessionsState = [
          ...sessionsState,
          makeSession({
            id: args.id,
            title: args.title,
            repo_id: null,
            namespace_id: "local",
          }),
        ];
        return Promise.resolve();
      }
      if (cmd === "update_session_repo") {
        sessionsState = sessionsState.map((s) =>
          s.id === args.sessionId ? { ...s, repo_id: args.repoId } : s,
        );
        return Promise.resolve();
      }
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const newBtn = screen.getByRole("button", { name: /新会话/ });
    expect(newBtn).toBeDisabled();
    expect(newBtn).toHaveAttribute(
      "title",
      expect.stringMatching(/请先添加 repo/),
    );
  });

  it("点 sidebar 里的 unbound session · openSession 同步 activeRepoId 为 null", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s-default",
            title: "默认会话",
            repo_id: "r-x",
            namespace_id: "local",
          }),
          makeSession({
            id: "s-other",
            title: "what's up",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "r-x",
          repos: [
            {
              id: "r-x",
              source: "local",
              owner: null,
              name: "20260527",
              path: "/x/20260527",
              status: "active",
              added_at: 1,
              last_used_at: null,
              namespace_id: "local",
            },
          ],
        });
      if (cmd === "list_repos")
        return Promise.resolve([
          {
            id: "r-x",
            source: "local",
            owner: null,
            name: "20260527",
            path: "/x/20260527",
            status: "active",
            added_at: 1,
            last_used_at: null,
            namespace_id: "local",
          },
        ]);
      if (cmd === "list_repos_by_status") return Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s-default",
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    fireEvent.click(screen.getByText("what's up"));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s-other",
      }),
    );
    expect(screen.getByRole("button", { name: /新会话/ })).toBeDisabled();
  });

  it("R4-1：切 namespace 清 currentId + messages + view → intro", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "x",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          { role: "user", content: [{ type: "text", text: "hi" }] },
        ]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
            {
              id: "ns-a",
              kind: "github_org",
              name: "myagenthubs",
              is_builtin: 0,
              last_active_repo_id: null,
              added_at: 100,
              last_used_at: 200,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
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
          ],
        });
      if (cmd === "list_repos")
        return Promise.resolve([
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
            id: "r-a",
            source: "github",
            owner: null,
            name: "agentloom",
            path: "/tmp/a",
            status: "active",
            added_at: 0,
            last_used_at: null,
            namespace_id: "ns-a",
          },
          {
            id: "r-b",
            source: "github",
            owner: null,
            name: "my-blog",
            path: "/tmp/b",
            status: "active",
            added_at: 0,
            last_used_at: null,
            namespace_id: "ns-a",
          },
        ]);
      if (cmd === "set_active_namespace") return Promise.resolve("r-a");
      if (cmd === "set_last_active_repo") return Promise.resolve();
      if (cmd === "list_namespaces")
        return Promise.resolve([
          {
            id: "local",
            kind: "local",
            name: "Local",
            is_builtin: 1,
            last_active_repo_id: "local-default",
            added_at: 0,
            last_used_at: null,
          },
          {
            id: "ns-a",
            kind: "github_org",
            name: "myagenthubs",
            is_builtin: 0,
            last_active_repo_id: "r-a",
            added_at: 100,
            last_used_at: 200,
          },
        ]);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.click(screen.getByLabelText("项目切换器"));
    await waitFor(() =>
      expect(screen.getByText("myagenthubs")).toBeInTheDocument(),
    );
    const agentloomRow = Array.from(
      document.querySelectorAll(".repo-switcher .dd-row"),
    ).find((row) => row.textContent?.includes("agentloom")) as Element;
    expect(agentloomRow).toBeTruthy();
    await act(async () => {
      fireEvent.click(agentloomRow);
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_last_active_repo",
        expect.objectContaining({ namespaceId: "ns-a", repoId: "r-a" }),
      ),
    );
    const setActiveNsCall = invokeMock.mock.calls.findIndex(
      (c) => c[0] === "set_active_namespace" && c[1]?.id === "ns-a",
    );
    const setLastRepoCall = invokeMock.mock.calls.findIndex(
      (c) =>
        c[0] === "set_last_active_repo" &&
        c[1]?.namespaceId === "ns-a" &&
        c[1]?.repoId === "r-a",
    );
    expect(invokeMock.mock.invocationCallOrder[setActiveNsCall]).toBeLessThan(
      invokeMock.mock.invocationCallOrder[setLastRepoCall],
    );
    // view 切到 intro · main 不渲染冗余 meta 行
    await waitFor(() => {
      const removedMetaClass = ["session", "meta"].join("-");
      expect(document.querySelector(`.${removedMetaClass}`)).toBeNull();
    });
  });

  it("R4-2：set_active_namespace IPC 失败 → 不切 ns（保留 activeNamespaceId）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "x",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
            {
              id: "ns-a",
              kind: "github_org",
              name: "ns-x",
              is_builtin: 0,
              last_active_repo_id: "r-a",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
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
              id: "r-a",
              source: "github",
              owner: null,
              name: "agentloom",
              path: "/tmp/a",
              status: "active",
              added_at: 0,
              last_used_at: null,
              namespace_id: "ns-a",
            },
          ],
        });
      if (cmd === "list_repos")
        return Promise.resolve([
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
            id: "r-a",
            source: "github",
            owner: null,
            name: "agentloom",
            path: "/tmp/a",
            status: "active",
            added_at: 0,
            last_used_at: null,
            namespace_id: "ns-a",
          },
        ]);
      if (cmd === "set_active_namespace")
        return Promise.reject("NAMESPACE_NOT_FOUND:ns-a");
      if (cmd === "list_namespaces")
        return Promise.resolve([
          {
            id: "local",
            kind: "local",
            name: "Local",
            is_builtin: 1,
            last_active_repo_id: "local-default",
            added_at: 0,
            last_used_at: null,
          },
          {
            id: "ns-a",
            kind: "github_org",
            name: "ns-x",
            is_builtin: 0,
            last_active_repo_id: "r-a",
            added_at: 0,
            last_used_at: null,
          },
        ]);
      return Promise.resolve();
    });
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.click(screen.getByLabelText("项目切换器"));
    await waitFor(() => expect(screen.getByText("ns-x")).toBeInTheDocument());
    const agentloomRow = Array.from(
      document.querySelectorAll(".repo-switcher .dd-row"),
    ).find((row) => row.textContent?.includes("agentloom")) as Element;
    expect(agentloomRow).toBeTruthy();
    await act(async () => {
      fireEvent.click(agentloomRow);
    });
    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "set_last_active_repo"),
    ).toHaveLength(0);
    // 失败：项目切换器仍指向 Local 默认
    await waitFor(() => {
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "Local 默认",
      );
    });
    errSpy.mockRestore();
  });

  it("R4-3：0 repo namespace 时 Sidebar 「+ 新会话」disabled + title 含「请先添加 repo」", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "ns-empty",
              kind: "github_org",
              name: "empty",
              is_builtin: 0,
              last_active_repo_id: null,
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "ns-empty",
          active_repo_id: null,
          repos: [],
        });
      if (cmd === "list_repos") return Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() => {
      const add = document.querySelector(".sb-grp__add") as HTMLButtonElement;
      expect(add).not.toBeNull();
      expect(add.disabled).toBe(true);
      expect(add.title).toMatch(/请先添加 repo/);
    });
  });

  it("R4-4：create_session 在 activeRepoId === null 时不调用（防御）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "x",
            repo_id: null,
            namespace_id: "ns-empty",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "ns-empty",
              kind: "github_org",
              name: "empty",
              is_builtin: 0,
              last_active_repo_id: null,
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "ns-empty",
          active_repo_id: null,
          repos: [],
        });
      if (cmd === "list_repos") return Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));
    // s1 已存在 · 不该再 create_session
    const createCalls = invokeMock.mock.calls.filter(
      (c) => c[0] === "create_session",
    );
    expect(createCalls.length).toBe(0);
  });

  it("REPO_NAMESPACE_MISMATCH 回归：新建会话 namespaceId 跟随 active repo", async () => {
    const ghNamespace = {
      ...githubNamespace,
      id: "gh:acme",
      name: "acme",
      last_active_repo_id: "gh-acme-repo",
    };
    const ghRepo = {
      ...githubRepo,
      id: "gh-acme-repo",
      owner: "acme",
      name: "repo",
      namespace_id: "gh:acme",
    };
    const sameNameLocalRepo = {
      ...localRepo,
      id: "local-same-name",
      name: "repo",
    };
    let sessionsState: Session[] = [
      makeSession({
        id: "archived",
        title: "归档会话",
        repo_id: ghRepo.id,
        namespace_id: ghRepo.namespace_id,
        archived: true,
        archived_at: 1,
      }),
    ];

    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([...sessionsState]);
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace, ghNamespace],
          active_namespace_id: "local",
          active_repo_id: ghRepo.id,
          repos: [ghRepo],
        });
      if (cmd === "list_repos")
        return Promise.resolve([sameNameLocalRepo, ghRepo]);
      if (cmd === "create_session") {
        sessionsState = [
          ...sessionsState,
          makeSession({
            id: args.id,
            title: args.title,
            repo_id: args.repoId,
            namespace_id: args.namespaceId,
          }),
        ];
        return Promise.resolve();
      }
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("repo");
    });
    const newButton = screen.getByRole("button", { name: /新会话/ });
    expect(newButton).not.toBeDisabled();

    fireEvent.click(newButton);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "create_session",
        expect.objectContaining({
          repoId: ghRepo.id,
          namespaceId: "gh:acme",
        }),
      ),
    );
  });

  it("R4-5：openSession 跨 namespace 时同步 activeNamespaceId + 刷 list_repos · crumb 切到他 ns", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "本 ns",
            repo_id: "local-default",
            namespace_id: "local",
          }),
          makeSession({
            id: "s2",
            title: "他 ns",
            repo_id: "r-a",
            namespace_id: "ns-a",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
            {
              id: "ns-a",
              kind: "github_org",
              name: "myagenthubs",
              is_builtin: 0,
              last_active_repo_id: "r-a",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
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
          ],
        });
      if (cmd === "list_repos")
        return Promise.resolve([
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
            id: "r-a",
            source: "github",
            owner: null,
            name: "agentloom",
            path: "/tmp/a",
            status: "active",
            added_at: 0,
            last_used_at: null,
            namespace_id: "ns-a",
          },
        ]);
      return Promise.resolve();
    });
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    expect(screen.getByLabelText("项目切换器")).toHaveTextContent("Local 默认");
    // 切总览 → 点 s2 跨 ns 打开
    fireEvent.click(screen.getByLabelText("总览"));
    await waitFor(() => expect(screen.getByText("他 ns")).toBeInTheDocument());
    await act(async () => {
      fireEvent.click(screen.getByText("他 ns"));
    });
    // openSession 跨 ns 触发 list_repos 第 2 次（启动 1 + 跨 ns 1）
    await waitFor(() => {
      const calls = invokeMock.mock.calls.filter((c) => c[0] === "list_repos");
      expect(calls.length).toBeGreaterThanOrEqual(2);
    });
    // 项目切换器切到 myagenthubs namespace 的 active repo
    await waitFor(() => {
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "agentloom",
      );
    });
  });
});
