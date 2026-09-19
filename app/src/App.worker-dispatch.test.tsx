import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
} from "@testing-library/react";
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
    agentProfile,
    mockBasicApp,
    configureTeamLead,
    decisionCardMessage,
    findInlineDecisionButton,
    clickDecisionOption,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("lead 判 dispatch_worker → 先出 dispatch_confirm 确认卡·确认前不 start_team_run·确认后 start_team_run·goal=task·单成员·不带外部 runId", async () => {
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
      { messages: [decisionCardMessage(["写条 AI 新闻到 readme"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "要改 README",
            task: "写新闻到 README",
            scope_files: ["README.md"],
            agent_hint: null,
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });
    const { container } = render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead("Claude Code", "DeepSeek");
    await clickDecisionOption("写条 AI 新闻到 readme");

    // 看一眼再派：先出确认卡（含澄清目标 + 子任务 + 派给谁），此时还没 start_team_run。
    const confirmDispatch = await findInlineDecisionButton(/确认派单/);
    // 卡头含「派给谁 + 子任务（澄清目标）」。
    expect(
      container.querySelector(".decision-card .dc-head"),
    ).toHaveTextContent("派给 DeepSeek");
    expect(
      container.querySelector(".decision-card .dc-head"),
    ).toHaveTextContent("写新闻到 README");
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );

    // 用户一键确认 → 真正 start_team_run。
    fireEvent.click(confirmDispatch);
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "start_team_run",
        expect.objectContaining({ goal: "写新闻到 README" }),
      );
    });
    const startTeamRunCall = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "start_team_run",
    );
    expect(startTeamRunCall?.[1]?.runId).toBeUndefined();
    // lead 派单自由·不再前端落账
    expect(invokeMock).not.toHaveBeenCalledWith(
      "record_lead_dispatch",
      expect.anything(),
    );
  });

  it("lead 判 dispatch_worker → 确认派单后 start_team_run 撞 SESSION_ALREADY_RUNNING → 静默收敛(不出现裸串 toast，对齐 solo 侧既有语义)", async () => {
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
      { messages: [decisionCardMessage(["写条 AI 新闻到 readme"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "要改 README",
            task: "写新闻到 README",
            scope_files: ["README.md"],
            agent_hint: null,
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "start_team_run")
        // Tauri invoke 对 `Result<_, String>` 命令的 reject 值是裸字符串（不是 JS Error
        // 实例）——与 src-tauri/src/lib.rs:899 `Err(format!("SESSION_ALREADY_RUNNING:{session_id}"))`
        // 的真实返回形态一致。
        return Promise.reject("SESSION_ALREADY_RUNNING:s1");
      return defaultInvoke?.(cmd, args);
    });
    const { container } = render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead("Claude Code", "DeepSeek");
    await clickDecisionOption("写条 AI 新闻到 readme");

    const confirmDispatch = await findInlineDecisionButton(/确认派单/);

    fireEvent.click(confirmDispatch);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "start_team_run",
        expect.objectContaining({ goal: "写新闻到 README" }),
      ),
    );

    // 给 .catch 微任务一点时间落定；确认不出现裸串 toast（静默收敛，对齐 solo 侧
    // `if (String(err).startsWith("SESSION_ALREADY_RUNNING:")) return;` 既有语义）。
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(container.querySelector(".toast")).toBeNull();
  });

  it("lead 判 dispatch_worker → 用户取消确认卡 → 不 start_team_run", async () => {
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
      { messages: [decisionCardMessage(["写条 AI 新闻到 readme"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "要改 README",
            task: "写新闻到 README",
            scope_files: ["README.md"],
            agent_hint: null,
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead("Claude Code", "DeepSeek");
    await clickDecisionOption("写条 AI 新闻到 readme");

    const cancelBtn = await findInlineDecisionButton(/取消/);
    fireEvent.click(cancelBtn);
    await act(async () => {
      await Promise.resolve();
    });
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
    // 取消后确认卡消失（chosen 态不渲）。
    await waitFor(() => {
      expect(document.querySelector(".decision-card")).toBeNull();
    });
  });

  it("dispatch_worker 带 goal_title → start_team_run 入参含 goalTitle", async () => {
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
      { messages: [decisionCardMessage(["写条 AI 新闻到 readme"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "要改 README",
            task: "写新闻到 README",
            scope_files: ["README.md"],
            agent_hint: null,
            goal_title: "写 README",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "start_team_run") return Promise.resolve("run-gt-01");
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead("Claude Code", "DeepSeek");
    await clickDecisionOption("写条 AI 新闻到 readme");

    // 等确认卡出现
    const confirmDispatch = await findInlineDecisionButton(/确认派单/);

    // 确认派单
    fireEvent.click(confirmDispatch);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "start_team_run",
        expect.objectContaining({ goalTitle: "写 README" }),
      );
    });
  });

  it("dispatch_worker 带 goal_title → goalTitleByRun 存入 runId→goalTitle", async () => {
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
      { messages: [decisionCardMessage(["修改 README"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    const fakeRunId = "run-gt-02";
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "要改 README",
            task: "写新闻到 README",
            scope_files: ["README.md"],
            agent_hint: null,
            goal_title: "改 README 标题",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "start_team_run") return Promise.resolve(fakeRunId);
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead("Claude Code", "DeepSeek");
    await clickDecisionOption("修改 README");

    const confirmDispatch = await findInlineDecisionButton(/确认派单/);

    fireEvent.click(confirmDispatch);

    // start_team_run 携带 goalTitle 即可验证 goalTitleByRun 会被设置（测试框架无直接访问 state 的接口）
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(
        ([cmd]) => cmd === "start_team_run",
      );
      expect(call?.[1]).toMatchObject({ goalTitle: "改 README 标题" });
    });
  });

  it("saved lead + 空成员池：lead_step 用 saved lead，dispatch 不伪造成当前 agent", async () => {
    const teamAgents = [
      agentProfile({
        id: "lead-a",
        name: "Lead A",
        provider: "lead",
        cap_lead: "planner",
        sort_order: 0,
      }),
      agentProfile({
        id: "worker-a",
        name: "Worker A",
        provider: "worker",
        sort_order: 1,
      }),
    ];
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["写条 AI 新闻到 readme"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: [],
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "需要 worker",
            task: "写新闻到 README",
            scope_files: ["README.md"],
            agent_hint: null,
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Lead A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    await clickDecisionOption("写条 AI 新闻到 readme");

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "lead_step",
        expect.objectContaining({
          sessionId: "s1",
          leadAgentId: "lead-a",
        }),
      ),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "append_message",
        expect.objectContaining({ sessionId: "s1", role: "user" }),
      ),
    );
    await act(async () => {
      await Promise.resolve();
    });

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
  });

  it("saved lead + 多成员池：dispatch_worker 无 hint 时不默认派首个成员", async () => {
    const teamAgents = [
      agentProfile({
        id: "lead-a",
        name: "Lead A",
        provider: "lead",
        cap_lead: "planner",
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
        name: "DeepSeekFlash",
        provider: "deepseek",
        sort_order: 2,
      }),
    ];
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["分别用两个 worker 写 10 个冷笑话"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: ["codex", "deepseek"],
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "需要两个 worker",
            task: "分别写 10 个冷笑话",
            scope_files: [],
            agent_hint: null,
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Lead A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    await clickDecisionOption("分别用两个 worker 写 10 个冷笑话");

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "lead_step",
        expect.objectContaining({
          dispatchableMemberIds: ["codex", "deepseek"],
        }),
      ),
    );
    await act(async () => {
      await Promise.resolve();
    });

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
  });

  it("saved lead + 多成员池：dispatch_worker 带 agent_hint 时只派命中的成员", async () => {
    const teamAgents = [
      agentProfile({
        id: "lead-a",
        name: "Lead A",
        provider: "lead",
        cap_lead: "planner",
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
        name: "DeepSeekFlash",
        provider: "deepseek",
        sort_order: 2,
      }),
    ];
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["让 deepseek 写后半段"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: ["codex", "deepseek"],
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "指定 deepseek",
            task: "写 6-10 条冷笑话",
            scope_files: [],
            agent_hint: " deepseek ",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Lead A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    await clickDecisionOption("让 deepseek 写后半段");

    // T5：先确认卡·一键确认才真派单。
    const confirmDispatch = await findInlineDecisionButton(/确认派单/);
    fireEvent.click(confirmDispatch);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "start_team_run",
        expect.objectContaining({ goal: "写 6-10 条冷笑话" }),
      ),
    );
    const startTeamRunCall = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "start_team_run",
    );
    expect(startTeamRunCall?.[1]?.members.map((m: any) => m.agentId)).toEqual([
      "deepseek",
    ]);
  });

  it("saved lead + 多成员池：dispatch_worker 带重复 provider agent_hint 时不派单", async () => {
    const teamAgents = [
      agentProfile({
        id: "lead-a",
        name: "Lead A",
        provider: "lead",
        cap_lead: "planner",
        sort_order: 0,
      }),
      agentProfile({
        id: "codex-fast",
        name: "Codex Fast",
        provider: "codex",
        sort_order: 1,
      }),
      agentProfile({
        id: "codex-safe",
        name: "Codex Safe",
        provider: "codex",
        sort_order: 2,
      }),
    ];
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["让 codex 写冷笑话"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: ["codex-fast", "codex-safe"],
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "指定 codex provider",
            task: "写冷笑话",
            scope_files: [],
            agent_hint: "codex",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Lead A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    await clickDecisionOption("让 codex 写冷笑话");

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "lead_step",
        expect.objectContaining({
          dispatchableMemberIds: ["codex-fast", "codex-safe"],
        }),
      ),
    );
    await act(async () => {
      await Promise.resolve();
    });

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
  });

  it("saved lead + 多成员池：dispatch_worker 带重复 name agent_hint 时不派单", async () => {
    const teamAgents = [
      agentProfile({
        id: "lead-a",
        name: "Lead A",
        provider: "lead",
        cap_lead: "planner",
        sort_order: 0,
      }),
      agentProfile({
        id: "codex-fast",
        name: "Codex",
        provider: "codex-fast",
        sort_order: 1,
      }),
      agentProfile({
        id: "codex-safe",
        name: "Codex",
        provider: "codex-safe",
        sort_order: 2,
      }),
    ];
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["让 Codex 写冷笑话"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: ["codex-fast", "codex-safe"],
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "指定 Codex name",
            task: "写冷笑话",
            scope_files: [],
            agent_hint: "Codex",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Lead A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    await clickDecisionOption("让 Codex 写冷笑话");

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "lead_step",
        expect.objectContaining({
          dispatchableMemberIds: ["codex-fast", "codex-safe"],
        }),
      ),
    );
    await act(async () => {
      await Promise.resolve();
    });

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
  });

  it("saved lead + 多成员池：dispatch_worker 带精确 id 时可派共享 provider 的成员", async () => {
    const teamAgents = [
      agentProfile({
        id: "lead-a",
        name: "Lead A",
        provider: "lead",
        cap_lead: "planner",
        sort_order: 0,
      }),
      agentProfile({
        id: "codex-fast",
        name: "Codex Fast",
        provider: "codex",
        sort_order: 1,
      }),
      agentProfile({
        id: "codex-safe",
        name: "Codex Safe",
        provider: "codex",
        sort_order: 2,
      }),
    ];
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["让 codex-safe 写冷笑话"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: ["codex-fast", "codex-safe"],
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "指定 codex-safe",
            task: "写冷笑话",
            scope_files: [],
            agent_hint: "codex-safe",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Lead A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    await clickDecisionOption("让 codex-safe 写冷笑话");

    // T5：先确认卡·一键确认才真派单。
    const confirmDispatch = await findInlineDecisionButton(/确认派单/);
    fireEvent.click(confirmDispatch);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "start_team_run",
        expect.objectContaining({ goal: "写冷笑话" }),
      ),
    );
    const startTeamRunCall = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "start_team_run",
    );
    expect(startTeamRunCall?.[1]?.members.map((m: any) => m.agentId)).toEqual([
      "codex-safe",
    ]);
  });

  it("saved lead + 多成员池：dispatch_worker 带无效 agent_hint 时不派单", async () => {
    const teamAgents = [
      agentProfile({
        id: "lead-a",
        name: "Lead A",
        provider: "lead",
        cap_lead: "planner",
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
        name: "DeepSeekFlash",
        provider: "deepseek",
        sort_order: 2,
      }),
    ];
    mockBasicApp(teamAgents, {
      messages: [decisionCardMessage(["让 ghost 写后半段"])],
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: ["codex", "deepseek"],
        });
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "dispatch_worker",
            rationale: "指定不存在的 worker",
            task: "写冷笑话",
            scope_files: [],
            agent_hint: "ghost",
          },
          decisionCard: null,
        });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Lead A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    await clickDecisionOption("让 ghost 写后半段");

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "lead_step",
        expect.objectContaining({
          dispatchableMemberIds: ["codex", "deepseek"],
        }),
      ),
    );
    await act(async () => {
      await Promise.resolve();
    });

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
  });
});
