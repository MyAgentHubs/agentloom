import type { vi } from "vitest";
import type { AgentProfile, Session, ChatMessage } from "../../types/agent";
import { makeSession } from "../../test/factories";
import type { createAppTestFixtures } from "./appTestFixtures";

export function createAppTestMocks(
  invokeMock: ReturnType<typeof vi.fn>,
  fixtures: ReturnType<typeof createAppTestFixtures>,
) {
  const {
    agentProfiles,
    emptyReview,
    reviewWithChanges,
    localNamespace,
    localRepo,
    githubNamespace,
    githubRepo,
  } = fixtures;

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
      sessions?: Session[];
      runtimeDetect?: {
        claude: { available: boolean; creds_hint?: boolean | null };
        codex: { available: boolean; creds_hint?: boolean | null };
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
        return Promise.resolve(
          options.sessions ?? [
            makeSession({
              id: "s1",
              title: "会话一",
              repo_id: "local-default",
              namespace_id: "local",
              ...options.session,
            }),
          ],
        );
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

  function sessionReviewCallCount() {
    return invokeMock.mock.calls.filter(
      ([command]) => command === "session_review",
    ).length;
  }

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

  return {
    mockAppWithReview,
    keepPreviewLoading,
    mockBasicApp,
    mockRemovableProjectApp,
    sessionReviewCallCount,
    setupRunningS1,
    mockAppWith,
  };
}
