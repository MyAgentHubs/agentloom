import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
  within,
} from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import type { AgentProfile } from "./types/agent";
import { makeSession } from "./test/factories";
import App from "./App";
import type { ChatMessage } from "./types/agent";
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
    LAST_AGENT_ID_KEY,
    mockBasicApp,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("detect_runtime codex 未装 + 无 key 借壳 → 输入区 dropdown 不含 codex/无 key 借壳", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: false },
        });
      if (cmd === "list_agents")
        return Promise.resolve([
          agentProfile({
            id: "claude",
            access: "native",
            provider: "claude",
            enabled: true,
            has_key: false,
          }),
          agentProfile({
            id: "codex",
            access: "native",
            provider: "codex",
            enabled: true,
            has_key: false,
          }),
          agentProfile({
            id: "ds",
            name: "DeepSeek",
            access: "borrow",
            provider: "deepseek",
            enabled: true,
            has_key: false,
          }),
        ]);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
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
      return Promise.resolve();
    });

    render(<App />);

    const trigger = await screen.findByRole("button", {
      name: /选择 agent/,
    });
    await waitFor(() => expect(trigger).not.toBeDisabled());
    fireEvent.click(trigger);
    const items = (await screen.findAllByRole("menuitemradio")).map(
      (button) => button.textContent ?? "",
    );
    expect(items.some((text) => /Claude/.test(text))).toBe(true);
    expect(items.some((text) => /Codex/.test(text))).toBe(false);
    expect(items.some((text) => /DeepSeek/i.test(text))).toBe(false);

    // 挂载时 useTeamConfig 会对当前 session 发起 get_session_agent_config 读取
    // （非阻塞，不影响本用例断言的 dropdown 内容）。测试须等它 resolve 落定，
    // 否则 unmount 后才落地的 setState 会打出 act() 警告。
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );
    await act(async () => {});
  });

  it("设置保存后刷新 runtime 检测：codex 从未装变已装 → dropdown 出现（F5 回归）", async () => {
    let detectCalls = 0;
    let savedAgent: AgentProfile | null = null;
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime") {
        detectCalls += 1;
        return Promise.resolve({
          claude: {
            available: true,
            version: null,
            path: null,
            creds_hint: true,
          },
          codex: {
            available: detectCalls > 1,
            version: null,
            path: null,
            creds_hint: true,
          },
        });
      }
      if (cmd === "list_agents")
        return Promise.resolve([
          agentProfile({
            id: "claude",
            access: "native",
            provider: "claude",
            enabled: true,
            has_key: false,
          }),
          agentProfile({
            id: "codex",
            name: "Codex",
            access: "native",
            provider: "codex",
            enabled: true,
            has_key: false,
          }),
          ...(savedAgent ? [savedAgent] : []),
        ]);
      if (cmd === "upsert_agent") {
        savedAgent = { ...args.profile, has_key: true };
        return Promise.resolve();
      }
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve([]);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
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
      return Promise.resolve();
    });

    const { container } = render(<App />);

    const trigger1 = await screen.findByRole("button", {
      name: /选择 agent/,
    });
    await waitFor(() => expect(trigger1).not.toBeDisabled());
    fireEvent.click(trigger1);
    expect(
      (await screen.findAllByRole("menuitemradio"))
        .map((button) => button.textContent ?? "")
        .some((text) => /Codex/.test(text)),
    ).toBe(false);
    fireEvent.click(trigger1);

    fireEvent.click(await screen.findByRole("button", { name: "设置" }));
    fireEvent.click(
      await screen.findByRole("button", { name: "＋ 添加 agent" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Kimi" }));
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-kimi-f5" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));
    await screen.findByText(/连接成功/);
    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    await waitFor(() => expect(detectCalls).toBeGreaterThan(1));

    fireEvent.click(
      within(container.querySelector(".sidebar")!).getByText("会话一"),
    );
    await screen.findByPlaceholderText(/输入消息/);
    // 切会话触发 openSession 里一串不等待调用方的后台调用（同 session 并发
    // Task 4 · NF1 的根因：refreshRunStates/list_interrupted_team_runs/
    // get_lead_loop_state/useTeamConfig 等 fire-and-forget 非阻塞刷新）。
    // findByPlaceholderText 只保证输入框已挂载，不保证这串后台调用已落定；
    // 若不等它冲平就紧接着点开 agent 下拉，偶发会撞上后台刷新引发的重渲染，
    // 导致下拉没能稳定展开、waitFor 一直等不到 Codex 菜单项（本用例曾以
    // "Unable to find an accessible element with the role menuitemradio"
    // 偶发失败）。这里在点下拉前先把已 resolve 的微任务链冲平。
    await act(async () => {
      for (let i = 0; i < 5; i++) {
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
    });
    const trigger2 = await screen.findByRole("button", {
      name: /选择 agent/,
    });
    await waitFor(() => expect(trigger2).not.toBeDisabled());
    fireEvent.click(trigger2);
    await waitFor(() =>
      expect(
        screen
          .getAllByRole("menuitemradio")
          .map((button) => button.textContent ?? "")
          .some((text) => /Codex/.test(text)),
      ).toBe(true),
    );
  });

  it("当前 agentId 不可用时兜底切首个可用（不卡在被隐藏的 agent）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: false },
        });
      if (cmd === "list_agents")
        return Promise.resolve([
          agentProfile({
            id: "claude",
            access: "native",
            provider: "claude",
            enabled: true,
            has_key: false,
          }),
        ]);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
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
      return Promise.resolve();
    });

    render(<App />);

    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).toHaveTextContent("Claude"),
    );
  });

  it("冷启动从 localStorage 恢复上次手选 agent", async () => {
    localStorage.setItem(LAST_AGENT_ID_KEY, "deepseek");
    mockBasicApp([
      agentProfile({ id: "claude", name: "Claude", sort_order: 0 }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 1,
      }),
    ]);

    render(<App />);

    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).toHaveTextContent("DeepSeek"),
    );
  });

  it("用户手选 agent 后写入 localStorage", async () => {
    mockBasicApp();

    render(<App />);

    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: "DeepSeek" }));

    await waitFor(() =>
      expect(localStorage.getItem(LAST_AGENT_ID_KEY)).toBe("deepseek"),
    );
  });

  it("sticky 回填历史 agent 不写入 localStorage", async () => {
    const stickyAgents: AgentProfile[] = [
      agentProfile({ id: "claude", name: "Claude", sort_order: 0 }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 1,
      }),
    ];
    mockBasicApp(stickyAgents, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "历史回复" }],
          engine: "deepseek",
          agent_id: "deepseek",
          agent_name_snapshot: "DeepSeek",
        },
      ],
    });

    render(<App />);

    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).toHaveTextContent("DeepSeek"),
    );
    expect(localStorage.getItem(LAST_AGENT_ID_KEY)).toBeNull();
  });

  it("自动兜底纠偏 agent 不写入 localStorage", async () => {
    const { sendCalls } = mockBasicApp([
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 0,
      }),
    ]);

    render(<App />);

    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).toHaveTextContent("DeepSeek"),
    );
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "fallback check" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(sendCalls[0].agentId).toBe("deepseek");
    expect(localStorage.getItem(LAST_AGENT_ID_KEY)).toBeNull();
  });

  it("启动拉 list_agents 并在 composer 显示动态 agent 名称", async () => {
    mockBasicApp();
    render(<App />);

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("list_agents"));
    expect(await screen.findByText("Claude Code")).toBeInTheDocument();
  });

  it("零可用 agent 时显示安装引导，打开 Agent 设置后关闭引导", async () => {
    mockBasicApp([], {
      runtimeDetect: {
        claude: { available: false },
        codex: { available: false },
      },
    });
    render(<App />);

    const guide = await screen.findByRole("dialog", {
      name: "还没有可用的 agent",
    });
    expect(guide).toHaveTextContent("内置引擎 myagent");

    fireEvent.click(
      within(guide).getByRole("button", { name: "打开 Agent 设置" }),
    );

    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "还没有可用的 agent" }),
      ).not.toBeInTheDocument(),
    );
    expect(
      await screen.findByRole("button", { name: "＋ 添加 agent" }),
    ).toBeInTheDocument();
  });

  it("Settings 新增 agent 后同步刷新输入区 dropdown", async () => {
    const initialAgents = [agentProfile()];
    let savedAgent: AgentProfile | null = null;
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents")
        return Promise.resolve(
          savedAgent ? [...initialAgents, savedAgent] : initialAgents,
        );
      if (cmd === "upsert_agent") {
        savedAgent = { ...args.profile, has_key: true };
        return Promise.resolve();
      }
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
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
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: {
            available: true,
            version: null,
            path: null,
            creds_hint: true,
          },
          codex: {
            available: true,
            version: null,
            path: null,
            creds_hint: true,
          },
        });
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve([]);
      return Promise.resolve();
    });

    const { container } = render(<App />);

    expect(await screen.findByText("Claude Code")).toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button", { name: "设置" }));
    fireEvent.click(
      await screen.findByRole("button", { name: "＋ 添加 agent" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Kimi" }));
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-kimi-test" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));
    await screen.findByText(/连接成功/);
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          name: "Kimi 中国区（Claude Code 借壳）",
        }),
      }),
    );
    expect(await screen.findByText(/Kimi 中国区/)).toBeInTheDocument();

    fireEvent.click(
      within(container.querySelector(".sidebar")!).getByText("会话一"),
    );
    await screen.findByPlaceholderText(/输入消息/);
    const trigger = await screen.findByRole("button", {
      name: /选择 agent/,
    });
    await waitFor(() => expect(trigger).not.toBeDisabled());
    fireEvent.click(trigger);

    expect(
      await screen.findByRole("menuitemradio", { name: /Kimi/ }),
    ).toBeInTheDocument();
  });

  it("打开有历史会话默认 sticky 到最后答的 agent + 可自由换", async () => {
    const sendCalls: any[] = [];
    const stickyAgents: AgentProfile[] = [
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
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(stickyAgents);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s_sticky",
            title: "历史会话",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          { role: "user", content: [{ type: "text", text: "q" }] },
          {
            role: "assistant",
            content: [{ type: "text", text: "hi" }],
            engine: "deepseek",
            agent_id: "deepseek",
            agent_name_snapshot: "DeepSeek",
          },
        ]);
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

    render(<App />);

    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).toHaveTextContent("DeepSeek"),
    );

    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "继续" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(sendCalls[0].agentId).toBe("deepseek");

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    expect(handler).toBeTruthy();
    act(() => {
      handler({
        payload: {
          session_id: "s_sticky",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: "done",
        },
      });
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument(),
    );

    fireEvent.click(screen.getByLabelText(/选择 agent/));
    const claudeItem = screen.getByRole("menuitemradio", { name: "Claude" });
    expect(claudeItem).not.toBeDisabled();
    fireEvent.click(claudeItem);
    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).toHaveTextContent("Claude"),
    );

    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "Claude 继续" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(2));
    expect(sendCalls[1].agentId).toBe("claude");
  });

  it("sticky 取最后答·非首条·非首个 available", async () => {
    const sendCalls: any[] = [];
    const stickyAgents: AgentProfile[] = [
      agentProfile({
        id: "claude",
        name: "Claude",
        provider: "claude",
        sort_order: 0,
      }),
      agentProfile({
        id: "codex",
        name: "Codex",
        provider: "codex",
        sort_order: 1,
      }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 2,
      }),
    ];
    const history: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "q1" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "a1" }],
        engine: "claude",
        agent_id: "claude",
        agent_name_snapshot: "Claude",
      },
      { role: "user", content: [{ type: "text", text: "q2" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "a2" }],
        engine: "deepseek",
        agent_id: "deepseek",
        agent_name_snapshot: "DeepSeek",
      },
    ];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(stickyAgents);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s_last",
            title: "最后答",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve(history);
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

    render(<App />);

    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).not.toHaveTextContent("…"),
    );
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "继续" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(sendCalls[0].agentId).toBe("deepseek");
  });

  it("冷启竞态：agents 晚于 messages 时仍 sticky 到最后答", async () => {
    const sendCalls: any[] = [];
    const stickyAgents: AgentProfile[] = [
      agentProfile({
        id: "claude",
        name: "Claude",
        provider: "claude",
        sort_order: 0,
      }),
      agentProfile({
        id: "codex",
        name: "Codex",
        provider: "codex",
        sort_order: 1,
      }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 2,
      }),
    ];
    const history: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "q1" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "a1" }],
        engine: "claude",
        agent_id: "claude",
        agent_name_snapshot: "Claude",
      },
      { role: "user", content: [{ type: "text", text: "q2" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "a2" }],
        engine: "deepseek",
        agent_id: "deepseek",
        agent_name_snapshot: "DeepSeek",
      },
    ];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents")
        return new Promise<AgentProfile[]>((resolve) => {
          setTimeout(() => resolve(stickyAgents), 30);
        });
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s_race",
            title: "冷启竞态",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve(history);
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

    render(<App />);

    await waitFor(() =>
      expect(screen.getByLabelText(/选择 agent/)).not.toHaveTextContent("…"),
    );
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "继续" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(sendCalls[0].agentId).toBe("deepseek");
  });

  it("native Codex creds_hint=false 时显示未登录软提示且仍照常发送", async () => {
    const codexAgent = agentProfile({
      id: "codex",
      name: "Codex",
      access: "native",
      provider: "codex",
    });
    const { sendCalls } = mockBasicApp([codexAgent], {
      runtimeDetect: {
        claude: { available: true, creds_hint: true },
        codex: { available: true, creds_hint: false },
      },
    });

    const { container } = render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Codex" });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("detect_runtime"),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() =>
      expect(container.querySelector(".turn--assistant")).toHaveTextContent(
        "Codex 似乎尚未登录（未找到凭据文件）。若长时间无响应，请在终端运行 codex login 后重试。",
      ),
    );
    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(sendCalls[0]).toMatchObject({
      sessionId: "s1",
      agentId: "codex",
      message: "test",
    });
  });

  it("borrow access 且 provider=codex 时即使 creds_hint=false 也不显示未登录提示", async () => {
    const borrowedCodexAgent = agentProfile({
      id: "borrowed-codex",
      name: "Borrowed Codex",
      access: "borrow",
      provider: "codex",
    });
    const { sendCalls } = mockBasicApp([borrowedCodexAgent], {
      runtimeDetect: {
        claude: { available: true, creds_hint: true },
        codex: { available: true, creds_hint: false },
      },
    });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Borrowed Codex" });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("detect_runtime"),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(
      screen.queryByText(/Codex 似乎尚未登录（未找到凭据文件）/),
    ).toBeNull();
  });

  it("native Claude creds_hint=false 时显示 Claude 版未登录软提示", async () => {
    const claudeAgent = agentProfile({
      id: "claude",
      name: "Claude Code",
      access: "native",
      provider: "claude",
    });
    const { sendCalls } = mockBasicApp([claudeAgent], {
      runtimeDetect: {
        claude: { available: true, creds_hint: false },
        codex: { available: true, creds_hint: true },
      },
    });

    const { container } = render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("detect_runtime"),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() =>
      expect(container.querySelector(".turn--assistant")).toHaveTextContent(
        "Claude Code 似乎尚未登录（未找到凭据文件）。若长时间无响应，请在终端运行 claude 完成登录后重试。",
      ),
    );
    await waitFor(() => expect(sendCalls).toHaveLength(1));
  });

  it.each([
    ["creds_hint=true", true],
    ["creds_hint 缺数据", null],
  ])("native Codex %s 时不显示未登录提示", async (_label, credsHint) => {
    const codexAgent = agentProfile({
      id: "codex",
      name: "Codex",
      access: "native",
      provider: "codex",
    });
    const { sendCalls } = mockBasicApp([codexAgent], {
      runtimeDetect: {
        claude: { available: true, creds_hint: true },
        codex: { available: true, creds_hint: credsHint },
      },
    });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Codex" });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("detect_runtime"),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(
      screen.queryByText(/Codex 似乎尚未登录（未找到凭据文件）/),
    ).toBeNull();
  });
});
