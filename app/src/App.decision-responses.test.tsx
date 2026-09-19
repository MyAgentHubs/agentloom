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
    emitAgentEventBatch,
    decisionCardMessage,
    inlineDecisionCard,
    clickDecisionOption,
    deferred,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

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
      if (cmd === "answer_lead_question")
        return Promise.resolve({
          resumed: false,
          lead_agent_id: null,
          resume_error: null,
        });
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

  it("onDecisionChoose: solo MCP 卡·run 已收工·后端回 resumed:false → 答复照常落库但不触发续跑绘制", async () => {
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
      // T3：solo 会话没有持久化 lead 配置——后端 try_resume_after_answer 的门天然关闭，
      // 回 resumed:false；迟到答案已由 commit_late_answer 落成真实 user 消息，留给下一轮
      // 普通 run 自然消费。
      if (cmd === "answer_lead_question")
        return Promise.resolve({
          resumed: false,
          lead_agent_id: null,
          resume_error: null,
        });
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
    // 续跑权已收归后端：前端不再自己 invoke resume_lead_session。
    expect(invokeMock).not.toHaveBeenCalledWith(
      "resume_lead_session",
      expect.anything(),
    );
    // resumed:false → 不做乐观绘制，不该出现「停止」按钮。
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
    expect(screen.queryByRole("button", { name: "重试" })).toBeNull();
  });

  it("onDecisionChoose: answer outcome 带 resume_error → 通过既有 showLeadError 显示错误提示", async () => {
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
            decision_id: "mcp-dc-resume-error",
            kind: "ask",
            question: "要不要重试恢复？",
            recommended: "继续",
            source_run_id: "mcp-lead-resume-error",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question")
        return Promise.resolve({
          resumed: false,
          lead_agent_id: null,
          resume_error: "provider unavailable",
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /继续/ }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
        sessionId: "s1",
        decisionId: "mcp-dc-resume-error",
        answer: "继续",
      }),
    );

    // 正向：非结构化错误沿用 showLeadError 的 generic 展示，不再静默吞掉 resume_error。
    expect(
      await screen.findByText("队长没想清楚下一步，换个说法再讲一遍？"),
    ).toBeInTheDocument();
    expect(screen.getByText("为什么：lead_step 失败")).toBeInTheDocument();
  });

  it("onDecisionChoose: team MCP 卡·run 已收工(不在跑)·后端回 resumed:true → 乐观绘制 run·不再自己 invoke resume_lead_session（T3 续跑权收归后端）", async () => {
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
      // T3：team 会话（有持久化 lead 配置）——后端 try_resume_after_answer 门开，落库成功
      // 后自己触发续跑，answer_lead_question 直接回 resumed:true。
      if (cmd === "answer_lead_question")
        return Promise.resolve({
          resumed: true,
          lead_agent_id: null,
          resume_error: null,
        });
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
    // 正向：前端只按 resumed:true 做乐观绘制（setRun + ensureStreamTail），真续跑已经在后端
    // 发生——不再自己 invoke resume_lead_session（否则本机路径会双触发、撞 busy 弹假错误）。
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument(),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "resume_lead_session",
      expect.anything(),
    );
  });

  it("onDecisionChoose: resumed:true 且 outcome 提供 lead_agent_id → 乐观 identity 优先采用后端值", async () => {
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
            decision_id: "mcp-dc-resume-identity",
            kind: "ask",
            question: "由谁继续？",
            recommended: "继续",
            source_run_id: "mcp-lead-resume-identity",
          }),
        ],
      },
    );

    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question")
        return Promise.resolve({
          resumed: true,
          lead_agent_id: "deepseek",
          resume_error: null,
        });
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    fireEvent.click(inlineDecisionCard().getByRole("button", { name: /继续/ }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
        sessionId: "s1",
        decisionId: "mcp-dc-resume-identity",
        answer: "继续",
      }),
    );

    // 正向：当前 team effectiveLeadId 是 claude；后端 outcome 明确给 deepseek 时，流尾身份须跟后端。
    expect(
      await screen.findByRole("status", { name: "DeepSeek 正在工作" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("status", { name: "Claude Code 正在工作" }),
    ).toBeNull();
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();
  });

  it("onDecisionChoose: answer 尚未落定时终态抢跑清态 → resumed:true 不得复活幽灵 run", async () => {
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
            decision_id: "mcp-dc-closeout-race",
            kind: "ask",
            question: "要不要继续这轮？",
            recommended: "继续",
            source_run_id: "mcp-lead-closeout-race",
          }),
        ],
      },
    );

    const answerDeferred = deferred<{
      resumed: boolean;
      lead_agent_id: string | null;
      resume_error: string | null;
    }>();
    const defaultInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "answer_lead_question") return answerDeferred.promise;
      return defaultInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    await clickDecisionOption("继续");
    await waitFor(() => {
      expect(
        inlineDecisionCard().getByRole("button", { name: /继续/ }),
      ).toBeDisabled();
    });

    // 反向时序：IPC 仍 pending 时，lead blocked 终态先抵达并经 setRun(sid, null) 清态。
    await act(async () => {
      emitAgentEventBatch([{ kind: "blocked", message: "provider stopped" }]);
    });
    await act(async () => {
      answerDeferred.resolve({
        resumed: true,
        lead_agent_id: "deepseek",
        resume_error: null,
      });
      await answerDeferred.promise;
    });

    // outcome 虽说 resumed:true，也不得在已发生的终态之后重新画出停止按钮。
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
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
      // 会话已在跑（Delivered 路由，非迟到路径）——commit_late_answer 不会被调用，后端
      // 天然回 resumed:false；前端也因 runningSessionsRef.current.has(sid) 先命中而不看它。
      if (cmd === "answer_lead_question")
        return Promise.resolve({
          resumed: false,
          lead_agent_id: null,
          resume_error: null,
        });
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
