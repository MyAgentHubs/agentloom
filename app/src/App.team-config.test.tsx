import {
  render,
  screen,
  fireEvent,
  waitFor,
  within,
} from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import type { LeadStepOutcome } from "./types/agent";
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
    agentProfile,
    agentProfiles,
    mockBasicApp,
    configureTeamLead,
    decisionCardMessage,
    clickDecisionOption,
    deferred,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("LeadStepOutcome 类型不再包含 pendingDispatch", () => {
    const outcome: LeadStepOutcome = {
      status: "decided",
      action: { action: "reply", rationale: "问答" },
      decisionCard: null,
    };
    expect(outcome.status).toBe("decided");

    const staleOutcome: LeadStepOutcome = {
      status: "decided",
      action: { action: "reply", rationale: "问答" },
      decisionCard: null,
      // @ts-expect-error pendingDispatch 已从 decided outcome 删除。
      pendingDispatch: null,
    };
    expect(staleOutcome.status).toBe("decided");
  });

  describe("PendingDecisionBar 只认最新一张决策卡", () => {
    it("旧卡 pending + 新卡 chosen → 不渲染置顶条", async () => {
      mockBasicApp(agentProfiles, {
        messages: [
          decisionCardMessage(["继续"], {
            decision_id: "dc-old-pending",
            question: "旧问题不应被重新钉住",
            source_run_id: "mcp-lead-old",
            status: "pending",
            created_at: 1,
          }),
          decisionCardMessage(["继续"], {
            decision_id: "dc-new-chosen",
            question: "新问题已经回答",
            source_run_id: "mcp-lead-new",
            status: "chosen",
            chosen_option: "继续",
            created_at: 2,
          }),
        ],
      });

      render(<App />);
      await screen.findByPlaceholderText(/输入消息/);
      await waitFor(() =>
        expect(document.querySelector(".decision-card")).not.toBeNull(),
      );

      expect(document.querySelector(".composer__pending")).toBeNull();
    });

    it("旧卡 chosen + 新卡 pending → 置顶条渲染新卡", async () => {
      mockBasicApp(agentProfiles, {
        messages: [
          decisionCardMessage(["继续"], {
            decision_id: "dc-old-chosen",
            question: "旧问题已经回答",
            source_run_id: "mcp-lead-old",
            status: "chosen",
            chosen_option: "继续",
            created_at: 1,
          }),
          decisionCardMessage(["继续"], {
            decision_id: "dc-new-pending",
            question: "最新问题等待回答",
            source_run_id: "mcp-lead-new",
            status: "pending",
            created_at: 2,
          }),
        ],
      });

      render(<App />);
      await screen.findByPlaceholderText(/输入消息/);
      const pendingBar = await waitFor(() => {
        const bar = document.querySelector<HTMLElement>(".composer__pending");
        expect(bar).not.toBeNull();
        return bar!;
      });

      expect(
        within(pendingBar).getByText("最新问题等待回答"),
      ).toBeInTheDocument();
      expect(within(pendingBar).queryByText("旧问题已经回答")).toBeNull();
    });
  });

  it("Agent Team 送出 → 调 start_lead_session（不再调 lead_step/propose_team_plan）", async () => {
    mockBasicApp([
      agentProfile({
        cap_lead: "planner",
        provider: "claude",
        access: "native",
      }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 1,
      }),
    ]);
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "start_lead_session") return Promise.resolve();
      if (cmd === "set_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: args.leadAgentId ?? null,
          member_agent_ids: args.memberAgentIds ?? [],
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "cautious",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Claude Code" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Claude Code" }),
    );
    await waitFor(() =>
      expect(
        screen.getByRole("button", {
          name: /选择 agent：队长 Claude Code/,
        }),
      ).toBeInTheDocument(),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "这项目做什么" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "start_lead_session",
        expect.objectContaining({
          sessionId: "s1",
          leadAgentId: "claude",
          message: "这项目做什么",
          memberIds: ["deepseek"],
        }),
      );
    });
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "lead_step",
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "propose_team_plan",
      expect.anything(),
    );
  });

  it("初始 session agent config 读取 pending 时仍按普通 agent 发送，不调 lead_step", async () => {
    const readConfig = deferred<unknown>();
    mockBasicApp([
      agentProfile({
        cap_lead: "planner",
        provider: "claude",
        access: "native",
      }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 1,
      }),
    ]);
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "get_session_agent_config") return readConfig.promise;
      if (cmd === "start_lead_session") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "读配置未完成时不能发送" },
    });

    expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "lead_step",
    );

    // 首条消息发送会触发后台 rename_session → refreshSessions 的
    // fire-and-forget 链路（onSend 不 await 它，产品上是有意的非阻塞行为）。
    // 测试须等它落定，否则 unmount 后才 resolve 的 setSessions 会打出 act()
    // 警告（偶发升级成 AggregateError 的根因之一）。
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

  it("当前 session Team 模式：session agent config 写入 pending 时禁用发送且不调 lead_step", async () => {
    const writeConfig = deferred<unknown>();
    const teamAgents = [
      agentProfile({
        id: "claude",
        name: "Claude Code",
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
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["写条 AI 新闻到 readme"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: null,
          member_agent_ids: [],
        });
      if (cmd === "set_session_agent_config") return writeConfig.promise;
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: { action: "reply", rationale: "问答" },
          decisionCard: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Claude Code" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Claude Code" }),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_session_agent_config",
        expect.objectContaining({
          sessionId: "s1",
          leadAgentId: "claude",
          memberAgentIds: ["deepseek"],
        }),
      ),
    );
    await waitFor(() => {
      const menu = screen.getByRole("menu");
      expect(menu).toBeVisible();
      expect(within(menu).getByText("这个会话用谁")).toBeVisible();
      expect(within(menu).getByText("Auto")).toBeInTheDocument();
    });
    expect(
      screen.getByRole("button", { name: "取消队长 Claude Code" }),
    ).not.toBeDisabled();
    expect(
      screen.getByRole("button", { name: "成员 DeepSeek" }),
    ).not.toBeDisabled();

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "写配置未完成时不能发送" },
    });

    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "lead_step",
    );
  });

  it("当前 session Team 模式：session agent config 写失败后禁用发送且不使用 optimistic cache 调 lead_step", async () => {
    const teamAgents = [
      agentProfile({
        id: "claude",
        name: "Claude Code",
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
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["写条 AI 新闻到 readme"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: null,
          member_agent_ids: [],
        });
      if (cmd === "set_session_agent_config")
        return Promise.reject(new Error("WRITE_FAILED"));
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: { action: "reply", rationale: "问答" },
          decisionCard: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Claude Code" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Claude Code" }),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_session_agent_config",
        expect.anything(),
      ),
    );
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).toBeDisabled(),
    );

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "写配置失败后不能发送" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "lead_step",
    );
  });

  it("当前 session Team 模式：session agent config 写失败后 selector 仍可重试配置", async () => {
    const teamAgents = [
      agentProfile({
        id: "claude",
        name: "Claude Code",
        provider: "claude",
        access: "native",
        cap_lead: "planner",
        sort_order: 0,
      }),
      agentProfile({
        id: "backup",
        name: "Backup Lead",
        provider: "claude",
        access: "native",
        cap_lead: "planner",
        sort_order: 1,
      }),
    ];
    let writeAttempts = 0;
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["写条 AI 新闻到 readme"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: null,
          member_agent_ids: [],
        });
      if (cmd === "set_session_agent_config") {
        writeAttempts += 1;
        if (writeAttempts === 1)
          return Promise.reject(new Error("WRITE_FAILED"));
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: args.leadAgentId ?? null,
          member_agent_ids: args.memberAgentIds ?? [],
        });
      }
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Claude Code" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Claude Code" }),
    );
    await waitFor(() => expect(writeAttempts).toBe(1));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).toBeDisabled(),
    );

    const retryTrigger = await screen.findByRole("button", {
      name: "选择 agent：Claude Code",
    });
    await waitFor(() => expect(retryTrigger).not.toBeDisabled());
    let retryLead = screen.queryByRole("button", {
      name: "设为队长 Backup Lead",
    });
    if (!retryLead) {
      fireEvent.click(retryTrigger);
      retryLead = screen.getByRole("button", { name: "设为队长 Backup Lead" });
    }
    fireEvent.click(retryLead);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "set_session_agent_config",
        expect.objectContaining({
          sessionId: "s1",
          leadAgentId: "backup",
          memberAgentIds: ["claude"],
        }),
      ),
    );
  });

  it("lead 判 reply → 转 send_message 用 leadId·不再触发 lead_step", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      { messages: [decisionCardMessage(["这项目做什么"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: { action: "reply", rationale: "问答" },
          decisionCard: null,
        });
      if (cmd === "set_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: args.leadAgentId ?? null,
          member_agent_ids: args.memberAgentIds ?? [],
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "cautious",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead();
    await clickDecisionOption("这项目做什么");
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "send_message",
        expect.objectContaining({ message: "这项目做什么" }),
      );
    });
    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "lead_step").length,
    ).toBe(1);
  });

  it("propose_verifier 无 active run → 退化成 ask_user 卡（保住验证意图）", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      { messages: [decisionCardMessage(["验证一下"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "propose_verifier",
            rationale: "想验证",
            cmd: "npm test",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "cautious",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead();
    await clickDecisionOption("验证一下");
    await waitFor(() => {
      expect(screen.getByText(/npm test/)).toBeInTheDocument();
    });
  });
});
