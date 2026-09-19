import type {
  AgentProfile,
  MemberUnit,
  Block,
  ChatMessage,
} from "../../types/agent";

export function createAppTestFixtures() {
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

  const reviewWithChanges = {
    has_changes: true,
    stat: " a.txt | 1 +",
    patch: "diff --git a/a.txt b/a.txt\n@@ -0,0 +1 @@\n+hello\n",
    files_changed: 1,
  };

  function escapeRegExp(value: string): string {
    return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
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

  function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason?: unknown) => void;
    const promise = new Promise<T>((promiseResolve, promiseReject) => {
      resolve = promiseResolve;
      reject = promiseReject;
    });

    return { promise, resolve, reject };
  }

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

  return {
    localNamespace,
    localRepo,
    githubNamespace,
    githubRepo,
    emptyReview,
    agentProfile,
    agentProfiles,
    LAST_AGENT_ID_KEY,
    runCard,
    appMember,
    reviewWithChanges,
    escapeRegExp,
    dEnv,
    decisionCardBlock,
    decisionCardMessage,
    deferred,
    askCardPayload,
    orchestratedTeamMessages,
    workerTerminalEvent,
  };
}
