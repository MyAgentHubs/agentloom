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
import { readFileSync } from "fs";
import { describe, it, expect, vi } from "vitest";
import { makeSession } from "./test/factories";
import App, { applyEventTransportBatch } from "./App";
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
    agentEventBatchCb,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

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

  it("session_started · 远程 run 用会话配置 agent 注册 running 身份", async () => {
    mockBasicApp([
      agentProfile(),
      agentProfile({
        id: "deepseek",
        name: "DeepSeek",
        provider: "deepseek",
        access: "borrow",
        sort_order: 1,
      }),
    ]);
    const { container } = render(<App />);
    await screen.findByText("Claude Code");
    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: "DeepSeek" }));
    await configureTeamLead("DeepSeek");

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "session_started",
          conversation_id: "remote-conversation",
        },
      });
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "text_delta",
          text: "remote streaming",
        },
      });
    });

    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();
    expect(
      container.querySelector('[data-session-id="s1"] .sess__dot'),
    ).toHaveClass("run");
    await screen.findByText("remote streaming");
    const remoteTail = sessionMainProps[
      sessionMainProps.length - 1
    ]?.messages?.find((message) =>
      message.content.some(
        (block) =>
          (block as { type?: string; text?: string }).type === "text" &&
          (block as { text?: string }).text === "remote streaming",
      ),
    );
    expect(remoteTail).toMatchObject({
      engine: "deepseek",
      agent_id: "deepseek",
      agent_name_snapshot: "DeepSeek",
    });
  });

  it("session_started · 本地 run 已乐观注册时不覆盖 workingTokens", async () => {
    const { sendCalls } = mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "run" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "usage_delta",
          input_tokens: 3,
          output_tokens: 4,
        },
      });
    });
    expect(
      within(
        document.querySelector(".composer__hint-cost") as HTMLElement,
      ).getByText(/↑ 7 tok/),
    ).toBeInTheDocument();

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "session_started",
          conversation_id: "local-conversation",
        },
      });
    });

    expect(
      within(
        document.querySelector(".composer__hint-cost") as HTMLElement,
      ).getByText(/↑ 7 tok/),
    ).toBeInTheDocument();
  });

  it("session_started · 远程 run 收到终态后照常清理 running 态", async () => {
    mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "session_started",
          conversation_id: "remote-conversation",
        },
      });
    });
    expect(screen.getByRole("button", { name: "停止" })).toBeInTheDocument();

    await act(async () => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: null,
          final_text: "remote done",
        },
      });
      await Promise.resolve();
    });

    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
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
});
