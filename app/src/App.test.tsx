import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
  within,
} from "@testing-library/react";
import type { ComponentProps } from "react";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { existsSync, readFileSync } from "fs";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type {
  AgentProfile,
  GroupMeta,
  LeadStepOutcome,
  MemberUnit,
  Session,
} from "./types/agent";
import { makeSession } from "./test/factories";

const { invokeMock, listenMock, openMock, sessionMainProps } = vi.hoisted(
  () => ({
    invokeMock: vi.fn(),
    listenMock: vi.fn(),
    openMock: vi.fn(),
    sessionMainProps: [] as Array<{
      onOpenPreview?: (path: string) => void;
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

import App, {
  applyEventTransportBatch,
  suppressBlockBShells,
  runIdForActiveCodingSession,
  pruneNavHistory,
  resetAutoResumeStreak,
} from "./App";
import type { CodingState } from "./lib/codingLoop";
import * as codingLoopDriver from "./lib/codingLoopDriver";
import type { Block, ChatMessage } from "./types/agent";
import { clearTeamConfigCache } from "./lib/useTeamConfig";

declare const process: { env: Record<string, string | undefined> };

describe("App", () => {
  beforeEach(() => {
    localStorage.clear();
    invokeMock.mockReset();
    listenMock.mockReset();
    openMock.mockReset();
    sessionMainProps.length = 0;
    clearTeamConfigCache();
    listenMock.mockResolvedValue(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  const localNamespace = {
    id: "local",
    kind: "local",
    name: "Local",
    is_builtin: 1,
    last_active_repo_id: "local-default",
    added_at: 0,
    last_used_at: null,
  };

  const localRepo = {
    id: "local-default",
    source: "local",
    owner: null,
    name: "Local 默认",
    path: "/tmp",
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: "local",
  };

  const githubNamespace = {
    id: "gh-org-x",
    kind: "github",
    name: "gh-org-x",
    is_builtin: 0,
    last_active_repo_id: "gh-repo",
    added_at: 0,
    last_used_at: null,
  };

  const githubRepo = {
    id: "gh-repo",
    source: "github",
    owner: "octo",
    name: "repo",
    path: "/tmp/repo",
    status: "active",
    added_at: 0,
    last_used_at: null,
    namespace_id: "gh-org-x",
  };

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

  const emptyReview = {
    has_changes: false,
    stat: "",
    patch: "",
    files_changed: 0,
  };

  function agentProfile(overrides: Partial<AgentProfile> = {}): AgentProfile {
    return {
      id: "claude",
      name: "Claude Code",
      access: "api",
      provider: "anthropic",
      primary_model: null,
      endpoint: null,
      auth_mode: null,
      model_opus: null,
      model_sonnet: null,
      model_haiku: null,
      model_subagent: null,
      reasoning_default: "auto",
      max_output_tokens: null,
      api_timeout_ms: null,
      compat_disable_betas: false,
      compat_disable_nonessential: false,
      compat_disable_thinking: false,
      compat_proxy: null,
      custom_headers: null,
      extra_body: null,
      cap_reasoning: null,
      cap_computer_use: null,
      cap_lead: null,
      has_key: true,
      is_builtin: true,
      enabled: true,
      sort_order: 0,
      created_at: 0,
      updated_at: 0,
      ...overrides,
    };
  }

  const agentProfiles = [
    agentProfile(),
    agentProfile({
      id: "deepseek",
      name: "DeepSeek",
      provider: "deepseek",
      sort_order: 1,
    }),
  ];
  const LAST_AGENT_ID_KEY = "agentloom.lastAgentId";

  function runCard(
    runId: string,
    filesChanged: number,
  ): Extract<Block, { type: "run_card" }> {
    return {
      type: "run_card",
      run_id: runId,
      commit_sha: `${runId}-sha`,
      files_changed: filesChanged,
      insertions: filesChanged * 10,
      deletions: filesChanged,
      interrupted: false,
    };
  }

  function appMember(overrides: Partial<MemberUnit> = {}): MemberUnit {
    return {
      participant_id: "p1",
      assignment_id: "a1",
      task_id: "t1",
      name: "worker-1",
      status: "running",
      sub: "改 README",
      steps_total: 1,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      ...overrides,
    };
  }

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

  const reviewWithChanges = {
    has_changes: true,
    stat: " a.txt | 1 +",
    patch: "diff --git a/a.txt b/a.txt\n@@ -0,0 +1 @@\n+hello\n",
    files_changed: 1,
  };

  function mockAppWithReview() {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          {
            id: "s1",
            title: "会话一",
            repo_id: "gh-repo",
            namespace_id: "gh-org-x",
          },
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(reviewWithChanges);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace, githubNamespace],
          active_namespace_id: "gh-org-x",
          active_repo_id: "gh-repo",
          repos: [githubRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([githubRepo]);
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      return Promise.resolve();
    });
  }

  async function openReviewPanel() {
    fireEvent.click(await screen.findByRole("button", { name: "展开右面板" }));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+hello");
  }

  function keepPreviewLoading() {
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "read_attachment") return new Promise(() => {});
      return fallback?.(cmd, args);
    });
  }

  function mockBasicApp(
    agentList: AgentProfile[] = agentProfiles,
    options: {
      messages?: ChatMessage[];
      session?: Partial<Session>;
      runtimeDetect?: {
        claude: { available: boolean };
        codex: { available: boolean };
      };
    } = {},
  ) {
    const sendCalls: any[] = [];
    const messages = options.messages ?? [];
    const teamConfigStore = new Map<
      string,
      { leadAgentId: string | null; memberAgentIds: string[] }
    >();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve(agentList);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
            ...options.session,
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve(messages);
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
      if (cmd === "send_message") {
        sendCalls.push(args);
        return Promise.resolve();
      }
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "start_team_run") return Promise.resolve("team-run");
      if (cmd === "answer_lead_question")
        return Promise.reject(`NO_PENDING_QUESTION:${args?.decisionId ?? ""}`);
      if (cmd === "choose_decision_card") return Promise.resolve(true);
      if (cmd === "get_session_agent_config") {
        const cfg = teamConfigStore.get(args.sessionId) ?? {
          leadAgentId: null,
          memberAgentIds: ["deepseek"],
        };
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: cfg.leadAgentId,
          member_agent_ids: cfg.memberAgentIds,
        });
      }
      if (cmd === "set_session_agent_config") {
        const cfg = {
          leadAgentId: args.leadAgentId ?? null,
          memberAgentIds: [...(args.memberAgentIds ?? [])],
        };
        teamConfigStore.set(args.sessionId, cfg);
        return Promise.resolve({
          session_id: args.sessionId,
          lead_agent_id: cfg.leadAgentId,
          member_agent_ids: cfg.memberAgentIds,
        });
      }
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "detect_runtime")
        return Promise.resolve(
          options.runtimeDetect ?? {
            claude: { available: true },
            codex: { available: true },
          },
        );
      return Promise.resolve();
    });
    return { sendCalls };
  }

  function mockRemovableProjectApp() {
    let projectArchived = false;
    const projectRepo = {
      ...localRepo,
      id: "project-novel",
      name: "我的小说",
      path: "/tmp/my-novel",
    };
    const projectSessions = [
      makeSession({ id: "novel-1", repo_id: projectRepo.id }),
      makeSession({ id: "novel-2", repo_id: projectRepo.id }),
      makeSession({
        id: "novel-archived",
        repo_id: projectRepo.id,
        archived: true,
      }),
      makeSession({ id: "other-project", repo_id: "another-project" }),
    ];

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve(projectSessions);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(emptyReview);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              ...localNamespace,
              last_active_repo_id: projectRepo.id,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: projectRepo.id,
          repos: [projectRepo],
        });
      if (cmd === "archive_repo") {
        projectArchived = true;
        return Promise.resolve();
      }
      if (cmd === "list_repos")
        return Promise.resolve(projectArchived ? [localRepo] : [projectRepo]);
      if (cmd === "list_namespaces") return Promise.resolve([localNamespace]);
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "detect_runtime")
        return Promise.resolve({
          claude: { available: true },
          codex: { available: true },
        });
      return Promise.resolve();
    });

    return projectRepo;
  }

  function escapeRegExp(value: string): string {
    return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }

  async function configureTeamLead(
    leadName = "Claude Code",
    memberName?: string,
  ) {
    fireEvent.click(
      screen.getByRole("button", { name: `选择 agent：${leadName}` }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: `设为队长 ${leadName}` }),
    );
    const teamTriggerName = new RegExp(
      `选择 agent：队长 ${escapeRegExp(leadName)}`,
    );
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: teamTriggerName }),
      ).toBeInTheDocument(),
    );

    if (!memberName) return;

    let memberToggle = screen.queryByRole("button", {
      name: `成员 ${memberName}`,
    });
    if (!memberToggle) {
      fireEvent.click(screen.getByRole("button", { name: teamTriggerName }));
      memberToggle = screen.getByRole("button", { name: `成员 ${memberName}` });
    }
    if (memberToggle.getAttribute("aria-pressed") !== "true") {
      fireEvent.click(memberToggle);
    }
    await waitFor(() =>
      expect(
        screen.getByRole("button", {
          name: new RegExp(
            `选择 agent：队长 ${escapeRegExp(leadName)}，成员 1`,
          ),
        }),
      ).toBeInTheDocument(),
    );
  }

  function agentEventCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    if (!handler) throw new Error("agent-event listener 未注册");
    return handler as (e: { payload: unknown }) => void;
  }

  function agentEventBatchCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event-batch",
    )?.[1];
    if (!handler) throw new Error("agent-event-batch listener 未注册");
    return handler as (e: { payload: unknown }) => void;
  }

  function sessionReviewCallCount() {
    return invokeMock.mock.calls.filter(
      ([command]) => command === "session_review",
    ).length;
  }

  async function startRunCloseoutLiveUi() {
    const { sendCalls } = mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    // commit 2：撤销按钮现在要 undo_total > 0 才显示。closeout 收尾后 App 会重新拉
    // list_run_commits（见 App.tsx run_closeout 分支新增的 refreshRunStates 调用），
    // 这里把每个跑完的 run_id 记下来、原样回填 undo_total:1——既保真反映「这轮真有可撤销
    // 记录」，也不用为每个测试各自写一份 run_id 相关的 mock。
    const closedOutRunIds = new Set<string>();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      if (cmd === "list_run_commits") {
        return Promise.resolve(
          Array.from(closedOutRunIds, (run_id) => ({
            run_id,
            state: "active",
            undo_total: 1,
            undo_undone: 0,
          })),
        );
      }
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    await waitFor(() => expect(sessionReviewCallCount()).toBeGreaterThan(0));

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const rawHandler = agentEventCb();
    const handler = (e: { payload: any }) => {
      const payload = e?.payload;
      if (
        (payload?.kind === "run_closeout" || payload?.kind === "completed") &&
        payload.run_id
      ) {
        closedOutRunIds.add(payload.run_id);
      }
      rawHandler(e);
    };

    return {
      container,
      handler,
      reviewCallCount: sessionReviewCallCount(),
      sendCalls,
    };
  }

  async function startRunCloseoutReviewRace() {
    let resolveStaleReview!: (review: typeof reviewWithChanges) => void;
    let rejectStaleReview!: (error: unknown) => void;
    const staleReview = new Promise<typeof reviewWithChanges>(
      (resolve, reject) => {
        resolveStaleReview = resolve;
        rejectStaleReview = reject;
      },
    );
    let s1ReviewCalls = 0;
    const s2Review = {
      has_changes: true,
      stat: " s2.txt | 1 +",
      patch: "diff --git a/s2.txt b/s2.txt\n@@ -0,0 +1 @@\n+S2_CURRENT\n",
      files_changed: 1,
    };

    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      if (cmd === "session_review") {
        if (args?.sessionId === "s1") {
          s1ReviewCalls += 1;
          return s1ReviewCalls === 1
            ? Promise.resolve(emptyReview)
            : staleReview;
        }
        if (args?.sessionId === "s2") return Promise.resolve(s2Review);
      }
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() => expect(s1ReviewCalls).toBe(1));

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-stale-review",
          commit_sha: "stale-review-sha",
          files_changed: 1,
          insertions: 1,
          deletions: 0,
          interrupted: false,
        },
      });
    });
    await waitFor(() => expect(s1ReviewCalls).toBe(2));

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("session_review", {
        sessionId: "s2",
      }),
    );
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+S2_CURRENT");

    return { container, resolveStaleReview, rejectStaleReview };
  }

  async function startSameSessionReviewRace() {
    let resolveOlderReview!: (review: typeof reviewWithChanges) => void;
    let rejectOlderReview!: (error: unknown) => void;
    const olderReview = new Promise<typeof reviewWithChanges>(
      (resolve, reject) => {
        resolveOlderReview = resolve;
        rejectOlderReview = reject;
      },
    );
    let resolveNewerReview!: (review: typeof reviewWithChanges) => void;
    const newerReview = new Promise<typeof reviewWithChanges>((resolve) => {
      resolveNewerReview = resolve;
    });
    let reviewCalls = 0;

    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "session_review" && args?.sessionId === "s1") {
        reviewCalls += 1;
        if (reviewCalls === 1) return Promise.resolve(emptyReview);
        if (reviewCalls === 2) return olderReview;
        if (reviewCalls === 3) return newerReview;
        return Promise.resolve({
          has_changes: true,
          stat: " newer.txt | 1 +",
          patch:
            "diff --git a/newer.txt b/newer.txt\n@@ -0,0 +1 @@\n+NEWER_REVIEW\n",
          files_changed: 1,
        });
      }
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() => expect(reviewCalls).toBe(1));

    const handler = agentEventCb();
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-older-review",
          commit_sha: "older-review-sha",
          files_changed: 1,
          insertions: 1,
          deletions: 0,
          interrupted: false,
        },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-newer-review",
          commit_sha: "newer-review-sha",
          files_changed: 1,
          insertions: 1,
          deletions: 0,
          interrupted: false,
        },
      });
    });
    await waitFor(() => expect(reviewCalls).toBe(3));

    await act(async () => {
      resolveNewerReview({
        has_changes: true,
        stat: " newer.txt | 1 +",
        patch:
          "diff --git a/newer.txt b/newer.txt\n@@ -0,0 +1 @@\n+NEWER_REVIEW\n",
        files_changed: 1,
      });
      await Promise.resolve();
    });
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+NEWER_REVIEW");

    return {
      container,
      resolveOlderReview,
      rejectOlderReview,
    };
  }

  async function startStaleOpenReviewRace(rejectStaleReview: boolean) {
    const s1Messages = deferred<ChatMessage[]>();
    const s2Review = deferred<typeof reviewWithChanges>();
    let s1ReviewCalls = 0;
    let s2ReviewCalls = 0;

    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      if (cmd === "get_messages") {
        return args?.sessionId === "s1"
          ? s1Messages.promise
          : Promise.resolve([]);
      }
      if (cmd === "session_review" && args?.sessionId === "s1") {
        s1ReviewCalls += 1;
        return rejectStaleReview
          ? Promise.reject(new Error("STALE_OPEN_REVIEW_FAILED"))
          : Promise.resolve(reviewWithChanges);
      }
      if (cmd === "session_review" && args?.sessionId === "s2") {
        s2ReviewCalls += 1;
        return s2Review.promise;
      }
      if (cmd === "get_session_goal") return Promise.resolve(null);
      if (cmd === "list_interrupted_team_runs") return Promise.resolve([]);
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      });
      expect(s2ReviewCalls).toBe(1);
    });

    await act(async () => {
      s1Messages.resolve([]);
      await s1Messages.promise;
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_lead_loop_state", {
        sessionId: "s1",
      }),
    );
    expect(s1ReviewCalls).toBe(0);

    await act(async () => {
      s2Review.resolve({
        has_changes: true,
        stat: " s2-current.txt | 1 +",
        patch:
          "diff --git a/s2-current.txt b/s2-current.txt\n@@ -0,0 +1 @@\n+S2_AFTER_STALE_OPEN\n",
        files_changed: 1,
      });
      await s2Review.promise;
    });
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+S2_AFTER_STALE_OPEN");

    return { container, s1ReviewCalls };
  }

  function leadDecisionCardCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-decision-card",
    )?.[1];
    if (!handler) throw new Error("lead-decision-card listener 未注册");
    return handler as (e: {
      payload: {
        session_id: string;
        block: Extract<Block, { type: "decision_card" }>;
      };
    }) => void;
  }

  // 决策打扰收敛刀 T1·症状 B：镜像 leadDecisionCardCb，取 lead-message-appended listener
  // 的回调直接手动触发（App 收到后端 append_decision_echo 写库成功的 emit）。
  function leadMessageAppendedCb() {
    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-message-appended",
    )?.[1];
    if (!handler) throw new Error("lead-message-appended listener 未注册");
    return handler as (e: {
      payload: {
        session_id: string;
        message: ChatMessage & { id: number };
      };
    }) => void;
  }

  const dEnv = (
    dispatch: Record<string, unknown>,
    ev: Record<string, unknown>,
    sessionId = "s1",
  ) => ({
    payload: { session_id: sessionId, dispatch, ...ev },
  });

  function decisionCardBlock(
    overrides: Partial<Extract<Block, { type: "decision_card" }>> = {},
  ): Extract<Block, { type: "decision_card" }> {
    const options = overrides.options ?? ["继续"];
    return {
      type: "decision_card",
      decision_id: "dc-preloaded",
      kind: "ask",
      question: "请选择下一步",
      options,
      recommended: options[0] ?? null,
      rationale: null,
      payload: null,
      source_run_id: "run-preloaded",
      status: "pending",
      chosen_option: null,
      created_at: 1,
      ...overrides,
    };
  }

  function decisionCardMessage(
    options: string[],
    overrides: Partial<Extract<Block, { type: "decision_card" }>> = {},
  ): ChatMessage {
    return {
      role: "assistant",
      engine: "claude",
      content: [decisionCardBlock({ options, ...overrides })],
    };
  }

  function inlineDecisionCard() {
    const card = document.querySelector<HTMLElement>(".decision-card");
    if (!card) throw new Error("inline decision card 未渲染");
    return within(card);
  }

  function findInlineDecisionButton(name: RegExp) {
    return waitFor(() => inlineDecisionCard().getByRole("button", { name }));
  }

  async function clickDecisionOption(option: string) {
    await waitFor(() =>
      expect(document.querySelector(".decision-card")).not.toBeNull(),
    );
    fireEvent.click(
      inlineDecisionCard().getByRole("button", {
        name: new RegExp(escapeRegExp(option)),
      }),
    );
  }

  function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason?: unknown) => void;
    const promise = new Promise<T>((promiseResolve, promiseReject) => {
      resolve = promiseResolve;
      reject = promiseReject;
    });

    return { promise, resolve, reject };
  }

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
    expect(guide).toHaveTextContent("当前两个都没有检测到");

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

  describe("runIdForActiveCodingSession（改动条交付动作落 runId）", () => {
    const cs = (
      runId: string,
      sessionId: string,
      phase: CodingState["phase"],
    ): CodingState => ({
      runId,
      sessionId,
      assignmentId: `a-${runId}`,
      baseSha: "base",
      phase,
      artifactId: null,
      verifyCmd: "",
      isInPlace: false,
    });

    it("无匹配 session → null", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "other", "finalizing")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBeNull();
    });

    it("空 loops → null", () => {
      expect(runIdForActiveCodingSession(new Map(), "s1")).toBeNull();
    });

    it("单匹配 run → 取该 runId", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "s1", "finalizing")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBe("r1");
    });

    it("多匹配 run：优先 phase applying/applied 的落地 run", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "s1", "finalizing")],
        ["r2", cs("r2", "s1", "applying")],
        ["r3", cs("r3", "s1", "verifying")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBe("r2");
    });

    it("多匹配 run·都非落地态 → 取最后一个", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "s1", "finalizing")],
        ["r2", cs("r2", "s1", "verifying")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBe("r2");
    });
  });

  describe("suppressBlockBShells（块B·GUI 验收折轻）", () => {
    const tr = (run_id: string, statuses: string[]): ChatMessage => ({
      role: "assistant",
      content: [
        {
          type: "team_run",
          run_id,
          goal: null,
          lead: "Claude",
          members: statuses.map((s, i) => ({
            participant_id: `w${i}`,
            assignment_id: `a${i}`,
            task_id: `t${i}`,
            name: `worker-${i}`,
            status: s,
            sub: "活",
            steps_total: 1,
            steps_done: 1,
            cost_usd: null,
            input_tokens: 0,
            output_tokens: 0,
            failed: s === "failed",
            blocks: [],
          })),
        } as any,
      ],
    });
    const coding = (run_id: string, phase: string): ChatMessage => ({
      role: "assistant",
      content: [
        {
          type: "coding_task",
          run_id,
          assignment_id: "a0",
          worker_name: "X",
          phase,
        } as any,
      ],
    });
    const verdict = (run_id: string): ChatMessage => ({
      role: "assistant",
      content: [
        {
          type: "lead_summary",
          run_id,
          summary_source: "single_passthrough",
          status: { kind: "all_succeeded", succeeded_count: 1, total: 1 },
          sections: [],
          findings: [],
          artifact_refs: [],
        } as any,
      ],
    });

    it("非 coding run 的 terminal team_run + verdict 都保留（用户定：任务条+verdict 都留）", () => {
      const out = suppressBlockBShells(
        [tr("r1", ["done", "failed"]), verdict("r1")],
        new Set(),
      );
      expect(out).toHaveLength(2);
    });
    it("空 members 的 team_run 消（空 turn）", () => {
      const out = suppressBlockBShells([tr("r2", [])], new Set());
      expect(out).toHaveLength(0);
    });
    it("coding run（持久 coding_task）→ 非空 team_run 保留 metadata·coding_task 行留", () => {
      const out = suppressBlockBShells(
        [tr("r3", ["done"]), coding("r3", "applied"), verdict("r3")],
        new Set(),
      );
      expect(out.some((m) => (m.content as any[])[0].type === "team_run")).toBe(
        true,
      );
      expect(
        out.some((m) => (m.content as any[])[0].type === "coding_task"),
      ).toBe(true);
      expect(
        out.some((m) => (m.content as any[])[0].type === "lead_summary"),
      ).toBe(true);
    });
    it("coding run（仅 live·尚无持久 coding_task）→ 非空 team_run 仍保留 metadata", () => {
      const out = suppressBlockBShells([tr("r4", ["done"])], new Set(["r4"]));
      expect(out).toHaveLength(1);
    });
  });

  it("清空 localStorage 后仍从后端账本批量回填 RunCard 部分撤销态", async () => {
    localStorage.clear();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          {
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          },
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            engine: "claude",
            content: [
              runCard("run-partial", 3),
              runCard("run-full", 2),
              runCard("run-normal", 1),
            ],
          },
        ]);
      if (cmd === "list_run_commits")
        return Promise.resolve([
          {
            run_id: "run-partial",
            state: "active",
            undo_total: 3,
            undo_undone: 2,
          },
          {
            run_id: "run-full",
            state: "active",
            undo_total: 2,
            undo_undone: 2,
          },
          {
            run_id: "run-normal",
            state: "active",
            undo_total: 1,
            undo_undone: 0,
          },
        ]);
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: false,
          stat: "",
          patch: "",
          files_changed: 0,
        });
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

    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("list_run_commits", {
        sessionId: "s1",
      }),
    );
    expect(await screen.findByText("已撤销 2 / 3")).toBeInTheDocument();
    expect(screen.getByText("已撤销本轮")).toBeInTheDocument();
    expect(screen.getByText("已完成")).toBeInTheDocument();
    expect(
      invokeMock.mock.calls.filter(
        ([command]) => command === "list_run_commits",
      ),
    ).toHaveLength(1);
  });

  it("点 RunCard 撤销 → Review tab 按 session + run 拉该轮清单", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          engine: "claude",
          content: [runCard("run-undo", 1)],
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    let undoComplete = false;
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      // undo_total 从一开始就要 > 0（真有可撤销记录），撤销按钮才会渲染——
      // commit 2 收紧「没有撤销记录时不显示撤销入口」后，undo_total 恒 0 会让按钮压根点不到。
      if (cmd === "list_run_commits") {
        return Promise.resolve([
          {
            run_id: "run-undo",
            state: "active",
            undo_total: 1,
            undo_undone: undoComplete ? 1 : 0,
          },
        ]);
      }
      if (cmd === "list_run_undo_entries") {
        return Promise.resolve([
          {
            file_path: "src/only-this-run.ts",
            change_kind: "modified",
            preimage_preview: { kind: "text", content: "old\n" },
            current_preview: { kind: "text", content: "new\n" },
            is_binary: false,
            size_bytes: 4,
            current_digest: "1".repeat(64),
            already_undone: false,
          },
        ]);
      }
      if (cmd === "undo_run_edits") {
        undoComplete = true;
        return Promise.resolve({
          restored: ["src/only-this-run.ts"],
          skipped: [],
          failed: [],
        });
      }
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "撤销…" }));

    expect(
      await screen.findByText("这一轮的改动 · 1 个文件"),
    ).toBeInTheDocument();
    expect(screen.getByText("src/only-this-run.ts")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("list_run_undo_entries", {
      sessionId: "s1",
      runId: "run-undo",
    });
    expect(screen.getByRole("tab", { name: "Review" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    fireEvent.click(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    );
    expect(await screen.findByText("已撤销本轮")).toBeInTheDocument();
  });

  it("team run 完成态点撤销这一轮 → Review tab 按 team run_id 打开", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          engine: "claude",
          content: [
            {
              type: "team_run",
              run_id: "team-run-undo",
              goal: null,
              lead: "Claude Code",
              members: [appMember({ status: "done" })],
            },
          ],
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_run_undo_entries") {
        return Promise.resolve([
          {
            file_path: "src/team-change.ts",
            change_kind: "modified",
            preimage_preview: { kind: "text", content: "old\n" },
            current_preview: { kind: "text", content: "new\n" },
            is_binary: false,
            size_bytes: 4,
            current_digest: "2".repeat(64),
            already_undone: false,
          },
        ]);
      }
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "撤销这一轮" }));

    expect(
      await screen.findByText("这一轮的改动 · 1 个文件"),
    ).toBeInTheDocument();
    expect(screen.getByText("src/team-change.ts")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("list_run_undo_entries", {
      sessionId: "s1",
      runId: "team-run-undo",
    });
    expect(screen.getByRole("tab", { name: "Review" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("撤销后账本刷新失败时保留最后可信累计状态，不冒充刷新成功", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          engine: "claude",
          content: [runCard("run-refresh-fails", 2)],
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    let undoComplete = false;
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_run_commits") {
        return undoComplete
          ? Promise.reject(new Error("ledger refresh unavailable"))
          : Promise.resolve([
              {
                run_id: "run-refresh-fails",
                state: "active",
                undo_total: 2,
                undo_undone: 1,
              },
            ]);
      }
      if (cmd === "list_run_undo_entries") {
        return Promise.resolve([
          {
            file_path: "src/remaining.ts",
            change_kind: "modified",
            preimage_preview: { kind: "text", content: "old\n" },
            current_preview: { kind: "text", content: "new\n" },
            is_binary: false,
            size_bytes: 4,
            current_digest: "1".repeat(64),
            already_undone: false,
          },
        ]);
      }
      if (cmd === "undo_run_edits") {
        undoComplete = true;
        return Promise.resolve({
          restored: ["src/remaining.ts"],
          skipped: [],
          failed: [],
        });
      }
      return fallback?.(cmd, args);
    });

    render(<App />);
    expect(await screen.findByText("已撤销 1 / 2")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续撤销…" }));
    await screen.findByText("src/remaining.ts");
    fireEvent.click(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    );

    expect(await screen.findByText("已还原 1 个文件")).toBeInTheDocument();
    expect(screen.getByText("已撤销 1 / 2")).toBeInTheDocument();
    expect(screen.queryByText("已撤销本轮")).not.toBeInTheDocument();
  });

  it("阶段1 · 左栏 .sidebar 宽 230 + .surface 左角圆", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sidebar\s*\{[^}]*flex:\s*0 0 230px/);
    expect(css).toMatch(/\.surface\s*\{[^}]*border-radius:\s*15px 0 0 15px/);
    expect(css).toMatch(
      /\.composer__input::placeholder\s*\{[^}]*color:\s*var\(--ink-4\)/,
    );
  });

  it("阶段1 收尾 · 旧三栏接缝 CSS 已删（.app--rpexpand / .body 容器 / .app__main / .topbar 容器）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).not.toMatch(/\.app--rpexpand/);
    expect(css).not.toMatch(/\.app__main\s*\{/);
    expect(css).not.toMatch(/^\.body\s*\{/m);
    expect(css).not.toMatch(/^\.topbar\s*\{/m);
  });

  it("阶段1 收尾 · tabs 行横滚、sf-tabs 允许右侧语言菜单溢出", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sf-tabs\s*\{[^}]*overflow:\s*visible/);
    expect(css).toMatch(/\.rptabs__tabrow\s*\{[^}]*overflow-x:\s*auto/);
  });

  it("阶段1 收尾 · .sf-tabs.expanded 最大化吃满 header（§2.D 右最大盖 main）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sf-tabs\.expanded\s*\{[^}]*flex:\s*1/);
  });

  it("设置 sheet 放大到业界尺寸 + sheet-scope 控件放大 + 全局 .st-* 基线未动", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    // 外框放大
    expect(css).toMatch(/\.settings-sheet\s*\{[^}]*max-width:\s*1080px/);
    expect(css).toMatch(/\.settings-sheet\s*\{[^}]*max-height:\s*760px/);
    expect(css).not.toMatch(/\.settings-sheet\s*\{[^}]*max-width:\s*880px/);
    // sheet-scope 控件放大
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-nav\s*\{[^}]*flex:\s*0 0 210px/,
    );
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-nav-item\s*\{[^}]*font-size:\s*13px/,
    );
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-content\s*\{[^}]*padding:\s*24px 30px/,
    );
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-form\s*\{[^}]*max-width:\s*720px/,
    );
    // 全局基线未动（防 worker 误改全局而非 sheet-scope）
    expect(css).toMatch(/^\.st-nav\s*\{[^}]*flex:\s*0 0 200px/m);
    expect(css).toMatch(/^\.st-nav-item\s*\{[^}]*font-size:\s*12\.5px/m);
  });

  it("composer/content 阅读宽度布局回归（与 shell 翻转无关·勿随骨架迁移误删）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/--content-max:\s*760px/);
    expect(css).toMatch(/--content-padding:\s*24px/);
    expect(css).toMatch(/\.turn\s*\{[^}]*max-width:\s*var\(--content-max\)/);
    expect(css).toMatch(
      /\.composer\s*\{[^}]*max-width:\s*calc\(var\(--content-max\) \+ var\(--content-padding\) \* 2\)/,
    );
    expect(css).toMatch(
      /\.composer__box:focus-within\s*\{[^}]*border-color:\s*var\(--accent\)/,
    );
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

  it("session 并发 Task 3 · 流式进行中时左栏新建按钮仍可用", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      // 永不 resolve：模拟流式进行中（不触发 completed，busy 维持 true）
      if (cmd === "send_message") return new Promise(() => {});
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    expect(screen.getByRole("button", { name: /新会话/ })).toBeInTheDocument();

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });

    // 发送后（当前 session busy）：仍允许新建其他 session
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /新会话/ })).not.toBeDisabled(),
    );
  });

  it("session 并发 Task 3 · 当前 busy 禁发但切 idle session 可发", async () => {
    const sendCalls: any[] = [];
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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "send_message") {
        sendCalls.push(args);
        return new Promise(() => {});
      }
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "first" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "发送" })).toBeNull();

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(sendCalls).toHaveLength(2));
    expect(sendCalls[1]).toMatchObject({ sessionId: "s2", message: "second" });
  });

  it("session 并发 Task 3 · cache miss loading 时 composer 禁发且加载完解禁", async () => {
    let resolveS2!: (msgs: any[]) => void;
    const s2Messages = new Promise<any[]>((resolve) => {
      resolveS2 = resolve;
    });
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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return args.sessionId === "s2" ? s2Messages : Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();

    await act(async () => {
      resolveS2([]);
      await s2Messages;
    });

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "loaded" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
  });

  it("session 并发 Task 2 · 当前 running 空 assistant 的 hint 只显示 0s", async () => {
    vi.spyOn(Date, "now").mockReturnValue(100_000);
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "send_message") return new Promise(() => {});
      return Promise.resolve();
    });
    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => {
      const statusLine = container.querySelector(".composer__hint-cost");
      // 运行态不显示秒数，只显示 token 成本（若有）
      expect(statusLine?.textContent).toBe(""); // workingTokens 为 null
      expect(statusLine?.textContent).not.toContain("工作中");
      expect(statusLine?.textContent).not.toContain("↑");
    });
  });

  it("Phase cluster05 plan A Task 2 · text_delta 节流：多个 delta 只触发一次 rAF flush", async () => {
    const rafCbs: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      rafCbs.push(cb);
      return rafCbs.length;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());

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
      if (cmd === "rename_session") return new Promise(() => {});
      if (cmd === "send_message") return new Promise(() => {});
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "test" } });
    fireEvent.keyDown(input, { key: "Enter" });

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    expect(handler).toBeTruthy();

    act(() => {
      handler({ payload: { session_id: "s1", kind: "text_delta", text: "a" } });
      handler({ payload: { session_id: "s1", kind: "text_delta", text: "b" } });
      handler({ payload: { session_id: "s1", kind: "text_delta", text: "c" } });
    });

    expect(rafCbs).toHaveLength(1);
    expect(screen.queryByText("abc")).not.toBeInTheDocument();

    act(() => {
      rafCbs[0](16);
    });

    expect(screen.getByText("abc")).toBeInTheDocument();
    vi.unstubAllGlobals();
  });

  it("EventTransport batch · 同 tick 文本整批应用且只安排一次渲染", async () => {
    const rafCbs: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      rafCbs.push(cb);
      return rafCbs.length;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const { sendCalls } = mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "run" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    const handler = agentEventBatchCb();
    act(() => {
      handler({
        payload: {
          batches: [
            {
              session_id: "s1",
              events: [
                { seq: 1, kind: "text_delta", text: "batch-" },
                { seq: 2, kind: "text_delta", text: "text" },
              ],
            },
          ],
        },
      });
    });

    expect(rafCbs).toHaveLength(1);
    expect(screen.queryByText("batch-text")).not.toBeInTheDocument();
    act(() => rafCbs[0](16));
    expect(screen.getByText("batch-text")).toBeInTheDocument();
  });

  it("EventTransport batch · 批尾终态立即应用并清理 working run", async () => {
    const rafCbs: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      rafCbs.push(cb);
      return rafCbs.length;
    });
    const cancelAnimationFrame = vi.fn();
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrame);
    const { sendCalls } = mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "run" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    const handler = agentEventBatchCb();
    act(() => {
      handler({
        payload: {
          batches: [
            {
              session_id: "s1",
              events: [{ seq: 1, kind: "text_delta", text: "pending-" }],
            },
          ],
        },
      });
      handler({
        payload: {
          batches: [
            {
              session_id: "s1",
              events: [
                { seq: 2, kind: "text_delta", text: "before-terminal" },
                { seq: 3, kind: "error", message: "batch failed" },
              ],
            },
          ],
        },
      });
    });

    expect(screen.getByText(/before-terminal/)).toBeInTheDocument();
    expect(screen.getByText(/batch failed/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
    expect(cancelAnimationFrame).toHaveBeenCalledTimes(1);
  });

  it("EventTransport batch · usage_delta 在批内保持累加语义", async () => {
    const { sendCalls } = mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");

    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "run" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    act(() => {
      agentEventBatchCb()({
        payload: {
          batches: [
            {
              session_id: "s1",
              events: [
                {
                  seq: 1,
                  kind: "usage_delta",
                  input_tokens: 3,
                  output_tokens: 2,
                },
                {
                  seq: 2,
                  kind: "usage_delta",
                  input_tokens: null,
                  output_tokens: 5,
                },
              ],
            },
          ],
        },
      });
    });

    const hintCost = document.querySelector(".composer__hint-cost");
    expect(hintCost).not.toBeNull();
    expect(
      within(hintCost as HTMLElement).getByText(/↑ 10 tok/),
    ).toBeInTheDocument();
  });

  it("EventTransport batch · 双会话同批共用一次外层 Map 克隆并各自保序", () => {
    const initial = new Map<string, string[]>([
      ["s1", []],
      ["s2", []],
    ]);
    const cloneMap = vi.fn((source: Map<string, string[]>) => new Map(source));
    let current = initial;

    const result = applyEventTransportBatch(
      {
        batches: [
          {
            session_id: "s1",
            events: [
              { seq: 1, kind: "text_delta", text: "A" },
              { seq: 2, kind: "text_delta", text: "B" },
            ],
          },
          {
            session_id: "s2",
            events: [
              { seq: 1, kind: "text_delta", text: "C" },
              { seq: 2, kind: "text_delta", text: "D" },
            ],
          },
        ],
      },
      () => current,
      (event, mutate) => {
        mutate(event.session_id, (parts) => [...parts, String(event.text)]);
      },
      () => false,
      (next) => {
        current = next;
      },
      cloneMap,
    );

    expect(cloneMap).toHaveBeenCalledTimes(1);
    expect(result.messagesChanged).toBe(true);
    expect(current.get("s1")).toEqual(["A", "B"]);
    expect(current.get("s2")).toEqual(["C", "D"]);
  });

  it.each([
    [
      "completed",
      {
        kind: "completed",
        cost_usd: null,
        input_tokens: null,
        output_tokens: null,
        final_text: null,
      },
    ],
    [
      "run_closeout",
      {
        kind: "run_closeout",
        run_id: "run-terminal",
        commit_sha: null,
        files_changed: null,
        insertions: null,
        deletions: null,
        interrupted: false,
      },
    ],
    ["error", { kind: "error", message: "terminal error" }],
    [
      "needs_decision",
      {
        kind: "needs_decision",
        run_id: "run-terminal",
        reason: "scope_change",
        changes: [
          {
            proposal_id: "proposal-1",
            kind: "scope",
            detail_text: "expand scope",
            detail_summary: null,
          },
        ],
      },
    ],
    ["blocked", { kind: "blocked", message: "terminal blocked" }],
  ])(
    "EventTransport batch · 五类终态 %s 都同步清理 working run",
    async (_kind, terminal) => {
      const { sendCalls } = mockBasicApp();
      render(<App />);
      await screen.findByText("Claude Code");

      const input = screen.getByPlaceholderText(/输入消息/);
      fireEvent.change(input, { target: { value: "run" } });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));
      await waitFor(() => expect(sendCalls).toHaveLength(1));

      const sessionReviewCallsBefore = invokeMock.mock.calls.filter(
        ([cmd]) => cmd === "session_review",
      ).length;

      act(() => {
        agentEventBatchCb()({
          payload: {
            batches: [
              {
                session_id: "s1",
                events: [{ seq: 1, ...terminal }],
              },
            ],
          },
        });
      });

      expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();

      // 首条消息发送会触发后台 rename_session → refreshSessions 的
      // fire-and-forget 链路（onSend 不 await 它，产品上是有意的非阻塞行为）；
      // "completed" 终态还会额外触发 refreshReview（session_review）。测试须
      // 等它们落定，否则 unmount 后才 resolve 的 setState 会打出 act() 警告
      // （偶发升级成 AggregateError 的根因之一）。
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("rename_session", {
          id: "s1",
          title: expect.any(String),
        }),
      );
      await waitFor(() =>
        expect(
          invokeMock.mock.calls.filter(([cmd]) => cmd === "list_sessions")
            .length,
        ).toBeGreaterThanOrEqual(2),
      );
      if (terminal.kind === "completed") {
        await waitFor(() =>
          expect(
            invokeMock.mock.calls.filter(([cmd]) => cmd === "session_review")
              .length,
          ).toBeGreaterThan(sessionReviewCallsBefore),
        );
      }
    },
  );

  it("session 并发 Task 1 · 两个 session 同帧 text_delta 各写各 cache 不串", async () => {
    const rafCbs: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      rafCbs.push(cb);
      return rafCbs.length;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());

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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve(
          args.sessionId === "s1"
            ? [
                {
                  role: "assistant",
                  engine: "claude",
                  content: [{ type: "text", text: "" }],
                },
              ]
            : [
                {
                  role: "assistant",
                  engine: "claude",
                  content: [{ type: "text", text: "" }],
                },
              ],
        );
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    fireEvent.click(screen.getByText("会话一"));

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    expect(handler).toBeTruthy();

    act(() => {
      handler({
        payload: { session_id: "s1", kind: "text_delta", text: "A" },
      });
      handler({
        payload: { session_id: "s2", kind: "text_delta", text: "B" },
      });
    });

    expect(rafCbs).toHaveLength(1);
    act(() => {
      rafCbs[0](16);
    });

    expect(screen.getByText("A", { selector: "p" })).toBeInTheDocument();
    expect(screen.queryByText("B")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() => expect(screen.getByText("B")).toBeInTheDocument());
    expect(screen.queryByText("A", { selector: "p" })).not.toBeInTheDocument();
    vi.unstubAllGlobals();
  });

  it("session 并发 Task 1 · 后台 completed 按 ev.session_id 落库而不是 currentId", async () => {
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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            engine: "claude",
            content: [{ type: "text", text: "" }],
          },
        ]);
      if (cmd === "append_message") return Promise.resolve();
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 9,
          final_text: "done in background",
        },
      });
    });

    // 刀 R R3：completed 的过程持久化已后端归约器（display_reduce）完成，前端不再补写 append_message（消双写）。
    // 落库按 ev.session_id 而不是 currentId 的保障已转到后端；前端此处只需验证 in-memory cache
    // 按 ev.session_id（s1）而非 currentId（s2）更新——切回 s1 应能看到该文本。
    expect(screen.queryByText("done in background")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("会话一"));
    await waitFor(() =>
      expect(screen.getByText("done in background")).toBeInTheDocument(),
    );
    expect(invokeMock.mock.calls.some((c) => c[0] === "append_message")).toBe(
      false,
    );
  });

  it("deepseek 完成也触发 session_review（Part A：写能力拉平）", async () => {
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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            engine: "deepseek",
            content: [{ type: "text", text: "" }],
          },
        ]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 9,
          final_text: "done by deepseek",
        },
      });
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("session_review", {
        sessionId: "s1",
      }),
    );
  });

  it("RunCloseout Review 乱序：迟到的 s1 success 不覆盖已切换的 s2", async () => {
    const { container, resolveStaleReview } =
      await startRunCloseoutReviewRace();

    await act(async () => {
      resolveStaleReview({
        has_changes: true,
        stat: " s1.txt | 1 +",
        patch: "diff --git a/s1.txt b/s1.txt\n@@ -0,0 +1 @@\n+S1_STALE\n",
        files_changed: 1,
      });
      await Promise.resolve();
    });

    const reviewText = container.querySelector(".review__files")?.textContent;
    expect(reviewText).toContain("+S2_CURRENT");
    expect(reviewText).not.toContain("+S1_STALE");
  });

  it("RunCloseout Review 乱序：迟到的 s1 failure 不清空已切换的 s2", async () => {
    const { container, rejectStaleReview } = await startRunCloseoutReviewRace();

    await act(async () => {
      rejectStaleReview(new Error("STALE_S1_REVIEW_FAILED"));
      await Promise.resolve();
    });

    expect(container.querySelector(".review__files")?.textContent).toContain(
      "+S2_CURRENT",
    );
  });

  it("RunCloseout Review 同 session 乱序：较老 success 不覆盖较新结果", async () => {
    const { container, resolveOlderReview } =
      await startSameSessionReviewRace();

    await act(async () => {
      resolveOlderReview({
        has_changes: true,
        stat: " older.txt | 1 +",
        patch:
          "diff --git a/older.txt b/older.txt\n@@ -0,0 +1 @@\n+OLDER_STALE\n",
        files_changed: 1,
      });
      await Promise.resolve();
    });

    const reviewText = container.querySelector(".review__files")?.textContent;
    expect(reviewText).toContain("+NEWER_REVIEW");
    expect(reviewText).not.toContain("+OLDER_STALE");
  });

  it("RunCloseout Review 同 session 乱序：较老 failure 不清空较新结果", async () => {
    const { container, rejectOlderReview } = await startSameSessionReviewRace();

    await act(async () => {
      rejectOlderReview(new Error("OLDER_REVIEW_FAILED"));
      await Promise.resolve();
    });

    expect(container.querySelector(".review__files")?.textContent).toContain(
      "+NEWER_REVIEW",
    );
  });

  it.each([
    { staleResult: "success", rejectStaleReview: false },
    { staleResult: "failure", rejectStaleReview: true },
  ])(
    "openSession 乱序：stale s1 Review $staleResult 不启动、不污染在途 s2 代次",
    async ({ rejectStaleReview }) => {
      const { container, s1ReviewCalls } =
        await startStaleOpenReviewRace(rejectStaleReview);

      expect(s1ReviewCalls).toBe(0);
      expect(container.querySelector(".review__files")?.textContent).toContain(
        "+S2_AFTER_STALE_OPEN",
      );
    },
  );

  it("session 并发 Task 4 · NF1 A completed 不取消 B pending flush", async () => {
    const rafCbs: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      rafCbs.push(cb);
      return rafCbs.length;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());

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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            engine: "claude",
            content: [{ type: "text", text: "" }],
          },
        ]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review") return new Promise(() => {});
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    fireEvent.click(screen.getByText("会话一"));

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: { session_id: "s1", kind: "text_delta", text: "A" },
      });
      handler({
        payload: { session_id: "s2", kind: "text_delta", text: "B" },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: "A done",
        },
      });
    });

    expect(rafCbs).toHaveLength(1);
    act(() => {
      rafCbs[0](16);
    });

    expect(screen.getByText("A", { selector: "p" })).toBeInTheDocument();
    fireEvent.click(screen.getByText("会话二"));
    expect(screen.getByText("B")).toBeInTheDocument();
    expect(
      invokeMock.mock.calls.filter(
        ([cmd, args]) => cmd === "get_messages" && args?.sessionId === "s2",
      ),
    ).toHaveLength(1);

    // 每次切会话都会触发 openSession 里一串不等待调用方的后台调用
    // （refreshRunStates/list_interrupted_team_runs/get_lead_loop_state/
    // useTeamConfig 等，产品上有意 fire-and-forget、不阻塞切会话主链）。
    // 测试期间三次切会话都没跟着 await 它们，须在收尾前把所有已 resolve 的
    // 微任务链彻底冲平，否则 unmount 后才落地的 setState 会打出 act() 警告
    // （偶发升级成 AggregateError 的根因之一）。冲平须在测试函数返回之前做
    // （而不是放在 afterEach 里）——vitest 在 testFn resolve 到 afterEach
    // 开始之间自有一段内部处理间隙，晚于测试体内落地的微任务链会在那段间隙
    // 里先于任何 afterEach 冲平代码抢跑，实测过 afterEach 兜底不了。
    // session_review 本用例故意用永不 resolve 的 Promise 模拟挂起中的旧
    // review 请求，不受此冲刷影响（不会、也不该被冲平）。
    await act(async () => {
      for (let i = 0; i < 5; i++) {
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
    });
  });

  it("session 并发 Task 4 · NF2 flush/completed 不原地修改历史 message 引用", async () => {
    const rafCbs: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      rafCbs.push(cb);
      return rafCbs.length;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const historyMessage = {
      role: "assistant",
      engine: "claude",
      content: [{ type: "text", text: "" }],
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
      if (cmd === "get_messages") return Promise.resolve([historyMessage]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review") return new Promise(() => {});
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: { session_id: "s1", kind: "text_delta", text: "clone" },
      });
    });
    act(() => {
      rafCbs[0](16);
    });

    expect(screen.getByText("clone")).toBeInTheDocument();
    expect(historyMessage.content[0].text).toBe("");

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

    expect(historyMessage.content[0].text).toBe("");
  });

  it("session 并发 Task 4 · NF3 listener 用 currentIdRef 识别切换后的当前 session", async () => {
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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: {
          session_id: "s2",
          kind: "tool_started",
          id: "tc-current",
          tool: "Bash",
          summary: "pwd",
          card: "command",
        },
      });
    });

    await waitFor(() => expect(screen.getByText("pwd")).toBeInTheDocument());
  });

  it("session 并发 Task 4 · NF4 loading 中拒发且加载完可发", async () => {
    let resolveS2!: (msgs: any[]) => void;
    const s2Messages = new Promise<any[]>((resolve) => {
      resolveS2 = resolve;
    });
    const sendCalls: any[] = [];
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
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return args.sessionId === "s2" ? s2Messages : Promise.resolve([]);
      if (cmd === "send_message") {
        sendCalls.push(args);
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
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "blocked" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(sendCalls).toHaveLength(0);

    await act(async () => {
      resolveS2([]);
      await s2Messages;
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "allowed" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(sendCalls).toHaveLength(1));
    expect(sendCalls[0]).toMatchObject({ sessionId: "s2", message: "allowed" });
  });

  it("session 并发 Task 4 · grep 防退化：保留 per-session cache 且不回旧 streaming 模型", () => {
    const app = readFileSync("src/App.tsx", "utf-8");
    expect(app).toMatch(/messagesBySession/);
    expect(app).toMatch(/currentIdRef/);
    expect(app).toMatch(/streamBlocks|sweepRunning/);
    expect(app).toMatch(/mutateSession/);
    expect(app).toMatch(/loadingSessionsRef/);
    expect(app).not.toMatch(/toolCalls|setToolCalls/);
    expect(app).not.toMatch(/owner|loadToken|streamingState/);
  });

  it("工具卡内联进当前会话、切到别的会话不串显", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: null,
            namespace_id: "local",
          }),
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    expect(handler).toBeTruthy();
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "tool_started",
          id: "tc1",
          tool: "Bash",
          summary: "ls -la",
          card: "command",
        },
      });
    });
    await waitFor(() => expect(screen.getByText("ls -la")).toBeInTheDocument());

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    expect(screen.queryByText("ls -la")).not.toBeInTheDocument();
  });

  it("执行态集成：text/tool/completed 完整 content 数组正确渲染（刀 R R3：持久化已后端化·前端不再补写）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false, stat: "", patch: "" });
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    // get_messages 被调用不等于 openSession 已完成：它还会等待 run ledger、goal、
    // interrupted runs 等异步来源；loading 清掉前点击发送会被禁用态按钮吞掉。
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: { session_id: "s1", kind: "text_delta", text: "开始" },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "tool_started",
          id: "t1",
          tool: "Bash",
          summary: "npm test",
          card: "command",
        },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "tool_completed",
          id: "t1",
          status: "ok",
          exit_code: 0,
          output: "pass",
        },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: "fallback",
        },
      });
    });

    // 刀 R R3：completed 的过程持久化已后端归约器（display_reduce）完成，前端不再补写 append_message（消双写）——
    // 这里只验证 in-memory content 数组仍完整组装（text + tool 卡都渲出）。
    await waitFor(() =>
      expect(screen.getByText("开始", { selector: "p" })).toBeInTheDocument(),
    );
    expect(screen.getByText("npm test")).toBeInTheDocument();
    expect(invokeMock.mock.calls.some((c) => c[0] === "append_message")).toBe(
      false,
    );
  });

  it.each([
    {
      terminalKind: "Blocked",
      terminal: { kind: "blocked", message: "CLOSEOUT_BLOCKED" },
      terminalText: "CLOSEOUT_BLOCKED",
      hasDecisionUi: false,
    },
    {
      terminalKind: "NeedsDecision",
      terminal: {
        kind: "needs_decision",
        run_id: "run-needs-decision",
        reason: "需要扩大范围",
        changes: [
          {
            proposal_id: "proposal-1",
            kind: "scope",
            detail_text: "必须扩大范围",
            detail_summary: null,
          },
        ],
      },
      terminalText: "必须扩大范围",
      hasDecisionUi: true,
    },
  ])(
    "$terminalKind -> RunCloseout：终态立即释放，closeout 后仍追加 RunCard",
    async ({ terminal, terminalText, hasDecisionUi }) => {
      const { container, handler, reviewCallCount, sendCalls } =
        await startRunCloseoutLiveUi();

      act(() => {
        handler({ payload: { session_id: "s1", ...terminal } });
      });

      expect(
        await screen.findByText(new RegExp(terminalText)),
      ).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
      expect(
        screen.queryByRole("group", { name: "本轮改动" }),
      ).not.toBeInTheDocument();
      expect(
        container.querySelector('[data-session-id="s1"] .sess__dot'),
      ).not.toHaveClass("run");
      expect(sendCalls).toHaveLength(1);

      act(() => {
        handler({
          payload: {
            session_id: "s1",
            kind: "run_closeout",
            run_id: `run-${terminal.kind}`,
            commit_sha: "closeout-sha",
            files_changed: 2,
            insertions: 4,
            deletions: 1,
            interrupted: false,
          },
        });
      });

      expect(
        await screen.findByRole("group", { name: "本轮改动" }),
      ).toBeInTheDocument();
      expect(
        await screen.findByRole("button", { name: "撤销…" }),
      ).toBeInTheDocument();
      expect(screen.getAllByRole("group", { name: "本轮改动" })).toHaveLength(
        1,
      );
      if (hasDecisionUi) {
        expect(
          screen.getByRole("button", { name: "采纳并继续" }),
        ).toBeInTheDocument();
      }
      await waitFor(() =>
        expect(sessionReviewCallCount()).toBe(reviewCallCount + 1),
      );
      expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
      fireEvent.click(screen.getByText("会话二"));
      await waitFor(() => {
        const dot = container.querySelector(
          '[data-session-id="s1"] .sess__dot',
        );
        expect(dot).toHaveClass("attention");
        expect(dot).not.toHaveClass("done");
      });
    },
  );

  it("执行态集成：error 终态立即清 running，保留错误文案与 attention", async () => {
    const { container, handler, sendCalls } = await startRunCloseoutLiveUi();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "error",
          message: "TERMINAL_ERROR_RELEASES_RUNNING",
        },
      });
    });

    expect(
      await screen.findByText(/TERMINAL_ERROR_RELEASES_RUNNING/),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "RETRY_AFTER_ERROR" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() => {
      const dot = container.querySelector('[data-session-id="s1"] .sess__dot');
      expect(dot).toHaveClass("attention");
      expect(dot).not.toHaveClass("run");
    });
    expect(sendCalls).toHaveLength(1);
  });

  it("RunCloseout files_changed=null：终态立即释放，closeout 不造卡", async () => {
    const { container, handler, reviewCallCount, sendCalls } =
      await startRunCloseoutLiveUi();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "blocked",
          message: "NO_CHECKPOINT_BLOCKED",
        },
      });
    });

    expect(
      await screen.findByText(/NO_CHECKPOINT_BLOCKED/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
    expect(sendCalls).toHaveLength(1);
    expect(container.querySelectorAll(".turn--user")).toHaveLength(1);

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-no-checkpoint",
          commit_sha: null,
          files_changed: null,
          insertions: null,
          deletions: null,
          interrupted: null,
        },
      });
    });

    expect(
      screen.queryByRole("group", { name: "本轮改动" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "撤销…" }),
    ).not.toBeInTheDocument();
    expect(sessionReviewCallCount()).toBe(reviewCallCount);

    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() => {
      expect(
        container.querySelector('[data-session-id="s1"] .sess__dot'),
      ).toHaveClass("attention");
      expect(
        container.querySelector('[data-session-id="s1"] .sess__dot'),
      ).not.toHaveClass("done");
    });
  });

  it("blocked 事件·已知 reason + 在场 pending MCP 决策卡 → 人话化文案 + 「还有问题在等你回答」提示（T1+T2）", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
      ],
      {
        messages: [
          decisionCardMessage(["继续", "先停下"], {
            decision_id: "mcp-dc-blocked-hint",
            kind: "ask",
            question: "要不要继续？",
            recommended: "继续",
            source_run_id: "mcp-lead-blocked-hint",
          }),
        ],
      },
    );

    render(<App />);
    await screen.findByText("Claude Code");
    const handler = agentEventCb();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "blocked",
          message: "no_progress",
        },
      });
    });

    // T1：裸 reason 码 "no_progress" 必须经 humanizeStopReason 人话化，不裸露给用户。
    expect(await screen.findByText(/连续多轮没有实质进展/)).toBeInTheDocument();
    expect(screen.queryByText(/^no_progress/)).not.toBeInTheDocument();
    // T2：在场一张 pending 的 mcp-lead-* 决策卡 → 文案末尾追加停摆点破提示。
    expect(screen.getByText(/还有问题在等你回答/)).toBeInTheDocument();
  });

  it("blocked 事件·无 pending MCP 决策卡 → 只有人话化文案，不带停摆提示（T2 对照组）", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
      ],
      {
        // 需要有一条既存 assistant 消息作为「当前流尾」，收工文案才有地方追加
        // （appendTextDelta 只会写进已存在的最后一条 assistant 消息·空消息列表时是 no-op·
        // 这里刻意不放任何 decision_card block，模拟「run 跑到一半被 blocked、没问过问题」）。
        messages: [
          { role: "user", content: [{ type: "text", text: "开始吧" }] },
          {
            role: "assistant",
            content: [],
            engine: "claude",
            agent_id: "claude",
          },
        ],
      },
    );

    render(<App />);
    await screen.findByText("Claude Code");
    const handler = agentEventCb();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "blocked",
          message: "no_progress",
        },
      });
    });

    expect(await screen.findByText(/连续多轮没有实质进展/)).toBeInTheDocument();
    expect(screen.queryByText(/还有问题在等你回答/)).not.toBeInTheDocument();
  });

  it("正常 Completed 仍只追加一张 RunCard，并保持 done 收尾", async () => {
    const { container, handler, reviewCallCount } =
      await startRunCloseoutLiveUi();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: 3,
          output_tokens: 7,
          final_text: "NORMAL_COMPLETED",
          run_id: "run-completed",
          commit_sha: "completed-sha",
          files_changed: 2,
          insertions: 4,
          deletions: 1,
          interrupted: false,
        },
      });
    });

    expect(await screen.findByText("NORMAL_COMPLETED")).toBeInTheDocument();
    expect(
      await screen.findAllByRole("group", { name: "本轮改动" }),
    ).toHaveLength(1);
    // 撤销按钮现在要等 closeout 后的 ledger 重新拉取（undo_total）落地才会出现——用
    // findAllByRole 而非同步 getAllByRole，给这次异步刷新一个机会。
    expect(
      await screen.findAllByRole("button", { name: "撤销…" }),
    ).toHaveLength(1);
    await waitFor(() => {
      expect(sessionReviewCallCount()).toBe(reviewCallCount + 1);
      expect(container.querySelector(".composer__hint-cost")).toHaveTextContent(
        "7 tok",
      );
    });

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() => {
      const dot = container.querySelector('[data-session-id="s1"] .sess__dot');
      expect(dot).toHaveClass("done");
      expect(dot).not.toHaveClass("attention");
    });
  });

  it("hidden 工具（ask_user）不建卡、不 warn（块②a-1 bug#3·决策卡为唯一呈现）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false, stat: "", patch: "" });
      return Promise.resolve();
    });
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: { session_id: "s1", kind: "text_delta", text: "问你" },
      });
      // ask_user 阻塞期 = running 态的裸工具卡（决策卡走独立路径渲·此卡多余且会卡死）
      handler({
        payload: {
          session_id: "s1",
          kind: "tool_started",
          id: "ask1",
          tool: "mcp__agentloom__ask_user",
          summary: "ask_user",
          card: "compact",
        },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "tool_completed",
          id: "ask1",
          status: "ok",
          exit_code: null,
          output: '{"answer":"A"}',
        },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: "",
        },
      });
    });

    // 刀 R R3：completed 的过程持久化已后端归约器（display_reduce）完成，前端不再补写 append_message（消双写）——
    // ask_user 工具块本就在 tool_started 时被 HIDDEN_TOOLS 拦下、不建裸卡，故 in-memory 渲染里也不应出现。
    await waitFor(() =>
      expect(screen.getByText("问你", { selector: "p" })).toBeInTheDocument(),
    );
    expect(screen.queryByText("ask_user")).not.toBeInTheDocument();
    expect(invokeMock.mock.calls.some((c) => c[0] === "append_message")).toBe(
      false,
    );
    // 不打 "无匹配 running 卡" warn（completion 静默跳过）
    expect(
      warnSpy.mock.calls.some((c) =>
        String(c[0]).includes("无匹配 running 卡"),
      ),
    ).toBe(false);
    warnSpy.mockRestore();
  });

  it("决策卡之后队长续写落新消息·正常显示（不被吞·块②a-1 narration）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false, stat: "", patch: "" });
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const agentHandler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    const cardHandler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-decision-card",
    )?.[1];
    act(() => {
      agentHandler({
        payload: { session_id: "s1", kind: "text_delta", text: "叙述1" },
      });
      // 决策卡作为独立 assistant 消息插入（整条被 consume·不走普通渲染）
      cardHandler({
        payload: {
          session_id: "s1",
          block: {
            type: "decision_card",
            decision_id: "d1",
            kind: "ask",
            question: "选 A 还是 B?",
            options: ["A", "B"],
            recommended: "A",
            rationale: null,
            payload: null,
            source_run_id: "mcp-lead-decision-r1",
            status: "pending",
            chosen_option: null,
            created_at: 1,
          },
        },
      });
      // 卡之后队长续写——修前会灌进卡那条消息被吞·修后落新消息可见
      agentHandler({
        payload: { session_id: "s1", kind: "text_delta", text: "叙述2续写" },
      });
    });

    await waitFor(() => {
      expect(screen.getByText("叙述1")).toBeInTheDocument();
      expect(screen.getByText("叙述2续写")).toBeInTheDocument();
    });
  });

  const askCardPayload = () => ({
    session_id: "s1",
    block: {
      type: "decision_card",
      decision_id: "d1",
      kind: "ask",
      question: "选 A 还是 B?",
      options: ["A", "B"],
      recommended: "A",
      rationale: null,
      payload: null,
      source_run_id: "mcp-lead-decision-r1",
      status: "pending",
      chosen_option: null,
      created_at: 1,
    },
  });

  function setupRunningS1() {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false, stat: "", patch: "" });
      return Promise.resolve();
    });
  }

  it("决策卡结尾·completed final_text 兜底续写不被吞（块②a-1·审查 Major）", async () => {
    setupRunningS1();
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    const agentHandler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    const cardHandler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-decision-card",
    )?.[1];
    act(() => {
      agentHandler({
        payload: { session_id: "s1", kind: "text_delta", text: "叙述1" },
      });
      cardHandler({ payload: askCardPayload() });
      // 答完无叙述直接收尾·final_text 兜底（streamed==="" 必命中·末条是卡）→ 修前吞·修后可见
      agentHandler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: "收尾结论X",
        },
      });
    });
    await waitFor(() =>
      expect(screen.getByText("收尾结论X")).toBeInTheDocument(),
    );
  });

  it("决策卡结尾·error 错误文案不被吞（队长阻塞在 ask 期间报错·审查同类）", async () => {
    setupRunningS1();
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    const agentHandler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    const cardHandler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-decision-card",
    )?.[1];
    act(() => {
      agentHandler({
        payload: { session_id: "s1", kind: "text_delta", text: "叙述1" },
      });
      cardHandler({ payload: askCardPayload() });
      agentHandler({
        payload: { session_id: "s1", kind: "error", message: "炸了X" },
      });
    });
    await waitFor(() => expect(screen.getByText(/炸了X/)).toBeInTheDocument());
  });

  it("MCP 卡等待和答完后都持续显示「工作中」", async () => {
    setupRunningS1();
    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    const agentHandler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    const cardHandler = listenMock.mock.calls.find(
      (c) => c[0] === "lead-decision-card",
    )?.[1];
    act(() => {
      agentHandler({
        payload: { session_id: "s1", kind: "text_delta", text: "叙述1" },
      });
      cardHandler({ payload: askCardPayload() });
    });
    // T15 的会话级运行状态不因决策卡临时隐藏。
    await waitFor(() => expect(screen.getByText("工作中")).toBeInTheDocument());
    // 点选项 B（A 带"推荐"pill·B 纯文本好定位）
    fireEvent.click(inlineDecisionCard().getByText("B"));
    // 答完（队长仍 busy）→ 立刻另起续写消息 → 显示「工作中」填空窗
    await waitFor(() => expect(screen.getByText("工作中")).toBeInTheDocument());
  });

  it("执行态集成：非当前 session completed 改对应 cache、不污染当前视图", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: null,
            namespace_id: "local",
          }),
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false, stat: "", patch: "" });
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "tool_started",
          id: "t1",
          tool: "Bash",
          summary: "npm test",
          card: "command",
        },
      });
    });
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "tool_completed",
          id: "t1",
          status: "failed",
          exit_code: 1,
          output: "boom",
        },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: null,
        },
      });
    });

    // 刀 R R3：completed 的过程持久化已后端归约器（display_reduce）完成，前端不再补写 append_message（消双写）——
    // 这里只验证非当前 session 的 in-memory cache 被正确更新（切回 s1 能看到），且不污染当前视图（s2 看不到）。
    expect(screen.queryByText("npm test")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("会话一"));
    await waitFor(() =>
      expect(screen.getByText("npm test")).toBeInTheDocument(),
    );
    expect(invokeMock.mock.calls.some((c) => c[0] === "append_message")).toBe(
      false,
    );
  });

  it.each([
    { stopResult: "resolved", rejectStop: false },
    { stopResult: "rejected", rejectStop: true },
  ])(
    "执行态集成：onStop $stopResult 调用结束后兜底清 running",
    async ({ rejectStop }) => {
      const sendCalls: unknown[] = [];
      invokeMock.mockImplementation((cmd: string, args?: any) => {
        if (cmd === "list_agents") return Promise.resolve(agentProfiles);
        if (cmd === "list_sessions")
          return Promise.resolve([
            makeSession({
              id: "s1",
              title: "会话一",
              repo_id: null,
              namespace_id: "local",
            }),
            makeSession({
              id: "s2",
              title: "会话二",
              repo_id: null,
              namespace_id: "local",
            }),
          ]);
        if (cmd === "get_messages") return Promise.resolve([]);
        if (cmd === "send_message") {
          sendCalls.push(args);
          return Promise.resolve();
        }
        if (cmd === "stop_session")
          return rejectStop
            ? Promise.reject(new Error("STOP_REQUEST_FAILED"))
            : Promise.resolve();
        return Promise.resolve();
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );
      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "go" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));
      await waitFor(() => expect(sendCalls).toHaveLength(1));

      const handler = agentEventCb();
      act(() => {
        handler({
          payload: {
            session_id: "s1",
            kind: "tool_started",
            id: "t1",
            tool: "Bash",
            summary: "sleep 10",
            card: "command",
          },
        });
      });
      await waitFor(() =>
        expect(screen.getByText("sleep 10")).toBeInTheDocument(),
      );

      fireEvent.click(screen.getByRole("button", { name: "停止" }));

      await waitFor(() =>
        expect(screen.getByText(/已中断|interrupted/i)).toBeInTheDocument(),
      );
      expect(invokeMock).toHaveBeenCalledWith("stop_session", {
        sessionId: "s1",
      });
      const input = screen.getByPlaceholderText(/输入消息/);
      fireEvent.change(input, { target: { value: "RETRY_AFTER_STOP" } });
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
      );

      fireEvent.keyDown(input, { key: "Enter" });
      await waitFor(() => expect(sendCalls).toHaveLength(2));
      expect(container.querySelectorAll(".turn--user")).toHaveLength(2);
    },
  );

  it("执行态集成：空标识 closeout 迟到于停止后的新 run 时不清 running", async () => {
    let nowMs = 100;
    vi.spyOn(Date, "now").mockImplementation(() => nowMs);
    const { handler, sendCalls } = await startRunCloseoutLiveUi();

    nowMs = 200;
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, { target: { value: "NEW_RUN_AFTER_STOP" } });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );

    nowMs = 300;
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(sendCalls).toHaveLength(2));
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "",
          commit_sha: null,
          files_changed: null,
          insertions: null,
          deletions: null,
          interrupted: true,
        },
      });
    });

    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "MUST_STAY_BLOCKED" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(sendCalls).toHaveLength(2);
  });

  it("执行态集成：空标识 closeout 且无新 run 时清掉停止前的 running", async () => {
    let nowMs = 100;
    vi.spyOn(Date, "now").mockImplementation(() => nowMs);
    const { handler } = await startRunCloseoutLiveUi();
    const fallback = invokeMock.getMockImplementation();
    let resolveStop!: () => void;
    const pendingStop = new Promise<void>((resolve) => {
      resolveStop = resolve;
    });
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "stop_session") return pendingStop;
      return fallback?.(cmd, args);
    });

    nowMs = 200;
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "",
          commit_sha: null,
          files_changed: null,
          insertions: null,
          deletions: null,
          interrupted: true,
        },
      });
    });

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "SEND_AFTER_EMPTY_CLOSEOUT" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    await act(async () => {
      resolveStop();
      await pendingStop;
    });
  });

  it("执行态集成：非空标识 closeout 保持现状，无条件清当前 running", async () => {
    let nowMs = 100;
    vi.spyOn(Date, "now").mockImplementation(() => nowMs);
    const { handler, sendCalls } = await startRunCloseoutLiveUi();

    nowMs = 200;
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    const input = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(input, {
      target: { value: "NEW_RUN_BEFORE_NORMAL_CLOSEOUT" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );

    nowMs = 300;
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(sendCalls).toHaveLength(2));
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "normal-run-id",
          commit_sha: null,
          files_changed: null,
          insertions: null,
          deletions: null,
          interrupted: false,
        },
      });
    });

    fireEvent.change(input, {
      target: { value: "SEND_AFTER_NORMAL_CLOSEOUT" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
  });

  it("plan B3：review 有改动时右面板不自动展开（纯手动）+ Review tab 角标数据就绪", async () => {
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
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: true,
          stat: "x",
          patch: "+hi",
          files_changed: 2,
        });
      return Promise.resolve();
    });
    render(<App />);
    // 等首屏稳定（review 已拉过一次 → 通知铃区的「展开右面板」出现）
    await screen.findByLabelText("展开右面板");
    // 右面板未自动开 → 无已删除的确认动作、无 Review tab
    expect(
      screen.queryByRole("button", { name: "留存" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("tab", { name: "Review" }),
    ).not.toBeInTheDocument();
    // 点「展开右面板」可手动打开（保留手动路径）
    fireEvent.click(screen.getByLabelText("展开右面板"));
    expect(await screen.findByLabelText("收起右面板")).toBeInTheDocument();
  });

  it("App 保留空 Review 对象并显示未纳入本次 Review 的变更数", async () => {
    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: false,
          other_dirty_count: 135,
          diff_available: true,
          files: [],
          patch: "",
          stat: "",
          files_changed: 0,
        });
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));

    expect(await screen.findByText("尚无改动")).toBeInTheDocument();
    expect(
      await screen.findByText("工作目录另有 135 个未纳入本次 Review 的变更"),
    ).toBeInTheDocument();
  });

  it("App 保留 diff unavailable 的空 Review 对象并显示降级文案", async () => {
    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: false,
          other_dirty_count: 0,
          diff_available: false,
          files: [],
          patch: "",
          stat: "",
          files_changed: 0,
        });
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));

    expect(await screen.findByText("无法生成改动对比")).toBeInTheDocument();
    expect(screen.queryByText("尚无改动")).not.toBeInTheDocument();
  });

  it("右面板 toggle/picker 在 intro/overview 常驻（去 view gate）", async () => {
    mockBasicApp();
    render(<App />);

    expect(await screen.findByLabelText("展开右面板")).toBeInTheDocument();

    fireEvent.click(screen.getByText("项目简介"));
    expect(await screen.findByLabelText("展开右面板")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("展开右面板"));
    expect(screen.getByLabelText("打开 Files")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("收起右面板"));
    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    expect(await screen.findByLabelText("展开右面板")).toBeInTheDocument();
  });

  it("点击聊天路径仍自动打开右面板并切到 Preview", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "请查看 `docs/guide/T4.md`" }],
        },
      ],
    });
    keepPreviewLoading();
    render(<App />);

    fireEvent.click(
      await screen.findByRole("button", { name: "docs/guide/T4.md" }),
    );

    expect(screen.getByLabelText("收起右面板")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Preview" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("切换右面板 tab 时传给 SessionMain 的 onOpenPreview 引用保持稳定", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "请查看 `docs/guide/T4.md`" }],
        },
      ],
    });
    render(<App />);

    await screen.findByRole("button", { name: "docs/guide/T4.md" });
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    const before = sessionMainProps[sessionMainProps.length - 1]?.onOpenPreview;
    expect(before).toBeTypeOf("function");

    fireEvent.click(screen.getByLabelText("打开 Files"));
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );

    const after = sessionMainProps[sessionMainProps.length - 1]?.onOpenPreview;
    expect(after).toBe(before);
  });

  it("从 Files 进入 Preview 后关闭：清空预览并回落 Files", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "请查看 `docs/guide/T4.md`" }],
        },
      ],
    });
    keepPreviewLoading();
    render(<App />);

    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(screen.getByLabelText("打开 Files"));
    fireEvent.click(
      await screen.findByRole("button", { name: "docs/guide/T4.md" }),
    );
    fireEvent.click(screen.getByLabelText("关闭预览"));

    expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      screen.queryByRole("tab", { name: "Preview" }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("新 tab / 回选择器"));
    expect(screen.queryByLabelText("打开 预览")).not.toBeInTheDocument();
  });

  it("非 session 右面板同样显最大化控件且可 max（expand 全视图一致）", async () => {
    mockBasicApp();
    const { container } = render(<App />);

    fireEvent.click(await screen.findByLabelText("展开右面板"));
    expect(screen.getByLabelText("展开（占用 main）")).toBeInTheDocument();

    fireEvent.click(screen.getByText("项目简介"));
    expect(await screen.findByLabelText("打开 Files")).toBeInTheDocument();
    expect(screen.getByLabelText("展开（占用 main）")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("展开（占用 main）"));
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();
    expect(screen.getByLabelText("恢复分栏")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("恢复分栏"));
    expect(container.querySelector(".session-pane.hidden")).toBeNull();
  });

  it("非 session 切换后 Review 不显上个会话 stale diff（review/badge guard）", async () => {
    const reviewWithTwoFiles = {
      has_changes: true,
      stat: " a.txt | 1 +\n b.txt | 1 +",
      patch:
        "diff --git a/a.txt b/a.txt\n@@ -0,0 +1 @@\n+hello\n" +
        "diff --git a/b.txt b/b.txt\n@@ -0,0 +1 @@\n+world\n",
      files_changed: 2,
    };
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "gh-repo",
            namespace_id: "gh-org-x",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(reviewWithTwoFiles);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace, githubNamespace],
          active_namespace_id: "gh-org-x",
          active_repo_id: "gh-repo",
          repos: [githubRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([githubRepo]);
      return Promise.resolve();
    });

    const { container } = render(<App />);

    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));

    expect(await screen.findByText(/改动 ·/)).toBeInTheDocument();
    expect(container.querySelector(".review__files")?.textContent).toContain(
      "+hello",
    );
    expect(container.querySelector(".rptab__badge")?.textContent).toBe("2");

    fireEvent.click(screen.getByText("项目简介"));
    expect(await screen.findByRole("tab", { name: "Review" })).toBeVisible();
    expect(screen.queryByText(/改动 ·/)).not.toBeInTheDocument();
    expect(container.querySelector(".review")).toBeNull();
    expect(container.querySelector(".rptab__badge")).toBeNull();
  });

  it("阶段1 · 右面板展开（占 main）：tools-pane.full + session-pane.hidden、sidebar 仍在", async () => {
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
              id: "r1",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    // 右面板默认收起：先点「展开右面板」打开
    fireEvent.click(screen.getByLabelText("展开右面板"));
    // 再点新「展开（占用 main）」按钮
    fireEvent.click(screen.getByLabelText("展开（占用 main）"));

    expect(container.querySelector(".tools-pane.full")).not.toBeNull();
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();
    expect(container.querySelector(".sidebar")).not.toBeNull();
    expect(screen.getByLabelText("恢复分栏")).toBeInTheDocument();
  });

  it("阶段1 · rightPanelMax 切走自动退出 max（snap-back reset）后不隐藏 body", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
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
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "local-default",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByLabelText("展开右面板"));
    fireEvent.click(screen.getByLabelText("展开（占用 main）"));
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();

    fireEvent.click(screen.getByText("项目简介"));
    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "ai-personal" }),
      ).toBeVisible(),
    );
    expect(container.querySelector(".session-pane.hidden")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "总览" })).toBeVisible(),
    );
    expect(container.querySelector(".session-pane.hidden")).toBeNull();

    fireEvent.click(screen.getByLabelText("设置"));
    await waitFor(() =>
      expect(container.querySelector(".settings-sheet")).not.toBeNull(),
    );
    expect(container.querySelector(".sf-body .st-app")).toBeNull();
    expect(container.querySelector(".session-pane.hidden")).toBeNull();
  });

  it("max 切走后切回不自动恢复（snap-back reset）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话A",
            repo_id: "local-default",
            namespace_id: "local",
          }),
          makeSession({
            id: "s2",
            title: "会话B",
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
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "local-default",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByLabelText("展开右面板"));
    fireEvent.click(screen.getByLabelText("展开（占用 main）"));
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();

    fireEvent.click(screen.getByText("会话B"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    await waitFor(() =>
      expect(container.querySelector(".session-pane.hidden")).toBeNull(),
    );

    fireEvent.click(screen.getByText("会话A"));
    await waitFor(() =>
      expect(container.querySelector(".session-pane.hidden")).toBeNull(),
    );
  });

  it("方案 B 三栏配色：sidebar var(--bg) / surface var(--panel)", async () => {
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
              id: "r1",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sidebar\s*\{[^}]*background:\s*var\(--bg\)/);
    expect(css).toMatch(/\.surface\s*\{[^}]*background:\s*var\(--panel\)/);
    expect(container.querySelector(".app-shell")).not.toBeNull();
    expect(container.querySelector(".sidebar")).not.toBeNull();
    expect(container.querySelector(".surface")).not.toBeNull();
    expect(container.querySelector(".sf-head")).not.toBeNull();
    expect(container.querySelector(".sf-body")).not.toBeNull();
    expect(container.querySelector(".topbar")).toBeNull();
  });

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

  // session-hover-menu Task 7：参数化 list_sessions 的 invoke mock（其余命令给最小可用返回）
  function mockAppWith(
    listSessions: ReturnType<typeof makeSession>[],
    overrides: Record<string, (args?: any) => Promise<any>> = {},
  ) {
    invokeMock.mockImplementation((cmd: string, _args?: any) => {
      if (overrides[cmd]) return overrides[cmd](_args);
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([...listSessions]);
      if (cmd === "list_groups") return Promise.resolve([]);
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
  }

  it("session-hover · 启动时首条 archived 不被自动打开（开首个活动会话）", async () => {
    mockAppWith([
      makeSession({ id: "z", title: "归档", archived: true, archived_at: 1 }),
      makeSession({ id: "a", title: "活动" }),
    ]);
    render(<App />);
    // 启动自动打开 = 首个活动会话 'a'（openSession → get_messages {sessionId:'a'}）
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "a",
      }),
    );
    // 绝不打开归档的 'z'
    expect(invokeMock).not.toHaveBeenCalledWith("get_messages", {
      sessionId: "z",
    });
  });

  it("session-hover · 打开 unread 会话自动清未读（set_session_unread false）", async () => {
    mockAppWith([makeSession({ id: "u1", title: "未读会话", unread: true })]);
    render(<App />);
    // 启动开 u1（唯一活动会话）→ openSession 检测 unread → 清未读
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_unread", {
        id: "u1",
        unread: false,
      }),
    );
  });

  describe("delete_session 集成", () => {
    it("点删除菜单 → 弹删除确认模态且标题含会话名", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      // 等启动完成（App 自动打开首个活动 s1）
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      // 找到第二个会话 row（非当前·避免 fallback 干扰）
      const row = container.querySelector('[data-session-id="s2"]')!;
      expect(row).toBeTruthy();

      // 右键开菜单 → 点 delete action
      fireEvent.contextMenu(row);
      const deleteBtn = row.querySelector('[data-action="delete"]')!;
      expect(deleteBtn).toBeTruthy();
      fireEvent.click(deleteBtn);

      // 核心断言 1：删除确认模态出现，标题含会话名
      const dialog = screen.getByRole("dialog");
      expect(dialog).toBeInTheDocument();
      expect(
        screen.getByRole("heading", { name: /删除会话「另一会话」？/ }),
      ).toBeInTheDocument();
    });

    it("删除父会话时提示仍有活跃接续会话但允许确认", async () => {
      const root = makeSession({
        id: "root",
        title: "父会话",
        continued_to_session_id: "child",
      });
      const child = makeSession({
        id: "child",
        title: "子会话",
        parent_session_id: "root",
        continued_to_session_id: "grandchild",
      });
      const grandchild = makeSession({
        id: "grandchild",
        title: "孙会话",
        parent_session_id: "child",
      });
      mockAppWith([root, child, grandchild]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "root",
        }),
      );

      const row = container.querySelector('[data-session-id="root"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      const dialog = screen.getByRole("dialog");
      expect(dialog).toHaveTextContent("还有 2 个活跃的接续会话");
      expect(dialog).toHaveTextContent("只会删除当前会话");

      fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("delete_session", {
          id: "root",
        }),
      );
    });

    it("删除确认 dialog 打开时 Esc 只关闭 dialog，保留设置 sheet", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      fireEvent.keyDown(window, { key: ",", metaKey: true });
      await waitFor(() =>
        expect(container.querySelector(".settings-sheet")).not.toBeNull(),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      const deleteBtn = row.querySelector('[data-action="delete"]')!;
      fireEvent.click(deleteBtn);
      expect(
        screen.getByRole("heading", { name: /删除会话「另一会话」？/ }),
      ).toBeInTheDocument();

      fireEvent.keyDown(document, { key: "Escape" });

      await waitFor(() =>
        expect(
          screen.queryByRole("heading", { name: /删除会话「另一会话」？/ }),
        ).not.toBeInTheDocument(),
      );
      expect(container.querySelector(".settings-sheet")).not.toBeNull();
    });

    it("删除确认 → 取消 → 不 invoke delete_session", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      // 点取消
      const dialog = screen.getByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));

      // 核心断言 2：dialog 消失 + 无 delete_session invoke
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(invokeMock.mock.calls.some((c) => c[0] === "delete_session")).toBe(
        false,
      );
    });

    it("删除确认 → 确认 → invoke delete_session({id})", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      // 点删除（确认）
      const dialog = screen.getByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

      // 核心断言 3：invoke delete_session 被调，参数含 id
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("delete_session", { id: "s2" }),
      );
    });

    it("删除失败时 toast 错误而不是未处理 Promise", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2], {
        delete_session: () => Promise.reject(new Error("DELETE_FAILED")),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      const dialog = screen.getByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

      await waitFor(() =>
        expect(container.querySelector(".toast")?.textContent).toContain(
          "DELETE_FAILED",
        ),
      );
    });
  });

  describe("continuation", () => {
    function mockContinuationApp(options: {
      generate?: (args?: any) => Promise<any>;
      start?: (args: any) => Promise<string>;
      sessions?: Session[];
      review?: any;
      githubContext?: boolean;
    }) {
      const sessionsState = options.sessions ?? [
        makeSession({
          id: "parent-1",
          title: "父会话",
          continued_to_session_id: null,
        }),
      ];
      invokeMock.mockImplementation((cmd: string, args?: any) => {
        if (cmd === "list_agents")
          return Promise.resolve([
            agentProfile({
              id: "claude",
              name: "Claude Code",
              provider: "claude",
              access: "native",
              cap_lead: "native_cli",
              has_key: false,
            }),
            agentProfile({
              id: "codex",
              name: "Codex",
              provider: "codex",
              sort_order: 1,
            }),
          ]);
        if (cmd === "list_sessions") return Promise.resolve([...sessionsState]);
        if (cmd === "list_groups") return Promise.resolve([]);
        if (cmd === "get_messages") return Promise.resolve([]);
        if (cmd === "list_run_commits") return Promise.resolve([]);
        if (cmd === "session_review")
          return Promise.resolve(options.review ?? emptyReview);
        if (cmd === "app_context")
          return options.githubContext
            ? Promise.resolve({
                namespaces: [localNamespace, githubNamespace],
                active_namespace_id: "gh-org-x",
                active_repo_id: "gh-repo",
                repos: [githubRepo],
              })
            : Promise.resolve({
                namespaces: [localNamespace],
                active_namespace_id: "local",
                active_repo_id: "local-default",
                repos: [localRepo],
              });
        if (cmd === "list_repos")
          return Promise.resolve(
            options.githubContext ? [githubRepo] : [localRepo],
          );
        if (cmd === "detect_runtime")
          return Promise.resolve({ claude: { available: true } });
        if (cmd === "get_session_agent_config")
          return Promise.resolve({
            session_id: args.sessionId,
            lead_agent_id: null,
            member_agent_ids: [],
          });
        if (cmd === "generate_handoff_doc")
          return (
            options.generate?.(args) ??
            Promise.resolve({
              doc_markdown: "# 测试交接文档\n\n内容",
              suggested_title: "测试标题",
              memory_projection: null,
              warnings: [],
            })
          );
        if (cmd === "start_continuation_session")
          return options.start?.(args) ?? Promise.resolve("child-1");
        return Promise.resolve();
      });
    }

    it("clicking 接续 shows handoff panel", async () => {
      mockContinuationApp({});
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(container.querySelector(".cc-brief")).not.toBeNull(),
      );
      expect(invokeMock).toHaveBeenCalledWith("generate_handoff_doc", {
        sessionId: "parent-1",
        requestId: expect.any(String),
      });
    });

    it("keeps a loading draft across session switches without regenerating", async () => {
      let resolveDraft!: (value: any) => void;
      const generate = vi.fn(
        () =>
          new Promise((resolve) => {
            resolveDraft = resolve;
          }),
      );
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二会话" }),
        ],
        generate,
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const parentRow = container.querySelector(
        '[data-session-id="parent-1"]',
      )!;
      fireEvent.contextMenu(parentRow);
      fireEvent.click(parentRow.querySelector('[data-action="handover"]')!);
      await waitFor(() => expect(generate).toHaveBeenCalledTimes(1));
      fireEvent.click(container.querySelector('[data-session-id="parent-2"]')!);
      await waitFor(() =>
        expect(container.querySelector(".cc-brief")).toBeNull(),
      );
      fireEvent.click(parentRow);

      await waitFor(() =>
        expect(
          container.querySelector(".cc-brief [role=status]"),
        ).not.toBeNull(),
      );
      expect(generate).toHaveBeenCalledTimes(1);

      await act(async () => {
        resolveDraft({
          doc_markdown: "# 已保留草稿",
          suggested_title: "接续标题",
          memory_projection: null,
          warnings: [],
        });
      });
      expect(await screen.findByText("已保留草稿")).toBeInTheDocument();
      expect(generate).toHaveBeenCalledTimes(1);
    });

    it("cancels handoff generation in the backend and ignores its late rejection", async () => {
      let rejectDraft!: (reason: unknown) => void;
      mockContinuationApp({
        generate: () =>
          new Promise((_resolve, reject) => {
            rejectDraft = reject;
          }),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);
      await waitFor(() =>
        expect(
          container.querySelector(".cc-brief [role=status]"),
        ).not.toBeNull(),
      );

      fireEvent.click(screen.getByRole("button", { name: "取消" }));

      const firstGenerateCall = invokeMock.mock.calls.find(
        ([command]) => command === "generate_handoff_doc",
      );
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("cancel_handoff_generation", {
          sessionId: "parent-1",
          requestId: firstGenerateCall?.[1]?.requestId,
        }),
      );
      expect(container.querySelector(".cc-brief")).toBeNull();

      await act(async () => {
        rejectDraft(new Error("AL_ERR:continuation.handoffCancelled"));
      });
      expect(container.querySelector(".cc-brief")).toBeNull();

      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);
      await waitFor(() =>
        expect(
          invokeMock.mock.calls.filter(
            ([command]) => command === "generate_handoff_doc",
          ),
        ).toHaveLength(2),
      );
    });

    it("retries SESSION_BUSY after immediate cancel and reopen without staying loading", async () => {
      let rejectOldDraft!: (reason: unknown) => void;
      const requestIds: string[] = [];
      let reopenedAttempts = 0;
      mockContinuationApp({
        generate: (args) => {
          requestIds.push(args.requestId);
          if (requestIds.length === 1) {
            return new Promise((_resolve, reject) => {
              rejectOldDraft = reject;
            });
          }
          reopenedAttempts += 1;
          if (reopenedAttempts === 1) {
            return Promise.reject(
              new Error("SESSION_BUSY:generate_handoff_doc"),
            );
          }
          return Promise.resolve({
            doc_markdown: "# 重开成功",
            suggested_title: "重开接续",
            memory_projection: null,
            warnings: [],
          });
        },
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(
          container.querySelector(".cc-brief [role=status]"),
        ).not.toBeNull(),
      );
      fireEvent.click(screen.getByRole("button", { name: "取消" }));
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);

      expect(await screen.findByText("重开成功")).toBeInTheDocument();
      expect(reopenedAttempts).toBe(2);
      expect(new Set(requestIds).size).toBe(2);
      expect(container.querySelector(".cc-brief [role=status]")).toBeNull();

      await act(async () => {
        rejectOldDraft(new Error("AL_ERR:continuation.handoffCancelled"));
      });
      expect(screen.getByText("重开成功")).toBeInTheDocument();
    });

    it("notifies and marks a ready draft off-session, then clears the mark on open", async () => {
      let resolveDraft!: (value: any) => void;
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二会话" }),
        ],
        generate: () =>
          new Promise((resolve) => {
            resolveDraft = resolve;
          }),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const parentRow = container.querySelector(
        '[data-session-id="parent-1"]',
      )!;
      fireEvent.contextMenu(parentRow);
      fireEvent.click(parentRow.querySelector('[data-action="handover"]')!);
      fireEvent.click(container.querySelector('[data-session-id="parent-2"]')!);
      await waitFor(() =>
        expect(
          container.querySelector('[data-session-id="parent-2"].active'),
        ).not.toBeNull(),
      );

      await act(async () => {
        resolveDraft({
          doc_markdown: "# 后台完成草稿",
          suggested_title: "接续标题",
          memory_projection: null,
          warnings: [],
        });
      });

      await waitFor(() =>
        expect(container.querySelector(".toast")).toHaveTextContent(
          "交接草稿已就绪：父会话",
        ),
      );
      expect(parentRow.querySelector(".sess__dot.done")).not.toBeNull();

      fireEvent.click(parentRow);
      await waitFor(() =>
        expect(parentRow.querySelector(".sess__dot.done")).toBeNull(),
      );
      expect(await screen.findByText("后台完成草稿")).toBeInTheDocument();
    });

    it("continuation handover uses the latest selected parent", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二父会话" }),
        ],
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const firstRow = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(firstRow);
      fireEvent.click(firstRow.querySelector('[data-action="handover"]')!);

      const secondRow = container.querySelector(
        '[data-session-id="parent-2"]',
      )!;
      fireEvent.contextMenu(secondRow);
      fireEvent.click(secondRow.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("generate_handoff_doc", {
          sessionId: "parent-2",
          requestId: expect.any(String),
        }),
      );
      await waitFor(() => {
        const parentInfo = container.querySelector(".cc-brief .cc-parent");
        expect(parentInfo).toHaveTextContent("第二父会话");
      });
    });

    it("second continuation handover keeps the newer panel visible", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二父会话" }),
        ],
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const firstRow = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(firstRow);
      fireEvent.click(firstRow.querySelector('[data-action="handover"]')!);

      const secondRow = container.querySelector(
        '[data-session-id="parent-2"]',
      )!;
      fireEvent.contextMenu(secondRow);
      fireEvent.click(secondRow.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("generate_handoff_doc", {
          sessionId: "parent-2",
          requestId: expect.any(String),
        }),
      );
      await waitFor(() => {
        const panel = container.querySelector(".cc-brief");
        expect(panel).not.toBeNull();
        expect(panel!.querySelector(".cc-parent")).toHaveTextContent(
          "第二父会话",
        );
      });
    });

    it("starts continuation with handoff document payload, refreshes, and opens child", async () => {
      const handoffDoc = "# 测试交接文档\n\n内容";
      const suggestedTitle = "测试标题";
      const sessionsState = [
        makeSession({
          id: "parent-1",
          title: "父会话",
          continued_to_session_id: null,
        }),
      ];
      const start = vi.fn(async () => {
        sessionsState[0] = {
          ...sessionsState[0],
          continued_to_session_id: "child-1",
        };
        sessionsState.push(
          makeSession({
            id: "child-1",
            title: "子会话",
            parent_session_id: "parent-1",
          }),
        );
        return "child-1";
      });
      mockContinuationApp({
        sessions: sessionsState,
        start,
        generate: () =>
          Promise.resolve({
            doc_markdown: handoffDoc,
            suggested_title: suggestedTitle,
            memory_projection: null,
            warnings: [],
          }),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);
      await screen.findByText("测试交接文档");
      fireEvent.click(screen.getByRole("button", { name: "启动子会话" }));

      await waitFor(() =>
        expect(start).toHaveBeenCalledWith({
          parentSessionId: "parent-1",
          handoffDoc,
          suggestedTitle,
        }),
      );
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "child-1",
        }),
      );
      expect(
        container.querySelector('[data-session-id="parent-1"]'),
      ).toHaveTextContent("已交接到 →");
      const childRow = container.querySelector('[data-session-id="child-1"]')!;
      const childLineage = childRow.querySelector(
        '[data-testid="session-lineage-child"]',
      );
      expect(childRow).toHaveTextContent("子会话");
      expect(childLineage).toHaveTextContent("↳");
      expect(childLineage?.getAttribute("title")).toContain("父会话");
    });

    it("persisted parent continuation makes composer readonly after reload", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({
            id: "parent-1",
            title: "父会话",
            continued_to_session_id: "child-1",
          }),
          makeSession({
            id: "child-1",
            title: "子会话",
            parent_session_id: "parent-1",
          }),
        ],
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      expect(screen.getByPlaceholderText(/输入消息/)).toBeDisabled();
      expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
      expect(
        screen.getByText("会话已交接到新会话·只读·请到新会话继续"),
      ).toBeInTheDocument();
      expect(
        container.querySelector('[data-session-id="parent-1"]'),
      ).toHaveTextContent("已交接到 →");
      const childRow = container.querySelector('[data-session-id="child-1"]')!;
      const childLineage = childRow.querySelector(
        '[data-testid="session-lineage-child"]',
      );
      expect(childRow).toHaveTextContent("子会话");
      expect(childLineage).toHaveTextContent("↳");
      expect(childLineage?.getAttribute("title")).toContain("父会话");
    });

    it("parent with child row but no continued pointer is readonly after reload", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({
            id: "parent-1",
            title: "父会话",
            continued_to_session_id: null,
          }),
          makeSession({
            id: "child-1",
            title: "子会话",
            parent_session_id: "parent-1",
          }),
        ],
      });
      render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      expect(screen.getByPlaceholderText(/输入消息/)).toBeDisabled();
      expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
      expect(
        screen.getByText("会话已交接到新会话·只读·请到新会话继续"),
      ).toBeInTheDocument();
    });

    it("按 activeRepoId 拉 list_groups", async () => {
      mockAppWith([makeSession({ id: "s1", title: "x" })]);
      render(<App />);
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("list_groups", {
          repoId: "local-default",
        }),
      );
    });

    describe("分组删除端到端", () => {
      const groupA: GroupMeta = {
        id: "gA",
        repo_id: "local-default",
        name: "前端",
        position: 0,
        created_at: 0,
      };

      function mockAppWithGroup() {
        invokeMock.mockImplementation((cmd: string, _args?: any) => {
          if (cmd === "list_agents") return Promise.resolve(agentProfiles);
          if (cmd === "list_sessions")
            return Promise.resolve([
              makeSession({ id: "s1", title: "组内会话", group_id: "gA" }),
            ]);
          if (cmd === "list_groups") return Promise.resolve([groupA]);
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
          if (cmd === "delete_group") return Promise.resolve();
          return Promise.resolve();
        });
      }

      it("⋯→删除分组→ConfirmDialog 含分组名 + 确认触发 delete_group", async () => {
        mockAppWithGroup();
        const { container } = render(<App />);

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "s1",
          }),
        );

        // 触发 group-more 按钮（需先 mouseEnter 让 hovered=true）
        const groupEl = container.querySelector(".sb-group") as Element;
        expect(groupEl).toBeTruthy();
        fireEvent.mouseEnter(groupEl);

        const moreBtn = container.querySelector(
          '[data-action="group-more"]',
        ) as Element;
        expect(moreBtn).toBeTruthy();
        fireEvent.click(moreBtn);

        const deleteBtn = container.querySelector(
          '[data-action="group-delete"]',
        ) as Element;
        expect(deleteBtn).toBeTruthy();
        fireEvent.click(deleteBtn);

        // ConfirmDialog 出现，heading 含分组名
        const dialog = screen.getByRole("dialog");
        expect(dialog).toBeInTheDocument();
        expect(
          screen.getByRole("heading", { name: /删除分组「前端」？/ }),
        ).toBeInTheDocument();

        // 点「删除」→ invoke delete_group
        fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("delete_group", { id: "gA" }),
        );
      });

      it("⋯→删除分组→ConfirmDialog 点取消 → dialog 消失 + 无 delete_group", async () => {
        mockAppWithGroup();
        const { container } = render(<App />);

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "s1",
          }),
        );

        const groupEl = container.querySelector(".sb-group") as Element;
        fireEvent.mouseEnter(groupEl);

        const moreBtn = container.querySelector(
          '[data-action="group-more"]',
        ) as Element;
        fireEvent.click(moreBtn);

        const deleteBtn = container.querySelector(
          '[data-action="group-delete"]',
        ) as Element;
        fireEvent.click(deleteBtn);

        const dialog = screen.getByRole("dialog");
        fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));

        expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
        expect(invokeMock.mock.calls.some((c) => c[0] === "delete_group")).toBe(
          false,
        );
      });
    });

    describe("crumb repo 切换链", () => {
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

      function mockAppWithTwoRepos() {
        invokeMock.mockImplementation((cmd: string, _args?: any) => {
          if (cmd === "list_agents") return Promise.resolve(agentProfiles);
          if (cmd === "list_sessions")
            return Promise.resolve([
              makeSession({ id: "sw1", title: "web 会话", repo_id: "r-web" }),
              makeSession({ id: "sa1", title: "api 会话", repo_id: "r-api" }),
            ]);
          if (cmd === "list_groups") return Promise.resolve([]);
          if (cmd === "get_messages") return Promise.resolve([]);
          if (cmd === "app_context")
            return Promise.resolve({
              namespaces: [
                {
                  id: "local",
                  kind: "local",
                  name: "Local",
                  is_builtin: 1,
                  last_active_repo_id: "r-web",
                  added_at: 0,
                  last_used_at: null,
                },
              ],
              active_namespace_id: "local",
              active_repo_id: "r-web",
              repos: [repoWeb, repoApi],
            });
          if (cmd === "list_repos") return Promise.resolve([repoWeb, repoApi]);
          if (cmd === "list_namespaces")
            return Promise.resolve([
              {
                id: "local",
                kind: "local",
                name: "Local",
                is_builtin: 1,
                last_active_repo_id: "r-web",
                added_at: 0,
                last_used_at: null,
              },
            ]);
          if (cmd === "set_active_namespace") return Promise.resolve("r-api");
          if (cmd === "set_last_active_repo") return Promise.resolve();
          return Promise.resolve();
        });
      }

      it("点 crumb repo 段选另一 repo → invoke set_last_active_repo + sidebar 会话集切换", async () => {
        mockAppWithTwoRepos();
        const { container } = render(<App />);

        // 等初始化：app_context 返回 r-web 为 active，sidebar 显 web 会话
        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "sw1",
          }),
        );

        // 两 repo → 左下项目切换器打开 RepoSwitcherDropdown
        fireEvent.click(screen.getByLabelText("项目切换器"));
        await waitFor(() =>
          expect(container.querySelector(".repo-switcher")).not.toBeNull(),
        );

        // 选 api repo（点 dd-row 含 "api" 文字的行）
        const rows = container.querySelectorAll(".repo-switcher .dd-row");
        const apiRow = Array.from(rows).find((r) =>
          r.textContent?.includes("api"),
        ) as Element;
        expect(apiRow).toBeTruthy();

        // wrap in act to flush all async state updates (onSelectRepo is async)
        await act(async () => {
          fireEvent.click(apiRow);
        });

        // set_last_active_repo 被 invoke
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
        expect(
          invokeMock.mock.invocationCallOrder[setActiveNsCall],
        ).toBeLessThan(invokeMock.mock.invocationCallOrder[setLastRepoCall]);
      });

      it("左栏收起：topbar 不再渲 repo 锚点（repo 切换归左下项目切换器）", async () => {
        invokeMock.mockImplementation((cmd: string, _args?: any) => {
          if (cmd === "list_agents") return Promise.resolve(agentProfiles);
          if (cmd === "list_sessions")
            return Promise.resolve([
              makeSession({
                id: "sw1",
                title: "web 会话",
                repo_id: "r-web",
              }),
              makeSession({
                id: "sa1",
                title: "api 会话",
                repo_id: "r-api",
              }),
            ]);
          if (cmd === "list_groups") return Promise.resolve([]);
          if (cmd === "get_messages") return Promise.resolve([]);
          if (cmd === "session_review") return Promise.resolve(emptyReview);
          if (cmd === "app_context")
            return Promise.resolve({
              namespaces: [
                {
                  id: "local",
                  kind: "local",
                  name: "Local",
                  is_builtin: 1,
                  last_active_repo_id: "r-web",
                  added_at: 0,
                  last_used_at: null,
                },
              ],
              active_namespace_id: "local",
              active_repo_id: "r-web",
              repos: [repoWeb, repoApi],
            });
          if (cmd === "list_repos") return Promise.resolve([repoWeb, repoApi]);
          if (cmd === "list_namespaces")
            return Promise.resolve([
              {
                id: "local",
                kind: "local",
                name: "Local",
                is_builtin: 1,
                last_active_repo_id: "r-web",
                added_at: 0,
                last_used_at: null,
              },
            ]);
          if (cmd === "set_active_namespace") return Promise.resolve("r-api");
          if (cmd === "set_last_active_repo") return Promise.resolve();
          return Promise.resolve();
        });
        const { container } = render(<App />);

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "sw1",
          }),
        );

        fireEvent.click(screen.getByLabelText("折叠会话栏"));

        await waitFor(() =>
          expect(container.querySelector(".surface.full")).not.toBeNull(),
        );
        expect(
          container.querySelector(".sf-head .project-switcher"),
        ).toBeNull();
        expect(container.querySelector(".sf-head .repo-switcher")).toBeNull();
        expect(container.querySelector(".sidebar")).toBeNull();
        expect(
          within(
            container.querySelector(".sf-head") as HTMLElement,
          ).queryByLabelText("项目切换器"),
        ).toBeNull();
      });
    });
  });

  describe("shell-redesign 阶段0 · 新顶层 surface CSS 基座", () => {
    const shellCss = readFileSync("src/styles/global.css", "utf-8");
    it("定义 .app-shell 横向 flex 容器", () => {
      expect(shellCss).toMatch(/\.app-shell\s*\{[^}]*display:\s*flex/);
    });
    it(".surface 填充 + 仅左侧圆角", () => {
      expect(shellCss).toMatch(
        /\.surface\s*\{[^}]*border-radius:\s*15px 0 0 15px/,
      );
    });
    it(".surface.full 收起全色接管（margin-left:0 + 无圆角）", () => {
      expect(shellCss).toMatch(/\.surface\.full\s*\{[^}]*margin-left:\s*0/);
      expect(shellCss).toMatch(/\.surface\.full\s*\{[^}]*border-radius:\s*0/);
    });
    it("--chrome-inset 默认归零，仅 macOS Overlay 为红绿灯让位 78px", () => {
      expect(shellCss).toMatch(/:root\s*\{[^}]*--chrome-inset:\s*0/);
      expect(shellCss).toMatch(
        /html\[data-os=["']macos["']\]\s*\{[^}]*--chrome-inset:\s*78px/,
      );
      expect(shellCss).toMatch(
        /\.sb-top\s*\{[^}]*padding:\s*0 8px 0 var\(--chrome-inset\)/,
      );

      const style = document.createElement("style");
      style.textContent = shellCss;
      document.head.appendChild(style);
      document.documentElement.dataset.os = "macos";
      expect(
        getComputedStyle(document.documentElement)
          .getPropertyValue("--chrome-inset")
          .trim(),
      ).toBe("78px");
      document.documentElement.dataset.os = "windows";
      expect(
        getComputedStyle(document.documentElement)
          .getPropertyValue("--chrome-inset")
          .trim(),
      ).toBe("0");
      style.remove();
      delete document.documentElement.dataset.os;
    });
    it(".session-pane.hidden 与 .tools-pane.full 组合态类存在", () => {
      expect(shellCss).toMatch(
        /\.session-pane\.hidden\s*\{[^}]*display:\s*none/,
      );
      expect(shellCss).toMatch(/\.tools-pane\.full\s*\{[^}]*flex:\s*1 1 auto/);
    });
    it("阶段1 收尾 · App 不再引用 TopBar + TopBar 文件已删", () => {
      const appSrc = readFileSync("src/App.tsx", "utf-8");
      expect(appSrc).not.toMatch(/components\/TopBar/);
      expect(existsSync("src/components/TopBar.tsx")).toBe(false);
      expect(existsSync("src/components/TopBar.test.tsx")).toBe(false);
    });
  });

  it("导航 IA：跨 namespace 选 repo 原子切换（set_active_namespace 先于 set_last_active_repo·显式 nsId·不 clobber·repo1 显选中）", async () => {
    const localNs = {
      id: "local",
      kind: "local",
      name: "本机",
      is_builtin: 1,
      last_active_repo_id: null,
      added_at: 0,
      last_used_at: null,
    };
    const acmeNs = {
      id: "gh:acme",
      kind: "github_org",
      name: "acme",
      is_builtin: 0,
      last_active_repo_id: "r-acme",
      added_at: 0,
      last_used_at: null,
    };
    // gh:other 的旧 last-active = r-stale（codex round-2 BLOCK：故意 ≠ 目标 r-other）
    const otherNs = {
      id: "gh:other",
      kind: "github_org",
      name: "other",
      is_builtin: 0,
      last_active_repo_id: "r-stale",
      added_at: 0,
      last_used_at: null,
    };
    const mk = (id: string, name: string, nsId: string, owner: string) => ({
      id,
      name,
      source: "github",
      owner,
      path: `/tmp/${name}`,
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: nsId,
    });
    const rAcme = mk("r-acme", "acme-web", "gh:acme", "acme");
    const rStale = mk("r-stale", "stale-svc", "gh:other", "other"); // gh:other 旧 last-active
    const rOther = mk("r-other", "other-svc", "gh:other", "other"); // 用户真正点选的

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNs, acmeNs, otherNs],
          active_namespace_id: "gh:acme",
          active_repo_id: "r-acme",
          repos: [rAcme],
        });
      if (cmd === "list_repos") return Promise.resolve([rAcme, rStale, rOther]);
      if (cmd === "list_namespaces")
        return Promise.resolve([localNs, acmeNs, otherNs]);
      // 关键陷阱：set_active_namespace 返回 gh:other 的旧 last-active = "r-stale"。
      // 正确实现忽略此返回值·用显式 repoId="r-other"；错误实现（用返回值 set repo）会落到 r-stale → 测试抓住。
      if (cmd === "set_active_namespace") return Promise.resolve("r-stale");
      if (cmd === "set_last_active_repo") return Promise.resolve();
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));
    // 初始项目切换器 repo = acme-web
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );

    // 开项目切换器下拉 → 选别 namespace 的 repo（other-svc 在 gh:other 段）
    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("other-svc"));

    // 持久化：set_last_active_repo 参数用新 nsId（非闭包旧 gh:acme）
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_last_active_repo", {
        namespaceId: "gh:other",
        repoId: "r-other",
      }),
    );
    // set_active_namespace 必先于 set_last_active_repo（禁顺序调 clobber 的核心断言）
    const calls = invokeMock.mock.calls.map((c) => c[0]);
    const iNs = calls.indexOf("set_active_namespace");
    const iRepo = calls.indexOf("set_last_active_repo");
    expect(iNs).toBeGreaterThanOrEqual(0);
    expect(iNs).toBeLessThan(iRepo);
    expect(invokeMock).toHaveBeenCalledWith("set_active_namespace", {
      id: "gh:other",
    });
    // 各只调一次（防顺序调/重复调污染·codex round-2）
    expect(calls.filter((c) => c === "set_active_namespace").length).toBe(1);
    expect(calls.filter((c) => c === "set_last_active_repo").length).toBe(1);
    // 切后项目切换器 = 选中的 other-svc（**非** set_active_namespace 返回的旧 stale-svc·这是抓 clobber 的关键断言）
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "other-svc",
      ),
    );
    expect(screen.getByLabelText("项目切换器")).not.toHaveTextContent(
      "stale-svc",
    );
  });

  it("导航历史：项目简介之间切 repo 后左侧栏后退/前进恢复具体 repo", async () => {
    const acmeNs = {
      id: "gh:acme",
      kind: "github_org",
      name: "acme",
      is_builtin: 0,
      last_active_repo_id: "r-acme",
      added_at: 0,
      last_used_at: null,
    };
    const otherNs = {
      id: "gh:other",
      kind: "github_org",
      name: "other",
      is_builtin: 0,
      last_active_repo_id: "r-other",
      added_at: 0,
      last_used_at: null,
    };
    const mk = (id: string, name: string, nsId: string, owner: string) => ({
      id,
      name,
      source: "github",
      owner,
      path: `/tmp/${name}`,
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: nsId,
    });
    const rAcme = mk("r-acme", "acme-web", "gh:acme", "acme");
    const rOther = mk("r-other", "other-svc", "gh:other", "other");

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [acmeNs, otherNs],
          active_namespace_id: "gh:acme",
          active_repo_id: "r-acme",
          repos: [rAcme],
        });
      if (cmd === "list_repos") return Promise.resolve([rAcme, rOther]);
      if (cmd === "list_namespaces") return Promise.resolve([acmeNs, otherNs]);
      if (cmd === "set_active_namespace") return Promise.resolve("r-other");
      if (cmd === "set_last_active_repo") return Promise.resolve();
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "create_session") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );

    fireEvent.click(screen.getByText("项目简介"));
    expect(
      await screen.findByRole("heading", { name: "acme-web" }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("other-svc"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "other-svc",
      ),
    );
    expect(
      await screen.findByRole("heading", { name: "other-svc" }),
    ).toBeInTheDocument();

    await waitFor(() =>
      expect(screen.getByLabelText("后退")).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByLabelText("后退"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );
    expect(
      await screen.findByRole("heading", { name: "acme-web" }),
    ).toBeInTheDocument();

    await waitFor(() =>
      expect(screen.getByLabelText("前进")).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByLabelText("前进"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "other-svc",
      ),
    );
    expect(
      await screen.findByRole("heading", { name: "other-svc" }),
    ).toBeInTheDocument();
  });

  it("导航 IA：onSelectRepoInNamespace 刷新/持久化 IPC 失败 → catch return 不半切（UI 保持旧 repo·codex T3 审 BLOCK-1）", async () => {
    const localNs = {
      id: "local",
      kind: "local",
      name: "本机",
      is_builtin: 1,
      last_active_repo_id: null,
      added_at: 0,
      last_used_at: null,
    };
    const acmeNs = {
      id: "gh:acme",
      kind: "github_org",
      name: "acme",
      is_builtin: 0,
      last_active_repo_id: "r-acme",
      added_at: 0,
      last_used_at: null,
    };
    const otherNs = {
      id: "gh:other",
      kind: "github_org",
      name: "other",
      is_builtin: 0,
      last_active_repo_id: "r-other",
      added_at: 0,
      last_used_at: null,
    };
    const mk = (id: string, name: string, nsId: string, owner: string) => ({
      id,
      name,
      source: "github",
      owner,
      path: `/tmp/${name}`,
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: nsId,
    });
    const rAcme = mk("r-acme", "acme-web", "gh:acme", "acme");
    const rOther = mk("r-other", "other-svc", "gh:other", "other");

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNs, acmeNs, otherNs],
          active_namespace_id: "gh:acme",
          active_repo_id: "r-acme",
          repos: [rAcme],
        });
      if (cmd === "list_repos") return Promise.resolve([rAcme, rOther]);
      if (cmd === "list_namespaces")
        return Promise.resolve([localNs, acmeNs, otherNs]);
      if (cmd === "set_active_namespace") return Promise.resolve("r-other");
      // 持久化第二步失败：handler 须 catch return·不切 UI（也不产生未捕获 rejection）
      if (cmd === "set_last_active_repo")
        return Promise.reject(new Error("DB_WRITE_FAILED"));
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );

    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("other-svc"));

    // set_last_active_repo 被调（确认走到了第二步才失败）
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_last_active_repo", {
        namespaceId: "gh:other",
        repoId: "r-other",
      }),
    );
    // 失败后 UI 不半切：项目切换器仍显旧 repo（catch return·未 set state）
    expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web");
  });

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

  it("②a：右面板开 → .surface 挂 rpopen（消息列自适应）·关 → 无 rpopen", async () => {
    mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");
    expect(document.querySelector(".surface.rpopen")).toBeNull();
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    await waitFor(() =>
      expect(document.querySelector(".surface.rpopen")).not.toBeNull(),
    );
  });

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

  const orchestratedTeamMessages = (): ChatMessage[] => [
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
  ];

  const workerTerminalEvent = (
    runId: string,
    assignmentId: string,
    sessionId = "s1",
  ) =>
    dEnv(
      {
        run_id: runId,
        assignment_id: assignmentId,
        orchestrated: true,
        status_transition: "done",
      },
      {
        kind: "completed",
        cost_usd: null,
        input_tokens: 10,
        output_tokens: 5,
        final_text: "干完了",
      },
      sessionId,
    );

  function mockResumeLeadSession() {
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "resume_lead_session") return Promise.resolve();
      return baseInvoke?.(cmd, args);
    });
  }

  function resumeLeadSessionCallCount() {
    return invokeMock.mock.calls.filter(
      ([cmd]) => cmd === "resume_lead_session",
    ).length;
  }

  // 竞速重试用例：`resume_lead_session` 调用按次序走不同结果（第 1 次 reject/第 2 次
  // resolve 等），别的 invoke 走既有 baseInvoke。
  function mockResumeLeadSessionSequence(
    handlers: Array<() => Promise<unknown>>,
  ) {
    const baseInvoke = invokeMock.getMockImplementation();
    let callIndex = 0;
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "resume_lead_session") {
        const handler = handlers[Math.min(callIndex, handlers.length - 1)];
        callIndex += 1;
        return handler();
      }
      return baseInvoke?.(cmd, args);
    });
  }

  it("worker 完成自动唤醒 lead：orchestrated worker 终态事件 + lead 空闲 → resume_lead_session 恰好被 invoke 一次(参数正确)", async () => {
    mockBasicApp(agentProfiles, { messages: orchestratedTeamMessages() });
    mockResumeLeadSession();

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();

    await act(async () => {
      cb(workerTerminalEvent("wrun-a", "wa-a"));
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "resume_lead_session",
        expect.objectContaining({ sessionId: "s1", leadAgentId: "claude" }),
      ),
    );
    expect(resumeLeadSessionCallCount()).toBe(1);
  });

  it("worker 完成自动唤醒 lead：同一 worker 终态事件重放 → 不重复 invoke", async () => {
    mockBasicApp(agentProfiles, { messages: orchestratedTeamMessages() });
    mockResumeLeadSession();

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();
    const ev = workerTerminalEvent("wrun-b", "wa-b");

    await act(async () => {
      cb(ev);
      cb(ev);
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "resume_lead_session",
        expect.anything(),
      ),
    );
    expect(resumeLeadSessionCallCount()).toBe(1);
  });

  it("worker 完成自动唤醒 lead：lead 正在跑 → 不 invoke（不排队，lead 自己下轮能看到 report）", async () => {
    // configureTeamLead 要点「设为队长」——hasLeadCapability 只认
    // provider==="claude"&&access==="native"（与 cap_lead 无关），默认 agentProfiles
    // fixture 是 provider:"anthropic"/access:"api"，故此处照 G3 先例custom 一份。
    mockBasicApp([
      agentProfile({ provider: "claude", access: "native" }),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        sort_order: 1,
      }),
    ]);
    mockResumeLeadSession();

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "先跑起来" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "start_lead_session",
        expect.objectContaining({ sessionId: "s1" }),
      ),
    );

    const cb = agentEventCb();
    await act(async () => {
      cb(workerTerminalEvent("wrun-c", "wa-c"));
    });

    expect(resumeLeadSessionCallCount()).toBe(0);
  });

  it("worker 完成自动唤醒 lead：连续 10 次自动续喂后第 11 个终态不再 invoke（cap=10）", async () => {
    mockBasicApp(agentProfiles, { messages: orchestratedTeamMessages() });
    mockResumeLeadSession();

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();

    for (let i = 0; i < 11; i++) {
      await act(async () => {
        cb(workerTerminalEvent(`wrun-cap-${i}`, `wa-cap-${i}`));
      });
    }

    await waitFor(() => expect(resumeLeadSessionCallCount()).toBe(10));
  });

  it("worker 完成自动唤醒 lead：首次撞 dispatch intent 竞速拒绝(AL_ERR:run.teamMembersActive) → 短延迟后重试一次并成功(恰好 invoke 两次)", async () => {
    mockBasicApp(agentProfiles, { messages: orchestratedTeamMessages() });
    mockResumeLeadSessionSequence([
      // Tauri invoke 对 `Result<_, String>` 命令的 reject 值是裸字符串（不是 JS Error 实例）
      // ——与 src-tauri/src/lib.rs::reserve_new_session_run 的真实返回形态一致
      // （`ui_msg::al_err("run.teamMembersActive", ...)` 产出 `AL_ERR:run.teamMembersActive:{...}`）。
      () =>
        Promise.reject(
          'AL_ERR:run.teamMembersActive:{"detail":"队员仍在执行上一轮派单"}',
        ),
      () => Promise.resolve(),
    ]);

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();

    await act(async () => {
      cb(workerTerminalEvent("wrun-race", "wa-race"));
    });

    // 首次拒绝已发生·此刻应恰好 1 次（重试还没到点）。
    await waitFor(() => expect(resumeLeadSessionCallCount()).toBe(1));

    // 竞速重试延迟窗口过后应补上第二次 invoke（成功）。
    await waitFor(() => expect(resumeLeadSessionCallCount()).toBe(2), {
      timeout: 2000,
    });
  });

  it("worker 完成自动唤醒 lead：拒因非 run.teamMembersActive → 不重试(保守)", async () => {
    mockBasicApp(agentProfiles, { messages: orchestratedTeamMessages() });
    mockResumeLeadSessionSequence([
      () => Promise.reject("boom: 别的原因"),
      () => Promise.resolve(),
    ]);

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });
    const cb = agentEventCb();

    await act(async () => {
      cb(workerTerminalEvent("wrun-noretry", "wa-noretry"));
    });

    await waitFor(() => expect(resumeLeadSessionCallCount()).toBe(1));

    // 给足竞速重试延迟窗口，确认确实没有第二次 invoke。
    await new Promise((resolve) => setTimeout(resolve, 900));
    expect(resumeLeadSessionCallCount()).toBe(1);
  });

  it("onDecisionChoose: MCP ask_user 卡 → 调 answer_lead_question·不调 choose_decision_card/lead_step", async () => {
    // Setup: a session with a pending decision card (not a local-dispatch card)
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
          decisionCardMessage(["继续", "先停下"], {
            decision_id: "mcp-dc-1",
            kind: "ask",
            question: "改哪个配置文件？",
            recommended: "继续",
            rationale: "队长需要更多信息",
            source_run_id: "mcp-lead-1",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    expect(
      inlineDecisionCard().getByText(/改哪个配置文件/),
    ).toBeInTheDocument();
    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /继续/ }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
        sessionId: "s1",
        decisionId: "mcp-dc-1",
        answer: "继续",
      }),
    );
    // MCP path: must NOT call choose_decision_card or lead_step
    expect(invokeMock).not.toHaveBeenCalledWith(
      "choose_decision_card",
      expect.anything(),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("lead_step", expect.anything());
  });

  it("onDecisionChoose: solo MCP 卡·run 已收工 → 答复照常落库但不误调 resume_lead_session", async () => {
    mockBasicApp(
      [
        agentProfile({
          id: "codex",
          name: "Codex",
          provider: "openai",
          access: "native",
        }),
      ],
      {
        messages: [
          decisionCardMessage(["继续", "先停下"], {
            decision_id: "mcp-dc-solo-late",
            kind: "ask",
            question: "要不要推送？",
            recommended: "继续",
            source_run_id: "mcp-lead-solo-late",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question") return Promise.resolve();
      if (cmd === "resume_lead_session")
        return Promise.reject("SOLO_SHOULD_NOT_RESUME");
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Codex");

    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /继续/ }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
        sessionId: "s1",
        decisionId: "mcp-dc-solo-late",
        answer: "继续",
      }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "resume_lead_session",
      expect.anything(),
    );
    expect(screen.queryByRole("button", { name: "重试" })).toBeNull();
  });

  it("onDecisionChoose: team MCP 卡·run 已收工(不在跑) → answer_lead_question 成功后自动调 resume_lead_session（G3 停摆修复）", async () => {
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
          decisionCardMessage(["继续", "先停下"], {
            decision_id: "mcp-dc-resume",
            kind: "ask",
            question: "要不要继续？",
            recommended: "继续",
            source_run_id: "mcp-lead-resume",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question") return Promise.resolve();
      if (cmd === "resume_lead_session") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    // 此会话没有任何 run 在跑（没送过消息、runningSessionsRef 里没有 s1）。
    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /继续/ }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
        sessionId: "s1",
        decisionId: "mcp-dc-resume",
        answer: "继续",
      }),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "resume_lead_session",
        expect.objectContaining({ sessionId: "s1", leadAgentId: "claude" }),
      ),
    );
  });

  it("onDecisionChoose: MCP 卡·run 仍在跑 → 不调 resume_lead_session（避免撞现有 Running 槽）", async () => {
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
          decisionCardMessage(["继续", "先停下"], {
            decision_id: "mcp-dc-running",
            kind: "ask",
            question: "要不要继续？",
            recommended: "继续",
            source_run_id: "mcp-lead-running",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question") return Promise.resolve();
      if (cmd === "resume_lead_session") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    // 先送一条消息，把该会话置成「run 在跑」（mode=team → startLeadSessionForComposer
    // 同步 setRun，runningSessionsRef 立刻有 s1）。
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "先跑起来" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "start_lead_session",
        expect.objectContaining({ sessionId: "s1" }),
      ),
    );

    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /继续/ }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
        sessionId: "s1",
        decisionId: "mcp-dc-running",
        answer: "继续",
      }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "resume_lead_session",
      expect.anything(),
    );
  });

  it("onDecisionChoose: legacy 决策卡(非 mcp-lead 前缀) → 直接 choose_decision_card + lead_step·不调 answer_lead_question", async () => {
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
          decisionCardMessage(["开跑", "先停下"], {
            decision_id: "legacy-dc-1",
            kind: "ask",
            question: "继续吗？",
            recommended: "开跑",
            source_run_id: "run-legacy-1",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question")
        return Promise.reject("NO_PENDING_QUESTION:legacy-dc-1");
      if (cmd === "choose_decision_card") return Promise.resolve(true);
      if (cmd === "lead_step")
        return Promise.resolve({
          status: "decided",
          action: { action: "reply", rationale: "ok" },
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
      if (cmd === "append_message") return Promise.resolve();
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    expect(inlineDecisionCard().getByText(/继续吗？/)).toBeInTheDocument();
    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /开跑/ }));

    // Legacy 卡（source_run_id 非 mcp-lead 前缀）→ 不探测 answer_lead_question·直接走 lead_step。
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "choose_decision_card",
        expect.objectContaining({
          decisionId: "legacy-dc-1",
          expectStatus: "pending",
          nextStatus: "submitting",
        }),
      ),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "answer_lead_question",
      expect.anything(),
    );
    // choose_decision_card CAS（保留·验 legacy 路完整）
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "choose_decision_card",
        expect.objectContaining({
          decisionId: "legacy-dc-1",
          expectStatus: "pending",
          nextStatus: "submitting",
        }),
      ),
    );
    // And lead_step with the user's answer
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "lead_step",
        expect.objectContaining({ userMsg: "开跑" }),
      ),
    );
  });

  it("onDecisionChoose: MCP 卡 + NO_PENDING_QUESTION(队长已停/陈旧) → 不触发 lead_step（整支终审 opus Important 回归）", async () => {
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
          decisionCardMessage(["继续", "先停下"], {
            decision_id: "mcp-stale-1",
            kind: "ask",
            question: "缺信息：改哪个？",
            recommended: "继续",
            source_run_id: "mcp-lead-stale-1",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      // 模拟队长已停：handler 已取消·decision_id 已移除 → NO_PENDING_QUESTION
      if (cmd === "answer_lead_question")
        return Promise.reject("NO_PENDING_QUESTION:mcp-stale-1");
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    expect(
      inlineDecisionCard().getByText(/缺信息：改哪个/),
    ).toBeInTheDocument();
    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /继续/ }));

    // MCP 卡按身份(mcp-lead 前缀)路由·试 answer_lead_question
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
        sessionId: "s1",
        decisionId: "mcp-stale-1",
        answer: "继续",
      }),
    );
    // 关键：NO_PENDING_QUESTION 也【绝不】回退 legacy lead_step（防停掉的会话被误唤起 LLM 跑）
    expect(invokeMock).not.toHaveBeenCalledWith(
      "choose_decision_card",
      expect.anything(),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("lead_step", expect.anything());
  });

  it("lead-decision-card 事件: 实时追加决策卡到会话消息·重复触发不产生重复", async () => {
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
      { messages: [] },
    );

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    const cb = leadDecisionCardCb();

    const block: Extract<Block, { type: "decision_card" }> = {
      type: "decision_card",
      decision_id: "live-dc-1",
      kind: "ask",
      question: "要改哪个配置？",
      options: ["config.json", "settings.ts"],
      recommended: "config.json",
      rationale: "队长需要更多信息",
      payload: null,
      source_run_id: "run-live-1",
      status: "pending",
      chosen_option: null,
      created_at: 1000,
    };

    // Fire the event once
    await act(async () => {
      cb({ payload: { session_id: "s1", block } });
    });

    // Card should be visible
    await waitFor(() => {
      expect(
        inlineDecisionCard().getByText(/要改哪个配置？/),
      ).toBeInTheDocument();
    });

    // Fire the SAME event again (same decision_id) — idempotency: must NOT duplicate
    await act(async () => {
      cb({ payload: { session_id: "s1", block } });
    });

    // Question text should appear exactly once (no duplicate)
    const matches = inlineDecisionCard().queryAllByText(/要改哪个配置？/);
    expect(matches.length).toBe(1);
  });

  it("lead-message-appended 事件: 实时追加回显消息到会话流·按 message.id 去重", async () => {
    // 决策打扰收敛刀 T1·症状 B 根修：后端 append_decision_echo 写库成功后 emit
    // "lead-message-appended"——前端应在停留当前进程时立刻把这条消息插进消息流，
    // 不必等下次 get_messages 全量拉取。同 id 二次 emit（未来重拉双份的防线）不应重复插入。
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
      { messages: [] },
    );

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    const cb = leadMessageAppendedCb();
    const echoText = "已选择「继续」（要不要继续？）";
    const message: ChatMessage & { id: number } = {
      id: 42,
      role: "assistant",
      content: [{ type: "text", text: echoText }],
      engine: "decision-echo",
      agent_id: "lead-claude",
      agent_name_snapshot: "Claude 队长",
      created_at: 1000,
    };

    await act(async () => {
      cb({ payload: { session_id: "s1", message } });
    });

    await waitFor(() => {
      expect(screen.getByText(echoText)).toBeInTheDocument();
    });

    // 同一条消息（同 id）再 emit 一次——不应产生第二条。
    await act(async () => {
      cb({ payload: { session_id: "s1", message } });
    });

    expect(screen.queryAllByText(echoText).length).toBe(1);
  });

  it("lead-message-appended 事件: 迟到回显按 id 插到无 id 流式尾巴之前", async () => {
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
          {
            id: "659",
            role: "assistant",
            content: [{ type: "text", text: "message-659" }],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
          } as ChatMessage & { id: string },
          {
            role: "assistant",
            content: [],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
          },
        ],
      },
    );

    render(<App />);
    await screen.findByText("message-659");

    const cb = leadMessageAppendedCb();
    await act(async () => {
      cb({
        payload: {
          session_id: "s1",
          message: {
            id: 664,
            role: "user",
            content: [{ type: "text", text: "message-664" }],
            engine: "decision-echo",
            agent_id: null,
            agent_name_snapshot: null,
          },
        },
      });
    });

    await screen.findByText("message-664");
    const turns = [
      ...document.querySelectorAll<HTMLElement>(".stream-content > .turn"),
    ];
    expect(
      turns.map((turn) =>
        turn.textContent?.includes("message-659")
          ? "659"
          : turn.textContent?.includes("message-664")
            ? "664"
            : "no-id-tail",
      ),
    ).toEqual(["659", "664", "no-id-tail"]);
  });

  it("lead-message-appended 事件: 目标会话没有 messagesRef 缓存时忽略·不挡后续 get_messages 全量拉取", async () => {
    // T3 顺手加固：会话「s2」从未被打开过（messagesRef 里没有它的 key），此时对它 emit
    // lead-message-appended 若照旧用「只有这一条回显」种下缓存，之后真正打开 s2 会因为
    // `!messagesRef.current.has(id)` 判假而跳过 get_messages 全量拉取——真实历史丢失，
    // 只剩这一条回显。守卫后：事件被忽略，s2 打开时仍会真的发起全量拉取。
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
      { messages: [] },
    );
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      return fallback?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const cb = leadMessageAppendedCb();
    const strayEchoText = "已选择「继续」（s2 从未打开·这条不该种下缓存）";
    await act(async () => {
      cb({
        payload: {
          session_id: "s2",
          message: {
            id: 99,
            role: "assistant",
            content: [{ type: "text", text: strayEchoText }],
            engine: "decision-echo",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            created_at: 1000,
          },
        },
      });
    });

    // 没有缓存条目时事件被忽略，不产生任何可见内容。
    expect(screen.queryByText(strayEchoText)).not.toBeInTheDocument();

    // 真正打开 s2：如果守卫失效（种下了只有一条消息的缓存），这里会因
    // `!messagesRef.current.has("s2")` 判假而跳过 get_messages，断言就会失败。
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    // 全量拉取（mock 返回 []）落地后，那条游离回显依旧不该出现在 s2 里。
    expect(screen.queryByText(strayEchoText)).not.toBeInTheDocument();
  });

  it("onDecisionChoose: MCP 卡点击后先置 submitting(按钮置灰)·非双击失败回滚 pending(可重新点选)", async () => {
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
          decisionCardMessage(["继续", "先停下"], {
            decision_id: "mcp-dc-submit",
            kind: "ask",
            question: "要不要继续？",
            recommended: "继续",
            source_run_id: "mcp-lead-submit",
          }),
        ],
      },
    );

    const answerDeferred = deferred<void>();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question") return answerDeferred.promise;
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    await clickDecisionOption("继续");

    // invoke 尚未落定：按钮应处于 submitting 态(置灰)。
    await waitFor(() => {
      expect(
        inlineDecisionCard().getByRole("button", { name: /继续/ }),
      ).toBeDisabled();
    });

    // 非 NO_PENDING_QUESTION 的真失败 → 回滚 pending，按钮重新可点。
    await act(async () => {
      answerDeferred.reject(new Error("network blip"));
      await answerDeferred.promise.catch(() => {});
    });

    await waitFor(() => {
      expect(
        inlineDecisionCard().getByRole("button", { name: /继续/ }),
      ).not.toBeDisabled();
    });
  });
});

describe("pruneNavHistory", () => {
  it("删除条目在当前索引之前时，正确修正索引", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 2（s3），删除 s1（索引 0 之前）
    const result = pruneNavHistory(history, 2, "s1");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(1); // 2 - 1 = 1
  });

  it("删除条目在当前索引之后时，索引不变", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 0（s1），删除 s3（索引 2 之后）
    const result = pruneNavHistory(history, 0, "s3");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(0);
  });

  it("删除当前条目时，索引前移到前一个有效条目", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 1（s2），删除 s2
    const result = pruneNavHistory(history, 1, "s2");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ]);
    // 删除当前条目后，索引应前移到 0（指向 s1），避免"按一下没反应"
    expect(result.index).toBe(0);
  });

  it("删除后出现相邻重复条目时正确合并", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 2（s1），删除 s2 后会变成 s1->s1，应合并
    const result = pruneNavHistory(history, 2, "s2");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ]);
    // 合并后只剩一条，索引应为 0
    expect(result.index).toBe(0);
  });

  it("合并时正确调整当前索引（当前索引在合并对的第二个条目）", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 2 是第二个 s1，删除 s2 后合并时索引应递减到 0
    const result = pruneNavHistory(history, 2, "s2");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(0);
  });

  it("删除所有条目后，索引为 -1（空历史）", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
    ];
    const result = pruneNavHistory(history, 0, "s1");
    // 假设我们再删一个 s2（这里只演示单个会话剪枝，实际会按会话 id 逐一删）
    const final = pruneNavHistory(result.history, result.index, "s2");
    expect(final.history).toEqual([]);
    expect(final.index).toBe(-1);
  });

  it("非 session 条目不受影响", () => {
    const history = [
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns1",
        repoId: "repo1",
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns2",
        repoId: "repo2",
      },
    ];
    const result = pruneNavHistory(history, 1, "s1");
    expect(result.history).toEqual([
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns1",
        repoId: "repo1",
      },
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns2",
        repoId: "repo2",
      },
    ]);
    // 索引从 1 变成 0（s1 被删，且原位置为 1）
    expect(result.index).toBe(0);
  });

  it("多个相同 sessionId 条目全部被移除", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    const result = pruneNavHistory(history, 4, "s1");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ]);
    // 索引从 4 变成 1（前面删了 3 个 s1）
    expect(result.index).toBe(1);
  });

  it("连续删除和合并的复杂场景", () => {
    // 历史栈：s1 -> s2 -> s1 -> s2 -> s1，删除 s2
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    const result = pruneNavHistory(history, 4, "s2");
    // 删除 s2 后变成：s1 -> s1 -> s1 -> s1，应合并成单个 s1
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(0);
  });
});

describe("resetAutoResumeStreak", () => {
  it("清零指定 session 的连续续喂计数·不动其他 session", () => {
    const ref = {
      current: new Map([
        ["s1", 5],
        ["s2", 3],
      ]),
    };
    resetAutoResumeStreak(ref, "s1");
    expect(ref.current.has("s1")).toBe(false);
    expect(ref.current.get("s2")).toBe(3);
  });

  it("对未记过账的 session 调用是安全 no-op", () => {
    const ref = { current: new Map<string, number>() };
    expect(() => resetAutoResumeStreak(ref, "s-unknown")).not.toThrow();
    expect(ref.current.size).toBe(0);
  });
});
