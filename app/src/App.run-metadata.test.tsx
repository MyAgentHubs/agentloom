import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
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
    agentProfiles,
    mockBasicApp,
    agentEventCb,
    dEnv,
    orchestratedTeamMessages,
    workerTerminalEvent,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("reload 含 team_run 历史 -> goalTitleByRun 从 get_run_goal_title 回填", async () => {
    const teamRunBlock: Extract<Block, { type: "team_run" }> = {
      type: "team_run",
      run_id: "r-reload-gt",
      goal: { goal: "X", status: "frozen", criteria: [] },
      lead: "Claude",
      members: [],
    };
    invokeMock.mockImplementation((cmd: string, _args?: any) => {
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
          { role: "assistant", content: [teamRunBlock] },
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
      if (cmd === "get_run_goal_title")
        return Promise.resolve("reload 后的短标题");
      return Promise.resolve();
    });

    render(<App />);
    expect(await screen.findByText("会话一")).toBeInTheDocument();

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("get_run_goal_title", {
        sessionId: "s1",
        runId: "r-reload-gt",
      });
    });
  });

  it("openSession 后并发加载所有 run 元数据且不阻塞主流程", async () => {
    const runBlocks = ["r-meta-1", "r-meta-2"].map(
      (runId): Extract<Block, { type: "team_run" }> => ({
        type: "team_run",
        run_id: runId,
        goal: { goal: runId, status: "frozen", criteria: [] },
        lead: "Claude",
        members: [],
      }),
    );
    const pendingMetadata = new Promise<never>(() => {});
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
        return Promise.resolve([{ role: "assistant", content: runBlocks }]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "list_acceptance" || cmd === "get_run_goal_title")
        return pendingMetadata;
      if (cmd === "get_session_goal") return Promise.resolve(null);
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
      return Promise.resolve();
    });

    render(<App />);
    expect(await screen.findByText("会话一")).toBeInTheDocument();

    await waitFor(() => {
      for (const runId of ["r-meta-1", "r-meta-2"]) {
        expect(invokeMock).toHaveBeenCalledWith("list_acceptance", {
          sessionId: "s1",
          runId,
        });
        expect(invokeMock).toHaveBeenCalledWith("get_run_goal_title", {
          sessionId: "s1",
          runId,
        });
      }
      expect(invokeMock).toHaveBeenCalledWith("get_session_goal", {
        sessionId: "s1",
      });
      expect(invokeMock).toHaveBeenCalledWith("list_interrupted_team_runs", {
        sessionId: "s1",
      });
    });
  });

  it("openSession 元数据迟到时不覆盖已切换会话的同 runId", async () => {
    const sharedRun: Extract<Block, { type: "team_run" }> = {
      type: "team_run",
      run_id: "r-shared",
      goal: { goal: "共享 run", status: "frozen", criteria: [] },
      lead: "Claude",
      members: [],
    };
    let resolveSessionATitle!: (title: string) => void;
    const sessionATitle = new Promise<string>((resolve) => {
      resolveSessionATitle = resolve;
    });
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s-a",
            title: "会话 A",
            repo_id: "local-default",
            namespace_id: "local",
          }),
          makeSession({
            id: "s-b",
            title: "会话 B",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([{ role: "assistant", content: [sharedRun] }]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "get_run_goal_title")
        return args?.sessionId === "s-a"
          ? sessionATitle
          : Promise.resolve("会话 B 标题");
      if (cmd === "get_session_goal") return Promise.resolve(null);
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
      return Promise.resolve();
    });

    render(<App />);
    await screen.findByText("会话 A");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_run_goal_title", {
        sessionId: "s-a",
        runId: "r-shared",
      }),
    );

    fireEvent.click(screen.getByText("会话 B"));
    expect(await screen.findByText("会话 B 标题")).toBeInTheDocument();

    await act(async () => {
      resolveSessionATitle("会话 A 迟到标题");
      await sessionATitle;
    });
    expect(screen.getByText("会话 B 标题")).toBeInTheDocument();
    expect(screen.queryByText("会话 A 迟到标题")).toBeNull();
  });

  it("orchestrated dispatch_card live -> goal-bar 渲出并显示 goal_title 短标题", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "user",
          content: [{ type: "text", text: "请做 orchestrated 任务" }],
        },
        {
          role: "assistant",
          content: [{ type: "text", text: "我来派发 worker。" }],
          engine: "agent-team",
          agent_id: "claude",
          agent_name_snapshot: "Claude Code",
        },
      ],
    });
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "get_run_goal_title")
        return Promise.resolve("orchestrated 短标题");
      return baseInvoke?.(cmd, args);
    });

    const { container } = render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();

    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "wrun-1",
            assignment_id: "wa1",
            origin_participant_id: "worker-p1",
            orchestrated: true,
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "实现功能" },
        ),
      );
    });

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("get_run_goal_title", {
        sessionId: "s1",
        runId: "wrun-1",
      });
      expect(container.querySelector(".goal-bar")).not.toBeNull();
    });
    expect(await screen.findByText("orchestrated 短标题")).toBeInTheDocument();
  });

  // T7：worker 唤醒 lead 续跑已统一收归后端 on_worker_settled（报告落账之后才触发，见
  // member_report_delivery 台账设计），前端不再自行 invoke resume_lead_session——即便
  // orchestrated worker 终态事件到达且 lead 空闲，也绝不产生这个 invoke（不再有「抢跑」
  // 空轮的风险）。
  it("worker 完成不再由前端唤醒 lead：orchestrated worker 终态事件 + lead 空闲 → 不 invoke resume_lead_session（唤醒权收归后端 on_worker_settled）", async () => {
    mockBasicApp(agentProfiles, { messages: orchestratedTeamMessages() });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();

    await act(async () => {
      cb(workerTerminalEvent("wrun-a", "wa-a"));
    });

    // 事件已处理（dispatch card 更新等），但绝不产生 resume_lead_session 这个 invoke。
    expect(invokeMock).not.toHaveBeenCalledWith(
      "resume_lead_session",
      expect.anything(),
    );
  });

  it("worker 完成不再由前端唤醒 lead：同一 worker 终态事件重放多次 → 始终不 invoke resume_lead_session", async () => {
    mockBasicApp(agentProfiles, { messages: orchestratedTeamMessages() });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();
    const ev = workerTerminalEvent("wrun-b", "wa-b");

    await act(async () => {
      cb(ev);
      cb(ev);
    });

    expect(invokeMock).not.toHaveBeenCalledWith(
      "resume_lead_session",
      expect.anything(),
    );
  });
});
