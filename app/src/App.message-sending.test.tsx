import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
} from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import type { AgentProfile, Session } from "./types/agent";
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
    emptyReview,
    agentProfile,
    agentProfiles,
    mockBasicApp,
    agentEventCb,
    emitAgentEventBatch,
    deferred,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("切换动态 agent 后发送 agentId，completed 不再前端补写 append_message（刀 R R3：已后端归约器持久化）", async () => {
    const { sendCalls } = mockBasicApp();
    render(<App />);

    expect(await screen.findByText("Claude Code")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /DeepSeek/ }));

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "hello" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() =>
      expect(sendCalls).toEqual([
        {
          sessionId: "s1",
          agentId: "deepseek",
          message: "hello",
          criteria: [],
        },
      ]),
    );

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    expect(handler).toBeTruthy();
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: "done",
        },
      });
    });

    // 刀 R R3：completed 的过程持久化已后端归约器（display_reduce）完成，前端不再补写 append_message（消双写）。
    await waitFor(() => expect(screen.getByText("done")).toBeInTheDocument());
    expect(invokeMock.mock.calls.some((c) => c[0] === "append_message")).toBe(
      false,
    );
  });

  it("Normal 前端创建的 user/assistant 均分配 client id，流式尾增长不 remount", async () => {
    const uuidSpy = vi
      .spyOn(crypto, "randomUUID")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000001")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000002");
    const { sendCalls } = mockBasicApp();
    const { container } = render(<App />);

    await screen.findByText("Claude Code");
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "stream" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(uuidSpy).toHaveBeenCalledTimes(2);
    const assistantTurn = container.querySelector(".turn--assistant");
    expect(assistantTurn).not.toBeNull();

    const handler = listenMock.mock.calls.find(
      (call) => call[0] === "agent-event",
    )?.[1];
    expect(handler).toBeTruthy();
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "text_delta",
          text: "流式内容",
        },
      });
    });

    await screen.findByText("流式内容");
    expect(screen.getByText("流式内容").closest(".turn")).toBe(assistantTurn);
  });

  it("Normal completed 后切换会话往返仍保留累计 token", async () => {
    mockBasicApp(agentProfiles, {
      session: { total_input_tokens: 11, total_output_tokens: 17 },
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
            total_input_tokens: 11,
            total_output_tokens: 17,
          }),
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      return defaultInvoke?.(cmd, args);
    });
    const { container } = render(<App />);

    expect(await screen.findByText("Claude Code")).toBeInTheDocument();
    await waitFor(() =>
      expect(container.querySelector(".composer__hint-cost")).toHaveTextContent(
        "全程 28 tok",
      ),
    );

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "hello" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "list_sessions"),
      ).toHaveLength(2),
    );

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: 7,
          output_tokens: 13,
          final_text: "done",
        },
      });
    });

    await waitFor(() => {
      const status = container.querySelector(".composer__hint-cost");
      expect(status).toHaveTextContent("全程 48 tok");
      expect(status).toHaveAttribute("title", "↑ 18 · ↓ 30");
    });

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(
        container.querySelector(".sf-session-title__text"),
      ).toHaveTextContent("会话二"),
    );
    fireEvent.click(screen.getByText("会话一"));
    await waitFor(() => {
      const status = container.querySelector(".composer__hint-cost");
      expect(status).toHaveTextContent("全程 48 tok");
      expect(status).toHaveAttribute("title", "↑ 18 · ↓ 30");
    });
  });

  it("intro currentId=null 仍能发送", async () => {
    const introAgents: AgentProfile[] = [
      agentProfile({
        id: "claude",
        name: "Claude",
        provider: "claude",
        sort_order: 0,
      }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 1,
      }),
    ];
    const repoWeb = {
      id: "r-web",
      source: "local" as const,
      owner: null,
      name: "web",
      path: "/tmp/web",
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: "local",
    };
    const repoApi = {
      id: "r-api",
      source: "local" as const,
      owner: null,
      name: "api",
      path: "/tmp/api",
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: "local",
    };
    let sessionsState: Session[] = [
      makeSession({
        id: "sw1",
        title: "web 会话",
        repo_id: "r-web",
        namespace_id: "local",
      }),
    ];
    const sendCalls: any[] = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(introAgents);
      if (cmd === "list_sessions") return Promise.resolve([...sessionsState]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "r-web",
          repos: [repoWeb, repoApi],
        });
      if (cmd === "list_repos") return Promise.resolve([repoWeb, repoApi]);
      if (cmd === "list_namespaces") return Promise.resolve([localNamespace]);
      if (cmd === "set_active_namespace") return Promise.resolve("r-api");
      if (cmd === "set_last_active_repo") return Promise.resolve();
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
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "detect_gh")
        return Promise.resolve({ available: false, version: null, path: null });
      if (cmd === "detect_brew") return Promise.resolve(false);
      if (cmd === "gh_accounts") return Promise.resolve([]);
      if (cmd === "send_message") {
        sendCalls.push(args);
        return Promise.resolve();
      }
      if (cmd === "append_message") return Promise.resolve();
      return Promise.resolve(null);
    });

    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "sw1",
      }),
    );

    fireEvent.click(screen.getByLabelText("项目切换器"));
    await waitFor(() =>
      expect(container.querySelector(".repo-switcher")).not.toBeNull(),
    );
    const apiRow = Array.from(
      container.querySelectorAll(".repo-switcher .dd-row"),
    ).find((row) => row.textContent?.includes("api")) as Element;
    expect(apiRow).toBeTruthy();
    await act(async () => {
      fireEvent.click(apiRow);
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_last_active_repo",
        expect.objectContaining({ repoId: "r-api" }),
      ),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_active_namespace", {
        id: "local",
      }),
    );
    const setActiveNsCall = invokeMock.mock.calls.findIndex(
      (c) => c[0] === "set_active_namespace" && c[1]?.id === "local",
    );
    const setLastRepoCall = invokeMock.mock.calls.findIndex(
      (c) =>
        c[0] === "set_last_active_repo" &&
        c[1]?.namespaceId === "local" &&
        c[1]?.repoId === "r-api",
    );
    expect(invokeMock.mock.invocationCallOrder[setActiveNsCall]).toBeLessThan(
      invokeMock.mock.invocationCallOrder[setLastRepoCall],
    );
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "api" })).toBeInTheDocument(),
    );

    expect(screen.getByLabelText(/选择 agent/)).not.toHaveTextContent("…");
    fireEvent.click(screen.getByLabelText(/选择 agent/));
    const deepseekItem = screen.getByRole("menuitemradio", {
      name: "DeepSeek",
    });
    expect(deepseekItem).not.toBeDisabled();
    fireEvent.click(deepseekItem);
    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).toHaveTextContent("DeepSeek"),
    );

    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "intro 发送" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(sendCalls[0].agentId).toBe("deepseek");
  });

  it("intro 新会话 Team 发送先把当前 selector lead/member 配置写入新 session", async () => {
    const introAgents: AgentProfile[] = [
      agentProfile({
        id: "claude",
        name: "Claude Lead",
        provider: "claude",
        access: "native",
        cap_lead: "planner",
        sort_order: 0,
      }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 1,
      }),
    ];
    const configStore = new Map<
      string,
      { leadId: string | null; rosterIds: string[] }
    >([["s1", { leadId: null, rosterIds: [] }]]);
    let sessionsState: Session[] = [
      makeSession({
        id: "s1",
        title: "会话一",
        repo_id: "local-default",
        namespace_id: "local",
      }),
    ];

    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(introAgents);
      if (cmd === "list_sessions") return Promise.resolve([...sessionsState]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "get_session_agent_config") {
        const cfg = configStore.get(args.sessionId) ?? {
          leadId: null,
          rosterIds: [],
        };
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: cfg.leadId,
          member_agent_ids: cfg.rosterIds,
        });
      }
      if (cmd === "set_session_agent_config") {
        const cfg = {
          leadId: args.leadAgentId ?? null,
          rosterIds: [...(args.memberAgentIds ?? [])],
        };
        configStore.set(args.sessionId, cfg);
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: cfg.leadId,
          member_agent_ids: cfg.rosterIds,
        });
      }
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
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: { action: "reply", rationale: "问答" },
          decisionCard: null,
        });
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "detect_gh")
        return Promise.resolve({ available: false, version: null, path: null });
      if (cmd === "detect_brew") return Promise.resolve(false);
      if (cmd === "gh_accounts") return Promise.resolve([]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "send_message") return Promise.resolve();
      return Promise.resolve(null);
    });

    render(<App />);
    await screen.findByText("Claude Lead");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByText("项目简介"));
    await screen.findByRole("heading", { name: "Local 默认" });

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Claude Lead" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Claude Lead" }),
    );
    expect(
      screen.getByRole("button", {
        name: /选择 agent：队长 Claude Lead，成员 1/,
      }),
    ).toBeInTheDocument();
    expect(
      invokeMock.mock.calls.some(
        ([cmd, args]) =>
          cmd === "set_session_agent_config" && args?.sessionId === "s1",
      ),
    ).toBe(false);

    let memberToggle = screen.queryByRole("button", { name: "成员 DeepSeek" });
    if (!memberToggle) {
      fireEvent.click(
        screen.getByRole("button", {
          name: /选择 agent：队长 Claude Lead，成员 1/,
        }),
      );
      memberToggle = screen.getByRole("button", { name: "成员 DeepSeek" });
    }
    fireEvent.click(memberToggle);
    expect(
      screen.getByRole("button", {
        name: /选择 agent：队长 Claude Lead，成员 0/,
      }),
    ).toBeInTheDocument();
    expect(
      invokeMock.mock.calls.some(
        ([cmd, args]) =>
          cmd === "set_session_agent_config" && args?.sessionId === "s1",
      ),
    ).toBe(false);

    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "intro team send" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "create_session",
        expect.objectContaining({ title: "新会话" }),
      ),
    );
    const createCall = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "create_session",
    );
    const newSessionId = createCall?.[1]?.id;
    expect(newSessionId).toBeTruthy();
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
        sessionId: newSessionId,
        leadAgentId: "claude",
        memberAgentIds: [],
      }),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "start_lead_session",
        expect.objectContaining({
          sessionId: newSessionId,
          leadAgentId: "claude",
          message: "intro team send",
          memberIds: [],
        }),
      ),
    );

    const newConfigCall = invokeMock.mock.calls.findIndex(
      ([cmd, args]) =>
        cmd === "set_session_agent_config" && args?.sessionId === newSessionId,
    );
    const startLeadCall = invokeMock.mock.calls.findIndex(
      ([cmd, args]) =>
        cmd === "start_lead_session" && args?.sessionId === newSessionId,
    );
    expect(newConfigCall).toBeGreaterThanOrEqual(0);
    expect(startLeadCall).toBeGreaterThan(newConfigCall);
  });

  it("Phase 3 C2-B Task 1 · 启动自动建 session 后立即出现在 sidebar · 新建按钮不被误置灰", async () => {
    let sessionsState: Session[] = [];

    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([...sessionsState]);
      if (cmd === "create_session") {
        sessionsState = [
          ...sessionsState,
          makeSession({
            id: args.id,
            title: "首个 Local 会话",
            repo_id: args.repoId,
            namespace_id: args.namespaceId,
          }),
        ];
        return Promise.resolve();
      }
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
        ]);
      return Promise.resolve();
    });

    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: sessionsState[0]?.id,
      }),
    );

    await waitFor(() =>
      expect(
        Array.from(container.querySelectorAll(".sess__nm")).map(
          (node) => node.textContent,
        ),
      ).toContain("首个 Local 会话"),
    );
    expect(screen.getByRole("button", { name: /新会话/ })).not.toBeDisabled();
  });

  it("send_message 失败时把错误显示在消息流里", async () => {
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
      if (cmd === "send_message")
        return Promise.reject("未设置 DEEPSEEK_API_KEY 环境变量");
      return Promise.resolve();
    });
    await act(async () => {
      render(<App />);
    });

    // 等初始化把会话 id 就位（get_messages 已被调用）再发消息
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    await act(async () => {
      fireEvent.change(input, { target: { value: "test" } });
      fireEvent.keyDown(input, { key: "Enter" });
    });

    await waitFor(() => {
      expect(
        screen.getByText("[启动失败] 未设置 DEEPSEEK_API_KEY 环境变量"),
      ).toBeInTheDocument();
    });
  });

  it("SESSION_ALREADY_RUNNING：零事件延迟拒绝时完整回滚并恢复发送前 attention 状态点", async () => {
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});
    const sendDeferred = deferred<void>();
    mockBasicApp(agentProfiles, {
      sessions: [
        makeSession({
          id: "s1",
          title: "会话一",
          repo_id: "local-default",
          namespace_id: "local",
        }),
        makeSession({
          id: "s2",
          title: "会话二",
          repo_id: "local-default",
          namespace_id: "local",
        }),
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_message") return sendDeferred.promise;
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    act(() => {
      emitAgentEventBatch([{ kind: "blocked", message: "no_progress" }]);
    });

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() =>
      expect(container.querySelector(".turn__working")).not.toBeNull(),
    );

    await act(async () => {
      sendDeferred.reject("SESSION_ALREADY_RUNNING:s1");
      await sendDeferred.promise.catch(() => {});
    });

    const hint = await screen.findByText(
      "后端显示上一次运行仍未结束（可能已卡住）。请稍候或点停止后重试。",
    );
    expect(container.querySelectorAll(".turn--assistant")).toHaveLength(2);
    expect(container.querySelectorAll(".turn--assistant")[0]).toHaveTextContent(
      /连续多轮没有实质进展/,
    );
    expect(container.querySelectorAll(".turn--user")).toHaveLength(1);
    const turns = container.querySelectorAll(".turn");
    expect(turns[turns.length - 1]).toBe(hint.closest(".turn--assistant"));
    expect(container.querySelector(".turn__working")).toBeNull();
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(
        container.querySelector('[data-session-id="s1"] .sess__dot'),
      ).toHaveClass("attention"),
    );
    expect(consoleError).toHaveBeenCalledWith(
      "[send_message] backend session already running",
      "SESSION_ALREADY_RUNNING:s1",
    );
  });

  it("SESSION_ALREADY_RUNNING：拒绝前 text_delta 保留活跃 run，提示不截胡后续正文", async () => {
    const sendDeferred = deferred<void>();
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "旧流起点" }],
          engine: "claude",
          agent_id: "claude",
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_message") return sendDeferred.promise;
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "test" },
    });
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), {
      key: "Enter",
    });

    act(() => {
      emitAgentEventBatch([{ kind: "text_delta", text: "拒绝前正文" }]);
    });
    await screen.findByText("拒绝前正文");
    await act(async () => {
      sendDeferred.reject("SESSION_ALREADY_RUNNING:s1");
      await sendDeferred.promise.catch(() => {});
    });

    const hint = await screen.findByText(
      "后端显示上一次运行仍未结束（可能已卡住）。请稍候或点停止后重试。",
    );
    const hintTurn = hint.closest(".turn--assistant");
    const hintText = hintTurn?.textContent;
    const assistantTurns = container.querySelectorAll(".turn--assistant");
    expect(assistantTurns[assistantTurns.length - 2]).toBe(hintTurn);
    expect(assistantTurns[assistantTurns.length - 1]).toHaveTextContent(
      "拒绝前正文",
    );
    expect(container.querySelector(".turn__working")).not.toBeNull();
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();

    act(() => {
      emitAgentEventBatch([{ kind: "text_delta", text: "拒绝后正文" }]);
    });
    await waitFor(() =>
      expect(
        container.querySelectorAll(".turn--assistant")[
          container.querySelectorAll(".turn--assistant").length - 1
        ],
      ).toHaveTextContent("拒绝前正文拒绝后正文"),
    );
    expect(hintTurn?.textContent).toBe(hintText);
  });

  it("SESSION_ALREADY_RUNNING：拒绝前 usage_delta 保留活跃 run，提示不截胡后续正文", async () => {
    const sendDeferred = deferred<void>();
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "旧流起点" }],
          engine: "claude",
          agent_id: "claude",
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_message") return sendDeferred.promise;
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "test" },
    });
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), {
      key: "Enter",
    });

    act(() => {
      emitAgentEventBatch([
        {
          kind: "usage_delta",
          input_tokens: 2,
          output_tokens: 3,
        },
      ]);
    });
    await act(async () => {
      sendDeferred.reject("SESSION_ALREADY_RUNNING:s1");
      await sendDeferred.promise.catch(() => {});
    });

    const hint = await screen.findByText(
      "后端显示上一次运行仍未结束（可能已卡住）。请稍候或点停止后重试。",
    );
    const hintTurn = hint.closest(".turn--assistant");
    const hintText = hintTurn?.textContent;
    const assistantTurns = container.querySelectorAll(".turn--assistant");
    expect(assistantTurns[assistantTurns.length - 2]).toBe(hintTurn);
    expect(assistantTurns[assistantTurns.length - 1]).toHaveTextContent(
      "旧流起点",
    );
    // V3b §2B ③ 改了 streaming 判据（busy && (stream_live===true ‖ 未打标且是
    // 末条消息且 role===assistant)）：旧判据按「末条 assistant」找到「旧流起点」误标
    // working；这里末条消息其实是用户刚发的 "test"（旧判据的 v1 病灶同款场景），
    // 新判据不再误标——下面 text_delta 断言也证实续写落进一条全新 assistant 消息、
    // 不是「旧流起点」，说明它本就不是真正在流的那条。busy 仍为 true（停止按钮仍在）。
    expect(container.querySelector(".turn__working")).toBeNull();
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();

    act(() => {
      emitAgentEventBatch([{ kind: "text_delta", text: "拒绝后正文" }]);
    });
    await waitFor(() =>
      expect(container.querySelectorAll(".turn--assistant")).toHaveLength(
        assistantTurns.length + 1,
      ),
    );
    const updatedAssistantTurns =
      container.querySelectorAll(".turn--assistant");
    expect(
      updatedAssistantTurns[updatedAssistantTurns.length - 1],
    ).toHaveTextContent("拒绝后正文");
    expect(
      updatedAssistantTurns[updatedAssistantTurns.length - 1],
    ).not.toHaveTextContent("旧流起点");
    expect(hintTurn?.textContent).toBe(hintText);
  });

  it("SESSION_ALREADY_RUNNING：空会话拒绝前仅 usage_delta 时保留乐观气泡承接后续正文", async () => {
    const sendDeferred = deferred<void>();
    mockBasicApp(agentProfiles);
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_message") return sendDeferred.promise;
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "test" },
    });
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), {
      key: "Enter",
    });

    act(() => {
      emitAgentEventBatch([
        {
          kind: "usage_delta",
          input_tokens: 2,
          output_tokens: 3,
        },
      ]);
    });
    await act(async () => {
      sendDeferred.reject("SESSION_ALREADY_RUNNING:s1");
      await sendDeferred.promise.catch(() => {});
    });

    const hint = await screen.findByText(
      "后端显示上一次运行仍未结束（可能已卡住）。请稍候或点停止后重试。",
    );
    const hintTurn = hint.closest(".turn--assistant");
    const hintText = hintTurn?.textContent;
    const assistantTurns = container.querySelectorAll(".turn--assistant");
    expect(assistantTurns).toHaveLength(2);
    expect(assistantTurns[0]).toBe(hintTurn);
    expect(assistantTurns[1]).toContainElement(
      container.querySelector(".turn__working"),
    );

    act(() => {
      emitAgentEventBatch([{ kind: "text_delta", text: "拒绝后正文" }]);
    });
    await waitFor(() =>
      expect(
        container.querySelectorAll(".turn--assistant")[1],
      ).toHaveTextContent("拒绝后正文"),
    );
    expect(hintTurn?.textContent).toBe(hintText);
  });
});
