import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
  within,
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
    localNamespace,
    localRepo,
    emptyReview,
    agentProfile,
    agentProfiles,
    appMember,
    mockBasicApp,
    agentEventCb,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

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

  it("session 并发 Task 3 · 当前 busy 排队（msgfix2 Q1，不再禁发）但切 idle session 仍可直接发", async () => {
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
    // msgfix2 Q1：运行中发送位不再被停止替换——发送位仍在，点它是「排队」而非直发。
    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();

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

  it("msgfix2 Q1：member running 时排队的消息在 member 收尾后自动递送恰一次（双触发不重复）", async () => {
    vi.useFakeTimers();
    let stillRunningCallCount = 0;
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
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "is_team_session_running") {
        stillRunningCallCount += 1;
        // 第 1 次：挂载即触发的 idle-poll 首检（仍在跑，poll 继续等 15s）。
        // 第 2 次：点发送时 InputArea 的复核（仍在跑 → 入队而非直发）。
        // 第 3 次起：15s 后 idle-poll 复检 + tryDeliverQueued 自己的复核 → 已 idle。
        return Promise.resolve(stillRunningCallCount <= 2);
      }
      return baseInvoke?.(cmd, args);
    });
    render(<App />);
    // 全程 fake timers：不用 findBy*/waitFor（内部靠 setTimeout 轮询，fake timers 下会
    // 一直等不到真实时钟推进而超时）——改手动 flush microtask + 已到期 timer 再同步断言。
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(
      screen.getByRole("button", { name: "选择 agent：Claude Code" }),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second msg" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(screen.getByText("已排队 1 条")).toBeInTheDocument();
    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message"),
    ).toHaveLength(0);
    expect(screen.getByPlaceholderText(/输入消息/)).toHaveValue("");

    // 15s 后 idle-poll 复检 → false → onIdle 触发递送；同一沿 member 卡收敛也会让
    // busy||memberRunning 下降沿的 effect 判定成立——双触发只应各投一次 send_message。
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });

    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message"),
    ).toHaveLength(1);
    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message")[0][1],
    ).toMatchObject({ sessionId: "s1", message: "second msg" });
    expect(screen.queryByText("已排队 1 条")).not.toBeInTheDocument();
    expect(screen.queryByTestId("composer-queue")).not.toBeInTheDocument();

    vi.useRealTimers();
  });

  it("msgfix2 Q1：显式停止后暂停自动递送，chip 转待手动发送态；点单条发送才递送", async () => {
    let stillRunning = true;
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
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "is_team_session_running")
        return Promise.resolve(stillRunning);
      return baseInvoke?.(cmd, args);
    });
    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "queued msg" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await screen.findByText("已排队 1 条");

    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("stop_session", {
        sessionId: "s1",
      }),
    );

    // 停止后暂停：即便后续沿再触发也不该自动递送。
    await screen.findByText(/待手动发送/);
    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message"),
    ).toHaveLength(0);

    // chip 上单条「发送」：无视 paused 门，手动递送这一条。
    stillRunning = false;
    const chip = screen.getByTestId("composer-queue-item");
    fireEvent.click(within(chip).getByRole("button", { name: "发送" }));

    await waitFor(() =>
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message"),
      ).toHaveLength(1),
    );
    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message")[0][1],
    ).toMatchObject({ sessionId: "s1", message: "queued msg" });
    expect(screen.queryByTestId("composer-queue")).not.toBeInTheDocument();
  });

  it("msgfix2 F3 T1：递送 await 复核期间用户撤回该条目 → 放弃递送，不照发", async () => {
    const { sendCalls } = mockBasicApp(agentProfiles);
    let resolveStillRunning!: (v: boolean) => void;
    const stillRunningPromise = new Promise<boolean>((resolve) => {
      resolveStillRunning = resolve;
    });
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "is_team_session_running") return stillRunningPromise;
      if (cmd === "send_message") {
        sendCalls.push(args);
        return new Promise(() => {});
      }
      return baseInvoke?.(cmd, args);
    });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await screen.findByText("已排队 1 条");

    // 第一条 run 收尾——触发全局下降沿 → tryDeliverQueued → deliverQueuedTarget
    // 卡在 is_team_session_running 这个 await 上（复核窗口）。
    act(() => {
      agentEventCb()({
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

    // await 复核尚未 resolve 期间，用户在 chip 上点了「删除」撤回这条排队消息。
    const chip = screen.getByTestId("composer-queue-item");
    fireEvent.click(within(chip).getByRole("button", { name: "删除" }));
    expect(screen.queryByTestId("composer-queue")).not.toBeInTheDocument();

    // 复核 resolve（已 idle）——deliverQueuedTarget 恢复执行，但目标条目已被用户拿走。
    await act(async () => {
      resolveStillRunning(false);
      await stillRunningPromise;
    });

    // 不该照发：send_message 总调用次数仍只有最初那 1 次（直发的第一条）。
    expect(sendCalls).toHaveLength(1);
    expect(screen.queryByTestId("composer-queue")).not.toBeInTheDocument();
  });

  it("msgfix2 F3 T2：空队列停止 → paused 不残留，后续排队仍自动递送", async () => {
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
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "send_message") {
        sendCalls.push(args);
        return new Promise(() => {});
      }
      return Promise.resolve();
    });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });

    // 第一条直发：session 进入 busy，队列为空。
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    // 空队列时点停止——旧 bug：无条件置 paused，之后没有任何出队/移除动作能清掉它。
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("stop_session", {
        sessionId: "s1",
      }),
    );

    // stop_session 的 .finally 会把仍 running 的 run 前端兜底清掉——busy 恢复 false。
    // 停止后立刻直发第二条——不经过队列（session 已不 busy）：开启新一轮 run。
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(2));

    // 第二轮跑着的时候再排队第三条。
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "third" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await screen.findByText("已排队 1 条");

    // 第二轮收尾——若 paused 残留（旧 bug）这里不会自动递送；修复后应正常投出第三条。
    act(() => {
      agentEventCb()({
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

    await waitFor(() => expect(sendCalls).toHaveLength(3));
    expect(sendCalls[2]).toMatchObject({ sessionId: "s1", message: "third" });
    expect(screen.queryByTestId("composer-queue")).not.toBeInTheDocument();
  });

  it("msgfix2 F3 T3：会话 A 排队后切到 B，A 在后台跑完 → 无需切回也自动递送", async () => {
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
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "is_team_session_running") return Promise.resolve(false);
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

    // 会话一：直发第一条（busy）→ 排队第二条。
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "a-first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "a-second" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await screen.findByText("已排队 1 条");

    // 切到会话二——不再看着会话一。
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    expect(screen.queryByText("已排队 1 条")).not.toBeInTheDocument();

    // 会话一在后台跑完（agent-event 按 session_id 精确落，不依赖当前视图）。
    act(() => {
      agentEventCb()({
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

    // 无需切回会话一，排队的第二条也该自动投出去。
    await waitFor(() => expect(sendCalls).toHaveLength(2));
    expect(sendCalls[1]).toMatchObject({
      sessionId: "s1",
      message: "a-second",
    });
  });

  it("msgfix2 F3 T4：递送前 agent 已不可用（native 检测转 false）→ 条目留队不丢", async () => {
    const claudeAgent = agentProfile({
      id: "claude",
      name: "Claude Code",
      access: "native",
      provider: "claude",
    });
    let resolveDetect!: (v: any) => void;
    const detectPromise = new Promise<any>((resolve) => {
      resolveDetect = resolve;
    });
    const sendCalls: any[] = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_agents") return Promise.resolve([claudeAgent]);
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
      if (cmd === "detect_runtime") return detectPromise;
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "is_team_session_running") return Promise.resolve(false);
      if (cmd === "send_message") {
        sendCalls.push(args);
        return new Promise(() => {});
      }
      return Promise.resolve();
    });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });

    // runtimeDetect 尚未 resolve（undefined）→ native 乐观可用，第一条直发。
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    // 排队第二条（session busy）。
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await screen.findByText("已排队 1 条");

    // detect_runtime 迟到解析：claude 原生 CLI 其实不可用。
    await act(async () => {
      resolveDetect({
        claude: { available: false },
        codex: { available: true },
      });
      await detectPromise;
    });

    // 第一条 run 收尾——触发全局下降沿，尝试自动递送。
    act(() => {
      agentEventCb()({
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
    await act(async () => {});

    // agent 已不可用——递送应放弃，条目留队可见，不丢也不误发。
    expect(sendCalls).toHaveLength(1);
    expect(screen.getByText("已排队 1 条")).toBeInTheDocument();
  });

  it("msgfix2 F3 T4：递送发起撞 SESSION_ALREADY_RUNNING → 条目回到队首不丢", async () => {
    let sendCallCount = 0;
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
          claude: { available: true },
          codex: { available: true },
        });
      if (cmd === "list_acceptance") return Promise.resolve([]);
      if (cmd === "is_team_session_running") return Promise.resolve(false);
      if (cmd === "send_message") {
        sendCallCount += 1;
        sendCalls.push(args);
        // 第一条直发：永不 resolve，维持 busy；递送时（第二次调用）撞后端占槽竞争。
        if (sendCallCount === 1) return new Promise(() => {});
        return Promise.reject("SESSION_ALREADY_RUNNING:s1");
      }
      return Promise.resolve();
    });

    render(<App />);
    await screen.findByRole("button", { name: "选择 agent：Claude Code" });

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await screen.findByText("已排队 1 条");

    act(() => {
      agentEventCb()({
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

    // 递送尝试撞 SESSION_ALREADY_RUNNING——条目应留在队列可见，不丢。
    await waitFor(() => expect(sendCalls).toHaveLength(2));
    expect(sendCalls[1]).toMatchObject({ sessionId: "s1", message: "second" });
    await waitFor(() =>
      expect(screen.getByText("已排队 1 条")).toBeInTheDocument(),
    );
  });
});
