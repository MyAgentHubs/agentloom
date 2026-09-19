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
import type { CodingState } from "./lib/codingLoop";
import * as codingLoopDriver from "./lib/codingLoopDriver";
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
    githubNamespace,
    githubRepo,
    agentProfile,
    mockBasicApp,
    configureTeamLead,
    agentEventCb,
    dEnv,
    decisionCardMessage,
    inlineDecisionCard,
    clickDecisionOption,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("crash 续：恢复 autonomy 后端态·UI 不再渲旋钮", async () => {
    mockBasicApp();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
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

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "get_lead_loop_state",
        expect.objectContaining({ sessionId: "s1" }),
      );
    });
    expect(screen.queryByRole("radiogroup")).toBeNull();
  });

  it("team composer 不调 lead_step/propose_team_plan（旧路径封闭）", async () => {
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
      if (cmd === "start_lead_session") return Promise.resolve();
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
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "这项目做什么" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "start_lead_session",
        expect.anything(),
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

  it("后台 A 的 coding loop 按事件所属会话取 isInPlace，不受当前展示 B 影响", async () => {
    mockBasicApp();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s-b", title: "会话 B", in_place: false }),
          makeSession({ id: "s-a", title: "会话 A", in_place: true }),
        ]);
      if (cmd === "finalize_member_artifact")
        return Promise.resolve("artifact-session-a");
      if (cmd === "run_landing_info")
        return Promise.resolve({ landedHead: "session-a-head" });
      return defaultInvoke?.(cmd, args);
    });

    const observedStates: CodingState[] = [];
    const advanceCodingLoop = codingLoopDriver.advanceCodingLoop;
    vi.spyOn(codingLoopDriver, "advanceCodingLoop").mockImplementation(
      async (state, invoker) => {
        observedStates.push(state);
        return advanceCodingLoop(state, invoker);
      },
    );

    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s-b",
      }),
    );

    const cb = agentEventCb();
    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "r-session-a",
            assignment_id: "a-session-a",
            task_id: "task-session-a",
            origin_participant_id: "worker-session-a",
            status_transition: "dispatched",
          },
          { kind: "text_delta", text: "改 A" },
          "s-a",
        ),
      );
      cb(
        dEnv(
          {
            run_id: "r-session-a",
            assignment_id: "a-session-a",
            task_id: "task-session-a",
            status_transition: "done",
          },
          {
            kind: "completed",
            cost_usd: null,
            input_tokens: 1,
            output_tokens: 1,
            final_text: null,
            result: {
              anchor: { base_sha: "base-session-a" },
              changed_files: [{ path: "README.md" }],
            },
          },
          "s-a",
        ),
      );
      await Promise.resolve();
    });

    await waitFor(() =>
      expect(observedStates).toContainEqual(
        expect.objectContaining({
          runId: "r-session-a",
          sessionId: "s-a",
          isInPlace: true,
        }),
      ),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("run_landing_info", {
        sessionId: "s-a",
        runId: "r-session-a",
      }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "merge_artifact_to_staging",
      expect.anything(),
    );
  });

  // T4 trust-land 反转：旧契约「无 verifier → 阻断落地（已阻止·不 merge/apply）」。
  // 新契约：in-place 会话 finalize 即落地（后端已置 merged + 记 LandingCommit）→ 直达 applied·
  // 不进 verify/merge/apply，landedHead 取 finalize 结果。
  it("无 verifier 时 in-place 会话信任落地·直达 applied·不 verify/merge/apply（T4 trust-land）", async () => {
    mockBasicApp();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", repo_id: "local-project", in_place: true }),
        ]);
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      // T7：finalize 返回 artifact_id（run-…）；landedHead 由 run_landing_info 给真 git sha。
      if (cmd === "finalize_member_artifact")
        return Promise.resolve("run-no-v-0001");
      if (cmd === "run_landing_info")
        return Promise.resolve({
          landedHead: "localhead1234567",
          preHead: "base-1",
          filesChanged: 1,
          insertions: 1,
          deletions: 0,
          files: [{ path: "README.md", insertions: 1, deletions: 0 }],
        });
      if (cmd === "merge_artifact_to_staging") return Promise.resolve();
      if (cmd === "apply_run_to_current_branch") return Promise.resolve();
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "list_acceptance") return Promise.resolve([]);
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "ready" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );

    const cb = agentEventCb();
    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "r-no-v",
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
            run_id: "r-no-v",
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

    // 旧断言：findByText("已阻止")。新断言：finalize 即落地·展示 landedHead（落地到当前分支）。
    await waitFor(() =>
      expect(screen.getByText(/localhea/)).toBeInTheDocument(),
    );
    expect(screen.queryByText("已阻止")).toBeNull();
    // in-place trust-land：finalize 即落地·不进 verify/merge/apply。
    const cmds = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(cmds).toContain("finalize_member_artifact");
    expect(cmds).not.toContain("run_verifier_artifact");
    expect(cmds).not.toContain("merge_artifact_to_staging");
    expect(cmds).not.toContain("apply_run_to_current_branch");
  });

  it("github_org in-place 会话 finalize 后直达 applied，不调 merge/apply", async () => {
    mockBasicApp();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      // namespace 是 github_org，但后端明确标记为 in-place。
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "gh-repo",
            namespace_id: "gh-org-x",
            in_place: true,
          }),
        ]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace, githubNamespace],
          active_namespace_id: "gh-org-x",
          active_repo_id: "gh-repo",
          repos: [githubRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([githubRepo]);
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "list_acceptance")
        return Promise.resolve([
          {
            id: "c1",
            session_id: "s1",
            run_id: args?.runId ?? "r-block",
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
      if (cmd === "finalize_member_artifact")
        return Promise.resolve("art-block");
      if (cmd === "run_landing_info")
        return Promise.resolve({ landedHead: "github-head-123456" });
      if (cmd === "merge_artifact_to_staging") return Promise.resolve("mc");
      if (cmd === "apply_run_to_current_branch")
        return Promise.resolve("landed");
      if (cmd === "append_message") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "ready" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );

    const cb = agentEventCb();
    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "r-block",
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
            run_id: "r-block",
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

    await waitFor(() =>
      expect(screen.getByText(/github-h/)).toBeInTheDocument(),
    );
    const commands = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(commands).toContain("finalize_member_artifact");
    expect(commands).not.toContain("run_verifier_artifact");
    expect(commands).not.toContain("merge_artifact_to_staging");
    expect(commands).not.toContain("apply_run_to_current_branch");
  });

  // local-default 是用户可见的「我的项目」，与其它项目一样就地完成；不得再进旧 merge/apply 链。
  it("local-default run finalize 后正常收场且不调用 merge/apply", async () => {
    mockBasicApp();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
            in_place: true,
          }),
        ]);
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "list_acceptance")
        return Promise.resolve([
          {
            id: "c1",
            session_id: "s1",
            run_id: args?.runId ?? "r-block",
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
      if (cmd === "finalize_member_artifact")
        return Promise.resolve("art-block");
      if (cmd === "run_landing_info")
        return Promise.resolve({ landedHead: "local-default-head" });
      if (cmd === "merge_artifact_to_staging")
        return Promise.reject(
          new Error("local-default must not enter the legacy merge chain"),
        );
      if (cmd === "append_message") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "ready" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );

    const cb = agentEventCb();
    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "r-block",
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
            run_id: "r-block",
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

    await waitFor(() =>
      expect(screen.getByText(/local-de/)).toBeInTheDocument(),
    );
    expect(screen.queryByText(/执行出错/)).toBeNull();
    const commands = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(commands).toContain("finalize_member_artifact");
    expect(commands).not.toContain("merge_artifact_to_staging");
    expect(commands).not.toContain("apply_run_to_current_branch");
  });

  // B2b 关自动落地：repo 会话 finalize→merge 后停在 staging（applying·非 terminal）·
  // 不再自动调 apply_run_to_current_branch·发停隔离区叙事·run 留在 codingLoopsRef 等用户点改动条。
  it("repo 会话 merge 进暂存后停隔离区·不自动落地·发停隔离区叙事（b2b 关自动落地）", async () => {
    mockBasicApp();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      // 改成 github(repo) 会话 → 走 repo merging→applying 分支。
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "gh-repo",
            namespace_id: "gh-org-x",
          }),
        ]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace, githubNamespace],
          active_namespace_id: "gh-org-x",
          active_repo_id: "gh-repo",
          repos: [githubRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([githubRepo]);
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "list_acceptance")
        return Promise.resolve([
          {
            id: "c1",
            session_id: "s1",
            run_id: args?.runId ?? "r-v",
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
      if (cmd === "finalize_member_artifact") return Promise.resolve("art-v");
      if (cmd === "merge_artifact_to_staging") return Promise.resolve("mc-v");
      if (cmd === "apply_run_to_current_branch")
        return Promise.resolve("abcdef1234567890");
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
            run_id: "r-v",
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
            run_id: "r-v",
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

    // 停隔离区叙事到达即说明 loop 跑到 applying 停住（队长在 break 后这个分支发的）。
    expect(await screen.findByText(/干完了，改动在隔离区/)).toBeInTheDocument();
    const calls = invokeMock.mock.calls.map(([cmd]) => cmd);
    // trust-land：repo 跳 verifying → 不再调 run_verifier_artifact；finalize→merge 仍发生。
    expect(calls).toContain("merge_artifact_to_staging");
    expect(calls).not.toContain("run_verifier_artifact");
    // b2b 关自动落地：merge 进 staging 后停在 applying·绝不再自动调 apply_run_to_current_branch。
    expect(calls).not.toContain("apply_run_to_current_branch");
    // 还没落地·不能出现 landedHead；卡片措辞绝不能写「已落地」。
    expect(screen.queryByText(/abcdef12/)).toBeNull();
    expect(screen.queryByText("已落地")).toBeNull();
  });

  // T-C3 b2b：交付动作路由——队长吐 create_pr → handleLeadOutcome 调后端 create_pr_run。
  // 先用 repo 会话 worker run 停在 applying（run 留在 codingLoopsRef）·再点决策选项让 lead_step 回 create_pr。
  it("队长 create_pr → 路由调后端 create_pr_run + append PR url", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
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
      { messages: [decisionCardMessage(["开 PR"])] },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "gh-repo",
            namespace_id: "gh-org-x",
          }),
        ]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace, githubNamespace],
          active_namespace_id: "gh-org-x",
          active_repo_id: "gh-repo",
          repos: [githubRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([githubRepo]);
      if (cmd === "get_lead_loop_state")
        return Promise.resolve({
          sessionId: "s1",
          autonomy: "auto",
          activeRunId: null,
          activeTaskId: null,
          lastEventCursor: null,
        });
      if (cmd === "list_acceptance")
        return Promise.resolve([
          {
            id: "c1",
            session_id: "s1",
            run_id: args?.runId ?? "r-pr",
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
      if (cmd === "finalize_member_artifact") return Promise.resolve("art-pr");
      if (cmd === "merge_artifact_to_staging") return Promise.resolve("mc-pr");
      // 队长被叫 → 决策 create_pr。
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: {
            action: "create_pr",
            rationale: "改完了开个 PR",
            title: "feat: x",
          },
          decisionCard: null,
        });
      if (cmd === "create_pr_run")
        return Promise.resolve("https://github.com/o/r/pull/7");
      if (cmd === "append_message") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    // 先把 worker run 跑到 applying（停隔离区）·让 run 落进 codingLoopsRef。
    const cb = agentEventCb();
    await act(async () => {
      cb(
        dEnv(
          {
            run_id: "r-pr",
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
            run_id: "r-pr",
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
    expect(await screen.findByText(/干完了，改动在隔离区/)).toBeInTheDocument();

    // 决策选项喂回 lead → lead_step 回 create_pr → 路由到 create_pr_run。
    await clickDecisionOption("开 PR");

    await waitFor(() => {
      expect(
        invokeMock.mock.calls.some(
          ([cmd, callArgs]) =>
            cmd === "create_pr_run" &&
            callArgs?.sessionId === "s1" &&
            callArgs?.runId === "r-pr" &&
            callArgs?.confirmed === true,
        ),
      ).toBe(true);
    });
    expect(confirmSpy).toHaveBeenCalledWith(
      expect.stringMatching(/Pull Request/),
    );
    // append 的结果消息带 rationale + PR url（markdown 把 url 渲成 autolink·分元素·分别核）。
    expect(await screen.findByText(/改完了开个 PR/)).toBeInTheDocument();
    expect(
      await screen.findByText("https://github.com/o/r/pull/7"),
    ).toBeInTheDocument();
  });

  it("决策卡任意 option：先 CAS，再把 option 喂回 lead，不走 dispatch_confirm 直派 worker", async () => {
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
      {
        messages: [
          decisionCardMessage(["开跑", "先只读探一下", "我来调整"], {
            decision_id: "dc-1",
            // 历史卡也必须走统一 onDecisionChoose，不再按 kind 直派 worker。
            kind: "dispatch_confirm",
            question: "派 worker 改 README.md，可以吗？",
            recommended: "开跑",
            rationale: "改 README",
            payload: {
              run_id: "run-pre",
              task: "写新闻",
              scope_files: ["README.md"],
              agent_hint: null,
            },
            source_run_id: "run-pre",
          }),
        ],
      },
    );
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "lead_step" && args?.userMsg === "开跑")
        return Promise.resolve({
          status: "decided",
          action: { action: "reply", rationale: "接着处理" },
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
      if (cmd === "choose_decision_card") return Promise.resolve(true);
      if (cmd === "append_message") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByText("Claude Code");

    await configureTeamLead();
    expect(
      inlineDecisionCard().getByText(/派 worker 改 README/),
    ).toBeInTheDocument();
    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /开跑/ }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "lead_step",
        expect.objectContaining({ userMsg: "开跑" }),
      ),
    );
    expect(invokeMock).toHaveBeenCalledWith(
      "choose_decision_card",
      expect.objectContaining({
        decisionId: "dc-1",
        expectStatus: "pending",
        nextStatus: "submitting",
      }),
    );
    expect(invokeMock).toHaveBeenCalledWith(
      "choose_decision_card",
      expect.objectContaining({
        decisionId: "dc-1",
        expectStatus: "submitting",
        nextStatus: "chosen",
        chosenOption: "开跑",
      }),
    );
    const submittingIndex = invokeMock.mock.calls.findIndex(
      ([cmd, args]) =>
        cmd === "choose_decision_card" &&
        args?.decisionId === "dc-1" &&
        args?.nextStatus === "submitting",
    );
    const optionLeadStepIndex = invokeMock.mock.calls.findIndex(
      ([cmd, args]) => cmd === "lead_step" && args?.userMsg === "开跑",
    );
    expect(submittingIndex).toBeGreaterThanOrEqual(0);
    expect(optionLeadStepIndex).toBeGreaterThan(submittingIndex);
    expect(invokeMock).not.toHaveBeenCalledWith(
      "record_lead_dispatch",
      expect.anything(),
    );
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain(
      "start_team_run",
    );
  });
});
