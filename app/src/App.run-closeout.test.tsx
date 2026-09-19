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
    agentEventCb,
    sessionReviewCallCount,
    startRunCloseoutLiveUi,
    decisionCardMessage,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

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

      const terminalEl = await screen.findByText(new RegExp(terminalText));
      expect(terminalEl).toBeInTheDocument();
      // U6 修复轮 2：终态文案落地时所在的 .turn，就是 run_closeout 的 run_card
      // 收尾续写理应续灌进的同一条消息——记下来，closeout 到达后核对没被劈开。
      const terminalTurn = terminalEl.closest(".turn");
      expect(terminalTurn).not.toBeNull();
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

      const runCardGroup = await screen.findByRole("group", {
        name: "本轮改动",
      });
      expect(runCardGroup).toBeInTheDocument();
      // run_closeout 是同一 run 的收尾续写，不是新一轮 delta：run_card 必须落进
      // 终态文案所在的同一条 .turn，不该另起孤儿气泡（修前的回归症状）。
      expect(runCardGroup.closest(".turn")).toBe(terminalTurn);
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

  it("onStop → blocked → run_closeout(files_changed 非空)：run_card 落进停止文案同一 .turn（U6 修复轮 2）", async () => {
    const { handler } = await startRunCloseoutLiveUi();

    // U6 修复轮 3（F1）：停止前先有一段已流式文本——这才是 onStop 要保护的场景本体：
    // 用户点停止时本轮文案还没流完。onStop() 里 sealStreamTail 先把这条消息封了口；
    // 随后到达的 blocked 若不传 allowSealedTail，会把停止文案劈进另起的孤儿气泡，
    // 跟已经流出来的这段文本分家（修前的回归症状——F1 定罪的正是这条链路）。
    act(() => {
      handler({
        payload: { session_id: "s1", kind: "text_delta", text: "已经流了一段" },
      });
    });
    const streamedEl = await screen.findByText("已经流了一段");
    const streamedTurn = streamedEl.closest(".turn");
    expect(streamedTurn).not.toBeNull();

    // 真走 onStop() 代码路径（点停止按钮），不是像 it.each 那组直接派终态事件：
    // onStop() 自己先 sweep+seal 一次（无文案）；随后到达的 blocked 事件才带真正
    // 的停止文案、并再触发一次 ensureStreamTail+sealStreamTail。这条接缝是
    // it.each 直接派事件那组用例没覆盖到的。
    fireEvent.click(screen.getByRole("button", { name: "停止" }));

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "blocked",
          message: "USER_STOPPED_MID_RUN",
        },
      });
    });

    const stopText = await screen.findByText(/USER_STOPPED_MID_RUN/);
    const stopTurn = stopText.closest(".turn");
    expect(stopTurn).not.toBeNull();
    // F1：停止文案必须续灌进已流式文本所在的同一条 .turn，不该另起孤儿气泡
    // （这是现有用例漏掉的归属校验——它此前只断言 run_card 与停止文案同 turn）。
    expect(stopTurn).toBe(streamedTurn);

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-onstop-closeout",
          commit_sha: "onstop-sha",
          files_changed: 3,
          insertions: 5,
          deletions: 2,
          interrupted: true,
        },
      });
    });

    const runCardGroup = await screen.findByRole("group", {
      name: "本轮改动",
    });
    // run_closeout 是这一个 run 的收尾续写，不是新一轮 delta：即便 onStop 已经
    // 先封过一次口、blocked 又封了一次口，run_card 依旧该续灌进停止文案那条
    // .turn，不该另起孤儿气泡（修前的回归症状）。
    expect(runCardGroup.closest(".turn")).toBe(stopTurn);
  });

  it.each([
    {
      terminalKind: "Error",
      terminal: { kind: "error", message: "USER_STOPPED_ERROR_RACE" },
      terminalTextRegex: /USER_STOPPED_ERROR_RACE/,
    },
    {
      terminalKind: "NeedsDecision",
      terminal: {
        kind: "needs_decision",
        run_id: "run-onstop-needs-decision",
        reason: "需要扩大范围",
        changes: [
          {
            proposal_id: "proposal-onstop-1",
            kind: "scope",
            detail_text: "必须扩大范围",
            detail_summary: null,
          },
        ],
      },
      terminalTextRegex: /必须扩大范围/,
    },
  ])(
    "onStop → $terminalKind：停止前已流式文本与终态文案落同一 .turn（U6 修复轮 3·F1）",
    async ({ terminal, terminalTextRegex }) => {
      const { handler } = await startRunCloseoutLiveUi();

      act(() => {
        handler({
          payload: {
            session_id: "s1",
            kind: "text_delta",
            text: "已经流了一段",
          },
        });
      });
      const streamedEl = await screen.findByText("已经流了一段");
      const streamedTurn = streamedEl.closest(".turn");
      expect(streamedTurn).not.toBeNull();

      // onStop() 先 sweep+seal 一次（无文案）——随后到达的终态事件若不传
      // allowSealedTail，会把自己的文案劈进另起的孤儿气泡（F1 回归症状）。
      fireEvent.click(screen.getByRole("button", { name: "停止" }));

      act(() => {
        handler({ payload: { session_id: "s1", ...terminal } });
      });

      const terminalEl = await screen.findByText(terminalTextRegex);
      expect(terminalEl.closest(".turn")).toBe(streamedTurn);
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
});
