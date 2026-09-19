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
    agentProfile,
    agentProfiles,
    mockBasicApp,
    configureTeamLead,
    agentEventCb,
    leadDecisionCardCb,
    decisionCardMessage,
    inlineDecisionCard,
    askCardPayload,
    setupRunningS1,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

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

  // V3a：`stream_live` 三处补标 + 活尾唯一不变量（设计稿 §2B「进行中判据」①②）。
  // 这些断言必须落在 App.test.tsx——三处造空 assistant 与决策卡/答卡续写全在 App
  // 内部发送路径上，streamBlocks.test.ts 只能证明纯函数本身，证明不了生产路径真的
  // 接上了这些纯函数。
  describe("V3a：stream_live 活尾唯一不变量", () => {
    function liveTailCount(): number {
      const last = sessionMainProps[sessionMainProps.length - 1];
      return (last?.messages ?? []).filter((m) => m.stream_live === true)
        .length;
    }

    it("新建会话 solo 首发：末条 assistant 打活标，completed 后清零", async () => {
      // 启动引导会自动开一个空会话（bootstrap 的「0 会话则建一个」兜底）——要真的
      // 走 createSessionAndSend（新建会话 solo 首发）这条目标代码路径，得像既有
      // 「intro 新会话 Team 发送」模板那样先经「项目简介」显式回到 intro composer
      // 再发送（此时 currentId 仍指着旧会话，但发送会另建一个全新 sid）。
      mockBasicApp(agentProfiles);
      render(<App />);
      await screen.findByText("Claude Code");

      fireEvent.click(screen.getByText("项目简介"));
      await screen.findByRole("heading", { name: "Local 默认" });

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "solo 新建首发" },
      });
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
      );
      fireEvent.click(screen.getByRole("button", { name: "发送" }));

      await waitFor(() =>
        expect(
          invokeMock.mock.calls.some(
            ([cmd, args]) => cmd === "create_session" && args?.id,
          ),
        ).toBe(true),
      );
      const createCalls = invokeMock.mock.calls.filter(
        ([cmd]) => cmd === "create_session",
      );
      const sid = createCalls[createCalls.length - 1]?.[1]?.id as string;
      expect(sid).toBeTruthy();

      await waitFor(() => expect(liveTailCount()).toBe(1));
      const last1 = sessionMainProps[sessionMainProps.length - 1]!;
      const tail1 = last1.messages![last1.messages!.length - 1];
      expect(tail1.role).toBe("assistant");
      expect(tail1.stream_live).toBe(true);

      const handler = agentEventCb();
      act(() => {
        handler({
          payload: {
            session_id: sid,
            kind: "completed",
            cost_usd: null,
            input_tokens: null,
            output_tokens: 1,
            final_text: "已完成",
            run_id: "run-solo-new-1",
            commit_sha: null,
            files_changed: null,
            insertions: 0,
            deletions: 0,
            interrupted: false,
          },
        });
      });

      await waitFor(() => expect(liveTailCount()).toBe(0));
    });

    it("team 首发：末条 assistant 打活标，completed 后清零", async () => {
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
      render(<App />);
      await screen.findByText("Claude Code");
      await configureTeamLead();

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "team 首发" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "start_lead_session",
          expect.objectContaining({ sessionId: "s1" }),
        ),
      );

      await waitFor(() => expect(liveTailCount()).toBe(1));
      const last1 = sessionMainProps[sessionMainProps.length - 1]!;
      const tail1 = last1.messages![last1.messages!.length - 1];
      expect(tail1.role).toBe("assistant");
      expect(tail1.stream_live).toBe(true);

      const handler = agentEventCb();
      act(() => {
        handler({
          payload: {
            session_id: "s1",
            kind: "completed",
            cost_usd: null,
            input_tokens: null,
            output_tokens: 1,
            final_text: "队长完成",
            run_id: "run-team-first-1",
            commit_sha: null,
            files_changed: null,
            insertions: 0,
            deletions: 0,
            interrupted: false,
          },
        });
      });

      await waitFor(() => expect(liveTailCount()).toBe(0));
    });

    it("lead reply：末条 assistant 打活标，completed 后清零", async () => {
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
              decision_id: "v3a-legacy-dc-1",
              kind: "ask",
              question: "继续吗？",
              recommended: "开跑",
              source_run_id: "run-v3a-legacy-1",
            }),
          ],
        },
      );

      const defaultInvoke = invokeMock.getMockImplementation();
      invokeMock.mockImplementation((cmd: string, args?: any) => {
        if (cmd === "answer_lead_question")
          return Promise.reject("NO_PENDING_QUESTION:v3a-legacy-dc-1");
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
        return defaultInvoke?.(cmd, args);
      });

      render(<App />);
      await screen.findByText("Claude Code");
      await configureTeamLead();

      expect(inlineDecisionCard().getByText(/继续吗？/)).toBeInTheDocument();
      fireEvent.click(
        inlineDecisionCard().getByRole("button", { name: /开跑/ }),
      );

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "send_message",
          expect.objectContaining({ message: "开跑" }),
        ),
      );

      await waitFor(() => expect(liveTailCount()).toBe(1));
      const last1 = sessionMainProps[sessionMainProps.length - 1]!;
      const tail1 = last1.messages![last1.messages!.length - 1];
      expect(tail1.role).toBe("assistant");
      expect(tail1.stream_live).toBe(true);

      const handler = agentEventCb();
      act(() => {
        handler({
          payload: {
            session_id: "s1",
            kind: "completed",
            cost_usd: null,
            input_tokens: null,
            output_tokens: 1,
            final_text: "回答完成",
            run_id: "run-lead-reply-1",
            commit_sha: null,
            files_changed: null,
            insertions: 0,
            deletions: 0,
            interrupted: false,
          },
        });
      });

      await waitFor(() => expect(liveTailCount()).toBe(0));
    });

    it("发起失败（team 首发 start_lead_session 抛错）：活标即时封口，计数 0", async () => {
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
        if (cmd === "start_lead_session") return Promise.reject("BACKEND_DOWN");
        return defaultInvoke?.(cmd, args);
      });

      render(<App />);
      await screen.findByText("Claude Code");
      await configureTeamLead();

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "发起即失败" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));

      // 乐观占位在发送那一刻就打了活标；start_lead_session 随后失败——这条失败路径
      // 只弹 showLeadError（不重建消息数组），必须显式经 sealStreamTail 封口，
      // 否则活尾会永远悬空（会话空闲后仍显示「工作中」）。
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "start_lead_session",
          expect.objectContaining({ sessionId: "s1" }),
        ),
      );
      await waitFor(() => expect(liveTailCount()).toBe(0));
    });

    it("过程尾 → MCP 决策卡插入 → 答卡续写 → completed：全程活标 ≤1，终态清零", async () => {
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

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "全程链路" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "start_lead_session",
          expect.objectContaining({ sessionId: "s1" }),
        ),
      );
      await waitFor(() => expect(liveTailCount()).toBe(1));

      // 过程块：一次工具调用挂在同一条活尾上（不新增活标）。
      const agentEvent = agentEventCb();
      act(() => {
        agentEvent({
          payload: {
            session_id: "s1",
            kind: "tool_started",
            id: "tc-v3a-1",
            tool: "Bash",
            summary: "ls",
            card: "command",
          },
        });
      });
      expect(liveTailCount()).toBeLessThanOrEqual(1);
      expect(liveTailCount()).toBe(1);

      // MCP 决策卡插入：追加一条未打标消息前先封掉活尾（活尾唯一不变量）。
      const decisionCard: Extract<Block, { type: "decision_card" }> = {
        type: "decision_card",
        decision_id: "v3a-mcp-dc-1",
        kind: "ask",
        question: "要不要继续？",
        options: ["继续", "先停下"],
        recommended: "继续",
        rationale: "需要用户确认",
        payload: null,
        source_run_id: "mcp-lead-v3a-1",
        status: "pending",
        chosen_option: null,
        created_at: 1000,
      };
      await act(async () => {
        leadDecisionCardCb()({
          payload: { session_id: "s1", block: decisionCard },
        });
      });
      await waitFor(() => {
        expect(document.querySelectorAll(".decision-card")).toHaveLength(1);
      });
      expect(liveTailCount()).toBeLessThanOrEqual(1);
      expect(liveTailCount()).toBe(0);

      // 答卡：点击推荐项 → answer_lead_question 成功 → 队长仍在跑 → 另起新活尾续写。
      fireEvent.click(
        inlineDecisionCard().getByRole("button", { name: /继续/ }),
      );
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("answer_lead_question", {
          sessionId: "s1",
          decisionId: "v3a-mcp-dc-1",
          answer: "继续",
        }),
      );
      await waitFor(() => expect(liveTailCount()).toBe(1));

      // 答卡后续写（text_delta）灌进同一条新活尾，不产生第二条活标。
      act(() => {
        agentEvent({
          payload: { session_id: "s1", kind: "text_delta", text: "继续处理中" },
        });
      });
      await screen.findByText("继续处理中");
      expect(liveTailCount()).toBeLessThanOrEqual(1);
      expect(liveTailCount()).toBe(1);

      // completed：终态清零。
      act(() => {
        agentEvent({
          payload: {
            session_id: "s1",
            kind: "completed",
            cost_usd: null,
            input_tokens: null,
            output_tokens: 1,
            final_text: null,
            run_id: "run-v3a-full-1",
            commit_sha: null,
            files_changed: null,
            insertions: 0,
            deletions: 0,
            interrupted: false,
          },
        });
      });
      await waitFor(() => expect(liveTailCount()).toBe(0));
    });

    // F9（如实断言现状，不修）：A 轮迟到的 run_closeout 到达时，若 B 轮已经开跑，会把
    // B 轮活尾误封、并把 busy 清假——设计稿
    // desktop-verbose-design §2B「进行中判据」F9
    // 记的已知局限；决策点 7 已定「不纳入本刀」，真正的 run 级闸门需要契约扩展（后端
    // 在 run 起点带 run_id + 前端 RunInfo.runId 比对），记 BACKLOG 单独小刀——这里只
    // 如实断言现状，不修它。
    it("documents current behavior: A 轮迟到 run_closeout 到达时误封已开跑的 B 轮活尾并清 busy", async () => {
      const { sendCalls } = mockBasicApp();
      render(<App />);
      await screen.findByText("Claude Code");

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "A 轮" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));
      await waitFor(() => expect(sendCalls).toHaveLength(1));
      await waitFor(() => expect(liveTailCount()).toBe(1));

      const handler = agentEventCb();
      // A 轮正常收工：completed 先到，封口 + 清 run。
      act(() => {
        handler({
          payload: {
            session_id: "s1",
            kind: "completed",
            cost_usd: null,
            input_tokens: null,
            output_tokens: 1,
            final_text: "A 轮完成",
            run_id: "run-A",
            commit_sha: null,
            files_changed: null,
            insertions: 0,
            deletions: 0,
            interrupted: false,
          },
        });
      });
      await waitFor(() => expect(liveTailCount()).toBe(0));
      expect(
        screen.queryByRole("button", { name: "停止" }),
      ).not.toBeInTheDocument();

      // B 轮开跑（此刻不在忙 → 直接发，不进队列）。
      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "B 轮" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));
      await waitFor(() => expect(sendCalls).toHaveLength(2));
      await waitFor(() => expect(liveTailCount()).toBe(1));
      expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();

      // A 轮迟到的 run_closeout 现在才到——run_id 是 A 的，但当前活尾已经是 B 轮的。
      act(() => {
        handler({
          payload: {
            session_id: "s1",
            kind: "run_closeout",
            run_id: "run-A",
            commit_sha: null,
            files_changed: null,
            insertions: null,
            deletions: null,
            interrupted: false,
          },
        });
      });

      // 现状（F9 已知局限，本 task 不修）：B 轮活尾被无条件误封，busy 随之清假。
      await waitFor(() => expect(liveTailCount()).toBe(0));
      expect(
        screen.queryByRole("button", { name: "停止" }),
      ).not.toBeInTheDocument();
    });
  });
});
