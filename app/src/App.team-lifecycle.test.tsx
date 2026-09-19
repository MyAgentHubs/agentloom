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
import { makeSession } from "./test/factories";
import App from "./App";
import type { Block } from "./types/agent";
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
    runCard,
    appMember,
    reviewWithChanges,
    mockAppWithReview,
    openReviewPanel,
    mockBasicApp,
    configureTeamLead,
    agentEventCb,
    dEnv,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("lead 空闲但 running dispatch_card 存在时显示全局停止并调用 stop_session", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [
            {
              type: "dispatch_card",
              run_id: "worker-run-1",
              member: appMember(),
            },
          ],
        },
      ],
    });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });

    fireEvent.click(await screen.findByRole("button", { name: "停止" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("stop_session", {
        sessionId: "s1",
      }),
    );
    expect(
      invokeMock.mock.calls.filter(([command]) => command === "stop_session"),
    ).toHaveLength(1);
  });

  it("真 run：执行中队员 live 卡注入主区·点卡进右面板 drill", async () => {
    const { sendCalls } = mockBasicApp([
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
      if (cmd === "lead_step") return Promise.resolve({ status: "duplicate" });
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "cautious",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "propose_team_plan")
        return Promise.resolve({
          outcome: "drafted",
          runId: "run1",
          contractId: "run1-gc",
          goal: "开干",
          tier: "tier2",
          riskLevel: "med",
          subtaskCount: 1,
          unassignedCount: 0,
          status: "draft",
          assignmentsJson: "[]",
        });
      return defaultInvoke?.(cmd, args);
    });
    const { container } = render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead();

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "开干" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    expect(sendCalls).toEqual([]);

    const handler = agentEventCb();
    await act(async () => {
      await handler(
        dEnv(
          { run_id: "run1" },
          {
            kind: "goal_declared",
            goal: "开干",
            status: "frozen",
            lead: "Claude",
            criteria: [
              { id: "1", claim: "a", status: "pending", scope: "task" },
              { id: "2", claim: "b", status: "pending", scope: "task" },
            ],
          },
        ),
      );
    });
    await act(async () => {
      await handler(
        dEnv(
          {
            run_id: "run1",
            assignment_id: "a1",
            origin_participant_id: "worker-1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "开干" },
        ),
      );
    });
    await waitFor(() =>
      expect(container.querySelector(".taskstack")).not.toBeNull(),
    );
    fireEvent.click(
      (await screen.findByText("worker-1")).closest('[role="button"]')!,
    );
    expect(await screen.findByLabelText("回 Lead")).toBeInTheDocument();
    expect(container.querySelector(".drillin__status")).toHaveTextContent(
      "进行中",
    );
    expect(container.querySelector(".drillin__head")).not.toHaveTextContent(
      "worker-1",
    );
  });

  it("③ 启用 topbar goal：goal_declared 后 topbar(.sf-head__main) 渲出目标条（goal-wrap--topbar）", async () => {
    mockBasicApp();
    const { container } = render(<App />);
    await screen.findByText("Claude Code");

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "开干" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const handler = agentEventCb();
    await act(async () => {
      await handler(
        dEnv(
          { run_id: "run1" },
          {
            kind: "goal_declared",
            goal: "开干",
            status: "frozen",
            lead: "Claude",
            criteria: [
              { id: "1", claim: "a", status: "pending", scope: "task" },
              { id: "2", claim: "b", status: "pending", scope: "task" },
            ],
          },
        ),
      );
    });
    await act(async () => {
      await handler(
        dEnv(
          {
            run_id: "run1",
            assignment_id: "a1",
            origin_participant_id: "worker-1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "开干" },
        ),
      );
    });

    // 等到任务条出现（run 已活跃）再断言：topbar goal 现已收起·两处都不渲目标条。
    await waitFor(() =>
      expect(container.querySelector(".taskstack")).not.toBeNull(),
    );
    expect(
      container.querySelector(".sf-head__main .goal-wrap--topbar"),
    ).not.toBeNull();
  });

  it("②a：team run 活跃时任务条保留", async () => {
    mockBasicApp();
    const { container } = render(<App />);
    await screen.findByText("Claude Code");

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "开干" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const handler = agentEventCb();
    await act(async () => {
      await handler(
        dEnv(
          { run_id: "run1" },
          {
            kind: "goal_declared",
            goal: "开干",
            status: "frozen",
            lead: "Claude",
            criteria: [
              { id: "1", claim: "a", status: "pending", scope: "task" },
              { id: "2", claim: "b", status: "pending", scope: "task" },
            ],
          },
        ),
      );
    });
    await act(async () => {
      await handler(
        dEnv(
          {
            run_id: "run1",
            assignment_id: "a1",
            origin_participant_id: "worker-1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "开干" },
        ),
      );
    });

    await waitFor(() => {
      expect(container.querySelector(".taskstack")).not.toBeNull();
    });
  });

  it("reload 遇中断 run → 显中断条 + 干净重派入口（spec §5.3）", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
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
      if (cmd === "list_interrupted_team_runs")
        return Promise.resolve([
          {
            session_id: args.sessionId,
            run_id: "old-run",
            goal: "上次的目标",
            lead_participant_id: "lead",
            assignments_json: "[]",
          },
        ]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "start_team_run") return Promise.resolve("team-run");
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      return Promise.resolve();
    });

    render(<App />);

    expect(await screen.findByText("上轮中断（重启）")).toBeInTheDocument();
    expect(screen.getByText("上次的目标")).toBeInTheDocument();

    fireEvent.click(screen.getByText("从头干净重派一次 ›"));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "start_team_run",
        expect.objectContaining({
          sessionId: "s1",
          goal: "上次的目标",
        }),
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText("上轮中断（重启）")).toBeNull(),
    );
  });

  it("saved lead + 空成员池：干净重派不把 start_team_run 成员回退成当前 agent", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents")
        return Promise.resolve([
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
      if (cmd === "get_session_agent_config")
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: "lead-a",
          member_agent_ids: [],
        });
      if (cmd === "list_interrupted_team_runs")
        return Promise.resolve([
          {
            session_id: args.sessionId,
            run_id: "old-run",
            goal: "上次的目标",
            lead_participant_id: "lead",
            assignments_json: "[]",
          },
        ]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "start_team_run") return Promise.resolve("team-run");
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      return Promise.resolve();
    });

    render(<App />);

    expect(await screen.findByText("上轮中断（重启）")).toBeInTheDocument();
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByText("从头干净重派一次 ›"));
    await act(async () => {
      await Promise.resolve();
    });

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
  });

  it("reload 含 team_run 历史的会话 → 不再恢复独立 Agent Team 模式按钮", async () => {
    const teamRunBlock: Extract<Block, { type: "team_run" }> = {
      type: "team_run",
      run_id: "r1",
      goal: { goal: "X", status: "frozen", criteria: [] },
      lead: "Claude",
      members: [],
    };
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            content: [teamRunBlock],
          },
        ]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "list_interrupted_team_runs") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      return Promise.resolve();
    });

    render(<App />);

    expect(await screen.findByText("会话一")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /选择模式/ })).toBeNull();
  });

  it("team_run 会话（无 dispatch_card）→ topbar 不出 taskbtn", async () => {
    const teamRunBlockWithMember: Extract<Block, { type: "team_run" }> = {
      type: "team_run",
      run_id: "r1",
      goal: { goal: "X", status: "frozen", criteria: [] },
      lead: "Claude",
      members: [
        {
          participant_id: "a1",
          assignment_id: "a1",
          task_id: "a1",
          name: "worker",
          status: "done",
          sub: "do thing",
          steps_total: 0,
          steps_done: 0,
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          failed: false,
          blocks: [],
        },
      ],
    };
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "team run session",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            content: [teamRunBlockWithMember],
          },
        ]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "list_interrupted_team_runs") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [localRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([localRepo]);
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      return Promise.resolve();
    });

    const { container } = render(<App />);
    expect(await screen.findByText("team run session")).toBeInTheDocument();
    expect(container.querySelector(".taskbtn")).toBeNull();
  });

  it("重载 Worker report 水合为既有任务条，查看打开右面板 TaskInspector", async () => {
    const reportText = [
      "[Worker report]",
      "agent: Reload Worker",
      "assignment_id: reload-a1",
      "status: done",
      "changed_files:",
      "- app/src/a.ts (+3/-1)",
      "final_text:",
      "完整右面板原文",
    ].join("\n");
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          engine: "agent-team",
          agent_id: "claude",
          agent_name_snapshot: "Claude 队长",
          content: [{ type: "text", text: "我来派单。" }],
        },
        {
          role: "assistant",
          engine: "agent-team",
          agent_id: "worker-1",
          created_at: 1234,
          content: [{ type: "text", text: reportText }],
        },
      ],
    });

    const { container } = render(<App />);

    expect(await screen.findByText("Reload Worker")).toBeInTheDocument();
    expect(screen.getByText("DONE")).toHaveClass("toolcard__badge--done");
    expect(screen.queryByText(/完整右面板原文/)).not.toBeInTheDocument();
    const workerRow = container.querySelector(".workerrow");
    expect(workerRow).not.toBeNull();
    const cardTurn = workerRow?.closest(".turn");
    expect(cardTurn).not.toBeNull();
    expect(cardTurn?.querySelector(".turn__name")).toHaveTextContent(
      "Claude 队长",
    );
    const authorNames = Array.from(
      container.querySelectorAll(".turn__name"),
      (node) => node.textContent,
    );
    expect(authorNames).not.toContain("Reload Worker");
    const view = within(workerRow as HTMLElement).getByText("查看");
    expect(view).toBeInTheDocument();

    fireEvent.click(view);

    await waitFor(() =>
      expect(container.querySelector(".task-inspector")).not.toBeNull(),
    );
    expect(screen.getByText(/完整右面板原文/)).toBeInTheDocument();
    expect(screen.getByLabelText("收起右面板")).toBeInTheDocument();
  });

  it("点队员卡进入右面板 drill，返回时恢复进入前 tab", async () => {
    mockAppWithReview();
    render(<App />);
    await screen.findByText("Claude Code");

    await openReviewPanel();
    const cb = agentEventCb();
    act(() => {
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a1",
            origin_participant_id: "worker-1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "X" },
        ),
      );
    });

    fireEvent.click(
      (await screen.findByText("worker-1")).closest('[role="button"]')!,
    );

    expect(await screen.findByLabelText("回 Lead")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("回 Lead"));
    await waitFor(() => expect(screen.queryByLabelText("回 Lead")).toBeNull());
    expect(await screen.findByText(/改动 ·/)).toBeInTheDocument();
    expect(screen.queryByText("选一个工具开成 tab")).toBeNull();
  });

  it("inspector_and_view_run_are_mutually_exclusive", async () => {
    const inspector = appMember({
      assignment_id: "inspect-a1",
      name: "Inspector Worker",
      sub: "Inspect detail",
    });
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [
            {
              type: "dispatch_card",
              run_id: "worker-run",
              member: inspector,
            },
          ],
        },
        {
          role: "assistant",
          engine: "claude",
          content: [runCard("r1", 1)],
        },
      ],
    });
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "session_review") return Promise.resolve(reviewWithChanges);
      return baseInvoke?.(cmd, args);
    });
    const { container } = render(<App />);

    fireEvent.click(
      (await screen.findByText("Inspector Worker")).closest('[role="button"]')!,
    );
    await waitFor(() =>
      expect(container.querySelector(".task-inspector")).not.toBeNull(),
    );

    fireEvent.click(
      within(screen.getByRole("group", { name: "本轮改动" })).getByRole(
        "button",
        { name: "查看" },
      ),
    );

    expect(await screen.findByText(/改动 ·/)).toBeInTheDocument();
    await waitFor(() =>
      expect(container.querySelector(".task-inspector")).toBeNull(),
    );
  });

  it("open_inspector_clears_drill", async () => {
    const drillMember = appMember({
      assignment_id: "drill-a1",
      name: "Drill Worker",
      sub: "Drill detail",
    });
    const inspector = appMember({
      assignment_id: "inspect-a1",
      name: "Inspector Worker",
      sub: "Inspect detail",
    });
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [
            {
              type: "team_run",
              run_id: "r-drill",
              goal: null,
              lead: "Claude",
              members: [drillMember],
            },
          ],
        },
        {
          role: "assistant",
          content: [
            {
              type: "dispatch_card",
              run_id: "worker-run",
              member: inspector,
            },
          ],
        },
      ],
    });
    const { container } = render(<App />);

    fireEvent.click(
      (await screen.findByText("Drill Worker")).closest('[role="button"]')!,
    );
    expect(await screen.findByLabelText("回 Lead")).toBeInTheDocument();

    fireEvent.click(
      screen.getByText("Inspector Worker").closest('[role="button"]')!,
    );
    await waitFor(() =>
      expect(container.querySelector(".task-inspector")).not.toBeNull(),
    );

    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    await waitFor(() =>
      expect(screen.queryByLabelText("回 Lead")).not.toBeInTheDocument(),
    );

    fireEvent.click(await screen.findByLabelText("展开右面板"));

    expect(await screen.findByText("选一个工具开成 tab")).toBeInTheDocument();
    expect(screen.queryByLabelText("回 Lead")).not.toBeInTheDocument();
  });

  it("派单事件 live 注入主区（跑完前执行中卡即可见）", async () => {
    mockBasicApp();
    const { container } = render(<App />);
    await screen.findByText("Claude Code");
    const cb = agentEventCb();

    act(() => {
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a1",
            origin_participant_id: "worker-1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "做X" },
        ),
      );
    });

    await waitFor(() =>
      expect(container.querySelector(".taskstack")).not.toBeNull(),
    );
  });

  it("块B·路B coding 闭环 applied 后主区补出 lead verdict（接线）", async () => {
    mockBasicApp(agentProfiles, {
      session: { repo_id: "user-project", in_place: true },
    });
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "list_acceptance")
        return Promise.resolve([
          {
            id: "c1",
            session_id: "s1",
            run_id: args?.runId ?? "r-verdict",
            task_id: "task-1",
            contract_id: null,
            scope: "task",
            claim: "测试通过",
            verifier: "npm test",
            evidence: null,
            status: "pending",
            waiver: null,
            created_at: 0,
          },
        ]);
      if (cmd === "finalize_member_artifact") return Promise.resolve("art-1");
      if (cmd === "run_landing_info")
        return Promise.resolve({ landedHead: "landed-head-1" });
      if (cmd === "run_verifier_artifact") return Promise.resolve("ver-1");
      if (cmd === "latest_verification_for_artifact_cmd")
        return Promise.resolve({ verdict: "passed", artifact_sha: "sha-1" });
      if (cmd === "merge_artifact_to_staging") return Promise.resolve();
      if (cmd === "apply_run_to_current_branch") return Promise.resolve();
      if (cmd === "append_message") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");
    const cb = agentEventCb();

    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "r-verdict",
            assignment_id: "a1",
            task_id: "task-1",
            origin_participant_id: "worker-1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "改 README" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r-verdict",
            assignment_id: "a1",
            task_id: "task-1",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 1,
            final_text: null,
            result: {
              anchor: { base_sha: "base-1" },
              changed_files: [{ path: "README.md" }],
            },
          },
        ),
      );
      await Promise.resolve();
    });

    // applied 终态后主区补出 verdict（结果节含「改动已落地」模板句 = 接线成立）
    await screen.findByText(/改动已落地/);
  });

  it("run 全终态 → optimistic append + DB append_message（P1 不闪）", async () => {
    mockBasicApp();
    const { container } = render(<App />);
    await screen.findByText("Claude Code");
    const cb = agentEventCb();

    act(() => {
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a1",
            origin_participant_id: "w1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "X" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a2",
            origin_participant_id: "w2",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "Y" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a1",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 1,
            final_text: null,
          },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a2",
            status_transition: "failed",
          },
          { kind: "text_delta", text: "炸了" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a1",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 1,
            final_text: null,
          },
        ),
      );
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "append_message",
        expect.objectContaining({
          sessionId: "s1",
          role: "assistant",
          content: [
            expect.objectContaining({ type: "team_run", run_id: "r1" }),
          ],
        }),
      ),
    );
    const teamAppends = invokeMock.mock.calls.filter(
      (c) =>
        c[0] === "append_message" &&
        Array.isArray(c[1]?.content) &&
        c[1].content.some(
          (b: Block) => b?.type === "team_run" && b?.run_id === "r1",
        ),
    );
    expect(teamAppends).toHaveLength(1);
    // 块B（GUI 验收折）：该 run 是 a1 done + a2 failed 的多 worker·非 coding run → team_run 任务条**保留**
    // （BackgroundTaskStack 渲 DONE 队员行·非空壳）+ 完成态 verdict 并存（用户定：任务条 + verdict 都留）。
    await waitFor(() =>
      expect(container.querySelector(".lead-summary")).not.toBeNull(),
    );
    expect(container.querySelectorAll(".taskstack")).toHaveLength(1);
  });

  it("单 worker complete 后拉验收并追加 lead_summary，team_run 只持久化一次", async () => {
    mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");
    const cb = agentEventCb();

    act(() => {
      cb(
        dEnv(
          { run_id: "r-single" },
          {
            kind: "goal_declared",
            goal: "实现单 worker 汇总",
            status: "frozen",
            lead: "Claude Code",
            criteria: [],
          },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r-single",
            assignment_id: "a1",
            origin_participant_id: "worker-1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "实现单 worker 汇总" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r-single",
            assignment_id: "a1",
          },
          { kind: "thinking_delta", text: "复核输出" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r-single",
            assignment_id: "a1",
          },
          { kind: "text_delta", text: "worker delivered" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r-single",
            assignment_id: "a1",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 2,
            final_text: null,
          },
        ),
      );
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("list_acceptance", {
        sessionId: "s1",
        runId: "r-single",
      }),
    );
    const summaryText = await waitFor(() => {
      const summary = screen.getAllByText("worker delivered").find((el) => {
        const turn = el.closest(".turn");
        return turn ? within(turn as HTMLElement).queryByText("· 队长") : false;
      });
      expect(summary).toBeTruthy();
      return summary!;
    });
    const summaryTurn = summaryText.closest(".turn");
    expect(summaryTurn).not.toBeNull();
    expect(
      within(summaryTurn as HTMLElement).getByText("· 队长"),
    ).toBeInTheDocument();

    // 幂等硬断言（opus 第二路）：重复终态事件不应触发第二次 team_run / summary append。
    // persistedRunsRef 的 pkey guard（add 在 void async 之外）必须挡住——否则下面两条 ===1 会变 2。
    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "r-single",
            assignment_id: "a1",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 2,
            final_text: null,
          },
        ),
      );
      await Promise.resolve();
    });

    const summaryAppends = invokeMock.mock.calls.filter(
      (c) =>
        c[0] === "append_message" &&
        Array.isArray(c[1]?.content) &&
        c[1].content.some(
          (b: Block) => b?.type === "lead_summary" && b?.run_id === "r-single",
        ),
    );
    expect(summaryAppends).toHaveLength(1);
    expect(summaryAppends[0][1]).toEqual(
      expect.objectContaining({
        role: "assistant",
        agentNameSnapshot: "Claude Code",
      }),
    );

    const teamAppends = invokeMock.mock.calls.filter(
      (c) =>
        c[0] === "append_message" &&
        Array.isArray(c[1]?.content) &&
        c[1].content.some(
          (b: Block) => b?.type === "team_run" && b?.run_id === "r-single",
        ),
    );
    expect(teamAppends).toHaveLength(1);
  });

  it("dispatch-all-first 的 team run 持久化 block 保留全部 member", async () => {
    mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");
    const cb = agentEventCb();

    act(() => {
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a1",
            origin_participant_id: "w1",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "X" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a2",
            origin_participant_id: "w2",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "Y" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a3",
            origin_participant_id: "w3",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "Z" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a1",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 1,
            final_text: null,
          },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a2",
            status_transition: "failed",
          },
          { kind: "text_delta", text: "炸了" },
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r1",
            assignment_id: "a3",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 1,
            final_text: null,
          },
        ),
      );
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "append_message",
        expect.objectContaining({
          sessionId: "s1",
          role: "assistant",
          content: [
            expect.objectContaining({ type: "team_run", run_id: "r1" }),
          ],
        }),
      ),
    );
    const teamAppends = invokeMock.mock.calls.filter(
      (c) =>
        c[0] === "append_message" &&
        c[1]?.content?.[0]?.type === "team_run" &&
        c[1].content[0].run_id === "r1",
    );
    const block = teamAppends[teamAppends.length - 1]?.[1]
      .content[0] as Extract<Block, { type: "team_run" }>;
    expect(block.members).toHaveLength(3);
  });

  it("backcompat：老 Normal 消息（无 team_run）仍单线渲染、不出折叠行", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            content: [{ type: "text", text: "普通回复" }],
            engine: "claude",
          },
        ]);
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
      return Promise.resolve([]);
    });

    const { container } = render(<App />);
    expect(await screen.findByText("普通回复")).toBeInTheDocument();
    expect(container.querySelector(".team-run")).toBeNull();
  });
});
