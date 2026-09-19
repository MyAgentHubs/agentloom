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
    agentProfiles,
    mockBasicApp,
    configureTeamLead,
    agentEventCb,
    decisionCardMessage,
    inlineDecisionCard,
    deferred,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  // P1（codex 整盘审·整盘返工）：三条发起路径收到 SESSION_ALREADY_RUNNING/
  // SESSION_BUSY 时，提前 return 曾发生在 setRun/封口之前——幽灵竞态（这期间没有
  // 任何真实事件到达）会永远留下 busy=false 但 stream_live=true 的死尾巴（或反过
  // 来 busy 该清而没清），摘要/精简档下会显示一条永远「正在运行」的实时线。修法
  // 是抽出 `convergeAlreadyRunningConflict` helper（判据抄自 sendSoloForSession
  // 早就有的 event-epoch 冲突收敛），三条路径统一调用；真实并发（这期间已经有
  // 真实 agent-event 到达）时原样保留 run/活尾，不收敛。
  describe("P1：SESSION_ALREADY_RUNNING/SESSION_BUSY 冲突收敛", () => {
    function liveTailCount(): number {
      const last = sessionMainProps[sessionMainProps.length - 1];
      return (last?.messages ?? []).filter((m) => m.stream_live === true)
        .length;
    }

    it("team 首发：SESSION_ALREADY_RUNNING 无真实事件到达 → 活标清零、停止按钮不在", async () => {
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
        if (cmd === "start_lead_session")
          return Promise.reject("SESSION_ALREADY_RUNNING:s1");
        return defaultInvoke?.(cmd, args);
      });

      render(<App />);
      await screen.findByText("Claude Code");
      await configureTeamLead();

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "team 首发撞占槽" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "start_lead_session",
          expect.objectContaining({ sessionId: "s1" }),
        ),
      );
      await waitFor(() => expect(liveTailCount()).toBe(0));
      expect(
        screen.queryByRole("button", { name: "停止" }),
      ).not.toBeInTheDocument();
    });

    it("lead reply：SESSION_ALREADY_RUNNING 无真实事件到达 → 活标清零、停止按钮不在", async () => {
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
              decision_id: "p1-legacy-dc-1",
              kind: "ask",
              question: "继续吗？",
              recommended: "开跑",
              source_run_id: "run-p1-legacy-1",
            }),
          ],
        },
      );
      const defaultInvoke = invokeMock.getMockImplementation();
      invokeMock.mockImplementation((cmd: string, args?: any) => {
        if (cmd === "answer_lead_question")
          return Promise.reject("NO_PENDING_QUESTION:p1-legacy-dc-1");
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
        if (cmd === "send_message")
          return Promise.reject("SESSION_ALREADY_RUNNING:s1");
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
      await waitFor(() => expect(liveTailCount()).toBe(0));
      expect(
        screen.queryByRole("button", { name: "停止" }),
      ).not.toBeInTheDocument();
    });

    it("新建会话 solo 首发：SESSION_ALREADY_RUNNING 无真实事件到达 → 活标清零、停止按钮不在", async () => {
      mockBasicApp(agentProfiles);
      const defaultInvoke = invokeMock.getMockImplementation();
      invokeMock.mockImplementation((cmd: string, args?: any) => {
        if (cmd === "send_message")
          return Promise.reject("SESSION_ALREADY_RUNNING:new-sid");
        return defaultInvoke?.(cmd, args);
      });

      render(<App />);
      await screen.findByText("Claude Code");

      fireEvent.click(screen.getByText("项目简介"));
      await screen.findByRole("heading", { name: "Local 默认" });

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "solo 新建首发撞占槽" },
      });
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
      );
      fireEvent.click(screen.getByRole("button", { name: "发送" }));

      await waitFor(() =>
        expect(
          invokeMock.mock.calls.some(([cmd]) => cmd === "send_message"),
        ).toBe(true),
      );
      await waitFor(() => expect(liveTailCount()).toBe(0));
      expect(
        screen.queryByRole("button", { name: "停止" }),
      ).not.toBeInTheDocument();
    });

    it("team 首发：SESSION_ALREADY_RUNNING 但期间已有真实事件到达 → run/活尾原样保留（不收敛）", async () => {
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
      const startLeadDeferred = deferred<unknown>();
      const defaultInvoke = invokeMock.getMockImplementation();
      invokeMock.mockImplementation((cmd: string, args?: any) => {
        if (cmd === "start_lead_session") return startLeadDeferred.promise;
        return defaultInvoke?.(cmd, args);
      });

      render(<App />);
      await screen.findByText("Claude Code");
      await configureTeamLead();

      fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
        target: { value: "team 首发真实并发" },
      });
      fireEvent.click(screen.getByRole("button", { name: "发送" }));

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "start_lead_session",
          expect.objectContaining({ sessionId: "s1" }),
        ),
      );
      expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();
      expect(liveTailCount()).toBe(1);

      // 真实事件先到——同一会话收到一条 agent-event（epoch 涨了），证明另一头真的在跑。
      act(() => {
        agentEventCb()({
          payload: {
            session_id: "s1",
            kind: "text_delta",
            text: "真实并发内容",
          },
        });
      });
      await screen.findByText("真实并发内容");

      // 随后这次占位请求才被后端拒绝——真实并发，不该收敛（run/活尾原样保留）。
      await act(async () => {
        startLeadDeferred.reject("SESSION_ALREADY_RUNNING:s1");
        await startLeadDeferred.promise.catch(() => {});
      });

      expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();
      expect(liveTailCount()).toBe(1);
    });
  });
});
